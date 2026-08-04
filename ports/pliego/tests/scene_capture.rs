/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::io::Cursor;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use pliego::capture::{
    CaptureError, CapturedFontSource, MissingTextMapping, UnsupportedPaintEvent,
    UnsupportedPaintKind, capture_document_scene,
};
use pliego::pdf::{PdfFontResource, render_document_pdf};
use pliego::raster::{
    RasterFontResource, render_first_page_png, render_first_page_png_with_images,
};
use pliego::{Color, FillRule, Glyph, Operation, OperationMeta, Rect, Size};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use vello_cpu::Pixmap;

const IMAGE_URL: &str = "https://example.test/logo.png";
const LINK_URL: &str = "https://example.test/wrapped";

fn address(character: char) -> String {
    format!("sha256:{}", character.to_string().repeat(64))
}

fn content_address(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn font_instance_address(bytes: &[u8], face_index: u32, variations: &[(u32, f32)]) -> String {
    let resource_digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut hasher = Sha256::new();
    hasher.update(b"pliego-font-instance-v1\0");
    hasher.update(resource_digest);
    hasher.update(face_index.to_be_bytes());
    hasher.update((variations.len() as u64).to_be_bytes());
    for (tag, value) in variations {
        hasher.update(tag.to_be_bytes());
        hasher.update(value.to_bits().to_be_bytes());
    }
    format!(
        "sha256:{}",
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn text_run(
    text: &str,
    font_instance: &str,
    glyph_id: u32,
    x: f32,
    y: f32,
    diagnostic_font_id: &str,
) -> Value {
    json!({
        "text": text,
        "font_instance_id": font_instance,
        "font_identifier": { "Web": diagnostic_font_id },
        "requested_families": ["Pliego Fixture"],
        "selected_family": "pliego fixture",
        "font_size": 12.0,
        "glyphs": [{
            "id": glyph_id,
            "x": x,
            "y": y,
            "advance": 7.0,
            "text_range": { "start": 0, "end": 1 }
        }]
    })
}

#[test]
fn reports_font_source_resolution_order_and_fallbacks() {
    let mut capture: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    capture["fragments"][1]["text_run"]["font_identifier"] = json!({ "Local": {} });
    capture["fragments"][1]["text_run"]["requested_families"] =
        json!(["Missing Preferred", "Pliego Fixture"]);

    let captured = convert(&serde_json::to_vec(&capture).unwrap());
    let instance = capture["fragments"][1]["text_run"]["font_instance_id"]
        .as_str()
        .unwrap();
    let selection = captured
        .font_selections
        .iter()
        .find(|selection| selection.instance == instance)
        .unwrap();
    assert_eq!(selection.source, CapturedFontSource::Host);
    assert_eq!(
        selection.requested_families,
        ["Missing Preferred", "Pliego Fixture"]
    );
    assert_eq!(selection.selected_family.as_deref(), Some("pliego fixture"));
    assert_eq!(captured.font_warnings.len(), 1);
    assert_eq!(captured.font_warnings[0].code, "FONT_FALLBACK_USED");
    assert_eq!(
        captured.font_warnings[0].fallback_chain,
        ["Missing Preferred", "Pliego Fixture"]
    );
}

#[test]
fn retains_resolved_text_color_and_ordered_solid_background() {
    let mut snapshot: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    snapshot["fragments"][1]["text_run"]["color"] =
        json!({ "r": 0.2, "g": 0.4, "b": 0.8, "a": 0.75 });
    snapshot["paint_events"][1] = json!({
        "sequence": 1,
        "kind": "paint-rect",
        "fragment_id": 10,
        "tag_id": null,
        "spatial_node_id": 110,
        "clip_id": null,
        "paint_rects": [{
            "rect": { "x": 4.0, "y": 5.0, "width": 40.0, "height": 12.0 },
            "color": { "r": 0.05, "g": 0.1, "b": 0.2, "a": 1.0 },
            "kind": "background"
        }]
    });

    let captured = convert(&serde_json::to_vec(&snapshot).unwrap());
    let operations = &captured.scene.pages[0].operations;
    let Operation::Path {
        bounds, fill, meta, ..
    } = &operations[0]
    else {
        panic!("solid background must precede text");
    };
    assert_eq!(
        bounds,
        &Rect {
            x: 4.0,
            y: 5.0,
            width: 40.0,
            height: 12.0,
        }
    );
    assert_eq!(
        meta.semantics.as_ref().unwrap().label.as_deref(),
        Some("background")
    );
    assert_eq!(fill.unwrap().r, f64::from(0.05_f32));
    let Operation::Text { color, .. } = &operations[1] else {
        panic!("text must follow its background");
    };
    assert_eq!(
        *color,
        Color {
            r: f64::from(0.2_f32),
            g: f64::from(0.4_f32),
            b: f64::from(0.8_f32),
            a: f64::from(0.75_f32),
        }
    );
}

fn snapshot(local_id_base: usize, link_tag: u64) -> Vec<u8> {
    let variation_b = [(u32::from_be_bytes(*b"wght"), 700.0)];
    let font_resource_a = content_address(b"A");
    let font_resource_b = content_address(b"B");
    let font_instance_a = font_instance_address(b"A", 0, &[]);
    let font_instance_b = font_instance_address(b"B", 1, &variation_b);
    let box_id = local_id_base;
    let first_text_id = local_id_base + 1;
    let second_text_id = local_id_base + 2;
    let image_id = local_id_base + 3;
    let iframe_id = local_id_base + 4;

    serde_json::to_vec(&json!({
        "boxes": [{
            "depth": 0,
            "kind": "block",
            "tag_id": local_id_base as u64 + 9000
        }],
        "fragments": [
            {
                "depth": 0,
                "kind": "box",
                "rect": { "x": 0.0, "y": 0.0, "width": 200.0, "height": 100.0 },
                "tag_id": null,
                "paint_fragment_id": box_id,
                "text_run": null,
                "image_url": null
            },
            {
                "depth": 1,
                "kind": "text",
                "rect": { "x": 10.0, "y": 20.0, "width": 10.0, "height": 12.0 },
                "tag_id": link_tag,
                "paint_fragment_id": first_text_id,
                "text_run": text_run(
                    "A",
                    &font_instance_a,
                    41,
                    10.0,
                    30.0,
                    &format!("https://diagnostic.invalid/{local_id_base}/a.woff2")
                ),
                "image_url": null
            },
            {
                "depth": 1,
                "kind": "text",
                "rect": { "x": 10.0, "y": 32.0, "width": 15.0, "height": 12.0 },
                "tag_id": link_tag,
                "paint_fragment_id": second_text_id,
                "text_run": text_run(
                    "B",
                    &font_instance_b,
                    42,
                    10.0,
                    42.0,
                    &format!("https://diagnostic.invalid/{local_id_base}/b.woff2")
                ),
                "image_url": null
            },
            {
                "depth": 1,
                "kind": "image",
                "rect": { "x": 50.0, "y": 20.0, "width": 30.0, "height": 20.0 },
                "tag_id": null,
                "paint_fragment_id": image_id,
                "text_run": null,
                "image_url": IMAGE_URL
            },
            {
                "depth": 1,
                "kind": "iframe",
                "rect": { "x": 90.0, "y": 20.0, "width": 40.0, "height": 30.0 },
                "tag_id": null,
                "paint_fragment_id": iframe_id,
                "text_run": null,
                "image_url": null
            }
        ],
        "font_resources": [
            { "resource": font_resource_b, "bytes_base64": BASE64_STANDARD.encode(b"B") },
            { "resource": font_resource_a, "bytes_base64": BASE64_STANDARD.encode(b"A") }
        ],
        "font_instances": [
            {
                "id": font_instance_b,
                "resource": font_resource_b,
                "face_index": 1,
                "variations": [{
                    "tag": variation_b[0].0,
                    "value": variation_b[0].1
                }]
            },
            {
                "id": font_instance_a,
                "resource": font_resource_a,
                "face_index": 0,
                "variations": []
            }
        ],
        "page_sequence": {
            "pages": [{
                "index": 0,
                "width": 612.0,
                "height": 792.0,
                "margin_top": 72.0,
                "margin_right": 54.0,
                "margin_bottom": 36.0,
                "margin_left": 18.0,
                "available_inline_size": 540.0,
                "available_block_size": 684.0
            }]
        },
        "paint_events": [
            {
                "sequence": 0,
                "kind": "stacking-context-enter",
                "fragment_id": null,
                "tag_id": null,
                "spatial_node_id": local_id_base + 100,
                "clip_id": null
            },
            {
                "sequence": 1,
                "kind": "box",
                "fragment_id": box_id,
                "tag_id": null,
                "spatial_node_id": local_id_base + 101,
                "clip_id": local_id_base + 201
            },
            {
                "sequence": 2,
                "kind": "text",
                "fragment_id": first_text_id,
                "tag_id": link_tag,
                "spatial_node_id": local_id_base + 102,
                "clip_id": local_id_base + 202
            },
            {
                "sequence": 3,
                "kind": "outline",
                "fragment_id": box_id,
                "tag_id": null,
                "spatial_node_id": local_id_base + 103,
                "clip_id": local_id_base + 203
            },
            {
                "sequence": 4,
                "kind": "text",
                "fragment_id": second_text_id,
                "tag_id": link_tag,
                "spatial_node_id": local_id_base + 104,
                "clip_id": local_id_base + 204
            },
            {
                "sequence": 5,
                "kind": "image",
                "fragment_id": image_id,
                "tag_id": null,
                "spatial_node_id": local_id_base + 105,
                "clip_id": local_id_base + 205
            },
            {
                "sequence": 6,
                "kind": "root-background",
                "fragment_id": box_id,
                "tag_id": null,
                "spatial_node_id": local_id_base + 106,
                "clip_id": local_id_base + 206
            },
            {
                "sequence": 7,
                "kind": "collapsed-table-borders",
                "fragment_id": box_id,
                "tag_id": null,
                "spatial_node_id": local_id_base + 107,
                "clip_id": local_id_base + 207
            },
            {
                "sequence": 8,
                "kind": "iframe",
                "fragment_id": iframe_id,
                "tag_id": null,
                "spatial_node_id": local_id_base + 108,
                "clip_id": local_id_base + 208
            },
            {
                "sequence": 9,
                "kind": "text-effects",
                "fragment_id": first_text_id,
                "tag_id": link_tag,
                "spatial_node_id": local_id_base + 109,
                "clip_id": null
            },
            {
                "sequence": 10,
                "kind": "content-geometry",
                "fragment_id": first_text_id,
                "tag_id": link_tag,
                "spatial_node_id": local_id_base + 110,
                "clip_id": local_id_base + 210
            },
            {
                "sequence": 11,
                "kind": "stacking-context-leave",
                "fragment_id": null,
                "tag_id": null,
                "spatial_node_id": local_id_base + 111,
                "clip_id": null
            }
        ],
        "paint_epoch": 7,
        "paint_content_width": 200.0,
        "paint_content_height": 100.0,
        "paint_scroll_node_count": local_id_base + 20,
        "paintable": true,
        "contentful": true,
        "first_reflow": false,
        "links": [{ "tag_id": link_tag, "url": LINK_URL }]
    }))
    .unwrap()
}

fn two_page_snapshot(local_id_base: usize, link_tag: u64) -> Value {
    let mut capture: Value = serde_json::from_slice(&snapshot(local_id_base, link_tag)).unwrap();
    let mut second_page = capture["page_sequence"]["pages"][0].clone();
    second_page["index"] = json!(1);
    second_page["width"] = json!(400.0);
    second_page["height"] = json!(300.0);
    second_page["margin_top"] = json!(20.0);
    second_page["margin_right"] = json!(30.0);
    second_page["margin_bottom"] = json!(40.0);
    second_page["margin_left"] = json!(50.0);
    second_page["available_inline_size"] = json!(320.0);
    second_page["available_block_size"] = json!(240.0);
    capture["page_sequence"]["pages"]
        .as_array_mut()
        .unwrap()
        .push(second_page);

    capture["fragments"][2]["rect"]["y"] = json!(812.0);
    capture["fragments"][2]["text_run"]["glyphs"][0]["y"] = json!(822.0);
    capture["fragments"][3]["rect"]["y"] = json!(840.0);

    let shared_font = capture["fragments"][1]["text_run"]["font_instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    capture["fragments"][2]["text_run"]["font_instance_id"] = json!(shared_font.clone());
    let shared_resource = capture["font_instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|instance| instance["id"].as_str() == Some(shared_font.as_str()))
        .unwrap()["resource"]
        .as_str()
        .unwrap()
        .to_owned();
    capture["font_instances"]
        .as_array_mut()
        .unwrap()
        .retain(|instance| instance["id"].as_str() == Some(shared_font.as_str()));
    capture["font_resources"]
        .as_array_mut()
        .unwrap()
        .retain(|resource| resource["resource"].as_str() == Some(shared_resource.as_str()));
    capture
}

fn convert(snapshot: &[u8]) -> pliego::capture::SceneCapture {
    let image_resource = address('3');
    capture_document_scene(snapshot, |url| {
        (url == IMAGE_URL).then(|| image_resource.clone())
    })
    .unwrap()
}

fn vector_only_snapshot() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "boxes": [],
        "fragments": [{
            "depth": 0,
            "kind": "image",
            "rect": { "x": 50.0, "y": 20.0, "width": 32.0, "height": 32.0 },
            "tag_id": null,
            "paint_fragment_id": 7,
            "text_run": null,
            "image_url": null,
            "vector_image": {
                "viewport_width": 16.0,
                "viewport_height": 16.0,
                "items": [
                    {
                        "kind": "path",
                        "segments": [
                            { "kind": "move-to", "x": 1.0, "y": 1.0 },
                            { "kind": "line-to", "x": 15.0, "y": 1.0 },
                            { "kind": "line-to", "x": 15.0, "y": 15.0 },
                            { "kind": "line-to", "x": 1.0, "y": 15.0 },
                            { "kind": "close" }
                        ],
                        "fill": { "red": 0, "green": 0, "blue": 0, "alpha": 1.0 },
                        "fill_rule": "non_zero",
                        "stroke": {
                            "color": { "red": 204, "green": 26, "blue": 51, "alpha": 1.0 },
                            "width": 1.5
                        }
                    },
                    { "kind": "unsupported", "reason": "compositing" }
                ]
            }
        }],
        "font_resources": [],
        "font_instances": [],
        "page_sequence": {
            "pages": [{
                "index": 0,
                "width": 100.0,
                "height": 60.0,
                "margin_top": 5.0,
                "margin_right": 5.0,
                "margin_bottom": 5.0,
                "margin_left": 5.0,
                "available_inline_size": 90.0,
                "available_block_size": 50.0
            }]
        },
        "paint_events": [{
            "sequence": 0,
            "kind": "image",
            "fragment_id": 7,
            "tag_id": null,
            "spatial_node_id": 11,
            "clip_id": null
        }],
        "paint_epoch": 1,
        "paint_content_width": 100.0,
        "paint_content_height": 60.0,
        "paint_scroll_node_count": 1,
        "paintable": true,
        "contentful": true,
        "first_reflow": false,
        "links": []
    }))
    .unwrap()
}

#[test]
fn expands_retained_filled_and_stroked_svg_path_for_both_backends() {
    let captured = capture_document_scene(&vector_only_snapshot(), |_| None).unwrap();

    assert_eq!(
        captured.scene.pages[0].operations,
        vec![Operation::Path {
            bounds: Rect {
                x: 52.0,
                y: 22.0,
                width: 28.0,
                height: 28.0,
            },
            data: "M52,22 L80,22 L80,50 L52,50 Z".into(),
            fill: Some(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            fill_rule: FillRule::NonZero,
            stroke: Some(pliego::Stroke {
                color: Color {
                    r: 0.8,
                    g: 26.0 / 255.0,
                    b: 0.2,
                    a: 1.0,
                },
                width: 3.0,
            }),
            meta: OperationMeta::default(),
        }]
    );
    assert_eq!(
        captured.unsupported_events,
        vec![UnsupportedPaintEvent {
            sequence: 0,
            kind: UnsupportedPaintKind::SvgCompositing,
        }]
    );

    let png = render_first_page_png(&captured.scene, |_| None).unwrap();
    let pixmap = Pixmap::from_png(Cursor::new(png)).unwrap();
    let data = pixmap.data_as_u8_slice();
    let pixel = |x: usize, y: usize| {
        let start = (y * usize::from(pixmap.width()) + x) * 4;
        &data[start..start + 4]
    };
    assert_eq!(pixel(60, 30), [0, 0, 0, 255]);
    assert_eq!(pixel(48, 18), [255, 255, 255, 255]);

    let pdf = render_document_pdf(&captured.scene, |_| None, |_| None).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn retains_svg_text_font_identity_and_embedded_png_for_both_backends() {
    static FONT_BYTES: &[u8] = include_bytes!("fixtures/text-scene/Ahem.ttf");
    let face = ttf_parser::Face::parse(FONT_BYTES, 0).unwrap();
    let glyph_id = face.glyph_index('A').unwrap();
    let font_size = 8.0_f32;
    let advance = f32::from(face.glyph_hor_advance(glyph_id).unwrap()) * font_size
        / f32::from(face.units_per_em());
    let font_resource = content_address(FONT_BYTES);
    let font_instance = font_instance_address(FONT_BYTES, 0, &[]);

    let mut source_image = Pixmap::new(2, 1);
    source_image
        .data_as_u8_slice_mut()
        .copy_from_slice(&[255, 102, 0, 255, 0, 102, 204, 255]);
    let source_png = source_image.into_png().unwrap();
    let image_resource = content_address(&source_png);

    let mut snapshot: Value = serde_json::from_slice(&vector_only_snapshot()).unwrap();
    let items = snapshot["fragments"][0]["vector_image"]["items"]
        .as_array_mut()
        .unwrap();
    items.insert(
        1,
        json!({
            "kind": "image",
            "x": 2.0,
            "y": 2.0,
            "width": 4.0,
            "height": 2.0,
            "resource": image_resource,
            "bytes_base64": BASE64_STANDARD.encode(&source_png)
        }),
    );
    items.insert(
        2,
        json!({
            "kind": "text",
            "text": "A",
            "font": {
                "resource": font_resource,
                "bytes_base64": BASE64_STANDARD.encode(FONT_BYTES),
                "face_index": 0,
                "family": "Ahem"
            },
            "font_size": font_size,
            "glyphs": [{
                "id": u32::from(glyph_id.0),
                "x": 4.0,
                "y": 12.0,
                "advance": advance,
                "text_start": 0,
                "text_end": 1
            }]
        }),
    );

    let captured =
        capture_document_scene(&serde_json::to_vec(&snapshot).unwrap(), |_| None).unwrap();
    assert_eq!(captured.embedded_image_resources.len(), 1);
    assert_eq!(
        captured.embedded_image_resources[0].resource,
        image_resource
    );
    assert_eq!(captured.embedded_image_resources[0].png, source_png);
    assert_eq!(captured.font_resources.len(), 1);
    assert_eq!(captured.font_resources[0].resource, font_resource);
    assert_eq!(captured.font_instances.len(), 1);
    assert_eq!(captured.font_instances[0].id, font_instance);
    assert_eq!(captured.font_selections.len(), 1);
    assert_eq!(
        captured.font_selections[0].source,
        CapturedFontSource::Memory
    );
    assert_eq!(
        captured.font_selections[0].selected_family.as_deref(),
        Some("Ahem")
    );

    let operations = &captured.scene.pages[0].operations;
    assert_eq!(operations.len(), 3);
    let Operation::Image {
        resource, bounds, ..
    } = &operations[1]
    else {
        panic!("expected retained embedded SVG image");
    };
    assert_eq!(resource, &image_resource);
    assert_eq!(
        bounds,
        &Rect {
            x: 54.0,
            y: 24.0,
            width: 8.0,
            height: 4.0
        }
    );
    let Operation::Text {
        text,
        font,
        font_size,
        glyphs,
        ..
    } = &operations[2]
    else {
        panic!("expected retained SVG text run");
    };
    assert_eq!(text, "A");
    assert_eq!(font, &font_instance);
    assert_eq!(*font_size, 16.0);
    assert_eq!(glyphs[0].x, 58.0);
    assert_eq!(glyphs[0].y, 44.0);
    assert_eq!(
        glyphs[0].text_range,
        Some(pliego::Utf8Range { start: 0, end: 1 })
    );

    let preview = render_first_page_png_with_images(
        &captured.scene,
        |id| {
            (id == font_instance).then_some(RasterFontResource {
                bytes: FONT_BYTES,
                face_index: 0,
                variations: &[],
            })
        },
        |resource| (resource == image_resource).then_some(source_png.as_slice()),
    )
    .unwrap();
    assert!(preview.starts_with(b"\x89PNG\r\n\x1a\n"));
    let pdf = render_document_pdf(
        &captured.scene,
        |id| {
            (id == font_instance).then_some(PdfFontResource {
                bytes: FONT_BYTES,
                face_index: 0,
                variations: &[],
            })
        },
        |resource| (resource == image_resource).then_some(source_png.as_slice()),
    )
    .unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(
        pdf.windows(b"/ToUnicode".len())
            .any(|window| window == b"/ToUnicode")
    );
}

#[test]
fn converts_dense_paint_order_without_leaking_capture_local_ids() {
    let first = convert(&snapshot(10, 1000));
    let second = convert(&snapshot(800, 9000));
    let font_instance_a = font_instance_address(b"A", 0, &[]);
    let font_instance_b = font_instance_address(b"B", 1, &[(u32::from_be_bytes(*b"wght"), 700.0)]);

    assert_eq!(first.scene, second.scene);
    assert_eq!(
        first.scene.sha256().unwrap(),
        second.scene.sha256().unwrap()
    );
    assert_eq!(first.font_resources, second.font_resources);
    assert_eq!(first.font_instances, second.font_instances);
    assert_eq!(first.unsupported_events, second.unsupported_events);
    assert!(first.text_mapping_gaps.is_empty());
    assert_eq!(
        first.scene.pages[0].size,
        Size {
            width: 612.0,
            height: 792.0,
        }
    );

    assert_eq!(
        first.scene.pages[0].operations,
        vec![
            Operation::Text {
                text: "A".into(),
                font: font_instance_a.clone(),
                font_size: 12.0,
                color: Color::default(),
                glyphs: vec![Glyph {
                    id: 41,
                    x: 10.0,
                    y: 30.0,
                    advance: 7.0,
                    text_range: Some(pliego::Utf8Range { start: 0, end: 1 }),
                }],
                meta: OperationMeta::default(),
            },
            Operation::Link {
                bounds: Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 10.0,
                    height: 12.0,
                },
                target: LINK_URL.into(),
                meta: OperationMeta::default(),
            },
            Operation::Text {
                text: "B".into(),
                font: font_instance_b.clone(),
                font_size: 12.0,
                color: Color::default(),
                glyphs: vec![Glyph {
                    id: 42,
                    x: 10.0,
                    y: 42.0,
                    advance: 7.0,
                    text_range: Some(pliego::Utf8Range { start: 0, end: 1 }),
                }],
                meta: OperationMeta::default(),
            },
            Operation::Link {
                bounds: Rect {
                    x: 10.0,
                    y: 32.0,
                    width: 15.0,
                    height: 12.0,
                },
                target: LINK_URL.into(),
                meta: OperationMeta::default(),
            },
            Operation::Image {
                bounds: Rect {
                    x: 50.0,
                    y: 20.0,
                    width: 30.0,
                    height: 20.0,
                },
                resource: address('3'),
                meta: OperationMeta::default(),
            },
        ]
    );

    let mut expected_resources = vec![content_address(b"A"), content_address(b"B")];
    expected_resources.sort();
    assert_eq!(
        first
            .font_resources
            .iter()
            .map(|resource| resource.resource.clone())
            .collect::<Vec<_>>(),
        expected_resources
    );
    let mut expected_instances = vec![font_instance_a.clone(), font_instance_b.clone()];
    expected_instances.sort();
    assert_eq!(
        first
            .font_instances
            .iter()
            .map(|instance| instance.id.clone())
            .collect::<Vec<_>>(),
        expected_instances
    );
    let captured_a = first
        .font_instances
        .iter()
        .find(|instance| instance.id == font_instance_a)
        .unwrap();
    let captured_b = first
        .font_instances
        .iter()
        .find(|instance| instance.id == font_instance_b)
        .unwrap();
    assert_eq!(captured_a.resource, content_address(b"A"));
    assert_eq!(captured_b.resource, content_address(b"B"));
    assert_eq!(captured_b.face_index, 1);
    assert_eq!(captured_b.variations[0].value, 700.0);

    assert_eq!(
        first.unsupported_events,
        vec![
            UnsupportedPaintEvent {
                sequence: 1,
                kind: UnsupportedPaintKind::Box,
            },
            UnsupportedPaintEvent {
                sequence: 3,
                kind: UnsupportedPaintKind::Outline,
            },
            UnsupportedPaintEvent {
                sequence: 6,
                kind: UnsupportedPaintKind::RootBackground,
            },
            UnsupportedPaintEvent {
                sequence: 7,
                kind: UnsupportedPaintKind::CollapsedTableBorders,
            },
            UnsupportedPaintEvent {
                sequence: 8,
                kind: UnsupportedPaintKind::Iframe,
            },
            UnsupportedPaintEvent {
                sequence: 9,
                kind: UnsupportedPaintKind::TextEffects,
            },
            UnsupportedPaintEvent {
                sequence: 10,
                kind: UnsupportedPaintKind::ContentGeometry,
            },
        ]
    );
    assert!(
        serde_json::to_string(&first.unsupported_events)
            .unwrap()
            .contains("collapsed-table-borders")
    );
}

#[test]
fn converts_two_pages_to_page_local_operations_with_shared_resources() {
    let capture = two_page_snapshot(10, 1000);
    let captured = convert(&serde_json::to_vec(&capture).unwrap());
    let second = two_page_snapshot(800, 9000);
    let second = convert(&serde_json::to_vec(&second).unwrap());
    let shared_font = font_instance_address(b"A", 0, &[]);

    assert_eq!(captured.scene, second.scene);
    assert_eq!(captured.font_resources, second.font_resources);
    assert_eq!(captured.font_instances, second.font_instances);
    assert_eq!(
        captured
            .scene
            .pages
            .iter()
            .map(|page| page.size.clone())
            .collect::<Vec<_>>(),
        vec![
            Size {
                width: 612.0,
                height: 792.0,
            },
            Size {
                width: 400.0,
                height: 300.0,
            },
        ]
    );
    assert_eq!(
        captured.scene.pages[0].operations,
        vec![
            Operation::Text {
                text: "A".into(),
                font: shared_font.clone(),
                font_size: 12.0,
                color: Color::default(),
                glyphs: vec![Glyph {
                    id: 41,
                    x: 10.0,
                    y: 30.0,
                    advance: 7.0,
                    text_range: Some(pliego::Utf8Range { start: 0, end: 1 }),
                }],
                meta: OperationMeta::default(),
            },
            Operation::Link {
                bounds: Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 10.0,
                    height: 12.0,
                },
                target: LINK_URL.into(),
                meta: OperationMeta::default(),
            },
        ]
    );
    assert_eq!(
        captured.scene.pages[1].operations,
        vec![
            Operation::Text {
                text: "B".into(),
                font: shared_font.clone(),
                font_size: 12.0,
                color: Color::default(),
                glyphs: vec![Glyph {
                    id: 42,
                    x: 10.0,
                    y: 30.0,
                    advance: 7.0,
                    text_range: Some(pliego::Utf8Range { start: 0, end: 1 }),
                }],
                meta: OperationMeta::default(),
            },
            Operation::Link {
                bounds: Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 15.0,
                    height: 12.0,
                },
                target: LINK_URL.into(),
                meta: OperationMeta::default(),
            },
            Operation::Image {
                bounds: Rect {
                    x: 50.0,
                    y: 48.0,
                    width: 30.0,
                    height: 20.0,
                },
                resource: address('3'),
                meta: OperationMeta::default(),
            },
        ]
    );
    assert_eq!(captured.font_resources.len(), 1);
    assert_eq!(captured.font_resources[0].resource, content_address(b"A"));
    assert_eq!(captured.font_instances.len(), 1);
    assert_eq!(captured.font_instances[0].id, shared_font);
    assert!(captured.text_mapping_gaps.is_empty());
    assert!(captured.scene.sha256().is_ok());
}

