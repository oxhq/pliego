/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The Pliego-owned paged-document root.
//!
//! Simple normal-flow blocks are continued at child boundaries, and retained paragraph line
//! fragments can continue at line boundaries. Unsupported content can still overflow, but nothing
//! here clips rendered output or reshapes text.

use std::fmt;
use std::ops::Range;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use app_units::Au;
use euclid::default::Size2D as UntypedSize2D;
use euclid::{Point2D, Rect, Size2D};
use layout_api::{
    LayoutDebugContinuation, LayoutDebugPage, LayoutDebugPageAppUnits, LayoutDebugPageContinuation,
    LayoutDebugPageSequence, LayoutDebugPageStyleSource, LayoutDebugPageWarning,
    LayoutDebugTableBreak, LayoutDebugTableCellContinuation, LayoutDebugTableConstraint,
    LayoutDebugTableGroupRepeat, LayoutDebugTableGroupRepeatAppUnits,
    LayoutDebugTableGroupUnsupportedReason,
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

pub(crate) fn has_paged_document_configuration() -> bool {
    PROCESS_PAGE_DEFINITION.get().is_some()
}
static PROCESS_PAGE_CONFIGURATION_STATE: AtomicU8 = AtomicU8::new(PAGE_CONFIGURATION_AVAILABLE);
const PAGE_CONFIGURATION_AVAILABLE: u8 = 0;
const PAGE_CONFIGURATION_RESERVED: u8 = 1;
const PAGE_CONFIGURATION_COMMITTED: u8 = 2;
const TABLE_ROW_RETRY_LIMIT: u8 = 1;

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
        if !nonnegative_finite(margins.top) ||
            !nonnegative_finite(margins.right) ||
            !nonnegative_finite(margins.bottom) ||
            !nonnegative_finite(margins.left)
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
        if definition.available_inline_size() <= Au::from_px(0) ||
            definition.available_block_size() <= Au::from_px(0)
        {
            return Err(PageGeometryError::MarginsConsumePage);
        }
        Ok(definition)
    }

    /// Construct an exact page definition from Servo app units without a floating-point round trip.
    ///
    /// The existing `f32` accessors are not a fixed-point API 2 serialization surface.
    pub fn from_app_units(
        width: i32,
        height: i32,
        margins: [i32; 4],
    ) -> Result<Self, PageGeometryError> {
        if width <= 0 || height <= 0 {
            return Err(PageGeometryError::InvalidPageSize);
        }
        if margins.iter().any(|margin| *margin < 0) {
            return Err(PageGeometryError::InvalidPageMargin);
        }
        let [top, right, bottom, left] = margins;
        if i64::from(left) + i64::from(right) >= i64::from(width) ||
            i64::from(top) + i64::from(bottom) >= i64::from(height)
        {
            return Err(PageGeometryError::MarginsConsumePage);
        }

        Ok(Self {
            size: Size2D::new(Au(width), Au(height)),
            margins: PhysicalSides::new(Au(top), Au(right), Au(bottom), Au(left)),
        })
    }

    pub fn width(&self) -> f32 {
        self.size.width.to_f32_px()
    }

    pub fn height(&self) -> f32 {
        self.size.height.to_f32_px()
    }

    /// Resolve the page's exact app-unit dimensions to the whole-pixel software surface.
    ///
    /// Surfman receives signed dimensions internally, so this rejects any value that cannot be
    /// represented by both the public `u32` surface size and its downstream `i32` boundary.
    pub fn surface_pixel_size(&self) -> Option<UntypedSize2D<u32>> {
        Some(UntypedSize2D::new(
            u32::try_from(self.size.width.ceil_to_px()).ok()?,
            u32::try_from(self.size.height.ceil_to_px()).ok()?,
        ))
    }

    pub fn margins(&self) -> PageMargins {
        PageMargins {
            top: self.margins.top.to_f32_px(),
            right: self.margins.right.to_f32_px(),
            bottom: self.margins.bottom.to_f32_px(),
            left: self.margins.left.to_f32_px(),
        }
    }

    pub(crate) fn debug_app_units(&self) -> LayoutDebugPageAppUnits {
        LayoutDebugPageAppUnits {
            width: self.size.width.0,
            height: self.size.height.0,
            margin_top: self.margins.top.0,
            margin_right: self.margins.right.0,
            margin_bottom: self.margins.bottom.0,
            margin_left: self.margins.left.0,
            available_inline_size: self.available_inline_size().0,
            available_block_size: self.available_block_size().0,
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

/// An exclusive, rollback-safe claim on Pliego's process page definition.
pub struct ProcessPageReservation {
    definition: PageDefinition,
    committed: bool,
}

impl ProcessPageReservation {
    /// Publish the reserved definition after the embedder's fallible local setup succeeds.
    pub fn commit(mut self) -> Result<(), PageDefinition> {
        let result = PROCESS_PAGE_DEFINITION.set(self.definition);
        self.committed = true;
        PROCESS_PAGE_CONFIGURATION_STATE.store(PAGE_CONFIGURATION_COMMITTED, Ordering::Release);
        result
    }
}

impl Drop for ProcessPageReservation {
    fn drop(&mut self) {
        if !self.committed {
            PROCESS_PAGE_CONFIGURATION_STATE.store(PAGE_CONFIGURATION_AVAILABLE, Ordering::Release);
        }
    }
}

/// Reserve the one page used by Pliego's one-render-per-process CLI.
///
/// The reservation rejects concurrent or later sessions before they can mutate process-global
/// embedder state. Dropping it before commit releases the claim so a failed local setup does not
/// poison a later valid session.
pub fn reserve_for_process(
    definition: PageDefinition,
) -> Result<ProcessPageReservation, PageDefinition> {
    PROCESS_PAGE_CONFIGURATION_STATE
        .compare_exchange(
            PAGE_CONFIGURATION_AVAILABLE,
            PAGE_CONFIGURATION_RESERVED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| definition)?;
    Ok(ProcessPageReservation {
        definition,
        committed: false,
    })
}

/// Configure the one page used by Pliego's one-render-per-process CLI immediately.
///
/// Servo embedders that do not call this retain continuous layout. This must be called before Servo
/// starts worker threads; the immutable process value intentionally cannot be replaced mid-session.
pub fn configure_for_process(definition: PageDefinition) -> Result<(), PageDefinition> {
    reserve_for_process(definition)?.commit()
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
            table_breaks: outcome.table_breaks,
            table_cell_continuations: outcome.table_cell_continuations,
            table_group_repeats: outcome.table_group_repeats,
            warnings: outcome.warnings,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlockContinuationToken {
    pub next_child_index: usize,
    pub next_node: Option<u64>,
    pub forced: bool,
    pub break_inside_avoid: bool,
    pub retry_count: u8,
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
pub(crate) struct TableContinuationToken {
    pub child_index: usize,
    pub table_node: Option<u64>,
    pub row_group_index: Option<usize>,
    pub next_row_index: usize,
    pub resume_page_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FragmentainerContinuation {
    Block(BlockContinuationToken),
    Inline(InlineContinuationToken),
    Table(TableContinuationToken),
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
pub(crate) struct TableRow {
    pub row_index: usize,
    pub row_group_index: Option<usize>,
    pub cell_count: usize,
    pub has_rowspan: bool,
    /// Unpaginated block start in the root fragmentainer coordinate space.
    pub block_start: Au,
    pub block_size: Au,
    pub breaks: ChildPageBreaks,
}

fn table_rowspan_ranges(rows: &[TableRow]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut position = 0;
    while position < rows.len() {
        if !rows[position].has_rowspan {
            position += 1;
            continue;
        }
        let start = position;
        while position < rows.len() && rows[position].has_rowspan {
            position += 1;
        }
        ranges.push(start..position);
    }
    ranges
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TableRowGroup {
    pub tag_id: Option<u64>,
    pub row_group_index: usize,
    pub first_row_index: usize,
    pub end_row_index: usize,
    pub block_start: Au,
    pub block_size: Au,
    pub kind: TableRowGroupKind,
    pub breaks: ChildPageBreaks,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TableRowGroupKind {
    Header,
    Body,
    Footer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TableCellFragment {
    pub row_index: usize,
    pub cell_index: usize,
    pub fragment_index: usize,
    /// Unpaginated block start in the root fragmentainer coordinate space.
    pub block_start: Au,
    pub block_size: Au,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct TableChildPlacement {
    pub row_translations: Vec<Au>,
    pub row_block_extensions: Vec<Au>,
    pub cell_fragment_translations: Vec<Au>,
    pub added_block_size: Au,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TableChildPlacementOutcome {
    Placed(TableChildPlacement),
    WholeChild,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlockBoundaryPlacement {
    CurrentPage,
    NewPage { block_origin: Au },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChildPageBreaks {
    pub before: bool,
    pub after: bool,
    pub inside_avoid: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlockPaginationWarning {
    OversizedUnbreakable {
        child_index: usize,
        node: Option<u64>,
        block_size: Au,
        available_block_size: Au,
    },
    OversizedBreakInsideAvoid {
        child_index: usize,
        node: Option<u64>,
        block_size: Au,
        available_block_size: Au,
        retry_count: u8,
    },
    UnsupportedTableCaptionPagination {
        child_index: usize,
        node: Option<u64>,
    },
    UnsupportedTableGroupPagination {
        child_index: usize,
        table_node: Option<u64>,
        reason: TableGroupUnsupportedReason,
    },
    UnsupportedTableRowspanPagination {
        child_index: usize,
        table_node: Option<u64>,
        first_row_index: usize,
        end_row_index: usize,
        block_size: Au,
        available_block_size: Au,
        forced_break_inside: bool,
    },
    OversizedTableCell {
        child_index: usize,
        table_node: Option<u64>,
        row_index: usize,
        cell_index: usize,
        fragment_index: usize,
        block_size: Au,
        available_block_size: Au,
    },
    OversizedTableRowBreakInsideAvoid {
        child_index: usize,
        table_node: Option<u64>,
        row_group_index: Option<usize>,
        row_index: usize,
        block_size: Au,
        available_block_size: Au,
        retry_count: u8,
        retry_limit: u8,
    },
    OversizedTableRowGroupBreakInsideAvoid {
        child_index: usize,
        table_node: Option<u64>,
        row_group_index: usize,
        block_size: Au,
        available_block_size: Au,
        retry_count: u8,
        retry_limit: u8,
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
    pub(crate) fn is_active(&self) -> bool {
        self.state.lock().request.is_some()
    }

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
        supports_single_fragmentable_child: bool,
    ) -> Option<BlockPageBuilder> {
        let mut state = self.state.lock();
        if (child_count < 2 && !supports_single_fragmentable_child) || state.claimed {
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

    /// Normal flow has completed before initial-containing-block fixed children
    /// are laid out. Freeze that page count; fixed content cannot add pages.
    pub(crate) fn fixed_page_layout(&self) -> Option<(usize, Au)> {
        let mut state = self.state.lock();
        let request = state.request?;
        // No flow claim may originate in a fixed subtree, including when the
        // document itself was empty or had only one unfragmentable wrapper.
        state.claimed = true;
        Some((
            state
                .outcome
                .as_ref()
                .map_or(1, |outcome| outcome.page_count.max(1)),
            request.page_stride,
        ))
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TableBreakDecision {
    page_index: usize,
    table_node: Option<u64>,
    row_group_index: Option<usize>,
    next_row_index: usize,
    constraint: LayoutDebugTableConstraint,
    retry_count: u8,
    retry_limit: u8,
    selected: bool,
    resume_page_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TableCellContinuationDecision {
    page_index: usize,
    table_node: Option<u64>,
    row_group_index: Option<usize>,
    row_index: usize,
    cell_index: usize,
    next_fragment_index: Option<usize>,
    resume_page_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TableGroupRepeatDecision {
    page_index: usize,
    table_node: Option<u64>,
    header_tag_id: u64,
    row_group_index: usize,
    source_block_start: Au,
    target_block_start: Au,
    block_size: Au,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TableGroupUnsupportedReason {
    MultipleFooters,
    FooterTooTallAfterHeader,
    Rowspan,
    CollapsedBorders,
    UnsupportedLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableRowsSupport {
    Supported,
    Unsupported,
}

#[derive(Default)]
pub(crate) struct BlockPaginationOutcome {
    page_count: usize,
    continuations: Vec<PageContinuation>,
    table_breaks: Vec<TableBreakDecision>,
    table_cell_continuations: Vec<TableCellContinuationDecision>,
    table_group_repeats: Vec<TableGroupRepeatDecision>,
    warnings: Vec<BlockPaginationWarning>,
}

/// Places one unbreakable child at a time and only advances at legal child boundaries.
pub(crate) struct BlockPageBuilder {
    request: BlockPaginationRequest,
    current_page_index: usize,
    current_page_origin: Au,
    next_child_index: usize,
    page_has_content: bool,
    previous_break_after: bool,
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
            previous_break_after: false,
            outcome: BlockPaginationOutcome {
                page_count: 1,
                ..Default::default()
            },
        }
    }

    pub(crate) fn available_block_size(&self) -> Au {
        self.request.available_block_size
    }

    /// Preserve source child indices without letting an out-of-flow placeholder
    /// consume space, change a pending break, or create an empty page.
    pub(crate) fn skip_out_of_flow_child(&mut self, child_index: usize) {
        assert_eq!(child_index, self.next_child_index);
        self.next_child_index += 1;
    }

    pub(crate) fn place_child(
        &mut self,
        child_index: usize,
        node: Option<u64>,
        current_block_position: Au,
        current_page_contribution: Au,
        fresh_page_contribution: Au,
        breaks: ChildPageBreaks,
    ) -> BlockBoundaryPlacement {
        assert_eq!(child_index, self.next_child_index);
        self.next_child_index += 1;

        if let Some(block_origin) =
            self.forced_break_before(child_index, node, current_block_position, breaks)
        {
            self.warn_if_oversized(
                child_index,
                node,
                fresh_page_contribution,
                breaks.inside_avoid,
            );
            self.page_has_content = true;
            self.include_content_through(block_origin + fresh_page_contribution);
            return BlockBoundaryPlacement::NewPage { block_origin };
        }

        let used = (current_block_position - self.current_page_origin).max(Au::zero());
        let remaining = (self.request.available_block_size - used).max(Au::zero());
        // https://drafts.csswg.org/css-break/#break-within
        if breaks.inside_avoid && fresh_page_contribution > self.request.available_block_size {
            self.warn_if_oversized(child_index, node, fresh_page_contribution, true);
            self.page_has_content = true;
            self.include_content_through(current_block_position + current_page_contribution);
            return BlockBoundaryPlacement::CurrentPage;
        }
        if current_page_contribution <= remaining || !self.page_has_content {
            self.warn_if_oversized(child_index, node, fresh_page_contribution, false);
            self.page_has_content = true;
            self.include_content_through(current_block_position + current_page_contribution);
            return BlockBoundaryPlacement::CurrentPage;
        }

        let block_origin = self.move_child_to_next_page(
            child_index,
            node,
            current_block_position,
            false,
            breaks.inside_avoid,
            u8::from(breaks.inside_avoid),
        );
        self.warn_if_oversized(child_index, node, fresh_page_contribution, false);
        self.page_has_content = true;
        self.include_content_through(block_origin + fresh_page_contribution);
        BlockBoundaryPlacement::NewPage { block_origin }
    }

    /// Place already-shaped lines without rebuilding the inline formatting context. Each output
    /// translation corresponds one-to-one with the input line, so pagination cannot consume or
    /// duplicate a retained line fragment.
    pub(crate) fn place_inline_child(
        &mut self,
        child_index: usize,
        node: Option<u64>,
        current_block_position: Au,
        lines: &[InlineLine],
        breaks: ChildPageBreaks,
    ) -> Option<InlineChildPlacement> {
        assert_eq!(child_index, self.next_child_index);
        if !self.supports_inline_lines(lines) {
            return None;
        }
        self.next_child_index += 1;

        let forced_origin =
            self.forced_break_before(child_index, node, current_block_position, breaks);
        let last_line = lines.last().expect("inline lines are non-empty");
        let fresh_block_size =
            (last_line.block_start + last_line.block_size - current_block_position).max(Au::zero());
        let mut translation = forced_origin.map_or_else(Au::zero, |block_origin| {
            block_origin - current_block_position
        });
        if breaks.inside_avoid && fresh_block_size > self.request.available_block_size {
            self.warn_if_oversized(child_index, node, fresh_block_size, true);
        } else if breaks.inside_avoid && forced_origin.is_none() {
            let used = (current_block_position - self.current_page_origin).max(Au::zero());
            let remaining = (self.request.available_block_size - used).max(Au::zero());
            if fresh_block_size > remaining && self.page_has_content {
                let block_origin = self.move_child_to_next_page(
                    child_index,
                    node,
                    current_block_position,
                    false,
                    true,
                    1,
                );
                translation = block_origin - current_block_position;
            }
        }

        let mut line_translations = Vec::with_capacity(lines.len());
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
            added_block_size: *line_translations
                .last()
                .expect("inline lines are non-empty"),
            line_translations,
        })
    }

    /// Place retained table rows at legal row boundaries without recomputing the table grid.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn place_table_child(
        &mut self,
        child_index: usize,
        table_node: Option<u64>,
        current_block_position: Au,
        fresh_block_size: Au,
        rows: &[TableRow],
        groups: &[TableRowGroup],
        cell_fragments: &[TableCellFragment],
        breaks: ChildPageBreaks,
    ) -> TableChildPlacementOutcome {
        assert_eq!(child_index, self.next_child_index);
        let rowspan_ranges = table_rowspan_ranges(rows);
        let support = self.table_rows_support(rows, groups, cell_fragments, &rowspan_ranges);
        let header = groups
            .iter()
            .find(|group| group.kind == TableRowGroupKind::Header)
            .copied();
        let footer = groups
            .iter()
            .find(|group| group.kind == TableRowGroupKind::Footer)
            .copied();
        if support == TableRowsSupport::Unsupported {
            if header.is_some() || footer.is_some() {
                self.warn_unsupported_table_group_pagination(
                    child_index,
                    table_node,
                    TableGroupUnsupportedReason::UnsupportedLayout,
                );
            }
            return TableChildPlacementOutcome::Unsupported;
        }
        let header_block_size = header.map_or_else(Au::zero, |group| group.block_size);
        let body_page_capacity = self.request.available_block_size - header_block_size;
        if header.is_some_and(|group| {
            group.tag_id.is_none() ||
                group.block_size <= Au::zero() ||
                group.block_size >= self.request.available_block_size
        }) {
            self.warn_unsupported_table_group_pagination(
                child_index,
                table_node,
                TableGroupUnsupportedReason::UnsupportedLayout,
            );
            return TableChildPlacementOutcome::Unsupported;
        }
        if footer.is_some_and(|group| {
            group.block_size <= Au::zero() || group.block_size > body_page_capacity
        }) {
            self.warn_unsupported_table_group_pagination(
                child_index,
                table_node,
                TableGroupUnsupportedReason::FooterTooTallAfterHeader,
            );
            return TableChildPlacementOutcome::Unsupported;
        }
        let body_start = header.map_or(0, |group| group.end_row_index);
        let body_end = footer.map_or(rows.len(), |group| group.first_row_index);
        if body_start > body_end ||
            body_end > rows.len() ||
            header.is_some_and(|group| group.first_row_index != 0) ||
            footer.is_some_and(|group| group.end_row_index != rows.len()) ||
            (header.is_some() &&
                rows[body_start..body_end]
                    .iter()
                    .filter(|row| row.block_size > body_page_capacity)
                    .any(|row| {
                        let mut has_fragment = false;
                        let mut crosses_body_capacity = false;
                        let mut fragments_fit = true;
                        for fragment in cell_fragments
                            .iter()
                            .filter(|fragment| fragment.row_index == row.row_index)
                        {
                            has_fragment = true;
                            crosses_body_capacity |= fragment.block_start + fragment.block_size >
                                row.block_start + body_page_capacity;
                            fragments_fit &= fragment.block_size <= body_page_capacity;
                        }
                        !has_fragment || !crosses_body_capacity || !fragments_fit
                    }))
        {
            self.warn_unsupported_table_group_pagination(
                child_index,
                table_node,
                TableGroupUnsupportedReason::UnsupportedLayout,
            );
            return TableChildPlacementOutcome::Unsupported;
        }
        for range in &rowspan_ranges {
            let first_row = &rows[range.start];
            let last_row = &rows[range.end - 1];
            let block_size = last_row.block_start + last_row.block_size - first_row.block_start;
            let crosses_group_segment = (range.start < body_start && range.end > body_start) ||
                (range.start < body_end && range.end > body_end);
            let available_block_size = if range.end <= body_start {
                self.request.available_block_size
            } else {
                body_page_capacity
            };
            let forced_break_inside = (range.start + 1..range.end).any(|position| {
                rows[position].breaks.before ||
                    rows[position - 1].breaks.after ||
                    groups.iter().any(|group| {
                        (group.first_row_index == position && group.breaks.before) ||
                            (group.end_row_index == position && group.breaks.after)
                    })
            });
            if crosses_group_segment || block_size > available_block_size || forced_break_inside {
                if header.is_some() || footer.is_some() {
                    self.warn_unsupported_table_group_pagination(
                        child_index,
                        table_node,
                        TableGroupUnsupportedReason::Rowspan,
                    );
                } else {
                    self.warn_unsupported_table_rowspan_pagination(
                        child_index,
                        table_node,
                        range,
                        block_size,
                        available_block_size,
                        forced_break_inside,
                    );
                }
                return TableChildPlacementOutcome::Unsupported;
            }
        }
        if breaks.inside_avoid {
            let has_forced_internal_break =
                rows.iter().any(|row| row.breaks.before || row.breaks.after) ||
                    groups
                        .iter()
                        .any(|group| group.breaks.before || group.breaks.after);
            if fresh_block_size <= self.request.available_block_size && !has_forced_internal_break {
                return TableChildPlacementOutcome::WholeChild;
            }
            if fresh_block_size > self.request.available_block_size {
                self.warn_if_oversized(child_index, table_node, fresh_block_size, true);
            }
        }
        self.next_child_index += 1;

        let forced_origin =
            self.forced_break_before(child_index, table_node, current_block_position, breaks);
        let mut translation = forced_origin.map_or_else(Au::zero, |block_origin| {
            block_origin - current_block_position
        });
        let mut row_translations = vec![translation; rows.len()];
        let mut row_block_extensions = vec![Au::zero(); rows.len()];
        let mut cell_fragment_translations = vec![Au::zero(); cell_fragments.len()];
        let mut table_page_has_content = self.page_has_content && forced_origin.is_none();

        if let Some(header) = header {
            let first_row = &rows[header.first_row_index];
            let constraint = if first_row.breaks.before || header.breaks.before {
                LayoutDebugTableConstraint::ForcedBefore
            } else if header.breaks.inside_avoid {
                LayoutDebugTableConstraint::AvoidGroup
            } else if first_row.breaks.inside_avoid {
                LayoutDebugTableConstraint::AvoidRow
            } else {
                LayoutDebugTableConstraint::Auto
            };
            let mut block_start = header.block_start + translation;
            let page_end = self.current_page_origin + self.request.available_block_size;
            let considered_page_index = self.current_page_index;
            let selected = table_page_has_content &&
                (constraint == LayoutDebugTableConstraint::ForcedBefore ||
                    block_start + header.block_size > page_end);
            let retry_count = u8::from(
                selected &&
                    matches!(
                        constraint,
                        LayoutDebugTableConstraint::AvoidRow |
                            LayoutDebugTableConstraint::AvoidGroup
                    ),
            );
            let mut resume_page_index = None;
            if selected {
                let previous_page_index = self.current_page_index;
                let mut next_page_index = self.current_page_index + 1;
                let mut next_page_origin = self.current_page_origin + self.request.page_stride;
                while next_page_origin < block_start {
                    next_page_index += 1;
                    next_page_origin += self.request.page_stride;
                }
                translation += next_page_origin - block_start;
                block_start = next_page_origin;
                self.current_page_index = next_page_index;
                self.current_page_origin = next_page_origin;
                resume_page_index = Some(next_page_index);
                self.outcome.continuations.push(PageContinuation {
                    page_index: previous_page_index,
                    token: FragmentainerContinuation::Table(TableContinuationToken {
                        child_index,
                        table_node,
                        row_group_index: Some(header.row_group_index),
                        next_row_index: header.first_row_index,
                        resume_page_index: next_page_index,
                    }),
                });
                for row_translation in
                    &mut row_translations[header.first_row_index..header.end_row_index]
                {
                    *row_translation = translation;
                }
            }
            if constraint != LayoutDebugTableConstraint::Auto {
                self.outcome.table_breaks.push(TableBreakDecision {
                    page_index: considered_page_index,
                    table_node,
                    row_group_index: Some(header.row_group_index),
                    next_row_index: header.first_row_index,
                    constraint,
                    retry_count,
                    retry_limit: TABLE_ROW_RETRY_LIMIT,
                    selected,
                    resume_page_index,
                });
            }
            self.page_has_content = true;
            table_page_has_content = true;
            self.include_content_through(block_start + header.block_size);
        }
        let header_source_block_start = header.map(|group| group.block_start + translation);

        let mut group_position = 0;
        let mut rowspan_position = 0;
        for position in body_start..body_end {
            let row = &rows[position];
            while rowspan_ranges
                .get(rowspan_position)
                .is_some_and(|range| range.end <= position)
            {
                rowspan_position += 1;
            }
            let rowspan_range = rowspan_ranges
                .get(rowspan_position)
                .filter(|range| range.start <= position && position < range.end);
            let rowspan_block_size =
                rowspan_range
                    .filter(|range| range.start == position)
                    .map(|range| {
                        let last_row = &rows[range.end - 1];
                        last_row.block_start + last_row.block_size - row.block_start
                    });
            let mut group_break_after = false;
            while groups
                .get(group_position)
                .is_some_and(|group| group.end_row_index <= position)
            {
                group_break_after = groups[group_position].end_row_index == position &&
                    groups[group_position].breaks.after;
                group_position += 1;
            }
            let starting_group = groups
                .get(group_position)
                .filter(|group| group.first_row_index == position);
            let avoided_group = starting_group.filter(|group| group.breaks.inside_avoid);
            let avoided_group_block_size = avoided_group.map(|group| {
                let last_row = &rows[group.end_row_index - 1];
                last_row.block_start + last_row.block_size - row.block_start
            });
            let mut block_start = row.block_start + translation;
            let page_end = self.current_page_origin + self.request.available_block_size;
            let considered_page_index = self.current_page_index;
            let oversized = row.block_size > body_page_capacity;
            let forced_before =
                row.breaks.before || starting_group.is_some_and(|group| group.breaks.before);
            let forced_after =
                position > 0 && (rows[position - 1].breaks.after || group_break_after);
            let constraint = if forced_before {
                LayoutDebugTableConstraint::ForcedBefore
            } else if forced_after {
                LayoutDebugTableConstraint::ForcedAfter
            } else if avoided_group.is_some() {
                LayoutDebugTableConstraint::AvoidGroup
            } else if row.breaks.inside_avoid {
                LayoutDebugTableConstraint::AvoidRow
            } else {
                LayoutDebugTableConstraint::Auto
            };
            let constraint_selected = table_page_has_content &&
                rowspan_range.is_none_or(|range| range.start == position) &&
                match constraint {
                    LayoutDebugTableConstraint::ForcedBefore |
                    LayoutDebugTableConstraint::ForcedAfter => true,
                    LayoutDebugTableConstraint::AvoidGroup => {
                        let group_block_size =
                            avoided_group_block_size.expect("avoid-group starts at this row");
                        group_block_size <= body_page_capacity &&
                            block_start + group_block_size > page_end
                    },
                    LayoutDebugTableConstraint::Auto | LayoutDebugTableConstraint::AvoidRow => {
                        !oversized && block_start + row.block_size > page_end
                    },
                };
            let rowspan_selected = table_page_has_content &&
                !constraint_selected &&
                rowspan_block_size.is_some_and(|block_size| block_start + block_size > page_end);
            let selected = constraint_selected || rowspan_selected;
            let retry_count = u8::from(
                constraint_selected &&
                    matches!(
                        constraint,
                        LayoutDebugTableConstraint::AvoidRow |
                            LayoutDebugTableConstraint::AvoidGroup
                    ),
            );
            let mut resume_page_index = None;

            if selected {
                let previous_page_index = self.current_page_index;
                let mut next_page_index = self.current_page_index + 1;
                let mut next_page_origin = self.current_page_origin + self.request.page_stride;
                while next_page_origin + header_block_size < block_start {
                    next_page_index += 1;
                    next_page_origin += self.request.page_stride;
                }
                translation += next_page_origin + header_block_size - block_start;
                block_start = next_page_origin + header_block_size;
                self.current_page_index = next_page_index;
                self.current_page_origin = next_page_origin;
                resume_page_index = Some(next_page_index);
                self.outcome.continuations.push(PageContinuation {
                    page_index: previous_page_index,
                    token: FragmentainerContinuation::Table(TableContinuationToken {
                        child_index,
                        table_node,
                        row_group_index: row.row_group_index,
                        next_row_index: row.row_index,
                        resume_page_index: next_page_index,
                    }),
                });
                self.push_table_header_repeat(
                    table_node,
                    header,
                    header_source_block_start,
                    next_page_index,
                    next_page_origin,
                );
            }

            if oversized && row.breaks.inside_avoid {
                self.warn_oversized_table_row_avoid(
                    child_index,
                    table_node,
                    *row,
                    body_page_capacity,
                );
            }
            if let (Some(group), Some(group_block_size)) = (avoided_group, avoided_group_block_size) &&
                group_block_size > body_page_capacity
            {
                self.warn_oversized_table_row_group_avoid(
                    child_index,
                    table_node,
                    *group,
                    group_block_size,
                    body_page_capacity,
                );
            }
            let oversized_fragment = oversized
                .then(|| {
                    cell_fragments
                        .iter()
                        .find(|fragment| {
                            fragment.row_index == row.row_index &&
                                fragment.block_size > body_page_capacity
                        })
                        .copied()
                })
                .flatten();
            if let Some(fragment) = oversized_fragment {
                self.warn_oversized_table_cell(child_index, table_node, fragment);
            }

            row_translations[position] = translation;
            let mut row_block_extension = Au::zero();
            if oversized && oversized_fragment.is_none() {
                let start_page_index = self.current_page_index;
                let start_page_origin = self.current_page_origin;
                let mut assigned_pages = vec![None; cell_fragments.len()];
                let mut last_page_index = start_page_index;

                for cell_index in 0..row.cell_count {
                    let mut cell_page_index = start_page_index;
                    let mut cell_page_origin = start_page_origin;
                    let mut cell_translation = Au::zero();
                    for (index, fragment) in
                        cell_fragments.iter().enumerate().filter(|(_, item)| {
                            item.row_index == row.row_index && item.cell_index == cell_index
                        })
                    {
                        let mut fragment_start =
                            fragment.block_start + translation + cell_translation;
                        while fragment_start >= cell_page_origin + self.request.page_stride {
                            cell_page_index += 1;
                            cell_page_origin += self.request.page_stride;
                            cell_translation += header_block_size;
                            fragment_start += header_block_size;
                        }
                        if fragment_start + fragment.block_size >
                            cell_page_origin + self.request.available_block_size
                        {
                            cell_page_index += 1;
                            cell_page_origin += self.request.page_stride;
                            cell_translation +=
                                cell_page_origin + header_block_size - fragment_start;
                            fragment_start = cell_page_origin + header_block_size;
                        }
                        debug_assert!(
                            fragment_start + fragment.block_size <=
                                cell_page_origin + self.request.available_block_size
                        );
                        cell_fragment_translations[index] = cell_translation;
                        assigned_pages[index] = Some(cell_page_index);
                        row_block_extension = row_block_extension.max(cell_translation);
                        last_page_index = last_page_index.max(cell_page_index);
                    }
                }

                if last_page_index == start_page_index {
                    let last = cell_fragments
                        .iter()
                        .rev()
                        .find(|fragment| fragment.row_index == row.row_index)
                        .expect("validated oversized rows retain cell fragments");
                    self.warn_oversized_table_cell(
                        child_index,
                        table_node,
                        TableCellFragment {
                            row_index: row.row_index,
                            cell_index: last.cell_index,
                            fragment_index: last.fragment_index + 1,
                            block_start: row.block_start,
                            block_size: row.block_size,
                        },
                    );
                }

                for resume_page_index in start_page_index + 1..=last_page_index {
                    let target_block_start = start_page_origin +
                        self.request.page_stride *
                            i32::try_from(resume_page_index - start_page_index)
                                .expect("page counts fit app units");
                    self.outcome.continuations.push(PageContinuation {
                        page_index: resume_page_index - 1,
                        token: FragmentainerContinuation::Table(TableContinuationToken {
                            child_index,
                            table_node,
                            row_group_index: row.row_group_index,
                            next_row_index: row.row_index,
                            resume_page_index,
                        }),
                    });
                    self.push_table_header_repeat(
                        table_node,
                        header,
                        header_source_block_start,
                        resume_page_index,
                        target_block_start,
                    );
                    for cell_index in 0..row.cell_count {
                        let next_fragment_index =
                            cell_fragments
                                .iter()
                                .enumerate()
                                .find_map(|(index, fragment)| {
                                    (fragment.row_index == row.row_index &&
                                        fragment.cell_index == cell_index &&
                                        assigned_pages[index].is_some_and(|page_index| {
                                            page_index >= resume_page_index
                                        }))
                                    .then_some(fragment.fragment_index)
                                });
                        self.outcome
                            .table_cell_continuations
                            .push(TableCellContinuationDecision {
                                page_index: resume_page_index - 1,
                                table_node,
                                row_group_index: row.row_group_index,
                                row_index: row.row_index,
                                cell_index,
                                next_fragment_index,
                                resume_page_index,
                            });
                    }
                }

                self.current_page_index = start_page_index;
                self.current_page_origin = start_page_origin;
                while self.current_page_index < last_page_index {
                    self.current_page_index += 1;
                    self.current_page_origin += self.request.page_stride;
                }
                translation += row_block_extension;
                block_start = row.block_start + translation;
            }
            if oversized &&
                (oversized_fragment.is_some() ||
                    self.current_page_index == considered_page_index)
            {
                let row_end = block_start + row.block_size;
                while self.current_page_origin + self.request.page_stride < row_end {
                    self.current_page_index += 1;
                    self.current_page_origin += self.request.page_stride;
                }
            }
            if position > 0 || constraint != LayoutDebugTableConstraint::Auto || rowspan_selected {
                self.outcome.table_breaks.push(TableBreakDecision {
                    page_index: considered_page_index,
                    table_node,
                    row_group_index: row.row_group_index,
                    next_row_index: row.row_index,
                    constraint,
                    retry_count,
                    retry_limit: TABLE_ROW_RETRY_LIMIT,
                    selected,
                    resume_page_index,
                });
            }

            row_block_extensions[position] = row_block_extension;
            self.page_has_content = true;
            table_page_has_content = true;
            self.include_content_through(block_start + row.block_size);
        }

        if let Some(footer) = footer {
            let first_row = &rows[footer.first_row_index];
            let group_break_after = groups
                .iter()
                .any(|group| group.end_row_index == footer.first_row_index && group.breaks.after);
            let forced_before = first_row.breaks.before || footer.breaks.before;
            let forced_after = footer.first_row_index > 0 &&
                (rows[footer.first_row_index - 1].breaks.after || group_break_after);
            let constraint = if forced_before {
                LayoutDebugTableConstraint::ForcedBefore
            } else if forced_after {
                LayoutDebugTableConstraint::ForcedAfter
            } else if footer.breaks.inside_avoid {
                LayoutDebugTableConstraint::AvoidGroup
            } else if first_row.breaks.inside_avoid {
                LayoutDebugTableConstraint::AvoidRow
            } else {
                LayoutDebugTableConstraint::Auto
            };
            let mut block_start = footer.block_start + translation;
            let page_end = self.current_page_origin + self.request.available_block_size;
            let considered_page_index = self.current_page_index;
            let selected = table_page_has_content &&
                (matches!(
                    constraint,
                    LayoutDebugTableConstraint::ForcedBefore |
                        LayoutDebugTableConstraint::ForcedAfter
                ) || block_start + footer.block_size > page_end);
            let retry_count = u8::from(
                selected &&
                    matches!(
                        constraint,
                        LayoutDebugTableConstraint::AvoidRow |
                            LayoutDebugTableConstraint::AvoidGroup
                    ),
            );
            let mut resume_page_index = None;
            if selected {
                let previous_page_index = self.current_page_index;
                let mut next_page_index = self.current_page_index + 1;
                let mut next_page_origin = self.current_page_origin + self.request.page_stride;
                while next_page_origin + header_block_size < block_start {
                    next_page_index += 1;
                    next_page_origin += self.request.page_stride;
                }
                translation += next_page_origin + header_block_size - block_start;
                block_start = next_page_origin + header_block_size;
                self.current_page_index = next_page_index;
                self.current_page_origin = next_page_origin;
                resume_page_index = Some(next_page_index);
                self.outcome.continuations.push(PageContinuation {
                    page_index: previous_page_index,
                    token: FragmentainerContinuation::Table(TableContinuationToken {
                        child_index,
                        table_node,
                        row_group_index: Some(footer.row_group_index),
                        next_row_index: footer.first_row_index,
                        resume_page_index: next_page_index,
                    }),
                });
                self.push_table_header_repeat(
                    table_node,
                    header,
                    header_source_block_start,
                    next_page_index,
                    next_page_origin,
                );
            }
            self.outcome.table_breaks.push(TableBreakDecision {
                page_index: considered_page_index,
                table_node,
                row_group_index: Some(footer.row_group_index),
                next_row_index: footer.first_row_index,
                constraint,
                retry_count,
                retry_limit: TABLE_ROW_RETRY_LIMIT,
                selected,
                resume_page_index,
            });
            for row_translation in
                &mut row_translations[footer.first_row_index..footer.end_row_index]
            {
                *row_translation = translation;
            }
            self.page_has_content = true;
            self.include_content_through(block_start + footer.block_size);
        }

        self.previous_break_after |= rows.last().is_some_and(|row| row.breaks.after) ||
            groups
                .last()
                .is_some_and(|group| group.end_row_index == rows.len() && group.breaks.after);

        TableChildPlacementOutcome::Placed(TableChildPlacement {
            row_translations,
            row_block_extensions,
            cell_fragment_translations,
            added_block_size: translation,
        })
    }

    fn push_table_header_repeat(
        &mut self,
        table_node: Option<u64>,
        header: Option<TableRowGroup>,
        source_block_start: Option<Au>,
        page_index: usize,
        target_block_start: Au,
    ) {
        let (Some(header), Some(source_block_start), Some(header_tag_id)) = (
            header,
            source_block_start,
            header.and_then(|group| group.tag_id),
        ) else {
            return;
        };
        self.outcome
            .table_group_repeats
            .push(TableGroupRepeatDecision {
                page_index,
                table_node,
                header_tag_id,
                row_group_index: header.row_group_index,
                source_block_start,
                target_block_start,
                block_size: header.block_size,
            });
    }

    fn forced_break_before(
        &mut self,
        child_index: usize,
        node: Option<u64>,
        current_block_position: Au,
        breaks: ChildPageBreaks,
    ) -> Option<Au> {
        // https://drafts.csswg.org/css-break/#forced-breaks
        let forced = self.previous_break_after || breaks.before;
        self.previous_break_after = breaks.after;
        if !forced || !self.page_has_content {
            return None;
        }

        Some(self.move_child_to_next_page(
            child_index,
            node,
            current_block_position,
            true,
            false,
            0,
        ))
    }

    fn move_child_to_next_page(
        &mut self,
        child_index: usize,
        node: Option<u64>,
        current_block_position: Au,
        forced: bool,
        break_inside_avoid: bool,
        retry_count: u8,
    ) -> Au {
        let previous_page_index = self.current_page_index;
        loop {
            self.current_page_index += 1;
            self.current_page_origin += self.request.page_stride;
            if self.current_page_origin >= current_block_position {
                break;
            }
        }
        self.outcome.continuations.push(PageContinuation {
            page_index: previous_page_index,
            token: FragmentainerContinuation::Block(BlockContinuationToken {
                next_child_index: child_index,
                next_node: node,
                forced,
                break_inside_avoid,
                retry_count,
                resume_page_index: self.current_page_index,
            }),
        });
        self.current_page_origin
    }

    fn supports_inline_lines(&self, lines: &[InlineLine]) -> bool {
        if lines.len() < 2 {
            return false;
        }
        let mut previous: Option<&InlineLine> = None;
        for line in lines {
            if line.block_size <= Au::zero() ||
                line.block_size > self.request.available_block_size ||
                line.source_start >= line.source_end ||
                line.resume.text_offset != line.source_start
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
                if line.block_start < previous.block_start ||
                    line.source_start < previous.source_end ||
                    progress <= previous_progress
                {
                    return false;
                }
            }
            previous = Some(line);
        }
        true
    }

    fn table_rows_support(
        &self,
        rows: &[TableRow],
        row_groups: &[TableRowGroup],
        cell_fragments: &[TableCellFragment],
        rowspan_ranges: &[Range<usize>],
    ) -> TableRowsSupport {
        if rows.is_empty() ||
            rows.iter().enumerate().any(|(position, row)| {
                row.row_index != position ||
                    row.cell_count == 0 ||
                    row.block_size <= Au::zero() ||
                    (position > 0 &&
                        row.block_start <
                            rows[position - 1].block_start + rows[position - 1].block_size)
            })
        {
            return TableRowsSupport::Unsupported;
        }

        if row_groups.iter().enumerate().any(|(position, group)| {
            group.first_row_index >= group.end_row_index ||
                group.end_row_index > rows.len() ||
                (position > 0 && group.first_row_index < row_groups[position - 1].end_row_index)
        }) {
            return TableRowsSupport::Unsupported;
        }
        let mut group_position = 0;
        for (position, row) in rows.iter().enumerate() {
            while row_groups
                .get(group_position)
                .is_some_and(|group| group.end_row_index <= position)
            {
                group_position += 1;
            }
            let retained_group = row_groups.get(group_position).filter(|group| {
                group.first_row_index <= position && position < group.end_row_index
            });
            if retained_group.map(|group| group.row_group_index) != row.row_group_index {
                return TableRowsSupport::Unsupported;
            }
        }

        let mut previous: Option<&TableCellFragment> = None;
        for fragment in cell_fragments {
            let Some(row) = rows.get(fragment.row_index) else {
                return TableRowsSupport::Unsupported;
            };
            let row_block_end = rowspan_ranges
                .iter()
                .find(|range| range.contains(&fragment.row_index))
                .map_or(row.block_start + row.block_size, |range| {
                    let last_row = &rows[range.end - 1];
                    last_row.block_start + last_row.block_size
                });
            if fragment.cell_index >= row.cell_count ||
                fragment.block_size <= Au::zero() ||
                fragment.block_start < row.block_start ||
                fragment.block_start + fragment.block_size > row_block_end
            {
                return TableRowsSupport::Unsupported;
            }
            if let Some(previous) = previous {
                let previous_key = (
                    previous.row_index,
                    previous.cell_index,
                    previous.fragment_index,
                );
                let key = (
                    fragment.row_index,
                    fragment.cell_index,
                    fragment.fragment_index,
                );
                if key <= previous_key ||
                    (fragment.row_index == previous.row_index &&
                        fragment.cell_index == previous.cell_index &&
                        (fragment.fragment_index != previous.fragment_index + 1 ||
                            fragment.block_start <
                                previous.block_start + previous.block_size))
                {
                    return TableRowsSupport::Unsupported;
                }
            } else if fragment.fragment_index != 0 {
                return TableRowsSupport::Unsupported;
            }
            if previous.is_none_or(|previous| {
                previous.row_index != fragment.row_index ||
                    previous.cell_index != fragment.cell_index
            }) && fragment.fragment_index != 0
            {
                return TableRowsSupport::Unsupported;
            }
            previous = Some(fragment);
        }

        for row in rows
            .iter()
            .filter(|row| row.block_size > self.request.available_block_size)
        {
            if !cell_fragments
                .iter()
                .any(|fragment| fragment.row_index == row.row_index)
            {
                return TableRowsSupport::Unsupported;
            }
        }
        TableRowsSupport::Supported
    }

    fn warn_oversized_table_cell(
        &mut self,
        child_index: usize,
        table_node: Option<u64>,
        fragment: TableCellFragment,
    ) {
        warn!(
            "paged layout cannot split table child {child_index} cell {}:{} fragment {} (node \
             {table_node:?}, {:.2}px > {:.2}px)",
            fragment.row_index,
            fragment.cell_index,
            fragment.fragment_index,
            fragment.block_size.to_f32_px(),
            self.request.available_block_size.to_f32_px(),
        );
        self.outcome
            .warnings
            .push(BlockPaginationWarning::OversizedTableCell {
                child_index,
                table_node,
                row_index: fragment.row_index,
                cell_index: fragment.cell_index,
                fragment_index: fragment.fragment_index,
                block_size: fragment.block_size,
                available_block_size: self.request.available_block_size,
            });
    }

    fn warn_oversized_table_row_avoid(
        &mut self,
        child_index: usize,
        table_node: Option<u64>,
        row: TableRow,
        available_block_size: Au,
    ) {
        warn!(
            "paged layout fragmented table child {child_index} row {} despite break-inside: avoid \
             (node {table_node:?}, {:.2}px > {:.2}px); split or shorten the row",
            row.row_index,
            row.block_size.to_f32_px(),
            available_block_size.to_f32_px(),
        );
        self.outcome
            .warnings
            .push(BlockPaginationWarning::OversizedTableRowBreakInsideAvoid {
                child_index,
                table_node,
                row_group_index: row.row_group_index,
                row_index: row.row_index,
                block_size: row.block_size,
                available_block_size,
                retry_count: 0,
                retry_limit: TABLE_ROW_RETRY_LIMIT,
            });
    }

    fn warn_oversized_table_row_group_avoid(
        &mut self,
        child_index: usize,
        table_node: Option<u64>,
        group: TableRowGroup,
        block_size: Au,
        available_block_size: Au,
    ) {
        warn!(
            "paged layout fragmented table child {child_index} row group {} despite break-inside: \
             avoid (node {table_node:?}, {:.2}px > {:.2}px); split or shorten the row group",
            group.row_group_index,
            block_size.to_f32_px(),
            available_block_size.to_f32_px(),
        );
        self.outcome.warnings.push(
            BlockPaginationWarning::OversizedTableRowGroupBreakInsideAvoid {
                child_index,
                table_node,
                row_group_index: group.row_group_index,
                block_size,
                available_block_size,
                retry_count: 0,
                retry_limit: TABLE_ROW_RETRY_LIMIT,
            },
        );
    }

    fn warn_if_oversized(
        &mut self,
        child_index: usize,
        node: Option<u64>,
        block_size: Au,
        break_inside_avoid: bool,
    ) {
        if block_size <= self.request.available_block_size {
            return;
        }
        if break_inside_avoid {
            warn!(
                "paged layout fragmented oversized break-inside: avoid child {child_index} (node \
                 {node:?}, {:.2}px > {:.2}px)",
                block_size.to_f32_px(),
                self.request.available_block_size.to_f32_px(),
            );
            self.outcome
                .warnings
                .push(BlockPaginationWarning::OversizedBreakInsideAvoid {
                    child_index,
                    node,
                    block_size,
                    available_block_size: self.request.available_block_size,
                    retry_count: 0,
                });
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

    pub(crate) fn warn_unsupported_table_caption_pagination(
        &mut self,
        child_index: usize,
        node: Option<u64>,
    ) {
        warn!(
            "paged layout retained table captions without row pagination for child {child_index} \
             (node {node:?})"
        );
        self.outcome
            .warnings
            .push(BlockPaginationWarning::UnsupportedTableCaptionPagination { child_index, node });
    }

    pub(crate) fn warn_unsupported_table_group_pagination(
        &mut self,
        child_index: usize,
        table_node: Option<u64>,
        reason: TableGroupUnsupportedReason,
    ) {
        warn!(
            "paged layout retained unsupported table groups for child {child_index} (node \
             {table_node:?}, reason {reason:?})"
        );
        self.outcome
            .warnings
            .push(BlockPaginationWarning::UnsupportedTableGroupPagination {
                child_index,
                table_node,
                reason,
            });
    }

    fn warn_unsupported_table_rowspan_pagination(
        &mut self,
        child_index: usize,
        table_node: Option<u64>,
        range: &Range<usize>,
        block_size: Au,
        available_block_size: Au,
        forced_break_inside: bool,
    ) {
        warn!(
            "paged layout cannot retain table child {child_index} rowspan rows {}..{} on one page \
             (node {table_node:?}, {:.2}px / {:.2}px, forced break inside: \
             {forced_break_inside}); remove the internal forced break or shorten the rowspan",
            range.start,
            range.end,
            block_size.to_f32_px(),
            available_block_size.to_f32_px(),
        );
        self.outcome
            .warnings
            .push(BlockPaginationWarning::UnsupportedTableRowspanPagination {
                child_index,
                table_node,
                first_row_index: range.start,
                end_row_index: range.end,
                block_size,
                available_block_size,
                forced_break_inside,
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
    table_breaks: Vec<TableBreakDecision>,
    table_cell_continuations: Vec<TableCellContinuationDecision>,
    table_group_repeats: Vec<TableGroupRepeatDecision>,
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
                        style_source: LayoutDebugPageStyleSource::RequestDefaults,
                        app_units: page.definition.debug_app_units(),
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
                                        forced: token.forced,
                                        break_inside_avoid: token.break_inside_avoid,
                                        retry_count: token.retry_count,
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
                                FragmentainerContinuation::Table(token) => {
                                    LayoutDebugContinuation::Table {
                                        child_index: token.child_index,
                                        table_node: token.table_node,
                                        row_group_index: token.row_group_index,
                                        next_row_index: token.next_row_index,
                                        resume_page_index: token.resume_page_index,
                                    }
                                },
                            },
                        })
                })
                .collect(),
            table_breaks: self
                .table_breaks
                .iter()
                .map(|decision| LayoutDebugTableBreak {
                    page_index: decision.page_index,
                    table_node: decision.table_node,
                    row_group_index: decision.row_group_index,
                    next_row_index: decision.next_row_index,
                    constraint: decision.constraint,
                    retry_count: decision.retry_count,
                    retry_limit: decision.retry_limit,
                    selected: decision.selected,
                    resume_page_index: decision.resume_page_index,
                })
                .collect(),
            table_cell_continuations: self
                .table_cell_continuations
                .iter()
                .map(|continuation| LayoutDebugTableCellContinuation {
                    page_index: continuation.page_index,
                    table_node: continuation.table_node,
                    row_group_index: continuation.row_group_index,
                    row_index: continuation.row_index,
                    cell_index: continuation.cell_index,
                    next_fragment_index: continuation.next_fragment_index,
                    resume_page_index: continuation.resume_page_index,
                })
                .collect(),
            table_group_repeats: self
                .table_group_repeats
                .iter()
                .map(|repeat| LayoutDebugTableGroupRepeat {
                    page_index: repeat.page_index,
                    table_node: repeat.table_node,
                    header_tag_id: repeat.header_tag_id,
                    row_group_index: repeat.row_group_index,
                    source_block_start: repeat.source_block_start.to_f32_px(),
                    target_block_start: repeat.target_block_start.to_f32_px(),
                    block_size: repeat.block_size.to_f32_px(),
                    app_units: Some(LayoutDebugTableGroupRepeatAppUnits {
                        source_block_start: repeat.source_block_start.0,
                        target_block_start: repeat.target_block_start.0,
                        block_size: repeat.block_size.0,
                    }),
                })
                .collect(),
            warnings: self
                .warnings
                .iter()
                .map(|warning| match *warning {
                    BlockPaginationWarning::OversizedUnbreakable {
                        child_index,
                        node,
                        block_size,
                        available_block_size,
                    } => LayoutDebugPageWarning::OversizedUnbreakable {
                        child_index,
                        node,
                        block_size: block_size.to_f32_px(),
                        available_block_size: available_block_size.to_f32_px(),
                    },
                    BlockPaginationWarning::OversizedBreakInsideAvoid {
                        child_index,
                        node,
                        block_size,
                        available_block_size,
                        retry_count,
                    } => LayoutDebugPageWarning::OversizedBreakInsideAvoid {
                        child_index,
                        node,
                        block_size: block_size.to_f32_px(),
                        available_block_size: available_block_size.to_f32_px(),
                        retry_count,
                    },
                    BlockPaginationWarning::UnsupportedTableCaptionPagination {
                        child_index,
                        node,
                    } => LayoutDebugPageWarning::UnsupportedTableCaptionPagination {
                        child_index,
                        node,
                    },
                    BlockPaginationWarning::UnsupportedTableGroupPagination {
                        child_index,
                        table_node,
                        reason,
                    } => LayoutDebugPageWarning::UnsupportedTableGroupPagination {
                        child_index,
                        table_node,
                        reason: match reason {
                            TableGroupUnsupportedReason::MultipleFooters => {
                                LayoutDebugTableGroupUnsupportedReason::MultipleFooters
                            },
                            TableGroupUnsupportedReason::FooterTooTallAfterHeader => {
                                LayoutDebugTableGroupUnsupportedReason::FooterTooTallAfterHeader
                            },
                            TableGroupUnsupportedReason::Rowspan => {
                                LayoutDebugTableGroupUnsupportedReason::Rowspan
                            },
                            TableGroupUnsupportedReason::CollapsedBorders => {
                                LayoutDebugTableGroupUnsupportedReason::CollapsedBorders
                            },
                            TableGroupUnsupportedReason::UnsupportedLayout => {
                                LayoutDebugTableGroupUnsupportedReason::UnsupportedLayout
                            },
                        },
                    },
                    BlockPaginationWarning::UnsupportedTableRowspanPagination {
                        child_index,
                        table_node,
                        first_row_index,
                        end_row_index,
                        block_size,
                        available_block_size,
                        forced_break_inside,
                    } => LayoutDebugPageWarning::UnsupportedTableRowspanPagination {
                        child_index,
                        table_node,
                        first_row_index,
                        end_row_index,
                        block_size: block_size.to_f32_px(),
                        available_block_size: available_block_size.to_f32_px(),
                        forced_break_inside,
                    },
                    BlockPaginationWarning::OversizedTableCell {
                        child_index,
                        table_node,
                        row_index,
                        cell_index,
                        fragment_index,
                        block_size,
                        available_block_size,
                    } => LayoutDebugPageWarning::OversizedTableCell {
                        child_index,
                        table_node,
                        row_index,
                        cell_index,
                        fragment_index,
                        block_size: block_size.to_f32_px(),
                        available_block_size: available_block_size.to_f32_px(),
                    },
                    BlockPaginationWarning::OversizedTableRowBreakInsideAvoid {
                        child_index,
                        table_node,
                        row_group_index,
                        row_index,
                        block_size,
                        available_block_size,
                        retry_count,
                        retry_limit,
                    } => LayoutDebugPageWarning::OversizedTableRowBreakInsideAvoid {
                        child_index,
                        table_node,
                        row_group_index,
                        row_index,
                        block_size: block_size.to_f32_px(),
                        available_block_size: available_block_size.to_f32_px(),
                        retry_count,
                        retry_limit,
                    },
                    BlockPaginationWarning::OversizedTableRowGroupBreakInsideAvoid {
                        child_index,
                        table_node,
                        row_group_index,
                        block_size,
                        available_block_size,
                        retry_count,
                        retry_limit,
                    } => LayoutDebugPageWarning::OversizedTableRowGroupBreakInsideAvoid {
                        child_index,
                        table_node,
                        row_group_index,
                        block_size: block_size.to_f32_px(),
                        available_block_size: available_block_size.to_f32_px(),
                        retry_count,
                        retry_limit,
                    },
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
    let mut fragment_tree =
        box_tree.layout_in_containing_block(layout_context, context.definition.content_rect());
    fragment_tree.is_paged = true;
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
    fn exact_app_unit_page_construction_does_not_narrow_large_contract_values() {
        let page = PageDefinition::from_app_units(i32::MAX, i32::MAX - 1, [1, 2, 3, i32::MAX - 3])
            .expect("valid app-unit geometry should not pass through f32");
        assert_eq!(page.size.width, Au(i32::MAX));
        assert_eq!(page.size.height, Au(i32::MAX - 1));
        assert_eq!(page.margins.top, Au(1));
        assert_eq!(page.margins.right, Au(2));
        assert_eq!(page.margins.bottom, Au(3));
        assert_eq!(page.margins.left, Au(i32::MAX - 3));

        assert_eq!(
            PageDefinition::from_app_units(100, 100, [0, 50, 0, 50]),
            Err(PageGeometryError::MarginsConsumePage)
        );
    }

    #[test]
    fn surface_pixel_size_ceil_is_exact_at_app_unit_boundaries() {
        let zero_margins = [0; 4];
        let maximum = PageDefinition::from_app_units(i32::MAX, i32::MAX, zero_margins)
            .expect("maximum positive app-unit dimensions should be valid");
        assert_eq!(
            maximum.surface_pixel_size(),
            Some(UntypedSize2D::new(35_791_395, 35_791_395))
        );

        let exact_multiples = PageDefinition::from_app_units(60, 120, zero_margins)
            .expect("exact pixel multiples should be valid");
        assert_eq!(
            exact_multiples.surface_pixel_size(),
            Some(UntypedSize2D::new(1, 2))
        );

        let one_app_unit_remainders = PageDefinition::from_app_units(61, 121, zero_margins)
            .expect("one-app-unit remainders should be valid");
        assert_eq!(
            one_app_unit_remainders.surface_pixel_size(),
            Some(UntypedSize2D::new(2, 3))
        );

        let one_app_unit_before_boundaries = PageDefinition::from_app_units(59, 119, zero_margins)
            .expect("near-boundary app-unit dimensions should be valid");
        assert_eq!(
            one_app_unit_before_boundaries.surface_pixel_size(),
            Some(UntypedSize2D::new(1, 2))
        );
    }

    #[test]
    fn surface_pixel_size_preserves_ordinary_api1_a4_rounding() {
        let page = PageDefinition::new(
            793.7008,
            1122.5197,
            PageMargins::new(45.3543, 60.4724, 45.3543, 60.4724),
        )
        .expect("A4 API 1 geometry should be valid");

        assert_eq!(
            page.surface_pixel_size(),
            Some(UntypedSize2D::new(
                page.width().ceil() as u32,
                page.height().ceil() as u32,
            ))
        );
        assert_eq!(
            page.surface_pixel_size(),
            Some(UntypedSize2D::new(794, 1123))
        );
    }

    fn short_table_page_builder() -> BlockPageBuilder {
        BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        })
    }

    fn placed(outcome: TableChildPlacementOutcome, context: &str) -> TableChildPlacement {
        let TableChildPlacementOutcome::Placed(placement) = outcome else {
            panic!("{context}: {outcome:?}");
        };
        placement
    }

    fn header_body_table(body_block_size: i32) -> ([TableRow; 2], [TableRowGroup; 2]) {
        (
            [
                TableRow {
                    row_index: 0,
                    row_group_index: Some(0),
                    cell_count: 1,
                    has_rowspan: false,
                    block_start: Au::zero(),
                    block_size: Au::from_px(20),
                    breaks: ChildPageBreaks::default(),
                },
                TableRow {
                    row_index: 1,
                    row_group_index: Some(1),
                    cell_count: 1,
                    has_rowspan: false,
                    block_start: Au::from_px(20),
                    block_size: Au::from_px(body_block_size),
                    breaks: ChildPageBreaks::default(),
                },
            ],
            [
                TableRowGroup {
                    tag_id: Some(11),
                    row_group_index: 0,
                    first_row_index: 0,
                    end_row_index: 1,
                    block_start: Au::zero(),
                    block_size: Au::from_px(20),
                    kind: TableRowGroupKind::Header,
                    breaks: ChildPageBreaks::default(),
                },
                TableRowGroup {
                    tag_id: None,
                    row_group_index: 1,
                    first_row_index: 1,
                    end_row_index: 2,
                    block_start: Au::from_px(20),
                    block_size: Au::from_px(body_block_size),
                    kind: TableRowGroupKind::Body,
                    breaks: ChildPageBreaks::default(),
                },
            ],
        )
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
    fn page_debug_geometry_retains_large_app_units_without_float_round_trip() {
        let definition =
            PageDefinition::from_app_units(100_000_001, 100_000_019, [7, 11, 13, 17]).unwrap();
        let exact = definition.debug_app_units();

        assert_eq!(
            exact,
            LayoutDebugPageAppUnits {
                width: 100_000_001,
                height: 100_000_019,
                margin_top: 7,
                margin_right: 11,
                margin_bottom: 13,
                margin_left: 17,
                available_inline_size: 99_999_973,
                available_block_size: 99_999_999,
            }
        );
        assert_ne!(Au::from_f32_px(definition.width()).0, exact.width);
        assert_ne!(Au::from_f32_px(definition.height()).0, exact.height);
    }

    #[test]
    fn table_repeat_debug_geometry_retains_original_app_units() {
        let repeat = TableGroupRepeatDecision {
            page_index: 1,
            table_node: Some(7),
            header_tag_id: 11,
            row_group_index: 0,
            source_block_start: Au(100_000_001),
            target_block_start: Au(100_000_019),
            block_size: Au(601),
        };
        let sequence = PageSequence {
            pages: Vec::new(),
            table_breaks: Vec::new(),
            table_cell_continuations: Vec::new(),
            table_group_repeats: vec![repeat],
            warnings: Vec::new(),
        };
        let debug = sequence.debug_snapshot();
        let [retained] = debug.table_group_repeats.as_slice() else {
            panic!("the original repeat must be retained exactly once");
        };
        let exact = LayoutDebugTableGroupRepeatAppUnits {
            source_block_start: 100_000_001,
            target_block_start: 100_000_019,
            block_size: 601,
        };
        assert_eq!(retained.app_units, Some(exact));
        assert_eq!(retained.page_index, repeat.page_index);
        assert_eq!(retained.table_node, repeat.table_node);
        assert_eq!(retained.header_tag_id, repeat.header_tag_id);
        assert_eq!(retained.row_group_index, repeat.row_group_index);
        assert_eq!(
            retained.source_block_start,
            repeat.source_block_start.to_f32_px()
        );
        assert_eq!(
            retained.target_block_start,
            repeat.target_block_start.to_f32_px()
        );
        assert_eq!(retained.block_size, repeat.block_size.to_f32_px());
        assert_ne!(
            Au::from_f32_px(retained.source_block_start).0,
            exact.source_block_start
        );
        assert_ne!(
            Au::from_f32_px(retained.target_block_start).0,
            exact.target_block_start
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
                    style_source: LayoutDebugPageStyleSource::RequestDefaults,
                    app_units: LayoutDebugPageAppUnits {
                        width: 36_720,
                        height: 47_520,
                        margin_top: 4_320,
                        margin_right: 3_240,
                        margin_bottom: 2_160,
                        margin_left: 1_080,
                        available_inline_size: 32_400,
                        available_block_size: 41_040,
                    },
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
                table_breaks: vec![],
                table_cell_continuations: vec![],
                table_group_repeats: vec![],
                warnings: vec![],
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
                ChildPageBreaks::default(),
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
                ChildPageBreaks::default(),
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
                ChildPageBreaks::default(),
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
                    forced: false,
                    break_inside_avoid: false,
                    retry_count: 0,
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
                    forced: false,
                    break_inside_avoid: false,
                    retry_count: 0,
                    resume_page_index: 1,
                },
            }]
        );
    }

    #[test]
    fn forced_breaks_collapse_without_empty_edge_pages() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        assert_eq!(
            builder.place_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks {
                    before: true,
                    after: true,
                    inside_avoid: false,
                },
            ),
            BlockBoundaryPlacement::CurrentPage
        );
        assert_eq!(
            builder.place_child(
                1,
                Some(20),
                Au::from_px(20),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks {
                    before: true,
                    after: false,
                    inside_avoid: false,
                },
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(120),
            }
        );
        assert_eq!(
            builder.place_child(
                2,
                Some(30),
                Au::from_px(140),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks {
                    before: true,
                    after: true,
                    inside_avoid: false,
                },
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(240),
            }
        );

        let outcome = builder.finish();
        assert_eq!(outcome.page_count, 3);
        assert_eq!(
            outcome.continuations,
            vec![
                PageContinuation {
                    page_index: 0,
                    token: FragmentainerContinuation::Block(BlockContinuationToken {
                        next_child_index: 1,
                        next_node: Some(20),
                        forced: true,
                        break_inside_avoid: false,
                        retry_count: 0,
                        resume_page_index: 1,
                    }),
                },
                PageContinuation {
                    page_index: 1,
                    token: FragmentainerContinuation::Block(BlockContinuationToken {
                        next_child_index: 2,
                        next_node: Some(30),
                        forced: true,
                        break_inside_avoid: false,
                        retry_count: 0,
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
                    token: LayoutDebugContinuation::Block {
                        next_child_index: 1,
                        next_node: Some(20),
                        forced: true,
                        break_inside_avoid: false,
                        retry_count: 0,
                        resume_page_index: 1,
                    },
                },
                LayoutDebugPageContinuation {
                    page_index: 1,
                    token: LayoutDebugContinuation::Block {
                        next_child_index: 2,
                        next_node: Some(30),
                        forced: true,
                        break_inside_avoid: false,
                        retry_count: 0,
                        resume_page_index: 2,
                    },
                },
            ]
        );
    }

    #[test]
    fn break_inside_avoid_moves_a_fitting_child_once() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        assert_eq!(
            builder.place_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(80),
                Au::from_px(80),
                ChildPageBreaks::default(),
            ),
            BlockBoundaryPlacement::CurrentPage
        );
        assert_eq!(
            builder.place_child(
                1,
                Some(20),
                Au::from_px(80),
                Au::from_px(40),
                Au::from_px(40),
                ChildPageBreaks {
                    inside_avoid: true,
                    ..Default::default()
                },
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(120),
            }
        );

        let snapshot = PageSequenceContext { definition: page() }
            .page_sequence(builder.finish())
            .debug_snapshot();
        assert!(snapshot.warnings.is_empty());
        assert_eq!(snapshot.pages.len(), 2);
        assert_eq!(
            snapshot.continuations,
            vec![LayoutDebugPageContinuation {
                page_index: 0,
                token: LayoutDebugContinuation::Block {
                    next_child_index: 1,
                    next_node: Some(20),
                    forced: false,
                    break_inside_avoid: true,
                    retry_count: 1,
                    resume_page_index: 1,
                },
            }]
        );
    }

    #[test]
    fn oversized_break_inside_avoid_fragments_without_retrying() {
        let lines = (0..3)
            .map(|line_index| InlineLine {
                block_start: Au::from_px(line_index as i32 * 40),
                block_size: Au::from_px(40),
                source_start: line_index * 4,
                source_end: line_index * 4 + 4,
                resume: InlineResumePoint {
                    inline_item_index: 0,
                    text_offset: line_index * 4,
                    shaping_result_index: line_index,
                },
            })
            .collect::<Vec<_>>();
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        let placement = builder
            .place_inline_child(
                0,
                Some(10),
                Au::zero(),
                &lines,
                ChildPageBreaks {
                    inside_avoid: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            placement.line_translations,
            vec![Au::zero(), Au::zero(), Au::from_px(40)]
        );

        let snapshot = PageSequenceContext { definition: page() }
            .page_sequence(builder.finish())
            .debug_snapshot();
        assert_eq!(snapshot.pages.len(), 2);
        assert_eq!(
            snapshot.warnings,
            vec![LayoutDebugPageWarning::OversizedBreakInsideAvoid {
                child_index: 0,
                node: Some(10),
                block_size: 120.0,
                available_block_size: 100.0,
                retry_count: 0,
            }]
        );
        assert!(matches!(
            snapshot.continuations.as_slice(),
            [LayoutDebugPageContinuation {
                token: LayoutDebugContinuation::Inline { .. },
                ..
            }]
        ));
    }

    #[test]
    fn a_forced_break_moves_retained_inline_lines_as_one_child() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        assert_eq!(
            builder.place_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks {
                    before: false,
                    after: true,
                    inside_avoid: false,
                },
            ),
            BlockBoundaryPlacement::CurrentPage
        );
        let lines = [
            InlineLine {
                block_start: Au::from_px(20),
                block_size: Au::from_px(20),
                source_start: 0,
                source_end: 4,
                resume: InlineResumePoint {
                    inline_item_index: 0,
                    text_offset: 0,
                    shaping_result_index: 0,
                },
            },
            InlineLine {
                block_start: Au::from_px(40),
                block_size: Au::from_px(20),
                source_start: 4,
                source_end: 8,
                resume: InlineResumePoint {
                    inline_item_index: 0,
                    text_offset: 4,
                    shaping_result_index: 1,
                },
            },
        ];
        let placement = builder
            .place_inline_child(
                1,
                Some(20),
                Au::from_px(20),
                &lines,
                ChildPageBreaks {
                    before: true,
                    after: false,
                    inside_avoid: false,
                },
            )
            .unwrap();
        assert_eq!(
            placement.line_translations,
            vec![Au::from_px(100), Au::from_px(100)]
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
                    forced: true,
                    break_inside_avoid: false,
                    retry_count: 0,
                    resume_page_index: 1,
                }),
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

        let placement = builder
            .place_inline_child(0, Some(10), Au::zero(), &lines, ChildPageBreaks::default())
            .unwrap();
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
    fn table_rows_continue_once_at_stable_row_boundaries() {
        let rows = (0..5)
            .map(|row_index| TableRow {
                row_index,
                row_group_index: Some(0),
                cell_count: 2,
                has_rowspan: false,
                block_start: Au::from_px(row_index as i32 * 40),
                block_size: Au::from_px(40),
                breaks: ChildPageBreaks::default(),
            })
            .collect::<Vec<_>>();
        let row_groups = [TableRowGroup {
            tag_id: None,
            row_group_index: 0,
            first_row_index: 0,
            end_row_index: rows.len(),
            block_start: Au::zero(),
            block_size: Au::from_px(200),
            kind: TableRowGroupKind::Body,
            breaks: ChildPageBreaks::default(),
        }];
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });

        let placement = placed(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(200),
                &rows,
                &row_groups,
                &[],
                ChildPageBreaks::default(),
            ),
            "basic retained rows should paginate",
        );
        assert_eq!(
            placement.row_translations,
            vec![
                Au::zero(),
                Au::zero(),
                Au::from_px(40),
                Au::from_px(40),
                Au::from_px(80),
            ]
        );
        assert_eq!(placement.added_block_size, Au::from_px(80));

        let outcome = builder.finish();
        assert_eq!(outcome.page_count, 3);
        assert_eq!(
            outcome.continuations,
            vec![
                PageContinuation {
                    page_index: 0,
                    token: FragmentainerContinuation::Table(TableContinuationToken {
                        child_index: 0,
                        table_node: Some(10),
                        row_group_index: Some(0),
                        next_row_index: 2,
                        resume_page_index: 1,
                    }),
                },
                PageContinuation {
                    page_index: 1,
                    token: FragmentainerContinuation::Table(TableContinuationToken {
                        child_index: 0,
                        table_node: Some(10),
                        row_group_index: Some(0),
                        next_row_index: 4,
                        resume_page_index: 2,
                    }),
                },
            ]
        );
        assert_eq!(
            outcome
                .table_breaks
                .iter()
                .map(|decision| (decision.next_row_index, decision.selected))
                .collect::<Vec<_>>(),
            vec![(1, false), (2, true), (3, false), (4, true)]
        );
        let snapshot = PageSequenceContext { definition: page() }
            .page_sequence(outcome)
            .debug_snapshot();
        assert_eq!(
            snapshot
                .table_breaks
                .iter()
                .filter(|decision| decision.selected)
                .map(|decision| decision.next_row_index)
                .collect::<Vec<_>>(),
            vec![2, 4]
        );
    }

    #[test]
    fn oversized_avoided_row_group_warns_once_and_propagates_break_after() {
        let rows = (0..2)
            .map(|row_index| TableRow {
                row_index,
                row_group_index: Some(0),
                cell_count: 1,
                has_rowspan: false,
                block_start: Au::from_px(row_index as i32 * 60),
                block_size: Au::from_px(60),
                breaks: ChildPageBreaks::default(),
            })
            .collect::<Vec<_>>();
        let row_groups = [TableRowGroup {
            tag_id: None,
            row_group_index: 0,
            first_row_index: 0,
            end_row_index: rows.len(),
            block_start: Au::zero(),
            block_size: Au::from_px(120),
            kind: TableRowGroupKind::Body,
            breaks: ChildPageBreaks {
                after: true,
                inside_avoid: true,
                ..Default::default()
            },
        }];
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });

        let placement = placed(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(120),
                &rows,
                &row_groups,
                &[],
                ChildPageBreaks::default(),
            ),
            "an oversized avoided row group falls back to row pagination",
        );
        assert_eq!(
            placement.row_translations,
            vec![Au::zero(), Au::from_px(60)]
        );
        assert_eq!(
            builder.place_child(
                1,
                Some(20),
                Au::from_px(180),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks::default(),
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(240),
            }
        );

        assert_eq!(
            builder.finish().warnings,
            vec![
                BlockPaginationWarning::OversizedTableRowGroupBreakInsideAvoid {
                    child_index: 0,
                    table_node: Some(10),
                    row_group_index: 0,
                    block_size: Au::from_px(120),
                    available_block_size: Au::from_px(100),
                    retry_count: 0,
                    retry_limit: TABLE_ROW_RETRY_LIMIT,
                },
            ]
        );
    }

    #[test]
    fn unsupported_table_rows_leave_the_child_boundary_unclaimed() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        let invalid_rows = [
            TableRow {
                row_index: 0,
                row_group_index: None,
                cell_count: 2,
                has_rowspan: false,
                block_start: Au::zero(),
                block_size: Au::from_px(40),
                breaks: ChildPageBreaks::default(),
            },
            TableRow {
                row_index: 2,
                row_group_index: None,
                cell_count: 2,
                has_rowspan: false,
                block_start: Au::from_px(40),
                block_size: Au::from_px(40),
                breaks: ChildPageBreaks::default(),
            },
        ];
        assert_eq!(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(80),
                &invalid_rows,
                &[],
                &[],
                ChildPageBreaks::default(),
            ),
            TableChildPlacementOutcome::Unsupported
        );
        assert_eq!(
            builder.place_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(80),
                Au::from_px(80),
                ChildPageBreaks::default(),
            ),
            BlockBoundaryPlacement::CurrentPage
        );
    }

    #[test]
    fn unsupported_table_caption_pagination_is_a_typed_warning() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        builder.warn_unsupported_table_caption_pagination(0, Some(20));
        assert_eq!(
            builder.place_child(
                0,
                Some(20),
                Au::zero(),
                Au::from_px(80),
                Au::from_px(80),
                ChildPageBreaks::default(),
            ),
            BlockBoundaryPlacement::CurrentPage
        );

        let snapshot = PageSequenceContext { definition: page() }
            .page_sequence(builder.finish())
            .debug_snapshot();
        assert_eq!(
            snapshot.warnings,
            vec![LayoutDebugPageWarning::UnsupportedTableCaptionPagination {
                child_index: 0,
                node: Some(20),
            }]
        );
    }

    #[test]
    fn oversized_row_retains_per_cell_progress_on_the_stable_grid() {
        let rows = [TableRow {
            row_index: 0,
            row_group_index: Some(0),
            cell_count: 2,
            has_rowspan: false,
            block_start: Au::from_px(30),
            block_size: Au::from_px(120),
            breaks: ChildPageBreaks::default(),
        }];
        let row_groups = [TableRowGroup {
            tag_id: None,
            row_group_index: 0,
            first_row_index: 0,
            end_row_index: 1,
            block_start: Au::from_px(30),
            block_size: Au::from_px(120),
            kind: TableRowGroupKind::Body,
            breaks: ChildPageBreaks::default(),
        }];
        let fragments = [
            TableCellFragment {
                row_index: 0,
                cell_index: 0,
                fragment_index: 0,
                block_start: Au::from_px(30),
                block_size: Au::from_px(40),
            },
            TableCellFragment {
                row_index: 0,
                cell_index: 0,
                fragment_index: 1,
                block_start: Au::from_px(70),
                block_size: Au::from_px(40),
            },
            TableCellFragment {
                row_index: 0,
                cell_index: 0,
                fragment_index: 2,
                block_start: Au::from_px(110),
                block_size: Au::from_px(40),
            },
            TableCellFragment {
                row_index: 0,
                cell_index: 1,
                fragment_index: 0,
                block_start: Au::from_px(30),
                block_size: Au::from_px(30),
            },
            TableCellFragment {
                row_index: 0,
                cell_index: 1,
                fragment_index: 1,
                block_start: Au::from_px(60),
                block_size: Au::from_px(30),
            },
            TableCellFragment {
                row_index: 0,
                cell_index: 1,
                fragment_index: 2,
                block_start: Au::from_px(90),
                block_size: Au::from_px(30),
            },
        ];
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });

        let placement = placed(
            builder.place_table_child(
                0,
                Some(10),
                Au::from_px(30),
                Au::from_px(120),
                &rows,
                &row_groups,
                &fragments,
                ChildPageBreaks::default(),
            ),
            "breakable cell fragments must make progress",
        );
        assert_eq!(placement.row_translations, vec![Au::zero()]);
        assert_eq!(placement.row_block_extensions, vec![Au::from_px(50)]);
        assert_eq!(
            placement.cell_fragment_translations,
            vec![
                Au::zero(),
                Au::from_px(50),
                Au::from_px(50),
                Au::zero(),
                Au::zero(),
                Au::from_px(30),
            ]
        );
        assert_eq!(placement.added_block_size, Au::from_px(50));

        let snapshot = PageSequenceContext { definition: page() }
            .page_sequence(builder.finish())
            .debug_snapshot();
        assert_eq!(snapshot.pages.len(), 2);
        assert_eq!(snapshot.table_cell_continuations.len(), 2);
        assert_eq!(
            snapshot
                .table_cell_continuations
                .iter()
                .map(|resume| (resume.cell_index, resume.next_fragment_index))
                .collect::<Vec<_>>(),
            vec![(0, Some(1)), (1, Some(2))]
        );
    }

    #[test]
    fn oversized_atomic_cell_warns_once_and_claims_the_table() {
        let rows = [TableRow {
            row_index: 0,
            row_group_index: None,
            cell_count: 1,
            has_rowspan: false,
            block_start: Au::zero(),
            block_size: Au::from_px(140),
            breaks: ChildPageBreaks::default(),
        }];
        let fragments = [TableCellFragment {
            row_index: 0,
            cell_index: 0,
            fragment_index: 0,
            block_start: Au::zero(),
            block_size: Au::from_px(140),
        }];
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });

        let placement = placed(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(140),
                &rows,
                &[],
                &fragments,
                ChildPageBreaks::default(),
            ),
            "an unbreakable cell is retained with a typed warning",
        );
        assert_eq!(placement.added_block_size, Au::zero());
        let outcome = builder.finish();
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(
            outcome.warnings[0],
            BlockPaginationWarning::OversizedTableCell {
                child_index: 0,
                table_node: Some(10),
                row_index: 0,
                cell_index: 0,
                fragment_index: 0,
                block_size: Au::from_px(140),
                available_block_size: Au::from_px(100),
            }
        );
    }

    #[test]
    fn fitting_wrapper_avoid_yields_to_forced_row_boundaries() {
        let rows = [
            TableRow {
                row_index: 0,
                row_group_index: Some(0),
                cell_count: 1,
                has_rowspan: false,
                block_start: Au::zero(),
                block_size: Au::from_px(40),
                breaks: ChildPageBreaks::default(),
            },
            TableRow {
                row_index: 1,
                row_group_index: Some(0),
                cell_count: 1,
                has_rowspan: false,
                block_start: Au::from_px(40),
                block_size: Au::from_px(40),
                breaks: ChildPageBreaks {
                    before: true,
                    after: true,
                    inside_avoid: false,
                },
            },
        ];
        let row_groups = [TableRowGroup {
            tag_id: None,
            row_group_index: 0,
            first_row_index: 0,
            end_row_index: rows.len(),
            block_start: Au::zero(),
            block_size: Au::from_px(80),
            kind: TableRowGroupKind::Body,
            breaks: ChildPageBreaks::default(),
        }];
        let mut builder = short_table_page_builder();

        let placement = placed(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(80),
                &rows,
                &row_groups,
                &[],
                ChildPageBreaks {
                    inside_avoid: true,
                    ..Default::default()
                },
            ),
            "forced row breaks must outrank a fitting wrapper avoid",
        );
        assert_eq!(
            placement.row_translations,
            vec![Au::zero(), Au::from_px(80)]
        );
        assert_eq!(
            builder.place_child(
                1,
                Some(20),
                Au::from_px(160),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks::default(),
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(240),
            }
        );

        let outcome = builder.finish();
        assert_eq!(
            outcome
                .table_breaks
                .iter()
                .map(|decision| (decision.constraint, decision.selected))
                .collect::<Vec<_>>(),
            vec![(LayoutDebugTableConstraint::ForcedBefore, true)]
        );
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn fitting_wrapper_avoid_explicitly_yields_the_whole_child() {
        let rows = [
            TableRow {
                row_index: 0,
                row_group_index: Some(0),
                cell_count: 1,
                has_rowspan: false,
                block_start: Au::zero(),
                block_size: Au::from_px(40),
                breaks: ChildPageBreaks::default(),
            },
            TableRow {
                row_index: 1,
                row_group_index: Some(0),
                cell_count: 1,
                has_rowspan: false,
                block_start: Au::from_px(40),
                block_size: Au::from_px(40),
                breaks: ChildPageBreaks::default(),
            },
        ];
        let row_groups = [TableRowGroup {
            tag_id: None,
            row_group_index: 0,
            first_row_index: 0,
            end_row_index: rows.len(),
            block_start: Au::zero(),
            block_size: Au::from_px(80),
            kind: TableRowGroupKind::Body,
            breaks: ChildPageBreaks::default(),
        }];
        let mut builder = short_table_page_builder();

        assert_eq!(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(80),
                &rows,
                &row_groups,
                &[],
                ChildPageBreaks {
                    inside_avoid: true,
                    ..Default::default()
                },
            ),
            TableChildPlacementOutcome::WholeChild
        );
        assert_eq!(
            builder.place_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(80),
                Au::from_px(80),
                ChildPageBreaks {
                    inside_avoid: true,
                    ..Default::default()
                },
            ),
            BlockBoundaryPlacement::CurrentPage
        );
        assert!(builder.finish().warnings.is_empty());
    }

    #[test]
    fn header_reduced_oversized_row_without_fragment_progress_is_unsupported() {
        let (rows, row_groups) = header_body_table(90);
        let fragments = [TableCellFragment {
            row_index: 1,
            cell_index: 0,
            fragment_index: 0,
            block_start: Au::from_px(20),
            block_size: Au::from_px(10),
        }];
        let mut builder = short_table_page_builder();

        assert_eq!(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(110),
                &rows,
                &row_groups,
                &fragments,
                ChildPageBreaks::default(),
            ),
            TableChildPlacementOutcome::Unsupported
        );
        assert_eq!(
            builder.finish().warnings,
            vec![BlockPaginationWarning::UnsupportedTableGroupPagination {
                child_index: 0,
                table_node: Some(10),
                reason: TableGroupUnsupportedReason::UnsupportedLayout,
            },]
        );
    }

    #[test]
    fn natural_cell_page_advance_reserves_the_repeated_header() {
        let (rows, row_groups) = header_body_table(140);
        let fragments = [
            TableCellFragment {
                row_index: 1,
                cell_index: 0,
                fragment_index: 0,
                block_start: Au::from_px(20),
                block_size: Au::from_px(20),
            },
            TableCellFragment {
                row_index: 1,
                cell_index: 0,
                fragment_index: 1,
                block_start: Au::from_px(120),
                block_size: Au::from_px(20),
            },
        ];
        let mut builder = short_table_page_builder();

        let placement = placed(
            builder.place_table_child(
                0,
                Some(10),
                Au::zero(),
                Au::from_px(160),
                &rows,
                &row_groups,
                &fragments,
                ChildPageBreaks::default(),
            ),
            "retained fragments cross the header-reduced body capacity",
        );
        assert_eq!(
            placement.cell_fragment_translations,
            vec![Au::zero(), Au::from_px(20)]
        );
        assert_eq!(
            placement.row_block_extensions,
            vec![Au::zero(), Au::from_px(20)]
        );

        let outcome = builder.finish();
        assert_eq!(
            outcome.table_group_repeats,
            vec![TableGroupRepeatDecision {
                page_index: 1,
                table_node: Some(10),
                header_tag_id: 11,
                row_group_index: 0,
                source_block_start: Au::zero(),
                target_block_start: Au::from_px(120),
                block_size: Au::from_px(20),
            }]
        );
        assert!(outcome.warnings.is_empty());
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
    fn a_single_wrapper_passes_the_paged_root_to_a_fragmentable_child() {
        let controller = BlockPaginationController::default();
        controller.begin(page());
        assert!(controller.claim(1, true, false).is_none());
        assert!(controller.claim(1, true, true).is_some());
    }

    #[test]
    fn fixed_subtrees_cannot_claim_or_extend_the_final_page_count() {
        let controller = BlockPaginationController::default();
        assert_eq!(controller.fixed_page_layout(), None);
        controller.begin(page());
        let stride = page().size.height;
        assert_eq!(controller.fixed_page_layout(), Some((1, stride)));
        assert!(controller.claim(12, true, false).is_none());
        assert_eq!(controller.finish().page_count, 0);

        controller.begin(page());
        assert!(controller.claim(12, true, false).is_some());
        controller.complete(BlockPaginationOutcome {
            page_count: 12,
            ..Default::default()
        });
        assert_eq!(controller.fixed_page_layout(), Some((12, stride)));
        assert_eq!(controller.fixed_page_layout(), Some((12, stride)));
        assert!(controller.claim(100, true, false).is_none());
        assert_eq!(controller.finish().page_count, 12);
        assert_eq!(controller.fixed_page_layout(), None);
    }

    #[test]
    fn out_of_flow_placeholders_preserve_breaks_and_source_child_indices() {
        let mut builder = BlockPageBuilder::new(BlockPaginationRequest {
            available_block_size: Au::from_px(100),
            page_stride: Au::from_px(120),
        });
        builder.skip_out_of_flow_child(0);
        assert_eq!(
            builder.place_child(
                1,
                Some(10),
                Au::zero(),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks {
                    after: true,
                    ..Default::default()
                },
            ),
            BlockBoundaryPlacement::CurrentPage
        );
        builder.skip_out_of_flow_child(2);
        assert_eq!(
            builder.place_child(
                3,
                Some(20),
                Au::from_px(20),
                Au::from_px(20),
                Au::from_px(20),
                ChildPageBreaks::default(),
            ),
            BlockBoundaryPlacement::NewPage {
                block_origin: Au::from_px(120),
            }
        );
        builder.skip_out_of_flow_child(4);
        let outcome = builder.finish();
        assert_eq!(outcome.page_count, 2);
        assert!(outcome.warnings.is_empty());
        assert!(matches!(
            outcome.continuations[0].token,
            FragmentainerContinuation::Block(BlockContinuationToken {
                next_child_index: 3,
                ..
            })
        ));
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
                ChildPageBreaks::default(),
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
                ChildPageBreaks::default(),
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
        assert_eq!(
            PageSequenceContext { definition: page() }
                .page_sequence(outcome)
                .debug_snapshot()
                .warnings,
            vec![LayoutDebugPageWarning::OversizedUnbreakable {
                child_index: 0,
                node: Some(10),
                block_size: 150.0,
                available_block_size: 100.0,
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
