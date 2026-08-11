/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::cmp::{Ord, Ordering};
use std::collections::VecDeque;
use std::default::Default;
use std::rc::Rc;
use std::time::Duration;

use deny_public_fields::DenyPublicFields;
use js::context::JSContext;
use js::jsapi::Heap;
use js::jsval::{JSVal, UndefinedValue};
use js::rust::wrappers2::JS_GetScriptedCallerPrivate;
use js::rust::{HandleValue, IntoHandle};
use net_traits::request::ParserMetadata;
use rustc_hash::FxHashMap;
use script_bindings::cell::DomRefCell;
use script_bindings::reflector::DomObject;
use serde::{Deserialize, Serialize};
use servo_base::id::PipelineId;
use servo_config::pref;
use servo_url::ServoUrl;
use timers::{
    BoxedTimerCallback, DocumentClock, DocumentClockError, DocumentTime, TimerEventRequest, TimerId,
};

use crate::dom::bindings::callback::ExceptionHandling::Report;
use crate::dom::bindings::codegen::Bindings::FunctionBinding::Function;
use crate::dom::bindings::codegen::UnionTypes::TrustedScriptOrString;
use crate::dom::bindings::error::Fallible;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::refcounted::Trusted;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{AsHandleValue, Dom};
use crate::dom::bindings::str::DOMString;
use crate::dom::csp::CspReporting;
use crate::dom::document::RefreshRedirectDue;
use crate::dom::eventsource::EventSourceTimeoutCallback;
use crate::dom::globalscope::GlobalScope;
use crate::dom::globalscope::script_execution::{ErrorReporting, RethrowErrors};
#[cfg(feature = "testbinding")]
use crate::dom::testbinding::TestBindingCallback;
use crate::dom::trustedtypes::trustedscript::TrustedScript;
use crate::dom::types::{Window, WorkerGlobalScope};
use crate::dom::xmlhttprequest::XHRTimeoutCallback;
use crate::script_module::{ScriptFetchOptions, module_script_from_reference_private};
use crate::script_runtime::IntroductionType;
use crate::script_thread::ScriptThread;
use crate::task_source::SendableTaskSource;

type TimerKey = i32;
type RunStepsDeadline = DocumentTime;
type CompletionStep = Box<dyn FnOnce(&mut JSContext, &GlobalScope) + 'static>;

/// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
/// OrderingIdentifier per spec ("orderingIdentifier")
type OrderingIdentifier = DOMString;

#[derive(JSTraceable, MallocSizeOf)]
struct OrderingEntry {
    milliseconds: u64,
    start_seq: u64,
    handle: OneshotTimerHandle,
}

// Per-ordering queues map
type OrderingQueues = FxHashMap<OrderingIdentifier, Vec<OrderingEntry>>;

// Active timers map for Run Steps After A Timeout
type RunStepsActiveMap = FxHashMap<TimerKey, RunStepsDeadline>;

#[derive(Clone, Copy, Debug, Eq, Hash, JSTraceable, MallocSizeOf, Ord, PartialEq, PartialOrd)]
pub(crate) struct OneshotTimerHandle(i32);

#[derive(DenyPublicFields, JSTraceable, MallocSizeOf)]
#[cfg_attr(crown, crown::unrooted_must_root_lint::must_root)]
pub(crate) struct OneshotTimers {
    global_scope: Dom<GlobalScope>,
    js_timers: JsTimers,
    next_timer_handle: Cell<OneshotTimerHandle>,
    timers: DomRefCell<VecDeque<OneshotTimer>>,
    #[no_trace]
    document_clock: DocumentClock,
    #[no_trace]
    timebase: Cell<TimerTimebase>,
    /// Calls to `fire_timer` with a different argument than this get ignored.
    /// They were previously scheduled and got invalidated when
    ///  - timers were suspended,
    ///  - the timer it was scheduled for got canceled or
    ///  - a timer was added with an earlier callback time. In this case the
    ///    original timer is rescheduled when it is the next one to get called.
    #[no_trace]
    expected_event_id: Cell<TimerEventId>,
    /// The outer scheduler event for the current earliest DOM timer.
    #[no_trace]
    scheduled_timer_id: Cell<Option<TimerId>>,
    /// <https://html.spec.whatwg.org/multipage/#map-of-active-timers>
    /// TODO this should also be used for the other timers
    /// as per <html.spec.whatwg.org/multipage/#map-of-settimeout-and-setinterval-ids>Z.
    #[no_trace]
    map_of_active_timers: DomRefCell<RunStepsActiveMap>,

    /// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
    /// Step 4.2 Wait until any invocations of this algorithm that had the same global and orderingIdentifier,
    /// that started before this one, and whose milliseconds is less than or equal to this one's, have completed.
    runsteps_queues: DomRefCell<OrderingQueues>,

    /// <html.spec.whatwg.org/multipage/#timers:unique-internal-value-5>
    next_runsteps_key: Cell<TimerKey>,

