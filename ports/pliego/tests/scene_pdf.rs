/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::io::Read;

use flate2::read::ZlibDecoder;
use pliego::pdf::{
    FontEmbeddingRestriction, PdfError, PdfFontResource, PdfFontVariation, render_document_pdf,
};
use pliego::{
    Color, DocumentScene, FillRule, Glyph, IMAGE_LIMITS, ImageLimit, Operation, OperationMeta,
    Page, Rect, Size, Utf8Range,
};

const DEJAVU_SANS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
));
const IMAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/wpt/tests/images/lcp-1x1.png"
));
const FONT_ID: &str = "sha256:dejavu-sans-fixture";
const IMAGE_ID: &str = "sha256:image-fixture";

fn scene() -> DocumentScene {
    DocumentScene::new(Page {
        size: Size {
            width: 200.0,
            height: 200.0,
        },
        operations: vec![
            Operation::Text {
                text: "Hi".into(),
                font: FONT_ID.into(),
                font_size: 12.0,
                glyphs: vec![
                    Glyph {
                        id: 43,
                        x: 10.0,
                        y: 60.0,
                        advance: 10.0,
                        text_range: Some(Utf8Range { start: 0, end: 1 }),
                    },
                    Glyph {
                        id: 76,
                        x: 20.0,
                        y: 60.0,
                        advance: 4.0,
                        text_range: Some(Utf8Range { start: 1, end: 2 }),
                    },
                ],
                meta: OperationMeta::default(),
            },
            Operation::Path {
                bounds: Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 20.0,
                    height: 20.0,
                },
                data: "M10 10L30 10L30 30Z".into(),
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
            Operation::Image {
                bounds: Rect {
                    x: 10.0,
                    y: 80.0,
                    width: 16.0,
                    height: 16.0,
                },
                resource: IMAGE_ID.into(),
                meta: OperationMeta::default(),
            },
            Operation::Link {
                bounds: Rect {
                    x: 10.0,
                    y: 110.0,
                    width: 80.0,
                    height: 12.0,
                },
                target: "https://example.com".into(),
                meta: OperationMeta::default(),
            },
        ],
    })
}

fn two_page_scene() -> DocumentScene {
    let mut scene = scene();
    scene.pages.push(Page {
        size: Size {
            width: 320.0,
            height: 240.0,
        },
        operations: vec![
            Operation::Text {
                text: "Hi".into(),
                font: FONT_ID.into(),
                font_size: 12.0,
                glyphs: vec![
                    Glyph {
                        id: 43,
                        x: 40.0,
                        y: 80.0,
                        advance: 10.0,
                        text_range: Some(Utf8Range { start: 0, end: 1 }),
                    },
                    Glyph {
                        id: 76,
                        x: 50.0,
                        y: 80.0,
                        advance: 4.0,
                        text_range: Some(Utf8Range { start: 1, end: 2 }),
                    },
                ],
                meta: OperationMeta::default(),
            },
            Operation::Image {
                bounds: Rect {
                    x: 100.0,
                    y: 120.0,
                    width: 16.0,
                    height: 16.0,
                },
                resource: IMAGE_ID.into(),
                meta: OperationMeta::default(),
            },
            Operation::Link {
                bounds: Rect {
                    x: 40.0,
                    y: 50.0,
                    width: 80.0,
                    height: 20.0,
                },
                target: "https://example.com/page-2".into(),
                meta: OperationMeta::default(),
            },
        ],
    });
    scene
}

fn fixture_font(font: &str) -> Option<PdfFontResource<'static>> {
    (font == FONT_ID).then_some(PdfFontResource {
        bytes: DEJAVU_SANS,
        face_index: 0,
        variations: &[],
    })
}

fn fixture_image(image: &str) -> Option<&'static [u8]> {
    (image == IMAGE_ID).then_some(IMAGE)
}

fn image_only_scene() -> DocumentScene {
    let mut image_only = scene();
    image_only.pages[0]
        .operations
        .retain(|operation| matches!(operation, Operation::Image { .. }));
    image_only
}

fn png_with_dimensions(width: u32, height: u32) -> Vec<u8> {
    let mut png = IMAGE.to_vec();
    png[16..20].copy_from_slice(&width.to_be_bytes());
    png[20..24].copy_from_slice(&height.to_be_bytes());
    png
}

fn render_image(image: &[u8]) -> Result<Vec<u8>, PdfError> {
    render_document_pdf(
        &image_only_scene(),
        |_| None,
        |resource| (resource == IMAGE_ID).then_some(image),
    )
}

