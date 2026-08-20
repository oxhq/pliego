/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Internal embedding protocol for mechanically driving one controlled document event loop.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use servo_base::Epoch;
use servo_base::generic_channel::{GenericReceiver, ReceiveError, TryReceiveError};
use servo_base::id::{PipelineId, ScriptEventLoopId, WebViewId};
pub use timers::{
    DocumentClockConfiguration, DocumentExecutionBudget, DocumentExecutionCounter,
    DocumentExecutionCounters, DocumentExecutionLimits, DocumentExecutionObservation,
    DocumentExecutionTerminal, DocumentUnixTime,
};
use timers::{
    DocumentClockError, DocumentClockId, DocumentProducerCheckpoint, DocumentProducerFenceError,
    DocumentProducerFenceId, DocumentProducerSnapshot, DocumentTime, DocumentTimeSurface,
    TimerControlError, TimerDeadlineSnapshot,
};

use crate::generation_capture::{
    DocumentCaptureCommit, DocumentCaptureConsumeRequest, DocumentCapturePreconditionId,
    DocumentCapturePreparation, DocumentCaptureSurfaceFingerprint,
    DocumentPaintPresentationTicketId,
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

/// Exact trusted-channel correlation nonce for abandoning one control response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[doc(hidden)]
pub struct DocumentTimeControlCancellationId(u64);

impl DocumentTimeControlCancellationId {
    /// Construct an identifier from the checked per-WebView sequence.
    #[doc(hidden)]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Return the underlying abandonment-correlation sequence.
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
    /// Observe page state without running page work, advancing the clock, or mutating the timer
    /// queue.
    ///
    /// Observation clears any previously issued advance token, performs bounded control-input
    /// intake that may advance the input revision, and advances producer qualification. A
    /// successful observation may issue one replacement token; if its response is lost, callers
    /// must treat the old token as stale and the replacement as unavailable.
    Observe,
    /// Conditionally advance to and activate the exact deadline bound by a fresh token.
    ///
    /// A typed rejection guarantees that neither the clock nor timer queue mutated. A matching
    /// `TimerActivated` response is committed even if Constellation observes a later navigation
    /// before forwarding the response.
    AdvanceTo(Box<DocumentTimeAdvanceToken>),
    /// Process one queued event-loop event and its normal checkpoint/render tail.
    ///
    /// If no page event is queued, the driver runs one existing no-op ScriptThread wake turn so
    /// that producer stability can advance across a fresh microtask checkpoint without host time.
    DriveOneTurn,
    /// Observe and qualify one exact ScriptThread generation for a later capture attempt.
    ///
    /// This command does not contact Paint or capture pixels. It invalidates any previously
    /// issued capture precondition before collecting the new source inventory.
    PrepareCapture(DocumentCaptureSurfaceFingerprint),
    /// Atomically consume one retained ScriptThread capture candidate against Paint's exact
    /// presentation ticket and serialize the cached layout snapshot in-handler.
    ///
    /// This command is single-use. Once submitted, a lost response is outcome-indeterminate and
    /// neither the candidate nor Paint ticket may be retried.
    ConsumeCapture(Box<DocumentCaptureConsumeRequest>),
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
    /// Sticky execution failure prevented the requested ordinary turn from starting.
    ExecutionTerminated,
    /// Capture qualification was evaluated without running page work.
    CapturePrepared,
    /// One retained candidate was consumed without running page work or advancing document time.
    CaptureConsumed,
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
    /// Controlled execution policy, counters, and sticky terminal failure when enabled.
    ///
    /// `None` preserves the pre-ledger controlled protocol for a clock configured without limits.
    pub execution: Option<DocumentExecutionObservation>,
    /// Present only for [`DocumentTimeControlCommand::PrepareCapture`].
    ///
    /// A contained precondition is ScriptThread generation proof only; it does not prove Paint
    /// presentation or successful artifact capture.
    pub capture_preparation: Option<DocumentCapturePreparation>,
    /// Present only after a successful [`DocumentTimeControlCommand::ConsumeCapture`].
    ///
    /// The commit contains the cached layout bytes and exact candidate/ticket correlation. It does
    /// not prove that a later artifact write or Paint finalization succeeded.
    pub capture_commit: Option<Box<DocumentCaptureCommit>>,
}

/// Definitive or explicitly indeterminate completion of one mechanical control command.
///
/// The indeterminate variants are intentionally not errors: after submission, shutdown, transport
/// failure, or a non-authoritative successful envelope can prevent proof of which side of a
/// single-use linearization point won. The caller must not interpret either outcome as proof that
/// no mutation occurred or retry its token, candidate, or presentation ticket.
///
/// This enum crosses Servo's callback channel and is a same-build protocol. Existing variant order
/// and payloads must remain stable; receiver-local transport states belong in
/// [`DocumentTimeControlReceiveOutcome`] instead of being inserted here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentTimeControlOutcome {
    /// ScriptThread completed the command and returned its authoritative observation.
    Completed(Box<DocumentTimeControlObservation>),
    /// The command was definitively rejected before a guarded clock mutation. This does not make a
    /// submitted single-use token reusable.
    Rejected(DocumentTimeControlError),
    /// The runtime cannot determine whether this guarded advance committed.
    AdvanceOutcomeIndeterminate {
        /// Single-use token whose outcome cannot be recovered.
        token_id: DocumentTimeAdvanceTokenId,
        /// Exact target bound when Constellation routed the token.
        target: Box<DocumentTimeControlTarget>,
        /// Exact deadline which may or may not have been activated.
        deadline: TimerDeadlineSnapshot,
    },
    /// The runtime cannot determine whether ScriptThread consumed this candidate.
    ///
    /// The caller must not retry either identity: a successful consume may have crossed its
    /// linearization point before the response was lost.
    CaptureConsumeOutcomeIndeterminate {
        /// Single-use ScriptThread candidate whose outcome cannot be recovered.
        candidate_id: DocumentCapturePreconditionId,
        /// Single-use Paint presentation ticket whose outcome cannot be recovered.
        ticket_id: DocumentPaintPresentationTicketId,
        /// Exact target bound when Constellation routed the consume request.
        target: Box<DocumentTimeControlTarget>,
    },
}

