/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::io::Cursor;

use pliego::raster::{
    RasterError, RasterFontResource, RasterFontVariation, render_first_page_png,
    render_first_page_png_with_images, render_pages_png_with_images,
};
use pliego::{
    Color, DocumentScene, FillRule, Glyph, IMAGE_LIMITS, ImageLimit, Operation, OperationMeta,
    Page, Rect, Size,
};
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
                    text_range: Some(pliego::Utf8Range { start: 0, end: 6 }),
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

fn image_scene() -> DocumentScene {
    DocumentScene::new(Page {
        size: Size {
            width: 8.0,
            height: 8.0,
        },
        operations: vec![Operation::Image {
            bounds: Rect {
                x: 3.0,
                y: 4.0,
                width: 2.0,
                height: 1.0,
            },
            resource: "sha256:image-fixture".into(),
            meta: OperationMeta::default(),
        }],
    })
}

fn image_png() -> Vec<u8> {
    let mut pixmap = Pixmap::new(2, 1);
    pixmap
        .data_as_u8_slice_mut()
        .copy_from_slice(&[255, 0, 0, 255, 0, 0, 255, 255]);
    pixmap.into_png().unwrap()
}

fn png_with_dimensions(width: u32, height: u32) -> Vec<u8> {
    let mut png = image_png();
    png[16..20].copy_from_slice(&width.to_be_bytes());
    png[20..24].copy_from_slice(&height.to_be_bytes());
    png
}

fn png_ihdr_dimensions(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    )
}

fn render_image(image: &[u8]) -> Result<Vec<u8>, RasterError> {
    render_first_page_png_with_images(
        &image_scene(),
        |_| None,
        |resource| (resource == "sha256:image-fixture").then_some(image),
    )
}

