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
from collections import Counter
from pathlib import Path
from typing import Any

TIMEOUT_SECONDS = 60
LINE_COUNT = 12
EXPECTED_TOKENS = (
    ["Lead."]
    + [token for index in range(LINE_COUNT) for token in (f"A{index}", f"B{index}")]
    + ["TailA", "TailB"]
)


def fail(message: str, code: int = 1) -> None:
    print(f"oversized table row check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_object(path: Path, description: str) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
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


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the oversized-row gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-nopgbrk", str(pdf), "-"],
        capture_output=True,
        timeout=20,
        check=False,
    )
    require(result.returncode == 0, result.stderr.decode(errors="replace"))
    return result.stdout.decode("utf-8-sig")


def text_operations(scene: dict[str, Any]) -> list[tuple[int, dict[str, Any]]]:
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages")
    return [
        (page_index, operation)
        for page_index, page in enumerate(pages)
        if isinstance(page, dict)
        for operation in page.get("operations", [])
        if isinstance(operation, dict) and operation.get("type") == "text"
    ]


def verify_render(
    scene: dict[str, Any],
    layout: dict[str, Any],
    structure: dict[str, Any],
    pdf_text: str,
) -> None:
    operations = text_operations(scene)
    pages = scene.get("pages", [])
    require(len(pages) >= 3, "oversized row did not make multi-page progress")
    tokens = [str(operation.get("text", "")) for _, operation in operations]
    require(Counter(tokens) == Counter(EXPECTED_TOKENS), "text was lost or duplicated")

    geometry: dict[str, tuple[int, float, float]] = {}
    column_x: dict[str, set[float]] = {"A": set(), "B": set()}
    for page_index, operation in operations:
        text = str(operation.get("text", ""))
        match = re.fullmatch(r"([AB])(\d+)", text)
        if not match:
            continue
        glyphs = operation.get("glyphs")
        require(isinstance(glyphs, list) and glyphs, f"{text} has no glyphs")
        x = glyphs[0].get("x")
        y = glyphs[0].get("y")
        require(isinstance(x, (int, float)) and isinstance(y, (int, float)), f"{text} has no geometry")
        geometry[text] = (page_index, round(float(x), 5), round(float(y), 5))
        column_x[match.group(1)].add(round(float(x), 5))
    require(len(geometry) == LINE_COUNT * 2, "cell text geometry is incomplete")
    require(all(len(values) == 1 for values in column_x.values()), f"column grid drifted: {column_x!r}")
    require(next(iter(column_x["A"])) < next(iter(column_x["B"])), "column order changed")
    for index in range(LINE_COUNT):
        a = geometry[f"A{index}"]
        b = geometry[f"B{index}"]
        require((a[0], a[2]) == (b[0], b[2]), f"cell fragments diverged at line {index}: {a!r} {b!r}")

    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "layout has no page sequence")
    require(len(page_sequence.get("pages", [])) == len(pages), "layout and scene page counts differ")
    resumes = page_sequence.get("table_cell_continuations")
    require(isinstance(resumes, list) and resumes, "per-cell continuation state is absent")
    resume_pages = sorted({item.get("resume_page_index") for item in resumes if isinstance(item, dict)})
    require(resume_pages == list(range(1, len(pages))), f"resume pages are not dense: {resume_pages!r}")
    for page_index in resume_pages:
        page_resumes = [item for item in resumes if item.get("resume_page_index") == page_index]
        require([item.get("cell_index") for item in page_resumes] == [0, 1], f"page {page_index} cells are not aligned")
    for cell_index in (0, 1):
        progress = [
            item.get("next_fragment_index")
            for item in resumes
            if item.get("cell_index") == cell_index and item.get("next_fragment_index") is not None
        ]
        require(progress == sorted(set(progress)), f"cell {cell_index} did not make strict progress: {progress!r}")
    continuations = page_sequence.get("continuations")
    row_continuations = [
        item
        for item in continuations
        if isinstance(item, dict)
        and item.get("token", {}).get("kind") == "table"
        and item.get("token", {}).get("next_row_index") == 0
    ]
    require(len(row_continuations) == len(pages) - 1, "row continuation count differs")
    require(page_sequence.get("warnings") in (None, []), "breakable oversized row warned")
    require(structure.get("page_count") == len(pages), "PDF page count differs")
    require(canonical_text(pdf_text) == " ".join(EXPECTED_TOKENS), f"pdftotext differs: {pdf_text!r}")


