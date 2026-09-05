#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Retain executable API 2 proof for uniform collapsed tables with optional headers.

This is a narrow geometry/text regression, not a business-document visual oracle.
Every render uses a closed input manifest and the committed Ahem font. Unsupported
border styles, mixed colors, rowspan, and headerless pagination remain explicit
rejection cases. Headed pagination must preserve the exact geometry of repeated
headers, row text, and every grid edge through the public scene and PDF.
Entirely invisible collapsed borders must emit no artificial grid paths.
Horizontal rules are tied to their row glyph baselines and page bounds. The
uniform-grid oracle does not establish absolute text/edge alignment; visual
document qualification remains separate.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path
from typing import Any

from check_api2_contract import (
    build_execution_fixture,
    canonical_json,
    create_private_job_root,
    run_probe,
    sha256_bytes,
    sha256_file,
    validate_probe,
    verify_artifact,
)


CASES = (
    {"name": "header-one-page", "rows": 2, "header": True},
    {"name": "headerless-one-page", "rows": 2, "header": False},
    {"name": "header-multi-page", "rows": 90, "header": True},
    {"name": "borderless-one-page", "rows": 2, "header": False, "borders": "none"},
    {"name": "borderless-multi-page", "rows": 90, "header": False, "borders": "none"},
    {"name": "transparent-one-page", "rows": 2, "header": False, "borders": "transparent"},
    {"name": "hidden-one-page", "rows": 2, "header": False, "borders": "hidden"},
    {"name": "horizontal-one-page", "rows": 2, "header": True, "borders": "horizontal"},
    {"name": "horizontal-multi-page", "rows": 90, "header": True, "borders": "horizontal"},
    {"name": "reject-headerless-multi-page", "rows": 90, "header": False, "reject": "pagination"},
    {"name": "reject-mixed-color", "rows": 2, "header": False, "reject": "mixed-color"},
    {"name": "reject-dashed", "rows": 2, "header": False, "reject": "dashed"},
    {"name": "reject-rowspan", "rows": 2, "header": False, "reject": "rowspan"},
    {"name": "reject-partially-visible", "rows": 2, "header": False, "reject": "partially-visible"},
    {"name": "reject-partial-horizontal", "rows": 2, "header": True, "reject": "partial-horizontal"},
)
TOKEN = re.compile(r"R[0-9]{3}C[01]|HDR[LR]")
BORDER_COLOR = {"r": 34, "g": 34, "b": 34, "a": 255}
LINE_HEIGHT_PX = 16
CELL_PADDING_PX = 8


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def expected_rows(count: int) -> list[str]:
    return [f"R{row:03}C{column}" for row in range(count) for column in range(2)]


def check_text(text: str, rows: int, header_count: int) -> None:
    tokens = TOKEN.findall(text)
    require(
        [token for token in tokens if token.startswith("R")] == expected_rows(rows),
        "body row tokens were lost, duplicated, reordered, or split across text runs",
    )
    headers = Counter(token for token in tokens if token.startswith("HDR"))
    expected = Counter({"HDRL": header_count, "HDRR": header_count}) if header_count else Counter()
    require(headers == expected, "table header count differs from the declared optional-header policy")


def check_coverage(intervals: list[tuple[int, int]], start: int, end: int) -> None:
    cursor = start
    for left, right in sorted(intervals):
        require(left <= cursor and right > left, "table border has a gap or reversed segment")
        cursor = max(cursor, right)
    require(cursor == end, "table border does not cover the complete edge")


