/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! This module contains traits in script used generically in the rest of Servo.
//! The traits are here instead of in script so that these modules won't have
//! to depend on script.

#![deny(unsafe_code)]

mod layout_damage;
mod layout_dom;
mod layout_element;
mod layout_node;
mod pseudo_element_chain;

use std::any::Any;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicIsize;
use std::thread::JoinHandle;
use std::time::Duration;

use app_units::Au;
use atomic_refcell::AtomicRefCell;
use background_hang_monitor_api::BackgroundHangMonitorRegister;
use bitflags::bitflags;
use embedder_traits::{Cursor, ScriptToEmbedderChan, Theme, UntrustedNodeAddress, ViewportDetails};
use euclid::{Point2D, Rect};
use fonts::{FontContext, FontIdentifier, TextByteRange, WebFontDocumentContext};
pub use layout_damage::{AccessibilityDamage, LayoutDamage};
pub use layout_dom::{
    DangerousStyleElementOf, DangerousStyleNodeOf, LayoutDomTypeBundle, LayoutElementOf,
    LayoutNodeOf,
};
pub use layout_element::{DangerousStyleElement, LayoutElement};
pub use layout_node::{DangerousStyleNode, LayoutNode};
use libc::c_void;
use malloc_size_of::{MallocSizeOf as MallocSizeOfTrait, MallocSizeOfOps, malloc_size_of_is_0};
use malloc_size_of_derive::MallocSizeOf;
use net_traits::image_cache::{ImageCache, ImageCacheFactory, PendingImageId, VectorImageSnapshot};
use net_traits::request::InternalRequest;
use paint_api::CrossProcessPaintApi;
use parking_lot::RwLock;
use pixels::{RasterImage, Repeat};
use profile_traits::mem::Report;
use profile_traits::time;
pub use pseudo_element_chain::PseudoElementChain;
use rustc_hash::{FxHashMap, FxHashSet};
use script_traits::{InitialScriptState, Painter, ScriptThreadMessage};
use serde::{Deserialize, Serialize};
use servo_arc::Arc as ServoArc;
use servo_base::Epoch;
use servo_base::generic_channel::GenericSender;
use servo_base::id::{BrowsingContextId, PipelineId, WebViewId};
use servo_url::{ImmutableOrigin, ServoUrl};
use style::Atom;
use style::animation::DocumentAnimationSet;
use style::attr::{AttrValue, parse_integer, parse_unsigned_integer};
use style::context::QuirksMode;
use style::data::ElementDataWrapper;
use style::device::Device;
use style::dom::OpaqueNode;
use style::invalidation::element::restyle_hints::RestyleHint;
use style::properties::style_structs::Font;
use style::properties::{ComputedValues, PropertyId};
use style::selector_parser::{PseudoElement, RestyleDamage, Snapshot};
use style::str::char_is_whitespace;
use style::stylesheets::{DocumentStyleSheet, Stylesheet};
use style::stylist::Stylist;
#[cfg(debug_assertions)]
use style::thread_state::{self, ThreadState};
use style::values::computed::Overflow;
use style_traits::CSSPixel;
use uuid::Uuid;
use webrender_api::units::{DeviceIntSize, LayoutPoint, LayoutVector2D};
use webrender_api::{ExternalScrollId, ImageKey};

pub trait GenericLayoutDataTrait: Any + MallocSizeOfTrait + Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
}

pub trait LayoutDataTrait: GenericLayoutDataTrait + Default {}
pub type GenericLayoutData = dyn GenericLayoutDataTrait;

#[derive(Default, MallocSizeOf)]
pub struct StyleData {
    /// Data that the style system associates with a node. When the
    /// style system is being used standalone, this is all that hangs
    /// off the node. This must be first to permit the various
    /// transmutations between ElementData and PersistentLayoutData.
    pub element_data: ElementDataWrapper,

    /// Information needed during parallel traversals.
    pub parallel: DomParallelInfo,
}

/// Information that we need stored in each DOM node.
#[derive(Default, MallocSizeOf)]
pub struct DomParallelInfo {
    /// The number of children remaining to process during bottom-up traversal.
    pub children_to_process: AtomicIsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutNodeType {
    Element(LayoutElementType),
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutElementType {
    Element,
    HTMLBodyElement,
    HTMLBRElement,
    HTMLCanvasElement,
    HTMLHtmlElement,
    HTMLIFrameElement,
    HTMLImageElement,
    HTMLInputElement,
    HTMLMediaElement,
    HTMLObjectElement,
    HTMLOptGroupElement,
    HTMLOptionElement,
    HTMLParagraphElement,
    HTMLPreElement,
    HTMLSelectElement,
    HTMLTableCellElement,
    HTMLTableColElement,
    HTMLTableElement,
    HTMLTableRowElement,
    HTMLTableSectionElement,
    HTMLTextAreaElement,
    SVGImageElement,
    SVGSVGElement,
}

/// A selection shared between script and layout. This selection is managed by the DOM
/// node that maintains it, and can be modified from script. Once modified, layout is
/// expected to reflect the new selection visual on the next display list update.
#[derive(Clone, Debug, Default, MallocSizeOf, PartialEq)]
pub struct ScriptSelection {
    /// The range of this selection in the DOM node that manages it.
    pub range: TextByteRange,
    /// The character range of this selection in the DOM node that manages it.
    pub character_range: Range<usize>,
    /// Whether or not this selection is enabled. Selections may be disabled
    /// when their node loses focus.
    pub enabled: bool,
}

pub type SharedSelection = Arc<AtomicRefCell<ScriptSelection>>;
pub struct HTMLCanvasData {
    pub image_key: Option<ImageKey>,
    pub width: u32,
    pub height: u32,
}

pub struct SVGElementData<'dom> {
    /// The SVG's XML source represented as a base64 encoded `data:` url.
    pub source: Option<Result<ServoUrl, ()>>,
    pub width: Option<&'dom AttrValue>,
    pub height: Option<&'dom AttrValue>,
    pub svg_id: Uuid,
    pub view_box: Option<&'dom AttrValue>,
}

impl SVGElementData<'_> {
    pub fn ratio_from_view_box(&self) -> Option<f32> {
        let mut iter = self.view_box?.chars();
        let _min_x = parse_integer(&mut iter).ok()?;
        let _min_y = parse_integer(&mut iter).ok()?;

        let width = parse_unsigned_integer(&mut iter).ok()?;
        if width == 0 {
            return None;
        }

        let height = parse_unsigned_integer(&mut iter).ok()?;
        if height == 0 {
            return None;
        }

        let mut iter = iter.skip_while(|c| char_is_whitespace(*c));
        iter.next().is_none().then(|| width as f32 / height as f32)
    }
}

/// The address of a node known to be valid. These are sent from script to layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TrustedNodeAddress(pub *const c_void);

