#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path
from typing import Any

MARKER = "PLIEGO_FCP_20"
POST5_MARKER = "PLIEGO_POST5MS_7C4E"
MARKER_RGBA = (197, 48, 80, 255)
STABLE_FRAME_SIZE = (200, 160)
FONT_PLACEHOLDER = "__PLIEGO_CONTROLLED_CAPTURE_FONT_BASE64__"
PINNED_FONT_SHA256 = "7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954"
NONDETERMINISTIC_LAYOUT_KEYS = {
    "canvas_image_key",
    "clip_id",
    "fragment_id",
    "paint_fragment_id",
    "spatial_node_id",
    "tag_id",
}


def fail(message: str, code: int = 1) -> None:
    print(f"controlled capture check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no terminal JSON")
    value = json.loads(lines[-1])
    require(isinstance(value, dict), "terminal JSON is not an object")
    return value


def artifact(summary: dict[str, Any], key: str, expected: Path) -> Path:
    value = summary.get(key)
    require(isinstance(value, str), f"summary has no {key}")
    path = Path(value)
    require(path.is_file(), f"missing {key}: {path}")
    require(
        path.resolve(strict=True) == expected.resolve(strict=True),
        f"summary {key} escaped its requested path: {path} != {expected}",
    )
    return path


def artifact_directory(summary: dict[str, Any], key: str, expected: Path) -> Path:
    value = summary.get(key)
    require(isinstance(value, str), f"summary has no {key}")
    path = Path(value)
    require(path.is_dir(), f"missing {key}: {path}")
    require(
        path.resolve(strict=True) == expected.resolve(strict=True),
        f"summary {key} escaped its requested path: {path} != {expected}",
    )
    return path


def read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"{path} is not a JSON object")
    return value


def normalized_layout(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: normalized_layout(item) for key, item in value.items() if key not in NONDETERMINISTIC_LAYOUT_KEYS}
    if isinstance(value, list):
        return [normalized_layout(item) for item in value]
    return value


def materialize_fixture(template: Path, font: Path, output: Path) -> None:
    font_bytes = font.read_bytes()
    require(
        hashlib.sha256(font_bytes).hexdigest() == PINNED_FONT_SHA256,
        f"pinned controlled-capture font bytes changed: {font}",
    )
    source = template.read_text(encoding="utf-8")
    require(source.count(FONT_PLACEHOLDER) == 1, "controlled-capture font placeholder is missing")
    output.write_text(
        source.replace(FONT_PLACEHOLDER, base64.b64encode(font_bytes).decode("ascii")),
        encoding="utf-8",
    )


def paeth(left: int, above: int, upper_left: int) -> int:
    estimate = left + above - upper_left
    left_distance = abs(estimate - left)
    above_distance = abs(estimate - above)
    upper_left_distance = abs(estimate - upper_left)
    if left_distance <= above_distance and left_distance <= upper_left_distance:
        return left
    if above_distance <= upper_left_distance:
        return above
    return upper_left


def png_dimensions(png: bytes) -> tuple[int, int]:
    require(png.startswith(b"\x89PNG\r\n\x1a\n"), "stable frame is not a PNG")
    require(len(png) >= 33, "stable frame has no complete PNG header")
    length = struct.unpack(">I", png[8:12])[0]
    require(length == 13 and png[12:16] == b"IHDR", "stable frame has no leading IHDR")
    width, height = struct.unpack(">II", png[16:24])
    require(width > 0 and height > 0, "stable frame has invalid dimensions")
    return width, height


