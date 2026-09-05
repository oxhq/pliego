/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
#![allow(rustdoc::private_intra_doc_links)]

//! # HTML Tables (╯°□°)╯︵ ┻━┻
//!
//! This implementation is based on the [table section of the HTML 5 Specification][1],
//! the draft [CSS Table Module Level! 3][2] and the [LayoutNG implementation of tables][3] in Blink.
//! In general, the draft specification differs greatly from what other browsers do, so we
//! generally follow LayoutNG when in question.
//!
//! [1]: https://html.spec.whatwg.org/multipage/#tables
//! [2]: https://drafts.csswg.org/css-tables
//! [3]: https://source.chromium.org/chromium/chromium/src/third_party/+/main:blink/renderer/core/layout/table
//!
//! Table layout is divided into two phases:
//!
//! 1. Box Tree Construction
//! 2. Fragment Tree Construction
//!
//! ## Box Tree Construction
//!
//! During box tree construction, table layout (`construct.rs`) will traverse the DOM and construct
//! the basic box tree representation of a table, using the structs defined in this file ([`Table`],
//! [`TableTrackGroup`], [`TableTrack`], etc). When processing the DOM, elements are handled
//! differently depending on their `display` value. For instance, an element with `display:
//! table-cell` is treated as a table cell. HTML table elements like `<table>` and `<td>` are
//! assigned the corresponding display value from the user agent stylesheet.
//!
//! Every [`Table`] holds an array of [`TableSlot`]. A [`TableSlot`] represents either a cell, a cell
//! location occupied by a cell originating from another place in the table, or is empty. In
//! addition, when there are table model errors, a slot may spanned by more than one cell.
//!
//! During processing, the box tree construction agorithm will also fix up table structure, for
//! instance, creating anonymous rows for lone table cells and putting non-table content into
//! anonymous cells. In addition, flow layout will collect table elements outside of tables and create
//! anonymous tables for them.
//!
//! After processing, box tree construction does a fix up pass on tables, converting rowspan=0 into
//! definite rowspan values and making sure that rowspan and celspan values are not larger than the
//! table itself. Finally, row groups may be reordered to enforce the fact that the first `<thead>`
//! comes before `<tbody>` which comes before the first `<tfoot>`.
//!
//! ## Fragment Tree Construction
//!
//! Fragment tree construction involves calculating the size and positioning of all table elements,
//! given their style, content, and cell and row spans. This happens both during intrinsic inline
//! size computation as well as layout into Fragments. In both of these cases, measurement and
//! layout is done by [`layout::TableLayout`], though for intrinsic size computation only a partial
//! layout is done.
//!
//! In general, we follow the following steps when laying out table content:
//!
//! 1. Compute track constrainedness and has originating cells
//! 2. Compute cell measures
//! 3. Compute column measures
//! 4. Compute intrinsic inline sizes for columns and the table
//! 5. Compute the final table inline size
//! 6. Distribute size to columns
//! 7. Do first pass cell layout
//! 8. Do row layout
//! 9. Compute table height and final row sizes
//! 10. Create fragments for table elements (columns, column groups, rows, row groups, cells)
//!
//! For intrinsic size computation this process stops at step 4.

mod construct;
mod layout;

use std::ops::Range;

use app_units::Au;
use atomic_refcell::AtomicRef;
pub(crate) use construct::AnonymousTableContent;
pub use construct::TableBuilder;
use euclid::{Point2D, Size2D, UnknownUnit, Vector2D};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::{ServoDangerousStyleElement, ServoLayoutNode};
use servo_arc::Arc;
use style::Zero;
use style::color::ColorSpace;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;
use style::properties::style_structs::Font;
use style::selector_parser::PseudoElement;
use style::values::computed::BorderStyle;

use super::flow::BlockFormattingContext;
use crate::cell::{ArcRefCell, WeakRefCell};
use crate::dom::WeakLayoutBox;
use crate::flow::BlockContainer;
use crate::formatting_contexts::{
    IndependentFormattingContext, IndependentFormattingContextContents,
};
use crate::fragment_tree::BaseFragmentInfo;
use crate::geom::PhysicalVec;
use crate::layout_box_base::LayoutBoxBase;
use crate::style_ext::BorderStyleColor;
use crate::table::layout::TableLayout;
use crate::{PropagatedBoxTreeData, SharedStyle};

