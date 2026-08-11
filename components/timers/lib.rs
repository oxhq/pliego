/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A generic timer scheduler module that can be integrated into a crossbeam based event
//! loop or used to launch a background timer thread.

#![deny(unsafe_code)]

use std::cmp::{self, Ord};
use std::collections::{BTreeMap, BinaryHeap};
use std::fmt;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, after, never};
use malloc_size_of_derive::MallocSizeOf;
use serde::{Deserialize, Serialize};

/// Immutable execution limits for one controlled document-clock domain.
///
/// These counters observe work at engine-owned hooks. Pre-work classes can be rejected at admission;
/// mutation-record accounting is non-rejecting. CPU and host wall time require an
/// interrupt/watchdog boundary and are intentionally not represented here.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentExecutionLimits {
    /// Ordinary controlled event-loop turns that may begin.
    pub ordinary_tasks: u64,
    /// Individual microtask jobs that may begin.
    pub microtasks: u64,
    /// Rendering opportunities that may begin.
    pub rendering_opportunities: u64,
    /// Calls to Servo's central DOM mutation-record hook.
    pub mutations: u64,
    /// Maximum monotonic time that may elapse from the controlled clock's initial offset.
    ///
    /// `None` preserves the pre-API2 execution-ledger behavior.
    #[serde(default)]
    pub virtual_span: Option<DocumentTime>,
}

impl DocumentExecutionLimits {
    /// API 2 version-1 zero-configuration limits for the work classes owned by this slice.
    ///
    /// This is the exact engine-side value for an already normalized request; it does not claim
    /// that the production driver currently maps API 2 request fields into this configuration.
    pub const API2_V1_DEFAULT: Self = Self {
        ordinary_tasks: 100_000,
        microtasks: 1_000_000,
        rendering_opportunities: 10_000,
        mutations: 1_000_000,
        virtual_span: Some(DocumentTime::from_nanos(86_400_000_000_000)),
    };
}

/// A controlled execution counter with a policy limit in this slice.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub enum DocumentExecutionBudget {
    /// Ordinary controlled event-loop turns.
    OrdinaryTasks,
    /// Individual jobs removed from the microtask queue.
    Microtasks,
    /// Invocations of the HTML update-the-rendering algorithm.
    RenderingOpportunities,
    /// Calls to the central DOM mutation-record hook.
    MutationRecords,
}

/// A controlled execution counter, including evidence that is not yet a policy budget.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub enum DocumentExecutionCounter {
    /// Ordinary controlled event-loop turns.
    OrdinaryTasks,
    /// Individual jobs removed from the microtask queue.
    Microtasks,
    /// Invocations of the HTML update-the-rendering algorithm.
    RenderingOpportunities,
    /// Calls to the central DOM mutation-record hook.
    MutationRecords,
    /// Resource producer tickets begun by the ScriptThread-owned producer fence.
    OwnedResourceEvents,
}

/// The first terminal execution failure latched for one controlled session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub enum DocumentExecutionTerminal {
    /// Starting or observing another unit would exceed its configured limit.
    BudgetExceeded {
        /// Work class whose policy boundary was reached.
        budget: DocumentExecutionBudget,
        /// Configured maximum.
        limit: u64,
        /// One-based unit rejected before work, or non-rejecting record observed beyond its limit.
        observed: u64,
    },
    /// An evidence counter exhausted its exact integer representation.
    CounterOverflow(DocumentExecutionCounter),
    /// A guarded clock advance would exceed the configured monotonic span.
    VirtualSpanExceeded {
        /// Maximum elapsed offset from the controlled clock's initial time.
        limit: DocumentTime,
        /// Elapsed offset requested by the rejected advance.
        requested: DocumentTime,
    },
    /// A guarded clock advance requested an offset before this ledger's initial clock time.
    VirtualTimeBeforeInitial {
        /// Initial monotonic offset bound to this execution ledger.
        initial: DocumentTime,
        /// Earlier absolute offset requested by the rejected advance.
        requested: DocumentTime,
    },
}

/// Exact counters retained by one controlled execution ledger.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentExecutionCounters {
    /// Ordinary event-loop turns admitted before work.
    pub ordinary_tasks: u64,
    /// Individual microtask jobs admitted before work.
    pub microtasks: u64,
    /// Rendering opportunities admitted before work.
    pub rendering_opportunities: u64,
    /// Invocations of the central DOM mutation-record hook.
    pub mutations: u64,
    /// Resource producer tickets observed at the owned fence.
    ///
    /// This is deliberately not named or enforced as the API 2 post-readiness resource budget:
    /// this slice does not yet own a truthful readiness phase boundary.
    pub owned_resource_events: u64,
}

/// One mutex-consistent observation of controlled execution policy and use.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentExecutionObservation {
    /// Immutable limits installed before navigation.
    pub limits: DocumentExecutionLimits,
    /// Counters frozen when the first terminal failure is latched.
    pub counters: DocumentExecutionCounters,
    /// Sticky first failure, if a budget or representation boundary was reached.
    pub terminal: Option<DocumentExecutionTerminal>,
}

#[derive(Debug)]
struct DocumentExecutionLedgerState {
    limits: DocumentExecutionLimits,
    initial_time: DocumentTime,
    counters: DocumentExecutionCounters,
    terminal: Option<DocumentExecutionTerminal>,
}

/// A clonable, session-scoped ledger shared by controlled ScriptThread work hooks.
#[derive(Clone, Debug, MallocSizeOf)]
pub struct DocumentExecutionLedger {
    #[ignore_malloc_size_of = "The execution ledger is shared and measured by its owner"]
    inner: Arc<Mutex<DocumentExecutionLedgerState>>,
}

/// Proof that one execution ledger was nonterminal when this guard was acquired.
///
/// The ledger lock remains held for the guard's lifetime, so a concurrent execution hook cannot
/// latch a terminal between a guarded precondition check and its engine-owned mutation.
pub struct DocumentExecutionActiveGuard<'a> {
    _state: MutexGuard<'a, DocumentExecutionLedgerState>,
}

impl DocumentExecutionLedger {
    /// Create an empty ledger with immutable limits.
    pub fn new(limits: DocumentExecutionLimits) -> Self {
        Self::new_at(limits, DocumentTime::ZERO)
    }

    /// Create an empty ledger bound to one controlled clock's initial offset.
    pub fn new_at(limits: DocumentExecutionLimits, initial_time: DocumentTime) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DocumentExecutionLedgerState {
                limits,
                initial_time,
                counters: DocumentExecutionCounters::default(),
                terminal: None,
            })),
        }
    }

    /// Capture policy, counters, and the sticky first terminal failure under one lock.
    pub fn observation(&self) -> DocumentExecutionObservation {
        let state = self
            .inner
            .lock()
            .expect("document execution ledger poisoned");
        DocumentExecutionObservation {
            limits: state.limits,
            counters: state.counters,
            terminal: state.terminal,
        }
    }

    /// Lock this ledger only if it has not reached a terminal boundary.
    ///
    /// Keep the returned guard alive across the engine mutation that the terminal check protects.
    pub fn active_guard(
        &self,
    ) -> Result<DocumentExecutionActiveGuard<'_>, DocumentExecutionTerminal> {
        let state = self
            .inner
            .lock()
            .expect("document execution ledger poisoned");
        if let Some(terminal) = state.terminal {
            return Err(terminal);
        }
        Ok(DocumentExecutionActiveGuard { _state: state })
    }

    /// Lock this ledger only if an exact controlled advance remains within its virtual span.
    ///
    /// The returned guard is the same lock acquisition that latches an over-span terminal, so the
    /// caller can keep it alive through clock mutation without a check-then-act race.
    /// ScriptThread's conditional-advance path is the current production authority that combines
    /// this policy guard with scheduler validation; raw clock/scheduler mechanisms do not enforce
    /// the execution policy by themselves.
    pub fn active_guard_for_time(
        &self,
        requested: DocumentTime,
    ) -> Result<DocumentExecutionActiveGuard<'_>, DocumentExecutionTerminal> {
        let mut state = self
            .inner
            .lock()
            .expect("document execution ledger poisoned");
        if let Some(terminal) = state.terminal {
            return Err(terminal);
        }
        let Some(elapsed) = requested
            .as_nanos()
            .checked_sub(state.initial_time.as_nanos())
        else {
            let terminal = DocumentExecutionTerminal::VirtualTimeBeforeInitial {
                initial: state.initial_time,
                requested,
            };
            state.terminal = Some(terminal);
            return Err(terminal);
        };
        if let Some(limit) = state.limits.virtual_span {
            let requested_elapsed = DocumentTime::from_nanos(elapsed);
            if requested_elapsed > limit {
                let terminal = DocumentExecutionTerminal::VirtualSpanExceeded {
                    limit,
                    requested: requested_elapsed,
                };
                state.terminal = Some(terminal);
                return Err(terminal);
            }
        }
        Ok(DocumentExecutionActiveGuard { _state: state })
    }

    /// Admit one ordinary event-loop turn before any of its work runs.
    pub fn begin_ordinary_task(&self) -> Result<(), DocumentExecutionTerminal> {
        self.begin_budgeted(DocumentExecutionBudget::OrdinaryTasks)
    }

    /// Admit one microtask before invoking its job.
    pub fn begin_microtask(&self) -> Result<(), DocumentExecutionTerminal> {
        self.begin_budgeted(DocumentExecutionBudget::Microtasks)
    }

    /// Admit one rendering opportunity before invoking update-the-rendering.
    pub fn begin_rendering_opportunity(&self) -> Result<(), DocumentExecutionTerminal> {
        self.begin_budgeted(DocumentExecutionBudget::RenderingOpportunities)
    }

    /// Record one invocation of Servo's central DOM mutation-record hook.
    ///
    /// This is deliberately non-rejecting accounting: the observed record is included before the
    /// limit comparison, and a breach latches failure without suppressing or rolling back the DOM
    /// algorithm. Servo call sites do not all place this record hook on the same side of the
    /// underlying write, so this slice does not claim a universal post-write mutation generation.
    pub fn record_mutation_record(&self) {
        let mut state = self
            .inner
            .lock()
            .expect("document execution ledger poisoned");
        if state.terminal.is_some() {
            return;
        }
        let Some(observed) = state.counters.mutations.checked_add(1) else {
            state.terminal = Some(DocumentExecutionTerminal::CounterOverflow(
                DocumentExecutionCounter::MutationRecords,
            ));
            return;
        };
        state.counters.mutations = observed;
        let limit = state.limits.mutations;
        if observed > limit {
            state.terminal = Some(DocumentExecutionTerminal::BudgetExceeded {
                budget: DocumentExecutionBudget::MutationRecords,
                limit,
                observed,
            });
        }
    }

    /// Record one resource producer ticket at the ScriptThread-owned fence.
    ///
    /// This is evidence only until a later slice supplies the readiness transition needed to
    /// enforce API 2's specifically post-readiness resource budget.
    pub fn record_owned_resource_event(&self) {
        let mut state = self
            .inner
            .lock()
            .expect("document execution ledger poisoned");
        if state.terminal.is_some() {
            return;
        }
        let Some(observed) = state.counters.owned_resource_events.checked_add(1) else {
            state.terminal = Some(DocumentExecutionTerminal::CounterOverflow(
                DocumentExecutionCounter::OwnedResourceEvents,
            ));
            return;
        };
        state.counters.owned_resource_events = observed;
    }

    fn begin_budgeted(
        &self,
        budget: DocumentExecutionBudget,
    ) -> Result<(), DocumentExecutionTerminal> {
        let mut state = self
            .inner
            .lock()
            .expect("document execution ledger poisoned");
        if let Some(terminal) = state.terminal {
            return Err(terminal);
        }
        let (current, limit, counter_kind) = match budget {
            DocumentExecutionBudget::OrdinaryTasks => (
                state.counters.ordinary_tasks,
                state.limits.ordinary_tasks,
                DocumentExecutionCounter::OrdinaryTasks,
            ),
            DocumentExecutionBudget::Microtasks => (
                state.counters.microtasks,
                state.limits.microtasks,
                DocumentExecutionCounter::Microtasks,
            ),
            DocumentExecutionBudget::RenderingOpportunities => (
                state.counters.rendering_opportunities,
                state.limits.rendering_opportunities,
                DocumentExecutionCounter::RenderingOpportunities,
            ),
            DocumentExecutionBudget::MutationRecords => unreachable!(
                "mutation records use post-mutation accounting rather than pre-work admission"
            ),
        };
        let Some(observed) = current.checked_add(1) else {
            let terminal = DocumentExecutionTerminal::CounterOverflow(counter_kind);
            state.terminal = Some(terminal);
            return Err(terminal);
        };
        if observed > limit {
            let terminal = DocumentExecutionTerminal::BudgetExceeded {
                budget,
                limit,
                observed,
            };
            state.terminal = Some(terminal);
            return Err(terminal);
        }
        match budget {
            DocumentExecutionBudget::OrdinaryTasks => {
                state.counters.ordinary_tasks = observed;
            },
            DocumentExecutionBudget::Microtasks => {
                state.counters.microtasks = observed;
            },
            DocumentExecutionBudget::RenderingOpportunities => {
                state.counters.rendering_opportunities = observed;
            },
            DocumentExecutionBudget::MutationRecords => unreachable!(
                "mutation records use post-mutation accounting rather than pre-work admission"
            ),
        }
        Ok(())
    }
}

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
        initial_time_ns: u128,
        /// Unix time, in nanoseconds, corresponding to [`DocumentTime::ZERO`].
        ///
        /// This is deliberately configuration rather than host time so JavaScript wall time and
        /// monotonic DOM time advance together without consulting the host clock.
        #[serde(default)]
        unix_time_origin_ns: DocumentUnixTime,
        /// Optional execution policy installed before navigation. `None` preserves the U1-U3
        /// controlled-clock behavior without accounting or enforcement.
        #[serde(default)]
        execution_limits: Option<DocumentExecutionLimits>,
    },
}