    /// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
    /// Start order sequence to break ties for Step 4.2.
    runsteps_start_seq: Cell<u64>,

    /// Stable creation order for timers that share a deadline.
    creation_sequence: Cell<u64>,
}

#[derive(DenyPublicFields, JSTraceable, MallocSizeOf)]
struct OneshotTimer {
    handle: OneshotTimerHandle,
    #[no_trace]
    source: TimerSource,
    callback: OneshotTimerCallback,
    #[no_trace]
    scheduled_for: DocumentTime,
    creation_sequence: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, MallocSizeOf, PartialEq)]
struct TimerTimebase {
    suspended_at: Option<DocumentTime>,
    suspension_offset: Duration,
}

impl TimerTimebase {
    fn now(self, clock: &DocumentClock) -> Result<DocumentTime, DocumentClockError> {
        self.suspended_at
            .unwrap_or(clock.try_now()?)
            .checked_sub(self.suspension_offset)
    }

    fn suspend(&mut self, clock: &DocumentClock) -> Result<bool, DocumentClockError> {
        if self.suspended_at.is_some() {
            return Ok(false);
        }
        self.suspended_at = Some(clock.try_now()?);
        Ok(true)
    }

    fn resume(&mut self, clock: &DocumentClock) -> Result<bool, DocumentClockError> {
        let Some(suspended_at) = self.suspended_at else {
            return Ok(false);
        };
        let paused_for = clock.try_now()?.saturating_duration_since(suspended_at);
        self.suspension_offset = self
            .suspension_offset
            .checked_add(paused_for)
            .ok_or(DocumentClockError::Overflow)?;
        self.suspended_at = None;
        Ok(true)
    }
}

// This enum is required to work around the fact that trait objects do not support generic methods.
// A replacement trait would have a method such as
//     `invoke<T: DomObject>(self: Box<Self>, this: &T, js_timers: &JsTimers);`.
#[derive(JSTraceable, MallocSizeOf)]
pub(crate) enum OneshotTimerCallback {
    XhrTimeout(XHRTimeoutCallback),
    EventSourceTimeout(EventSourceTimeoutCallback),
    JsTimer(JsTimerTask),
    #[cfg(feature = "testbinding")]
    TestBindingCallback(TestBindingCallback),
    RefreshRedirectDue(RefreshRedirectDue),
    /// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
    RunStepsAfterTimeout {
        /// Step 1. timerKey
        timer_key: i32,
        /// Step 4. orderingIdentifier
        ordering_id: DOMString,
        /// Spec: milliseconds (the algorithm input)
        milliseconds: u64,
        /// Perform completionSteps.
        #[no_trace]
        #[ignore_malloc_size_of = "Closure"]
        completion: CompletionStep,
    },
}

impl OneshotTimerCallback {
    fn invoke<T: DomObject>(self, this: &T, js_timers: &JsTimers, cx: &mut JSContext) {
        match self {
            OneshotTimerCallback::XhrTimeout(callback) => callback.invoke(cx),
            OneshotTimerCallback::EventSourceTimeout(callback) => callback.invoke(),
            OneshotTimerCallback::JsTimer(task) => task.invoke(this, js_timers, cx),
            #[cfg(feature = "testbinding")]
            OneshotTimerCallback::TestBindingCallback(callback) => callback.invoke(cx),
            OneshotTimerCallback::RefreshRedirectDue(callback) => callback.invoke(cx),
            OneshotTimerCallback::RunStepsAfterTimeout { completion, .. } => {
                // <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
                // Step 4.4 Perform completionSteps.
                completion(cx, &this.global());
            },
        }
    }
}

impl Ord for OneshotTimer {
    fn cmp(&self, other: &OneshotTimer) -> Ordering {
        compare_timer_order(
            self.scheduled_for,
            self.creation_sequence,
            other.scheduled_for,
            other.creation_sequence,
        )
    }
}

fn compare_timer_order(
    left_deadline: DocumentTime,
    left_sequence: u64,
    right_deadline: DocumentTime,
    right_sequence: u64,
) -> Ordering {
    match left_deadline.cmp(&right_deadline).reverse() {
        Ordering::Equal => left_sequence.cmp(&right_sequence).reverse(),
        ordering => ordering,
    }
}

impl PartialOrd for OneshotTimer {
    fn partial_cmp(&self, other: &OneshotTimer) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for OneshotTimer {}
impl PartialEq for OneshotTimer {
    fn eq(&self, other: &OneshotTimer) -> bool {
        std::ptr::eq(self, other)
    }
}

impl OneshotTimers {
    pub(crate) fn new(global_scope: &GlobalScope) -> OneshotTimers {
        OneshotTimers {
            global_scope: Dom::from_ref(global_scope),
            document_clock: global_scope.document_clock(),
            js_timers: JsTimers::default(),
            next_timer_handle: Cell::new(OneshotTimerHandle(1)),
            timers: DomRefCell::new(VecDeque::new()),
            timebase: Cell::new(TimerTimebase::default()),
            expected_event_id: Cell::new(TimerEventId(0)),
            scheduled_timer_id: Cell::new(None),
            map_of_active_timers: Default::default(),
            runsteps_queues: Default::default(),
            next_runsteps_key: Cell::new(1),
            runsteps_start_seq: Cell::new(0),
            creation_sequence: Cell::new(0),
        }
    }