def verify_warning(layout: dict[str, Any]) -> None:
    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "warning fixture has no page sequence")
    warnings = page_sequence.get("warnings")
    warning = warnings[0] if isinstance(warnings, list) and len(warnings) == 1 else {}
    require(warning.get("kind") == "oversized-table-cell", f"typed warning differs: {warnings!r}")
    require(warning.get("row_index") == 0 and warning.get("cell_index") == 0, repr(warning))
    require(warning.get("block_size", 0) > warning.get("available_block_size", 0), repr(warning))


def run(binary: Path, fixture_name: str, temp: Path) -> subprocess.CompletedProcess[str]:
    fixture = Path(__file__).resolve().parent / f"fixtures/{fixture_name}"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(temp), "TMP": str(temp), "TEMP": str(temp)})
    try:
        return subprocess.run(
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
            timeout=TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        fail(f"{fixture_name} exceeded {TIMEOUT_SECONDS}s without progress")


def self_test() -> None:
    pages = [{"operations": []} for _ in range(3)]
    pages[0]["operations"].append({"type": "text", "text": "Lead.", "glyphs": [{"x": 10.0, "y": 10.0}]})
    for index in range(LINE_COUNT):
        page = 0 if index < 4 else 1 if index < 9 else 2
        y = float((index % 5) * 24 + 20)
        pages[page]["operations"].extend(
            [
                {"type": "text", "text": f"A{index}", "glyphs": [{"x": 10.0, "y": y}]},
                {"type": "text", "text": f"B{index}", "glyphs": [{"x": 80.0, "y": y}]},
            ]
        )
    pages[2]["operations"].extend(
        [
            {"type": "text", "text": "TailA", "glyphs": [{"x": 10.0, "y": 110.0}]},
            {"type": "text", "text": "TailB", "glyphs": [{"x": 80.0, "y": 110.0}]},
        ]
    )
    resumes = [
        {
            "page_index": page - 1,
            "row_index": 0,
            "cell_index": cell,
            "next_fragment_index": 4 if page == 1 else 9,
            "resume_page_index": page,
        }
        for page in (1, 2)
        for cell in (0, 1)
    ]
    continuations = [
        {"page_index": page - 1, "token": {"kind": "table", "next_row_index": 0}}
        for page in (1, 2)
    ]
    layout = {
        "page_sequence": {
            "pages": [{}, {}, {}],
            "continuations": continuations,
            "table_cell_continuations": resumes,
            "warnings": [],
        }
    }
    verify_render(
        {"pages": pages},
        layout,
        {"page_count": 3},
        " ".join(EXPECTED_TOKENS),
    )
    verify_warning(
        {
            "page_sequence": {
                "warnings": [
                    {
                        "kind": "oversized-table-cell",
                        "row_index": 0,
                        "cell_index": 0,
                        "block_size": 180.0,
                        "available_block_size": 120.0,
                    }
                ]
            }
        }
    )


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("oversized table row self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")

    with tempfile.TemporaryDirectory(prefix="pliego-table-oversized-row-") as temp:
        rendered = run(binary, "table-oversized-row", Path(temp))
        require(rendered.returncode == 0, f"breakable fixture failed: {rendered.stderr[-4000:]}")
        summary = final_summary(rendered)
        require(summary.get("status") == "rendered", repr(summary))
        verify_render(
            read_object(Path(summary["scene_artifact"]), "scene artifact"),
            read_object(Path(summary["layout_debug"]), "layout debug"),
            read_object(Path(summary["pdf_structure"]), "PDF structure"),
            extract_pdf_text(Path(summary["document_pdf"])),
        )

    with tempfile.TemporaryDirectory(prefix="pliego-table-unbreakable-") as temp:
        unbreakable = run(binary, "table-oversized-row-unbreakable", Path(temp))
        require(unbreakable.returncode == 0, f"unbreakable fixture failed: {unbreakable.stderr[-4000:]}")
        warning_summary = final_summary(unbreakable)
        require(warning_summary.get("status") == "rendered", repr(warning_summary))
        artifacts = Path(warning_summary["artifacts"])
        verify_warning(read_object(artifacts / "layout-debug.json", "warning layout debug"))
    print("oversized table row check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
