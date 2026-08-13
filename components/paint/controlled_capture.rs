/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Paint-local state and typed failures for generation-bound controlled capture.

use std::cell::{Cell, RefCell};

use embedder_traits::DocumentPaintPresentationTicket;
use servo_base::id::WebViewId;

/// A condition which can become true after the owner thread processes more Paint work.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlledDocumentCaptureRetry {
    /// The required display list or renderer frame is still pending.
    FramePending,
    /// This framebuffer already retains another single-use capture reservation.
    ReservationOccupied,
}

/// A fail-closed condition which cannot be repaired by waiting on the same candidate.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlledDocumentCaptureFailure {
    /// Paint has started shutting down and cannot accept a new capture transaction.
    ShuttingDown,
    /// The target WebView is no longer owned by Paint.
    WebViewDoesNotExist,
    /// Candidate structure is outside the supported single-root capture slice.
    InvalidPrecondition,
    /// The candidate names a different WebView.
    WebViewMismatch,
    /// Paint's current root does not exactly match the candidate root.
    RootPipelineMismatch,
    /// Another visible WebView shares the framebuffer being captured.
    SharedFramebufferMismatch,
    /// Paint's exact raster surface does not match the candidate fingerprint.
    SurfaceMismatch,
    /// Paint has already received a display list newer than the candidate epoch.
    NewerDisplayListEpoch,
    /// WebRender does not present the exact candidate epoch.
    RendererEpochMismatch,
    /// WebRender failed to synchronously render the reserved frame.
    RenderFailed,
    /// A deterministic paint-timing entry could not be queued to the control plane.
    PaintMetricDispatchFailed,
    /// A Paint animation, caret, requestAnimationFrame, or pending scroll can mutate pixels.
    AnimationActive,
    /// Frame publish notifications were missing, reordered, or otherwise ambiguous.
    PublishGenerationAmbiguous,
    /// Paint's checked presentation generation can no longer advance.
    PresentationRevisionOverflow,
    /// No retained ticket exists; this also rejects ticket replay.
    TicketNotReserved,
    /// The supplied ticket is not byte-for-byte equal to Paint's retained ticket.
    TicketMismatch,
    /// Script's commit does not exactly correlate to the retained Paint ticket.
    CommitMismatch,
    /// Paint state changed after the ticket was issued.
    PresentationInvalidated,
    /// The rendering context could not be made current for readback.
    RenderingContextUnavailable,
    /// Reading the exact framebuffer rectangle failed.
    ReadbackFailed,
}

/// Typed outcome of a controlled Paint reservation or finalization.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlledDocumentCaptureError {
    /// The same operation may be retried after owner-thread Paint progress.
    Retryable(ControlledDocumentCaptureRetry),
    /// The candidate or ticket must be discarded.
    Terminal(ControlledDocumentCaptureFailure),
}

/// Result type for generation-bound controlled Paint capture.
#[doc(hidden)]
pub type ControlledDocumentCaptureResult<T> = Result<T, ControlledDocumentCaptureError>;

/// Result of the presentation-reservation phase before Script consumes its candidate.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlledDocumentCaptureReservation {
    /// The exact composite queued paint-timing work toward Script. The caller must fully settle
    /// and obtain a new Script candidate before attempting another reservation.
    DocumentWorkQueued,
    /// Paint retained one exact, single-use presentation ticket.
    Reserved(DocumentPaintPresentationTicket),
}

pub(crate) fn frame_pending() -> ControlledDocumentCaptureError {
    ControlledDocumentCaptureError::Retryable(ControlledDocumentCaptureRetry::FramePending)
}

pub(crate) fn terminal(
    failure: ControlledDocumentCaptureFailure,
) -> ControlledDocumentCaptureError {
    ControlledDocumentCaptureError::Terminal(failure)
}

/// Classify an exact epoch comparison. Missing or older Paint state can still catch up; newer
/// state proves that this single-use candidate was missed.
pub(crate) fn require_exact_epoch(
    actual: Option<u32>,
    expected: u32,
) -> ControlledDocumentCaptureResult<()> {
    match actual {
        Some(actual) if actual == expected => Ok(()),
        None => Err(frame_pending()),
        Some(actual) if actual < expected => Err(frame_pending()),
        Some(_) => Err(terminal(
            ControlledDocumentCaptureFailure::NewerDisplayListEpoch,
        )),
    }
}

