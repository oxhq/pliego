/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use vello_cpu::color::palette::css::BLACK;
use vello_cpu::color::{AlphaColor, Srgb};
use vello_cpu::kurbo::{Affine, BezPath, Rect as KurboRect, Stroke as KurboStroke};
use vello_cpu::peniko::{self, Blob, FontData};
use vello_cpu::{
    Glyph, Image, ImageSource, Level, Pixmap, RenderContext, RenderMode, RenderSettings, Resources,
};

use crate::image_limits::{ImageBudget, ImageLimitExceeded, check_encoded_bytes};
use crate::{Color, DocumentScene, FillRule, ImageLimit, Operation, Page};

/// A variation coordinate retained for a font instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterFontVariation {
    pub tag: u32,
    pub value: f32,
}

/// Borrowed font data supplied by the scene resource table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterFontResource<'a> {
    pub bytes: &'a [u8],
    pub face_index: u32,
    pub variations: &'a [RasterFontVariation],
}

#[derive(Clone, Debug, PartialEq)]
pub enum RasterError {
    InvalidScene(&'static str),
    InvalidPageSize {
        width: f64,
        height: f64,
    },
    ValueOutOfRange {
        field: &'static str,
        value: f64,
    },
    MissingFont(String),
    UnsupportedFontVariations(String),
    UnsupportedOperation {
        index: usize,
        kind: &'static str,
    },
    InvalidPath {
        index: usize,
        message: String,
    },
    MissingImage(String),
    ImageLimitExceeded {
        resource: String,
        limit: ImageLimit,
        configured: u64,
        observed: u64,
    },
    InvalidImage {
        index: usize,
        resource: String,
        message: String,
    },
    PngEncoding(String),
}

impl fmt::Display for RasterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScene(message) => write!(formatter, "invalid document scene: {message}"),
            Self::InvalidPageSize { width, height } => write!(
                formatter,
                "page size {width}x{height} cannot be represented by the CPU raster backend"
            ),
            Self::ValueOutOfRange { field, value } => {
                write!(
                    formatter,
                    "{field} value {value} cannot be represented as f32"
                )
            },
            Self::MissingFont(font) => write!(formatter, "scene font resource not found: {font}"),
            Self::UnsupportedFontVariations(font) => write!(
                formatter,
                "variable font instance is not supported by the structural raster backend: {font}"
            ),
            Self::UnsupportedOperation { index, kind } => {
                write!(
                    formatter,
                    "scene operation {index} has unsupported type {kind}"
                )
            },
            Self::InvalidPath { index, message } => {
                write!(
                    formatter,
                    "scene operation {index} has an invalid path: {message}"
                )
            },
            Self::MissingImage(resource) => {
                write!(formatter, "scene image resource not found: {resource}")
            },
            Self::ImageLimitExceeded {
                resource,
                limit,
                configured,
                observed,
            } => write!(
                formatter,
                "scene image resource {resource} exceeds the {limit} limit (configured {configured}, observed {observed})"
            ),
            Self::InvalidImage {
                index,
                resource,
                message,
            } => write!(
                formatter,
                "scene operation {index} image resource {resource} is invalid: {message}"
            ),
            Self::PngEncoding(message) => write!(formatter, "cannot encode scene PNG: {message}"),
        }
    }
}

impl std::error::Error for RasterError {}