    /// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
    #[inline]
    pub(crate) fn now_for_runsteps(&self) -> DocumentTime {
        // Step 2. Let startTime be the current high resolution time given global.
        self.base_time()
    }

    /// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
    /// Step 1. Let timerKey be a new unique internal value.
    pub(crate) fn fresh_runsteps_key(&self) -> TimerKey {
        let k = self.next_runsteps_key.get();
        self.next_runsteps_key.set(k + 1);
        k
    }

    /// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
    /// Step 3. Set global's map of active timers[timerKey] to startTime plus milliseconds.
    pub(crate) fn runsteps_set_active(&self, timer_key: TimerKey, deadline: RunStepsDeadline) {
        self.map_of_active_timers
            .borrow_mut()
            .insert(timer_key, deadline);
    }

    /// <https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout>
    /// Helper for Step 4.2: maintain per-ordering sorted queue by (milliseconds, startSeq, handle).
    fn runsteps_enqueue_sorted(
        &self,
        ordering_id: &DOMString,
        handle: OneshotTimerHandle,
        milliseconds: u64,
    ) {
        let mut map = self.runsteps_queues.borrow_mut();
        let q = map.entry(ordering_id.clone()).or_default();

        let seq = {
            let cur = self.runsteps_start_seq.get();
            self.runsteps_start_seq.set(cur + 1);
            cur
        };

        let key = OrderingEntry {
            milliseconds,
            start_seq: seq,
            handle,
        };

        let idx = q
            .binary_search_by(|ordering_entry| {
                match ordering_entry.milliseconds.cmp(&milliseconds) {
                    Ordering::Less => Ordering::Less,
                    Ordering::Greater => Ordering::Greater,
                    Ordering::Equal => ordering_entry.start_seq.cmp(&seq),
                }
            })
            .unwrap_or_else(|i| i);

        q.insert(idx, key);
    }

    pub(crate) fn schedule_callback(
        &self,
        callback: OneshotTimerCallback,
        duration: Duration,
        source: TimerSource,
    ) -> OneshotTimerHandle {
        let new_handle = self.next_timer_handle.get();
        self.next_timer_handle.set(OneshotTimerHandle(
            new_handle
                .0
                .checked_add(1)
                .expect("DOM timer handle sequence exhausted"),
        ));
        let creation_sequence = self.creation_sequence.get();
        self.creation_sequence.set(
            creation_sequence
                .checked_add(1)
                .expect("DOM timer creation sequence exhausted"),
        );

        let timer = OneshotTimer {
            handle: new_handle,
            source,
            callback,
            scheduled_for: self
                .base_time()
                .checked_add(duration)
                .expect("validated DOM timer duration exceeded the document clock"),
            creation_sequence,
        };

        // https://html.spec.whatwg.org/multipage/#run-steps-after-a-timeout
        // Step 4.2: maintain per-orderingIdentifier order by milliseconds (and start order for ties).
        if let OneshotTimerCallback::RunStepsAfterTimeout {
            ordering_id,
            milliseconds,
            ..
        } = &timer.callback
        {
            self.runsteps_enqueue_sorted(ordering_id, new_handle, *milliseconds);
        }

        {
            let mut timers = self.timers.borrow_mut();
            let insertion_index = timers.binary_search(&timer).err().unwrap();
            timers.insert(insertion_index, timer);
        }

        if self.is_next_timer(new_handle) {
            self.schedule_timer_call();
        }

        new_handle
    }

    pub(crate) fn unschedule_callback(&self, handle: OneshotTimerHandle) {
        let was_next = self.is_next_timer(handle);

        self.timers.borrow_mut().retain(|t| t.handle != handle);

        if was_next {
            self.invalidate_expected_event_id();
            self.schedule_timer_call();
        }
    }

    fn is_next_timer(&self, handle: OneshotTimerHandle) -> bool {
        match self.timers.borrow().back() {
            None => false,
            Some(max_timer) => max_timer.handle == handle,
        }
    }