/// An integer nanosecond offset in one document clock domain.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct DocumentTime(u128);

impl DocumentTime {
    /// The zero offset.
    pub const ZERO: Self = Self(0);

    /// Construct an offset from an integer number of nanoseconds.
    pub const fn from_nanos(nanos: u128) -> Self {
        Self(nanos)
    }

    /// Return this offset as integer nanoseconds.
    pub const fn as_nanos(self) -> u128 {
        self.0
    }

    /// Convert a duration without truncating or wrapping it.
    pub fn checked_from_duration(duration: Duration) -> Result<Self, DocumentClockError> {
        Ok(Self(duration.as_nanos()))
    }

    /// Convert this offset into a standard duration without truncating or wrapping it.
    pub fn checked_to_duration(self) -> Result<Duration, DocumentClockError> {
        let seconds =
            u64::try_from(self.0 / 1_000_000_000).map_err(|_| DocumentClockError::Overflow)?;
        let nanoseconds =
            u32::try_from(self.0 % 1_000_000_000).map_err(|_| DocumentClockError::Overflow)?;
        Ok(Duration::new(seconds, nanoseconds))
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

    /// Return the duration since an earlier offset without hiding a mismatched or future origin.
    pub fn checked_duration_since(self, earlier: Self) -> Result<Duration, DocumentClockError> {
        let elapsed =
            self.0
                .checked_sub(earlier.0)
                .ok_or(DocumentClockError::TimeMovedBackwards {
                    current: earlier,
                    requested: self,
                })?;
        Self(elapsed).checked_to_duration()
    }
}

/// A signed Unix timestamp in integer nanoseconds.
///
/// This is deliberately distinct from [`DocumentTime`]: wall time can precede the Unix epoch,
/// while the monotonic document-clock offset cannot be negative.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    Eq,
    MallocSizeOf,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
pub struct DocumentUnixTime(i128);

impl DocumentUnixTime {
    /// Construct a signed Unix timestamp from an integer number of nanoseconds.
    pub const fn from_nanos(nanos: i128) -> Self {
        Self(nanos)
    }

    /// Return this Unix timestamp as signed integer nanoseconds.
    pub const fn as_nanos(self) -> i128 {
        self.0
    }

    fn checked_add_document_time(self, time: DocumentTime) -> Result<Self, DocumentClockError> {
        let time = i128::try_from(time.as_nanos()).map_err(|_| DocumentClockError::Overflow)?;
        self.0
            .checked_add(time)
            .map(Self)
            .ok_or(DocumentClockError::Overflow)
    }
}

const TIME_CLIP_LIMIT_MILLISECONDS: i128 = 8_640_000_000_000_000;
const NANOSECONDS_PER_MILLISECOND: i128 = 1_000_000;

fn checked_javascript_date_time_microseconds(
    unix_time: DocumentUnixTime,
) -> Result<f64, DocumentClockError> {
    let nanoseconds = unix_time.as_nanos();
    let whole_microseconds = nanoseconds.div_euclid(1000);
    let sub_microsecond_nanoseconds = nanoseconds.rem_euclid(1000);
    let exact_candidate_microseconds =
        whole_microseconds as f64 + sub_microsecond_nanoseconds as f64 / 1000.0;
    let time_clip_limit_nanoseconds = TIME_CLIP_LIMIT_MILLISECONDS * NANOSECONDS_PER_MILLISECOND;
    if nanoseconds < -time_clip_limit_nanoseconds || nanoseconds > time_clip_limit_nanoseconds {
        // Return the spec result directly: rounding this exact out-of-range value through f64
        // microseconds could otherwise collapse it back onto a finite TimeClip boundary.
        return Ok(f64::NAN);
    }

    let expected_milliseconds = nanoseconds / NANOSECONDS_PER_MILLISECOND;
    let millisecond_anchor = expected_milliseconds as f64 * 1000.0;
    let mut best = None;
    for anchor in [exact_candidate_microseconds, millisecond_anchor] {
        for candidate in [anchor, f64_next_down(anchor), f64_next_up(anchor)] {
            let observed_time_clip = simulated_javascript_date_time_clip(candidate);
            if !observed_time_clip.is_finite() ||
                observed_time_clip as i128 != expected_milliseconds
            {
                continue;
            }
            let distance = (candidate - exact_candidate_microseconds).abs();
            if best.is_none_or(|(_, best_distance)| distance < best_distance) {
                best = Some((candidate, distance));
            }
        }
    }
    if let Some((candidate, _)) = best {
        return Ok(candidate);
    }

    Err(DocumentClockError::JavaScriptDatePrecisionLoss {
        unix_time,
        expected_milliseconds,
        observed_milliseconds: (exact_candidate_microseconds / 1000.0).trunc() as i128,
    })
}

fn simulated_javascript_date_time_clip(candidate_microseconds: f64) -> f64 {
    let milliseconds = candidate_microseconds / 1000.0;
    if !milliseconds.is_finite() || milliseconds.abs() > TIME_CLIP_LIMIT_MILLISECONDS as f64 {
        return f64::NAN;
    }
    milliseconds.trunc() + 0.0
}

fn f64_next_up(value: f64) -> f64 {
    if value.is_nan() || value == f64::INFINITY {
        return value;
    }
    if value == 0.0 {
        return f64::from_bits(1);
    }
    let bits = value.to_bits();
    f64::from_bits(if value > 0.0 { bits + 1 } else { bits - 1 })
}

fn f64_next_down(value: f64) -> f64 {
    if value.is_nan() || value == f64::NEG_INFINITY {
        return value;
    }
    if value == 0.0 {
        return f64::from_bits((1_u64 << 63) | 1);
    }
    let bits = value.to_bits();
    f64::from_bits(if value > 0.0 { bits - 1 } else { bits + 1 })
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
#[repr(u8)]
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
    /// A navigation that would replace the controlled WebView's one event-loop authority.
    CrossEventLoopNavigation,
    /// An auxiliary WebView that would share or replace a controlled event-loop authority.
    AuxiliaryWebView,
}

impl DocumentTimeSurface {
    const fn latch_value(self) -> u8 {
        self as u8 + 1
    }