/// Why a bounded local wait failed to deliver a command outcome.
///
/// This type is local to the embedding caller and is deliberately not serialized into the
/// same-build Constellation protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub enum DocumentTimeControlTransportFailure {
    /// The bounded wait elapsed while the callback might still complete later.
    TimedOut,
    /// The callback channel closed without an outcome.
    Disconnected,
    /// The callback payload could not be decoded.
    DeserializationFailed(String),
    /// The IPC transport returned an I/O failure.
    Io {
        /// Standard I/O category retained from the transport.
        kind: std::io::ErrorKind,
        /// Transport-provided diagnostic.
        message: String,
    },
}

impl From<TryReceiveError> for DocumentTimeControlTransportFailure {
    fn from(error: TryReceiveError) -> Self {
        match error {
            TryReceiveError::Empty => Self::TimedOut,
            TryReceiveError::ReceiveError(ReceiveError::Disconnected) => Self::Disconnected,
            TryReceiveError::ReceiveError(ReceiveError::DeserializationFailed(message)) => {
                Self::DeserializationFailed(message)
            },
            TryReceiveError::ReceiveError(ReceiveError::Io(error)) => Self::Io {
                kind: error.kind(),
                message: error.to_string(),
            },
        }
    }
}

/// The sole result of consuming a bounded document-time receiver.
///
/// A missing `Observe` response cannot hide page work or a clock advance, though the command may
/// have consumed a prior token and updated internal control bookkeeping. A missing `DriveOneTurn`
/// response is indeterminate because the turn may complete after the caller stops waiting; callers
/// must not retry it as though no event was processed. Guarded advances retain their exact-token
/// [`DocumentTimeControlOutcome::AdvanceOutcomeIndeterminate`]; capture consumes retain their exact
/// candidate/ticket [`DocumentTimeControlOutcome::CaptureConsumeOutcomeIndeterminate`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub enum DocumentTimeControlReceiveOutcome {
    /// Constellation delivered a typed command outcome.
    CommandOutcome(DocumentTimeControlOutcome),
    /// No observation was delivered; `Observe` ran no page work and did not advance the clock.
    ObserveTransportFailure(DocumentTimeControlTransportFailure),
    /// The caller cannot know whether the requested event-loop turn completed.
    DriveOneTurnOutcomeIndeterminate(DocumentTimeControlTransportFailure),
    /// No preparation result was delivered. A later command invalidates any unseen issued token.
    CapturePreparationTransportFailure(DocumentTimeControlTransportFailure),
}

/// Result of one consuming, nonblocking document-time receive attempt.
///
/// An empty callback channel returns the original receiver in [`Self::Pending`], preserving its
/// exact cancellation action and command-specific transport semantics for a later attempt. Every
/// terminal command or transport result consumes the receiver into [`Self::Complete`].
#[doc(hidden)]
pub enum DocumentTimeControlTryReceiveOutcome {
    /// No callback result is ready; the receiver remains armed and may be polled again or dropped.
    Pending(DocumentTimeControlReceiver),
    /// The callback produced the receiver's only terminal outcome.
    Complete(DocumentTimeControlReceiveOutcome),
}

enum DocumentTimeControlTransportSemantics {
    Observe,
    DriveOneTurn,
    PrepareCapture,
    GuardedAdvance(DocumentTimeControlOutcome),
    CaptureConsume(DocumentTimeControlOutcome),
}