    /// <https://html.spec.whatwg.org/multipage/#timer-initialisation-steps>
    pub(crate) fn fire_timer(&self, id: TimerEventId, global: &GlobalScope, cx: &mut JSContext) {
        // Step 9.2. If id does not exist in global's map of setTimeout and setInterval IDs, then abort these steps.
        let expected_id = self.expected_event_id.get();
        if expected_id != id {
            debug!(
                "ignoring timer fire event {:?} (expected {:?})",
                id, expected_id
            );
            return;
        }

        assert!(self.timebase.get().suspended_at.is_none());

        let base_time = self.base_time();

        // Since the event id was the expected one, at least one timer should be due.
        if base_time < self.timers.borrow().back().unwrap().scheduled_for {
            warn!("Unexpected timing!");
            return;
        }

        // Pop exactly one timer. Each due DOM timer must become its own event-loop task so the
        // existing task path performs the required microtask checkpoint before another timer.
        let timer = {
            let mut timers = self.timers.borrow_mut();
            timers
                .pop_back()
                .expect("an expected timer event must have one due DOM timer")
        };

        for timer in [timer] {
            // Since timers can be coalesced together inside a task,
            // this loop can keep running, including after an interrupt of the JS,
            // and prevent a clean-shutdown of a JS-running thread.
            // This check prevents such a situation.
            if !global.can_continue_running() {
                return;
            }
            match &timer.callback {
                // TODO: https://github.com/servo/servo/issues/40060
                OneshotTimerCallback::RunStepsAfterTimeout { ordering_id, .. } => {
                    // Step 4.2 Wait until any invocations of this algorithm that had the same global and orderingIdentifier,
                    // that started before this one, and whose milliseconds is less than or equal to this one's, have completed.
                    let head_handle_opt = {
                        let queues_ref = self.runsteps_queues.borrow();
                        queues_ref
                            .get(ordering_id)
                            .and_then(|v| v.first().map(|t| t.handle))
                    };
                    let is_head = head_handle_opt.is_none_or(|head| head == timer.handle);

                    if !is_head {
                        // TODO: this re queuing would go away when we revisit timers implementation.
                        let rein = OneshotTimer {
                            handle: timer.handle,
                            source: timer.source,
                            callback: timer.callback,
                            scheduled_for: self.base_time(),
                            creation_sequence: timer.creation_sequence,
                        };
                        let mut timers = self.timers.borrow_mut();
                        let idx = timers.binary_search(&rein).err().unwrap();
                        timers.insert(idx, rein);
                        continue;
                    }

                    let (timer_key, ordering_id_owned, completion) = match timer.callback {
                        OneshotTimerCallback::RunStepsAfterTimeout {
                            timer_key,
                            ordering_id,
                            milliseconds: _,
                            completion,
                        } => (timer_key, ordering_id, completion),
                        _ => unreachable!(),
                    };

                    // Step 4.3 Optionally, wait a further implementation-defined length of time.
                    // (No additional delay applied.)

                    // Step 4.4 Perform completionSteps.
                    (completion)(cx, global);

                    // Step 4.5 Remove global's map of active timers[timerKey].
                    self.map_of_active_timers.borrow_mut().remove(&timer_key);

                    {
                        let mut queues_mut = self.runsteps_queues.borrow_mut();
                        if let Some(q) = queues_mut.get_mut(&ordering_id_owned) {
                            if !q.is_empty() {
                                q.remove(0);
                            }
                            if q.is_empty() {
                                queues_mut.remove(&ordering_id_owned);
                            }
                        }
                    }
                },
                _ => {
                    let cb = timer.callback;
                    cb.invoke(global, &self.js_timers, cx);
                },
            }
        }

        self.schedule_timer_call();
    }

    fn base_time(&self) -> DocumentTime {
        self.timebase
            .get()
            .now(&self.document_clock)
            .expect("DOM timer suspension offset exceeded the document clock")
    }

    pub(crate) fn slow_down(&self) {
        let min_duration_ms = pref!(js_timers_minimum_duration) as u64;
        self.js_timers
            .set_min_duration(Duration::from_millis(min_duration_ms));
    }

    pub(crate) fn speed_up(&self) {
        self.js_timers.remove_min_duration();
    }

    pub(crate) fn suspend(&self) {
        // Suspend is idempotent: do nothing if the timers are already suspended.
        let mut timebase = self.timebase.get();
        if !timebase
            .suspend(&self.document_clock)
            .expect("document clock failed while suspending DOM timers")
        {
            return warn!("Suspending an already suspended timer.");
        }

        debug!("Suspending timers.");
        self.timebase.set(timebase);
        self.invalidate_expected_event_id();
    }

    pub(crate) fn resume(&self) {
        // Resume is idempotent: do nothing if the timers are already resumed.
        let mut timebase = self.timebase.get();
        if !timebase
            .resume(&self.document_clock)
            .expect("document clock failed while resuming DOM timers")
        {
            return warn!("Resuming an already resumed timer.");
        }

        debug!("Resuming timers.");
        self.timebase.set(timebase);

        self.schedule_timer_call();
    }

    /// <https://html.spec.whatwg.org/multipage/#timer-initialisation-steps>
    fn schedule_timer_call(&self) {
        if self.timebase.get().suspended_at.is_some() {
            // The timer will be scheduled when the pipeline is fully activated.
            return;
        }

        let timers = self.timers.borrow();
        let Some(timer) = timers.back() else {
            return;
        };

        let expected_event_id = self.invalidate_expected_event_id();
        // Step 12. Let completionStep be an algorithm step which queues a global
        // task on the timer task source given global to run task.
        let callback = TimerListener {
            context: Trusted::new(&*self.global_scope),
            task_source: self
                .global_scope
                .task_manager()
                .timer_task_source()
                .to_sendable(),
            source: timer.source,
            id: expected_event_id,
        }
        .into_callback();

        let event_request = TimerEventRequest {
            callback,
            duration: timer
                .scheduled_for
                .saturating_duration_since(self.base_time()),
        };

        self.scheduled_timer_id
            .set(self.global_scope.schedule_timer(event_request));
    }

