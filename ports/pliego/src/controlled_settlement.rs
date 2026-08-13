/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pliego-owned mechanical coordinator for one controlled ScriptThread event loop.
//!
//! The coordinator stops at an opaque generation-bound capture candidate. It neither contacts
//! Paint nor authorizes publication; those are separate atomic-consume requirements.

use std::fmt;

use embedder_traits::{
    DocumentCaptureBlocker, DocumentCapturePrecondition, DocumentCaptureSurfaceFingerprint,
    DocumentProducerStability, DocumentSettlementSourceDisposition, DocumentTimeControlAction,
    DocumentTimeControlCommand, DocumentTimeControlError, DocumentTimeControlObservation,
    DocumentTimeControlOutcome, DocumentTimeControlReceiveOutcome,
    DocumentTimeControlTransportFailure,
};
use timers::TimerControlError;

/// The next host action required by mechanical settlement.
#[derive(Debug)]
pub(crate) enum ControlledSettlementProgress {
    Command(DocumentTimeControlCommand),
    WaitForWake,
    Candidate(Box<DocumentCapturePrecondition>),
}

/// Stateless transition policy bound to one exact capture surface.
pub(crate) struct ControlledSettlementCoordinator {
    surface: DocumentCaptureSurfaceFingerprint,
}

impl ControlledSettlementCoordinator {
    pub(crate) const fn new(surface: DocumentCaptureSurfaceFingerprint) -> Self {
        Self { surface }
    }

    pub(crate) const fn start(&self) -> ControlledSettlementProgress {
        ControlledSettlementProgress::Command(DocumentTimeControlCommand::Observe)
    }

    pub(crate) fn consume_receive_outcome(
        &self,
        outcome: DocumentTimeControlReceiveOutcome,
    ) -> Result<ControlledSettlementProgress, ControlledSettlementError> {
        match outcome {
            DocumentTimeControlReceiveOutcome::CommandOutcome(outcome) => {
                self.consume_command_outcome(outcome)
            },
            DocumentTimeControlReceiveOutcome::ObserveTransportFailure(error) => {
                Err(ControlledSettlementError::ObserveTransport(error))
            },
            DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(error) => {
                Err(ControlledSettlementError::DriveOutcomeIndeterminate(error))
            },
            DocumentTimeControlReceiveOutcome::CapturePreparationTransportFailure(error) => {
                Err(ControlledSettlementError::CaptureTransport(error))
            },
        }
    }

    fn consume_command_outcome(
        &self,
        outcome: DocumentTimeControlOutcome,
    ) -> Result<ControlledSettlementProgress, ControlledSettlementError> {
        match outcome {
            DocumentTimeControlOutcome::Completed(observation) => {
                self.consume_observation(*observation)
            },
            DocumentTimeControlOutcome::Rejected(error) if should_reobserve(&error) => Ok(
                ControlledSettlementProgress::Command(DocumentTimeControlCommand::Observe),
            ),
            DocumentTimeControlOutcome::Rejected(error) => {
                Err(ControlledSettlementError::Rejected(error))
            },
            outcome @ DocumentTimeControlOutcome::AdvanceOutcomeIndeterminate { .. } => Err(
                ControlledSettlementError::AdvanceOutcomeIndeterminate(outcome),
            ),
            outcome @ DocumentTimeControlOutcome::CaptureConsumeOutcomeIndeterminate { .. } => {
                Err(ControlledSettlementError::CaptureConsumeOutcomeIndeterminate(outcome))
            },
        }
    }