/// One bounded, single-result receiver for a submitted document-time command.
///
/// The receiver consumes itself so a timeout or transport failure is terminal for the caller and a
/// late response cannot be mistaken for a second result. A production receiver also carries an
/// exact trusted-channel abandonment action that runs on timeout, transport failure, or receiver
/// drop. A successor submitted after the bounded receive returns is ordered after that
/// abandonment; concurrent receiver drop and successor submission have no ordering guarantee and
/// may observe `CommandAlreadyPending`. For `AdvanceTo` and `ConsumeCapture`, every timeout,
/// disconnection, I/O failure, or deserialization failure is conservatively indeterminate and the
/// submitted token or candidate/ticket pair must not be retried.
#[doc(hidden)]
pub struct DocumentTimeControlReceiver {
    receiver: GenericReceiver<DocumentTimeControlOutcome>,
    transport_semantics: DocumentTimeControlTransportSemantics,
    cancellation: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl DocumentTimeControlReceiver {
    /// Bind transport-failure semantics before the command is submitted.
    #[doc(hidden)]
    pub fn new(
        receiver: GenericReceiver<DocumentTimeControlOutcome>,
        command: &DocumentTimeControlCommand,
    ) -> Self {
        Self::new_internal(receiver, command, None)
    }

    /// Bind an exact abandonment action to timeout, transport failure, or receiver drop.
    #[doc(hidden)]
    pub fn new_cancellable(
        receiver: GenericReceiver<DocumentTimeControlOutcome>,
        command: &DocumentTimeControlCommand,
        cancellation: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self::new_internal(receiver, command, Some(Box::new(cancellation)))
    }

    fn new_internal(
        receiver: GenericReceiver<DocumentTimeControlOutcome>,
        command: &DocumentTimeControlCommand,
        cancellation: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Self {
        let transport_semantics = match command {
            DocumentTimeControlCommand::AdvanceTo(token) => {
                DocumentTimeControlTransportSemantics::GuardedAdvance(
                    DocumentTimeControlOutcome::AdvanceOutcomeIndeterminate {
                        token_id: token.id(),
                        target: Box::new(token.target().clone()),
                        deadline: token.deadline(),
                    },
                )
            },
            DocumentTimeControlCommand::Observe => DocumentTimeControlTransportSemantics::Observe,
            DocumentTimeControlCommand::DriveOneTurn => {
                DocumentTimeControlTransportSemantics::DriveOneTurn
            },
            DocumentTimeControlCommand::PrepareCapture(_) => {
                DocumentTimeControlTransportSemantics::PrepareCapture
            },
            DocumentTimeControlCommand::ConsumeCapture(request) => {
                DocumentTimeControlTransportSemantics::CaptureConsume(
                    DocumentTimeControlOutcome::CaptureConsumeOutcomeIndeterminate {
                        candidate_id: request.precondition().id(),
                        ticket_id: request.ticket().id(),
                        target: Box::new(request.precondition().target().clone()),
                    },
                )
            },
        };
        Self {
            receiver,
            transport_semantics,
            cancellation,
        }
    }

    /// Wait at most `timeout` for the command's only terminal outcome.
    pub fn recv_timeout(self, timeout: Duration) -> DocumentTimeControlReceiveOutcome {
        let result = self.receiver.try_recv_timeout(timeout);
        self.resolve(result)
    }

    /// Attempt to consume the command's only result without blocking.
    ///
    /// Unlike a zero-duration [`Self::recv_timeout`], an empty channel is not a terminal timeout:
    /// the still-armed receiver is returned in [`DocumentTimeControlTryReceiveOutcome::Pending`].
    pub fn try_recv(self) -> DocumentTimeControlTryReceiveOutcome {
        let result = self.receiver.try_recv();
        self.resolve_nonblocking(result)
    }

    fn resolve_nonblocking(
        self,
        result: Result<DocumentTimeControlOutcome, TryReceiveError>,
    ) -> DocumentTimeControlTryReceiveOutcome {
        match result {
            Err(TryReceiveError::Empty) => DocumentTimeControlTryReceiveOutcome::Pending(self),
            result => DocumentTimeControlTryReceiveOutcome::Complete(self.resolve(result)),
        }
    }

    fn resolve(
        mut self,
        result: Result<DocumentTimeControlOutcome, TryReceiveError>,
    ) -> DocumentTimeControlReceiveOutcome {
        match result {
            Ok(outcome) => {
                // Constellation already removed the matching pending request before sending its
                // terminal outcome, so a successful receive must not enqueue a late cancellation.
                self.cancellation.take();
                DocumentTimeControlReceiveOutcome::CommandOutcome(outcome)
            },
            Err(error) => {
                let failure = error.into();
                if let Some(cancellation) = self.cancellation.take() {
                    cancellation();
                }
                match &self.transport_semantics {
                    DocumentTimeControlTransportSemantics::Observe => {
                        DocumentTimeControlReceiveOutcome::ObserveTransportFailure(failure)
                    },
                    DocumentTimeControlTransportSemantics::DriveOneTurn => {
                        DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(failure)
                    },
                    DocumentTimeControlTransportSemantics::PrepareCapture => {
                        DocumentTimeControlReceiveOutcome::CapturePreparationTransportFailure(
                            failure,
                        )
                    },
                    DocumentTimeControlTransportSemantics::GuardedAdvance(outcome) => {
                        DocumentTimeControlReceiveOutcome::CommandOutcome(outcome.clone())
                    },
                    DocumentTimeControlTransportSemantics::CaptureConsume(outcome) => {
                        DocumentTimeControlReceiveOutcome::CommandOutcome(outcome.clone())
                    },
                }
            },
        }
    }
}

impl Drop for DocumentTimeControlReceiver {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation();
        }
    }
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
    /// A sticky controlled-execution terminal was observed before guarded clock mutation.
    ExecutionTerminated(DocumentExecutionTerminal),
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
    /// The per-WebView abandonment-correlation sequence could not be incremented.
    CancellationSequenceOverflow,
    /// The ScriptThread capture-precondition sequence could not be incremented.
    CapturePreconditionSequenceOverflow,
    /// The canonical source-inventory epoch overflowed and is permanently terminal for this
    /// ScriptThread.
    CaptureSourceEpochOverflow,
    /// No retained capture candidate remained, including after a replayed consume.
    CapturePreconditionUnavailable {
        /// Candidate supplied by the caller.
        observed: DocumentCapturePreconditionId,
    },
    /// The caller did not return the exact latest candidate retained by ScriptThread.
    StaleCapturePrecondition {
        /// Latest retained candidate invalidated by this failed attempt.
        expected: DocumentCapturePreconditionId,
        /// Candidate supplied by the caller.
        observed: DocumentCapturePreconditionId,
    },
    /// Paint's presentation ticket did not bind every required candidate dimension.
    CapturePresentationMismatch,
    /// A candidate-bound ScriptThread dimension changed before the consume linearization point.
    CaptureStateChanged,
    /// The exact cached layout snapshot was unavailable for the candidate pipeline.
    CaptureLayoutUnavailable,
    /// The cached layout snapshot did not carry the candidate's exact Script/Layout paint epoch.
    CaptureLayoutEpochMismatch {
        /// Script/Layout epoch bound by the candidate and Paint ticket.
        expected: Epoch,
        /// Paint epoch embedded in the cached layout snapshot.
        observed: Epoch,
    },
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use euclid::{Box2D, Point2D, Size2D};
    use servo_base::generic_channel::{GenericCallback, ReceiveError, TryReceiveError};
    use servo_base::id::{TEST_PIPELINE_ID, TEST_SCRIPT_EVENT_LOOP_ID, TEST_WEBVIEW_ID};
    use servo_geometry::DeviceIndependentPixel;
    use timers::{DocumentClock, DocumentProducerFence, TimerEventRequest, TimerScheduler};