    fn invalidate_expected_event_id(&self) -> TimerEventId {
        if let Some(timer_id) = self.scheduled_timer_id.take() {
            self.global_scope.cancel_timer(timer_id);
        }
        let TimerEventId(currently_expected) = self.expected_event_id.get();
        let next_id = TimerEventId(
            currently_expected
                .checked_add(1)
                .expect("DOM timer event sequence exhausted"),
        );
        debug!(
            "invalidating expected timer (was {:?}, now {:?}",
            currently_expected, next_id
        );
        self.expected_event_id.set(next_id);
        next_id
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_timeout_or_interval(
        &self,
        cx: &mut JSContext,
        global: &GlobalScope,
        callback: TimerCallback,
        arguments: Vec<HandleValue>,
        timeout: Duration,
        is_interval: IsInterval,
        source: TimerSource,
    ) -> Fallible<i32> {
        self.js_timers.set_timeout_or_interval(
            cx,
            global,
            callback,
            arguments,
            timeout,
            is_interval,
            source,
        )
    }

    pub(crate) fn clear_timeout_or_interval(&self, global: &GlobalScope, handle: i32) {
        self.js_timers.clear_timeout_or_interval(global, handle)
    }
}

#[derive(Clone, Copy, Eq, Hash, JSTraceable, MallocSizeOf, Ord, PartialEq, PartialOrd)]
pub(crate) struct JsTimerHandle(i32);

#[derive(DenyPublicFields, JSTraceable, MallocSizeOf)]
pub(crate) struct JsTimers {
    next_timer_handle: Cell<JsTimerHandle>,
    /// <https://html.spec.whatwg.org/multipage/#list-of-active-timers>
    active_timers: DomRefCell<FxHashMap<JsTimerHandle, JsTimerEntry>>,
    /// The nesting level of the currently executing timer task or 0.
    nesting_level: Cell<u32>,
    /// Used to introduce a minimum delay in event intervals
    min_duration: Cell<Option<Duration>>,
}

#[derive(JSTraceable, MallocSizeOf)]
struct JsTimerEntry {
    oneshot_handle: OneshotTimerHandle,
}

// Holder for the various JS values associated with setTimeout
// (ie. function value to invoke and all arguments to pass
//      to the function when calling it)
// TODO: Handle rooting during invocation when movable GC is turned on
#[derive(JSTraceable, MallocSizeOf)]
pub(crate) struct JsTimerTask {
    handle: JsTimerHandle,
    #[no_trace]
    source: TimerSource,
    callback: InternalTimerCallback,
    is_interval: IsInterval,
    nesting_level: u32,
    duration: Duration,
    is_user_interacting: bool,
}

// Enum allowing more descriptive values for the is_interval field
#[derive(Clone, Copy, JSTraceable, MallocSizeOf, PartialEq)]
pub(crate) enum IsInterval {
    Interval,
    NonInterval,
}

pub(crate) enum TimerCallback {
    StringTimerCallback(TrustedScriptOrString),
    FunctionTimerCallback(Rc<Function>),
}

#[derive(Clone, JSTraceable, MallocSizeOf)]
#[cfg_attr(crown, expect(crown::unrooted_must_root))]
enum InternalTimerCallback {
    StringTimerCallback(DOMString, InitiatingScriptFetchInfo),
    FunctionTimerCallback(
        #[conditional_malloc_size_of] Rc<Function>,
        #[ignore_malloc_size_of = "mozjs"] Rc<Box<[Heap<JSVal>]>>,
    ),
}

impl Default for JsTimers {
    fn default() -> Self {
        JsTimers {
            next_timer_handle: Cell::new(JsTimerHandle(1)),
            active_timers: DomRefCell::new(FxHashMap::default()),
            nesting_level: Cell::new(0),
            min_duration: Cell::new(None),
        }
    }
}

impl JsTimers {
    /// <https://html.spec.whatwg.org/multipage/#timer-initialisation-steps>
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(crown, expect(crown::unrooted_must_root))]
    pub(crate) fn set_timeout_or_interval(
        &self,
        cx: &mut JSContext,
        global: &GlobalScope,
        callback: TimerCallback,
        arguments: Vec<HandleValue>,
        timeout: Duration,
        is_interval: IsInterval,
        source: TimerSource,
    ) -> Fallible<i32> {
        let callback = match callback {
            TimerCallback::StringTimerCallback(trusted_script_or_string) => {
                // Step 9.6.1.1. Let globalName be "Window" if global is a Window object; "WorkerGlobalScope" otherwise.
                let global_name = if global.is::<Window>() {
                    "Window"
                } else {
                    "WorkerGlobalScope"
                };
                // Step 9.6.1.2. Let methodName be "setInterval" if repeat is true; "setTimeout" otherwise.
                let method_name = if is_interval == IsInterval::Interval {
                    "setInterval"
                } else {
                    "setTimeout"
                };
                // Step 9.6.1.3. Let sink be a concatenation of globalName, U+0020 SPACE, and methodName.
                let sink = format!("{} {}", global_name, method_name);
                // Step 9.6.1.4. Set handler to the result of invoking the
                // Get Trusted Type compliant string algorithm with TrustedScript, global, handler, sink, and "script".
                let code_str = TrustedScript::get_trusted_type_compliant_string(
                    cx,
                    global,
                    trusted_script_or_string,
                    &sink,
                )?;

                let initiating_script_fetch_info = active_script_fetch_info(cx, global);

                // Step 9.6.3. Perform EnsureCSPDoesNotBlockStringCompilation(realm, « », handler, handler, timer, « », handler).
                // If this throws an exception, catch it, report it for global, and abort these steps.
                if global
                    .get_csp_list()
                    .is_js_evaluation_allowed(cx, global, &code_str.str())
                {
                    // Step 9.6.2. Assert: handler is a string.
                    InternalTimerCallback::StringTimerCallback(
                        code_str,
                        initiating_script_fetch_info,
                    )
                } else {
                    return Ok(0);
                }
            },
            TimerCallback::FunctionTimerCallback(function) => {
                // This is a bit complicated, but this ensures that the vector's
                // buffer isn't reallocated (and moved) after setting the Heap values
                let mut args = Vec::with_capacity(arguments.len());
                for _ in 0..arguments.len() {
                    args.push(Heap::default());
                }
                for (i, item) in arguments.iter().enumerate() {
                    args.get_mut(i).unwrap().set(item.get());
                }
                // Step 9.5. If handler is a Function, then invoke handler given arguments and "report",
                // and with callback this value set to thisArg.
                InternalTimerCallback::FunctionTimerCallback(
                    function,
                    Rc::new(args.into_boxed_slice()),
                )
            },
        };

        // Step 2. If previousId was given, let id be previousId; otherwise,
        // let id be an implementation-defined integer that is greater than zero
        // and does not already exist in global's map of setTimeout and setInterval IDs.
        let JsTimerHandle(new_handle) = self.next_timer_handle.get();
        self.next_timer_handle.set(JsTimerHandle(new_handle + 1));

        // Step 3. If the surrounding agent's event loop's currently running task
        // is a task that was created by this algorithm, then let nesting level
        // be the task's timer nesting level. Otherwise, let nesting level be 0.
        let mut task = JsTimerTask {
            handle: JsTimerHandle(new_handle),
            source,
            callback,
            is_interval,
            is_user_interacting: ScriptThread::is_user_interacting(),
            nesting_level: 0,
            duration: Duration::ZERO,
        };

        // Step 4. If timeout is less than 0, then set timeout to 0.
        task.duration = timeout.max(Duration::ZERO);

        self.initialize_and_schedule(global, task);

        // Step 15. Return id.
        Ok(new_handle)
    }