pub type TableSize = Size2D<usize, UnknownUnit>;

#[derive(Debug, MallocSizeOf)]
pub struct Table {
    /// The style of this table. These are the properties that apply to the "wrapper" ie the element
    /// that contains both the grid and the captions. Not all properties are actually used on the
    /// wrapper though, such as background and borders, which apply to the grid.
    style: Arc<ComputedValues>,

    /// The style of this table's grid. This is an anonymous style based on the table's style, but
    /// eliminating all the properties handled by the "wrapper."
    grid_style: Arc<ComputedValues>,

    /// The [`BaseFragmentInfo`] for this table's grid. This is necessary so that when the
    /// grid has a background image, it can be associated with the table's node.
    grid_base_fragment_info: BaseFragmentInfo,

    /// The captions for this table.
    pub captions: Vec<ArcRefCell<TableCaption>>,

    /// The column groups for this table.
    pub column_groups: Vec<ArcRefCell<TableTrackGroup>>,

    /// The columns of this table defined by `<colgroup> | display: table-column-group`
    /// and `<col> | display: table-column` elements as well as `display: table-column`.
    pub columns: Vec<ArcRefCell<TableTrack>>,

    /// The rows groups for this table defined by `<tbody>`, `<thead>`, and `<tfoot>`.
    pub row_groups: Vec<ArcRefCell<TableTrackGroup>>,

    /// The rows of this table defined by `<tr>` or `display: table-row` elements.
    pub rows: Vec<ArcRefCell<TableTrack>>,

    /// The content of the slots of this table.
    pub slots: Vec<Vec<TableSlot>>,

    /// The size of this table.
    pub size: TableSize,

    /// Whether or not this Table is anonymous.
    anonymous: bool,

    /// Whether percentage columns are taken into account during inline content sizes calculation.
    percentage_columns_allowed_for_inline_content_sizes: bool,
}

impl Table {
    pub(crate) fn new(
        style: Arc<ComputedValues>,
        grid_style: Arc<ComputedValues>,
        base_fragment_info: BaseFragmentInfo,
        percentage_columns_allowed_for_inline_content_sizes: bool,
    ) -> Self {
        Self {
            style,
            grid_style,
            grid_base_fragment_info: base_fragment_info,
            captions: Vec::new(),
            column_groups: Vec::new(),
            columns: Vec::new(),
            row_groups: Vec::new(),
            rows: Vec::new(),
            slots: Vec::new(),
            size: TableSize::zero(),
            anonymous: false,
            percentage_columns_allowed_for_inline_content_sizes,
        }
    }

    /// Return the slot at the given coordinates, if it exists in the table, otherwise
    /// return None.
    fn get_slot(&self, coords: TableSlotCoordinates) -> Option<&TableSlot> {
        self.slots.get(coords.y)?.get(coords.x)
    }

    fn resolve_first_cell_coords(
        &self,
        coords: TableSlotCoordinates,
    ) -> Option<TableSlotCoordinates> {
        match self.get_slot(coords) {
            Some(&TableSlot::Cell(_)) => Some(coords),
            Some(TableSlot::Spanned(offsets)) => Some(coords - offsets[0]),
            _ => None,
        }
    }

    fn resolve_first_cell(
        &self,
        coords: TableSlotCoordinates,
    ) -> Option<AtomicRef<'_, TableSlotCell>> {
        let resolved_coords = self.resolve_first_cell_coords(coords)?;
        let slot = self.get_slot(resolved_coords);
        match slot {
            Some(TableSlot::Cell(cell)) => Some(cell.borrow()),
            _ => unreachable!(
                "Spanned slot should not point to an empty cell or another spanned slot."
            ),
        }
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        new_style: &Arc<ComputedValues>,
    ) {
        self.style = new_style.clone();
        self.grid_style = context
            .stylist
            .style_for_anonymous::<ServoDangerousStyleElement>(
                &context.guards,
                &PseudoElement::ServoTableGrid,
                new_style,
            );
    }

    pub(crate) fn subtree_size(&self) -> usize {
        self.slots
            .iter()
            .flat_map(|row| row.iter())
            .map(TableSlot::subtree_size)
            .sum()
    }
}

