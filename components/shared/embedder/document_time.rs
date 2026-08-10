/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Internal embedding protocol for mechanically driving one controlled document event loop.

use serde::{Deserialize, Serialize};
use servo_base::Epoch;
use servo_base::id::{PipelineId, ScriptEventLoopId, WebViewId};
pub use timers::DocumentClockConfiguration;
use timers::{
    DocumentClockError, DocumentClockId, DocumentProducerCheckpoint, DocumentProducerFenceError,
    DocumentProducerFenceId, DocumentProducerSnapshot, DocumentTime, DocumentTimeSurface,
    TimerControlError, TimerDeadlineSnapshot,
};

/// Stable identity for one in-flight controlled document-time request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct DocumentTimeControlRequestId(u64);

impl DocumentTimeControlRequestId {
    /// Construct an identifier from a checked Constellation sequence.
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Return the underlying request sequence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One immutable identity snapshot used to reject navigation races.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentTimeControlTarget {
    /// The opt-in embedding WebView.
    pub webview_id: WebViewId,
    /// The only ScriptEventLoop allowed to own this WebView while controlled.
    pub event_loop_id: ScriptEventLoopId,
    /// Constellation generation of the active top-level pipeline.
    pub webview_epoch: Epoch,
    /// Every pending, active, or retained pipeline in this WebView assigned to the event loop.
    pub pipelines: Vec<PipelineId>,
    /// The fully active pipeline vector for per-document readiness observations.
    pub fully_active_pipelines: Vec<PipelineId>,
}

/// Stable identity of one ScriptThread-issued conditional-advance precondition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct DocumentTimeAdvanceTokenId(u64);

impl DocumentTimeAdvanceTokenId {
    /// Construct an identifier from a checked ScriptThread sequence.
    #[doc(hidden)]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Return the underlying ScriptThread sequence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A single-use, ScriptThread-issued precondition for one exact clock advance.
///
/// The private fields prevent callers from modifying individual scheduling facts. ScriptThread
/// also retains the exact issued value and consumes it on every advance attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentTimeAdvanceToken {
    id: DocumentTimeAdvanceTokenId,
    target: DocumentTimeControlTarget,
    clock_id: DocumentClockId,
    now: DocumentTime,
    deadline: TimerDeadlineSnapshot,
    input_revision: u64,
    pending_events: u64,
    input_batch_saturated: bool,
    producers: DocumentTimeProducerObservation,
}

impl DocumentTimeAdvanceToken {
    /// Construct a token from one internally qualified ScriptThread observation.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn new_internal(
        id: DocumentTimeAdvanceTokenId,
        target: DocumentTimeControlTarget,
        clock_id: DocumentClockId,
        now: DocumentTime,
        deadline: TimerDeadlineSnapshot,
        input_revision: u64,
        pending_events: u64,
        input_batch_saturated: bool,
        producers: DocumentTimeProducerObservation,
    ) -> Self {
        Self {
            id,
            target,
            clock_id,
            now,
            deadline,
            input_revision,
            pending_events,
            input_batch_saturated,
            producers,
        }
    }

    /// Return this token's single-use sequence.
    pub const fn id(&self) -> DocumentTimeAdvanceTokenId {
        self.id
    }

    /// Return the exact WebView, pipeline, and event-loop target bound by this token.
    pub const fn target(&self) -> &DocumentTimeControlTarget {
        &self.target
    }

    /// Return the identity of the one clock domain this token may advance.
    pub const fn clock_id(&self) -> DocumentClockId {
        self.clock_id
    }

    /// Return the clock offset observed when this token was issued.
    pub const fn now(&self) -> DocumentTime {
        self.now
    }

    /// Return the exact finite deadline this token may activate.
    pub const fn deadline(&self) -> TimerDeadlineSnapshot {
        self.deadline
    }

    /// Return the ordinary-input revision observed when this token was issued.
    pub const fn input_revision(&self) -> u64 {
        self.input_revision
    }

    /// Return the buffered ordinary-event count bound by this token.
    pub const fn pending_events(&self) -> u64 {
        self.pending_events
    }

    /// Return whether bounded input intake was saturated when this token was issued.
    pub const fn input_batch_saturated(&self) -> bool {
        self.input_batch_saturated
    }

    /// Return the qualified producer observation bound by this token.
    pub const fn producers(&self) -> DocumentTimeProducerObservation {
        self.producers
    }
}

