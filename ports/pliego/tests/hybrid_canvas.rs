/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::io::Cursor;

use pliego::Operation;
use pliego::hybrid_canvas::{CanvasFallbackReason, CanvasTranscript, adapt_canvas};
use pliego::raster::render_first_page_png_with_images;
use sha2::{Digest, Sha256};
use vello_cpu::Pixmap;

const FIXTURE: &[u8] = include_bytes!("fixtures/hybrid-canvas/transcript.json");

#[test]
fn preserves_chart_vectors_and_bounds_one_pixel_fallback_repeatably() {
    let adapt =
        || adapt_canvas(serde_json::from_slice::<CanvasTranscript>(FIXTURE).unwrap()).unwrap();
    let first = adapt();
    let second = adapt();

    assert_eq!(
        first.scene.normalized_json().unwrap(),
        second.scene.normalized_json().unwrap()
    );
    assert_eq!(
        first.scene.sha256().unwrap(),
        second.scene.sha256().unwrap()
    );
    assert_eq!(first.resources, second.resources);
    assert_eq!(
        first.diagnostics_json().unwrap(),
        second.diagnostics_json().unwrap()
    );

    let operations = &first.scene.pages[0].operations;
    assert_eq!(operations.len(), 4);
    assert!(
        operations[..3]
            .iter()
            .all(|operation| matches!(operation, Operation::Path { .. }))
    );
    let Operation::Image {
        bounds, resource, ..
    } = &operations[3]
    else {
        panic!("pixel-dependent command must become one image patch")
    };
    assert_eq!(
        (bounds.x, bounds.y, bounds.width, bounds.height),
        (6.0, 4.0, 2.0, 2.0)
    );
    assert_eq!(first.resources.len(), 1);
    assert_eq!(resource, &first.resources[0].resource);
    let digest = Sha256::digest(&first.resources[0].png)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(resource, &format!("sha256:{digest}"));

    let fallback = &first.diagnostics.fallbacks[0];
    assert_eq!(first.diagnostics.vector_operation_count, 3);
    assert_eq!(first.diagnostics.rasterized_area_px, 4);
    assert_eq!(fallback.command_index, 3);
    assert_eq!(fallback.reason, CanvasFallbackReason::PixelReadback);
    assert_eq!(fallback.bounds, *bounds);
    assert_eq!(fallback.area_px, 4);
    assert_eq!(fallback.resource, *resource);
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&first.diagnostics_json().unwrap()).unwrap();
    assert_eq!(diagnostics["fallbacks"][0]["reason"], "pixel_readback");
    assert_eq!(diagnostics["fallbacks"][0]["bounds"]["x"], 6.0);
    assert_eq!(diagnostics["fallbacks"][0]["area_px"], 4);

    let render = |capture: &pliego::hybrid_canvas::HybridCanvasCapture| {
        render_first_page_png_with_images(
            &capture.scene,
            |_| None,
            |wanted| {
                capture
                    .resources
                    .iter()
                    .find(|resource| resource.resource == wanted)
                    .map(|resource| resource.png.as_slice())
            },
        )
        .unwrap()
    };
    let first_preview = render(&first);
    let second_preview = render(&second);
    assert_eq!(
        Sha256::digest(&first_preview),
        Sha256::digest(&second_preview)
    );

    let patch = Pixmap::from_png(Cursor::new(&first.resources[0].png)).unwrap();
    assert_eq!((patch.width(), patch.height()), (2, 2));
    let preview = Pixmap::from_png(Cursor::new(first_preview)).unwrap();
    assert_eq!((preview.width(), preview.height()), (16, 12));
    let rgba = |x: usize, y: usize| {
        let start = (y * usize::from(preview.width()) + x) * 4;
        <[u8; 4]>::try_from(&preview.data_as_u8_slice()[start..start + 4]).unwrap()
    };
    assert_eq!(rgba(6, 4), [255, 0, 0, 255]);
    assert_eq!(rgba(7, 4), [0, 255, 0, 255]);
    assert_eq!(rgba(5, 4), [0, 0, 0, 0]);
}

#[test]
fn rejects_a_patch_that_cannot_be_its_declared_region() {
    let mut transcript: CanvasTranscript = serde_json::from_slice(FIXTURE).unwrap();
    let pliego::hybrid_canvas::CanvasCommand::RasterPatch {
        premultiplied_rgba, ..
    } = transcript.commands.last_mut().unwrap()
    else {
        unreachable!()
    };
    premultiplied_rgba.pop();
    assert!(matches!(
        adapt_canvas(transcript),
        Err(pliego::hybrid_canvas::CanvasError::RasterLength {
            command_index: 3,
            expected: 16,
            actual: 15,
        })
    ));
}
