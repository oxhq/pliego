/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::reflector::Reflector;
use servo_base::cross_process_instant::CrossProcessInstant;
use strum::VariantArray;
use time::Duration;
use timers::DocumentTimeSurface;

use super::performance::ToDOMHighResTimeStamp;
use crate::dom::bindings::codegen::Bindings::PerformanceBinding::DOMHighResTimeStamp;
use crate::dom::bindings::codegen::Bindings::PerformanceEntryBinding::PerformanceEntryMethods;
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::str::DOMString;

/// The clock domain that produced a performance-entry timestamp.
///
/// Controlled Window time is stored as a signed offset from that Window's time origin. Host and
/// cross-process producers remain explicitly distinct so they cannot silently enter the controlled
/// document-time domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerformanceEntryTime {
    Host(CrossProcessInstant),
    Document(Duration),
}

impl PartialOrd for PerformanceEntryTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Host(left), Self::Host(right)) => left.partial_cmp(right),
            (Self::Document(left), Self::Document(right)) => left.partial_cmp(right),
            _ => None,
        }
    }
}

/// The clock domain that produced a performance-entry duration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerformanceEntryDuration {
    Host(Duration),
    Document(Duration),
}

impl PerformanceEntryDuration {
    pub(crate) fn for_time(time: PerformanceEntryTime, duration: Duration) -> Self {
        match time {
            PerformanceEntryTime::Host(_) => Self::Host(duration),
            PerformanceEntryTime::Document(_) => Self::Document(duration),
        }
    }
}

/// All supported entry types, in alphabetical order.
#[derive(Clone, Copy, JSTraceable, MallocSizeOf, PartialEq, VariantArray)]
pub(crate) enum EntryType {
    LargestContentfulPaint,
    Mark,
    Measure,
    Navigation,
    Paint,
    Resource,
    VisibilityState,
}

impl EntryType {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            EntryType::Measure => "measure",
            EntryType::Mark => "mark",
            EntryType::LargestContentfulPaint => "largest-contentful-paint",
            EntryType::Paint => "paint",
            EntryType::Navigation => "navigation",
            EntryType::Resource => "resource",
            EntryType::VisibilityState => "visibility-state",
        }
    }
}

impl<'a> TryFrom<&'a str> for EntryType {
    type Error = ();

    fn try_from(value: &'a str) -> Result<EntryType, ()> {
        Ok(match value {
            "measure" => EntryType::Measure,
            "mark" => EntryType::Mark,
            "largest-contentful-paint" => EntryType::LargestContentfulPaint,
            "paint" => EntryType::Paint,
            "navigation" => EntryType::Navigation,
            "resource" => EntryType::Resource,
            "visibility-state" => EntryType::VisibilityState,
            _ => return Err(()),
        })
    }
}

/// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry>
#[dom_struct]
pub(crate) struct PerformanceEntry {
    reflector_: Reflector,

    /// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry-name>
    name: DOMString,

    /// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry-entrytype>
    entry_type: EntryType,

    /// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry-starttime>
    #[no_trace]
    #[ignore_malloc_size_of = "The timestamp provenance has no heap allocations"]
    start_time: Option<PerformanceEntryTime>,

    /// The duration of this [`PerformanceEntry`]. This is a [`time::Duration`],
    /// because it can be negative and `std::time::Duration` cannot be.
    ///
    /// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry-duration>
    #[no_trace]
    #[ignore_malloc_size_of = "The duration provenance has no heap allocations"]
    duration: PerformanceEntryDuration,
}

impl PerformanceEntry {
    pub(crate) fn new_inherited(
        name: DOMString,
        entry_type: EntryType,
        start_time: Option<PerformanceEntryTime>,
        duration: PerformanceEntryDuration,
    ) -> PerformanceEntry {
        PerformanceEntry {
            reflector_: Reflector::new(),
            name,
            entry_type,
            start_time,
            duration,
        }
    }

    /// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry-name>
    pub(crate) fn entry_type(&self) -> EntryType {
        self.entry_type
    }

    /// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry-entrytype>
    pub(crate) fn name(&self) -> &DOMString {
        &self.name
    }

