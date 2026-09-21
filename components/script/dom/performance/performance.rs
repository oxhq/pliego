/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::VecDeque;

use dom_struct::dom_struct;
use js::context::JSContext;
use js::jsval::NullValue;
use script_bindings::cell::DomRefCell;
use script_bindings::cformat;
use script_bindings::codegen::GenericBindings::PerformanceBinding::PerformanceMarkOptions;
use script_bindings::codegen::GenericBindings::PerformanceMarkBinding::PerformanceMarkMethods;
use script_bindings::codegen::GenericBindings::WindowBinding::WindowMethods;
use script_bindings::codegen::GenericUnionTypes::StringOrPerformanceMeasureOptions;
use script_bindings::reflector::reflect_dom_object_with_cx;
use servo_base::cross_process_instant::CrossProcessInstant;
use time::Duration;
use timers::{DocumentClock, DocumentClockError, DocumentTime, DocumentTimeSurface};

use super::performanceentry::{
    EntryType, PerformanceEntry, PerformanceEntryDuration, PerformanceEntryTime,
};
use super::performancemark::PerformanceMark;
use super::performancemeasure::PerformanceMeasure;
use super::performancenavigation::PerformanceNavigation;
use super::performancenavigationtiming::PerformanceNavigationTiming;
use super::performanceobserver::PerformanceObserver as DOMPerformanceObserver;
use crate::dom::PERFORMANCE_TIMING_ATTRIBUTES;
use crate::dom::bindings::codegen::Bindings::PerformanceBinding::{
    DOMHighResTimeStamp, PerformanceMethods,
};
use crate::dom::bindings::codegen::UnionTypes::StringOrDouble;
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::refcounted::Trusted;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::bindings::str::DOMString;
use crate::dom::bindings::structuredclone;
use crate::dom::bindings::trace::RootedTraceableBox;
use crate::dom::eventtarget::EventTarget;
use crate::dom::globalscope::GlobalScope;
use crate::dom::window::Window;

/// Implementation of a list of PerformanceEntry items shared by the
/// Performance and PerformanceObserverEntryList interfaces implementations.
#[derive(JSTraceable, MallocSizeOf)]
#[cfg_attr(crown, crown::unrooted_must_root_lint::must_root)]
pub(crate) struct PerformanceEntryList {
    /// <https://w3c.github.io/performance-timeline/#dfn-performance-entry-buffer>
    entries: Vec<Dom<PerformanceEntry>>,
}

impl PerformanceEntryList {
    pub(crate) fn new(entries: Vec<DomRoot<PerformanceEntry>>) -> Self {
        PerformanceEntryList {
            entries: entries.into_iter().map(|entry| entry.as_traced()).collect(),
        }
    }

    /// <https://www.w3.org/TR/performance-timeline/#dfn-filter-buffer-map-by-name-and-type>
    pub(crate) fn get_entries_by_name_and_type(
        &self,
        name: Option<DOMString>,
        entry_type: Option<EntryType>,
    ) -> Vec<DomRoot<PerformanceEntry>> {
        let mut result = self
            .entries
            .iter()
            .filter(|e| {
                name.as_ref().is_none_or(|name_| *e.name() == *name_) &&
                    entry_type
                        .as_ref()
                        .is_none_or(|type_| e.entry_type() == *type_)
            })
            .map(|entry| entry.as_rooted())
            .collect::<Vec<DomRoot<PerformanceEntry>>>();

        // Step 6. Sort results's entries in chronological order with respect to startTime
        result.sort_by(|a, b| {
            a.start_time_for_sorting()
                .partial_cmp(&b.start_time_for_sorting())
                .unwrap_or(Ordering::Equal)
        });

        // Step 7. Return result.
        result
    }

    pub(crate) fn clear_entries_by_name_and_type(
        &mut self,
        name: Option<DOMString>,
        entry_type: EntryType,
    ) {
        self.entries.retain(|e| {
            e.entry_type() != entry_type || name.as_ref().is_some_and(|name_| e.name() != name_)
        });
    }

    fn get_last_entry_start_time_with_name_and_type(
        &self,
        name: DOMString,
        entry_type: EntryType,
    ) -> Option<PerformanceEntryTime> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.entry_type() == entry_type && *e.name() == name)
            .and_then(|entry| entry.start_time())
    }
}

#[derive(JSTraceable, MallocSizeOf)]
#[cfg_attr(crown, crown::unrooted_must_root_lint::must_root)]
struct PerformanceObserver {
    observer: Dom<DOMPerformanceObserver>,
    entry_types: Vec<EntryType>,
}

#[derive(MallocSizeOf)]
struct WindowPerformanceClock {
    clock: DocumentClock,
    origin: DocumentTime,
}

impl WindowPerformanceClock {
    fn new(clock: DocumentClock, origin: DocumentTime) -> Self {
        clock
            .require_surface(DocumentTimeSurface::Performance)
            .expect("Window performance must use a supported document-clock surface");
        Self { clock, origin }
    }

    fn relative_now(&self) -> Result<Duration, DocumentClockError> {
        let elapsed = self.clock.try_now()?.checked_duration_since(self.origin)?;
        Duration::try_from(elapsed).map_err(|_| DocumentClockError::Overflow)
    }

    fn now(&self) -> Result<DOMHighResTimeStamp, DocumentClockError> {
        let elapsed = self.clock.try_now()?.checked_duration_since(self.origin)?;
        Ok(document_duration_to_dom_high_res_time_stamp(elapsed))
    }

    fn time_origin(&self) -> Result<DOMHighResTimeStamp, DocumentClockError> {
        Ok(signed_nanoseconds_to_dom_high_res_time_stamp(
            self.clock.unix_time_ns_at(self.origin)?.as_nanos(),
        ))
    }

    fn accepts_host_timestamp(&self) -> bool {
        self.clock
            .require_surface(DocumentTimeSurface::HostTimestamp)
            .is_ok()
    }
}

pub(crate) fn document_duration_to_dom_high_res_time_stamp(
    duration: std::time::Duration,
) -> DOMHighResTimeStamp {
    unsigned_microseconds_to_dom_high_res_time_stamp(duration.as_micros())
}

