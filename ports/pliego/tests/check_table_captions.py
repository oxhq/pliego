#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

ROW_COUNT = 6
EXPECTED_TEXT = (
    ["TOP_CAPTION"]
    + [f"T{row}{column}" for row in range(ROW_COUNT) for column in "AB"]
    + [f"B{row}{column}" for row in range(ROW_COUNT) for column in "AB"]
    + ["BOTTOM_CAPTION"]
)


def fail(message: str, code: int = 1) -> None:
    print(f"table caption check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_json(path: Any, description: str) -> dict[str, Any]:
    require(isinstance(path, str), f"terminal result has no {description} path")
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    require(isinstance(value, dict), f"{description} is not an object")
    return value


def final_summary(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def page_text_operations(scene: dict[str, Any]) -> list[list[dict[str, Any]]]:
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages array")
    return [
        [
            operation
            for operation in page.get("operations", [])
            if isinstance(operation, dict) and operation.get("type") == "text"
        ]
        for page in pages
        if isinstance(page, dict)
    ]


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the table caption gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-nopgbrk", str(pdf), "-"],
        capture_output=True,
        timeout=20,
        check=False,
    )
    require(result.returncode == 0, f"pdftotext failed: {result.stderr.decode(errors='replace')}")
    return result.stdout.decode("utf-8-sig")


def verify_supported(
    scene: dict[str, Any],
    layout: dict[str, Any],
    structure: dict[str, Any],
    extracted_pdf_text: str,
) -> None:
    pages = page_text_operations(scene)
    flattened = [str(operation.get("text", "")) for page in pages for operation in page]
    require(flattened == EXPECTED_TEXT, f"caption or row text differs: {flattened!r}")

    occurrences: dict[str, list[int]] = {"TOP_CAPTION": [], "BOTTOM_CAPTION": []}
    row_pages: dict[tuple[str, int], set[int]] = {(table, row): set() for table in "TB" for row in range(ROW_COUNT)}
    column_x: dict[tuple[str, str], set[float]] = {(table, column): set() for table in "TB" for column in "AB"}
    for page_index, operations in enumerate(pages):
        for operation in operations:
            text = str(operation.get("text", ""))
            if text in occurrences:
                occurrences[text].append(page_index)
                continue
            match = re.fullmatch(r"([TB])([0-5])([AB])", text)
            if not match:
                continue
            table, row, column = match.groups()
            row_pages[(table, int(row))].add(page_index)
            glyphs = operation.get("glyphs")
            require(isinstance(glyphs, list) and glyphs, f"{text} has no glyph geometry")
            x = glyphs[0].get("x")
            require(isinstance(x, (int, float)), f"{text} has no x coordinate")
            column_x[(table, column)].add(round(float(x), 6))

    require(all(len(found) == 1 for found in row_pages.values()), "a row split or disappeared")
    require(all(len(values) == 1 for values in column_x.values()), f"column grid drifted: {column_x!r}")
    for table in "TB":
        require(
            next(iter(column_x[(table, "A")])) < next(iter(column_x[(table, "B")])),
            f"{table} column order changed",
        )
    require(
        occurrences["TOP_CAPTION"] == [next(iter(row_pages[("T", 0)]))],
        f"top caption was not retained once on the first table fragment: {occurrences!r}",
    )
    require(
        occurrences["BOTTOM_CAPTION"] == [next(iter(row_pages[("B", ROW_COUNT - 1)]))],
        f"bottom caption was not retained once on the last table fragment: {occurrences!r}",
    )

    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "layout has no page sequence")
    layout_pages = page_sequence.get("pages")
    require(isinstance(layout_pages, list) and len(layout_pages) == len(pages), "page counts differ")
    continuations = page_sequence.get("continuations")
    require(isinstance(continuations, list), "continuation trace is absent")
    table_continuations = [
        item for item in continuations if isinstance(item, dict) and item.get("token", {}).get("kind") == "table"
    ]
    table_nodes = {item["token"].get("table_node") for item in table_continuations}
    require(len(table_nodes) == 2 and None not in table_nodes, f"table identity differs: {table_nodes!r}")
    for node in table_nodes:
        resumed_rows = [
            item["token"].get("next_row_index")
            for item in table_continuations
            if item["token"].get("table_node") == node
        ]
        require(resumed_rows == sorted(set(resumed_rows)), f"continuation regressed: {resumed_rows!r}")

    decisions = page_sequence.get("table_breaks")
    require(isinstance(decisions, list), "table break decisions are absent")
    require({item.get("table_node") for item in decisions} == table_nodes, "decision identity differs")
    for node in table_nodes:
        boundaries = [item.get("next_row_index") for item in decisions if item.get("table_node") == node]
        require(boundaries == list(range(1, ROW_COUNT)), f"row boundaries differ: {boundaries!r}")
    require(page_sequence.get("warnings") in (None, []), "supported captions emitted a warning")

    require(structure.get("page_count") == len(pages), "PDF page count differs")
    require(
        canonical_text(extracted_pdf_text) == canonical_text(" ".join(EXPECTED_TEXT)),
        f"pdftotext differs: {extracted_pdf_text!r}",
    )