    fn consume_observation(
        &self,
        mut observation: DocumentTimeControlObservation,
    ) -> Result<ControlledSettlementProgress, ControlledSettlementError> {
        if observation.action == DocumentTimeControlAction::ExecutionTerminated ||
            observation
                .execution
                .is_some_and(|execution| execution.terminal.is_some())
        {
            return Err(ControlledSettlementError::ExecutionTerminated);
        }
        if observation.execution.is_none() {
            return Err(ControlledSettlementError::ExecutionPolicyUnavailable);
        }

        if let Some(preparation) = observation.capture_preparation.take() {
            if observation.action != DocumentTimeControlAction::CapturePrepared {
                return Err(ControlledSettlementError::InconsistentPreparation);
            }
            if preparation.surface != self.surface {
                return Err(ControlledSettlementError::CaptureSurfaceChanged);
            }
            if let Some(precondition) = preparation.precondition {
                if !preparation.blockers.is_empty() {
                    return Err(ControlledSettlementError::InconsistentPreparation);
                }
                return Ok(ControlledSettlementProgress::Candidate(precondition));
            }

            let terminal = preparation
                .blockers
                .iter()
                .copied()
                .filter(is_terminal_capture_blocker)
                .collect::<Vec<_>>();
            if !terminal.is_empty() {
                return Err(ControlledSettlementError::TerminalCaptureBlockers(terminal));
            }
            if preparation.sources.sources().iter().any(|source| {
                matches!(
                    source.disposition(),
                    DocumentSettlementSourceDisposition::OpenEnded(_) |
                        DocumentSettlementSourceDisposition::Unsupported(_)
                )
            }) {
                return Err(ControlledSettlementError::InconsistentPreparation);
            }

            // A preparation may observe new input at its bounded pre-command drain. Terminal
            // source classification above remains definitive; otherwise run that input before
            // using any deadline authority from the same observation.
            if observation.pending_events != 0 || observation.input_batch_saturated {
                return Ok(ControlledSettlementProgress::Command(
                    DocumentTimeControlCommand::DriveOneTurn,
                ));
            }
            if !observation.producers.snapshot.is_empty() {
                return Ok(ControlledSettlementProgress::WaitForWake);
            }

            // Source classification must precede use of the single-use deadline authority. An
            // interval also owns a next deadline, but policy requires failing it as OpenEnded
            // instead of advancing forever. A finite preparation returns the exact token issued
            // by the same observation, so move rather than clone it only after terminal blockers
            // have been ruled out.
            let has_finite_deadline = preparation.sources.sources().iter().any(|source| {
                matches!(
                    source.disposition(),
                    DocumentSettlementSourceDisposition::FiniteDeadline(_)
                )
            }) || preparation
                .blockers
                .contains(&DocumentCaptureBlocker::FiniteTimerDeadline) ||
                observation.next_deadline.is_some();
            if has_finite_deadline {
                let Some(token) = observation.advance_token.take() else {
                    return Err(ControlledSettlementError::FiniteSourceWithoutAuthority);
                };
                if Some(token.deadline()) != observation.next_deadline {
                    return Err(ControlledSettlementError::InconsistentPreparation);
                }
                return Ok(ControlledSettlementProgress::Command(
                    DocumentTimeControlCommand::AdvanceTo(Box::new(token)),
                ));
            }
            let needs_rendering_opportunity = preparation.sources.sources().iter().any(|source| {
                matches!(
                    source.disposition(),
                    DocumentSettlementSourceDisposition::FiniteRenderingOpportunity
                )
            }) || preparation
                .blockers
                .contains(&DocumentCaptureBlocker::ActiveFiniteSource);
            if needs_rendering_opportunity {
                return Ok(ControlledSettlementProgress::Command(
                    DocumentTimeControlCommand::DriveOneTurn,
                ));
            }
            if preparation.blockers.iter().any(|blocker| {
                matches!(
                    blocker,
                    DocumentCaptureBlocker::ProducerNotStableEmpty |
                        DocumentCaptureBlocker::QuietCheckpointsUnqualified
                )
            }) {
                return Ok(ControlledSettlementProgress::Command(
                    DocumentTimeControlCommand::DriveOneTurn,
                ));
            }
            return Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::DriveOneTurn,
            ));
        }

        // Already-received input always runs before a future classification or capture attempt.
        if observation.pending_events != 0 || observation.input_batch_saturated {
            return Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::DriveOneTurn,
            ));
        }

        let producers_stable = observation.producers.stability ==
            DocumentProducerStability::StableEmpty &&
            observation.producers.snapshot.is_empty();
        if !observation.producers.snapshot.is_empty() {
            return Ok(ControlledSettlementProgress::WaitForWake);
        }
        if !producers_stable {
            return Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::DriveOneTurn,
            ));
        }

        // Classify every stable source set before advancing a deadline. PrepareCapture performs no
        // page work and returns any exact advance token together with the source inventory.
        Ok(ControlledSettlementProgress::Command(
            DocumentTimeControlCommand::PrepareCapture(self.surface),
        ))
    }
}