def check_borders(paths: list[dict[str, Any]], rows: int) -> None:
    horizontal: dict[int, list[tuple[int, int]]] = {}
    vertical: dict[int, list[tuple[int, int]]] = {}
    for path in paths:
        bounds = path.get("bounds", {})
        require(path.get("fill") == BORDER_COLOR, "border color differs from the uniform input")
        require(
            bounds.get("width", 0) > 0 and bounds.get("height", 0) > 0 and min(bounds["width"], bounds["height"]) == 60,
            "border path is not a one-CSS-pixel axis-aligned rectangle",
        )
        require(bool(path.get("data")), "border path has no geometry")
        x, y, width, height = (bounds[key] for key in ("x", "y", "width", "height"))
        if height == 60:
            horizontal.setdefault(y, []).append((x, x + width))
        else:
            vertical.setdefault(x, []).append((y, y + height))
    require(len(horizontal) == rows + 1 and len(vertical) == 3, "table grid edge or seam count differs")
    left, right = min(vertical), max(vertical) + 60
    top, bottom = min(horizontal), max(horizontal) + 60
    for intervals in horizontal.values():
        check_coverage(intervals, left, right)
    for intervals in vertical.values():
        check_coverage(intervals, top, bottom)


def check_horizontal_borders(
    paths: list[dict[str, Any]], body: list[str], operations: list[dict[str, Any]], page_size: dict[str, int]
) -> None:
    row_tokens = [("HDRL", "HDRR"), *zip(body[::2], body[1::2])]
    require(len(paths) == len(row_tokens), "horizontal invoice rules lost or duplicated a row boundary")
    baselines = {}
    for operation in operations:
        if operation.get("type") != "text":
            continue
        tokens = TOKEN.findall(operation["text"])
        if not tokens:
            continue
        require(len(tokens) == 1 and tokens[0] not in baselines, "fixture cell baseline is ambiguous or duplicated")
        ys = [glyph["y"] for glyph in operation.get("glyphs", [])]
        require(bool(ys) and all(type(y) is int for y in ys), "fixture cell has no exact glyph baselines")
        baselines[tokens[0]] = (min(ys), max(ys))
    require(all(token in baselines for row in row_tokens for token in row), "fixture row baseline is missing")
    row_baselines = [
        (min(baselines[left][0], baselines[right][0]), max(baselines[left][1], baselines[right][1]))
        for left, right in row_tokens
    ]
    ordered = sorted(paths, key=lambda path: path["bounds"]["y"])
    last_bottom = -1
    for index, path in enumerate(ordered):
        bounds = path["bounds"]
        require(path.get("fill") == BORDER_COLOR and bool(path.get("data")), "invoice rule paint differs")
        require(bounds["x"] == 0 and bounds["width"] == 420 * 60, "invoice rule does not span the exact grid width")
        require(bounds["height"] == (120 if index == 0 else 60), "header/body invoice rule width differs")
        require(bounds["y"] > last_bottom, "invoice rules overlap or are out of order")
        last_bottom = bounds["y"] + bounds["height"]
        require(
            bounds["x"] + bounds["width"] <= page_size["width"] and last_bottom <= page_size["height"],
            "invoice rule escaped the page",
        )
        require(bounds["y"] > row_baselines[index][1], "invoice rule does not follow its row baseline")
        if index + 1 < len(row_baselines):
            require(last_bottom < row_baselines[index + 1][0], "invoice rule does not precede the next row baseline")
        # A generous metric-independent bound: the entire declared line box,
        # bottom padding and rule thickness. Do not guess font ascent/descent.
        require(
            last_bottom - row_baselines[index][1] <= (LINE_HEIGHT_PX + CELL_PADDING_PX) * 60 + bounds["height"],
            "invoice rule is too far below its row baseline",
        )


