/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A generic timer scheduler module that can be integrated into a crossbeam based event
//! loop or used to launch a background timer thread.

#![deny(unsafe_code)]

use std::cmp::{self, Ord};
use std::collections::BinaryHeap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, after, never};
use malloc_size_of_derive::MallocSizeOf;
use serde::{Deserialize, Serialize};

/// A callback to pass to the [`TimerScheduler`] to be called when the timer is
/// dispatched.
pub type BoxedTimerCallback = Box<dyn Fn() + Send + 'static>;

/// Requests a TimerEvent-Message be sent after the given duration.
#[derive(MallocSizeOf)]
pub struct TimerEventRequest {
    #[ignore_malloc_size_of = "Size of a boxed function"]
    pub callback: BoxedTimerCallback,
    pub duration: Duration,
}

impl TimerEventRequest {
    fn dispatch(self) {
        (self.callback)()
    }
}

/// Configuration used when constructing a document-observable monotonic clock.
///
/// Interactive Servo uses [`Self::Realtime`]. The controlled mode does not advance by itself and
/// never creates a host-clock wake-up; its owner must advance and activate deadlines explicitly.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentClockConfiguration {
    /// Use elapsed host monotonic time.
    #[default]
    Realtime,
    /// Start a controlled clock at the supplied integer nanosecond offset.
    Controlled {
        /// Initial monotonic time in this document-clock domain.
        initial_time_ns: u64,
        /// Unix time, in nanoseconds, corresponding to [`DocumentTime::ZERO`].
        ///
        /// This is deliberately configuration rather than host time so JavaScript wall time and
        /// monotonic DOM time advance together without consulting the host clock.
        #[serde(default)]
        unix_time_origin_ns: u64,
    },
}

/// An integer nanosecond offset in one document clock domain.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct DocumentTime(u64);

impl DocumentTime {
    /// The zero offset.
    pub const ZERO: Self = Self(0);

    /// Construct an offset from an integer number of nanoseconds.
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Return this offset as integer nanoseconds.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Convert a duration without truncating or wrapping it.
    pub fn checked_from_duration(duration: Duration) -> Result<Self, DocumentClockError> {
        u64::try_from(duration.as_nanos())
            .map(Self)
            .map_err(|_| DocumentClockError::Overflow)
    }

    /// Add a duration without truncating or wrapping it.
    pub fn checked_add(self, duration: Duration) -> Result<Self, DocumentClockError> {
        let duration = Self::checked_from_duration(duration)?;
        self.0
            .checked_add(duration.0)
            .map(Self)
            .ok_or(DocumentClockError::Overflow)
    }

    /// Subtract a duration without wrapping it.
    pub fn checked_sub(self, duration: Duration) -> Result<Self, DocumentClockError> {
        let duration = Self::checked_from_duration(duration)?;
        self.0
            .checked_sub(duration.0)
            .map(Self)
            .ok_or(DocumentClockError::Overflow)
    }

    /// Return the non-negative duration since an earlier offset.
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// Return the duration since an earlier offset without hiding a mismatched or future origin.
    pub fn checked_duration_since(self, earlier: Self) -> Result<Duration, DocumentClockError> {
        self.0
            .checked_sub(earlier.0)
            .map(Duration::from_nanos)
            .ok_or(DocumentClockError::TimeMovedBackwards {
                current: earlier,
                requested: self,
            })
    }
}

/// One immutable timestamp captured for an "update the rendering" invocation.
///
/// The token prevents animation timelines and animation-frame callbacks from independently
/// sampling the clock while one rendering update is in progress.
#[derive(Clone, Copy, Debug, Eq, MallocSizeOf, PartialEq)]
pub struct DocumentRenderingTime(DocumentTime);

impl DocumentRenderingTime {
    /// Return the timestamp in the underlying document-clock domain.
    pub const fn document_time(self) -> DocumentTime {
        self.0
    }
}