fn unsigned_microseconds_to_dom_high_res_time_stamp(
    whole_microseconds: u128,
) -> DOMHighResTimeStamp {
    let whole_milliseconds = whole_microseconds / 1000;
    let rounded_submillisecond_microseconds = whole_microseconds % 1000 / 10 * 10;
    Finite::wrap(whole_milliseconds as f64 + rounded_submillisecond_microseconds as f64 / 1000.0)
}

fn signed_nanoseconds_to_dom_high_res_time_stamp(nanoseconds: i128) -> DOMHighResTimeStamp {
    signed_microseconds_to_dom_high_res_time_stamp(nanoseconds / 1000)
}

fn signed_microseconds_to_dom_high_res_time_stamp(whole_microseconds: i128) -> DOMHighResTimeStamp {
    let whole_milliseconds = whole_microseconds / 1000;
    let submillisecond_microseconds = whole_microseconds % 1000;
    let rounded_submillisecond_microseconds = if submillisecond_microseconds >= 0 {
        submillisecond_microseconds / 10 * 10
    } else {
        submillisecond_microseconds.div_euclid(10) * 10
    };
    Finite::wrap(whole_milliseconds as f64 + rounded_submillisecond_microseconds as f64 / 1000.0)
}

fn controlled_window_performance_clock(
    clock: DocumentClock,
    origin: Option<DocumentTime>,
) -> Option<WindowPerformanceClock> {
    if !clock.is_controlled() {
        return None;
    }
    origin.map(|origin| WindowPerformanceClock::new(clock, origin))
}

#[dom_struct]
pub(crate) struct Performance {
    eventtarget: EventTarget,
    buffer: DomRefCell<PerformanceEntryList>,
    observers: DomRefCell<Vec<PerformanceObserver>>,
    pending_notification_observers_task: Cell<bool>,
    #[no_trace]
    /// The `timeOrigin` as described in
    /// <https://html.spec.whatwg.org/multipage/#concept-settings-object-time-origin>.
    time_origin: CrossProcessInstant,
    /// Window-only monotonic time domain. Workers retain their existing realtime Performance path.
    #[no_trace]
    window_clock: Option<WindowPerformanceClock>,
    /// <https://w3c.github.io/resource-timing/#performance-resource-timing-buffer-size-limit>
    /// The max-size of the buffer, set to 0 once the pipeline exits.
    /// TODO: have one max-size per entry type.
    resource_timing_buffer_size_limit: Cell<usize>,
    /// <https://w3c.github.io/resource-timing/#performance-resource-timing-buffer-current-size>
    resource_timing_buffer_current_size: Cell<usize>,
    /// <https://w3c.github.io/resource-timing/#performance-resource-timing-buffer-full-event-pending-flag>
    resource_timing_buffer_pending_full_event: Cell<bool>,
    /// <https://w3c.github.io/resource-timing/#performance-resource-timing-secondary-buffer>
    resource_timing_secondary_entries: DomRefCell<VecDeque<Dom<PerformanceEntry>>>,
}

