/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use paint_api::largest_contentful_paint_candidate::{LCPCandidate, LargestContentfulPaint};
use rustc_hash::{FxHashMap, FxHashSet};
use servo_base::cross_process_instant::CrossProcessInstant;
use servo_base::id::WebViewId;
use servo_constellation_traits::PaintMetricTime;
use webrender_api::PipelineId;

#[derive(Clone)]
pub(crate) struct LargestContentfulPaintMetric {
    pub(crate) paint: LargestContentfulPaint,
    pub(crate) time: PaintMetricTime,
}

/// Holds the [`LargestContentfulPaintsContainer`] for each pipeline.
#[derive(Default)]
pub(crate) struct LargestContentfulPaintCalculator {
    lcp_containers: FxHashMap<PipelineId, LargestContentfulPaintsContainer>,
    disabled_webviews: FxHashSet<WebViewId>,
}

impl LargestContentfulPaintCalculator {
    pub(crate) fn new() -> Self {
        Self {
            lcp_containers: Default::default(),
            disabled_webviews: Default::default(),
        }
    }

    pub(crate) fn append_lcp_candidate(
        &mut self,
        candidate: LCPCandidate,
        pipeline_id: PipelineId,
        webview_id: &WebViewId,
    ) {
        assert!(self.enabled_for_webview(webview_id));
        self.lcp_containers
            .entry(pipeline_id)
            .or_default()
            .lcp_candidates
            .push(candidate);
    }

    pub(crate) fn enabled_for_webview(&self, webview_id: &WebViewId) -> bool {
        !self.disabled_webviews.contains(webview_id)
    }

    pub(crate) fn remove_lcp_candidates_for_pipeline(&mut self, pipeline_id: &PipelineId) {
        self.lcp_containers.remove(pipeline_id);
    }

    pub(crate) fn calculate_largest_contentful_paint(
        &mut self,
        metric_time: PaintMetricTime,
        pipeline_id: PipelineId,
    ) -> Option<LargestContentfulPaintMetric> {
        self.lcp_containers
            .get_mut(&pipeline_id)
            .and_then(|container| container.calculate_largest_contentful_paint(metric_time))
    }

    /// <https://www.w3.org/TR/largest-contentful-paint/#limitations>
    pub(crate) fn disable_for_webview(&mut self, webview_id: WebViewId) {
        self.disabled_webviews.insert(webview_id);
    }

    pub(crate) fn enable_for_webview(&mut self, webview_id: &WebViewId) {
        self.disabled_webviews.remove(webview_id);
    }
}

/// Holds the LCP candidates and the latest LCP for a specific pipeline.
#[derive(Default)]
struct LargestContentfulPaintsContainer {
    /// List of candidates for Largest Contentful Paint in this pipeline.
    lcp_candidates: Vec<LCPCandidate>,
    /// The most recent Largest Contentful Paint, if any.
    latest_lcp: Option<LargestContentfulPaintMetric>,
}

impl LargestContentfulPaintsContainer {
    fn calculate_largest_contentful_paint(
        &mut self,
        metric_time: PaintMetricTime,
    ) -> Option<LargestContentfulPaintMetric> {
        if self.lcp_candidates.is_empty() {
            return self.latest_lcp.clone();
        }

        let calculator_time = match metric_time {
            PaintMetricTime::Host(time) => time,
            PaintMetricTime::Document(_) => CrossProcessInstant::epoch(),
        };
        let candidates = std::mem::take(&mut self.lcp_candidates);
        if let Some(max_candidate) = candidates.into_iter().max_by_key(|c| c.area) {
            match self.latest_lcp {
                None => {
                    self.latest_lcp = Some(LargestContentfulPaintMetric {
                        paint: LargestContentfulPaint::from(max_candidate, calculator_time),
                        time: metric_time,
                    });
                },
                Some(ref latest_lcp) => {
                    if max_candidate.area > latest_lcp.paint.area {
                        self.latest_lcp = Some(LargestContentfulPaintMetric {
                            paint: LargestContentfulPaint::from(max_candidate, calculator_time),
                            time: metric_time,
                        });
                    }
                },
            }
        }

        self.latest_lcp.clone()
    }
}

#[cfg(test)]
mod tests {
    use paint_api::largest_contentful_paint_candidate::LCPCandidateID;

    use super::*;

    fn candidate(id: usize, area: usize) -> LCPCandidate {
        LCPCandidate::new(LCPCandidateID(id), area, None)
    }

    #[test]
    fn smaller_host_candidate_retains_the_original_metric_time() {
        let first_time = CrossProcessInstant::epoch();
        let later_time = CrossProcessInstant::now();
        assert_ne!(first_time, later_time);
        let mut container = LargestContentfulPaintsContainer::default();
        container.lcp_candidates.push(candidate(1, 100));
        let first = container
            .calculate_largest_contentful_paint(PaintMetricTime::Host(first_time))
            .expect("candidate should produce an LCP metric");
        container.lcp_candidates.push(candidate(2, 50));
        let retained = container
            .calculate_largest_contentful_paint(PaintMetricTime::Host(later_time))
            .expect("candidate should retain an LCP metric");

        assert_eq!(first.paint.area, 100);
        assert_eq!(retained.paint.area, 100);
        assert_eq!(retained.paint.paint_time, first_time);
        assert_eq!(retained.time, PaintMetricTime::Host(first_time));
    }

    #[test]
    fn smaller_document_candidate_retains_the_original_metric_time() {
        let mut container = LargestContentfulPaintsContainer::default();
        container.lcp_candidates.push(candidate(1, 100));
        container
            .calculate_largest_contentful_paint(PaintMetricTime::Document(10))
            .expect("candidate should produce an LCP metric");
        container.lcp_candidates.push(candidate(2, 50));
        let retained = container
            .calculate_largest_contentful_paint(PaintMetricTime::Document(20))
            .expect("candidate should retain an LCP metric");

        assert_eq!(retained.paint.area, 100);
        assert_eq!(retained.time, PaintMetricTime::Document(10));
    }

    #[test]
    fn larger_candidate_replaces_area_and_metric_time() {
        let mut container = LargestContentfulPaintsContainer::default();
        container.lcp_candidates.push(candidate(1, 100));
        container
            .calculate_largest_contentful_paint(PaintMetricTime::Document(10))
            .expect("candidate should produce an LCP metric");
        container.lcp_candidates.push(candidate(2, 200));
        let replaced = container
            .calculate_largest_contentful_paint(PaintMetricTime::Document(20))
            .expect("larger candidate should replace the LCP metric");

        assert_eq!(replaced.paint.area, 200);
        assert_eq!(replaced.time, PaintMetricTime::Document(20));
    }
}