/// Document-observable surfaces that eventually need to share one controlled clock.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentTimeSurface {
    /// Window timers on one script event loop.
    WindowTimers,
    /// A same-event-loop nested browsing context.
    SameEventLoopIframe,
    /// JavaScript `Date` in a Window realm.
    JavaScriptDate,
    /// `performance.now()` and `performance.timeOrigin` in a Window realm.
    Performance,
    /// A DOM high-resolution timestamp supplied by a host or another process.
    HostTimestamp,
    /// The timestamp captured once for an "update the rendering" invocation.
    UpdateRendering,
    /// Window `requestAnimationFrame` timestamps and scheduling.
    AnimationFrame,
    /// CSS and Web Animations document timelines.
    DocumentTimeline,
    /// Dedicated, shared, or service workers.
    Worker,
    /// A worklet running on another event loop.
    Worklet,
    /// A nested browsing context hosted by another script event loop.
    CrossEventLoopIframe,
}

/// A checked document-clock failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentClockError {
    /// The requested operation requires a controlled clock.
    RealtimeClock,
    /// Controlled document time cannot move backwards.
    TimeMovedBackwards {
        /// The current clock offset.
        current: DocumentTime,
        /// The rejected new clock offset.
        requested: DocumentTime,
    },
    /// An integer nanosecond conversion or calculation overflowed.
    Overflow,
    /// The current clock slices do not yet control the requested observable surface.
    UnsupportedSurface(DocumentTimeSurface),
}

impl fmt::Display for DocumentClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RealtimeClock => formatter.write_str("document clock is using host time"),
            Self::TimeMovedBackwards { current, requested } => write!(
                formatter,
                "document time cannot move backwards from {}ns to {}ns",
                current.as_nanos(),
                requested.as_nanos()
            ),
            Self::Overflow => formatter.write_str("document time exceeds the u64 nanosecond range"),
            Self::UnsupportedSurface(surface) => {
                write!(
                    formatter,
                    "controlled document time does not yet support {surface:?}"
                )
            },
        }
    }
}

impl std::error::Error for DocumentClockError {}

enum DocumentClockInner {
    Realtime {
        origin: Instant,
    },
    Controlled {
        now_ns: AtomicU64,
        unix_time_origin_ns: u64,
    },
}

/// One clonable clock shared by the timer scheduler and every same-event-loop Window realm.
#[derive(Clone, MallocSizeOf)]
pub struct DocumentClock {
    #[ignore_malloc_size_of = "The clock state is shared and has no owned heap allocations"]
    inner: Arc<DocumentClockInner>,
}

impl Default for DocumentClock {
    fn default() -> Self {
        Self::new(DocumentClockConfiguration::Realtime)
    }
}

