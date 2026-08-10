/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Internal embedding protocol for mechanically driving one controlled document event loop.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use servo_base::Epoch;
use servo_base::generic_channel::{GenericReceiver, ReceiveError, TryReceiveError};
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

/// Definitive or explicitly indeterminate completion of one mechanical control command.
///
/// `AdvanceOutcomeIndeterminate` is intentionally not an error: after submission, shutdown,
/// transport failure, or a non-authoritative successful envelope can prevent proof of which side of
/// the clock linearization point won. The caller must not interpret that outcome as proof that time
/// stayed unchanged or retry the token.
///
/// This enum crosses Servo's callback channel and is a same-build protocol. Existing variant order
/// and payloads must remain stable; receiver-local transport states belong in
/// [`DocumentTimeControlReceiveOutcome`] instead of being inserted here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocumentTimeControlOutcome {
    /// ScriptThread completed the command and returned its authoritative observation.
    Completed(DocumentTimeControlObservation),
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
/// A missing `Observe` response is a transport failure only: observing never mutates the event
/// loop. A missing `DriveOneTurn` response is indeterminate because the turn may complete after the
/// caller stops waiting; callers must not retry it as though no event was processed. Guarded
/// advances retain their exact-token [`DocumentTimeControlOutcome::AdvanceOutcomeIndeterminate`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub enum DocumentTimeControlReceiveOutcome {
    /// Constellation delivered a typed command outcome.
    CommandOutcome(DocumentTimeControlOutcome),
    /// No observation was delivered; the `Observe` command itself was non-mutating.
    ObserveTransportFailure(DocumentTimeControlTransportFailure),
    /// The caller cannot know whether the requested event-loop turn completed.
    DriveOneTurnOutcomeIndeterminate(DocumentTimeControlTransportFailure),
}

enum DocumentTimeControlTransportSemantics {
    Observe,
    DriveOneTurn,
    GuardedAdvance(DocumentTimeControlOutcome),
}

/// One bounded, single-result receiver for a submitted document-time command.
///
/// The receiver consumes itself so a timeout or transport failure is terminal for the caller and a
/// late response cannot be mistaken for a second result. For `AdvanceTo`, every timeout,
/// disconnection, I/O failure, or deserialization failure is conservatively indeterminate and the
/// submitted token must not be retried.
#[doc(hidden)]
pub struct DocumentTimeControlReceiver {
    receiver: GenericReceiver<DocumentTimeControlOutcome>,
    transport_semantics: DocumentTimeControlTransportSemantics,
}

impl DocumentTimeControlReceiver {
    /// Bind transport-failure semantics before the command is submitted.
    #[doc(hidden)]
    pub fn new(
        receiver: GenericReceiver<DocumentTimeControlOutcome>,
        command: &DocumentTimeControlCommand,
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
        };
        Self {
            receiver,
            transport_semantics,
        }
    }

    /// Wait at most `timeout` for the command's only terminal outcome.
    pub fn recv_timeout(self, timeout: Duration) -> DocumentTimeControlReceiveOutcome {
        let result = self.receiver.try_recv_timeout(timeout);
        self.resolve(result)
    }

    fn resolve(
        self,
        result: Result<DocumentTimeControlOutcome, TryReceiveError>,
    ) -> DocumentTimeControlReceiveOutcome {
        match result {
            Ok(outcome) => DocumentTimeControlReceiveOutcome::CommandOutcome(outcome),
            Err(error) => {
                let failure = error.into();
                match self.transport_semantics {
                    DocumentTimeControlTransportSemantics::Observe => {
                        DocumentTimeControlReceiveOutcome::ObserveTransportFailure(failure)
                    },
                    DocumentTimeControlTransportSemantics::DriveOneTurn => {
                        DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(failure)
                    },
                    DocumentTimeControlTransportSemantics::GuardedAdvance(outcome) => {
                        DocumentTimeControlReceiveOutcome::CommandOutcome(outcome)
                    },
                }
            },
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
}

#[cfg(test)]
mod tests {
    use servo_base::generic_channel::{GenericCallback, ReceiveError, TryReceiveError};
    use servo_base::id::{TEST_PIPELINE_ID, TEST_SCRIPT_EVENT_LOOP_ID, TEST_WEBVIEW_ID};
    use timers::{DocumentClock, DocumentProducerFence, TimerEventRequest, TimerScheduler};

    use super::*;

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
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: 0,
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
    fn observe_timeout_is_a_non_mutating_transport_failure() {
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
        let command = DocumentTimeControlCommand::AdvanceTo(token.clone());
        let (_response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);

        assert_indeterminate(receiver.recv_timeout(Duration::ZERO), &token);
    }

    #[test]
    fn guarded_disconnect_is_indeterminate() {
        let token = advance_token();
        let command = DocumentTimeControlCommand::AdvanceTo(token.clone());
        let (response, receiver) = GenericCallback::new_blocking()
            .expect("test control callback channel should be created");
        let receiver = DocumentTimeControlReceiver::new(receiver, &command);
        drop(response);

        assert_indeterminate(receiver.recv_timeout(Duration::from_secs(1)), &token);
    }

    #[test]
    fn guarded_deserialization_failure_is_indeterminate() {
        let token = advance_token();
        let command = DocumentTimeControlCommand::AdvanceTo(token.clone());
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