impl Performance {
    fn new_inherited(
        time_origin: CrossProcessInstant,
        window_clock: Option<WindowPerformanceClock>,
    ) -> Performance {
        Performance {
            eventtarget: EventTarget::new_inherited(),
            buffer: DomRefCell::new(PerformanceEntryList::new(Vec::new())),
            observers: DomRefCell::new(Vec::new()),
            pending_notification_observers_task: Cell::new(false),
            time_origin,
            window_clock,
            resource_timing_buffer_size_limit: Cell::new(250),
            resource_timing_buffer_current_size: Cell::new(0),
            resource_timing_buffer_pending_full_event: Cell::new(false),
            resource_timing_secondary_entries: DomRefCell::new(VecDeque::new()),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        global: &GlobalScope,
        navigation_start: CrossProcessInstant,
        document_time_origin: Option<DocumentTime>,
    ) -> DomRoot<Performance> {
        let clock = global.document_clock();
        let window_clock = controlled_window_performance_clock(clock, document_time_origin);
        reflect_dom_object_with_cx(
            Box::new(Performance::new_inherited(navigation_start, window_clock)),
            global,
            cx,
        )
    }

    fn entry_time_origin(&self) -> PerformanceEntryTime {
        self.entry_time_from_relative(Duration::ZERO)
    }

    pub(crate) fn entry_time_from_relative(&self, relative: Duration) -> PerformanceEntryTime {
        if self.window_clock.is_some() {
            PerformanceEntryTime::Document(relative)
        } else {
            PerformanceEntryTime::Host(self.time_origin + relative)
        }
    }

    pub(crate) fn current_entry_time(&self) -> Fallible<PerformanceEntryTime> {
        current_user_timing_time(self.window_clock.as_ref(), CrossProcessInstant::now)
            .map_err(|error| Error::InvalidState(Some(error.to_string())))
    }

    pub(crate) fn entry_time_to_dom_high_res_time_stamp(
        &self,
        time: PerformanceEntryTime,
    ) -> DOMHighResTimeStamp {
        match time {
            PerformanceEntryTime::Document(relative) => relative.to_dom_high_res_time_stamp(),
            PerformanceEntryTime::Host(instant) => self.to_dom_high_res_time_stamp(instant),
        }
    }

    pub(crate) fn to_dom_high_res_time_stamp(
        &self,
        instant: CrossProcessInstant,
    ) -> DOMHighResTimeStamp {
        if self
            .window_clock
            .as_ref()
            .is_some_and(|clock| !clock.accepts_host_timestamp())
        {
            // Host- and cross-process-stamped entries need a producer-side document timestamp.
            // Until that upstream slice lands, latch UnsupportedSurface and return the only finite
            // neutral sentinel available to this WebIDL type. The control-plane terminal prevents
            // publication, so this value is never valid settlement evidence.
            return Finite::wrap(0.0);
        }
        (instant - self.time_origin).to_dom_high_res_time_stamp()
    }

    pub(crate) fn maybe_to_dom_high_res_time_stamp(
        &self,
        instant: Option<CrossProcessInstant>,
    ) -> DOMHighResTimeStamp {
        instant.map_or_else(
            || Finite::wrap(0.0),
            |instant| self.to_dom_high_res_time_stamp(instant),
        )
    }

    /// Clear all buffered performance entries, and disable the buffer.
    /// Called as part of the window's "clear_js_runtime" workflow,
    /// performed when exiting a pipeline.
    pub(crate) fn clear_and_disable_performance_entry_buffer(&self) {
        let mut buffer = self.buffer.borrow_mut();
        buffer.entries.clear();
        self.resource_timing_buffer_size_limit.set(0);
    }

    // Add a PerformanceObserver to the list of observers with a set of
    // observed entry types.

    pub(crate) fn add_multiple_type_observer(
        &self,
        observer: &DOMPerformanceObserver,
        entry_types: Vec<EntryType>,
    ) {
        let mut observers = self.observers.borrow_mut();
        match observers.iter().position(|o| *o.observer == *observer) {
            // If the observer is already in the list, we only update the observed
            // entry types.
            Some(p) => observers[p].entry_types = entry_types,
            // Otherwise, we create and insert the new PerformanceObserver.
            None => observers.push(PerformanceObserver {
                observer: Dom::from_ref(observer),
                entry_types,
            }),
        };
    }

    pub(crate) fn add_single_type_observer(
        &self,
        observer: &DOMPerformanceObserver,
        entry_type: EntryType,
        buffered: bool,
    ) {
        if buffered {
            let buffer = self.buffer.borrow();
            let new_entries = buffer.get_entries_by_name_and_type(None, Some(entry_type));
            if !new_entries.is_empty() {
                let new_entries = new_entries.into_iter().map(|entry| entry.as_traced());
                observer.entries_mut().extend(new_entries);
            }

            if !self.pending_notification_observers_task.get() {
                self.pending_notification_observers_task.set(true);
                let owner = Trusted::new(self);
                self.global()
                    .task_manager()
                    .performance_timeline_task_source()
                    .queue(task!(notify_performance_observers: move |cx| {
                        owner.root().notify_observers(cx);
                    }));
            }
        }
        let mut observers = self.observers.borrow_mut();
        match observers.iter().position(|o| *o.observer == *observer) {
            // If the observer is already in the list, we only update
            // the observed entry types.
            Some(p) => {
                // Append the type if not already present, otherwise do nothing
                if !observers[p].entry_types.contains(&entry_type) {
                    observers[p].entry_types.push(entry_type)
                }
            },
            // Otherwise, we create and insert the new PerformanceObserver.
            None => observers.push(PerformanceObserver {
                observer: Dom::from_ref(observer),
                entry_types: vec![entry_type],
            }),
        };
    }

    /// Remove a PerformanceObserver from the list of observers.
    pub(crate) fn remove_observer(&self, observer: &DOMPerformanceObserver) {
        let mut observers = self.observers.borrow_mut();
        let index = match observers.iter().position(|o| &(*o.observer) == observer) {
            Some(p) => p,
            None => return,
        };

        observers.remove(index);
    }

    /// Queue a notification for each performance observer interested in
    /// this type of performance entry and queue a low priority task to
    /// notify the observers if no other notification task is already queued.
    ///
    /// Algorithm spec:
    /// <https://w3c.github.io/performance-timeline/#queue-a-performanceentry>
    /// Also this algorithm has been extented according to :
    /// <https://w3c.github.io/resource-timing/#sec-extensions-performance-interface>
    pub(crate) fn queue_entry(&self, entry: &PerformanceEntry) -> Option<usize> {
        // https://w3c.github.io/performance-timeline/#dfn-determine-eligibility-for-adding-a-performance-entry
        if entry.entry_type() == EntryType::Resource && !self.should_queue_resource_entry(entry) {
            return None;
        }

        // Steps 1-3.
        // Add the performance entry to the list of performance entries that have not
        // been notified to each performance observer owner, filtering the ones it's
        // interested in.
        for observer in self
            .observers
            .borrow()
            .iter()
            .filter(|o| o.entry_types.contains(&entry.entry_type()))
        {
            observer.observer.queue_entry(entry);
        }

        // Step 4.
        // add the new entry to the buffer.
        self.buffer.borrow_mut().entries.push(Dom::from_ref(entry));

        let entry_last_index = self.buffer.borrow_mut().entries.len() - 1;

        // Step 5.
        // If there is already a queued notification task, we just bail out.
        if self.pending_notification_observers_task.get() {
            return None;
        }

        // Step 6.
        // Queue a new notification task.
        self.pending_notification_observers_task.set(true);

        let owner = Trusted::new(self);
        self.global()
            .task_manager()
            .performance_timeline_task_source()
            .queue(task!(notify_performance_observers: move |cx| {
                owner.root().notify_observers(cx);
            }));

        Some(entry_last_index)
    }

    /// Observers notifications task.
    ///
    /// Algorithm spec (step 7):
    /// <https://w3c.github.io/performance-timeline/#queue-a-performanceentry>
    fn notify_observers(&self, cx: &mut JSContext) {
        // Step 7.1.
        self.pending_notification_observers_task.set(false);

        // Step 7.2.
        // We have to operate over a copy of the performance observers to avoid
        // the risk of an observer's callback modifying the list of registered
        // observers. This is a shallow copy, so observers can
        // disconnect themselves by using the argument of their own callback.
        let observers: Vec<DomRoot<DOMPerformanceObserver>> = self
            .observers
            .borrow()
            .iter()
            .map(|o| DomRoot::from_ref(&*o.observer))
            .collect();

        // Step 7.3.
        for o in observers.iter() {
            o.notify(cx);
        }
    }

    /// <https://w3c.github.io/resource-timing/#performance-can-add-resource-timing-entry>
    fn can_add_resource_timing_entry(&self) -> bool {
        // Step 1. If resource timing buffer current size is smaller than resource timing buffer size limit, return true.
        // Step 2. Return false.
        self.resource_timing_buffer_current_size.get() <
            self.resource_timing_buffer_size_limit.get()
    }

    /// <https://w3c.github.io/resource-timing/#dfn-copy-secondary-buffer>
    fn copy_secondary_resource_timing_buffer(&self) {
        // Step 1. While resource timing secondary buffer is not empty and can add resource timing entry returns true, run the following substeps:
        while self.can_add_resource_timing_entry() {
            // Step 1.1. Let entry be the oldest PerformanceResourceTiming in resource timing secondary buffer.
            if let Some(ref entry) = self
                .resource_timing_secondary_entries
                .borrow_mut()
                .pop_front()
            {
                // Step 1.2. Add entry to the end of performance entry buffer.
                self.buffer.borrow_mut().entries.push(Dom::from_ref(entry));
                // Step 1.3. Increment resource timing buffer current size by 1.
                self.resource_timing_buffer_current_size
                    .set(self.resource_timing_buffer_current_size.get() + 1);
                // Step 1.4. Remove entry from resource timing secondary buffer.
                // Step 1.5. Decrement resource timing secondary buffer current size by 1.
                // Handled by popping the entry earlier.
            } else {
                break;
            }
        }
    }

    /// <https://w3c.github.io/resource-timing/#dfn-fire-a-buffer-full-event>
    fn fire_buffer_full_event(&self, cx: &mut js::context::JSContext) {
        while !self.resource_timing_secondary_entries.borrow().is_empty() {
            let no_of_excess_entries_before = self.resource_timing_secondary_entries.borrow().len();

            if !self.can_add_resource_timing_entry() {
                self.upcast::<EventTarget>()
                    .fire_event(cx, atom!("resourcetimingbufferfull"));
            }
            self.copy_secondary_resource_timing_buffer();
            let no_of_excess_entries_after = self.resource_timing_secondary_entries.borrow().len();
            if no_of_excess_entries_before <= no_of_excess_entries_after {
                self.resource_timing_secondary_entries.borrow_mut().clear();
                break;
            }
        }
        self.resource_timing_buffer_pending_full_event.set(false);
    }

    /// <https://w3c.github.io/resource-timing/#dfn-add-a-performanceresourcetiming-entry>
    fn should_queue_resource_entry(&self, entry: &PerformanceEntry) -> bool {
        // Step 1. If can add resource timing entry returns true and resource timing buffer full event pending flag is false, run the following substeps:
        if !self.resource_timing_buffer_pending_full_event.get() {
            if self.can_add_resource_timing_entry() {
                // Step 1.a.  Add new entry to the performance entry buffer.
                //   This is done in queue_entry, which calls this method.
                // Step 1.b. Increase resource timing buffer current size by 1.
                self.resource_timing_buffer_current_size
                    .set(self.resource_timing_buffer_current_size.get() + 1);
                // Step 1.c. Return.
                return true;
            }

            // Step 2.a. Set resource timing buffer full event pending flag to true.
            self.resource_timing_buffer_pending_full_event.set(true);
            // Step 2.b. Queue a task on the performance timeline task source to run fire a buffer full event.
            let performance = Trusted::new(self);
            self.global()
                .task_manager()
                .performance_timeline_task_source()
                .queue(task!(fire_a_buffer_full_event: move |cx| {
                    performance.root().fire_buffer_full_event(cx);
                }));
        }

        // Step 3. Add new entry to the resource timing secondary buffer.
        self.resource_timing_secondary_entries
            .borrow_mut()
            .push_back(Dom::from_ref(entry));

        // Step 4. Increase resource timing secondary buffer current size by 1.
        //   This is tracked automatically via `.len()`.
        false
    }

    pub(crate) fn update_entry(&self, index: usize, entry: &PerformanceEntry) {
        if let Some(e) = self.buffer.borrow_mut().entries.get_mut(index) {
            *e = Dom::from_ref(entry);
        }
    }

    /// <https://w3c.github.io/user-timing/#convert-a-name-to-a-timestamp>
    fn convert_a_name_to_a_timestamp(&self, name: &str) -> Fallible<PerformanceEntryTime> {
        // Step 1. If the global object is not a Window object, throw a TypeError.
        let Some(window) = DomRoot::downcast::<Window>(self.global()) else {
            return Err(Error::Type(cformat!(
                "Cannot use {name} from non-window global"
            )));
        };

        // Step 2. If name is navigationStart, return 0.
        if name == "navigationStart" {
            return Ok(self.entry_time_origin());
        }

        // Step 3. Let startTime be the value of navigationStart in the PerformanceTiming interface.
        // FIXME: We don't implement this value yet, so we assume it's zero (and then we don't need it at all)

        // Step 4. Let endTime be the value of name in the PerformanceTiming interface.
        //
        // NOTE: We store all performance values on the document
        let end_time = window.Document().performance_timing_attribute(name)?;

        // Step 5. If endTime is 0, throw an InvalidAccessError.
        let Some(end_time) = end_time else {
            return Err(Error::InvalidAccess(Some(format!(
                "{name} hasn't happened yet"
            ))));
        };

        // Step 6. Return result of subtracting startTime from endTime.
        Ok(PerformanceEntryTime::Host(end_time))
    }

    /// <https://w3c.github.io/user-timing/#convert-a-mark-to-a-timestamp>
    fn convert_a_mark_to_a_timestamp(
        &self,
        mark: &StringOrDouble,
    ) -> Fallible<PerformanceEntryTime> {
        match mark {
            StringOrDouble::String(name) => {
                // Step 1. If mark is a DOMString and it has the same name as a read only attribute in the
                // PerformanceTiming interface, let end time be the value returned by running the convert
                // a name to a timestamp algorithm with name set to the value of mark.
                if PERFORMANCE_TIMING_ATTRIBUTES.contains(&&*name.str()) {
                    self.convert_a_name_to_a_timestamp(&name.str())
                }
                // Step 2. Otherwise, if mark is a DOMString, let end time be the value of the startTime
                // attribute from the most recent occurrence of a PerformanceMark object in the performance entry
                // buffer whose name is mark. If no matching entry is found, throw a SyntaxError.
                else {
                    self.buffer
                        .borrow()
                        .get_last_entry_start_time_with_name_and_type(name.clone(), EntryType::Mark)
                        .ok_or(Error::Syntax(Some(format!(
                            "No PerformanceMark named {name} exists"
                        ))))
                }
            },
            // Step 3. Otherwise, if mark is a DOMHighResTimeStamp:
            StringOrDouble::Double(timestamp) => {
                // Step 3.1 If mark is negative, throw a TypeError.
                if timestamp.is_sign_negative() {
                    return Err(Error::Type(c"Time stamps must not be negative".to_owned()));
                }

                // Step 3.2 Otherwise, let end time be mark.
                // NOTE: I think the spec wants us to return the value.
                Ok(self.entry_time_from_relative(Duration::microseconds(
                    timestamp.mul_add(1000.0, 0.0) as i64,
                )))
            },
        }
    }
}

fn current_user_timing_time<F>(
    window_clock: Option<&WindowPerformanceClock>,
    host_time: F,
) -> Result<PerformanceEntryTime, DocumentClockError>
where
    F: FnOnce() -> CrossProcessInstant,
{
    match window_clock {
        Some(clock) if clock.clock.is_controlled() => {
            clock.relative_now().map(PerformanceEntryTime::Document)
        },
        _ => Ok(PerformanceEntryTime::Host(host_time())),
    }
}

fn add_performance_duration(
    time: PerformanceEntryTime,
    duration: PerformanceEntryDuration,
) -> Fallible<PerformanceEntryTime> {
    match (time, duration) {
        (PerformanceEntryTime::Host(time), PerformanceEntryDuration::Host(duration)) => {
            Ok(PerformanceEntryTime::Host(time + duration))
        },
        (PerformanceEntryTime::Document(time), PerformanceEntryDuration::Document(duration)) => {
            Ok(PerformanceEntryTime::Document(time + duration))
        },
        _ => Err(mixed_performance_time_error()),
    }
}

fn subtract_performance_duration(
    time: PerformanceEntryTime,
    duration: PerformanceEntryDuration,
) -> Fallible<PerformanceEntryTime> {
    match (time, duration) {
        (PerformanceEntryTime::Host(time), PerformanceEntryDuration::Host(duration)) => {
            Ok(PerformanceEntryTime::Host(time - duration))
        },
        (PerformanceEntryTime::Document(time), PerformanceEntryDuration::Document(duration)) => {
            Ok(PerformanceEntryTime::Document(time - duration))
        },
        _ => Err(mixed_performance_time_error()),
    }
}

fn mixed_performance_time_error() -> Error {
    Error::NotSupported(Some(
        "cannot combine host-stamped performance timing with controlled document time".to_owned(),
    ))
}

fn performance_duration_between(
    end: PerformanceEntryTime,
    start: PerformanceEntryTime,
) -> Fallible<PerformanceEntryDuration> {
    match (end, start) {
        (PerformanceEntryTime::Host(end), PerformanceEntryTime::Host(start)) => {
            Ok(PerformanceEntryDuration::Host(end - start))
        },
        (PerformanceEntryTime::Document(end), PerformanceEntryTime::Document(start)) => {
            Ok(PerformanceEntryDuration::Document(end - start))
        },
        _ => Err(mixed_performance_time_error()),
    }
}

impl PerformanceMethods<crate::DomTypeHolder> for Performance {
    /// <https://w3c.github.io/navigation-timing/#dom-performance-timing>
    fn Timing(&self) -> DomRoot<PerformanceNavigationTiming> {
        let entries = self.GetEntriesByType(DOMString::from("navigation"));
        if !entries.is_empty() {
            return DomRoot::from_ref(
                entries[0]
                    .downcast::<PerformanceNavigationTiming>()
                    .unwrap(),
            );
        }
        unreachable!("Are we trying to expose Performance.timing in workers?");
    }

