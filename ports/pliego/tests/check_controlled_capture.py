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
SYNTHETIC_READINESS_KEYS = {
    "source",
    "document_time_ns",
    "script_rendering_epoch",
    "layout_paint_epoch",
    "paint_presented_epoch",
}
OPEN_ENDED_SOURCE = "<!doctype html><script>window.pliego.defer();setInterval(()=>{},1)</script>"
MARKER_RGBA = (197, 48, 80, 255)
CONTROLLED_CANVAS_ID = "controlled-capture-canvas"
CONTROLLED_CANVAS_SIZE = (48, 24)
CONTROLLED_READINESS_PAYLOAD = {
    "fixture": "controlled-production-capture",
    "phase": "post-5ms",
    "marker": POST5_MARKER,
    "canvasWidth": CONTROLLED_CANVAS_SIZE[0],
    "canvasHeight": CONTROLLED_CANVAS_SIZE[1],
    "readbackBytes": CONTROLLED_CANVAS_SIZE[0] * CONTROLLED_CANVAS_SIZE[1] * 4,
}
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


def exact_ready_snapshot(
    summary: dict[str, Any],
    artifacts: Path,
    expected_payload: dict[str, Any],
) -> dict[str, Any]:
    render_id = summary.get("render_id")
    require(isinstance(render_id, str) and render_id, repr(summary))
    require(summary.get("readiness") == expected_payload, repr(summary.get("readiness")))
    require(SYNTHETIC_READINESS_KEYS.isdisjoint(expected_payload), repr(expected_payload))
    snapshot = read_json(artifacts / "readiness.json")
    require(
        snapshot
        == {
            "status": "ready",
            "font_status": "loaded",
            "payload": expected_payload,
            "render_id": render_id,
        },
        repr(snapshot),
    )
    return snapshot


def validate_failure_proof(
    output: Path,
    directory: str,
    evidence: dict[str, Any],
    *,
    expected_code: str,
    message_fragment: str,
    expected_input_sha256: str,
    readiness_payload: dict[str, Any] | None = None,
) -> None:
    require(evidence.get("exit_code") not in (None, 0), repr(evidence))
    require(evidence.get("failure_code") == expected_code, repr(evidence))
    require(message_fragment in str(evidence.get("failure_message")), repr(evidence))
    render_id = evidence.get("render_id")
    require(isinstance(render_id, str) and render_id, repr(evidence))
    require(evidence.get("input_sha256") == expected_input_sha256, repr(evidence))
    readiness = evidence.get("readiness_snapshot")
    if readiness_payload is None:
        require(
            readiness
            == {
                "status": "failed",
                "render_id": render_id,
                "error": {
                    "code": expected_code,
                    "message": evidence.get("failure_message"),
                },
            },
            repr(evidence),
        )
    else:
        require(
            readiness
            == {
                "status": "ready",
                "font_status": "loaded",
                "payload": readiness_payload,
                "render_id": render_id,
            },
            repr(evidence),
        )
    require(evidence.get("success_artifacts_absent") == list(SUCCESS_ARTIFACTS), repr(evidence))
    evidence_hashes = evidence.get("evidence_sha256")
    require(
        isinstance(evidence_hashes, dict) and set(evidence_hashes) == {"failure.json", "readiness.json"},
        repr(evidence),
    )
    for name, expected_hash in evidence_hashes.items():
        retained = output / directory / name
        require(retained.is_file(), f"controlled proof did not retain {directory}/{name}")
        require(sha256_file(retained) == expected_hash, f"retained controlled proof changed {directory}/{name}")