    pub(crate) fn clear_timeout_or_interval(&self, global: &GlobalScope, handle: i32) {
        let mut active_timers = self.active_timers.borrow_mut();

        if let Some(entry) = active_timers.remove(&JsTimerHandle(handle)) {
            global.unschedule_callback(entry.oneshot_handle);
        }
    }

    pub(crate) fn set_min_duration(&self, duration: Duration) {
        self.min_duration.set(Some(duration));
    }

    pub(crate) fn remove_min_duration(&self) {
        self.min_duration.set(None);
    }

    // see step 13 of https://html.spec.whatwg.org/multipage/#timer-initialisation-steps
    fn user_agent_pad(&self, current_duration: Duration) -> Duration {
        match self.min_duration.get() {
            Some(min_duration) => min_duration.max(current_duration),
            None => current_duration,
        }
    }

    /// <https://html.spec.whatwg.org/multipage/#timer-initialisation-steps>
    fn initialize_and_schedule(&self, global: &GlobalScope, mut task: JsTimerTask) {
        let handle = task.handle;
        let mut active_timers = self.active_timers.borrow_mut();

        // Step 3. If the surrounding agent's event loop's currently running task
        // is a task that was created by this algorithm, then let nesting level be
        // the task's timer nesting level. Otherwise, let nesting level be 0.
        let nesting_level = self.nesting_level.get();

        let duration = self.user_agent_pad(clamp_duration(nesting_level, task.duration));
        // Step 10. Increment nesting level by one.
        // Step 11. Set task's timer nesting level to nesting level.
        task.nesting_level = nesting_level + 1;

        // Step 13. Set uniqueHandle to the result of running steps after a timeout given global,
        // "setTimeout/setInterval", timeout, and completionStep.
        let callback = OneshotTimerCallback::JsTimer(task);
        let oneshot_handle = global.schedule_callback(callback, duration);

        // Step 14. Set global's map of setTimeout and setInterval IDs[id] to uniqueHandle.
        let entry = active_timers
            .entry(handle)
            .or_insert(JsTimerEntry { oneshot_handle });
        entry.oneshot_handle = oneshot_handle;
    }
}

/// Step 5 of <https://html.spec.whatwg.org/multipage/#timer-initialisation-steps>
fn clamp_duration(nesting_level: u32, unclamped: Duration) -> Duration {
    // Step 5. If nesting level is greater than 5, and timeout is less than 4, then set timeout to 4.
    let lower_bound_ms = if nesting_level > 5 { 4 } else { 0 };
    let lower_bound = Duration::from_millis(lower_bound_ms);
    lower_bound.max(unclamped)
}

impl JsTimerTask {
    // see https://html.spec.whatwg.org/multipage/#timer-initialisation-steps
    fn invoke<T: DomObject>(self, this: &T, timers: &JsTimers, cx: &mut JSContext) {
        // step 9.2 can be ignored, because we proactively prevent execution
        // of this task when its scheduled execution is canceled.

        // prep for step ? in nested set_timeout_or_interval calls
        timers.nesting_level.set(self.nesting_level);

        let _guard = ScriptThread::user_interacting_guard();
        match self.callback {
            InternalTimerCallback::StringTimerCallback(ref code_str, ref fetch_info) => {
                // Step 6.4. Let settings object be global's relevant settings object.
                // Step 6. Let realm be global's relevant realm.
                let global = this.global();

                // Note: the steps to retrieve *fetch options* and *base URL* are performed in
                // `active_script_fetch_info`.
                let InitiatingScriptFetchInfo {
                    fetch_options,
                    base_url,
                } = fetch_info.clone();

                // Step 9.6.8. Let script be the result of creating a classic script given handler,
                // settings object, base URL, and fetch options.
                let script = global.create_a_classic_script(
                    cx,
                    (*code_str.str()).into(),
                    base_url,
                    fetch_options,
                    ErrorReporting::Unmuted,
                    Some(IntroductionType::DOM_TIMER),
                    1,
                    false,
                );

                // Step 9.6.9. Run the classic script script.
                _ = global.run_a_classic_script(cx, script, RethrowErrors::No);
            },
            // Step 9.5. If handler is a Function, then invoke handler given arguments and
            // "report", and with callback this value set to thisArg.
            InternalTimerCallback::FunctionTimerCallback(ref function, ref arguments) => {
                let arguments = self.collect_heap_args(arguments);
                rooted!(&in(cx) let mut value: JSVal);
                let _ = function.Call_(cx, this, arguments, value.handle_mut(), Report);
            },
        };

        // reset nesting level (see above)
        timers.nesting_level.set(0);

        // Step 9.9. If repeat is true, then perform the timer initialization steps again,
        // given global, handler, timeout, arguments, true, and id.
        //
        // Since we choose proactively prevent execution (see 4.1 above), we must only
        // reschedule repeating timers when they were not canceled as part of step 4.2.
        if self.is_interval == IsInterval::Interval &&
            timers.active_timers.borrow().contains_key(&self.handle)
        {
            timers.initialize_and_schedule(&this.global(), self);
        }
    }