#[test]
fn rejects_an_operation_crossing_a_page_boundary() {
    let mut capture = two_page_snapshot(10, 1000);
    capture["fragments"][1]["rect"]["y"] = json!(785.0);
    capture["fragments"][1]["rect"]["height"] = json!(12.0);
    capture["fragments"][1]["text_run"]["glyphs"][0]["y"] = json!(790.0);

    assert_eq!(
        capture_document_scene(&serde_json::to_vec(&capture).unwrap(), |url| {
            (url == IMAGE_URL).then(|| address('3'))
        }),
        Err(CaptureError::OperationCrossesPageBoundary {
            sequence: 2,
            page_index: 0,
        })
    );
}

#[test]
fn rejects_a_sparse_paint_sequence_instead_of_reordering_it() {
    let mut capture: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    capture["paint_events"][4]["sequence"] = json!(5);

    assert_eq!(
        capture_document_scene(&serde_json::to_vec(&capture).unwrap(), |_| None),
        Err(CaptureError::NonDensePaintEvents {
            expected: 4,
            actual: 5,
        })
    );
}

#[test]
fn rejects_missing_or_invalid_page_geometry() {
    let original: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();

    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("page_sequence");
    assert_eq!(
        capture_document_scene(&serde_json::to_vec(&missing).unwrap(), |_| None),
        Err(CaptureError::MissingPageSequence)
    );

    let mut empty = original.clone();
    empty["page_sequence"]["pages"] = json!([]);
    assert_eq!(
        capture_document_scene(&serde_json::to_vec(&empty).unwrap(), |_| None),
        Err(CaptureError::InvalidPageCount { actual: 0 })
    );

    let mut non_dense_pages = original.clone();
    let mut second = non_dense_pages["page_sequence"]["pages"][0].clone();
    second["index"] = json!(2);
    non_dense_pages["page_sequence"]["pages"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert_eq!(
        capture_document_scene(&serde_json::to_vec(&non_dense_pages).unwrap(), |_| None),
        Err(CaptureError::InvalidPageIndex { actual: 2 })
    );

    let mut wrong_index = original.clone();
    wrong_index["page_sequence"]["pages"][0]["index"] = json!(1);
    assert_eq!(
        capture_document_scene(&serde_json::to_vec(&wrong_index).unwrap(), |_| None),
        Err(CaptureError::InvalidPageIndex { actual: 1 })
    );

    let mut invalid_geometry = original;
    invalid_geometry["page_sequence"]["pages"][0]["margin_left"] = json!(-1.0);
    assert_eq!(
        capture_document_scene(&serde_json::to_vec(&invalid_geometry).unwrap(), |_| None),
        Err(CaptureError::InvalidPageGeometry)
    );
}

#[test]
fn rejects_untrusted_font_resource_and_instance_identities() {
    let mut invalid_base64: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    invalid_base64["font_resources"][0]["bytes_base64"] = json!("not-base64");
    assert!(matches!(
        capture_document_scene(&serde_json::to_vec(&invalid_base64).unwrap(), |_| None),
        Err(CaptureError::InvalidFontResourceBase64(_))
    ));

    let mut mismatched_resource: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    mismatched_resource["font_resources"][0]["bytes_base64"] =
        json!(BASE64_STANDARD.encode(b"different bytes"));
    assert!(matches!(
        capture_document_scene(&serde_json::to_vec(&mismatched_resource).unwrap(), |_| None),
        Err(CaptureError::FontResourceDigestMismatch { .. })
    ));

    let mut mismatched_instance: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    mismatched_instance["font_instances"][0]["id"] = json!(address('f'));
    assert!(matches!(
        capture_document_scene(&serde_json::to_vec(&mismatched_instance).unwrap(), |_| None),
        Err(CaptureError::FontInstanceIdMismatch { .. })
    ));
}

#[test]
fn rejects_glyph_ranges_outside_the_retained_utf8_text() {
    let mut capture: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    capture["fragments"][1]["text_run"]["glyphs"][0]["text_range"]["end"] = json!(2);

    assert!(matches!(
        capture_document_scene(&serde_json::to_vec(&capture).unwrap(), |url| {
            (url == IMAGE_URL).then(|| address('3'))
        }),
        Err(CaptureError::InvalidScene(
            "glyph text range is out of bounds"
        ))
    ));
}

#[test]
fn reports_missing_glyph_source_mapping_as_a_capture_gap() {
    let mut capture: Value = serde_json::from_slice(&snapshot(10, 1000)).unwrap();
    capture["fragments"][1]["text_run"]["glyphs"][0]
        .as_object_mut()
        .unwrap()
        .remove("text_range");

    let captured = capture_document_scene(&serde_json::to_vec(&capture).unwrap(), |url| {
        (url == IMAGE_URL).then(|| address('3'))
    })
    .unwrap();

    assert_eq!(
        captured.text_mapping_gaps,
        vec![MissingTextMapping {
            sequence: 2,
            glyph_index: 0,
        }]
    );
}