impl DocumentClock {
    /// Construct a clock from its immutable mode configuration.
    pub fn new(configuration: DocumentClockConfiguration) -> Self {
        let inner = match configuration {
            DocumentClockConfiguration::Realtime => DocumentClockInner::Realtime {
                origin: Instant::now(),
            },
            DocumentClockConfiguration::Controlled {
                initial_time_ns,
                unix_time_origin_ns,
            } => DocumentClockInner::Controlled {
                now_ns: AtomicU64::new(initial_time_ns),
                unix_time_origin_ns,
            },
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Return whether this clock is explicitly controlled rather than host-driven.
    pub fn is_controlled(&self) -> bool {
        matches!(&*self.inner, DocumentClockInner::Controlled { .. })
    }

    /// Return the current offset, checking the host-duration conversion.
    pub fn try_now(&self) -> Result<DocumentTime, DocumentClockError> {
        match &*self.inner {
            DocumentClockInner::Realtime { origin } => {
                DocumentTime::checked_from_duration(origin.elapsed())
            },
            DocumentClockInner::Controlled { now_ns, .. } => Ok(DocumentTime::from_nanos(
                now_ns.load(AtomicOrdering::Acquire),
            )),
        }
    }

    /// Return the current offset.
    ///
    /// A real monotonic clock would need to run for roughly 584 years to exceed this U1 range.
    pub fn now(&self) -> DocumentTime {
        self.try_now()
            .expect("document clock exceeded its checked integer nanosecond range")
    }

    /// Return the current offset after checking that the observable surface is controlled.
    pub fn now_for_surface(
        &self,
        surface: DocumentTimeSurface,
    ) -> Result<DocumentTime, DocumentClockError> {
        self.require_surface(surface)?;
        self.try_now()
    }

    /// Capture the one timestamp shared by all consumers in a rendering update.
    pub fn rendering_time(&self) -> Result<DocumentRenderingTime, DocumentClockError> {
        self.now_for_surface(DocumentTimeSurface::UpdateRendering)
            .map(DocumentRenderingTime)
    }

    /// Return a checked elapsed duration in the requested observable surface.
    pub fn duration_since_for_surface(
        &self,
        surface: DocumentTimeSurface,
        origin: DocumentTime,
        observed: DocumentTime,
    ) -> Result<Duration, DocumentClockError> {
        self.require_surface(surface)?;
        observed.checked_duration_since(origin)
    }

    /// Advance a controlled clock monotonically without sleeping.
    pub fn advance_to(&self, requested: DocumentTime) -> Result<(), DocumentClockError> {
        let DocumentClockInner::Controlled { now_ns, .. } = &*self.inner else {
            return Err(DocumentClockError::RealtimeClock);
        };

        let mut current = now_ns.load(AtomicOrdering::Acquire);
        loop {
            if requested.as_nanos() < current {
                return Err(DocumentClockError::TimeMovedBackwards {
                    current: DocumentTime::from_nanos(current),
                    requested,
                });
            }
            match now_ns.compare_exchange_weak(
                current,
                requested.as_nanos(),
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    /// Return Unix time at a point in a controlled clock domain, checking integer overflow.
    pub fn unix_time_ns_at(&self, time: DocumentTime) -> Result<u64, DocumentClockError> {
        let DocumentClockInner::Controlled {
            unix_time_origin_ns,
            ..
        } = &*self.inner
        else {
            return Err(DocumentClockError::RealtimeClock);
        };
        unix_time_origin_ns
            .checked_add(time.as_nanos())
            .ok_or(DocumentClockError::Overflow)
    }

    /// Return current Unix time in a controlled clock domain, checking integer overflow.
    pub fn unix_time_ns(&self) -> Result<u64, DocumentClockError> {
        self.unix_time_ns_at(self.try_now()?)
    }

    /// Check whether this slice controls an observable surface.
    ///
    /// Only surfaces routed in the current upstream slices are accepted. Later slices must remove
    /// the remaining typed blockers as they install the same clock in those subsystems.
    pub fn require_surface(&self, surface: DocumentTimeSurface) -> Result<(), DocumentClockError> {
        if !self.is_controlled() ||
            matches!(
                surface,
                DocumentTimeSurface::WindowTimers |
                    DocumentTimeSurface::SameEventLoopIframe |
                    DocumentTimeSurface::JavaScriptDate |
                    DocumentTimeSurface::Performance |
                    DocumentTimeSurface::UpdateRendering |
                    DocumentTimeSurface::AnimationFrame |
                    DocumentTimeSurface::DocumentTimeline
            )
        {
            Ok(())
        } else {
            Err(DocumentClockError::UnsupportedSurface(surface))
        }
    }

    fn realtime_deadline(
        &self,
        deadline: DocumentTime,
    ) -> Result<Option<Instant>, DocumentClockError> {
        let DocumentClockInner::Realtime { origin } = &*self.inner else {
            return Ok(None);
        };
        origin
            .checked_add(Duration::from_nanos(deadline.as_nanos()))
            .map(Some)
            .ok_or(DocumentClockError::Overflow)
    }
}

#[derive(MallocSizeOf)]
struct ScheduledEvent {
    id: TimerId,
    request: TimerEventRequest,
    deadline: DocumentTime,
}

impl Ord for ScheduledEvent {
    fn cmp(&self, other: &ScheduledEvent) -> cmp::Ordering {
        match self.deadline.cmp(&other.deadline).reverse() {
            cmp::Ordering::Equal => self.id.cmp(&other.id).reverse(),
            ordering => ordering,
        }
    }
}

impl PartialOrd for ScheduledEvent {
    fn partial_cmp(&self, other: &ScheduledEvent) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for ScheduledEvent {}
impl PartialEq for ScheduledEvent {
    fn eq(&self, other: &ScheduledEvent) -> bool {
        self.id == other.id
    }
}

/// A stable timer identity whose value is also its scheduler insertion order.
#[derive(Clone, Copy, Debug, Eq, MallocSizeOf, Ord, PartialEq, PartialOrd)]
pub struct TimerId(u64);

impl TimerId {
    /// Return the stable scheduler insertion sequence.
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

/// The finite deadline exposed by a controlled scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerDeadlineSnapshot {
    /// Stable identity and insertion order for this event.
    pub id: TimerId,
    /// Absolute integer-nanosecond offset in the document clock.
    pub deadline: DocumentTime,
}

/// A checked scheduler/control failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerControlError {
    /// The underlying document clock rejected an operation.
    Clock(DocumentClockError),
    /// Adding a request duration exceeded the document-time range.
    DeadlineOverflow,
    /// The stable timer insertion sequence was exhausted.
    SequenceExhausted,
    /// A finite-deadline operation was requested from a realtime scheduler.
    RealtimeScheduler,
    /// The caller tried to activate a snapshot that is no longer current.
    StaleDeadline {
        /// Snapshot supplied by the caller.
        expected: TimerDeadlineSnapshot,
        /// Current next snapshot, if any.
        observed: Option<TimerDeadlineSnapshot>,
    },
    /// The selected timer is still in the future.
    TimerNotDue {
        /// Selected timer deadline.
        deadline: DocumentTime,
        /// Current controlled time.
        now: DocumentTime,
    },
}

impl From<DocumentClockError> for TimerControlError {
    fn from(error: DocumentClockError) -> Self {
        Self::Clock(error)
    }
}

impl fmt::Display for TimerControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for TimerControlError {}

/// A queue of [`TimerEventRequest`]s that are stored in order of next-to-fire.
#[derive(MallocSizeOf)]
pub struct TimerScheduler {
    /// A priority queue of future events, sorted by due time and insertion sequence.
    queue: BinaryHeap<ScheduledEvent>,
    /// The next stable timer insertion sequence.
    next_id: u64,
    /// The same clock used by the DOM timer layer for this event loop.
    clock: DocumentClock,
}

impl Default for TimerScheduler {
    fn default() -> Self {
        Self::with_clock(DocumentClock::default())
    }
}

impl TimerScheduler {
    /// Create a scheduler driven by the supplied document clock.
    pub fn with_clock(clock: DocumentClock) -> Self {
        Self {
            queue: BinaryHeap::new(),
            next_id: 0,
            clock,
        }
    }

    /// Return the scheduler's shared document clock.
    pub fn clock(&self) -> DocumentClock {
        self.clock.clone()
    }

    /// Schedule a timer, returning a typed failure instead of truncating time or sequence values.
    pub fn try_schedule_timer(
        &mut self,
        request: TimerEventRequest,
    ) -> Result<TimerId, TimerControlError> {
        let deadline = self
            .clock
            .try_now()?
            .checked_add(request.duration)
            .map_err(|_| TimerControlError::DeadlineOverflow)?;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or(TimerControlError::SequenceExhausted)?;
        let id = TimerId(self.next_id);
        self.next_id = next_id;
        self.queue.push(ScheduledEvent {
            id,
            request,
            deadline,
        });
        Ok(id)
    }

    /// Schedule a timer for an interactive caller whose bounded durations are already validated.
    pub fn schedule_timer(&mut self, request: TimerEventRequest) -> TimerId {
        self.try_schedule_timer(request)
            .expect("validated timer request exceeded the checked scheduler range")
    }

    /// Cancel a timer with the given [`TimerId`]. If it is no longer pending, do nothing.
    pub fn cancel_timer(&mut self, id: TimerId) {
        self.queue.retain(|event| event.id != id);
    }

    /// Get a receiver that wakes for the next realtime timer.
    ///
    /// Controlled schedulers never wake from host time.
    pub fn wait_channel(&self) -> Receiver<Instant> {
        self.next_deadline()
            .map(|deadline| {
                let now = Instant::now();
                after(deadline.saturating_duration_since(now))
            })
            .unwrap_or_else(never)
    }

    /// The host deadline of the next realtime timer.
    ///
    /// Returns `None` for controlled schedulers.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.queue
            .peek()
            .and_then(|event| self.clock.realtime_deadline(event.deadline).ok().flatten())
    }

    /// Return the next finite controlled deadline.
    pub fn finite_deadline_snapshot(
        &self,
    ) -> Result<Option<TimerDeadlineSnapshot>, TimerControlError> {
        if !self.clock.is_controlled() {
            return Err(TimerControlError::RealtimeScheduler);
        }
        Ok(self.queue.peek().map(|event| TimerDeadlineSnapshot {
            id: event.id,
            deadline: event.deadline,
        }))
    }

    /// Advance the shared controlled clock monotonically without activating a timer.
    pub fn advance_controlled_time_to(&self, now: DocumentTime) -> Result<(), TimerControlError> {
        self.clock.advance_to(now).map_err(Into::into)
    }

    /// Activate exactly one due event selected from a fresh finite-deadline snapshot.
    pub fn activate_due_timer(
        &mut self,
        expected: TimerDeadlineSnapshot,
    ) -> Result<(), TimerControlError> {
        let observed = self.finite_deadline_snapshot()?;
        if observed != Some(expected) {
            return Err(TimerControlError::StaleDeadline { expected, observed });
        }
        let now = self.clock.try_now()?;
        if expected.deadline > now {
            return Err(TimerControlError::TimerNotDue {
                deadline: expected.deadline,
                now,
            });
        }
        self.queue
            .pop()
            .expect("a matching finite-deadline snapshot must still have an event")
            .request
            .dispatch();
        Ok(())
    }

    /// Dispatch all timers due on the host clock.
    ///
    /// This preserves Servo's interactive behavior. Controlled schedulers are activated only via
    /// [`Self::activate_due_timer`] so a host event-loop batch cannot coalesce virtual timers.
    pub fn dispatch_completed_timers(&mut self) {
        if self.clock.is_controlled() {
            return;
        }
        let Ok(now) = self.clock.try_now() else {
            return;
        };
        while matches!(self.queue.peek(), Some(event) if event.deadline <= now) {
            self.queue
                .pop()
                .expect("a due timer must still be queued")
                .request
                .dispatch();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn controlled_clock(initial_time_ns: u64) -> DocumentClock {
        DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns,
            unix_time_origin_ns: 0,
        })
    }

    fn recording_request(
        events: &Arc<Mutex<Vec<u8>>>,
        value: u8,
        duration: Duration,
    ) -> TimerEventRequest {
        let events = Arc::clone(events);
        TimerEventRequest {
            callback: Box::new(move || events.lock().unwrap().push(value)),
            duration,
        }
    }

    #[test]
    fn controlled_deadlines_activate_one_at_a_time_in_stable_creation_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let clock = controlled_clock(0);
        let mut scheduler = TimerScheduler::with_clock(clock);

        scheduler.schedule_timer(recording_request(&events, 1, Duration::from_nanos(10)));
        scheduler.schedule_timer(recording_request(&events, 2, Duration::from_nanos(10)));
        scheduler.schedule_timer(recording_request(&events, 0, Duration::from_nanos(5)));

        let first = scheduler.finite_deadline_snapshot().unwrap().unwrap();
        assert_eq!(first.deadline, DocumentTime::from_nanos(5));
        assert!(matches!(
            scheduler.activate_due_timer(first),
            Err(TimerControlError::TimerNotDue { .. })
        ));

        scheduler
            .advance_controlled_time_to(DocumentTime::from_nanos(5))
            .unwrap();
        scheduler.activate_due_timer(first).unwrap();
        assert_eq!(*events.lock().unwrap(), vec![0]);

        scheduler
            .advance_controlled_time_to(DocumentTime::from_nanos(10))
            .unwrap();
        let second = scheduler.finite_deadline_snapshot().unwrap().unwrap();
        scheduler.activate_due_timer(second).unwrap();
        assert_eq!(*events.lock().unwrap(), vec![0, 1]);
        let third = scheduler.finite_deadline_snapshot().unwrap().unwrap();
        scheduler.activate_due_timer(third).unwrap();
        assert_eq!(*events.lock().unwrap(), vec![0, 1, 2]);
        assert_eq!(scheduler.finite_deadline_snapshot().unwrap(), None);
    }

    #[test]
    fn cancellation_invalidates_a_deadline_snapshot_without_running_it() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let clock = controlled_clock(0);
        let mut scheduler = TimerScheduler::with_clock(clock);
        let id = scheduler.schedule_timer(recording_request(&events, 1, Duration::from_nanos(10)));
        let snapshot = scheduler.finite_deadline_snapshot().unwrap().unwrap();

        scheduler.cancel_timer(id);
        scheduler
            .advance_controlled_time_to(DocumentTime::from_nanos(10))
            .unwrap();
        assert!(matches!(
            scheduler.activate_due_timer(snapshot),
            Err(TimerControlError::StaleDeadline { observed: None, .. })
        ));
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn replacement_deadline_rejects_a_stale_cancelled_snapshot() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let clock = controlled_clock(0);
        let mut scheduler = TimerScheduler::with_clock(clock);
        let old_id =
            scheduler.schedule_timer(recording_request(&events, 1, Duration::from_nanos(10)));
        let stale = scheduler.finite_deadline_snapshot().unwrap().unwrap();

        scheduler.cancel_timer(old_id);
        scheduler.schedule_timer(recording_request(&events, 2, Duration::from_nanos(5)));
        scheduler
            .advance_controlled_time_to(DocumentTime::from_nanos(10))
            .unwrap();

        let replacement = scheduler.finite_deadline_snapshot().unwrap().unwrap();
        assert!(matches!(
            scheduler.activate_due_timer(stale),
            Err(TimerControlError::StaleDeadline {
                observed: Some(observed),
                ..
            }) if observed == replacement
        ));
        assert!(events.lock().unwrap().is_empty());
        scheduler.activate_due_timer(replacement).unwrap();
        assert_eq!(*events.lock().unwrap(), vec![2]);
    }

    #[test]
    fn controlled_scheduler_never_uses_the_realtime_dispatch_path() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let clock = controlled_clock(0);
        let mut scheduler = TimerScheduler::with_clock(clock);
        scheduler.schedule_timer(recording_request(&events, 1, Duration::ZERO));

        scheduler.dispatch_completed_timers();

        assert!(events.lock().unwrap().is_empty());
        let snapshot = scheduler.finite_deadline_snapshot().unwrap().unwrap();
        scheduler.activate_due_timer(snapshot).unwrap();
        assert_eq!(*events.lock().unwrap(), vec![1]);
    }

    #[test]
    fn realtime_scheduler_keeps_the_existing_host_dispatch_path() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut scheduler = TimerScheduler::default();
        scheduler.schedule_timer(recording_request(&events, 1, Duration::ZERO));

        assert_eq!(
            scheduler.finite_deadline_snapshot(),
            Err(TimerControlError::RealtimeScheduler)
        );
        scheduler.dispatch_completed_timers();
        assert_eq!(*events.lock().unwrap(), vec![1]);
    }