    const fn from_latch_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::WindowTimers),
            2 => Some(Self::SameEventLoopIframe),
            3 => Some(Self::JavaScriptDate),
            4 => Some(Self::Performance),
            5 => Some(Self::HostTimestamp),
            6 => Some(Self::UpdateRendering),
            7 => Some(Self::AnimationFrame),
            8 => Some(Self::DocumentTimeline),
            9 => Some(Self::Worker),
            10 => Some(Self::Worklet),
            11 => Some(Self::CrossEventLoopIframe),
            12 => Some(Self::CrossEventLoopNavigation),
            13 => Some(Self::AuxiliaryWebView),
            _ => None,
        }
    }
}

/// A checked document-clock failure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    /// A conditional advance observed a different offset than its scheduling precondition.
    TimeChanged {
        /// Offset bound by the conditional scheduling precondition.
        expected: DocumentTime,
        /// Offset observed at the controlled clock's linearization point.
        observed: DocumentTime,
    },
    /// An integer nanosecond conversion or calculation overflowed.
    Overflow,
    /// The current clock slices do not yet control the requested observable surface.
    UnsupportedSurface(DocumentTimeSurface),
    /// SpiderMonkey's f64-microsecond Date hook cannot preserve this otherwise-valid TimeClip
    /// millisecond.
    JavaScriptDatePrecisionLoss {
        /// Exact signed wall time that could not be transported without rounding to another
        /// page-visible millisecond.
        unix_time: DocumentUnixTime,
        /// Millisecond that exact TimeClip truncation would produce.
        expected_milliseconds: i128,
        /// Millisecond produced after the f64-microsecond callback transport and truncation.
        observed_milliseconds: i128,
    },
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
            Self::TimeChanged { expected, observed } => write!(
                formatter,
                "document time changed from expected {}ns to {}ns",
                expected.as_nanos(),
                observed.as_nanos()
            ),
            Self::Overflow => {
                formatter.write_str("document time conversion or calculation overflowed")
            },
            Self::UnsupportedSurface(surface) => {
                write!(
                    formatter,
                    "controlled document time does not yet support {surface:?}"
                )
            },
            Self::JavaScriptDatePrecisionLoss {
                unix_time,
                expected_milliseconds,
                observed_milliseconds,
            } => write!(
                formatter,
                "controlled JavaScript Date wall time {}ns would round from exact {}ms to {}ms",
                unix_time.as_nanos(),
                expected_milliseconds,
                observed_milliseconds
            ),
        }
    }
}

impl std::error::Error for DocumentClockError {}

static NEXT_DOCUMENT_CLOCK_ID: AtomicU64 = AtomicU64::new(1);

/// Stable process-local identity for one shared document clock domain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentClockId(u64);

impl DocumentClockId {
    /// Return the process-local clock identifier.
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
struct ControlledClockState {
    now: DocumentTime,
    terminal: Option<DocumentClockError>,
}

enum DocumentClockInner {
    Realtime {
        origin: Instant,
    },
    Controlled {
        state: Mutex<ControlledClockState>,
        unix_time_origin_ns: DocumentUnixTime,
        unsupported_surface: AtomicU8,
        execution_ledger: Option<DocumentExecutionLedger>,
    },
}

/// One clonable clock shared by the timer scheduler and every same-event-loop Window realm.
#[derive(Clone, MallocSizeOf)]
pub struct DocumentClock {
    id: DocumentClockId,
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
        Self::try_new(configuration).expect("invalid document clock configuration")
    }

    /// Construct a clock after validating every controlled-time representation boundary.
    pub fn try_new(configuration: DocumentClockConfiguration) -> Result<Self, DocumentClockError> {
        let inner = match configuration {
            DocumentClockConfiguration::Realtime => DocumentClockInner::Realtime {
                origin: Instant::now(),
            },
            DocumentClockConfiguration::Controlled {
                initial_time_ns,
                unix_time_origin_ns,
                execution_limits,
            } => {
                let initial_time = DocumentTime::from_nanos(initial_time_ns);
                unix_time_origin_ns.checked_add_document_time(initial_time)?;
                DocumentClockInner::Controlled {
                    state: Mutex::new(ControlledClockState {
                        now: initial_time,
                        terminal: None,
                    }),
                    unix_time_origin_ns,
                    unsupported_surface: AtomicU8::new(0),
                    execution_ledger: execution_limits
                        .map(|limits| DocumentExecutionLedger::new_at(limits, initial_time)),
                }
            },
        };
        let id = DocumentClockId(
            NEXT_DOCUMENT_CLOCK_ID
                .fetch_update(
                    AtomicOrdering::Relaxed,
                    AtomicOrdering::Relaxed,
                    |current| current.checked_add(1),
                )
                .expect("document clock identifier exhausted"),
        );
        Ok(Self {
            id,
            inner: Arc::new(inner),
        })
    }

    /// Return the identity shared by every clone in this clock domain.
    pub const fn id(&self) -> DocumentClockId {
        self.id
    }

    /// Return whether this clock is explicitly controlled rather than host-driven.
    pub fn is_controlled(&self) -> bool {
        matches!(&*self.inner, DocumentClockInner::Controlled { .. })
    }

    /// Return the optional execution ledger installed with this controlled clock domain.
    pub fn execution_ledger(&self) -> Option<DocumentExecutionLedger> {
        match &*self.inner {
            DocumentClockInner::Realtime { .. } => None,
            DocumentClockInner::Controlled {
                execution_ledger, ..
            } => execution_ledger.clone(),
        }
    }

    /// Return the first sticky controlled-clock terminal, if one has been latched.
    pub fn terminal_error(&self) -> Option<DocumentClockError> {
        match &*self.inner {
            DocumentClockInner::Realtime { .. } => None,
            DocumentClockInner::Controlled { state, .. } => {
                state
                    .lock()
                    .expect("controlled document clock poisoned")
                    .terminal
            },
        }
    }

    /// Return the current offset, checking the host-duration conversion.
    pub fn try_now(&self) -> Result<DocumentTime, DocumentClockError> {
        match &*self.inner {
            DocumentClockInner::Realtime { origin } => {
                DocumentTime::checked_from_duration(origin.elapsed())
            },
            DocumentClockInner::Controlled { state, .. } => Ok(state
                .lock()
                .expect("controlled document clock poisoned")
                .now),
        }
    }

    /// Return the current offset.
    ///
    /// Realtime conversion remains checked even though the document-time representation is wider
    /// than a standard duration.
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
        let DocumentClockInner::Controlled {
            state,
            unix_time_origin_ns,
            ..
        } = &*self.inner
        else {
            return Err(DocumentClockError::RealtimeClock);
        };

        let mut state = state.lock().expect("controlled document clock poisoned");
        if let Some(terminal) = state.terminal {
            return Err(terminal);
        }
        if requested < state.now {
            return Err(DocumentClockError::TimeMovedBackwards {
                current: state.now,
                requested,
            });
        }
        if let Err(error) = unix_time_origin_ns.checked_add_document_time(requested) {
            state.terminal = Some(error);
            return Err(error);
        }
        state.now = requested;
        Ok(())
    }

    /// Atomically advance only if the clock still equals an observed scheduling precondition.
    pub fn advance_from_to(
        &self,
        expected: DocumentTime,
        requested: DocumentTime,
    ) -> Result<(), DocumentClockError> {
        let DocumentClockInner::Controlled {
            state,
            unix_time_origin_ns,
            ..
        } = &*self.inner
        else {
            return Err(DocumentClockError::RealtimeClock);
        };
        let mut state = state.lock().expect("controlled document clock poisoned");
        if let Some(terminal) = state.terminal {
            return Err(terminal);
        }
        if requested < expected {
            return Err(DocumentClockError::TimeMovedBackwards {
                current: expected,
                requested,
            });
        }
        if state.now != expected {
            return Err(DocumentClockError::TimeChanged {
                expected,
                observed: state.now,
            });
        }
        if let Err(error) = unix_time_origin_ns.checked_add_document_time(requested) {
            state.terminal = Some(error);
            return Err(error);
        }
        state.now = requested;
        Ok(())
    }

    /// Return Unix time at a point in a controlled clock domain, checking integer overflow.
    pub fn unix_time_ns_at(
        &self,
        time: DocumentTime,
    ) -> Result<DocumentUnixTime, DocumentClockError> {
        let DocumentClockInner::Controlled {
            unix_time_origin_ns,
            ..
        } = &*self.inner
        else {
            return Err(DocumentClockError::RealtimeClock);
        };
        unix_time_origin_ns.checked_add_document_time(time)
    }

    /// Return current Unix time in a controlled clock domain, checking integer overflow.
    pub fn unix_time_ns(&self) -> Result<DocumentUnixTime, DocumentClockError> {
        self.unix_time_ns_at(self.try_now()?)
    }

    /// Return the f64-microsecond value consumed by SpiderMonkey's JavaScript Date hook.
    ///
    /// A value inside ECMAScript's TimeClip domain is returned only when the hook's later
    /// millisecond conversion preserves exact truncation. Otherwise this method latches and
    /// returns a typed terminal before rounded wall time can reach the page. An exact value outside
    /// TimeClip returns NaN directly without latching: that is the specified page-visible result,
    /// not a representation failure.
    pub fn javascript_date_time_microseconds(&self) -> Result<f64, DocumentClockError> {
        self.require_surface(DocumentTimeSurface::JavaScriptDate)?;
        let DocumentClockInner::Controlled {
            state,
            unix_time_origin_ns,
            ..
        } = &*self.inner
        else {
            return Err(DocumentClockError::RealtimeClock);
        };
        let mut state = state.lock().expect("controlled document clock poisoned");
        if let Some(terminal) = state.terminal {
            return Err(terminal);
        }
        let unix_time = unix_time_origin_ns.checked_add_document_time(state.now)?;
        match checked_javascript_date_time_microseconds(unix_time) {
            Ok(microseconds) => Ok(microseconds),
            Err(error) => {
                state.terminal = Some(error);
                Err(error)
            },
        }
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
            if let DocumentClockInner::Controlled {
                unsupported_surface,
                ..
            } = &*self.inner
            {
                let _ = unsupported_surface.compare_exchange(
                    0,
                    surface.latch_value(),
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                );
            }
            Err(DocumentClockError::UnsupportedSurface(surface))
        }
    }

    /// Return the first unsupported observable surface touched by this controlled clock.
    ///
    /// The latch is monotonic: once an unsupported producer or host timestamp is observed, later
    /// control-plane observations keep failing closed instead of allowing a subsequent quiet turn
    /// to hide it.
    pub fn unsupported_surface(&self) -> Option<DocumentTimeSurface> {
        match &*self.inner {
            DocumentClockInner::Realtime { .. } => None,
            DocumentClockInner::Controlled {
                unsupported_surface,
                ..
            } => DocumentTimeSurface::from_latch_value(
                unsupported_surface.load(AtomicOrdering::Acquire),
            ),
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
            .checked_add(deadline.checked_to_duration()?)
            .map(Some)
            .ok_or(DocumentClockError::Overflow)
    }
}