type TableSlotCoordinates = Point2D<usize, UnknownUnit>;
pub type TableSlotOffset = Vector2D<usize, UnknownUnit>;

#[derive(Debug, MallocSizeOf)]
pub struct TableSlotCell {
    /// The independent formatting context that the cell establishes for its contents.
    /// Currently, this should always be a block formatting context.
    pub(crate) context: IndependentFormattingContext,

    /// Number of columns that the cell is to span. Must be greater than zero.
    colspan: usize,

    /// Number of rows that the cell is to span. Zero means that the cell is to span all
    /// the remaining rows in the row group.
    rowspan: usize,
}

impl TableSlotCell {
    pub fn mock_for_testing(id: usize, colspan: usize, rowspan: usize) -> Self {
        let base = LayoutBoxBase::new(
            BaseFragmentInfo::new_for_testing(id),
            ComputedValues::initial_values_with_font_override(Font::initial_values()).to_arc(),
        );
        let contents = IndependentFormattingContextContents::Flow(BlockFormattingContext {
            contents: BlockContainer::BlockLevelBoxes(Vec::new()),
            contains_floats: false,
        });
        let propagated_data =
            PropagatedBoxTreeData::default().disallowing_percentage_table_columns();
        Self {
            context: IndependentFormattingContext::new(base, contents, propagated_data),
            colspan,
            rowspan,
        }
    }

    /// Get the node id of this cell's [`BaseFragmentInfo`]. This is used for unit tests.
    pub fn node_id(&self) -> usize {
        self.context
            .base
            .base_fragment_info
            .tag
            .map_or(0, |tag| tag.node.0)
    }
}

/// A single table slot. It may be an actual cell, or a reference
/// to a previous cell that is spanned here
///
/// In case of table model errors, it may be multiple references
#[derive(MallocSizeOf)]
pub enum TableSlot {
    /// A table cell, with a colspan and a rowspan.
    Cell(ArcRefCell<TableSlotCell>),

    /// This slot is spanned by one or more multiple cells earlier in the table, which are
    /// found at the given negative coordinate offsets. The vector is in the order of most
    /// recent to earliest cell.
    ///
    /// If there is more than one cell that spans a slot, this is a table model error, but
    /// we still keep track of it. See
    /// <https://html.spec.whatwg.org/multipage/#table-model-error>
    Spanned(Vec<TableSlotOffset>),

    /// An empty spot in the table. This can happen when there is a gap in columns between
    /// cells that are defined and one which should exist because of cell with a rowspan
    /// from a previous row.
    Empty,
}

impl std::fmt::Debug for TableSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cell(_) => f.debug_tuple("Cell").finish(),
            Self::Spanned(spanned) => f.debug_tuple("Spanned").field(spanned).finish(),
            Self::Empty => write!(f, "Empty"),
        }
    }
}

impl TableSlot {
    fn new_spanned(offset: TableSlotOffset) -> Self {
        Self::Spanned(vec![offset])
    }

    pub(crate) fn subtree_size(&self) -> usize {
        match self {
            TableSlot::Cell(cell) => cell.borrow().context.subtree_size(),
            TableSlot::Spanned(..) | TableSlot::Empty => 0,
        }
    }
}

/// A row or column of a table.
#[derive(Debug, MallocSizeOf)]
pub struct TableTrack {
    /// The [`LayoutBoxBase`] of this [`TableTrack`].
    base: LayoutBoxBase,

    /// The index of the table row or column group parent in the table's list of row or column
    /// groups.
    group_index: Option<usize>,

    /// Whether or not this [`TableTrack`] was anonymous, for instance created due to
    /// a `span` attribute set on a parent `<colgroup>`.
    is_anonymous: bool,

    /// A shared container for this track's style, used to share the style for the purposes
    /// of drawing backgrounds in individual cells. This allows updating the style in a
    /// single place and having it affect all cell `Fragment`s.
    shared_background_style: SharedStyle,
}

