/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Machinery for [task-queue](https://html.spec.whatwg.org/multipage/#task-queue).

use std::cell::Cell;
use std::collections::VecDeque;
use std::default::Default;

use crossbeam_channel::{self, Receiver, Sender};
use rustc_hash::{FxHashMap, FxHashSet};
use script_bindings::cell::DomRefCell;
use servo_base::id::PipelineId;
use strum::VariantArray;

use crate::dom::worker::TrustedWorkerAddress;
use crate::script_runtime::ScriptThreadEventCategory;
use crate::task::TaskBox;
use crate::task_source::TaskSourceName;

#[derive(MallocSizeOf)]
pub(crate) struct QueuedTask {
    pub(crate) worker: Option<TrustedWorkerAddress>,
    pub(crate) event_category: ScriptThreadEventCategory,
    #[ignore_malloc_size_of = "TaskBox is difficult"]
    pub(crate) task: Box<dyn TaskBox>,
    pub(crate) pipeline_id: Option<PipelineId>,
    pub(crate) task_source: TaskSourceName,
}

/// Defining the operations used to convert from a msg T to a QueuedTask.
pub(crate) trait QueuedTaskConversion {
    fn task_source_name(&self) -> Option<&TaskSourceName>;
    fn pipeline_id(&self) -> Option<PipelineId>;
    fn into_queued_task(self) -> Option<QueuedTask>;
    fn from_queued_task(queued_task: QueuedTask) -> Self;
    fn inactive_msg() -> Self;
    fn wake_up_msg() -> Self;
    fn is_wake_up(&self) -> bool;
}

#[derive(MallocSizeOf)]
pub(crate) struct TaskQueue<T> {
    /// The original port on which the task-sources send tasks as messages.
    port: Receiver<T>,
    /// A sender to ensure the port doesn't block on select while there are throttled tasks.
    wake_up_sender: Sender<T>,
    /// A queue from which the event-loop can drain tasks.
    msg_queue: DomRefCell<VecDeque<T>>,
    /// A "business" counter, reset for each iteration of the event-loop
    taken_task_counter: Cell<u64>,
    /// Tasks that will be throttled for as long as we are "busy".
    throttled: DomRefCell<FxHashMap<TaskSourceName, VecDeque<QueuedTask>>>,
    /// Tasks for not fully-active documents.
    inactive: DomRefCell<FxHashMap<PipelineId, VecDeque<QueuedTask>>>,
}

impl<T: QueuedTaskConversion> TaskQueue<T> {
    pub(crate) fn new(port: Receiver<T>, wake_up_sender: Sender<T>) -> TaskQueue<T> {
        TaskQueue {
            port,
            wake_up_sender,
            msg_queue: DomRefCell::new(VecDeque::new()),
            taken_task_counter: Default::default(),
            throttled: Default::default(),
            inactive: Default::default(),
        }
    }

    /// Release previously held-back tasks for documents that are now fully-active.
    /// <https://html.spec.whatwg.org/multipage/#event-loop-processing-model:fully-active>
    fn release_tasks_for_fully_active_documents(
        &self,
        fully_active: &FxHashSet<PipelineId>,
    ) -> Vec<T> {
        self.inactive
            .borrow_mut()
            .iter_mut()
            .filter(|(pipeline_id, _)| fully_active.contains(pipeline_id))
            .flat_map(|(_, inactive_queue)| {
                inactive_queue
                    .drain(0..)
                    .map(|queued_task| T::from_queued_task(queued_task))
            })
            .collect()
    }

    /// Hold back tasks for currently not fully-active documents.
    /// <https://html.spec.whatwg.org/multipage/#event-loop-processing-model:fully-active>
    fn store_task_for_inactive_pipeline(&self, msg: T, pipeline_id: &PipelineId) {
        let mut inactive = self.inactive.borrow_mut();
        let inactive_queue = inactive.entry(*pipeline_id).or_default();
        inactive_queue.push_back(
            msg.into_queued_task()
                .expect("Incoming messages should always be convertible into queued tasks"),
        );
        let mut msg_queue = self.msg_queue.borrow_mut();
        if msg_queue.is_empty() {
            // Ensure there is at least one message.
            // Otherwise if the just stored inactive message
            // was the first and last of this iteration,
            // it will result in a spurious wake-up of the event-loop.
            msg_queue.push_back(T::inactive_msg());
        }
    }

    /// Process incoming tasks, immediately sending priority ones downstream,
    /// and categorizing potential throttles.
    fn process_incoming_tasks(
        &self,
        first_msg: T,
        fully_active: &FxHashSet<PipelineId>,
        drain_ready_port: bool,
    ) {
        // 1. Make any previously stored task from now fully-active document available.
        let mut incoming = self.release_tasks_for_fully_active_documents(fully_active);

        // 2. Process the first message(artifact of the fact that select always returns a message).
        if !first_msg.is_wake_up() {
            incoming.push(first_msg);
        }

        // 3. Process any other incoming message.
        if drain_ready_port {
            while let Ok(msg) = self.port.try_recv() {
                if !msg.is_wake_up() {
                    incoming.push(msg);
                }
            }
        }

        // 4. Filter tasks from non-priority task-sources.
        // TODO: This can use `extract_if` once that is stabilized.
        let mut to_be_throttled = Vec::new();
        let mut index = 0;
        while index != incoming.len() {
            index += 1; // By default we go to the next index of the vector.

            let task_source = match incoming[index - 1].task_source_name() {
                Some(task_source) => task_source,
                None => continue,
            };

            match task_source {
                TaskSourceName::PerformanceTimeline => {
                    to_be_throttled.push(incoming.remove(index - 1));
                    index -= 1; // We've removed an element, so the next has the same index.
                },
                _ => {
                    // A task that will not be throttled, start counting "business"
                    self.taken_task_counter
                        .set(self.taken_task_counter.get() + 1);
                },
            }
        }

        for msg in incoming {
            // Always run "update the rendering" tasks,
            // TODO: fix "fully active" concept for iframes.
            if let Some(TaskSourceName::Rendering) = msg.task_source_name() {
                self.msg_queue.borrow_mut().push_back(msg);
                continue;
            }
            if let Some(pipeline_id) = msg.pipeline_id() &&
                !fully_active.contains(&pipeline_id)
            {
                self.store_task_for_inactive_pipeline(msg, &pipeline_id);
                continue;
            }
            // Immediately send non-throttled tasks for processing.
            self.msg_queue.borrow_mut().push_back(msg);
        }

        for msg in to_be_throttled {
            // Categorize tasks per task queue.
            let Some(queued_task) = msg.into_queued_task() else {
                unreachable!(
                    "A message to be throttled should always be convertible into a queued task"
                );
            };
            let mut throttled_tasks = self.throttled.borrow_mut();
            throttled_tasks
                .entry(queued_task.task_source)
                .or_default()
                .push_back(queued_task);
        }
    }

    /// Reset the queue for a new iteration of the event-loop,
    /// returning the port about whose readiness we want to be notified.
    pub(crate) fn select(&self) -> &crossbeam_channel::Receiver<T> {
        // This is a new iteration of the event-loop, so we reset the "business" counter.
        self.start_event_loop_iteration();
        // We want to be notified when the script-port is ready to receive.
        // Hence that's the one we need to include in the select.
        &self.port
    }

    /// Reset per-iteration task throttling before a controlled event-loop turn.
    pub(crate) fn start_event_loop_iteration(&self) {
        self.taken_task_counter.set(0);
    }

    /// Take a message from the front of the queue, without waiting if empty.
    pub(crate) fn recv(&self) -> Result<T, ()> {
        self.msg_queue.borrow_mut().pop_front().ok_or(())
    }

    /// Take all tasks again and then run `recv()`.
    pub(crate) fn take_tasks_and_recv(
        &self,
        fully_active: &FxHashSet<PipelineId>,
    ) -> Result<T, ()> {
        self.take_tasks(T::wake_up_msg(), fully_active);
        self.recv()
    }

    /// Take at most one newly received task and then run `recv()`.
    ///
    /// Controlled document time uses this path so a continuously-ready producer cannot make one
    /// driver command drain an unbounded channel batch. Ordinary event-loop intake remains
    /// unchanged in [`Self::take_tasks_and_recv`].
    pub(crate) fn take_one_task_and_recv(
        &self,
        fully_active: &FxHashSet<PipelineId>,
    ) -> Result<T, ()> {
        if let Ok(message) = self.recv() {
            return Ok(message);
        }
        if let Ok(first_msg) = self.port.try_recv() {
            return Ok(self.take_controlled_task_and_recv(first_msg, fully_active));
        }

        // With no producer-port input to order first, promote retained work whose document became
        // active and release throttles eligible in this iteration.
        self.take_one_task(T::wake_up_msg(), fully_active);
        if let Ok(message) = self.recv() {
            return Ok(message);
        }

        // The synthetic promotion may have generated a throttle wake-up. Consume that progress so
        // it is represented by the controlled batch rather than reported as empty.
        let first_msg = self.port.try_recv().map_err(|_| ())?;
        Ok(self.take_controlled_task_and_recv(first_msg, fully_active))
    }

    /// Process one selected controlled task-port input without letting stale wake-ups overtake a
    /// ready ordinary task.
    pub(crate) fn take_controlled_task_and_recv(
        &self,
        mut first_msg: T,
        fully_active: &FxHashSet<PipelineId>,
    ) -> T {
        const WAKE_SCAN_LIMIT: usize = 64;

        for wake_index in 0..WAKE_SCAN_LIMIT {
            if !first_msg.is_wake_up() {
                // Keep eligible throttles retained until the producer port is empty. Otherwise a
                // throttle released while handling this item could overtake the next ordinary
                // item that is already ready on the port.
                self.take_one_incoming_task(first_msg, fully_active);
                return self.recv().unwrap_or_else(|_| T::wake_up_msg());
            }
            if wake_index + 1 == WAKE_SCAN_LIMIT {
                // Leave any successor on the port once this command has consumed its bounded
                // wake budget. In particular, never fetch and then drop a ready ordinary task.
                return T::wake_up_msg();
            }
            let Ok(next_msg) = self.port.try_recv() else {
                // With no ready ordinary input to order first, promote newly-active retained work
                // and eligible throttles. A wake that produces no task remains one visible no-op.
                self.take_one_task(T::wake_up_msg(), fully_active);
                return self.recv().unwrap_or_else(|_| T::wake_up_msg());
            };
            first_msg = next_msg;
        }

        // A bounded run of stale wakes remains visible and cannot monopolize one command.
        T::wake_up_msg()
    }

    /// Drain the queue for the current iteration of the event-loop.
    /// Holding-back throttles above a given high-water mark.
    pub(crate) fn take_tasks(&self, first_msg: T, fully_active: &FxHashSet<PipelineId>) {
        self.take_tasks_with_options(first_msg, fully_active, true, true);
    }

    /// Make one already-received task available without draining the ready producer port.
    pub(crate) fn take_one_task(&self, first_msg: T, fully_active: &FxHashSet<PipelineId>) {
        self.take_tasks_with_options(first_msg, fully_active, false, true);
    }

    /// Categorize one ready producer-port item while keeping eligible throttles retained.
    fn take_one_incoming_task(&self, first_msg: T, fully_active: &FxHashSet<PipelineId>) {
        self.take_tasks_with_options(first_msg, fully_active, false, false);
    }

    fn take_tasks_with_options(
        &self,
        first_msg: T,
        fully_active: &FxHashSet<PipelineId>,
        drain_ready_port: bool,
        release_throttles: bool,
    ) {
        // High-watermark: once reached, throttled tasks will be held-back.
        const PER_ITERATION_MAX: u64 = 5;
        // Always first check for new tasks, but don't reset 'taken_task_counter'.
        self.process_incoming_tasks(first_msg, fully_active, drain_ready_port);
        if !release_throttles {
            return;
        }
        let mut throttled = self.throttled.borrow_mut();
        let mut throttled_length: usize = throttled.values().map(|queue| queue.len()).sum();
        let mut task_source_cycler = TaskSourceName::VARIANTS.iter().cycle();
        // "being busy", is defined as having more than x tasks for this loop's iteration.
        // As long as we're not busy, and there are throttled tasks left:
        loop {
            let max_reached = self.taken_task_counter.get() > PER_ITERATION_MAX;
            let none_left = throttled_length == 0;
            match (max_reached, none_left) {
                (_, true) => break,
                (true, false) => {
                    // We have reached the high-watermark for this iteration of the event-loop,
                    // yet also have throttled messages left in the queue.
                    // Ensure the select wakes up in the next iteration of the event-loop
                    let _ = self.wake_up_sender.send(T::wake_up_msg());
                    break;
                },
                (false, false) => {
                    // Cycle through non-priority task sources, taking one throttled task from each.
                    let task_source = task_source_cycler.next().unwrap();
                    let throttled_queue = match throttled.get_mut(task_source) {
                        Some(queue) => queue,
                        None => continue,
                    };
                    let queued_task = match throttled_queue.pop_front() {
                        Some(queued_task) => queued_task,
                        None => continue,
                    };
                    let msg = T::from_queued_task(queued_task);

                    // Hold back tasks for currently inactive documents.
                    if let Some(pipeline_id) = msg.pipeline_id() &&
                        !fully_active.contains(&pipeline_id)
                    {
                        self.store_task_for_inactive_pipeline(msg, &pipeline_id);
                        // Reduce the length of throttles,
                        // but don't add the task to "msg_queue",
                        // and neither increment "taken_task_counter".
                        throttled_length -= 1;
                        continue;
                    }

                    // Make the task available for the event-loop to handle as a message.
                    self.msg_queue.borrow_mut().push_back(msg);
                    self.taken_task_counter
                        .set(self.taken_task_counter.get() + 1);
                    throttled_length -= 1;
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use servo_base::id::TEST_PIPELINE_ID;
    use style::thread_state::{self, ThreadState};

    use super::*;

    struct ScriptThreadStateGuard(bool);

    impl ScriptThreadStateGuard {
        fn enter() -> Self {
            let entered = !thread_state::get().is_script();
            if entered {
                thread_state::enter(ThreadState::SCRIPT);
            }
            Self(entered)
        }
    }

    impl Drop for ScriptThreadStateGuard {
        fn drop(&mut self) {
            if self.0 {
                thread_state::exit(ThreadState::SCRIPT);
            }
        }
    }

    struct NeverRunTask;

    impl TaskBox for NeverRunTask {
        fn name(&self) -> &'static str {
            "NeverRunTask"
        }

        fn run_box(self: Box<Self>, _: &mut js::context::JSContext) {
            panic!("task-queue tests never execute tasks")
        }
    }

    enum TestMessage {
        Task {
            source: TaskSourceName,
            pipeline_id: Option<PipelineId>,
        },
        Inactive,
        WakeUp,
    }

    impl TestMessage {
        fn task(source: TaskSourceName) -> Self {
            Self::Task {
                source,
                pipeline_id: None,
            }
        }
    }

    impl QueuedTaskConversion for TestMessage {
        fn task_source_name(&self) -> Option<&TaskSourceName> {
            match self {
                Self::Task { source, .. } => Some(source),
                Self::Inactive | Self::WakeUp => None,
            }
        }

        fn pipeline_id(&self) -> Option<PipelineId> {
            match self {
                Self::Task { pipeline_id, .. } => *pipeline_id,
                Self::Inactive | Self::WakeUp => None,
            }
        }

        fn into_queued_task(self) -> Option<QueuedTask> {
            let Self::Task {
                source,
                pipeline_id,
            } = self
            else {
                return None;
            };
            Some(QueuedTask {
                worker: None,
                event_category: source.into(),
                task: Box::new(NeverRunTask),
                pipeline_id,
                task_source: source,
            })
        }

        fn from_queued_task(queued_task: QueuedTask) -> Self {
            Self::Task {
                source: queued_task.task_source,
                pipeline_id: queued_task.pipeline_id,
            }
        }

        fn inactive_msg() -> Self {
            Self::Inactive
        }

        fn wake_up_msg() -> Self {
            Self::WakeUp
        }

        fn is_wake_up(&self) -> bool {
            matches!(self, Self::WakeUp)
        }
    }

    #[test]
    fn controlled_poll_promotes_newly_active_retained_tasks_without_fresh_input() {
        let _script_thread_state = ScriptThreadStateGuard::enter();
        let (sender, receiver) = crossbeam_channel::unbounded();
        let queue = TaskQueue::new(receiver, sender.clone());
        let mut fully_active = FxHashSet::default();
        sender
            .send(TestMessage::Task {
                source: TaskSourceName::Timer,
                pipeline_id: Some(TEST_PIPELINE_ID),
            })
            .unwrap();

        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Inactive)
        ));

        fully_active.insert(TEST_PIPELINE_ID);
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Task {
                source: TaskSourceName::Timer,
                pipeline_id: Some(TEST_PIPELINE_ID)
            })
        ));
    }

    #[test]
    fn controlled_poll_skips_duplicate_throttle_wakeups_without_false_empty() {
        let _script_thread_state = ScriptThreadStateGuard::enter();
        let (sender, receiver) = crossbeam_channel::unbounded();
        let queue = TaskQueue::new(receiver, sender.clone());
        let fully_active = FxHashSet::default();
        queue.start_event_loop_iteration();

        for _ in 0..6 {
            sender
                .send(TestMessage::task(TaskSourceName::Timer))
                .unwrap();
            assert!(matches!(
                queue.take_one_task_and_recv(&fully_active),
                Ok(TestMessage::Task {
                    source: TaskSourceName::Timer,
                    ..
                })
            ));
        }

        sender
            .send(TestMessage::task(TaskSourceName::PerformanceTimeline))
            .unwrap();
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::WakeUp)
        ));
        sender
            .send(TestMessage::task(TaskSourceName::Timer))
            .unwrap();
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Task {
                source: TaskSourceName::Timer,
                ..
            })
        ));
        sender.send(TestMessage::WakeUp).unwrap();
        sender.send(TestMessage::WakeUp).unwrap();

        queue.start_event_loop_iteration();
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Task {
                source: TaskSourceName::PerformanceTimeline,
                ..
            })
        ));

        assert!(queue.take_one_task_and_recv(&fully_active).is_err());
    }

    #[test]
    fn controlled_poll_keeps_ready_ordinary_tasks_ahead_of_released_throttles() {
        let _script_thread_state = ScriptThreadStateGuard::enter();
        let (sender, receiver) = crossbeam_channel::unbounded();
        let queue = TaskQueue::new(receiver, sender.clone());
        let fully_active = FxHashSet::default();
        queue.start_event_loop_iteration();

        for _ in 0..6 {
            sender
                .send(TestMessage::task(TaskSourceName::Timer))
                .unwrap();
            assert!(matches!(
                queue.take_one_task_and_recv(&fully_active),
                Ok(TestMessage::Task {
                    source: TaskSourceName::Timer,
                    ..
                })
            ));
        }
        sender
            .send(TestMessage::task(TaskSourceName::PerformanceTimeline))
            .unwrap();
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::WakeUp)
        ));

        queue.start_event_loop_iteration();
        sender.send(TestMessage::WakeUp).unwrap();
        sender
            .send(TestMessage::task(TaskSourceName::Timer))
            .unwrap();
        sender
            .send(TestMessage::task(TaskSourceName::Timer))
            .unwrap();
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Task {
                source: TaskSourceName::Timer,
                ..
            })
        ));
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Task {
                source: TaskSourceName::Timer,
                ..
            })
        ));
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Task {
                source: TaskSourceName::PerformanceTimeline,
                ..
            })
        ));
    }

    #[test]
    fn controlled_poll_does_not_drop_task_after_exact_wake_scan_limit() {
        let _script_thread_state = ScriptThreadStateGuard::enter();
        let (sender, receiver) = crossbeam_channel::unbounded();
        let queue = TaskQueue::new(receiver, sender.clone());
        let fully_active = FxHashSet::default();

        for _ in 0..64 {
            sender.send(TestMessage::WakeUp).unwrap();
        }
        sender
            .send(TestMessage::task(TaskSourceName::Timer))
            .unwrap();

        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::WakeUp)
        ));
        assert!(matches!(
            queue.take_one_task_and_recv(&fully_active),
            Ok(TestMessage::Task {
                source: TaskSourceName::Timer,
                ..
            })
        ));
    }
}
