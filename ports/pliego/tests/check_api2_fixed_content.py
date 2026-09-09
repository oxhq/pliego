#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Check the bounded page-root fixed Flow/text/inline API 2 profile.

Twelve forced normal-flow pages exercise fresh decimal page/page-count text,
including the 9-to-10 width change. Exact Ahem baselines and text extents bind
centering and margin placement to source geometry. Fixed siblings cannot alter
body rows or pagination. Unsupported descendants/insets/counter forms must fail
closed with the specific retained unsupported marker, never an arbitrary error.
The pinned Stylo writing-mode preference is disabled: the separately named
fallback case proves CSS.supports/CSSOM absence plus exact horizontal output,
not vertical-layout support. A Rust predicate unit covers computed vertical
rejection independently of that disabled author-property surface.
This is a native scene/PDF regression, not general CSS fixed-content support,
business-document visual qualification, or a performance benchmark.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path
from typing import Any
from unittest.mock import patch

from check_api2_contract import (
    PROBE_TIMEOUT_SECONDS,
    build_execution_fixture,
    canonical_json,
    create_private_job_root,
    run_probe,
    sha256_bytes,
    sha256_file,
    validate_probe,
    verify_artifact,
)


PAGE_SIZE = {"width": 24000, "height": 18000}  # 400 by 300 CSS px.
MARGINS = {"top": 1800, "right": 1800, "bottom": 3600, "left": 1800}
FONT_SIZE = 720  # 12px Ahem, 0.8em ascent, 0.2em descent.
LINE_HEIGHT = 1200  # 20px: 4px half-leading plus 9.6px ascent.
BASELINE_IN_LINE = 816
FONT_PATH = Path(__file__).parent / "fixtures/text-scene/Ahem.ttf"
BODY_TOKEN = re.compile(r"BODY[0-9]{2}|TAIL[0-9]{2}")
CASES = (
    {"name": "plain-normal-flow", "pages": 12, "plain": True},
    {"name": "fixed-before", "pages": 12, "placement": "before"},
    {"name": "fixed-between", "pages": 12, "placement": "between"},
    {"name": "fixed-after", "pages": 12, "placement": "after"},
    {"name": "fixed-wrapper", "pages": 12, "wrapper": True},
    {"name": "fixed-with-tables", "pages": 12, "table_body": True},
    {"name": "fixed-one-page", "pages": 1},
    {"name": "fixed-top-one-page", "pages": 1, "top": True},
    {"name": "fixed-literal-zero", "pages": 12, "literal_zero": True},
    {"name": "reject-nested-fixed", "pages": 1, "reject": "nested"},
    {"name": "reject-absolute-child", "pages": 1, "reject": "absolute"},
    {"name": "reject-float-child", "pages": 1, "reject": "float"},
    {"name": "reject-replaced-child", "pages": 1, "reject": "replaced"},
    {"name": "reject-table-footer", "pages": 1, "reject": "table"},
    {"name": "reject-both-auto-insets", "pages": 1, "reject": "auto"},
    {"name": "reject-counter-outside-fixed", "pages": 1, "reject": "outside"},
    {"name": "reject-counter-form", "pages": 1, "reject": "form"},
    {"name": "reject-counter-name", "pages": 1, "reject": "name"},
    {"name": "reject-counter-style", "pages": 1, "reject": "style"},
    {"name": "disabled-writing-mode-fallback", "pages": 1, "disabled_writing_mode": True},
    {"name": "reject-relative-inline", "pages": 1, "reject": "relative"},
    {"name": "reject-counter-transform", "pages": 1, "reject": "transform"},
    {"name": "reject-counter-security", "pages": 1, "reject": "security"},
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def footer_text(case: dict[str, Any], page: int) -> str:
    return ("Literal 0 " if case.get("literal_zero") else "") + f"Page {page} of {case['pages']}"


def footer_top(case: dict[str, Any]) -> int:
    # The fixed containing block is the page content rectangle. bottom:-40px
    # places this 20px line at y=260px; top:-25px places it at y=5px.
    if case.get("top"):
        return MARGINS["top"] - 25 * 60
    return PAGE_SIZE["height"] - MARGINS["bottom"] + 40 * 60 - LINE_HEIGHT


def check_disabled_writing_mode(diagnostics: list[dict[str, Any]]) -> dict[str, Any]:
    payloads = [item.get("readiness", {}).get("payload") for item in diagnostics if "readiness" in item]
    expected = {
        "fixture": "disabled-writing-mode-fallback",
        "supports": False,
        "computedWritingMode": "",
    }
    require(
        payloads == [expected] and type(payloads[0]["supports"]) is bool,
        "disabled writing-mode CSSOM/support evidence differs or is missing",
    )
    return expected


def text_bounds(operations: list[dict[str, Any]]) -> tuple[int, int, int]:
    glyphs = [glyph for operation in operations for glyph in operation.get("glyphs", [])]
    require(bool(glyphs), "text has no retained glyphs")
    require(
        all(type(glyph.get(key)) is int for glyph in glyphs for key in ("x", "y", "advance")),
        "text geometry lost integer app-unit authority",
    )
    require(all(glyph["advance"] > 0 for glyph in glyphs), "fixture glyph has invalid advance")
    ys = {glyph["y"] for glyph in glyphs}
    require(len(ys) == 1, "fixture text unexpectedly wrapped or has mixed baselines")
    return min(glyph["x"] for glyph in glyphs), max(glyph["x"] + glyph["advance"] for glyph in glyphs), ys.pop()


def check_scene(scene: dict[str, Any], case: dict[str, Any], font_resource: str) -> dict[str, Any]:
    require(scene.get("schema") == "pliego.document-scene" and scene.get("version") == 2, "not Scene v2")
    require(scene.get("app_units_per_css_px") == 60, "scene app-unit scale changed")
    pages = scene.get("pages", [])
    require(len(pages) == case["pages"], "fixed content altered the forced normal-flow page count")
    footer_geometry = []
    for index, page in enumerate(pages, 1):
        require(page.get("number") == index, "page numbering is missing, duplicated, or out of order")
        require(page.get("style_source") == "request-defaults", "fixture page is not request-owned")
        require(
            page.get("size_app_units") == PAGE_SIZE and page.get("margins_app_units") == MARGINS,
            "page geometry differs from source request",
        )
        operations = page.get("operations", [])
        require(
            bool(operations) and all(op.get("type") == "text" for op in operations),
            "unexpected non-text or empty fixture scene",
        )
        require(all(op.get("font_size_app_units") == FONT_SIZE for op in operations), "fixture font size changed")
        require(
            all(
                op.get("font")
                == {"resource": font_resource, "face_index": 0, "variations": [], "synthetic_bold": False}
                for op in operations
            ),
            "fixture text did not use the exact resolved Ahem resource/face without synthesis",
        )
        body = [op for op in operations if BODY_TOKEN.search(op.get("text", ""))]
        footer = [op for op in operations if op not in body]
        body_text = "".join(op["text"] for op in body)
        expected = [f"BODY{index:02}", f"TAIL{index:02}"]
        require(BODY_TOKEN.findall(body_text) == expected, "body row text lost, duplicated, aliased, or out of order")
        require(not BODY_TOKEN.sub("", body_text).strip(), "body row contains unexpected text")
        for row, token in enumerate(expected):
            row_ops = [op for op in body if token in op["text"]]
            require(len(row_ops) == 1 and row_ops[0]["text"] == token, "body token is ambiguous or split")
            left, right, baseline = text_bounds(row_ops)
            require(
                left == MARGINS["left"] and right - left == len(token) * FONT_SIZE,
                "body row horizontal geometry changed",
            )
            require(
                baseline == MARGINS["top"] + row * LINE_HEIGHT + BASELINE_IN_LINE,
                "fixed sibling poisoned body pagination or placement",
            )
        if case.get("plain"):
            require(not footer, "plain normal flow gained fixed content")
            continue
        require(bool(footer), "fixed footer is missing")
        ordered = sorted(footer, key=lambda op: text_bounds([op])[0])
        actual = "".join(op["text"] for op in ordered)
        expected_footer = footer_text(case, index)
        require(
            actual == expected_footer, "footer text is stale, missing, duplicated, reordered, or changed literal zero"
        )
        left, right, baseline = text_bounds(footer)
        width = len(expected_footer) * FONT_SIZE
        expected_left = (PAGE_SIZE["width"] - width) // 2
        require(
            left == expected_left and right == expected_left + width,
            "footer is not freshly shaped and centered at its exact page-specific width",
        )
        top = footer_top(case)
        require(baseline == top + BASELINE_IN_LINE, "fixed footer differs from the authored top/bottom inset")
        bottom = top + LINE_HEIGHT
        require(0 <= top < bottom <= PAGE_SIZE["height"], "fixed line escaped the physical page")
        require(
            bottom <= MARGINS["top"] if case.get("top") else top >= PAGE_SIZE["height"] - MARGINS["bottom"],
            "fixed line overlaps the normal-flow content area",
        )
        footer_geometry.append(
            {
                "page": index,
                "text": actual,
                "left": left,
                "right": right,
                "baseline": baseline,
                "line_top": top,
                "line_bottom": bottom,
            }
        )
    return {"pages": len(pages), "body_rows": 2 * len(pages), "footer_geometry": footer_geometry}


def check_pdf_text(text: str, case: dict[str, Any]) -> None:
    pages = text.split("\f")
    if pages[-1].strip() == "":
        pages.pop()
    require(len(pages) == case["pages"], "PDF extraction page count differs")
    for index, page in enumerate(pages, 1):
        tokens = BODY_TOKEN.findall(page)
        require(tokens == [f"BODY{index:02}", f"TAIL{index:02}"], "PDF body rows changed")
        remainder = " ".join(BODY_TOKEN.sub("", page).split())
        require(
            remainder == ("" if case.get("plain") else footer_text(case, index)),
            "PDF footer text differs from its page",
        )


def font_tables(data: bytes) -> dict[bytes, bytes]:
    require(len(data) >= 12 and data[:4] == b"\x00\x01\x00\x00", "expected a TrueType sfnt font")
    count = struct.unpack_from(">H", data, 4)[0]
    require(1 <= count <= 64 and 12 + 16 * count <= len(data), "invalid font table directory")
    tables = {}
    for index in range(count):
        tag, _, offset, length = struct.unpack_from(">4sIII", data, 12 + index * 16)
        require(
            tag not in tables and offset >= 12 + 16 * count and offset + length <= len(data),
            "font table bounds/identity invalid",
        )
        tables[tag] = data[offset : offset + length]
    return tables


def resolve_font_resource(delivery: Path, bundle: dict[str, Any]) -> dict[str, Any]:
    # OTS may sanitize/repack the supplied font, changing its complete digest.
    # Bind the oracle to the independently verified bundle entry, not to a hash
    # chosen from scene operations; compare identifying/metric sfnt tables with
    # the source Ahem. Glyph encoding/cmap/loca/head bytes need not be identical.
    entries = [item for item in bundle["entries"] if item["path"].startswith("resources/")]
    require(len(entries) == 1, "fixture must publish exactly one font resource")
    entry = entries[0]
    require(entry["media_type"] == "application/octet-stream", "fixture resource is not a font artifact")
    require(entry["path"] == "resources/" + entry["sha256"].removeprefix("sha256:"), "font resource name/hash differs")
    path = verify_artifact(delivery, entry)
    raw, resolved = font_tables(FONT_PATH.read_bytes()), font_tables(path.read_bytes())
    compared = (b"name", b"hhea", b"hmtx", b"maxp", b"OS/2")
    require(
        all(tag in raw and raw[tag] == resolved.get(tag) for tag in compared),
        "resolved font does not preserve source Ahem identity/metrics",
    )
    return {
        "raw_input_sha256": sha256_file(FONT_PATH),
        "resolved_sha256": entry["sha256"],
        "resolved_bytes": entry["bytes"],
        "path": entry["path"],
        "preserved_tables": [tag.decode("ascii") for tag in compared],
    }


def one_pixel_png() -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(b"\0\0\0\0"))
        + chunk(b"IEND", b"")
    )


