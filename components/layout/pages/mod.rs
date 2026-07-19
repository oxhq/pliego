/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The Pliego-owned paged-document root.
//!
//! Simple normal-flow blocks are continued at child boundaries, and retained paragraph line
//! fragments can continue at line boundaries. Unsupported content can still overflow, but nothing
//! here clips rendered output or reshapes text.

use std::fmt;
use std::sync::OnceLock;

use app_units::Au;
use euclid::default::Size2D as UntypedSize2D;
use euclid::{Point2D, Rect, Size2D};
use layout_api::{
    LayoutDebugContinuation, LayoutDebugPage, LayoutDebugPageContinuation, LayoutDebugPageSequence,
};
use log::warn;
use parking_lot::Mutex;
use style::Zero;
use style_traits::CSSPixel;

use crate::context::LayoutContext;
use crate::flow::BoxTree;
use crate::fragment_tree::FragmentTree;
use crate::geom::PhysicalSides;

static PROCESS_PAGE_DEFINITION: OnceLock<PageDefinition> = OnceLock::new();

/// Physical page margins in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageMargins {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl PageMargins {
    pub const fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }
}

/// A validated physical page definition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageDefinition {
    size: Size2D<Au, CSSPixel>,
    margins: PhysicalSides<Au>,
}

impl PageDefinition {
    /// Resolve CSS-pixel page options into app units before layout starts.
    pub fn new(width: f32, height: f32, margins: PageMargins) -> Result<Self, PageGeometryError> {
        if !positive_finite(width) || !positive_finite(height) {
            return Err(PageGeometryError::InvalidPageSize);
        }
        if !nonnegative_finite(margins.top)
            || !nonnegative_finite(margins.right)
            || !nonnegative_finite(margins.bottom)
            || !nonnegative_finite(margins.left)
        {
            return Err(PageGeometryError::InvalidPageMargin);
        }
        if margins.left + margins.right >= width || margins.top + margins.bottom >= height {
            return Err(PageGeometryError::MarginsConsumePage);
        }

        let definition = Self {
            size: Size2D::new(Au::from_f32_px(width), Au::from_f32_px(height)),
            margins: PhysicalSides::new(
                Au::from_f32_px(margins.top),
                Au::from_f32_px(margins.right),
                Au::from_f32_px(margins.bottom),
                Au::from_f32_px(margins.left),
            ),
        };
        if definition.available_inline_size() <= Au::from_px(0)
            || definition.available_block_size() <= Au::from_px(0)
        {
            return Err(PageGeometryError::MarginsConsumePage);
        }
        Ok(definition)
    }

    pub fn width(&self) -> f32 {
        self.size.width.to_f32_px()
    }

    pub fn height(&self) -> f32 {
        self.size.height.to_f32_px()
    }

    pub fn margins(&self) -> PageMargins {
        PageMargins {
            top: self.margins.top.to_f32_px(),
            right: self.margins.right.to_f32_px(),
            bottom: self.margins.bottom.to_f32_px(),
            left: self.margins.left.to_f32_px(),
        }
    }

    fn available_inline_size(&self) -> Au {
        self.size.width - self.margins.left - self.margins.right
    }

    fn available_block_size(&self) -> Au {
        self.size.height - self.margins.top - self.margins.bottom
    }

    fn content_rect(&self) -> Rect<Au, CSSPixel> {
        Rect::new(
            Point2D::new(self.margins.left, self.margins.top),
            Size2D::new(self.available_inline_size(), self.available_block_size()),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageGeometryError {
    InvalidPageSize,
    InvalidPageMargin,
    MarginsConsumePage,
}

impl fmt::Display for PageGeometryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPageSize => "page width and height must be finite and positive",
            Self::InvalidPageMargin => "page margins must be finite and nonnegative",
            Self::MarginsConsumePage => "page margins must leave positive inline and block space",
        })
    }
}

impl std::error::Error for PageGeometryError {}