    /// <https://w3c.github.io/navigation-timing/#dom-performance-navigation>
    fn Navigation(&self, cx: &mut JSContext) -> DomRoot<PerformanceNavigation> {
        PerformanceNavigation::new(cx, &self.global())
    }

    /// <https://w3c.github.io/hr-time/#dom-performance-now>
    fn Now(&self) -> DOMHighResTimeStamp {
        self.window_clock.as_ref().map_or_else(
            || self.to_dom_high_res_time_stamp(CrossProcessInstant::now()),
            |clock| {
                clock
                    .now()
                    .expect("Window document time cannot precede its navigation origin")
            },
        )
    }

    /// <https://www.w3.org/TR/hr-time-2/#dom-performance-timeorigin>
    fn TimeOrigin(&self) -> DOMHighResTimeStamp {
        if let Some(clock) = &self.window_clock &&
            clock.clock.is_controlled()
        {
            return clock
                .time_origin()
                .expect("controlled performance.timeOrigin must use validated signed wall time");
        }
        (self.time_origin - CrossProcessInstant::epoch()).to_dom_high_res_time_stamp()
    }

    /// <https://www.w3.org/TR/performance-timeline-2/#dom-performance-getentries>
    fn GetEntries(&self) -> Vec<DomRoot<PerformanceEntry>> {
        // > Returns a PerformanceEntryList object returned by the filter buffer map by name and type
        // > algorithm with name and type set to null.
        self.buffer
            .borrow()
            .get_entries_by_name_and_type(None, None)
    }

