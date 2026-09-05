/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use embedder_traits::DocumentCanvasCaptureBinding;
use serde::{Deserialize, Serialize};
use servo_canvas::retained_canvas::{
    FreezeCanvasSnapshotsError, FrozenCanvasSnapshots, RetainedCanvasSnapshot,
};
use sha2::{Digest, Sha256};
use vello_cpu::kurbo::{BezPath, Shape};

use crate::hybrid_canvas::{
    CanvasDiagnostics, CanvasResource, HybridCanvasCapture, adapt_canvas, transcript_from_retained,
};
use crate::{
    Color, DocumentScene, FillRule, Glyph, Operation, OperationMeta, Page, Rect, Semantics, Size,
    Stroke,
};

const APP_UNITS_PER_CSS_PIXEL: f32 = 60.0;

fn app_units_to_f32_px(value: i32) -> f32 {
    value as f32 / APP_UNITS_PER_CSS_PIXEL
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SceneCapture {
    pub scene: DocumentScene,
    /// Fixed-point source authority for a future API 2 encoder.
    ///
    /// This ledger is deliberately absent from the API 1 scene/inspect serialization surface.
    #[serde(skip_serializing)]
    pub fixed_point_authority: CapturedFixedPointAuthority,
    #[serde(skip_serializing)]
    pub canvas_resources: Vec<CanvasResource>,
    #[serde(skip_serializing)]
    pub embedded_image_resources: Vec<CanvasResource>,
    pub canvas_diagnostics: Vec<CapturedCanvasDiagnostics>,
    pub font_resources: Vec<CapturedFontResource>,
    pub font_instances: Vec<CapturedFontInstance>,
    pub font_selections: Vec<CapturedFontSelection>,
    pub font_warnings: Vec<CapturedFontWarning>,
    pub unsupported_events: Vec<UnsupportedPaintEvent>,
    pub text_mapping_gaps: Vec<MissingTextMapping>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapturedFixedPointAuthority {
    /// One entry per retained page, in page order. Legacy diagnostic fixtures can lack authority;
    /// real paged layout always emits both source and geometry.
    pub pages: Vec<CapturedPageAuthority>,
    /// One entry per final scene page and operation, in the exact emitted order.
    ///
    /// `None` means that operation cannot be encoded from retained fixed-point authority. This
    /// ledger is built alongside final operations; it must never be reconstructed by matching the
    /// dense paint-event ledger after pagination.
    pub page_operations: Vec<Vec<Option<CapturedOperationAuthority>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapturedOperationAuthority {
    Text {
        font_size_app_units: i32,
        glyphs: Vec<CapturedGlyphAppUnits>,
    },
    /// Exact operation bounds. `Operation` supplies the discriminant; a fixed-point encoder must
    /// regenerate and validate rectangle path data from these bounds rather than trusting the API
    /// 1 f64 path data.
    Bounds(CapturedRectAppUnits),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapturedPageAuthority {
    pub index: usize,
    pub style_source: Option<CapturedPageStyleSource>,
    pub app_units: Option<CapturedPageAppUnits>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
pub enum CapturedPageStyleSource {
    RequestDefaults,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapturedPageAppUnits {
    pub width: i32,
    pub height: i32,
    pub margin_top: i32,
    pub margin_right: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub available_inline_size: i32,
    pub available_block_size: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapturedRectAppUnits {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapturedGlyphAppUnits {
    pub x: i32,
    pub y: i32,
    pub advance: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CapturedCanvasDiagnostics {
    pub sequences: Vec<usize>,
    pub diagnostics: CanvasDiagnostics,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapturedCanvasImageKey {
    pub namespace: u32,
    pub key: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CapturedFontResource {
    pub resource: String,
    pub bytes_base64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CapturedFontInstance {
    pub id: String,
    pub resource: String,
    pub face_index: u32,
    pub variations: Vec<CapturedFontVariation>,
    pub synthetic_bold: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CapturedFontVariation {
    pub tag: u32,
    pub value: f32,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapturedFontSource {
    Bundled,
    Data,
    Host,
    Memory,
    Remote,
    Unknown,
}

impl CapturedFontSource {
    pub fn is_host(self) -> bool {
        self == Self::Host
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapturedFontSelection {
    pub instance: String,
    pub resource: String,
    pub face_index: u32,
    pub source: CapturedFontSource,
    pub requested_families: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_family: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapturedFontWarning {
    pub code: &'static str,
    pub instance: String,
    pub requested_family: String,
    pub selected_family: String,
    pub fallback_chain: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnsupportedPaintEvent {
    pub sequence: usize,
    pub kind: UnsupportedPaintKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MissingTextMapping {
    pub sequence: usize,
    pub glyph_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnsupportedPaintKind {
    Box,
    RootBackground,
    Outline,
    CollapsedTableBorders,
    Iframe,
    TextEffects,
    ContentGeometry,
    SvgAnimation,
    SvgCompositing,
    SvgStroke,
    SvgPaint,
    SvgImage,
    SvgText,
    SvgInvalidPath,
}

impl UnsupportedPaintKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Box => "box",
            Self::RootBackground => "root-background",
            Self::Outline => "outline",
            Self::CollapsedTableBorders => "collapsed-table-borders",
            Self::Iframe => "iframe",
            Self::TextEffects => "text-effects",
            Self::ContentGeometry => "content-geometry",
            Self::SvgAnimation => "svg-animation",
            Self::SvgCompositing => "svg-compositing",
            Self::SvgStroke => "svg-stroke",
            Self::SvgPaint => "svg-paint",
            Self::SvgImage => "svg-image",
            Self::SvgText => "svg-text",
            Self::SvgInvalidPath => "svg-invalid-path",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanvasCaptureLimit {
    Placements,
    PlacedOperations,
    DiagnosticsBytes,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureError {
    InvalidJson(String),
    MissingPageSequence,
    InvalidPageCount {
        actual: usize,
    },
    InvalidPageIndex {
        actual: usize,
    },
    InvalidPageGeometry,
    InvalidPageGeometryAuthority,
    InvalidPaintGeometryAuthority,
    MissingTextRect {
        sequence: usize,
    },
    OperationCrossesPageBoundary {
        sequence: usize,
        page_index: usize,
    },
    OperationOutsidePageSequence {
        sequence: usize,
    },
    PageLocalPathUnsupported {
        sequence: usize,
    },
    InvalidTableGroupRepeat {
        page_index: usize,
    },
    MissingRepeatedTableHeader {
        tag_id: u64,
    },
    DuplicateRepeatedTableHeader {
        tag_id: u64,
    },
    RepeatedTableHeaderPathUnsupported {
        sequence: usize,
        tag_id: u64,
        page_index: usize,
    },
    NonDensePaintEvents {
        expected: usize,
        actual: usize,
    },
    UnbalancedStackingContext {
        sequence: usize,
    },
    UnclosedStackingContexts {
        count: usize,
    },
    UnknownPaintEvent {
        sequence: usize,
        kind: String,
    },
    DuplicateFragmentJoin,
    MissingFragmentJoin {
        sequence: usize,
        kind: String,
    },
    FragmentKindMismatch {
        sequence: usize,
        event_kind: String,
        fragment_kind: String,
    },
    FragmentTagMismatch {
        sequence: usize,
    },
    MissingTextRun {
        sequence: usize,
    },
    MissingTextFontInstance {
        sequence: usize,
    },
    UnknownFontInstance {
        sequence: usize,
        instance: String,
    },
    MissingImageRect {
        sequence: usize,
    },
    MissingImageUrl {
        sequence: usize,
    },
    InvalidVectorImageGeometry {
        sequence: usize,
    },
    InvalidVectorPath {
        sequence: usize,
    },
    UnresolvedImage {
        sequence: usize,
        url: String,
    },
    Canvas {
        sequence: usize,
        message: String,
    },
    CanvasRetentionBudgetExceeded,
    CanvasCaptureLimitExceeded {
        sequence: usize,
        limit: CanvasCaptureLimit,
        configured: u64,
        observed: u64,
    },
    MissingLinkRect {
        sequence: usize,
    },
    DuplicateLinkTag,
    DuplicateFontResource(String),
    DuplicateFontInstance(String),
    MissingFontResource {
        instance: String,
        resource: String,
    },
    InvalidFontResourceBase64(String),
    FontResourceDigestMismatch {
        resource: String,
        actual: String,
    },
    InvalidVectorImageBase64(String),
    VectorImageDigestMismatch {
        resource: String,
        actual: String,
    },
    ConflictingVectorResource(String),
    FontInstanceIdMismatch {
        instance: String,
        actual: String,
    },
    InvalidContentAddress(String),
    InvalidFontVariation(String),
    InvalidScene(&'static str),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(message) => {
                write!(formatter, "invalid layout snapshot JSON: {message}")
            },
            Self::MissingPageSequence => {
                write!(formatter, "layout snapshot has no paged-root sequence")
            },
            Self::InvalidPageCount { actual } => write!(
                formatter,
                "layout snapshot must contain at least one page, found {actual}"
            ),
            Self::InvalidPageIndex { actual } => write!(
                formatter,
                "layout snapshot page index does not match its sequence position: {actual}"
            ),
            Self::InvalidPageGeometry => {
                write!(formatter, "layout snapshot contains invalid page geometry")
            },
            Self::InvalidPageGeometryAuthority => write!(
                formatter,
                "layout snapshot page has incomplete or inconsistent app-unit authority"
            ),
            Self::InvalidPaintGeometryAuthority => write!(
                formatter,
                "layout snapshot paint geometry disagrees with its app-unit authority"
            ),
            Self::MissingTextRect { sequence } => {
                write!(
                    formatter,
                    "text paint event {sequence} has no retained rectangle"
                )
            },
            Self::OperationCrossesPageBoundary {
                sequence,
                page_index,
            } => write!(
                formatter,
                "paint event {sequence} crosses the boundary after page {page_index}"
            ),
            Self::OperationOutsidePageSequence { sequence } => write!(
                formatter,
                "paint event {sequence} lies outside the retained page sequence"
            ),
            Self::PageLocalPathUnsupported { sequence } => write!(
                formatter,
                "path paint event {sequence} cannot be translated to a later page"
            ),
            Self::InvalidTableGroupRepeat { page_index } => write!(
                formatter,
                "table header repeat for page {page_index} has invalid geometry"
            ),
            Self::MissingRepeatedTableHeader { tag_id } => write!(
                formatter,
                "table header repeat has no fragment subtree for tag {tag_id}"
            ),
            Self::DuplicateRepeatedTableHeader { tag_id } => write!(
                formatter,
                "table header repeat tag {tag_id} does not identify one fragment subtree"
            ),
            Self::RepeatedTableHeaderPathUnsupported {
                sequence,
                tag_id,
                page_index,
            } => write!(
                formatter,
                "table header {tag_id} path paint event {sequence} cannot be repeated on page \
                 {page_index}"
            ),
            Self::NonDensePaintEvents { expected, actual } => write!(
                formatter,
                "paint event sequence is not dense: expected {expected}, found {actual}"
            ),
            Self::UnbalancedStackingContext { sequence } => write!(
                formatter,
                "stacking-context leave at paint event {sequence} has no matching enter"
            ),
            Self::UnclosedStackingContexts { count } => {
                write!(
                    formatter,
                    "paint capture has {count} unclosed stacking contexts"
                )
            },
            Self::UnknownPaintEvent { sequence, kind } => {
                write!(formatter, "paint event {sequence} has unknown kind {kind}")
            },
            Self::DuplicateFragmentJoin => {
                write!(
                    formatter,
                    "layout snapshot repeats a paint fragment join key"
                )
            },
            Self::MissingFragmentJoin { sequence, kind } => write!(
                formatter,
                "{kind} paint event {sequence} has no matching fragment"
            ),
            Self::FragmentKindMismatch {
                sequence,
                event_kind,
                fragment_kind,
            } => write!(
                formatter,
                "{event_kind} paint event {sequence} joined fragment kind {fragment_kind}"
            ),
            Self::FragmentTagMismatch { sequence } => write!(
                formatter,
                "paint event {sequence} and its fragment disagree on the capture-local tag"
            ),
            Self::MissingTextRun { sequence } => {
                write!(
                    formatter,
                    "text paint event {sequence} has no retained text run"
                )
            },
            Self::MissingTextFontInstance { sequence } => write!(
                formatter,
                "text paint event {sequence} has no stable font instance"
            ),
            Self::UnknownFontInstance { sequence, instance } => write!(
                formatter,
                "text paint event {sequence} references unknown font instance {instance}"
            ),
            Self::MissingImageRect { sequence } => {
                write!(
                    formatter,
                    "image paint event {sequence} has no retained rectangle"
                )
            },
            Self::MissingImageUrl { sequence } => {
                write!(
                    formatter,
                    "image paint event {sequence} has no retained source URL"
                )
            },
            Self::InvalidVectorImageGeometry { sequence } => write!(
                formatter,
                "image paint event {sequence} has invalid retained vector geometry"
            ),
            Self::InvalidVectorPath { sequence } => write!(
                formatter,
                "image paint event {sequence} has an invalid retained vector path"
            ),
            Self::UnresolvedImage { sequence, url } => write!(
                formatter,
                "image paint event {sequence} has no content address for {url}"
            ),
            Self::Canvas { sequence, message } => {
                write!(
                    formatter,
                    "Canvas paint event {sequence} cannot be retained: {message}"
                )
            },
            Self::CanvasRetentionBudgetExceeded => {
                formatter.write_str("live Canvas retention exceeded the session budget")
            },
            Self::CanvasCaptureLimitExceeded {
                sequence,
                limit,
                configured,
                observed,
            } => {
                let limit = match limit {
                    CanvasCaptureLimit::Placements => "placement count",
                    CanvasCaptureLimit::PlacedOperations => "placed operation count",
                    CanvasCaptureLimit::DiagnosticsBytes => "diagnostics bytes",
                };
                write!(
                    formatter,
                    "Canvas paint event {sequence} cannot be retained: capture {limit} exceeds \
                     the session limit of {configured} (observed {observed})"
                )
            },
            Self::MissingLinkRect { sequence } => write!(
                formatter,
                "linked paint event {sequence} has no retained rectangle"
            ),
            Self::DuplicateLinkTag => {
                write!(formatter, "layout snapshot repeats a link tag join key")
            },
            Self::DuplicateFontResource(resource) => {
                write!(
                    formatter,
                    "layout snapshot repeats font resource {resource}"
                )
            },
            Self::DuplicateFontInstance(instance) => {
                write!(
                    formatter,
                    "layout snapshot repeats font instance {instance}"
                )
            },
            Self::MissingFontResource { instance, resource } => write!(
                formatter,
                "font instance {instance} references missing resource {resource}"
            ),
            Self::InvalidFontResourceBase64(resource) => write!(
                formatter,
                "font resource {resource} is not canonical padded base64"
            ),
            Self::FontResourceDigestMismatch { resource, actual } => write!(
                formatter,
                "font resource {resource} contains bytes addressed as {actual}"
            ),
            Self::InvalidVectorImageBase64(resource) => write!(
                formatter,
                "embedded SVG image resource {resource} is not canonical padded base64"
            ),
            Self::VectorImageDigestMismatch { resource, actual } => write!(
                formatter,
                "embedded SVG image resource {resource} contains bytes addressed as {actual}"
            ),
            Self::ConflictingVectorResource(resource) => write!(
                formatter,
                "retained SVG resources disagree on bytes for {resource}"
            ),
            Self::FontInstanceIdMismatch { instance, actual } => write!(
                formatter,
                "font instance {instance} has derived identity {actual}"
            ),
            Self::InvalidContentAddress(resource) => {
                write!(
                    formatter,
                    "resource is not a lowercase SHA-256 content address: {resource}"
                )
            },
            Self::InvalidFontVariation(instance) => {
                write!(
                    formatter,
                    "font instance {instance} has a non-finite variation"
                )
            },
            Self::InvalidScene(message) => {
                write!(formatter, "captured document scene is invalid: {message}")
            },
        }
    }
}

impl std::error::Error for CaptureError {}

#[derive(Clone, Copy)]
struct CanvasCaptureLimits {
    placements: u64,
    placed_operations: u64,
    diagnostics_bytes: u64,
}

const DEFAULT_CANVAS_CAPTURE_LIMITS: CanvasCaptureLimits = CanvasCaptureLimits {
    // Derived capture state must not be able to amplify the producer-side retention envelope.
    placements: servo_canvas::retained_canvas::MAX_RETAINED_OBJECTS,
    placed_operations: servo_canvas::retained_canvas::MAX_RETAINED_COMMANDS,
    diagnostics_bytes: servo_canvas::retained_canvas::MAX_RETAINED_RASTER_BYTES,
};

#[derive(Clone, Copy, Default)]
struct CanvasCaptureBudget {
    placements: u64,
    placed_operations: u64,
    diagnostics_bytes: u64,
}

impl CanvasCaptureBudget {
    fn preflight(
        self,
        limits: CanvasCaptureLimits,
        sequence: usize,
        placed_operations: usize,
        diagnostics_bytes: u64,
    ) -> Result<Self, CaptureError> {
        Ok(Self {
            placements: reserve_canvas_capture_cost(
                self.placements,
                1,
                limits.placements,
                sequence,
                CanvasCaptureLimit::Placements,
            )?,
            placed_operations: reserve_canvas_capture_cost(
                self.placed_operations,
                u64::try_from(placed_operations).unwrap_or(u64::MAX),
                limits.placed_operations,
                sequence,
                CanvasCaptureLimit::PlacedOperations,
            )?,
            diagnostics_bytes: reserve_canvas_capture_cost(
                self.diagnostics_bytes,
                diagnostics_bytes,
                limits.diagnostics_bytes,
                sequence,
                CanvasCaptureLimit::DiagnosticsBytes,
            )?,
        })
    }
}

#[derive(Default)]
struct JsonByteCounter {
    bytes: u64,
}

impl std::io::Write for JsonByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn canvas_diagnostics_json_len(diagnostics: &CanvasDiagnostics) -> Result<u64, serde_json::Error> {
    let mut counter = JsonByteCounter::default();
    serde_json::to_writer(&mut counter, diagnostics)?;
    Ok(counter.bytes)
}

fn reserve_canvas_capture_cost(
    current: u64,
    additional: u64,
    configured: u64,
    sequence: usize,
    limit: CanvasCaptureLimit,
) -> Result<u64, CaptureError> {
    let observed =
        current
            .checked_add(additional)
            .ok_or(CaptureError::CanvasCaptureLimitExceeded {
                sequence,
                limit,
                configured,
                observed: u64::MAX,
            })?;
    if observed > configured {
        return Err(CaptureError::CanvasCaptureLimitExceeded {
            sequence,
            limit,
            configured,
            observed,
        });
    }
    Ok(observed)
}

/// Convert the retained layout snapshot JSON into a canonical document scene.
///
/// The converter consumes only retained capture data. Capture-local fragment, tag, spatial-node,
/// clip, and diagnostic font identifiers never enter the canonical scene. Font identifiers are
/// reduced to a portable source kind in the inspect report.
pub fn capture_document_scene(
    snapshot_json: &[u8],
    resolve_image: impl FnMut(&str) -> Option<String>,
) -> Result<SceneCapture, CaptureError> {
    capture_document_scene_with_canvas_limits(
        snapshot_json,
        resolve_image,
        |key| {
            Err(format!(
                "no live snapshot resolver for image key {}:{}",
                key.namespace, key.key
            ))
        },
        DEFAULT_CANVAS_CAPTURE_LIMITS,
    )
}

pub fn capture_document_scene_with_canvas(
    snapshot_json: &[u8],
    resolve_image: impl FnMut(&str) -> Option<String>,
    freeze_canvas: impl FnOnce(
        &[(u32, u32)],
    ) -> Result<FrozenCanvasSnapshots, FreezeCanvasSnapshotsError>,
) -> Result<SceneCapture, CaptureError> {
    let capture: LayoutCapture = serde_json::from_slice(snapshot_json)
        .map_err(|error| CaptureError::InvalidJson(error.to_string()))?;
    let requests = canvas_freeze_requests(&capture, DEFAULT_CANVAS_CAPTURE_LIMITS.placements)?;
    let first_sequence = requests.first().map_or(0, |(sequence, _)| *sequence);
    let requested_keys = requests
        .iter()
        .map(|(_, key)| (key.namespace, key.key))
        .collect::<Vec<_>>();
    let frozen_canvas = freeze_canvas(&requested_keys)
        .map_err(|error| canvas_freeze_error(&requests, first_sequence, error))?;
    if frozen_canvas.retention_budget_exceeded() {
        return Err(CaptureError::CanvasRetentionBudgetExceeded);
    }
    capture_layout_with_canvas_limits(
        capture,
        resolve_image,
        move |key| {
            frozen_canvas.get(key.namespace, key.key).ok_or_else(|| {
                format!(
                    "frozen Canvas snapshot bundle omitted requested image key {}:{}",
                    key.namespace, key.key
                )
            })
        },
        DEFAULT_CANVAS_CAPTURE_LIMITS,
    )
}

/// Convert one consumed controlled candidate using only its generation-bound Canvas sources.
///
/// Every Canvas image key used by the retained layout must have been observed in the candidate.
/// The registry generation is checked atomically with freezing the complete requested key set.
/// Extra candidate-bound keys are allowed because a live Canvas need not be placed by this layout.
#[doc(hidden)]
pub fn capture_controlled_document_scene_with_canvas(
    snapshot_json: &[u8],
    resolve_image: impl FnMut(&str) -> Option<String>,
    canvas_binding: Option<&DocumentCanvasCaptureBinding>,
    freeze_canvas: impl FnOnce(
        &[(u32, u32)],
        u64,
    ) -> Result<FrozenCanvasSnapshots, FreezeCanvasSnapshotsError>,
) -> Result<SceneCapture, CaptureError> {
    let capture: LayoutCapture = serde_json::from_slice(snapshot_json)
        .map_err(|error| CaptureError::InvalidJson(error.to_string()))?;
    let requests = canvas_freeze_requests(&capture, DEFAULT_CANVAS_CAPTURE_LIMITS.placements)?;
    let first_sequence = requests.first().map_or(0, |(sequence, _)| *sequence);
    let requested_keys = requests
        .iter()
        .map(|(_, key)| (key.namespace, key.key))
        .collect::<Vec<_>>();

    let Some(canvas_binding) = canvas_binding else {
        if let Some((sequence, key)) = requests.first() {
            return Err(CaptureError::Canvas {
                sequence: *sequence,
                message: format!(
                    "layout Canvas image key {}:{} was not bound by the consumed controlled candidate",
                    key.namespace, key.key
                ),
            });
        }
        return capture_layout_with_canvas_limits(
            capture,
            resolve_image,
            |key| {
                Err(format!(
                    "layout requested unbound Canvas image key {}:{}",
                    key.namespace, key.key
                ))
            },
            DEFAULT_CANVAS_CAPTURE_LIMITS,
        );
    };

    for (sequence, requested) in &requests {
        let requested_key = (requested.namespace, requested.key);
        if canvas_binding
            .image_keys()
            .binary_search_by_key(&requested_key, |bound| (bound.namespace(), bound.key()))
            .is_err()
        {
            return Err(CaptureError::Canvas {
                sequence: *sequence,
                message: format!(
                    "layout Canvas image key {}:{} was not bound by the consumed controlled candidate",
                    requested.namespace, requested.key
                ),
            });
        }
    }

    let expected_generation = canvas_binding.registry_generation();
    let frozen_canvas = freeze_canvas(&requested_keys, expected_generation)
        .map_err(|error| canvas_freeze_error(&requests, first_sequence, error))?;
    if frozen_canvas.generation() != expected_generation {
        return Err(CaptureError::Canvas {
            sequence: first_sequence,
            message: format!(
                "Canvas freezer returned registry generation {} for controlled generation {expected_generation}",
                frozen_canvas.generation()
            ),
        });
    }
    if frozen_canvas.retention_budget_exceeded() {
        return Err(CaptureError::CanvasRetentionBudgetExceeded);
    }
    capture_layout_with_canvas_limits(
        capture,
        resolve_image,
        move |key| {
            frozen_canvas.get(key.namespace, key.key).ok_or_else(|| {
                format!(
                    "frozen Canvas snapshot bundle omitted requested image key {}:{}",
                    key.namespace, key.key
                )
            })
        },
        DEFAULT_CANVAS_CAPTURE_LIMITS,
    )
}

fn canvas_freeze_error(
    requests: &[(usize, CapturedCanvasImageKey)],
    first_sequence: usize,
    error: FreezeCanvasSnapshotsError,
) -> CaptureError {
    let sequence = match &error {
        FreezeCanvasSnapshotsError::MissingImageKey { namespace, key, .. } => requests
            .iter()
            .find_map(|(sequence, requested)| {
                (requested.namespace == *namespace && requested.key == *key).then_some(*sequence)
            })
            .unwrap_or(first_sequence),
        FreezeCanvasSnapshotsError::RetentionDisabled |
        FreezeCanvasSnapshotsError::GenerationChanged { .. } => first_sequence,
    };
    CaptureError::Canvas {
        sequence,
        message: error.to_string(),
    }
}

fn canvas_freeze_requests(
    capture: &LayoutCapture,
    placement_limit: u64,
) -> Result<Vec<(usize, CapturedCanvasImageKey)>, CaptureError> {
    let by_fragment = capture
        .fragments
        .iter()
        .filter(|fragment| fragment.kind == "image" && fragment.vector_image.is_none())
        .filter_map(|fragment| Some((fragment.paint_fragment_id?, fragment.canvas_image_key?)))
        .collect::<HashMap<_, _>>();
    let mut placements = 0;
    for (expected_sequence, event) in capture.paint_events.iter().enumerate() {
        if event.sequence != expected_sequence {
            return Err(CaptureError::NonDensePaintEvents {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }
        if event.kind != "image" {
            continue;
        }
        if event
            .fragment_id
            .and_then(|fragment_id| by_fragment.get(&fragment_id))
            .is_none()
        {
            continue;
        }
        placements = reserve_canvas_capture_cost(
            placements,
            1,
            placement_limit,
            event.sequence,
            CanvasCaptureLimit::Placements,
        )?;
    }

    // Only materialize unique freeze requests after every placement has passed the bound. The
    // second pass preserves each key's first paint sequence and keeps all deduplication outside the
    // Canvas registry lock.
    let request_capacity = usize::try_from(placements).unwrap_or(capture.paint_events.len());
    let mut unique_keys = HashSet::with_capacity(request_capacity);
    let mut requests = Vec::with_capacity(request_capacity);
    for event in &capture.paint_events {
        if event.kind != "image" {
            continue;
        }
        let Some(key) = event
            .fragment_id
            .and_then(|fragment_id| by_fragment.get(&fragment_id))
            .copied()
        else {
            continue;
        };
        if unique_keys.insert((key.namespace, key.key)) {
            requests.push((event.sequence, key));
        }
    }
    Ok(requests)
}

fn capture_document_scene_with_canvas_limits(
    snapshot_json: &[u8],
    resolve_image: impl FnMut(&str) -> Option<String>,
    resolve_canvas: impl FnMut(CapturedCanvasImageKey) -> Result<Arc<RetainedCanvasSnapshot>, String>,
    canvas_limits: CanvasCaptureLimits,
) -> Result<SceneCapture, CaptureError> {
    let capture: LayoutCapture = serde_json::from_slice(snapshot_json)
        .map_err(|error| CaptureError::InvalidJson(error.to_string()))?;
    capture_layout_with_canvas_limits(capture, resolve_image, resolve_canvas, canvas_limits)
}

fn capture_layout_with_canvas_limits(
    capture: LayoutCapture,
    mut resolve_image: impl FnMut(&str) -> Option<String>,
    mut resolve_canvas: impl FnMut(
        CapturedCanvasImageKey,
    ) -> Result<Arc<RetainedCanvasSnapshot>, String>,
    canvas_limits: CanvasCaptureLimits,
) -> Result<SceneCapture, CaptureError> {
    validate_paint_app_unit_authority(&capture)?;
    let pages = capture_pages(&capture)?;
    let mut fixed_point_authority = capture_fixed_point_authority(&capture);
    let table_group_repeats = capture
        .page_sequence
        .as_ref()
        .map(|sequence| sequence.table_group_repeats.clone())
        .unwrap_or_default();
    let repeated_header_fragments =
        repeated_table_header_fragments(&capture.fragments, &table_group_repeats)?;
    let unsupported_box_fragments =
        unsupported_box_subtree_fragments(&capture.fragments, &capture.paint_events);

    let mut font_resources = collect_font_resources(capture.font_resources)?;
    let mut font_instances = collect_font_instances(capture.font_instances, &font_resources)?;
    let links = collect_links(capture.links)?;
    let box_link_placements =
        collect_box_link_placements(&capture.fragments, &capture.paint_events, &links)?;
    let fragments = collect_fragments(capture.fragments)?;
    let mut emitted_links = HashSet::new();
    let mut unsupported_events = Vec::new();
    let mut text_mapping_gaps = Vec::new();
    let mut font_selections = BTreeMap::new();
    let mut font_warnings = BTreeMap::new();
    let mut operations = Vec::new();
    let mut canvas_resources = BTreeMap::<String, CanvasResource>::new();
    let mut embedded_image_resources = BTreeMap::<String, CanvasResource>::new();
    let mut canvas_diagnostics = Vec::new();
    let mut adapted_canvases = HashMap::new();
    let mut canvas_budget = CanvasCaptureBudget::default();
    let mut stacking_context_depth = 0usize;

    for (expected_sequence, event) in capture.paint_events.iter().enumerate() {
        if event.sequence != expected_sequence {
            return Err(CaptureError::NonDensePaintEvents {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }

        match event.kind.as_str() {
            "stacking-context-enter" => stacking_context_depth += 1,
            "stacking-context-leave" => {
                stacking_context_depth = stacking_context_depth.checked_sub(1).ok_or(
                    CaptureError::UnbalancedStackingContext {
                        sequence: event.sequence,
                    },
                )?;
            },
            "text" => {
                let (fragment_id, fragment) = joined_fragment(event, &fragments)?;
                if fragment.kind != "text" {
                    return Err(CaptureError::FragmentKindMismatch {
                        sequence: event.sequence,
                        event_kind: event.kind.clone(),
                        fragment_kind: fragment.kind.clone(),
                    });
                }
                let text_run = fragment
                    .text_run
                    .as_ref()
                    .ok_or(CaptureError::MissingTextRun {
                        sequence: event.sequence,
                    })?;
                let font = text_run.font_instance_id.as_ref().ok_or(
                    CaptureError::MissingTextFontInstance {
                        sequence: event.sequence,
                    },
                )?;
                if !font_instances.contains_key(font) {
                    return Err(CaptureError::UnknownFontInstance {
                        sequence: event.sequence,
                        instance: font.clone(),
                    });
                }
                let instance = &font_instances[font];
                let source = captured_font_source(&text_run.font_identifier);
                let selection_key = (
                    font.clone(),
                    source,
                    text_run.requested_families.clone(),
                    text_run.selected_family.clone(),
                );
                font_selections
                    .entry(selection_key)
                    .or_insert_with(|| CapturedFontSelection {
                        instance: font.clone(),
                        resource: instance.resource.clone(),
                        face_index: instance.face_index,
                        source,
                        requested_families: text_run.requested_families.clone(),
                        selected_family: text_run.selected_family.clone(),
                    });
                if let (Some(requested), Some(selected)) = (
                    text_run.requested_families.first(),
                    text_run.selected_family.as_ref(),
                ) && !requested.eq_ignore_ascii_case(selected)
                {
                    font_warnings
                        .entry((
                            font.clone(),
                            requested.clone(),
                            selected.clone(),
                            text_run.requested_families.clone(),
                        ))
                        .or_insert_with(|| CapturedFontWarning {
                            code: "FONT_FALLBACK_USED",
                            instance: font.clone(),
                            requested_family: requested.clone(),
                            selected_family: selected.clone(),
                            fallback_chain: text_run.requested_families.clone(),
                        });
                }
                text_mapping_gaps.extend(
                    text_run
                        .glyphs
                        .iter()
                        .enumerate()
                        .filter(|(_, glyph)| glyph.text_range.is_none())
                        .map(|(glyph_index, _)| MissingTextMapping {
                            sequence: event.sequence,
                            glyph_index,
                        }),
                );
                let bounds = fragment
                    .rect
                    .as_ref()
                    .ok_or(CaptureError::MissingTextRect {
                        sequence: event.sequence,
                    })?
                    .into_scene_rect();
                operations.push(PositionedOperation {
                    sequence: event.sequence,
                    structural_fragment_index: None,
                    bounds,
                    authority: captured_text_operation_authority(text_run),
                    operation: Operation::Text {
                        text: text_run.text.clone(),
                        font: font.clone(),
                        font_size: f64::from(text_run.font_size),
                        color: text_run.color.into_scene_color(),
                        glyphs: text_run
                            .glyphs
                            .iter()
                            .map(|glyph| Glyph {
                                id: glyph.id,
                                x: f64::from(glyph.x),
                                y: f64::from(glyph.y),
                                advance: f64::from(glyph.advance),
                                text_range: glyph.text_range.map(|range| crate::Utf8Range {
                                    start: range.start,
                                    end: range.end,
                                }),
                            })
                            .collect(),
                        meta: OperationMeta::default(),
                    },
                });
                append_link(
                    &mut operations,
                    &mut emitted_links,
                    &links,
                    fragment_id,
                    fragment,
                    event.sequence,
                )?;
            },
            "image" => {
                let (fragment_id, fragment) = joined_fragment(event, &fragments)?;
                if fragment.kind != "image" {
                    return Err(CaptureError::FragmentKindMismatch {
                        sequence: event.sequence,
                        event_kind: event.kind.clone(),
                        fragment_kind: fragment.kind.clone(),
                    });
                }
                let rect = fragment
                    .rect
                    .as_ref()
                    .ok_or(CaptureError::MissingImageRect {
                        sequence: event.sequence,
                    })?;
                if let Some(vector_image) = fragment.vector_image.as_ref() {
                    append_vector_image(
                        &mut operations,
                        &mut unsupported_events,
                        &mut embedded_image_resources,
                        &mut font_resources,
                        &mut font_instances,
                        &mut font_selections,
                        vector_image,
                        rect,
                        event.sequence,
                    )?;
                    append_link(
                        &mut operations,
                        &mut emitted_links,
                        &links,
                        fragment_id,
                        fragment,
                        event.sequence,
                    )?;
                    append_box_links(
                        &mut operations,
                        box_link_placements.get(&event.sequence).map(Vec::as_slice),
                        event.sequence,
                    );
                    continue;
                }
                if let Some(key) = fragment.canvas_image_key {
                    // Reject an excess placement before resolving or adapting another snapshot.
                    reserve_canvas_capture_cost(
                        canvas_budget.placements,
                        1,
                        canvas_limits.placements,
                        event.sequence,
                        CanvasCaptureLimit::Placements,
                    )?;
                    let snapshot = resolve_canvas(key).map_err(|message| CaptureError::Canvas {
                        sequence: event.sequence,
                        message,
                    })?;
                    let (canvas, diagnostics_index, is_new) = adapt_retained_canvas(
                        &mut adapted_canvases,
                        snapshot,
                        canvas_diagnostics.len(),
                    )
                    .map_err(|error| CaptureError::Canvas {
                        sequence: event.sequence,
                        message: error.to_string(),
                    })?;
                    let diagnostics_bytes = if is_new {
                        canvas_diagnostics_json_len(&canvas.diagnostics).map_err(|error| {
                            CaptureError::Canvas {
                                sequence: event.sequence,
                                message: format!("cannot measure Canvas diagnostics: {error}"),
                            }
                        })?
                    } else {
                        0
                    };
                    let next_canvas_budget = canvas_budget.preflight(
                        canvas_limits,
                        event.sequence,
                        canvas.scene.pages[0].operations.len(),
                        diagnostics_bytes,
                    )?;
                    append_canvas(
                        &mut operations,
                        &mut canvas_resources,
                        &canvas,
                        rect,
                        event.sequence,
                    )?;
                    if is_new {
                        debug_assert_eq!(diagnostics_index, canvas_diagnostics.len());
                        canvas_diagnostics.push(CapturedCanvasDiagnostics {
                            sequences: Vec::new(),
                            diagnostics: canvas.diagnostics.clone(),
                        });
                    }
                    canvas_diagnostics[diagnostics_index]
                        .sequences
                        .push(event.sequence);
                    canvas_budget = next_canvas_budget;
                    append_link(
                        &mut operations,
                        &mut emitted_links,
                        &links,
                        fragment_id,
                        fragment,
                        event.sequence,
                    )?;
                    append_box_links(
                        &mut operations,
                        box_link_placements.get(&event.sequence).map(Vec::as_slice),
                        event.sequence,
                    );
                    continue;
                }
                let bounds = rect.into_scene_rect();
                let url = fragment
                    .image_url
                    .as_deref()
                    .ok_or(CaptureError::MissingImageUrl {
                        sequence: event.sequence,
                    })?;
                let resource = resolve_image(url).ok_or_else(|| CaptureError::UnresolvedImage {
                    sequence: event.sequence,
                    url: url.into(),
                })?;
                ensure_content_address(&resource)?;
                operations.push(PositionedOperation {
                    sequence: event.sequence,
                    structural_fragment_index: None,
                    bounds: bounds.clone(),
                    authority: rect.app_units.map(CapturedOperationAuthority::Bounds),
                    operation: Operation::Image {
                        bounds,
                        resource,
                        meta: OperationMeta::default(),
                    },
                });
                append_link(
                    &mut operations,
                    &mut emitted_links,
                    &links,
                    fragment_id,
                    fragment,
                    event.sequence,
                )?;
            },
            "table-border" => {
                for border in &event.table_borders {
                    let bounds = border.rect.into_scene_rect();
                    operations.push(PositionedOperation {
                        sequence: event.sequence,
                        structural_fragment_index: None,
                        bounds: bounds.clone(),
                        authority: border
                            .rect
                            .app_units
                            .map(CapturedOperationAuthority::Bounds),
                        operation: Operation::Path {
                            data: rectangle_path_data(&bounds),
                            bounds,
                            fill: Some(Color {
                                r: f64::from(border.color.r),
                                g: f64::from(border.color.g),
                                b: f64::from(border.color.b),
                                a: f64::from(border.color.a),
                            }),
                            fill_rule: FillRule::NonZero,
                            stroke: None,
                            meta: OperationMeta {
                                semantics: Some(Semantics {
                                    role: "artifact".into(),
                                    label: Some("table-border".into()),
                                }),
                                source: None,
                            },
                        },
                    });
                }
            },
            "paint-rect" => {
                if event
                    .fragment_id
                    .is_some_and(|fragment_id| unsupported_box_fragments.contains(&fragment_id))
                {
                    append_box_links(
                        &mut operations,
                        box_link_placements.get(&event.sequence).map(Vec::as_slice),
                        event.sequence,
                    );
                    continue;
                }
                for paint_rect in &event.paint_rects {
                    let bounds = paint_rect.rect.into_scene_rect();
                    operations.push(PositionedOperation {
                        sequence: event.sequence,
                        structural_fragment_index: None,
                        bounds: bounds.clone(),
                        authority: paint_rect
                            .rect
                            .app_units
                            .map(CapturedOperationAuthority::Bounds),
                        operation: Operation::Path {
                            data: rectangle_path_data(&bounds),
                            bounds,
                            fill: Some(paint_rect.color.into_scene_color()),
                            fill_rule: FillRule::NonZero,
                            stroke: None,
                            meta: OperationMeta {
                                semantics: Some(Semantics {
                                    role: "artifact".into(),
                                    label: Some(
                                        match paint_rect.kind {
                                            CapturePaintRectKind::Background => "background",
                                            CapturePaintRectKind::Border => "border",
                                        }
                                        .into(),
                                    ),
                                }),
                                source: None,
                            },
                        },
                    });
                }
            },
            kind @ ("box" |
            "root-background" |
            "outline" |
            "collapsed-table-borders" |
            "iframe" |
            "text-effects" |
            "content-geometry") => {
                unsupported_events.push(UnsupportedPaintEvent {
                    sequence: event.sequence,
                    kind: match kind {
                        "box" => UnsupportedPaintKind::Box,
                        "root-background" => UnsupportedPaintKind::RootBackground,
                        "outline" => UnsupportedPaintKind::Outline,
                        "collapsed-table-borders" => UnsupportedPaintKind::CollapsedTableBorders,
                        "iframe" => UnsupportedPaintKind::Iframe,
                        "text-effects" => UnsupportedPaintKind::TextEffects,
                        "content-geometry" => UnsupportedPaintKind::ContentGeometry,
                        _ => {
                            return Err(CaptureError::UnknownPaintEvent {
                                sequence: event.sequence,
                                kind: kind.into(),
                            });
                        },
                    },
                });

                if let Some((fragment_id, fragment)) = optionally_joined_fragment(event, &fragments)
                {
                    append_link(
                        &mut operations,
                        &mut emitted_links,
                        &links,
                        fragment_id,
                        fragment,
                        event.sequence,
                    )?;
                }
            },
            kind => {
                return Err(CaptureError::UnknownPaintEvent {
                    sequence: event.sequence,
                    kind: kind.into(),
                });
            },
        }
        append_box_links(
            &mut operations,
            box_link_placements.get(&event.sequence).map(Vec::as_slice),
            event.sequence,
        );
    }

    if stacking_context_depth != 0 {
        return Err(CaptureError::UnclosedStackingContexts {
            count: stacking_context_depth,
        });
    }

    let distributed = distribute_operations(
        &pages,
        operations,
        &capture.paint_events,
        &table_group_repeats,
        &repeated_header_fragments,
    )?;
    let scene = DocumentScene {
        schema: crate::SCHEMA.into(),
        version: crate::SCHEMA_VERSION,
        pages: distributed.pages,
    };
    scene.validate().map_err(CaptureError::InvalidScene)?;
    fixed_point_authority.page_operations = distributed.page_operations;

    Ok(SceneCapture {
        scene,
        fixed_point_authority,
        canvas_resources: canvas_resources.into_values().collect(),
        embedded_image_resources: embedded_image_resources.into_values().collect(),
        canvas_diagnostics,
        font_resources: font_resources.into_values().collect(),
        font_instances: font_instances.into_values().collect(),
        font_selections: font_selections.into_values().collect(),
        font_warnings: font_warnings.into_values().collect(),
        unsupported_events,
        text_mapping_gaps,
    })
}

fn capture_fixed_point_authority(capture: &LayoutCapture) -> CapturedFixedPointAuthority {
    CapturedFixedPointAuthority {
        pages: capture
            .page_sequence
            .as_ref()
            .into_iter()
            .flat_map(|sequence| &sequence.pages)
            .map(|page| CapturedPageAuthority {
                index: page.index,
                style_source: page.style_source,
                app_units: page.app_units,
            })
            .collect(),
        page_operations: Vec::new(),
    }
}

fn captured_text_operation_authority(text: &CaptureTextRun) -> Option<CapturedOperationAuthority> {
    Some(CapturedOperationAuthority::Text {
        font_size_app_units: text.font_size_app_units?,
        glyphs: text
            .glyphs
            .iter()
            .map(|glyph| glyph.app_units)
            .collect::<Option<Vec<_>>>()?,
    })
}

fn validate_paint_app_unit_authority(capture: &LayoutCapture) -> Result<(), CaptureError> {
    for fragment in &capture.fragments {
        if fragment
            .rect
            .as_ref()
            .is_some_and(|rect| !rect.app_unit_authority_matches())
        {
            return Err(CaptureError::InvalidPaintGeometryAuthority);
        }
        let Some(text_run) = fragment.text_run.as_ref() else {
            continue;
        };
        if text_run
            .font_size_app_units
            .is_some_and(|exact| app_units_to_f32_px(exact) != text_run.font_size) ||
            text_run
                .glyphs
                .iter()
                .any(|glyph| !glyph.app_unit_authority_matches())
        {
            return Err(CaptureError::InvalidPaintGeometryAuthority);
        }
    }
    for event in &capture.paint_events {
        if event
            .table_borders
            .iter()
            .any(|border| !border.rect.app_unit_authority_matches()) ||
            event
                .paint_rects
                .iter()
                .any(|paint_rect| !paint_rect.rect.app_unit_authority_matches())
        {
            return Err(CaptureError::InvalidPaintGeometryAuthority);
        }
    }
    Ok(())
}

struct CachedRetainedCanvas {
    source: Arc<RetainedCanvasSnapshot>,
    capture: Arc<HybridCanvasCapture>,
    diagnostics_index: usize,
}

fn adapt_retained_canvas(
    cache: &mut HashMap<usize, CachedRetainedCanvas>,
    snapshot: Arc<RetainedCanvasSnapshot>,
    diagnostics_index: usize,
) -> Result<(Arc<HybridCanvasCapture>, usize, bool), crate::hybrid_canvas::CanvasError> {
    let identity = Arc::as_ptr(&snapshot) as usize;
    if let Some(cached) = cache.get(&identity) &&
        Arc::ptr_eq(&cached.source, &snapshot)
    {
        return Ok((Arc::clone(&cached.capture), cached.diagnostics_index, false));
    }

    let capture = Arc::new(adapt_canvas(transcript_from_retained(Arc::clone(
        &snapshot,
    ))?)?);
    // Pin the source allocation for the capture lifetime so its pointer remains a stable identity.
    cache.insert(
        identity,
        CachedRetainedCanvas {
            source: snapshot,
            capture: Arc::clone(&capture),
            diagnostics_index,
        },
    );
    Ok((capture, diagnostics_index, true))
}

fn append_canvas(
    operations: &mut Vec<PositionedOperation>,
    resources: &mut BTreeMap<String, CanvasResource>,
    canvas: &HybridCanvasCapture,
    destination: &CaptureRect,
    sequence: usize,
) -> Result<(), CaptureError> {
    let source_page = &canvas.scene.pages[0];
    if source_page.size.width <= 0.0 ||
        source_page.size.height <= 0.0 ||
        destination.width <= 0.0 ||
        destination.height <= 0.0
    {
        return Err(CaptureError::Canvas {
            sequence,
            message: "Canvas source and destination must have positive dimensions".into(),
        });
    }
    for resource in &canvas.resources {
        if let Some(existing) = resources.get(&resource.resource) &&
            existing.png != resource.png
        {
            return Err(CaptureError::Canvas {
                sequence,
                message: format!(
                    "Canvas resource {} has conflicting bytes",
                    resource.resource
                ),
            });
        }
        resources
            .entry(resource.resource.clone())
            .or_insert_with(|| resource.clone());
    }

    let destination = destination.into_scene_rect();
    let scale_x = destination.width / source_page.size.width;
    let scale_y = destination.height / source_page.size.height;
    for operation in &source_page.operations {
        let operation = match operation {
            Operation::Path {
                bounds,
                fill,
                fill_rule,
                stroke,
                meta,
                ..
            } => {
                let bounds = place_canvas_rect(bounds, &destination, scale_x, scale_y);
                Operation::Path {
                    data: format!(
                        "M{} {}h{}v{}h-{}z",
                        bounds.x, bounds.y, bounds.width, bounds.height, bounds.width
                    ),
                    bounds,
                    fill: *fill,
                    fill_rule: *fill_rule,
                    stroke: stroke.clone(),
                    meta: meta.clone(),
                }
            },
            Operation::Image {
                bounds,
                resource,
                meta,
            } => Operation::Image {
                bounds: place_canvas_rect(bounds, &destination, scale_x, scale_y),
                resource: resource.clone(),
                meta: meta.clone(),
            },
            Operation::Text { .. } | Operation::Link { .. } => {
                return Err(CaptureError::Canvas {
                    sequence,
                    message: "hybrid Canvas adapter emitted an unexpected operation".into(),
                });
            },
        };
        let bounds = match &operation {
            Operation::Path { bounds, .. } | Operation::Image { bounds, .. } => bounds.clone(),
            Operation::Text { .. } | Operation::Link { .. } => {
                return Err(CaptureError::Canvas {
                    sequence,
                    message: "hybrid Canvas adapter emitted an unexpected operation".into(),
                });
            },
        };
        operations.push(PositionedOperation {
            sequence,
            structural_fragment_index: None,
            bounds,
            operation,
            authority: None,
        });
    }
    Ok(())
}

fn place_canvas_rect(source: &Rect, destination: &Rect, scale_x: f64, scale_y: f64) -> Rect {
    Rect {
        x: destination.x + source.x * scale_x,
        y: destination.y + source.y * scale_y,
        width: source.width * scale_x,
        height: source.height * scale_y,
    }
}

fn captured_font_source(identifier: &serde_json::Value) -> CapturedFontSource {
    let Some(identifier) = identifier.as_object() else {
        return CapturedFontSource::Unknown;
    };
    if identifier.contains_key("Local") {
        return CapturedFontSource::Host;
    }
    if identifier.contains_key("ArrayBuffer") {
        return CapturedFontSource::Memory;
    }
    let Some(url) = identifier.get("Web").and_then(serde_json::Value::as_str) else {
        return CapturedFontSource::Unknown;
    };
    match url.split_once(':').map(|(scheme, _)| scheme) {
        Some("file") => CapturedFontSource::Bundled,
        Some("data") => CapturedFontSource::Data,
        Some("http" | "https") => CapturedFontSource::Remote,
        _ => CapturedFontSource::Unknown,
    }
}

struct RepeatedTableHeaderFragments {
    fragment_indices: std::ops::Range<usize>,
    paint_fragment_ids: HashSet<usize>,
}

fn repeated_table_header_fragments(
    fragments: &[CaptureFragment],
    repeats: &[CaptureTableGroupRepeat],
) -> Result<HashMap<u64, RepeatedTableHeaderFragments>, CaptureError> {
    let mut tags = repeats
        .iter()
        .map(|repeat| repeat.header_tag_id)
        .collect::<Vec<_>>();
    tags.sort_unstable();
    tags.dedup();

    let mut subtrees = HashMap::new();
    for tag_id in tags {
        let matches = fragments
            .iter()
            .enumerate()
            .filter(|(_, fragment)| fragment.kind == "box" && fragment.tag_id == Some(tag_id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [root_index] = matches.as_slice() else {
            return Err(if matches.is_empty() {
                CaptureError::MissingRepeatedTableHeader { tag_id }
            } else {
                CaptureError::DuplicateRepeatedTableHeader { tag_id }
            });
        };
        let root_depth = fragments[*root_index].depth;
        let subtree_end = fragments[*root_index + 1..]
            .iter()
            .position(|fragment| fragment.depth <= root_depth)
            .map(|offset| *root_index + offset + 1)
            .unwrap_or(fragments.len());
        let paint_fragment_ids = fragments[*root_index..subtree_end]
            .iter()
            .filter_map(|fragment| fragment.paint_fragment_id)
            .collect::<HashSet<_>>();
        if paint_fragment_ids.is_empty() {
            return Err(CaptureError::MissingRepeatedTableHeader { tag_id });
        }
        subtrees.insert(
            tag_id,
            RepeatedTableHeaderFragments {
                fragment_indices: *root_index..subtree_end,
                paint_fragment_ids,
            },
        );
    }
    Ok(subtrees)
}

fn capture_pages(capture: &LayoutCapture) -> Result<Vec<CapturePage>, CaptureError> {
    let sequence = capture
        .page_sequence
        .as_ref()
        .ok_or(CaptureError::MissingPageSequence)?;
    if sequence.pages.is_empty() {
        return Err(CaptureError::InvalidPageCount {
            actual: sequence.pages.len(),
        });
    }
    for (expected_index, page) in sequence.pages.iter().enumerate() {
        if page.index != expected_index {
            return Err(CaptureError::InvalidPageIndex { actual: page.index });
        }
        if !positive_finite(page.width) ||
            !positive_finite(page.height) ||
            !nonnegative_finite(page.margin_top) ||
            !nonnegative_finite(page.margin_right) ||
            !nonnegative_finite(page.margin_bottom) ||
            !nonnegative_finite(page.margin_left) ||
            !positive_finite(page.available_inline_size) ||
            !positive_finite(page.available_block_size) ||
            page.margin_left + page.margin_right >= page.width ||
            page.margin_top + page.margin_bottom >= page.height ||
            page.available_inline_size > page.width ||
            page.available_block_size > page.height
        {
            return Err(CaptureError::InvalidPageGeometry);
        }
        match (page.style_source, page.app_units) {
            (None, None) => {},
            (Some(CapturedPageStyleSource::RequestDefaults), Some(app_units))
                if valid_page_app_unit_authority(page, app_units) => {},
            _ => return Err(CaptureError::InvalidPageGeometryAuthority),
        }
    }
    Ok(sequence.pages.clone())
}

fn valid_page_app_unit_authority(page: &CapturePage, app_units: CapturedPageAppUnits) -> bool {
    if app_units.width <= 0 ||
        app_units.height <= 0 ||
        app_units.margin_top < 0 ||
        app_units.margin_right < 0 ||
        app_units.margin_bottom < 0 ||
        app_units.margin_left < 0 ||
        app_units.available_inline_size <= 0 ||
        app_units.available_block_size <= 0
    {
        return false;
    }
    let horizontal_margins = i64::from(app_units.margin_left) + i64::from(app_units.margin_right);
    let vertical_margins = i64::from(app_units.margin_top) + i64::from(app_units.margin_bottom);
    if i64::from(app_units.width) - horizontal_margins != i64::from(app_units.available_inline_size) ||
        i64::from(app_units.height) - vertical_margins !=
            i64::from(app_units.available_block_size)
    {
        return false;
    }

    [
        (app_units.width, page.width),
        (app_units.height, page.height),
        (app_units.margin_top, page.margin_top),
        (app_units.margin_right, page.margin_right),
        (app_units.margin_bottom, page.margin_bottom),
        (app_units.margin_left, page.margin_left),
        (app_units.available_inline_size, page.available_inline_size),
        (app_units.available_block_size, page.available_block_size),
    ]
    .into_iter()
    .all(|(exact, compatibility)| app_units_to_f32_px(exact) == compatibility)
}

#[derive(Clone)]
struct PositionedOperation {
    sequence: usize,
    // Synthetic operations can use a descendant paint event for ordering without inheriting that
    // descendant's repeated-header membership.
    structural_fragment_index: Option<usize>,
    bounds: Rect,
    operation: Operation,
    authority: Option<CapturedOperationAuthority>,
}

struct DistributedOperations {
    pages: Vec<Page>,
    page_operations: Vec<Vec<Option<CapturedOperationAuthority>>>,
}

fn distribute_operations(
    pages: &[CapturePage],
    operations: Vec<PositionedOperation>,
    paint_events: &[CapturePaintEvent],
    repeats: &[CaptureTableGroupRepeat],
    repeated_header_fragments: &HashMap<u64, RepeatedTableHeaderFragments>,
) -> Result<DistributedOperations, CaptureError> {
    let operations = split_solid_rect_operations(pages, operations)?;
    let mut positioned_pages = vec![Vec::new(); pages.len()];
    let mut repeated_operations = vec![Vec::new(); pages.len()];

    for repeat in repeats {
        if repeat.page_index == 0 ||
            repeat.page_index >= pages.len() ||
            !nonnegative_finite(repeat.source_block_start) ||
            !nonnegative_finite(repeat.target_block_start) ||
            !positive_finite(repeat.block_size) ||
            !repeat.app_unit_authority_matches()
        {
            return Err(CaptureError::InvalidTableGroupRepeat {
                page_index: repeat.page_index,
            });
        }
        let fragments = repeated_header_fragments.get(&repeat.header_tag_id).ok_or(
            CaptureError::MissingRepeatedTableHeader {
                tag_id: repeat.header_tag_id,
            },
        )?;
        let translation =
            f64::from(repeat.target_block_start) - f64::from(repeat.source_block_start);
        for operation in operations.iter().filter(|operation| {
            if let Some(fragment_index) = operation.structural_fragment_index {
                return fragments.fragment_indices.contains(&fragment_index);
            }
            paint_events
                .get(operation.sequence)
                .and_then(|event| event.fragment_id)
                .is_some_and(|fragment_id| fragments.paint_fragment_ids.contains(&fragment_id))
        }) {
            if matches!(&operation.operation, Operation::Path { .. }) &&
                !is_splittable_rect_path(&operation.operation)
            {
                return Err(CaptureError::RepeatedTableHeaderPathUnsupported {
                    sequence: operation.sequence,
                    tag_id: repeat.header_tag_id,
                    page_index: repeat.page_index,
                });
            }
            let mut repeated = operation.clone();
            // Use the layout-owned integer transform, never recover coordinates
            // from compatibility floats. Missing or overflowing authority stays
            // absent, and operation_page may still discard it after clipping.
            repeated.authority = repeated.authority.take().and_then(|authority| {
                let exact = repeat.app_units?;
                translate_operation_authority_y(
                    authority,
                    i64::from(exact.source_block_start) - i64::from(exact.target_block_start),
                )
            });
            repeated.bounds.y += translation;
            translate_operation_y(&mut repeated.operation, -translation, repeated.sequence)?;
            let (page_index, page_origin) = operation_page(pages, &mut repeated)?;
            if page_index != repeat.page_index {
                return Err(CaptureError::InvalidTableGroupRepeat {
                    page_index: repeat.page_index,
                });
            }
            translate_positioned_operation_to_page(
                &mut repeated,
                page_origin,
                page_origin_app_units(pages, page_index),
            )?;
            mark_repeated_table_header(&mut repeated.operation);
            repeated_operations[page_index].push(repeated);
        }
    }

    for mut positioned in operations {
        let (page_index, page_origin) = operation_page(pages, &mut positioned)?;
        translate_positioned_operation_to_page(
            &mut positioned,
            page_origin,
            page_origin_app_units(pages, page_index),
        )?;
        positioned_pages[page_index].push(positioned);
    }

    let mut scene_pages = Vec::with_capacity(pages.len());
    let mut page_operations = Vec::with_capacity(pages.len());
    for ((page, mut positioned), mut repeated) in
        pages.iter().zip(positioned_pages).zip(repeated_operations)
    {
        repeated.append(&mut positioned);
        let (operations, authority) = repeated
            .into_iter()
            .map(|positioned| (positioned.operation, positioned.authority))
            .unzip();
        scene_pages.push(Page {
            size: Size {
                width: f64::from(page.width),
                height: f64::from(page.height),
            },
            operations,
        });
        page_operations.push(authority);
    }
    Ok(DistributedOperations {
        pages: scene_pages,
        page_operations,
    })
}

fn page_origin_app_units(pages: &[CapturePage], page_index: usize) -> Option<i64> {
    pages[..page_index].iter().try_fold(0_i64, |origin, page| {
        origin.checked_add(i64::from(page.app_units?.height))
    })
}

fn translate_positioned_operation_to_page(
    positioned: &mut PositionedOperation,
    page_origin: f64,
    page_origin_app_units: Option<i64>,
) -> Result<(), CaptureError> {
    translate_operation_y(&mut positioned.operation, page_origin, positioned.sequence)?;
    if let Some(authority) = positioned.authority.take() {
        positioned.authority = page_origin_app_units
            .and_then(|origin| translate_operation_authority_y(authority, origin));
    }
    if is_page_spanning_rect_path(&positioned.operation) &&
        let Some(CapturedOperationAuthority::Bounds(exact)) = positioned.authority.as_ref()
    {
        // Compatibility geometry follows the translated integers, not a sum of
        // rounded page heights. No authority is created here.
        let exact = *exact;
        set_positioned_rect_app_units(positioned, exact);
    }
    Ok(())
}

fn translate_operation_authority_y(
    authority: CapturedOperationAuthority,
    page_origin: i64,
) -> Option<CapturedOperationAuthority> {
    let translate_y = |value: i32| {
        i64::from(value)
            .checked_sub(page_origin)
            .and_then(|value| i32::try_from(value).ok())
    };
    let translate_rect = |mut bounds: CapturedRectAppUnits| {
        bounds.y = translate_y(bounds.y)?;
        Some(bounds)
    };

    match authority {
        CapturedOperationAuthority::Text {
            font_size_app_units,
            glyphs,
        } => Some(CapturedOperationAuthority::Text {
            font_size_app_units,
            glyphs: glyphs
                .into_iter()
                .map(|mut glyph| {
                    glyph.y = translate_y(glyph.y)?;
                    Some(glyph)
                })
                .collect::<Option<Vec<_>>>()?,
        }),
        CapturedOperationAuthority::Bounds(bounds) => {
            Some(CapturedOperationAuthority::Bounds(translate_rect(bounds)?))
        },
    }
}

fn rect_from_app_units(exact: CapturedRectAppUnits) -> Rect {
    Rect {
        x: f64::from(app_units_to_f32_px(exact.x)),
        y: f64::from(app_units_to_f32_px(exact.y)),
        width: f64::from(app_units_to_f32_px(exact.width)),
        height: f64::from(app_units_to_f32_px(exact.height)),
    }
}

fn set_positioned_rect_app_units(
    positioned: &mut PositionedOperation,
    exact: CapturedRectAppUnits,
) {
    let Operation::Path { bounds, data, .. } = &mut positioned.operation else {
        unreachable!("exact rectangle placement requires a path")
    };
    *bounds = rect_from_app_units(exact);
    *data = rectangle_path_data(bounds);
    positioned.bounds = bounds.clone();
}

fn solid_rect_app_unit_authority(
    positioned: &PositionedOperation,
) -> Result<Option<CapturedRectAppUnits>, CaptureError> {
    if !is_page_spanning_rect_path(&positioned.operation) {
        return Ok(None);
    }
    let exact = match &positioned.authority {
        None => return Ok(None),
        Some(CapturedOperationAuthority::Bounds(exact)) => *exact,
        Some(_) => {
            return Err(CaptureError::InvalidScene(
                "solid rectangle has non-bounds authority",
            ));
        },
    };
    if !matches!(&positioned.operation,
        Operation::Path { bounds, data, fill: Some(_), stroke: None, .. }
        if bounds == &positioned.bounds && data == &rectangle_path_data(bounds)) ||
        exact.x < 0 ||
        exact.y < 0 ||
        exact.width <= 0 ||
        exact.height <= 0 ||
        exact.x.checked_add(exact.width).is_none() ||
        exact.y.checked_add(exact.height).is_none()
    {
        return Err(CaptureError::InvalidScene(
            "invalid exact solid rectangle authority",
        ));
    }
    Ok(Some(exact))
}

fn exact_page_heights(pages: &[CapturePage]) -> Result<Option<Vec<i32>>, CaptureError> {
    let Some(exact_pages) = pages
        .iter()
        .map(|page| page.app_units)
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    if !pages.iter().zip(&exact_pages).all(|(page, exact)| {
        page.style_source == Some(CapturedPageStyleSource::RequestDefaults) &&
            valid_page_app_unit_authority(page, *exact)
    }) {
        return Err(CaptureError::InvalidPageGeometryAuthority);
    }
    Ok(Some(
        exact_pages.into_iter().map(|page| page.height).collect(),
    ))
}

fn split_solid_rect_operations(
    pages: &[CapturePage],
    operations: Vec<PositionedOperation>,
) -> Result<Vec<PositionedOperation>, CaptureError> {
    let exact_heights = exact_page_heights(pages)?;
    let mut split = Vec::with_capacity(operations.len());
    for operation in operations {
        if !is_page_spanning_rect_path(&operation.operation) || operation.bounds.height == 0.0 {
            split.push(operation);
            continue;
        }

        let exact = solid_rect_app_unit_authority(&operation)?;
        if exact.is_some_and(|exact| operation.bounds != rect_from_app_units(exact)) {
            return Err(CaptureError::InvalidScene(
                "solid rectangle differs from source app-unit authority",
            ));
        }
        if let (Some(exact), Some(heights)) = (exact, exact_heights.as_ref()) {
            let top = i64::from(exact.y);
            let bottom = top
                .checked_add(i64::from(exact.height))
                .ok_or(CaptureError::InvalidPageGeometryAuthority)?;
            let mut origin = 0_i64;
            let mut covered = 0_i64;
            for height in heights {
                let end = origin
                    .checked_add(i64::from(*height))
                    .ok_or(CaptureError::InvalidPageGeometryAuthority)?;
                let intersection_top = top.max(origin);
                let intersection_bottom = bottom.min(end);
                if intersection_top < intersection_bottom {
                    let mut part = operation.clone();
                    let part_exact = CapturedRectAppUnits {
                        y: i32::try_from(intersection_top)
                            .map_err(|_| CaptureError::InvalidPageGeometryAuthority)?,
                        height: i32::try_from(intersection_bottom - intersection_top)
                            .map_err(|_| CaptureError::InvalidPageGeometryAuthority)?,
                        ..exact
                    };
                    set_positioned_rect_app_units(&mut part, part_exact);
                    part.authority = Some(CapturedOperationAuthority::Bounds(part_exact));
                    covered = covered
                        .checked_add(i64::from(part_exact.height))
                        .ok_or(CaptureError::InvalidPageGeometryAuthority)?;
                    split.push(part);
                }
                origin = end;
            }
            if covered != i64::from(exact.height) {
                return Err(CaptureError::OperationOutsidePageSequence {
                    sequence: operation.sequence,
                });
            }
            continue;
        }

        let top = operation.bounds.y;
        let bottom = top + operation.bounds.height;
        let mut page_origin = 0.0;
        let mut covered_height = 0.0;
        let split_start = split.len();
        for page in pages {
            let page_end = page_origin + f64::from(page.height);
            let intersection_top = top.max(page_origin);
            let intersection_bottom = bottom.min(page_end);
            if intersection_bottom > intersection_top {
                let mut part = operation.clone();
                part.bounds.y = intersection_top;
                part.bounds.height = intersection_bottom - intersection_top;
                let Operation::Path { bounds, data, .. } = &mut part.operation else {
                    return Err(CaptureError::InvalidScene(
                        "splittable rectangle operation is not a path",
                    ));
                };
                bounds.y = intersection_top;
                bounds.height = intersection_bottom - intersection_top;
                *data = rectangle_path_data(bounds);
                covered_height += part.bounds.height;
                split.push(part);
            }
            page_origin = page_end;
        }
        if (covered_height - operation.bounds.height).abs() > 0.000_001 {
            return Err(CaptureError::OperationOutsidePageSequence {
                sequence: operation.sequence,
            });
        }
        let part_count = split.len() - split_start;
        if part_count != 1 || split[split_start].bounds != operation.bounds {
            for part in &mut split[split_start..] {
                // Splitting and clipping currently use f64 page intersections. Retaining the
                // unsplit source rectangle here would give a future encoder stale geometry.
                part.authority = None;
            }
        }
    }
    Ok(split)
}

fn mark_repeated_table_header(operation: &mut Operation) {
    let meta = match operation {
        Operation::Text { meta, .. } |
        Operation::Image { meta, .. } |
        Operation::Link { meta, .. } |
        Operation::Path { meta, .. } => meta,
    };
    meta.semantics = Some(Semantics {
        role: "artifact".into(),
        label: Some("repeated-table-header".into()),
    });
}

fn operation_page(
    pages: &[CapturePage],
    operation: &mut PositionedOperation,
) -> Result<(usize, f64), CaptureError> {
    if let Some(exact) = solid_rect_app_unit_authority(operation)? &&
        let Some(heights) = exact_page_heights(pages)?
    {
        // Source compatibility is checked before splitting. A repeated header
        // can subsequently translate these integers and its compatibility
        // floats independently; page ownership must follow only the integers.
        let top = i64::from(exact.y);
        let bottom = top
            .checked_add(i64::from(exact.height))
            .ok_or(CaptureError::InvalidPageGeometryAuthority)?;
        let mut origin = 0_i64;
        let mut compatibility_origin = 0.0;
        for (page_index, (page, height)) in pages.iter().zip(heights).enumerate() {
            let end = origin
                .checked_add(i64::from(height))
                .ok_or(CaptureError::InvalidPageGeometryAuthority)?;
            if top >= origin && top < end {
                if bottom > end {
                    return Err(CaptureError::OperationCrossesPageBoundary {
                        sequence: operation.sequence,
                        page_index,
                    });
                }
                return Ok((page_index, compatibility_origin));
            }
            origin = end;
            compatibility_origin += f64::from(page.height);
        }
        return Err(CaptureError::OperationOutsidePageSequence {
            sequence: operation.sequence,
        });
    }
    let top = operation.bounds.y;
    let bottom = top + operation.bounds.height;
    let mut origin = 0.0;
    for (page_index, page) in pages.iter().enumerate() {
        let end = origin + f64::from(page.height);
        let is_last_zero_height_edge = page_index + 1 == pages.len() && top == end && bottom == top;
        if top >= origin && (top < end || is_last_zero_height_edge) {
            if bottom > end {
                let maximum_centered_edge_overrun =
                    operation.bounds.width.min(operation.bounds.height);
                if bottom - end > maximum_centered_edge_overrun {
                    return Err(CaptureError::OperationCrossesPageBoundary {
                        sequence: operation.sequence,
                        page_index,
                    });
                }
                let Operation::Path {
                    bounds, data, meta, ..
                } = &mut operation.operation
                else {
                    return Err(CaptureError::OperationCrossesPageBoundary {
                        sequence: operation.sequence,
                        page_index,
                    });
                };
                if !is_splittable_rect_meta(meta) {
                    return Err(CaptureError::OperationCrossesPageBoundary {
                        sequence: operation.sequence,
                        page_index,
                    });
                }
                operation.bounds.height = end - top;
                bounds.height = operation.bounds.height;
                *data = rectangle_path_data(bounds);
                // The centered-edge clamp is currently calculated in f64 compatibility geometry.
                operation.authority = None;
            }
            return Ok((page_index, origin));
        }
        origin = end;
    }
    Err(CaptureError::OperationOutsidePageSequence {
        sequence: operation.sequence,
    })
}

fn translate_operation_y(
    operation: &mut Operation,
    page_origin: f64,
    sequence: usize,
) -> Result<(), CaptureError> {
    match operation {
        Operation::Text { glyphs, .. } => {
            for glyph in glyphs {
                glyph.y -= page_origin;
            }
        },
        Operation::Path {
            bounds, data, meta, ..
        } if is_splittable_rect_meta(meta) => {
            bounds.y -= page_origin;
            *data = rectangle_path_data(bounds);
        },
        Operation::Path { .. } if page_origin != 0.0 => {
            return Err(CaptureError::PageLocalPathUnsupported { sequence });
        },
        Operation::Path { .. } => {},
        Operation::Image { bounds, .. } | Operation::Link { bounds, .. } => {
            bounds.y -= page_origin;
        },
    }
    Ok(())
}

fn is_splittable_rect_path(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Path { meta, .. } if is_splittable_rect_meta(meta)
    )
}

fn is_page_spanning_rect_path(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Path { meta, .. }
            if meta.semantics.as_ref().is_some_and(|semantics| {
                semantics.role == "artifact"
                    && matches!(semantics.label.as_deref(), Some("background" | "border"))
            })
    )
}

fn is_splittable_rect_meta(meta: &OperationMeta) -> bool {
    meta.semantics.as_ref().is_some_and(|semantics| {
        semantics.role == "artifact" &&
            matches!(
                semantics.label.as_deref(),
                Some("table-border" | "background" | "border")
            )
    })
}

fn rectangle_path_data(bounds: &Rect) -> String {
    format!(
        "M{} {}h{}v{}h-{}z",
        bounds.x, bounds.y, bounds.width, bounds.height, bounds.width
    )
}

fn positive_finite(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

fn nonnegative_finite(value: f32) -> bool {
    value.is_finite() && value >= 0.0 && !value.is_sign_negative()
}

fn collect_font_resources(
    resources: Vec<CaptureFontResource>,
) -> Result<BTreeMap<String, CapturedFontResource>, CaptureError> {
    let mut by_id = BTreeMap::new();
    for resource in resources {
        let expected_digest = content_address_digest(&resource.resource)?;
        let bytes = BASE64_STANDARD
            .decode(&resource.bytes_base64)
            .map_err(|_| CaptureError::InvalidFontResourceBase64(resource.resource.clone()))?;
        if BASE64_STANDARD.encode(&bytes) != resource.bytes_base64 {
            return Err(CaptureError::InvalidFontResourceBase64(resource.resource));
        }
        let actual_digest: [u8; 32] = Sha256::digest(&bytes).into();
        if actual_digest != expected_digest {
            return Err(CaptureError::FontResourceDigestMismatch {
                resource: resource.resource,
                actual: content_address(&actual_digest),
            });
        }
        let id = resource.resource.clone();
        if by_id
            .insert(
                id.clone(),
                CapturedFontResource {
                    resource: resource.resource,
                    bytes_base64: resource.bytes_base64,
                },
            )
            .is_some()
        {
            return Err(CaptureError::DuplicateFontResource(id));
        }
    }
    Ok(by_id)
}

fn collect_font_instances(
    instances: Vec<CaptureFontInstance>,
    resources: &BTreeMap<String, CapturedFontResource>,
) -> Result<BTreeMap<String, CapturedFontInstance>, CaptureError> {
    let mut by_id = BTreeMap::new();
    for instance in instances {
        ensure_content_address(&instance.id)?;
        ensure_content_address(&instance.resource)?;
        if !resources.contains_key(&instance.resource) {
            return Err(CaptureError::MissingFontResource {
                instance: instance.id,
                resource: instance.resource,
            });
        }

        let mut variations = instance
            .variations
            .into_iter()
            .map(|variation| {
                if !variation.value.is_finite() {
                    return Err(CaptureError::InvalidFontVariation(instance.id.clone()));
                }
                Ok(CapturedFontVariation {
                    tag: variation.tag,
                    value: if variation.value == 0.0 {
                        0.0
                    } else {
                        variation.value
                    },
                })
            })
            .collect::<Result<Vec<_>, CaptureError>>()?;
        variations.sort_unstable_by_key(|variation| (variation.tag, variation.value.to_bits()));

        let resource_digest = content_address_digest(&instance.resource)?;
        let actual_id = font_instance_id(
            &resource_digest,
            instance.face_index,
            &variations,
            instance.synthetic_bold,
        );
        if actual_id != instance.id {
            return Err(CaptureError::FontInstanceIdMismatch {
                instance: instance.id,
                actual: actual_id,
            });
        }

        let id = instance.id.clone();
        if by_id
            .insert(
                id.clone(),
                CapturedFontInstance {
                    id: instance.id,
                    resource: instance.resource,
                    face_index: instance.face_index,
                    variations,
                    synthetic_bold: instance.synthetic_bold,
                },
            )
            .is_some()
        {
            return Err(CaptureError::DuplicateFontInstance(id));
        }
    }
    Ok(by_id)
}

fn collect_fragments(
    fragments: Vec<CaptureFragment>,
) -> Result<HashMap<usize, CaptureFragment>, CaptureError> {
    let mut by_id = HashMap::new();
    for fragment in fragments {
        let Some(id) = fragment.paint_fragment_id else {
            continue;
        };
        if by_id.insert(id, fragment).is_some() {
            return Err(CaptureError::DuplicateFragmentJoin);
        }
    }
    Ok(by_id)
}

fn unsupported_box_subtree_fragments(
    fragments: &[CaptureFragment],
    events: &[CapturePaintEvent],
) -> HashSet<usize> {
    let roots = events
        .iter()
        .filter(|event| event.kind == "box")
        .filter_map(|event| event.fragment_id)
        .collect::<HashSet<_>>();
    let mut unsupported = HashSet::new();
    for (index, root) in fragments.iter().enumerate() {
        if !root
            .paint_fragment_id
            .is_some_and(|fragment_id| roots.contains(&fragment_id))
        {
            continue;
        }
        for (offset, fragment) in fragments[index..].iter().enumerate() {
            if offset > 0 && fragment.depth <= root.depth {
                break;
            }
            unsupported.extend(fragment.paint_fragment_id);
        }
    }
    unsupported
}

fn collect_links(links: Vec<CaptureLink>) -> Result<HashMap<u64, String>, CaptureError> {
    let mut by_tag = HashMap::new();
    for link in links {
        if by_tag.insert(link.tag_id, link.url).is_some() {
            return Err(CaptureError::DuplicateLinkTag);
        }
    }
    Ok(by_tag)
}

struct BoxLinkPlacement {
    fragment_index: usize,
    bounds: Rect,
    app_units: Option<CapturedRectAppUnits>,
    target: String,
}

fn collect_box_link_placements(
    fragments: &[CaptureFragment],
    paint_events: &[CapturePaintEvent],
    links: &HashMap<u64, String>,
) -> Result<BTreeMap<usize, Vec<BoxLinkPlacement>>, CaptureError> {
    let mut first_sequence_by_fragment = HashMap::new();
    for event in paint_events {
        if let Some(fragment_id) = event.fragment_id {
            first_sequence_by_fragment
                .entry(fragment_id)
                .or_insert(event.sequence);
        }
    }
    let directly_linked_fragments = paint_events
        .iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "text" |
                    "image" |
                    "box" |
                    "root-background" |
                    "outline" |
                    "collapsed-table-borders" |
                    "iframe" |
                    "text-effects" |
                    "content-geometry"
            )
        })
        .filter_map(|event| event.fragment_id)
        .collect::<HashSet<_>>();

    let mut by_sequence = BTreeMap::<usize, Vec<BoxLinkPlacement>>::new();
    for (fragment_index, fragment) in fragments.iter().enumerate() {
        if fragment.kind != "box" {
            continue;
        }
        let Some(tag_id) = fragment.tag_id else {
            continue;
        };
        let Some(target) = links.get(&tag_id) else {
            continue;
        };
        let subtree_end = fragments[fragment_index + 1..]
            .iter()
            .position(|descendant| descendant.depth <= fragment.depth)
            .map(|offset| fragment_index + offset + 1)
            .unwrap_or(fragments.len());
        let subtree = &fragments[fragment_index..subtree_end];
        if subtree
            .iter()
            .filter(|candidate| candidate.tag_id == Some(tag_id))
            .filter_map(|candidate| candidate.paint_fragment_id)
            .any(|fragment_id| directly_linked_fragments.contains(&fragment_id))
        {
            continue;
        }
        let sequence = subtree
            .iter()
            .filter_map(|candidate| candidate.paint_fragment_id)
            .filter_map(|fragment_id| first_sequence_by_fragment.get(&fragment_id).copied())
            .min();
        let Some(sequence) = sequence else {
            continue;
        };
        let rect = fragment
            .rect
            .as_ref()
            .ok_or(CaptureError::MissingLinkRect { sequence })?;
        by_sequence
            .entry(sequence)
            .or_default()
            .push(BoxLinkPlacement {
                fragment_index,
                bounds: rect.into_scene_rect(),
                app_units: rect.app_units,
                target: target.clone(),
            });
    }
    Ok(by_sequence)
}

fn joined_fragment<'a>(
    event: &CapturePaintEvent,
    fragments: &'a HashMap<usize, CaptureFragment>,
) -> Result<(usize, &'a CaptureFragment), CaptureError> {
    let fragment_id = event
        .fragment_id
        .ok_or_else(|| CaptureError::MissingFragmentJoin {
            sequence: event.sequence,
            kind: event.kind.clone(),
        })?;
    let fragment =
        fragments
            .get(&fragment_id)
            .ok_or_else(|| CaptureError::MissingFragmentJoin {
                sequence: event.sequence,
                kind: event.kind.clone(),
            })?;
    if event.tag_id != fragment.tag_id {
        return Err(CaptureError::FragmentTagMismatch {
            sequence: event.sequence,
        });
    }
    Ok((fragment_id, fragment))
}

fn optionally_joined_fragment<'a>(
    event: &CapturePaintEvent,
    fragments: &'a HashMap<usize, CaptureFragment>,
) -> Option<(usize, &'a CaptureFragment)> {
    let fragment_id = event.fragment_id?;
    let fragment = fragments.get(&fragment_id)?;
    (event.tag_id == fragment.tag_id).then_some((fragment_id, fragment))
}

fn append_link(
    operations: &mut Vec<PositionedOperation>,
    emitted: &mut HashSet<(usize, u64)>,
    links: &HashMap<u64, String>,
    fragment_id: usize,
    fragment: &CaptureFragment,
    sequence: usize,
) -> Result<(), CaptureError> {
    let Some(tag_id) = fragment.tag_id else {
        return Ok(());
    };
    let Some(target) = links.get(&tag_id) else {
        return Ok(());
    };
    if !emitted.insert((fragment_id, tag_id)) {
        return Ok(());
    }
    let bounds = fragment
        .rect
        .as_ref()
        .ok_or(CaptureError::MissingLinkRect { sequence })?
        .into_scene_rect();
    let authority = fragment
        .rect
        .as_ref()
        .and_then(|rect| rect.app_units)
        .map(CapturedOperationAuthority::Bounds);
    operations.push(PositionedOperation {
        sequence,
        structural_fragment_index: None,
        bounds: bounds.clone(),
        authority,
        operation: Operation::Link {
            bounds,
            target: target.clone(),
            meta: OperationMeta::default(),
        },
    });
    Ok(())
}

fn append_box_links(
    operations: &mut Vec<PositionedOperation>,
    placements: Option<&[BoxLinkPlacement]>,
    sequence: usize,
) {
    let Some(placements) = placements else {
        return;
    };
    for placement in placements {
        operations.push(PositionedOperation {
            sequence,
            structural_fragment_index: Some(placement.fragment_index),
            bounds: placement.bounds.clone(),
            // The unpainted anchor owns this rectangle, not its painted descendant.
            authority: placement.app_units.map(CapturedOperationAuthority::Bounds),
            operation: Operation::Link {
                bounds: placement.bounds.clone(),
                target: placement.target.clone(),
                meta: OperationMeta::default(),
            },
        });
    }
}

fn append_vector_image(
    operations: &mut Vec<PositionedOperation>,
    unsupported_events: &mut Vec<UnsupportedPaintEvent>,
    embedded_image_resources: &mut BTreeMap<String, CanvasResource>,
    font_resources: &mut BTreeMap<String, CapturedFontResource>,
    font_instances: &mut BTreeMap<String, CapturedFontInstance>,
    font_selections: &mut BTreeMap<
        (String, CapturedFontSource, Vec<String>, Option<String>),
        CapturedFontSelection,
    >,
    vector_image: &CaptureVectorImage,
    rect: &CaptureRect,
    sequence: usize,
) -> Result<(), CaptureError> {
    if !positive_finite(vector_image.viewport_width) ||
        !positive_finite(vector_image.viewport_height)
    {
        return Err(CaptureError::InvalidVectorImageGeometry { sequence });
    }
    let scale_x = f64::from(rect.width) / f64::from(vector_image.viewport_width);
    let scale_y = f64::from(rect.height) / f64::from(vector_image.viewport_height);
    let point = |x: f32, y: f32| {
        (
            f64::from(rect.x) + f64::from(x) * scale_x,
            f64::from(rect.y) + f64::from(y) * scale_y,
        )
    };

    for item in &vector_image.items {
        match item {
            CaptureVectorImageItem::Path {
                segments,
                fill,
                fill_rule,
                stroke,
            } => {
                let mut path = BezPath::new();
                for segment in segments {
                    match *segment {
                        CaptureVectorPathSegment::MoveTo { x, y } => path.move_to(point(x, y)),
                        CaptureVectorPathSegment::LineTo { x, y } => path.line_to(point(x, y)),
                        CaptureVectorPathSegment::QuadTo { x1, y1, x, y } => {
                            path.quad_to(point(x1, y1), point(x, y));
                        },
                        CaptureVectorPathSegment::CubicTo {
                            x1,
                            y1,
                            x2,
                            y2,
                            x,
                            y,
                        } => path.curve_to(point(x1, y1), point(x2, y2), point(x, y)),
                        CaptureVectorPathSegment::Close => path.close_path(),
                    }
                }
                if path.elements().is_empty() {
                    return Err(CaptureError::InvalidVectorPath { sequence });
                }
                let path_bounds = path.bounding_box();
                let bounds = Rect {
                    x: path_bounds.x0,
                    y: path_bounds.y0,
                    width: path_bounds.width(),
                    height: path_bounds.height(),
                };
                let stroke = match stroke {
                    Some(stroke) if approximately_equal_f64(scale_x, scale_y) => Some(Stroke {
                        color: vector_color(&stroke.color),
                        width: f64::from(stroke.width) * scale_x,
                    }),
                    Some(_) => {
                        unsupported_events.push(UnsupportedPaintEvent {
                            sequence,
                            kind: UnsupportedPaintKind::SvgStroke,
                        });
                        None
                    },
                    None => None,
                };
                operations.push(PositionedOperation {
                    sequence,
                    structural_fragment_index: None,
                    bounds: bounds.clone(),
                    authority: None,
                    operation: Operation::Path {
                        bounds,
                        data: path.to_svg(),
                        fill: fill.as_ref().map(vector_color),
                        fill_rule: match fill_rule {
                            CaptureVectorFillRule::NonZero => FillRule::NonZero,
                            CaptureVectorFillRule::EvenOdd => FillRule::EvenOdd,
                        },
                        stroke,
                        meta: OperationMeta::default(),
                    },
                });
            },
            CaptureVectorImageItem::Image {
                x,
                y,
                width,
                height,
                resource,
                bytes_base64,
            } => {
                if !positive_finite(*width) || !positive_finite(*height) {
                    return Err(CaptureError::InvalidVectorImageGeometry { sequence });
                }
                let bytes = decode_vector_resource(resource, bytes_base64)?;
                if let Some(existing) = embedded_image_resources.get(resource) &&
                    existing.png != bytes
                {
                    return Err(CaptureError::ConflictingVectorResource(resource.clone()));
                }
                embedded_image_resources
                    .entry(resource.clone())
                    .or_insert_with(|| CanvasResource {
                        resource: resource.clone(),
                        png: bytes,
                    });
                let (x, y) = point(*x, *y);
                let bounds = Rect {
                    x,
                    y,
                    width: f64::from(*width) * scale_x,
                    height: f64::from(*height) * scale_y,
                };
                operations.push(PositionedOperation {
                    sequence,
                    structural_fragment_index: None,
                    bounds: bounds.clone(),
                    authority: None,
                    operation: Operation::Image {
                        bounds,
                        resource: resource.clone(),
                        meta: OperationMeta::default(),
                    },
                });
            },
            CaptureVectorImageItem::Text {
                text,
                font,
                font_size,
                glyphs,
            } => {
                if !approximately_equal_f64(scale_x, scale_y) {
                    unsupported_events.push(UnsupportedPaintEvent {
                        sequence,
                        kind: UnsupportedPaintKind::SvgText,
                    });
                    continue;
                }
                let bytes = decode_vector_font_resource(font)?;
                merge_vector_font_resource(font_resources, font, &bytes)?;
                let resource_digest = content_address_digest(&font.resource)?;
                let instance_id = font_instance_id(&resource_digest, font.face_index, &[], false);
                let instance = CapturedFontInstance {
                    id: instance_id.clone(),
                    resource: font.resource.clone(),
                    face_index: font.face_index,
                    variations: Vec::new(),
                    synthetic_bold: false,
                };
                if let Some(existing) = font_instances.get(&instance_id) {
                    if existing != &instance {
                        return Err(CaptureError::DuplicateFontInstance(instance_id));
                    }
                } else {
                    font_instances.insert(instance_id.clone(), instance);
                }
                let requested_families = vec![font.family.clone()];
                let selected_family = Some(font.family.clone());
                let selection_key = (
                    instance_id.clone(),
                    CapturedFontSource::Memory,
                    requested_families.clone(),
                    selected_family.clone(),
                );
                font_selections
                    .entry(selection_key)
                    .or_insert_with(|| CapturedFontSelection {
                        instance: instance_id.clone(),
                        resource: font.resource.clone(),
                        face_index: font.face_index,
                        source: CapturedFontSource::Memory,
                        requested_families,
                        selected_family,
                    });
                let glyphs = glyphs
                    .iter()
                    .map(|glyph| {
                        let (x, y) = point(glyph.x, glyph.y);
                        Glyph {
                            id: glyph.id,
                            x,
                            y,
                            advance: f64::from(glyph.advance) * scale_x,
                            text_range: Some(crate::Utf8Range {
                                start: glyph.text_start,
                                end: glyph.text_end,
                            }),
                        }
                    })
                    .collect();
                let bounds = rect.into_scene_rect();
                operations.push(PositionedOperation {
                    sequence,
                    structural_fragment_index: None,
                    bounds,
                    authority: None,
                    operation: Operation::Text {
                        text: text.clone(),
                        font: instance_id,
                        font_size: f64::from(*font_size) * scale_x,
                        color: Color::default(),
                        glyphs,
                        meta: OperationMeta::default(),
                    },
                });
            },
            CaptureVectorImageItem::Unsupported { reason } => {
                unsupported_events.push(UnsupportedPaintEvent {
                    sequence,
                    kind: match reason {
                        CaptureVectorUnsupportedReason::Animation => {
                            UnsupportedPaintKind::SvgAnimation
                        },
                        CaptureVectorUnsupportedReason::Compositing => {
                            UnsupportedPaintKind::SvgCompositing
                        },
                        CaptureVectorUnsupportedReason::Stroke => UnsupportedPaintKind::SvgStroke,
                        CaptureVectorUnsupportedReason::Paint => UnsupportedPaintKind::SvgPaint,
                        CaptureVectorUnsupportedReason::Image => UnsupportedPaintKind::SvgImage,
                        CaptureVectorUnsupportedReason::Text => UnsupportedPaintKind::SvgText,
                        CaptureVectorUnsupportedReason::InvalidPath => {
                            UnsupportedPaintKind::SvgInvalidPath
                        },
                    },
                });
            },
        }
    }
    Ok(())
}

fn vector_color(color: &CaptureVectorColor) -> Color {
    Color {
        r: f64::from(color.red) / 255.0,
        g: f64::from(color.green) / 255.0,
        b: f64::from(color.blue) / 255.0,
        a: f64::from(color.alpha),
    }
}

fn approximately_equal_f64(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= scale * 1.0e-8
}

fn decode_vector_resource(resource: &str, bytes_base64: &str) -> Result<Vec<u8>, CaptureError> {
    ensure_content_address(resource)?;
    let bytes = BASE64_STANDARD
        .decode(bytes_base64)
        .map_err(|_| CaptureError::InvalidVectorImageBase64(resource.into()))?;
    if BASE64_STANDARD.encode(&bytes) != bytes_base64 {
        return Err(CaptureError::InvalidVectorImageBase64(resource.into()));
    }
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let actual = content_address(&digest);
    if actual != resource {
        return Err(CaptureError::VectorImageDigestMismatch {
            resource: resource.into(),
            actual,
        });
    }
    Ok(bytes)
}

fn decode_vector_font_resource(font: &CaptureVectorFont) -> Result<Vec<u8>, CaptureError> {
    ensure_content_address(&font.resource)?;
    let bytes = BASE64_STANDARD
        .decode(&font.bytes_base64)
        .map_err(|_| CaptureError::InvalidFontResourceBase64(font.resource.clone()))?;
    if BASE64_STANDARD.encode(&bytes) != font.bytes_base64 {
        return Err(CaptureError::InvalidFontResourceBase64(
            font.resource.clone(),
        ));
    }
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let actual = content_address(&digest);
    if actual != font.resource {
        return Err(CaptureError::FontResourceDigestMismatch {
            resource: font.resource.clone(),
            actual,
        });
    }
    Ok(bytes)
}

fn merge_vector_font_resource(
    resources: &mut BTreeMap<String, CapturedFontResource>,
    font: &CaptureVectorFont,
    bytes: &[u8],
) -> Result<(), CaptureError> {
    if let Some(existing) = resources.get(&font.resource) {
        let existing_bytes = BASE64_STANDARD
            .decode(&existing.bytes_base64)
            .map_err(|_| CaptureError::InvalidFontResourceBase64(font.resource.clone()))?;
        if existing_bytes != bytes {
            return Err(CaptureError::DuplicateFontResource(font.resource.clone()));
        }
        return Ok(());
    }
    resources.insert(
        font.resource.clone(),
        CapturedFontResource {
            resource: font.resource.clone(),
            bytes_base64: font.bytes_base64.clone(),
        },
    );
    Ok(())
}

fn ensure_content_address(resource: &str) -> Result<(), CaptureError> {
    content_address_digest(resource).map(|_| ())
}

fn content_address_digest(resource: &str) -> Result<[u8; 32], CaptureError> {
    let Some(digest) = resource.strip_prefix("sha256:") else {
        return Err(CaptureError::InvalidContentAddress(resource.into()));
    };
    if digest.len() != 64 {
        return Err(CaptureError::InvalidContentAddress(resource.into()));
    }

    let mut decoded = [0u8; 32];
    for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
        let Some(high) = lowercase_hex_nibble(pair[0]) else {
            return Err(CaptureError::InvalidContentAddress(resource.into()));
        };
        let Some(low) = lowercase_hex_nibble(pair[1]) else {
            return Err(CaptureError::InvalidContentAddress(resource.into()));
        };
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

fn lowercase_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn font_instance_id(
    resource_digest: &[u8; 32],
    face_index: u32,
    variations: &[CapturedFontVariation],
    synthetic_bold: bool,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(if synthetic_bold {
        b"pliego-font-instance-v2\0".as_slice()
    } else {
        b"pliego-font-instance-v1\0".as_slice()
    });
    hasher.update(resource_digest);
    hasher.update(face_index.to_be_bytes());
    hasher.update((variations.len() as u64).to_be_bytes());
    for variation in variations {
        hasher.update(variation.tag.to_be_bytes());
        hasher.update(variation.value.to_bits().to_be_bytes());
    }
    if synthetic_bold {
        hasher.update([1]);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    content_address(&digest)
}

fn content_address(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutCapture {
    #[serde(rename = "boxes")]
    _boxes: Vec<CaptureBox>,
    fragments: Vec<CaptureFragment>,
    font_resources: Vec<CaptureFontResource>,
    font_instances: Vec<CaptureFontInstance>,
    page_sequence: Option<CapturePageSequence>,
    paint_events: Vec<CapturePaintEvent>,
    #[serde(rename = "paint_epoch")]
    _paint_epoch: u32,
    #[serde(rename = "paint_content_width")]
    _paint_content_width: f32,
    #[serde(rename = "paint_content_height")]
    _paint_content_height: f32,
    #[serde(rename = "paint_scroll_node_count")]
    _paint_scroll_node_count: usize,
    #[serde(rename = "paintable")]
    _paintable: bool,
    #[serde(rename = "contentful")]
    _contentful: bool,
    #[serde(rename = "first_reflow")]
    _first_reflow: bool,
    links: Vec<CaptureLink>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturePageSequence {
    pages: Vec<CapturePage>,
    #[serde(default, rename = "continuations")]
    _continuations: Vec<serde_json::Value>,
    #[serde(default, rename = "table_breaks")]
    _table_breaks: Vec<serde_json::Value>,
    #[serde(default, rename = "table_cell_continuations")]
    _table_cell_continuations: Vec<serde_json::Value>,
    #[serde(default)]
    table_group_repeats: Vec<CaptureTableGroupRepeat>,
    #[serde(default, rename = "warnings")]
    _warnings: Vec<serde_json::Value>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureTableGroupRepeat {
    page_index: usize,
    #[serde(rename = "table_node")]
    _table_node: Option<u64>,
    header_tag_id: u64,
    #[serde(rename = "row_group_index")]
    _row_group_index: usize,
    source_block_start: f32,
    target_block_start: f32,
    block_size: f32,
    #[serde(default)]
    app_units: Option<CaptureTableGroupRepeatAppUnits>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureTableGroupRepeatAppUnits {
    source_block_start: i32,
    target_block_start: i32,
    block_size: i32,
}

impl CaptureTableGroupRepeat {
    fn app_unit_authority_matches(&self) -> bool {
        self.app_units.is_none_or(|exact| {
            exact.source_block_start >= 0 &&
                exact.target_block_start >= 0 &&
                exact.block_size > 0 &&
                app_units_to_f32_px(exact.source_block_start) == self.source_block_start &&
                app_units_to_f32_px(exact.target_block_start) == self.target_block_start &&
                app_units_to_f32_px(exact.block_size) == self.block_size
        })
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturePage {
    index: usize,
    #[serde(default)]
    style_source: Option<CapturedPageStyleSource>,
    #[serde(default)]
    app_units: Option<CapturedPageAppUnits>,
    width: f32,
    height: f32,
    margin_top: f32,
    margin_right: f32,
    margin_bottom: f32,
    margin_left: f32,
    available_inline_size: f32,
    available_block_size: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureBox {
    #[serde(rename = "depth")]
    _depth: usize,
    #[serde(rename = "kind")]
    _kind: String,
    #[serde(rename = "tag_id")]
    _tag_id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFragment {
    depth: usize,
    kind: String,
    rect: Option<CaptureRect>,
    tag_id: Option<u64>,
    paint_fragment_id: Option<usize>,
    text_run: Option<CaptureTextRun>,
    image_url: Option<String>,
    #[serde(default)]
    canvas_image_key: Option<CapturedCanvasImageKey>,
    #[serde(default)]
    vector_image: Option<CaptureVectorImage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureVectorImage {
    viewport_width: f32,
    viewport_height: f32,
    items: Vec<CaptureVectorImageItem>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum CaptureVectorImageItem {
    Path {
        segments: Vec<CaptureVectorPathSegment>,
        fill: Option<CaptureVectorColor>,
        fill_rule: CaptureVectorFillRule,
        stroke: Option<CaptureVectorStroke>,
    },
    Image {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        resource: String,
        bytes_base64: String,
    },
    Text {
        text: String,
        font: CaptureVectorFont,
        font_size: f32,
        glyphs: Vec<CaptureVectorGlyph>,
    },
    Unsupported {
        reason: CaptureVectorUnsupportedReason,
    },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum CaptureVectorPathSegment {
    MoveTo {
        x: f32,
        y: f32,
    },
    LineTo {
        x: f32,
        y: f32,
    },
    QuadTo {
        x1: f32,
        y1: f32,
        x: f32,
        y: f32,
    },
    CubicTo {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x: f32,
        y: f32,
    },
    Close,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureVectorColor {
    red: u8,
    green: u8,
    blue: u8,
    alpha: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureVectorStroke {
    color: CaptureVectorColor,
    width: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureVectorFont {
    resource: String,
    bytes_base64: String,
    face_index: u32,
    family: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureVectorGlyph {
    id: u32,
    x: f32,
    y: f32,
    advance: f32,
    text_start: u32,
    text_end: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CaptureVectorFillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CaptureVectorUnsupportedReason {
    Animation,
    Compositing,
    Stroke,
    Paint,
    Image,
    Text,
    InvalidPath,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturePaintEvent {
    sequence: usize,
    kind: String,
    fragment_id: Option<usize>,
    tag_id: Option<u64>,
    #[serde(rename = "spatial_node_id")]
    _spatial_node_id: usize,
    #[serde(rename = "clip_id")]
    _clip_id: Option<usize>,
    #[serde(default)]
    table_borders: Vec<CaptureTableBorder>,
    #[serde(default)]
    paint_rects: Vec<CapturePaintRect>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturePaintRect {
    rect: CaptureRect,
    color: CaptureColor,
    kind: CapturePaintRectKind,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CapturePaintRectKind {
    Background,
    Border,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureTableBorder {
    rect: CaptureRect,
    color: CaptureColor,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureColor {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Default for CaptureColor {
    fn default() -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    }
}

impl CaptureColor {
    fn into_scene_color(self) -> Color {
        Color {
            r: f64::from(self.r),
            g: f64::from(self.g),
            b: f64::from(self.b),
            a: f64::from(self.a),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureTextRun {
    text: String,
    font_instance_id: Option<String>,
    font_identifier: serde_json::Value,
    #[serde(default)]
    requested_families: Vec<String>,
    #[serde(default)]
    selected_family: Option<String>,
    font_size: f32,
    #[serde(default)]
    font_size_app_units: Option<i32>,
    #[serde(default)]
    color: CaptureColor,
    glyphs: Vec<CaptureGlyph>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureGlyph {
    id: u32,
    x: f32,
    y: f32,
    advance: f32,
    #[serde(default)]
    app_units: Option<CapturedGlyphAppUnits>,
    #[serde(default)]
    text_range: Option<CaptureUtf8Range>,
}

impl CaptureGlyph {
    fn app_unit_authority_matches(&self) -> bool {
        // The compatibility point is the sum of two independently projected `f32` values
        // (baseline plus shaping offset), so it can legitimately differ from projecting their
        // exact app-unit sum. Advance is a direct projection and must still agree.
        self.app_units
            .is_none_or(|exact| app_units_to_f32_px(exact.advance) == self.advance)
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureUtf8Range {
    start: u32,
    end: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    #[serde(default)]
    app_units: Option<CapturedRectAppUnits>,
}

impl CaptureRect {
    fn into_scene_rect(&self) -> Rect {
        Rect {
            x: f64::from(self.x),
            y: f64::from(self.y),
            width: f64::from(self.width),
            height: f64::from(self.height),
        }
    }

    fn app_unit_authority_matches(&self) -> bool {
        self.app_units.is_none_or(|exact| {
            app_units_to_f32_px(exact.x) == self.x &&
                app_units_to_f32_px(exact.y) == self.y &&
                app_units_to_f32_px(exact.width) == self.width &&
                app_units_to_f32_px(exact.height) == self.height
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureLink {
    tag_id: u64,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFontResource {
    resource: String,
    bytes_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFontInstance {
    id: String,
    resource: String,
    face_index: u32,
    variations: Vec<CaptureFontVariation>,
    #[serde(default)]
    synthetic_bold: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFontVariation {
    tag: u32,
    value: f32,
}

#[cfg(test)]
mod tests {
    use embedder_traits::{
        DocumentCanvasCaptureBinding, DocumentCanvasCaptureStatus, DocumentCanvasImageKey,
        DocumentSettlementSource, DocumentSettlementSourceEpoch, DocumentSettlementSourceSnapshot,
    };
    use servo_base::id::TEST_PIPELINE_ID;

    use super::*;

    fn controlled_canvas_binding(
        image_keys: &[(u32, u32)],
        registry_generation: u64,
    ) -> DocumentCanvasCaptureBinding {
        let sources = image_keys
            .iter()
            .enumerate()
            .map(|(index, &(namespace, key))| {
                DocumentSettlementSource::canvas_2d_internal(
                    TEST_PIPELINE_ID,
                    index,
                    index as u64,
                    Some(DocumentCanvasImageKey::new_internal(namespace, key)),
                    1,
                    1,
                    Some(registry_generation),
                    DocumentCanvasCaptureStatus::Ready,
                )
            })
            .collect();
        DocumentSettlementSourceSnapshot::new_internal(
            DocumentSettlementSourceEpoch::new(1),
            sources,
        )
        .canvas_capture_binding()
        .unwrap()
        .unwrap()
    }

    #[test]
    fn retained_canvas_aliases_share_one_adapted_capture() {
        let snapshot = Arc::new(RetainedCanvasSnapshot {
            width: 1,
            height: 1,
            commands: vec![
                servo_canvas::retained_canvas::RetainedCanvasCommand::RasterPatch {
                    x: 0.0,
                    y: 0.0,
                    width: 1,
                    height: 1,
                    premultiplied_rgba: vec![255, 0, 0, 255],
                    reason:
                        servo_canvas::retained_canvas::RetainedCanvasFallbackReason::PixelReadback,
                },
            ],
            unsupported: None,
        });
        let mut cache = HashMap::new();

        let (first, first_diagnostics, first_is_new) =
            adapt_retained_canvas(&mut cache, Arc::clone(&snapshot), 3).unwrap();
        let (alias, alias_diagnostics, alias_is_new) =
            adapt_retained_canvas(&mut cache, snapshot, 4).unwrap();

        assert!(Arc::ptr_eq(&first, &alias));
        assert_eq!(first_diagnostics, 3);
        assert_eq!(alias_diagnostics, 3);
        assert!(first_is_new);
        assert!(!alias_is_new);
        assert_eq!(cache.len(), 1);
        assert_eq!(first.resources.len(), 1);
    }

    fn retained_canvas_with_fallbacks(count: u32) -> Arc<RetainedCanvasSnapshot> {
        Arc::new(RetainedCanvasSnapshot {
            width: count,
            height: 1,
            commands: (0..count)
                .map(|index| {
                    servo_canvas::retained_canvas::RetainedCanvasCommand::RasterPatch {
                        x: f64::from(index),
                        y: 0.0,
                        width: 1,
                        height: 1,
                        premultiplied_rgba: vec![index as u8, 0, 0, 255],
                        reason:
                            servo_canvas::retained_canvas::RetainedCanvasFallbackReason::PixelReadback,
                    }
                })
                .collect(),
            unsupported: None,
        })
    }

    fn canvas_alias_layout(placements: usize, width: u32) -> Vec<u8> {
        let fragments = (0..placements)
            .map(|sequence| {
                serde_json::json!({
                    "depth": 0,
                    "kind": "image",
                    "rect": {
                        "x": 0.0,
                        "y": 0.0,
                        "width": width,
                        "height": 1.0
                    },
                    "tag_id": null,
                    "paint_fragment_id": sequence,
                    "text_run": null,
                    "image_url": null,
                    "canvas_image_key": { "namespace": 7, "key": 9 }
                })
            })
            .collect::<Vec<_>>();
        let paint_events = (0..placements)
            .map(|sequence| {
                serde_json::json!({
                    "sequence": sequence,
                    "kind": "image",
                    "fragment_id": sequence,
                    "tag_id": null,
                    "spatial_node_id": sequence,
                    "clip_id": null
                })
            })
            .collect::<Vec<_>>();
        serde_json::to_vec(&serde_json::json!({
            "boxes": [],
            "fragments": fragments,
            "font_resources": [],
            "font_instances": [],
            "page_sequence": {
                "pages": [{
                    "index": 0,
                    "width": 100.0,
                    "height": 100.0,
                    "margin_top": 0.0,
                    "margin_right": 0.0,
                    "margin_bottom": 0.0,
                    "margin_left": 0.0,
                    "available_inline_size": 100.0,
                    "available_block_size": 100.0
                }]
            },
            "paint_events": paint_events,
            "paint_epoch": 1,
            "paint_content_width": 100.0,
            "paint_content_height": 100.0,
            "paint_scroll_node_count": placements,
            "paintable": true,
            "contentful": true,
            "first_reflow": false,
            "links": []
        }))
        .unwrap()
    }

    fn capture_canvas_aliases(
        placements: usize,
        snapshot: Arc<RetainedCanvasSnapshot>,
        limits: CanvasCaptureLimits,
    ) -> Result<SceneCapture, CaptureError> {
        let width = snapshot.width;
        capture_document_scene_with_canvas_limits(
            &canvas_alias_layout(placements, width),
            |_| None,
            move |_| Ok(Arc::clone(&snapshot)),
            limits,
        )
    }

    fn retained_canvas_diagnostics_bytes(snapshot: Arc<RetainedCanvasSnapshot>) -> u64 {
        let capture = adapt_canvas(transcript_from_retained(snapshot).unwrap()).unwrap();
        let counted = canvas_diagnostics_json_len(&capture.diagnostics).unwrap();
        assert_eq!(
            counted,
            u64::try_from(capture.diagnostics_json().unwrap().len()).unwrap()
        );
        counted
    }

    #[test]
    fn aliases_store_diagnostics_once_and_keep_ordered_placement_references() {
        let snapshot = retained_canvas_with_fallbacks(32);
        let diagnostics_bytes = retained_canvas_diagnostics_bytes(Arc::clone(&snapshot));

        let capture = capture_canvas_aliases(
            2,
            snapshot,
            CanvasCaptureLimits {
                placements: 2,
                placed_operations: 64,
                diagnostics_bytes,
            },
        )
        .unwrap();

        assert_eq!(capture.canvas_diagnostics.len(), 1);
        assert_eq!(capture.canvas_diagnostics[0].sequences, [0, 1]);
        assert_eq!(
            capture.canvas_diagnostics[0].diagnostics.fallbacks.len(),
            32
        );
        assert_eq!(capture.canvas_resources.len(), 32);
        assert_eq!(capture.scene.pages[0].operations.len(), 64);
    }

    #[test]
    fn repeated_aliases_fail_at_the_first_excess_placement_before_capture_exists() {
        let snapshot = retained_canvas_with_fallbacks(2);
        let resolver_calls = std::cell::Cell::new(0);
        let error = capture_document_scene_with_canvas_limits(
            &canvas_alias_layout(3, snapshot.width),
            |_| None,
            |_| {
                resolver_calls.set(resolver_calls.get() + 1);
                Ok(Arc::clone(&snapshot))
            },
            CanvasCaptureLimits {
                placements: 2,
                placed_operations: u64::MAX,
                diagnostics_bytes: u64::MAX,
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::CanvasCaptureLimitExceeded {
                sequence: 2,
                limit: CanvasCaptureLimit::Placements,
                configured: 2,
                observed: 3,
            }
        );
        assert_eq!(resolver_calls.get(), 2);
    }

    #[test]
    fn freeze_request_preflight_bounds_aliases_and_deduplicates_before_locking() {
        let capture: LayoutCapture = serde_json::from_slice(&canvas_alias_layout(2, 1)).unwrap();
        let requests = canvas_freeze_requests(&capture, 2).unwrap();
        assert_eq!(
            requests,
            [(
                0,
                CapturedCanvasImageKey {
                    namespace: 7,
                    key: 9,
                },
            )]
        );

        assert_eq!(
            canvas_freeze_requests(&capture, 1),
            Err(CaptureError::CanvasCaptureLimitExceeded {
                sequence: 1,
                limit: CanvasCaptureLimit::Placements,
                configured: 1,
                observed: 2,
            })
        );
    }

    #[test]
    fn a_missing_frozen_key_reports_its_first_paint_sequence() {
        let mut layout: serde_json::Value =
            serde_json::from_slice(&canvas_alias_layout(2, 1)).unwrap();
        layout["fragments"][1]["canvas_image_key"]["key"] = serde_json::json!(10);
        let snapshot_json = serde_json::to_vec(&layout).unwrap();

        let error = capture_document_scene_with_canvas(
            &snapshot_json,
            |_| None,
            |keys| {
                assert_eq!(keys, &[(7, 9), (7, 10)]);
                Err(FreezeCanvasSnapshotsError::MissingImageKey {
                    namespace: 7,
                    key: 10,
                    generation: 17,
                })
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::Canvas {
                sequence: 1,
                message:
                    "no retained command snapshot for image key 7:10 at Canvas registry generation 17"
                        .into(),
            }
        );
    }

    #[test]
    fn controlled_canvas_freeze_uses_the_bound_generation_and_visible_subset() {
        let binding = controlled_canvas_binding(&[(7, 9), (8, 3)], 17);

        let error = capture_controlled_document_scene_with_canvas(
            &canvas_alias_layout(1, 1),
            |_| None,
            Some(&binding),
            |keys, generation| {
                assert_eq!(keys, &[(7, 9)]);
                assert_eq!(generation, 17);
                Err(FreezeCanvasSnapshotsError::GenerationChanged {
                    expected: 17,
                    observed: 18,
                })
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::Canvas {
                sequence: 0,
                message: "Canvas registry generation changed from expected 17 to observed 18"
                    .into(),
            }
        );
    }

    #[test]
    fn controlled_canvas_rejects_a_layout_key_absent_from_the_candidate() {
        let binding = controlled_canvas_binding(&[(7, 10)], 17);

        let error = capture_controlled_document_scene_with_canvas(
            &canvas_alias_layout(1, 1),
            |_| None,
            Some(&binding),
            |_, _| panic!("an unbound layout key must be rejected before freezing"),
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::Canvas {
                sequence: 0,
                message:
                    "layout Canvas image key 7:9 was not bound by the consumed controlled candidate"
                        .into(),
            }
        );
    }

    #[test]
    fn controlled_canvas_requires_a_candidate_binding_for_layout_canvas() {
        let error = capture_controlled_document_scene_with_canvas(
            &canvas_alias_layout(1, 1),
            |_| None,
            None,
            |_, _| panic!("a missing candidate binding must be rejected before freezing"),
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::Canvas {
                sequence: 0,
                message:
                    "layout Canvas image key 7:9 was not bound by the consumed controlled candidate"
                        .into(),
            }
        );
    }

    #[test]
    fn controlled_hidden_canvas_still_checks_the_bound_generation() {
        let binding = controlled_canvas_binding(&[(7, 9)], 17);

        let error = capture_controlled_document_scene_with_canvas(
            &canvas_alias_layout(0, 1),
            |_| None,
            Some(&binding),
            |keys, generation| {
                assert!(keys.is_empty());
                assert_eq!(generation, 17);
                Err(FreezeCanvasSnapshotsError::GenerationChanged {
                    expected: 17,
                    observed: 19,
                })
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::Canvas {
                sequence: 0,
                message: "Canvas registry generation changed from expected 17 to observed 19"
                    .into(),
            }
        );
    }

    #[test]
    fn repeated_aliases_preflight_flattened_operations_before_appending_the_event() {
        let error = capture_canvas_aliases(
            2,
            retained_canvas_with_fallbacks(2),
            CanvasCaptureLimits {
                placements: 2,
                placed_operations: 3,
                diagnostics_bytes: u64::MAX,
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::CanvasCaptureLimitExceeded {
                sequence: 1,
                limit: CanvasCaptureLimit::PlacedOperations,
                configured: 3,
                observed: 4,
            }
        );
    }

    #[test]
    fn unique_diagnostics_use_an_exact_cumulative_serialized_byte_limit() {
        let snapshot = retained_canvas_with_fallbacks(32);
        let diagnostics_bytes = retained_canvas_diagnostics_bytes(Arc::clone(&snapshot));
        let error = capture_canvas_aliases(
            1,
            snapshot,
            CanvasCaptureLimits {
                placements: 1,
                placed_operations: 32,
                diagnostics_bytes: diagnostics_bytes - 1,
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::CanvasCaptureLimitExceeded {
                sequence: 0,
                limit: CanvasCaptureLimit::DiagnosticsBytes,
                configured: diagnostics_bytes - 1,
                observed: diagnostics_bytes,
            }
        );
    }

    #[test]
    fn canvas_capture_cost_overflow_fails_closed_even_at_the_maximum_limit() {
        assert_eq!(
            reserve_canvas_capture_cost(
                u64::MAX - 1,
                2,
                u64::MAX,
                7,
                CanvasCaptureLimit::DiagnosticsBytes,
            ),
            Err(CaptureError::CanvasCaptureLimitExceeded {
                sequence: 7,
                limit: CanvasCaptureLimit::DiagnosticsBytes,
                configured: u64::MAX,
                observed: u64::MAX,
            })
        );
    }

    #[test]
    fn accepts_pagination_diagnostics_without_relaxing_the_capture_schema() {
        let sequence = serde_json::json!({
            "pages": [],
            "continuations": [{"token": {"kind": "table"}}],
            "table_breaks": [{"next_row_index": 1}],
            "table_cell_continuations": [{"next_fragment_index": 2}],
            "table_group_repeats": [{
                "page_index": 1,
                "table_node": 10,
                "header_tag_id": 20,
                "row_group_index": 0,
                "source_block_start": 30.0,
                "target_block_start": 120.0,
                "block_size": 20.0
            }],
            "warnings": [{"kind": "oversized-table-cell"}],
        });
        assert!(serde_json::from_value::<CapturePageSequence>(sequence).is_ok());
        assert!(
            serde_json::from_value::<CapturePageSequence>(serde_json::json!({
                "pages": [],
                "unknown": [],
            }))
            .is_err()
        );
    }

    fn exact_paint_authority_layout() -> serde_json::Value {
        serde_json::json!({
            "boxes": [],
            "fragments": [],
            "font_resources": [],
            "font_instances": [],
            "page_sequence": {
                "pages": [{
                    "index": 0,
                    "style_source": "request-defaults",
                    "app_units": {
                        "width": 600,
                        "height": 600,
                        "margin_top": 0,
                        "margin_right": 0,
                        "margin_bottom": 0,
                        "margin_left": 0,
                        "available_inline_size": 600,
                        "available_block_size": 600
                    },
                    "width": 10.0,
                    "height": 10.0,
                    "margin_top": 0.0,
                    "margin_right": 0.0,
                    "margin_bottom": 0.0,
                    "margin_left": 0.0,
                    "available_inline_size": 10.0,
                    "available_block_size": 10.0
                }]
            },
            "paint_events": [
                {
                    "sequence": 0,
                    "kind": "paint-rect",
                    "fragment_id": null,
                    "tag_id": null,
                    "spatial_node_id": 0,
                    "clip_id": null,
                    "paint_rects": [{
                        "rect": {
                            "x": 1.0,
                            "y": 1.0,
                            "width": 1.0,
                            "height": 1.0,
                            "app_units": {"x": 60, "y": 60, "width": 60, "height": 60}
                        },
                        "color": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0},
                        "kind": "background"
                    }]
                },
                {
                    "sequence": 1,
                    "kind": "paint-rect",
                    "fragment_id": null,
                    "tag_id": null,
                    "spatial_node_id": 0,
                    "clip_id": null,
                    "paint_rects": [{
                        "rect": {
                            "x": 2.0,
                            "y": 2.0,
                            "width": 1.0,
                            "height": 1.0,
                            "app_units": {"x": 120, "y": 120, "width": 60, "height": 60}
                        },
                        "color": {"r": 0.0, "g": 0.0, "b": 1.0, "a": 1.0},
                        "kind": "border"
                    }]
                }
            ],
            "paint_epoch": 1,
            "paint_content_width": 10.0,
            "paint_content_height": 10.0,
            "paint_scroll_node_count": 1,
            "paintable": true,
            "contentful": true,
            "first_reflow": false,
            "links": []
        })
    }

    fn exact_text_link_layout() -> serde_json::Value {
        let font_bytes = b"fixed-point-font";
        let resource_digest: [u8; 32] = Sha256::digest(font_bytes).into();
        let resource = content_address(&resource_digest);
        let instance = font_instance_id(&resource_digest, 0, &[], false);
        let large_x = 100_000_001;
        let compatibility_x = app_units_to_f32_px(large_x);

        serde_json::json!({
            "boxes": [],
            "fragments": [{
                "depth": 0,
                "kind": "text",
                "rect": {
                    "x": compatibility_x,
                    "y": 1.0,
                    "width": 2.0,
                    "height": 2.0,
                    "app_units": {
                        "x": large_x,
                        "y": 60,
                        "width": 120,
                        "height": 120
                    }
                },
                "tag_id": 7,
                "paint_fragment_id": 0,
                "text_run": {
                    "text": "A",
                    "font_instance_id": instance,
                    "font_identifier": {"ArrayBuffer": {}},
                    "requested_families": ["Fixed Point"],
                    "selected_family": "Fixed Point",
                    "font_size": 12.0,
                    "font_size_app_units": 720,
                    "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0},
                    "glyphs": [{
                        "id": 42,
                        "x": compatibility_x,
                        "y": 2.0,
                        "advance": 7.0,
                        "app_units": {
                            "x": large_x,
                            "y": 120,
                            "advance": 420
                        },
                        "text_range": {"start": 0, "end": 1}
                    }]
                },
                "image_url": null
            }],
            "font_resources": [{
                "resource": resource,
                "bytes_base64": BASE64_STANDARD.encode(font_bytes)
            }],
            "font_instances": [{
                "id": instance,
                "resource": resource,
                "face_index": 0,
                "variations": [],
                "synthetic_bold": false
            }],
            "page_sequence": {
                "pages": [{
                    "index": 0,
                    "style_source": "request-defaults",
                    "app_units": {
                        "width": 6000,
                        "height": 6000,
                        "margin_top": 0,
                        "margin_right": 0,
                        "margin_bottom": 0,
                        "margin_left": 0,
                        "available_inline_size": 6000,
                        "available_block_size": 6000
                    },
                    "width": 100.0,
                    "height": 100.0,
                    "margin_top": 0.0,
                    "margin_right": 0.0,
                    "margin_bottom": 0.0,
                    "margin_left": 0.0,
                    "available_inline_size": 100.0,
                    "available_block_size": 100.0
                }]
            },
            "paint_events": [{
                "sequence": 0,
                "kind": "text",
                "fragment_id": 0,
                "tag_id": 7,
                "spatial_node_id": 0,
                "clip_id": null
            }],
            "paint_epoch": 1,
            "paint_content_width": 100.0,
            "paint_content_height": 100.0,
            "paint_scroll_node_count": 1,
            "paintable": true,
            "contentful": true,
            "first_reflow": false,
            "links": [{"tag_id": 7, "url": "https://example.test/exact"}]
        })
    }

    fn exact_box_link_layout() -> serde_json::Value {
        let mut layout = exact_text_link_layout();
        layout["fragments"][0]["depth"] = serde_json::json!(1);
        layout["fragments"][0]["tag_id"] = serde_json::json!(8);
        layout["paint_events"][0]["tag_id"] = serde_json::json!(8);
        layout["fragments"].as_array_mut().unwrap().insert(
            0,
            serde_json::json!({
                "depth": 0,
                "kind": "box",
                "rect": {
                    "x": app_units_to_f32_px(100_000_003),
                    "y": 1.0,
                    "width": 3.0,
                    "height": 3.0,
                    "app_units": {
                        "x": 100_000_003,
                        "y": 60,
                        "width": 180,
                        "height": 180
                    }
                },
                "tag_id": 7,
                "paint_fragment_id": null,
                "text_run": null,
                "image_url": null
            }),
        );
        layout
    }

    #[test]
    fn exact_paint_authority_preserves_event_operation_order() {
        let capture = capture_document_scene(
            &serde_json::to_vec(&exact_paint_authority_layout()).unwrap(),
            |_| None,
        )
        .unwrap();

        let x_positions = capture.scene.pages[0]
            .operations
            .iter()
            .map(|operation| match operation {
                Operation::Path { bounds, .. } => bounds.x,
                _ => panic!("paint-rect fixture must emit only paths"),
            })
            .collect::<Vec<_>>();
        assert_eq!(x_positions, [1.0, 2.0]);
        assert!(
            serde_json::to_value(&capture)
                .unwrap()
                .get("fixed_point_authority")
                .is_none(),
            "capture authority must not change the API 1 inspect serialization"
        );
        assert_eq!(
            capture.fixed_point_authority.pages,
            [CapturedPageAuthority {
                index: 0,
                style_source: Some(CapturedPageStyleSource::RequestDefaults),
                app_units: Some(CapturedPageAppUnits {
                    width: 600,
                    height: 600,
                    margin_top: 0,
                    margin_right: 0,
                    margin_bottom: 0,
                    margin_left: 0,
                    available_inline_size: 600,
                    available_block_size: 600,
                }),
            }]
        );
        assert_eq!(
            capture.fixed_point_authority.page_operations,
            [vec![
                Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                    x: 60,
                    y: 60,
                    width: 60,
                    height: 60,
                })),
                Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                    x: 120,
                    y: 120,
                    width: 60,
                    height: 60,
                })),
            ]]
        );
    }

    #[test]
    fn text_and_fragment_link_keep_exact_authority_in_final_operation_order() {
        let capture = capture_document_scene(
            &serde_json::to_vec(&exact_text_link_layout()).unwrap(),
            |_| None,
        )
        .unwrap();

        assert!(matches!(
            capture.scene.pages[0].operations[0],
            Operation::Text { .. }
        ));
        assert!(matches!(
            capture.scene.pages[0].operations[1],
            Operation::Link { .. }
        ));
        assert_eq!(
            capture.fixed_point_authority.page_operations,
            [vec![
                Some(CapturedOperationAuthority::Text {
                    font_size_app_units: 720,
                    glyphs: vec![CapturedGlyphAppUnits {
                        x: 100_000_001,
                        y: 120,
                        advance: 420,
                    }],
                }),
                Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                    x: 100_000_001,
                    y: 60,
                    width: 120,
                    height: 120,
                })),
            ]]
        );
    }

    #[test]
    fn unpainted_box_link_keeps_owner_rect_exact_authority() {
        let capture = capture_document_scene(
            &serde_json::to_vec(&exact_box_link_layout()).unwrap(),
            |_| None,
        )
        .unwrap();

        assert_eq!(capture.scene.pages[0].operations.len(), 2);
        assert!(matches!(
            capture.scene.pages[0].operations[0],
            Operation::Text { .. }
        ));
        let Operation::Link { bounds, target, .. } = &capture.scene.pages[0].operations[1] else {
            panic!("painted descendant must retain its unpainted anchor link");
        };
        assert_eq!(bounds.width, 3.0);
        assert_eq!(bounds.height, 3.0);
        assert_eq!(target, "https://example.test/exact");
        assert_ne!((bounds.x * 60.0).round() as i32, 100_000_003);
        assert_eq!(
            capture.fixed_point_authority.page_operations[0][1],
            Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 100_000_003,
                y: 60,
                width: 180,
                height: 180,
            }))
        );
    }

    #[test]
    fn legacy_unpainted_box_link_does_not_invent_exact_authority() {
        let mut layout = exact_box_link_layout();
        layout["fragments"][0]["rect"]
            .as_object_mut()
            .unwrap()
            .remove("app_units");
        let capture =
            capture_document_scene(&serde_json::to_vec(&layout).unwrap(), |_| None).unwrap();

        assert_eq!(capture.scene.pages[0].operations.len(), 2);
        assert!(matches!(
            capture.scene.pages[0].operations[1],
            Operation::Link { .. }
        ));
        // API 1 may retain float-only links; API 2 must still see missing authority.
        assert_eq!(capture.fixed_point_authority.page_operations[0][1], None);
    }

    #[test]
    fn directly_painted_link_does_not_gain_duplicate_owner_box_link() {
        let mut layout = exact_box_link_layout();
        layout["fragments"][1]["tag_id"] = serde_json::json!(7);
        layout["paint_events"][0]["tag_id"] = serde_json::json!(7);
        let capture =
            capture_document_scene(&serde_json::to_vec(&layout).unwrap(), |_| None).unwrap();

        assert_eq!(capture.scene.pages[0].operations.len(), 2);
        assert!(matches!(
            capture.scene.pages[0].operations[1],
            Operation::Link { .. }
        ));
        assert_eq!(
            capture.fixed_point_authority.page_operations[0][1],
            Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 100_000_001,
                y: 60,
                width: 120,
                height: 120,
            }))
        );
    }

    #[test]
    fn unpainted_box_link_page_translation_keeps_integer_authority() {
        let mut layout = exact_box_link_layout();
        let mut second_page = layout["page_sequence"]["pages"][0].clone();
        second_page["index"] = serde_json::json!(1);
        layout["page_sequence"]["pages"]
            .as_array_mut()
            .unwrap()
            .push(second_page);
        for fragment in layout["fragments"].as_array_mut().unwrap() {
            let rect = &mut fragment["rect"];
            let y = rect["app_units"]["y"].as_i64().unwrap() as i32 + 6001;
            rect["y"] = serde_json::json!(app_units_to_f32_px(y));
            rect["app_units"]["y"] = serde_json::json!(y);
        }
        let glyph = &mut layout["fragments"][1]["text_run"]["glyphs"][0];
        glyph["y"] = serde_json::json!(app_units_to_f32_px(6121));
        glyph["app_units"]["y"] = serde_json::json!(6121);
        layout["paint_content_height"] = serde_json::json!(200.0);
        let capture =
            capture_document_scene(&serde_json::to_vec(&layout).unwrap(), |_| None).unwrap();

        assert!(capture.scene.pages[0].operations.is_empty());
        assert_eq!(capture.scene.pages[1].operations.len(), 2);
        assert!(matches!(
            capture.scene.pages[1].operations[1],
            Operation::Link { .. }
        ));
        assert_eq!(
            capture.fixed_point_authority.page_operations[1][1],
            Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 100_000_003,
                y: 61,
                width: 180,
                height: 180,
            }))
        );
    }

    #[test]
    fn unpainted_box_link_rejects_mismatched_owner_rect_authority() {
        let mut layout = exact_box_link_layout();
        layout["fragments"][0]["rect"]["app_units"]["width"] = serde_json::json!(240);

        assert_eq!(
            capture_document_scene(&serde_json::to_vec(&layout).unwrap(), |_| None),
            Err(CaptureError::InvalidPaintGeometryAuthority)
        );
    }

    #[test]
    fn capture_structs_retain_large_signed_app_units_without_round_trip() {
        let x = app_units_to_f32_px(-100_000_001);
        let events: Vec<CapturePaintEvent> = serde_json::from_value(serde_json::json!([{
            "sequence": 0,
            "kind": "paint-rect",
            "fragment_id": null,
            "tag_id": null,
            "spatial_node_id": 0,
            "clip_id": null,
            "paint_rects": [{
                "rect": {
                    "x": x,
                    "y": 0.0,
                    "width": 1.0,
                    "height": 1.0,
                    "app_units": {
                        "x": -100_000_001,
                        "y": 0,
                        "width": 60,
                        "height": 60
                    }
                },
                "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0},
                "kind": "background"
            }]
        }]))
        .unwrap();

        let exact = events[0].paint_rects[0].rect.app_units.unwrap();
        assert_eq!(exact.x, -100_000_001);
        assert_ne!((x * APP_UNITS_PER_CSS_PIXEL).round() as i32, exact.x);
        assert!(events[0].paint_rects[0].rect.app_unit_authority_matches());
    }

    #[test]
    fn mismatched_or_incomplete_app_unit_authority_fails_closed() {
        let mut paint_mismatch = exact_paint_authority_layout();
        paint_mismatch["paint_events"][0]["paint_rects"][0]["rect"]["app_units"]["x"] =
            serde_json::json!(61);
        assert_eq!(
            capture_document_scene(&serde_json::to_vec(&paint_mismatch).unwrap(), |_| None),
            Err(CaptureError::InvalidPaintGeometryAuthority)
        );

        let mut incomplete_page = exact_paint_authority_layout();
        incomplete_page["page_sequence"]["pages"][0]
            .as_object_mut()
            .unwrap()
            .remove("app_units");
        assert_eq!(
            capture_document_scene(&serde_json::to_vec(&incomplete_page).unwrap(), |_| None),
            Err(CaptureError::InvalidPageGeometryAuthority)
        );
    }

    #[test]
    fn css_page_provenance_is_unrepresentable_even_with_equal_geometry() {
        let mut layout = exact_paint_authority_layout();
        layout["page_sequence"]["pages"][0]["style_source"] = serde_json::json!("css-page");

        assert!(matches!(
            capture_document_scene(&serde_json::to_vec(&layout).unwrap(), |_| None),
            Err(CaptureError::InvalidJson(_))
        ));
    }

    #[test]
    fn rejects_later_page_paths_without_rewriting_path_data() {
        let mut operation = Operation::Path {
            bounds: Rect {
                x: 10.0,
                y: 812.0,
                width: 20.0,
                height: 10.0,
            },
            data: "M10 812h20v10z".into(),
            fill: Some(crate::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            fill_rule: crate::FillRule::NonZero,
            stroke: None,
            meta: OperationMeta::default(),
        };
        let original = operation.clone();

        assert_eq!(
            translate_operation_y(&mut operation, 792.0, 7),
            Err(CaptureError::PageLocalPathUnsupported { sequence: 7 })
        );
        assert_eq!(operation, original);
    }

    #[test]
    fn translates_marked_table_border_rectangle_paths() {
        let mut operation = Operation::Path {
            bounds: Rect {
                x: 10.0,
                y: 812.0,
                width: 20.0,
                height: 2.0,
            },
            data: "M10 812h20v2h-20z".into(),
            fill: Some(crate::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            fill_rule: crate::FillRule::NonZero,
            stroke: None,
            meta: OperationMeta {
                semantics: Some(Semantics {
                    role: "artifact".into(),
                    label: Some("table-border".into()),
                }),
                source: None,
            },
        };

        assert_eq!(translate_operation_y(&mut operation, 792.0, 7), Ok(()));
        let Operation::Path { bounds, data, .. } = operation else {
            unreachable!();
        };
        assert_eq!(bounds.y, 20.0);
        assert_eq!(data, "M10 20h20v2h-20z");
    }

    fn repeated_header_distribution(
        target: f32,
        exact: Option<CaptureTableGroupRepeatAppUnits>,
        rectangle: bool,
    ) -> Result<DistributedOperations, CaptureError> {
        let page = |index| CapturePage {
            index,
            style_source: Some(CapturedPageStyleSource::RequestDefaults),
            app_units: Some(CapturedPageAppUnits {
                width: 6_000,
                height: 6_000,
                margin_top: 0,
                margin_right: 0,
                margin_bottom: 0,
                margin_left: 0,
                available_inline_size: 6_000,
                available_block_size: 6_000,
            }),
            width: 100.0,
            height: 100.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            available_inline_size: 100.0,
            available_block_size: 100.0,
        };
        let text_bounds = Rect {
            x: 10.0,
            y: 20.0,
            width: 20.0,
            height: 10.0,
        };
        let image_bounds = Rect {
            x: 10.0,
            y: 150.0,
            width: 20.0,
            height: 10.0,
        };
        let mut operations = vec![
            PositionedOperation {
                sequence: 0,
                structural_fragment_index: None,
                bounds: text_bounds,
                authority: Some(CapturedOperationAuthority::Text {
                    font_size_app_units: 720,
                    glyphs: vec![CapturedGlyphAppUnits {
                        x: 600,
                        y: 1_800,
                        advance: 420,
                    }],
                }),
                operation: Operation::Text {
                    text: "header".into(),
                    font: "font".into(),
                    font_size: 12.0,
                    color: Color::default(),
                    glyphs: vec![Glyph {
                        id: 1,
                        x: 10.0,
                        y: 30.0,
                        advance: 7.0,
                        text_range: None,
                    }],
                    meta: OperationMeta::default(),
                },
            },
            PositionedOperation {
                sequence: 1,
                structural_fragment_index: None,
                bounds: image_bounds.clone(),
                authority: Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                    x: 600,
                    y: 9_000,
                    width: 1_200,
                    height: 600,
                })),
                operation: Operation::Image {
                    bounds: image_bounds,
                    resource:
                        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                            .into(),
                    meta: OperationMeta::default(),
                },
            },
        ];
        if rectangle {
            let bounds = operations[0].bounds.clone();
            operations[0].authority =
                Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                    x: 600,
                    y: 1_200,
                    width: 1_200,
                    height: 600,
                }));
            operations[0].operation = Operation::Path {
                data: rectangle_path_data(&bounds),
                bounds,
                fill: Some(Color::default()),
                fill_rule: FillRule::NonZero,
                stroke: None,
                meta: OperationMeta {
                    semantics: Some(Semantics {
                        role: "artifact".into(),
                        label: Some("table-border".into()),
                    }),
                    source: None,
                },
            };
        }
        let paint_events = vec![
            CapturePaintEvent {
                sequence: 0,
                kind: "text".into(),
                fragment_id: Some(5),
                tag_id: Some(9),
                _spatial_node_id: 0,
                _clip_id: None,
                table_borders: Vec::new(),
                paint_rects: Vec::new(),
            },
            CapturePaintEvent {
                sequence: 1,
                kind: "image".into(),
                fragment_id: None,
                tag_id: None,
                _spatial_node_id: 0,
                _clip_id: None,
                table_borders: Vec::new(),
                paint_rects: Vec::new(),
            },
        ];
        let repeats = [CaptureTableGroupRepeat {
            page_index: 1,
            _table_node: Some(1),
            header_tag_id: 9,
            _row_group_index: 0,
            source_block_start: 20.0,
            target_block_start: target,
            block_size: 10.0,
            app_units: exact,
        }];
        let repeated_fragments = HashMap::from([(
            9,
            RepeatedTableHeaderFragments {
                fragment_indices: 0..0,
                paint_fragment_ids: HashSet::from([5]),
            },
        )]);

        distribute_operations(
            &[page(0), page(1)],
            operations,
            &paint_events,
            &repeats,
            &repeated_fragments,
        )
    }

    #[test]
    fn repeated_header_prepending_cannot_inherit_source_authority() {
        let distributed = repeated_header_distribution(120.0, None, false).unwrap();

        assert_eq!(
            distributed.page_operations,
            [
                vec![Some(CapturedOperationAuthority::Text {
                    font_size_app_units: 720,
                    glyphs: vec![CapturedGlyphAppUnits {
                        x: 600,
                        y: 1_800,
                        advance: 420,
                    }],
                })],
                vec![
                    None,
                    Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                        x: 600,
                        y: 3_000,
                        width: 1_200,
                        height: 600,
                    })),
                ],
            ]
        );
        assert!(matches!(
            distributed.pages[1].operations[0],
            Operation::Text { .. }
        ));
        assert!(matches!(
            distributed.pages[1].operations[1],
            Operation::Image { .. }
        ));
    }

    #[test]
    fn repeated_header_translates_exact_text_and_rectangle_authority() {
        let exact = Some(CaptureTableGroupRepeatAppUnits {
            source_block_start: 1_200,
            target_block_start: 7_380,
            block_size: 600,
        });
        for rectangle in [false, true] {
            let distributed = repeated_header_distribution(123.0, exact, rectangle).unwrap();
            let expected = if rectangle {
                CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                    x: 600,
                    y: 1_380,
                    width: 1_200,
                    height: 600,
                })
            } else {
                CapturedOperationAuthority::Text {
                    font_size_app_units: 720,
                    glyphs: vec![CapturedGlyphAppUnits {
                        x: 600,
                        y: 1_980,
                        advance: 420,
                    }],
                }
            };
            assert_eq!(distributed.page_operations[1][0], Some(expected));
            assert_eq!(distributed.pages[1].operations.len(), 2);
            assert!(matches!(
                distributed.pages[1].operations[1],
                Operation::Image { .. }
            ));
        }
    }

    #[test]
    fn repeated_header_rejects_mismatched_authority_and_does_not_restore_clipped_geometry() {
        let exact = CaptureTableGroupRepeatAppUnits {
            source_block_start: 1_200,
            target_block_start: 7_380,
            block_size: 600,
        };
        assert!(matches!(
            repeated_header_distribution(120.0, Some(exact), false),
            Err(CaptureError::InvalidTableGroupRepeat { page_index: 1 })
        ));
        let clipped = repeated_header_distribution(
            195.0,
            Some(CaptureTableGroupRepeatAppUnits {
                target_block_start: 11_700,
                ..exact
            }),
            true,
        )
        .unwrap();
        assert!(clipped.page_operations[1][0].is_none());
        let Operation::Path { bounds, .. } = &clipped.pages[1].operations[0] else {
            unreachable!()
        };
        assert_eq!(bounds.y, 95.0);
        assert_eq!(bounds.height, 5.0);
    }

    #[test]
    fn repeat_authority_preserves_large_integers_and_checks_overflow() {
        let repeat: CaptureTableGroupRepeat = serde_json::from_value(serde_json::json!({
            "page_index": 1, "table_node": 1, "header_tag_id": 9, "row_group_index": 0,
            "source_block_start": app_units_to_f32_px(100_000_001),
            "target_block_start": app_units_to_f32_px(100_000_019),
            "block_size": app_units_to_f32_px(601),
            "app_units": { "source_block_start": 100_000_001,
                "target_block_start": 100_000_019, "block_size": 601 }
        }))
        .unwrap();
        assert!(repeat.app_unit_authority_matches());
        let exact = repeat.app_units.unwrap();
        let authority = CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
            x: 0,
            y: 100_000_001,
            width: 60,
            height: 601,
        });
        let translated = translate_operation_authority_y(
            authority.clone(),
            i64::from(exact.source_block_start) - i64::from(exact.target_block_start),
        )
        .unwrap();
        assert_eq!(
            translated,
            CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 0,
                y: 100_000_019,
                width: 60,
                height: 601,
            })
        );
        for invalid_offset in [i64::MIN, i64::MAX, i64::from(i32::MIN)] {
            assert!(translate_operation_authority_y(authority.clone(), invalid_offset).is_none());
        }
        for invalid in [
            CaptureTableGroupRepeatAppUnits {
                source_block_start: -1,
                ..exact
            },
            CaptureTableGroupRepeatAppUnits {
                target_block_start: -1,
                ..exact
            },
            CaptureTableGroupRepeatAppUnits {
                block_size: 0,
                ..exact
            },
        ] {
            let invalid = CaptureTableGroupRepeat {
                app_units: Some(invalid),
                ..repeat.clone()
            };
            assert!(!invalid.app_unit_authority_matches());
        }
    }

    fn exact_split_page(index: usize, width: i32, height: i32) -> CapturePage {
        CapturePage {
            index,
            style_source: Some(CapturedPageStyleSource::RequestDefaults),
            app_units: Some(CapturedPageAppUnits {
                width,
                height,
                margin_top: 0,
                margin_right: 0,
                margin_bottom: 0,
                margin_left: 0,
                available_inline_size: width,
                available_block_size: height,
            }),
            width: app_units_to_f32_px(width),
            height: app_units_to_f32_px(height),
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            available_inline_size: app_units_to_f32_px(width),
            available_block_size: app_units_to_f32_px(height),
        }
    }

    fn exact_split_rectangle(exact: CapturedRectAppUnits) -> PositionedOperation {
        let bounds = rect_from_app_units(exact);
        PositionedOperation {
            sequence: 4,
            structural_fragment_index: None,
            bounds: bounds.clone(),
            authority: Some(CapturedOperationAuthority::Bounds(exact)),
            operation: Operation::Path {
                data: rectangle_path_data(&bounds),
                bounds,
                fill: Some(Color::default()),
                fill_rule: FillRule::NonZero,
                stroke: None,
                meta: OperationMeta {
                    semantics: Some(Semantics {
                        role: "artifact".into(),
                        label: Some("background".into()),
                    }),
                    source: None,
                },
            },
        }
    }

    #[test]
    fn exact_background_split_preserves_intact_rectangles_and_a4_page_ownership() {
        let intact = CapturedRectAppUnits {
            x: 123,
            y: 234,
            width: 601,
            height: 121,
        };
        let parts = split_solid_rect_operations(
            &[exact_split_page(0, 2000, 2000)],
            vec![exact_split_rectangle(intact)],
        )
        .unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(
            parts[0].authority,
            Some(CapturedOperationAuthority::Bounds(intact))
        );

        let pages = (0..5)
            .map(|index| exact_split_page(index, 47_622, 67_351))
            .collect::<Vec<_>>();
        let full = CapturedRectAppUnits {
            x: 0,
            y: 0,
            width: 47_622,
            height: 5 * 67_351,
        };
        // Page four exposes accumulated f32-origin drift; integer ownership must win.
        assert!(f64::from(app_units_to_f32_px(3 * 67_351)) < 3.0 * f64::from(pages[0].height));
        let distributed = distribute_operations(
            &pages,
            vec![exact_split_rectangle(full)],
            &[],
            &[],
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(distributed.pages.len(), 5);
        for (page, authority) in distributed.pages.iter().zip(&distributed.page_operations) {
            assert_eq!(page.operations.len(), 1);
            assert_eq!(
                *authority,
                vec![Some(CapturedOperationAuthority::Bounds(
                    CapturedRectAppUnits {
                        x: 0,
                        y: 0,
                        width: 47_622,
                        height: 67_351,
                    }
                ))]
            );
            let Operation::Path { bounds, .. } = &page.operations[0] else {
                unreachable!()
            };
            assert_eq!(bounds.y, 0.0);
        }
    }

    #[test]
    fn exact_background_intersections_do_not_round_trip_large_source_integers() {
        let source = CapturedRectAppUnits {
            x: 100_000_001,
            y: 100_000_019,
            width: 601,
            height: 121,
        };
        let pages = [
            exact_split_page(0, 200_000_000, 100_000_050),
            exact_split_page(1, 200_000_000, 100_000_050),
        ];
        let operation = exact_split_rectangle(source);
        assert_ne!((operation.bounds.y * 60.0).round() as i32, source.y);
        let distributed =
            distribute_operations(&pages, vec![operation], &[], &[], &HashMap::new()).unwrap();
        assert_eq!(
            distributed.page_operations,
            vec![
                vec![Some(CapturedOperationAuthority::Bounds(
                    CapturedRectAppUnits {
                        height: 31,
                        ..source
                    }
                ))],
                vec![Some(CapturedOperationAuthority::Bounds(
                    CapturedRectAppUnits {
                        y: 0,
                        height: 90,
                        ..source
                    }
                ))],
            ]
        );
    }

    #[test]
    fn background_splitting_never_invents_missing_authority() {
        let source = CapturedRectAppUnits {
            x: 0,
            y: 3000,
            width: 3000,
            height: 6000,
        };
        let mut pages = [
            exact_split_page(0, 6000, 6000),
            exact_split_page(1, 6000, 6000),
        ];
        let mut missing_operation = exact_split_rectangle(source);
        missing_operation.authority = None;
        let parts = split_solid_rect_operations(&pages, vec![missing_operation]).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|part| part.authority.is_none()));

        pages[1].app_units = None;
        pages[1].style_source = None;
        let parts =
            split_solid_rect_operations(&pages, vec![exact_split_rectangle(source)]).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|part| part.authority.is_none()));
    }

    #[test]
    fn exact_background_splitting_rejects_incomplete_coverage_and_invalid_authority() {
        let pages = [
            exact_split_page(0, 6000, 6000),
            exact_split_page(1, 6000, 6000),
        ];
        let source = CapturedRectAppUnits {
            x: 0,
            y: 0,
            width: 3000,
            height: 6000,
        };
        assert!(matches!(
            split_solid_rect_operations(
                &pages,
                vec![exact_split_rectangle(CapturedRectAppUnits {
                    height: 12001,
                    ..source
                })]
            ),
            Err(CaptureError::OperationOutsidePageSequence { sequence: 4 })
        ));
        for invalid in [
            CapturedRectAppUnits { x: -1, ..source },
            CapturedRectAppUnits {
                x: i32::MAX,
                width: 1,
                ..source
            },
            CapturedRectAppUnits {
                y: i32::MAX,
                height: 1,
                ..source
            },
        ] {
            assert!(
                split_solid_rect_operations(&pages, vec![exact_split_rectangle(invalid)]).is_err()
            );
        }
        let mut mismatched = exact_split_rectangle(source);
        mismatched.authority = Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
            width: 3001,
            ..source
        }));
        assert!(split_solid_rect_operations(&pages, vec![mismatched]).is_err());
        let mut arbitrary = exact_split_rectangle(source);
        let Operation::Path { data, .. } = &mut arbitrary.operation else {
            unreachable!()
        };
        *data = "M0 0L1 1Z".into();
        assert!(split_solid_rect_operations(&pages, vec![arbitrary]).is_err());
        let mut wrong_page = pages;
        wrong_page[1].app_units.as_mut().unwrap().height += 1;
        assert!(matches!(
            split_solid_rect_operations(&wrong_page, vec![exact_split_rectangle(source)]),
            Err(CaptureError::InvalidPageGeometryAuthority)
        ));
    }

    #[test]
    fn splits_a_solid_background_across_every_intersected_page() {
        let page = |index| CapturePage {
            index,
            style_source: None,
            app_units: None,
            width: 100.0,
            height: 100.0,
            margin_top: 10.0,
            margin_right: 10.0,
            margin_bottom: 10.0,
            margin_left: 10.0,
            available_inline_size: 80.0,
            available_block_size: 80.0,
        };
        let bounds = Rect {
            x: 5.0,
            y: 50.0,
            width: 90.0,
            height: 220.0,
        };
        let parts = split_solid_rect_operations(
            &[page(0), page(1), page(2)],
            vec![PositionedOperation {
                sequence: 4,
                structural_fragment_index: None,
                bounds: bounds.clone(),
                authority: Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                    x: 300,
                    y: 3_000,
                    width: 5_400,
                    height: 13_200,
                })),
                operation: Operation::Path {
                    data: rectangle_path_data(&bounds),
                    bounds,
                    fill: Some(crate::Color::default()),
                    fill_rule: crate::FillRule::NonZero,
                    stroke: None,
                    meta: OperationMeta {
                        semantics: Some(Semantics {
                            role: "artifact".into(),
                            label: Some("background".into()),
                        }),
                        source: None,
                    },
                },
            }],
        )
        .unwrap();

        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts
                .iter()
                .map(|part| (part.bounds.y, part.bounds.height))
                .collect::<Vec<_>>(),
            [(50.0, 50.0), (100.0, 100.0), (200.0, 70.0)]
        );
        assert!(
            parts.iter().all(|part| part.authority.is_none()),
            "split operations must not inherit the unsplit source rectangle"
        );
    }

    #[test]
    fn clips_only_a_centered_table_edge_to_its_owning_page() {
        let page = |index| CapturePage {
            index,
            style_source: None,
            app_units: None,
            width: 100.0,
            height: 100.0,
            margin_top: 10.0,
            margin_right: 10.0,
            margin_bottom: 10.0,
            margin_left: 10.0,
            available_inline_size: 80.0,
            available_block_size: 80.0,
        };
        let bounds = Rect {
            x: 10.0,
            y: 199.0,
            width: 20.0,
            height: 2.0,
        };
        let mut positioned = PositionedOperation {
            sequence: 7,
            structural_fragment_index: None,
            bounds: bounds.clone(),
            authority: Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 600,
                y: 11_940,
                width: 1_200,
                height: 120,
            })),
            operation: Operation::Path {
                data: rectangle_path_data(&bounds),
                bounds,
                fill: Some(crate::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                fill_rule: crate::FillRule::NonZero,
                stroke: None,
                meta: OperationMeta {
                    semantics: Some(Semantics {
                        role: "artifact".into(),
                        label: Some("table-border".into()),
                    }),
                    source: None,
                },
            },
        };
        let mut oversized = positioned.clone();
        oversized.bounds.y = 150.0;
        oversized.bounds.width = 2.0;
        oversized.bounds.height = 80.0;
        let Operation::Path { bounds, data, .. } = &mut oversized.operation else {
            unreachable!();
        };
        *bounds = oversized.bounds.clone();
        *data = rectangle_path_data(bounds);

        let (page_index, page_origin) =
            operation_page(&[page(0), page(1)], &mut positioned).unwrap();
        assert_eq!((page_index, page_origin), (1, 100.0));
        assert_eq!(positioned.authority, None);
        translate_operation_y(&mut positioned.operation, page_origin, positioned.sequence).unwrap();
        let Operation::Path { bounds, data, .. } = positioned.operation else {
            unreachable!();
        };
        assert_eq!(
            bounds,
            Rect {
                x: 10.0,
                y: 99.0,
                width: 20.0,
                height: 1.0,
            }
        );
        assert_eq!(data, "M10 99h20v1h-20z");
        assert_eq!(
            operation_page(&[page(0), page(1)], &mut oversized),
            Err(CaptureError::OperationCrossesPageBoundary {
                sequence: 7,
                page_index: 1,
            })
        );
    }
}
