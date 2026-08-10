/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

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
// Retention is an opt-in document-capture side channel with its own 64 MiB cumulative raw-pixel
// envelope, separate from the document renderer's resident-resource budget. Bound the vector
// transcript and registry independently so a script cannot replace byte pressure with command or
// object pressure. All counters are cumulative for one guard: replacing a readback or completing a
// Canvas does not refund its charge, so churn cannot evade the session limits.
pub const MAX_RETAINED_RASTER_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_RETAINED_COMMANDS: u64 = 262_144;
pub const MAX_RETAINED_OBJECTS: u64 = 65_536;

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
    RetentionBudget,
    Shadow,
    Transform,
    UnsupportedCommand,
}

/// Enables the in-process command snapshot used by Pliego document capture.
///
/// Normal Servo sessions pay only one relaxed atomic load per Canvas command. The retained state
/// is cleared when this guard is dropped, after the caller has converted the layout snapshot.
pub fn start_retaining_canvas_commands() -> CanvasRetentionGuard {
    start_retaining_canvas_commands_with_limits(
        MAX_RETAINED_COMMANDS,
        MAX_RETAINED_RASTER_BYTES,
        MAX_RETAINED_OBJECTS,
    )
}

/// Internal deterministic-test seam for document-session failure coverage.
///
/// Production callers must use [`start_retaining_canvas_commands`].
#[cfg(feature = "document-session-testing")]
#[doc(hidden)]
pub fn start_retaining_canvas_commands_for_testing(
    max_retained_commands: u64,
    max_retained_raster_bytes: u64,
    max_retained_objects: u64,
) -> CanvasRetentionGuard {
    start_retaining_canvas_commands_with_limits(
        max_retained_commands,
        max_retained_raster_bytes,
        max_retained_objects,
    )
}

fn start_retaining_canvas_commands_with_limits(
    max_retained_commands: u64,
    max_retained_raster_bytes: u64,
    max_retained_objects: u64,
) -> CanvasRetentionGuard {
    let mut registry = registry();
    registry.clear();
    registry.max_retained_commands = max_retained_commands;
    registry.max_retained_raster_bytes = max_retained_raster_bytes;
    registry.max_retained_objects = max_retained_objects;
    drop(registry);
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

pub fn snapshot_for_image_key(namespace: u32, key: u32) -> Option<Arc<RetainedCanvasSnapshot>> {
    if !ENABLED.load(Ordering::Acquire) {
        return None;
    }
    let registry = registry();
    let image_key = ImageKey(IdNamespace(namespace), key);
    if let Some(canvas_id) = registry.image_keys.get(&image_key) {
        return registry.canvases.get(canvas_id).map(Arc::clone);
    }
    registry.completed.get(&image_key).map(Arc::clone)
}

/// Reports whether the active capture guard exhausted any session-wide retention budget.
pub fn retention_budget_exceeded() -> bool {
    ENABLED.load(Ordering::Acquire) && registry().retention_budget_exceeded
}

struct Registry {
    canvases: HashMap<CanvasId, Arc<RetainedCanvasSnapshot>>,
    image_keys: HashMap<ImageKey, CanvasId>,
    completed: HashMap<ImageKey, Arc<RetainedCanvasSnapshot>>,
    retained_command_count: u64,
    retained_raster_bytes: u64,
    retained_object_count: u64,
    max_retained_commands: u64,
    max_retained_raster_bytes: u64,
    max_retained_objects: u64,
    retention_budget_exceeded: bool,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            canvases: HashMap::new(),
            image_keys: HashMap::new(),
            completed: HashMap::new(),
            retained_command_count: 0,
            retained_raster_bytes: 0,
            retained_object_count: 0,
            max_retained_commands: MAX_RETAINED_COMMANDS,
            max_retained_raster_bytes: MAX_RETAINED_RASTER_BYTES,
            max_retained_objects: MAX_RETAINED_OBJECTS,
            retention_budget_exceeded: false,
        }
    }
}

impl Registry {
    fn clear(&mut self) {
        self.canvases.clear();
        self.image_keys.clear();
        self.completed.clear();
        self.retained_command_count = 0;
        self.retained_raster_bytes = 0;
        self.retained_object_count = 0;
        self.max_retained_commands = MAX_RETAINED_COMMANDS;
        self.max_retained_raster_bytes = MAX_RETAINED_RASTER_BYTES;
        self.max_retained_objects = MAX_RETAINED_OBJECTS;
        self.retention_budget_exceeded = false;
    }

    fn mark_retention_budget_exceeded(&mut self, canvas_id: Option<CanvasId>) {
        self.retention_budget_exceeded = true;
        let Some(canvas_id) = canvas_id else {
            return;
        };
        if let Some(canvas) = self.canvases.get_mut(&canvas_id) {
            Arc::make_mut(canvas)
                .unsupported
                .get_or_insert(RetainedCanvasUnsupported {
                    command: "retention",
                    reason: RetainedCanvasUnsupportedReason::RetentionBudget,
                });
        }
    }

