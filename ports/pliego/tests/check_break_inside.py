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

TIMEOUT_SECONDS = 60
FITTING_TEXT = "ALPHAONE BETATWOO"
FITTING_SOURCE = f"Lead.{FITTING_TEXT}"
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
    return [page.get("expected_extracted_unicode", "") for page in pages if isinstance(page, dict)]


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


def continuation_tokens(page_sequence: dict[str, Any]) -> list[dict[str, Any]]:
    continuations = page_sequence.get("continuations")
    if not isinstance(continuations, list):
        return []
    return [item["token"] for item in continuations if isinstance(item, dict) and isinstance(item.get("token"), dict)]


def proves_fitting_avoidance(
    avoided_text: list[str],
    avoided_sequence: dict[str, Any],
    control_text: list[str],
    control_sequence: dict[str, Any],
) -> bool:
    if (
        len(avoided_text) != 2
        or len(control_text) != 2
        or "".join(avoided_text) != FITTING_SOURCE
        or "".join(control_text) != FITTING_SOURCE
        or avoided_text[0] != "Lead."
        or "ALPHAONE" not in avoided_text[1]
        or "BETATWOO" not in avoided_text[1]
        or "ALPHAONE" not in control_text[0]
        or "BETATWOO" in control_text[0]
        or "ALPHAONE" in control_text[1]
        or "BETATWOO" not in control_text[1]
        or avoided_text == control_text
        or len(avoided_sequence.get("pages", [])) != 2
        or len(control_sequence.get("pages", [])) != 2
    ):
        return False
    avoided = continuation_tokens(avoided_sequence)
    control = continuation_tokens(control_sequence)
    if len(avoided) != 1 or len(control) != 1:
        return False
    avoided_token = avoided[0]
    control_token = control[0]
    return (
        avoided_token.get("kind") == "block"
        and avoided_token.get("next_child_index") == 1
        and avoided_token.get("forced") is False
        and avoided_token.get("break_inside_avoid") is True
        and avoided_token.get("retry_count") == 1
        and avoided_token.get("resume_page_index") == 1
        and control_token.get("kind") == "inline"
        and control_token.get("child_index") == 1
        and control_token.get("next_line_index") == 1
        and control_token.get("resume_page_index") == 1
    )


def run_fixture(
    binary: Path,
    fixture_name: str,
    page_size: str,
    page_margins: str,
    output_root: Path | None,
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
        scene = read_json(summary.get("scene_artifact"), "scene artifact")
        layout = read_json(summary.get("layout_debug"), "layout debug artifact")
        structure = read_json(summary.get("pdf_structure"), "PDF structure artifact")
        if output_root is not None:
            artifacts = summary.get("artifacts")
            if not isinstance(artifacts, str) or not Path(artifacts).is_dir():
                fail(f"{fixture_name} has no artifact bundle to retain")
            shutil.copytree(artifacts, output_root / fixture_name)
        return scene, layout, structure


def check_fitting(binary: Path, output_root: Path | None) -> None:
    avoided_scene, avoided_layout, avoided_structure = run_fixture(
        binary,
        "break-inside-fit",
        "200x120",
        "10,10,10,10",
        output_root,
    )
    control_scene, control_layout, control_structure = run_fixture(
        binary,
        "break-inside-control",
        "200x120",
        "10,10,10,10",
        output_root,
    )
    avoided_text = scene_page_text(avoided_scene)
    control_text = scene_page_text(control_scene)
    avoided_sequence = avoided_layout.get("page_sequence")
    control_sequence = control_layout.get("page_sequence")
    # https://drafts.csswg.org/css-break/#break-within
    if (
        not isinstance(avoided_sequence, dict)
        or not isinstance(control_sequence, dict)
        or not proves_fitting_avoidance(
            avoided_text,
            avoided_sequence,
            control_text,
            control_sequence,
        )
    ):
        fail(
            "fitting avoid/control pair did not exercise distinct block-vs-inline pagination: "
            f"{avoided_text!r}, {control_text!r}, "
            f"{avoided_sequence!r}, {control_sequence!r}"
        )
    if pdf_page_text(avoided_structure) != avoided_text:
        fail("fitting avoided child PDF/source order differs")
    if pdf_page_text(control_structure) != control_text:
        fail("fitting control child PDF/source order differs")
    if avoided_sequence.get("warnings") or control_sequence.get("warnings"):
        fail("fitting avoid/control pair unexpectedly warned")
    if any(retry > 1 for retry in retry_counts(avoided_sequence)):
        fail("fitting avoidance retried more than once")


def check_oversized(binary: Path, output_root: Path | None) -> None:
    scene, layout, structure = run_fixture(
        binary,
        "break-inside-oversized",
        "180x80",
        "10,10,10,10",
        output_root,
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
    if (
        not isinstance(continuations, list)
        or not continuations
        or any(
            continuation.get("token", {}).get("kind") != "inline"
            for continuation in continuations
            if isinstance(continuation, dict)
        )
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
    avoided_text = ["Lead.", FITTING_TEXT]
    avoided_sequence = {
        "pages": [{}, {}],
        "continuations": [
            {
                "token": {
                    "kind": "block",
                    "next_child_index": 1,
                    "forced": False,
                    "break_inside_avoid": True,
                    "retry_count": 1,
                    "resume_page_index": 1,
                }
            }
        ],
    }
    control_text = ["Lead.ALPHAONE ", "BETATWOO"]
    control_sequence = {
        "pages": [{}, {}],
        "continuations": [
            {
                "token": {
                    "kind": "inline",
                    "child_index": 1,
                    "next_line_index": 1,
                    "resume_page_index": 1,
                }
            }
        ],
    }
    if not proves_fitting_avoidance(
        avoided_text,
        avoided_sequence,
        control_text,
        control_sequence,
    ):
        fail("self-test did not recognize a discriminating avoid/control pair")
    old_false_green = {
        "pages": [{}, {}],
        "continuations": [
            {
                "token": {
                    "kind": "block",
                    "next_child_index": 1,
                    "forced": False,
                    "break_inside_avoid": False,
                    "retry_count": 0,
                    "resume_page_index": 1,
                }
            }
        ],
    }
    if proves_fitting_avoidance(
        avoided_text,
        avoided_sequence,
        avoided_text,
        old_false_green,
    ):
        fail("self-test accepted the prior one-line generic-move false green")


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("break-inside self-test: ok")
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
    check_fitting(binary, output)
    check_oversized(binary, output)
    suffix = f" (artifacts: {output})" if output is not None else ""
    print(f"break-inside check: ok{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