def rgba_pixel(png: bytes, x: int, y: int) -> tuple[int, int, int, int]:
    expected_width, expected_height = png_dimensions(png)
    offset = 8
    width = height = 0
    compressed = bytearray()
    while offset < len(png):
        length = struct.unpack(">I", png[offset : offset + 4])[0]
        kind = png[offset + 4 : offset + 8]
        payload = png[offset + 8 : offset + 8 + length]
        offset += 12 + length
        if kind == b"IHDR":
            width, height, depth, color, compression, filtering, interlace = struct.unpack(">IIBBBBB", payload)
            require(
                (depth, color, compression, filtering, interlace) == (8, 6, 0, 0, 0),
                "stable frame is not non-interlaced RGBA8",
            )
        elif kind == b"IDAT":
            compressed.extend(payload)
        elif kind == b"IEND":
            break
    require(
        (width, height) == (expected_width, expected_height),
        "stable frame PNG dimensions changed while decoding",
    )
    require(0 <= x < width and 0 <= y < height, "sample pixel is outside the stable frame")
    encoded = zlib.decompress(bytes(compressed))
    stride = width * 4
    require(len(encoded) == height * (stride + 1), "unexpected PNG scanline length")
    rows: list[bytearray] = []
    cursor = 0
    for _ in range(height):
        filter_kind = encoded[cursor]
        source = encoded[cursor + 1 : cursor + 1 + stride]
        cursor += stride + 1
        row = bytearray(stride)
        previous = rows[-1] if rows else bytearray(stride)
        for index, byte in enumerate(source):
            left = row[index - 4] if index >= 4 else 0
            above = previous[index]
            upper_left = previous[index - 4] if index >= 4 else 0
            predictor = {
                0: 0,
                1: left,
                2: above,
                3: (left + above) // 2,
                4: paeth(left, above, upper_left),
            }.get(filter_kind)
            require(predictor is not None, f"unsupported PNG filter {filter_kind}")
            row[index] = (byte + predictor) & 0xFF
        rows.append(row)
    start = x * 4
    return tuple(rows[y][start : start + 4])  # type: ignore[return-value]


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the controlled capture gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-nopgbrk", str(pdf), "-"],
        capture_output=True,
        timeout=20,
        check=False,
    )
    require(
        result.returncode == 0,
        f"pdftotext failed: {result.stderr.decode(errors='replace')}",
    )
    return result.stdout.decode("utf-8-sig")


