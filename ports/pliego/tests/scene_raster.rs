/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::io::Cursor;

use pliego::raster::{RasterError, RasterFontResource, RasterFontVariation, render_first_page_png};
use pliego::{DocumentScene, Glyph, Operation, OperationMeta, Page, Rect, Size};
use vello_cpu::Pixmap;

const DEJAVU_SANS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
));
const FONT_ID: &str = "sha256:dejavu-sans-fixture";

fn text_scene() -> DocumentScene {
    DocumentScene::new(Page {
        size: Size {
            width: 64.0,
            height: 64.0,
        },
        operations: vec![
            Operation::Text {
                text: "notdef".into(),
                font: FONT_ID.into(),
                font_size: 32.0,
                glyphs: vec![Glyph {
                    id: 0,
                    x: 8.0,
                    y: 40.0,
                    advance: 20.0,
                }],
                meta: OperationMeta::default(),
            },
            Operation::Link {
                bounds: Rect {
                    x: 8.0,
                    y: 8.0,
                    width: 32.0,
                    height: 32.0,
                },
                target: "https://example.com".into(),
                meta: OperationMeta::default(),
            },
        ],
    })
}

fn fixture_font(font: &str) -> Option<RasterFontResource<'static>> {
    (font == FONT_ID).then_some(RasterFontResource {
        bytes: DEJAVU_SANS,
        face_index: 0,
        variations: &[],
    })
}

#[test]
fn rasterizes_positioned_glyphs_without_dom_layout_or_shaping() {
    let scene = text_scene();
    let first = render_first_page_png(&scene, fixture_font).unwrap();
    let second = render_first_page_png(&scene, fixture_font).unwrap();

    assert_eq!(first, second);
    let pixmap = Pixmap::from_png(Cursor::new(first)).unwrap();
    assert_eq!((pixmap.width(), pixmap.height()), (64, 64));
    assert!(
        pixmap
            .data_as_u8_slice()
            .chunks_exact(4)
            .any(|pixel| pixel[3] != 0),
        "the positioned glyph should paint at least one pixel"
    );
}

#[test]
fn rejects_operations_and_font_instances_without_a_truthful_mapping() {
    let mut path_scene = text_scene();
    path_scene.pages[0].operations.insert(
        0,
        Operation::Path {
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            data: "M0 0h1v1z".into(),
            meta: OperationMeta::default(),
        },
    );
    assert_eq!(
        render_first_page_png(&path_scene, fixture_font),
        Err(RasterError::UnsupportedOperation {
            index: 0,
            kind: "path",
        })
    );

    let mut image_scene = text_scene();
    image_scene.pages[0].operations.insert(
        0,
        Operation::Image {
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            resource: "sha256:image-fixture".into(),
            meta: OperationMeta::default(),
        },
    );
    assert_eq!(
        render_first_page_png(&image_scene, fixture_font),
        Err(RasterError::UnsupportedOperation {
            index: 0,
            kind: "image",
        })
    );

    let variations = [RasterFontVariation {
        tag: u32::from_be_bytes(*b"wght"),
        value: 700.0,
    }];
    assert_eq!(
        render_first_page_png(&text_scene(), |font| {
            (font == FONT_ID).then_some(RasterFontResource {
                bytes: DEJAVU_SANS,
                face_index: 0,
                variations: &variations,
            })
        }),
        Err(RasterError::UnsupportedFontVariations(FONT_ID.into()))
    );
}