pub(crate) fn require_exact_renderer_epoch(
    actual: Option<u32>,
    expected: u32,
) -> ControlledDocumentCaptureResult<()> {
    match actual {
        Some(actual) if actual == expected => Ok(()),
        None => Err(frame_pending()),
        Some(actual) if actual < expected => Err(frame_pending()),
        Some(_) => Err(terminal(
            ControlledDocumentCaptureFailure::RendererEpochMismatch,
        )),
    }
}

pub(crate) fn require_root_and_surface(
    root_matches: bool,
    surface_matches: bool,
) -> ControlledDocumentCaptureResult<()> {
    if !root_matches {
        return Err(terminal(
            ControlledDocumentCaptureFailure::RootPipelineMismatch,
        ));
    }
    if !surface_matches {
        return Err(terminal(ControlledDocumentCaptureFailure::SurfaceMismatch));
    }
    Ok(())
}

pub(crate) fn require_no_pending_or_animation(
    pending: bool,
    animation: bool,
) -> ControlledDocumentCaptureResult<()> {
    if pending {
        return Err(frame_pending());
    }
    if animation {
        return Err(terminal(ControlledDocumentCaptureFailure::AnimationActive));
    }
    Ok(())
}

/// Keep the irreversible framebuffer read behind the complete finalization validation. This
/// helper is deliberately generic so the ordering guarantee is unit-testable without GL.
pub(crate) fn perform_after_validation<V, R>(
    validate: impl FnOnce() -> ControlledDocumentCaptureResult<V>,
    operation: impl FnOnce(V) -> ControlledDocumentCaptureResult<R>,
) -> ControlledDocumentCaptureResult<R> {
    operation(validate()?)
}

/// Owner-thread ledger for one framebuffer. `T` is generic so the single-use and overflow
/// invariants can be tested without constructing a WebRender or GL context.
pub(crate) struct ControlledCaptureLedger<T> {
    presentation_revision: Cell<u64>,
    revision_overflowed: Cell<bool>,
    reservation: RefCell<Option<T>>,
    last_publish_generation: Cell<Option<u64>>,
    highest_publish_generation: Cell<Option<u64>>,
    publish_generation_ambiguous: Cell<bool>,
}

impl<T> Default for ControlledCaptureLedger<T> {
    fn default() -> Self {
        Self {
            presentation_revision: Cell::new(0),
            revision_overflowed: Cell::new(false),
            reservation: RefCell::new(None),
            last_publish_generation: Cell::new(None),
            highest_publish_generation: Cell::new(None),
            publish_generation_ambiguous: Cell::new(false),
        }
    }
}

impl<T> ControlledCaptureLedger<T> {
    /// Advance the checked presentation revision while retaining the single ticket as a bounded
    /// failure witness. Revision validation makes it unusable, and finalization or abort consumes
    /// it exactly once.
    pub(crate) fn note_mutation(&self) -> ControlledDocumentCaptureResult<u64> {
        if self.revision_overflowed.get() {
            return Err(terminal(
                ControlledDocumentCaptureFailure::PresentationRevisionOverflow,
            ));
        }
        let Some(next) = self.presentation_revision.get().checked_add(1) else {
            self.revision_overflowed.set(true);
            return Err(terminal(
                ControlledDocumentCaptureFailure::PresentationRevisionOverflow,
            ));
        };
        self.presentation_revision.set(next);
        Ok(next)
    }

    pub(crate) fn revision(&self) -> ControlledDocumentCaptureResult<u64> {
        if self.revision_overflowed.get() {
            Err(terminal(
                ControlledDocumentCaptureFailure::PresentationRevisionOverflow,
            ))
        } else {
            Ok(self.presentation_revision.get())
        }
    }

    pub(crate) fn has_reservation(&self) -> bool {
        self.reservation.borrow().is_some()
    }