    /// <https://www.w3.org/TR/performance-timeline-2/#dom-performance-getentriesbytype>
    fn GetEntriesByType(&self, entry_type: DOMString) -> Vec<DomRoot<PerformanceEntry>> {
        let Ok(entry_type) = EntryType::try_from(&*entry_type.str()) else {
            return Vec::new();
        };
        self.buffer
            .borrow()
            .get_entries_by_name_and_type(None, Some(entry_type))
    }

    /// <https://www.w3.org/TR/performance-timeline-2/#dom-performance-getentriesbyname>
    fn GetEntriesByName(
        &self,
        name: DOMString,
        entry_type: Option<DOMString>,
    ) -> Vec<DomRoot<PerformanceEntry>> {
        let entry_type = match entry_type {
            Some(entry_type) => {
                let Ok(entry_type) = EntryType::try_from(&*entry_type.str()) else {
                    return Vec::new();
                };
                Some(entry_type)
            },
            None => None,
        };
        self.buffer
            .borrow()
            .get_entries_by_name_and_type(Some(name), entry_type)
    }

    /// <https://w3c.github.io/user-timing/#dom-performance-mark>
    fn Mark(
        &self,
        cx: &mut JSContext,
        mark_name: DOMString,
        mark_options: RootedTraceableBox<PerformanceMarkOptions>,
    ) -> Fallible<DomRoot<PerformanceMark>> {
        // Step 1. Run the PerformanceMark constructor and let entry be the newly created object.
        let entry =
            PerformanceMark::Constructor(cx, &self.global(), None, mark_name, mark_options)?;

        // Step 2. Queue a PerformanceEntry entry.
        // Step 3. Add entry to the performance entry buffer. (This is done in queue_entry itself)
        self.queue_entry(entry.upcast::<PerformanceEntry>());

        // Step 4. Return entry.
        Ok(entry)
    }

