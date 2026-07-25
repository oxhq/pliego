#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

TIMEOUT_SECONDS = 60
RETRY_LIMIT = 1
CONTAINED_TEXT = ["Lead.", "R0A", "R0B", "SPAN", "R1B", "R2B", "TAILA", "TAILB"]
FALLBACK_TEXT = ["Lead.", "SPAN", "R0B", "R1B", "TAILA", "TAILB"]


def fail(message: str, code: int = 1) -> None:
    print(f"table rowspan check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_object(path: Any, description: str) -> dict[str, Any]:
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


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def text_pages(scene: dict[str, Any]) -> list[list[dict[str, Any]]]:
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages")
    return [
        [
            operation
            for operation in page.get("operations", [])
            if isinstance(operation, dict) and operation.get("type") == "text"
        ]
        for page in pages
        if isinstance(page, dict)
    ]


def text_tokens(pages: list[list[dict[str, Any]]]) -> list[str]:
    return [
        canonical_text(str(operation.get("text", "")))
        for operations in pages
        for operation in operations
    ]


def token_geometry(
    pages: list[list[dict[str, Any]]],
) -> dict[str, tuple[int, float]]:
    geometry: dict[str, tuple[int, float]] = {}
    for page_index, operations in enumerate(pages):
        for operation in operations:
            token = canonical_text(str(operation.get("text", "")))
            glyphs = operation.get("glyphs")
            require(isinstance(glyphs, list) and glyphs, f"{token} has no glyph geometry")
            x = glyphs[0].get("x")
            require(isinstance(x, (int, float)), f"{token} has no x coordinate")
            require(token not in geometry, f"{token} was painted more than once")
            geometry[token] = (page_index, round(float(x), 6))
    return geometry


def page_sequence(layout: dict[str, Any]) -> dict[str, Any]:
    sequence = layout.get("page_sequence")
    require(isinstance(sequence, dict), "layout has no page sequence")
    return sequence


def table_continuations(sequence: dict[str, Any]) -> list[dict[str, Any]]:
    continuations = sequence.get("continuations")
    require(isinstance(continuations, list), "continuation trace is absent")
    return [
        item
        for item in continuations
        if isinstance(item, dict) and item.get("token", {}).get("kind") == "table"
    ]


def verify_contained(scene: dict[str, Any], layout: dict[str, Any]) -> None:
    pages = text_pages(scene)
    require(text_tokens(pages) == CONTAINED_TEXT, "contained rowspan text was lost, duplicated, or reordered")
    require(len(pages) == 2, "contained rowspan did not produce the expected two pages")
    geometry = token_geometry(pages)

    span_page = geometry["SPAN"][0]
    require(
        {geometry[token][0] for token in ("SPAN", "R1B", "R2B")} == {span_page},
        "active rowspan run split across pages",
    )
    require(geometry["R0A"][0] < span_page, "fitting rowspan run was not moved as a unit")
    require(geometry["TAILA"][0] == span_page, "tail row unexpectedly moved")
    require(
        len({geometry[token][1] for token in ("R0A", "SPAN", "TAILA")}) == 1,
        "first table column drifted",
    )
    require(
        len({geometry[token][1] for token in ("R0B", "R1B", "R2B", "TAILB")}) == 1,
        "second table column drifted",
    )
    require(geometry["R0A"][1] < geometry["R0B"][1], "table column order changed")

    sequence = page_sequence(layout)
    require(len(sequence.get("pages", [])) == len(pages), "scene and layout page counts differ")
    decisions = sequence.get("table_breaks")
    require(isinstance(decisions, list), "table break trace is absent")
    require(
        [item.get("next_row_index") for item in decisions] == [1, 2, 3],
        f"rowspan boundary trace differs: {decisions!r}",
    )
    selected = [item for item in decisions if item.get("selected") is True]
    require(
        len(selected) == 1
        and selected[0].get("next_row_index") == 1
        and selected[0].get("constraint") == "auto"
        and selected[0].get("retry_count") == 0,
        f"rowspan selection differs: {selected!r}",
    )
    require(
        all(
            item.get("retry_count") == 0 and item.get("retry_limit") == RETRY_LIMIT
            for item in decisions
        ),
        "rowspan changed constraint retry accounting",
    )
    continuations = table_continuations(sequence)
    require(
        len(continuations) == 1
        and continuations[0].get("token", {}).get("next_row_index") == 1
        and continuations[0].get("token", {}).get("resume_page_index") == 1,
        f"rowspan continuation differs: {continuations!r}",
    )
    require(sequence.get("warnings") in (None, []), "contained rowspan unexpectedly warned")


def verify_forced_fallback(scene: dict[str, Any], layout: dict[str, Any]) -> None:
    require(
        text_tokens(text_pages(scene)) == FALLBACK_TEXT,
        "forced-cross-page fallback text was lost, duplicated, or reordered",
    )
    sequence = page_sequence(layout)
    warnings = sequence.get("warnings")
    warning = warnings[0] if isinstance(warnings, list) and len(warnings) == 1 else {}
    require(
        warning.get("kind") == "unsupported-table-rowspan-pagination"
        and warning.get("table_node") is not None
        and warning.get("first_row_index") == 0
        and warning.get("end_row_index") == 2
        and warning.get("forced_break_inside") is True
        and warning.get("block_size", 0) <= warning.get("available_block_size", 0),
        f"forced-cross-page warning differs: {warnings!r}",
    )
    require(sequence.get("table_breaks") in (None, []), "fallback partially applied retained row breaks")
    require(not table_continuations(sequence), "fallback emitted a retained-table continuation")


def run(binary: Path, document: str, temp: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    fixture = Path(__file__).resolve().parent / "fixtures/table-rowspans"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(temp), "TMP": str(temp), "TEMP": str(temp)})
    try:
        result = subprocess.run(
            [
                str(binary),
                "--allow-host-fonts",
                "--page-size",
                "240x140",
                "--page-margins",
                "10,10,10,10",
                document,
            ],
            cwd=fixture,
            env=environment,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        fail(f"{document} exceeded {TIMEOUT_SECONDS}s without progress")
    require(result.returncode == 0, f"{document} failed: {result.stderr[-4000:]}")
    summary = final_summary(result)
    require(summary.get("status") == "rendered", repr(summary))
    return (
        read_object(summary.get("scene_artifact"), "scene artifact"),
        read_object(summary.get("layout_debug"), "layout debug artifact"),
    )


def operation(text: str, x: float) -> dict[str, Any]:
    return {"type": "text", "text": text, "glyphs": [{"x": x}]}


def self_test() -> None:
    contained_pages = [
        {"operations": [operation("Lead.", 10.0), operation("R0A", 10.0), operation("R0B", 80.0)]},
        {
            "operations": [
                operation("SPAN", 10.0),
                operation("R1B", 80.0),
                operation("R2B", 80.0),
                operation("TAILA", 10.0),
                operation("TAILB", 80.0),
            ]
        },
    ]
    decisions = [
        {
            "next_row_index": row,
            "constraint": "auto",
            "retry_count": 0,
            "retry_limit": RETRY_LIMIT,
            "selected": row == 1,
        }
        for row in (1, 2, 3)
    ]
    verify_contained(
        {"pages": contained_pages},
        {
            "page_sequence": {
                "pages": [{}, {}],
                "continuations": [
                    {
                        "page_index": 0,
                        "token": {
                            "kind": "table",
                            "next_row_index": 1,
                            "resume_page_index": 1,
                        },
                    }
                ],
                "table_breaks": decisions,
                "warnings": [],
            }
        },
    )

    verify_forced_fallback(
        {"pages": [{"operations": [operation(token, 10.0) for token in FALLBACK_TEXT]}]},
        {
            "page_sequence": {
                "continuations": [],
                "table_breaks": [],
                "warnings": [
                    {
                        "kind": "unsupported-table-rowspan-pagination",
                        "table_node": 99,
                        "first_row_index": 0,
                        "end_row_index": 2,
                        "block_size": 64.0,
                        "available_block_size": 120.0,
                        "forced_break_inside": True,
                    }
                ],
            }
        },
    )


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("table rowspan self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    with tempfile.TemporaryDirectory(prefix="pliego-table-rowspans-") as temp:
        verify_contained(*run(binary, "index.html", Path(temp)))
    with tempfile.TemporaryDirectory(prefix="pliego-table-rowspans-forced-") as temp:
        verify_forced_fallback(*run(binary, "forced-cross-page.html", Path(temp)))
    print("table rowspan check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