    pub(crate) fn retain(&self, ticket: T) -> ControlledDocumentCaptureResult<()> {
        if self.has_reservation() {
            return Err(ControlledDocumentCaptureError::Retryable(
                ControlledDocumentCaptureRetry::ReservationOccupied,
            ));
        }
        self.reservation.borrow_mut().replace(ticket);
        Ok(())
    }

    pub(crate) fn abort_where(&self, predicate: impl FnOnce(&T) -> bool) -> bool {
        let matches = self.reservation.borrow().as_ref().is_some_and(predicate);
        if matches {
            self.reservation.borrow_mut().take();
        }
        matches
    }

    pub(crate) fn note_publish_generation(&self, generation: u64) {
        // WebRender may advance its global ID for an internal frame build which does not request a
        // `new_frame_ready` callback, so gaps are legitimate. A regression is not: it proves that
        // Paint observed notifications out of publish order.
        if self
            .last_publish_generation
            .get()
            .is_some_and(|last| generation < last)
        {
            self.publish_generation_ambiguous.set(true);
        }
        self.last_publish_generation.set(Some(generation));
        if self
            .highest_publish_generation
            .get()
            .is_none_or(|highest| generation > highest)
        {
            self.highest_publish_generation.set(Some(generation));
        }
    }

    pub(crate) fn mark_publish_generation_ambiguous(&self) {
        self.publish_generation_ambiguous.set(true);
    }

    pub(crate) fn publish_generation(&self) -> ControlledDocumentCaptureResult<u64> {
        if self.publish_generation_ambiguous.get() {
            return Err(terminal(
                ControlledDocumentCaptureFailure::PublishGenerationAmbiguous,
            ));
        }
        match (
            self.last_publish_generation.get(),
            self.highest_publish_generation.get(),
        ) {
            (Some(last), Some(highest)) if last == highest && highest != 0 => Ok(highest),
            _ => Err(frame_pending()),
        }
    }

    #[cfg(test)]
    fn set_revision_for_test(&self, revision: u64) {
        self.presentation_revision.set(revision);
    }
}

impl<T: PartialEq> ControlledCaptureLedger<T> {
    /// Consume the reservation on every finalization attempt. A mismatch cannot leave authority
    /// behind for a second attempt.
    pub(crate) fn take_matching(&self, supplied: &T) -> ControlledDocumentCaptureResult<T> {
        let Some(retained) = self.reservation.borrow_mut().take() else {
            return Err(terminal(
                ControlledDocumentCaptureFailure::TicketNotReserved,
            ));
        };
        if &retained != supplied {
            return Err(terminal(ControlledDocumentCaptureFailure::TicketMismatch));
        }
        Ok(retained)
    }
}

impl ControlledCaptureLedger<DocumentPaintPresentationTicket> {
    /// Discard only the reservation owned by one WebView sharing this framebuffer.
    pub(crate) fn abort_for_webview(&self, webview_id: WebViewId) -> bool {
        self.abort_where(|ticket| ticket.target().webview_id == webview_id)
    }
}

#[cfg(test)]
mod tests {
    use embedder_traits::{
        DocumentCapturePreconditionId, DocumentCaptureSurfaceFingerprint,
        DocumentPaintPresentationTicketId, DocumentTimeControlTarget,
    };
    use euclid::{Box2D, Point2D, Size2D};
    use servo_base::Epoch;
    use servo_base::id::{
        BrowsingContextId, TEST_PIPELINE_ID, TEST_SCRIPT_EVENT_LOOP_ID, TEST_WEBVIEW_ID, WebViewId,
    };
    use servo_geometry::DeviceIndependentPixel;

    use super::*;