fn should_reobserve(error: &DocumentTimeControlError) -> bool {
    matches!(
        error,
        DocumentTimeControlError::EventLoopUnavailable |
            DocumentTimeControlError::AdvanceInputChanged { .. } |
            DocumentTimeControlError::AdvanceClockChanged { .. } |
            DocumentTimeControlError::AdvanceProducerChanged { .. } |
            DocumentTimeControlError::TargetChanged { .. } |
            DocumentTimeControlError::Timer(TimerControlError::StaleDeadline { .. })
    )
}

fn is_terminal_capture_blocker(blocker: &DocumentCaptureBlocker) -> bool {
    matches!(
        blocker,
        DocumentCaptureBlocker::MultipleFullyActivePipelines |
            DocumentCaptureBlocker::ExecutionPolicyUnavailable |
            DocumentCaptureBlocker::ExecutionTerminated |
            DocumentCaptureBlocker::OpenEndedSource |
            DocumentCaptureBlocker::UnsupportedSource |
            DocumentCaptureBlocker::InvalidSurface
    )
}

#[derive(Debug)]
pub(crate) enum ControlledSettlementError {
    Rejected(DocumentTimeControlError),
    ObserveTransport(DocumentTimeControlTransportFailure),
    DriveOutcomeIndeterminate(DocumentTimeControlTransportFailure),
    CaptureTransport(DocumentTimeControlTransportFailure),
    AdvanceOutcomeIndeterminate(DocumentTimeControlOutcome),
    CaptureConsumeOutcomeIndeterminate(DocumentTimeControlOutcome),
    ExecutionPolicyUnavailable,
    ExecutionTerminated,
    CaptureSurfaceChanged,
    InconsistentPreparation,
    FiniteSourceWithoutAuthority,
    TerminalCaptureBlockers(Vec<DocumentCaptureBlocker>),
}

impl fmt::Display for ControlledSettlementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(error) => write!(formatter, "controlled command rejected: {error:?}"),
            Self::ObserveTransport(error) => {
                write!(
                    formatter,
                    "controlled observation transport failed: {error:?}"
                )
            },
            Self::DriveOutcomeIndeterminate(error) => write!(
                formatter,
                "controlled turn outcome is indeterminate: {error:?}"
            ),
            Self::CaptureTransport(error) => {
                write!(formatter, "capture preparation transport failed: {error:?}")
            },
            Self::AdvanceOutcomeIndeterminate(outcome) => {
                write!(
                    formatter,
                    "controlled advance outcome is indeterminate: {outcome:?}"
                )
            },
            Self::CaptureConsumeOutcomeIndeterminate(outcome) => write!(
                formatter,
                "controlled capture consume outcome is indeterminate: {outcome:?}"
            ),
            Self::ExecutionPolicyUnavailable => {
                formatter.write_str("controlled execution policy is unavailable")
            },
            Self::ExecutionTerminated => {
                formatter.write_str("controlled execution reached a terminal limit")
            },
            Self::CaptureSurfaceChanged => {
                formatter.write_str("capture preparation returned another surface")
            },
            Self::InconsistentPreparation => {
                formatter.write_str("capture preparation response is internally inconsistent")
            },
            Self::FiniteSourceWithoutAuthority => formatter
                .write_str("finite settlement source has no exact scheduler advance authority"),
            Self::TerminalCaptureBlockers(blockers) => {
                write!(formatter, "capture preparation failed closed: {blockers:?}")
            },
        }
    }
}