impl TableTrack {
    fn repair_style(&mut self, new_style: &Arc<ComputedValues>) {
        self.base.repair_style(new_style);
        self.shared_background_style = SharedStyle::new(new_style.clone());
    }
}

#[derive(Debug, MallocSizeOf, PartialEq)]
pub enum TableTrackGroupType {
    HeaderGroup,
    FooterGroup,
    RowGroup,
    ColumnGroup,
}

#[derive(Debug, MallocSizeOf)]
pub struct TableTrackGroup {
    /// The [`LayoutBoxBase`] of this [`TableTrackGroup`].
    base: LayoutBoxBase,

    /// The type of this [`TableTrackGroup`].
    group_type: TableTrackGroupType,

    /// The range of tracks in this [`TableTrackGroup`].
    track_range: Range<usize>,

    /// A shared container for this track's style, used to share the style for the purposes
    /// of drawing backgrounds in individual cells. This allows updating the style in a
    /// single place and having it affect all cell `Fragment`s.
    shared_background_style: SharedStyle,
}

impl TableTrackGroup {
    pub(super) fn is_empty(&self) -> bool {
        self.track_range.is_empty()
    }

    fn repair_style(&mut self, new_style: &Arc<ComputedValues>) {
        self.base.repair_style(new_style);
        self.shared_background_style = SharedStyle::new(new_style.clone());
    }
}

#[derive(Debug, MallocSizeOf)]
pub struct TableCaption {
    /// The contents of this cell, with its own layout.
    pub(crate) context: IndependentFormattingContext,
}

/// A calculated collapsed border.
#[derive(Clone, Debug, Default, MallocSizeOf, PartialEq)]
pub(crate) struct CollapsedBorder {
    pub style_color: BorderStyleColor,
    pub width: Au,
}

impl CollapsedBorder {
    pub(crate) fn is_invisible(&self) -> bool {
        self.width >= Au::zero() &&
            (self.width.is_zero() ||
                matches!(
                    self.style_color.style,
                    BorderStyle::None | BorderStyle::Hidden
                ) ||
                self.style_color
                    .color
                    .clone()
                    .to_color_space(ColorSpace::Srgb)
                    .alpha <=
                    0.0)
    }
}

/// Represents a piecewise sequence of collapsed borders along a line.
pub(crate) type CollapsedBorderLine = Vec<CollapsedBorder>;

#[derive(Clone, Debug, MallocSizeOf)]
pub(crate) struct SpecificTableGridInfo {
    pub collapsed_borders: PhysicalVec<Vec<CollapsedBorderLine>>,
    pub track_sizes: PhysicalVec<Vec<Au>>,
}

impl SpecificTableGridInfo {
    // Inspect the resolved edges, not authored styles: an invisible winner can
    // still reserve border width, but must not produce synthetic border paint.
    pub(crate) fn has_no_visible_borders(&self) -> bool {
        let mut borders = self
            .collapsed_borders
            .x
            .iter()
            .chain(&self.collapsed_borders.y)
            .flat_map(|line| line.iter())
            .peekable();
        borders.peek().is_some() && borders.all(CollapsedBorder::is_invisible)
    }

    // This profile captures uninterrupted row rules, not arbitrary collapsed
    // grids. Even transparent vertical edges must have zero width: WebRender
    // uses their widths for horizontal segment bounds and corner joins.
    pub(crate) fn has_solid_horizontal_borders(&self) -> bool {
        if self.collapsed_borders.x.is_empty() ||
            self.collapsed_borders
                .x
                .iter()
                .any(|line| line.is_empty() || line.iter().any(|border| !border.width.is_zero()))
        {
            return false;
        }

        let mut has_visible_line = false;
        for line in &self.collapsed_borders.y {
            let Some(first) = line.first() else {
                return false;
            };
            if first.is_invisible() {
                if !line.iter().all(CollapsedBorder::is_invisible) {
                    return false;
                }
                continue;
            }
            let color = first
                .style_color
                .color
                .clone()
                .to_color_space(ColorSpace::Srgb);
            if first.width <= Au::zero() ||
                first.style_color.style != BorderStyle::Solid ||
                !(color.alpha > 0.0) ||
                line.iter().any(|border| {
                    border.width != first.width ||
                        border.style_color.style != BorderStyle::Solid ||
                        border
                            .style_color
                            .color
                            .clone()
                            .to_color_space(ColorSpace::Srgb) !=
                            color
                })
            {
                return false;
            }
            has_visible_line = true;
        }
        has_visible_line
    }

