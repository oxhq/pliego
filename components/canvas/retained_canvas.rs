/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use euclid::default::{Rect, Size2D, Transform2D};
use pixels::{Snapshot, SnapshotAlphaMode, SnapshotPixelFormat};
use servo_canvas_traits::canvas::{
    CanvasId, CompositionOptions, CompositionOrBlending, CompositionStyle, FillOrStrokeStyle,
    ShadowOptions,
};
use webrender_api::{IdNamespace, ImageKey};

const MAX_RASTER_DIMENSION: u64 = 16_384;
const MAX_RASTER_PIXELS: u64 = 100_000_000;
const MAX_RASTER_BYTES: u64 = 256 * 1024 * 1024;

static ENABLED: AtomicBool = AtomicBool::new(false);
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq)]
pub struct RetainedCanvasSnapshot {
    pub width: u32,
    pub height: u32,
    pub commands: Vec<RetainedCanvasCommand>,
    pub unsupported: Option<RetainedCanvasUnsupported>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RetainedCanvasCommand {
    FillRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        red: f64,
        green: f64,
        blue: f64,
        alpha: f64,
    },
    RasterPatch {
        x: f64,
        y: f64,
        width: u32,
        height: u32,
        premultiplied_rgba: Vec<u8>,
        reason: RetainedCanvasFallbackReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedCanvasFallbackReason {
    PixelReadback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedCanvasUnsupported {
    pub command: &'static str,
    pub reason: RetainedCanvasUnsupportedReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedCanvasUnsupportedReason {
    Clip,
    Compositing,
    Paint,
    RasterLimit,
    Shadow,
    Transform,
    UnsupportedCommand,
}

/// Enables the in-process command snapshot used by Pliego document capture.
///
/// Normal Servo sessions pay only one relaxed atomic load per Canvas command. The retained state
/// is cleared when this guard is dropped, after the caller has converted the layout snapshot.
pub fn start_retaining_canvas_commands() -> CanvasRetentionGuard {
    registry().clear();
    ENABLED.store(true, Ordering::Release);
    CanvasRetentionGuard { active: true }
}

pub struct CanvasRetentionGuard {
    active: bool,
}

impl Drop for CanvasRetentionGuard {
    fn drop(&mut self) {
        if self.active {
            ENABLED.store(false, Ordering::Release);
            registry().clear();
        }
    }
}

pub fn snapshot_for_image_key(namespace: u32, key: u32) -> Option<RetainedCanvasSnapshot> {
    if !ENABLED.load(Ordering::Acquire) {
        return None;
    }
    let registry = registry();
    let image_key = ImageKey(IdNamespace(namespace), key);
    if let Some(canvas_id) = registry.image_keys.get(&image_key) {
        return registry.canvases.get(canvas_id).cloned();
    }
    registry.completed.get(&image_key).cloned()
}

#[derive(Default)]
struct Registry {
    canvases: HashMap<CanvasId, RetainedCanvasSnapshot>,
    image_keys: HashMap<ImageKey, CanvasId>,
    completed: HashMap<ImageKey, RetainedCanvasSnapshot>,
}

impl Registry {
    fn clear(&mut self) {
        self.canvases.clear();
        self.image_keys.clear();
        self.completed.clear();
    }
}

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn register_canvas(canvas_id: CanvasId, size: Size2D<u64>) {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    let Ok(width) = u32::try_from(size.width) else {
        return;
    };
    let Ok(height) = u32::try_from(size.height) else {
        return;
    };
    registry().canvases.insert(
        canvas_id,
        RetainedCanvasSnapshot {
            width,
            height,
            commands: Vec::new(),
            unsupported: None,
        },
    );
}

pub(crate) fn recreate_canvas(canvas_id: CanvasId, size: Option<Size2D<u64>>) {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    let mut registry = registry();
    let Some(canvas) = registry.canvases.get_mut(&canvas_id) else {
        return;
    };
    if let Some(size) = size {
        let (Ok(width), Ok(height)) = (u32::try_from(size.width), u32::try_from(size.height))
        else {
            canvas.unsupported = Some(RetainedCanvasUnsupported {
                command: "recreate",
                reason: RetainedCanvasUnsupportedReason::RasterLimit,
            });
            return;
        };
        canvas.width = width;
        canvas.height = height;
    }
    canvas.commands.clear();
    canvas.unsupported = None;
}

pub(crate) fn associate_image_key(canvas_id: CanvasId, image_key: ImageKey) {
    if ENABLED.load(Ordering::Acquire) {
        let mut registry = registry();
        registry.completed.remove(&image_key);
        registry.image_keys.insert(image_key, canvas_id);
    }
}

pub(crate) fn finish_canvas(canvas_id: CanvasId) {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    let mut registry = registry();
    let Some(snapshot) = registry.canvases.remove(&canvas_id) else {
        return;
    };
    let image_keys = registry
        .image_keys
        .iter()
        .filter_map(|(key, owner)| (*owner == canvas_id).then_some(*key))
        .collect::<Vec<_>>();
    for image_key in image_keys {
        registry.image_keys.remove(&image_key);
        registry.completed.insert(image_key, snapshot.clone());
    }
}

pub(crate) fn retain_fill_rect(
    canvas_id: CanvasId,
    rect: &Rect<f32>,
    style: &FillOrStrokeStyle,
    shadow: &ShadowOptions,
    composition: CompositionOptions,
    transform: Transform2D<f64>,
) {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    if shadow.need_to_draw_shadow() {
        mark_unsupported(
            canvas_id,
            "fill_rect",
            RetainedCanvasUnsupportedReason::Shadow,
        );
        return;
    }
    if transform != Transform2D::identity() {
        mark_unsupported(
            canvas_id,
            "fill_rect",
            RetainedCanvasUnsupportedReason::Transform,
        );
        return;
    }
    if composition.alpha != 1.0
        || composition.composition_operation
            != CompositionOrBlending::Composition(CompositionStyle::SourceOver)
    {
        mark_unsupported(
            canvas_id,
            "fill_rect",
            RetainedCanvasUnsupportedReason::Compositing,
        );
        return;
    }
    let FillOrStrokeStyle::Color(color) = style else {
        mark_unsupported(
            canvas_id,
            "fill_rect",
            RetainedCanvasUnsupportedReason::Paint,
        );
        return;
    };
    if rect.size.width < 0.0 || rect.size.height < 0.0 {
        mark_unsupported(
            canvas_id,
            "fill_rect",
            RetainedCanvasUnsupportedReason::Paint,
        );
        return;
    }

    let mut registry = registry();
    let Some(canvas) = registry.canvases.get_mut(&canvas_id) else {
        return;
    };
    let canvas_rect = Rect::from_size(Size2D::new(canvas.width as f32, canvas.height as f32));
    let Some(rect) = rect
        .intersection(&canvas_rect)
        .filter(|rect| !rect.is_empty())
    else {
        return;
    };
    let srgb = (*color).into_srgb_legacy();
    canvas.commands.push(RetainedCanvasCommand::FillRect {
        x: f64::from(rect.origin.x),
        y: f64::from(rect.origin.y),
        width: f64::from(rect.size.width),
        height: f64::from(rect.size.height),
        red: f64::from(srgb.components.0),
        green: f64::from(srgb.components.1),
        blue: f64::from(srgb.components.2),
        alpha: f64::from(srgb.alpha),
    });
}

pub(crate) fn retain_pixel_readback(
    canvas_id: CanvasId,
    bounds: Option<Rect<u32>>,
    snapshot: &Snapshot,
) {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    let size = snapshot.size();
    let pixels = u64::from(size.width).saturating_mul(u64::from(size.height));
    let bytes = pixels.saturating_mul(4);
    if u64::from(size.width) > MAX_RASTER_DIMENSION
        || u64::from(size.height) > MAX_RASTER_DIMENSION
        || pixels > MAX_RASTER_PIXELS
        || bytes > MAX_RASTER_BYTES
    {
        mark_unsupported(
            canvas_id,
            "get_image_data",
            RetainedCanvasUnsupportedReason::RasterLimit,
        );
        return;
    }
    let mut snapshot = snapshot.clone();
    snapshot.transform(
        SnapshotAlphaMode::Transparent {
            premultiplied: true,
        },
        SnapshotPixelFormat::RGBA,
    );
    let origin = bounds.map_or_else(Default::default, |bounds| bounds.origin);
    let mut registry = registry();
    let Some(canvas) = registry.canvases.get_mut(&canvas_id) else {
        return;
    };
    if origin.x == 0 && origin.y == 0 && size.width == canvas.width && size.height == canvas.height
    {
        // A synchronous full-canvas readback is an authoritative fallback for every command
        // that produced those pixels. Later unsupported commands still invalidate it normally.
        canvas.commands.clear();
        canvas.unsupported = None;
    }
    canvas.commands.push(RetainedCanvasCommand::RasterPatch {
        x: f64::from(origin.x),
        y: f64::from(origin.y),
        width: size.width,
        height: size.height,
        premultiplied_rgba: snapshot.as_raw_bytes().to_vec(),
        reason: RetainedCanvasFallbackReason::PixelReadback,
    });
}

pub(crate) fn mark_unsupported(
    canvas_id: CanvasId,
    command: &'static str,
    reason: RetainedCanvasUnsupportedReason,
) {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    let mut registry = registry();
    if let Some(canvas) = registry.canvases.get_mut(&canvas_id) {
        canvas
            .unsupported
            .get_or_insert(RetainedCanvasUnsupported { command, reason });
    }
}

#[cfg(test)]
mod tests {
    use euclid::default::{Point2D, Rect, Size2D, Transform2D};
    use pixels::{Snapshot, SnapshotAlphaMode, SnapshotPixelFormat};
    use servo_canvas_traits::canvas::{
        CanvasId, CompositionOptions, FillOrStrokeStyle, ShadowOptions,
    };
    use style::color::AbsoluteColor;
    use webrender_api::{IdNamespace, ImageKey};

    use super::{
        RetainedCanvasCommand, RetainedCanvasUnsupportedReason, associate_image_key, finish_canvas,
        mark_unsupported, register_canvas, registry, retain_fill_rect, retain_pixel_readback,
        snapshot_for_image_key, start_retaining_canvas_commands,
    };

    #[test]
    fn retains_only_while_the_capture_guard_is_live() {
        let key = ImageKey(IdNamespace(7), 11);
        {
            let _guard = start_retaining_canvas_commands();
            register_canvas(CanvasId(3), Size2D::new(20, 10));
            associate_image_key(CanvasId(3), key);
            retain_fill_rect(
                CanvasId(3),
                &Rect::new(Point2D::new(2.0, 3.0), Size2D::new(4.0, 5.0)),
                &FillOrStrokeStyle::Color(AbsoluteColor::BLACK),
                &ShadowOptions {
                    offset_x: 0.0,
                    offset_y: 0.0,
                    blur: 0.0,
                    color: AbsoluteColor::TRANSPARENT_BLACK,
                },
                CompositionOptions {
                    alpha: 1.0,
                    composition_operation: Default::default(),
                },
                Transform2D::identity(),
            );
            let snapshot = snapshot_for_image_key(7, 11).unwrap();
            assert_eq!((snapshot.width, snapshot.height), (20, 10));
            assert!(matches!(
                snapshot.commands.as_slice(),
                [RetainedCanvasCommand::FillRect {
                    x: 2.0,
                    y: 3.0,
                    width: 4.0,
                    height: 5.0,
                    ..
                }]
            ));
            finish_canvas(CanvasId(3));
            assert!(registry().canvases.is_empty());
            assert!(registry().image_keys.is_empty());
            assert!(snapshot_for_image_key(7, 11).is_some());
        }
        assert!(snapshot_for_image_key(7, 11).is_none());
    }

    #[test]
    fn full_readback_replaces_unsupported_commands_with_authoritative_pixels() {
        let key = ImageKey(IdNamespace(7), 12);
        let _guard = start_retaining_canvas_commands();
        register_canvas(CanvasId(4), Size2D::new(2, 2));
        associate_image_key(CanvasId(4), key);
        mark_unsupported(
            CanvasId(4),
            "fill_path",
            RetainedCanvasUnsupportedReason::UnsupportedCommand,
        );

        let pixels = Snapshot::from_vec(
            Size2D::new(2, 2),
            SnapshotPixelFormat::RGBA,
            SnapshotAlphaMode::Transparent {
                premultiplied: true,
            },
            vec![255; 16],
        );
        retain_pixel_readback(
            CanvasId(4),
            Some(Rect::new(Point2D::zero(), Size2D::new(2, 2))),
            &pixels,
        );

        let snapshot = snapshot_for_image_key(7, 12).unwrap();
        assert!(snapshot.unsupported.is_none());
        assert!(matches!(
            snapshot.commands.as_slice(),
            [RetainedCanvasCommand::RasterPatch {
                x: 0.0,
                y: 0.0,
                width: 2,
                height: 2,
                ..
            }]
        ));
    }
}