    fn ticket_for(owner: WebViewId, sequence: u64) -> DocumentPaintPresentationTicket {
        let surface = DocumentCaptureSurfaceFingerprint::new(
            Size2D::<i32, DeviceIndependentPixel>::new(200, 160),
            Box2D::new(Point2D::new(0, 0), Point2D::new(200, 160)),
            1.0,
        )
        .expect("test capture surface should be valid");
        DocumentPaintPresentationTicket::new_internal(
            DocumentPaintPresentationTicketId::new(sequence),
            DocumentCapturePreconditionId::new(sequence),
            DocumentTimeControlTarget {
                webview_id: owner,
                event_loop_id: TEST_SCRIPT_EVENT_LOOP_ID,
                webview_epoch: Epoch::default(),
                pipelines: vec![TEST_PIPELINE_ID],
                fully_active_pipelines: vec![TEST_PIPELINE_ID],
            },
            TEST_PIPELINE_ID,
            Epoch::default(),
            surface,
            sequence,
            sequence,
        )
    }

    #[test]
    fn exact_epoch_issues_without_retry() {
        assert_eq!(require_exact_epoch(Some(7), 7), Ok(()));
    }

    #[test]
    fn missing_or_behind_epoch_is_retryable_frame_pending() {
        assert_eq!(require_exact_epoch(None, 7), Err(frame_pending()));
        assert_eq!(require_exact_epoch(Some(6), 7), Err(frame_pending()));
    }

    #[test]
    fn newer_epoch_is_terminal() {
        assert_eq!(
            require_exact_epoch(Some(8), 7),
            Err(terminal(
                ControlledDocumentCaptureFailure::NewerDisplayListEpoch
            ))
        );
        assert_eq!(
            require_exact_renderer_epoch(Some(8), 7),
            Err(terminal(
                ControlledDocumentCaptureFailure::RendererEpochMismatch
            ))
        );
    }

    #[test]
    fn root_or_surface_mismatch_is_terminal() {
        assert_eq!(
            require_root_and_surface(false, true),
            Err(terminal(
                ControlledDocumentCaptureFailure::RootPipelineMismatch
            ))
        );
        assert_eq!(
            require_root_and_surface(true, false),
            Err(terminal(ControlledDocumentCaptureFailure::SurfaceMismatch))
        );
    }

    #[test]
    fn pending_frame_is_retryable() {
        assert_eq!(
            require_no_pending_or_animation(true, false),
            Err(frame_pending())
        );
    }

    #[test]
    fn mutation_invalidates_a_reservation() {
        let ledger = ControlledCaptureLedger::default();
        assert_eq!(ledger.retain(11_u64), Ok(()));
        assert_eq!(ledger.note_mutation(), Ok(1));
        assert_eq!(
            ledger.retain(12),
            Err(ControlledDocumentCaptureError::Retryable(
                ControlledDocumentCaptureRetry::ReservationOccupied
            ))
        );
        assert_eq!(ledger.revision(), Ok(1));
        assert_eq!(ledger.take_matching(&11), Ok(11));
        assert_eq!(
            ledger.take_matching(&11),
            Err(terminal(
                ControlledDocumentCaptureFailure::TicketNotReserved
            ))
        );
    }

    #[test]
    fn one_reservation_is_retained_and_abort_is_idempotent() {
        let ledger = ControlledCaptureLedger::default();
        assert_eq!(ledger.retain(11_u64), Ok(()));
        assert_eq!(
            ledger.retain(12_u64),
            Err(ControlledDocumentCaptureError::Retryable(
                ControlledDocumentCaptureRetry::ReservationOccupied
            ))
        );
        assert!(ledger.abort_where(|ticket| *ticket == 11));
        assert!(!ledger.abort_where(|ticket| *ticket == 11));
        assert_eq!(ledger.retain(12_u64), Ok(()));
        assert_eq!(ledger.note_mutation(), Ok(1));
        assert!(ledger.abort_where(|ticket| *ticket == 12));
        assert!(!ledger.abort_where(|ticket| *ticket == 12));
    }