const DOCUMENT_PRODUCER_KIND_COUNT: usize = 5;
static NEXT_DOCUMENT_PRODUCER_FENCE_ID: AtomicU64 = AtomicU64::new(1);

/// A class of asynchronous work that can later affect a document's rendered result.
///
/// The fence deliberately contains only producers owned by a single `ScriptThread`. Workers,
/// worklets, and cross-event-loop frames remain unsupported controlled-time surfaces rather than
/// being silently treated as idle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
#[repr(usize)]
pub enum DocumentProducerKind {
    /// A task queued through a Window's task manager.
    Task = 0,
    /// A document or navigation resource fetch.
    Resource = 1,
    /// A logical web-font load, including source fallback.
    Font = 2,
    /// An image-cache or vector-rasterization completion listener.
    Image = 3,
    /// An outbound one-shot callback whose reply will be handed off to a Window task.
    ExternalCallback = 4,
}

impl DocumentProducerKind {
    const fn index(self) -> usize {
        self as usize
    }
}

/// The stable enqueue identity assigned to one producer ticket.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct DocumentProducerSequence(u64);

impl DocumentProducerSequence {
    /// Return the global enqueue sequence within this fence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable identity of one ScriptThread-owned producer fence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentProducerFenceId(u64);

impl DocumentProducerFenceId {
    /// Return the process-local fence identifier.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Serializable identity for an explicitly acknowledged producer lease.
///
/// The ID carries no synchronization state. Its owning [`DocumentProducerFence`] validates that
/// the sequence is still live and belongs to the expected producer class before completing it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentProducerLeaseId {
    fence_id: DocumentProducerFenceId,
    sequence: DocumentProducerSequence,
    kind: DocumentProducerKind,
}

impl DocumentProducerLeaseId {
    /// Return the stable global enqueue sequence for this lease.
    pub const fn sequence(self) -> DocumentProducerSequence {
        self.sequence
    }

    /// Return the producer class registered for this lease.
    pub const fn kind(self) -> DocumentProducerKind {
        self.kind
    }
}

/// Enqueue, completion, and pending watermarks for one producer class.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentProducerWatermark {
    enqueued: u64,
    completed: u64,
    pending: u64,
}

impl DocumentProducerWatermark {
    /// Number of tickets ever enqueued for this producer class.
    pub const fn enqueued(self) -> u64 {
        self.enqueued
    }

    /// Number of tickets whose producer callback or task has completed.
    pub const fn completed(self) -> u64 {
        self.completed
    }

    /// Number of currently live producer tickets.
    pub const fn pending(self) -> u64 {
        self.pending
    }
}

/// One mutex-consistent snapshot of all participating document producers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, PartialEq, Serialize)]
pub struct DocumentProducerSnapshot {
    revision: u64,
    enqueued: u64,
    completed: u64,
    pending: u64,
    by_kind: [DocumentProducerWatermark; DOCUMENT_PRODUCER_KIND_COUNT],
}

impl DocumentProducerSnapshot {
    /// A mutation sequence incremented for every enqueue and completion.
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Global number of producer tickets ever enqueued.
    pub const fn enqueued(self) -> u64 {
        self.enqueued
    }

    /// Global number of producer tickets completed.
    pub const fn completed(self) -> u64 {
        self.completed
    }

    /// Global number of live producer tickets.
    pub const fn pending(self) -> u64 {
        self.pending
    }

    /// Return whether no participating producer ticket is live.
    pub const fn is_empty(self) -> bool {
        self.pending == 0
    }

    /// Return watermarks for one producer class.
    pub const fn for_kind(self, kind: DocumentProducerKind) -> DocumentProducerWatermark {
        self.by_kind[kind.index()]
    }
}

#[derive(Default)]
struct DocumentProducerFenceState {
    revision: u64,
    enqueued: u64,
    completed: u64,
    pending: u64,
    by_kind: [DocumentProducerWatermark; DOCUMENT_PRODUCER_KIND_COUNT],
    active_leases: BTreeMap<DocumentProducerSequence, DocumentProducerKind>,
}

impl DocumentProducerFenceState {
    fn snapshot(&self) -> DocumentProducerSnapshot {
        DocumentProducerSnapshot {
            revision: self.revision,
            enqueued: self.enqueued,
            completed: self.completed,
            pending: self.pending,
            by_kind: self.by_kind,
        }
    }
}

/// A checked producer-fence failure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentProducerFenceError {
    /// A sequence or watermark would exceed the `u64` representation.
    CounterOverflow,
    /// No ScriptThread microtask checkpoint has completed yet.
    CheckpointNotCompleted,
    /// An observation reused or moved backwards from an already observed checkpoint.
    StaleCheckpoint {
        /// The last checkpoint accepted by this observer.
        previous: DocumentProducerCheckpoint,
        /// The rejected checkpoint.
        observed: DocumentProducerCheckpoint,
    },
    /// A lease acknowledgement did not name a live lease on this fence.
    UnknownLease(DocumentProducerLeaseId),
}

impl fmt::Display for DocumentProducerFenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for DocumentProducerFenceError {}

/// A producer snapshot changed before a conditional action reached its linearization point.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentProducerSnapshotMismatch {
    observed: Box<DocumentProducerSnapshot>,
}

impl DocumentProducerSnapshotMismatch {
    /// Return the producer state observed under the fence lock.
    pub fn observed(&self) -> DocumentProducerSnapshot {
        *self.observed
    }
}

/// A clonable, linearizable fence shared by all participating producers on one ScriptThread.
///
/// Enqueue and completion mutate one locked state so a snapshot cannot combine watermarks from
/// different instants. Every successful enqueue reserves enough revision space for its eventual
/// RAII completion, making overflow a checked enqueue failure rather than a fallible destructor.
#[derive(Clone, MallocSizeOf)]
pub struct DocumentProducerFence {
    fence_id: DocumentProducerFenceId,
    #[ignore_malloc_size_of = "The producer state is shared and measured by its owner"]
    inner: Arc<Mutex<DocumentProducerFenceState>>,
    #[ignore_malloc_size_of = "The optional execution ledger is shared with the document clock"]
    execution_ledger: Option<DocumentExecutionLedger>,
}

impl Default for DocumentProducerFence {
    fn default() -> Self {
        Self::with_execution_ledger(None)
    }
}

impl DocumentProducerFence {
    /// Construct a producer fence optionally bound to one controlled execution ledger.
    pub fn with_execution_ledger(execution_ledger: Option<DocumentExecutionLedger>) -> Self {
        let fence_id = DocumentProducerFenceId(
            NEXT_DOCUMENT_PRODUCER_FENCE_ID
                .fetch_update(
                    AtomicOrdering::Relaxed,
                    AtomicOrdering::Relaxed,
                    |current| current.checked_add(1),
                )
                .expect("document producer fence identifier exhausted"),
        );
        Self {
            fence_id,
            inner: Arc::new(Mutex::new(DocumentProducerFenceState::default())),
            execution_ledger,
        }
    }

    /// Begin one producer operation and return its stable RAII ticket.
    pub fn begin(
        &self,
        kind: DocumentProducerKind,
    ) -> Result<DocumentProducerGuard, DocumentProducerFenceError> {
        let mut state = self.inner.lock().expect("document producer fence poisoned");
        let index = kind.index();

        let revision = state
            .revision
            .checked_add(1)
            .ok_or(DocumentProducerFenceError::CounterOverflow)?;
        let enqueued = state
            .enqueued
            .checked_add(1)
            .ok_or(DocumentProducerFenceError::CounterOverflow)?;
        let pending = state
            .pending
            .checked_add(1)
            .ok_or(DocumentProducerFenceError::CounterOverflow)?;
        let kind_enqueued = state.by_kind[index]
            .enqueued
            .checked_add(1)
            .ok_or(DocumentProducerFenceError::CounterOverflow)?;
        let kind_pending = state.by_kind[index]
            .pending
            .checked_add(1)
            .ok_or(DocumentProducerFenceError::CounterOverflow)?;

        // Each live ticket will consume one more revision when its guard is dropped.
        revision
            .checked_add(pending)
            .ok_or(DocumentProducerFenceError::CounterOverflow)?;

        state.revision = revision;
        state.enqueued = enqueued;
        state.pending = pending;
        state.by_kind[index].enqueued = kind_enqueued;
        state.by_kind[index].pending = kind_pending;

        let lease_id = DocumentProducerLeaseId {
            fence_id: self.fence_id,
            sequence: DocumentProducerSequence(enqueued),
            kind,
        };
        let previous = state.active_leases.insert(lease_id.sequence, kind);
        debug_assert!(previous.is_none());

        drop(state);
        if kind == DocumentProducerKind::Resource &&
            let Some(ledger) = &self.execution_ledger
        {
            ledger.record_owned_resource_event();
        }

        Ok(DocumentProducerGuard {
            fence: self.clone(),
            lease_id: Some(lease_id),
        })
    }

    /// Capture all producer watermarks under one lock acquisition.
    pub fn snapshot(&self) -> DocumentProducerSnapshot {
        self.inner
            .lock()
            .expect("document producer fence poisoned")
            .snapshot()
    }

