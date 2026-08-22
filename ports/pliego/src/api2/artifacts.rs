/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Inactive canonical profile-null artifact encoding.
//!
//! This module is test-only until the complete scene, PDF, bundle, result, and publication path is
//! proven. It consumes only final-operation-aligned fixed-point authority and fails instead of
//! recovering geometry from the API 1 floating-point scene.

use std::collections::BTreeMap;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use pliego::capture::{
    CapturedFontInstance, CapturedOperationAuthority, CapturedPageAppUnits,
    CapturedPageStyleSource, CapturedRectAppUnits, SceneCapture,
};
use pliego::pdf::{PdfFontResource, PdfFontVariation, render_document_pdf};
use pliego::{
    Color, DocumentScene, FillRule, Glyph, Operation, OperationMeta, Page, Rect, Size, Utf8Range,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{closed_object, hex_lower, required, validate_request};

const SCENE_MEDIA_TYPE: &str = "application/vnd.pliego.document-scene+json";
const FONT_MEDIA_TYPE: &str = "application/octet-stream";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EncodedResource {
    pub(crate) media_type: &'static str,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EncodedProfileNullScene {
    pub(crate) bytes: Vec<u8>,
    pub(crate) sha256: String,
    pub(crate) media_type: &'static str,
    pub(crate) resources: BTreeMap<String, EncodedResource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactError(String);

impl ArtifactError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ArtifactError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicDocumentScene {
    schema: PublicSceneSchema,
    version: u32,
    app_units_per_css_px: u32,
    request_page: PublicRequestPage,
    semantic_layer: Option<()>,
    pages: Vec<PublicPage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PublicSceneSchema {
    #[serde(rename = "pliego.document-scene")]
    DocumentScene,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicRequestPage {
    size: PublicPageSize,
    margins_app_units: PublicMargins,
    geometry_authority: GeometryAuthority,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum PublicPageSize {
    Named(NamedPageSize),
    Explicit(ExplicitPageSize),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedPageSize {
    name: A4Name,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExplicitPageSize {
    width_app_units: i32,
    height_app_units: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum A4Name {
    #[serde(rename = "A4")]
    A4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum GeometryAuthority {
    #[serde(rename = "request-only-v1")]
    RequestOnlyV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicMargins {
    top: i32,
    right: i32,
    bottom: i32,
    left: i32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicPage {
    number: u32,
    style_source: PublicPageStyleSource,
    size_app_units: PublicSize,
    margins_app_units: PublicMargins,
    operations: Vec<PublicOperation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PublicPageStyleSource {
    #[serde(rename = "request-defaults")]
    RequestDefaults,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicSize {
    width: i32,
    height: i32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum PublicOperation {
    Text {
        text: String,
        font: PublicFontInstance,
        font_size_app_units: i32,
        color: PublicColor,
        glyphs: Vec<PublicGlyph>,
    },
    Path {
        bounds: PublicRect,
        data: String,
        fill: PublicColor,
        fill_rule: PublicFillRule,
    },
    Image {
        bounds: PublicRect,
        resource: String,
        media_type: PublicImageMediaType,
    },
    Link {
        bounds: PublicRect,
        target: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicFontInstance {
    resource: String,
    face_index: u32,
    variations: Vec<PublicFontVariation>,
    synthetic_bold: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicFontVariation {
    tag: u32,
    value_f32_bits: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicGlyph {
    id: u32,
    x: i32,
    y: i32,
    advance: i32,
    text_range: PublicTextRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicTextRange {
    start: u32,
    end: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicColor {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PublicFillRule {
    #[serde(rename = "non-zero")]
    NonZero,
    #[serde(rename = "even-odd")]
    EvenOdd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PublicImageMediaType {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/webp")]
    Webp,
}

impl PublicImageMediaType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

pub(crate) fn encode_profile_null_scene<'a>(
    request: &Value,
    capture: &SceneCapture,
    mut resolve_image: impl FnMut(&str) -> Option<&'a [u8]>,
) -> Result<EncodedProfileNullScene, ArtifactError> {
    validate_request(request).map_err(ArtifactError::new)?;
    let request_object =
        closed_object(request, "$", super::TOP_LEVEL_FIELDS).map_err(ArtifactError::new)?;
    if !required(request_object, "$", "profile")
        .map_err(ArtifactError::new)?
        .is_null()
    {
        return Err(ArtifactError::new(
            "profile-null scene encoding requires profile: null",
        ));
    }
    let request_page: PublicRequestPage = serde_json::from_value(
        required(request_object, "$", "page")
            .map_err(ArtifactError::new)?
            .clone(),
    )
    .map_err(|error| ArtifactError::new(format!("cannot bind accepted request page: {error}")))?;
    let (request_width, request_height) = request_page_dimensions(&request_page);

    if !capture.unsupported_events.is_empty() || !capture.text_mapping_gaps.is_empty() {
        return Err(ArtifactError::new(
            "capture contains unsupported paint events or missing text mappings",
        ));
    }
    if capture.scene.pages.is_empty() {
        return Err(ArtifactError::new(
            "canonical scene requires at least one page",
        ));
    }
    if capture.scene.pages.len() != capture.fixed_point_authority.pages.len() ||
        capture.scene.pages.len() != capture.fixed_point_authority.page_operations.len()
    {
        return Err(ArtifactError::new(
            "fixed-point page authority does not match final scene pages",
        ));
    }

    let font_instances = unique_font_instances(&capture.font_instances)?;
    let font_resources = decoded_font_resources(capture)?;
    let captured_images = captured_image_resources(capture)?;
    let mut resources = BTreeMap::new();
    let mut pages = Vec::with_capacity(capture.scene.pages.len());

    for (page_index, ((scene_page, page_authority), operation_authority)) in capture
        .scene
        .pages
        .iter()
        .zip(&capture.fixed_point_authority.pages)
        .zip(&capture.fixed_point_authority.page_operations)
        .enumerate()
    {
        if scene_page.operations.len() != operation_authority.len() {
            return Err(ArtifactError::new(format!(
                "fixed-point operation authority does not match page {}",
                page_index + 1
            )));
        }
        let page_app_units = exact_page(
            page_index,
            page_authority.index,
            page_authority.style_source,
            page_authority.app_units,
            request_width,
            request_height,
            request_page.margins_app_units,
        )?;
        let operations = scene_page
            .operations
            .iter()
            .zip(operation_authority)
            .enumerate()
            .map(|(operation_index, (operation, authority))| {
                encode_operation(
                    page_index,
                    operation_index,
                    operation,
                    authority.as_ref(),
                    &font_instances,
                    &font_resources,
                    &captured_images,
                    &mut resolve_image,
                    &mut resources,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        pages.push(PublicPage {
            number: u32::try_from(page_index + 1)
                .map_err(|_| ArtifactError::new("page count exceeds unsigned 32-bit range"))?,
            style_source: PublicPageStyleSource::RequestDefaults,
            size_app_units: PublicSize {
                width: page_app_units.width,
                height: page_app_units.height,
            },
            margins_app_units: PublicMargins {
                top: page_app_units.margin_top,
                right: page_app_units.margin_right,
                bottom: page_app_units.margin_bottom,
                left: page_app_units.margin_left,
            },
            operations,
        });
    }

    let scene = PublicDocumentScene {
        schema: PublicSceneSchema::DocumentScene,
        version: 2,
        app_units_per_css_px: 60,
        request_page,
        semantic_layer: None,
        pages,
    };
    let mut bytes = serde_json::to_vec(&scene).map_err(|error| {
        ArtifactError::new(format!("cannot serialize canonical scene: {error}"))
    })?;
    bytes.push(b'\n');
    let sha256 = content_address(&bytes);
    Ok(EncodedProfileNullScene {
        bytes,
        sha256,
        media_type: SCENE_MEDIA_TYPE,
        resources,
    })
}

#[derive(Debug)]
struct PdfAdapterInput {
    scene: DocumentScene,
    fonts: BTreeMap<String, PdfFontBinding>,
}

#[derive(Debug, PartialEq)]
struct PdfFontBinding {
    resource: String,
    face_index: u32,
    variations: Vec<PdfFontVariation>,
    synthetic_bold: bool,
}

/// Render PDF exclusively from the canonical DocumentScene v2 bytes and their exact resource
/// closure. The capture-time floating-point scene is deliberately not an input to this boundary.
pub(crate) fn render_profile_null_pdf(
    encoded: &EncodedProfileNullScene,
) -> Result<Vec<u8>, ArtifactError> {
    let input = decode_pdf_adapter_input(encoded)?;
    render_document_pdf(
        &input.scene,
        |identity| {
            let binding = input.fonts.get(identity)?;
            let resource = encoded.resources.get(&binding.resource)?;
            Some(PdfFontResource {
                bytes: &resource.bytes,
                face_index: binding.face_index,
                variations: &binding.variations,
                synthetic_bold: binding.synthetic_bold,
            })
        },
        |resource| {
            encoded
                .resources
                .get(resource)
                .map(|entry| entry.bytes.as_slice())
        },
    )
    .map_err(|error| ArtifactError::new(format!("canonical scene PDF rendering failed: {error}")))
}

fn decode_pdf_adapter_input(
    encoded: &EncodedProfileNullScene,
) -> Result<PdfAdapterInput, ArtifactError> {
    if encoded.media_type != SCENE_MEDIA_TYPE {
        return Err(ArtifactError::new(format!(
            "canonical scene has unexpected media type {}",
            encoded.media_type
        )));
    }
    let observed_identity = content_address(&encoded.bytes);
    if encoded.sha256 != observed_identity {
        return Err(ArtifactError::new(format!(
            "canonical scene identity {} does not match retained bytes {observed_identity}",
            encoded.sha256
        )));
    }

    let public: PublicDocumentScene = serde_json::from_slice(&encoded.bytes)
        .map_err(|error| ArtifactError::new(format!("cannot decode canonical scene: {error}")))?;
    let mut canonical = serde_json::to_vec(&public).map_err(|error| {
        ArtifactError::new(format!("cannot re-encode canonical scene: {error}"))
    })?;
    canonical.push(b'\n');
    if canonical != encoded.bytes {
        return Err(ArtifactError::new(
            "canonical scene bytes are not the strict DocumentScene v2 encoding",
        ));
    }

    public_scene_to_pdf_input(&public, &encoded.resources)
}

fn public_scene_to_pdf_input(
    public: &PublicDocumentScene,
    resources: &BTreeMap<String, EncodedResource>,
) -> Result<PdfAdapterInput, ArtifactError> {
    if public.version != 2 {
        return Err(ArtifactError::new(format!(
            "unsupported canonical scene version {}",
            public.version
        )));
    }
    if public.app_units_per_css_px != 60 {
        return Err(ArtifactError::new(format!(
            "unsupported canonical scene app-unit scale {}",
            public.app_units_per_css_px
        )));
    }
    if public.pages.is_empty() {
        return Err(ArtifactError::new(
            "canonical scene requires at least one page",
        ));
    }

    for (resource, body) in resources {
        require_content_address(resource, &body.bytes)?;
        if body.bytes.is_empty() {
            return Err(ArtifactError::new(format!(
                "canonical scene resource {resource} is empty"
            )));
        }
    }

    let (request_width, request_height) = request_page_dimensions(&public.request_page);
    validate_page_box(
        request_width,
        request_height,
        public.request_page.margins_app_units,
        "accepted request page",
    )?;

    let mut required_resources = BTreeMap::<String, &'static str>::new();
    let mut fonts = BTreeMap::<String, PdfFontBinding>::new();
    let mut pages = Vec::with_capacity(public.pages.len());
    for (page_index, source_page) in public.pages.iter().enumerate() {
        let number = u32::try_from(page_index + 1)
            .map_err(|_| ArtifactError::new("canonical scene page count exceeds u32"))?;
        if source_page.number != number {
            return Err(ArtifactError::new(format!(
                "canonical scene page {} has non-sequential number {}",
                page_index + 1,
                source_page.number
            )));
        }
        if source_page.size_app_units.width != request_width ||
            source_page.size_app_units.height != request_height ||
            source_page.margins_app_units != public.request_page.margins_app_units
        {
            return Err(ArtifactError::new(format!(
                "canonical scene page {} does not resolve the accepted request page",
                page_index + 1
            )));
        }
        validate_page_box(
            source_page.size_app_units.width,
            source_page.size_app_units.height,
            source_page.margins_app_units,
            &format!("canonical scene page {}", page_index + 1),
        )?;

        let mut operations = Vec::with_capacity(source_page.operations.len());
        for (operation_index, operation) in source_page.operations.iter().enumerate() {
            let location = format!(
                "canonical scene page {} operation {}",
                page_index + 1,
                operation_index
            );
            operations.push(public_operation_to_pdf(
                operation,
                &location,
                resources,
                &mut required_resources,
                &mut fonts,
            )?);
        }
        pages.push(Page {
            size: Size {
                width: app_units_to_css(source_page.size_app_units.width),
                height: app_units_to_css(source_page.size_app_units.height),
            },
            operations,
        });
    }

    if let Some(resource) = resources
        .keys()
        .find(|resource| !required_resources.contains_key(*resource))
    {
        return Err(ArtifactError::new(format!(
            "canonical scene resource closure contains unreferenced resource {resource}"
        )));
    }
    if resources.len() != required_resources.len() {
        return Err(ArtifactError::new(
            "canonical scene resource closure is incomplete",
        ));
    }

    Ok(PdfAdapterInput {
        scene: DocumentScene {
            schema: pliego::SCHEMA.into(),
            version: pliego::SCHEMA_VERSION,
            pages,
        },
        fonts,
    })
}

fn validate_page_box(
    width: i32,
    height: i32,
    margins: PublicMargins,
    location: &str,
) -> Result<(), ArtifactError> {
    if width <= 0 || height <= 0 {
        return Err(ArtifactError::new(format!(
            "{location} has non-positive fixed-point dimensions"
        )));
    }
    if [margins.top, margins.right, margins.bottom, margins.left]
        .into_iter()
        .any(|margin| margin < 0)
    {
        return Err(ArtifactError::new(format!(
            "{location} has negative fixed-point margins"
        )));
    }
    let inline = width
        .checked_sub(margins.left)
        .and_then(|value| value.checked_sub(margins.right));
    let block = height
        .checked_sub(margins.top)
        .and_then(|value| value.checked_sub(margins.bottom));
    if inline.is_none_or(|value| value <= 0) || block.is_none_or(|value| value <= 0) {
        return Err(ArtifactError::new(format!(
            "{location} has no positive fixed-point content box"
        )));
    }
    Ok(())
}

fn public_operation_to_pdf(
    operation: &PublicOperation,
    location: &str,
    resources: &BTreeMap<String, EncodedResource>,
    required_resources: &mut BTreeMap<String, &'static str>,
    fonts: &mut BTreeMap<String, PdfFontBinding>,
) -> Result<Operation, ArtifactError> {
    match operation {
        PublicOperation::Text {
            text,
            font,
            font_size_app_units,
            color,
            glyphs,
        } => {
            if text.is_empty() || text.len() > u32::MAX as usize {
                return Err(ArtifactError::new(format!(
                    "{location} text is empty or exceeds u32 UTF-8 bytes"
                )));
            }
            if *font_size_app_units <= 0 || glyphs.is_empty() {
                return Err(ArtifactError::new(format!(
                    "{location} has non-positive font size or no glyphs"
                )));
            }
            require_pdf_resource(
                &font.resource,
                FONT_MEDIA_TYPE,
                location,
                resources,
                required_resources,
            )?;
            let mut previous_tag = None;
            let variations = font
                .variations
                .iter()
                .map(|variation| {
                    if previous_tag.is_some_and(|tag| tag >= variation.tag) {
                        return Err(ArtifactError::new(format!(
                            "{location} font variation tags are not strictly ascending"
                        )));
                    }
                    previous_tag = Some(variation.tag);
                    let value = f32::from_bits(variation.value_f32_bits);
                    if !value.is_finite() || (value == 0.0 && value.is_sign_negative()) {
                        return Err(ArtifactError::new(format!(
                            "{location} font variation is not canonical finite binary32"
                        )));
                    }
                    Ok(PdfFontVariation {
                        tag: variation.tag,
                        value,
                    })
                })
                .collect::<Result<Vec<_>, ArtifactError>>()?;
            let identity = pdf_font_identity(font)?;
            let binding = PdfFontBinding {
                resource: font.resource.clone(),
                face_index: font.face_index,
                variations,
                synthetic_bold: font.synthetic_bold,
            };
            if let Some(existing) = fonts.get(&identity) &&
                existing != &binding
            {
                return Err(ArtifactError::new(format!(
                    "{location} font tuple identity collision"
                )));
            }
            fonts.entry(identity.clone()).or_insert(binding);

            let glyphs = glyphs
                .iter()
                .enumerate()
                .map(|(glyph_index, glyph)| {
                    if glyph.id > u16::MAX.into()
                        || glyph.x < 0
                        || glyph.y < 0
                        || glyph.advance < 0
                    {
                        return Err(ArtifactError::new(format!(
                            "{location} glyph {glyph_index} cannot be represented by the PDF backend"
                        )));
                    }
                    validate_text_range(text, glyph.text_range.start, glyph.text_range.end)
                        .map_err(|message| {
                            ArtifactError::new(format!(
                                "{location} glyph {glyph_index} {message}"
                            ))
                        })?;
                    Ok(Glyph {
                        id: glyph.id,
                        x: app_units_to_css(glyph.x),
                        y: app_units_to_css(glyph.y),
                        advance: app_units_to_css(glyph.advance),
                        text_range: Some(Utf8Range {
                            start: glyph.text_range.start,
                            end: glyph.text_range.end,
                        }),
                    })
                })
                .collect::<Result<Vec<_>, ArtifactError>>()?;
            Ok(Operation::Text {
                text: text.clone(),
                font: identity,
                font_size: app_units_to_css(*font_size_app_units),
                color: pdf_color(*color),
                glyphs,
                meta: OperationMeta::default(),
            })
        },
        PublicOperation::Path {
            bounds,
            data,
            fill,
            fill_rule,
        } => {
            validate_pdf_rect(*bounds, false, location)?;
            let expected = rectangle_path(*bounds, location)?;
            if data != &expected {
                return Err(ArtifactError::new(format!(
                    "{location} path data does not match canonical fixed-point bounds"
                )));
            }
            Ok(Operation::Path {
                bounds: pdf_rect(*bounds),
                data: pdf_rectangle_path(*bounds, location)?,
                fill: Some(pdf_color(*fill)),
                fill_rule: match fill_rule {
                    PublicFillRule::NonZero => FillRule::NonZero,
                    PublicFillRule::EvenOdd => FillRule::EvenOdd,
                },
                stroke: None,
                meta: OperationMeta::default(),
            })
        },
        PublicOperation::Image {
            bounds,
            resource,
            media_type,
        } => {
            validate_pdf_rect(*bounds, true, location)?;
            require_pdf_resource(
                resource,
                media_type.as_str(),
                location,
                resources,
                required_resources,
            )?;
            Ok(Operation::Image {
                bounds: pdf_rect(*bounds),
                resource: resource.clone(),
                meta: OperationMeta::default(),
            })
        },
        PublicOperation::Link { bounds, target } => {
            validate_pdf_rect(*bounds, true, location)?;
            if !canonical_link_target(target) {
                return Err(ArtifactError::new(format!(
                    "{location} link target is not canonical"
                )));
            }
            Ok(Operation::Link {
                bounds: pdf_rect(*bounds),
                target: target.clone(),
                meta: OperationMeta::default(),
            })
        },
    }
}

fn require_pdf_resource(
    resource: &str,
    media_type: &'static str,
    location: &str,
    resources: &BTreeMap<String, EncodedResource>,
    required_resources: &mut BTreeMap<String, &'static str>,
) -> Result<(), ArtifactError> {
    let body = resources.get(resource).ok_or_else(|| {
        ArtifactError::new(format!(
            "{location} references missing canonical resource {resource}"
        ))
    })?;
    if body.media_type != media_type {
        return Err(ArtifactError::new(format!(
            "{location} resource {resource} media type {} does not match {media_type}",
            body.media_type
        )));
    }
    if media_type != FONT_MEDIA_TYPE &&
        image_media_type(&body.bytes).map(PublicImageMediaType::as_str) != Some(media_type)
    {
        return Err(ArtifactError::new(format!(
            "{location} resource {resource} bytes do not match {media_type}"
        )));
    }
    if let Some(existing) = required_resources.insert(resource.to_owned(), media_type) &&
        existing != media_type
    {
        return Err(ArtifactError::new(format!(
            "{location} resource {resource} has conflicting media identities"
        )));
    }
    Ok(())
}

fn pdf_font_identity(font: &PublicFontInstance) -> Result<String, ArtifactError> {
    let tuple = serde_json::to_vec(font)
        .map_err(|error| ArtifactError::new(format!("cannot encode PDF font tuple: {error}")))?;
    Ok(format!("pliego-font-instance:{}", content_address(&tuple)))
}

fn validate_pdf_rect(
    bounds: PublicRect,
    require_area: bool,
    location: &str,
) -> Result<(), ArtifactError> {
    if bounds.x < 0 || bounds.y < 0 || bounds.width < 0 || bounds.height < 0 {
        return Err(ArtifactError::new(format!(
            "{location} has PDF-incompatible negative fixed-point bounds"
        )));
    }
    bounds.x.checked_add(bounds.width).ok_or_else(|| {
        ArtifactError::new(format!("{location} rectangle right edge exceeds i32"))
    })?;
    bounds.y.checked_add(bounds.height).ok_or_else(|| {
        ArtifactError::new(format!("{location} rectangle bottom edge exceeds i32"))
    })?;
    if require_area && (bounds.width == 0 || bounds.height == 0) {
        return Err(ArtifactError::new(format!(
            "{location} has an empty PDF rectangle"
        )));
    }
    Ok(())
}

fn pdf_rect(bounds: PublicRect) -> Rect {
    Rect {
        x: app_units_to_css(bounds.x),
        y: app_units_to_css(bounds.y),
        width: app_units_to_css(bounds.width),
        height: app_units_to_css(bounds.height),
    }
}

fn pdf_rectangle_path(bounds: PublicRect, location: &str) -> Result<String, ArtifactError> {
    let right = bounds.x.checked_add(bounds.width).ok_or_else(|| {
        ArtifactError::new(format!("{location} rectangle right edge exceeds i32"))
    })?;
    let bottom = bounds.y.checked_add(bounds.height).ok_or_else(|| {
        ArtifactError::new(format!("{location} rectangle bottom edge exceeds i32"))
    })?;
    Ok(format!(
        "M {} {} L {} {} L {} {} L {} {} Z",
        app_units_to_css(bounds.x),
        app_units_to_css(bounds.y),
        app_units_to_css(right),
        app_units_to_css(bounds.y),
        app_units_to_css(right),
        app_units_to_css(bottom),
        app_units_to_css(bounds.x),
        app_units_to_css(bottom)
    ))
}

fn pdf_color(color: PublicColor) -> Color {
    Color {
        r: f64::from(color.r) / 255.0,
        g: f64::from(color.g) / 255.0,
        b: f64::from(color.b) / 255.0,
        a: f64::from(color.a) / 255.0,
    }
}

fn app_units_to_css(value: i32) -> f64 {
    f64::from(value) / 60.0
}

fn request_page_dimensions(page: &PublicRequestPage) -> (i32, i32) {
    match page.size {
        PublicPageSize::Named(_) => (47_622, 67_351),
        PublicPageSize::Explicit(ref size) => (size.width_app_units, size.height_app_units),
    }
}

fn exact_page(
    page_index: usize,
    authority_index: usize,
    style_source: Option<CapturedPageStyleSource>,
    app_units: Option<CapturedPageAppUnits>,
    request_width: i32,
    request_height: i32,
    request_margins: PublicMargins,
) -> Result<CapturedPageAppUnits, ArtifactError> {
    if authority_index != page_index ||
        !matches!(style_source, Some(CapturedPageStyleSource::RequestDefaults))
    {
        return Err(ArtifactError::new(format!(
            "page {} lacks request-default fixed-point provenance",
            page_index + 1
        )));
    }
    let page = app_units.ok_or_else(|| {
        ArtifactError::new(format!(
            "page {} lacks fixed-point geometry authority",
            page_index + 1
        ))
    })?;
    if page.width != request_width ||
        page.height != request_height ||
        page.margin_top != request_margins.top ||
        page.margin_right != request_margins.right ||
        page.margin_bottom != request_margins.bottom ||
        page.margin_left != request_margins.left
    {
        return Err(ArtifactError::new(format!(
            "page {} geometry does not resolve the accepted request",
            page_index + 1
        )));
    }
    let expected_inline = page
        .width
        .checked_sub(page.margin_left)
        .and_then(|value| value.checked_sub(page.margin_right));
    let expected_block = page
        .height
        .checked_sub(page.margin_top)
        .and_then(|value| value.checked_sub(page.margin_bottom));
    if page.width <= 0 ||
        page.height <= 0 ||
        expected_inline != Some(page.available_inline_size) ||
        expected_block != Some(page.available_block_size) ||
        page.available_inline_size <= 0 ||
        page.available_block_size <= 0
    {
        return Err(ArtifactError::new(format!(
            "page {} fixed-point content box is invalid",
            page_index + 1
        )));
    }
    Ok(page)
}

#[allow(clippy::too_many_arguments)]
fn encode_operation<'a>(
    page_index: usize,
    operation_index: usize,
    operation: &Operation,
    authority: Option<&CapturedOperationAuthority>,
    font_instances: &BTreeMap<&str, &CapturedFontInstance>,
    font_resources: &BTreeMap<&str, Vec<u8>>,
    captured_images: &BTreeMap<&str, &[u8]>,
    resolve_image: &mut impl FnMut(&str) -> Option<&'a [u8]>,
    resources: &mut BTreeMap<String, EncodedResource>,
) -> Result<PublicOperation, ArtifactError> {
    let location = format!("page {} operation {}", page_index + 1, operation_index);
    let authority = authority.ok_or_else(|| {
        ArtifactError::new(format!("{location} has no exact fixed-point authority"))
    })?;
    match (operation, authority) {
        (
            Operation::Text {
                text,
                font,
                color,
                glyphs,
                ..
            },
            CapturedOperationAuthority::Text {
                font_size_app_units,
                glyphs: exact_glyphs,
            },
        ) => {
            if text.is_empty() || text.len() > u32::MAX as usize {
                return Err(ArtifactError::new(format!(
                    "{location} text is empty or exceeds unsigned 32-bit UTF-8 bytes"
                )));
            }
            if *font_size_app_units <= 0 || glyphs.is_empty() || glyphs.len() != exact_glyphs.len()
            {
                return Err(ArtifactError::new(format!(
                    "{location} has incomplete text fixed-point authority"
                )));
            }
            let instance = font_instances.get(font.as_str()).ok_or_else(|| {
                ArtifactError::new(format!(
                    "{location} references unknown font instance {font}"
                ))
            })?;
            let public_font = encode_font(instance, font_resources, resources, &location)?;
            let glyphs = glyphs
                .iter()
                .zip(exact_glyphs)
                .enumerate()
                .map(|(glyph_index, (glyph, exact))| {
                    let range = glyph.text_range.ok_or_else(|| {
                        ArtifactError::new(format!(
                            "{location} glyph {glyph_index} lacks a UTF-8 text range"
                        ))
                    })?;
                    validate_text_range(text, range.start, range.end).map_err(|message| {
                        ArtifactError::new(format!("{location} glyph {glyph_index} {message}"))
                    })?;
                    Ok(PublicGlyph {
                        id: glyph.id,
                        x: exact.x,
                        y: exact.y,
                        advance: exact.advance,
                        text_range: PublicTextRange {
                            start: range.start,
                            end: range.end,
                        },
                    })
                })
                .collect::<Result<Vec<_>, ArtifactError>>()?;
            Ok(PublicOperation::Text {
                text: text.clone(),
                font: public_font,
                font_size_app_units: *font_size_app_units,
                color: rgba8(*color, &location)?,
                glyphs,
            })
        },
        (
            Operation::Path {
                fill,
                fill_rule,
                stroke,
                ..
            },
            CapturedOperationAuthority::Bounds(bounds),
        ) => {
            if stroke.is_some() {
                return Err(ArtifactError::new(format!(
                    "{location} lacks exact stroke-width authority"
                )));
            }
            let fill = fill
                .as_ref()
                .ok_or_else(|| ArtifactError::new(format!("{location} path has no fill")))?;
            let bounds = public_rect(*bounds, &location)?;
            Ok(PublicOperation::Path {
                bounds,
                data: rectangle_path(bounds, &location)?,
                fill: rgba8(*fill, &location)?,
                fill_rule: match fill_rule {
                    FillRule::NonZero => PublicFillRule::NonZero,
                    FillRule::EvenOdd => PublicFillRule::EvenOdd,
                },
            })
        },
        (Operation::Image { resource, .. }, CapturedOperationAuthority::Bounds(bounds)) => {
            let bytes = captured_images
                .get(resource.as_str())
                .copied()
                .or_else(|| resolve_image(resource))
                .ok_or_else(|| {
                    ArtifactError::new(format!(
                        "{location} references missing image resource {resource}"
                    ))
                })?;
            let media_type = image_media_type(bytes).ok_or_else(|| {
                ArtifactError::new(format!(
                    "{location} image resource {resource} has unsupported bytes"
                ))
            })?;
            bind_resource(resources, resource, media_type.as_str(), bytes, &location)?;
            Ok(PublicOperation::Image {
                bounds: public_rect(*bounds, &location)?,
                resource: resource.clone(),
                media_type,
            })
        },
        (Operation::Link { target, .. }, CapturedOperationAuthority::Bounds(bounds)) => {
            if !canonical_link_target(target) {
                return Err(ArtifactError::new(format!(
                    "{location} target is not a canonical absolute URL"
                )));
            }
            Ok(PublicOperation::Link {
                bounds: public_rect(*bounds, &location)?,
                target: target.clone(),
            })
        },
        _ => Err(ArtifactError::new(format!(
            "{location} operation kind does not match fixed-point authority"
        ))),
    }
}

fn unique_font_instances(
    instances: &[CapturedFontInstance],
) -> Result<BTreeMap<&str, &CapturedFontInstance>, ArtifactError> {
    let mut result = BTreeMap::new();
    for instance in instances {
        if result.insert(instance.id.as_str(), instance).is_some() {
            return Err(ArtifactError::new(format!(
                "duplicate captured font instance {}",
                instance.id
            )));
        }
    }
    Ok(result)
}

fn decoded_font_resources(
    capture: &SceneCapture,
) -> Result<BTreeMap<&str, Vec<u8>>, ArtifactError> {
    let mut result = BTreeMap::new();
    for resource in &capture.font_resources {
        let bytes = BASE64_STANDARD
            .decode(&resource.bytes_base64)
            .map_err(|error| {
                ArtifactError::new(format!(
                    "cannot decode font resource {}: {error}",
                    resource.resource
                ))
            })?;
        if BASE64_STANDARD.encode(&bytes) != resource.bytes_base64 {
            return Err(ArtifactError::new(format!(
                "font resource {} is not canonical padded base64",
                resource.resource
            )));
        }
        require_content_address(&resource.resource, &bytes)?;
        if result.insert(resource.resource.as_str(), bytes).is_some() {
            return Err(ArtifactError::new(format!(
                "duplicate captured font resource {}",
                resource.resource
            )));
        }
    }
    Ok(result)
}

fn captured_image_resources(
    capture: &SceneCapture,
) -> Result<BTreeMap<&str, &[u8]>, ArtifactError> {
    let mut result = BTreeMap::new();
    for resource in capture
        .canvas_resources
        .iter()
        .chain(&capture.embedded_image_resources)
    {
        require_content_address(&resource.resource, &resource.png)?;
        if let Some(existing) = result.insert(resource.resource.as_str(), resource.png.as_slice()) &&
            existing != resource.png
        {
            return Err(ArtifactError::new(format!(
                "captured image resource {} has conflicting bytes",
                resource.resource
            )));
        }
    }
    Ok(result)
}

fn encode_font(
    instance: &CapturedFontInstance,
    font_resources: &BTreeMap<&str, Vec<u8>>,
    resources: &mut BTreeMap<String, EncodedResource>,
    location: &str,
) -> Result<PublicFontInstance, ArtifactError> {
    let bytes = font_resources
        .get(instance.resource.as_str())
        .ok_or_else(|| {
            ArtifactError::new(format!(
                "{location} font instance references missing resource {}",
                instance.resource
            ))
        })?;
    bind_resource(
        resources,
        &instance.resource,
        FONT_MEDIA_TYPE,
        bytes,
        location,
    )?;
    let mut previous_tag = None;
    let variations = instance
        .variations
        .iter()
        .map(|variation| {
            if previous_tag.is_some_and(|tag| tag >= variation.tag) {
                return Err(ArtifactError::new(format!(
                    "{location} font variation tags are not strictly ascending"
                )));
            }
            previous_tag = Some(variation.tag);
            if !variation.value.is_finite() ||
                (variation.value == 0.0 && variation.value.is_sign_negative())
            {
                return Err(ArtifactError::new(format!(
                    "{location} font variation is not canonical finite binary32"
                )));
            }
            Ok(PublicFontVariation {
                tag: variation.tag,
                value_f32_bits: variation.value.to_bits(),
            })
        })
        .collect::<Result<Vec<_>, ArtifactError>>()?;
    Ok(PublicFontInstance {
        resource: instance.resource.clone(),
        face_index: instance.face_index,
        variations,
        synthetic_bold: instance.synthetic_bold,
    })
}

fn bind_resource(
    resources: &mut BTreeMap<String, EncodedResource>,
    resource: &str,
    media_type: &'static str,
    bytes: &[u8],
    location: &str,
) -> Result<(), ArtifactError> {
    require_content_address(resource, bytes)?;
    if bytes.is_empty() {
        return Err(ArtifactError::new(format!(
            "{location} resource {resource} is empty"
        )));
    }
    let candidate = EncodedResource {
        media_type,
        bytes: bytes.to_vec(),
    };
    if let Some(existing) = resources.get(resource) &&
        existing != &candidate
    {
        return Err(ArtifactError::new(format!(
            "{location} resource {resource} has conflicting bytes or media type"
        )));
    }
    resources.entry(resource.to_owned()).or_insert(candidate);
    Ok(())
}

fn public_rect(bounds: CapturedRectAppUnits, location: &str) -> Result<PublicRect, ArtifactError> {
    if bounds.width < 0 || bounds.height < 0 {
        return Err(ArtifactError::new(format!(
            "{location} has negative fixed-point bounds"
        )));
    }
    Ok(PublicRect {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    })
}

fn rectangle_path(bounds: PublicRect, location: &str) -> Result<String, ArtifactError> {
    let right = bounds.x.checked_add(bounds.width).ok_or_else(|| {
        ArtifactError::new(format!("{location} rectangle right edge exceeds i32"))
    })?;
    let bottom = bounds.y.checked_add(bounds.height).ok_or_else(|| {
        ArtifactError::new(format!("{location} rectangle bottom edge exceeds i32"))
    })?;
    Ok(format!(
        "M {} {} L {} {} L {} {} L {} {} Z",
        bounds.x, bounds.y, right, bounds.y, right, bottom, bounds.x, bottom
    ))
}

fn rgba8(color: Color, location: &str) -> Result<PublicColor, ArtifactError> {
    let channel = |value: f64| {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(ArtifactError::new(format!(
                "{location} has a color channel outside [0, 1]"
            )));
        }
        Ok((value * 255.0).round() as u8)
    };
    Ok(PublicColor {
        r: channel(color.r)?,
        g: channel(color.g)?,
        b: channel(color.b)?,
        a: channel(color.a)?,
    })
}

fn validate_text_range(text: &str, start: u32, end: u32) -> Result<(), &'static str> {
    let start = usize::try_from(start).map_err(|_| "range start does not fit usize")?;
    let end = usize::try_from(end).map_err(|_| "range end does not fit usize")?;
    if start >= end || end > text.len() {
        return Err("range is empty or outside UTF-8 text");
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return Err("range is not on UTF-8 boundaries");
    }
    Ok(())
}

fn image_media_type(bytes: &[u8]) -> Option<PublicImageMediaType> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(PublicImageMediaType::Png)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(PublicImageMediaType::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(PublicImageMediaType::Gif)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(PublicImageMediaType::Webp)
    } else {
        None
    }
}

fn canonical_link_target(target: &str) -> bool {
    if target.len() > 8_192 ||
        !target.is_ascii() ||
        target.ends_with(['?', '#']) ||
        !canonical_percent_encoding(target)
    {
        return false;
    }
    let Ok(parsed) = url::Url::parse(target) else {
        return false;
    };
    if parsed.as_str() != target {
        return false;
    }
    match parsed.scheme() {
        "http" | "https" => {
            parsed.host_str().is_some() &&
                parsed.username().is_empty() &&
                parsed.password().is_none() &&
                parsed.path().starts_with('/') &&
                decoded_path_has_no_dot_segments(parsed.path())
        },
        "mailto" => {
            if parsed.host_str().is_some() || parsed.fragment().is_some() {
                return false;
            }
            let Some((local, domain)) = parsed.path().rsplit_once('@') else {
                return false;
            };
            !local.is_empty() && !domain.is_empty() && domain == domain.to_ascii_lowercase()
        },
        _ => false,
    }
}

fn decoded_path_has_no_dot_segments(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut segment = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let decoded = if bytes[index] == b'%' {
            let Some(high) = bytes.get(index + 1).and_then(|byte| hex_nibble(*byte)) else {
                return false;
            };
            let Some(low) = bytes.get(index + 2).and_then(|byte| hex_nibble(*byte)) else {
                return false;
            };
            index += 3;
            high * 16 + low
        } else {
            let decoded = bytes[index];
            index += 1;
            decoded
        };
        if decoded == b'/' {
            if segment == b"." || segment == b".." {
                return false;
            }
            segment.clear();
        } else {
            segment.push(decoded);
        }
    }
    segment != b"." && segment != b".."
}

fn canonical_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let Some((&high, rest)) = bytes.get(index + 1).zip(bytes.get(index + 2..)) else {
            return false;
        };
        let Some(&low) = rest.first() else {
            return false;
        };
        let Some(decoded) = hex_nibble(high).and_then(|high| {
            hex_nibble(low).map(|low| high.saturating_mul(16).saturating_add(low))
        }) else {
            return false;
        };
        if decoded.is_ascii_alphanumeric() || b"-._~".contains(&decoded) {
            return false;
        }
        index += 3;
    }
    true
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn require_content_address(resource: &str, bytes: &[u8]) -> Result<(), ArtifactError> {
    if resource != content_address(bytes) {
        return Err(ArtifactError::new(format!(
            "resource {resource} does not match retained bytes"
        )));
    }
    Ok(())
}

fn content_address(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_lower(&Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use pliego::capture::{
        CapturedFixedPointAuthority, CapturedFontResource, CapturedFontVariation,
        CapturedGlyphAppUnits, CapturedPageAuthority,
    };
    use pliego::hybrid_canvas::CanvasResource;
    use pliego::{DocumentScene, Glyph, OperationMeta, Page, Rect, Size, Stroke, Utf8Range};

    use super::*;

    const FONT_RESOURCE: &str =
        "sha256:bf0cf8c8fff21d37f18617edb5677ed60cd2ea9dae07bfc8571d9f11da706a00";
    const IMAGE_RESOURCE: &str =
        "sha256:2fae544893c4d6cdb5e76b60c9a0395b828b2b76a9bdbcd498b2392d9179758a";
    const FONT_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/fixtures/delivery/resources/bf0cf8c8fff21d37f18617edb5677ed60cd2ea9dae07bfc8571d9f11da706a00"
    ));
    const IMAGE_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/fixtures/delivery/resources/2fae544893c4d6cdb5e76b60c9a0395b828b2b76a9bdbcd498b2392d9179758a"
    ));
    const REQUEST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/goldens/accepted/render-request.a4.json"
    ));
    const PDF_FONT_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/text-scene/Ahem.ttf"
    ));
    const EXPECTED_SCENE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/fixtures/delivery/scene.json"
    ));

    fn fixture_capture() -> SceneCapture {
        let page = CapturedPageAppUnits {
            width: 47_622,
            height: 67_351,
            margin_top: 2_880,
            margin_right: 2_880,
            margin_bottom: 2_880,
            margin_left: 2_880,
            available_inline_size: 41_862,
            available_block_size: 61_591,
        };
        let bounds = |x, y, width, height| CapturedRectAppUnits {
            x,
            y,
            width,
            height,
        };
        SceneCapture {
            scene: DocumentScene {
                schema: pliego::SCHEMA.into(),
                version: pliego::SCHEMA_VERSION,
                pages: vec![Page {
                    size: Size {
                        width: 47_622.0 / 60.0,
                        height: 67_351.0 / 60.0,
                    },
                    operations: vec![
                        Operation::Text {
                            text: "Invoice".into(),
                            font: "internal-font-instance".into(),
                            font_size: 18.0,
                            color: Color::default(),
                            glyphs: vec![Glyph {
                                id: 42,
                                x: 48.0,
                                y: 72.0,
                                advance: 54.0,
                                text_range: Some(Utf8Range { start: 0, end: 7 }),
                            }],
                            meta: OperationMeta::default(),
                        },
                        Operation::Path {
                            bounds: Rect {
                                x: 48.0,
                                y: 96.0,
                                width: 697.7,
                                height: 1.0,
                            },
                            data: "legacy-float-path-is-not-authority".into(),
                            fill: Some(Color {
                                r: 64.0 / 255.0,
                                g: 64.0 / 255.0,
                                b: 64.0 / 255.0,
                                a: 1.0,
                            }),
                            fill_rule: FillRule::NonZero,
                            stroke: None,
                            meta: OperationMeta::default(),
                        },
                        Operation::Image {
                            bounds: Rect {
                                x: 48.0,
                                y: 120.0,
                                width: 64.0,
                                height: 64.0,
                            },
                            resource: IMAGE_RESOURCE.into(),
                            meta: OperationMeta::default(),
                        },
                        Operation::Link {
                            bounds: Rect {
                                x: 48.0,
                                y: 200.0,
                                width: 120.0,
                                height: 18.0,
                            },
                            target: "https://example.test/invoices/42".into(),
                            meta: OperationMeta::default(),
                        },
                    ],
                }],
            },
            fixed_point_authority: CapturedFixedPointAuthority {
                pages: vec![CapturedPageAuthority {
                    index: 0,
                    style_source: Some(CapturedPageStyleSource::RequestDefaults),
                    app_units: Some(page),
                }],
                page_operations: vec![vec![
                    Some(CapturedOperationAuthority::Text {
                        font_size_app_units: 1_080,
                        glyphs: vec![CapturedGlyphAppUnits {
                            x: 2_880,
                            y: 4_320,
                            advance: 3_240,
                        }],
                    }),
                    Some(CapturedOperationAuthority::Bounds(bounds(
                        2_880, 5_760, 41_862, 60,
                    ))),
                    Some(CapturedOperationAuthority::Bounds(bounds(
                        2_880, 7_200, 3_840, 3_840,
                    ))),
                    Some(CapturedOperationAuthority::Bounds(bounds(
                        2_880, 12_000, 7_200, 1_080,
                    ))),
                ]],
            },
            canvas_resources: vec![CanvasResource {
                resource: IMAGE_RESOURCE.into(),
                png: IMAGE_BYTES.to_vec(),
            }],
            embedded_image_resources: vec![],
            canvas_diagnostics: vec![],
            font_resources: vec![CapturedFontResource {
                resource: FONT_RESOURCE.into(),
                bytes_base64: BASE64_STANDARD.encode(FONT_BYTES),
            }],
            font_instances: vec![CapturedFontInstance {
                id: "internal-font-instance".into(),
                resource: FONT_RESOURCE.into(),
                face_index: 2,
                variations: vec![CapturedFontVariation {
                    tag: 2_003_265_652,
                    value: f32::from_bits(1_137_180_672),
                }],
                synthetic_bold: true,
            }],
            font_selections: vec![],
            font_warnings: vec![],
            unsupported_events: vec![],
            text_mapping_gaps: vec![],
        }
    }

    fn renderable_encoded_scene() -> EncodedProfileNullScene {
        let mut capture = fixture_capture();
        let font_resource = content_address(PDF_FONT_BYTES);
        capture.font_resources = vec![CapturedFontResource {
            resource: font_resource.clone(),
            bytes_base64: BASE64_STANDARD.encode(PDF_FONT_BYTES),
        }];
        capture.font_instances[0].resource = font_resource;
        capture.font_instances[0].face_index = 0;
        capture.font_instances[0].variations.clear();
        capture.font_instances[0].synthetic_bold = false;
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        encode_profile_null_scene(&request, &capture, |_| None).unwrap()
    }

    #[test]
    fn exact_authority_encodes_the_canonical_profile_null_scene_and_resource_closure() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let encoded = encode_profile_null_scene(&request, &fixture_capture(), |_| None).unwrap();

        assert_eq!(encoded.bytes, EXPECTED_SCENE);
        assert_eq!(encoded.sha256, content_address(EXPECTED_SCENE));
        assert_eq!(encoded.media_type, SCENE_MEDIA_TYPE);
        assert_eq!(encoded.resources.len(), 2);
        assert_eq!(encoded.resources[FONT_RESOURCE].media_type, FONT_MEDIA_TYPE);
        assert_eq!(encoded.resources[FONT_RESOURCE].bytes, FONT_BYTES);
        assert_eq!(encoded.resources[IMAGE_RESOURCE].media_type, "image/png");
        assert_eq!(encoded.resources[IMAGE_RESOURCE].bytes, IMAGE_BYTES);
    }

    #[test]
    fn pdf_adapter_uses_fixed_point_scene_authority_and_preserves_resource_identities() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let mut capture = fixture_capture();
        capture.scene.pages[0].size.width = 1.25;
        let Operation::Text {
            font_size, glyphs, ..
        } = &mut capture.scene.pages[0].operations[0]
        else {
            unreachable!();
        };
        *font_size = 999.5;
        glyphs[0].x = 777.25;
        let Operation::Path { bounds, data, .. } = &mut capture.scene.pages[0].operations[1] else {
            unreachable!();
        };
        bounds.x = 321.5;
        *data = "capture-floating-path-must-not-cross-the-adapter".into();

        let encoded = encode_profile_null_scene(&request, &capture, |_| None).unwrap();
        assert_eq!(encoded.bytes, EXPECTED_SCENE);
        let input = decode_pdf_adapter_input(&encoded).unwrap();
        assert_eq!(input.scene.pages[0].size.width, 47_622.0 / 60.0);
        let Operation::Text {
            font,
            font_size,
            glyphs,
            ..
        } = &input.scene.pages[0].operations[0]
        else {
            unreachable!();
        };
        assert_eq!(*font_size, 18.0);
        assert_eq!(glyphs[0].x, 48.0);
        let binding = &input.fonts[font];
        assert_eq!(binding.resource, FONT_RESOURCE);
        assert_eq!(binding.face_index, 2);
        assert_eq!(binding.variations.len(), 1);
        assert_eq!(binding.variations[0].tag, 2_003_265_652);
        assert_eq!(binding.variations[0].value.to_bits(), 1_137_180_672);
        assert!(binding.synthetic_bold);
        let Operation::Path { bounds, data, .. } = &input.scene.pages[0].operations[1] else {
            unreachable!();
        };
        assert_eq!(bounds.x, 48.0);
        assert_eq!(data, "M 48 96 L 745.7 96 L 745.7 97 L 48 97 Z");
        let Operation::Image { resource, .. } = &input.scene.pages[0].operations[2] else {
            unreachable!();
        };
        assert_eq!(resource, IMAGE_RESOURCE);

        let pdf = render_profile_null_pdf(&renderable_encoded_scene()).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(pdf.len() > 1_000);
    }

    #[test]
    fn pdf_adapter_requires_strict_canonical_scene_bytes_and_identity() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let encoded = encode_profile_null_scene(&request, &fixture_capture(), |_| None).unwrap();

        let mut mismatched_identity = encoded.clone();
        mismatched_identity.bytes.push(b' ');
        assert!(
            decode_pdf_adapter_input(&mismatched_identity)
                .unwrap_err()
                .to_string()
                .contains("does not match retained bytes")
        );

        let mut noncanonical = encoded.clone();
        noncanonical.bytes.push(b' ');
        noncanonical.sha256 = content_address(&noncanonical.bytes);
        assert!(
            decode_pdf_adapter_input(&noncanonical)
                .unwrap_err()
                .to_string()
                .contains("not the strict DocumentScene v2 encoding")
        );

        let mut unknown_member = encoded.clone();
        let mut value: Value = serde_json::from_slice(&unknown_member.bytes).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), Value::Bool(true));
        unknown_member.bytes = serde_json::to_vec(&value).unwrap();
        unknown_member.bytes.push(b'\n');
        unknown_member.sha256 = content_address(&unknown_member.bytes);
        assert!(
            decode_pdf_adapter_input(&unknown_member)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
    }

    #[test]
    fn pdf_adapter_fails_closed_on_missing_forged_or_extra_resources() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let encoded = encode_profile_null_scene(&request, &fixture_capture(), |_| None).unwrap();

        let mut missing = encoded.clone();
        missing.resources.remove(IMAGE_RESOURCE);
        assert!(
            decode_pdf_adapter_input(&missing)
                .unwrap_err()
                .to_string()
                .contains("references missing canonical resource")
        );

        let mut forged = encoded.clone();
        forged
            .resources
            .get_mut(IMAGE_RESOURCE)
            .unwrap()
            .bytes
            .push(0);
        assert!(
            decode_pdf_adapter_input(&forged)
                .unwrap_err()
                .to_string()
                .contains("does not match retained bytes")
        );

        let mut wrong_media = encoded.clone();
        wrong_media
            .resources
            .get_mut(IMAGE_RESOURCE)
            .unwrap()
            .media_type = FONT_MEDIA_TYPE;
        assert!(
            decode_pdf_adapter_input(&wrong_media)
                .unwrap_err()
                .to_string()
                .contains("media type application/octet-stream does not match image/png")
        );

        let mut extra = encoded;
        let bytes = b"unreferenced";
        extra.resources.insert(
            content_address(bytes),
            EncodedResource {
                media_type: FONT_MEDIA_TYPE,
                bytes: bytes.to_vec(),
            },
        );
        assert!(
            decode_pdf_adapter_input(&extra)
                .unwrap_err()
                .to_string()
                .contains("contains unreferenced resource")
        );
    }

    #[test]
    fn encoder_fails_instead_of_recovering_missing_or_mismatched_operation_authority() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let mut missing = fixture_capture();
        missing.fixed_point_authority.page_operations[0][0] = None;
        let error = encode_profile_null_scene(&request, &missing, |_| None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("has no exact fixed-point authority")
        );

        let mut mismatched = fixture_capture();
        mismatched.fixed_point_authority.page_operations[0][0] =
            Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }));
        let error = encode_profile_null_scene(&request, &mismatched, |_| None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match fixed-point authority")
        );
    }

    #[test]
    fn encoder_rejects_unproven_stroke_width_and_noncanonical_links() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let mut stroke = fixture_capture();
        let Operation::Path { stroke: value, .. } = &mut stroke.scene.pages[0].operations[1] else {
            unreachable!();
        };
        *value = Some(Stroke {
            color: Color::default(),
            width: 1.0,
        });
        let error = encode_profile_null_scene(&request, &stroke, |_| None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("lacks exact stroke-width authority")
        );

        let mut link = fixture_capture();
        let Operation::Link { target, .. } = &mut link.scene.pages[0].operations[3] else {
            unreachable!();
        };
        *target = "https://example.test:443/invoices/42".into();
        let error = encode_profile_null_scene(&request, &link, |_| None).unwrap_err();
        assert!(error.to_string().contains("not a canonical absolute URL"));
    }

    #[test]
    fn canonical_link_targets_match_the_public_contract() {
        for target in [
            "https://example.test/a%2Fb",
            "https://example.test:8443/path",
            "mailto:user@example.test",
        ] {
            assert!(canonical_link_target(target), "should accept {target}");
        }
        let exact_limit = format!("https://example.test/{}", "a".repeat(8_192 - 21));
        assert_eq!(exact_limit.len(), 8_192);
        assert!(canonical_link_target(&exact_limit));

        for target in [
            "https://example.test/a%2F..%2Fb",
            "https://user@example.test/a",
            "https://example.test:443/a",
            "https://example.test/a/../b",
            "https://example.test/a/%2E%2E/b",
            "https://example.test/a%2fb",
            "https://example.test/%41",
            "https://example.test",
            "mailto:user@Example.test",
            "https://example.test/café",
            "https://example.test/path?",
            "https://example.test/path#",
        ] {
            assert!(!canonical_link_target(target), "should reject {target}");
        }
        assert!(!canonical_link_target(&format!("{exact_limit}a")));
    }

    #[test]
    fn encoder_rejects_unbound_page_geometry_and_unrepresentable_paths() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let assert_error = |capture: &SceneCapture, expected: &str| {
            let error = encode_profile_null_scene(&request, capture, |_| None).unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?}, observed {error}"
            );
        };

        let mut empty = fixture_capture();
        empty.scene.pages.clear();
        empty.fixed_point_authority.pages.clear();
        empty.fixed_point_authority.page_operations.clear();
        assert_error(&empty, "requires at least one page");

        let mut missing_page_vector = fixture_capture();
        missing_page_vector.fixed_point_authority.pages.clear();
        assert_error(
            &missing_page_vector,
            "fixed-point page authority does not match final scene pages",
        );

        let mut missing_operation_vector = fixture_capture();
        missing_operation_vector
            .fixed_point_authority
            .page_operations
            .clear();
        assert_error(
            &missing_operation_vector,
            "fixed-point page authority does not match final scene pages",
        );

        let mut short_operation_vector = fixture_capture();
        short_operation_vector.fixed_point_authority.page_operations[0].pop();
        assert_error(
            &short_operation_vector,
            "fixed-point operation authority does not match page 1",
        );

        let mut wrong_index = fixture_capture();
        wrong_index.fixed_point_authority.pages[0].index = 1;
        assert_error(
            &wrong_index,
            "page 1 lacks request-default fixed-point provenance",
        );

        let mut wrong_style = fixture_capture();
        wrong_style.fixed_point_authority.pages[0].style_source = None;
        assert_error(
            &wrong_style,
            "page 1 lacks request-default fixed-point provenance",
        );

        let mut missing_geometry = fixture_capture();
        missing_geometry.fixed_point_authority.pages[0].app_units = None;
        assert_error(
            &missing_geometry,
            "page 1 lacks fixed-point geometry authority",
        );

        let mut wrong_page = fixture_capture();
        wrong_page.fixed_point_authority.pages[0]
            .app_units
            .as_mut()
            .unwrap()
            .width += 1;
        assert_error(&wrong_page, "does not resolve the accepted request");

        let mut invalid_content_box = fixture_capture();
        invalid_content_box.fixed_point_authority.pages[0]
            .app_units
            .as_mut()
            .unwrap()
            .available_inline_size += 1;
        assert_error(&invalid_content_box, "fixed-point content box is invalid");

        let mut negative = fixture_capture();
        negative.fixed_point_authority.page_operations[0][1] =
            Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 0,
                y: 0,
                width: -1,
                height: 1,
            }));
        assert_error(&negative, "has negative fixed-point bounds");

        let mut overflow = fixture_capture();
        overflow.fixed_point_authority.page_operations[0][1] =
            Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: i32::MAX,
                y: 0,
                width: 1,
                height: 1,
            }));
        assert_error(&overflow, "rectangle right edge exceeds i32");

        let mut bottom_overflow = fixture_capture();
        bottom_overflow.fixed_point_authority.page_operations[0][1] =
            Some(CapturedOperationAuthority::Bounds(CapturedRectAppUnits {
                x: 0,
                y: i32::MAX,
                width: 1,
                height: 1,
            }));
        assert_error(&bottom_overflow, "rectangle bottom edge exceeds i32");

        let mut invalid_color = fixture_capture();
        let Operation::Path { fill, .. } = &mut invalid_color.scene.pages[0].operations[1] else {
            unreachable!();
        };
        fill.as_mut().unwrap().r = f64::NAN;
        assert_error(&invalid_color, "color channel outside [0, 1]");
    }

    #[test]
    fn encoder_preserves_visual_order_utf8_ranges_and_canonical_variations() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let mut bidi = fixture_capture();
        let Operation::Text { text, glyphs, .. } = &mut bidi.scene.pages[0].operations[0] else {
            unreachable!();
        };
        *text = "אב".into();
        *glyphs = vec![
            Glyph {
                id: 1,
                x: 2.0,
                y: 3.0,
                advance: 4.0,
                text_range: Some(Utf8Range { start: 2, end: 4 }),
            },
            Glyph {
                id: 2,
                x: 6.0,
                y: 3.0,
                advance: 4.0,
                text_range: Some(Utf8Range { start: 0, end: 2 }),
            },
        ];
        let Some(CapturedOperationAuthority::Text { glyphs, .. }) =
            &mut bidi.fixed_point_authority.page_operations[0][0]
        else {
            unreachable!();
        };
        *glyphs = vec![
            CapturedGlyphAppUnits {
                x: 120,
                y: 180,
                advance: 240,
            },
            CapturedGlyphAppUnits {
                x: 360,
                y: 180,
                advance: 240,
            },
        ];
        bidi.font_instances[0].variations = vec![
            CapturedFontVariation { tag: 1, value: 0.0 },
            CapturedFontVariation {
                tag: 2,
                value: f32::from_bits(1),
            },
            CapturedFontVariation {
                tag: 3,
                value: f32::from_bits(0x8000_0001),
            },
            CapturedFontVariation {
                tag: 4,
                value: f32::MAX,
            },
        ];
        let encoded = encode_profile_null_scene(&request, &bidi, |_| None).unwrap();
        let scene: Value = serde_json::from_slice(&encoded.bytes).unwrap();
        let glyphs = scene["pages"][0]["operations"][0]["glyphs"]
            .as_array()
            .unwrap();
        assert_eq!(
            glyphs[0]["text_range"],
            serde_json::json!({"start": 2, "end": 4})
        );
        assert_eq!(
            glyphs[1]["text_range"],
            serde_json::json!({"start": 0, "end": 2})
        );
        let variations = scene["pages"][0]["operations"][0]["font"]["variations"]
            .as_array()
            .unwrap();
        assert_eq!(
            variations
                .iter()
                .map(|variation| variation["value_f32_bits"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![0, 1, 0x8000_0001, u64::from(f32::MAX.to_bits())]
        );

        for range in [
            Utf8Range { start: 0, end: 0 },
            Utf8Range { start: 0, end: 5 },
            Utf8Range { start: 1, end: 2 },
        ] {
            let mut invalid = bidi.clone();
            let Operation::Text { glyphs, .. } = &mut invalid.scene.pages[0].operations[0] else {
                unreachable!();
            };
            glyphs[0].text_range = Some(range);
            let error = encode_profile_null_scene(&request, &invalid, |_| None).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("range is empty or outside UTF-8 text") ||
                    error
                        .to_string()
                        .contains("range is not on UTF-8 boundaries")
            );
        }

        for (variations, expected) in [
            (
                vec![
                    CapturedFontVariation { tag: 2, value: 1.0 },
                    CapturedFontVariation { tag: 1, value: 1.0 },
                ],
                "tags are not strictly ascending",
            ),
            (
                vec![
                    CapturedFontVariation { tag: 1, value: 1.0 },
                    CapturedFontVariation { tag: 1, value: 2.0 },
                ],
                "tags are not strictly ascending",
            ),
            (
                vec![CapturedFontVariation {
                    tag: 1,
                    value: -0.0,
                }],
                "not canonical finite binary32",
            ),
            (
                vec![CapturedFontVariation {
                    tag: 1,
                    value: f32::NAN,
                }],
                "not canonical finite binary32",
            ),
        ] {
            let mut invalid = fixture_capture();
            invalid.font_instances[0].variations = variations;
            assert!(
                encode_profile_null_scene(&request, &invalid, |_| None)
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
    }

    #[test]
    fn encoder_binds_supported_image_signatures_and_rejects_missing_or_forged_bodies() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        for (bytes, media_type) in [
            (b"\x89PNG\r\n\x1a\n".as_slice(), "image/png"),
            (b"\xff\xd8\xffjpeg".as_slice(), "image/jpeg"),
            (b"GIF87a".as_slice(), "image/gif"),
            (b"GIF89a".as_slice(), "image/gif"),
            (b"RIFFxxxxWEBP".as_slice(), "image/webp"),
        ] {
            let resource = content_address(bytes);
            let mut capture = fixture_capture();
            let Operation::Image {
                resource: operation_resource,
                ..
            } = &mut capture.scene.pages[0].operations[2]
            else {
                unreachable!();
            };
            *operation_resource = resource.clone();
            capture.canvas_resources = vec![CanvasResource {
                resource: resource.clone(),
                png: bytes.to_vec(),
            }];
            let encoded = encode_profile_null_scene(&request, &capture, |_| None).unwrap();
            assert_eq!(encoded.resources[&resource].media_type, media_type);
            assert_eq!(encoded.resources[&resource].bytes, bytes);
        }

        let mut resolver_backed = fixture_capture();
        resolver_backed.canvas_resources.clear();
        let encoded = encode_profile_null_scene(&request, &resolver_backed, |resource| {
            (resource == IMAGE_RESOURCE).then_some(IMAGE_BYTES)
        })
        .unwrap();
        assert_eq!(encoded.resources[IMAGE_RESOURCE].media_type, "image/png");
        assert_eq!(encoded.resources[IMAGE_RESOURCE].bytes, IMAGE_BYTES);

        let mut unsupported = fixture_capture();
        let bytes = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        let resource = content_address(bytes);
        let Operation::Image {
            resource: operation_resource,
            ..
        } = &mut unsupported.scene.pages[0].operations[2]
        else {
            unreachable!();
        };
        *operation_resource = resource.clone();
        unsupported.canvas_resources = vec![CanvasResource {
            resource,
            png: bytes.to_vec(),
        }];
        assert!(
            encode_profile_null_scene(&request, &unsupported, |_| None)
                .unwrap_err()
                .to_string()
                .contains("has unsupported bytes")
        );

        let mut missing = fixture_capture();
        missing.canvas_resources.clear();
        assert!(
            encode_profile_null_scene(&request, &missing, |_| None)
                .unwrap_err()
                .to_string()
                .contains("references missing image resource")
        );

        let mut forged = fixture_capture();
        forged.canvas_resources[0].png.push(0);
        assert!(
            encode_profile_null_scene(&request, &forged, |_| None)
                .unwrap_err()
                .to_string()
                .contains("does not match retained bytes")
        );
    }

    #[test]
    fn encoder_emits_only_referenced_resources_and_rejects_media_conflicts() {
        let request: Value = serde_json::from_slice(REQUEST).unwrap();
        let mut closure = fixture_capture();
        let unused_font = b"unused-font";
        closure.font_resources.push(CapturedFontResource {
            resource: content_address(unused_font),
            bytes_base64: BASE64_STANDARD.encode(unused_font),
        });
        let unused_image = b"unused-image";
        closure.canvas_resources.push(CanvasResource {
            resource: content_address(unused_image),
            png: unused_image.to_vec(),
        });
        let repeated_image = closure.scene.pages[0].operations[2].clone();
        closure.scene.pages[0].operations.push(repeated_image);
        let repeated_authority = closure.fixed_point_authority.page_operations[0][2].clone();
        closure.fixed_point_authority.page_operations[0].push(repeated_authority);
        let encoded = encode_profile_null_scene(&request, &closure, |_| None).unwrap();
        assert_eq!(encoded.resources.len(), 2);
        assert_eq!(encoded.resources[IMAGE_RESOURCE].bytes, IMAGE_BYTES);
        assert_eq!(encoded.resources[FONT_RESOURCE].bytes, FONT_BYTES);

        let mut conflict = fixture_capture();
        conflict.font_resources = vec![CapturedFontResource {
            resource: IMAGE_RESOURCE.into(),
            bytes_base64: BASE64_STANDARD.encode(IMAGE_BYTES),
        }];
        conflict.font_instances[0].resource = IMAGE_RESOURCE.into();
        assert!(
            encode_profile_null_scene(&request, &conflict, |_| None)
                .unwrap_err()
                .to_string()
                .contains("conflicting bytes or media type")
        );
    }
}