    pub(crate) fn uniform_solid_visible_border(&self) -> Option<&CollapsedBorder> {
        let mut borders = self
            .collapsed_borders
            .x
            .iter()
            .chain(&self.collapsed_borders.y)
            .flat_map(|line| line.iter());
        let first = borders.next()?;
        let color = first
            .style_color
            .color
            .clone()
            .to_color_space(ColorSpace::Srgb);
        (first.width > Au::zero() &&
            first.style_color.style == BorderStyle::Solid &&
            color.alpha > 0.0 &&
            borders.all(|border| {
                border.width == first.width &&
                    border.style_color.style == first.style_color.style &&
                    border
                        .style_color
                        .color
                        .clone()
                        .to_color_space(ColorSpace::Srgb) ==
                        color
            }))
        .then_some(first)
    }
}

#[cfg(test)]
mod collapsed_border_profile_tests {
    use style::color::AbsoluteColor;

    use super::*;

    fn border(width: i32, red: f32) -> CollapsedBorder {
        CollapsedBorder {
            style_color: BorderStyleColor::new(
                BorderStyle::Solid,
                AbsoluteColor::new(ColorSpace::Srgb, red, 0.0, 0.0, 1.0),
            ),
            width: Au::from_px(width),
        }
    }

    fn one_cell_grid(border: CollapsedBorder) -> SpecificTableGridInfo {
        SpecificTableGridInfo {
            collapsed_borders: PhysicalVec::new(
                vec![vec![border.clone()], vec![border.clone()]],
                vec![vec![border.clone()], vec![border]],
            ),
            track_sizes: PhysicalVec::new(vec![Au::from_px(10)], vec![Au::from_px(10)]),
        }
    }

    fn invisible_borders() -> [CollapsedBorder; 4] {
        let mut none = border(3, 0.5);
        none.style_color.style = BorderStyle::None;
        let mut hidden = border(3, 0.5);
        hidden.style_color.style = BorderStyle::Hidden;
        let transparent = CollapsedBorder {
            style_color: BorderStyleColor::new(
                BorderStyle::Dashed,
                AbsoluteColor::new(ColorSpace::Srgb, 0.5, 0.0, 0.0, 0.0),
            ),
            width: Au::from_px(4),
        };
        [border(0, 0.5), none, hidden, transparent]
    }

    fn horizontal_grid() -> SpecificTableGridInfo {
        SpecificTableGridInfo {
            collapsed_borders: PhysicalVec::new(
                vec![vec![border(0, 0.0); 2]; 3],
                vec![
                    vec![border(0, 0.0); 2],
                    vec![border(2, 0.5); 2],
                    vec![border(1, 0.75); 2],
                ],
            ),
            track_sizes: PhysicalVec::new(vec![Au::from_px(10); 2], vec![Au::from_px(10); 2]),
        }
    }

    #[test]
    fn accepts_full_width_horizontal_rules_with_distinct_header_and_body_values() {
        let info = horizontal_grid();
        assert!(info.has_solid_horizontal_borders());
        assert!(!info.has_no_visible_borders());
        assert!(info.uniform_solid_visible_border().is_none());
    }

    #[test]
    fn accepts_entirely_invisible_horizontal_boundaries_without_paint() {
        for invisible in invisible_borders() {
            let mut info = horizontal_grid();
            info.collapsed_borders.y[0][0] = invisible;
            assert!(info.has_solid_horizontal_borders());
        }
        let mut info = horizontal_grid();
        for line in &mut info.collapsed_borders.y {
            line.fill(border(0, 0.0));
        }
        assert!(!info.has_solid_horizontal_borders());
        assert!(info.has_no_visible_borders());
    }

