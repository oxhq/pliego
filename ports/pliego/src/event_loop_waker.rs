/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pliego-owned wake transport for its Servo event-loop owner.
//!
//! Servo may invoke [`EventLoopWaker::wake`] from any thread. This primitive only records that
//! signal and wakes host-side waiters; it never calls Servo. The document-session owner remains
//! responsible for calling `Servo::spin_event_loop` on the thread that constructed Servo.

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use embedder_traits::EventLoopWaker;

/// One opaque snapshot of the wake sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventLoopWakeGeneration {
    event_loop: u128,
    control_response: u128,
}

impl EventLoopWakeGeneration {
    fn checked_event_loop_next(self) -> Option<Self> {
        Some(Self {
            event_loop: self.event_loop.checked_add(1)?,
            control_response: self.control_response,
        })
    }

    fn checked_control_response_next(self) -> Option<Self> {
        Some(Self {
            event_loop: self.event_loop,
            control_response: self.control_response.checked_add(1)?,
        })
    }

    /// Return whether Servo work changed after an earlier snapshot, excluding a control response
    /// which merely made its already-computed observation receivable.
    pub fn event_loop_changed_since(self, earlier: Self) -> bool {
        self.event_loop != earlier.event_loop
    }
}

/// Result of waiting for a wake generation to change before one host deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventLoopWakeWaitOutcome {
    /// At least one wake followed the caller's observed generation.
    Woken(EventLoopWakeGeneration),
    /// The host deadline elapsed without a newer generation.
    DeadlineReached(EventLoopWakeGeneration),
    /// The generation space was exhausted, so sleeping can no longer be made race-free.
    GenerationExhausted(EventLoopWakeGeneration),
}

struct EventLoopWakeState {
    generation: EventLoopWakeGeneration,
    generation_exhausted: bool,
}

impl Default for EventLoopWakeState {
    fn default() -> Self {
        Self {
            generation: EventLoopWakeGeneration {
                event_loop: 0,
                control_response: 0,
            },
            generation_exhausted: false,
        }
    }
}

#[derive(Default)]
struct SharedEventLoopWakeState {
    state: Mutex<EventLoopWakeState>,
    changed: Condvar,
    #[cfg(test)]
    about_to_wait: AtomicBool,
}

/// Cloneable wake handle accepted by `ServoBuilder::event_loop_waker`.
///
/// A host loop should snapshot [`Self::generation`] before spinning Servo or inspecting callback
/// state, then pass that snapshot to [`Self::wait_for_generation`] if no work completed. A wake
/// arriving anywhere between those operations changes the generation and therefore cannot be
/// lost between the predicate check and the condition-variable wait.
#[derive(Clone, Default)]
pub struct PliegoEventLoopWaker {
    shared: Arc<SharedEventLoopWakeState>,
}

impl PliegoEventLoopWaker {
    /// Create an independent wake domain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the current wake generation without consuming it.
    pub fn generation(&self) -> EventLoopWakeGeneration {
        self.lock_state().generation
    }

    /// Wake the owner after a controlled-command result has entered its local receiver without
    /// classifying that delivery as new Servo or document-producer work.
    pub fn notify_control_response(&self) {
        self.advance_generation(EventLoopWakeGeneration::checked_control_response_next);
    }

