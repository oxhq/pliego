/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Implementation of [microtasks](https://html.spec.whatwg.org/multipage/#microtask) and
//! microtask queues. It is up to implementations of event loops to store a queue and
//! perform checkpoints at appropriate times, as well as enqueue microtasks as required.

use std::cell::{Cell, RefCell};
use std::mem;
use std::rc::Rc;

use js::context::JSContext;
use js::realm::AutoRealm;
use js::rust::wrappers2::JobQueueMayNotBeEmpty;
use script_bindings::cell::DomRefCell;
use servo_base::id::PipelineId;
use timers::DocumentExecutionLedger;

use crate::dom::bindings::callback::ExceptionHandling;
use crate::dom::bindings::codegen::Bindings::PromiseBinding::PromiseJobCallback;
use crate::dom::bindings::codegen::Bindings::VoidFunctionBinding::VoidFunction;
use crate::dom::bindings::root::DomRoot;
use crate::dom::globalscope::GlobalScope;
use crate::dom::html::htmlimageelement::ImageElementMicrotask;
use crate::dom::html::htmlmediaelement::MediaElementMicrotask;
use crate::dom::html::htmltrackelement::TrackElementMicrotask;
use crate::dom::promise::WaitForAllSuccessStepsMicrotask;
use crate::dom::stream::byteteereadintorequest::ByteTeeReadIntoRequestMicrotask;
use crate::dom::stream::byteteereadrequest::ByteTeeReadRequestMicrotask;
use crate::dom::stream::defaultteereadrequest::DefaultTeeReadRequestMicrotask;
use crate::realms::enter_auto_realm;
use crate::script_runtime::notify_about_rejected_promises;
use crate::script_thread::ScriptThread;

/// Policy slot shared by the main SpiderMonkey job queue and every nested interrupt queue.
#[derive(Clone, Default)]
pub(crate) struct MicrotaskExecutionLedgerSlot(Rc<RefCell<Option<DocumentExecutionLedger>>>);

/// Outcome of attempting one HTML microtask checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub(crate) enum MicrotaskCheckpointResult {
    /// A surrounding checkpoint already owns this queue; no work or checkpoint completed.
    AlreadyPerforming,
    /// The queue drained and all end-of-checkpoint steps completed.
    Completed,
    /// Controlled execution became terminal and all remaining work was discarded.
    ExecutionTerminated,
}

/// A collection of microtasks in FIFO order.
#[derive(Default, JSTraceable, MallocSizeOf)]
pub(crate) struct MicrotaskQueue {
    /// The list of enqueued microtasks that will be invoked at the next microtask checkpoint.
    microtask_queue: DomRefCell<Vec<Microtask>>,
    /// <https://html.spec.whatwg.org/multipage/#performing-a-microtask-checkpoint>
    performing_a_microtask_checkpoint: Cell<bool>,
    /// Controlled-session accounting installed before the first navigation.
    #[no_trace]
    #[ignore_malloc_size_of = "The execution ledger is shared with the document clock"]
    execution_ledger: MicrotaskExecutionLedgerSlot,
}

#[derive(JSTraceable, MallocSizeOf)]
pub(crate) enum Microtask {
    Promise(EnqueuedPromiseCallback),
    User(UserMicrotask),
    MediaElement(MediaElementMicrotask),
    ImageElement(ImageElementMicrotask),
    TrackElement(TrackElementMicrotask),
    ReadableStreamTeeReadRequest(DefaultTeeReadRequestMicrotask),
    WaitForAllSuccessSteps(WaitForAllSuccessStepsMicrotask),
    ReadableStreamByteTeeReadRequest(ByteTeeReadRequestMicrotask),
    ReadableStreamByteTeeReadIntoRequest(ByteTeeReadIntoRequestMicrotask),
    CustomElementReaction,
    NotifyMutationObservers,
}

pub(crate) trait MicrotaskRunnable {
    fn handler(&self, _cx: &mut JSContext) {}
    fn enter_realm<'cx>(&self, cx: &'cx mut JSContext) -> AutoRealm<'cx>;
}