/// A single mechanical operation. None of these operations imply visual settlement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentTimeControlCommand {
    /// Observe state without processing an event or advancing time.
    Observe,
    /// Conditionally advance to and activate the exact deadline bound by a fresh token.
    ///
    /// A typed rejection guarantees that neither the clock nor timer queue mutated. A matching
    /// `TimerActivated` response is committed even if Constellation observes a later navigation
    /// before forwarding the response.
    AdvanceTo(DocumentTimeAdvanceToken),
    /// Process one queued event-loop event and its normal checkpoint/render tail.
    ///
    /// If no page event is queued, the driver runs one existing no-op ScriptThread wake turn so
    /// that producer stability can advance across a fresh microtask checkpoint without host time.
    DriveOneTurn,
}

/// The operation completed before an observation was captured.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentTimeControlAction {
    /// No event-loop work was requested.
    Observed,
    /// Exactly one timer callback was activated; its resulting loop work has not run yet.
    TimerActivated(TimerDeadlineSnapshot),
    /// No page event was queued, so one no-op checkpoint turn was processed.
    CheckpointTurnProcessed {
        /// Whether the no-op turn completed its microtask checkpoint.
        microtask_checkpoint_advanced: bool,
    },
    /// One event was processed through the existing event-loop path.
    TurnProcessed {
        /// Whether that event caused at least one microtask checkpoint to complete.
        microtask_checkpoint_advanced: bool,
    },
}

/// Qualification of one producer snapshot at the latest completed microtask checkpoint.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentProducerStability {
    /// No Window microtask checkpoint has completed yet.
    NotCheckpointed,
    /// At least one participating producer is still live.
    Busy,
    /// This is the first empty observation at this producer revision.
    FirstEmpty,
    /// Two fresh checkpoints observed the same empty producer revision.
    StableEmpty,
    /// This request did not advance the checkpoint, so it cannot strengthen readiness.
    UnchangedCheckpoint,
}

/// Producer identity, watermarks, and two-checkpoint qualification from one ScriptThread.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentTimeProducerObservation {
    /// Identity of the one fence shared by all participating producers on the event loop.
    pub fence_id: DocumentProducerFenceId,
    /// Latest completed ScriptThread microtask checkpoint.
    pub checkpoint: DocumentProducerCheckpoint,
    /// Mutex-consistent enqueue/completion/pending watermarks.
    pub snapshot: DocumentProducerSnapshot,
    /// Mechanical two-fence qualification at this checkpoint.
    pub stability: DocumentProducerStability,
}

/// A reason one ScriptThread document is not mechanically ready for a visual capture attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentTimeReadinessBlocker {
    /// The pipeline has not installed its Document on this ScriptThread yet.
    DocumentUnavailable,
    /// The document is not fully active.
    NotFullyActive,
    /// `document.readyState` is not `complete`.
    Loading,
    /// A render-blocking element remains.
    RenderBlocked,
    /// The document root still has a reftest/test wait marker.
    WaitMarker,
    /// Web-font loading is still active.
    FontsLoading,
    /// `document.fonts.ready` has not resolved.
    FontReadyPromise,
    /// Layout image notifications remain pending.
    LayoutImages,
    /// Image rasterization remains pending.
    RasterImages,
    /// A rendering update is still required.
    RenderingUpdate,
    /// Canvas image uploads remain pending.
    CanvasImageUpdates,
}

/// Script-side mechanical readiness and rendering generation for one pipeline.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentTimeDocumentObservation {
    /// Pipeline containing the document.
    pub pipeline_id: PipelineId,
    /// Script/layout generation; this is not evidence that Paint presented the epoch.
    pub script_rendering_epoch: Option<Epoch>,
    /// Empty only when the current ScriptThread readiness predicates all pass.
    pub readiness_blockers: Vec<DocumentTimeReadinessBlocker>,
}

/// Post-command facts from one controlled ScriptEventLoop.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentTimeControlObservation {
    /// Target identity verified by both ScriptThread and Constellation.
    pub target: DocumentTimeControlTarget,
    /// Current checked integer-nanosecond time in this event-loop domain.
    pub now: DocumentTime,
    /// Next finite timer deadline, without activating it.
    pub next_deadline: Option<TimerDeadlineSnapshot>,
    /// Single-use conditional-advance precondition, issued only for qualified empty state.
    pub advance_token: Option<DocumentTimeAdvanceToken>,
    /// Number of already-received events held for later controlled turns.
    pub pending_events: u64,
    /// The bounded pre/post-command intake batch filled completely.
    ///
    /// This fail-closed fact means more channel input may already be ready even when the producer
    /// snapshot is otherwise empty.
    pub input_batch_saturated: bool,
    /// Operation completed immediately before this observation.
    pub action: DocumentTimeControlAction,
    /// Producer fence facts qualified only across fresh checkpoints.
    pub producers: DocumentTimeProducerObservation,
    /// Per-pipeline script readiness and rendering generations.
    pub documents: Vec<DocumentTimeDocumentObservation>,
}

