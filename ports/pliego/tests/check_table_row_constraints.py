#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import json
import os
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path
from typing import Any

TIMEOUT_SECONDS = 60
RETRY_LIMIT = 1
ROW_TOKENS = ["Lead."] + [f"R{row}{column}" for row in range(9) for column in "AB"]
OVERSIZED_TOKENS = ["Lead."] + [token for row in range(12) for token in (f"A{row}", f"B{row}")] + ["TailA", "TailB"]
SELECTED_CONSTRAINTS = {
    1: "forced-before",
    3: "avoid-group",
    6: "avoid-row",
    8: "forced-after",
}


def fail(message: str, code: int = 1) -> None:
    print(f"table row constraints check: {message}", file=sys.stderr)
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


def scene_tokens(scene: dict[str, Any]) -> list[str]:
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages")
    return [
        text
        for page in pages
        if isinstance(page, dict)
        for operation in page.get("operations", [])
        if isinstance(operation, dict) and operation.get("type") == "text"
        if (text := canonical_text(str(operation.get("text", ""))))
    ]


def page_sequence(layout: dict[str, Any]) -> dict[str, Any]:
    value = layout.get("page_sequence")
    require(isinstance(value, dict), "layout has no page sequence")
    return value


def table_tokens(sequence: dict[str, Any]) -> list[dict[str, Any]]:
    continuations = sequence.get("continuations")
    require(isinstance(continuations, list), "continuation trace is absent")
    return [item for item in continuations if isinstance(item, dict) and item.get("token", {}).get("kind") == "table"]


def table_breaks(sequence: dict[str, Any]) -> list[dict[str, Any]]:
    decisions = sequence.get("table_breaks")
    require(isinstance(decisions, list), "table break trace is absent")
    return [item for item in decisions if isinstance(item, dict)]


def verify_retry_bounds(sequence: dict[str, Any]) -> None:
    decisions = table_breaks(sequence)
    require(
        all(item.get("retry_limit") == RETRY_LIMIT for item in decisions),
        "table break retry limit differs",
    )
    require(
        all(
            isinstance(item.get("retry_count"), int) and 0 <= item["retry_count"] <= item["retry_limit"]
            for item in decisions
        ),
        "table break retry count exceeded its limit",
    )


def verify_constraints(scene: dict[str, Any], layout: dict[str, Any]) -> None:
    require(scene_tokens(scene) == ROW_TOKENS, "constrained rows were lost, duplicated, or reordered")
    sequence = page_sequence(layout)
    decisions = table_breaks(sequence)
    require(
        [item.get("next_row_index") for item in decisions] == list(range(1, 9)),
        "not every constrained table boundary was traced once",
    )
    selected = {item["next_row_index"]: item.get("constraint") for item in decisions if item.get("selected") is True}
    require(selected == SELECTED_CONSTRAINTS, f"selected constraints differ: {selected!r}")
    require(
        all(
            item.get("retry_count") == (1 if item.get("constraint", "").startswith("avoid-") else 0)
            for item in decisions
            if item.get("selected") is True
        ),
        "forced/avoid retry counts differ",
    )
    selected_rows = list(selected)
    continuations = table_tokens(sequence)
    token_rows = [item.get("token", {}).get("next_row_index") for item in continuations]
    require(token_rows == selected_rows, "selected table breaks and continuation tokens differ")
    require(len(token_rows) == len(set(token_rows)), "a selected boundary emitted duplicate tokens")
    require(
        all(item.get("token", {}).get("resume_page_index") is not None for item in continuations),
        "selected continuation made no page progress",
    )
    require(sequence.get("warnings") in (None, []), "fitting constraints unexpectedly warned")
    verify_retry_bounds(sequence)