    /// Run an action only while the producer snapshot still exactly matches `expected`.
    ///
    /// Producer enqueue and completion remain blocked until `action` returns. Callers must keep
    /// the action small and must not dispatch callbacks that can acquire another producer lease.
    pub fn with_matching_snapshot<T>(
        &self,
        expected: DocumentProducerSnapshot,
        action: impl FnOnce() -> T,
    ) -> Result<T, DocumentProducerSnapshotMismatch> {
        let state = self.inner.lock().expect("document producer fence poisoned");
        let observed = state.snapshot();
        if observed != expected {
            return Err(DocumentProducerSnapshotMismatch {
                observed: Box::new(observed),
            });
        }
        let result = action();
        drop(state);
        Ok(result)
    }

    /// Return the stable identity bound to this ScriptThread fence.
    pub const fn id(&self) -> DocumentProducerFenceId {
        self.fence_id
    }

    /// Complete a live lease after its terminal message has been handled.
    pub fn complete_lease(
        &self,
        lease_id: DocumentProducerLeaseId,
    ) -> Result<(), DocumentProducerFenceError> {
        if lease_id.fence_id != self.fence_id {
            return Err(DocumentProducerFenceError::UnknownLease(lease_id));
        }
        let mut state = self.inner.lock().expect("document producer fence poisoned");
        if state.active_leases.get(&lease_id.sequence) != Some(&lease_id.kind) {
            return Err(DocumentProducerFenceError::UnknownLease(lease_id));
        }
        state.active_leases.remove(&lease_id.sequence);
        let index = lease_id.kind.index();

        // `begin` reserves this revision and every completion count is bounded by its matching
        // checked enqueue count, so these failures indicate an internal fence invariant bug.
        state.revision = state
            .revision
            .checked_add(1)
            .expect("reserved document producer completion revision exhausted");
        state.completed = state
            .completed
            .checked_add(1)
            .expect("document producer completion exceeded enqueue watermark");
        state.pending = state
            .pending
            .checked_sub(1)
            .expect("document producer ticket completed twice");
        state.by_kind[index].completed = state.by_kind[index]
            .completed
            .checked_add(1)
            .expect("document producer kind completion exceeded enqueue watermark");
        state.by_kind[index].pending = state.by_kind[index]
            .pending
            .checked_sub(1)
            .expect("document producer kind ticket completed twice");
        Ok(())
    }
}

/// A live producer ticket. Dropping it records completion after the guarded callback or task.
pub struct DocumentProducerGuard {
    fence: DocumentProducerFence,
    lease_id: Option<DocumentProducerLeaseId>,
}

impl DocumentProducerGuard {
    /// Return this ticket's stable global enqueue sequence.
    pub const fn sequence(&self) -> DocumentProducerSequence {
        self.lease_id
            .expect("a detached document producer guard has no sequence")
            .sequence
    }

    /// Transfer completion responsibility to a serializable lease ID.
    pub fn into_lease_id(mut self) -> DocumentProducerLeaseId {
        self.lease_id
            .take()
            .expect("document producer guard detached twice")
    }
}

impl Drop for DocumentProducerGuard {
    fn drop(&mut self) {
        if let Some(lease_id) = self.lease_id.take() {
            self.fence
                .complete_lease(lease_id)
                .expect("live document producer guard named an unknown lease");
        }
    }
}

/// A monotonically increasing token created only after a ScriptThread microtask checkpoint.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    Eq,
    MallocSizeOf,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
pub struct DocumentProducerCheckpoint(u64);

impl DocumentProducerCheckpoint {
    /// The initial token before any checkpoint has completed.
    pub const ZERO: Self = Self(0);

    /// Return the underlying checkpoint sequence.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Advance to the next checkpoint without wrapping.
    pub fn checked_next(self) -> Result<Self, DocumentProducerFenceError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DocumentProducerFenceError::CounterOverflow)
    }
}

/// Mechanical result of one fenced observation after a microtask checkpoint.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentProducerObservation {
    /// At least one producer is live.
    Busy(DocumentProducerSnapshot),
    /// This is the first empty snapshot at this revision.
    FirstEmpty(DocumentProducerSnapshot),
    /// Two fresh checkpoints observed the same empty producer revision.
    StableEmpty(DocumentProducerSnapshot),
}

/// Per-driver state that qualifies an empty producer fence only across two fresh checkpoints.
#[derive(Default)]
pub struct DocumentProducerObserver {
    last_checkpoint: Option<DocumentProducerCheckpoint>,
    last_empty: Option<DocumentProducerSnapshot>,
}

impl DocumentProducerObserver {
    /// Observe the fence after `checkpoint` and mechanically qualify stable emptiness.
    pub fn observe(
        &mut self,
        fence: &DocumentProducerFence,
        checkpoint: DocumentProducerCheckpoint,
    ) -> Result<DocumentProducerObservation, DocumentProducerFenceError> {
        if checkpoint == DocumentProducerCheckpoint::ZERO {
            return Err(DocumentProducerFenceError::CheckpointNotCompleted);
        }
        if let Some(previous) = self.last_checkpoint &&
            checkpoint <= previous
        {
            return Err(DocumentProducerFenceError::StaleCheckpoint {
                previous,
                observed: checkpoint,
            });
        }
        self.last_checkpoint = Some(checkpoint);

        let snapshot = fence.snapshot();
        if !snapshot.is_empty() {
            self.last_empty = None;
            return Ok(DocumentProducerObservation::Busy(snapshot));
        }

        let stable = self.last_empty == Some(snapshot);
        self.last_empty = Some(snapshot);
        if stable {
            Ok(DocumentProducerObservation::StableEmpty(snapshot))
        } else {
            Ok(DocumentProducerObservation::FirstEmpty(snapshot))
        }
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
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, MallocSizeOf, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct TimerId(u64);

impl TimerId {
    /// Return the stable scheduler insertion sequence.
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

/// The finite deadline exposed by a controlled scheduler.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TimerDeadlineSnapshot {
    /// Stable identity and insertion order for this event.
    pub id: TimerId,
    /// Absolute integer-nanosecond offset in the document clock.
    pub deadline: DocumentTime,
}

/// A checked scheduler/control failure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

    /// Require one exact finite deadline snapshot without mutating the scheduler or clock.
    pub fn validate_deadline_snapshot(
        &self,
        expected: TimerDeadlineSnapshot,
    ) -> Result<(), TimerControlError> {
        let observed = self.finite_deadline_snapshot()?;
        if observed != Some(expected) {
            return Err(TimerControlError::StaleDeadline { expected, observed });
        }
        Ok(())
    }

    /// Advance the shared controlled clock monotonically without activating a timer.
    pub fn advance_controlled_time_to(&self, now: DocumentTime) -> Result<(), TimerControlError> {
        self.clock.advance_to(now).map_err(Into::into)
    }

    /// Validate one fresh finite deadline, advance to it, and activate exactly that event.
    ///
    /// Validation happens before the clock mutates so a canceled or replaced snapshot cannot move
    /// controlled time and then fail stale.
    pub fn advance_to_and_activate(
        &mut self,
        expected: TimerDeadlineSnapshot,
    ) -> Result<(), TimerControlError> {
        self.validate_and_advance_to(expected)?;
        self.activate_due_timer(expected)
    }

    /// Validate one fresh finite deadline and advance to it without dispatching its callback.
    ///
    /// This seam lets a controller hold its producer-fence snapshot lock only across validation
    /// and clock mutation, then release the lock before activating callback-producing work.
    pub fn validate_and_advance_to(
        &self,
        expected: TimerDeadlineSnapshot,
    ) -> Result<(), TimerControlError> {
        self.validate_deadline_snapshot(expected)?;
        self.clock.advance_to(expected.deadline).map_err(Into::into)
    }

    /// Validate one deadline and require the previously observed clock offset at one linearization
    /// point.
    pub fn validate_and_advance_from(
        &self,
        expected_now: DocumentTime,
        expected: TimerDeadlineSnapshot,
    ) -> Result<(), TimerControlError> {
        self.validate_deadline_snapshot(expected)?;
        self.clock
            .advance_from_to(expected_now, expected.deadline)
            .map_err(Into::into)
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
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;

    use super::*;

    fn controlled_clock(initial_time_ns: u128) -> DocumentClock {
        DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: None,
        })
    }

    fn execution_limits() -> DocumentExecutionLimits {
        DocumentExecutionLimits {
            ordinary_tasks: 2,
            microtasks: 3,
            rendering_opportunities: 1,
            mutations: 1,
            virtual_span: None,
        }
    }