#[expect(unsafe_code)]
unsafe impl Send for TrustedNodeAddress {}

/// Whether the pending image needs to be fetched or is waiting on an existing fetch.
#[derive(Debug)]
pub enum PendingImageState {
    Unrequested(ServoUrl),
    PendingResponse,
}

/// The destination in layout where an image is needed.
#[derive(Debug, MallocSizeOf)]
pub enum LayoutImageDestination {
    BoxTreeConstruction,
    DisplayListBuilding,
}

/// The data associated with an image that is not yet present in the image cache.
/// Used by the script thread to hold on to DOM elements that need to be repainted
/// when an image fetch is complete.
#[derive(Debug)]
pub struct PendingImage {
    pub state: PendingImageState,
    pub node: UntrustedNodeAddress,
    pub id: PendingImageId,
    pub origin: ImmutableOrigin,
    pub destination: LayoutImageDestination,
    pub is_internal_request: InternalRequest,
}

/// A data structure to tarck vector image that are fully loaded (i.e has a parsed SVG
/// tree) but not yet rasterized to the size needed by layout. The rasterization is
/// happening in the image cache.
#[derive(Debug)]
pub struct PendingRasterizationImage {
    pub node: UntrustedNodeAddress,
    pub id: PendingImageId,
    pub size: DeviceIntSize,
}

#[derive(Clone, Copy, Debug, MallocSizeOf)]
pub struct MediaFrame {
    pub image_key: webrender_api::ImageKey,
    pub width: i32,
    pub height: i32,
}

pub struct MediaMetadata {
    pub width: u32,
    pub height: u32,
}

pub struct HTMLMediaData {
    pub current_frame: Option<MediaFrame>,
    pub metadata: Option<MediaMetadata>,
}

pub struct LayoutConfig {
    pub id: PipelineId,
    pub webview_id: WebViewId,
    pub url: ServoUrl,
    pub is_iframe: bool,
    pub script_chan: GenericSender<ScriptThreadMessage>,
    pub image_cache: Arc<dyn ImageCache>,
    pub font_context: Arc<FontContext>,
    pub time_profiler_chan: time::ProfilerChan,
    pub paint_api: CrossProcessPaintApi,
    pub viewport_details: ViewportDetails,
    pub user_stylesheets: Rc<Vec<DocumentStyleSheet>>,
    pub theme: Theme,
    pub embedder_chan: ScriptToEmbedderChan,
}

pub trait LayoutFactory: Send + Sync {
    fn create(&self, config: LayoutConfig) -> Box<dyn Layout>;
}

/// A read-only view of the layout data retained after a reflow.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugSnapshot {
    pub boxes: Vec<LayoutDebugBox>,
    pub fragments: Vec<LayoutDebugFragment>,
    /// Paged-root geometry retained by Pliego. Continuous Servo layouts omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_sequence: Option<LayoutDebugPageSequence>,
    /// Exact font payloads referenced by visible text fragments, sorted by resource ID.
    #[serde(default)]
    pub font_resources: Vec<LayoutDebugFontResource>,
    /// Font faces and variation coordinates referenced by visible text fragments, sorted by ID.
    #[serde(default)]
    pub font_instances: Vec<LayoutDebugFontInstance>,
    /// Fragment-level events retained from the display-list traversal, in traversal order.
    ///
    /// This is diagnostic capture data, not a canonical document scene. In particular,
    /// `fragment_id`, `spatial_node_id`, and `clip_id` are only meaningful within this
    /// snapshot.
    #[serde(default)]
    pub paint_events: Vec<LayoutDebugPaintEvent>,
    pub paint_epoch: u32,
    pub paint_content_width: f32,
    pub paint_content_height: f32,
    pub paint_scroll_node_count: usize,
    pub paintable: bool,
    pub contentful: bool,
    pub first_reflow: bool,
}