def verify_oversized(scene: dict[str, Any], layout: dict[str, Any]) -> None:
    tokens = scene_tokens(scene)
    require(Counter(tokens) == Counter(OVERSIZED_TOKENS), "oversized row text was lost or duplicated")
    sequence = page_sequence(layout)
    require(len(sequence.get("pages", [])) >= 3, "oversized avoided row made no multi-page progress")
    decisions = table_breaks(sequence)
    avoided = [item for item in decisions if item.get("next_row_index") == 0]
    require(
        len(avoided) == 1
        and avoided[0].get("constraint") == "avoid-row"
        and avoided[0].get("selected") is False
        and avoided[0].get("retry_count") == 0,
        f"oversized row fallback trace differs: {avoided!r}",
    )
    continuations = [item for item in table_tokens(sequence) if item.get("token", {}).get("next_row_index") == 0]
    progress = [(item.get("page_index"), item.get("token", {}).get("resume_page_index")) for item in continuations]
    require(bool(progress) and progress == sorted(set(progress)), "oversized row tokens did not progress once")
    resumes = sequence.get("table_cell_continuations")
    require(isinstance(resumes, list) and resumes, "oversized row has no cell continuation state")
    for cell_index in (0, 1):
        cell_progress = [
            item.get("next_fragment_index")
            for item in resumes
            if isinstance(item, dict)
            and item.get("cell_index") == cell_index
            and item.get("next_fragment_index") is not None
        ]
        require(
            cell_progress == sorted(set(cell_progress)),
            f"cell {cell_index} did not make strict progress",
        )
    warnings = sequence.get("warnings")
    warning = warnings[0] if isinstance(warnings, list) and len(warnings) == 1 else {}
    require(
        warning.get("kind") == "oversized-table-row-break-inside-avoid"
        and warning.get("row_index") == 0
        and warning.get("retry_count") == 0
        and warning.get("retry_limit") == RETRY_LIMIT
        and warning.get("block_size", 0) > warning.get("available_block_size", 0),
        f"oversized row warning differs: {warnings!r}",
    )
    verify_retry_bounds(sequence)


def run(binary: Path, document: str, temp: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    fixture = Path(__file__).resolve().parent / "fixtures/table-row-constraints"
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


def self_test() -> None:
    decisions = [
        {
            "next_row_index": row,
            "constraint": SELECTED_CONSTRAINTS.get(row, "auto"),
            "retry_count": 1 if row in (3, 6) else 0,
            "retry_limit": RETRY_LIMIT,
            "selected": row in SELECTED_CONSTRAINTS,
        }
        for row in range(1, 9)
    ]
    continuations = [
        {
            "page_index": page,
            "token": {
                "kind": "table",
                "next_row_index": row,
                "resume_page_index": page + 1,
            },
        }
        for page, row in enumerate(SELECTED_CONSTRAINTS)
    ]
    verify_constraints(
        {"pages": [{"operations": [{"type": "text", "text": token} for token in ROW_TOKENS]}]},
        {
            "page_sequence": {
                "continuations": continuations,
                "table_breaks": decisions,
                "warnings": [],
            }
        },
    )

    oversized_resumes = [
        {
            "cell_index": cell,
            "next_fragment_index": fragment,
        }
        for fragment in (4, 9)
        for cell in (0, 1)
    ]
    verify_oversized(
        {
            "pages": [
                {"operations": [{"type": "text", "text": token} for token in OVERSIZED_TOKENS]},
                {"operations": []},
                {"operations": []},
            ]
        },
        {
            "page_sequence": {
                "pages": [{}, {}, {}],
                "continuations": [
                    {
                        "page_index": page,
                        "token": {
                            "kind": "table",
                            "next_row_index": 0,
                            "resume_page_index": page + 1,
                        },
                    }
                    for page in (0, 1)
                ],
                "table_breaks": [
                    {
                        "next_row_index": 0,
                        "constraint": "avoid-row",
                        "retry_count": 0,
                        "retry_limit": RETRY_LIMIT,
                        "selected": False,
                    }
                ],
                "table_cell_continuations": oversized_resumes,
                "warnings": [
                    {
                        "kind": "oversized-table-row-break-inside-avoid",
                        "row_index": 0,
                        "block_size": 288.0,
                        "available_block_size": 120.0,
                        "retry_count": 0,
                        "retry_limit": RETRY_LIMIT,
                    }
                ],
            }
        },
    )


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("table row constraints self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    with tempfile.TemporaryDirectory(prefix="pliego-table-row-constraints-") as temp:
        verify_constraints(*run(binary, "index.html", Path(temp)))
    with tempfile.TemporaryDirectory(prefix="pliego-table-row-constraints-oversized-") as temp:
        verify_oversized(*run(binary, "oversized.html", Path(temp)))
    print("table row constraints check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