    #[test]
    fn horizontal_rules_reject_visible_or_nonzero_invisible_vertical_edges() {
        let mut info = horizontal_grid();
        info.collapsed_borders.x[1][0] = border(1, 0.5);
        assert!(!info.has_solid_horizontal_borders());
        for invisible in invisible_borders().into_iter().skip(1) {
            info.collapsed_borders.x[1][0] = invisible;
            assert!(!info.has_solid_horizontal_borders());
        }
    }

    #[test]
    fn horizontal_rules_reject_dashed_partial_and_nonuniform_lines() {
        let mut info = horizontal_grid();
        info.collapsed_borders.y[1][0].style_color.style = BorderStyle::Dashed;
        assert!(!info.has_solid_horizontal_borders());
        for invisible in invisible_borders() {
            for column in 0..2 {
                let mut info = horizontal_grid();
                info.collapsed_borders.y[1][column] = invisible.clone();
                assert!(!info.has_solid_horizontal_borders());
            }
        }
        for different in [border(1, 0.5), border(2, 0.75)] {
            let mut info = horizontal_grid();
            info.collapsed_borders.y[1][1] = different;
            assert!(!info.has_solid_horizontal_borders());
        }
    }

    #[test]
    fn horizontal_rules_reject_negative_widths_and_empty_edges() {
        for horizontal in [false, true] {
            let mut info = horizontal_grid();
            if horizontal {
                info.collapsed_borders.y[0][0].width = Au(-1);
            } else {
                info.collapsed_borders.x[0][0].width = Au(-1);
            }
            assert!(!info.has_solid_horizontal_borders());
        }
        let mut info = horizontal_grid();
        info.collapsed_borders.x.clear();
        assert!(!info.has_solid_horizontal_borders());
        let mut info = horizontal_grid();
        info.collapsed_borders.y[1].clear();
        assert!(!info.has_solid_horizontal_borders());
    }

    #[test]
    fn accepts_only_entirely_invisible_resolved_borders() {
        for invisible in invisible_borders() {
            let info = one_cell_grid(invisible);
            assert!(info.has_no_visible_borders());
            assert!(info.uniform_solid_visible_border().is_none());
        }

        let [zero, none, hidden, transparent] = invisible_borders();
        let mut info = one_cell_grid(zero);
        info.collapsed_borders.x[1][0] = none;
        info.collapsed_borders.y[0][0] = hidden;
        info.collapsed_borders.y[1][0] = transparent;
        assert!(info.has_no_visible_borders());
        assert!(info.uniform_solid_visible_border().is_none());
    }

    #[test]
    fn rejects_mixed_visible_and_invisible_resolved_borders() {
        assert!(!one_cell_grid(border(2, 0.5)).has_no_visible_borders());
        for invisible in invisible_borders() {
            let mut info = one_cell_grid(border(2, 0.5));
            info.collapsed_borders.y[1][0] = invisible;
            assert!(!info.has_no_visible_borders());
            assert!(info.uniform_solid_visible_border().is_none());
        }
    }

    #[test]
    fn invisible_borders_do_not_accept_negative_widths_or_empty_edges() {
        for mut invisible in invisible_borders() {
            invisible.width = Au(-1);
            assert!(!one_cell_grid(invisible).has_no_visible_borders());
        }
        let mut empty = one_cell_grid(border(0, 0.5));
        empty.collapsed_borders.x.clear();
        empty.collapsed_borders.y.clear();
        assert!(!empty.has_no_visible_borders());
        assert!(empty.uniform_solid_visible_border().is_none());
    }

    #[test]
    fn requires_one_resolved_collapsed_border_value() {
        let uniform = border(2, 0.5);
        let mut info = SpecificTableGridInfo {
            collapsed_borders: PhysicalVec::new(
                vec![vec![uniform.clone()], vec![uniform.clone()]],
                vec![vec![uniform.clone()], vec![uniform.clone()]],
            ),
            track_sizes: PhysicalVec::new(vec![Au::from_px(10)], vec![Au::from_px(10)]),
        };
        assert_eq!(info.uniform_solid_visible_border(), Some(&uniform));

        info.collapsed_borders.y[1][0] = border(3, 0.5);
        assert!(info.uniform_solid_visible_border().is_none());
        info.collapsed_borders.y[1][0] = border(2, 0.75);
        assert!(info.uniform_solid_visible_border().is_none());
    }
}