/// Rasterize the single page in a validated v1 scene without shaping or laying out text again.
///
/// This compatibility entry point paints positioned glyphs and SVG paths, treats links as
/// non-painting metadata, and returns `UnsupportedOperation` for images. Call
/// [`render_first_page_png_with_images`] when exact image resource bytes are available.
pub fn render_first_page_png<'font>(
    scene: &DocumentScene,
    resolve_font: impl FnMut(&str) -> Option<RasterFontResource<'font>>,
) -> Result<Vec<u8>, RasterError> {
    render_first_page_png_impl(scene, resolve_font, |_| None::<&'static [u8]>, false)
}

/// Rasterize the single page directly from its canonical scene and exact PNG resource bytes.
pub fn render_first_page_png_with_images<'font, 'image>(
    scene: &DocumentScene,
    resolve_font: impl FnMut(&str) -> Option<RasterFontResource<'font>>,
    resolve_image: impl FnMut(&str) -> Option<&'image [u8]>,
) -> Result<Vec<u8>, RasterError> {
    render_first_page_png_impl(scene, resolve_font, resolve_image, true)
}

/// Rasterize every canonical scene page into one PNG, preserving page-local geometry and paint.
/// Font and decoded image resources are cached across the whole document.
pub fn render_pages_png_with_images<'font, 'image>(
    scene: &DocumentScene,
    mut resolve_font: impl FnMut(&str) -> Option<RasterFontResource<'font>>,
    mut resolve_image: impl FnMut(&str) -> Option<&'image [u8]>,
) -> Result<Vec<Vec<u8>>, RasterError> {
    scene.validate().map_err(RasterError::InvalidScene)?;
    let mut fonts = HashMap::<String, FontData>::new();
    let mut images = HashMap::<String, Arc<Pixmap>>::new();
    let mut image_budget = ImageBudget::default();
    scene
        .pages
        .iter()
        .map(|page| {
            render_page_png(
                page,
                &mut resolve_font,
                &mut resolve_image,
                true,
                &mut fonts,
                &mut images,
                &mut image_budget,
            )
        })
        .collect()
}

fn render_first_page_png_impl<'font, 'image>(
    scene: &DocumentScene,
    mut resolve_font: impl FnMut(&str) -> Option<RasterFontResource<'font>>,
    mut resolve_image: impl FnMut(&str) -> Option<&'image [u8]>,
    images_supported: bool,
) -> Result<Vec<u8>, RasterError> {
    scene.validate().map_err(RasterError::InvalidScene)?;
    let mut fonts = HashMap::<String, FontData>::new();
    let mut images = HashMap::<String, Arc<Pixmap>>::new();
    let mut image_budget = ImageBudget::default();
    render_page_png(
        &scene.pages[0],
        &mut resolve_font,
        &mut resolve_image,
        images_supported,
        &mut fonts,
        &mut images,
        &mut image_budget,
    )
}

fn render_page_png<'font, 'image>(
    page: &Page,
    resolve_font: &mut impl FnMut(&str) -> Option<RasterFontResource<'font>>,
    resolve_image: &mut impl FnMut(&str) -> Option<&'image [u8]>,
    images_supported: bool,
    fonts: &mut HashMap<String, FontData>,
    images: &mut HashMap<String, Arc<Pixmap>>,
    image_budget: &mut ImageBudget,
) -> Result<Vec<u8>, RasterError> {
    let width = raster_dimension(page.size.width, page.size.width, page.size.height)?;
    let height = raster_dimension(page.size.height, page.size.width, page.size.height)?;

    let mut context = RenderContext::new_with(
        width,
        height,
        RenderSettings {
            level: Level::baseline(),
            num_threads: 0,
            render_mode: RenderMode::OptimizeSpeed,
        },
    );
    let mut resources = Resources::new();
    context.set_paint(BLACK);

    for (index, operation) in page.operations.iter().enumerate() {
        match operation {
            Operation::Text {
                font,
                font_size,
                glyphs,
                ..
            } => {
                context.set_paint(BLACK);
                if !fonts.contains_key(font) {
                    let resource =
                        resolve_font(font).ok_or_else(|| RasterError::MissingFont(font.clone()))?;
                    if !resource.variations.is_empty() {
                        return Err(RasterError::UnsupportedFontVariations(font.clone()));
                    }
                    fonts.insert(
                        font.clone(),
                        FontData::new(Blob::from(resource.bytes.to_vec()), resource.face_index),
                    );
                }

                let positioned_glyphs = glyphs
                    .iter()
                    .map(|glyph| {
                        Ok(Glyph {
                            id: glyph.id,
                            x: raster_value(glyph.x, "glyph.x")?,
                            y: raster_value(glyph.y, "glyph.y")?,
                        })
                    })
                    .collect::<Result<Vec<_>, RasterError>>()?;
                let font_size = raster_value(*font_size, "font_size")?;
                context
                    .glyph_run(&mut resources, &fonts[font])
                    .font_size(font_size)
                    .hint(false)
                    .fill_glyphs(positioned_glyphs.into_iter());
            },
            Operation::Link { .. } => {},
            Operation::Path {
                data,
                fill,
                fill_rule,
                stroke,
                ..
            } => {
                let path = BezPath::from_svg(data).map_err(|error| RasterError::InvalidPath {
                    index,
                    message: error.to_string(),
                })?;
                if let Some(fill) = fill {
                    context.set_fill_rule(match fill_rule {
                        FillRule::NonZero => peniko::Fill::NonZero,
                        FillRule::EvenOdd => peniko::Fill::EvenOdd,
                    });
                    context.set_paint(raster_color(fill));
                    context.fill_path(&path);
                }
                if let Some(stroke) = stroke {
                    context.set_paint(raster_color(&stroke.color));
                    context.set_stroke(KurboStroke::new(
                        raster_value(stroke.width, "path.stroke.width")?.into(),
                    ));
                    context.stroke_path(&path);
                }
            },
            Operation::Image {
                bounds, resource, ..
            } => {
                if !images_supported {
                    return Err(RasterError::UnsupportedOperation {
                        index,
                        kind: "image",
                    });
                }
                if !images.contains_key(resource) {
                    let bytes = resolve_image(resource)
                        .ok_or_else(|| RasterError::MissingImage(resource.clone()))?;
                    images.insert(
                        resource.clone(),
                        load_png(index, resource, bytes, image_budget)?,
                    );
                }
                let image = images[resource].clone();
                let x = f64::from(raster_value(bounds.x, "image.x")?);
                let y = f64::from(raster_value(bounds.y, "image.y")?);
                let width = f64::from(raster_value(bounds.width, "image.width")?);
                let height = f64::from(raster_value(bounds.height, "image.height")?);
                let destination = KurboRect::new(x, y, x + width, y + height);
                let transform = Affine::translate((x, y)).pre_scale_non_uniform(
                    width / f64::from(image.width()),
                    height / f64::from(image.height()),
                );
                context.set_paint(Image {
                    image: ImageSource::Pixmap(image),
                    sampler: peniko::ImageSampler {
                        quality: peniko::ImageQuality::Low,
                        ..Default::default()
                    },
                });
                context.set_paint_transform(transform);
                context.fill_rect(&destination);
                context.reset_paint_transform();
            },
        }
    }

    context.flush();
    let mut pixmap = Pixmap::new(width, height);
    context.render_to_pixmap(&mut resources, &mut pixmap);
    pixmap
        .into_png()
        .map_err(|error| RasterError::PngEncoding(error.to_string()))
}

fn load_png(
    index: usize,
    resource: &str,
    bytes: &[u8],
    image_budget: &mut ImageBudget,
) -> Result<Arc<Pixmap>, RasterError> {
    check_encoded_bytes(bytes.len()).map_err(|error| image_limit_error(resource, error))?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(RasterError::InvalidImage {
            index,
            resource: resource.into(),
            message: "CPU preview supports PNG resources only".into(),
        });
    }
    let (width, height) = png_dimensions(bytes).ok_or_else(|| RasterError::InvalidImage {
        index,
        resource: resource.into(),
        message: "PNG resource has no valid IHDR dimensions".into(),
    })?;
    let charge = image_budget
        .check(width, height, 4)
        .map_err(|error| image_limit_error(resource, error))?;
    let pixmap =
        Pixmap::from_png(Cursor::new(bytes)).map_err(|error| RasterError::InvalidImage {
            index,
            resource: resource.into(),
            message: error.to_string(),
        })?;
    image_budget.commit(charge);
    Ok(Arc::new(pixmap))
}

