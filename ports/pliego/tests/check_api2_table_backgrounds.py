#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Bounded API 2 table-track solid background proof, not general table/CSS support.

The original Au cell border box, not the row/column image positioning rectangle,
owns each extra color layer. Independent authored geometry includes odd Au
coordinates, a layered paint-order case and two separately paginated tables.
The latter does not qualify splitting a row or one table across pages. Closed
Ahem inputs, canonical request/result, native identity, bundle hashes, exact
rectangle paths and PDF cell text are checked using the existing link harness.
"""

from __future__ import annotations

import argparse
import copy
import json
import subprocess
import sys
import tempfile
import time
from collections import Counter
from pathlib import Path
from typing import Any

import pypdf
from pypdf import PdfReader
from pypdf.errors import PyPdfError

from check_api2_contract import PROBE_TIMEOUT_SECONDS, canonical_json, run_probe, sha256_file, validate_probe
from check_api2_links import (
    build_fixture,
    check_excluded_failure,
    process_timeout_seconds,
    require,
    run_case as run_closed_case,
    scene_links,
)


# Request margins are 48px (2880Au); wrapper padding .35px is exactly 21Au.
# Fixed table width180.7px =10842Au; columns100.35/80.35px =6021/4821Au.
# Separate borders, spacing and cell padding are zero; authored height24.35px
# =1461Au exceeds one Ahem12/18px line. No native output supplies these values.
ORIGIN = 2901
WIDTHS = (6021, 4821)
ROW_HEIGHT = 1461
COLORS = {
    "colgroup": [17, 34, 51, 255],
    "column": [51, 68, 85, 255],
    "rowgroup": [85, 102, 119, 255],
    "row": [119, 136, 153, 255],
    "cell": [153, 170, 187, 255],
}
CSS = (
    ".pad{padding:.35px}.next{break-before:page}"
    "table{table-layout:fixed;width:180.7px;border-collapse:separate;border-spacing:0}"
    "table,colgroup,col,tbody,tr,td{margin:0;border:0;padding:0}"
    "col:first-child{width:100.35px}col:last-child{width:80.35px}"
    "td{height:24.35px;vertical-align:top;font-size:12px;line-height:18px}"
)


def table(tokens: str, *, next_page: bool = False) -> str:
    require(len(tokens) == 4, "fixture needs four single-glyph cells")
    return (
        f'<div class="pad{" next" if next_page else ""}"><table>'
        "<colgroup><col><col></colgroup><tbody>"
        f"<tr><td>{tokens[0]}</td><td>{tokens[1]}</td></tr>"
        f"<tr><td>{tokens[2]}</td><td>{tokens[3]}</td></tr>"
        "</tbody></table></div>"
    )


def cell_rect(index: int) -> list[int]:
    row, column = divmod(index, 2)
    return [ORIGIN + (WIDTHS[0] if column else 0), ORIGIN + row * ROW_HEIGHT, WIDTHS[column], ROW_HEIGHT]


def fixture(name: str, layers: tuple[str, ...], *, pages: int = 1, extra_css: str = "") -> dict[str, Any]:
    selectors = {"colgroup": "colgroup", "column": "col", "rowgroup": "tbody", "row": "tr", "cell": "td"}
    paints = [[cell_rect(index), COLORS[layer]] for index in range(4) for layer in layers]
    tokens = ["ABCD", "EFGH"][:pages]
    return {
        "name": name,
        "body": "".join(table(text, next_page=index > 0) for index, text in enumerate(tokens)),
        "css": CSS
        + "".join(
            f"{selectors[layer]}{{background-color:#{''.join(f'{c:02x}' for c in COLORS[layer][:3])}}}"
            for layer in layers
        )
        + extra_css,
        "pages": pages,
        "text": list("".join(tokens)),
        "page_text": [list(text) for text in tokens],
        "solid_paths": [copy.deepcopy(paints) for _ in tokens],
    }


CASES = [
    fixture("cell-only-control", ("cell",)),
    fixture("row-solid", ("row",)),
    fixture("rowgroup-solid", ("rowgroup",)),
    fixture("column-solid", ("column",)),
    fixture("colgroup-solid", ("colgroup",)),
    fixture("all-layer-order", tuple(COLORS)),
    fixture("two-page-row-solid", ("row",), pages=2),
]
for name, extra_css in (
    ("row-image-layer", "tr{background-image:linear-gradient(#123,#456)}"),
    ("rounded-cell-overlay", "td{border-radius:3px}"),
):
    case = fixture(name, ("row",), extra_css=extra_css)
    case.update(
        exclude=True,
        failure=["artifact", "SCENE_ENCODING_FAILED"],
        failure_message_contains="unsupported paint kinds: box",
    )
    CASES.append(case)
for name, extra_css in (
    ("row-fixed-attachment", "tr{background-attachment:fixed}"),
    ("row-content-clip", "tr{background-clip:content-box}"),
):
    case = fixture(name, ("row",), extra_css=extra_css)
    # These isolated excluded styles deliberately retain the current exact-
    # authority serializer rejection. This is not an ideal user-facing error
    # taxonomy and cannot be replaced by an unrelated capture/runtime failure.
    case.update(
        exclude=True,
        failure=["artifact", "SCENE_ENCODING_FAILED"],
        failure_message_contains="page 1 operation 0 has no exact fixed-point authority",
    )
    CASES.append(case)


def check_scene(scene: dict[str, Any], case: dict[str, Any]) -> None:
    # Also checks every exact path's closed rectangle bytes, non-zero fill rule,
    # integer Au bounds, page containment, color, multiplicity and layer order.
    scene_links(scene, case)
    for page, tokens in zip(scene["pages"], case["page_text"]):
        require(page["size_app_units"] == {"width": 47622, "height": 67351}, "not exact A4")
        operations = page["operations"]
        require(all(op["type"] in ("text", "path") for op in operations), "unexpected paint operation")
        text_positions = [(position, op) for position, op in enumerate(operations) if op["type"] == "text"]
        texts = [op for _, op in text_positions]
        require([op["text"] for op in texts] == tokens, "cell text missing, duplicated, reordered or on wrong page")
        for index, operation in enumerate(texts):
            x, y, width, height = cell_rect(index)
            text_position = text_positions[index][0]
            # Do not filter away paint order: opaque track layers drawn after
            # their own cell text would hide otherwise extractable glyphs.
            # Interleaving one cell's layers/text with the next cell is valid.
            for paint_position, paint in enumerate(operations):
                if paint["type"] == "path" and paint["bounds"] == {"x": x, "y": y, "width": width, "height": height}:
                    require(paint_position < text_position, "cell background paints over its own text")
            require(operation["font_size_app_units"] == 720, "authored cell font size changed")
            require(operation["color"] == {"r": 0, "g": 0, "b": 0, "a": 255}, "cell text color changed")
            glyphs = operation["glyphs"]
            require(len(glyphs) == 1 and glyphs[0]["advance"] == 720, "Ahem single-cell glyph changed")
            glyph = glyphs[0]
            require(type(glyph["x"]) is int and type(glyph["y"]) is int, "noninteger text geometry")
            require(glyph["x"] == x and y < glyph["y"] < y + height, "cell text escaped its authored cell")
            # Ahem ascent .8em *720Au + half the 360Au line leading.
            require(glyph["y"] == y + 756, "cell baseline differs from authored Ahem metrics")
            require(glyph["x"] + glyph["advance"] <= x + width, "cell glyph extends beyond its column")


def run_case(
    binary: Path, repository: Path, root: Path, case: dict[str, Any], probe: dict[str, Any], timeout: int
) -> dict[str, Any]:
    result = run_closed_case(binary, repository, root, case, probe, timeout)
    if case.get("exclude"):
        return result
    scene_path, pdf_path = root / "job/delivery/scene.json", root / "job/delivery/document.pdf"
    check_scene(json.loads(scene_path.read_bytes()), case)
    reader = PdfReader(pdf_path, strict=True)
    for page, tokens in zip(reader.pages, case["page_text"]):
        require(Counter((page.extract_text() or "").split()) == Counter(tokens), "PDF changed cell text/page/count")
    result["source_geometry_qualified"] = True
    result["pdf_cell_text_qualified"] = True
    return result


def fake_scene(case: dict[str, Any]) -> dict[str, Any]:
    pages = []
    for index, paints in enumerate(case["solid_paths"]):
        operations = []
        for rect, rgba in paints:
            x, y, width, height = rect
            operations.append(
                {
                    "type": "path",
                    "bounds": dict(zip(("x", "y", "width", "height"), rect)),
                    "fill": dict(zip(("r", "g", "b", "a"), rgba)),
                    "fill_rule": "non-zero",
                    "data": f"M {x} {y} L {x + width} {y} L {x + width} {y + height} L {x} {y + height} Z",
                }
            )
        for cell, token in enumerate(case["page_text"][index]):
            x, y, _, _ = cell_rect(cell)
            operations.append(
                {
                    "type": "text",
                    "text": token,
                    "font_size_app_units": 720,
                    "color": {"r": 0, "g": 0, "b": 0, "a": 255},
                    "glyphs": [{"x": x, "y": y + 756, "advance": 720}],
                }
            )
        pages.append(
            {"number": index + 1, "size_app_units": {"width": 47622, "height": 67351}, "operations": operations}
        )
    return {"schema": "pliego.document-scene", "version": 2, "app_units_per_css_px": 60, "pages": pages}


def self_test() -> None:
    for case in CASES:
        if not case.get("exclude"):
            check_scene(fake_scene(case), case)
        else:
            kind, code = case["failure"]
            check_excluded_failure(case, kind, [code], [case["failure_message_contains"]])
            for wrong_kind, codes, messages in (
                ("capture", [code], [case["failure_message_contains"]]),
                (kind, ["SCENE_CAPTURE_FAILED"], [case["failure_message_contains"]]),
                (kind, [code], ["unrelated render failure"]),
                (kind, [code, code], [case["failure_message_contains"]]),
            ):
                try:
                    check_excluded_failure(case, wrong_kind, codes, messages)
                except AssertionError:
                    pass
                else:
                    raise AssertionError("table-background exclusion accepted an unrelated/duplicate typed cause")
    case = next(case for case in CASES if case["name"] == "all-layer-order")
    scene = fake_scene(case)
    for change in (
        "missing",
        "duplicate",
        "layer-order",
        "row-area",
        "shifted",
        "color",
        "path",
        "float",
        "outside-page",
        "text-missing",
        "text-order",
        "text-position",
        "text-baseline",
        "text-first-then-paths",
        "paint-after-own-text",
    ):
        wrong = copy.deepcopy(scene)
        operations = wrong["pages"][0]["operations"]
        if change == "missing":
            operations.pop(0)
        elif change == "duplicate":
            operations.insert(0, copy.deepcopy(operations[0]))
        elif change == "layer-order":
            operations[0], operations[1] = operations[1], operations[0]
        elif change in ("row-area", "shifted"):
            # Rebuild a self-consistent but wrong path; authored bounds must fail.
            op = operations[0]
            op["bounds"]["width" if change == "row-area" else "x"] += WIDTHS[1] if change == "row-area" else 1
            x, y, w, h = (op["bounds"][key] for key in ("x", "y", "width", "height"))
            op["data"] = f"M {x} {y} L {x + w} {y} L {x + w} {y + h} L {x} {y + h} Z"
        elif change == "color":
            operations[0]["fill"]["r"] += 1
        elif change == "path":
            operations[0]["data"] = operations[0]["data"].removesuffix(" Z")
        elif change == "float":
            operations[0]["bounds"]["x"] = float(operations[0]["bounds"]["x"])
        elif change == "outside-page":
            operations[0]["bounds"]["y"] = 67351
        elif change == "text-missing":
            operations.pop()
        elif change == "text-order":
            operations[-1], operations[-2] = operations[-2], operations[-1]
        elif change == "text-position":
            operations[-1]["glyphs"][0]["y"] += ROW_HEIGHT
        elif change == "text-baseline":
            operations[-1]["glyphs"][0]["y"] += 1
        elif change == "text-first-then-paths":
            wrong["pages"][0]["operations"] = [op for op in operations if op["type"] == "text"] + [
                op for op in operations if op["type"] == "path"
            ]
        else:
            last_paint = max(index for index, op in enumerate(operations) if op["type"] == "path")
            operations.append(operations.pop(last_paint))
        try:
            check_scene(wrong, case)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"table-background oracle accepted {change}")
    case = next(case for case in CASES if case["pages"] == 2)
    wrong = fake_scene(case)
    wrong["pages"][1]["operations"] = copy.deepcopy(wrong["pages"][0]["operations"])
    try:
        check_scene(wrong, case)
    except AssertionError:
        pass
    else:
        raise AssertionError("table-background oracle accepted last-page alias")
    repository = Path(__file__).resolve().parents[3]
    golden = json.loads((repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes())
    with tempfile.TemporaryDirectory(prefix="pliego-table-background-self-test-") as temporary:
        for case in CASES:
            root = Path(temporary) / case["name"]
            first, _ = build_fixture(repository, root / "first", case)
            second, _ = build_fixture(repository, root / "second", case)
            require(first == second, "nonrepeatable fixture closure")
            require(
                json.loads(first)["settlement"]["limits"] == golden["settlement"]["limits"]
                and golden["settlement"]["limits"]["host_wall_ms"] == 60000,
                "qualification changed API default limits",
            )
            require(
                json.loads(first)["resources"] == {"network": "deny", "host_fonts": "deny"}, "fixture is not closed"
            )
    print("API 2 table-background self-test: ok (pure oracle/fixture checks; no native render)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--out", "--proof-directory", dest="out", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--process-timeout-seconds", type=process_timeout_seconds, default=65)
    parser.add_argument("--case", choices=[case["name"] for case in CASES], action="append")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.binary is None or args.out is None:
        parser.error("--binary and --out are required")
    binary, output = args.binary.resolve(strict=True), args.out.resolve()
    output.mkdir(parents=True, exist_ok=False)
    process = {"binary_bytes": binary.stat().st_size, "limit_seconds": PROBE_TIMEOUT_SECONDS, "outcome": "failed"}
    started = time.monotonic()
    try:
        probe, raw = run_probe(binary)
        process["outcome"] = "success"
    finally:
        process["wall_seconds"] = time.monotonic() - started
        (output / "probe-process.json").write_bytes(canonical_json(process))
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
        "schema": "pliego.api2-table-background-proof",
        "version": 1,
        "engine": engine,
        "probe_process": process,
        "process_timeout_seconds": args.process_timeout_seconds,
        "pypdf_version": pypdf.__version__,
        "cases": [],
    }
    repository = Path(__file__).resolve().parents[3]
    for case in CASES:
        if args.case and case["name"] not in args.case:
            continue
        row = {"case": case, "passed": False}
        try:
            row.update(
                run_case(binary, repository, output / case["name"], case, probe, args.process_timeout_seconds),
                passed=True,
            )
        except (
            AssertionError,
            OSError,
            subprocess.SubprocessError,
            ValueError,
            KeyError,
            TypeError,
            PyPdfError,
        ) as error:
            row["failure"] = str(error)
        process_path = output / case["name"] / "process.json"
        if process_path.is_file():
            row["process"] = json.loads(process_path.read_bytes())
        report["cases"].append(row)
        (output / "report.json").write_bytes(canonical_json(report))
        print(f"{case['name']}: {'passed' if row['passed'] else row['failure']}", flush=True)
    require(all(row["passed"] for row in report["cases"]), f"table-background gate failed; retained evidence: {output}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError, PyPdfError) as error:
        print(f"API 2 table-background check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
