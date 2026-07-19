/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::fmt;

use vello_cpu::color::palette::css::BLACK;
use vello_cpu::peniko::{Blob, FontData};
use vello_cpu::{Glyph, Level, Pixmap, RenderContext, RenderMode, RenderSettings, Resources};

use crate::{DocumentScene, Operation};

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
    InvalidPageSize { width: f64, height: f64 },
    ValueOutOfRange { field: &'static str, value: f64 },
    MissingFont(String),
    UnsupportedFontVariations(String),
    UnsupportedOperation { index: usize, kind: &'static str },
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
            Self::PngEncoding(message) => write!(formatter, "cannot encode scene PNG: {message}"),
        }
    }
}

impl std::error::Error for RasterError {}

/// Rasterize the single page in a validated v1 scene without shaping or laying out text again.
///
/// This first structural backend paints positioned glyphs in opaque black and treats links as
/// non-painting metadata. Paths, images, and variable font instances return typed errors until the
/// scene carries the paint and normalized-axis data needed to render them truthfully.
pub fn render_first_page_png<'a>(
    scene: &DocumentScene,
    mut resolve_font: impl FnMut(&str) -> Option<RasterFontResource<'a>>,
) -> Result<Vec<u8>, RasterError> {
    scene.validate().map_err(RasterError::InvalidScene)?;
    let page = &scene.pages[0];
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
    let mut fonts = HashMap::<String, FontData>::new();
    context.set_paint(BLACK);

    for (index, operation) in page.operations.iter().enumerate() {
        match operation {
            Operation::Text {
                font,
                font_size,
                glyphs,
                ..
            } => {
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
            Operation::Path { .. } => {
                return Err(RasterError::UnsupportedOperation {
                    index,
                    kind: "path",
                });
            },
            Operation::Image { .. } => {
                return Err(RasterError::UnsupportedOperation {
                    index,
                    kind: "image",
                });
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