/// A promise callback scheduled to run during the next microtask checkpoint (#4283).
#[derive(JSTraceable, MallocSizeOf)]
pub(crate) struct EnqueuedPromiseCallback {
    #[conditional_malloc_size_of]
    pub(crate) callback: Rc<PromiseJobCallback>,
    #[no_trace]
    pub(crate) pipeline: PipelineId,
    pub(crate) is_user_interacting: bool,
}

/// A microtask that comes from a queueMicrotask() Javascript call,
/// identical to EnqueuedPromiseCallback once it's on the queue
#[derive(JSTraceable, MallocSizeOf)]
pub(crate) struct UserMicrotask {
    #[conditional_malloc_size_of]
    pub(crate) callback: Rc<VoidFunction>,
    #[no_trace]
    pub(crate) pipeline: PipelineId,
}

impl MicrotaskQueue {
    /// Construct an empty SpiderMonkey interrupt queue bound to the main queue's policy slot.
    pub(crate) fn with_execution_ledger_slot(
        execution_ledger: MicrotaskExecutionLedgerSlot,
    ) -> Self {
        Self {
            microtask_queue: Default::default(),
            performing_a_microtask_checkpoint: Default::default(),
            execution_ledger,
        }
    }

    /// Clone the policy slot used to construct nested SpiderMonkey interrupt queues.
    pub(crate) fn execution_ledger_slot(&self) -> MicrotaskExecutionLedgerSlot {
        self.execution_ledger.clone()
    }

    /// Install the execution ledger before any page microtask can be queued.
    pub(crate) fn install_execution_ledger(&self, ledger: Option<DocumentExecutionLedger>) {
        debug_assert!(self.microtask_queue.borrow().is_empty());
        debug_assert!(!self.performing_a_microtask_checkpoint.get());
        *self.execution_ledger.0.borrow_mut() = ledger;
    }

    /// Add a new microtask to this queue. It will be invoked as part of the next
    /// microtask checkpoint.
    #[expect(unsafe_code)]
    pub(crate) fn enqueue(&self, cx: &JSContext, job: Microtask) {
        self.microtask_queue.borrow_mut().push(job);
        unsafe { JobQueueMayNotBeEmpty(cx) };
    }