#[test]
fn rasterizes_positioned_glyphs_without_dom_layout_or_shaping() {
    let mut scene = text_scene();
    scene.pages[0].operations.insert(
        0,
        Operation::Path {
            bounds: Rect {
                x: 2.0,
                y: 2.0,
                width: 12.0,
                height: 12.0,
            },
            data: "M2 2h12v12H2z".into(),
            fill: Some(Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            fill_rule: FillRule::NonZero,
            stroke: None,
            meta: OperationMeta::default(),
        },
    );
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
fn rasterizes_exact_png_resource_at_scene_bounds() {
    let scene = image_scene();
    let source = image_png();
    let render = || {
        render_first_page_png_with_images(
            &scene,
            |_| None,
            |resource| (resource == "sha256:image-fixture").then_some(source.as_slice()),
        )
        .unwrap()
    };
    let first = render();
    let second = render();

    assert_eq!(first, second);
    let pixmap = Pixmap::from_png(Cursor::new(first)).unwrap();
    let data = pixmap.data_as_u8_slice();
    let pixel = |x: usize, y: usize| {
        let start = (y * usize::from(pixmap.width()) + x) * 4;
        &data[start..start + 4]
    };
    assert_eq!(pixel(3, 4), [255, 0, 0, 255]);
    assert_eq!(pixel(4, 4), [0, 0, 255, 255]);
    assert_eq!(pixel(2, 4), [0, 0, 0, 0]);
}

#[test]
fn rasterizes_all_pages_without_dom_or_layout_and_shares_resource_caches() {
    let first_image = Operation::Image {
        bounds: Rect {
            x: 1.0,
            y: 1.0,
            width: 2.0,
            height: 1.0,
        },
        resource: "sha256:image-fixture".into(),
        meta: OperationMeta::default(),
    };
    let second_image = Operation::Image {
        bounds: Rect {
            x: 20.0,
            y: 44.0,
            width: 2.0,
            height: 1.0,
        },
        resource: "sha256:image-fixture".into(),
        meta: OperationMeta::default(),
    };
    let mut scene = text_scene();
    scene.pages[0].operations.push(first_image);
    scene.pages.push(Page {
        size: Size {
            width: 32.0,
            height: 48.0,
        },
        operations: vec![
            Operation::Text {
                text: "second".into(),
                font: FONT_ID.into(),
                font_size: 16.0,
                glyphs: vec![Glyph {
                    id: 0,
                    x: 4.0,
                    y: 20.0,
                    advance: 10.0,
                    text_range: Some(pliego::Utf8Range { start: 0, end: 6 }),
                }],
                meta: OperationMeta::default(),
            },
            second_image,
        ],
    });

    let source = image_png();
    let mut font_resolves = 0;
    let mut image_resolves = 0;
    let pages = render_pages_png_with_images(
        &scene,
        |font| {
            font_resolves += 1;
            fixture_font(font)
        },
        |resource| {
            image_resolves += 1;
            (resource == "sha256:image-fixture").then_some(source.as_slice())
        },
    )
    .unwrap();

    assert_eq!(pages.len(), 2);
    assert_eq!(png_ihdr_dimensions(&pages[0]), (64, 64));
    assert_eq!(png_ihdr_dimensions(&pages[1]), (32, 48));
    assert_ne!(pages[0], pages[1], "page-local content must not be reused");
    assert_eq!(font_resolves, 1);
    assert_eq!(image_resolves, 1);

    let first = Pixmap::from_png(Cursor::new(&pages[0])).unwrap();
    let second = Pixmap::from_png(Cursor::new(&pages[1])).unwrap();
    let rgba = |pixmap: &Pixmap, x: usize, y: usize| {
        let start = (y * usize::from(pixmap.width()) + x) * 4;
        <[u8; 4]>::try_from(&pixmap.data_as_u8_slice()[start..start + 4]).unwrap()
    };
    assert_eq!(rgba(&first, 1, 1), [255, 0, 0, 255]);
    assert_eq!(rgba(&second, 20, 44), [255, 0, 0, 255]);

    let mut compatibility_scene = scene.clone();
    let Operation::Image { resource, .. } = &mut compatibility_scene.pages[1].operations[1] else {
        panic!("second page must end with its image fixture")
    };
    *resource = "sha256:second-page-must-not-resolve".into();
    let compatibility =
        render_first_page_png_with_images(&compatibility_scene, fixture_font, |resource| {
            (resource == "sha256:image-fixture").then_some(source.as_slice())
        })
        .unwrap();
    assert_eq!(compatibility, pages[0]);
}

#[test]
fn bounds_image_decoding_and_recovers_after_each_typed_rejection() {
    let encoded = vec![0; usize::try_from(IMAGE_LIMITS.encoded_bytes + 1).unwrap()];
    assert_eq!(
        render_image(&encoded),
        Err(RasterError::ImageLimitExceeded {
            resource: "sha256:image-fixture".into(),
            limit: ImageLimit::EncodedBytes,
            configured: IMAGE_LIMITS.encoded_bytes,
            observed: IMAGE_LIMITS.encoded_bytes + 1,
        })
    );
    drop(encoded);

    let dimension = png_with_dimensions(IMAGE_LIMITS.declared_dimension as u32 + 1, 1);
    assert_eq!(
        render_image(&dimension),
        Err(RasterError::ImageLimitExceeded {
            resource: "sha256:image-fixture".into(),
            limit: ImageLimit::DeclaredWidth,
            configured: IMAGE_LIMITS.declared_dimension,
            observed: IMAGE_LIMITS.declared_dimension + 1,
        })
    );

    let pixels = png_with_dimensions(7_122, 14_041);
    assert_eq!(
        render_image(&pixels),
        Err(RasterError::ImageLimitExceeded {
            resource: "sha256:image-fixture".into(),
            limit: ImageLimit::DecodedPixels,
            configured: IMAGE_LIMITS.decoded_pixels,
            observed: 100_000_002,
        })
    );

    let decompressed = png_with_dimensions(8_321, 8_065);
    assert_eq!(
        render_image(&decompressed),
        Err(RasterError::ImageLimitExceeded {
            resource: "sha256:image-fixture".into(),
            limit: ImageLimit::DecompressedBytes,
            configured: IMAGE_LIMITS.decompressed_bytes,
            observed: IMAGE_LIMITS.decompressed_bytes + 4,
        })
    );

    assert!(render_image(&image_png()).is_ok());
}

#[test]
fn rejects_missing_and_non_png_image_resources() {
    let scene = image_scene();
    assert_eq!(
        render_first_page_png_with_images(&scene, |_| None, |_| None),
        Err(RasterError::MissingImage("sha256:image-fixture".into()))
    );
    assert!(matches!(
        render_first_page_png_with_images(&scene, |_| None, |_| Some(b"not a PNG")),
        Err(RasterError::InvalidImage { index: 0, .. })
    ));
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
            data: "not an SVG path".into(),
            fill: Some(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            fill_rule: FillRule::NonZero,
            stroke: None,
            meta: OperationMeta::default(),
        },
    );
    assert!(matches!(
        render_first_page_png(&path_scene, fixture_font),
        Err(RasterError::InvalidPath { index: 0, .. })
    ));

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
