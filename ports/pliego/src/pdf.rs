/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::fmt;

use krilla::action::{Action, LinkAction};
use krilla::annotation::{Annotation, LinkAnnotation, Target};
use krilla::color::rgb;
use krilla::geom::{PathBuilder, Point, Rect as PdfRect, Size as PdfSize, Transform};
use krilla::image::Image;
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill, FillRule as PdfFillRule, Stroke as PdfStroke};
use krilla::text::{Font, GlyphId, KrillaGlyph, Tag};
use krilla::{Data, Document, SerializeSettings};
use unicode_segmentation::UnicodeSegmentation;
use url::Url;
use vello_cpu::kurbo::{BezPath, PathEl};

use crate::image_limits::{ImageBudget, ImageLimitExceeded, check_encoded_bytes};
use crate::{Color, DocumentScene, FillRule, ImageLimit, Operation};

const MAX_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_LINK_BYTES: usize = 8 * 1024;
pub const CSS_PX_TO_PDF_PT: f64 = 72.0 / 96.0;

/// A variation coordinate retained for a PDF font instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfFontVariation {
    pub tag: u32,
    pub value: f32,
}

/// Borrowed exact font data supplied by the scene resource table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfFontResource<'a> {
    pub bytes: &'a [u8],
    pub face_index: u32,
    pub variations: &'a [PdfFontVariation],
    pub synthetic_bold: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontEmbeddingRestriction {
    RestrictedLicense,
    NoSubsetting,
    BitmapOnly,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PdfError {
    InvalidScene(&'static str),
    InvalidPageSize {
        width: f64,
        height: f64,
    },
    InvalidFontSize(f64),
    ValueOutOfRange {
        field: &'static str,
        value: f64,
    },
    MissingTextMapping {
        operation: usize,
        glyph: usize,
    },
    InvalidTextMapping {
        operation: usize,
        glyph: usize,
    },
    InvalidGlyph {
        operation: usize,
        glyph: usize,
    },
    MissingFont(String),
    InvalidFont(String),
    InvalidFontVariation(String),
    RestrictedFontEmbedding {
        resource: String,
        restriction: FontEmbeddingRestriction,
    },
    MissingImage(String),
    ResourceTooLarge {
        resource: String,
        bytes: usize,
    },
    ImageLimitExceeded {
        resource: String,
        limit: ImageLimit,
        configured: u64,
        observed: u64,
    },
    InvalidImage {
        resource: String,
        message: String,
    },
    InvalidPath {
        operation: usize,
        message: String,
    },
    InvalidRectangle {
        operation: usize,
        kind: &'static str,
    },
    UnsafeLink {
        operation: usize,
        target: String,
    },
    FontEmbedding(String),
    Backend(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScene(message) => write!(formatter, "invalid document scene: {message}"),
            Self::InvalidPageSize { width, height } => {
                write!(formatter, "invalid PDF page size {width}x{height}")
            },
            Self::InvalidFontSize(size) => write!(formatter, "invalid PDF font size {size}"),
            Self::ValueOutOfRange { field, value } => {
                write!(
                    formatter,
                    "{field} value {value} cannot be represented as f32"
                )
            },
            Self::MissingTextMapping { operation, glyph } => write!(
                formatter,
                "scene text operation {operation} glyph {glyph} has no source-text mapping"
            ),
            Self::InvalidTextMapping { operation, glyph } => write!(
                formatter,
                "scene text operation {operation} glyph {glyph} has an invalid source-text mapping"
            ),
            Self::InvalidGlyph { operation, glyph } => write!(
                formatter,
                "scene text operation {operation} glyph {glyph} is outside the OpenType glyph range"
            ),
            Self::MissingFont(resource) => {
                write!(formatter, "scene font resource not found: {resource}")
            },
            Self::InvalidFont(resource) => {
                write!(formatter, "scene font resource is invalid: {resource}")
            },
            Self::InvalidFontVariation(resource) => write!(
                formatter,
                "scene font resource has an invalid variation coordinate: {resource}"
            ),
            Self::RestrictedFontEmbedding {
                resource,
                restriction,
            } => {
                let reason = match restriction {
                    FontEmbeddingRestriction::RestrictedLicense => "has a restricted license",
                    FontEmbeddingRestriction::NoSubsetting => "prohibits subsetting",
                    FontEmbeddingRestriction::BitmapOnly => "permits bitmap embedding only",
                };
                write!(
                    formatter,
                    "scene font resource cannot be embedded by the PDF backend ({reason}): {resource}"
                )
            },
            Self::MissingImage(resource) => {
                write!(formatter, "scene image resource not found: {resource}")
            },
            Self::ResourceTooLarge { resource, bytes } => write!(
                formatter,
                "scene resource {resource} is too large for PDF export ({bytes} bytes)"
            ),
            Self::ImageLimitExceeded {
                resource,
                limit,
                configured,
                observed,
            } => write!(
                formatter,
                "scene image resource {resource} exceeds the {limit} limit (configured {configured}, observed {observed})"
            ),
            Self::InvalidImage { resource, message } => {
                write!(
                    formatter,
                    "scene image resource {resource} is invalid: {message}"
                )
            },
            Self::InvalidPath { operation, message } => {
                write!(
                    formatter,
                    "scene path operation {operation} is invalid: {message}"
                )
            },
            Self::InvalidRectangle { operation, kind } => {
                write!(
                    formatter,
                    "scene {kind} operation {operation} has an empty rectangle"
                )
            },
            Self::UnsafeLink { operation, target } => write!(
                formatter,
                "scene link operation {operation} has a disallowed target: {target}"
            ),
            Self::FontEmbedding(message) => {
                write!(formatter, "failed to embed scene font: {message}")
            },
            Self::Backend(message) => write!(formatter, "failed to serialize PDF: {message}"),
        }
    }
}

impl std::error::Error for PdfError {}

/// Serialize the validated scene directly to PDF without reshaping or laying out text again.
pub fn render_document_pdf<'font, 'image>(
    scene: &DocumentScene,
    mut resolve_font: impl FnMut(&str) -> Option<PdfFontResource<'font>>,
    mut resolve_image: impl FnMut(&str) -> Option<&'image [u8]>,
) -> Result<Vec<u8>, PdfError> {
    if let Err(message) = scene.validate() {
        if message.starts_with("glyph text range") {
            validate_text_mappings(scene)?;
        }
        return Err(PdfError::InvalidScene(message));
    }
    validate_text_mappings(scene)?;

    let mut settings = SerializeSettings::default();
    settings.enable_tagging = false;
    let mut document = Document::new_with(settings);
    let mut fonts = HashMap::<String, (Font, bool)>::new();
    let mut images = HashMap::<String, Image>::new();
    let mut image_budget = ImageBudget::default();
    for source_page in &scene.pages {
        let width = pdf_length(source_page.size.width, "page.width")?;
        let height = pdf_length(source_page.size.height, "page.height")?;
        let page_settings =
            PageSettings::from_wh(width, height).ok_or(PdfError::InvalidPageSize {
                width: source_page.size.width,
                height: source_page.size.height,
            })?;
        let mut page = document.start_page_with(page_settings);
        let mut annotations = Vec::<Annotation>::new();

        {
            let mut surface = page.surface();
            for (operation_index, operation) in source_page.operations.iter().enumerate() {
                match operation {
                    Operation::Text {
                        text,
                        font,
                        font_size,
                        color,
                        glyphs,
                        ..
                    } => {
                        let Some(first_glyph) = glyphs.first() else {
                            continue;
                        };
                        let source_font_size = *font_size;
                        let font_size = pdf_length(source_font_size, "font_size")?;
                        if font_size <= 0.0 {
                            return Err(PdfError::InvalidFontSize(source_font_size));
                        }
                        if !fonts.contains_key(font) {
                            let resource = resolve_font(font)
                                .ok_or_else(|| PdfError::MissingFont(font.clone()))?;
                            fonts.insert(
                                font.clone(),
                                (load_font(font, resource)?, resource.synthetic_bold),
                            );
                        }
                        let (pdf_font, synthetic_bold) = fonts[font].clone();
                        let text_opacity = pdf_opacity(color.a)?;
                        surface.set_fill(Some(Fill {
                            paint: pdf_color(color).into(),
                            opacity: if synthetic_bold {
                                NormalizedF32::ONE
                            } else {
                                text_opacity
                            },
                            rule: PdfFillRule::NonZero,
                        }));
                        surface.set_stroke(synthetic_bold.then(|| PdfStroke {
                            paint: pdf_color(color).into(),
                            opacity: NormalizedF32::ONE,
                            width: font_size / 24.0,
                            ..PdfStroke::default()
                        }));

                        // Keep the run intact so Krilla can span shared shaping clusters with ActualText.
                        let text_ranges = pdf_text_ranges(text, glyphs, operation_index)?;
                        let mut cursor_x = first_glyph.x;
                        let mut pdf_glyphs = Vec::with_capacity(glyphs.len());
                        for (glyph_index, (glyph, range)) in
                            glyphs.iter().zip(text_ranges).enumerate()
                        {
                            if glyph.id > u16::MAX.into() {
                                return Err(PdfError::InvalidGlyph {
                                    operation: operation_index,
                                    glyph: glyph_index,
                                });
                            }
                            pdf_glyphs.push(KrillaGlyph::new(
                                GlyphId::new(glyph.id),
                                pdf_number(glyph.advance / source_font_size, "glyph.advance")?,
                                pdf_number(
                                    (glyph.x - cursor_x) / source_font_size,
                                    "glyph.x_offset",
                                )?,
                                pdf_number(
                                    (first_glyph.y - glyph.y) / source_font_size,
                                    "glyph.y_offset",
                                )?,
                                0.0,
                                range,
                                None,
                            ));
                            cursor_x += glyph.advance;
                        }
                        if synthetic_bold && text_opacity != NormalizedF32::ONE {
                            surface.push_opacity(text_opacity);
                        }
                        surface.draw_glyphs(
                            Point::from_xy(
                                pdf_length(first_glyph.x, "glyph.x")?,
                                pdf_length(first_glyph.y, "glyph.y")?,
                            ),
                            &pdf_glyphs,
                            pdf_font,
                            text,
                            font_size,
                            false,
                        );
                        if synthetic_bold && text_opacity != NormalizedF32::ONE {
                            surface.pop();
                        }
                    },
                    Operation::Path {
                        data,
                        fill,
                        fill_rule,
                        stroke,
                        ..
                    } => {
                        surface.set_fill(
                            fill.as_ref()
                                .map(|color| pdf_fill(color, *fill_rule))
                                .transpose()?,
                        );
                        surface.set_stroke(match stroke {
                            Some(stroke) => Some(PdfStroke {
                                paint: pdf_color(&stroke.color).into(),
                                opacity: pdf_opacity(stroke.color.a)?,
                                width: pdf_length(stroke.width, "path.stroke.width")?,
                                ..PdfStroke::default()
                            }),
                            None => None,
                        });
                        let path = pdf_path(data, operation_index)?;
                        surface.draw_path(&path);
                    },
                    Operation::Image {
                        bounds, resource, ..
                    } => {
                        if !images.contains_key(resource) {
                            let bytes = resolve_image(resource)
                                .ok_or_else(|| PdfError::MissingImage(resource.clone()))?;
                            images.insert(
                                resource.clone(),
                                load_image(resource, bytes, &mut image_budget)?,
                            );
                        }
                        let size = PdfSize::from_wh(
                            pdf_length(bounds.width, "image.width")?,
                            pdf_length(bounds.height, "image.height")?,
                        )
                        .ok_or(PdfError::InvalidRectangle {
                            operation: operation_index,
                            kind: "image",
                        })?;
                        surface.push_transform(&Transform::from_translate(
                            pdf_length(bounds.x, "image.x")?,
                            pdf_length(bounds.y, "image.y")?,
                        ));
                        surface.draw_image(images[resource].clone(), size);
                        surface.pop();
                    },
                    Operation::Link { bounds, target, .. } => {
                        let rect = PdfRect::from_xywh(
                            pdf_length(bounds.x, "link.x")?,
                            pdf_length(bounds.y, "link.y")?,
                            pdf_length(bounds.width, "link.width")?,
                            pdf_length(bounds.height, "link.height")?,
                        )
                        .ok_or(PdfError::InvalidRectangle {
                            operation: operation_index,
                            kind: "link",
                        })?;
                        let target = safe_link_target(target, operation_index)?;
                        annotations.push(
                            LinkAnnotation::new(
                                rect,
                                Target::Action(Action::from(LinkAction::new(target))),
                            )
                            .into(),
                        );
                    },
                }
            }
            surface.finish();
        }

        for annotation in annotations {
            page.add_annotation(annotation);
        }
        page.finish();
    }
    document.finish().map_err(|error| match error {
        krilla::error::KrillaError::Font(_, message) => PdfError::FontEmbedding(message),
        error => PdfError::Backend(error.to_string()),
    })
}

/// Coalesce per-character shaping ranges into PDF extraction units. Positioned marks inside an
/// extended grapheme must share one `ActualText` span or PDF readers can infer word boundaries from
/// their positioning adjustments.
fn pdf_text_ranges(
    text: &str,
    glyphs: &[crate::Glyph],
    operation: usize,
) -> Result<Vec<std::ops::Range<usize>>, PdfError> {
    let graphemes = text
        .grapheme_indices(true)
        .map(|(start, grapheme)| start..start + grapheme.len())
        .collect::<Vec<_>>();
    let expanded = glyphs
        .iter()
        .enumerate()
        .map(|(glyph, value)| {
            let range = value
                .text_range
                .ok_or(PdfError::MissingTextMapping { operation, glyph })?;
            let range = usize::try_from(range.start)
                .map_err(|_| PdfError::InvalidTextMapping { operation, glyph })?
                ..usize::try_from(range.end)
                    .map_err(|_| PdfError::InvalidTextMapping { operation, glyph })?;
            let start = graphemes
                .iter()
                .find(|item| item.end > range.start)
                .ok_or(PdfError::InvalidTextMapping { operation, glyph })?
                .start;
            let end = graphemes
                .iter()
                .rev()
                .find(|item| item.start < range.end)
                .ok_or(PdfError::InvalidTextMapping { operation, glyph })?
                .end;
            Ok(start..end)
        })
        .collect::<Result<Vec<_>, PdfError>>()?;
    // Poppler ignores ReversedChars, so a visual-order RTL run needs one logical ActualText span.
    if let (Some(first), Some(last)) = (expanded.first(), expanded.last()) &&
        expanded.len() > 1 &&
        expanded
            .windows(2)
            .all(|ranges| ranges[0].start >= ranges[1].start) &&
        first.start > last.start
    {
        return Ok(vec![0..text.len(); expanded.len()]);
    }

    let mut sorted = expanded.clone();
    sorted.sort_by_key(|range| range.start);
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for range in sorted {
        if let Some(group) = groups.last_mut() &&
            range.start < group.end
        {
            group.end = group.end.max(range.end);
        } else {
            groups.push(range);
        }
    }
    expanded
        .into_iter()
        .enumerate()
        .map(|(glyph, range)| {
            groups
                .iter()
                .find(|group| group.start <= range.start && group.end >= range.end)
                .cloned()
                .ok_or(PdfError::InvalidTextMapping { operation, glyph })
        })
        .collect()
}

fn validate_text_mappings(scene: &DocumentScene) -> Result<(), PdfError> {
    for page in &scene.pages {
        for (operation_index, operation) in page.operations.iter().enumerate() {
            let Operation::Text { text, glyphs, .. } = operation else {
                continue;
            };
            for (glyph_index, glyph) in glyphs.iter().enumerate() {
                let Some(range) = glyph.text_range else {
                    return Err(PdfError::MissingTextMapping {
                        operation: operation_index,
                        glyph: glyph_index,
                    });
                };
                let (Ok(start), Ok(end)) =
                    (usize::try_from(range.start), usize::try_from(range.end))
                else {
                    return Err(PdfError::InvalidTextMapping {
                        operation: operation_index,
                        glyph: glyph_index,
                    });
                };
                if start >= end ||
                    end > text.len() ||
                    !text.is_char_boundary(start) ||
                    !text.is_char_boundary(end)
                {
                    return Err(PdfError::InvalidTextMapping {
                        operation: operation_index,
                        glyph: glyph_index,
                    });
                }
            }
        }
    }
    Ok(())
}

fn load_font(resource_id: &str, resource: PdfFontResource<'_>) -> Result<Font, PdfError> {
    ensure_resource_size(resource_id, resource.bytes)?;
    if resource
        .variations
        .iter()
        .any(|variation| !variation.value.is_finite())
    {
        return Err(PdfError::InvalidFontVariation(resource_id.into()));
    }
    if let Some(restriction) = embedding_restriction(resource.bytes, resource.face_index)
        .map_err(|()| PdfError::InvalidFont(resource_id.into()))?
    {
        return Err(PdfError::RestrictedFontEmbedding {
            resource: resource_id.into(),
            restriction,
        });
    }

    let variations = resource
        .variations
        .iter()
        .map(|variation| (Tag::new(&variation.tag.to_be_bytes()), variation.value))
        .collect::<Vec<_>>();
    Font::new_variable(
        Data::from(resource.bytes.to_vec()),
        resource.face_index,
        &variations,
    )
    .ok_or_else(|| PdfError::InvalidFont(resource_id.into()))
}

fn load_image(
    resource_id: &str,
    bytes: &[u8],
    image_budget: &mut ImageBudget,
) -> Result<Image, PdfError> {
    check_encoded_bytes(bytes.len()).map_err(|error| image_limit_error(resource_id, error))?;
    let png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    let jpeg = bytes.starts_with(b"\xff\xd8\xff");
    let gif = bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a");
    let webp = bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP";
    let decompressed_bytes_per_pixel = if png {
        // Krilla expands supported PNGs to at most RGBA at their declared bit depth.
        if bytes.get(24) == Some(&16) { 8 } else { 4 }
    } else if gif || webp {
        4
    } else if jpeg {
        // Krilla retains JPEG data and only reads its headers in this path.
        0
    } else {
        return Err(PdfError::InvalidImage {
            resource: resource_id.into(),
            message: "unsupported image format".into(),
        });
    };
    let dimensions = imagesize::blob_size(bytes).map_err(|error| PdfError::InvalidImage {
        resource: resource_id.into(),
        message: error.to_string(),
    })?;
    let charge = image_budget
        .check(
            u64::try_from(dimensions.width).unwrap_or(u64::MAX),
            u64::try_from(dimensions.height).unwrap_or(u64::MAX),
            decompressed_bytes_per_pixel,
        )
        .map_err(|error| image_limit_error(resource_id, error))?;

    let data = Data::from(bytes.to_vec());
    let result = if png {
        Image::from_png(data, false)
    } else if jpeg {
        Image::from_jpeg(data, false)
    } else if gif {
        Image::from_gif(data, false)
    } else if webp {
        Image::from_webp(data, false)
    } else {
        return Err(PdfError::InvalidImage {
            resource: resource_id.into(),
            message: "unsupported image format".into(),
        });
    };
    let image = result.map_err(|message| PdfError::InvalidImage {
        resource: resource_id.into(),
        message,
    })?;
    image_budget.commit(charge);
    Ok(image)
}

fn image_limit_error(resource: &str, error: ImageLimitExceeded) -> PdfError {
    PdfError::ImageLimitExceeded {
        resource: resource.into(),
        limit: error.limit,
        configured: error.configured,
        observed: error.observed,
    }
}

fn ensure_resource_size(resource_id: &str, bytes: &[u8]) -> Result<(), PdfError> {
    if bytes.len() > MAX_RESOURCE_BYTES {
        Err(PdfError::ResourceTooLarge {
            resource: resource_id.into(),
            bytes: bytes.len(),
        })
    } else {
        Ok(())
    }
}

fn pdf_path(data: &str, operation: usize) -> Result<krilla::geom::Path, PdfError> {
    let source = BezPath::from_svg(data).map_err(|error| PdfError::InvalidPath {
        operation,
        message: error.to_string(),
    })?;
    let mut target = PathBuilder::new();
    for element in source.elements() {
        match element {
            PathEl::MoveTo(point) => target.move_to(
                pdf_length(point.x, "path.x")?,
                pdf_length(point.y, "path.y")?,
            ),
            PathEl::LineTo(point) => target.line_to(
                pdf_length(point.x, "path.x")?,
                pdf_length(point.y, "path.y")?,
            ),
            PathEl::QuadTo(control, point) => target.quad_to(
                pdf_length(control.x, "path.control_x")?,
                pdf_length(control.y, "path.control_y")?,
                pdf_length(point.x, "path.x")?,
                pdf_length(point.y, "path.y")?,
            ),
            PathEl::CurveTo(first, second, point) => target.cubic_to(
                pdf_length(first.x, "path.control_x")?,
                pdf_length(first.y, "path.control_y")?,
                pdf_length(second.x, "path.control_x")?,
                pdf_length(second.y, "path.control_y")?,
                pdf_length(point.x, "path.x")?,
                pdf_length(point.y, "path.y")?,
            ),
            PathEl::ClosePath => target.close(),
        }
    }
    target.finish().ok_or_else(|| PdfError::InvalidPath {
        operation,
        message: "path is empty".into(),
    })
}

fn pdf_fill(color: &Color, rule: FillRule) -> Result<Fill, PdfError> {
    Ok(Fill {
        paint: pdf_color(color).into(),
        opacity: pdf_opacity(color.a)?,
        rule: match rule {
            FillRule::NonZero => PdfFillRule::NonZero,
            FillRule::EvenOdd => PdfFillRule::EvenOdd,
        },
    })
}

fn pdf_color(color: &Color) -> rgb::Color {
    let channel = |value: f64| (value * 255.0).round() as u8;
    rgb::Color::new(channel(color.r), channel(color.g), channel(color.b))
}

fn pdf_opacity(value: f64) -> Result<NormalizedF32, PdfError> {
    NormalizedF32::new(value as f32).ok_or(PdfError::ValueOutOfRange {
        field: "color.alpha",
        value,
    })
}

fn safe_link_target(target: &str, operation: usize) -> Result<String, PdfError> {
    if target.len() > MAX_LINK_BYTES || target.chars().any(char::is_control) {
        return Err(PdfError::UnsafeLink {
            operation,
            target: target.into(),
        });
    }
    let url = Url::parse(target).map_err(|_| PdfError::UnsafeLink {
        operation,
        target: target.into(),
    })?;
    if !matches!(url.scheme(), "http" | "https" | "mailto") {
        return Err(PdfError::UnsafeLink {
            operation,
            target: target.into(),
        });
    }
    Ok(url.to_string())
}

fn pdf_number(value: f64, field: &'static str) -> Result<f32, PdfError> {
    if value.is_finite() && value >= f64::from(f32::MIN) && value <= f64::from(f32::MAX) {
        Ok(value as f32)
    } else {
        Err(PdfError::ValueOutOfRange { field, value })
    }
}

fn pdf_length(value: f64, field: &'static str) -> Result<f32, PdfError> {
    pdf_number(value * CSS_PX_TO_PDF_PT, field)
}

fn embedding_restriction(
    bytes: &[u8],
    face_index: u32,
) -> Result<Option<FontEmbeddingRestriction>, ()> {
    let face_offset = if bytes.starts_with(b"ttcf") {
        let count = read_u32(bytes, 8).ok_or(())?;
        if face_index >= count {
            return Err(());
        }
        usize::try_from(
            read_u32(
                bytes,
                12usize
                    .checked_add(
                        usize::try_from(face_index)
                            .map_err(|_| ())?
                            .checked_mul(4)
                            .ok_or(())?,
                    )
                    .ok_or(())?,
            )
            .ok_or(())?,
        )
        .map_err(|_| ())?
    } else if face_index == 0 {
        0
    } else {
        return Err(());
    };
    let table_count =
        usize::from(read_u16(bytes, face_offset.checked_add(4).ok_or(())?).ok_or(())?);
    let table_start = face_offset.checked_add(12).ok_or(())?;
    for index in 0..table_count {
        let record = table_start
            .checked_add(index.checked_mul(16).ok_or(())?)
            .ok_or(())?;
        if bytes.get(record..record.checked_add(4).ok_or(())?) == Some(b"OS/2") {
            let offset =
                usize::try_from(read_u32(bytes, record.checked_add(8).ok_or(())?).ok_or(())?)
                    .map_err(|_| ())?;
            let fs_type = read_u16(bytes, offset.checked_add(8).ok_or(())?).ok_or(())?;
            return Ok(if fs_type & 0x0002 != 0 {
                Some(FontEmbeddingRestriction::RestrictedLicense)
            } else if fs_type & 0x0100 != 0 {
                Some(FontEmbeddingRestriction::NoSubsetting)
            } else if fs_type & 0x0200 != 0 {
                Some(FontEmbeddingRestriction::BitmapOnly)
            } else {
                None
            });
        }
    }
    Ok(None)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}