    /// <https://w3c.github.io/user-timing/#dom-performance-clearmarks>
    fn ClearMarks(&self, mark_name: Option<DOMString>) {
        self.buffer
            .borrow_mut()
            .clear_entries_by_name_and_type(mark_name, EntryType::Mark);
    }

    /// <https://w3c.github.io/user-timing/#dom-performance-measure>
    fn Measure(
        &self,
        cx: &mut JSContext,
        measure_name: DOMString,
        start_or_measure_options: StringOrPerformanceMeasureOptions,
        end_mark: Option<DOMString>,
    ) -> Fallible<DomRoot<PerformanceMeasure>> {
        // Step 1. If startOrMeasureOptions is a PerformanceMeasureOptions object and at least one of start,
        // end, duration, and detail exist, run the following checks:
        if let StringOrPerformanceMeasureOptions::PerformanceMeasureOptions(options) =
            &start_or_measure_options &&
            (options.start.is_some() ||
                options.duration.is_some() ||
                options.end.is_some() ||
                options.detail.get().is_object_or_null())
        {
            // Step 1.1 If endMark is given, throw a TypeError.
            if end_mark.is_some() {
                return Err(Error::Type(
                    c"Must not provide endMark if PerformanceMeasureOptions is also provided"
                        .to_owned(),
                ));
            }

            // Step 1.2 If startOrMeasureOptions’s start and end members are both omitted, throw a TypeError.
            if options.start.is_none() && options.end.is_none() {
                return Err(Error::Type(
                    c"Either 'start' or 'end' member of PerformanceMeasureOptions must be provided"
                        .to_owned(),
                ));
            }

            // Step 1.3 If startOrMeasureOptions’s start, duration, and end members all exist, throw a TypeError.
            if options.start.is_some() && options.duration.is_some() && options.end.is_some() {
                return Err(Error::Type(c"Either 'start' or 'end' or 'duration' member of PerformanceMeasureOptions must be omitted".to_owned()));
            }
        }

        // Step 2. Compute end time as follows:
        // Step 2.1 If endMark is given, let end time be the value returned
        // by running the convert a mark to a timestamp algorithm passing in endMark.
        let end_time = if let Some(end_mark) = end_mark {
            self.convert_a_mark_to_a_timestamp(&StringOrDouble::String(end_mark))?
        } else {
            match &start_or_measure_options {
                StringOrPerformanceMeasureOptions::PerformanceMeasureOptions(options) => {
                    // Step 2.2 Otherwise, if startOrMeasureOptions is a PerformanceMeasureOptions object,
                    // and if its end member exists, let end time be the value returned by running the
                    // convert a mark to a timestamp algorithm passing in startOrMeasureOptions’s end.
                    if let Some(end) = &options.end {
                        self.convert_a_mark_to_a_timestamp(end)?
                    }
                    // Step 2.3 Otherwise, if startOrMeasureOptions is a PerformanceMeasureOptions object,
                    // and if its start and duration members both exist:
                    else if let Some((start, duration)) =
                        options.start.as_ref().zip(options.duration)
                    {
                        // Step 2.3.1 Let start be the value returned by running the convert a mark to a timestamp
                        // algorithm passing in start.
                        let start = self.convert_a_mark_to_a_timestamp(start)?;

                        // Step 2.3.2 Let duration be the value returned by running the convert a mark to a timestamp
                        // algorithm passing in duration.
                        let duration = performance_duration_between(
                            self.convert_a_mark_to_a_timestamp(&StringOrDouble::Double(duration))?,
                            self.entry_time_origin(),
                        )?;

                        // Step 2.3.3 Let end time be start plus duration.
                        add_performance_duration(start, duration)?
                    } else {
                        // Step 2.4 Otherwise, let end time be the value that would be returned by the
                        // Performance object’s now() method.
                        self.current_entry_time()?
                    }
                },
                _ => {
                    // Step 2.4 Otherwise, let end time be the value that would be returned by the
                    // Performance object’s now() method.
                    self.current_entry_time()?
                },
            }
        };

        // Step 3. Compute start time as follows:
        let start_time = match &start_or_measure_options {
            StringOrPerformanceMeasureOptions::PerformanceMeasureOptions(options) => {
                // Step 3.1 If startOrMeasureOptions is a PerformanceMeasureOptions object, and if its start member exists,
                // let start time be the value returned by running the convert a mark to a timestamp algorithm passing in
                // startOrMeasureOptions’s start.
                if let Some(start) = &options.start {
                    self.convert_a_mark_to_a_timestamp(start)?
                }
                // Step 3.2 Otherwise, if startOrMeasureOptions is a PerformanceMeasureOptions object,
                // and if its duration and end members both exist:
                else if let Some((duration, end)) = options.duration.zip(options.end.as_ref()) {
                    // Step 3.2.1 Let duration be the value returned by running the convert a mark to a timestamp
                    // algorithm passing in duration.
                    let duration = performance_duration_between(
                        self.convert_a_mark_to_a_timestamp(&StringOrDouble::Double(duration))?,
                        self.entry_time_origin(),
                    )?;

                    // Step 3.2.2 Let end be the value returned by running the convert a mark to a timestamp algorithm
                    // passing in end.
                    let end = self.convert_a_mark_to_a_timestamp(end)?;

                    // Step 3.3.3 Let start time be end minus duration.
                    subtract_performance_duration(end, duration)?
                }
                // Step 3.4 Otherwise, let start time be 0.
                else {
                    self.entry_time_origin()
                }
            },
            StringOrPerformanceMeasureOptions::String(string) => {
                // Step 3.3 Otherwise, if startOrMeasureOptions is a DOMString, let start time be the value returned
                // by running the convert a mark to a timestamp algorithm passing in startOrMeasureOptions.
                self.convert_a_mark_to_a_timestamp(&StringOrDouble::String(string.clone()))?
            },
        };

        // Step 4. Create a new PerformanceMeasure object (entry) with this’s relevant realm.
        // Step 5. Set entry’s name attribute to measureName.
        // Step 6. Set entry’s entryType attribute to DOMString "measure".
        // Step 7. Set entry’s startTime attribute to start time.
        // Step 8. Set entry’s duration attribute to the duration from start time to end time.
        // The resulting duration value MAY be negative.

        let entry = PerformanceMeasure::new(
            cx,
            &self.global(),
            measure_name,
            start_time,
            performance_duration_between(end_time, start_time)?,
        );

        // Step 9. Set entry’s detail attribute as follows:
        rooted!(&in(cx) let mut detail = NullValue());
        // Step 9.1. If startOrMeasureOptions is a PerformanceMeasureOptions object and startOrMeasureOptions’s detail member exists:
        if let StringOrPerformanceMeasureOptions::PerformanceMeasureOptions(options) =
            &start_or_measure_options &&
            !options.detail.get().is_null_or_undefined()
        {
            // Step 9.1.1. Let record be the result of calling the StructuredSerialize algorithm on startOrMeasureOptions’s detail.
            let record = structuredclone::write(cx, options.detail.handle(), None)?;

            // Step 9.1.2. Set entry’s detail to the result of calling the StructuredDeserialize algorithm on record and the current realm.
            structuredclone::read(cx, &self.global(), record, detail.handle_mut())?;
        }
        // Step 9.2. Otherwise, set it to null.
        //
        // Note: This is already the default value we set when creating the detail above

        entry.set_detail(detail.handle());

        // Step 10. Queue a PerformanceEntry entry.
        // Step 11. Add entry to the performance entry buffer. (This is done in queue_entry itself)
        self.queue_entry(entry.upcast::<PerformanceEntry>());

        // Step 12. Return entry.
        Ok(entry)
    }