    #[test]
    fn controlled_time_is_monotonic_and_deadline_math_is_checked() {
        assert_eq!(
            DocumentTime::checked_from_duration(Duration::MAX),
            Err(DocumentClockError::Overflow)
        );

        let clock = controlled_clock(u64::MAX - 1);
        assert_eq!(
            clock.advance_to(DocumentTime::from_nanos(u64::MAX - 2)),
            Err(DocumentClockError::TimeMovedBackwards {
                current: DocumentTime::from_nanos(u64::MAX - 1),
                requested: DocumentTime::from_nanos(u64::MAX - 2),
            })
        );

        let mut scheduler = TimerScheduler::with_clock(clock);
        let events = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            scheduler
                .try_schedule_timer(recording_request(&events, 1, Duration::from_nanos(2)))
                .unwrap_err(),
            TimerControlError::DeadlineOverflow
        );
    }

    #[test]
    fn controlled_clock_exposes_the_current_surface_boundary() {
        let clock = controlled_clock(0);
        for surface in [
            DocumentTimeSurface::WindowTimers,
            DocumentTimeSurface::SameEventLoopIframe,
            DocumentTimeSurface::JavaScriptDate,
            DocumentTimeSurface::Performance,
            DocumentTimeSurface::UpdateRendering,
            DocumentTimeSurface::AnimationFrame,
            DocumentTimeSurface::DocumentTimeline,
        ] {
            assert_eq!(clock.require_surface(surface), Ok(()));
        }
        for surface in [
            DocumentTimeSurface::HostTimestamp,
            DocumentTimeSurface::Worker,
            DocumentTimeSurface::Worklet,
            DocumentTimeSurface::CrossEventLoopIframe,
        ] {
            assert_eq!(
                clock.require_surface(surface),
                Err(DocumentClockError::UnsupportedSurface(surface))
            );
        }
    }

    #[test]
    fn one_rendering_snapshot_drives_raf_and_timeline_time() {
        let clock = controlled_clock(5_000_000);
        let navigation_origin = clock.now();
        clock
            .advance_to(DocumentTime::from_nanos(12_000_000))
            .unwrap();

        let frame_time = clock.rendering_time().unwrap();
        let raf_time = clock
            .duration_since_for_surface(
                DocumentTimeSurface::AnimationFrame,
                navigation_origin,
                frame_time.document_time(),
            )
            .unwrap();
        let timeline_time = clock
            .duration_since_for_surface(
                DocumentTimeSurface::DocumentTimeline,
                navigation_origin,
                frame_time.document_time(),
            )
            .unwrap();

        assert_eq!(raf_time, Duration::from_millis(7));
        assert_eq!(timeline_time, raf_time);

        clock
            .advance_to(DocumentTime::from_nanos(20_000_000))
            .unwrap();
        assert_eq!(
            frame_time.document_time(),
            DocumentTime::from_nanos(12_000_000)
        );
        assert_eq!(
            clock.rendering_time().unwrap().document_time(),
            DocumentTime::from_nanos(20_000_000)
        );
    }

    #[test]
    fn realtime_rendering_surfaces_keep_the_monotonic_host_clock_path() {
        let clock = DocumentClock::default();
        let navigation_origin = clock
            .now_for_surface(DocumentTimeSurface::DocumentTimeline)
            .unwrap();
        let frame_time = clock.rendering_time().unwrap();

        let raf_time = clock
            .duration_since_for_surface(
                DocumentTimeSurface::AnimationFrame,
                navigation_origin,
                frame_time.document_time(),
            )
            .unwrap();
        let timeline_time = clock
            .duration_since_for_surface(
                DocumentTimeSurface::DocumentTimeline,
                navigation_origin,
                frame_time.document_time(),
            )
            .unwrap();

        assert_eq!(timeline_time, raf_time);
    }

    #[test]
    fn controlled_wall_and_monotonic_time_advance_in_exact_order() {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 7_000_000,
            unix_time_origin_ns: 1_700_000_000_000_000_000,
        });
        let navigation_origin = clock.now();

        assert_eq!(
            clock.unix_time_ns_at(navigation_origin),
            Ok(1_700_000_000_007_000_000)
        );
        assert_eq!(clock.unix_time_ns(), Ok(1_700_000_000_007_000_000));

        clock
            .advance_to(DocumentTime::from_nanos(12_000_000))
            .unwrap();
        assert_eq!(
            clock
                .now()
                .checked_duration_since(navigation_origin)
                .unwrap(),
            Duration::from_millis(5)
        );
        assert_eq!(clock.unix_time_ns(), Ok(1_700_000_000_012_000_000));
    }

    #[test]
    fn controlled_wall_time_overflow_is_checked_without_corrupting_monotonic_time() {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 1,
            unix_time_origin_ns: u64::MAX,
        });

        assert_eq!(clock.unix_time_ns(), Err(DocumentClockError::Overflow));
        assert_eq!(clock.now(), DocumentTime::from_nanos(1));
        assert_eq!(
            DocumentTime::from_nanos(1).checked_duration_since(DocumentTime::from_nanos(2)),
            Err(DocumentClockError::TimeMovedBackwards {
                current: DocumentTime::from_nanos(2),
                requested: DocumentTime::from_nanos(1),
            })
        );
    }

    #[test]
    fn realtime_clock_rejects_controlled_wall_time_mapping() {
        let clock = DocumentClock::default();
        assert_eq!(clock.unix_time_ns(), Err(DocumentClockError::RealtimeClock));
        assert_eq!(
            clock.unix_time_ns_at(DocumentTime::ZERO),
            Err(DocumentClockError::RealtimeClock)
        );
    }
}
