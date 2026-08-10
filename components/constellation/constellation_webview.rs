/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use embedder_traits::user_contents::UserContentManagerId;
use embedder_traits::{DocumentClockConfiguration, InputEvent, MouseLeftViewportEvent, Theme};
use euclid::Point2D;
use log::warn;
use rustc_hash::FxHashMap;
use script_traits::{ConstellationInputEvent, ScriptThreadMessage};
use servo_base::Epoch;
use servo_base::id::{BrowsingContextId, PipelineId, ScriptEventLoopId, WebViewId};
use style_traits::CSSPixel;
use timers::DocumentTimeSurface;

use crate::browsingcontext::BrowsingContext;
use crate::pipeline::Pipeline;
use crate::session_history::JointSessionHistory;

/// The `Constellation`'s view of a `WebView` in the embedding layer. This tracks all of the
/// `Constellation` state for this `WebView`.
pub(crate) struct ConstellationWebView {
    /// The [`WebViewId`] of this [`ConstellationWebView`].
    webview_id: WebViewId,

    /// The [`PipelineId`] of the currently active pipeline at the top level of this WebView.
    pub active_top_level_pipeline_id: Option<PipelineId>,
    /// A counter for changes to [`Self::active_top_level_pipeline_id`].
    pub active_top_level_pipeline_epoch: Epoch,

    /// The currently focused browsing context in this webview for key events.
    /// The focused pipeline is the current entry of the focused browsing
    /// context.
    pub focused_browsing_context_id: BrowsingContextId,

    /// The [`BrowsingContextId`] of the currently hovered browsing context, to use for
    /// knowing which frame is currently receiving cursor events.
    pub hovered_browsing_context_id: Option<BrowsingContextId>,

    /// The last mouse move point in the coordinate space of the Pipeline that it
    /// happened int.
    pub last_mouse_move_point: Point2D<f32, CSSPixel>,

    /// The joint session history for this webview.
    pub session_history: JointSessionHistory,

    /// The [`UserContentManagerId`] for all pipelines in this `WebView`. This is `Some`
    /// if the embedder has set a `UserContentManager` using the WebViewBuilder API and
    /// it is `None` otherwise.
    pub user_content_manager_id: Option<UserContentManagerId>,

    /// Clock mode selected before this WebView's initial navigation.
    document_clock: DocumentClockConfiguration,

    /// The one ScriptEventLoop that owns a controlled WebView's clock and producer fence.
    controlled_event_loop_id: Option<ScriptEventLoopId>,

    /// Sticky unsupported-surface failure for this controlled WebView.
    document_time_failure: Option<DocumentTimeSurface>,

    /// The [`Theme`] that this [`ConstellationWebView`] uses. This is communicated to all
    /// `ScriptThread`s so that they know how to render the contents of a particular `WebView.
    theme: Theme,

    /// Whether accessibility is active for this webview.
    ///
    /// Set by [`crate::Constellation::set_accessibility_active()`], and forwarded to the
    /// webview’s *active* pipelines (of those that represent documents) at any given moment
    /// via [`ScriptThreadMessage::SetAccessibilityActive`] in `set_accessibility_active()`
    /// and [`crate::Constellation::set_frame_tree_for_webview()`].
    pub accessibility_active: bool,
}

impl ConstellationWebView {
    pub(crate) fn new(
        webview_id: WebViewId,
        focused_browsing_context_id: BrowsingContextId,
        user_content_manager_id: Option<UserContentManagerId>,
        document_clock: DocumentClockConfiguration,
    ) -> Self {
        Self {
            webview_id,
            user_content_manager_id,
            document_clock,
            controlled_event_loop_id: None,
            document_time_failure: None,
            active_top_level_pipeline_id: None,
            active_top_level_pipeline_epoch: Epoch::default(),
            focused_browsing_context_id,
            hovered_browsing_context_id: None,
            last_mouse_move_point: Default::default(),
            session_history: JointSessionHistory::new(),
            theme: Theme::Light,
            accessibility_active: false,
        }
    }

    pub(crate) const fn document_clock(&self) -> DocumentClockConfiguration {
        self.document_clock
    }

    pub(crate) const fn controlled_event_loop_id(&self) -> Option<ScriptEventLoopId> {
        self.controlled_event_loop_id
    }

    pub(crate) const fn document_time_failure(&self) -> Option<DocumentTimeSurface> {
        self.document_time_failure
    }

    pub(crate) fn bind_controlled_event_loop(
        &mut self,
        event_loop_id: ScriptEventLoopId,
    ) -> Result<(), DocumentTimeSurface> {
        if self.document_clock == DocumentClockConfiguration::Realtime {
            return Ok(());
        }
        if let Some(failure) = self.document_time_failure {
            return Err(failure);
        }
        match self.controlled_event_loop_id {
            None => {
                self.controlled_event_loop_id = Some(event_loop_id);
                Ok(())
            },
            Some(bound) if bound == event_loop_id => Ok(()),
            Some(_) => {
                self.document_time_failure = Some(DocumentTimeSurface::CrossEventLoopIframe);
                Err(DocumentTimeSurface::CrossEventLoopIframe)
            },
        }
    }

    pub(crate) fn fail_document_time(&mut self, surface: DocumentTimeSurface) {
        if self.document_clock != DocumentClockConfiguration::Realtime &&
            self.document_time_failure.is_none()
        {
            self.document_time_failure = Some(surface);
        }
    }

    /// Set the [`Theme`] on this [`ConstellationWebView`] returning true if the theme changed.
    pub(crate) fn set_theme(&mut self, new_theme: Theme) -> bool {
        let old_theme = std::mem::replace(&mut self.theme, new_theme);
        old_theme != self.theme
    }

