/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Direct `DocumentScene` to PDF benchmark. Run with `--no-default-features` so the shell,
//! `DocumentSession`, and their dependency graph are not compiled or initialized.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::io::Read;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use flate2::read::ZlibDecoder;
use pliego::pdf::{PdfFontResource, render_document_pdf};
use pliego::{
    Color, DocumentScene, FillRule, Glyph, Operation, OperationMeta, Page, Rect, Size, Stroke,
    Utf8Range,
};
use sha2::{Digest, Sha256};
use ttf_parser::Face;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
));
const FONT_ID: &str = "sha256:dejavu-sans-core-benchmark";
const PAGE_WIDTH: f64 = 816.0;
const PAGE_HEIGHT: f64 = 1056.0;

struct Case {
    name: &'static str,
    scene: DocumentScene,
    expected_pdf_sha256: &'static str,
}

fn cases() -> [Case; 4] {
    // The committed Rust constructors below are the benchmark inputs. They keep the 20- and
    // 100-page cases reviewable without generated JSON fixtures.
    let face = Face::parse(FONT, 0).unwrap();
    [
        Case {
            name: "one-page",
            scene: DocumentScene::new(Page {
                size: Size {
                    width: PAGE_WIDTH,
                    height: PAGE_HEIGHT,
                },
                operations: vec![
                    text(&face, "Pliego core", 48.0, 72.0, 24.0, ink()),
                    rule(48.0, 88.0, 720.0),
                ],
            }),
            expected_pdf_sha256: "9812cab207d111c007b78b51023c787c072af0126945eafc3b4e58b7564f25df",
        },
        Case {
            name: "invoice-table",
            scene: DocumentScene::new(table_page(&face, 1, 16)),
            expected_pdf_sha256: "6d2da2744c8bed3b4e9efda8a61893cb121837385fc8aa285a77ffe54cada5a1",
        },
        Case {
            name: "twenty-pages",
            scene: paged_scene(&face, 20),
            expected_pdf_sha256: "28cd3dfb4300845e8b12ca1276ae0854886f712616cb28bb90a0621d781a9abb",
        },
        Case {
            name: "one-hundred-pages",
            scene: paged_scene(&face, 100),
            expected_pdf_sha256: "a11ffc629a1e62dadd1e31e9b80186a709d8f4a73ecd967c2dd14d83d702fbb9",
        },
    ]
}

fn paged_scene(face: &Face<'_>, page_count: usize) -> DocumentScene {
    DocumentScene {
        schema: pliego::SCHEMA.into(),
        version: pliego::SCHEMA_VERSION,
        pages: (1..=page_count)
            .map(|page| table_page(face, page, 12))
            .collect(),
    }
}

fn table_page(face: &Face<'_>, page_number: usize, rows: usize) -> Page {
    let mut operations = vec![
        rectangle(40.0, 32.0, 736.0, 56.0, navy()),
        text(face, "PLIEGO INVOICE", 56.0, 68.0, 20.0, white()),
        text(
            face,
            &format!("PAGE {page_number:03}"),
            660.0,
            68.0,
            11.0,
            white(),
        ),
        text(face, "ITEM", 48.0, 124.0, 11.0, ink()),
        text(face, "QTY", 500.0, 124.0, 11.0, ink()),
        text(face, "AMOUNT", 620.0, 124.0, 11.0, ink()),
        rule(40.0, 136.0, 736.0),
    ];
    for row in 0..rows {
        let baseline = 166.0 + row as f64 * 42.0;
        operations.extend([
            text(
                face,
                &format!("Service line {:02}", row + 1),
                48.0,
                baseline,
                12.0,
                ink(),
            ),
            text(
                face,
                &format!("{}", row % 9 + 1),
                512.0,
                baseline,
                12.0,
                ink(),
            ),
            text(
                face,
                &format!("${}.00", 125 + row * 17),
                628.0,
                baseline,
                12.0,
                ink(),
            ),
            rule(40.0, baseline + 14.0, 736.0),
        ]);
    }
    Page {
        size: Size {
            width: PAGE_WIDTH,
            height: PAGE_HEIGHT,
        },
        operations,
    }
}

fn text(face: &Face<'_>, value: &str, x: f64, y: f64, font_size: f64, color: Color) -> Operation {
    let mut cursor = x;
    let units_per_em = f64::from(face.units_per_em());
    let glyphs = value
        .char_indices()
        .map(|(start, character)| {
            let id = face.glyph_index(character).unwrap();
            let advance = f64::from(face.glyph_hor_advance(id).unwrap()) * font_size / units_per_em;
            let glyph = Glyph {
                id: u32::from(id.0),
                x: cursor,
                y,
                advance,
                text_range: Some(Utf8Range {
                    start: u32::try_from(start).unwrap(),
                    end: u32::try_from(start + character.len_utf8()).unwrap(),
                }),
            };
            cursor += advance;
            glyph
        })
        .collect();
    Operation::Text {
        text: value.into(),
        font: FONT_ID.into(),
        font_size,
        color,
        glyphs,
        meta: OperationMeta::default(),
    }
}