    /// <https://html.spec.whatwg.org/multipage/#perform-a-microtask-checkpoint>
    /// Perform a microtask checkpoint, executing all queued microtasks until the queue is empty.
    #[expect(unsafe_code)]
    pub(crate) fn checkpoint<F>(
        &self,
        cx: &mut JSContext,
        target_provider: F,
        globalscopes: Vec<DomRoot<GlobalScope>>,
    ) -> MicrotaskCheckpointResult
    where
        F: Fn(PipelineId) -> Option<DomRoot<GlobalScope>>,
    {
        // Steps 1-2. Enter only when no surrounding checkpoint already owns this queue.
        if let Err(result) = self.begin_checkpoint() {
            return result;
        }

        debug!("Now performing a microtask checkpoint");

        // Step 3. While the event loop's microtask queue is not empty:
        while !self.microtask_queue.borrow().is_empty() {
            rooted_vec!(let mut pending_queue);
            mem::swap(&mut *pending_queue, &mut *self.microtask_queue.borrow_mut());

            for (idx, job) in pending_queue.iter().enumerate() {
                // Controlled mode counts every individual job before invoking it. A sticky
                // failure stops this checkpoint even when the preceding job requeued itself;
                // the terminal session never resumes or publishes the discarded queue suffix.
                if !self.begin_microtask_job() {
                    return self.abort_terminal_checkpoint(|| unsafe {
                        js::rust::wrappers2::JobQueueIsEmpty(cx)
                    });
                }
                if idx == pending_queue.len() - 1 && self.microtask_queue.borrow().is_empty() {
                    unsafe { js::rust::wrappers2::JobQueueIsEmpty(cx) };
                }

                match *job {
                    Microtask::Promise(ref job) => {
                        if let Some(target) = target_provider(job.pipeline) {
                            let _guard = ScriptThread::user_interacting_guard();
                            let mut realm = enter_auto_realm(cx, &*target);
                            let cx = &mut realm;
                            let _ = job.callback.Call_(cx, &*target, ExceptionHandling::Report);
                        }
                    },
                    Microtask::User(ref job) => {
                        if let Some(target) = target_provider(job.pipeline) {
                            let mut realm = enter_auto_realm(cx, &*target);
                            let cx = &mut realm;
                            let _ = job.callback.Call_(cx, &*target, ExceptionHandling::Report);
                        }
                    },
                    Microtask::MediaElement(ref task) => {
                        let mut realm = task.enter_realm(cx);
                        let cx = &mut realm;
                        task.handler(cx);
                    },
                    Microtask::ImageElement(ref task) => {
                        let mut realm = task.enter_realm(cx);
                        let cx = &mut realm;
                        task.handler(cx);
                    },
                    Microtask::TrackElement(ref task) => {
                        let mut realm = task.enter_realm(cx);
                        let cx = &mut realm;
                        task.handler(cx);
                    },
                    Microtask::ReadableStreamTeeReadRequest(ref task) => {
                        let mut realm = task.enter_realm(cx);
                        let cx = &mut realm;
                        task.handler(cx);
                    },
                    Microtask::WaitForAllSuccessSteps(ref task) => {
                        let mut realm = task.enter_realm(cx);
                        let cx = &mut realm;
                        task.handler(cx);
                    },
                    Microtask::CustomElementReaction => {
                        ScriptThread::invoke_backup_element_queue(cx);
                    },
                    Microtask::NotifyMutationObservers => {
                        ScriptThread::mutation_observers().notify_mutation_observers(cx);
                    },
                    Microtask::ReadableStreamByteTeeReadRequest(ref task) => {
                        task.microtask_chunk_steps(cx)
                    },
                    Microtask::ReadableStreamByteTeeReadIntoRequest(ref task) => {
                        task.microtask_chunk_steps(cx)
                    },
                }

                // Central mutation-record accounting is non-rejecting and can therefore latch
                // during a job. Stop before invoking another queued job; individual DOM call sites
                // decide whether their underlying write precedes or follows the record hook.
                if self.execution_is_terminal() {
                    return self.abort_terminal_checkpoint(|| unsafe {
                        js::rust::wrappers2::JobQueueIsEmpty(cx)
                    });
                }
            }
        }

        // Step 4. For each environment settings object settingsObject whose responsible
        // event loop is this event loop, notify about rejected promises given
        // settingsObject's global object.
        for global in globalscopes.clone().into_iter() {
            notify_about_rejected_promises(cx, &global);
        }

        // https://html.spec.whatwg.org/multipage/#perform-a-microtask-checkpoint
        // Step 5. Cleanup Indexed Database transactions.
        // https://w3c.github.io/IndexedDB/#cleanup-indexed-database-transactions
        // “These steps are invoked by [HTML]. They ensure that transactions created by a script call
        // to transaction() are deactivated once the task that invoked the script has completed.”
        for global in globalscopes.iter() {
            let _ = global.get_indexeddb(cx).cleanup_indexeddb_transactions(cx);
        }

        // TODO: Step 6. Perform ClearKeptObjects().

        // Step 7. Set the event loop's performing a microtask checkpoint to false.
        self.performing_a_microtask_checkpoint.set(false);
        // TODO: Step 8. Record timing info for microtask checkpoint.
        MicrotaskCheckpointResult::Completed
    }

    fn begin_checkpoint(&self) -> Result<(), MicrotaskCheckpointResult> {
        if self.performing_a_microtask_checkpoint.get() {
            return Err(MicrotaskCheckpointResult::AlreadyPerforming);
        }
        self.performing_a_microtask_checkpoint.set(true);
        Ok(())
    }

    fn abort_terminal_checkpoint(
        &self,
        notify_job_queue_empty: impl FnOnce(),
    ) -> MicrotaskCheckpointResult {
        // Jobs moved into the local pending queue are dropped by the caller's early return. Jobs
        // requeued by an already-run job remain in this active queue, so clear them explicitly.
        // This same method runs for main and SpiderMonkey interrupt queues.
        self.microtask_queue.borrow_mut().clear();
        notify_job_queue_empty();
        self.performing_a_microtask_checkpoint.set(false);
        MicrotaskCheckpointResult::ExecutionTerminated
    }

    fn begin_microtask_job(&self) -> bool {
        self.execution_ledger
            .0
            .borrow()
            .as_ref()
            .is_none_or(|ledger| ledger.begin_microtask().is_ok())
    }