    #[test]
    fn execution_ledger_is_opt_in_and_never_exists_for_realtime() {
        assert!(DocumentClock::default().execution_ledger().is_none());
        assert!(controlled_clock(0).execution_ledger().is_none());

        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: Some(execution_limits()),
        });
        assert_eq!(
            clock.execution_ledger().unwrap().observation(),
            DocumentExecutionObservation {
                limits: execution_limits(),
                counters: DocumentExecutionCounters::default(),
                terminal: None,
            }
        );
    }

    #[test]
    fn api2_execution_defaults_include_the_exact_one_day_virtual_span() {
        assert_eq!(
            DocumentExecutionLimits::API2_V1_DEFAULT.virtual_span,
            Some(DocumentTime::from_nanos(86_400_000_000_000))
        );
        assert_eq!(
            DocumentExecutionLimits::API2_V1_DEFAULT
                .virtual_span
                .unwrap()
                .checked_to_duration(),
            Ok(Duration::from_millis(86_400_000))
        );
    }

    #[test]
    fn prework_budget_latches_first_breach_without_executing_the_rejected_unit() {
        let ledger = DocumentExecutionLedger::new(execution_limits());
        assert!(ledger.begin_ordinary_task().is_ok());
        assert!(ledger.begin_ordinary_task().is_ok());

        let first = ledger.begin_ordinary_task().unwrap_err();
        assert_eq!(
            first,
            DocumentExecutionTerminal::BudgetExceeded {
                budget: DocumentExecutionBudget::OrdinaryTasks,
                limit: 2,
                observed: 3,
            }
        );
        assert_eq!(ledger.begin_microtask(), Err(first));
        assert_eq!(
            ledger.observation().counters,
            DocumentExecutionCounters {
                ordinary_tasks: 2,
                ..DocumentExecutionCounters::default()
            }
        );
        assert_eq!(ledger.observation().terminal, Some(first));
    }

    #[test]
    fn terminal_active_guard_prevents_controlled_clock_mutation() {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: Some(execution_limits()),
        });
        let ledger = clock.execution_ledger().unwrap();
        let mut scheduler = TimerScheduler::with_clock(clock.clone());
        scheduler.schedule_timer(TimerEventRequest {
            callback: Box::new(|| {}),
            duration: Duration::from_nanos(10),
        });
        let deadline = scheduler
            .finite_deadline_snapshot()
            .unwrap()
            .expect("fixture has one controlled timer");

        ledger.begin_ordinary_task().unwrap();
        ledger.begin_ordinary_task().unwrap();
        let terminal = ledger.begin_ordinary_task().unwrap_err();
        let guarded = ledger
            .active_guard()
            .map(|_guard| scheduler.validate_and_advance_to(deadline));

        assert!(matches!(guarded, Err(observed) if observed == terminal));
        assert_eq!(clock.now(), DocumentTime::ZERO);
        assert_eq!(
            scheduler.finite_deadline_snapshot().unwrap(),
            Some(deadline)
        );
    }

    #[test]
    fn virtual_span_terminal_is_exact_sticky_and_precedes_clock_mutation() {
        const API2_VIRTUAL_SPAN_MAX_MS: u64 = 9_007_199_254_740_991;
        let span = DocumentTime::from_nanos(
            u128::from(API2_VIRTUAL_SPAN_MAX_MS)
                .checked_mul(1_000_000)
                .unwrap(),
        );
        let mut limits = execution_limits();
        limits.virtual_span = Some(span);

        let exact_clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: Some(limits),
        });
        let exact_ledger = exact_clock.execution_ledger().unwrap();
        let mut exact_scheduler = TimerScheduler::with_clock(exact_clock.clone());
        exact_scheduler.schedule_timer(TimerEventRequest {
            callback: Box::new(|| {}),
            duration: Duration::from_millis(API2_VIRTUAL_SPAN_MAX_MS),
        });
        let exact_deadline = exact_scheduler.finite_deadline_snapshot().unwrap().unwrap();
        let exact_guard = exact_ledger
            .active_guard_for_time(exact_deadline.deadline)
            .unwrap();
        exact_scheduler
            .validate_and_advance_from(DocumentTime::ZERO, exact_deadline)
            .unwrap();
        drop(exact_guard);
        assert_eq!(exact_clock.now(), span);
        assert_eq!(exact_ledger.observation().terminal, None);

        let over_clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: Some(limits),
        });
        let over_ledger = over_clock.execution_ledger().unwrap();
        let mut over_scheduler = TimerScheduler::with_clock(over_clock.clone());
        over_scheduler.schedule_timer(TimerEventRequest {
            callback: Box::new(|| {}),
            duration: Duration::from_millis(API2_VIRTUAL_SPAN_MAX_MS + 1),
        });
        let over_deadline = over_scheduler.finite_deadline_snapshot().unwrap().unwrap();
        let terminal = DocumentExecutionTerminal::VirtualSpanExceeded {
            limit: span,
            requested: DocumentTime::from_nanos(span.as_nanos() + 1_000_000),
        };
        assert!(matches!(
            over_ledger.active_guard_for_time(over_deadline.deadline),
            Err(observed) if observed == terminal
        ));
        assert_eq!(over_clock.now(), DocumentTime::ZERO);
        assert_eq!(
            over_scheduler.finite_deadline_snapshot().unwrap(),
            Some(over_deadline)
        );
        assert!(matches!(
            over_ledger.active_guard(),
            Err(observed) if observed == terminal
        ));
        assert_eq!(over_ledger.observation().terminal, Some(terminal));

        let initial = DocumentTime::from_nanos(100);
        let before_initial_ledger = DocumentExecutionLedger::new_at(limits, initial);
        let requested = DocumentTime::from_nanos(99);
        let before_initial =
            DocumentExecutionTerminal::VirtualTimeBeforeInitial { initial, requested };
        assert!(matches!(
            before_initial_ledger.active_guard_for_time(requested),
            Err(observed) if observed == before_initial
        ));
        assert!(matches!(
            before_initial_ledger.active_guard_for_time(initial),
            Err(observed) if observed == before_initial
        ));
        assert_eq!(
            before_initial_ledger.observation().terminal,
            Some(before_initial)
        );
    }

    #[test]
    fn mutation_breach_counts_the_nonrejecting_over_limit_record() {
        let ledger = DocumentExecutionLedger::new(execution_limits());
        ledger.record_mutation_record();
        assert_eq!(ledger.observation().terminal, None);

        ledger.record_mutation_record();
        assert_eq!(ledger.observation().counters.mutations, 2);
        assert_eq!(
            ledger.observation().terminal,
            Some(DocumentExecutionTerminal::BudgetExceeded {
                budget: DocumentExecutionBudget::MutationRecords,
                limit: 1,
                observed: 2,
            })
        );
    }

    #[test]
    fn owned_resource_events_are_counted_only_at_successful_central_fence_admission() {
        let ledger = DocumentExecutionLedger::new(execution_limits());
        let fence = DocumentProducerFence::with_execution_ledger(Some(ledger.clone()));
        let resource = fence.begin(DocumentProducerKind::Resource).unwrap();
        let task = fence.begin(DocumentProducerKind::Task).unwrap();

        assert_eq!(ledger.observation().counters.owned_resource_events, 1);
        drop((resource, task));
    }

    #[test]
    fn external_callback_error_drop_and_task_handoff_never_false_green() {
        let fence = DocumentProducerFence::default();

        let failed_callback = fence.begin(DocumentProducerKind::ExternalCallback).unwrap();
        assert_eq!(fence.snapshot().pending(), 1);
        drop(failed_callback);
        assert!(fence.snapshot().is_empty());

        let callback = fence.begin(DocumentProducerKind::ExternalCallback).unwrap();
        let task = fence.begin(DocumentProducerKind::Task).unwrap();
        let during_handoff = fence.snapshot();
        assert_eq!(during_handoff.pending(), 2);
        assert_eq!(
            during_handoff
                .for_kind(DocumentProducerKind::ExternalCallback)
                .pending(),
            1
        );
        assert_eq!(
            during_handoff
                .for_kind(DocumentProducerKind::Task)
                .pending(),
            1
        );

        drop(callback);
        assert_eq!(fence.snapshot().pending(), 1);
        drop(task);
        assert!(fence.snapshot().is_empty());
    }

    #[test]
    fn controlled_clock_latches_the_first_unsupported_surface() {
        let clock = controlled_clock(0);
        assert_eq!(clock.unsupported_surface(), None);
        assert_eq!(
            clock.require_surface(DocumentTimeSurface::HostTimestamp),
            Err(DocumentClockError::UnsupportedSurface(
                DocumentTimeSurface::HostTimestamp
            ))
        );
        assert_eq!(
            clock.require_surface(DocumentTimeSurface::Worker),
            Err(DocumentClockError::UnsupportedSurface(
                DocumentTimeSurface::Worker
            ))
        );
        assert_eq!(
            clock.unsupported_surface(),
            Some(DocumentTimeSurface::HostTimestamp)
        );
    }

    #[test]
    fn document_clock_identity_is_shared_only_within_one_domain() {
        let clock = controlled_clock(0);
        let clone = clock.clone();
        let other = controlled_clock(0);

        assert_eq!(clock.id(), clone.id());
        assert_ne!(clock.id(), other.id());
    }

    #[test]
    fn matching_snapshot_lock_excludes_new_producers_until_action_finishes() {
        let fence = DocumentProducerFence::default();
        let expected = fence.snapshot();
        let action_fence = fence.clone();
        let producer_fence = fence.clone();
        let (locked_sender, locked_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let (attempt_sender, attempt_receiver) = mpsc::channel();
        let (producer_sender, producer_receiver) = mpsc::channel();

        let action = thread::spawn(move || {
            action_fence
                .with_matching_snapshot(expected, || {
                    locked_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                })
                .unwrap();
        });
        locked_receiver.recv().unwrap();
        let producer = thread::spawn(move || {
            attempt_sender.send(()).unwrap();
            let guard = producer_fence
                .begin(DocumentProducerKind::Resource)
                .unwrap();
            producer_sender.send(()).unwrap();
            drop(guard);
        });

        attempt_receiver.recv().unwrap();
        assert!(
            producer_receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        release_sender.send(()).unwrap();
        action.join().unwrap();
        producer_receiver.recv().unwrap();
        producer.join().unwrap();
        assert_eq!(fence.snapshot().revision(), 2);
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
    fn producer_fence_requires_two_fresh_unchanged_empty_checkpoints() {
        let fence = DocumentProducerFence::default();
        let mut observer = DocumentProducerObserver::default();
        let first = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let second = first.checked_next().unwrap();

        assert_eq!(
            observer.observe(&fence, DocumentProducerCheckpoint::ZERO),
            Err(DocumentProducerFenceError::CheckpointNotCompleted)
        );
        assert!(matches!(
            observer.observe(&fence, first),
            Ok(DocumentProducerObservation::FirstEmpty(snapshot)) if snapshot.is_empty()
        ));
        assert_eq!(
            observer.observe(&fence, first),
            Err(DocumentProducerFenceError::StaleCheckpoint {
                previous: first,
                observed: first,
            })
        );
        assert!(matches!(
            observer.observe(&fence, second),
            Ok(DocumentProducerObservation::StableEmpty(snapshot)) if snapshot.is_empty()
        ));
    }

    #[test]
    fn producer_activity_between_empty_checkpoints_restarts_the_two_fence() {
        let fence = DocumentProducerFence::default();
        let mut observer = DocumentProducerObserver::default();
        let first = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let second = first.checked_next().unwrap();
        let third = second.checked_next().unwrap();

        assert!(matches!(
            observer.observe(&fence, first),
            Ok(DocumentProducerObservation::FirstEmpty(_))
        ));
        let guard = fence.begin(DocumentProducerKind::Resource).unwrap();
        drop(guard);
        assert!(matches!(
            observer.observe(&fence, second),
            Ok(DocumentProducerObservation::FirstEmpty(snapshot))
                if snapshot.revision() == 2 && snapshot.enqueued() == 1 && snapshot.completed() == 1
        ));
        assert!(matches!(
            observer.observe(&fence, third),
            Ok(DocumentProducerObservation::StableEmpty(_))
        ));
    }

    #[test]
    fn producer_watermarks_are_stable_when_tickets_complete_out_of_order() {
        let fence = DocumentProducerFence::default();
        let first = fence.begin(DocumentProducerKind::Task).unwrap();
        let second = fence.begin(DocumentProducerKind::Task).unwrap();
        let image = fence.begin(DocumentProducerKind::Image).unwrap();

        assert_eq!(first.sequence().get(), 1);
        assert_eq!(second.sequence().get(), 2);
        assert_eq!(image.sequence().get(), 3);
        assert_eq!(fence.snapshot().pending(), 3);

        drop(second);
        let middle = fence.snapshot();
        assert_eq!(middle.revision(), 4);
        assert_eq!(middle.pending(), 2);
        assert_eq!(
            middle.for_kind(DocumentProducerKind::Task),
            DocumentProducerWatermark {
                enqueued: 2,
                completed: 1,
                pending: 1,
            }
        );

        drop(first);
        drop(image);
        let complete = fence.snapshot();
        assert!(complete.is_empty());
        assert_eq!(complete.revision(), 6);
        assert_eq!(complete.enqueued(), complete.completed());
        for kind in [
            DocumentProducerKind::Task,
            DocumentProducerKind::Resource,
            DocumentProducerKind::Font,
            DocumentProducerKind::Image,
            DocumentProducerKind::ExternalCallback,
        ] {
            let watermark = complete.for_kind(kind);
            assert_eq!(watermark.enqueued(), watermark.completed());
            assert_eq!(watermark.pending(), 0);
        }
    }

    #[test]
    fn busy_observation_clears_the_previous_empty_candidate() {
        let fence = DocumentProducerFence::default();
        let mut observer = DocumentProducerObserver::default();
        let first = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let second = first.checked_next().unwrap();
        let third = second.checked_next().unwrap();
        let fourth = third.checked_next().unwrap();

        assert!(matches!(
            observer.observe(&fence, first),
            Ok(DocumentProducerObservation::FirstEmpty(_))
        ));
        let guard = fence.begin(DocumentProducerKind::Font).unwrap();
        assert!(matches!(
            observer.observe(&fence, second),
            Ok(DocumentProducerObservation::Busy(snapshot))
                if snapshot.for_kind(DocumentProducerKind::Font).pending() == 1
        ));
        drop(guard);
        assert!(matches!(
            observer.observe(&fence, third),
            Ok(DocumentProducerObservation::FirstEmpty(_))
        ));
        assert!(matches!(
            observer.observe(&fence, fourth),
            Ok(DocumentProducerObservation::StableEmpty(_))
        ));
    }

    #[test]
    fn producer_overflow_is_checked_before_mutating_any_watermark() {
        let state = DocumentProducerFenceState {
            revision: u64::MAX - 1,
            ..DocumentProducerFenceState::default()
        };
        let fence = DocumentProducerFence {
            fence_id: DocumentProducerFenceId(1),
            inner: Arc::new(Mutex::new(state)),
            execution_ledger: None,
        };
        let before = fence.snapshot();

        assert!(matches!(
            fence.begin(DocumentProducerKind::Task),
            Err(DocumentProducerFenceError::CounterOverflow)
        ));
        assert_eq!(fence.snapshot(), before);
        assert_eq!(
            DocumentProducerCheckpoint(u64::MAX).checked_next(),
            Err(DocumentProducerFenceError::CounterOverflow)
        );
    }

    #[test]
    fn queued_font_terminal_ack_stays_busy_until_the_script_thread_handles_it() {
        let fence = DocumentProducerFence::default();
        let lease_id = fence
            .begin(DocumentProducerKind::Font)
            .unwrap()
            .into_lease_id();
        let mut observer = DocumentProducerObserver::default();
        let first = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let second = first.checked_next().unwrap();
        let third = second.checked_next().unwrap();
        let fourth = third.checked_next().unwrap();

        assert!(matches!(
            observer.observe(&fence, first),
            Ok(DocumentProducerObservation::Busy(snapshot)) if snapshot.pending() == 1
        ));
        assert!(matches!(
            observer.observe(&fence, second),
            Ok(DocumentProducerObservation::Busy(snapshot)) if snapshot.pending() == 1
        ));
        fence.complete_lease(lease_id).unwrap();
        assert_eq!(
            fence.complete_lease(lease_id),
            Err(DocumentProducerFenceError::UnknownLease(lease_id))
        );
        assert!(matches!(
            observer.observe(&fence, third),
            Ok(DocumentProducerObservation::FirstEmpty(_))
        ));
        assert!(matches!(
            observer.observe(&fence, fourth),
            Ok(DocumentProducerObservation::StableEmpty(_))
        ));
    }

    #[test]
    fn lease_from_another_fence_cannot_complete_a_matching_local_sequence() {
        let first_fence = DocumentProducerFence::default();
        let second_fence = DocumentProducerFence::default();
        let first_lease = first_fence
            .begin(DocumentProducerKind::Resource)
            .unwrap()
            .into_lease_id();
        let second_lease = second_fence
            .begin(DocumentProducerKind::Resource)
            .unwrap()
            .into_lease_id();

        assert_eq!(first_lease.sequence(), second_lease.sequence());
        assert_eq!(
            first_fence.complete_lease(second_lease),
            Err(DocumentProducerFenceError::UnknownLease(second_lease))
        );
        assert_eq!(first_fence.snapshot().pending(), 1);
        assert_eq!(second_fence.snapshot().pending(), 1);

        first_fence.complete_lease(first_lease).unwrap();
        second_fence.complete_lease(second_lease).unwrap();
        assert!(first_fence.snapshot().is_empty());
        assert!(second_fence.snapshot().is_empty());
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
    fn stale_exact_advance_does_not_move_the_controlled_clock() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let clock = controlled_clock(3);
        let mut scheduler = TimerScheduler::with_clock(clock.clone());
        let id = scheduler.schedule_timer(recording_request(&events, 1, Duration::from_nanos(7)));
        let stale = scheduler.finite_deadline_snapshot().unwrap().unwrap();
        scheduler.cancel_timer(id);

        assert!(matches!(
            scheduler.advance_to_and_activate(stale),
            Err(TimerControlError::StaleDeadline { observed: None, .. })
        ));
        assert_eq!(clock.now(), DocumentTime::from_nanos(3));
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn conditional_advance_atomically_rejects_clock_drift() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let clock = controlled_clock(0);
        let mut scheduler = TimerScheduler::with_clock(clock.clone());
        scheduler.schedule_timer(recording_request(&events, 1, Duration::from_nanos(10)));
        let snapshot = scheduler.finite_deadline_snapshot().unwrap().unwrap();
        clock.advance_to(DocumentTime::from_nanos(1)).unwrap();

        assert_eq!(
            scheduler.validate_and_advance_from(DocumentTime::ZERO, snapshot),
            Err(TimerControlError::Clock(DocumentClockError::TimeChanged {
                expected: DocumentTime::ZERO,
                observed: DocumentTime::from_nanos(1),
            }))
        );
        assert_eq!(clock.now(), DocumentTime::from_nanos(1));
        assert_eq!(
            scheduler.finite_deadline_snapshot().unwrap(),
            Some(snapshot)
        );
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
        let beyond_duration = DocumentTime::from_nanos(Duration::MAX.as_nanos() + 1);
        assert_eq!(
            beyond_duration.checked_to_duration(),
            Err(DocumentClockError::Overflow)
        );

        let beyond_u64 = u128::from(u64::MAX) + 1;
        let clock = controlled_clock(beyond_u64);
        assert_eq!(
            clock.advance_to(DocumentTime::from_nanos(beyond_u64 - 1)),
            Err(DocumentClockError::TimeMovedBackwards {
                current: DocumentTime::from_nanos(beyond_u64),
                requested: DocumentTime::from_nanos(beyond_u64 - 1),
            })
        );

        let mut scheduler = TimerScheduler::with_clock(clock);
        let events = Arc::new(Mutex::new(Vec::new()));
        scheduler
            .try_schedule_timer(recording_request(&events, 1, Duration::from_nanos(2)))
            .unwrap();
        assert_eq!(
            scheduler
                .finite_deadline_snapshot()
                .unwrap()
                .unwrap()
                .deadline,
            DocumentTime::from_nanos(beyond_u64 + 2)
        );

        assert_eq!(
            DocumentTime::from_nanos(u128::MAX).checked_add(Duration::from_nanos(1)),
            Err(DocumentClockError::Overflow)
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
            unix_time_origin_ns: DocumentUnixTime::from_nanos(1_700_000_000_000_000_000),
            execution_limits: None,
        });
        let navigation_origin = clock.now();

        assert_eq!(
            clock.unix_time_ns_at(navigation_origin),
            Ok(DocumentUnixTime::from_nanos(1_700_000_000_007_000_000))
        );
        assert_eq!(
            clock.unix_time_ns(),
            Ok(DocumentUnixTime::from_nanos(1_700_000_000_007_000_000))
        );

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
        assert_eq!(
            clock.unix_time_ns(),
            Ok(DocumentUnixTime::from_nanos(1_700_000_000_012_000_000))
        );
    }

    #[test]
    fn controlled_wall_time_overflow_is_sticky_and_precedes_monotonic_mutation() {
        assert!(matches!(
            DocumentClock::try_new(DocumentClockConfiguration::Controlled {
                initial_time_ns: 1,
                unix_time_origin_ns: DocumentUnixTime::from_nanos(i128::MAX),
                execution_limits: None,
            }),
            Err(DocumentClockError::Overflow)
        ));

        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(i128::MAX),
            execution_limits: None,
        });
        assert_eq!(
            clock.advance_to(DocumentTime::from_nanos(1)),
            Err(DocumentClockError::Overflow)
        );
        assert_eq!(clock.try_now(), Ok(DocumentTime::ZERO));
        assert_eq!(clock.terminal_error(), Some(DocumentClockError::Overflow));
        assert_eq!(
            clock.advance_to(DocumentTime::ZERO),
            Err(DocumentClockError::Overflow)
        );
        assert_eq!(
            clock.advance_from_to(DocumentTime::from_nanos(1), DocumentTime::ZERO),
            Err(DocumentClockError::Overflow)
        );

        let ordinary = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 1,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: None,
        });
        assert_eq!(
            DocumentTime::from_nanos(1).checked_duration_since(DocumentTime::from_nanos(2)),
            Err(DocumentClockError::TimeMovedBackwards {
                current: DocumentTime::from_nanos(2),
                requested: DocumentTime::from_nanos(1),
            })
        );
        assert_eq!(ordinary.now(), DocumentTime::from_nanos(1));
    }

    #[test]
    fn javascript_date_transport_is_exact_or_latches_a_typed_terminal() {
        const ADVERSARIAL_MILLISECONDS: i128 = 8_639_999_999_999_979;
        const LIMIT_NANOSECONDS: i128 = TIME_CLIP_LIMIT_MILLISECONDS * NANOSECONDS_PER_MILLISECOND;

        for milliseconds in [ADVERSARIAL_MILLISECONDS, -ADVERSARIAL_MILLISECONDS] {
            let unix_time =
                DocumentUnixTime::from_nanos(milliseconds * NANOSECONDS_PER_MILLISECOND);
            let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
                initial_time_ns: 0,
                unix_time_origin_ns: unix_time,
                execution_limits: None,
            });
            let error = clock
                .javascript_date_time_microseconds()
                .expect_err("unreachable TimeClip millisecond must fail closed");
            assert!(matches!(
                error,
                DocumentClockError::JavaScriptDatePrecisionLoss {
                    unix_time: observed_time,
                    expected_milliseconds,
                    observed_milliseconds,
                } if observed_time == unix_time &&
                    expected_milliseconds == milliseconds &&
                    observed_milliseconds != expected_milliseconds
            ));
            assert_eq!(clock.terminal_error(), Some(error));
            assert_eq!(clock.try_now(), Ok(DocumentTime::ZERO));
            assert_eq!(clock.advance_to(DocumentTime::from_nanos(1)), Err(error));

            let encoded = postcard::to_stdvec(&error).unwrap();
            let decoded: DocumentClockError = postcard::from_bytes(&encoded).unwrap();
            assert_eq!(decoded, error);
        }

        for nanoseconds in [-1, -999_999, 0, 1, 999_999] {
            let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
                initial_time_ns: 0,
                unix_time_origin_ns: DocumentUnixTime::from_nanos(nanoseconds),
                execution_limits: None,
            });
            let candidate = clock.javascript_date_time_microseconds().unwrap();
            let clipped = simulated_javascript_date_time_clip(candidate);
            assert_eq!(clipped, 0.0);
            assert!(!clipped.is_sign_negative());
            assert_eq!(clock.terminal_error(), None);
        }

        for (nanoseconds, expected_milliseconds) in
            [(-1_000_001, -1), (1_000_001, 1), (2_000_000_000, 2000)]
        {
            let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
                initial_time_ns: 0,
                unix_time_origin_ns: DocumentUnixTime::from_nanos(nanoseconds),
                execution_limits: None,
            });
            let candidate = clock.javascript_date_time_microseconds().unwrap();
            assert_eq!(
                simulated_javascript_date_time_clip(candidate),
                expected_milliseconds as f64
            );
            assert_eq!(clock.terminal_error(), None);
        }

        for nanoseconds in [
            -LIMIT_NANOSECONDS,
            -LIMIT_NANOSECONDS + 1,
            LIMIT_NANOSECONDS - 1,
            LIMIT_NANOSECONDS,
            -LIMIT_NANOSECONDS - 1,
            LIMIT_NANOSECONDS + 1,
        ] {
            let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
                initial_time_ns: 0,
                unix_time_origin_ns: DocumentUnixTime::from_nanos(nanoseconds),
                execution_limits: None,
            });
            let candidate = clock.javascript_date_time_microseconds().unwrap();
            if nanoseconds.abs() <= LIMIT_NANOSECONDS {
                let clipped = simulated_javascript_date_time_clip(candidate);
                assert_eq!(clipped as i128, nanoseconds / NANOSECONDS_PER_MILLISECOND);
            } else {
                assert!(candidate.is_nan());
            }
            assert_eq!(clock.terminal_error(), None);
        }

        let crossing = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(LIMIT_NANOSECONDS),
            execution_limits: None,
        });
        assert_eq!(
            simulated_javascript_date_time_clip(
                crossing.javascript_date_time_microseconds().unwrap()
            ),
            TIME_CLIP_LIMIT_MILLISECONDS as f64
        );
        crossing.advance_to(DocumentTime::from_nanos(1)).unwrap();
        assert!(
            crossing
                .javascript_date_time_microseconds()
                .unwrap()
                .is_nan()
        );
        assert_eq!(crossing.terminal_error(), None);
    }

    #[test]
    fn api2_time_boundaries_preserve_signed_epoch_and_full_virtual_span() {
        const API2_EPOCH_LIMIT_MS: i128 = 8_640_000_000_000_000;
        const API2_VIRTUAL_SPAN_MAX_MS: u128 = 9_007_199_254_740_991;
        const NANOS_PER_MILLISECOND: i128 = 1_000_000;

        let epoch_ns = API2_EPOCH_LIMIT_MS
            .checked_mul(NANOS_PER_MILLISECOND)
            .unwrap();
        let span_ns = API2_VIRTUAL_SPAN_MAX_MS
            .checked_mul(NANOS_PER_MILLISECOND as u128)
            .unwrap();
        assert!(span_ns > u128::from(u64::MAX));

        let negative = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: span_ns,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(-epoch_ns),
            execution_limits: None,
        });
        assert_eq!(
            negative.unix_time_ns(),
            Ok(DocumentUnixTime::from_nanos(-epoch_ns + span_ns as i128))
        );

        let positive = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: span_ns,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(epoch_ns),
            execution_limits: None,
        });
        assert_eq!(
            positive.unix_time_ns(),
            Ok(DocumentUnixTime::from_nanos(epoch_ns + span_ns as i128))
        );
        assert_eq!(
            DocumentTime::from_nanos(span_ns).checked_to_duration(),
            Ok(Duration::new(
                u64::try_from(span_ns / 1_000_000_000).unwrap(),
                u32::try_from(span_ns % 1_000_000_000).unwrap(),
            ))
        );
    }

    #[test]
    fn postcard_round_trips_widened_controlled_time_endpoints() {
        const API2_MIN_EPOCH_NS: i128 = -8_640_000_000_000_000_000_000;
        const API2_MAX_SPAN_NS: u128 = 9_007_199_254_740_991_000_000;
        let configuration = DocumentClockConfiguration::Controlled {
            initial_time_ns: API2_MAX_SPAN_NS,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(API2_MIN_EPOCH_NS),
            execution_limits: Some(DocumentExecutionLimits {
                virtual_span: Some(DocumentTime::from_nanos(API2_MAX_SPAN_NS)),
                ..DocumentExecutionLimits::API2_V1_DEFAULT
            }),
        };

        let encoded = postcard::to_stdvec(&configuration).unwrap();
        let decoded: DocumentClockConfiguration = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded, configuration);

        let terminal = DocumentExecutionTerminal::VirtualSpanExceeded {
            limit: DocumentTime::from_nanos(API2_MAX_SPAN_NS),
            requested: DocumentTime::from_nanos(API2_MAX_SPAN_NS + 1),
        };
        let encoded = postcard::to_stdvec(&terminal).unwrap();
        let decoded: DocumentExecutionTerminal = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded, terminal);
    }

    #[test]
    fn postcard_preserves_existing_controlled_time_discriminants() {
        let realtime = DocumentClockConfiguration::Realtime;
        let controlled = DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: None,
        };
        let budget = DocumentExecutionTerminal::BudgetExceeded {
            budget: DocumentExecutionBudget::OrdinaryTasks,
            limit: 1,
            observed: 2,
        };
        let counter =
            DocumentExecutionTerminal::CounterOverflow(DocumentExecutionCounter::OrdinaryTasks);
        let span = DocumentExecutionTerminal::VirtualSpanExceeded {
            limit: DocumentTime::from_nanos(1),
            requested: DocumentTime::from_nanos(2),
        };
        let before_initial = DocumentExecutionTerminal::VirtualTimeBeforeInitial {
            initial: DocumentTime::from_nanos(1),
            requested: DocumentTime::ZERO,
        };
        let realtime_error = DocumentClockError::RealtimeClock;
        let backwards_error = DocumentClockError::TimeMovedBackwards {
            current: DocumentTime::from_nanos(1),
            requested: DocumentTime::ZERO,
        };
        let changed_error = DocumentClockError::TimeChanged {
            expected: DocumentTime::ZERO,
            observed: DocumentTime::from_nanos(1),
        };
        let overflow_error = DocumentClockError::Overflow;
        let surface_error = DocumentClockError::UnsupportedSurface(DocumentTimeSurface::Worker);
        let date_error = DocumentClockError::JavaScriptDatePrecisionLoss {
            unix_time: DocumentUnixTime::from_nanos(1),
            expected_milliseconds: 1,
            observed_milliseconds: 0,
        };

        assert_eq!(postcard::to_stdvec(&realtime).unwrap()[0], 0);
        assert_eq!(postcard::to_stdvec(&controlled).unwrap()[0], 1);
        assert_eq!(postcard::to_stdvec(&budget).unwrap()[0], 0);
        assert_eq!(postcard::to_stdvec(&counter).unwrap()[0], 1);
        assert_eq!(postcard::to_stdvec(&span).unwrap()[0], 2);
        assert_eq!(postcard::to_stdvec(&before_initial).unwrap()[0], 3);
        assert_eq!(postcard::to_stdvec(&realtime_error).unwrap()[0], 0);
        assert_eq!(postcard::to_stdvec(&backwards_error).unwrap()[0], 1);
        assert_eq!(postcard::to_stdvec(&changed_error).unwrap()[0], 2);
        assert_eq!(postcard::to_stdvec(&overflow_error).unwrap()[0], 3);
        assert_eq!(postcard::to_stdvec(&surface_error).unwrap()[0], 4);
        assert_eq!(postcard::to_stdvec(&date_error).unwrap()[0], 5);
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
