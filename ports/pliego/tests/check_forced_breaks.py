#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


def fail(message: str, code: int = 1) -> None:
    print(f"forced break check: {message}", file=sys.stderr)
    raise SystemExit(code)


def read_json(path: Any, description: str) -> dict[str, Any]:
    if not isinstance(path, str):
        fail(f"terminal result has no {description} path")
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        fail(f"{description} is not an object")
    return value


def final_summary(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    if not lines:
        fail("Pliego produced no stdout JSON")
    try:
        summary = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}")
    if not isinstance(summary, dict):
        fail("final stdout JSON is not an object")
    return summary


def scene_page_text(scene: dict[str, Any]) -> list[str]:
    pages = scene.get("pages")
    if not isinstance(pages, list):
        fail("scene has no pages array")
    return [
        "".join(
            operation.get("text", "")
            for operation in page.get("operations", [])
            if isinstance(operation, dict) and operation.get("type") == "text"
        )
        for page in pages
        if isinstance(page, dict)
    ]


def continuation_trace(page_sequence: dict[str, Any]) -> list[tuple[Any, ...]]:
    continuations = page_sequence.get("continuations")
    if not isinstance(continuations, list):
        return []
    return [
        (
            continuation.get("page_index"),
            continuation.get("token", {}).get("kind"),
            continuation.get("token", {}).get("next_child_index"),
            continuation.get("token", {}).get("forced"),
            continuation.get("token", {}).get("resume_page_index"),
        )
        for continuation in continuations
        if isinstance(continuation, dict)
    ]


def check_case(
    binary: Path,
    fixture_name: str,
    expected_text: list[str],
    output_root: Path | None,
) -> None:
    fixture = Path(__file__).resolve().parent / f"fixtures/{fixture_name}"
    with tempfile.TemporaryDirectory(prefix=f"pliego-{fixture_name}-") as temp:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp, "TMP": temp, "TEMP": temp})
        result = subprocess.run(
            [
                str(binary),
                "--allow-host-fonts",
                "--page-size",
                "200x120",
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
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "no process output"
            fail(f"Pliego exited with {result.returncode}: {detail[-4000:]}")

        summary = final_summary(result)
        if summary.get("status") != "rendered":
            fail(f"Pliego did not render: {summary!r}")
        scene = read_json(summary.get("scene_artifact"), "scene artifact")
        layout = read_json(summary.get("layout_debug"), "layout debug artifact")
        structure = read_json(summary.get("pdf_structure"), "PDF structure artifact")

        page_text = scene_page_text(scene)
        if page_text != expected_text:
            fail(f"{fixture_name} page/source order differs: {page_text!r}")

        page_sequence = layout.get("page_sequence")
        page_count = len(expected_text)
        if not isinstance(page_sequence, dict) or len(page_sequence.get("pages", [])) != page_count:
            fail(f"{fixture_name} did not produce {page_count} non-empty pages: {page_sequence!r}")
        normalized = continuation_trace(page_sequence)
        # https://drafts.csswg.org/css-break/#forced-breaks
        expected_continuations = [
            (page, "block", page + 1, True, page + 1)
            for page in range(page_count - 1)
        ]
        if normalized != expected_continuations:
            fail(f"{fixture_name} forced boundary trace differs: {normalized!r}")
        if structure.get("page_count") != page_count:
            fail(f"{fixture_name} PDF page count differs: {structure!r}")
        pdf_text = [
            page.get("expected_extracted_unicode")
            for page in structure.get("pages", [])
            if isinstance(page, dict)
        ]
        if pdf_text != expected_text:
            fail(f"{fixture_name} PDF page/source order differs: {pdf_text!r}")
        if output_root is not None:
            artifacts = summary.get("artifacts")
            if not isinstance(artifacts, str) or not Path(artifacts).is_dir():
                fail(f"{fixture_name} has no artifact bundle to retain")
            shutil.copytree(artifacts, output_root / fixture_name)


def self_test() -> None:
    scene = {
        "pages": [
            {"operations": [{"type": "text", "text": "Alpha."}]},
            {"operations": [{"type": "text", "text": "Beta."}]},
        ]
    }
    page_sequence = {
        "continuations": [{
            "page_index": 0,
            "token": {
                "kind": "block",
                "next_child_index": 1,
                "forced": True,
                "resume_page_index": 1,
            },
        }]
    }
    if scene_page_text(scene) != ["Alpha.", "Beta."] or continuation_trace(page_sequence) != [
        (0, "block", 1, True, 1)
    ]:
        fail("self-test did not retain source order and the forced marker")


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("forced break self-test: ok")
        return 0
    if len(arguments) not in (1, 2):
        fail(
            f"usage: {Path(sys.argv[0]).name} <pliego-binary> [output-directory] | --self-test",
            2,
        )
    binary = Path(arguments[0]).expanduser().resolve()
    if not binary.is_file():
        fail(f"Pliego binary does not exist: {binary}", 2)
    output = Path(arguments[1]).expanduser().resolve() if len(arguments) == 2 else None
    if output is not None:
        if output.exists():
            fail(f"output already exists: {output}", 2)
        output.mkdir(parents=True)
    check_case(binary, "forced-break-before", ["Alpha.", "Beta."], output)
    check_case(binary, "forced-break-after", ["Alpha.", "Beta."], output)
    check_case(binary, "forced-breaks", ["Alpha.", "Beta.", "Gamma.", "Delta."], output)
    suffix = f" (artifacts: {output})" if output is not None else ""
    print(f"forced break check: ok{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
