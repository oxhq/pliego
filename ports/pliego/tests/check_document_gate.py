#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import json
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

FIXTURE_SCHEMA = "pliego.document-gate-fixtures"
BENCHMARK_SCHEMA = "pliego.document-benchmark.v1"
PROCESS_TIMEOUT_SECONDS = 900
ROUND_DIGITS = 6


def fail(message: str, code: int = 1) -> None:
    print(f"document gate check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_object(path: Path, description: str) -> dict[str, Any]:
    require(path.is_file(), f"missing {description}: {path}")
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {description} {path}: {error}")
    require(isinstance(value, dict), f"{description} is not an object: {path}")
    return value


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def final_summary(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def validate_fixture_pins(directory: Path) -> dict[str, Any]:
    manifest = read_object(directory / "manifest.json", "fixture manifest")
    require(
        manifest.get("schema") == FIXTURE_SCHEMA and manifest.get("version") == 1,
        f"unexpected fixture manifest schema: {manifest!r}",
    )
    require(
        isinstance(manifest.get("rights"), str) and "synthetic" in manifest["rights"].lower(),
        "fixture rights declaration is absent",
    )
    font = manifest.get("font")
    require(isinstance(font, dict), "font pin is absent")
    font_path = (directory / str(font.get("path", ""))).resolve()
    require(font_path.is_file(), f"pinned font is absent: {font_path}")
    require(font.get("sha256") == sha256(font_path), "pinned font digest differs")

    fixtures = manifest.get("fixtures")
    require(isinstance(fixtures, list) and len(fixtures) == 2, "invoice and statement pins are required")
    require(
        {item.get("name") for item in fixtures if isinstance(item, dict)}
        == {
            "invoice",
            "statement-100-page",
        },
        "fixture names differ",
    )
    for fixture in fixtures:
        require(isinstance(fixture, dict), f"invalid fixture pin: {fixture!r}")
        path = directory / str(fixture.get("path", ""))
        require(path.is_file(), f"pinned fixture is absent: {path}")
        require(fixture.get("sha256") == sha256(path), f"{fixture.get('name')} digest differs")
        pages = fixture.get("expected_pages")
        rows = fixture.get("rows")
        rows_per_page = fixture.get("rows_per_page")
        require(
            all(isinstance(value, int) and value > 0 for value in (pages, rows, rows_per_page))
            and rows == pages * rows_per_page,
            f"{fixture.get('name')} row/page contract differs",
        )
        require(
            fixture.get("page_size") == [612, 792] and fixture.get("page_margins") == [36, 36, 36, 36],
            f"{fixture.get('name')} page geometry differs",
        )
        markup = path.read_text(encoding="utf-8")
        require(
            "window.pliego?.ready" in markup
            and "Math.random" not in markup
            and "new Date" not in markup
            and "http://" not in markup
            and "https://" not in markup,
            f"{fixture.get('name')} is not a deterministic local fixture",
        )
    return manifest


def expected_ids(fixture: dict[str, Any]) -> list[str]:
    prefix = str(fixture["id_prefix"])
    width = int(fixture["id_width"])
    return [f"{prefix}{row:0{width}d}" for row in range(1, int(fixture["rows"]) + 1)]


def row_ids(value: str, fixture: dict[str, Any]) -> list[str]:
    prefix = re.escape(str(fixture["id_prefix"]))
    width = int(fixture["id_width"])
    return re.findall(rf"\b{prefix}\d{{{width}}}\b", value)


def text_x(operation: dict[str, Any], description: str) -> float:
    glyphs = operation.get("glyphs")
    require(isinstance(glyphs, list) and glyphs, f"{description} has no glyph geometry")
    glyph = glyphs[0]
    require(isinstance(glyph, dict), f"{description} glyph is invalid")
    value = glyph.get("x")
    require(
        isinstance(value, (int, float)) and not isinstance(value, bool),
        f"{description} x coordinate is invalid",
    )
    return round(float(value), ROUND_DIGITS)


def is_repeated_header(operation: dict[str, Any]) -> bool:
    semantics = operation.get("meta", {}).get("semantics")
    return (
        isinstance(semantics, dict)
        and semantics.get("role") == "artifact"
        and semantics.get("label") == "repeated-table-header"
    )


def border_x_coordinates(
    operations: list[dict[str, Any]],
    page_index: int,
    column_count: int,
) -> tuple[float, ...]:
    rectangles = []
    for operation in operations:
        semantics = operation.get("meta", {}).get("semantics")
        if (
            operation.get("type") != "path"
            or not isinstance(semantics, dict)
            or semantics.get("label") != "table-border"
        ):
            continue
        bounds = operation.get("bounds")
        require(isinstance(bounds, dict), f"page {page_index} table border has no bounds")
        values = tuple(bounds.get(field) for field in ("x", "y", "width", "height"))
        require(
            all(
                isinstance(value, (int, float)) and not isinstance(value, bool) and float(value) >= 0
                for value in values
            )
            and float(values[2]) > 0
            and float(values[3]) > 0,
            f"page {page_index} table border geometry is invalid: {values!r}",
        )
        rectangle = tuple(round(float(value), ROUND_DIGITS) for value in values)
        require(operation.get("stroke") is None, f"page {page_index} table border uses a stroke")
        rectangles.append(rectangle)
    require(rectangles, f"page {page_index} has no captured table borders")
    require(
        len(rectangles) == len(set(rectangles)),
        f"page {page_index} contains duplicate table borders",
    )
    vertical = sorted({item[0] for item in rectangles if item[3] > item[2]})
    require(
        len(vertical) >= column_count + 1,
        f"page {page_index} does not retain the complete column grid: {vertical!r}",
    )
    return tuple(vertical)


def verify_scene_and_trace(
    scene: dict[str, Any],
    layout: dict[str, Any],
    fixture: dict[str, Any],
) -> None:
    pages = scene.get("pages")
    expected_pages = int(fixture["expected_pages"])
    require(isinstance(pages, list) and len(pages) == expected_pages, f"{fixture['name']} page count differs")

    physical_text = []
    header_positions = []
    border_positions = []
    headers = list(fixture["headers"])
    for page_index, page in enumerate(pages):
        require(isinstance(page, dict), f"{fixture['name']} scene page {page_index} is invalid")
        require(page.get("size") == {"width": 612.0, "height": 792.0}, f"page {page_index} size differs")
        operations = page.get("operations")
        require(isinstance(operations, list), f"{fixture['name']} scene page {page_index} has no operations")
        objects = [operation for operation in operations if isinstance(operation, dict)]
        texts = [
            (operation, canonical_text(str(operation.get("text", ""))))
            for operation in objects
            if operation.get("type") == "text"
        ]
        physical_text.extend(text for _, text in texts if text)
        positions = []
        for header in headers:
            matches = [(operation, text) for operation, text in texts if text == header]
            require(len(matches) == 1, f"page {page_index} does not contain one {header!r} header")
            operation = matches[0][0]
            require(
                page_index == 0 or is_repeated_header(operation),
                f"page {page_index} header {header!r} is not marked as a repeated artifact",
            )
            positions.append(text_x(operation, f"page {page_index} header {header!r}"))
        header_positions.append(tuple(positions))
        border_positions.append(border_x_coordinates(objects, page_index, len(headers)))

    require(
        row_ids(" ".join(physical_text), fixture) == expected_ids(fixture),
        f"{fixture['name']} scene rows are missing, duplicated, or reordered",
    )
    require(len(set(header_positions)) == 1, f"{fixture['name']} header columns drifted")
    require(len(set(border_positions)) == 1, f"{fixture['name']} border columns drifted")

    sequence = layout.get("page_sequence")
    require(isinstance(sequence, dict), f"{fixture['name']} table trace is absent")
    layout_pages = sequence.get("pages")
    require(isinstance(layout_pages, list) and len(layout_pages) == expected_pages, "layout page count differs")
    expected_resumes = list(range(1, expected_pages))
    continuations = sequence.get("continuations")
    require(isinstance(continuations, list), f"{fixture['name']} continuation trace is absent")
    table_resumes = [
        item.get("token", {}).get("resume_page_index")
        for item in continuations
        if isinstance(item, dict) and item.get("token", {}).get("kind") == "table"
    ]
    require(table_resumes == expected_resumes, f"{fixture['name']} table continuation trace differs")
    repeats = sequence.get("table_group_repeats")
    require(isinstance(repeats, list), f"{fixture['name']} header repeat trace is absent")
    require(
        [repeat.get("page_index") for repeat in repeats] == expected_resumes,
        f"{fixture['name']} header repeat trace differs",
    )
    breaks = sequence.get("table_breaks")
    require(isinstance(breaks, list), f"{fixture['name']} table break trace is absent")
    selected_resumes = [item.get("resume_page_index") for item in breaks if item.get("selected") is True]
    require(selected_resumes == expected_resumes, f"{fixture['name']} selected table breaks differ")
    require(sequence.get("warnings", []) == [], f"{fixture['name']} emitted pagination warnings")


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the document gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-nopgbrk", str(pdf), "-"],
        capture_output=True,
        timeout=120,
        check=False,
    )
    require(result.returncode == 0, f"pdftotext failed: {result.stderr.decode(errors='replace')}")
    return result.stdout.decode("utf-8-sig")


def verify_pdf(structure: dict[str, Any], pdf: Path, fixture: dict[str, Any]) -> None:
    expected_pages = int(fixture["expected_pages"])
    pages = structure.get("pages")
    require(
        structure.get("schema") == "pliego.pdf-structure"
        and structure.get("page_count") == expected_pages
        and isinstance(pages, list)
        and len(pages) == expected_pages,
        f"{fixture['name']} PDF structure differs",
    )
    expected = expected_ids(fixture)
    rows_per_page = int(fixture["rows_per_page"])
    for page_index, page in enumerate(pages):
        require(isinstance(page, dict), f"{fixture['name']} PDF page {page_index} is invalid")
        require(
            row_ids(str(page.get("expected_extracted_unicode", "")), fixture)
            == expected[page_index * rows_per_page : (page_index + 1) * rows_per_page],
            f"{fixture['name']} PDF page {page_index} rows differ",
        )
        counts = page.get("operation_counts")
        require(
            isinstance(counts, dict)
            and isinstance(counts.get("vector"), int)
            and counts["vector"] > 0
            and isinstance(page.get("embedded_font_ids"), list)
            and bool(page["embedded_font_ids"]),
            f"{fixture['name']} PDF page {page_index} lacks borders or embedded fonts",
        )
    contents = pdf.read_bytes()
    require(contents.startswith(b"%PDF-") and b"/ToUnicode" in contents, f"{fixture['name']} PDF is invalid")
    require(
        row_ids(extract_pdf_text(pdf), fixture) == expected,
        f"{fixture['name']} extracted PDF rows are missing, duplicated, or reordered",
    )


def png_dimensions(contents: bytes) -> tuple[int, int]:
    require(
        len(contents) > 64 and contents.startswith(b"\x89PNG\r\n\x1a\n") and contents[12:16] == b"IHDR",
        "preview is not a complete PNG",
    )
    return struct.unpack(">II", contents[16:24])


def verify_previews(summary: dict[str, Any], fixture: dict[str, Any]) -> None:
    pages = read_object(Path(str(summary.get("pages_artifact", ""))), "page preview manifest").get("pages")
    expected_pages = int(fixture["expected_pages"])
    require(isinstance(pages, list) and len(pages) == expected_pages, f"{fixture['name']} previews differ")
    artifacts = Path(str(summary.get("artifacts", "")))
    for page_index, page in enumerate(pages):
        require(isinstance(page, dict), f"{fixture['name']} preview {page_index} is invalid")
        preview = artifacts / str(page.get("artifact", ""))
        require(preview.is_file(), f"{fixture['name']} preview {page_index} is absent")
        require(
            png_dimensions(preview.read_bytes()) == (612, 792),
            f"{fixture['name']} preview {page_index} dimensions differ",
        )


def session_span_ms(artifacts: Path) -> int:
    states = []
    for line in (artifacts / "session-state.jsonl").read_text(encoding="utf-8").splitlines():
        value = json.loads(line)
        require(isinstance(value, dict), "session state is not an object")
        states.append(value)
    require([state.get("state") for state in states] == ["started", "rendered"], "session states differ")
    started, rendered = (state.get("timestamp_ms") for state in states)
    require(
        isinstance(started, int) and isinstance(rendered, int) and rendered >= started,
        "session timestamps differ",
    )
    return rendered - started


def run_document(
    binary: Path,
    fixtures_root: Path,
    fixture: dict[str, Any],
    destination: Path,
) -> dict[str, Any]:
    destination.mkdir()
    temporary = destination / "tmp"
    temporary.mkdir()
    artifacts = destination / "artifacts"
    output = destination / "document.pdf"
    page_size = "x".join(str(value) for value in fixture["page_size"])
    margins = ",".join(str(value) for value in fixture["page_margins"])
    command = [
        str(binary),
        "render",
        f"document-gate/{fixture['path']}",
        "--output",
        str(output),
        "--artifacts",
        str(artifacts),
        "--page-size",
        page_size,
        "--page-margins",
        margins,
    ]
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(temporary), "TMP": str(temporary), "TEMP": str(temporary)})
    started = time.perf_counter()
    try:
        result = subprocess.run(
            command,
            cwd=fixtures_root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=PROCESS_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        fail(f"{fixture['name']} exceeded {PROCESS_TIMEOUT_SECONDS}s")
    process_ms = round((time.perf_counter() - started) * 1000, 3)
    (destination / "stdout.log").write_text(result.stdout, encoding="utf-8")
    (destination / "stderr.log").write_text(result.stderr, encoding="utf-8")
    require(result.returncode == 0, f"{fixture['name']} exited with {result.returncode}: {result.stderr[-4000:]}")
    summary = final_summary(result)
    require(summary.get("status") == "rendered", f"{fixture['name']} did not render: {summary!r}")
    scene = read_object(Path(str(summary.get("scene_artifact", ""))), "scene artifact")
    layout = read_object(Path(str(summary.get("layout_debug", ""))), "layout artifact")
    structure = read_object(Path(str(summary.get("pdf_structure", ""))), "PDF structure artifact")
    verify_scene_and_trace(scene, layout, fixture)
    verify_pdf(structure, output, fixture)
    verify_previews(summary, fixture)
    return {
        "fixture": fixture["name"],
        "warm_cold_state": "cold one-shot process",
        "concurrency": 1,
        "process_total_ms": process_ms,
        "controlled_session_ms": session_span_ms(artifacts),
        "pages": fixture["expected_pages"],
        "scene_hash": summary.get("scene", {}).get("hash"),
    }


def benchmark_report(
    binary: Path,
    build_profile: str,
    manifest: dict[str, Any],
    measurements: list[dict[str, Any]],
) -> dict[str, Any]:
    version = subprocess.run(
        [str(binary), "--version"],
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )
    require(version.returncode == 0 and version.stdout.strip(), "cannot identify the Pliego build")
    return {
        "schema": BENCHMARK_SCHEMA,
        "hardware": {
            "machine": platform.machine(),
            "processor": platform.processor(),
            "logical_cpu_count": os.cpu_count(),
        },
        "os": {
            "system": platform.system(),
            "release": platform.release(),
            "version": platform.version(),
        },
        "build_profile": build_profile,
        "engine": version.stdout.strip().splitlines(),
        "inputs": [
            {
                "name": fixture["name"],
                "path": fixture["path"],
                "sha256": fixture["sha256"],
                "page_size": fixture["page_size"],
                "page_margins": fixture["page_margins"],
            }
            for fixture in manifest["fixtures"]
        ],
        "fonts": [manifest["font"]],
        "measurements": measurements,
        "output_validation_method": [
            "page PNG dimensions",
            "canonical scene row and geometry checks",
            "PDF page structure and embedded font checks",
            "pdftotext row identity and order",
            "table continuation, break, header-repeat, and warning traces",
        ],
        "limitations": [
            "The one-shot CLI currently exposes whole-process and controlled-session timing boundaries only.",
            "These correctness-gated values are a baseline, not a competitive performance claim.",
        ],
    }


def synthetic_operation(kind: str, **values: Any) -> dict[str, Any]:
    return {"type": kind, **values}


def self_test(directory: Path) -> None:
    manifest = validate_fixture_pins(directory)
    fixture = manifest["fixtures"][0]
    pages = []
    expected = expected_ids(fixture)
    rows_per_page = int(fixture["rows_per_page"])
    header_x = [40.0, 136.0, 208.0, 484.0]
    border_x = [36.0, 132.0, 204.0, 480.0, 576.0]
    for page_index in range(int(fixture["expected_pages"])):
        operations = []
        for header, x in zip(fixture["headers"], header_x, strict=True):
            meta = {"semantics": {"role": "artifact", "label": "repeated-table-header"}} if page_index else {}
            operations.append(synthetic_operation("text", text=header, glyphs=[{"x": x}], meta=meta))
        for value in expected[page_index * rows_per_page : (page_index + 1) * rows_per_page]:
            operations.append(synthetic_operation("text", text=value, glyphs=[{"x": 40.0}]))
        for x in border_x:
            operations.append(
                synthetic_operation(
                    "path",
                    bounds={"x": x, "y": 32.0, "width": 1.0, "height": 600.0},
                    fill={"r": 0.1, "g": 0.2, "b": 0.3, "a": 1.0},
                    stroke=None,
                    meta={"semantics": {"role": "artifact", "label": "table-border"}},
                )
            )
        pages.append({"size": {"width": 612.0, "height": 792.0}, "operations": operations})
    resumes = list(range(1, int(fixture["expected_pages"])))
    layout = {
        "page_sequence": {
            "pages": [{"index": page} for page in range(int(fixture["expected_pages"]))],
            "continuations": [{"token": {"kind": "table", "resume_page_index": page}} for page in resumes],
            "table_group_repeats": [{"page_index": page} for page in resumes],
            "table_breaks": [{"selected": True, "resume_page_index": page} for page in resumes],
            "warnings": [],
        }
    }
    verify_scene_and_trace({"pages": pages}, layout, fixture)
    report = benchmark_report(Path(sys.executable), "self-test", manifest, [])
    require(
        report["schema"] == BENCHMARK_SCHEMA
        and report["build_profile"] == "self-test"
        and report["fonts"] == [manifest["font"]],
        "benchmark metadata differs",
    )


def main() -> int:
    directory = Path(__file__).resolve().parent / "fixtures/document-gate"
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test(directory)
        print("document gate self-test: ok")
        return 0
    if len(arguments) != 3:
        fail(
            f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory> <build-profile> | --self-test",
            2,
        )
    binary = Path(arguments[0]).expanduser().resolve()
    output = Path(arguments[1]).expanduser().resolve()
    build_profile = arguments[2].strip()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(not output.exists(), f"output directory already exists: {output}")
    require(bool(build_profile), "build profile is empty")
    output.mkdir(parents=True)
    manifest = validate_fixture_pins(directory)
    fixtures_root = directory.parent
    measurements = [
        run_document(binary, fixtures_root, fixture, output / fixture["name"]) for fixture in manifest["fixtures"]
    ]
    report = benchmark_report(binary, build_profile, manifest, measurements)
    (output / "benchmark.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"document gate check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