impl std::error::Error for ControlledSettlementError {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use embedder_traits::{
        DocumentCapturePreconditionId, DocumentCapturePreparation, DocumentExecutionLimits,
        DocumentSettlementSource, DocumentSettlementSourceEpoch, DocumentSettlementSourceSnapshot,
        DocumentTimeAdvanceToken, DocumentTimeAdvanceTokenId, DocumentTimeControlTarget,
        DocumentTimeDocumentObservation, DocumentTimeProducerObservation, DocumentUnixTime,
    };
    use servo_base::id::{TEST_PIPELINE_ID, TEST_SCRIPT_EVENT_LOOP_ID, TEST_WEBVIEW_ID};
    use servo_geometry::{
        DeviceIndependentIntPoint, DeviceIndependentIntRect, DeviceIndependentIntSize,
    };
    use timers::{
        DocumentClock, DocumentClockConfiguration, DocumentProducerCheckpoint,
        DocumentProducerFence, DocumentTime, TimerEventRequest, TimerScheduler,
    };

    use super::*;

    #[test]
    fn inconsistent_preparation_has_a_generic_message() {
        assert_eq!(
            ControlledSettlementError::InconsistentPreparation.to_string(),
            "capture preparation response is internally inconsistent"
        );
    }

    fn surface() -> DocumentCaptureSurfaceFingerprint {
        DocumentCaptureSurfaceFingerprint::new(
            DeviceIndependentIntSize::new(800, 600),
            DeviceIndependentIntRect::new(
                DeviceIndependentIntPoint::new(0, 0),
                DeviceIndependentIntPoint::new(800, 600),
            ),
            1.0,
        )
        .unwrap()
    }

    fn target() -> DocumentTimeControlTarget {
        DocumentTimeControlTarget {
            webview_id: TEST_WEBVIEW_ID,
            event_loop_id: TEST_SCRIPT_EVENT_LOOP_ID,
            webview_epoch: Default::default(),
            pipelines: vec![TEST_PIPELINE_ID],
            fully_active_pipelines: vec![TEST_PIPELINE_ID],
        }
    }

    fn observation() -> (DocumentClock, DocumentTimeControlObservation) {
        let clock = DocumentClock::new(DocumentClockConfiguration::Controlled {
            initial_time_ns: 0,
            unix_time_origin_ns: DocumentUnixTime::default(),
            execution_limits: Some(DocumentExecutionLimits::API2_V1_DEFAULT),
        });
        let fence = DocumentProducerFence::with_execution_ledger(clock.execution_ledger());
        let observation = DocumentTimeControlObservation {
            target: target(),
            now: clock.now(),
            next_deadline: None,
            advance_token: None,
            pending_events: 0,
            input_batch_saturated: false,
            action: DocumentTimeControlAction::Observed,
            producers: DocumentTimeProducerObservation {
                fence_id: fence.id(),
                checkpoint: DocumentProducerCheckpoint::ZERO,
                snapshot: fence.snapshot(),
                stability: DocumentProducerStability::StableEmpty,
            },
            documents: vec![DocumentTimeDocumentObservation {
                pipeline_id: TEST_PIPELINE_ID,
                script_rendering_epoch: Some(Default::default()),
                readiness_blockers: Vec::new(),
            }],
            execution: Some(clock.execution_ledger().unwrap().observation()),
            capture_preparation: None,
            capture_commit: None,
        };
        (clock, observation)
    }

    fn completed(observation: DocumentTimeControlObservation) -> DocumentTimeControlReceiveOutcome {
        DocumentTimeControlReceiveOutcome::CommandOutcome(DocumentTimeControlOutcome::Completed(
            Box::new(observation),
        ))
    }