#[test]
fn emits_a_deterministic_structural_pdf_with_selectable_unicode() {
    let first = render_document_pdf(&scene(), fixture_font, fixture_image).unwrap();
    let second = render_document_pdf(&scene(), fixture_font, fixture_image).unwrap();

    assert_eq!(first, second);
    assert!(first.starts_with(b"%PDF-"));
    assert_eq!(number_array(&first, b"/MediaBox"), [0.0, 0.0, 150.0, 150.0]);
    for marker in [
        b"/MediaBox".as_slice(),
        b"/ToUnicode".as_slice(),
        b"/FontFile2".as_slice(),
        b"/Annots".as_slice(),
        b"https://example.com/".as_slice(),
    ] {
        assert!(contains(&first, marker), "missing PDF marker {marker:?}");
    }
    assert!(
        contains(&first, b"/Subtype/Image") || contains(&first, b"/Subtype /Image"),
        "the scene image must remain a PDF image object"
    );
    assert!(
        decoded_streams(&first)
            .iter()
            .any(|stream| contains(stream, b"7.5 7.5 m")),
        "the scene vector path must use PDF points and remain vector content"
    );

    let cmap = referenced_stream(&first, b"/ToUnicode");
    assert!(
        contains(&cmap, b"0048"),
        "ToUnicode must map the retained H cluster"
    );
    assert!(
        contains(&cmap, b"0069"),
        "ToUnicode must map the retained i cluster"
    );
}

#[test]
fn emits_every_page_with_local_geometry_and_shared_resources() {
    let mut font_resolutions = 0;
    let mut image_resolutions = 0;
    let pdf = render_document_pdf(
        &two_page_scene(),
        |font| {
            font_resolutions += 1;
            fixture_font(font)
        },
        |image| {
            image_resolutions += 1;
            fixture_image(image)
        },
    )
    .unwrap();

    assert_eq!(
        number_arrays::<4>(&pdf, b"/MediaBox"),
        vec![[0.0, 0.0, 150.0, 150.0], [0.0, 0.0, 240.0, 180.0]]
    );
    assert!(contains(&pdf, b"/Count 2"));
    let streams = decoded_streams(&pdf);
    assert!(
        streams
            .iter()
            .any(|stream| contains(stream, b"1 0 0 -1 7.5 45 Tm")),
        "page 1 text must retain its scaled page-local origin"
    );
    assert!(
        streams
            .iter()
            .any(|stream| contains(stream, b"1 0 0 -1 30 60 Tm")),
        "page 2 text must retain its scaled page-local origin"
    );
    let mut link_rects = number_arrays::<4>(&pdf, b"/Rect");
    link_rects.sort_by(|left, right| left[0].total_cmp(&right[0]));
    assert_eq!(
        link_rects,
        vec![[7.5, 58.5, 67.5, 67.5], [30.0, 127.5, 90.0, 142.5]]
    );
    assert!(contains(&pdf, b"https://example.com/"));
    assert!(contains(&pdf, b"https://example.com/page-2"));

    assert_eq!(font_resolutions, 1);
    assert_eq!(image_resolutions, 1);
    assert_eq!(occurrences(&pdf, b"/FontFile2"), 1);
    assert_eq!(
        occurrences(&pdf, b"/Subtype/Image") + occurrences(&pdf, b"/Subtype /Image"),
        1
    );
}

#[test]
fn preserves_shared_text_clusters_with_actual_text() {
    let mut clustered = scene();
    let Operation::Text { glyphs, .. } = &mut clustered.pages[0].operations[0] else {
        unreachable!()
    };
    for glyph in glyphs {
        glyph.text_range = Some(Utf8Range { start: 0, end: 2 });
    }

    let pdf = render_document_pdf(&clustered, fixture_font, fixture_image).unwrap();
    assert!(
        decoded_streams(&pdf)
            .iter()
            .any(|stream| contains(stream, b"/ActualText")),
        "glyphs sharing a shaping cluster must expose one ActualText span"
    );
}