    fn execution_is_terminal(&self) -> bool {
        self.execution_ledger
            .0
            .borrow()
            .as_ref()
            .is_some_and(|ledger| ledger.observation().terminal.is_some())
    }

    pub(crate) fn empty(&self) -> bool {
        self.microtask_queue.borrow().is_empty()
    }

    pub(crate) fn clear(&self) {
        self.microtask_queue.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;

    use timers::{DocumentExecutionBudget, DocumentExecutionLimits, DocumentExecutionTerminal};

    use super::{Microtask, MicrotaskCheckpointResult, MicrotaskQueue};

    fn execution_limits(microtasks: u64) -> DocumentExecutionLimits {
        DocumentExecutionLimits {
            ordinary_tasks: 1,
            microtasks,
            rendering_opportunities: 1,
            mutations: 1,
            virtual_span: None,
        }
    }

    #[test]
    fn self_rescheduling_microtask_is_cut_off_inside_one_checkpoint() {
        let queue = MicrotaskQueue::default();
        let ledger = timers::DocumentExecutionLedger::new(execution_limits(3));
        queue.install_execution_ledger(Some(ledger.clone()));

        let mut pending = VecDeque::from([()]);
        let mut invoked = 0;
        while pending.pop_front().is_some() {
            if !queue.begin_microtask_job() {
                break;
            }
            invoked += 1;
            pending.push_back(());
        }

        assert_eq!(invoked, 3);
        assert_eq!(ledger.observation().counters.microtasks, 3);
        assert!(matches!(
            ledger.observation().terminal,
            Some(DocumentExecutionTerminal::BudgetExceeded {
                budget: DocumentExecutionBudget::Microtasks,
                limit: 3,
                observed: 4,
            })
        ));
    }

    #[test]
    fn interrupt_queue_created_before_install_shares_the_exact_ledger() {
        let main_queue = MicrotaskQueue::default();
        let interrupt_queue =
            MicrotaskQueue::with_execution_ledger_slot(main_queue.execution_ledger_slot());
        let ledger = timers::DocumentExecutionLedger::new(execution_limits(2));
        main_queue.install_execution_ledger(Some(ledger.clone()));

        assert!(interrupt_queue.begin_microtask_job());
        assert!(main_queue.begin_microtask_job());
        assert!(!interrupt_queue.begin_microtask_job());
        assert_eq!(ledger.observation().counters.microtasks, 2);
        assert!(matches!(
            ledger.observation().terminal,
            Some(DocumentExecutionTerminal::BudgetExceeded {
                budget: DocumentExecutionBudget::Microtasks,
                limit: 2,
                observed: 3,
            })
        ));
    }

    #[test]
    fn reentrant_checkpoint_reports_already_performing() {
        let queue = MicrotaskQueue::default();
        assert_eq!(queue.begin_checkpoint(), Ok(()));
        assert_eq!(
            queue.begin_checkpoint(),
            Err(MicrotaskCheckpointResult::AlreadyPerforming)
        );
    }

    #[test]
    fn terminal_abort_discards_work_requeued_by_the_last_admitted_job() {
        let queue = MicrotaskQueue::default();
        let ledger = timers::DocumentExecutionLedger::new(DocumentExecutionLimits {
            mutations: 0,
            ..execution_limits(1)
        });
        queue.install_execution_ledger(Some(ledger.clone()));
        queue.performing_a_microtask_checkpoint.set(true);

        // Model the last admitted job requeueing work before a non-rejecting mutation hook latches
        // the terminal. The pending suffix is local to checkpoint(); this is the active suffix that
        // previously survived its early return.
        assert!(queue.begin_microtask_job());
        queue
            .microtask_queue
            .borrow_mut()
            .push(Microtask::CustomElementReaction);
        ledger.record_mutation_record();
        assert!(queue.execution_is_terminal());

        let empty_notifications = Cell::new(0);
        assert_eq!(
            queue.abort_terminal_checkpoint(|| {
                empty_notifications.set(empty_notifications.get() + 1)
            }),
            MicrotaskCheckpointResult::ExecutionTerminated
        );
        assert!(queue.empty());
        assert!(!queue.performing_a_microtask_checkpoint.get());
        assert_eq!(empty_notifications.get(), 1);
    }
}