    fn advance_token(
        clock: &DocumentClock,
        producers: DocumentTimeProducerObservation,
    ) -> DocumentTimeAdvanceToken {
        let mut scheduler = TimerScheduler::with_clock(clock.clone());
        scheduler.schedule_timer(TimerEventRequest {
            callback: Box::new(|| {}),
            duration: Duration::from_nanos(10),
        });
        DocumentTimeAdvanceToken::new_internal(
            DocumentTimeAdvanceTokenId::new(1),
            target(),
            clock.id(),
            clock.now(),
            scheduler
                .finite_deadline_snapshot()
                .unwrap()
                .expect("test timer should expose one deadline"),
            0,
            0,
            false,
            producers,
        )
    }

    #[test]
    fn stable_ready_observation_prepares_exact_surface() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (_, observation) = observation();

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::PrepareCapture(observed)
            )) if observed == surface()
        ));
    }

    #[test]
    fn pending_input_is_driven_before_capture() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (_, mut observation) = observation();
        observation.pending_events = 1;

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::DriveOneTurn
            ))
        ));
    }

    #[test]
    fn open_ended_source_is_terminal_before_finite_authority_checks() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (clock, mut observation) = observation();
        let token = advance_token(&clock, observation.producers);
        observation.next_deadline = Some(token.deadline());
        observation.advance_token = Some(token);
        observation.pending_events = 1;
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                vec![DocumentSettlementSource::timer_internal(
                    TEST_PIPELINE_ID,
                    1,
                    1,
                    1,
                    DocumentTime::from_nanos(10),
                    true,
                )],
            ),
            blockers: vec![DocumentCaptureBlocker::OpenEndedSource],
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Err(ControlledSettlementError::TerminalCaptureBlockers(blockers))
                if blockers == vec![DocumentCaptureBlocker::OpenEndedSource]
        ));
    }

    #[test]
    fn preparation_payload_requires_the_capture_prepared_action() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (_, mut observation) = observation();
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                Vec::new(),
            ),
            blockers: vec![DocumentCaptureBlocker::QuietCheckpointsUnqualified],
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Err(ControlledSettlementError::InconsistentPreparation)
        ));
    }

    #[test]
    fn terminal_source_without_its_fail_closed_blocker_is_inconsistent() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (_, mut observation) = observation();
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                vec![DocumentSettlementSource::timer_internal(
                    TEST_PIPELINE_ID,
                    1,
                    1,
                    1,
                    DocumentTime::from_nanos(10),
                    true,
                )],
            ),
            blockers: Vec::new(),
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Err(ControlledSettlementError::InconsistentPreparation)
        ));
    }

    #[test]
    fn finite_preparation_uses_only_its_same_observation_authority() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (clock, mut observation) = observation();
        let token = advance_token(&clock, observation.producers);
        let expected_id = token.id();
        observation.next_deadline = Some(token.deadline());
        observation.advance_token = Some(token);
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                vec![DocumentSettlementSource::timer_internal(
                    TEST_PIPELINE_ID,
                    1,
                    1,
                    1,
                    DocumentTime::from_nanos(10),
                    false,
                )],
            ),
            blockers: vec![DocumentCaptureBlocker::FiniteTimerDeadline],
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::AdvanceTo(token)
            )) if token.id() == expected_id
        ));
    }

    #[test]
    fn finite_timer_authority_precedes_a_mixed_rendering_opportunity() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (clock, mut observation) = observation();
        let token = advance_token(&clock, observation.producers);
        let expected_id = token.id();
        observation.next_deadline = Some(token.deadline());
        observation.advance_token = Some(token);
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                vec![
                    DocumentSettlementSource::animation_frame_internal(TEST_PIPELINE_ID, 7, true),
                    DocumentSettlementSource::timer_internal(
                        TEST_PIPELINE_ID,
                        1,
                        1,
                        1,
                        DocumentTime::from_nanos(10),
                        false,
                    ),
                ],
            ),
            blockers: vec![
                DocumentCaptureBlocker::ActiveFiniteSource,
                DocumentCaptureBlocker::FiniteTimerDeadline,
            ],
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::AdvanceTo(token)
            )) if token.id() == expected_id
        ));
    }

    #[test]
    fn finite_preparation_without_same_observation_authority_fails_closed() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (clock, mut observation) = observation();
        observation.next_deadline = Some(advance_token(&clock, observation.producers).deadline());
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                vec![DocumentSettlementSource::timer_internal(
                    TEST_PIPELINE_ID,
                    1,
                    1,
                    1,
                    DocumentTime::from_nanos(10),
                    false,
                )],
            ),
            blockers: vec![DocumentCaptureBlocker::FiniteTimerDeadline],
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Err(ControlledSettlementError::FiniteSourceWithoutAuthority)
        ));
    }

    #[test]
    fn finite_authority_must_match_the_observed_scheduler_deadline() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (clock, mut observation) = observation();
        observation.advance_token = Some(advance_token(&clock, observation.producers));
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                vec![DocumentSettlementSource::timer_internal(
                    TEST_PIPELINE_ID,
                    1,
                    1,
                    1,
                    DocumentTime::from_nanos(10),
                    false,
                )],
            ),
            blockers: vec![DocumentCaptureBlocker::FiniteTimerDeadline],
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Err(ControlledSettlementError::InconsistentPreparation)
        ));
    }

    #[test]
    fn finite_rendering_opportunity_drives_without_timer_authority() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (_, mut observation) = observation();
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources: DocumentSettlementSourceSnapshot::new_internal(
                DocumentSettlementSourceEpoch::new(1),
                vec![DocumentSettlementSource::animation_frame_internal(
                    TEST_PIPELINE_ID,
                    7,
                    true,
                )],
            ),
            blockers: vec![DocumentCaptureBlocker::ActiveFiniteSource],
            precondition: None,
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::DriveOneTurn
            ))
        ));
    }

    #[test]
    fn successful_preparation_returns_candidate_and_sends_no_command() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (clock, mut observation) = observation();
        let sources = DocumentSettlementSourceSnapshot::new_internal(
            DocumentSettlementSourceEpoch::new(1),
            Vec::new(),
        );
        let precondition = DocumentCapturePrecondition::new_internal(
            DocumentCapturePreconditionId::new(1),
            target(),
            clock.id(),
            clock.now(),
            0,
            0,
            false,
            observation.producers,
            observation.execution.unwrap(),
            None,
            sources.clone(),
            Vec::new(),
            surface(),
        );
        observation.action = DocumentTimeControlAction::CapturePrepared;
        observation.capture_preparation = Some(DocumentCapturePreparation {
            surface: surface(),
            sources,
            blockers: Vec::new(),
            precondition: Some(Box::new(precondition)),
        });

        assert!(matches!(
            coordinator.consume_receive_outcome(completed(observation)),
            Ok(ControlledSettlementProgress::Candidate(candidate))
                if candidate.id().get() == 1
        ));
    }

    #[test]
    fn guarded_races_reobserve_but_indeterminate_outcomes_are_terminal() {
        let coordinator = ControlledSettlementCoordinator::new(surface());
        let (_, observation) = observation();
        let target = observation.target;
        assert!(matches!(
            coordinator.consume_receive_outcome(DocumentTimeControlReceiveOutcome::CommandOutcome(
                DocumentTimeControlOutcome::Rejected(DocumentTimeControlError::TargetChanged {
                    expected: Box::new(target.clone()),
                    observed: None,
                })
            )),
            Ok(ControlledSettlementProgress::Command(
                DocumentTimeControlCommand::Observe
            ))
        ));

        assert!(matches!(
            coordinator.consume_receive_outcome(
                DocumentTimeControlReceiveOutcome::DriveOneTurnOutcomeIndeterminate(
                    DocumentTimeControlTransportFailure::TimedOut,
                )
            ),
            Err(ControlledSettlementError::DriveOutcomeIndeterminate(
                DocumentTimeControlTransportFailure::TimedOut
            ))
        ));
    }
}
