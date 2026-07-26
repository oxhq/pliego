#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import hashlib
import io
import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from contextlib import redirect_stderr
from pathlib import Path
from typing import Any

from PIL import Image, ImageChops

FIXTURE_SCHEMA = "pliego.document-gate-fixtures"
BENCHMARK_SCHEMA = "pliego.document-benchmark.v1"
SCENE_SCHEMA = "pliego.document-scene"
SANITIZED_AHEM_SHA256 = "649a76135ca32e68e36c7880075cf897ec1a1fdfe011308bdcf8ada12a539869"
CSS_PX_TO_PDF_PT = 0.75
PROCESS_TIMEOUT_SECONDS = 900
ROUND_DIGITS = 6
GEOMETRY_TOLERANCE = 0.51
PHASES = (
    "controlled_runtime",
    "scene_capture",
    "scene_setup",
    "preview_raster",
    "pdf_serialize",
)


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


def object_value(value: Any, description: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{description} is not an object")
    return value


def list_value(value: Any, description: str) -> list[Any]:
    require(isinstance(value, list), f"{description} is not a list")
    return value


def number(value: Any, description: str) -> float:
    require(
        isinstance(value, (int, float)) and not isinstance(value, bool),
        f"{description} is not numeric",
    )
    result = float(value)
    require(math.isfinite(result), f"{description} is not finite")
    return result


def sha256_bytes(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def sha256(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


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


def artifact_path(summary: dict[str, Any], key: str, description: str) -> Path:
    value = summary.get(key)
    require(isinstance(value, str) and bool(value), f"terminal summary has no {description} path")
    path = Path(value).resolve()
    require(path.is_file(), f"missing {description}: {path}")
    return path


def artifacts_directory(summary: dict[str, Any]) -> Path:
    value = summary.get("artifacts")
    require(isinstance(value, str) and bool(value), "terminal summary has no artifacts directory")
    path = Path(value).resolve()
    require(path.is_dir(), f"missing artifacts directory: {path}")
    return path


def operation_counts(operations: list[dict[str, Any]]) -> dict[str, int]:
    return {
        "text": sum(operation.get("type") == "text" for operation in operations),
        "vector": sum(operation.get("type") == "path" for operation in operations),
        "image": sum(operation.get("type") == "image" for operation in operations),
        "link": sum(operation.get("type") == "link" for operation in operations),
    }


def validate_fixture_pins(directory: Path) -> dict[str, Any]:
    manifest = read_object(directory / "manifest.json", "fixture manifest")
    document_root = directory.parent.resolve()
    require(
        manifest.get("schema") == FIXTURE_SCHEMA and manifest.get("version") == 1,
        f"unexpected fixture manifest schema: {manifest!r}",
    )
    require(
        isinstance(manifest.get("rights"), str) and "synthetic" in manifest["rights"].lower(),
        "fixture rights declaration is absent",
    )
    font = object_value(manifest.get("font"), "font pin")
    font_path = (directory / str(font.get("path", ""))).resolve()
    require(font_path.is_relative_to(document_root), f"pinned font escapes document root: {font_path}")
    require(font_path.is_file(), f"pinned font is absent: {font_path}")
    require(font.get("sha256") == sha256(font_path), "pinned font digest differs")
    require(font.get("sanitized_sha256") == SANITIZED_AHEM_SHA256, "sanitized font digest differs")

    fixtures = list_value(manifest.get("fixtures"), "fixtures")
    require(len(fixtures) == 2, "invoice and statement pins are required")
    require(
        {item.get("name") for item in fixtures if isinstance(item, dict)} == {"invoice", "statement-100-page"},
        "fixture names differ",
    )
    natural_fixture_count = 0
    for raw_fixture in fixtures:
        fixture = object_value(raw_fixture, "fixture pin")
        name = str(fixture.get("name"))
        path = (directory / str(fixture.get("path", ""))).resolve()
        require(path.is_relative_to(document_root), f"{name} escapes document root: {path}")
        require(path.is_file(), f"pinned fixture is absent: {path}")
        require(fixture.get("sha256") == sha256(path), f"{name} digest differs")
        pages = fixture.get("expected_pages")
        rows = fixture.get("rows")
        rows_per_page = fixture.get("rows_per_page")
        require(
            all(isinstance(value, int) and value > 0 for value in (pages, rows, rows_per_page))
            and rows == pages * rows_per_page,
            f"{name} row/page contract differs",
        )
        require(
            fixture.get("page_size") == [612, 792] and fixture.get("page_margins") == [36, 36, 36, 36],
            f"{name} page geometry differs",
        )
        headers = list_value(fixture.get("headers"), f"{name} headers")
        boundaries = list_value(fixture.get("column_boundaries"), f"{name} column boundaries")
        text_columns = list_value(fixture.get("text_x"), f"{name} text columns")
        require(
            len(headers) == 4
            and len(boundaries) == len(headers) + 1
            and len(text_columns) == len(headers)
            and all(isinstance(value, (int, float)) for value in boundaries + text_columns),
            f"{name} column contract differs",
        )
        require(
            fixture.get("break_constraint") in {"auto", "forced-after"}
            and isinstance(fixture.get("caption"), str)
            and isinstance(fixture.get("footer"), list),
            f"{name} content or break contract differs",
        )
        markup = path.read_text(encoding="utf-8")
        require(
            "window.pliego?.ready" in markup
            and "Math.random" not in markup
            and "new Date" not in markup
            and "http://" not in markup
            and "https://" not in markup,
            f"{name} is not a deterministic local fixture",
        )
        require(
            'src: url("../text-scene/Ahem.ttf")' in markup
            and (path.parent / "../text-scene/Ahem.ttf").resolve() == font_path,
            f"{name} does not use the pinned in-root font",
        )
        natural = fixture.get("natural_overflow") is True
        if natural:
            natural_fixture_count += 1
            require(
                re.search(r"break-(?:before|after)\s*:", markup) is None and "page-end" not in markup,
                f"{name} claims natural overflow but contains forced page breaks",
            )
            require(fixture.get("break_constraint") == "auto", f"{name} natural break contract differs")
        else:
            require(
                "break-after: page" in markup and fixture.get("break_constraint") == "forced-after",
                f"{name} forced-break fixture contract differs",
            )
    require(natural_fixture_count >= 1, "at least one multipage fixture must paginate by natural overflow")
    return manifest


def expected_row(fixture: dict[str, Any], row: int) -> list[str]:
    if fixture["name"] == "invoice":
        return [f"INV-{row:03d}", "1", f"SERVICE-{row:03d}", f"{row * 10}.00"]
    if fixture["name"] == "statement-100-page":
        return [f"STMT-{row:04d}", f"D{row:04d}", f"ENTRY-{row:04d}", f"{row}.00"]
    fail(f"unknown document fixture: {fixture['name']}")
    raise AssertionError("unreachable")


def expected_page_tokens(fixture: dict[str, Any], page_index: int) -> list[str]:
    rows_per_page = int(fixture["rows_per_page"])
    start = page_index * rows_per_page + 1
    tokens = [str(fixture["caption"])] if page_index == 0 else []
    tokens.extend(str(value) for value in fixture["headers"])
    for row in range(start, start + rows_per_page):
        tokens.extend(expected_row(fixture, row))
    if page_index + 1 == int(fixture["expected_pages"]):
        tokens.extend(str(value) for value in fixture["footer"])
    return tokens


def expected_token_x(fixture: dict[str, Any], page_index: int) -> list[float]:
    columns = [number(value, f"{fixture['name']} text column") for value in fixture["text_x"]]
    values = [number(fixture["caption_x"], f"{fixture['name']} caption x")] if page_index == 0 else []
    values.extend(columns)
    values.extend(columns * int(fixture["rows_per_page"]))
    if page_index + 1 == int(fixture["expected_pages"]):
        values.extend(columns[: len(fixture["footer"])])
    return values


def text_coordinate(operation: dict[str, Any], field: str, description: str) -> float:
    glyphs = list_value(operation.get("glyphs"), f"{description} glyphs")
    require(bool(glyphs), f"{description} has no glyph geometry")
    glyph = object_value(glyphs[0], f"{description} first glyph")
    return number(glyph.get(field), f"{description} {field}")


def is_repeated_header(operation: dict[str, Any]) -> bool:
    meta = operation.get("meta")
    semantics = meta.get("semantics") if isinstance(meta, dict) else None
    return (
        isinstance(semantics, dict)
        and semantics.get("role") == "artifact"
        and semantics.get("label") == "repeated-table-header"
    )


def border_operation(operation: dict[str, Any]) -> bool:
    meta = operation.get("meta")
    semantics = meta.get("semantics") if isinstance(meta, dict) else None
    return (
        operation.get("type") == "path"
        and isinstance(semantics, dict)
        and semantics.get("label") in {"table-border", "repeated-table-header"}
    )


def border_rectangle(
    operation: dict[str, Any],
    fixture: dict[str, Any],
    page_index: int,
) -> tuple[float, float, float, float]:
    bounds = object_value(operation.get("bounds"), f"{fixture['name']} page {page_index} border bounds")
    values = tuple(
        round(number(bounds.get(field), f"{fixture['name']} page {page_index} border {field}"), ROUND_DIGITS)
        for field in ("x", "y", "width", "height")
    )
    require(values[2] > 0 and values[3] > 0, f"{fixture['name']} page {page_index} has empty border")
    require(
        not math.isclose(values[2], values[3], abs_tol=10**-ROUND_DIGITS),
        f"{fixture['name']} page {page_index} border is not an edge rectangle: {values!r}",
    )
    fill = object_value(operation.get("fill"), f"{fixture['name']} page {page_index} border fill")
    require(number(fill.get("a"), f"{fixture['name']} page {page_index} border alpha") > 0, "border is invisible")
    require(operation.get("stroke") is None, f"{fixture['name']} page {page_index} border uses a stroke")
    require(
        isinstance(operation.get("data"), str) and bool(operation["data"]),
        f"{fixture['name']} page {page_index} border has no path data",
    )
    return values


def close_geometry(actual: float, expected: float) -> bool:
    return math.isclose(actual, expected, abs_tol=GEOMETRY_TOLERANCE)


def verify_horizontal_seams(
    rectangles: list[tuple[float, float, float, float]],
    fixture: dict[str, Any],
    page_index: int,
) -> None:
    left = number(fixture["column_boundaries"][0], "table left boundary")
    right = number(fixture["column_boundaries"][-1], "table right boundary")
    horizontal = [value for value in rectangles if value[2] > value[3]]
    levels: dict[float, list[tuple[float, float]]] = {}
    for x, y, width, height in horizontal:
        levels.setdefault(round(y + height / 2, 3), []).append((x, x + width))
    footer_rows = 1 if page_index + 1 == int(fixture["expected_pages"]) and fixture["footer"] else 0
    require(
        len(levels) == int(fixture["rows_per_page"]) + 2 + footer_rows,
        f"{fixture['name']} page {page_index} table row seam count differs",
    )
    for level, intervals in levels.items():
        ordered = sorted(intervals)
        require(close_geometry(ordered[0][0], left), f"page {page_index} seam {level} starts at {ordered[0][0]}")
        for previous, current in zip(ordered, ordered[1:], strict=False):
            require(
                close_geometry(previous[1], current[0]),
                f"page {page_index} seam {level} overlaps or has a gap: {ordered!r}",
            )
        require(close_geometry(ordered[-1][1], right), f"page {page_index} seam {level} ends at {ordered[-1][1]}")


def verify_border_geometry(
    operations: list[dict[str, Any]],
    fixture: dict[str, Any],
    page_index: int,
) -> tuple[float, ...]:
    rectangles = [
        border_rectangle(operation, fixture, page_index) for operation in operations if border_operation(operation)
    ]
    require(rectangles, f"{fixture['name']} page {page_index} has no table borders")
    require(
        len(rectangles) == len(set(rectangles)),
        f"{fixture['name']} page {page_index} contains duplicate table borders",
    )
    vertical_centers = sorted({round(x + width / 2, 3) for x, _, width, height in rectangles if height > width})
    expected = [number(value, "expected column boundary") for value in fixture["column_boundaries"]]
    require(
        len(vertical_centers) == len(expected)
        and all(close_geometry(actual, target) for actual, target in zip(vertical_centers, expected, strict=True)),
        f"{fixture['name']} page {page_index} column grid differs: {vertical_centers!r}",
    )
    verify_horizontal_seams(rectangles, fixture, page_index)
    return tuple(vertical_centers)


def verify_text_geometry(
    text_operations: list[dict[str, Any]],
    fixture: dict[str, Any],
    page_index: int,
) -> None:
    expected_x = expected_token_x(fixture, page_index)
    require(len(text_operations) == len(expected_x), f"{fixture['name']} page {page_index} text count differs")
    for index, (operation, target) in enumerate(zip(text_operations, expected_x, strict=True)):
        actual = text_coordinate(operation, "x", f"{fixture['name']} page {page_index} text {index}")
        require(
            close_geometry(actual, target),
            f"{fixture['name']} page {page_index} text {index} x drifted: {actual} != {target}",
        )

    cursor = 1 if page_index == 0 else 0
    header_y = [
        text_coordinate(operation, "y", f"{fixture['name']} page {page_index} header")
        for operation in text_operations[cursor : cursor + len(fixture["headers"])]
    ]
    require(max(header_y) - min(header_y) <= GEOMETRY_TOLERANCE, f"{fixture['name']} header baseline drifted")
    cursor += len(fixture["headers"])
    row_y = []
    for _ in range(int(fixture["rows_per_page"])):
        values = [
            text_coordinate(operation, "y", f"{fixture['name']} page {page_index} row")
            for operation in text_operations[cursor : cursor + len(fixture["headers"])]
        ]
        require(max(values) - min(values) <= GEOMETRY_TOLERANCE, f"{fixture['name']} row baseline drifted")
        row_y.append(sum(values) / len(values))
        cursor += len(fixture["headers"])
    require(
        header_y[0] < row_y[0] and all(first < second for first, second in zip(row_y, row_y[1:], strict=False)),
        f"{fixture['name']} page {page_index} row order or vertical geometry differs",
    )
    if fixture["footer"] and page_index + 1 == int(fixture["expected_pages"]):
        footer_y = [
            text_coordinate(operation, "y", f"{fixture['name']} footer") for operation in text_operations[cursor:]
        ]
        require(
            footer_y and max(footer_y) - min(footer_y) <= GEOMETRY_TOLERANCE and min(footer_y) > row_y[-1],
            f"{fixture['name']} footer geometry differs",
        )


def verify_pagination_trace(layout: dict[str, Any], fixture: dict[str, Any]) -> None:
    sequence = object_value(layout.get("page_sequence"), f"{fixture['name']} page sequence")
    expected_pages = int(fixture["expected_pages"])
    pages = list_value(sequence.get("pages"), f"{fixture['name']} layout pages")
    require(
        len(pages) == expected_pages
        and [page.get("index") for page in pages if isinstance(page, dict)] == list(range(expected_pages)),
        f"{fixture['name']} layout page indexes differ",
    )
    expected_resumes = list(range(1, expected_pages))
    expected_rows = [1 + int(fixture["rows_per_page"]) * page for page in expected_resumes]
    continuations = [
        object_value(item, "table continuation")
        for item in list_value(sequence.get("continuations"), f"{fixture['name']} continuations")
        if isinstance(item, dict) and item.get("token", {}).get("kind") == "table"
    ]
    require(
        [item["token"].get("resume_page_index") for item in continuations] == expected_resumes
        and [item["token"].get("next_row_index") for item in continuations] == expected_rows,
        f"{fixture['name']} table continuation trace differs",
    )
    table_nodes = {item["token"].get("table_node") for item in continuations}
    row_groups = {item["token"].get("row_group_index") for item in continuations}
    require(
        len(table_nodes) == 1 and None not in table_nodes and len(row_groups) == 1 and None not in row_groups,
        f"{fixture['name']} table continuation identity differs",
    )

    repeats = [
        object_value(item, "table repeat")
        for item in list_value(sequence.get("table_group_repeats"), f"{fixture['name']} header repeats")
    ]
    require(
        [item.get("page_index") for item in repeats] == expected_resumes
        and {item.get("table_node") for item in repeats} == table_nodes
        and len({item.get("header_tag_id") for item in repeats}) == 1,
        f"{fixture['name']} header repeat identity differs",
    )
    repeat_sizes = [number(item.get("block_size"), "repeated header block size") for item in repeats]
    require(
        all(close_geometry(value, number(fixture["header_height"], "header height")) for value in repeat_sizes)
        and [number(item.get("target_block_start"), "repeat target") for item in repeats]
        == sorted(number(item.get("target_block_start"), "repeat target") for item in repeats),
        f"{fixture['name']} repeated header geometry differs",
    )

    decisions = [
        object_value(item, "table break")
        for item in list_value(sequence.get("table_breaks"), f"{fixture['name']} table breaks")
    ]
    selected = [item for item in decisions if item.get("selected") is True]
    require(
        [item.get("next_row_index") for item in selected] == expected_rows
        and [item.get("resume_page_index") for item in selected] == expected_resumes
        and all(item.get("constraint") == fixture["break_constraint"] for item in selected)
        and all(item.get("retry_count") == 0 for item in selected),
        f"{fixture['name']} selected table break trace differs",
    )
    require(
        {item.get("table_node") for item in selected} == table_nodes
        and {item.get("row_group_index") for item in selected} == row_groups,
        f"{fixture['name']} selected table break identity differs",
    )
    require(sequence.get("table_cell_continuations", []) == [], f"{fixture['name']} split a table row")
    require(sequence.get("warnings", []) == [], f"{fixture['name']} emitted pagination warnings")


def verify_scene_and_trace(
    scene: dict[str, Any],
    layout: dict[str, Any],
    fixture: dict[str, Any],
) -> set[str]:
    require(scene.get("schema") == SCENE_SCHEMA and scene.get("version") == 1, "scene schema differs")
    pages = list_value(scene.get("pages"), f"{fixture['name']} scene pages")
    expected_pages = int(fixture["expected_pages"])
    require(len(pages) == expected_pages, f"{fixture['name']} page count differs")

    font_ids = set()
    border_signatures = []
    for page_index, raw_page in enumerate(pages):
        page = object_value(raw_page, f"{fixture['name']} scene page {page_index}")
        require(page.get("size") == {"width": 612.0, "height": 792.0}, f"page {page_index} size differs")
        operations = [
            object_value(operation, f"{fixture['name']} scene operation")
            for operation in list_value(page.get("operations"), f"{fixture['name']} scene operations")
        ]
        text_operations = [
            operation
            for operation in operations
            if operation.get("type") == "text" and canonical_text(str(operation.get("text", "")))
        ]
        actual_text = [canonical_text(str(operation.get("text", ""))) for operation in text_operations]
        expected_text = expected_page_tokens(fixture, page_index)
        require(actual_text == expected_text, f"{fixture['name']} page {page_index} text/cell oracle differs")
        for operation in text_operations:
            font = operation.get("font")
            require(isinstance(font, str) and bool(font), f"{fixture['name']} text has no font identity")
            font_ids.add(font)

        headers_start = 1 if page_index == 0 else 0
        headers = text_operations[headers_start : headers_start + len(fixture["headers"])]
        require(
            all(is_repeated_header(operation) == (page_index > 0) for operation in headers),
            f"{fixture['name']} page {page_index} header artifact state differs",
        )
        require(
            not any(is_repeated_header(operation) for operation in text_operations[len(headers) + headers_start :]),
            f"{fixture['name']} page {page_index} marks body text as a repeated artifact",
        )
        verify_text_geometry(text_operations, fixture, page_index)
        border_signatures.append(verify_border_geometry(operations, fixture, page_index))
    require(len(set(border_signatures)) == 1, f"{fixture['name']} border columns drifted across pages")
    verify_pagination_trace(layout, fixture)
    return font_ids


def verify_scene_bundle(
    summary: dict[str, Any],
    fixture: dict[str, Any],
) -> tuple[bytes, dict[str, Any], set[str]]:
    scene_path = artifact_path(summary, "scene_artifact", "scene artifact")
    scene_bytes = scene_path.read_bytes()
    scene = read_object(scene_path, "scene artifact")
    scene_hash = f"sha256:{sha256_bytes(scene_bytes)}"
    scene_summary = object_value(summary.get("scene"), "terminal scene summary")
    require(
        scene_summary.get("schema") == SCENE_SCHEMA
        and scene_summary.get("version") == 1
        and scene_summary.get("hash") == scene_hash
        and scene_summary.get("validation") == "valid"
        and scene_summary.get("capture_status") == "complete"
        and scene_summary.get("capture_code") is None
        and scene_summary.get("preview_status") == "rendered"
        and scene_summary.get("unsupported_event_count") == 0
        and scene_summary.get("text_mapping_gap_count") == 0,
        f"{fixture['name']} terminal scene summary differs",
    )
    report = read_object(artifact_path(summary, "scene_report", "scene report"), "scene report")
    require(
        report.get("scene") == {"schema": SCENE_SCHEMA, "version": 1, "hash": scene_hash, "validation": "valid"},
        f"{fixture['name']} scene report identity differs",
    )
    capture = object_value(report.get("capture"), "scene capture report")
    require(
        capture.get("status") == "complete"
        and capture.get("code") is None
        and capture.get("unsupported_events") == []
        and capture.get("text_mapping_gaps") == [],
        f"{fixture['name']} scene capture is incomplete",
    )
    require(
        report.get("document_pdf", {}).get("status") == "rendered"
        and report.get("document_pdf", {}).get("error") is None
        and report.get("pdf_structure", {}).get("status") == "rendered"
        and report.get("pdf_structure", {}).get("error") is None,
        f"{fixture['name']} report PDF status differs",
    )
    preview = object_value(report.get("preview"), "scene preview report")
    scene_pages = list_value(scene.get("pages"), "scene pages")
    first_page = object_value(scene_pages[0], "first scene page")
    first_operations = [
        object_value(operation, "first scene operation")
        for operation in list_value(first_page.get("operations"), "first scene operations")
    ]
    require(
        preview.get("status") == "rendered"
        and preview.get("artifact") is None
        and preview.get("page_count") == int(fixture["expected_pages"])
        and len(list_value(preview.get("pages"), "preview report pages")) == len(scene_pages)
        and preview.get("page_size") == first_page.get("size")
        and preview.get("operation_counts") == operation_counts(first_operations)
        and preview.get("unsupported") == [],
        f"{fixture['name']} scene preview report differs",
    )
    layout = read_object(artifact_path(summary, "layout_debug", "layout artifact"), "layout artifact")
    return scene_bytes, scene, verify_scene_and_trace(scene, layout, fixture)


def verify_fonts(summary: dict[str, Any], referenced_fonts: set[str]) -> bytes:
    path = artifact_path(summary, "fonts_artifact", "font report")
    contents = path.read_bytes()
    fonts = read_object(path, "font report")
    require(
        fonts.get("schema") == "pliego.font-report"
        and fonts.get("version") == 1
        and fonts.get("policy") == {"host_fonts": "denied"},
        "font report schema or policy differs",
    )
    expected_resource = f"sha256:{SANITIZED_AHEM_SHA256}"
    resources_by_id: dict[str, bytes] = {}
    for raw_resource in list_value(fonts.get("font_resources"), "font resources"):
        resource = object_value(raw_resource, "font resource")
        address = resource.get("resource")
        encoded = resource.get("bytes_base64")
        require(isinstance(address, str) and isinstance(encoded, str), "font resource address differs")
        try:
            decoded = base64.b64decode(encoded, validate=True)
        except ValueError as error:
            fail(f"font resource {address} is not strict base64: {error}")
        require(base64.b64encode(decoded).decode("ascii") == encoded, "font resource base64 is not canonical")
        require(address == f"sha256:{sha256_bytes(decoded)}", f"font resource {address} is not content addressed")
        resource_path = artifacts_directory(summary) / "resources" / address.removeprefix("sha256:")
        require(resource_path.read_bytes() == decoded, f"font resource bytes differ: {resource_path}")
        resources_by_id[address] = decoded
    require(
        set(resources_by_id) == {expected_resource}, f"unexpected document font resources: {set(resources_by_id)!r}"
    )

    instances_by_id = {}
    for raw_instance in list_value(fonts.get("font_instances"), "font instances"):
        instance = object_value(raw_instance, "font instance")
        identifier = instance.get("id")
        require(
            isinstance(identifier, str)
            and instance.get("resource") == expected_resource
            and isinstance(instance.get("face_index"), int)
            and isinstance(instance.get("variations"), list),
            f"font instance differs: {instance!r}",
        )
        instances_by_id[identifier] = instance
    require(referenced_fonts == set(instances_by_id), "scene and font-report instance identities differ")

    selections = [
        object_value(selection, "font selection")
        for selection in list_value(fonts.get("selections"), "font selections")
    ]
    require(bool(selections), "font selection report is empty")
    require(
        all(
            selection.get("source") == "bundled"
            and selection.get("resource") == expected_resource
            and selection.get("instance") in referenced_fonts
            and selection.get("requested_families") == ["Ahem"]
            and str(selection.get("selected_family", "")).casefold() == "ahem"
            for selection in selections
        ),
        f"document font selection differs: {selections!r}",
    )
    manifest = object_value(fonts.get("manifest"), "font manifest")
    require(
        manifest.get("resolution") == "css-order" and manifest.get("entries") == selections,
        "font manifest and selections differ",
    )
    require(fonts.get("warnings") == [], f"document font resolution warned: {fonts.get('warnings')!r}")
    return contents


def extract_pdf_pages(pdf: Path, destination: Path) -> tuple[bytes, list[str]]:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the document gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-layout", str(pdf), "-"],
        capture_output=True,
        timeout=120,
        check=False,
    )
    require(result.returncode == 0, f"pdftotext failed: {result.stderr.decode(errors='replace')}")
    destination.write_bytes(result.stdout)
    value = result.stdout.decode("utf-8-sig")
    pages = value.split("\f")
    while pages and not canonical_text(pages[-1]):
        pages.pop()
    return result.stdout, pages


def verify_pdf(
    summary: dict[str, Any],
    scene: dict[str, Any],
    fixture: dict[str, Any],
    published_pdf: Path,
    extraction_path: Path,
) -> tuple[bytes, bytes]:
    require(summary.get("document_pdf_status") == "rendered", f"{fixture['name']} PDF status differs")
    require(summary.get("pdf_structure_status") == "rendered", f"{fixture['name']} PDF structure status differs")
    pdf_bytes = published_pdf.read_bytes()
    require(
        pdf_bytes.startswith(b"%PDF-")
        and b"/ToUnicode" in pdf_bytes
        and (b"/FontFile2" in pdf_bytes or b"/FontFile3" in pdf_bytes),
        f"{fixture['name']} PDF lacks its header, Unicode map, or embedded font",
    )
    artifact_pdf = artifacts_directory(summary) / "document.pdf"
    require(artifact_pdf.read_bytes() == pdf_bytes, f"{fixture['name']} artifact and published PDFs differ")
    require(
        artifact_path(summary, "document_pdf", "published PDF") == published_pdf.resolve(), "published PDF path differs"
    )

    structure = read_object(artifact_path(summary, "pdf_structure", "PDF structure"), "PDF structure")
    structure_pages = list_value(structure.get("pages"), "PDF structure pages")
    scene_pages = list_value(scene.get("pages"), "scene pages")
    require(
        structure.get("schema") == "pliego.pdf-structure"
        and structure.get("version") == 1
        and structure.get("backend") == "krilla"
        and structure.get("page_count") == int(fixture["expected_pages"])
        and len(structure_pages) == len(scene_pages),
        f"{fixture['name']} PDF structure schema or page count differs",
    )
    require(
        structure.get("pdf")
        == {
            "artifact": "document.pdf",
            "sha256": f"sha256:{sha256_bytes(pdf_bytes)}",
            "bytes": len(pdf_bytes),
        },
        f"{fixture['name']} PDF structure identity differs",
    )
    for page_index, (raw_scene_page, raw_structure_page) in enumerate(zip(scene_pages, structure_pages, strict=True)):
        scene_page = object_value(raw_scene_page, f"scene page {page_index}")
        structure_page = object_value(raw_structure_page, f"PDF page {page_index}")
        size = object_value(scene_page.get("size"), f"scene page {page_index} size")
        operations = [
            object_value(operation, f"scene page {page_index} operation")
            for operation in list_value(scene_page.get("operations"), "scene operations")
        ]
        expected_unicode = "".join(
            str(operation.get("text")) for operation in operations if operation.get("type") == "text"
        )
        expected_fonts = list(
            dict.fromkeys(str(operation.get("font")) for operation in operations if operation.get("type") == "text")
        )
        require(
            structure_page.get("index") == page_index
            and structure_page.get("scene_page_size_css_px") == size
            and structure_page.get("media_box_pt")
            == [
                0.0,
                0.0,
                number(size.get("width"), "page width") * CSS_PX_TO_PDF_PT,
                number(size.get("height"), "page height") * CSS_PX_TO_PDF_PT,
            ]
            and structure_page.get("expected_extracted_unicode") == expected_unicode
            and structure_page.get("expected_extracted_unicode") == "".join(expected_page_tokens(fixture, page_index))
            and structure_page.get("embedded_font_ids") == expected_fonts
            and structure_page.get("operation_counts") == operation_counts(operations),
            f"{fixture['name']} PDF page {page_index} does not join exactly to the scene",
        )

    extracted_bytes, extracted_pages = extract_pdf_pages(published_pdf, extraction_path)
    require(len(extracted_pages) == len(scene_pages), f"{fixture['name']} extracted PDF page count differs")
    for page_index, extracted in enumerate(extracted_pages):
        require(
            canonical_text(extracted) == " ".join(expected_page_tokens(fixture, page_index)),
            f"{fixture['name']} extracted PDF page {page_index} text differs",
        )
    return pdf_bytes, extracted_bytes


def verify_previews(
    summary: dict[str, Any],
    scene: dict[str, Any],
    fixture: dict[str, Any],
) -> tuple[bytes, ...]:
    pages_manifest = read_object(artifact_path(summary, "pages_artifact", "page preview manifest"), "page manifest")
    pages = list_value(pages_manifest.get("pages"), "page preview records")
    scene_pages = list_value(scene.get("pages"), "scene pages")
    require(
        pages_manifest.get("schema") == "pliego.pages"
        and pages_manifest.get("version") == 1
        and pages_manifest.get("page_count") == int(fixture["expected_pages"])
        and len(pages) == len(scene_pages),
        f"{fixture['name']} page preview manifest differs",
    )
    contents = []
    artifacts = artifacts_directory(summary)
    for page_index, (raw_page, raw_scene_page) in enumerate(zip(pages, scene_pages, strict=True)):
        page = object_value(raw_page, f"{fixture['name']} preview {page_index}")
        scene_page = object_value(raw_scene_page, f"{fixture['name']} scene page {page_index}")
        operations = [
            object_value(operation, "scene operation")
            for operation in list_value(scene_page.get("operations"), "scene operations")
        ]
        require(
            page.get("index") == page_index
            and page.get("page_size") == scene_page.get("size")
            and page.get("operation_counts") == operation_counts(operations),
            f"{fixture['name']} preview {page_index} does not join to the scene",
        )
        preview = artifacts / str(page.get("artifact", ""))
        require(preview.is_file(), f"{fixture['name']} preview {page_index} is absent")
        try:
            with Image.open(preview) as image:
                require(image.format == "PNG" and image.size == (612, 792), "preview PNG metadata differs")
                image.load()
                pixels = image.convert("RGB")
        except OSError as error:
            fail(f"{fixture['name']} preview {page_index} is not decodable: {error}")
        white = Image.new("RGB", pixels.size, "white")
        require(
            ImageChops.difference(pixels, white).getbbox() is not None,
            f"{fixture['name']} preview {page_index} is blank",
        )
        contents.append(preview.read_bytes())
    return tuple(contents)


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


def phase_timings(summary: dict[str, Any]) -> dict[str, float]:
    timings = object_value(summary.get("phase_timings_ms"), "phase timings")
    require(set(timings) == set(PHASES), f"phase timing surface differs: {timings!r}")
    return {phase: round(number(timings[phase], f"{phase} timing"), 3) for phase in PHASES}


def clean_environment(destination: Path) -> dict[str, str]:
    home = destination / "home"
    cache = home / ".cache"
    config = home / ".config"
    data = home / ".local/share"
    runtime = home / ".runtime"
    temporary = destination / "tmp"
    for path in (home, cache, config, data, runtime, temporary):
        path.mkdir(parents=True, exist_ok=True)
    runtime.chmod(0o700)
    return {
        "HOME": str(home),
        "USERPROFILE": str(home),
        "XDG_CACHE_HOME": str(cache),
        "XDG_CONFIG_HOME": str(config),
        "XDG_DATA_HOME": str(data),
        "XDG_RUNTIME_DIR": str(runtime),
        "TMPDIR": str(temporary),
        "TMP": str(temporary),
        "TEMP": str(temporary),
    }


def run_document(
    binary: Path,
    fixtures_root: Path,
    fixture: dict[str, Any],
    destination: Path,
    run_number: int,
) -> tuple[dict[str, Any], dict[str, Any]]:
    destination.mkdir(parents=True)
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
    environment.update(clean_environment(destination))
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
        fail(f"{fixture['name']} run {run_number} exceeded {PROCESS_TIMEOUT_SECONDS}s")
    process_ms = round((time.perf_counter() - started) * 1000, 3)
    (destination / "stdout.log").write_text(result.stdout, encoding="utf-8")
    (destination / "stderr.log").write_text(result.stderr, encoding="utf-8")
    require(
        result.returncode == 0,
        f"{fixture['name']} run {run_number} exited with {result.returncode}: {result.stderr[-4000:]}",
    )
    summary = final_summary(result)
    require(summary.get("status") == "rendered", f"{fixture['name']} did not render: {summary!r}")
    scene_bytes, scene, referenced_fonts = verify_scene_bundle(summary, fixture)
    fonts_bytes = verify_fonts(summary, referenced_fonts)
    pdf_bytes, extracted_bytes = verify_pdf(
        summary,
        scene,
        fixture,
        output,
        destination / "extracted-text.txt",
    )
    preview_bytes = verify_previews(summary, scene, fixture)
    measurement = {
        "fixture": fixture["name"],
        "run": run_number,
        "warm_cold_state": "cold one-shot process with a clean home",
        "concurrency": 1,
        "process_total_ms": process_ms,
        "controlled_session_ms": session_span_ms(artifacts),
        "phase_timings_ms": phase_timings(summary),
        "pages": fixture["expected_pages"],
        "scene_hash": f"sha256:{sha256_bytes(scene_bytes)}",
    }
    fingerprints = {
        "scene": scene_bytes,
        "fonts": fonts_bytes,
        "pdf": pdf_bytes,
        "previews": preview_bytes,
        "extracted_text": extracted_bytes,
    }
    return measurement, fingerprints


def verify_determinism(
    fixture: dict[str, Any],
    first: dict[str, Any],
    second: dict[str, Any],
) -> None:
    for artifact in ("scene", "fonts", "pdf", "previews", "extracted_text"):
        require(
            first[artifact] == second[artifact],
            f"{fixture['name']} {artifact} differs across clean-home runs",
        )


def identify_binary(binary: Path) -> list[str]:
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    result = subprocess.run(
        [str(binary), "--version"],
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )
    require(result.returncode == 0 and result.stdout.strip(), "cannot identify the Pliego build")
    require(shutil.which("pdftotext") is not None, "pdftotext is required for the document gate")
    require(bool(Image.registered_extensions()), "Pillow cannot decode preview images")
    return result.stdout.strip().splitlines()


def benchmark_report(
    engine: list[str],
    build_profile: str,
    manifest: dict[str, Any],
    measurements: list[dict[str, Any]],
) -> dict[str, Any]:
    return {
        "schema": BENCHMARK_SCHEMA,
        "rights": manifest["rights"],
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
        "engine": engine,
        "inputs": [
            {
                "name": fixture["name"],
                "path": fixture["path"],
                "sha256": fixture["sha256"],
                "page_size": fixture["page_size"],
                "page_margins": fixture["page_margins"],
                "natural_overflow": fixture["natural_overflow"],
            }
            for fixture in manifest["fixtures"]
        ],
        "fonts": [manifest["font"]],
        "warm_cold_method": "two one-shot processes per fixture, each with an isolated empty HOME and XDG/TMP roots",
        "concurrency": 1,
        "measurements": measurements,
        "output_validation_method": [
            "full expected caption, header, body-cell, footer, and total text per physical page",
            "exact scene/PDF structure joins, PDF hashes and bytes, media boxes, fonts, and operation counts",
            "decoded nonblank page PNGs and byte-identical clean-home reruns",
            "column text positions, complete border grids, visible edge rectangles, and continuous row seams",
            "pdftotext page count, text identity, order, and clean-home determinism",
            "table continuation rows, break constraints, identities, header repeats, and warning traces",
        ],
        "limitations": [
            "Wall-clock timings are environment-sensitive and are a correctness-gated baseline, not a competitive claim.",
            "Phase spans report current CLI boundaries and can overlap; they are not an additive decomposition.",
        ],
    }


def synthetic_path(x: float, y: float, width: float, height: float) -> dict[str, Any]:
    return {
        "type": "path",
        "bounds": {"x": x, "y": y, "width": width, "height": height},
        "data": f"M{x} {y}h{width}v{height}h-{width}z",
        "fill": {"r": 0.1, "g": 0.2, "b": 0.3, "a": 1.0},
        "stroke": None,
        "meta": {"semantics": {"role": "artifact", "label": "table-border"}},
    }


def synthetic_scene_and_layout(fixture: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    pages = []
    boundaries = [float(value) for value in fixture["column_boundaries"]]
    for page_index in range(int(fixture["expected_pages"])):
        tokens = expected_page_tokens(fixture, page_index)
        xs = expected_token_x(fixture, page_index)
        operations = []
        cursor = 0
        if page_index == 0:
            operations.append(
                {"type": "text", "text": tokens[0], "font": "font:ahem", "glyphs": [{"x": xs[0], "y": 16.0}]}
            )
            cursor = 1
        for index, (token, x) in enumerate(zip(tokens[cursor:], xs[cursor:], strict=True)):
            row = index // len(fixture["headers"])
            y = 48.0 + row * 20.0
            operation: dict[str, Any] = {
                "type": "text",
                "text": token,
                "font": "font:ahem",
                "glyphs": [{"x": x, "y": y}],
            }
            if page_index > 0 and index < len(fixture["headers"]):
                operation["meta"] = {"semantics": {"role": "artifact", "label": "repeated-table-header"}}
            operations.append(operation)
        table_bottom = 700.0
        for boundary in boundaries:
            operations.append(synthetic_path(boundary - 0.5, 32.0, 1.0, table_bottom - 32.0))
        footer_rows = 1 if page_index + 1 == int(fixture["expected_pages"]) and fixture["footer"] else 0
        seam_count = int(fixture["rows_per_page"]) + 2 + footer_rows
        for seam in range(seam_count):
            y = 32.0 + seam * 10.0
            for left, right in zip(boundaries[:-1], boundaries[1:], strict=True):
                operations.append(synthetic_path(left, y - 0.5, right - left, 1.0))
        pages.append({"size": {"width": 612.0, "height": 792.0}, "operations": operations})

    resumes = list(range(1, int(fixture["expected_pages"])))
    rows = [1 + int(fixture["rows_per_page"]) * page for page in resumes]
    continuations = [
        {
            "page_index": page - 1,
            "token": {
                "kind": "table",
                "child_index": 0,
                "table_node": 42,
                "row_group_index": 1,
                "next_row_index": row,
                "resume_page_index": page,
            },
        }
        for page, row in zip(resumes, rows, strict=True)
    ]
    repeats = [
        {
            "page_index": page,
            "table_node": 42,
            "header_tag_id": 7,
            "row_group_index": 0,
            "source_block_start": 32.0,
            "target_block_start": page * 792.0 + 32.0,
            "block_size": fixture["header_height"],
        }
        for page in resumes
    ]
    breaks = [
        {
            "page_index": page - 1,
            "table_node": 42,
            "row_group_index": 1,
            "next_row_index": row,
            "constraint": fixture["break_constraint"],
            "retry_count": 0,
            "retry_limit": 2,
            "selected": True,
            "resume_page_index": page,
        }
        for page, row in zip(resumes, rows, strict=True)
    ]
    scene = {"schema": SCENE_SCHEMA, "version": 1, "pages": pages}
    layout = {
        "page_sequence": {
            "pages": [{"index": page} for page in range(int(fixture["expected_pages"]))],
            "continuations": continuations,
            "table_group_repeats": repeats,
            "table_breaks": breaks,
            "table_cell_continuations": [],
            "warnings": [],
        }
    }
    return scene, layout


def self_test(directory: Path) -> None:
    manifest = validate_fixture_pins(directory)
    for fixture in manifest["fixtures"]:
        scene, layout = synthetic_scene_and_layout(fixture)
        require(
            verify_scene_and_trace(scene, layout, fixture) == {"font:ahem"},
            f"{fixture['name']} self-test font identity differs",
        )
    fixture = manifest["fixtures"][0]
    scene, layout = synthetic_scene_and_layout(fixture)
    boundaries = [float(value) for value in fixture["column_boundaries"]]
    operations = scene["pages"][0]["operations"]
    for left, right in zip(boundaries[:-1], boundaries[1:], strict=True):
        operations.append(synthetic_path(left, 499.5, right - left, 1.0))
    error_output = io.StringIO()
    try:
        with redirect_stderr(error_output):
            verify_scene_and_trace(scene, layout, fixture)
    except SystemExit:
        require("table row seam count differs" in error_output.getvalue(), "extra-seam failure differs")
    else:
        fail("extra table row seam was accepted")
    report = benchmark_report(["self-test"], "self-test", manifest, [])
    require(
        report["schema"] == BENCHMARK_SCHEMA
        and report["build_profile"] == "self-test"
        and report["fonts"] == [manifest["font"]]
        and len(report["output_validation_method"]) == 6,
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
    require(not output.exists(), f"output directory already exists: {output}")
    require(bool(build_profile), "build profile is empty")
    engine = identify_binary(binary)
    manifest = validate_fixture_pins(directory)
    output.mkdir(parents=True)
    fixtures_root = directory.parent
    measurements = []
    for fixture in manifest["fixtures"]:
        first_measurement, first_artifacts = run_document(
            binary,
            fixtures_root,
            fixture,
            output / fixture["name"] / "run-1",
            1,
        )
        second_measurement, second_artifacts = run_document(
            binary,
            fixtures_root,
            fixture,
            output / fixture["name"] / "run-2",
            2,
        )
        verify_determinism(fixture, first_artifacts, second_artifacts)
        measurements.extend((first_measurement, second_measurement))
    report = benchmark_report(engine, build_profile, manifest, measurements)
    (output / "benchmark.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"document gate check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
