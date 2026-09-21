/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use servo_canvas::retained_canvas::{
    RetainedCanvasCommand, RetainedCanvasFallbackReason, RetainedCanvasSnapshot,
};
use sha2::{Digest, Sha256};
use vello_cpu::Pixmap;

use crate::{
    Color, DocumentScene, FillRule, IMAGE_LIMITS, Operation, OperationMeta, Page, Rect, Size,
};

/// Stable prototype input for the Canvas subset that Pliego can preserve without pixels.
///
/// This is deliberately not Servo's full `CanvasCommand`: the paint thread translates the bounded
/// subset into this stable form while preserving the normal raster Canvas path.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasTranscript {
    pub size: Size,
    pub commands: Vec<CanvasCommand>,
}

/// The initial vector-safe subset is an identity-transformed, source-over `fillRect` with a solid
/// color and no clip, shadow, or filter. Pixel-dependent output must arrive as the exact affected
/// premultiplied RGBA patch; the adapter never reimplements browser pixel semantics.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanvasCommand {
    FillRect {
        bounds: Rect,
        color: Color,
    },
    RasterPatch {
        bounds: Rect,
        width: u32,
        height: u32,
        premultiplied_rgba: Vec<u8>,
        reason: CanvasFallbackReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasFallbackReason {
    PixelReadback,
    Filter,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CanvasFallback {
    pub command_index: usize,
    pub reason: CanvasFallbackReason,
    pub bounds: Rect,
    pub area_px: u64,
    pub resource: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CanvasDiagnostics {
    pub schema: &'static str,
    pub version: u32,
    pub vector_operation_count: usize,
    pub rasterized_area_px: u64,
    pub fallbacks: Vec<CanvasFallback>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CanvasResource {
    pub resource: String,
    pub png: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HybridCanvasCapture {
    pub scene: DocumentScene,
    pub resources: Vec<CanvasResource>,
    pub diagnostics: CanvasDiagnostics,
}

impl HybridCanvasCapture {
    pub fn diagnostics_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&self.diagnostics)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasError {
    InvalidScene(&'static str),
    InvalidRasterBounds {
        command_index: usize,
    },
    RasterLength {
        command_index: usize,
        expected: u64,
        actual: usize,
    },
    RasterEncoding(String),
    UnsupportedLiveCommand {
        command: &'static str,
        reason: String,
    },
}

impl fmt::Display for CanvasError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScene(message) => {
                write!(formatter, "invalid hybrid Canvas scene: {message}")
            },
            Self::InvalidRasterBounds { command_index } => write!(
                formatter,
                "hybrid Canvas command {command_index} has invalid raster bounds"
            ),
            Self::RasterLength {
                command_index,
                expected,
                actual,
            } => write!(
                formatter,
                "hybrid Canvas command {command_index} has {actual} premultiplied RGBA bytes, expected {expected}"
            ),
            Self::RasterEncoding(message) => {
                write!(formatter, "cannot encode hybrid Canvas patch: {message}")
            },
            Self::UnsupportedLiveCommand { command, reason } => write!(
                formatter,
                "live Canvas command {command} is outside the retained subset: {reason}"
            ),
        }
    }
}

impl std::error::Error for CanvasError {}

pub fn transcript_from_retained(
    snapshot: Arc<RetainedCanvasSnapshot>,
) -> Result<CanvasTranscript, CanvasError> {
    let snapshot = Arc::unwrap_or_clone(snapshot);
    if let Some(unsupported) = snapshot.unsupported {
        return Err(CanvasError::UnsupportedLiveCommand {
            command: unsupported.command,
            reason: format!("{:?}", unsupported.reason).to_ascii_lowercase(),
        });
    }
    Ok(CanvasTranscript {
        size: Size {
            width: f64::from(snapshot.width),
            height: f64::from(snapshot.height),
        },
        commands: snapshot
            .commands
            .into_iter()
            .map(|command| match command {
                RetainedCanvasCommand::FillRect {
                    x,
                    y,
                    width,
                    height,
                    red,
                    green,
                    blue,
                    alpha,
                } => CanvasCommand::FillRect {
                    bounds: Rect {
                        x,
                        y,
                        width,
                        height,
                    },
                    color: Color {
                        r: red,
                        g: green,
                        b: blue,
                        a: alpha,
                    },
                },
                RetainedCanvasCommand::RasterPatch {
                    x,
                    y,
                    width,
                    height,
                    premultiplied_rgba,
                    reason,
                } => CanvasCommand::RasterPatch {
                    bounds: Rect {
                        x,
                        y,
                        width: f64::from(width),
                        height: f64::from(height),
                    },
                    width,
                    height,
                    premultiplied_rgba,
                    reason: match reason {
                        RetainedCanvasFallbackReason::PixelReadback => {
                            CanvasFallbackReason::PixelReadback
                        },
                    },
                },
            })
            .collect(),
    })
}

pub fn adapt_canvas(transcript: CanvasTranscript) -> Result<HybridCanvasCapture, CanvasError> {
    DocumentScene::new(Page {
        size: transcript.size.clone(),
        operations: Vec::new(),
    })
    .validate()
    .map_err(CanvasError::InvalidScene)?;
    let mut operations = Vec::new();
    let mut resources = BTreeMap::new();
    let mut fallbacks = Vec::new();
    let mut vector_operation_count = 0;
    let mut rasterized_area_px = 0u64;

    for (command_index, command) in transcript.commands.into_iter().enumerate() {
        match command {
            CanvasCommand::FillRect { bounds, color } => {
                if bounds.width == 0.0 || bounds.height == 0.0 {
                    continue;
                }
                let data = format!(
                    "M{} {}h{}v{}h-{}z",
                    bounds.x, bounds.y, bounds.width, bounds.height, bounds.width
                );
                operations.push(Operation::Path {
                    bounds,
                    data,
                    fill: Some(color),
                    fill_rule: FillRule::NonZero,
                    stroke: None,
                    meta: OperationMeta::default(),
                });
                vector_operation_count += 1;
            },
            CanvasCommand::RasterPatch {
                bounds,
                width,
                height,
                premultiplied_rgba,
                reason,
            } => {
                let Some(area) = u64::from(width).checked_mul(u64::from(height)) else {
                    return Err(CanvasError::InvalidRasterBounds { command_index });
                };
                if width == 0 ||
                    height == 0 ||
                    u64::from(width) > IMAGE_LIMITS.declared_dimension ||
                    u64::from(height) > IMAGE_LIMITS.declared_dimension ||
                    area > IMAGE_LIMITS.decoded_pixels ||
                    bounds.width != f64::from(width) ||
                    bounds.height != f64::from(height) ||
                    !contained_by(&bounds, &transcript.size)
                {
                    return Err(CanvasError::InvalidRasterBounds { command_index });
                }
                let expected = area
                    .checked_mul(4)
                    .ok_or(CanvasError::InvalidRasterBounds { command_index })?;
                if expected > IMAGE_LIMITS.decompressed_bytes {
                    return Err(CanvasError::InvalidRasterBounds { command_index });
                }
                if u64::try_from(premultiplied_rgba.len()).unwrap_or(u64::MAX) != expected {
                    return Err(CanvasError::RasterLength {
                        command_index,
                        expected,
                        actual: premultiplied_rgba.len(),
                    });
                }
                let width = u16::try_from(width)
                    .map_err(|_| CanvasError::InvalidRasterBounds { command_index })?;
                let height = u16::try_from(height)
                    .map_err(|_| CanvasError::InvalidRasterBounds { command_index })?;
                let mut pixmap = Pixmap::new(width, height);
                pixmap
                    .data_as_u8_slice_mut()
                    .copy_from_slice(&premultiplied_rgba);
                let png = pixmap
                    .into_png()
                    .map_err(|error| CanvasError::RasterEncoding(error.to_string()))?;
                let resource = format!("sha256:{}", lowercase_hex(&Sha256::digest(&png)));
                resources.entry(resource.clone()).or_insert(png);
                rasterized_area_px = rasterized_area_px
                    .checked_add(area)
                    .ok_or(CanvasError::InvalidRasterBounds { command_index })?;
                fallbacks.push(CanvasFallback {
                    command_index,
                    reason,
                    bounds: bounds.clone(),
                    area_px: area,
                    resource: resource.clone(),
                });
                operations.push(Operation::Image {
                    bounds,
                    resource,
                    meta: OperationMeta::default(),
                });
            },
        }
    }

    let scene = DocumentScene::new(Page {
        size: transcript.size,
        operations,
    });
    scene.validate().map_err(CanvasError::InvalidScene)?;
    Ok(HybridCanvasCapture {
        scene,
        resources: resources
            .into_iter()
            .map(|(resource, png)| CanvasResource { resource, png })
            .collect(),
        diagnostics: CanvasDiagnostics {
            schema: "pliego.hybrid-canvas-diagnostics",
            version: 1,
            vector_operation_count,
            rasterized_area_px,
            fallbacks,
        },
    })
}

fn contained_by(bounds: &Rect, size: &Size) -> bool {
    [bounds.x, bounds.y, bounds.width, bounds.height]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0 && !value.is_sign_negative()) &&
        bounds.x + bounds.width <= size.width &&
        bounds.y + bounds.height <= size.height
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