fn image_limit_error(resource: &str, error: ImageLimitExceeded) -> RasterError {
    RasterError::ImageLimitExceeded {
        resource: resource.into(),
        limit: error.limit,
        configured: error.configured,
        observed: error.observed,
    }
}

fn png_dimensions(bytes: &[u8]) -> Option<(u64, u64)> {
    let header = bytes.get(8..24)?;
    if header[..4] != 13_u32.to_be_bytes() || &header[4..8] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[8..12].try_into().ok()?);
    let height = u32::from_be_bytes(header[12..16].try_into().ok()?);
    (width != 0 && height != 0).then(|| (u64::from(width), u64::from(height)))
}

fn raster_color(color: &Color) -> AlphaColor<Srgb> {
    AlphaColor::new([
        color.r as f32,
        color.g as f32,
        color.b as f32,
        color.a as f32,
    ])
}

fn raster_dimension(value: f64, width: f64, height: f64) -> Result<u16, RasterError> {
    let value = value.ceil();
    if value >= 1.0 && value <= f64::from(u16::MAX) {
        Ok(value as u16)
    } else {
        Err(RasterError::InvalidPageSize { width, height })
    }
}

fn raster_value(value: f64, field: &'static str) -> Result<f32, RasterError> {
    if value.is_finite() && value >= f64::from(f32::MIN) && value <= f64::from(f32::MAX) {
        Ok(value as f32)
    } else {
        Err(RasterError::ValueOutOfRange { field, value })
    }
}