#[test]
fn bounds_image_decoding_and_recovers_after_each_typed_rejection() {
    let encoded = vec![0; usize::try_from(IMAGE_LIMITS.encoded_bytes + 1).unwrap()];
    assert_eq!(
        render_image(&encoded),
        Err(PdfError::ImageLimitExceeded {
            resource: IMAGE_ID.into(),
            limit: ImageLimit::EncodedBytes,
            configured: IMAGE_LIMITS.encoded_bytes,
            observed: IMAGE_LIMITS.encoded_bytes + 1,
        })
    );
    assert!(render_image(IMAGE).is_ok());
    drop(encoded);

    let dimension = png_with_dimensions(IMAGE_LIMITS.declared_dimension as u32 + 1, 1);
    assert_eq!(
        render_image(&dimension),
        Err(PdfError::ImageLimitExceeded {
            resource: IMAGE_ID.into(),
            limit: ImageLimit::DeclaredWidth,
            configured: IMAGE_LIMITS.declared_dimension,
            observed: IMAGE_LIMITS.declared_dimension + 1,
        })
    );
    assert!(render_image(IMAGE).is_ok());

    let height = png_with_dimensions(1, IMAGE_LIMITS.declared_dimension as u32 + 1);
    assert_eq!(
        render_image(&height),
        Err(PdfError::ImageLimitExceeded {
            resource: IMAGE_ID.into(),
            limit: ImageLimit::DeclaredHeight,
            configured: IMAGE_LIMITS.declared_dimension,
            observed: IMAGE_LIMITS.declared_dimension + 1,
        })
    );
    assert!(render_image(IMAGE).is_ok());

    let pixels = png_with_dimensions(7_122, 14_041);
    assert_eq!(
        render_image(&pixels),
        Err(PdfError::ImageLimitExceeded {
            resource: IMAGE_ID.into(),
            limit: ImageLimit::DecodedPixels,
            configured: IMAGE_LIMITS.decoded_pixels,
            observed: 100_000_002,
        })
    );
    assert!(render_image(IMAGE).is_ok());

    let mut decompressed = png_with_dimensions(8_283, 4_051);
    decompressed[24] = 16;
    decompressed[25] = 6;
    assert_eq!(
        render_image(&decompressed),
        Err(PdfError::ImageLimitExceeded {
            resource: IMAGE_ID.into(),
            limit: ImageLimit::DecompressedBytes,
            configured: IMAGE_LIMITS.decompressed_bytes,
            observed: IMAGE_LIMITS.decompressed_bytes + 8,
        })
    );

    assert!(render_image(IMAGE).is_ok());
}

#[test]
fn rejects_a_malformed_pdf_image_and_recovers_on_the_next_render() {
    assert_eq!(
        render_image(b"not an image"),
        Err(PdfError::InvalidImage {
            resource: IMAGE_ID.into(),
            message: "unsupported image format".into(),
        })
    );
    assert!(render_image(IMAGE).is_ok());
}

#[test]
fn rejects_unmapped_text_and_unsafe_or_oversized_resources() {
    let mut missing_mapping = scene();
    let Operation::Text { glyphs, .. } = &mut missing_mapping.pages[0].operations[0] else {
        unreachable!()
    };
    glyphs[0].text_range = None;
    assert_eq!(
        render_document_pdf(&missing_mapping, fixture_font, fixture_image),
        Err(PdfError::MissingTextMapping {
            operation: 0,
            glyph: 0,
        })
    );

    let mut invalid_mapping = scene();
    let Operation::Text { glyphs, .. } = &mut invalid_mapping.pages[0].operations[0] else {
        unreachable!()
    };
    glyphs[0].text_range = Some(Utf8Range { start: 0, end: 3 });
    assert_eq!(
        render_document_pdf(&invalid_mapping, fixture_font, fixture_image),
        Err(PdfError::InvalidTextMapping {
            operation: 0,
            glyph: 0,
        })
    );

    let mut unsafe_link = scene();
    let Operation::Link { target, .. } = &mut unsafe_link.pages[0].operations[3] else {
        unreachable!()
    };
    *target = "javascript:alert(1)".into();
    assert!(matches!(
        render_document_pdf(&unsafe_link, fixture_font, fixture_image),
        Err(PdfError::UnsafeLink { operation: 3, .. })
    ));

    let mut huge_png = IMAGE.to_vec();
    huge_png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
    huge_png[20..24].copy_from_slice(&20_000u32.to_be_bytes());
    assert_eq!(
        render_document_pdf(&scene(), fixture_font, |image| {
            (image == IMAGE_ID).then_some(huge_png.as_slice())
        }),
        Err(PdfError::ImageLimitExceeded {
            resource: IMAGE_ID.into(),
            limit: ImageLimit::DeclaredWidth,
            configured: IMAGE_LIMITS.declared_dimension,
            observed: 20_000,
        })
    );

    let variations = [PdfFontVariation {
        tag: u32::from_be_bytes(*b"wght"),
        value: f32::NAN,
    }];
    assert_eq!(
        render_document_pdf(
            &scene(),
            |font| {
                (font == FONT_ID).then_some(PdfFontResource {
                    bytes: DEJAVU_SANS,
                    face_index: 0,
                    variations: &variations,
                })
            },
            fixture_image,
        ),
        Err(PdfError::InvalidFontVariation(FONT_ID.into()))
    );
}