/// Configure the one page used by Pliego's one-render-per-process CLI.
///
/// Servo embedders that do not call this retain continuous layout. This must be called before Servo
/// starts worker threads; the immutable process value intentionally cannot be replaced mid-session.
pub fn configure_for_process(definition: PageDefinition) -> Result<(), PageDefinition> {
    PROCESS_PAGE_DEFINITION.set(definition)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContinuousViewport {
    size: UntypedSize2D<Au>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PageSequenceContext {
    definition: PageDefinition,
}

impl PageSequenceContext {
    #[cfg(test)]
    fn one_page_sequence(self) -> PageSequence {
        self.page_sequence(BlockPaginationOutcome::default())
    }

    fn page_sequence(self, outcome: BlockPaginationOutcome) -> PageSequence {
        let page_count = outcome.page_count.max(1);
        let pages = (0..page_count)
            .map(|page_index| Page {
                definition: self.definition,
                fragmentainer: FragmentainerContext {
                    page_index,
                    available_inline_size: self.definition.available_inline_size(),
                    available_block_size: self.definition.available_block_size(),
                    continuation: outcome
                        .continuations
                        .iter()
                        .find(|continuation| continuation.page_index == page_index)
                        .map(|continuation| continuation.token),
                    overflow: FragmentainerOverflow::RetainInFragmentTree,
                },
            })
            .collect();
        PageSequence {
            pages,
            warnings: outcome.warnings,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlockContinuationToken {
    pub next_child_index: usize,
    pub next_node: Option<u64>,
    pub resume_page_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InlineContinuationToken {
    pub child_index: usize,
    pub next_line_index: usize,
    pub inline_item_index: usize,
    pub text_offset: usize,
    pub shaping_result_index: usize,
    pub resume_page_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FragmentainerContinuation {
    Block(BlockContinuationToken),
    Inline(InlineContinuationToken),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InlineResumePoint {
    pub inline_item_index: usize,
    pub text_offset: usize,
    pub shaping_result_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InlineLine {
    /// Unpaginated block start in the root fragmentainer coordinate space.
    pub block_start: Au,
    pub block_size: Au,
    pub source_start: usize,
    pub source_end: usize,
    pub resume: InlineResumePoint,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct InlineChildPlacement {
    pub line_translations: Vec<Au>,
    pub added_block_size: Au,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlockBoundaryPlacement {
    CurrentPage,
    NewPage { block_origin: Au },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlockPaginationWarning {
    OversizedUnbreakable {
        child_index: usize,
        node: Option<u64>,
        block_size: Au,
        available_block_size: Au,
    },
}

#[derive(Clone, Copy)]
struct BlockPaginationRequest {
    available_block_size: Au,
    page_stride: Au,
}

#[derive(Default)]
struct BlockPaginationControllerState {
    request: Option<BlockPaginationRequest>,
    claimed: bool,
    outcome: Option<BlockPaginationOutcome>,
}

/// Per-reflow hand-off between the paged root and the outermost multi-child block container.
#[derive(Default)]
pub(crate) struct BlockPaginationController {
    state: Mutex<BlockPaginationControllerState>,
}

impl BlockPaginationController {
    fn begin(&self, definition: PageDefinition) {
        *self.state.lock() = BlockPaginationControllerState {
            request: Some(BlockPaginationRequest {
                available_block_size: definition.available_block_size(),
                page_stride: definition.size.height,
            }),
            ..Default::default()
        };
    }

    pub(crate) fn claim(
        &self,
        child_count: usize,
        supports_simple_block_pagination: bool,
        supports_single_inline_child: bool,
    ) -> Option<BlockPageBuilder> {
        let mut state = self.state.lock();
        if (child_count < 2 && !supports_single_inline_child) || state.claimed {
            return None;
        }
        let request = state.request?;
        state.claimed = true;
        supports_simple_block_pagination.then(|| BlockPageBuilder::new(request))
    }

    pub(crate) fn complete(&self, outcome: BlockPaginationOutcome) {
        let mut state = self.state.lock();
        debug_assert!(state.claimed);
        debug_assert!(state.outcome.is_none());
        state.outcome = Some(outcome);
    }

    fn finish(&self) -> BlockPaginationOutcome {
        let mut state = self.state.lock();
        state.request = None;
        state.outcome.take().unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PageContinuation {
    page_index: usize,
    token: FragmentainerContinuation,
}

#[derive(Default)]
pub(crate) struct BlockPaginationOutcome {
    page_count: usize,
    continuations: Vec<PageContinuation>,
    warnings: Vec<BlockPaginationWarning>,
}

/// Places one unbreakable child at a time and only advances at legal child boundaries.
pub(crate) struct BlockPageBuilder {
    request: BlockPaginationRequest,
    current_page_index: usize,
    current_page_origin: Au,
    next_child_index: usize,
    page_has_content: bool,
    outcome: BlockPaginationOutcome,
}

impl BlockPageBuilder {
    fn new(request: BlockPaginationRequest) -> Self {
        Self {
            request,
            current_page_index: 0,
            current_page_origin: Au::zero(),
            next_child_index: 0,
            page_has_content: false,
            outcome: BlockPaginationOutcome {
                page_count: 1,
                ..Default::default()
            },
        }
    }

    pub(crate) fn place_child(
        &mut self,
        child_index: usize,
        node: Option<u64>,
        current_block_position: Au,
        current_page_contribution: Au,
        fresh_page_contribution: Au,
    ) -> BlockBoundaryPlacement {
        assert_eq!(child_index, self.next_child_index);
        self.next_child_index += 1;

        let used = (current_block_position - self.current_page_origin).max(Au::zero());
        let remaining = (self.request.available_block_size - used).max(Au::zero());
        if current_page_contribution <= remaining || !self.page_has_content {
            self.warn_if_oversized(child_index, node, fresh_page_contribution);
            self.page_has_content = true;
            self.include_content_through(current_block_position + current_page_contribution);
            return BlockBoundaryPlacement::CurrentPage;
        }

        let previous_page_index = self.current_page_index;
        loop {
            self.current_page_index += 1;
            self.current_page_origin += self.request.page_stride;
            if self.current_page_origin >= current_block_position {
                break;
            }
        }

        let token = BlockContinuationToken {
            next_child_index: child_index,
            next_node: node,
            resume_page_index: self.current_page_index,
        };
        self.outcome.continuations.push(PageContinuation {
            page_index: previous_page_index,
            token: FragmentainerContinuation::Block(token),
        });
        self.warn_if_oversized(child_index, node, fresh_page_contribution);
        self.page_has_content = true;
        self.include_content_through(self.current_page_origin + fresh_page_contribution);
        BlockBoundaryPlacement::NewPage {
            block_origin: self.current_page_origin,
        }
    }

    /// Place already-shaped lines without rebuilding the inline formatting context. Each output
    /// translation corresponds one-to-one with the input line, so pagination cannot consume or
    /// duplicate a retained line fragment.
    pub(crate) fn place_inline_child(
        &mut self,
        child_index: usize,
        lines: &[InlineLine],
    ) -> Option<InlineChildPlacement> {
        assert_eq!(child_index, self.next_child_index);
        if !self.supports_inline_lines(lines) {
            return None;
        }
        self.next_child_index += 1;

        let mut line_translations = Vec::with_capacity(lines.len());
        let mut translation = Au::zero();
        for (line_index, line) in lines.iter().enumerate() {
            let mut block_start = line.block_start + translation;
            let page_end = self.current_page_origin + self.request.available_block_size;
            if block_start + line.block_size > page_end {
                let previous_page_index = self.current_page_index;
                let mut resume_page_index = self.current_page_index + 1;
                let mut resume_page_origin = self.current_page_origin + self.request.page_stride;
                while resume_page_origin < block_start {
                    resume_page_index += 1;
                    resume_page_origin += self.request.page_stride;
                }
                translation += resume_page_origin - block_start;
                block_start = resume_page_origin;
                self.current_page_index = resume_page_index;
                self.current_page_origin = resume_page_origin;
                self.outcome.continuations.push(PageContinuation {
                    page_index: previous_page_index,
                    token: FragmentainerContinuation::Inline(InlineContinuationToken {
                        child_index,
                        next_line_index: line_index,
                        inline_item_index: line.resume.inline_item_index,
                        text_offset: line.resume.text_offset,
                        shaping_result_index: line.resume.shaping_result_index,
                        resume_page_index,
                    }),
                });
            }

            line_translations.push(translation);
            self.page_has_content = true;
            self.include_content_through(block_start + line.block_size);
        }

        Some(InlineChildPlacement {
            added_block_size: *line_translations.last().expect("inline lines are non-empty"),
            line_translations,
        })
    }

    fn supports_inline_lines(&self, lines: &[InlineLine]) -> bool {
        if lines.len() < 2 {
            return false;
        }
        let mut previous: Option<&InlineLine> = None;
        for line in lines {
            if line.block_size <= Au::zero()
                || line.block_size > self.request.available_block_size
                || line.source_start >= line.source_end
                || line.resume.text_offset != line.source_start
            {
                return false;
            }
            if let Some(previous) = previous {
                let previous_progress = (
                    previous.resume.inline_item_index,
                    previous.resume.shaping_result_index,
                    previous.resume.text_offset,
                );
                let progress = (
                    line.resume.inline_item_index,
                    line.resume.shaping_result_index,
                    line.resume.text_offset,
                );
                if line.block_start < previous.block_start
                    || line.source_start < previous.source_end
                    || progress <= previous_progress
                {
                    return false;
                }
            }
            previous = Some(line);
        }
        true
    }

    fn warn_if_oversized(&mut self, child_index: usize, node: Option<u64>, block_size: Au) {
        if block_size <= self.request.available_block_size {
            return;
        }
        warn!(
            "paged layout retained oversized unbreakable child {child_index} (node {node:?}, \
             {:.2}px > {:.2}px)",
            block_size.to_f32_px(),
            self.request.available_block_size.to_f32_px(),
        );
        self.outcome
            .warnings
            .push(BlockPaginationWarning::OversizedUnbreakable {
                child_index,
                node,
                block_size,
                available_block_size: self.request.available_block_size,
            });
    }

    fn include_content_through(&mut self, block_end: Au) {
        let mut page_origin = self.current_page_origin;
        let mut page_index = self.current_page_index;
        while page_origin + self.request.page_stride < block_end {
            page_origin += self.request.page_stride;
            page_index += 1;
        }
        self.outcome.page_count = self.outcome.page_count.max(page_index + 1);
    }

    pub(crate) fn finish(mut self) -> BlockPaginationOutcome {
        self.outcome.page_count = self.outcome.page_count.max(self.current_page_index + 1);
        self.outcome
    }
}

/// The root-layout dispatch. This is distinct from Servo's incremental absolute-position layout
/// root in `crate::layout_root`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LayoutRoot {
    Continuous(ContinuousViewport),
    Paged(PageSequenceContext),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FragmentainerContext {
    pub page_index: usize,
    pub available_inline_size: Au,
    pub available_block_size: Au,
    pub continuation: Option<FragmentainerContinuation>,
    pub overflow: FragmentainerOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FragmentainerOverflow {
    /// Keep content beyond the one-page fragmentainer in the fragment tree for later pagination.
    RetainInFragmentTree,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Page {
    pub definition: PageDefinition,
    pub fragmentainer: FragmentainerContext,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PageSequence {
    pub pages: Vec<Page>,
    pub warnings: Vec<BlockPaginationWarning>,
}

impl PageSequence {
    pub(crate) fn debug_snapshot(&self) -> LayoutDebugPageSequence {
        LayoutDebugPageSequence {
            pages: self
                .pages
                .iter()
                .map(|page| {
                    let margins = page.definition.margins();
                    LayoutDebugPage {
                        index: page.fragmentainer.page_index,
                        width: page.definition.width(),
                        height: page.definition.height(),
                        margin_top: margins.top,
                        margin_right: margins.right,
                        margin_bottom: margins.bottom,
                        margin_left: margins.left,
                        available_inline_size: page.fragmentainer.available_inline_size.to_f32_px(),
                        available_block_size: page.fragmentainer.available_block_size.to_f32_px(),
                    }
                })
                .collect(),
            continuations: self
                .pages
                .iter()
                .filter_map(|page| {
                    page.fragmentainer
                        .continuation
                        .map(|token| LayoutDebugPageContinuation {
                            page_index: page.fragmentainer.page_index,
                            token: match token {
                                FragmentainerContinuation::Block(token) => {
                                    LayoutDebugContinuation::Block {
                                        next_child_index: token.next_child_index,
                                        next_node: token.next_node,
                                        resume_page_index: token.resume_page_index,
                                    }
                                },
                                FragmentainerContinuation::Inline(token) => {
                                    LayoutDebugContinuation::Inline {
                                        child_index: token.child_index,
                                        next_line_index: token.next_line_index,
                                        inline_item_index: token.inline_item_index,
                                        text_offset: token.text_offset,
                                        shaping_result_index: token.shaping_result_index,
                                        resume_page_index: token.resume_page_index,
                                    }
                                },
                            },
                        })
                })
                .collect(),
        }
    }
}

pub(crate) struct RootLayout {
    pub fragment_tree: FragmentTree,
    pub page_sequence: Option<PageSequence>,
}

impl RootLayout {
    pub(crate) fn continuous(fragment_tree: FragmentTree) -> Self {
        Self {
            fragment_tree,
            page_sequence: None,
        }
    }
}

pub(crate) fn configured_layout_root(is_iframe: bool, viewport: UntypedSize2D<Au>) -> LayoutRoot {
    select_layout_root(PROCESS_PAGE_DEFINITION.get().copied(), is_iframe, viewport)
}

/// Paged layout is deliberately sequential until continuation ownership is carried explicitly
/// through every nested formatting context. Continuous documents and iframes keep Servo's setting.
pub(crate) fn allow_parallel_layout(is_iframe: bool, pool_available: bool) -> bool {
    parallel_layout_allowed(
        PROCESS_PAGE_DEFINITION.get().copied(),
        is_iframe,
        pool_available,
    )
}

fn parallel_layout_allowed(
    page: Option<PageDefinition>,
    is_iframe: bool,
    pool_available: bool,
) -> bool {
    pool_available && (page.is_none() || is_iframe)
}

fn select_layout_root(
    page: Option<PageDefinition>,
    is_iframe: bool,
    viewport: UntypedSize2D<Au>,
) -> LayoutRoot {
    match (page, is_iframe) {
        (Some(definition), false) => LayoutRoot::Paged(PageSequenceContext { definition }),
        _ => LayoutRoot::Continuous(ContinuousViewport { size: viewport }),
    }
}

pub(crate) fn layout(
    box_tree: &BoxTree,
    layout_context: &LayoutContext,
    root: LayoutRoot,
) -> RootLayout {
    match root {
        LayoutRoot::Continuous(viewport) => {
            RootLayout::continuous(box_tree.layout(layout_context, viewport.size))
        },
        LayoutRoot::Paged(context) => layout_one_page(box_tree, layout_context, context),
    }
}

fn layout_one_page(
    box_tree: &BoxTree,
    layout_context: &LayoutContext,
    context: PageSequenceContext,
) -> RootLayout {
    layout_context.block_pagination.begin(context.definition);
    let fragment_tree =
        box_tree.layout_in_containing_block(layout_context, context.definition.content_rect());
    let page_sequence = context.page_sequence(layout_context.block_pagination.finish());
    RootLayout {
        fragment_tree,
        page_sequence: Some(page_sequence),
    }
}

fn positive_finite(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

fn nonnegative_finite(value: f32) -> bool {
    value.is_finite() && value >= 0.0 && !value.is_sign_negative()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PageDefinition {
        PageDefinition::new(612.0, 792.0, PageMargins::new(72.0, 54.0, 36.0, 18.0))
            .expect("test page geometry should be valid")
    }

    #[test]
    fn page_geometry_resolves_explicit_size_and_margins() {
        let page = page();
        assert_eq!(page.width(), 612.0);
        assert_eq!(page.height(), 792.0);
        assert_eq!(page.margins(), PageMargins::new(72.0, 54.0, 36.0, 18.0));
        assert_eq!(
            page.content_rect(),
            Rect::new(
                Point2D::new(Au::from_px(18), Au::from_px(72)),
                Size2D::new(Au::from_px(540), Au::from_px(684)),
            )
        );
    }

    #[test]
    fn paged_root_creates_exactly_one_page_and_fragmentainer() {
        let page = page();
        let LayoutRoot::Paged(context) = select_layout_root(
            Some(page),
            false,
            Size2D::new(Au::from_px(1), Au::from_px(1)),
        ) else {
            panic!("a configured top-level document should use the paged root")
        };
        let sequence = context.one_page_sequence();
        assert_eq!(sequence.pages.len(), 1);
        let page = sequence.pages[0];
        assert_eq!(page.definition, context.definition);
        assert_eq!(page.fragmentainer.page_index, 0);
        assert_eq!(page.fragmentainer.available_inline_size.to_f32_px(), 540.0);
        assert_eq!(page.fragmentainer.available_block_size.to_f32_px(), 684.0);
        assert_eq!(page.fragmentainer.continuation, None);
        assert!(sequence.warnings.is_empty());
        assert_eq!(
            sequence.debug_snapshot(),
            LayoutDebugPageSequence {
                pages: vec![LayoutDebugPage {
                    index: 0,
                    width: 612.0,
                    height: 792.0,
                    margin_top: 72.0,
                    margin_right: 54.0,
                    margin_bottom: 36.0,
                    margin_left: 18.0,
                    available_inline_size: 540.0,
                    available_block_size: 684.0,
                }],
                continuations: vec![],
            }
        );
    }

    #[test]
    fn one_page_fragmentainer_retains_overflow_for_later_fragmentation() {
        let LayoutRoot::Paged(context) = select_layout_root(
            Some(page()),
            false,
            Size2D::new(Au::from_px(1), Au::from_px(1)),
        ) else {
            panic!("a configured top-level document should use the paged root")
        };
        let sequence = context.one_page_sequence();
        assert_eq!(
            sequence.pages[0].fragmentainer.overflow,
            FragmentainerOverflow::RetainInFragmentTree
        );
    }

    #[test]
    fn continuous_is_the_default_and_iframes_stay_continuous() {
        let viewport = Size2D::new(Au::from_px(800), Au::from_px(600));
        assert!(matches!(
            select_layout_root(None, false, viewport),
            LayoutRoot::Continuous(ContinuousViewport { size }) if size == viewport
        ));
        assert!(matches!(
            select_layout_root(Some(page()), true, viewport),
            LayoutRoot::Continuous(ContinuousViewport { size }) if size == viewport
        ));
        assert!(matches!(
            select_layout_root(Some(page()), false, viewport),
            LayoutRoot::Paged(_)
        ));
        assert!(parallel_layout_allowed(None, false, true));
        assert!(parallel_layout_allowed(Some(page()), true, true));
        assert!(!parallel_layout_allowed(Some(page()), false, true));
        assert!(!parallel_layout_allowed(None, false, false));
    }

    #[test]
    fn block_children_continue_at_monotonic_child_boundaries() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        assert_eq!(
            builder.place_child(
                0,
                Some(10),
                Au::from_px(0),
                Au::from_px(60),
                Au::from_px(60),
            ),
            BlockBoundaryPlacement::CurrentPage
        );
        assert_eq!(
            builder.place_child(
                1,
                Some(20),
                Au::from_px(60),
                Au::from_px(60),
                Au::from_px(60),
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(120),
            }
        );
        assert_eq!(
            builder.place_child(
                2,
                Some(30),
                Au::from_px(180),
                Au::from_px(20),
                Au::from_px(20),
            ),
            BlockBoundaryPlacement::CurrentPage
        );

        let outcome = builder.finish();
        assert_eq!(outcome.page_count, 2);
        assert_eq!(
            outcome.continuations,
            vec![PageContinuation {
                page_index: 0,
                token: FragmentainerContinuation::Block(BlockContinuationToken {
                    next_child_index: 1,
                    next_node: Some(20),
                    resume_page_index: 1,
                }),
            }]
        );
        assert!(outcome.warnings.is_empty());
        assert_eq!(
            PageSequenceContext { definition: page() }
                .page_sequence(outcome)
                .debug_snapshot()
                .continuations,
            vec![LayoutDebugPageContinuation {
                page_index: 0,
                token: LayoutDebugContinuation::Block {
                    next_child_index: 1,
                    next_node: Some(20),
                    resume_page_index: 1,
                },
            }]
        );
    }

    #[test]
    fn paragraph_lines_continue_without_text_loss_or_duplicate_progress() {
        let source = "abcdefghijklmnopqrst";
        let lines = (0..5)
            .map(|line_index| InlineLine {
                block_start: Au::from_px(line_index as i32 * 40),
                block_size: Au::from_px(40),
                source_start: line_index * 4,
                source_end: line_index * 4 + 4,
                resume: InlineResumePoint {
                    inline_item_index: 3,
                    text_offset: line_index * 4,
                    shaping_result_index: line_index,
                },
            })
            .collect::<Vec<_>>();
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });

        let placement = builder.place_inline_child(0, &lines).unwrap();
        assert_eq!(
            placement.line_translations,
            vec![
                Au::from_px(0),
                Au::from_px(0),
                Au::from_px(40),
                Au::from_px(40),
                Au::from_px(80),
            ]
        );
        assert_eq!(placement.added_block_size, Au::from_px(80));
        assert_eq!(placement.line_translations.len(), lines.len());
        assert_eq!(
            lines
                .iter()
                .map(|line| &source[line.source_start..line.source_end])
                .collect::<String>(),
            source
        );

        let outcome = builder.finish();
        assert_eq!(outcome.page_count, 3);
        assert_eq!(
            outcome.continuations,
            vec![
                PageContinuation {
                    page_index: 0,
                    token: FragmentainerContinuation::Inline(InlineContinuationToken {
                        child_index: 0,
                        next_line_index: 2,
                        inline_item_index: 3,
                        text_offset: 8,
                        shaping_result_index: 2,
                        resume_page_index: 1,
                    }),
                },
                PageContinuation {
                    page_index: 1,
                    token: FragmentainerContinuation::Inline(InlineContinuationToken {
                        child_index: 0,
                        next_line_index: 4,
                        inline_item_index: 3,
                        text_offset: 16,
                        shaping_result_index: 4,
                        resume_page_index: 2,
                    }),
                },
            ]
        );
        assert_eq!(
            PageSequenceContext { definition: page() }
                .page_sequence(outcome)
                .debug_snapshot()
                .continuations,
            vec![
                LayoutDebugPageContinuation {
                    page_index: 0,
                    token: LayoutDebugContinuation::Inline {
                        child_index: 0,
                        next_line_index: 2,
                        inline_item_index: 3,
                        text_offset: 8,
                        shaping_result_index: 2,
                        resume_page_index: 1,
                    },
                },
                LayoutDebugPageContinuation {
                    page_index: 1,
                    token: LayoutDebugContinuation::Inline {
                        child_index: 0,
                        next_line_index: 4,
                        inline_item_index: 3,
                        text_offset: 16,
                        shaping_result_index: 4,
                        resume_page_index: 2,
                    },
                },
            ]
        );
    }

    #[test]
    fn outer_unsupported_container_prevents_a_nested_claim() {
        let controller = BlockPaginationController::default();
        controller.begin(page());
        assert!(controller.claim(1, true, false).is_none());
        assert!(controller.claim(3, false, false).is_none());
        assert!(controller.claim(3, true, false).is_none());
        assert_eq!(controller.finish().page_count, 0);
    }

    #[test]
    fn a_single_inline_child_can_claim_the_paged_root() {
        let controller = BlockPaginationController::default();
        controller.begin(page());
        assert!(controller.claim(1, true, true).is_some());
    }

    #[test]
    fn oversized_unbreakable_child_warns_and_next_child_still_advances() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        assert_eq!(
            builder.place_child(
                0,
                Some(10),
                Au::from_px(0),
                Au::from_px(150),
                Au::from_px(150),
            ),
            BlockBoundaryPlacement::CurrentPage
        );
        assert_eq!(
            builder.place_child(
                1,
                Some(20),
                Au::from_px(150),
                Au::from_px(10),
                Au::from_px(10),
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(240),
            }
        );

        let outcome = builder.finish();
        assert_eq!(outcome.page_count, 3);
        assert!(matches!(
            outcome.continuations[0].token,
            FragmentainerContinuation::Block(BlockContinuationToken {
                next_child_index: 1,
                resume_page_index: 2,
                ..
            })
        ));
        assert_eq!(
            outcome.warnings,
            vec![BlockPaginationWarning::OversizedUnbreakable {
                child_index: 0,
                node: Some(10),
                block_size: Au::from_px(150),
                available_block_size: Au::from_px(100),
            }]
        );
    }

    #[test]
    fn invalid_or_consumed_page_geometry_is_rejected() {
        let zero = PageMargins::new(0.0, 0.0, 0.0, 0.0);
        assert_eq!(
            PageDefinition::new(f32::NAN, 792.0, zero),
            Err(PageGeometryError::InvalidPageSize)
        );
        assert_eq!(
            PageDefinition::new(612.0, 792.0, PageMargins::new(-0.0, 0.0, 0.0, 0.0)),
            Err(PageGeometryError::InvalidPageMargin)
        );
        let infinite_margin = PageMargins::new(0.0, f32::INFINITY, 0.0, 0.0);
        assert_eq!(
            PageDefinition::new(612.0, 792.0, infinite_margin),
            Err(PageGeometryError::InvalidPageMargin)
        );
        assert_eq!(
            PageDefinition::new(100.0, 100.0, PageMargins::new(50.0, 0.0, 50.0, 0.0)),
            Err(PageGeometryError::MarginsConsumePage)
        );
        assert_eq!(
            PageDefinition::new(100.0, 100.0, PageMargins::new(0.0, 75.0, 0.0, 25.0)),
            Err(PageGeometryError::MarginsConsumePage)
        );
    }
}