/// Typed failure from activation, routing, one-turn driving, or observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentTimeControlError {
    /// The WebView no longer exists.
    WebViewUnavailable,
    /// The WebView did not opt into controlled time before navigation.
    NotControlled,
    /// No ScriptEventLoop has been bound to the controlled WebView yet.
    EventLoopUnavailable,
    /// The WebView currently spans more than one ScriptEventLoop.
    MultipleEventLoops,
    /// The selected ScriptEventLoop also owns a different WebView.
    SharedEventLoopWebView,
    /// Another command is already in flight for this WebView.
    CommandAlreadyPending,
    /// The Constellation request sequence could not be incremented.
    RequestSequenceOverflow,
    /// The ScriptThread advance-token sequence could not be incremented.
    AdvanceTokenSequenceOverflow,
    /// The checked ordinary-input revision could not represent another admitted event.
    InputRevisionOverflow,
    /// No issued token remained for the attempted advance (including token reuse).
    AdvanceTokenUnavailable {
        /// Token supplied by the caller.
        observed: DocumentTimeAdvanceTokenId,
    },
    /// The supplied token was not the exact latest token retained by ScriptThread.
    StaleAdvanceToken {
        /// Latest retained token consumed by this failed attempt.
        expected: DocumentTimeAdvanceTokenId,
        /// Token supplied by the caller.
        observed: DocumentTimeAdvanceTokenId,
    },
    /// A guarded response did not acknowledge the exact deadline routed by Constellation.
    AdvanceResponseMismatch {
        /// Deadline bound by the pending guarded request.
        expected: TimerDeadlineSnapshot,
        /// Action returned by ScriptThread.
        observed: DocumentTimeControlAction,
    },
    /// Buffered or ready ordinary input changed after token issuance.
    AdvanceInputChanged {
        /// Input revision bound by the token.
        expected_revision: u64,
        /// Input revision at the conditional action's linearization point.
        observed_revision: u64,
        /// Buffered ordinary events at the linearization point.
        pending_events: u64,
        /// Whether the final bounded intake filled completely.
        input_batch_saturated: bool,
    },
    /// The clock identity or offset changed after token issuance.
    AdvanceClockChanged {
        /// Clock domain bound by the token.
        expected_id: DocumentClockId,
        /// Clock domain observed by ScriptThread.
        observed_id: DocumentClockId,
        /// Clock offset bound by the token.
        expected_now: DocumentTime,
        /// Clock offset observed at the linearization point.
        observed_now: DocumentTime,
    },
    /// Producer identity, checkpoint, or watermarks changed after token issuance.
    AdvanceProducerChanged {
        /// Producer observation bound by the token.
        expected: Box<DocumentTimeProducerObservation>,
        /// Producer observation at validation time.
        observed: Box<DocumentTimeProducerObservation>,
    },
    /// The target identity changed between command routing and response validation.
    TargetChanged {
        /// Identity captured when the command was routed.
        expected: Box<DocumentTimeControlTarget>,
        /// Current identity, if a controlled target still exists.
        observed: Option<Box<DocumentTimeControlTarget>>,
    },
    /// The ScriptThread response envelope did not belong to the routed target.
    ResponseSourceMismatch {
        /// WebView carried by the ScriptThread response envelope.
        webview_id: WebViewId,
        /// Pipeline carried by the ScriptThread response envelope.
        pipeline_id: PipelineId,
    },
    /// A worker, worklet, cross-loop navigation, or host timestamp escaped this slice.
    UnsupportedSurface(DocumentTimeSurface),
    /// Checked document-clock operation failed.
    Clock(DocumentClockError),
    /// Exact finite-deadline operation failed.
    Timer(TimerControlError),
    /// Producer-fence observation failed.
    ProducerFence(DocumentProducerFenceError),
    /// A queue length could not be represented in the protocol.
    QueueLengthOverflow,
    /// The selected ScriptThread channel was closed.
    ChannelClosed,
}
