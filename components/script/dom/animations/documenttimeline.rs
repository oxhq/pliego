/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use js::gc::HandleObject;
use num_traits::ToPrimitive;
use script_bindings::codegen::GenericBindings::DocumentTimelineBinding::DocumentTimelineOptions;
use script_bindings::num::Finite;
use script_bindings::reflector::{
    reflect_dom_object_with_cx, reflect_dom_object_with_proto_and_cx,
};
use script_bindings::root::DomRoot;
use servo_config::pref;
use time::Duration;
use timers::{DocumentRenderingTime, DocumentTime, DocumentTimeSurface};

use crate::dom::bindings::codegen::Bindings::DocumentTimelineBinding::DocumentTimelineMethods;
use crate::dom::performance::performance::document_duration_to_dom_high_res_time_stamp;
use crate::dom::types::{AnimationTimeline, Window};

/// <https://drafts.csswg.org/web-animations-1/#the-documenttimeline-interface>
#[dom_struct]
pub(crate) struct DocumentTimeline {
    animation_timeline: AnimationTimeline,
    /// An offset from the `Document`'s time origin as a [`Duration`] offset. This is determined by the original
    /// "originTime" specified during construction of the [`AnimationTimeline`] in the options object.
    /// Note that this value might be negative.
    ///
    /// See:
    ///   - <https://drafts.csswg.org/web-animations-1/#dom-documenttimelineoptions-origintime>
    ///   - <https://html.spec.whatwg.org/multipage/#concept-settings-object-time-origin>
    #[no_trace]
    origin_offset: Duration,
}

impl DocumentTimeline {
    fn timeline_time_at(
        window: &Window,
        observed: DocumentTime,
        origin_offset: Duration,
    ) -> Duration {
        let elapsed = window
            .document_time_since_navigation(observed, DocumentTimeSurface::DocumentTimeline)
            .expect("document timeline time cannot precede the Window navigation origin");
        timeline_time_from_elapsed(elapsed, origin_offset)
    }

    fn current_timeline_time(window: &Window, origin_offset: Duration) -> Duration {
        let clock = window.as_global_scope().document_clock();
        let now = clock
            .now_for_surface(DocumentTimeSurface::DocumentTimeline)
            .expect("Window document timelines require a supported document clock");
        Self::timeline_time_at(window, now, origin_offset)
    }

    fn new_with_duration(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
        origin_time: Duration,
    ) -> DomRoot<Self> {
        let duration_since_time_origin = Self::current_timeline_time(window, origin_time);
        reflect_dom_object_with_proto_and_cx(
            Box::new(Self {
                animation_timeline: AnimationTimeline::new_inherited(duration_since_time_origin),
                origin_offset: origin_time,
            }),
            window,
            proto,
            cx,
        )
    }

    pub(crate) fn new(cx: &mut JSContext, window: &Window) -> DomRoot<DocumentTimeline> {
        let duration = if pref!(layout_animations_test_enabled) {
            Duration::ZERO
        } else {
            Self::current_timeline_time(window, Duration::ZERO)
        };
        reflect_dom_object_with_cx(
            Box::new(Self {
                animation_timeline: AnimationTimeline::new_inherited(duration),
                origin_offset: Duration::ZERO,
            }),
            window,
            cx,
        )
    }

    /// Updates the value of the `AnimationTimeline` to the rendering update's clock snapshot.
    pub(crate) fn update(&self, window: &Window, frame_time: DocumentRenderingTime) {
        let duration_since_time_origin =
            Self::timeline_time_at(window, frame_time.document_time(), self.origin_offset);
        self.animation_timeline
            .set_current_time(duration_since_time_origin);
    }

    /// Increments the current value of the timeline by a specific number of seconds.
    /// This is used for testing.
    pub(crate) fn advance_specific(&self, by: Duration) {
        self.animation_timeline.advance_specific(by);
    }
}

fn timeline_time_from_elapsed(elapsed: std::time::Duration, origin_offset: Duration) -> Duration {
    let timestamp = rendering_timestamp_from_elapsed(elapsed);
    Duration::microseconds((*timestamp * 1000.0) as i64) - origin_offset
}

pub(crate) fn rendering_timestamp_from_elapsed(elapsed: std::time::Duration) -> Finite<f64> {
    document_duration_to_dom_high_res_time_stamp(elapsed)
}

impl DocumentTimelineMethods<crate::DomTypeHolder> for DocumentTimeline {
    fn Constructor(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
        options: &DocumentTimelineOptions,
    ) -> DomRoot<Self> {
        Self::new_with_duration(
            cx,
            window,
            proto,
            Duration::seconds_f64(options.originTime.to_f64().unwrap_or_default() / 1000.),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_time_tracks_exact_frame_progress_and_origin_offset() {
        let origin_offset = Duration::milliseconds(125);

        assert_eq!(
            timeline_time_from_elapsed(std::time::Duration::from_millis(500), origin_offset),
            Duration::milliseconds(375)
        );
        assert_eq!(
            timeline_time_from_elapsed(std::time::Duration::from_millis(750), origin_offset),
            Duration::milliseconds(625)
        );
    }

    #[test]
    fn timeline_origin_offset_can_place_current_time_before_zero() {
        assert_eq!(
            timeline_time_from_elapsed(
                std::time::Duration::from_millis(25),
                Duration::milliseconds(100),
            ),
            Duration::milliseconds(-75)
        );
    }

    #[test]
    fn timeline_and_raf_share_the_same_high_resolution_quantization() {
        let elapsed = std::time::Duration::from_nanos(7_009_999);

        assert_eq!(rendering_timestamp_from_elapsed(elapsed), Finite::wrap(7.0));
        assert_eq!(
            timeline_time_from_elapsed(elapsed, Duration::ZERO),
            Duration::milliseconds(7)
        );
    }
}