def check_scene(scene: dict[str, Any], case: dict[str, Any]) -> dict[str, Any]:
    require(scene.get("schema") == "pliego.document-scene" and scene.get("version") == 2, "not Scene v2")
    pages = scene.get("pages", [])
    require(bool(pages), "scene has no pages")
    require(len(pages) == 1 if case["rows"] == 2 else 2 <= len(pages) <= 4, "unexpected table pagination")
    all_text = []
    path_counts = []
    rows_per_page = []
    for index, page in enumerate(pages):
        require(page.get("number") == index + 1, "page numbering is not sequential")
        operations = page.get("operations", [])
        paths = [operation for operation in operations if operation.get("type") == "path"]
        text = " ".join(operation["text"] for operation in operations if operation.get("type") == "text")
        tokens = TOKEN.findall(text)
        body = [token for token in tokens if token.startswith("R")]
        require(bool(body), f"page {index + 1} contains no body rows")
        require(len(body) % 2 == 0, "a table row was split between pages")
        for left, right in zip(body[::2], body[1::2]):
            require(left.endswith("C0") and right == left[:-1] + "1", "row cells do not share a page")
        headers = [token for token in tokens if token.startswith("HDR")]
        require(headers == (["HDRL", "HDRR"] if case["header"] else []), "implicit or missing page header")
        if case.get("borders") in ("none", "transparent", "hidden"):
            require(not paths, "invisible collapsed borders emitted artificial paths")
        elif case.get("borders") == "horizontal":
            check_horizontal_borders(paths, body, operations, page["size_app_units"])
        else:
            check_borders(paths, len(body) // 2 + int(case["header"]))
        for operation in operations:
            if operation.get("type") == "text":
                require(bool(operation.get("glyphs")), "captured row text has no glyphs")
                for glyph in operation["glyphs"]:
                    require(
                        0 <= glyph["x"] < page["size_app_units"]["width"]
                        and 0 <= glyph["y"] < page["size_app_units"]["height"],
                        "table glyph baseline escaped the page",
                    )
        all_text.append(text)
        path_counts.append(len(paths))
        rows_per_page.append(len(body) // 2)
    check_text(" ".join(all_text), case["rows"], len(pages) if case["header"] else 0)
    return {"pages": len(pages), "rows_per_page": rows_per_page, "border_paths_per_page": path_counts}


def build_fixture(repository: Path, root: Path, case: dict[str, Any]) -> tuple[bytes, Path]:
    template = (repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes()
    payload, source = build_execution_fixture(repository, root, template)
    header = "<thead><tr><th>HDRL</th><th>HDRR</th></tr></thead>" if case["header"] else ""
    rows = "".join(f"<tr><td>R{row:03}C0</td><td>R{row:03}C1</td></tr>" for row in range(case["rows"]))
    if case.get("reject") == "rowspan":
        rows = '<tr><td rowspan="2">R000C0</td><td>R000C1</td></tr><tr><td>R001C1</td></tr>'
    html = (
        '<!doctype html><html lang="en-US"><head><meta charset="utf-8">'
        '<link rel="stylesheet" href="styles.css"><title>API 2 headerless table</title>'
        f"</head><body><table>{header}<tbody>{rows}</tbody></table></body></html>\n"
    )
    css = (
        '@font-face{font-family:Ahem;src:url("font.ttf") format("truetype");}'
        "html,body{margin:0}body{font-family:Ahem;font-size:12px;"
        f"line-height:{LINE_HEIGHT_PX}px;color:#000}}"
        "table{border-collapse:collapse;width:420px}"
        f"td,th{{border:1px solid #222;padding:{CELL_PADDING_PX}px;font-weight:normal;text-align:left}}"
    )
    if case.get("reject") == "mixed-color":
        css += "td:first-child{border-color:#d00}"
    elif case.get("reject") == "dashed":
        css += "td{border-style:dashed}"
    elif case.get("reject") == "partially-visible":
        css += "td{border-color:transparent}td:first-child{border-left-color:#222}"
    elif case.get("borders") == "none":
        css += "td,th{border:none}"
    elif case.get("borders") == "transparent":
        css += "td,th{border-color:transparent}"
    elif case.get("borders") == "hidden":
        css += "td,th{border-style:hidden}"
    elif case.get("borders") == "horizontal" or case.get("reject") == "partial-horizontal":
        css += "td,th{border:0}td{border-bottom:1px solid #222}th{border-bottom:2px solid #222}"
        if case.get("reject") == "partial-horizontal":
            css += "td:last-child{border-bottom:0}"
    (source / "input/document.html").write_text(html, encoding="utf-8", newline="\n")
    (source / "input/styles.css").write_text(css + "\n", encoding="utf-8", newline="\n")
    manifest = json.loads((source / "input-manifest.json").read_bytes())
    for entry in manifest["entries"]:
        path = source / "input" / entry["path"]
        entry.update(sha256=sha256_file(path), bytes=path.stat().st_size)
    manifest_bytes = canonical_json(manifest)
    (source / "input-manifest.json").write_bytes(manifest_bytes)
    request = json.loads(payload)
    request["input"]["manifest"].update(sha256=sha256_bytes(manifest_bytes), bytes=len(manifest_bytes))
    if case.get("borders") == "horizontal" or case.get("reject") == "partial-horizontal":
        # Match the invoice minimizer's zero-margin page, so grid x=0 is exact.
        request["page"]["margins_app_units"] = dict.fromkeys(("top", "right", "bottom", "left"), 0)
    request["settlement"]["limits"]["host_wall_ms"] = 10000
    return canonical_json(request), source


def run_case(binary: Path, repository: Path, root: Path, case: dict[str, Any], probe: dict[str, Any]) -> dict[str, Any]:
    payload, source = build_fixture(repository, root, case)
    (root / "request.json").write_bytes(payload)
    job = root / "job"
    create_private_job_root(job)
    shutil.copy2(source / "input-manifest.json", job / "input-manifest.json")
    shutil.copytree(source / "input", job / "input")
    completed = subprocess.run(
        [str(binary), "render-api2"],
        cwd=job,
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=30,
    )
    (root / "stdout.json").write_bytes(completed.stdout)
    (root / "stderr.txt").write_bytes(completed.stderr)
    (root / "process.json").write_bytes(canonical_json({"exit_code": completed.returncode}))
    require(not completed.stderr, "API 2 execution emitted stderr")
    result = json.loads(completed.stdout)
    require(canonical_json(result) == completed.stdout, "result is not canonical compact JSON plus LF")
    require(
        result.get("schema") == "pliego.render-result" and result.get("version") == 1 and result.get("api") == 2,
        "execution did not return RenderResult v1",
    )
    require(result.get("request") == json.loads(payload) and result.get("engine") == probe["engine"], "identity lost")
    require(result["diagnostics"]["retained"] and result["diagnostics"]["artifacts"], "missing retained diagnostics")
    diagnostics = [verify_artifact(job, item) for item in result["diagnostics"]["artifacts"]]
    if case.get("reject"):
        require(completed.returncode == 1 and result.get("status") == "failed", "unsupported table was accepted")
        require(result.get("delivery") is None and not (job / "delivery").exists(), "failed table published delivery")
        codes = [json.loads(path.read_bytes()).get("code") for path in diagnostics if path.suffix == ".json"]
        kind = result.get("error", {}).get("kind")
        expected = {("artifact", "SCENE_ENCODING_FAILED")}
        if case["reject"] == "pagination":
            # The old build fails on cross-page paint; the intact-table guard
            # instead rejects unsupported border capture before encoding.
            expected.add(("capture", "SCENE_CAPTURE_FAILED"))
        matches = [(kind, code) for code in codes if (kind, code) in expected]
        require(len(matches) == 1, "unsupported table did not retain the declared typed rejection")
        return {"outcome": "rejected-as-declared", "error_kind": kind, "code": matches[0][1]}
    require(
        completed.returncode == 0 and result.get("status") == "success" and result.get("error") is None,
        f"supported table failed: exit={completed.returncode}, error={result.get('error')}",
    )
    delivery = job / "delivery"
    for kind in ("pdf", "scene", "bundle"):
        verify_artifact(
            delivery,
            result["delivery"][kind],
            {"pdf": "document.pdf", "scene": "scene.json", "bundle": "bundle.json"}[kind],
        )
    bundle = json.loads((delivery / "bundle.json").read_bytes())
    for entry in bundle["entries"]:
        verify_artifact(delivery, entry)
    pdf = delivery / "document.pdf"
    require(pdf.read_bytes().startswith(b"%PDF-"), "delivery has no PDF header")
    evidence = check_scene(json.loads((delivery / "scene.json").read_bytes()), case)
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
        check_text(extracted.stdout.decode("utf-8"), case["rows"], evidence["pages"] if case["header"] else 0)
        evidence["pdf_text"] = "passed"
    else:
        evidence["pdf_text"] = "not-run: pdftotext unavailable"
    return {"outcome": "accepted", "pdf_sha256": sha256_file(pdf), **evidence}


def self_test() -> None:
    require(len(CASES) == 15 and sum(bool(case.get("reject")) for case in CASES) == 6, "case inventory changed")
    case = CASES[1]
    paths = [
        {
            "type": "path",
            "bounds": {"x": x, "y": y, "width": width, "height": height},
            "fill": BORDER_COLOR,
            "data": "M 0 0 Z",
        }
        for x, y, width, height in (
            (0, 0, 60, 600),
            (300, 0, 60, 600),
            (600, 0, 60, 600),
            (0, 0, 660, 60),
            (0, 300, 660, 60),
            (0, 540, 660, 60),
        )
    ]
    scene = {
        "schema": "pliego.document-scene",
        "version": 2,
        "pages": [
            {
                "number": 1,
                "size_app_units": {"width": 47622, "height": 67351},
                "operations": paths
                + [{"type": "text", "text": token, "glyphs": [{"x": 60, "y": 120}]} for token in expected_rows(2)],
            }
        ],
    }
    check_scene(scene, case)
    for change in ("duplicate", "missing", "border", "missing-top-border", "border-gap", "header", "clipped", "page"):
        invalid = copy.deepcopy(scene)
        operations = invalid["pages"][0]["operations"]
        if change == "duplicate":
            operations.append(copy.deepcopy(operations[-1]))
        elif change == "missing":
            operations.pop()
        elif change == "border":
            operations[0]["bounds"]["width"] = 120
        elif change == "missing-top-border":
            operations.pop(3)
        elif change == "border-gap":
            operations[3]["bounds"]["width"] = 300
        elif change == "header":
            operations[-1]["text"] = "HDRL HDRR"
        elif change == "clipped":
            operations[-1]["glyphs"][0]["y"] = 67351
        else:
            invalid["pages"][0]["number"] = 2
        try:
            check_scene(invalid, case)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"self-test accepted {change} corruption")
    invisible = copy.deepcopy(scene)
    invisible["pages"][0]["operations"] = [
        operation for operation in invisible["pages"][0]["operations"] if operation["type"] == "text"
    ]
    for border_kind in ("none", "transparent", "hidden"):
        invisible_case = {**case, "borders": border_kind}
        check_scene(invisible, invisible_case)
        try:
            check_scene(scene, invisible_case)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"self-test accepted artificial {border_kind} borders")
    horizontal = [
        {
            "type": "path",
            "bounds": {"x": 0, "y": y, "width": 25200, "height": height},
            "fill": BORDER_COLOR,
            "data": "M 0 0 Z",
        }
        for y, height in ((600, 120), (1200, 60), (1800, 60))
    ]
    horizontal_scene = copy.deepcopy(scene)
    horizontal_scene["pages"][0]["operations"] = horizontal + [
        {"type": "text", "text": token, "glyphs": [{"x": 60 + column * 12000, "y": y}]}
        for row, y in ((["HDRL", "HDRR"], 300), (["R000C0", "R000C1"], 900), (["R001C0", "R001C1"], 1500))
        for column, token in enumerate(row)
    ]
    horizontal_case = {**case, "header": True, "borders": "horizontal"}
    check_scene(horizontal_scene, horizontal_case)
    for change in (
        "width",
        "missing",
        "duplicate",
        "header-thickness",
        "overlap",
        "displaced-rules",
        "final-rule-off-page",
        "header-body-reordered",
        "before-baseline",
        "after-next-baseline",
        "excessive-gap",
    ):
        invalid = copy.deepcopy(horizontal_scene)
        operations = invalid["pages"][0]["operations"]
        if change == "width":
            operations[1]["bounds"]["width"] -= 60
        elif change == "missing":
            operations.pop(2)
        elif change == "duplicate":
            operations.append(copy.deepcopy(operations[2]))
        elif change == "header-thickness":
            operations[0]["bounds"]["height"] = 60
        elif change == "displaced-rules":
            for rule in operations[:3]:
                rule["bounds"]["y"] += 40000
        elif change == "final-rule-off-page":
            operations[2]["bounds"]["y"] = 67340
        elif change == "header-body-reordered":
            for cell in operations[3:5]:
                cell["glyphs"][0]["y"] = 900
            for cell in operations[5:7]:
                cell["glyphs"][0]["y"] = 300
        elif change == "before-baseline":
            operations[1]["bounds"]["y"] = 850
        elif change == "after-next-baseline":
            operations[1]["bounds"]["y"] = 1460
        elif change == "excessive-gap":
            operations[2]["bounds"]["y"] = 3000
        else:
            operations[1]["bounds"]["y"] = 610
        try:
            check_scene(invalid, horizontal_case)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"self-test accepted horizontal rule {change} corruption")
    with tempfile.TemporaryDirectory(prefix="pliego-headerless-self-test-") as temporary:
        first, source = build_fixture(Path(__file__).resolve().parents[3], Path(temporary) / "first", case)
        second, _ = build_fixture(Path(__file__).resolve().parents[3], Path(temporary) / "second", case)
        require(first == second, "fixture input manifest is not repeatable")
        require(b"<thead>" not in (source / "input/document.html").read_bytes(), "headerless fixture has a header")
        require(
            json.loads(first)["resources"] == {"network": "deny", "host_fonts": "deny"},
            "fixture resource policy changed",
        )
    print("API 2 headerless table checker self-test: ok")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--proof-directory", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--require-pdf-text", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.binary is None or args.proof_directory is None:
        parser.error("--binary and --proof-directory are required outside --self-test")
    if args.require_pdf_text and shutil.which("pdftotext") is None:
        parser.error("--require-pdf-text needs pdftotext on PATH")
    binary = args.binary.resolve(strict=True)
    output = args.proof_directory.resolve()
    output.mkdir(parents=True, exist_ok=False)
    probe, raw_probe = run_probe(binary)
    (output / "probe.json").write_bytes(raw_probe)
    engine = probe["engine"]
    validate_probe(
        probe,
        binary_sha256=sha256_file(binary),
        expected_version=engine["version"],
        expected_source_commit=args.source_commit or engine["source_commit"],
        expected_target=engine["runtime"]["target"],
        expected_servo_base=engine["runtime"]["servo_base"],
    )
    report = {"schema": "pliego.api2-headerless-tables-proof", "version": 1, "engine": engine, "cases": []}
    repository = Path(__file__).resolve().parents[3]
    for case in CASES:
        row = {"case": case, "passed": False}
        try:
            row.update(run_case(binary, repository, output / case["name"], case, probe), passed=True)
        except (AssertionError, OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
            row["failure"] = str(error)
        report["cases"].append(row)
        (output / "report.json").write_bytes(canonical_json(report))
        print(f"{case['name']}: {'passed' if row['passed'] else row['failure']}")
    require(all(row["passed"] for row in report["cases"]), f"table regression failed; retained evidence: {output}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"API 2 headerless table check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