def verify_unsupported(layout: dict[str, Any]) -> None:
    warnings = layout.get("page_sequence", {}).get("warnings")
    require(isinstance(warnings, list), "unsupported fixture has no warning array")
    require(len(warnings) == 2, f"unsupported caption warning count differs: {warnings!r}")
    table_warning, caption_warning = warnings
    require(
        table_warning.get("kind") == "unsupported-table-group-pagination"
        and table_warning.get("child_index") == 1
        and table_warning.get("table_node") is not None
        and table_warning.get("reason") == "unsupported-layout"
        and caption_warning.get("kind") == "unsupported-table-caption-pagination"
        and caption_warning.get("child_index") == 1
        and caption_warning.get("node") == table_warning.get("table_node"),
        f"unsupported caption warning differs: {warnings!r}",
    )


def operation(text: str, x: float) -> dict[str, Any]:
    return {"type": "text", "text": text, "glyphs": [{"x": x}]}


def self_test() -> None:
    pages: list[dict[str, Any]] = [{"operations": []} for _ in range(4)]
    pages[0]["operations"].append(operation("TOP_CAPTION", 10.0))
    for table, page_by_row in (("T", [0, 0, 0, 1, 1, 1]), ("B", [1, 2, 2, 2, 2, 3])):
        for row, page in enumerate(page_by_row):
            pages[page]["operations"].extend([operation(f"{table}{row}A", 10.0), operation(f"{table}{row}B", 70.0)])
    pages[3]["operations"].append(operation("BOTTOM_CAPTION", 10.0))

    decisions = []
    continuations = []
    for node, selected in ((11, {3}), (22, {1, 5})):
        for row in range(1, ROW_COUNT):
            is_selected = row in selected
            decisions.append(
                {
                    "table_node": node,
                    "next_row_index": row,
                    "selected": is_selected,
                    "resume_page_index": row if is_selected else None,
                }
            )
            if is_selected:
                continuations.append(
                    {
                        "token": {
                            "kind": "table",
                            "table_node": node,
                            "next_row_index": row,
                        }
                    }
                )
    layout = {
        "page_sequence": {
            "pages": [{"index": index} for index in range(4)],
            "continuations": continuations,
            "table_breaks": decisions,
            "warnings": [],
        }
    }
    verify_supported(
        {"pages": pages},
        layout,
        {"page_count": 4},
        " ".join(EXPECTED_TEXT),
    )
    verify_unsupported(
        {
            "page_sequence": {
                "warnings": [
                    {
                        "kind": "unsupported-table-group-pagination",
                        "child_index": 1,
                        "table_node": 33,
                        "reason": "unsupported-layout",
                    },
                    {
                        "kind": "unsupported-table-caption-pagination",
                        "child_index": 1,
                        "node": 33,
                    },
                ]
            }
        }
    )


def run(binary: Path, fixture: Path) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="pliego-table-caption-") as temp:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp, "TMP": temp, "TEMP": temp})
        result = subprocess.run(
            [
                str(binary),
                "--allow-host-fonts",
                "--page-size",
                "240x140",
                "--page-margins",
                "10,10,10,10",
                fixture.name,
            ],
            cwd=fixture.parent,
            env=environment,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        require(result.returncode == 0, f"Pliego exited with {result.returncode}: {result.stderr[-4000:]}")
        summary = final_summary(result)
        require(summary.get("status") == "rendered", f"Pliego did not render: {summary!r}")
        return {
            "summary": summary,
            "scene": read_json(summary.get("scene_artifact"), "scene artifact"),
            "layout": read_json(summary.get("layout_debug"), "layout debug artifact"),
            "structure": read_json(summary.get("pdf_structure"), "PDF structure artifact"),
            "pdf_text": extract_pdf_text(Path(summary["document_pdf"])),
        }


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("table caption self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    fixture = Path(__file__).resolve().parent / "fixtures/table-captions"
    supported = run(binary, fixture / "index.html")
    verify_supported(
        supported["scene"],
        supported["layout"],
        supported["structure"],
        supported["pdf_text"],
    )
    unsupported = run(binary, fixture / "unsupported.html")
    verify_unsupported(unsupported["layout"])
    print("table caption check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