    /// Wait until the generation differs from `observed` or `deadline` is reached.
    ///
    /// The predicate is checked while holding the same mutex used by [`EventLoopWaker::wake`].
    /// Condition-variable notifications are never treated as proof of a wake by themselves, so
    /// spurious notifications cannot advance the host loop. Exhausting the sequence returns
    /// [`EventLoopWakeWaitOutcome::GenerationExhausted`] immediately instead of wrapping or
    /// sleeping on an ambiguous generation.
    pub fn wait_for_generation(
        &self,
        observed: EventLoopWakeGeneration,
        deadline: Instant,
    ) -> EventLoopWakeWaitOutcome {
        let mut state = self.lock_state();
        loop {
            if state.generation_exhausted {
                return EventLoopWakeWaitOutcome::GenerationExhausted(state.generation);
            }
            if state.generation != observed {
                return EventLoopWakeWaitOutcome::Woken(state.generation);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return EventLoopWakeWaitOutcome::DeadlineReached(state.generation);
            }

            #[cfg(test)]
            self.shared.about_to_wait.store(true, Ordering::SeqCst);
            let waited = self.shared.changed.wait_timeout(state, remaining);
            #[cfg(test)]
            self.shared.about_to_wait.store(false, Ordering::SeqCst);
            state = match waited {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Run one owner-thread turn, then wait on the generation sampled before that turn.
    ///
    /// Keeping these operations together pins the lost-wake invariant used by controlled
    /// settlement: work committed during the turn must change the generation before the
    /// condition-variable predicate is inspected.
    pub fn owner_turn_then_wait<E>(
        &self,
        observed: EventLoopWakeGeneration,
        deadline: Instant,
        owner_turn: impl FnOnce() -> Result<(), E>,
    ) -> Result<EventLoopWakeWaitOutcome, E> {
        owner_turn()?;
        Ok(self.wait_for_generation(observed, deadline))
    }

    fn lock_state(&self) -> MutexGuard<'_, EventLoopWakeState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn advance_generation(
        &self,
        next: fn(EventLoopWakeGeneration) -> Option<EventLoopWakeGeneration>,
    ) {
        let mut state = self.lock_state();
        match next(state.generation) {
            Some(generation) => state.generation = generation,
            None => state.generation_exhausted = true,
        }
        self.shared.changed.notify_all();
    }
}

impl EventLoopWaker for PliegoEventLoopWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.advance_generation(EventLoopWakeGeneration::checked_event_loop_next);
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use crossbeam_channel::{TryRecvError, unbounded};
    use embedder_traits::ScriptToEmbedderChan;

    use super::*;

    fn wait_until_waiter_holds_predicate_lock(waker: &PliegoEventLoopWaker) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !waker.shared.about_to_wait.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "test waiter did not reach the condition-variable boundary"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn wake_at_the_wait_boundary_is_not_lost() {
        let waker = PliegoEventLoopWaker::new();
        let observed = waker.generation();
        let waiter_waker = waker.clone();
        let waiter = thread::spawn(move || {
            waiter_waker.wait_for_generation(observed, Instant::now() + Duration::from_secs(1))
        });

        wait_until_waiter_holds_predicate_lock(&waker);
        waker.wake();
        let current = waker.generation();
        assert_eq!(
            waiter.join().expect("test waiter should not panic"),
            EventLoopWakeWaitOutcome::Woken(current)
        );
    }

    #[test]
    fn wake_before_wait_is_observed_even_after_the_deadline() {
        let waker = PliegoEventLoopWaker::new();
        let observed = waker.generation();
        let servo_waker: Box<dyn EventLoopWaker> = Box::new(waker.clone());
        servo_waker.wake();

        assert_eq!(
            waker.wait_for_generation(observed, Instant::now()),
            EventLoopWakeWaitOutcome::Woken(waker.generation())
        );
    }

    #[test]
    fn control_response_wakes_without_forging_servo_work() {
        let waker = PliegoEventLoopWaker::new();
        let observed = waker.generation();

        waker.notify_control_response();

        let current = waker.generation();
        assert_eq!(
            waker.wait_for_generation(observed, Instant::now()),
            EventLoopWakeWaitOutcome::Woken(current)
        );
        assert!(!current.event_loop_changed_since(observed));
    }

    #[test]
    fn owner_turn_commit_after_an_older_wake_cannot_be_lost() {
        let waker = PliegoEventLoopWaker::new();
        let (embedder_sender, embedder_receiver) = unbounded();
        let script_sender = ScriptToEmbedderChan::new(embedder_sender, Box::new(waker.clone()));

        waker.wake();
        let observed = waker.generation();
        let outcome = waker
            .owner_turn_then_wait(observed, Instant::now(), || {
                script_sender.wake().unwrap();
                Ok::<(), ()>(())
            })
            .unwrap();

        assert_eq!(outcome, EventLoopWakeWaitOutcome::Woken(waker.generation()));
        assert!(matches!(
            embedder_receiver.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[test]
    fn spurious_notification_does_not_mask_timeout() {
        let waker = PliegoEventLoopWaker::new();
        let observed = waker.generation();
        let waiter_waker = waker.clone();
        let waiter = thread::spawn(move || {
            waiter_waker.wait_for_generation(observed, Instant::now() + Duration::from_millis(100))
        });

        wait_until_waiter_holds_predicate_lock(&waker);
        // Acquiring the predicate lock after the wait-boundary marker proves the waiter has
        // atomically released it into `Condvar::wait_timeout`. This notification deliberately does
        // not change the guarded generation.
        let state = waker.lock_state();
        waker.shared.changed.notify_all();
        drop(state);

        assert_eq!(
            waiter.join().expect("test waiter should not panic"),
            EventLoopWakeWaitOutcome::DeadlineReached(observed)
        );
    }

    #[test]
    fn generation_exhaustion_is_typed_instead_of_wrapping_or_sleeping() {
        let waker = PliegoEventLoopWaker::new();
        let exhausted_generation = EventLoopWakeGeneration {
            event_loop: u128::MAX,
            control_response: 0,
        };
        waker.lock_state().generation = exhausted_generation;

        waker.wake();

        assert_eq!(
            waker.wait_for_generation(exhausted_generation, Instant::now()),
            EventLoopWakeWaitOutcome::GenerationExhausted(exhausted_generation)
        );
    }
}
