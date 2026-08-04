/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod capture;
pub mod hybrid_canvas;
mod image_limits;
pub mod pdf;
pub mod raster;

pub use image_limits::{IMAGE_LIMITS, ImageLimit, ImageLimits};

pub const SCHEMA: &str = "pliego.document-scene";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct DocumentScene {
    pub schema: String,
    pub version: u32,
    pub pages: Vec<Page>,
}

impl DocumentScene {
    pub fn new(page: Page) -> Self {
        Self {
            schema: SCHEMA.into(),
            version: SCHEMA_VERSION,
            pages: vec![page],
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema != SCHEMA || self.version != SCHEMA_VERSION {
            return Err("unknown DocumentScene schema");
        }
        if self.pages.is_empty() {
            return Err("DocumentScene must contain at least one page");
        }

        for page in &self.pages {
            page.size.validate()?;
            for operation in &page.operations {
                operation.validate()?;
            }
        }
        Ok(())
    }

    pub fn normalized_json(&self) -> Result<Vec<u8>, String> {
        self.validate().map_err(str::to_owned)?;
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }

    pub fn sha256(&self) -> Result<[u8; 32], String> {
        Ok(Sha256::digest(self.normalized_json()?).into())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Page {
    pub size: Size,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    fn validate(&self) -> Result<(), &'static str> {
        finite_nonnegative(self.width)?;
        finite_nonnegative(self.height)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn validate(&self) -> Result<(), &'static str> {
        finite_nonnegative(self.x)?;
        finite_nonnegative(self.y)?;
        finite_nonnegative(self.width)?;
        finite_nonnegative(self.height)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Glyph {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub advance: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_range: Option<Utf8Range>,
}

impl Glyph {
    fn validate(&self, text: &str) -> Result<(), &'static str> {
        finite_nonnegative(self.x)?;
        finite_nonnegative(self.y)?;
        finite_nonnegative(self.advance)?;
        if let Some(range) = self.text_range {
            range.validate(text)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Utf8Range {
    pub start: u32,
    pub end: u32,
}

impl Utf8Range {
    fn validate(&self, text: &str) -> Result<(), &'static str> {
        let start = usize::try_from(self.start).map_err(|_| "glyph text range is out of bounds")?;
        let end = usize::try_from(self.end).map_err(|_| "glyph text range is out of bounds")?;
        if start >= end || end > text.len() {
            return Err("glyph text range is out of bounds");
        }
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return Err("glyph text range is not on UTF-8 boundaries");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    }
}

impl Color {
    fn validate(&self) -> Result<(), &'static str> {
        unit_interval(self.r)?;
        unit_interval(self.g)?;
        unit_interval(self.b)?;
        unit_interval(self.a)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
}

impl Stroke {
    fn validate(&self) -> Result<(), &'static str> {
        self.color.validate()?;
        finite_nonnegative(self.width)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct OperationMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantics: Option<Semantics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRef>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Semantics {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SourceRef {
    pub uri: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
    Text {
        text: String,
        font: String,
        font_size: f64,
        #[serde(default)]
        color: Color,
        glyphs: Vec<Glyph>,
        #[serde(default)]
        meta: OperationMeta,
    },
    Path {
        bounds: Rect,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default)]
        fill_rule: FillRule,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Stroke>,
        #[serde(default)]
        meta: OperationMeta,
    },
    Image {
        bounds: Rect,
        resource: String,
        #[serde(default)]
        meta: OperationMeta,
    },
    Link {
        bounds: Rect,
        target: String,
        #[serde(default)]
        meta: OperationMeta,
    },
}

impl Operation {
    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Text {
                text,
                font,
                font_size,
                color,
                glyphs,
                meta,
            } => {
                nonempty(text)?;
                nonempty(font)?;
                finite_nonnegative(*font_size)?;
                color.validate()?;
                for glyph in glyphs {
                    glyph.validate(text)?;
                }
                meta.validate()
            },
            Self::Path {
                bounds,
                data,
                fill,
                stroke,
                meta,
                ..
            } => {
                bounds.validate()?;
                nonempty(data)?;
                if fill.is_none() && stroke.is_none() {
                    return Err("scene path must have a fill or stroke");
                }
                if let Some(fill) = fill {
                    fill.validate()?;
                }
                if let Some(stroke) = stroke {
                    stroke.validate()?;
                }
                meta.validate()
            },
            Self::Image {
                bounds,
                resource,
                meta,
            } => {
                bounds.validate()?;
                nonempty(resource)?;
                meta.validate()
            },
            Self::Link {
                bounds,
                target,
                meta,
            } => {
                bounds.validate()?;
                nonempty(target)?;
                meta.validate()
            },
        }
    }
}

impl OperationMeta {
    fn validate(&self) -> Result<(), &'static str> {
        if let Some(semantics) = &self.semantics {
            nonempty(&semantics.role)?;
        }
        if let Some(source) = &self.source {
            nonempty(&source.uri)?;
        }
        Ok(())
    }
}

fn finite_nonnegative(value: f64) -> Result<(), &'static str> {
    if value.is_finite() && value >= 0.0 && !value.is_sign_negative() {
        Ok(())
    } else {
        Err("geometry must be finite and nonnegative")
    }
}

fn unit_interval(value: f64) -> Result<(), &'static str> {
    if value.is_finite() && (0.0..=1.0).contains(&value) && !value.is_sign_negative() {
        Ok(())
    } else {
        Err("scene color components must be finite and between zero and one")
    }
}