    /// Get the [`Theme`] of this [`ConstellationWebView`].
    pub(crate) fn theme(&self) -> Theme {
        self.theme
    }

    fn target_pipeline_id_for_input_event(
        &self,
        event: &ConstellationInputEvent,
        browsing_contexts: &FxHashMap<BrowsingContextId, BrowsingContext>,
    ) -> Option<PipelineId> {
        if let Some(hit_test_result) = &event.hit_test_result {
            return Some(hit_test_result.pipeline_id);
        }

        // If there's no hit test, send the event to either the hovered or focused browsing context,
        // depending on the event type.
        let browsing_context_id = if matches!(event.event.event, InputEvent::MouseLeftViewport(_)) {
            self.hovered_browsing_context_id
                .unwrap_or(self.focused_browsing_context_id)
        } else {
            self.focused_browsing_context_id
        };

        Some(browsing_contexts.get(&browsing_context_id)?.pipeline_id)
    }

    /// Forward the [`InputEvent`] to this [`ConstellationWebView`]. Returns false if
    /// the event could not be forwarded or true otherwise.
    pub(crate) fn forward_input_event(
        &mut self,
        event: ConstellationInputEvent,
        pipelines: &FxHashMap<PipelineId, Pipeline>,
        browsing_contexts: &FxHashMap<BrowsingContextId, BrowsingContext>,
    ) -> bool {
        let Some(pipeline_id) = self.target_pipeline_id_for_input_event(&event, browsing_contexts)
        else {
            warn!("Unknown pipeline for input event. Ignoring.");
            return false;
        };
        let Some(pipeline) = pipelines.get(&pipeline_id) else {
            warn!("Unknown pipeline id {pipeline_id:?} for input event. Ignoring.");
            return false;
        };

        let mut update_hovered_browsing_context =
            |newly_hovered_browsing_context_id, focus_moving_to_another_iframe: bool| {
                let old_hovered_context_id = std::mem::replace(
                    &mut self.hovered_browsing_context_id,
                    newly_hovered_browsing_context_id,
                );
                if old_hovered_context_id == newly_hovered_browsing_context_id {
                    return;
                }
                let Some(old_hovered_context_id) = old_hovered_context_id else {
                    return;
                };
                let Some(pipeline) = browsing_contexts
                    .get(&old_hovered_context_id)
                    .and_then(|browsing_context| pipelines.get(&browsing_context.pipeline_id))
                else {
                    return;
                };

                let mut synthetic_mouse_leave_event = event.clone();
                synthetic_mouse_leave_event.event.event =
                    InputEvent::MouseLeftViewport(MouseLeftViewportEvent {
                        focus_moving_to_another_iframe,
                    });

                let _ = pipeline
                    .event_loop
                    .send(ScriptThreadMessage::SendInputEvent(
                        self.webview_id,
                        pipeline.id,
                        synthetic_mouse_leave_event,
                    ));
            };

        if let InputEvent::MouseLeftViewport(_) = &event.event.event {
            update_hovered_browsing_context(None, false);
            return true;
        }

        if let InputEvent::MouseMove(_) = &event.event.event {
            update_hovered_browsing_context(Some(pipeline.browsing_context_id), true);
            self.last_mouse_move_point = event
                .hit_test_result
                .as_ref()
                .expect("MouseMove events should always have hit tests.")
                .point_in_viewport;
        }

        let _ = pipeline
            .event_loop
            .send(ScriptThreadMessage::SendInputEvent(
                self.webview_id,
                pipeline.id,
                event,
            ));
        true
    }
}

#[cfg(test)]
mod tests {
    use servo_base::id::{
        ScriptEventLoopId, TEST_BROWSING_CONTEXT_ID, TEST_SCRIPT_EVENT_LOOP_ID, TEST_WEBVIEW_ID,
    };

    use super::*;

    fn controlled_webview() -> ConstellationWebView {
        ConstellationWebView::new(
            TEST_WEBVIEW_ID,
            TEST_BROWSING_CONTEXT_ID,
            None,
            DocumentClockConfiguration::Controlled {
                initial_time_ns: 7,
                unix_time_origin_ns: 11,
            },
        )
    }

    #[test]
    fn controlled_webview_binds_one_event_loop_and_latches_cross_loop_use() {
        let mut webview = controlled_webview();
        assert_eq!(webview.controlled_event_loop_id(), None);
        assert!(
            webview
                .bind_controlled_event_loop(TEST_SCRIPT_EVENT_LOOP_ID)
                .is_ok()
        );
        assert!(
            webview
                .bind_controlled_event_loop(TEST_SCRIPT_EVENT_LOOP_ID)
                .is_ok()
        );

        let other_event_loop = ScriptEventLoopId::new();
        assert_ne!(other_event_loop, TEST_SCRIPT_EVENT_LOOP_ID);
        assert_eq!(
            webview.bind_controlled_event_loop(other_event_loop),
            Err(DocumentTimeSurface::CrossEventLoopIframe)
        );
        assert_eq!(
            webview.document_time_failure,
            Some(DocumentTimeSurface::CrossEventLoopIframe)
        );
        assert_eq!(
            webview.controlled_event_loop_id(),
            Some(TEST_SCRIPT_EVENT_LOOP_ID)
        );
        assert_eq!(
            webview.bind_controlled_event_loop(TEST_SCRIPT_EVENT_LOOP_ID),
            Err(DocumentTimeSurface::CrossEventLoopIframe)
        );
    }
}