    /// <https://www.w3.org/TR/performance-timeline/#dom-performanceentry-starttime>
    pub(crate) fn start_time(&self) -> Option<PerformanceEntryTime> {
        self.start_time
    }

    /// Return the start time in the same domain exposed to this entry's global.
    pub(crate) fn start_time_for_sorting(&self) -> Option<PerformanceEntryTime> {
        match self.start_time {
            Some(PerformanceEntryTime::Host(_)) => {
                observable_start_time(self.start_time, self.accepts_host_timestamp())
            },
            start_time => start_time,
        }
    }

    fn accepts_host_timestamp(&self) -> bool {
        self.global()
            .document_clock()
            .require_surface(DocumentTimeSurface::HostTimestamp)
            .is_ok()
    }
}

fn observable_start_time(
    start_time: Option<PerformanceEntryTime>,
    accepts_host_timestamp: bool,
) -> Option<PerformanceEntryTime> {
    match start_time {
        Some(PerformanceEntryTime::Host(_)) if !accepts_host_timestamp => {
            Some(PerformanceEntryTime::Document(Duration::ZERO))
        },
        start_time => start_time,
    }
}

fn observable_duration(
    duration: PerformanceEntryDuration,
    accepts_host_timestamp: bool,
) -> DOMHighResTimeStamp {
    match duration {
        PerformanceEntryDuration::Host(_) if !accepts_host_timestamp => Finite::wrap(0.0),
        PerformanceEntryDuration::Host(duration) | PerformanceEntryDuration::Document(duration) => {
            duration.to_dom_high_res_time_stamp()
        },
    }
}

impl PerformanceEntryMethods<crate::DomTypeHolder> for PerformanceEntry {
    /// <https://w3c.github.io/performance-timeline/#dom-performanceentry-name>
    fn Name(&self) -> DOMString {
        self.name.clone()
    }

    /// <https://w3c.github.io/performance-timeline/#dom-performanceentry-entrytype>
    fn EntryType(&self) -> DOMString {
        DOMString::from(self.entry_type.as_str())
    }

    /// <https://w3c.github.io/performance-timeline/#dom-performanceentry-starttime>
    fn StartTime(&self, cx: &mut JSContext) -> DOMHighResTimeStamp {
        let performance = self.global().performance(cx);
        self.start_time.map_or_else(
            || Finite::wrap(0.0),
            |time| performance.entry_time_to_dom_high_res_time_stamp(time),
        )
    }

    /// <https://w3c.github.io/performance-timeline/#dom-performanceentry-duration>
    fn Duration(&self) -> DOMHighResTimeStamp {
        match self.duration {
            PerformanceEntryDuration::Host(_) => {
                observable_duration(self.duration, self.accepts_host_timestamp())
            },
            PerformanceEntryDuration::Document(duration) => duration.to_dom_high_res_time_stamp(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controlled_entries_suppress_host_time_without_hiding_document_time() {
        let duration = Duration::milliseconds(37);
        let host_time = CrossProcessInstant::epoch() + duration;

        assert_eq!(
            observable_start_time(Some(PerformanceEntryTime::Host(host_time)), false),
            Some(PerformanceEntryTime::Document(Duration::ZERO))
        );
        assert_eq!(
            observable_start_time(Some(PerformanceEntryTime::Document(duration)), false,),
            Some(PerformanceEntryTime::Document(duration))
        );

        assert_eq!(
            observable_duration(PerformanceEntryDuration::Host(duration), false),
            Finite::wrap(0.0)
        );
        assert_eq!(
            observable_duration(PerformanceEntryDuration::Document(duration), false),
            Finite::wrap(37.0)
        );
    }

    #[test]
    fn realtime_entries_preserve_existing_host_values() {
        let duration = Duration::milliseconds(37);
        let host_time = CrossProcessInstant::epoch() + duration;

        assert_eq!(
            observable_start_time(Some(PerformanceEntryTime::Host(host_time)), true),
            Some(PerformanceEntryTime::Host(host_time))
        );

        assert_eq!(
            observable_duration(PerformanceEntryDuration::Host(duration), true),
            Finite::wrap(37.0)
        );
    }
}