fn nonempty(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        Err("scene strings must not be empty")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> DocumentScene {
        DocumentScene::new(Page {
            size: Size {
                width: 612.0,
                height: 792.0,
            },
            operations: vec![
                Operation::Text {
                    text: "Hi".into(),
                    font: "font/inter".into(),
                    font_size: 12.0,
                    color: Color::default(),
                    glyphs: vec![Glyph {
                        id: 42,
                        x: 24.0,
                        y: 36.0,
                        advance: 7.0,
                        text_range: Some(Utf8Range { start: 0, end: 2 }),
                    }],
                    meta: OperationMeta {
                        semantics: Some(Semantics {
                            role: "heading".into(),
                            label: None,
                        }),
                        source: Some(SourceRef {
                            uri: "invoice.html".into(),
                            line: 1,
                            column: 1,
                        }),
                    },
                },
                Operation::Path {
                    bounds: Rect {
                        x: 24.0,
                        y: 48.0,
                        width: 100.0,
                        height: 1.0,
                    },
                    data: "M24 48h100".into(),
                    fill: None,
                    fill_rule: FillRule::NonZero,
                    stroke: Some(Stroke {
                        color: Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        },
                        width: 1.0,
                    }),
                    meta: OperationMeta::default(),
                },
                Operation::Image {
                    bounds: Rect {
                        x: 24.0,
                        y: 60.0,
                        width: 32.0,
                        height: 32.0,
                    },
                    resource: "sha256:logo".into(),
                    meta: OperationMeta::default(),
                },
                Operation::Link {
                    bounds: Rect {
                        x: 24.0,
                        y: 100.0,
                        width: 80.0,
                        height: 12.0,
                    },
                    target: "https://example.com".into(),
                    meta: OperationMeta::default(),
                },
            ],
        })
    }

    #[test]
    fn validates_and_serializes_deterministically() {
        let first = scene();
        let second = scene();
        assert!(first.validate().is_ok());
        assert_eq!(
            first.normalized_json().unwrap(),
            second.normalized_json().unwrap()
        );
        assert_eq!(first.sha256().unwrap(), second.sha256().unwrap());

        let mut invalid = scene();
        invalid.pages[0].size.width = f64::NAN;
        assert!(invalid.validate().is_err());

        let mut negative_zero = scene();
        negative_zero.pages[0].size.width = -0.0;
        assert!(negative_zero.validate().is_err());

        let mut unpainted_path = scene();
        let Operation::Path { fill, stroke, .. } = &mut unpainted_path.pages[0].operations[1]
        else {
            unreachable!()
        };
        *fill = None;
        *stroke = None;
        assert!(unpainted_path.validate().is_err());

        let mut invalid_text_range = scene();
        let Operation::Text { glyphs, .. } = &mut invalid_text_range.pages[0].operations[0] else {
            unreachable!()
        };
        glyphs[0].text_range = Some(Utf8Range { start: 1, end: 3 });
        assert!(invalid_text_range.validate().is_err());

        let unknown = br#"{"schema":"pliego.document-scene","version":1,"pages":[{"size":{"width":1,"height":1},"operations":[{"type":"video"}]}]}"#;
        assert!(serde_json::from_slice::<DocumentScene>(unknown).is_err());
    }
}