#[test]
fn enforces_font_embedding_and_subsetting_rights() {
    for (fs_type, restriction) in [
        (0x0002, FontEmbeddingRestriction::RestrictedLicense),
        (0x0100, FontEmbeddingRestriction::NoSubsetting),
        (0x0200, FontEmbeddingRestriction::BitmapOnly),
    ] {
        let font = font_with_fs_type(fs_type);
        assert_eq!(
            render_document_pdf(
                &scene(),
                |resource| {
                    (resource == FONT_ID).then_some(PdfFontResource {
                        bytes: &font,
                        face_index: 0,
                        variations: &[],
                    })
                },
                fixture_image,
            ),
            Err(PdfError::RestrictedFontEmbedding {
                resource: FONT_ID.into(),
                restriction,
            })
        );
    }

    for fs_type in [0x0000, 0x0008] {
        let font = font_with_fs_type(fs_type);
        assert!(
            render_document_pdf(
                &scene(),
                |resource| {
                    (resource == FONT_ID).then_some(PdfFontResource {
                        bytes: &font,
                        face_index: 0,
                        variations: &[],
                    })
                },
                fixture_image,
            )
            .is_ok(),
            "installable/editable fsType {fs_type:#06x} should permit subsetting"
        );
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn number_array<const N: usize>(pdf: &[u8], key: &[u8]) -> [f32; N] {
    number_arrays(pdf, key).into_iter().next().unwrap()
}

fn number_arrays<const N: usize>(pdf: &[u8], key: &[u8]) -> Vec<[f32; N]> {
    let mut arrays = Vec::new();
    let mut offset = 0;
    while let Some(key_offset) = position(&pdf[offset..], key) {
        let after_key = &pdf[offset + key_offset + key.len()..];
        let start = position(after_key, b"[").unwrap() + 1;
        let end = start + position(&after_key[start..], b"]").unwrap();
        arrays.push(
            std::str::from_utf8(&after_key[start..end])
                .unwrap()
                .split_ascii_whitespace()
                .map(|value| value.parse().unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .ok()
                .unwrap(),
        );
        offset += key_offset + key.len() + end + 1;
    }
    arrays
}

fn referenced_stream(pdf: &[u8], key: &[u8]) -> Vec<u8> {
    let key_offset = position(pdf, key).unwrap() + key.len();
    let value = pdf[key_offset..]
        .iter()
        .copied()
        .skip_while(|byte| byte.is_ascii_whitespace())
        .take_while(|byte| byte.is_ascii_digit())
        .collect::<Vec<_>>();
    let object = [String::from_utf8(value).unwrap(), " 0 obj".into()].concat();
    let object_offset = position(pdf, object.as_bytes()).unwrap();
    let object_end = object_offset + position(&pdf[object_offset..], b"endobj").unwrap();
    decode_stream(&pdf[object_offset..object_end]).unwrap()
}

fn decoded_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut streams = Vec::new();
    let mut offset = 0;
    while let Some(end) = position(&pdf[offset..], b"endobj") {
        let object_end = offset + end + b"endobj".len();
        if let Some(stream) = decode_stream(&pdf[offset..object_end]) {
            streams.push(stream);
        }
        offset = object_end;
    }
    streams
}

fn decode_stream(object: &[u8]) -> Option<Vec<u8>> {
    let keyword = position(object, b"stream")?;
    let mut start = keyword + b"stream".len();
    if object.get(start..start + 2) == Some(b"\r\n") {
        start += 2;
    } else if object.get(start) == Some(&b'\n') {
        start += 1;
    } else {
        return None;
    }
    let mut end = start + position(&object[start..], b"endstream")?;
    while end > start && matches!(object[end - 1], b'\r' | b'\n') {
        end -= 1;
    }
    let encoded = &object[start..end];
    if contains(&object[..keyword], b"/FlateDecode") {
        let mut decoded = Vec::new();
        ZlibDecoder::new(encoded).read_to_end(&mut decoded).ok()?;
        Some(decoded)
    } else {
        Some(encoded.to_vec())
    }
}

fn font_with_fs_type(fs_type: u16) -> Vec<u8> {
    let mut font = DEJAVU_SANS.to_vec();
    let table_count = usize::from(u16::from_be_bytes(font[4..6].try_into().unwrap()));
    for index in 0..table_count {
        let record = 12 + index * 16;
        if &font[record..record + 4] == b"OS/2" {
            let table =
                u32::from_be_bytes(font[record + 8..record + 12].try_into().unwrap()) as usize;
            font[table + 8..table + 10].copy_from_slice(&fs_type.to_be_bytes());
            return font;
        }
    }
    panic!("fixture font has no OS/2 table")
}