    fn reserve_object(&mut self, canvas_id: Option<CanvasId>) -> bool {
        if self.retention_budget_exceeded {
            self.mark_retention_budget_exceeded(canvas_id);
            return false;
        }
        let Some(next_object_count) = self.retained_object_count.checked_add(1) else {
            self.mark_retention_budget_exceeded(canvas_id);
            return false;
        };
        if next_object_count > self.max_retained_objects {
            self.mark_retention_budget_exceeded(canvas_id);
            return false;
        }
        self.retained_object_count = next_object_count;
        true
    }

    fn reserve_command(
        &mut self,
        canvas_id: CanvasId,
        raster_bytes: u64,
        can_replace_unsupported: bool,
    ) -> bool {
        if !self.canvases.contains_key(&canvas_id) {
            return false;
        }
        if self.retention_budget_exceeded {
            self.mark_retention_budget_exceeded(Some(canvas_id));
            return false;
        }
        if self.canvases[&canvas_id]
            .unsupported
            .as_ref()
            .is_some_and(|unsupported| {
                !can_replace_unsupported ||
                    unsupported.reason == RetainedCanvasUnsupportedReason::RetentionBudget
            })
        {
            return false;
        }
        let next_command_count = self.retained_command_count.checked_add(1);
        let next_raster_bytes = self.retained_raster_bytes.checked_add(raster_bytes);
        let within_budget = next_command_count
            .is_some_and(|count| count <= self.max_retained_commands) &&
            next_raster_bytes.is_some_and(|bytes| bytes <= self.max_retained_raster_bytes);
        if !within_budget {
            self.mark_retention_budget_exceeded(Some(canvas_id));
            return false;
        }
        self.retained_command_count = next_command_count.unwrap();
        self.retained_raster_bytes = next_raster_bytes.unwrap();
        true
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
    let mut registry = registry();
    if !registry.canvases.contains_key(&canvas_id) && !registry.reserve_object(None) {
        return;
    }
    registry.canvases.insert(
        canvas_id,
        Arc::new(RetainedCanvasSnapshot {
            width,
            height,
            commands: Vec::new(),
            unsupported: None,
        }),
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
    let canvas = Arc::make_mut(canvas);
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
        let is_new = !registry.image_keys.contains_key(&image_key) &&
            !registry.completed.contains_key(&image_key);
        if is_new && !registry.reserve_object(Some(canvas_id)) {
            return;
        }
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
        registry.completed.insert(image_key, Arc::clone(&snapshot));
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
    if composition.alpha != 1.0 ||
        composition.composition_operation !=
            CompositionOrBlending::Composition(CompositionStyle::SourceOver)
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
    let Some(canvas) = registry.canvases.get(&canvas_id) else {
        return;
    };
    let canvas_rect = Rect::from_size(Size2D::new(canvas.width as f32, canvas.height as f32));
    let Some(rect) = rect
        .intersection(&canvas_rect)
        .filter(|rect| !rect.is_empty())
    else {
        return;
    };
    if !registry.reserve_command(canvas_id, 0, false) {
        return;
    }
    let canvas = Arc::make_mut(registry.canvases.get_mut(&canvas_id).unwrap());
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
    if u64::from(size.width) > MAX_RASTER_DIMENSION ||
        u64::from(size.height) > MAX_RASTER_DIMENSION ||
        pixels > MAX_RASTER_PIXELS ||
        bytes > MAX_RASTER_BYTES
    {
        mark_unsupported(
            canvas_id,
            "get_image_data",
            RetainedCanvasUnsupportedReason::RasterLimit,
        );
        return;
    }
    let origin = bounds.map_or_else(Default::default, |bounds| bounds.origin);
    {
        let mut registry = registry();
        let can_replace_unsupported = registry.canvases.get(&canvas_id).is_some_and(|canvas| {
            origin.x == 0 &&
                origin.y == 0 &&
                size.width == canvas.width &&
                size.height == canvas.height
        });
        if !registry.reserve_command(canvas_id, bytes, can_replace_unsupported) {
            return;
        }
    }
    let mut snapshot = snapshot.clone();
    snapshot.transform(
        SnapshotAlphaMode::Transparent {
            premultiplied: true,
        },
        SnapshotPixelFormat::RGBA,
    );
    let mut registry = registry();
    let Some(canvas) = registry.canvases.get_mut(&canvas_id) else {
        return;
    };
    let canvas = Arc::make_mut(canvas);
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
        let canvas = Arc::make_mut(canvas);
        canvas
            .unsupported
            .get_or_insert(RetainedCanvasUnsupported { command, reason });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use euclid::default::{Point2D, Rect, Size2D, Transform2D};
    use pixels::{Snapshot, SnapshotAlphaMode, SnapshotPixelFormat};
    use servo_canvas_traits::canvas::{
        CanvasId, CompositionOptions, FillOrStrokeStyle, ShadowOptions,
    };
    use style::color::AbsoluteColor;
    use webrender_api::{IdNamespace, ImageKey};

    use super::{
        Registry, RetainedCanvasCommand, RetainedCanvasSnapshot, RetainedCanvasUnsupportedReason,
        associate_image_key, finish_canvas, mark_unsupported, register_canvas, registry,
        retain_fill_rect, retain_pixel_readback, retention_budget_exceeded, snapshot_for_image_key,
        start_retaining_canvas_commands, start_retaining_canvas_commands_with_limits,
    };

    static RETENTION_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn retains_only_while_the_capture_guard_is_live() {
        let _serial = RETENTION_TEST_LOCK.lock().unwrap();
        let key = ImageKey(IdNamespace(7), 11);
        let second_key = ImageKey(IdNamespace(7), 13);
        {
            let _guard = start_retaining_canvas_commands();
            register_canvas(CanvasId(3), Size2D::new(20, 10));
            associate_image_key(CanvasId(3), key);
            associate_image_key(CanvasId(3), second_key);
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
            let first = snapshot_for_image_key(7, 11).unwrap();
            let second = snapshot_for_image_key(7, 13).unwrap();
            assert!(std::sync::Arc::ptr_eq(&first, &second));
            assert_eq!(registry().retained_object_count, 3);
        }
        assert!(snapshot_for_image_key(7, 11).is_none());
    }

    #[test]
    fn aggregate_budget_fails_before_crossing_across_canvases_and_readbacks() {
        let _serial = RETENTION_TEST_LOCK.lock().unwrap();
        let mut registry = Registry {
            max_retained_commands: 3,
            max_retained_raster_bytes: 8,
            ..Registry::default()
        };
        for canvas_id in [CanvasId(20), CanvasId(21)] {
            registry.canvases.insert(
                canvas_id,
                std::sync::Arc::new(RetainedCanvasSnapshot {
                    width: 2,
                    height: 2,
                    commands: Vec::new(),
                    unsupported: None,
                }),
            );
        }

        assert!(registry.reserve_command(CanvasId(20), 4, false));
        assert!(registry.reserve_command(CanvasId(20), 4, false));
        assert!(!registry.reserve_command(CanvasId(21), 4, false));
        assert_eq!(registry.retained_command_count, 2);
        assert_eq!(registry.retained_raster_bytes, 8);
        assert_eq!(
            registry.canvases[&CanvasId(21)]
                .unsupported
                .as_ref()
                .map(|unsupported| unsupported.reason),
            Some(RetainedCanvasUnsupportedReason::RetentionBudget)
        );
        assert!(!registry.reserve_command(CanvasId(20), 0, false));
        assert_eq!(registry.retained_command_count, 2);

        let mut command_registry = Registry {
            max_retained_commands: 2,
            ..Registry::default()
        };
        command_registry.canvases.insert(
            CanvasId(22),
            std::sync::Arc::new(RetainedCanvasSnapshot {
                width: 2,
                height: 2,
                commands: Vec::new(),
                unsupported: None,
            }),
        );
        assert!(command_registry.reserve_command(CanvasId(22), 0, false));
        assert!(command_registry.reserve_command(CanvasId(22), 0, false));
        assert!(!command_registry.reserve_command(CanvasId(22), 0, false));
        assert_eq!(command_registry.retained_command_count, 2);
        assert_eq!(command_registry.retained_raster_bytes, 0);
    }

    #[test]
    fn object_budget_rejects_canvas_and_image_key_insertions_before_crossing() {
        let _serial = RETENTION_TEST_LOCK.lock().unwrap();
        let _guard = start_retaining_canvas_commands_with_limits(64, 64, 2);
        let first_key = ImageKey(IdNamespace(8), 1);
        let rejected_key = ImageKey(IdNamespace(8), 2);

        register_canvas(CanvasId(30), Size2D::new(2, 2));
        associate_image_key(CanvasId(30), first_key);
        associate_image_key(CanvasId(30), rejected_key);
        register_canvas(CanvasId(31), Size2D::new(2, 2));

        let registry = registry();
        assert_eq!(registry.retained_object_count, 2);
        assert_eq!(registry.canvases.len(), 1);
        assert_eq!(registry.image_keys.len(), 1);
        assert!(registry.image_keys.contains_key(&first_key));
        assert!(!registry.image_keys.contains_key(&rejected_key));
        drop(registry);
        assert!(retention_budget_exceeded());
    }

    #[test]
    fn full_readback_replaces_unsupported_commands_with_authoritative_pixels() {
        let _serial = RETENTION_TEST_LOCK.lock().unwrap();
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