pub(crate) struct TableLayoutStyle<'a> {
    table: &'a Table,
    layout: Option<&'a TableLayout<'a>>,
}

/// Table parts that are stored in the DOM. This is used in order to map from
/// the DOM to the box tree and will eventually be important for incremental
/// layout.
#[derive(Debug, MallocSizeOf)]
pub(crate) enum TableLevelBox {
    Caption(ArcRefCell<TableCaption>),
    Cell(ArcRefCell<TableSlotCell>),
    TrackGroup(ArcRefCell<TableTrackGroup>),
    Track(ArcRefCell<TableTrack>),
}

impl TableLevelBox {
    pub(crate) fn with_base<T>(&self, callback: impl FnOnce(&LayoutBoxBase) -> T) -> T {
        match self {
            TableLevelBox::Caption(caption) => callback(&caption.borrow().context.base),
            TableLevelBox::Cell(cell) => callback(&cell.borrow().context.base),
            TableLevelBox::TrackGroup(track_group) => callback(&track_group.borrow().base),
            TableLevelBox::Track(track) => callback(&track.borrow().base),
        }
    }

    pub(crate) fn with_base_mut<T>(&mut self, callback: impl FnOnce(&mut LayoutBoxBase) -> T) -> T {
        match self {
            TableLevelBox::Caption(caption) => callback(&mut caption.borrow_mut().context.base),
            TableLevelBox::Cell(cell) => callback(&mut cell.borrow_mut().context.base),
            TableLevelBox::TrackGroup(track_group) => callback(&mut track_group.borrow_mut().base),
            TableLevelBox::Track(track) => callback(&mut track.borrow_mut().base),
        }
    }

    pub(crate) fn repair_style(
        &self,
        context: &SharedStyleContext<'_>,
        node: &ServoLayoutNode,
        new_style: &Arc<ComputedValues>,
    ) {
        match self {
            TableLevelBox::Caption(caption) => caption
                .borrow_mut()
                .context
                .repair_style(context, node, new_style),
            TableLevelBox::Cell(cell) => cell
                .borrow_mut()
                .context
                .repair_style(context, node, new_style),
            TableLevelBox::TrackGroup(track_group) => {
                track_group.borrow_mut().repair_style(new_style);
            },
            TableLevelBox::Track(track) => track.borrow_mut().repair_style(new_style),
        }
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        match self {
            Self::Caption(caption) => caption.borrow().context.attached_to_tree(layout_box),
            Self::Cell(cell) => cell.borrow().context.attached_to_tree(layout_box),
            Self::TrackGroup(_) | Self::Track(_) => {
                // The parentage of tracks within a track group, and cells within a row, is handled
                // when the entire table is attached to the tree.
            },
        }
    }

    pub(crate) fn downgrade(&self) -> WeakTableLevelBox {
        match self {
            Self::Caption(caption) => WeakTableLevelBox::Caption(caption.downgrade()),
            Self::Cell(cell) => WeakTableLevelBox::Cell(cell.downgrade()),
            Self::TrackGroup(track_group) => WeakTableLevelBox::TrackGroup(track_group.downgrade()),
            Self::Track(track) => WeakTableLevelBox::Track(track.downgrade()),
        }
    }
}

#[derive(Clone, Debug, MallocSizeOf)]
pub(crate) enum WeakTableLevelBox {
    Caption(WeakRefCell<TableCaption>),
    Cell(WeakRefCell<TableSlotCell>),
    TrackGroup(WeakRefCell<TableTrackGroup>),
    Track(WeakRefCell<TableTrack>),
}

impl WeakTableLevelBox {
    pub(crate) fn upgrade(&self) -> Option<TableLevelBox> {
        Some(match self {
            Self::Caption(caption) => TableLevelBox::Caption(caption.upgrade()?),
            Self::Cell(cell) => TableLevelBox::Cell(cell.upgrade()?),
            Self::TrackGroup(track_group) => TableLevelBox::TrackGroup(track_group.upgrade()?),
            Self::Track(track) => TableLevelBox::Track(track.upgrade()?),
        })
    }
}