def build_fixture(repository: Path, root: Path, case: dict[str, Any]) -> tuple[bytes, Path]:
    template = (repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes()
    payload, source = build_execution_fixture(repository, root, template)
    sheets = []
    for index in range(1, case["pages"] + 1):
        rows = f'<div class="row">BODY{index:02}</div><div class="row">TAIL{index:02}</div>'
        if case.get("table_body"):
            rows = f"<table><tbody><tr><td>BODY{index:02}</td></tr><tr><td>TAIL{index:02}</td></tr></tbody></table>"
        sheets.append(f'<section class="sheet{" first" if index == 1 else ""}">{rows}</section>')
    content = '<span class="counter"></span>'
    reject = case.get("reject")
    if reject in ("nested", "absolute", "float"):
        style = {"nested": "position:fixed;top:0", "absolute": "position:absolute;top:0", "float": "float:left"}[reject]
        content += f'<span style="{style}">X</span>'
    elif reject == "replaced":
        content += '<img src="pixel.png" width="8" height="8" alt="">'
    elif reject == "table":
        content = f"<table><tbody><tr><td>{content}</td></tr></tbody></table>"
    footer = f'<div class="fixed">{content}</div>' if not case.get("plain") else ""
    placement = case.get("placement", "before")
    parts = [*sheets]
    parts.insert({"before": 0, "between": len(parts) // 2, "after": len(parts)}[placement], footer)
    body = "".join(parts)
    if case.get("wrapper"):
        body = f"<main><article>{body}</article></main>"
    if case.get("disabled_writing_mode"):
        # The pinned Stylo pref disables the property and its CSSOM getter.
        # Retain actual feature/computed-style evidence without altering text.
        body += (
            '<script>window.pliego.defer();addEventListener("load",()=>window.pliego.ready({'
            'fixture:"disabled-writing-mode-fallback",supports:CSS.supports("writing-mode","vertical-rl"),'
            'computedWritingMode:getComputedStyle(document.querySelector(".fixed")).getPropertyValue("writing-mode")'
            "}));</script>"
        )
    html = (
        '<!doctype html><html lang="en-US"><head><meta charset="utf-8"><link rel="stylesheet" href="styles.css"><title>Fixed page content</title></head><body>'
        + body
        + "</body></html>\n"
    )
    inset = "top:-25px" if case.get("top") else "bottom:-40px"
    if reject == "auto":
        inset = "top:auto;bottom:auto"
    counter = '"Page " counter(page) " of " counter(pages)'
    if case.get("literal_zero"):
        counter = '"Literal 0 Page " counter(page, decimal) " of " counter(pages, decimal)'
    if reject == "form":
        counter = 'counters(page, ".")'
    elif reject == "name":
        counter = "counter(chapter)"
    elif reject == "style":
        counter = "counter(page, lower-roman)"
    css = '@font-face{font-family:Ahem;src:url("font.ttf") format("truetype");}html,body{margin:0}body{font:12px/20px Ahem;color:#000}'
    css += ".sheet{break-before:page}.sheet.first{break-before:auto}.row{height:20px}table{border-collapse:collapse}td{height:20px;padding:0;border:0}"
    css += f".fixed{{position:fixed;left:0;right:0;{inset};height:20px;text-align:center}}.counter::after{{content:{counter}}}"
    if reject == "outside":
        css += ".row:first-child::after{content:counter(page)}"
    elif reject == "relative":
        css += ".counter{position:relative;left:1px}"
    elif reject == "transform":
        css += ".counter{text-transform:full-width}"
    elif reject == "security":
        css += ".counter{-webkit-text-security:disc}"
    if case.get("disabled_writing_mode"):
        css += ".fixed{writing-mode:vertical-rl}"
    (source / "input/document.html").write_text(html, encoding="utf-8", newline="\n")
    (source / "input/styles.css").write_text(css + "\n", encoding="utf-8", newline="\n")
    manifest = json.loads((source / "input-manifest.json").read_bytes())
    if reject == "replaced":
        (source / "input/pixel.png").write_bytes(one_pixel_png())
        manifest["entries"].append({"path": "pixel.png", "media_type": "image/png"})
    for entry in manifest["entries"]:
        path = source / "input" / entry["path"]
        entry.update(sha256=sha256_file(path), bytes=path.stat().st_size)
    manifest["entries"].sort(key=lambda entry: entry["path"])
    manifest_bytes = canonical_json(manifest)
    (source / "input-manifest.json").write_bytes(manifest_bytes)
    request = json.loads(payload)
    request["input"]["manifest"].update(sha256=sha256_bytes(manifest_bytes), bytes=len(manifest_bytes))
    request["page"]["size"] = {"width_app_units": PAGE_SIZE["width"], "height_app_units": PAGE_SIZE["height"]}
    request["page"]["margins_app_units"] = MARGINS
    return canonical_json(request), source


def process_timeout_seconds(value: str) -> int:
    try:
        seconds = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("process timeout must be an integer") from error
    if not 1 <= seconds <= PROBE_TIMEOUT_SECONDS:
        raise argparse.ArgumentTypeError(f"process timeout must be between 1 and {PROBE_TIMEOUT_SECONDS}")
    return seconds


def check_rejection(result: dict[str, Any], diagnostics: list[dict[str, Any]], case: dict[str, Any]) -> dict[str, Any]:
    require(result.get("error", {}).get("kind") == "artifact", "exclusion did not retain an artifact failure")
    expected = "unresolved-page-counter" if case["reject"] == "outside" else "paged-fixed-content"
    matches = [
        item
        for item in diagnostics
        if item.get("code") == "SCENE_ENCODING_FAILED" and expected in item.get("message", "")
    ]
    require(bool(matches), f"exclusion did not retain the specific {expected} marker")
    return {"outcome": "rejected-as-excluded", "kind": "artifact", "code": "SCENE_ENCODING_FAILED", "marker": expected}


def run_case(
    binary: Path, repository: Path, root: Path, case: dict[str, Any], probe: dict[str, Any], process_timeout: int
) -> dict[str, Any]:
    payload, source = build_fixture(repository, root, case)
    (root / "request.json").write_bytes(payload)
    job = root / "job"
    create_private_job_root(job)
    shutil.copy2(source / "input-manifest.json", job / "input-manifest.json")
    shutil.copytree(source / "input", job / "input")
    started = time.monotonic()
    try:
        completed = subprocess.run(
            [str(binary), "render-api2"],
            cwd=job,
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=process_timeout,
        )
    except subprocess.TimeoutExpired as error:
        (root / "stdout.json").write_bytes(error.stdout or b"")
        (root / "stderr.txt").write_bytes(error.stderr or b"")
        (root / "process.json").write_bytes(
            canonical_json(
                {
                    "outcome": "host-timeout",
                    "limit_seconds": process_timeout,
                    "wall_seconds": time.monotonic() - started,
                }
            )
        )
        raise
    (root / "stdout.json").write_bytes(completed.stdout)
    (root / "stderr.txt").write_bytes(completed.stderr)
    (root / "process.json").write_bytes(
        canonical_json(
            {
                "exit_code": completed.returncode,
                "limit_seconds": process_timeout,
                "wall_seconds": time.monotonic() - started,
            }
        )
    )
    require(not completed.stderr, "API 2 emitted stderr")
    result = json.loads(completed.stdout)
    require(canonical_json(result) == completed.stdout, "result is not canonical compact JSON plus LF")
    require(
        result.get("schema") == "pliego.render-result" and result.get("version") == 1 and result.get("api") == 2,
        "not RenderResult v1",
    )
    require(
        result.get("request") == json.loads(payload) and result.get("engine") == probe["engine"],
        "request/runtime identity lost",
    )
    require(result["diagnostics"]["retained"] and result["diagnostics"]["artifacts"], "missing retained diagnostics")
    paths = [verify_artifact(job, item) for item in result["diagnostics"]["artifacts"]]
    if case.get("reject"):
        require(completed.returncode == 1 and result.get("status") == "failed", "excluded fixed content was accepted")
        require(
            result.get("delivery") is None and not (job / "delivery").exists(),
            "excluded fixed content published delivery",
        )
        return check_rejection(
            result, [json.loads(path.read_bytes()) for path in paths if path.suffix == ".json"], case
        )
    require(
        completed.returncode == 0 and result.get("status") == "success" and result.get("error") is None,
        f"supported fixed content failed: {result.get('error')}",
    )
    delivery = job / "delivery"
    for kind, name in (("pdf", "document.pdf"), ("scene", "scene.json"), ("bundle", "bundle.json")):
        verify_artifact(delivery, result["delivery"][kind], name)
    bundle = json.loads((delivery / "bundle.json").read_bytes())
    for item in bundle["entries"]:
        verify_artifact(delivery, item)
    pdf = delivery / "document.pdf"
    require(pdf.read_bytes().startswith(b"%PDF-"), "no PDF header")
    font_binding = resolve_font_resource(delivery, bundle)
    evidence = check_scene(json.loads((delivery / "scene.json").read_bytes()), case, font_binding["resolved_sha256"])
    evidence["font_binding"] = font_binding
    if case.get("disabled_writing_mode"):
        evidence["disabled_writing_mode"] = check_disabled_writing_mode(
            [json.loads(path.read_bytes()) for path in paths if path.suffix == ".json"]
        )
    text_tool = shutil.which("pdftotext")
    if text_tool:
        extracted = subprocess.run(
            [text_tool, "-enc", "UTF-8", "-layout", str(pdf), "-"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=15,
        )
        (root / "pdf-text.txt").write_bytes(extracted.stdout)
        (root / "pdftotext.stderr.txt").write_bytes(extracted.stderr)
        require(extracted.returncode == 0, "PDF text extraction failed")
        check_pdf_text(extracted.stdout.decode("utf-8"), case)
        evidence["pdf_text"] = "passed"
    else:
        evidence["pdf_text"] = "not-run: pdftotext unavailable"
    return {"outcome": "accepted", "pdf_sha256": sha256_file(pdf), **evidence}


def fake_scene(case: dict[str, Any]) -> dict[str, Any]:
    font_resource = sha256_file(FONT_PATH)

    def text(value: str, x: int, y: int) -> dict[str, Any]:
        return {
            "type": "text",
            "text": value,
            "font_size_app_units": FONT_SIZE,
            "font": {"resource": font_resource, "face_index": 0, "variations": [], "synthetic_bold": False},
            "glyphs": [{"x": x + index * FONT_SIZE, "y": y, "advance": FONT_SIZE} for index in range(len(value))],
        }

    pages = []
    for index in range(1, case["pages"] + 1):
        operations = [
            text(f"{prefix}{index:02}", MARGINS["left"], MARGINS["top"] + row * LINE_HEIGHT + BASELINE_IN_LINE)
            for row, prefix in enumerate(("BODY", "TAIL"))
        ]
        if not case.get("plain"):
            value = footer_text(case, index)
            operations.append(
                text(value, (PAGE_SIZE["width"] - len(value) * FONT_SIZE) // 2, footer_top(case) + BASELINE_IN_LINE)
            )
        pages.append(
            {
                "number": index,
                "style_source": "request-defaults",
                "size_app_units": dict(PAGE_SIZE),
                "margins_app_units": dict(MARGINS),
                "operations": operations,
            }
        )
    return {"schema": "pliego.document-scene", "version": 2, "app_units_per_css_px": 60, "pages": pages}


def self_test() -> None:
    require(len(CASES) == 23 and sum(bool(case.get("reject")) for case in CASES) == 13, "case inventory changed")
    fallback = {"fixture": "disabled-writing-mode-fallback", "supports": False, "computedWritingMode": ""}
    check_disabled_writing_mode([{"readiness": {"payload": fallback}}])
    for payload in (
        None,
        {},
        dict(fallback, supports=True),
        dict(fallback, supports=0),
        dict(fallback, computedWritingMode="vertical-rl"),
    ):
        try:
            check_disabled_writing_mode([{"readiness": {"payload": payload}}])
        except AssertionError:
            pass
        else:
            raise AssertionError("disabled-property fallback accepted absent or changed runtime evidence")
    font = FONT_PATH.read_bytes()
    font_resource = sha256_file(FONT_PATH)
    tables = {
        tag: offset
        for tag, _, offset, _ in (
            struct.unpack_from(">4sIII", font, 12 + index * 16) for index in range(struct.unpack_from(">H", font, 4)[0])
        )
    }
    require(struct.unpack_from(">H", font, tables[b"head"] + 18)[0] == 1000, "Ahem units/em changed")
    require(struct.unpack_from(">hhh", font, tables[b"hhea"] + 4) == (800, -200, 0), "Ahem baseline metrics changed")
    require(
        struct.unpack_from(">hhh", font, tables[b"OS/2"] + 68) == (800, -200, 0), "Ahem typographic metrics changed"
    )
    for case in CASES:
        if not case.get("reject"):
            check_scene(fake_scene(case), case, font_resource)
            text = (
                "\f".join(
                    f"BODY{page:02}\nTAIL{page:02}\n" + ("" if case.get("plain") else footer_text(case, page))
                    for page in range(1, case["pages"] + 1)
                )
                + "\f"
            )
            check_pdf_text(text, case)
    case = CASES[1]
    original = fake_scene(case)
    require(
        text_bounds(original["pages"][8]["operations"][2:])[0] - text_bounds(original["pages"][9]["operations"][2:])[0]
        == FONT_SIZE // 2,
        "9-to-10 fixture does not exercise centering-width change",
    )
    for change in (
        "stale-page-one",
        "alias-last-page",
        "missing",
        "duplicate",
        "wrong-placement",
        "wrong-center",
        "off-page",
        "body-missing",
        "body-duplicate",
        "body-order",
        "page-order",
        "fractional-authority",
        "footer-word-order",
        "wrong-total",
        "wrong-font",
    ):
        invalid = copy.deepcopy(original)
        page = invalid["pages"][9]
        operations = page["operations"]
        if change == "stale-page-one":
            operations[-1] = copy.deepcopy(invalid["pages"][0]["operations"][-1])
        elif change == "alias-last-page":
            for item in invalid["pages"]:
                item["operations"][-1] = copy.deepcopy(original["pages"][-1]["operations"][-1])
        elif change == "missing":
            operations.pop()
        elif change == "duplicate":
            operations.append(copy.deepcopy(operations[-1]))
        elif change in ("wrong-placement", "off-page", "wrong-center", "fractional-authority"):
            for glyph in operations[-1]["glyphs"]:
                if change == "wrong-center":
                    glyph["x"] += 1
                elif change == "fractional-authority":
                    glyph["y"] = float(glyph["y"])
                else:
                    glyph["y"] += 60 if change == "wrong-placement" else PAGE_SIZE["height"]
        elif change == "body-missing":
            operations.pop(0)
        elif change == "body-duplicate":
            operations.insert(0, copy.deepcopy(operations[0]))
        elif change == "body-order":
            operations[0], operations[1] = operations[1], operations[0]
        elif change == "footer-word-order":
            operations[-1]["text"] = "of Page 10 12"
        elif change == "wrong-total":
            operations[-1]["text"] = "Page 10 of 11"
        elif change == "wrong-font":
            operations[-1]["font"]["resource"] = "sha256:" + "0" * 64
        else:
            page["number"] = 1
        try:
            check_scene(invalid, case, font_resource)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"fixed-content oracle accepted {change}")
    literal = fake_scene(CASES[8])
    literal["pages"][9]["operations"][-1]["text"] = footer_text(CASES[8], 10).replace("Literal 0", "Literal 10")
    try:
        check_scene(literal, CASES[8], font_resource)
    except AssertionError:
        pass
    else:
        raise AssertionError("page substitution changed unowned literal zero")
    for rejected in (CASES[9], CASES[15]):
        marker = "unresolved-page-counter" if rejected["reject"] == "outside" else "paged-fixed-content"
        result = {"error": {"kind": "artifact"}}
        diagnostic = {
            "code": "SCENE_ENCODING_FAILED",
            "message": f"capture is incomplete: unsupported paint kinds: {marker} (1 events; first sequence 0)",
        }
        check_rejection(result, [diagnostic], rejected)
        for mutation in ("kind", "code", "marker"):
            bad_result, bad_diagnostic = copy.deepcopy(result), copy.deepcopy(diagnostic)
            if mutation == "kind":
                bad_result["error"]["kind"] = "resource"
            elif mutation == "code":
                bad_diagnostic["code"] = "SCENE_CAPTURE_FAILED"
            else:
                bad_diagnostic["message"] = "unrelated unsupported paint"
            try:
                check_rejection(bad_result, [bad_diagnostic], rejected)
            except AssertionError:
                pass
            else:
                raise AssertionError(f"exclusion oracle accepted unrelated {mutation}")
    for valid in ("1", "30", "65", str(PROBE_TIMEOUT_SECONDS)):
        require(process_timeout_seconds(valid) == int(valid), "valid timeout changed")
    for invalid in ("0", "-1", "1.5", "nan", str(PROBE_TIMEOUT_SECONDS + 1)):
        try:
            process_timeout_seconds(invalid)
        except argparse.ArgumentTypeError:
            pass
        else:
            raise AssertionError("invalid process timeout accepted")
    repository = Path(__file__).resolve().parents[3]
    with tempfile.TemporaryDirectory(prefix="pliego-fixed-content-self-test-") as temporary:
        root = Path(temporary)
        first, _ = build_fixture(repository, root / "first", case)
        second, _ = build_fixture(repository, root / "second", case)
        golden = json.loads((repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes())
        require(
            json.loads(first)["settlement"]["limits"] == golden["settlement"]["limits"]
            and golden["settlement"]["limits"]["host_wall_ms"] == 60000,
            "qualification changed API default limits",
        )
        require(
            first == second and json.loads(first)["resources"] == {"network": "deny", "host_fonts": "deny"},
            "fixture closure is not repeatable/offline",
        )
        fallback_case = next(item for item in CASES if item.get("disabled_writing_mode"))
        _, fallback_source = build_fixture(repository, root / "fallback", fallback_case)
        fallback_html = (fallback_source / "input/document.html").read_text(encoding="utf-8")
        require(
            'CSS.supports("writing-mode","vertical-rl")' in fallback_html
            and 'getComputedStyle(document.querySelector(".fixed")).getPropertyValue("writing-mode")' in fallback_html
            and "window.pliego.defer()" in fallback_html,
            "fallback evidence must come from actual runtime CSSOM/support calls",
        )
        real_run = subprocess.run

        def timeout_run(command: list[str], **kwargs: Any) -> subprocess.CompletedProcess:
            if command == [sys.executable, "render-api2"]:
                raise subprocess.TimeoutExpired(command, kwargs["timeout"], output=b"partial", stderr=b"error")
            return real_run(command, **kwargs)

        with patch.object(subprocess, "run", side_effect=timeout_run):
            try:
                run_case(Path(sys.executable), repository, root / "timeout", case, {}, 180)
            except subprocess.TimeoutExpired:
                pass
            else:
                raise AssertionError("timeout did not propagate")
        require(
            (root / "timeout/stdout.json").read_bytes() == b"partial"
            and (root / "timeout/stderr.txt").read_bytes() == b"error",
            "timeout streams lost",
        )
        process = json.loads((root / "timeout/process.json").read_bytes())
        require(process["outcome"] == "host-timeout" and process["limit_seconds"] == 180, "timeout identity lost")
    print("API 2 fixed-content checker self-test: ok (pure oracle/fixture checks; no native render)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--out", "--proof-directory", dest="out", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument(
        "--process-timeout-seconds",
        type=process_timeout_seconds,
        default=65,
        help="Caller bound including startup/self-hashing; optimized default65s, direct-debug allowance180s.",
    )
    parser.add_argument("--require-pdf-text", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.binary is None or args.out is None:
        parser.error("--binary and --out are required outside --self-test")
    if args.require_pdf_text and shutil.which("pdftotext") is None:
        parser.error("--require-pdf-text needs pdftotext on PATH")
    binary = args.binary.resolve(strict=True)
    output = args.out.resolve()
    output.mkdir(parents=True, exist_ok=False)
    probe_process = {"binary_bytes": binary.stat().st_size, "limit_seconds": PROBE_TIMEOUT_SECONDS, "outcome": "failed"}
    started = time.monotonic()
    try:
        probe, raw = run_probe(binary)
        probe_process["outcome"] = "success"
    finally:
        probe_process["wall_seconds"] = time.monotonic() - started
        (output / "probe-process.json").write_bytes(canonical_json(probe_process))
    (output / "probe.json").write_bytes(raw)
    engine = probe["engine"]
    validate_probe(
        probe,
        binary_sha256=sha256_file(binary),
        expected_version=engine["version"],
        expected_source_commit=args.source_commit or engine["source_commit"],
        expected_target=engine["runtime"]["target"],
        expected_servo_base=engine["runtime"]["servo_base"],
    )
    report = {
        "schema": "pliego.api2-fixed-content-proof",
        "version": 1,
        "engine": engine,
        "probe_process": probe_process,
        "process_timeout_seconds": args.process_timeout_seconds,
        "profile": "page-root-flow-inline-decimal-counters-v1",
        "cases": [],
    }
    repository = Path(__file__).resolve().parents[3]
    for case in CASES:
        row = {"case": case, "passed": False}
        try:
            row.update(
                run_case(binary, repository, output / case["name"], case, probe, args.process_timeout_seconds),
                passed=True,
            )
        except (AssertionError, OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
            row["failure"] = str(error)
        process_file = output / case["name"] / "process.json"
        if process_file.is_file():
            row["process"] = json.loads(process_file.read_bytes())
        report["cases"].append(row)
        (output / "report.json").write_bytes(canonical_json(report))
        print(f"{case['name']}: {'passed' if row['passed'] else row['failure']}", flush=True)
    require(all(row["passed"] for row in report["cases"]), f"fixed-content regression failed; evidence: {output}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"API 2 fixed-content check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
