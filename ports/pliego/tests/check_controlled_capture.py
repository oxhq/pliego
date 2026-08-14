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
CONTROLLED_CANVAS_ID = "controlled-capture-canvas"
CONTROLLED_CANVAS_SIZE = (48, 24)
CONTROLLED_CANVAS_BACKGROUND_RGBA = (12, 180, 96, 255)
CONTROLLED_CANVAS_INSET_RGBA = (244, 196, 48, 255)
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
SUCCESS_ARTIFACTS = (
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
)


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


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def write_proof_manifest(
    output: Path,
    binary: Path,
    fixture: Path,
    active_clip_fixture: Path,
    font: Path,
    materialized_inputs: tuple[Path, ...],
    comparisons: tuple[tuple[bytes, ...], ...],
    open_ended_failure: dict[str, Any],
    active_clip_failure: dict[str, Any],
) -> None:
    names = (
        "artifacts/render.png",
        "normalized-layout.json",
        "artifacts/scene.json",
        "document.pdf",
    )
    require(len(comparisons) == 2, "controlled proof requires two successful fresh processes")
    require(len(materialized_inputs) == len(comparisons), "controlled proof input/run counts differ")
    comparison_hashes = []
    for comparison in comparisons:
        require(len(comparison) == len(names), "controlled comparison has an unexpected shape")
        comparison_hashes.append({name: sha256_bytes(value) for name, value in zip(names, comparison, strict=True)})
    require(
        comparison_hashes[0] == comparison_hashes[1],
        "successful controlled runs do not have identical comparison hashes",
    )
    for name, value in zip(names, comparisons[0], strict=True):
        retained = output / name
        require(retained.is_file(), f"controlled proof did not retain {name}")
        require(
            sha256_file(retained) == sha256_bytes(value),
            f"retained controlled proof changed {name}",
        )
    require(open_ended_failure.get("exit_code") not in (None, 0), repr(open_ended_failure))
    require(open_ended_failure.get("failure_code") == "SETTLEMENT_FAILED", repr(open_ended_failure))
    require("[OpenEndedSource]" in str(open_ended_failure.get("failure_message")), repr(open_ended_failure))
    require(open_ended_failure.get("readiness_status") == "failed", repr(open_ended_failure))
    require(open_ended_failure.get("readiness_error_code") == "SETTLEMENT_FAILED", repr(open_ended_failure))
    require(open_ended_failure.get("success_artifacts_absent") == list(SUCCESS_ARTIFACTS), repr(open_ended_failure))
    failure_hashes = open_ended_failure.get("evidence_sha256")
    require(
        isinstance(failure_hashes, dict) and set(failure_hashes) == {"failure.json", "readiness.json"},
        repr(open_ended_failure),
    )
    for name, expected_hash in failure_hashes.items():
        retained = output / "open-ended-failure" / name
        require(retained.is_file(), f"controlled proof did not retain open-ended {name}")
        require(sha256_file(retained) == expected_hash, f"retained open-ended proof changed {name}")
    require(active_clip_failure.get("exit_code") not in (None, 0), repr(active_clip_failure))
    require(active_clip_failure.get("failure_code") == "SETTLEMENT_FAILED", repr(active_clip_failure))
    require("[UnsupportedSource]" in str(active_clip_failure.get("failure_message")), repr(active_clip_failure))
    require(active_clip_failure.get("readiness_status") == "failed", repr(active_clip_failure))
    require(active_clip_failure.get("readiness_error_code") == "SETTLEMENT_FAILED", repr(active_clip_failure))
    require(active_clip_failure.get("success_artifacts_absent") == list(SUCCESS_ARTIFACTS), repr(active_clip_failure))
    active_clip_hashes = active_clip_failure.get("evidence_sha256")
    require(
        isinstance(active_clip_hashes, dict) and set(active_clip_hashes) == {"failure.json", "readiness.json"},
        repr(active_clip_failure),
    )
    for name, expected_hash in active_clip_hashes.items():
        retained = output / "active-clip-failure" / name
        require(retained.is_file(), f"controlled proof did not retain active-clip {name}")
        require(sha256_file(retained) == expected_hash, f"retained active-clip proof changed {name}")
    manifest = {
        "schema": "pliego.controlled-capture-proof",
        "version": 1,
        "binary_sha256": sha256_file(binary),
        "fixture_template_sha256": sha256_file(fixture),
        "active_clip_fixture_sha256": sha256_file(active_clip_fixture),
        "font_sha256": sha256_file(font),
        "successful_runs": [
            {
                "process": index,
                "materialized_input_sha256": sha256_file(materialized_input),
                "comparison_sha256": hashes,
            }
            for index, (materialized_input, hashes) in enumerate(
                zip(materialized_inputs, comparison_hashes, strict=True),
                start=1,
            )
        ],
        "open_ended_failure": open_ended_failure,
        "active_clip_failure": active_clip_failure,
    }
    (output / "proof.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


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
    epochs = (
        readiness.get("script_rendering_epoch"),
        readiness.get("layout_paint_epoch"),
        readiness.get("paint_presented_epoch"),
    )
    require(all(type(epoch) is int and epoch >= 0 for epoch in epochs), repr(readiness))
    require(epochs[0] == epochs[1] == epochs[2], repr(readiness))

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
    layout_canvas_keys = [
        fragment.get("canvas_image_key")
        for fragment in fragments
        if isinstance(fragment, dict) and isinstance(fragment.get("canvas_image_key"), dict)
    ]
    require(
        len(layout_canvas_keys) == 1,
        f"layout did not retain exactly one generation-bound Canvas key: {layout_canvas_keys!r}",
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
    scene_images = [
        operation for operation in operations if isinstance(operation, dict) and operation.get("type") == "image"
    ]
    require(
        len(scene_images) >= 1,
        f"scene did not retain a Canvas image operation: {operations!r}",
    )
    require(
        all(
            isinstance(operation.get("resource"), str) and operation["resource"].startswith("sha256:")
            for operation in scene_images
        ),
        f"scene Canvas image operation has no content-addressed resource: {scene_images!r}",
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
    captured_canvases = capture.get("canvases")
    require(
        isinstance(captured_canvases, list) and len(captured_canvases) == 1,
        f"scene report did not retain exactly one controlled Canvas: {captured_canvases!r}",
    )
    canvas_diagnostics = captured_canvases[0].get("diagnostics")
    require(isinstance(canvas_diagnostics, dict), repr(captured_canvases[0]))
    require(
        canvas_diagnostics.get("rasterized_area_px") == CONTROLLED_CANVAS_SIZE[0] * CONTROLLED_CANVAS_SIZE[1],
        repr(canvas_diagnostics),
    )
    canvas_fallbacks = canvas_diagnostics.get("fallbacks")
    require(
        isinstance(canvas_fallbacks, list) and len(canvas_fallbacks) == 1,
        repr(canvas_diagnostics),
    )
    canvas_resource = canvas_fallbacks[0].get("resource")
    require(
        isinstance(canvas_resource, str) and canvas_resource in {operation["resource"] for operation in scene_images},
        f"Canvas diagnostics fallback is not bound to a scene image: {canvas_fallbacks!r}",
    )
    canvas_png_path = root / "artifacts/resources" / canvas_resource.removeprefix("sha256:")
    require(canvas_png_path.is_file(), f"missing controlled Canvas resource: {canvas_png_path}")
    canvas_png = canvas_png_path.read_bytes()
    require(
        png_dimensions(canvas_png) == CONTROLLED_CANVAS_SIZE,
        f"controlled Canvas resource dimensions differ from {CONTROLLED_CANVAS_SIZE!r}",
    )
    require(
        rgba_pixel(canvas_png, 0, 0) == CONTROLLED_CANVAS_BACKGROUND_RGBA,
        "controlled Canvas resource lost its background pixels",
    )
    require(
        rgba_pixel(canvas_png, 10, 8) == CONTROLLED_CANVAS_INSET_RGBA,
        "controlled Canvas resource lost its inset pixels",
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
    timeout_message = "controlled-production-ready:946684800005:5"
    require(
        console.count(timeout_message) == 1,
        "console evidence did not retain exactly one 5 ms controlled-time callback",
    )
    paint_observer_message = "controlled-paint-observed:first-contentful-paint:20:0:20"
    require(
        console.count(paint_observer_message) == 1,
        "console evidence did not retain exactly one deterministic paint-observer callback",
    )
    require(
        console.count("controlled-production-frame") == 1,
        "console evidence did not retain exactly one requestAnimationFrame callback",
    )
    return stable_png, layout_projection, scene, pdf


def run_open_ended_failure(binary: Path, root: Path) -> dict[str, Any]:
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
    for relative in SUCCESS_ARTIFACTS:
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
    return {
        "exit_code": result.returncode,
        "failure_code": failure.get("error", {}).get("code"),
        "failure_message": failure.get("error", {}).get("message"),
        "readiness_status": readiness.get("status"),
        "readiness_error_code": readiness.get("error", {}).get("code"),
        "success_artifacts_absent": list(SUCCESS_ARTIFACTS),
        "evidence_sha256": {
            "failure.json": sha256_file(failure_path),
            "readiness.json": sha256_file(root / "artifacts/readiness.json"),
        },
    }


def run_active_clip_failure(binary: Path, fixture: Path, root: Path) -> dict[str, Any]:
    shutil.copy2(fixture, root / "input.html")
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
    require(result.returncode != 0, "active-clip Canvas source unexpectedly published")
    for relative in SUCCESS_ARTIFACTS:
        require(
            not (root / relative).exists(),
            f"active-clip Canvas failure exposed success artifact {relative}",
        )
    failure_path = root / "artifacts/failure.json"
    require(failure_path.is_file(), "active-clip Canvas failure retained no typed evidence")
    failure = read_json(failure_path)
    require(failure.get("error", {}).get("code") == "SETTLEMENT_FAILED", repr(failure))
    require("UnsupportedSource" in str(failure.get("error", {}).get("message")), repr(failure))
    readiness_path = root / "artifacts/readiness.json"
    readiness = read_json(readiness_path)
    require(readiness.get("status") == "failed", repr(readiness))
    require(readiness.get("error", {}).get("code") == "SETTLEMENT_FAILED", repr(readiness))
    return {
        "exit_code": result.returncode,
        "failure_code": failure.get("error", {}).get("code"),
        "failure_message": failure.get("error", {}).get("message"),
        "readiness_status": readiness.get("status"),
        "readiness_error_code": readiness.get("error", {}).get("code"),
        "success_artifacts_absent": list(SUCCESS_ARTIFACTS),
        "evidence_sha256": {
            "failure.json": sha256_file(failure_path),
            "readiness.json": sha256_file(readiness_path),
        },
    }


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
        require(
            f'id="{CONTROLLED_CANVAS_ID}"' in source,
            "materialized fixture has no visible controlled Canvas",
        )
        require(
            'getContext("2d")' in source
            and "getImageData(0, 0, controlledCanvas.width, controlledCanvas.height)" in source,
            "materialized fixture has no retained-supported full-surface Canvas 2D transcript",
        )
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
        active_clip_fixture = fixture.parents[1] / "controlled-canvas-active-clip/index.html"
        active_clip_source = active_clip_fixture.read_text(encoding="utf-8")
        clip = active_clip_source.index("context.clip()")
        full_readback = active_clip_source.index("context.getImageData(0, 0, canvas.width, canvas.height)")
        later_fill = active_clip_source.rindex("context.fillRect")
        require(
            clip < full_readback < later_fill,
            "active-clip fixture must draw after a full readback without popping its clip",
        )
        proof_root = Path(temporary)
        (proof_root / "artifacts").mkdir()
        (proof_root / "artifacts/render.png").write_bytes(b"png")
        (proof_root / "normalized-layout.json").write_bytes(b"layout")
        (proof_root / "artifacts/scene.json").write_bytes(b"scene")
        (proof_root / "document.pdf").write_bytes(b"pdf")
        failure_root = proof_root / "open-ended-failure"
        failure_root.mkdir()
        (failure_root / "failure.json").write_bytes(b"failure")
        (failure_root / "readiness.json").write_bytes(b"readiness")
        active_clip_failure_root = proof_root / "active-clip-failure"
        active_clip_failure_root.mkdir()
        (active_clip_failure_root / "failure.json").write_bytes(b"active clip failure")
        (active_clip_failure_root / "readiness.json").write_bytes(b"active clip readiness")
        write_proof_manifest(
            proof_root,
            materialized,
            fixture,
            active_clip_fixture,
            font,
            (materialized, materialized),
            (
                (b"png", b"layout", b"scene", b"pdf"),
                (b"png", b"layout", b"scene", b"pdf"),
            ),
            {
                "exit_code": 1,
                "failure_code": "SETTLEMENT_FAILED",
                "failure_message": "capture preparation failed: [OpenEndedSource]",
                "readiness_status": "failed",
                "readiness_error_code": "SETTLEMENT_FAILED",
                "success_artifacts_absent": list(SUCCESS_ARTIFACTS),
                "evidence_sha256": {
                    "failure.json": sha256_bytes(b"failure"),
                    "readiness.json": sha256_bytes(b"readiness"),
                },
            },
            {
                "exit_code": 1,
                "failure_code": "SETTLEMENT_FAILED",
                "failure_message": "capture preparation failed: [UnsupportedSource]",
                "readiness_status": "failed",
                "readiness_error_code": "SETTLEMENT_FAILED",
                "success_artifacts_absent": list(SUCCESS_ARTIFACTS),
                "evidence_sha256": {
                    "failure.json": sha256_bytes(b"active clip failure"),
                    "readiness.json": sha256_bytes(b"active clip readiness"),
                },
            },
        )
        proof = read_json(proof_root / "proof.json")
        require(proof.get("schema") == "pliego.controlled-capture-proof", repr(proof))
        require(proof.get("version") == 1, repr(proof))
        runs = proof.get("successful_runs")
        require(isinstance(runs, list) and len(runs) == 2, repr(proof))
        require(runs[0] == {**runs[1], "process": 1}, repr(proof))
        require(runs[1].get("process") == 2, repr(proof))
        hashes = runs[0].get("comparison_sha256")
        require(
            isinstance(hashes, dict)
            and set(hashes)
            == {
                "artifacts/render.png",
                "artifacts/scene.json",
                "document.pdf",
                "normalized-layout.json",
            },
            repr(proof),
        )
        require(
            hashes
            == {
                "artifacts/render.png": "8f8cbb7dcf46e0bc7d53265749a6c17d116093a6ba95e442764060c76fd4a86c",
                "normalized-layout.json": "1dc5ae5b68174891b6aa9850aa05ee0d9ae8a20468d9517259951a2dd9e9c0f0",
                "artifacts/scene.json": "4f48f7207cebf92638debb5694e4a8677350cf88a9e98e73c6c466b6b1d43890",
                "document.pdf": "c35b21d6ca39aa7cc3b79a705d989f1a6e88b99ab43988d74048799e3db926a3",
            },
            repr(proof),
        )
        require(
            proof.get("open_ended_failure", {}).get("failure_message")
            == "capture preparation failed: [OpenEndedSource]",
            repr(proof),
        )
        require(
            proof.get("active_clip_fixture_sha256") == sha256_file(active_clip_fixture),
            repr(proof),
        )
        require(
            proof.get("active_clip_failure", {}).get("failure_message")
            == "capture preparation failed: [UnsupportedSource]",
            repr(proof),
        )


def main() -> int:
    test_root = Path(__file__).resolve().parent
    repository = Path(__file__).resolve().parents[3]
    fixture = test_root / "fixtures/controlled-production-capture/index.html"
    active_clip_fixture = test_root / "fixtures/controlled-canvas-active-clip/index.html"
    font = repository / "components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
    if sys.argv[1:] == ["--self-test"]:
        require(fixture.is_file(), f"missing fixture: {fixture}")
        require(active_clip_fixture.is_file(), f"missing fixture: {active_clip_fixture}")
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
    require(active_clip_fixture.is_file(), f"missing fixture: {active_clip_fixture}")
    require(font.is_file(), f"missing controlled-capture font: {font}")
    require(not output.exists(), f"output already exists: {output}")
    output.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="pliego-controlled-capture-") as temporary:
        temporary_root = Path(temporary)
        first_root = temporary_root / "first"
        second_root = temporary_root / "second"
        failure_root = temporary_root / "failure"
        active_clip_failure_root = temporary_root / "active-clip-failure"
        first_root.mkdir()
        second_root.mkdir()
        failure_root.mkdir()
        active_clip_failure_root.mkdir()
        first = run_success(binary, fixture, font, first_root)
        second = run_success(binary, fixture, font, second_root)
        require(
            first == second,
            "identical controlled captures changed pixels, semantic layout, scene, or PDF",
        )
        open_ended_failure = run_open_ended_failure(binary, failure_root)
        active_clip_failure = run_active_clip_failure(
            binary,
            active_clip_fixture,
            active_clip_failure_root,
        )
        shutil.copytree(first_root / "artifacts", output / "artifacts")
        shutil.copy2(first_root / "document.pdf", output / "document.pdf")
        (output / "normalized-layout.json").write_bytes(first[1])
        failure_output = output / "open-ended-failure"
        failure_output.mkdir()
        shutil.copy2(failure_root / "artifacts/failure.json", failure_output / "failure.json")
        shutil.copy2(failure_root / "artifacts/readiness.json", failure_output / "readiness.json")
        active_clip_failure_output = output / "active-clip-failure"
        active_clip_failure_output.mkdir()
        shutil.copy2(
            active_clip_failure_root / "artifacts/failure.json",
            active_clip_failure_output / "failure.json",
        )
        shutil.copy2(
            active_clip_failure_root / "artifacts/readiness.json",
            active_clip_failure_output / "readiness.json",
        )
        write_proof_manifest(
            output,
            binary,
            fixture,
            active_clip_fixture,
            font,
            (first_root / "input.html", second_root / "input.html"),
            (first, second),
            open_ended_failure,
            active_clip_failure,
        )
    print(f"controlled capture check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