    #[test]
    fn shared_framebuffer_abort_clears_only_the_matching_webview() {
        let ledger = ControlledCaptureLedger::default();
        let owner = TEST_WEBVIEW_ID;
        let sibling = WebViewId::mock_for_testing(
            BrowsingContextId::from_string("BrowsingContext(1234,8766)")
                .expect("test sibling browsing-context id should parse"),
        );
        assert_ne!(owner, sibling);
        let owner_ticket = ticket_for(owner, 11);
        let sibling_ticket = ticket_for(sibling, 12);

        assert_eq!(ledger.retain(owner_ticket), Ok(()));
        assert!(!ledger.abort_for_webview(sibling));
        assert!(ledger.has_reservation());
        assert!(ledger.abort_for_webview(owner));
        assert!(!ledger.abort_for_webview(owner));
        assert_eq!(ledger.retain(sibling_ticket), Ok(()));
    }

    #[test]
    fn pending_or_ambiguous_publish_cannot_issue() {
        let ledger = ControlledCaptureLedger::<u64>::default();
        assert_eq!(ledger.publish_generation(), Err(frame_pending()));
        ledger.note_publish_generation(2);
        ledger.note_publish_generation(1);
        assert_eq!(
            ledger.publish_generation(),
            Err(terminal(
                ControlledDocumentCaptureFailure::PublishGenerationAmbiguous
            ))
        );
        let skipped = ControlledCaptureLedger::<u64>::default();
        skipped.note_publish_generation(4);
        skipped.note_publish_generation(6);
        assert_eq!(skipped.publish_generation(), Ok(6));

        let invalidated = ControlledCaptureLedger::default();
        assert_eq!(invalidated.retain(11_u64), Ok(()));
        invalidated.mark_publish_generation_ambiguous();
        assert_eq!(
            invalidated.publish_generation(),
            Err(terminal(
                ControlledDocumentCaptureFailure::PublishGenerationAmbiguous
            ))
        );
        assert_eq!(invalidated.take_matching(&11), Ok(11));
    }

    #[test]
    fn processed_publish_generation_is_retained_exactly_and_a_later_frame_invalidates() {
        let ledger = ControlledCaptureLedger::default();
        let generation = u64::from(u32::MAX) + 23;
        ledger.note_publish_generation(generation);
        assert_eq!(ledger.publish_generation(), Ok(generation));
        assert_eq!(ledger.retain(11_u64), Ok(()));
        assert_eq!(ledger.note_mutation(), Ok(1));
        ledger.note_publish_generation(generation + 1);
        assert_eq!(ledger.publish_generation(), Ok(generation + 1));
        assert_eq!(ledger.revision(), Ok(1));
        assert_eq!(ledger.take_matching(&11), Ok(11));
    }

    #[test]
    fn finalization_mismatch_consumes_and_replay_fails() {
        let ledger = ControlledCaptureLedger::default();
        assert_eq!(ledger.retain(11_u64), Ok(()));
        assert_eq!(
            ledger.take_matching(&12),
            Err(terminal(ControlledDocumentCaptureFailure::TicketMismatch))
        );
        assert_eq!(
            ledger.take_matching(&11),
            Err(terminal(
                ControlledDocumentCaptureFailure::TicketNotReserved
            ))
        );
    }

    #[test]
    fn revision_overflow_latches_terminal() {
        let ledger = ControlledCaptureLedger::<u64>::default();
        ledger.set_revision_for_test(u64::MAX);
        let overflow = Err(terminal(
            ControlledDocumentCaptureFailure::PresentationRevisionOverflow,
        ));
        assert_eq!(ledger.note_mutation(), overflow);
        assert_eq!(ledger.note_mutation(), overflow);
        assert_eq!(ledger.revision(), overflow);
    }

    #[test]
    fn readback_operation_runs_only_after_matching_validation() {
        let readbacks = Cell::new(0);
        let validation_error = terminal(ControlledDocumentCaptureFailure::CommitMismatch);

        let rejected = perform_after_validation::<(), ()>(
            || Err(validation_error),
            |_| {
                readbacks.set(readbacks.get() + 1);
                Ok(())
            },
        );
        assert_eq!(rejected, Err(validation_error));
        assert_eq!(readbacks.get(), 0);

        let accepted = perform_after_validation(
            || Ok(23_u64),
            |validated| {
                readbacks.set(readbacks.get() + 1);
                Ok(validated)
            },
        );
        assert_eq!(accepted, Ok(23));
        assert_eq!(readbacks.get(), 1);
    }
}