def write_proof_manifest(
    output: Path,
    binary: Path,
    fixture: Path,
    active_clip_fixture: Path,
    explicit_fail_fixture: Path,
    defer_timeout_fixture: Path,
    font: Path,
    materialized_inputs: tuple[Path, ...],
    comparisons: tuple[tuple[bytes, ...], ...],
    explicit_failure: dict[str, Any],
    deferred_timeout_failure: dict[str, Any],
    open_ended_failure: dict[str, Any],
    active_clip_failure: dict[str, Any],
) -> None:
    names = (
        "artifacts/render.png",
        "artifacts/readiness.json",
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
    validate_failure_proof(
        output,
        "explicit-failure",
        explicit_failure,
        expected_code="FIXTURE_READINESS_FAILED",
        message_fragment="expected explicit failure",
        expected_input_sha256=sha256_file(explicit_fail_fixture),
    )
    validate_failure_proof(
        output,
        "deferred-timeout-failure",
        deferred_timeout_failure,
        expected_code="READINESS_TIMEOUT",
        message_fragment="Document readiness timed out after 10000 ms",
        expected_input_sha256=sha256_file(defer_timeout_fixture),
    )
    validate_failure_proof(
        output,
        "open-ended-failure",
        open_ended_failure,
        expected_code="SETTLEMENT_FAILED",
        message_fragment="[OpenEndedSource]",
        expected_input_sha256=sha256_bytes(OPEN_ENDED_SOURCE.encode()),
    )
    validate_failure_proof(
        output,
        "active-clip-failure",
        active_clip_failure,
        expected_code="SETTLEMENT_FAILED",
        message_fragment="[UnsupportedSource]",
        expected_input_sha256=sha256_file(active_clip_fixture),
    )
    manifest = {
        "schema": "pliego.controlled-capture-proof",
        "version": 2,
        "binary_sha256": sha256_file(binary),
        "fixture_template_sha256": sha256_file(fixture),
        "active_clip_fixture_sha256": sha256_file(active_clip_fixture),
        "explicit_fail_fixture_sha256": sha256_file(explicit_fail_fixture),
        "defer_timeout_fixture_sha256": sha256_file(defer_timeout_fixture),
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
        "explicit_failure": explicit_failure,
        "deferred_timeout_failure": deferred_timeout_failure,
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
            "render",
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
    exact_ready_snapshot(summary, root / "artifacts", CONTROLLED_READINESS_PAYLOAD)

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
    return stable_png, (root / "artifacts/readiness.json").read_bytes(), layout_projection, scene, pdf


def run_failure(
    binary: Path,
    root: Path,
    description: str,
    *,
    expected_code: str,
    message_fragment: str,
    readiness_payload: dict[str, Any] | None = None,
) -> dict[str, Any]:
    result = subprocess.run(
        [
            str(binary),
            "render",
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
    require(result.returncode != 0, f"{description} unexpectedly published")
    for relative in SUCCESS_ARTIFACTS:
        require(
            not (root / relative).exists(),
            f"{description} exposed success artifact {relative}",
        )
    summary = final_json(result)
    render_id = summary.get("render_id")
    require(isinstance(render_id, str) and render_id, repr(summary))
    require(summary.get("status") == "failed", repr(summary))
    require(summary.get("error", {}).get("code") == expected_code, repr(summary))
    require(message_fragment in str(summary.get("error", {}).get("message")), repr(summary))
    failure_path = root / "artifacts/failure.json"
    require(failure_path.is_file(), f"{description} did not retain typed failure evidence")
    failure = read_json(failure_path)
    error = failure.get("error")
    require(isinstance(error, dict), repr(failure))
    require(error.get("code") == expected_code, repr(failure))
    require(message_fragment in str(error.get("message")), repr(failure))
    require(
        failure == {"status": "failed", "render_id": render_id, "error": error},
        repr(failure),
    )
    require(summary.get("error") == error, repr(summary))
    readiness_path = root / "artifacts/readiness.json"
    readiness = read_json(readiness_path)
    expected_readiness = (
        {"status": "failed", "render_id": render_id, "error": error}
        if readiness_payload is None
        else {
            "status": "ready",
            "font_status": "loaded",
            "payload": readiness_payload,
            "render_id": render_id,
        }
    )
    require(readiness == expected_readiness, repr(readiness))
    return {
        "exit_code": result.returncode,
        "failure_code": error.get("code"),
        "failure_message": error.get("message"),
        "render_id": render_id,
        "input_sha256": sha256_file(root / "input.html"),
        "readiness_snapshot": readiness,
        "success_artifacts_absent": list(SUCCESS_ARTIFACTS),
        "evidence_sha256": {
            "failure.json": sha256_file(failure_path),
            "readiness.json": sha256_file(readiness_path),
        },
    }


def run_fixture_failure(
    binary: Path,
    fixture: Path,
    root: Path,
    description: str,
    *,
    expected_code: str,
    message_fragment: str,
    readiness_payload: dict[str, Any] | None = None,
) -> dict[str, Any]:
    shutil.copy2(fixture, root / "input.html")
    return run_failure(
        binary,
        root,
        description,
        expected_code=expected_code,
        message_fragment=message_fragment,
        readiness_payload=readiness_payload,
    )


def run_open_ended_failure(binary: Path, root: Path) -> dict[str, Any]:
    (root / "input.html").write_text(OPEN_ENDED_SOURCE, encoding="utf-8")
    return run_failure(
        binary,
        root,
        "open-ended deferred source",
        expected_code="SETTLEMENT_FAILED",
        message_fragment="[OpenEndedSource]",
    )


def self_test(
    fixture: Path,
    active_clip_fixture: Path,
    explicit_fail_fixture: Path,
    defer_timeout_fixture: Path,
    font: Path,
) -> None:
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
            source.count("window.pliego.defer();") == 1 and source.count("window.pliego.ready({") == 1,
            "materialized fixture does not directly author one defer/ready transition",
        )
        require(
            all(f"{key}: " in source for key in CONTROLLED_READINESS_PAYLOAD),
            "materialized fixture does not author the expected readiness payload",
        )
        require(
            "controlled-generation-capture" not in source and "__pliegoReadiness" not in source,
            "materialized fixture still depends on synthetic controlled readiness",
        )
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
            and "const controlledPixels = controlledContext.getImageData(" in source
            and "controlledCanvas.width" in source
            and "controlledCanvas.height" in source,
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
        active_clip_source = active_clip_fixture.read_text(encoding="utf-8")
        clip = active_clip_source.index("context.clip()")
        full_readback = active_clip_source.index("context.getImageData(0, 0, canvas.width, canvas.height)")
        later_fill = active_clip_source.rindex("context.fillRect")
        require(
            clip < full_readback < later_fill,
            "active-clip fixture must draw after a full readback without popping its clip",
        )
        require(
            'window.pliego.fail({\n  code: "FIXTURE_READINESS_FAILED"'
            in explicit_fail_fixture.read_text(encoding="utf-8"),
            "explicit-fail fixture does not directly report its typed readiness error",
        )
        require(
            "window.pliego.defer();" in defer_timeout_fixture.read_text(encoding="utf-8"),
            "defer-timeout fixture does not directly defer readiness",
        )
        proof_root = Path(temporary)
        readiness_artifacts = proof_root / "readiness-self-test"
        readiness_artifacts.mkdir()
        readiness_summary = {
            "render_id": "self-test-render",
            "readiness": CONTROLLED_READINESS_PAYLOAD,
        }
        (readiness_artifacts / "readiness.json").write_text(
            json.dumps(
                {
                    "status": "ready",
                    "font_status": "loaded",
                    "payload": CONTROLLED_READINESS_PAYLOAD,
                    "render_id": "self-test-render",
                }
            ),
            encoding="utf-8",
        )
        require(
            exact_ready_snapshot(readiness_summary, readiness_artifacts, CONTROLLED_READINESS_PAYLOAD)["payload"]
            == CONTROLLED_READINESS_PAYLOAD,
            repr(readiness_summary),
        )
        (proof_root / "artifacts").mkdir()
        (proof_root / "artifacts/render.png").write_bytes(b"png")
        (proof_root / "artifacts/readiness.json").write_bytes(b"ready")
        (proof_root / "normalized-layout.json").write_bytes(b"layout")
        (proof_root / "artifacts/scene.json").write_bytes(b"scene")
        (proof_root / "document.pdf").write_bytes(b"pdf")

        def retained_failure(
            directory: str,
            code: str,
            message: str,
            *,
            input_sha256: str,
            readiness_payload: dict[str, Any] | None = None,
        ) -> dict[str, Any]:
            retained = proof_root / directory
            retained.mkdir()
            failure_bytes = f"{directory} failure".encode()
            readiness_bytes = f"{directory} readiness".encode()
            (retained / "failure.json").write_bytes(failure_bytes)
            (retained / "readiness.json").write_bytes(readiness_bytes)
            render_id = f"render-{directory}"
            readiness = (
                {
                    "status": "failed",
                    "render_id": render_id,
                    "error": {"code": code, "message": message},
                }
                if readiness_payload is None
                else {
                    "status": "ready",
                    "font_status": "loaded",
                    "payload": readiness_payload,
                    "render_id": render_id,
                }
            )
            return {
                "exit_code": 1,
                "failure_code": code,
                "failure_message": message,
                "render_id": render_id,
                "input_sha256": input_sha256,
                "readiness_snapshot": readiness,
                "success_artifacts_absent": list(SUCCESS_ARTIFACTS),
                "evidence_sha256": {
                    "failure.json": sha256_bytes(failure_bytes),
                    "readiness.json": sha256_bytes(readiness_bytes),
                },
            }

        explicit_failure = retained_failure(
            "explicit-failure",
            "FIXTURE_READINESS_FAILED",
            "expected explicit failure",
            input_sha256=sha256_file(explicit_fail_fixture),
        )
        deferred_timeout_failure = retained_failure(
            "deferred-timeout-failure",
            "READINESS_TIMEOUT",
            "Document readiness timed out after 10000 ms",
            input_sha256=sha256_file(defer_timeout_fixture),
        )
        open_ended_failure = retained_failure(
            "open-ended-failure",
            "SETTLEMENT_FAILED",
            "capture preparation failed: [OpenEndedSource]",
            input_sha256=sha256_bytes(OPEN_ENDED_SOURCE.encode()),
        )
        active_clip_failure = retained_failure(
            "active-clip-failure",
            "SETTLEMENT_FAILED",
            "capture preparation failed: [UnsupportedSource]",
            input_sha256=sha256_file(active_clip_fixture),
        )
        write_proof_manifest(
            proof_root,
            materialized,
            fixture,
            active_clip_fixture,
            explicit_fail_fixture,
            defer_timeout_fixture,
            font,
            (materialized, materialized),
            (
                (b"png", b"ready", b"layout", b"scene", b"pdf"),
                (b"png", b"ready", b"layout", b"scene", b"pdf"),
            ),
            explicit_failure,
            deferred_timeout_failure,
            open_ended_failure,
            active_clip_failure,
        )
        proof = read_json(proof_root / "proof.json")
        require(proof.get("schema") == "pliego.controlled-capture-proof", repr(proof))
        require(proof.get("version") == 2, repr(proof))
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
                "artifacts/readiness.json",
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
                "artifacts/readiness.json": "b24d6d33736ecd5604a4b17bc9c6481039fac362bb7df044ef1c10a2bfd21db6",
                "normalized-layout.json": "1dc5ae5b68174891b6aa9850aa05ee0d9ae8a20468d9517259951a2dd9e9c0f0",
                "artifacts/scene.json": "4f48f7207cebf92638debb5694e4a8677350cf88a9e98e73c6c466b6b1d43890",
                "document.pdf": "c35b21d6ca39aa7cc3b79a705d989f1a6e88b99ab43988d74048799e3db926a3",
            },
            repr(proof),
        )
        require(
            proof.get("explicit_failure", {}).get("readiness_snapshot", {}).get("error", {}).get("code")
            == "FIXTURE_READINESS_FAILED",
            repr(proof),
        )
        require(
            proof.get("deferred_timeout_failure", {}).get("readiness_snapshot", {}).get("error", {}).get("code")
            == "READINESS_TIMEOUT",
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
        require(
            proof.get("active_clip_failure", {}).get("readiness_snapshot", {}).get("error", {}).get("code")
            == "SETTLEMENT_FAILED",
            repr(proof),
        )


def main() -> int:
    test_root = Path(__file__).resolve().parent
    repository = Path(__file__).resolve().parents[3]
    fixture = test_root / "fixtures/controlled-production-capture/index.html"
    active_clip_fixture = test_root / "fixtures/controlled-canvas-active-clip/index.html"
    explicit_fail_fixture = test_root / "fixtures/document-session/explicit-fail.html"
    defer_timeout_fixture = test_root / "fixtures/document-session/defer-timeout.html"
    font = repository / "components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
    if sys.argv[1:] == ["--self-test"]:
        require(fixture.is_file(), f"missing fixture: {fixture}")
        require(active_clip_fixture.is_file(), f"missing fixture: {active_clip_fixture}")
        require(explicit_fail_fixture.is_file(), f"missing fixture: {explicit_fail_fixture}")
        require(defer_timeout_fixture.is_file(), f"missing fixture: {defer_timeout_fixture}")
        require(font.is_file(), f"missing controlled-capture font: {font}")
        self_test(fixture, active_clip_fixture, explicit_fail_fixture, defer_timeout_fixture, font)
        print("controlled capture checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)
    binary = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2]).resolve()
    require(binary.is_file(), f"missing Pliego binary: {binary}")
    require(fixture.is_file(), f"missing fixture: {fixture}")
    require(active_clip_fixture.is_file(), f"missing fixture: {active_clip_fixture}")
    require(explicit_fail_fixture.is_file(), f"missing fixture: {explicit_fail_fixture}")
    require(defer_timeout_fixture.is_file(), f"missing fixture: {defer_timeout_fixture}")
    require(font.is_file(), f"missing controlled-capture font: {font}")
    require(not output.exists(), f"output already exists: {output}")
    output.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="pliego-controlled-capture-") as temporary:
        temporary_root = Path(temporary)
        first_root = temporary_root / "first"
        second_root = temporary_root / "second"
        explicit_failure_root = temporary_root / "explicit-failure"
        deferred_timeout_failure_root = temporary_root / "deferred-timeout-failure"
        open_ended_failure_root = temporary_root / "open-ended-failure"
        active_clip_failure_root = temporary_root / "active-clip-failure"
        for root in (
            first_root,
            second_root,
            explicit_failure_root,
            deferred_timeout_failure_root,
            open_ended_failure_root,
            active_clip_failure_root,
        ):
            root.mkdir()
        first = run_success(binary, fixture, font, first_root)
        second = run_success(binary, fixture, font, second_root)
        require(
            first == second,
            "identical controlled captures changed readiness, pixels, semantic layout, scene, or PDF",
        )
        explicit_failure = run_fixture_failure(
            binary,
            explicit_fail_fixture,
            explicit_failure_root,
            "explicit readiness failure",
            expected_code="FIXTURE_READINESS_FAILED",
            message_fragment="expected explicit failure",
        )
        deferred_timeout_failure = run_fixture_failure(
            binary,
            defer_timeout_fixture,
            deferred_timeout_failure_root,
            "deferred readiness timeout",
            expected_code="READINESS_TIMEOUT",
            message_fragment="Document readiness timed out after 10000 ms",
        )
        open_ended_failure = run_open_ended_failure(binary, open_ended_failure_root)
        active_clip_failure = run_fixture_failure(
            binary,
            active_clip_fixture,
            active_clip_failure_root,
            "active-clip Canvas failure",
            expected_code="SETTLEMENT_FAILED",
            message_fragment="[UnsupportedSource]",
        )
        shutil.copytree(first_root / "artifacts", output / "artifacts")
        shutil.copy2(first_root / "document.pdf", output / "document.pdf")
        (output / "normalized-layout.json").write_bytes(first[2])
        for directory, root in (
            ("explicit-failure", explicit_failure_root),
            ("deferred-timeout-failure", deferred_timeout_failure_root),
            ("open-ended-failure", open_ended_failure_root),
            ("active-clip-failure", active_clip_failure_root),
        ):
            retained = output / directory
            retained.mkdir()
            shutil.copy2(root / "artifacts/failure.json", retained / "failure.json")
            shutil.copy2(root / "artifacts/readiness.json", retained / "readiness.json")
        write_proof_manifest(
            output,
            binary,
            fixture,
            active_clip_fixture,
            explicit_fail_fixture,
            defer_timeout_fixture,
            font,
            (first_root / "input.html", second_root / "input.html"),
            (first, second),
            explicit_failure,
            deferred_timeout_failure,
            open_ended_failure,
            active_clip_failure,
        )
    print(f"controlled capture check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
