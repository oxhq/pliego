/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The Pliego-owned paged-document root.
//!
//! This module deliberately starts with one page and no continuation. The fragment tree is laid
//! out directly into the page content box, and overflowing fragments remain in that tree for the
//! later fragmentation milestones to continue. Nothing in this module clips, slices, or re-lays
//! out rendered output.

use std::fmt;
use std::sync::OnceLock;

use app_units::Au;
use euclid::default::Size2D as UntypedSize2D;
use euclid::{Point2D, Rect, Size2D};
use layout_api::{LayoutDebugPage, LayoutDebugPageSequence};
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
    fn one_page_sequence(self) -> PageSequence {
        let fragmentainer = FragmentainerContext {
            page_index: 0,
            available_inline_size: self.definition.available_inline_size(),
            available_block_size: self.definition.available_block_size(),
            overflow: FragmentainerOverflow::RetainInFragmentTree,
        };
        PageSequence {
            pages: vec![Page {
                definition: self.definition,
                fragmentainer,
            }],
        }
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
    let page_sequence = context.one_page_sequence();
    let fragment_tree = match page_sequence.pages[0].fragmentainer.overflow {
        FragmentainerOverflow::RetainInFragmentTree => {
            box_tree.layout_in_containing_block(layout_context, context.definition.content_rect())
        },
    };
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