    fn collect_heap_args<'b>(&self, args: &'b [Heap<JSVal>]) -> Vec<HandleValue<'b>> {
        args.iter().map(|arg| arg.as_handle_value()).collect()
    }
}

/// Describes the source that requested the [`TimerEvent`].
#[derive(Clone, Copy, Debug, Deserialize, MallocSizeOf, Serialize)]
pub enum TimerSource {
    /// The event was requested from a window (`ScriptThread`).
    FromWindow(PipelineId),
    /// The event was requested from a worker (`DedicatedGlobalWorkerScope`).
    FromWorker,
}

/// The id to be used for a [`TimerEvent`] is defined by the corresponding [`TimerEventRequest`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct TimerEventId(pub u32);

/// A notification that a timer has fired. [`TimerSource`] must be `FromWindow` when
/// dispatched to `ScriptThread` and must be `FromWorker` when dispatched to a
/// `DedicatedGlobalWorkerScope`
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct TimerEvent(pub TimerSource, pub TimerEventId);

/// A wrapper between timer events coming in over IPC, and the event-loop.
#[derive(Clone)]
struct TimerListener {
    task_source: SendableTaskSource,
    context: Trusted<GlobalScope>,
    source: TimerSource,
    id: TimerEventId,
}

impl TimerListener {
    /// Handle a timer-event coming from the [`timers::TimerScheduler`]
    /// by queuing the appropriate task on the relevant event-loop.
    /// <https://html.spec.whatwg.org/multipage/#timer-initialisation-steps>
    fn handle(&self, event: TimerEvent) {
        let context = self.context.clone();
        // Step 9. Let task be a task that runs the following substeps:
        self.task_source.queue(task!(timer_event: move |cx| {
                let global = context.root();
                let TimerEvent(source, id) = event;
                match source {
                    TimerSource::FromWorker => {
                        global.downcast::<WorkerGlobalScope>().expect("Window timer delivered to worker");
                    },
                    TimerSource::FromWindow(pipeline) => {
                        assert_eq!(pipeline, global.pipeline_id());
                        global.downcast::<Window>().expect("Worker timer delivered to window");
                    },
                };
                global.fire_timer(id, cx);
            })
        );
    }