    use super::*;
    use crate::generation_capture::{
        DocumentCaptureConsumeRequest, DocumentCaptureDocumentEpoch, DocumentCapturePrecondition,
        DocumentCapturePreconditionId, DocumentPaintPresentationTicket,
        DocumentPaintPresentationTicketId, DocumentSettlementSourceEpoch,
        DocumentSettlementSourceSnapshot,
    };

    fn target() -> DocumentTimeControlTarget {
        DocumentTimeControlTarget {
            webview_id: TEST_WEBVIEW_ID,
            event_loop_id: TEST_SCRIPT_EVENT_LOOP_ID,
            webview_epoch: Epoch::default(),
            pipelines: vec![TEST_PIPELINE_ID],
            fully_active_pipelines: vec![TEST_PIPELINE_ID],
        }
    }

    fn advance_token() -> DocumentTimeAdvanceToken {
        advance_token_at(0)
    }

    fn advance_token_at(initial_time_ns: u128) -> DocumentTimeAdvanceToken {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: None,
        });
        let mut scheduler = TimerScheduler::with_clock(clock.clone());
        scheduler.schedule_timer(TimerEventRequest {
            callback: Box::new(|| {}),
            duration: Duration::from_nanos(10),
        });
        let fence = DocumentProducerFence::default();
        DocumentTimeAdvanceToken::new_internal(
            DocumentTimeAdvanceTokenId::new(3),
            target(),
            clock.id(),
            clock.now(),
            scheduler
                .finite_deadline_snapshot()
                .expect("test scheduler is controlled")
                .expect("test timer has a finite deadline"),
            0,
            0,
            false,
            DocumentTimeProducerObservation {
                fence_id: fence.id(),
                checkpoint: DocumentProducerCheckpoint::ZERO,
                snapshot: fence.snapshot(),
                stability: DocumentProducerStability::StableEmpty,
            },
        )
    }

    #[test]
    fn actual_ipc_round_trips_signed_origin_and_widened_advance_outcome() {
        const API2_MIN_EPOCH_NS: i128 = -8_640_000_000_000_000_000_000;
        const API2_MAX_SPAN_NS: u128 = 9_007_199_254_740_991_000_000;
        let configuration = DocumentClockConfiguration::Controlled {
            initial_time_ns: API2_MAX_SPAN_NS,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(API2_MIN_EPOCH_NS),
            execution_limits: None,
        };
        let (configuration_sender, configuration_receiver) =
            ipc_channel::ipc::channel::<DocumentClockConfiguration>().unwrap();
        configuration_sender.send(configuration).unwrap();
        assert_eq!(configuration_receiver.recv().unwrap(), configuration);

        let token = advance_token_at(API2_MAX_SPAN_NS - 10);
        let command = DocumentTimeControlCommand::AdvanceTo(Box::new(token.clone()));
        let (command_sender, command_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlCommand>().unwrap();
        command_sender.send(command.clone()).unwrap();
        assert_eq!(command_receiver.recv().unwrap(), command);

        let outcome = DocumentTimeControlOutcome::AdvanceOutcomeIndeterminate {
            token_id: token.id(),
            target: Box::new(token.target().clone()),
            deadline: token.deadline(),
        };
        let (outcome_sender, outcome_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlOutcome>().unwrap();
        outcome_sender.send(outcome.clone()).unwrap();
        assert_eq!(outcome_receiver.recv().unwrap(), outcome);

        let date_clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(8_639_999_999_999_979_000_000),
            execution_limits: None,
        });
        let date_error = date_clock
            .javascript_date_time_microseconds()
            .expect_err("adversarial Date millisecond must fail closed");
        let date_terminal =
            DocumentTimeControlOutcome::Rejected(DocumentTimeControlError::Clock(date_error));
        let (terminal_sender, terminal_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlOutcome>().unwrap();
        terminal_sender.send(date_terminal.clone()).unwrap();
        assert_eq!(terminal_receiver.recv().unwrap(), date_terminal);

        let fence = DocumentProducerFence::default();
        let completed =
            DocumentTimeControlOutcome::Completed(Box::new(DocumentTimeControlObservation {
                target: token.target().clone(),
                now: token.now(),
                next_deadline: Some(token.deadline()),
                advance_token: Some(token),
                pending_events: 0,
                input_batch_saturated: false,
                action: DocumentTimeControlAction::Observed,
                producers: DocumentTimeProducerObservation {
                    fence_id: fence.id(),
                    checkpoint: DocumentProducerCheckpoint::ZERO,
                    snapshot: fence.snapshot(),
                    stability: DocumentProducerStability::StableEmpty,
                },
                documents: Vec::new(),
                execution: None,
                capture_preparation: None,
                capture_commit: None,
            }));
        let (completed_sender, completed_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlOutcome>().unwrap();
        completed_sender.send(completed.clone()).unwrap();
        assert_eq!(completed_receiver.recv().unwrap(), completed);
    }

    fn capture_surface() -> DocumentCaptureSurfaceFingerprint {
        DocumentCaptureSurfaceFingerprint::new(
            Size2D::<i32, DeviceIndependentPixel>::new(800, 600),
            Box2D::new(Point2D::new(0, 0), Point2D::new(800, 600)),
            1.0,
        )
        .unwrap()
    }

    fn capture_consume_fixture() -> (
        DocumentTimeControlCommand,
        DocumentTimeControlOutcome,
        DocumentCapturePreconditionId,
        DocumentPaintPresentationTicketId,
    ) {
        let surface = capture_surface();
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 7,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: Some(DocumentExecutionLimits::API2_V1_DEFAULT),
        });
        let fence = DocumentProducerFence::with_execution_ledger(clock.execution_ledger());
        let checkpoint = DocumentProducerCheckpoint::ZERO
            .checked_next()
            .expect("test checkpoint is representable");
        let producers = DocumentTimeProducerObservation {
            fence_id: fence.id(),
            checkpoint,
            snapshot: fence.snapshot(),
            stability: DocumentProducerStability::StableEmpty,
        };
        let execution = clock
            .execution_ledger()
            .expect("controlled test clock has an execution ledger")
            .observation();
        let sources = DocumentSettlementSourceSnapshot::new_internal(
            DocumentSettlementSourceEpoch::new(3),
            Vec::new(),
        );
        let script_rendering_epoch = Epoch(11);
        let candidate_id = DocumentCapturePreconditionId::new(41);
        let ticket_id = DocumentPaintPresentationTicketId::new(97);
        let precondition = DocumentCapturePrecondition::new_internal(
            candidate_id,
            target(),
            clock.id(),
            clock.now(),
            13,
            0,
            false,
            producers,
            execution,
            None,
            sources,
            vec![DocumentCaptureDocumentEpoch {
                pipeline_id: TEST_PIPELINE_ID,
                script_rendering_epoch,
                readiness_blockers: Vec::new(),
            }],
            surface,
        );
        let ticket = DocumentPaintPresentationTicket::new_internal(
            ticket_id,
            candidate_id,
            target(),
            TEST_PIPELINE_ID,
            script_rendering_epoch,
            surface,
            5,
            8,
        );
        let command = DocumentTimeControlCommand::ConsumeCapture(Box::new(
            DocumentCaptureConsumeRequest::new_internal(
                Box::new(precondition.clone()),
                ticket.clone(),
            ),
        ));
        let commit = DocumentCaptureCommit::new_internal(
            candidate_id,
            ticket_id,
            target(),
            TEST_PIPELINE_ID,
            script_rendering_epoch,
            surface,
            ticket.presentation_generation(),
            ticket.publish_generation(),
            script_rendering_epoch,
            "{\"paint_epoch\":11}".to_owned(),
        );
        let outcome =
            DocumentTimeControlOutcome::Completed(Box::new(DocumentTimeControlObservation {
                target: target(),
                now: clock.now(),
                next_deadline: None,
                advance_token: None,
                pending_events: 0,
                input_batch_saturated: false,
                action: DocumentTimeControlAction::CaptureConsumed,
                producers,
                documents: vec![DocumentTimeDocumentObservation {
                    pipeline_id: TEST_PIPELINE_ID,
                    script_rendering_epoch: Some(script_rendering_epoch),
                    readiness_blockers: Vec::new(),
                }],
                execution: Some(execution),
                capture_preparation: None,
                capture_commit: Some(Box::new(commit)),
            }));
        (command, outcome, candidate_id, ticket_id)
    }

    fn assert_capture_consume_indeterminate(
        outcome: DocumentTimeControlReceiveOutcome,
        candidate_id: DocumentCapturePreconditionId,
        ticket_id: DocumentPaintPresentationTicketId,
    ) {
        assert!(matches!(
            outcome,
            DocumentTimeControlReceiveOutcome::CommandOutcome(
                DocumentTimeControlOutcome::CaptureConsumeOutcomeIndeterminate {
                    candidate_id: observed_candidate,
                    ticket_id: observed_ticket,
                    target: observed_target,
                }
            ) if observed_candidate == candidate_id && observed_ticket == ticket_id &&
                *observed_target == target()
        ));
    }

    #[test]
    fn capture_consume_command_and_success_round_trip_through_ipc() {
        let (command, outcome, _, _) = capture_consume_fixture();
        let (command_sender, command_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlCommand>().unwrap();
        command_sender.send(command.clone()).unwrap();
        assert_eq!(command_receiver.recv().unwrap(), command);

        let (outcome_sender, outcome_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlOutcome>().unwrap();
        outcome_sender.send(outcome.clone()).unwrap();
        assert_eq!(outcome_receiver.recv().unwrap(), outcome);
    }

    #[test]
    fn every_lost_capture_consume_response_is_indeterminate_and_identity_bound() {
        let (command, _, candidate_id, ticket_id) = capture_consume_fixture();
        let (response, receiver) = GenericCallback::new_blocking()
            .expect("test capture-consume callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);
        drop(response);
        assert_capture_consume_indeterminate(
            receiver.recv_timeout(Duration::from_secs(1)),
            candidate_id,
            ticket_id,
        );

        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test capture-consume timeout channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);
        assert_capture_consume_indeterminate(
            receiver.recv_timeout(Duration::ZERO),
            candidate_id,
            ticket_id,
        );

        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test capture-consume decode channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);
        assert_capture_consume_indeterminate(
            receiver.resolve(Err(TryReceiveError::ReceiveError(
                ReceiveError::DeserializationFailed("test corruption".to_owned()),
            ))),
            candidate_id,
            ticket_id,
        );

        // Each receiver above is consumed into one terminal outcome. There is deliberately no
        // second receive path which could make either single-use identity appear retryable.
    }

    #[test]
    fn actual_ipc_round_trips_generation_bound_capture_candidate() {
        let surface = capture_surface();
        let command = DocumentTimeControlCommand::PrepareCapture(surface);
        let (command_sender, command_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlCommand>().unwrap();
        command_sender.send(command.clone()).unwrap();
        assert_eq!(command_receiver.recv().unwrap(), command);

        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 19_000_000_000_000_000_000,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: Some(DocumentExecutionLimits::API2_V1_DEFAULT),
        });
        let fence = DocumentProducerFence::with_execution_ledger(clock.execution_ledger());
        let producers = DocumentTimeProducerObservation {
            fence_id: fence.id(),
            checkpoint: DocumentProducerCheckpoint::ZERO,
            snapshot: fence.snapshot(),
            stability: DocumentProducerStability::StableEmpty,
        };
        let execution = clock
            .execution_ledger()
            .expect("controlled test clock has an execution ledger")
            .observation();
        let sources = DocumentSettlementSourceSnapshot::new_internal(
            DocumentSettlementSourceEpoch::new(u64::MAX),
            Vec::new(),
        );
        let document_epoch = DocumentCaptureDocumentEpoch {
            pipeline_id: TEST_PIPELINE_ID,
            script_rendering_epoch: Epoch::default(),
            readiness_blockers: Vec::new(),
        };
        let precondition = DocumentCapturePrecondition::new_internal(
            DocumentCapturePreconditionId::new(u64::MAX),
            target(),
            clock.id(),
            clock.now(),
            u64::MAX,
            0,
            false,
            producers,
            execution,
            None,
            sources.clone(),
            vec![document_epoch.clone()],
            surface,
        );
        let completed =
            DocumentTimeControlOutcome::Completed(Box::new(DocumentTimeControlObservation {
                target: target(),
                now: clock.now(),
                next_deadline: None,
                advance_token: None,
                pending_events: 0,
                input_batch_saturated: false,
                action: DocumentTimeControlAction::CapturePrepared,
                producers,
                documents: vec![DocumentTimeDocumentObservation {
                    pipeline_id: TEST_PIPELINE_ID,
                    script_rendering_epoch: Some(document_epoch.script_rendering_epoch),
                    readiness_blockers: Vec::new(),
                }],
                execution: Some(execution),
                capture_preparation: Some(DocumentCapturePreparation {
                    surface,
                    sources,
                    blockers: Vec::new(),
                    precondition: Some(Box::new(precondition)),
                }),
                capture_commit: None,
            }));
        let (outcome_sender, outcome_receiver) =
            ipc_channel::ipc::channel::<DocumentTimeControlOutcome>().unwrap();
        outcome_sender.send(completed.clone()).unwrap();
        assert_eq!(outcome_receiver.recv().unwrap(), completed);
    }

    fn assert_indeterminate(
        outcome: DocumentTimeControlReceiveOutcome,
        token: &DocumentTimeAdvanceToken,
    ) {
        assert!(matches!(
            outcome,
            DocumentTimeControlReceiveOutcome::CommandOutcome(
                DocumentTimeControlOutcome::AdvanceOutcomeIndeterminate {
                    token_id,
                    target: observed_target,
                    deadline,
                }
            ) if token_id == token.id() && *observed_target == *token.target() &&
                    deadline == token.deadline()
        ));
    }

    #[test]
    fn observe_timeout_cannot_hide_page_work_or_clock_mutation() {
        let command = DocumentTimeControlCommand::Observe;
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);

        assert_eq!(
            receiver.recv_timeout(Duration::ZERO),
            DocumentTimeControlReceiveOutcome::ObserveTransportFailure(
                DocumentTimeControlTransportFailure::TimedOut
            )
        );
    }

    #[test]
    fn receiver_cancels_on_timeout_or_drop_but_not_after_a_terminal_outcome() {
        let timeout_cancellations = Arc::new(AtomicUsize::new(0));
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test timeout callback channel should be created");
        let cancellation_counter = timeout_cancellations.clone();
        let receiver = DocumentTimeControlReceiver::new_cancellable(
            receiver,
            &DocumentTimeControlCommand::Observe,
            move || {
                cancellation_counter.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::ZERO),
            DocumentTimeControlReceiveOutcome::ObserveTransportFailure(
                DocumentTimeControlTransportFailure::TimedOut
            )
        ));
        assert_eq!(timeout_cancellations.load(Ordering::SeqCst), 1);

        let drop_cancellations = Arc::new(AtomicUsize::new(0));
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test dropped callback channel should be created");
        let cancellation_counter = drop_cancellations.clone();
        drop(DocumentTimeControlReceiver::new_cancellable(
            receiver,
            &DocumentTimeControlCommand::DriveOneTurn,
            move || {
                cancellation_counter.fetch_add(1, Ordering::SeqCst);
            },
        ));
        assert_eq!(drop_cancellations.load(Ordering::SeqCst), 1);

        let completed_cancellations = Arc::new(AtomicUsize::new(0));
        let (response, receiver) = GenericCallback::new_blocking()
            .expect("test completed callback channel should be created");
        let cancellation_counter = completed_cancellations.clone();
        let receiver = DocumentTimeControlReceiver::new_cancellable(
            receiver,
            &DocumentTimeControlCommand::Observe,
            move || {
                cancellation_counter.fetch_add(1, Ordering::SeqCst);
            },
        );
        response
            .send(DocumentTimeControlOutcome::Rejected(
                DocumentTimeControlError::ChannelClosed,
            ))
            .expect("test terminal outcome should be sent");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            DocumentTimeControlReceiveOutcome::CommandOutcome(
                DocumentTimeControlOutcome::Rejected(DocumentTimeControlError::ChannelClosed)
            )
        ));
        assert_eq!(completed_cancellations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn nonblocking_receiver_stays_armed_from_empty_to_success() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        let (response, receiver) = GenericCallback::new_blocking()
            .expect("test nonblocking callback channel should be created");
        let cancellation_counter = cancellations.clone();
        let receiver = DocumentTimeControlReceiver::new_cancellable(
            receiver,
            &DocumentTimeControlCommand::Observe,
            move || {
                cancellation_counter.fetch_add(1, Ordering::SeqCst);
            },
        );

        let receiver = match receiver.try_recv() {
            DocumentTimeControlTryReceiveOutcome::Pending(receiver) => receiver,
            DocumentTimeControlTryReceiveOutcome::Complete(_) => {
                panic!("an empty callback channel must remain pending")
            },
        };
        assert_eq!(cancellations.load(Ordering::SeqCst), 0);

        response
            .send(DocumentTimeControlOutcome::Rejected(
                DocumentTimeControlError::ChannelClosed,
            ))
            .expect("test terminal outcome should be sent");
        assert!(matches!(
            receiver.try_recv(),
            DocumentTimeControlTryReceiveOutcome::Complete(
                DocumentTimeControlReceiveOutcome::CommandOutcome(
                    DocumentTimeControlOutcome::Rejected(DocumentTimeControlError::ChannelClosed)
                )
            )
        ));
        assert_eq!(
            cancellations.load(Ordering::SeqCst),
            0,
            "a successful nonblocking receive must disarm cancellation"
        );
    }

    #[test]
    fn dropping_a_pending_nonblocking_receiver_cancels_exactly_once() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test nonblocking callback channel should be created");
        let cancellation_counter = cancellations.clone();
        let receiver = DocumentTimeControlReceiver::new_cancellable(
            receiver,
            &DocumentTimeControlCommand::DriveOneTurn,
            move || {
                cancellation_counter.fetch_add(1, Ordering::SeqCst);
            },
        );

        let receiver = match receiver.try_recv() {
            DocumentTimeControlTryReceiveOutcome::Pending(receiver) => receiver,
            DocumentTimeControlTryReceiveOutcome::Complete(_) => {
                panic!("an empty callback channel must remain pending")
            },
        };
        assert_eq!(cancellations.load(Ordering::SeqCst), 0);
        drop(receiver);
        assert_eq!(cancellations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn nonblocking_disconnect_completes_with_each_command_transport_semantics() {
        let (observe_response, observe_receiver) = GenericCallback::new_blocking()
            .expect("test observe callback channel should be created");
        let observe_receiver = DocumentTimeControlReceiver::new(
            observe_receiver,
            &DocumentTimeControlCommand::Observe,
        );
        drop(observe_response);
        assert!(matches!(
            observe_receiver.try_recv(),
            DocumentTimeControlTryReceiveOutcome::Complete(
                DocumentTimeControlReceiveOutcome::ObserveTransportFailure(
                    DocumentTimeControlTransportFailure::Disconnected
                )
            )
        ));

        let (drive_response, drive_receiver) =
            GenericCallback::new_blocking().expect("test drive callback channel should be created");
        let drive_receiver = DocumentTimeControlReceiver::new(
            drive_receiver,
            &DocumentTimeControlCommand::DriveOneTurn,
        );
        drop(drive_response);
        assert!(matches!(
            drive_receiver.try_recv(),
            DocumentTimeControlTryReceiveOutcome::Complete(
                DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(
                    DocumentTimeControlTransportFailure::Disconnected
                )
            )
        ));

        let (capture_response, capture_receiver) = GenericCallback::new_blocking()
            .expect("test capture callback channel should be created");
        let capture_receiver = DocumentTimeControlReceiver::new(
            capture_receiver,
            &DocumentTimeControlCommand::PrepareCapture(capture_surface()),
        );
        drop(capture_response);
        assert!(matches!(
            capture_receiver.try_recv(),
            DocumentTimeControlTryReceiveOutcome::Complete(
                DocumentTimeControlReceiveOutcome::CapturePreparationTransportFailure(
                    DocumentTimeControlTransportFailure::Disconnected
                )
            )
        ));

        let token = advance_token();
        let command = DocumentTimeControlCommand::AdvanceTo(Box::new(token.clone()));
        let (advance_response, advance_receiver) = GenericCallback::new_blocking()
            .expect("test advance callback channel should be created");
        let advance_receiver = DocumentTimeControlReceiver::new(advance_receiver, &command);
        drop(advance_response);
        match advance_receiver.try_recv() {
            DocumentTimeControlTryReceiveOutcome::Complete(outcome) => {
                assert_indeterminate(outcome, &token)
            },
            DocumentTimeControlTryReceiveOutcome::Pending(_) => {
                panic!("a disconnected callback channel must complete")
            },
        }
    }

    #[test]
    fn nonblocking_transport_error_is_terminal_and_runs_cancellation_once() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        let (_response, receiver) =
            GenericCallback::new_blocking().expect("test error callback channel should be created");
        let cancellation_counter = cancellations.clone();
        let receiver = DocumentTimeControlReceiver::new_cancellable(
            receiver,
            &DocumentTimeControlCommand::Observe,
            move || {
                cancellation_counter.fetch_add(1, Ordering::SeqCst);
            },
        );

        assert!(matches!(
            receiver.resolve_nonblocking(Err(TryReceiveError::ReceiveError(
                ReceiveError::DeserializationFailed("test corruption".to_owned())
            ))),
            DocumentTimeControlTryReceiveOutcome::Complete(
                DocumentTimeControlReceiveOutcome::ObserveTransportFailure(
                    DocumentTimeControlTransportFailure::DeserializationFailed(message)
                )
            ) if message == "test corruption"
        ));
        assert_eq!(cancellations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn drive_one_turn_timeout_is_indeterminate() {
        let command = DocumentTimeControlCommand::DriveOneTurn;
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);

        assert_eq!(
            receiver.recv_timeout(Duration::ZERO),
            DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(
                DocumentTimeControlTransportFailure::TimedOut
            )
        );
    }

    #[test]
    fn capture_preparation_timeout_is_a_distinct_transport_failure() {
        let command = DocumentTimeControlCommand::PrepareCapture(capture_surface());
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test capture-preparation callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);

        assert_eq!(
            receiver.recv_timeout(Duration::ZERO),
            DocumentTimeControlReceiveOutcome::CapturePreparationTransportFailure(
                DocumentTimeControlTransportFailure::TimedOut
            )
        );
    }

    #[test]
    fn ordinary_disconnects_remain_distinct_from_timeout() {
        let (observe_response, observe_receiver) = GenericCallback::new_blocking()
            .expect("test observe callback channel should be created");
        let observe_receiver = DocumentTimeControlReceiver::new(
            observe_receiver,
            &DocumentTimeControlCommand::Observe,
        );
        drop(observe_response);
        assert_eq!(
            observe_receiver.recv_timeout(Duration::from_secs(1)),
            DocumentTimeControlReceiveOutcome::ObserveTransportFailure(
                DocumentTimeControlTransportFailure::Disconnected
            )
        );

        let (drive_response, drive_receiver) =
            GenericCallback::new_blocking().expect("test drive callback channel should be created");
        let drive_receiver = DocumentTimeControlReceiver::new(
            drive_receiver,
            &DocumentTimeControlCommand::DriveOneTurn,
        );
        drop(drive_response);
        assert_eq!(
            drive_receiver.recv_timeout(Duration::from_secs(1)),
            DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(
                DocumentTimeControlTransportFailure::Disconnected
            )
        );
    }

    #[test]
    fn ordinary_decode_and_io_failures_keep_their_transport_identity() {
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test observe callback channel should be created");
        let receiver =
            DocumentTimeControlReceiver::new(receiver, &DocumentTimeControlCommand::Observe);
        assert_eq!(
            receiver.resolve(Err(TryReceiveError::ReceiveError(
                ReceiveError::DeserializationFailed("test corruption".to_owned()),
            ))),
            DocumentTimeControlReceiveOutcome::ObserveTransportFailure(
                DocumentTimeControlTransportFailure::DeserializationFailed(
                    "test corruption".to_owned()
                )
            )
        );

        let (_response, receiver) =
            GenericCallback::new_blocking().expect("test drive callback channel should be created");
        let receiver =
            DocumentTimeControlReceiver::new(receiver, &DocumentTimeControlCommand::DriveOneTurn);
        assert_eq!(
            receiver.resolve(Err(TryReceiveError::ReceiveError(ReceiveError::Io(
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "test pipe"),
            )))),
            DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(
                DocumentTimeControlTransportFailure::Io {
                    kind: std::io::ErrorKind::BrokenPipe,
                    message: "test pipe".to_owned(),
                }
            )
        );
    }

    #[test]
    fn late_drive_result_has_no_second_receive_path_after_timeout() {
        let (response, receiver) =
            GenericCallback::new_blocking().expect("test drive callback channel should be created");
        let receiver =
            DocumentTimeControlReceiver::new(receiver, &DocumentTimeControlCommand::DriveOneTurn);

        assert_eq!(
            receiver.recv_timeout(Duration::ZERO),
            DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(
                DocumentTimeControlTransportFailure::TimedOut
            )
        );
        // `recv_timeout` consumed and dropped the only receiver. A late callback cannot expose a
        // second result, regardless of whether the channel implementation reports send failure.
        let _ = response.send(DocumentTimeControlOutcome::Rejected(
            DocumentTimeControlError::ChannelClosed,
        ));
    }

    #[test]
    fn guarded_timeout_is_terminal_and_consumes_the_receiver() {
        let token = advance_token();
        let command = DocumentTimeControlCommand::AdvanceTo(Box::new(token.clone()));
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);

        assert_indeterminate(receiver.recv_timeout(Duration::ZERO), &token);
    }

    #[test]
    fn guarded_execution_terminal_response_is_a_definitive_rejection() {
        let token = advance_token();
        let command = DocumentTimeControlCommand::AdvanceTo(Box::new(token));
        let (response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);
        let terminal = DocumentExecutionTerminal::BudgetExceeded {
            budget: DocumentExecutionBudget::OrdinaryTasks,
            limit: 1,
            observed: 2,
        };
        response
            .send(DocumentTimeControlOutcome::Rejected(
                DocumentTimeControlError::ExecutionTerminated(terminal),
            ))
            .unwrap();

        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)),
            DocumentTimeControlReceiveOutcome::CommandOutcome(
                DocumentTimeControlOutcome::Rejected(
                    DocumentTimeControlError::ExecutionTerminated(terminal)
                )
            )
        );
    }

    #[test]
    fn guarded_disconnect_is_indeterminate() {
        let token = advance_token();
        let command = DocumentTimeControlCommand::AdvanceTo(Box::new(token.clone()));
        let (response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);
        drop(response);

        assert_indeterminate(receiver.recv_timeout(Duration::from_secs(1)), &token);
    }

    #[test]
    fn guarded_deserialization_failure_is_indeterminate() {
        let token = advance_token();
        let command = DocumentTimeControlCommand::AdvanceTo(Box::new(token.clone()));
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);

        assert_indeterminate(
            receiver.resolve(Err(TryReceiveError::ReceiveError(
                ReceiveError::DeserializationFailed("test corruption".to_owned()),
            ))),
            &token,
        );
    }
}
