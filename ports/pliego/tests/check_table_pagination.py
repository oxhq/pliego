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

EXPECTED_TEXT = ["Lead."] + [f"R{row}{column}" for row in range(8) for column in "AB"]


def fail(message: str, code: int = 1) -> None:
    print(f"table pagination check: {message}", file=sys.stderr)
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


def table_tokens(page_operations: list[list[dict[str, Any]]]) -> list[str]:
    return [
        str(operation.get("text", ""))
        for operations in page_operations
        for operation in operations
    ]


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the table pagination gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-nopgbrk", str(pdf), "-"],
        capture_output=True,
        timeout=20,
        check=False,
    )
    require(result.returncode == 0, f"pdftotext failed: {result.stderr.decode(errors='replace')}")
    return result.stdout.decode("utf-8-sig")


def verify(
    scene: dict[str, Any],
    layout: dict[str, Any],
    structure: dict[str, Any],
    extracted_pdf_text: str,
) -> None:
    operations = page_text_operations(scene)
    require(len(operations) >= 2, "table did not span multiple pages")
    require(table_tokens(operations) == EXPECTED_TEXT, "rows were lost, duplicated, or reordered")

    column_x: dict[str, set[float]] = {"A": set(), "B": set()}
    row_pages: dict[int, set[int]] = {row: set() for row in range(8)}
    for page_index, page in enumerate(operations):
        for operation in page:
            text = operation.get("text")
            match = re.fullmatch(r"R([0-7])([AB])", str(text))
            if not match:
                continue
            glyphs = operation.get("glyphs")
            require(isinstance(glyphs, list) and glyphs, f"{text} has no glyph geometry")
            x = glyphs[0].get("x")
            require(isinstance(x, (int, float)), f"{text} has no x coordinate")
            column_x[match.group(2)].add(round(float(x), 6))
            row_pages[int(match.group(1))].add(page_index)
    require(all(len(pages) == 1 for pages in row_pages.values()), "a row split across pages")
    require(all(len(values) == 1 for values in column_x.values()), f"column grid drifted: {column_x!r}")
    require(next(iter(column_x["A"])) < next(iter(column_x["B"])), "column order changed")

    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "layout has no page sequence")
    layout_pages = page_sequence.get("pages")
    require(isinstance(layout_pages, list) and len(layout_pages) == len(operations), "page counts differ")
    continuations = page_sequence.get("continuations")
    require(isinstance(continuations, list), "continuation trace is absent")
    table_continuations = [
        continuation
        for continuation in continuations
        if isinstance(continuation, dict)
        and continuation.get("token", {}).get("kind") == "table"
    ]
    require(bool(table_continuations), "no table continuation was selected")
    selected_rows = [item["token"]["next_row_index"] for item in table_continuations]
    require(selected_rows == sorted(set(selected_rows)), "table continuation progress is not monotonic")
    table_nodes = {item["token"].get("table_node") for item in table_continuations}
    require(len(table_nodes) == 1 and None not in table_nodes, "table identity is not stable")
    require(
        all(item["token"].get("row_group_index") == 0 for item in table_continuations),
        "row-group identity changed",
    )

    decisions = page_sequence.get("table_breaks")
    require(isinstance(decisions, list), "table break decision trace is absent")
    require([item.get("next_row_index") for item in decisions] == list(range(1, 8)), "not every row boundary was considered")
    traced_selected = [item.get("next_row_index") for item in decisions if item.get("selected") is True]
    require(traced_selected == selected_rows, "considered/selected break traces differ")
    require(
        all(
            item.get("resume_page_index") is not None
            if item.get("selected")
            else item.get("resume_page_index") is None
            for item in decisions
        ),
        "table break resume trace is inconsistent",
    )
    require(page_sequence.get("warnings") in (None, []), "basic table pagination warned")

    require(structure.get("page_count") == len(operations), "PDF page count differs")
    require(
        canonical_text(extracted_pdf_text) == canonical_text(" ".join(EXPECTED_TEXT)),
        f"pdftotext differs: {extracted_pdf_text!r}",
    )


def self_test() -> None:
    def operation(text: str, x: float) -> dict[str, Any]:
        return {"type": "text", "text": text, "glyphs": [{"x": x}]}

    scene = {"pages": [{"operations": [operation("Lead.", 10.0)]}, {"operations": []}]}
    for row in range(8):
        page = 0 if row < 4 else 1
        scene["pages"][page]["operations"].extend(
            [operation(f"R{row}A", 10.0), operation(f"R{row}B", 70.0)]
        )
    decisions = [
        {
            "page_index": 0 if row <= 4 else 1,
            "table_node": 99,
            "row_group_index": 0,
            "next_row_index": row,
            "selected": row == 4,
            "resume_page_index": 1 if row == 4 else None,
        }
        for row in range(1, 8)
    ]
    layout = {
        "page_sequence": {
            "pages": [{"index": 0}, {"index": 1}],
            "continuations": [
                {
                    "page_index": 0,
                    "token": {
                        "kind": "table",
                        "child_index": 1,
                        "table_node": 99,
                        "row_group_index": 0,
                        "next_row_index": 4,
                        "resume_page_index": 1,
                    },
                }
            ],
            "table_breaks": decisions,
            "warnings": [],
        }
    }
    verify(scene, layout, {"page_count": 2}, " ".join(EXPECTED_TEXT))


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("table pagination self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    fixture = Path(__file__).resolve().parent / "fixtures/table-pagination"
    with tempfile.TemporaryDirectory(prefix="pliego-table-pagination-") as temp:
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
                "index.html",
            ],
            cwd=fixture,
            env=environment,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        require(result.returncode == 0, f"Pliego exited with {result.returncode}: {result.stderr[-4000:]}")
        summary = final_summary(result)
        require(summary.get("status") == "rendered", f"Pliego did not render: {summary!r}")
        verify(
            read_json(summary.get("scene_artifact"), "scene artifact"),
            read_json(summary.get("layout_debug"), "layout debug artifact"),
            read_json(summary.get("pdf_structure"), "PDF structure artifact"),
            extract_pdf_text(Path(summary["document_pdf"])),
        )
    print("table pagination check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