    fn into_callback(self) -> BoxedTimerCallback {
        let timer_event = TimerEvent(self.source, self.id);
        Box::new(move || self.handle(timer_event))
    }
}

#[derive(Clone, JSTraceable, MallocSizeOf)]
struct InitiatingScriptFetchInfo {
    fetch_options: ScriptFetchOptions,
    #[no_trace]
    base_url: ServoUrl,
}

#[expect(unsafe_code)]
/// <https://html.spec.whatwg.org/multipage/#timer-initialisation-steps>
fn active_script_fetch_info(cx: &mut JSContext, global: &GlobalScope) -> InitiatingScriptFetchInfo {
    rooted!(&in(cx) let mut value = UndefinedValue());
    unsafe { JS_GetScriptedCallerPrivate(cx, value.handle_mut()) };

    let reference_private = value.handle().into_handle();

    // Step 7. Let initiating script be the active script.
    let initiating_script = unsafe { module_script_from_reference_private(&reference_private) };

    let (fetch_options, base_url) = match initiating_script {
        // Step 9.6.7. If initiating script is not null, then:
        Some(script) => (
            // Step 9.6.7.1. Set fetch options to a script fetch options whose
            ScriptFetchOptions {
                // cryptographic nonce is initiating script's fetch options's cryptographic nonce,
                cryptographic_nonce: script.options.cryptographic_nonce.clone(),
                // integrity metadata is the empty string,
                integrity_metadata: String::new(),
                // parser metadata is "not-parser-inserted",
                parser_metadata: ParserMetadata::NotParserInserted,
                // credentials mode is initiating script's fetch options's credentials mode,
                credentials_mode: script.options.credentials_mode,
                // referrer policy is initiating script's fetch options's referrer policy,
                referrer_policy: script.options.referrer_policy,
                // TODO and fetch priority is "auto".
                render_blocking: false,
            },
            // Step 9.6.7.2. Set base URL to initiating script's base URL.
            script.base_url.clone(),
        ),
        None => (
            // Step 9.6.5. Let fetch options be the default script fetch options.
            ScriptFetchOptions::default_classic_script(),
            // Step 9.6.6. Let base URL be settings object's API base URL.
            global.api_base_url(),
        ),
    };

    InitiatingScriptFetchInfo {
        fetch_options,
        base_url,
    }
}

#[cfg(test)]
mod tests {
    use timers::DocumentClockConfiguration;

    use super::*;

    #[test]
    fn suspension_freezes_dom_timer_time_and_resume_excludes_the_pause() {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 10,
            unix_time_origin_ns: 0,
            execution_limits: None,
        });
        let mut timebase = TimerTimebase::default();
        assert_eq!(timebase.now(&clock).unwrap(), DocumentTime::from_nanos(10));

        assert!(timebase.suspend(&clock).unwrap());
        assert!(!timebase.suspend(&clock).unwrap());
        clock.advance_to(DocumentTime::from_nanos(20)).unwrap();
        assert_eq!(timebase.now(&clock).unwrap(), DocumentTime::from_nanos(10));

        assert!(timebase.resume(&clock).unwrap());
        assert!(!timebase.resume(&clock).unwrap());
        clock.advance_to(DocumentTime::from_nanos(25)).unwrap();
        assert_eq!(timebase.now(&clock).unwrap(), DocumentTime::from_nanos(15));
        let original_deadline = DocumentTime::from_nanos(20);
        assert_eq!(
            original_deadline.saturating_duration_since(timebase.now(&clock).unwrap()),
            Duration::from_nanos(5)
        );
    }

    #[test]
    fn dom_timer_order_is_deadline_then_creation_sequence() {
        let early = DocumentTime::from_nanos(5);
        let late = DocumentTime::from_nanos(10);
        assert_eq!(compare_timer_order(early, 9, late, 0), Ordering::Greater);
        assert_eq!(compare_timer_order(late, 0, early, 9), Ordering::Less);
        assert_eq!(compare_timer_order(early, 0, early, 1), Ordering::Greater);
        assert_eq!(compare_timer_order(early, 1, early, 0), Ordering::Less);
    }

    #[test]
    fn timer_nesting_preserves_the_existing_minimum_delay_boundary() {
        assert_eq!(clamp_duration(5, Duration::ZERO), Duration::ZERO);
        assert_eq!(clamp_duration(6, Duration::ZERO), Duration::from_millis(4));
        assert_eq!(
            clamp_duration(6, Duration::from_millis(9)),
            Duration::from_millis(9)
        );
    }
}