    /// <https://w3c.github.io/user-timing/#dom-performance-clearmeasures>
    fn ClearMeasures(&self, measure_name: Option<DOMString>) {
        self.buffer
            .borrow_mut()
            .clear_entries_by_name_and_type(measure_name, EntryType::Measure);
    }
    /// <https://w3c.github.io/resource-timing/#dom-performance-clearresourcetimings>
    fn ClearResourceTimings(&self) {
        self.buffer
            .borrow_mut()
            .clear_entries_by_name_and_type(None, EntryType::Resource);
        self.resource_timing_buffer_current_size.set(0);
    }

    /// <https://w3c.github.io/resource-timing/#performance-setresourcetimingbuffersize>
    fn SetResourceTimingBufferSize(&self, max_size: u32) {
        self.resource_timing_buffer_size_limit
            .set(max_size as usize);
    }

    // https://w3c.github.io/resource-timing/#dom-performance-onresourcetimingbufferfull
    event_handler!(
        resourcetimingbufferfull,
        GetOnresourcetimingbufferfull,
        SetOnresourcetimingbufferfull
    );
}

pub(crate) trait ToDOMHighResTimeStamp {
    fn to_dom_high_res_time_stamp(&self) -> DOMHighResTimeStamp;
}

impl ToDOMHighResTimeStamp for Duration {
    fn to_dom_high_res_time_stamp(&self) -> DOMHighResTimeStamp {
        // https://www.w3.org/TR/hr-time-2/#clock-resolution
        // We need a granularity no finer than 5 microseconds. 5 microseconds isn't an
        // exactly representable f64 so WPT tests might occasionally corner-case on
        // rounding.  web-platform-tests/wpt#21526 wants us to use an integer number of
        // microseconds; the next divisor of milliseconds up from 5 microseconds is 10.
        signed_microseconds_to_dom_high_res_time_stamp(self.whole_microseconds())
    }
}

#[cfg(test)]
mod tests {
    use timers::{DocumentClockConfiguration, DocumentUnixTime};

    use super::*;

    #[test]
    fn controlled_window_performance_is_monotonic_and_exactly_ordered() {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 7_000_000,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(1_700_000_000_000_000_000),
            execution_limits: None,
        });
        let performance_clock =
            controlled_window_performance_clock(clock.clone(), Some(clock.now())).unwrap();

        assert_eq!(performance_clock.now(), Ok(Finite::wrap(0.0)));
        assert_eq!(
            performance_clock.time_origin(),
            Ok(Finite::wrap(1_700_000_000_007.0))
        );