def run_success(binary: Path, fixture: Path, font: Path, root: Path) -> tuple[bytes, ...]:
    materialize_fixture(fixture, font, root / "input.html")
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(root), "TMP": str(root), "TEMP": str(root)})
    result = subprocess.run(
        [
            str(binary),
            "render-controlled",
            "input.html",
            "--output",
            str(root / "document.pdf"),
            "--artifacts",
            str(root / "artifacts"),
            "--page-size",
            "200x160",
            "--page-margins",
            "0,0,0,0",
        ],
        cwd=root,
        env=environment,
        capture_output=True,
        text=True,
        timeout=75,
        check=False,
    )
    if result.returncode != 0:
        fail(result.stderr[-5000:] or result.stdout[-5000:])
    summary = final_json(result)
    require(summary.get("status") == "rendered", repr(summary))
    artifact_directory(summary, "artifacts", root / "artifacts")
    artifact(summary, "bundle", root / "artifacts/bundle.json")
    artifact(summary, "environment_artifact", root / "artifacts/environment.json")
    artifact(summary, "pages_artifact", root / "artifacts/pages.json")
    artifact(summary, "fonts_artifact", root / "artifacts/fonts.json")
    expected_preview = root / "artifacts/scene-preview.png"
    artifact(summary, "scene_preview", expected_preview)
    scene_previews = summary.get("scene_previews")
    require(isinstance(scene_previews, list), "summary has no scene_previews")
    require(scene_previews == [str(expected_preview)], repr(scene_previews))
    require(summary.get("document_pdf_status") == "rendered", repr(summary))
    require(summary.get("pdf_structure_status") == "rendered", repr(summary))
    scene_summary = summary.get("scene")
    require(isinstance(scene_summary, dict), "summary has no scene status")
    require(scene_summary.get("capture_status") == "complete", repr(scene_summary))
    require(scene_summary.get("capture_code") is None, repr(scene_summary))
    require(scene_summary.get("preview_status") == "rendered", repr(scene_summary))
    readiness = summary.get("readiness")
    require(isinstance(readiness, dict), repr(summary))
    require(readiness.get("source") == "controlled-generation-capture", repr(readiness))
    require(readiness.get("document_time_ns") == "40000000", repr(readiness))
    require(
        readiness.get("script_rendering_epoch")
        == readiness.get("layout_paint_epoch")
        == readiness.get("paint_presented_epoch"),
        repr(readiness),
    )

    layout_path = artifact(summary, "layout_debug", root / "artifacts/layout-debug.json")
    layout = read_json(layout_path)
    layout_projection = json.dumps(
        normalized_layout(layout),
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    fragments = layout.get("fragments")
    require(isinstance(fragments, list), "layout snapshot has no fragments")
    layout_marker_runs = [
        fragment.get("text_run", {}).get("text")
        for fragment in fragments
        if isinstance(fragment, dict) and isinstance(fragment.get("text_run"), dict)
    ]
    require(
        layout_marker_runs.count(MARKER) == 1,
        f"layout did not retain exactly one paint-observer marker text run: {layout_marker_runs!r}",
    )

    stable_png = artifact(summary, "rendered_image", root / "artifacts/render.png").read_bytes()
    require(
        png_dimensions(stable_png) == STABLE_FRAME_SIZE,
        f"stable frame dimensions differ from {STABLE_FRAME_SIZE!r}",
    )
    require(rgba_pixel(stable_png, 2, 2) == (36, 104, 172, 255), "stable pixels predate mutation")
    require(
        rgba_pixel(stable_png, 13, 13) == MARKER_RGBA,
        "stable pixels do not contain the post-5ms marker rectangle",
    )

    scene_path = artifact(summary, "scene_artifact", root / "artifacts/scene.json")
    scene = scene_path.read_bytes()
    scene_object = read_json(scene_path)
    pages = scene_object.get("pages")
    require(isinstance(pages, list) and len(pages) == 1, "scene does not contain exactly one page")
    operations = pages[0].get("operations") if isinstance(pages[0], dict) else None
    require(isinstance(operations, list), "scene page has no operations")
    scene_marker_runs = [
        operation.get("text")
        for operation in operations
        if isinstance(operation, dict) and operation.get("type") == "text"
    ]
    require(
        scene_marker_runs.count(MARKER) == 1,
        f"scene did not retain exactly one paint-observer marker text operation: {scene_marker_runs!r}",
    )

    report = read_json(artifact(summary, "scene_report", root / "artifacts/scene-report.json"))
    capture = report.get("capture")
    require(isinstance(capture, dict), "scene report has no capture status")
    require(
        capture.get("status") == "complete"
        and capture.get("code") is None
        and capture.get("unsupported_events") == []
        and capture.get("text_mapping_gaps") == [],
        repr(capture),
    )
    preview = report.get("preview")
    require(isinstance(preview, dict) and preview.get("status") == "rendered", repr(preview))
    report_pdf = report.get("document_pdf")
    require(
        isinstance(report_pdf, dict) and report_pdf.get("status") == "rendered" and report_pdf.get("error") is None,
        repr(report_pdf),
    )
    report_pdf_structure = report.get("pdf_structure")
    require(
        isinstance(report_pdf_structure, dict)
        and report_pdf_structure.get("status") == "rendered"
        and report_pdf_structure.get("error") is None,
        repr(report_pdf_structure),
    )

    pdf_structure = read_json(artifact(summary, "pdf_structure", root / "artifacts/pdf-structure.json"))
    structure_pages = pdf_structure.get("pages")
    require(
        pdf_structure.get("schema") == "pliego.pdf-structure"
        and pdf_structure.get("version") == 1
        and pdf_structure.get("backend") == "krilla"
        and pdf_structure.get("page_count") == 1
        and isinstance(structure_pages, list)
        and len(structure_pages) == 1,
        repr(pdf_structure),
    )
    structure_page = structure_pages[0]
    require(isinstance(structure_page, dict), "PDF structure page is not an object")
    require(
        MARKER in str(structure_page.get("expected_extracted_unicode", "")),
        "PDF structure did not join the marker text to the scene",
    )

    pdf_path = artifact(summary, "document_pdf", root / "document.pdf")
    pdf = pdf_path.read_bytes()
    require(pdf.startswith(b"%PDF-"), "published document is not a PDF")
    extracted_pdf = "".join(extract_pdf_text(pdf_path).split())
    require(
        extracted_pdf.count(MARKER) == 1,
        f"published PDF did not expose exactly one paint-observer marker: {extracted_pdf!r}",
    )
    console = (root / "artifacts/console.jsonl").read_text(encoding="utf-8")
    require(
        "controlled-production-ready:946684800005:5" in console,
        "console evidence did not retain the 5 ms controlled-time callback",
    )
    paint_observer_message = "controlled-paint-observed:first-contentful-paint:20:0:20"
    require(
        console.count(paint_observer_message) == 1,
        "console evidence did not retain exactly one deterministic paint-observer callback",
    )
    return stable_png, layout_projection, scene, pdf


def run_open_ended_failure(binary: Path, root: Path) -> None:
    (root / "input.html").write_text("<!doctype html><script>setInterval(()=>{},1)</script>", encoding="utf-8")
    result = subprocess.run(
        [
            str(binary),
            "render-controlled",
            "input.html",
            "--output",
            str(root / "document.pdf"),
            "--artifacts",
            str(root / "artifacts"),
        ],
        cwd=root,
        capture_output=True,
        text=True,
        timeout=75,
        check=False,
    )
    require(result.returncode != 0, "open-ended source unexpectedly published")
    for relative in (
        "document.pdf",
        "artifacts/bundle.json",
        "artifacts/document.pdf",
        "artifacts/render.png",
        "artifacts/layout-debug.json",
        "artifacts/fonts.json",
        "artifacts/pages.json",
        "artifacts/scene.json",
        "artifacts/scene-report.json",
        "artifacts/scene-preview.png",
        "artifacts/pdf-structure.json",
        "artifacts/pages",
        "artifacts/publication/outcome.json",
        "artifacts/publication/prepared.json",
        "artifacts/publication/committed.json",
    ):
        require(
            not (root / relative).exists(),
            f"failed capture exposed success artifact {relative}",
        )
    failure_path = root / "artifacts/failure.json"
    require(failure_path.is_file(), "failed capture did not retain typed failure evidence")
    failure = read_json(failure_path)
    require(failure.get("error", {}).get("code") == "SETTLEMENT_FAILED", repr(failure))
    require("OpenEndedSource" in str(failure.get("error", {}).get("message")), repr(failure))
    readiness = read_json(root / "artifacts/readiness.json")
    require(readiness.get("status") == "failed", repr(readiness))
    require(readiness.get("error", {}).get("code") == "SETTLEMENT_FAILED", repr(readiness))


def self_test(fixture: Path, font: Path) -> None:
    # 1x1 RGBA8, filter 0, pixel 36/104/172/255.
    raw = b"\x00\x24\x68\xac\xff"
    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)

    def chunk(kind: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + kind
            + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
    require(png_dimensions(png) == (1, 1), "PNG dimension oracle failed")
    require(rgba_pixel(png, 0, 0) == (36, 104, 172, 255), "PNG oracle failed")
    with tempfile.TemporaryDirectory(prefix="pliego-controlled-capture-self-test-") as temporary:
        materialized = Path(temporary) / "input.html"
        materialize_fixture(fixture, font, materialized)
        source = materialized.read_text(encoding="utf-8")
        require(FONT_PLACEHOLDER not in source, "font placeholder survived materialization")
        require(POST5_MARKER in source, "materialized fixture has no post-5ms marker")
        require(
            "PLIEGO_FCP_${fcp.startTime}" in source,
            "materialized fixture has no paint-observer marker",
        )
        require("PLIEGO_FONT_PREWARM" in source, "materialized fixture does not prewarm its font")
        projection = normalized_layout(
            {
                "tag_id": 1,
                "fragments": [
                    {
                        "paint_fragment_id": 2,
                        "text_run": {"text": MARKER},
                    }
                ],
                "paint_epoch": 3,
            }
        )
        require("tag_id" not in projection, "layout projection retained a process-local tag")
        require(
            "paint_fragment_id" not in projection["fragments"][0],
            "layout projection retained a capture-local fragment ID",
        )
        require(
            projection["fragments"][0]["text_run"]["text"] == MARKER,
            "layout projection removed semantic marker evidence",
        )


def main() -> int:
    test_root = Path(__file__).resolve().parent
    repository = Path(__file__).resolve().parents[3]
    fixture = test_root / "fixtures/controlled-production-capture/index.html"
    font = repository / "components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
    if sys.argv[1:] == ["--self-test"]:
        require(fixture.is_file(), f"missing fixture: {fixture}")
        require(font.is_file(), f"missing controlled-capture font: {font}")
        self_test(fixture, font)
        print("controlled capture checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)
    binary = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2]).resolve()
    require(binary.is_file(), f"missing Pliego binary: {binary}")
    require(fixture.is_file(), f"missing fixture: {fixture}")
    require(font.is_file(), f"missing controlled-capture font: {font}")
    require(not output.exists(), f"output already exists: {output}")
    output.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="pliego-controlled-capture-") as temporary:
        temporary_root = Path(temporary)
        first_root = temporary_root / "first"
        second_root = temporary_root / "second"
        failure_root = temporary_root / "failure"
        first_root.mkdir()
        second_root.mkdir()
        failure_root.mkdir()
        first = run_success(binary, fixture, font, first_root)
        second = run_success(binary, fixture, font, second_root)
        require(
            first == second,
            "identical controlled captures changed pixels, semantic layout, scene, or PDF",
        )
        run_open_ended_failure(binary, failure_root)
        shutil.copytree(first_root / "artifacts", output / "artifacts")
        shutil.copy2(first_root / "document.pdf", output / "document.pdf")
    print(f"controlled capture check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