fn rectangle(x: f64, y: f64, width: f64, height: f64, fill: Color) -> Operation {
    Operation::Path {
        bounds: Rect {
            x,
            y,
            width,
            height,
        },
        data: format!("M{x} {y}h{width}v{height}h-{width}Z"),
        fill: Some(fill),
        fill_rule: FillRule::NonZero,
        stroke: None,
        meta: OperationMeta::default(),
    }
}

fn rule(x: f64, y: f64, width: f64) -> Operation {
    Operation::Path {
        bounds: Rect {
            x,
            y,
            width,
            height: 1.0,
        },
        data: format!("M{x} {y}h{width}"),
        fill: None,
        fill_rule: FillRule::NonZero,
        stroke: Some(Stroke {
            color: Color {
                r: 0.78,
                g: 0.81,
                b: 0.85,
                a: 1.0,
            },
            width: 1.0,
        }),
        meta: OperationMeta::default(),
    }
}

fn ink() -> Color {
    Color {
        r: 0.04,
        g: 0.09,
        b: 0.16,
        a: 1.0,
    }
}

fn navy() -> Color {
    Color {
        r: 0.04,
        g: 0.13,
        b: 0.25,
        a: 1.0,
    }
}

fn white() -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    }
}

fn render(scene: &DocumentScene) -> Vec<u8> {
    render_document_pdf(
        scene,
        |font| {
            (font == FONT_ID).then_some(PdfFontResource {
                bytes: FONT,
                face_index: 0,
                variations: &[],
                synthetic_bold: false,
            })
        },
        |_| None,
    )
    .unwrap()
}

fn check(case: &Case) -> (usize, String) {
    case.scene.validate().unwrap();
    let first = render(&case.scene);
    let second = render(&case.scene);
    assert_eq!(
        first, second,
        "{} PDF bytes changed between runs",
        case.name
    );
    assert!(
        first.starts_with(b"%PDF-"),
        "{} has no PDF header",
        case.name
    );
    assert!(
        first.trim_ascii_end().ends_with(b"%%EOF"),
        "{} has no PDF trailer",
        case.name
    );
    assert_eq!(
        occurrences(&first, b"/MediaBox"),
        case.scene.pages.len(),
        "{} emitted the wrong page count",
        case.name
    );
    assert!(
        contains(
            &first,
            format!("/Count {}", case.scene.pages.len()).as_bytes()
        ),
        "{} has the wrong PDF page tree",
        case.name
    );
    for marker in [b"/ToUnicode".as_slice(), b"/FontFile2".as_slice()] {
        assert!(contains(&first, marker), "{} lacks {marker:?}", case.name);
    }
    let cmap = referenced_stream(&first, b"/ToUnicode");
    let expected_characters = case
        .scene
        .pages
        .iter()
        .flat_map(|page| &page.operations)
        .filter_map(|operation| match operation {
            Operation::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .flat_map(str::chars)
        .filter(|character| !character.is_whitespace())
        .collect::<BTreeSet<_>>();
    for character in expected_characters {
        assert!(character.is_ascii());
        let encoded = format!("{:04X}", u32::from(character));
        assert!(
            contains(&cmap, encoded.as_bytes()),
            "{} lost the text mapping for {character:?}",
            case.name
        );
    }
    let hash = Sha256::digest(&first)
        .iter()
        .fold(String::with_capacity(64), |mut hash, byte| {
            write!(&mut hash, "{byte:02x}").unwrap();
            hash
        });
    assert_eq!(
        hash, case.expected_pdf_sha256,
        "{} PDF hash drifted",
        case.name
    );
    (first.len(), hash)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn referenced_stream(pdf: &[u8], key: &[u8]) -> Vec<u8> {
    let key_offset = position(pdf, key).unwrap() + key.len();
    let object_number = pdf[key_offset..]
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .take_while(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    let object = [String::from_utf8(object_number).unwrap(), " 0 obj".into()].concat();
    let object_offset = position(pdf, object.as_bytes()).unwrap();
    let object_end = object_offset + position(&pdf[object_offset..], b"endobj").unwrap();
    decode_stream(&pdf[object_offset..object_end]).unwrap()
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

fn benchmark(c: &mut Criterion) {
    let cases = cases();
    for case in &cases {
        let (bytes, hash) = check(case);
        eprintln!(
            "pliego-core case={} pages={} pdf_bytes={bytes} pdf_sha256={hash}",
            case.name,
            case.scene.pages.len(),
        );
    }
    eprintln!(
        "pliego-core allocations=N/A rss=N/A reason=no-existing-in-process-measurement-facility"
    );

    let mut group = c.benchmark_group("document_scene_pdf");
    for case in cases {
        group.throughput(Throughput::Elements(case.scene.pages.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(case.name),
            &case.scene,
            |b, scene| {
                b.iter(|| std::hint::black_box(render(std::hint::black_box(scene))));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
