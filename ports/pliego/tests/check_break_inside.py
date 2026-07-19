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
OVERSIZED_SOURCE = (
    "Café niño jalapeño déjà vu coöperate façade résumé naïve voilà über großstraße. "
    "Página siguiente conserva cada carácter sin perder ni duplicar texto."
)


def fail(message: str, code: int = 1) -> None:
    print(f"break-inside check: {message}", file=sys.stderr)
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


def pdf_page_text(structure: dict[str, Any]) -> list[str]:
    pages = structure.get("pages")
    if not isinstance(pages, list):
        fail("PDF structure has no pages array")
    return [
        page.get("expected_extracted_unicode", "")
        for page in pages
        if isinstance(page, dict)
    ]


def retry_counts(page_sequence: dict[str, Any]) -> list[int]:
    counts = []
    for collection, nested in (("continuations", "token"), ("warnings", None)):
        for item in page_sequence.get(collection, []):
            if not isinstance(item, dict):
                continue
            value = item.get(nested, {}).get("retry_count") if nested else item.get("retry_count")
            if isinstance(value, int):
                counts.append(value)
    return counts


def run_fixture(
    binary: Path,
    fixture_name: str,
    page_size: str,
    page_margins: str,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    fixture = Path(__file__).resolve().parent / f"fixtures/{fixture_name}"
    with tempfile.TemporaryDirectory(prefix=f"pliego-{fixture_name}-") as temp:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp, "TMP": temp, "TEMP": temp})
        try:
            result = subprocess.run(
                [
                    str(binary),
                    "--allow-host-fonts",
                    "--page-size",
                    page_size,
                    "--page-margins",
                    page_margins,
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
            fail(f"{fixture_name} exceeded {TIMEOUT_SECONDS}s")
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "no process output"
            fail(f"{fixture_name} exited with {result.returncode}: {detail[-4000:]}")
        summary = final_summary(result)
        if summary.get("status") != "rendered":
            fail(f"{fixture_name} did not render: {summary!r}")
        return (
            read_json(summary.get("scene_artifact"), "scene artifact"),
            read_json(summary.get("layout_debug"), "layout debug artifact"),
            read_json(summary.get("pdf_structure"), "PDF structure artifact"),
        )


def check_fitting(binary: Path) -> None:
    scene, layout, structure = run_fixture(
        binary,
        "break-inside-fit",
        "200x120",
        "10,10,10,10",
    )
    expected = ["Lead.", "Kept together."]
    if scene_page_text(scene) != expected or pdf_page_text(structure) != expected:
        fail("fitting avoided child did not remain intact on the next page")
    page_sequence = layout.get("page_sequence")
    if not isinstance(page_sequence, dict) or len(page_sequence.get("pages", [])) != 2:
        fail(f"fitting avoided child did not produce two pages: {page_sequence!r}")
    continuations = page_sequence.get("continuations")
    token = continuations[0].get("token", {}) if isinstance(continuations, list) and len(continuations) == 1 else {}
    # https://drafts.csswg.org/css-break/#break-within
    if (
        token.get("kind") != "block"
        or token.get("next_child_index") != 1
        or token.get("forced") is not False
        or token.get("break_inside_avoid") is not True
        or token.get("retry_count") != 1
        or token.get("resume_page_index") != 1
    ):
        fail(f"fitting avoidance decision differs: {continuations!r}")
    if page_sequence.get("warnings"):
        fail(f"fitting avoidance unexpectedly warned: {page_sequence['warnings']!r}")
    if any(retry > 1 for retry in retry_counts(page_sequence)):
        fail("fitting avoidance retried more than once")


def check_oversized(binary: Path) -> None:
    scene, layout, structure = run_fixture(
        binary,
        "break-inside-oversized",
        "180x80",
        "10,10,10,10",
    )
    scene_text = scene_page_text(scene)
    pdf_text = pdf_page_text(structure)
    if len(scene_text) < 2 or any(not page for page in scene_text):
        fail(f"oversized avoided child did not make page progress: {scene_text!r}")
    if "".join(scene_text) != OVERSIZED_SOURCE or "".join(pdf_text) != OVERSIZED_SOURCE:
        fail("oversized avoided child lost or duplicated source text")
    if structure.get("page_count") != len(scene_text) or len(pdf_text) != len(scene_text):
        fail("oversized scene/PDF page count differs")
    page_sequence = layout.get("page_sequence")
    if not isinstance(page_sequence, dict) or len(page_sequence.get("pages", [])) != len(scene_text):
        fail(f"oversized layout/scene page count differs: {page_sequence!r}")
    continuations = page_sequence.get("continuations")
    if not isinstance(continuations, list) or not continuations or any(
        continuation.get("token", {}).get("kind") != "inline"
        for continuation in continuations
        if isinstance(continuation, dict)
    ):
        fail(f"oversized avoided child did not use inline continuation: {continuations!r}")
    warnings = page_sequence.get("warnings")
    warning = warnings[0] if isinstance(warnings, list) and len(warnings) == 1 else {}
    if (
        warning.get("kind") != "oversized-break-inside-avoid"
        or warning.get("retry_count") != 0
        or warning.get("block_size", 0) <= warning.get("available_block_size", 0)
    ):
        fail(f"oversized avoidance warning differs: {warnings!r}")
    if any(retry > 1 for retry in retry_counts(page_sequence)):
        fail("oversized avoidance retried more than once")


def self_test() -> None:
    scene = {"pages": [{"operations": [{"type": "text", "text": "Alpha."}]}]}
    structure = {"pages": [{"expected_extracted_unicode": "Alpha."}]}
    if scene_page_text(scene) != ["Alpha."] or pdf_page_text(structure) != ["Alpha."]:
        fail("self-test did not preserve source order")
    page_sequence = {
        "continuations": [{"token": {"retry_count": 1}}],
        "warnings": [{"retry_count": 0}],
    }
    if retry_counts(page_sequence) != [1, 0]:
        fail("self-test did not retain bounded retry diagnostics")


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("break-inside self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    if not binary.is_file():
        fail(f"Pliego binary does not exist: {binary}", 2)
    check_fitting(binary)
    check_oversized(binary)
    print("break-inside check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