        clock
            .advance_to(DocumentTime::from_nanos(12_000_000))
            .unwrap();
        assert_eq!(performance_clock.now(), Ok(Finite::wrap(5.0)));
        assert_eq!(performance_clock.now(), Ok(Finite::wrap(5.0)));
        assert!(!performance_clock.accepts_host_timestamp());
        assert_eq!(
            clock.unsupported_surface(),
            Some(DocumentTimeSurface::HostTimestamp)
        );
    }

    #[test]
    fn controlled_user_timing_does_not_advance_with_host_time() {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 7_000_000,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(1_700_000_000_000_000_000),
            execution_limits: None,
        });
        let performance_clock = WindowPerformanceClock::new(clock.clone(), clock.now());
        let host_time = CrossProcessInstant::epoch() + Duration::seconds(1);
        let sampled_host_time = Cell::new(false);

        assert_eq!(
            current_user_timing_time(Some(&performance_clock), || {
                sampled_host_time.set(true);
                host_time
            }),
            Ok(PerformanceEntryTime::Document(Duration::ZERO))
        );
        assert!(!sampled_host_time.get());
        assert_eq!(
            current_user_timing_time(Some(&performance_clock), || {
                host_time + Duration::hours(1)
            }),
            Ok(PerformanceEntryTime::Document(Duration::ZERO))
        );

        clock
            .advance_to(DocumentTime::from_nanos(12_000_000))
            .unwrap();
        assert_eq!(
            current_user_timing_time(Some(&performance_clock), || {
                host_time + Duration::hours(2)
            }),
            Ok(PerformanceEntryTime::Document(Duration::milliseconds(5)))
        );
    }

    #[test]
    fn controlled_window_performance_checks_future_origin() {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 1,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: None,
        });
        let future_origin = WindowPerformanceClock::new(clock.clone(), DocumentTime::from_nanos(2));

        assert_eq!(
            future_origin.now(),
            Err(DocumentClockError::TimeMovedBackwards {
                current: DocumentTime::from_nanos(2),
                requested: DocumentTime::from_nanos(1),
            })
        );
        assert_eq!(future_origin.time_origin(), Ok(Finite::wrap(0.0)));
    }

    #[test]
    fn controlled_window_performance_preserves_negative_api2_epoch() {
        const API2_MIN_EPOCH_NS: i128 = -8_640_000_000_000_000_000_000;
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(API2_MIN_EPOCH_NS),
            execution_limits: None,
        });
        let performance_clock = WindowPerformanceClock::new(clock, DocumentTime::ZERO);

        assert_eq!(
            performance_clock.time_origin(),
            Ok(Finite::wrap(-8_640_000_000_000_000.0))
        );
        assert_eq!(performance_clock.now(), Ok(Finite::wrap(0.0)));
    }

    #[test]
    fn controlled_window_performance_preserves_the_full_api2_virtual_span() {
        const API2_MAX_EPOCH_NS: i128 = 8_640_000_000_000_000_000_000;
        const API2_MAX_SPAN_NS: u128 = 9_007_199_254_740_991_000_000;
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(API2_MAX_EPOCH_NS),
            execution_limits: None,
        });
        let performance_clock = WindowPerformanceClock::new(clock.clone(), DocumentTime::ZERO);
        clock
            .advance_to(DocumentTime::from_nanos(API2_MAX_SPAN_NS))
            .unwrap();

        assert_eq!(
            performance_clock.time_origin(),
            Ok(Finite::wrap(8_640_000_000_000_000.0))
        );
        assert_eq!(
            performance_clock.now(),
            Ok(Finite::wrap(9_007_199_254_740_991.0))
        );
    }

    #[test]
    fn controlled_window_performance_preserves_adversarial_exact_milliseconds() {
        const EPOCH_MS: i128 = 8_639_999_999_999_979;
        const SPAN_MS: u128 = 9_007_199_254_740_971;
        const NANOS_PER_MILLISECOND: i128 = 1_000_000;
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(EPOCH_MS * NANOS_PER_MILLISECOND),
            execution_limits: None,
        });
        let performance_clock = WindowPerformanceClock::new(clock.clone(), DocumentTime::ZERO);
        clock
            .advance_to(DocumentTime::from_nanos(
                SPAN_MS * NANOS_PER_MILLISECOND as u128,
            ))
            .unwrap();

        assert_eq!(
            performance_clock.time_origin(),
            Ok(Finite::wrap(EPOCH_MS as f64))
        );
        assert_eq!(performance_clock.now(), Ok(Finite::wrap(SPAN_MS as f64)));

        let negative_clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(-EPOCH_MS * NANOS_PER_MILLISECOND),
            execution_limits: None,
        });
        let negative_performance_clock =
            WindowPerformanceClock::new(negative_clock, DocumentTime::ZERO);
        assert_eq!(
            negative_performance_clock.time_origin(),
            Ok(Finite::wrap(-(EPOCH_MS as f64)))
        );
    }

    #[test]
    fn performance_entry_duration_casts_integer_milliseconds_before_fractional_precision() {
        const NEAR_MAX_MS: i64 = 9_007_199_254_740_971;

        assert_eq!(
            Duration::milliseconds(NEAR_MAX_MS).to_dom_high_res_time_stamp(),
            Finite::wrap(NEAR_MAX_MS as f64)
        );
        assert_eq!(
            Duration::milliseconds(-NEAR_MAX_MS).to_dom_high_res_time_stamp(),
            Finite::wrap(-(NEAR_MAX_MS as f64))
        );
        assert_eq!(
            Duration::microseconds(-1).to_dom_high_res_time_stamp(),
            Finite::wrap(-0.01)
        );
        assert_eq!(
            Duration::microseconds(-1001).to_dom_high_res_time_stamp(),
            Finite::wrap(-1.01)
        );
    }

    #[test]
    fn realtime_window_performance_retains_host_timestamp_compatibility() {
        let clock = DocumentClock::default();
        let origin = clock.now();
        let performance_clock = WindowPerformanceClock::new(clock, origin);

        assert!(*performance_clock.now().unwrap() >= 0.0);
        assert!(performance_clock.accepts_host_timestamp());
        assert_eq!(
            performance_clock.time_origin(),
            Err(DocumentClockError::RealtimeClock)
        );

        let clock = DocumentClock::default();
        let origin = clock.now();
        assert!(controlled_window_performance_clock(clock, Some(origin)).is_none());
    }

    #[test]
    fn realtime_user_timing_preserves_the_host_clock_domain() {
        let clock = DocumentClock::default();
        let origin = clock.now();
        let performance_clock = WindowPerformanceClock::new(clock, origin);
        let host_time = CrossProcessInstant::epoch() + Duration::seconds(23);

        assert_eq!(
            current_user_timing_time(None, || host_time),
            Ok(PerformanceEntryTime::Host(host_time))
        );
        assert_eq!(
            current_user_timing_time(Some(&performance_clock), || host_time),
            Ok(PerformanceEntryTime::Host(host_time))
        );
    }

    #[test]
    fn user_timing_rejects_mixed_host_and_document_arithmetic() {
        let host_time = CrossProcessInstant::epoch() + Duration::seconds(23);
        let document_time = Duration::milliseconds(5);

        assert!(matches!(
            performance_duration_between(
                PerformanceEntryTime::Host(host_time),
                PerformanceEntryTime::Document(document_time),
            ),
            Err(Error::NotSupported(_))
        ));
        assert!(matches!(
            performance_duration_between(
                PerformanceEntryTime::Document(document_time),
                PerformanceEntryTime::Host(host_time),
            ),
            Err(Error::NotSupported(_))
        ));
        assert!(matches!(
            add_performance_duration(
                PerformanceEntryTime::Document(document_time),
                PerformanceEntryDuration::Host(Duration::milliseconds(1)),
            ),
            Err(Error::NotSupported(_))
        ));
        assert!(matches!(
            subtract_performance_duration(
                PerformanceEntryTime::Host(host_time),
                PerformanceEntryDuration::Document(Duration::milliseconds(1)),
            ),
            Err(Error::NotSupported(_))
        ));
    }
}