/// Page and fragmentainer geometry retained at the root-layout boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugPageSequence {
    pub pages: Vec<LayoutDebugPage>,
    /// Continuation tokens recorded at page boundaries, in page order.
    #[serde(default)]
    pub continuations: Vec<LayoutDebugPageContinuation>,
    /// Row-boundary break opportunities considered while retaining a laid-out table grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_breaks: Vec<LayoutDebugTableBreak>,
    /// Per-cell resume points retained when an oversized row continues across pages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_cell_continuations: Vec<LayoutDebugTableCellContinuation>,
    /// Synthetic header copies requested for continued table fragments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_group_repeats: Vec<LayoutDebugTableGroupRepeat>,
    /// Pagination warnings retained for document diagnostics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<LayoutDebugPageWarning>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LayoutDebugPageContinuation {
    /// Page after which layout resumes using `token`.
    pub page_index: usize,
    pub token: LayoutDebugContinuation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LayoutDebugContinuation {
    Block {
        next_child_index: usize,
        next_node: Option<u64>,
        #[serde(default)]
        forced: bool,
        #[serde(default)]
        break_inside_avoid: bool,
        #[serde(default)]
        retry_count: u8,
        resume_page_index: usize,
    },
    Inline {
        child_index: usize,
        next_line_index: usize,
        inline_item_index: usize,
        text_offset: usize,
        shaping_result_index: usize,
        resume_page_index: usize,
    },
    Table {
        child_index: usize,
        table_node: Option<u64>,
        row_group_index: Option<usize>,
        next_row_index: usize,
        resume_page_index: usize,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LayoutDebugTableBreak {
    pub page_index: usize,
    pub table_node: Option<u64>,
    pub row_group_index: Option<usize>,
    pub next_row_index: usize,
    pub constraint: LayoutDebugTableConstraint,
    pub retry_count: u8,
    pub retry_limit: u8,
    pub selected: bool,
    pub resume_page_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutDebugTableConstraint {
    #[default]
    Auto,
    ForcedBefore,
    ForcedAfter,
    AvoidRow,
    AvoidGroup,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LayoutDebugTableCellContinuation {
    pub page_index: usize,
    pub table_node: Option<u64>,
    pub row_group_index: Option<usize>,
    pub row_index: usize,
    pub cell_index: usize,
    pub next_fragment_index: Option<usize>,
    pub resume_page_index: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugTableGroupRepeat {
    pub page_index: usize,
    pub table_node: Option<u64>,
    pub header_tag_id: u64,
    pub row_group_index: usize,
    pub source_block_start: f32,
    pub target_block_start: f32,
    pub block_size: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutDebugTableGroupUnsupportedReason {
    MultipleFooters,
    FooterTooTallAfterHeader,
    Rowspan,
    CollapsedBorders,
    UnsupportedLayout,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LayoutDebugPageWarning {
    OversizedUnbreakable {
        child_index: usize,
        node: Option<u64>,
        block_size: f32,
        available_block_size: f32,
    },
    OversizedBreakInsideAvoid {
        child_index: usize,
        node: Option<u64>,
        block_size: f32,
        available_block_size: f32,
        retry_count: u8,
    },
    UnsupportedTableCaptionPagination {
        child_index: usize,
        node: Option<u64>,
    },
    UnsupportedTableGroupPagination {
        child_index: usize,
        table_node: Option<u64>,
        reason: LayoutDebugTableGroupUnsupportedReason,
    },
    UnsupportedTableRowspanPagination {
        child_index: usize,
        table_node: Option<u64>,
        first_row_index: usize,
        end_row_index: usize,
        block_size: f32,
        available_block_size: f32,
        forced_break_inside: bool,
    },
    OversizedTableCell {
        child_index: usize,
        table_node: Option<u64>,
        row_index: usize,
        cell_index: usize,
        fragment_index: usize,
        block_size: f32,
        available_block_size: f32,
    },
    OversizedTableRowBreakInsideAvoid {
        child_index: usize,
        table_node: Option<u64>,
        row_group_index: Option<usize>,
        row_index: usize,
        block_size: f32,
        available_block_size: f32,
        retry_count: u8,
        retry_limit: u8,
    },
    OversizedTableRowGroupBreakInsideAvoid {
        child_index: usize,
        table_node: Option<u64>,
        row_group_index: usize,
        block_size: f32,
        available_block_size: f32,
        retry_count: u8,
        retry_limit: u8,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugPage {
    pub index: usize,
    pub width: f32,
    pub height: f32,
    pub margin_top: f32,
    pub margin_right: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    pub available_inline_size: f32,
    pub available_block_size: f32,
}

/// Link semantics captured from the live DOM at the same boundary as a layout debug snapshot.
///
/// `tag_id` is a process-local join key for fragments in that snapshot. It is not a stable source
/// identifier and must not be included in deterministic scene hashes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugLink {
    pub tag_id: u64,
    /// The fully resolved URL after applying the document's effective base URL.
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugBox {
    pub depth: usize,
    pub kind: String,
    pub tag_id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugFragment {
    pub depth: usize,
    pub kind: String,
    pub rect: Option<LayoutDebugRect>,
    pub tag_id: Option<u64>,
    /// A capture-local join key for entries in [`LayoutDebugSnapshot::paint_events`].
    /// It is deliberately excluded from deterministic scene identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_fragment_id: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_run: Option<LayoutDebugTextRun>,
    /// The resolved source URL retained by laid-out image fragments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// Runtime join key for an in-process Canvas command snapshot. It is diagnostic-only and must
    /// never enter canonical scene identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_image_key: Option<LayoutDebugCanvasImageKey>,
    /// Renderer-neutral vector content retained from the already parsed SVG image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_image: Option<VectorImageSnapshot>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugCanvasImageKey {
    pub namespace: u32,
    pub key: u32,
}

/// A fragment-level event observed by the real display-list paint traversal.
///
/// One event may expand to several WebRender primitives. The event stream preserves the
/// traversal's ordering and scope without retaining or decoding a `BuiltDisplayList`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugPaintEvent {
    /// Monotonically increasing position in this snapshot's paint stream.
    pub sequence: usize,
    /// The paint phase observed by the display-list traversal.
    pub kind: String,
    /// A capture-local fragment identifier, when the event belongs to a fragment.
    pub fragment_id: Option<usize>,
    /// The process-local DOM/pseudo-element join key already used by debug fragments.
    pub tag_id: Option<u64>,
    /// The Servo scroll-tree node active for this event.
    pub spatial_node_id: usize,
    /// The Servo clip-store entry active for this event; `None` means no clip.
    pub clip_id: Option<usize>,
    /// Solid table-border rectangles owned by this event's fragment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_borders: Vec<LayoutDebugTableBorder>,
    /// Axis-aligned solid rectangles retained at their exact display-list paint position.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paint_rects: Vec<LayoutDebugPaintRect>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugPaintRect {
    pub rect: LayoutDebugRect,
    pub color: LayoutDebugColor,
    pub kind: LayoutDebugPaintRectKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutDebugPaintRectKind {
    Background,
    Border,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugTableBorder {
    pub rect: LayoutDebugRect,
    pub color: LayoutDebugColor,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Default for LayoutDebugColor {
    fn default() -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugTextRun {
    /// Text retained after layout whitespace processing and text transformation.
    pub text: String,
    /// Stable content-derived identity for the exact face and variations used by this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_instance_id: Option<String>,
    /// Diagnostic compatibility identifier. This is not a stable resource identity.
    pub font_identifier: FontIdentifier,
    /// CSS fallback chain requested by the fragment, in resolution order.
    pub requested_families: Vec<String>,
    /// Family declared by the selected `@font-face`, when the selected font came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_family: Option<String>,
    pub font_size: f32,
    /// Resolved sRGB text color used by the display-list paint traversal.
    #[serde(default)]
    pub color: LayoutDebugColor,
    /// Positioned glyphs, including retained whitespace slices.
    pub glyphs: Vec<LayoutDebugGlyph>,
}

/// Owned exact bytes for a font file used by the retained layout snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugFontResource {
    /// Content address in `sha256:<lowercase hex>` form.
    pub resource: String,
    /// Exact post-sanitization or local-file bytes encoded with standard padded base64.
    pub bytes_base64: String,
}

/// A stable font face and variation-coordinate instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugFontInstance {
    /// SHA-256 identity derived from the resource digest, face index, and variations.
    pub id: String,
    /// Content-addressed entry in [`LayoutDebugSnapshot::font_resources`].
    pub resource: String,
    pub face_index: u32,
    pub variations: Vec<LayoutDebugFontVariation>,
    #[serde(default)]
    pub synthetic_bold: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugFontVariation {
    pub tag: u32,
    pub value: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugGlyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    /// Effective pen advance, including word-justification adjustment.
    pub advance: f32,
    /// Exact UTF-8 byte range in [`LayoutDebugTextRun::text`] for this glyph's
    /// shaping cluster. Multiple glyphs can share a range. `None` explicitly
    /// denotes that Servo could not retain or validate the source cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_range: Option<LayoutDebugUtf8Range>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutDebugUtf8Range {
    pub start: usize,
    pub end: usize,
}

pub trait Layout {
    /// Get a reference to this Layout's Stylo `Device` used to handle media queries and
    /// resolve font metrics.
    fn device(&self) -> &Device;

    /// Set the theme on this [`Layout`]'s [`Device`]. The caller should also trigger a
    /// new layout when this happens, though it can happen later. Returns `true` if the
    /// [`Theme`] actually changed or `false` otherwise.
    fn set_theme(&mut self, theme: Theme) -> bool;

    /// Set the [`ViewportDetails`] on this [`Layout`]'s [`Device`]. The caller should also
    /// trigger a new layout when this happens, though it can happen later. Returns `true`
    /// if the [`ViewportDetails`] actually changed or `false` otherwise.
    fn set_viewport_details(&mut self, viewport_details: ViewportDetails) -> bool;

    /// Add a stylesheet to this Layout. This will add it to the Layout's `Stylist` as well as
    /// loading all web fonts defined in the stylesheet. The second stylesheet is the insertion
    /// point (if it exists, the sheet needs to be inserted before it).
    fn add_stylesheet(
        &mut self,
        stylesheet: ServoArc<Stylesheet>,
        before_stylsheet: Option<ServoArc<Stylesheet>>,
    );

    /// Inform the layout that its ScriptThread is about to exit.
    fn exit_now(&mut self);

    /// Requests that layout measure its memory usage. The resulting reports are sent back
    /// via the supplied channel.
    fn collect_reports(&self, reports: &mut Vec<Report>, ops: &mut MallocSizeOfOps);

    /// Sets quirks mode for the document, causing the quirks mode stylesheet to be used.
    fn set_quirks_mode(&mut self, quirks_mode: QuirksMode);

    /// Removes a stylesheet from the Layout.
    fn remove_stylesheet(&mut self, stylesheet: ServoArc<Stylesheet>);

    /// Removes an image from the Layout image resolver cache.
    fn remove_cached_image(&mut self, image_url: &ServoUrl);

    /// Requests a reflow.
    fn reflow(&mut self, reflow_request: ReflowRequest) -> Option<ReflowResult>;

    /// Do not request a reflow, but ensure that any previous reflow completes building a stacking
    /// context tree so that it is ready to query the final size of any elements in script.
    fn ensure_stacking_context_tree(&self, viewport_details: ViewportDetails);

    /// Return a snapshot of already-cached layout data without triggering layout.
    fn debug_snapshot(&self) -> Option<LayoutDebugSnapshot>;

    /// Tells layout that script has added some paint worklet modules.
    fn register_paint_worklet_modules(
        &mut self,
        name: Atom,
        properties: Vec<Atom>,
        painter: Box<dyn Painter>,
    );

    /// Set the scroll states of this layout after a `Paint` scroll.
    fn set_scroll_offsets_from_renderer(
        &mut self,
        scroll_states: &FxHashMap<ExternalScrollId, LayoutVector2D>,
    );

    /// Get the scroll offset of the given scroll node with id of [`ExternalScrollId`] or `None` if it does
    /// not exist in the tree.
    fn scroll_offset(&self, id: ExternalScrollId) -> Option<LayoutVector2D>;

    /// Returns true if this layout needs to produce a new display list for rendering updates.
    fn needs_new_display_list(&self) -> bool;

    /// Marks that this layout needs to produce a new display list for rendering updates.
    fn set_needs_new_display_list(&self);

    /// Returns the [`NodeRenderingType`] for this node and pseudo. This is used to determine
    /// if a node is being rendered, delegating its rendering, or not being rendered at all.
    fn node_rendering_type(
        &self,
        node: TrustedNodeAddress,
        pseudo: Option<PseudoElement>,
    ) -> NodeRenderingType;

    fn query_containing_block(&self, node: TrustedNodeAddress) -> Option<UntrustedNodeAddress>;
    fn query_containing_block_is_descendant(
        &self,
        root: TrustedNodeAddress,
        possible_descendant: TrustedNodeAddress,
    ) -> bool;
    fn query_padding(&self, node: TrustedNodeAddress) -> Option<PhysicalSides>;
    fn query_box_area(
        &self,
        node: TrustedNodeAddress,
        area: BoxAreaType,
        exclude_transform_and_inline: bool,
    ) -> Option<Rect<Au, CSSPixel>>;
    fn query_box_areas(&self, node: TrustedNodeAddress, area: BoxAreaType) -> CSSPixelRectVec;
    fn query_client_rect(&self, node: TrustedNodeAddress) -> Rect<i32, CSSPixel>;
    fn query_current_css_zoom(&self, node: TrustedNodeAddress) -> f32;
    fn query_element_inner_outer_text(&self, node: TrustedNodeAddress) -> String;
    fn query_offset_parent(&self, node: TrustedNodeAddress) -> OffsetParentResponse;
    /// Query the scroll container for the given node. If node is `None`, the scroll container for
    /// the viewport is returned.
    fn query_scroll_container(
        &self,
        node: Option<TrustedNodeAddress>,
        flags: ScrollContainerQueryFlags,
    ) -> Option<ScrollContainerResponse>;
    fn query_resolved_style(
        &self,
        node: TrustedNodeAddress,
        pseudo: Option<PseudoElement>,
        property_id: PropertyId,
        animations: DocumentAnimationSet,
        animation_timeline_value: f64,
    ) -> String;
    fn query_resolved_font_style(
        &self,
        node: TrustedNodeAddress,
        value: &str,
        animations: DocumentAnimationSet,
        animation_timeline_value: f64,
    ) -> Option<ServoArc<Font>>;
    fn query_scrolling_area(&self, node: Option<TrustedNodeAddress>) -> Rect<i32, CSSPixel>;
    /// Find the character offset of the point in the given node, if it has text content.
    fn query_text_index(
        &self,
        node: TrustedNodeAddress,
        point: Point2D<Au, CSSPixel>,
    ) -> Option<usize>;
    fn query_elements_from_point(&self, point: LayoutPoint) -> Vec<ElementsFromPointResult>;
    fn query_effective_overflow(&self, node: TrustedNodeAddress) -> Option<AxesOverflow>;
    fn stylist_mut(&mut self) -> &mut Stylist;

    /// Set whether the accessibility tree should be constructed for this Layout.
    /// This should be called by the embedder when accessibility is requested by the user.
    fn set_accessibility_active(&self, enabled: bool, epoch: Epoch);

    /// Returns whether accessibility is active for this Layout.
    fn accessibility_active(&self) -> bool;

    /// Whether the accessibility tree needs updating. This is set to true when
    /// - accessibility is activated; or
    /// - a page is loaded after accesibility is activated.
    ///
    /// In future, this should be set to true if DOM or style have changed in a way that
    /// impacts the accessibility tree.
    ///
    /// Checked in can_skip_reflow_request_entirely(), as a dirty accessibility tree
    /// should force a reflow, and handle_reflow() to determine whether to update the
    /// accessibility tree during reflow.
    fn needs_accessibility_update(&self) -> bool;

    /// See [Self::needs_accessibility_update()].
    fn set_needs_accessibility_update(&self);
}

/// This trait is part of `layout_api` because it depends on both `script_traits`
/// and also `LayoutFactory` from this crate. If it was in `script_traits` there would be a
/// circular dependency.
pub trait ScriptThreadFactory {
    /// Create a `ScriptThread`.
    fn create(
        state: InitialScriptState,
        layout_factory: Arc<dyn LayoutFactory>,
        image_cache_factory: Arc<dyn ImageCacheFactory>,
        background_hang_monitor_register: Box<dyn BackgroundHangMonitorRegister>,
    ) -> JoinHandle<()>;
}

/// Type of the area of CSS box for query.
/// See <https://www.w3.org/TR/css-box-3/#box-model>.
#[derive(Copy, Clone)]
pub enum BoxAreaType {
    Content,
    Padding,
    Border,
}

pub type CSSPixelRectVec = Vec<Rect<Au, CSSPixel>>;

/// Whether or not this node is being rendered or delegates rendering according
/// to the HTML standard.
#[derive(Copy, Clone)]
pub enum NodeRenderingType {
    /// <https://html.spec.whatwg.org/multipage/#being-rendered>
    Rendered,
    /// <https://html.spec.whatwg.org/multipage/#delegating-its-rendering-to-its-children>
    DelegatesRendering,
    /// If neither of the other two cases are true, this is. The node is effectively not
    /// taking part in the final layout of the page.
    NotRendered,
}

#[derive(Default)]
pub struct PhysicalSides {
    pub left: Au,
    pub top: Au,
    pub right: Au,
    pub bottom: Au,
}

#[derive(Clone, Default)]
pub struct OffsetParentResponse {
    pub node_address: Option<UntrustedNodeAddress>,
    pub rect: Rect<Au, CSSPixel>,
}

bitflags! {
    #[derive(PartialEq)]
    pub struct ScrollContainerQueryFlags: u8 {
        /// Whether or not this query is for the purposes of a `scrollParent` layout query.
        const ForScrollParent = 1 << 0;
        /// Whether or not to consider the original element's scroll box for the return value.
        const Inclusive = 1 << 1;
    }
}

#[derive(Clone, Copy, Debug, MallocSizeOf)]
pub struct AxesOverflow {
    pub x: Overflow,
    pub y: Overflow,
}

impl Default for AxesOverflow {
    fn default() -> Self {
        Self {
            x: Overflow::Visible,
            y: Overflow::Visible,
        }
    }
}

impl From<&ComputedValues> for AxesOverflow {
    fn from(style: &ComputedValues) -> Self {
        Self {
            x: style.clone_overflow_x(),
            y: style.clone_overflow_y(),
        }
    }
}

impl AxesOverflow {
    pub fn to_scrollable(&self) -> Self {
        Self {
            x: self.x.to_scrollable(),
            y: self.y.to_scrollable(),
        }
    }

    /// Whether or not the `overflow` value establishes a scroll container.
    pub fn establishes_scroll_container(&self) -> bool {
        // Checking one axis suffices, because the computed value ensures that
        // either both axes are scrollable, or none is scrollable.
        self.x.is_scrollable()
    }
}

#[derive(Clone)]
pub enum ScrollContainerResponse {
    Viewport(AxesOverflow),
    Element(UntrustedNodeAddress, AxesOverflow),
}

#[derive(Debug, PartialEq)]
pub enum QueryMsg {
    BoxArea,
    BoxAreas,
    ClientRectQuery,
    CurrentCSSZoomQuery,
    EffectiveOverflow,
    ElementInnerOuterTextQuery,
    ElementsFromPoint,
    InnerWindowDimensionsQuery,
    NodesFromPointQuery,
    OffsetParentQuery,
    ScrollParentQuery,
    ResolvedFontStyleQuery,
    /// A style query, with an optional [`PropertyId`], used to limit the phases
    /// of layout run before the query.
    ResolvedStyleQuery(PropertyId),
    ScrollingAreaOrOffsetQuery,
    StyleQuery,
    TextIndexQuery,
    PaddingQuery,
    FlushForUpdateTheRenderingQuery,
}

/// The goal of a reflow request.
///
/// Please do not add any other types of reflows. In general, all reflow should
/// go through the *update the rendering* step of the HTML specification. Exceptions
/// should have careful review.
#[derive(Debug, PartialEq)]
pub enum ReflowGoal {
    /// A reflow has been requesting by the *update the rendering* step of the HTML
    /// event loop. This nominally driven by the display's VSync.
    UpdateTheRendering,

    /// Script has done a layout query and this reflow ensurs that layout is up-to-date
    /// with the latest changes to the DOM.
    LayoutQuery(QueryMsg),

    /// Tells layout about a single new scrolling offset from the script. The rest will
    /// remain untouched. Layout will forward whether the element is scrolled through
    /// [ReflowResult].
    UpdateScrollNode(ExternalScrollId, LayoutVector2D),
}

#[derive(Clone, Debug, MallocSizeOf)]
pub struct IFrameSize {
    pub browsing_context_id: BrowsingContextId,
    pub pipeline_id: PipelineId,
    pub viewport_details: ViewportDetails,
}

pub type IFrameSizes = FxHashMap<BrowsingContextId, IFrameSize>;

bitflags! {
    /// Conditions which cause a [`Document`] to need to be restyled during reflow, which
    /// might cause the rest of layout to happen as well.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct RestyleReason: u16 {
        const StylesheetsChanged = 1 << 0;
        const DOMChanged = 1 << 1;
        const PendingRestyles = 1 << 2;
        const HighlightedDOMNodeChanged = 1 << 3;
        const ThemeChanged = 1 << 4;
        const ViewportChanged = 1 << 5;
        const PaintWorkletLoaded = 1 << 6;
    }
}

malloc_size_of_is_0!(RestyleReason);

impl RestyleReason {
    pub fn needs_restyle(&self) -> bool {
        !self.is_empty()
    }
}

/// Information derived from a layout pass that needs to be returned to the script thread.
#[derive(Debug, Default)]
pub struct ReflowResult {
    /// The phases that were run during this reflow.
    pub reflow_phases_run: ReflowPhasesRun,
    pub reflow_statistics: ReflowStatistics,
    /// The list of images that were encountered that are in progress.
    pub pending_images: Vec<PendingImage>,
    /// The list of vector images that were encountered that still need to be rasterized.
    pub pending_rasterization_images: Vec<PendingRasterizationImage>,
    /// The list of `SVGSVGElement`s encountered in the DOM that need to be serialized.
    /// This is needed to support inline SVGs as the serialization needs to happen on
    /// the script thread.
    pub pending_svg_elements_for_serialization: Vec<UntrustedNodeAddress>,
    /// The list of iframes in this layout and their sizes, used in order
    /// to communicate them with the Constellation and also the `Window`
    /// element of their content pages. Returning None if incremental reflow
    /// finished before reaching this stage of the layout. I.e., no update
    /// required.
    pub iframe_sizes: Option<IFrameSizes>,
}

bitflags! {
    /// The phases of reflow that were run when processing a reflow in layout.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct ReflowPhasesRun: u8 {
        const RanLayout = 1 << 0;
        const BuiltStackingContextTree = 1 << 2;
        const BuiltDisplayList = 1 << 3;
        const UpdatedScrollNodeOffset = 1 << 4;
        /// Image data for a WebRender image key has been updated, without necessarily
        /// updating style or layout. This is used when updating canvas contents and
        /// progressing to a new animated image frame.
        const UpdatedImageData = 1 << 5;
        const UpdatedAccessibilityTree = 1 << 6;
    }
}

impl ReflowPhasesRun {
    pub fn needs_frame(&self) -> bool {
        self.intersects(
            Self::BuiltDisplayList | Self::UpdatedScrollNodeOffset | Self::UpdatedImageData,
        )
    }
}

#[derive(Debug, Default)]
pub struct ReflowStatistics {
    /// A count of the number of fragments that have been completely rebuilt.
    pub rebuilt_fragment_count: u32,
    /// A count of the number of fragments that are reused, but have had their style change.
    pub restyle_fragment_count: u32,
    /// A count of the number of fragments that are reused, but may have had some descendant
    /// fragment change.
    pub only_descendants_changed_count: u32,
}

/// Information needed for a script-initiated reflow that requires a restyle
/// and reconstruction of box and fragment trees.
#[derive(Debug)]
pub struct ReflowRequestRestyle {
    /// Whether or not (and for what reasons) restyle needs to happen.
    pub reason: RestyleReason,
    /// The dirty root from which to restyle.
    pub dirty_root: Option<TrustedNodeAddress>,
    /// Whether the document's stylesheets have changed since the last script reflow.
    pub stylesheets_changed: bool,
    /// Restyle snapshot map.
    pub pending_restyles: Vec<(TrustedNodeAddress, PendingRestyle)>,
}

/// Information needed for a script-initiated reflow.
#[derive(Debug)]
pub struct ReflowRequest {
    /// The document node.
    pub document: TrustedNodeAddress,
    /// The current layout [`Epoch`] managed by the script thread.
    pub epoch: Epoch,
    /// If a restyle is necessary, all of the informatio needed to do that restyle.
    pub restyle: Option<ReflowRequestRestyle>,
    /// The current [`ViewportDetails`] to use for this reflow.
    pub viewport_details: ViewportDetails,
    /// The goal of this reflow.
    pub reflow_goal: ReflowGoal,
    /// The current window origin
    pub origin: ImmutableOrigin,
    /// The current animation timeline value.
    pub animation_timeline_value: f64,
    /// The set of animations for this document.
    pub animations: DocumentAnimationSet,
    /// An [`AnimatingImages`] struct used to track images that are animating.
    pub animating_images: Arc<RwLock<AnimatingImages>>,
    /// The node highlighted by the devtools, if any
    pub highlighted_dom_node: Option<OpaqueNode>,
    /// The current font context.
    pub document_context: WebFontDocumentContext,
    /// Damage to the accessibility tree from DOM mutations.
    pub accessibility_damage: Option<Vec<(TrustedNodeAddress, AccessibilityDamage)>>,
    /// Nodes which were removed from the DOM tree since the last reflow, which were rooted in
    /// [`AccessibilityData`]. Only set if [`pref::expensive_accessibility_test_assertions_enabled`]
    /// is set.
    pub rooted_nodes_for_accessibility_integrity_check: Option<FxHashSet<OpaqueNode>>,
}

impl ReflowRequest {
    pub fn stylesheets_changed(&self) -> bool {
        self.restyle
            .as_ref()
            .is_some_and(|restyle| restyle.stylesheets_changed)
    }
}

/// A pending restyle.
#[derive(Debug, Default, MallocSizeOf)]
pub struct PendingRestyle {
    /// If this element had a state or attribute change since the last restyle, track
    /// the original condition of the element.
    pub snapshot: Option<Snapshot>,

    /// Any explicit restyles hints that have been accumulated for this element.
    pub hint: RestyleHint,

    /// Any explicit restyles damage that have been accumulated for this element.
    pub damage: RestyleDamage,
}

const FRAGMENT_TYPE_MASK: usize = 0b11;

/// The type of fragment that a scroll root is created for.
///
/// This can only ever grow to maximum 4 entries. That's because we cram the value of this enum
/// into the lower 2 bits of the `OpaqueNodeId`, which otherwise contains a 32-bit-aligned
/// or 64-bit-aligned heap address depending on the machine.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, MallocSizeOf, PartialEq, Serialize)]
#[repr(u8)]
pub enum FragmentType {
    /// A StackingContext for the fragment body itself.
    FragmentBody = 0,
    /// A StackingContext created to contain ::before pseudo-element content.
    BeforePseudoContent = 1,
    /// A StackingContext created to contain ::after pseudo-element content.
    AfterPseudoContent = 2,
    /// A StackingContext created to contain ::marker pseudo-element content.
    MarkerPseudoContent = 3,
}

impl From<Option<PseudoElement>> for FragmentType {
    fn from(value: Option<PseudoElement>) -> Self {
        match value {
            Some(PseudoElement::After) => FragmentType::AfterPseudoContent,
            Some(PseudoElement::Before) => FragmentType::BeforePseudoContent,
            Some(PseudoElement::Marker) => FragmentType::MarkerPseudoContent,
            None |
            Some(
                PseudoElement::Selection |
                PseudoElement::FirstLetter |
                PseudoElement::Backdrop |
                PseudoElement::DetailsContent |
                PseudoElement::ColorSwatch |
                PseudoElement::FileSelectorButton |
                PseudoElement::Placeholder |
                PseudoElement::SliderFill |
                PseudoElement::SliderThumb |
                PseudoElement::SliderTrack |
                PseudoElement::ServoTextControlInnerContainer |
                PseudoElement::ServoTextControlInnerEditor |
                PseudoElement::ServoAnonymousBox |
                PseudoElement::ServoAnonymousTable |
                PseudoElement::ServoAnonymousTableCell |
                PseudoElement::ServoAnonymousTableRow |
                PseudoElement::ServoTableGrid |
                PseudoElement::ServoTableWrapper,
            ) => FragmentType::FragmentBody,
        }
    }
}

pub fn combine_id_with_fragment_type(id: usize, fragment_type: FragmentType) -> u64 {
    assert_eq!(
        id & FRAGMENT_TYPE_MASK,
        0,
        "fragment IDs reserve their low two bits for FragmentType",
    );
    (id as u64) | (fragment_type as u64)
}

pub fn node_id_from_scroll_id(id: usize) -> usize {
    id & !FRAGMENT_TYPE_MASK
}

#[derive(Clone, Debug, MallocSizeOf)]
pub struct ImageAnimationState {
    #[conditional_malloc_size_of]
    pub image: Arc<RasterImage>,
    pub active_frame: usize,
    frame_start_time: f64,

    /// The number of loops that have fully completed in this [`ImageAnimationState`].
    /// If this is greater than or equal to the maximum number of loops in the
    /// [`RasterImage`], then the animation has ended. If it is `None`, then the image
    /// will loop infinitely.
    pub completed_loops: Option<u32>,
}

impl ImageAnimationState {
    pub fn new(image: Arc<RasterImage>, last_update_time: f64) -> Self {
        let completd_loops = match &image.loop_count {
            None => unreachable!("Loop count of an animated Image should never be None"),
            Some(repeat) if Repeat::Infinite == *repeat => None,
            _ => Some(0),
        };

        Self {
            image,
            active_frame: 0,
            frame_start_time: last_update_time,
            completed_loops: completd_loops,
        }
    }

    pub fn image_key(&self) -> Option<ImageKey> {
        self.image.id
    }

    pub fn duration_to_next_frame(&self, now: f64) -> Option<Duration> {
        if self.is_finished() {
            return None;
        }
        let frame_delay = self
            .image
            .frames
            .get(self.active_frame)
            .expect("Image frame should always be valid")
            .delay
            .unwrap_or_default();

        let time_since_frame_start = (now - self.frame_start_time).max(0.0) * 1000.0;
        let time_since_frame_start = Duration::from_secs_f64(time_since_frame_start);
        Some(frame_delay - time_since_frame_start.min(frame_delay))
    }

    /// check whether image active frame need to be updated given current time,
    /// return true if there are image that need to be updated.
    /// false otherwise.
    pub fn update_frame_for_animation_timeline_value(&mut self, now: f64) -> bool {
        if self.image.frames.len() <= 1 || self.is_finished() {
            return false;
        }
        let time_interval_since_last_update = now - self.frame_start_time;
        let mut remain_time_interval = time_interval_since_last_update -
            self.image
                .frames
                .get(self.active_frame)
                .unwrap()
                .delay()
                .unwrap()
                .as_secs_f64();
        let mut next_active_frame_id = self.active_frame;

        let frame_count = self.image.frames.len();
        while remain_time_interval > 0.0 {
            next_active_frame_id = (next_active_frame_id + 1) % frame_count;

            // If the next active frame is 0, this means the animation is about to loop.
            if next_active_frame_id == 0 {
                self.advance_completed_loops();

                // If we have just finished the animation, advance to the final frame if
                // necessary and stop walking through frames.
                if self.is_finished() {
                    if self.active_frame == frame_count - 1 {
                        return false;
                    }
                    self.active_frame = frame_count - 1;
                    self.frame_start_time = now;
                    return true;
                }
            }

            remain_time_interval -= self
                .image
                .frames
                .get(next_active_frame_id)
                .unwrap()
                .delay()
                .unwrap()
                .as_secs_f64();
        }
        if self.active_frame == next_active_frame_id {
            return false;
        }
        self.active_frame = next_active_frame_id;
        self.frame_start_time = now;
        true
    }

    /// Whether or not this animation has finished looping and has reached its final frame.
    fn is_finished(&self) -> bool {
        let Some(Repeat::Finite(maximum_loops)) = self.image.loop_count.as_ref() else {
            return false;
        };
        self.completed_loops
            .is_some_and(|completed_loops| completed_loops >= maximum_loops.get())
    }

    /// If this animation has a finite number of loops, advance the count of completed loops.
    fn advance_completed_loops(&mut self) {
        if let Some(completed_loops) = self.completed_loops.as_mut() {
            *completed_loops += 1;
        }
    }
}

/// Describe an item that matched a hit-test query.
#[derive(Debug)]
pub struct ElementsFromPointResult {
    /// An [`OpaqueNode`] that contains a pointer to the node hit by
    /// this hit test result.
    pub node: OpaqueNode,
    /// The [`Point2D`] of the original query point relative to the
    /// node fragment rectangle.
    pub point_in_target: Point2D<f32, CSSPixel>,
    /// The [`Cursor`] that's defined on the item that is hit by this
    /// hit test result.
    pub cursor: Cursor,
}

#[derive(Debug, Default, MallocSizeOf)]
pub struct AnimatingImages {
    /// A map from the [`OpaqueNode`] to the state of an animating image. This is used
    /// to update frames in script and to track newly animating nodes.
    pub node_to_state_map: FxHashMap<OpaqueNode, ImageAnimationState>,
    /// Whether or not this map has changed during a layout. This is used by script to
    /// trigger future animation updates.
    pub dirty: bool,
}

impl AnimatingImages {
    pub fn maybe_insert_or_update(
        &mut self,
        node: OpaqueNode,
        image: Arc<RasterImage>,
        current_timeline_value: f64,
    ) {
        let entry = self.node_to_state_map.entry(node).or_insert_with(|| {
            self.dirty = true;
            ImageAnimationState::new(image.clone(), current_timeline_value)
        });

        // If the entry exists, but it is for a different image id, replace it as the image
        // has changed during this layout.
        if entry.image.id != image.id {
            self.dirty = true;
            *entry = ImageAnimationState::new(image.clone(), current_timeline_value);
        }
    }

    pub fn remove(&mut self, node: OpaqueNode) {
        if self.node_to_state_map.remove(&node).is_some() {
            self.dirty = true;
        }
    }

    /// Clear the dirty bit on this [`AnimatingImages`] and return the previous value.
    pub fn clear_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn is_empty(&self) -> bool {
        self.node_to_state_map.is_empty()
    }
}

struct ThreadStateRestorer;

impl ThreadStateRestorer {
    fn new() -> Self {
        #[cfg(debug_assertions)]
        {
            thread_state::exit(ThreadState::SCRIPT);
            thread_state::enter(ThreadState::LAYOUT);
        }
        Self
    }
}

impl Drop for ThreadStateRestorer {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        {
            thread_state::exit(ThreadState::LAYOUT);
            thread_state::enter(ThreadState::SCRIPT);
        }
    }
}

/// Set up the thread-local state to reflect that layout code is about to run,
/// then call the provided function.
/// This must be used when running code that will interact with the DOM tree
/// through types like `ServoLayoutNode`, `ServoLayoutElement`, and `LayoutDom`,
/// which have rules about how they must be used from layout worker threads.
pub fn with_layout_state<R>(f: impl FnOnce() -> R) -> R {
    let _guard = ThreadStateRestorer::new();
    f()
}

#[cfg(test)]
mod test {
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::time::Duration;

    use pixels::{CorsStatus, ImageFrame, ImageMetadata, PixelFormat, RasterImage, Repeat};
    use style::selector_parser::PseudoElement;

    use crate::{
        FRAGMENT_TYPE_MASK, FragmentType, ImageAnimationState, combine_id_with_fragment_type,
        node_id_from_scroll_id,
    };

    #[test]
    fn fragment_type_low_bits_are_stable_and_distinct() {
        let node_id = 0x1000;
        let cases = [
            (None, FragmentType::FragmentBody, 0, 0x1000),
            (
                Some(PseudoElement::Before),
                FragmentType::BeforePseudoContent,
                1,
                0x1001,
            ),
            (
                Some(PseudoElement::After),
                FragmentType::AfterPseudoContent,
                2,
                0x1002,
            ),
            (
                Some(PseudoElement::Marker),
                FragmentType::MarkerPseudoContent,
                3,
                0x1003,
            ),
        ];

        for (pseudo, expected_type, expected_low_bits, expected_id) in cases {
            assert_eq!(FragmentType::from(pseudo), expected_type);
            assert_eq!(expected_type as u8, expected_low_bits);
            assert_eq!(
                combine_id_with_fragment_type(node_id, expected_type),
                expected_id
            );
        }
    }

    #[test]
    fn non_fragment_pseudo_elements_map_to_the_fragment_body() {
        for pseudo in [
            PseudoElement::Selection,
            PseudoElement::FirstLetter,
            PseudoElement::Backdrop,
            PseudoElement::DetailsContent,
            PseudoElement::ColorSwatch,
            PseudoElement::FileSelectorButton,
            PseudoElement::Placeholder,
            PseudoElement::SliderFill,
            PseudoElement::SliderThumb,
            PseudoElement::SliderTrack,
            PseudoElement::ServoTextControlInnerContainer,
            PseudoElement::ServoTextControlInnerEditor,
            PseudoElement::ServoAnonymousBox,
            PseudoElement::ServoAnonymousTable,
            PseudoElement::ServoAnonymousTableCell,
            PseudoElement::ServoAnonymousTableRow,
            PseudoElement::ServoTableGrid,
            PseudoElement::ServoTableWrapper,
        ] {
            assert_eq!(FragmentType::from(Some(pseudo)), FragmentType::FragmentBody);
        }
    }

    #[test]
    fn fragment_type_encoding_rejects_every_reserved_low_bit_for_every_tag() {
        for low_bits in 1..=FRAGMENT_TYPE_MASK {
            let misaligned_node_id = 0x1000 | low_bits;

            for fragment_type in [
                FragmentType::FragmentBody,
                FragmentType::BeforePseudoContent,
                FragmentType::AfterPseudoContent,
                FragmentType::MarkerPseudoContent,
            ] {
                assert!(
                    std::panic::catch_unwind(|| {
                        combine_id_with_fragment_type(misaligned_node_id, fragment_type)
                    })
                    .is_err(),
                    "accepted node ID {misaligned_node_id:#x} for {fragment_type:?}",
                );
            }
        }
    }

    #[test]
    fn maximum_aligned_node_id_has_four_distinct_round_tripping_fragment_ids() {
        let node_id = usize::MAX & !FRAGMENT_TYPE_MASK;
        let cases = [
            (FragmentType::FragmentBody, 0usize),
            (FragmentType::BeforePseudoContent, 1),
            (FragmentType::AfterPseudoContent, 2),
            (FragmentType::MarkerPseudoContent, 3),
        ];
        let mut fragment_ids = Vec::new();

        for (fragment_type, expected_low_bits) in cases {
            let fragment_id = combine_id_with_fragment_type(node_id, fragment_type);
            assert_eq!(fragment_id, node_id as u64 | expected_low_bits as u64);
            assert_eq!(node_id_from_scroll_id(fragment_id as usize), node_id);
            fragment_ids.push(fragment_id);
        }

        fragment_ids.sort_unstable();
        fragment_ids.dedup();
        assert_eq!(fragment_ids.len(), 4);
    }

    #[test]
    fn test_animated_image_update() {
        let image_frames: Vec<ImageFrame> = std::iter::repeat_with(|| ImageFrame {
            delay: Some(Duration::from_millis(100)),
            byte_range: 0..1,
            width: 100,
            height: 100,
        })
        .take(10)
        .collect();
        let image = RasterImage {
            metadata: ImageMetadata {
                width: 100,
                height: 100,
            },
            format: PixelFormat::BGRA8,
            id: None,
            bytes: Arc::new(vec![1]),
            frames: image_frames,
            cors_status: CorsStatus::Unsafe,
            loop_count: Some(Repeat::Infinite),
            is_opaque: false,
        };
        let mut image_animation_state = ImageAnimationState::new(Arc::new(image), 0.0);

        assert_eq!(image_animation_state.active_frame, 0);
        assert_eq!(image_animation_state.frame_start_time, 0.0);
        assert_eq!(
            image_animation_state.update_frame_for_animation_timeline_value(0.101),
            true
        );
        assert_eq!(image_animation_state.active_frame, 1);
        assert_eq!(image_animation_state.frame_start_time, 0.101);
        assert_eq!(
            image_animation_state.update_frame_for_animation_timeline_value(0.116),
            false
        );
        assert_eq!(image_animation_state.active_frame, 1);
        assert_eq!(image_animation_state.frame_start_time, 0.101);
    }

    #[test]
    fn test_finite_image_repeat() {
        let image_frames: Vec<ImageFrame> = std::iter::repeat_with(|| ImageFrame {
            delay: Some(Duration::from_millis(100)),
            byte_range: 0..1,
            width: 100,
            height: 100,
        })
        .take(2)
        .collect();
        let image = RasterImage {
            metadata: ImageMetadata {
                width: 100,
                height: 100,
            },
            format: PixelFormat::BGRA8,
            id: None,
            bytes: Arc::new(vec![1]),
            frames: image_frames,
            cors_status: CorsStatus::Unsafe,
            loop_count: Some(Repeat::Finite(NonZeroU32::new(1).unwrap())),
            is_opaque: false,
        };
        let mut image_animation_state = ImageAnimationState::new(Arc::new(image), 0.0);

        assert_eq!(image_animation_state.active_frame, 0);
        assert_eq!(image_animation_state.frame_start_time, 0.0);
        assert_eq!(
            image_animation_state.update_frame_for_animation_timeline_value(0.101),
            true
        );
        assert_eq!(image_animation_state.active_frame, 1);
        assert_eq!(image_animation_state.frame_start_time, 0.101);
        assert_eq!(
            image_animation_state.update_frame_for_animation_timeline_value(0.202),
            false
        );
        assert_eq!(
            image_animation_state.update_frame_for_animation_timeline_value(0.303),
            false
        );

        assert_eq!(image_animation_state.active_frame, 1);
    }
}
