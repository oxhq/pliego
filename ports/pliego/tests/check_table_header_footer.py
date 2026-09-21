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

HEADER = ["ITEM", "AMOUNT"]
BODY = [token for row in range(8) for token in (f"R{row}", f"${(row + 1) * 10}")]
FOOTER = ["TOTAL", "$360"]
LOGICAL_TEXT = ["LEAD", *HEADER, *BODY, *FOOTER]
UNSUPPORTED_REASONS = {
    "multiple-footers",
    "footer-too-tall-after-header",
    "rowspan",
    "collapsed-borders",
    "unsupported-layout",
}
UNSUPPORTED_CAPTURE_CODE = "SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS"
UNSUPPORTED_CAPTURE_MESSAGE = (
    "document scene capture is incomplete (unsupported paint kinds: collapsed-table-borders); "
    "rerun with --allow-partial-scene to inspect scene diagnostics"
)


def fail(message: str, code: int = 1) -> None:
    print(f"table header/footer check: {message}", file=sys.stderr)
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


def artifact(operation: dict[str, Any]) -> bool:
    semantics = operation.get("meta", {}).get("semantics")
    return (
        isinstance(semantics, dict)
        and semantics.get("role") == "artifact"
        and semantics.get("label") == "repeated-table-header"
    )


def scene_pages(scene: dict[str, Any]) -> list[list[dict[str, Any]]]:
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages")
    result = []
    for page in pages:
        require(isinstance(page, dict), "scene page is not an object")
        operations = page.get("operations")
        require(isinstance(operations, list), "scene page has no operations")
        result.append([operation for operation in operations if isinstance(operation, dict)])
    return result


def text_operations(operations: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [operation for operation in operations if operation.get("type") == "text"]


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def canonical_text_operations(
    operations: list[dict[str, Any]],
) -> list[tuple[dict[str, Any], str]]:
    canonical = [
        (operation, canonical_text(str(operation.get("text", "")))) for operation in text_operations(operations)
    ]
    return [(operation, text) for operation, text in canonical if text]


def operation_y(operation: dict[str, Any]) -> float:
    glyphs = operation.get("glyphs")
    require(isinstance(glyphs, list) and glyphs, f"text operation has no glyphs: {operation!r}")
    value = glyphs[0].get("y")
    require(isinstance(value, (int, float)), f"text operation has no y coordinate: {operation!r}")
    return float(value)


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the table header/footer gate")
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
    pdf_text: str,
) -> None:
    pages = scene_pages(scene)
    require(len(pages) == 4, f"supported table should occupy four pages: {len(pages)}")
    page_text = [canonical_text_operations(page) for page in pages]
    physical_text = [text for page in page_text for _, text in page]
    logical_text = [text for page in page_text for operation, text in page if not artifact(operation)]
    require(logical_text == LOGICAL_TEXT, f"logical table text differs: {logical_text!r}")
    require(
        canonical_text(pdf_text) == canonical_text(" ".join(physical_text)),
        f"PDF physical text order differs: {pdf_text!r}",
    )

    for page_index, operations in enumerate(page_text):
        headers = [(operation, text) for operation, text in operations if text in HEADER]
        require(
            [text for _, text in headers] == HEADER,
            f"page {page_index} does not start with one table header: {headers!r}",
        )
        require(
            all(artifact(operation) == (page_index > 0) for operation, _ in headers),
            f"page {page_index} header artifact state differs",
        )

    artifact_operations = [operation for page in pages[1:] for operation in page if artifact(operation)]
    require(
        {operation.get("type") for operation in artifact_operations} == {"text", "image", "link"},
        "repeated header did not clone text, raster image, and link operations",
    )
    require(all(physical_text.count(token) == 1 for token in BODY + FOOTER), "body or footer duplicated")

    occurrences: dict[str, tuple[int, float]] = {}
    for page_index, operations in enumerate(page_text):
        for operation, text in operations:
            if text in BODY + FOOTER:
                require(text not in occurrences, f"{text} appeared more than once")
                occurrences[text] = (page_index, operation_y(operation))
    last_body_page, last_body_y = max((occurrences[token] for token in BODY), key=lambda item: item)
    footer_page, footer_y = occurrences["TOTAL"]
    require(
        footer_page > last_body_page or (footer_page == last_body_page and footer_y > last_body_y),
        "terminal footer was not placed below the body",
    )

    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "layout has no page sequence")
    require(len(page_sequence.get("pages", [])) == len(pages), "layout and scene page counts differ")
    repeats = page_sequence.get("table_group_repeats")
    require(isinstance(repeats, list), "table header repeat trace is absent")
    require([repeat.get("page_index") for repeat in repeats] == [1, 2, 3], f"repeat pages differ: {repeats!r}")
    require(
        len({repeat.get("header_tag_id") for repeat in repeats}) == 1
        and all(repeat.get("table_node") is not None for repeat in repeats)
        and all(isinstance(repeat.get("block_size"), (int, float)) and repeat["block_size"] > 0 for repeat in repeats),
        f"repeat identity or geometry differs: {repeats!r}",
    )
    require(
        [repeat.get("target_block_start") for repeat in repeats]
        == sorted(repeat.get("target_block_start") for repeat in repeats),
        "repeat targets are not monotonic",
    )

    continuations = page_sequence.get("continuations")
    require(isinstance(continuations, list), "table continuation trace is absent")
    table_continuations = [item for item in continuations if item.get("token", {}).get("kind") == "table"]
    require(
        [item["token"].get("resume_page_index") for item in table_continuations] == [1, 2, 3],
        f"table continuation pages differ: {table_continuations!r}",
    )
    require(
        len({item["token"].get("table_node") for item in table_continuations}) == 1,
        "table continuation identity changed",
    )
    decisions = page_sequence.get("table_breaks")
    require(isinstance(decisions, list), "table break trace is absent")
    require(
        any(
            decision.get("selected") and decision.get("next_row_index") == 9 and decision.get("resume_page_index") == 3
            for decision in decisions
        ),
        "terminal footer move is absent from the break trace",
    )
    warnings = page_sequence.get("warnings")
    require(isinstance(warnings, list), "supported table warning trace is absent")
    require(
        len(warnings) == 1
        and warnings[0].get("kind") == "oversized-break-inside-avoid"
        and warnings[0].get("retry_count") == 0
        and warnings[0].get("block_size", 0) > warnings[0].get("available_block_size", 0),
        f"oversized wrapper avoid fallback warning differs: {warnings!r}",
    )

    events = layout.get("paint_events")
    require(isinstance(events, list), "paint trace is absent")
    require([event.get("sequence") for event in events] == list(range(len(events))), "paint trace is not dense")
    require(structure.get("page_count") == len(pages), "PDF page count differs")


def verify_unsupported(layout: dict[str, Any]) -> None:
    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "unsupported fixture has no page sequence")
    warnings = page_sequence.get("warnings")
    require(isinstance(warnings, list), "unsupported fixture has no warnings")
    group_warnings = [warning for warning in warnings if warning.get("kind") == "unsupported-table-group-pagination"]
    require(
        {warning.get("reason") for warning in group_warnings} == UNSUPPORTED_REASONS,
        f"typed unsupported reasons differ: {group_warnings!r}",
    )
    require(
        all(warning.get("table_node") is not None for warning in group_warnings),
        "unsupported table identity is absent",
    )
    require(page_sequence.get("table_group_repeats") in (None, []), "unsupported groups emitted repeat plans")


def operation(
    kind: str,
    text: str | None = None,
    y: float = 10.0,
    repeated: bool = False,
) -> dict[str, Any]:
    value: dict[str, Any] = {"type": kind}
    if text is not None:
        value.update({"text": text, "glyphs": [{"y": y}]})
    if repeated:
        value["meta"] = {"semantics": {"role": "artifact", "label": "repeated-table-header"}}
    return value


def self_test() -> None:
    pages: list[dict[str, Any]] = [{"operations": []} for _ in range(4)]
    pages[0]["operations"].append(operation("text", "LEAD"))
    body_pages = ([0, 0], [1, 1, 1], [2, 2, 2])
    next_row = 0
    for page_index, rows in enumerate(body_pages):
        pages[page_index]["operations"].extend(
            [operation("text", token) for token in HEADER]
            if page_index == 0
            else [
                operation("text", HEADER[0], repeated=True),
                operation("text", HEADER[1], repeated=True),
                operation("image", repeated=True),
                operation("link", repeated=True),
            ]
        )
        for _ in rows:
            pages[page_index]["operations"].extend(
                [
                    operation("text", f"R{next_row}", y=40.0 + next_row),
                    operation("text", f"${(next_row + 1) * 10}", y=40.0 + next_row),
                ]
            )
            next_row += 1
    pages[3]["operations"].extend(
        [
            operation("text", HEADER[0], repeated=True),
            operation("text", HEADER[1], repeated=True),
            operation("image", repeated=True),
            operation("link", repeated=True),
            operation("text", "TOTAL", y=60.0),
            operation("text", "$360", y=60.0),
        ]
    )
    repeats = [
        {
            "page_index": page,
            "table_node": 10,
            "header_tag_id": 20,
            "row_group_index": 0,
            "source_block_start": 20.0,
            "target_block_start": page * 140.0,
            "block_size": 24.0,
        }
        for page in (1, 2, 3)
    ]
    continuations = [
        {
            "page_index": page - 1,
            "token": {
                "kind": "table",
                "table_node": 10,
                "resume_page_index": page,
            },
        }
        for page in (1, 2, 3)
    ]
    layout = {
        "paint_events": [{"sequence": index} for index in range(4)],
        "page_sequence": {
            "pages": [{"index": page} for page in range(4)],
            "continuations": continuations,
            "table_breaks": [
                {
                    "selected": True,
                    "next_row_index": 9,
                    "resume_page_index": 3,
                }
            ],
            "table_group_repeats": repeats,
            "warnings": [
                {
                    "kind": "oversized-break-inside-avoid",
                    "retry_count": 0,
                    "block_size": 324.0,
                    "available_block_size": 120.0,
                }
            ],
        },
    }
    physical = [operation["text"] for page in pages for operation in page["operations"] if operation["type"] == "text"]
    verify_supported(
        {"pages": pages},
        layout,
        {"page_count": 4},
        " ".join(physical),
    )
    verify_unsupported(
        {
            "page_sequence": {
                "warnings": [
                    {
                        "kind": "unsupported-table-group-pagination",
                        "table_node": index,
                        "reason": reason,
                    }
                    for index, reason in enumerate(sorted(UNSUPPORTED_REASONS), 1)
                ]
            }
        }
    )

    with tempfile.TemporaryDirectory(prefix="pliego-table-header-footer-self-test-") as temp:
        failure_artifacts = Path(temp) / "failure-artifacts"
        failure_artifacts.mkdir()
        (failure_artifacts / "resources").mkdir()
        for name in ("console.jsonl", "resources.jsonl", "session-state.jsonl"):
            (failure_artifacts / name).write_bytes(b"")
        failure_error = {
            "code": UNSUPPORTED_CAPTURE_CODE,
            "message": UNSUPPORTED_CAPTURE_MESSAGE,
        }
        failure_render_id = "sha256:" + "a" * 64
        (failure_artifacts / "failure.json").write_text(
            json.dumps(
                {
                    "status": "failed",
                    "render_id": failure_render_id,
                    "error": failure_error,
                }
            ),
            encoding="utf-8",
        )
        failure = subprocess.CompletedProcess(
            args=[],
            returncode=1,
            stdout=json.dumps(
                {
                    "artifacts": str(failure_artifacts),
                    "document_pdf": str(Path(temp) / "failed.pdf"),
                    "error": failure_error,
                    "render_id": failure_render_id,
                    "status": "failed",
                }
            ),
            stderr="",
        )
        require_unsupported_failure(failure)

        diagnostic_artifacts = Path(temp) / "diagnostic-artifacts"
        diagnostic_artifacts.mkdir()
        layout_path = diagnostic_artifacts / "layout-debug.json"
        layout_path.write_text(json.dumps(layout), encoding="utf-8")
        diagnostic = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=json.dumps(
                {
                    "artifacts": str(diagnostic_artifacts),
                    "document_pdf": str(Path(temp) / "document.pdf"),
                    "scene": {
                        "capture_code": UNSUPPORTED_CAPTURE_CODE,
                        "capture_status": "partial",
                    },
                    "status": "rendered",
                }
            ),
            stderr="",
        )
        require(read_diagnostic_layout(diagnostic) == layout, "unsupported diagnostic layout differs")


def invoke(
    binary: Path,
    fixture: Path,
    temp: str,
    *,
    allow_partial_scene: bool = False,
) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment.update({"TMPDIR": temp, "TMP": temp, "TEMP": temp})
    command = [str(binary)]
    if allow_partial_scene:
        command.append("--allow-partial-scene")
    command.extend(
        [
            "--allow-host-fonts",
            "--page-size",
            "240x140",
            "--page-margins",
            "10,10,10,10",
            fixture.name,
        ]
    )
    return subprocess.run(
        command,
        cwd=fixture.parent,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )


def require_unsupported_failure(result: subprocess.CompletedProcess[str]) -> None:
    require(result.returncode != 0, "unsupported fixture did not fail closed")
    summary = final_summary(result)
    require(summary.get("status") == "failed", f"unsupported fixture status differs: {summary!r}")
    error = summary.get("error")
    require(
        error == {"code": UNSUPPORTED_CAPTURE_CODE, "message": UNSUPPORTED_CAPTURE_MESSAGE},
        f"unsupported fixture error differs: {error!r}",
    )
    artifacts_value = summary.get("artifacts")
    require(isinstance(artifacts_value, str), "unsupported fixture has no artifact path")
    artifacts = Path(artifacts_value)
    require(artifacts.is_dir(), "unsupported fixture did not publish validated failure evidence")
    files = {path.name for path in artifacts.iterdir() if path.is_file()}
    directories = {path.name for path in artifacts.iterdir() if path.is_dir()}
    allowed_files = {
        "console.jsonl",
        "environment.json",
        "failure.json",
        "readiness.json",
        "render.png",
        "resources.jsonl",
        "session-state.jsonl",
    }
    required_files = {"console.jsonl", "failure.json", "resources.jsonl", "session-state.jsonl"}
    require(files <= allowed_files, f"unsupported failure files drifted: {sorted(files)}")
    require(required_files <= files, f"unsupported failure files are incomplete: {sorted(files)}")
    require(directories == {"resources"}, f"unsupported failure directories drifted: {sorted(directories)}")
    render_id = summary.get("render_id")
    require(isinstance(render_id, str) and render_id, "unsupported fixture has no render ID")
    failure = read_json(str(artifacts / "failure.json"), "failure evidence")
    require(
        failure == {"status": "failed", "render_id": render_id, "error": error},
        f"unsupported fixture failure evidence differs: {failure!r}",
    )
    document_value = summary.get("document_pdf")
    require(isinstance(document_value, str), "unsupported fixture has no document PDF path")
    require(not Path(document_value).exists(), "unsupported fixture published a PDF")
    require(not list(artifacts.parent.glob(".pliego-runtime-*")), "unsupported fixture retained private evidence")


def read_diagnostic_layout(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    require(result.returncode == 0, f"partial diagnostic render failed: {result.stderr[-4000:]}")
    summary = final_summary(result)
    require(summary.get("status") == "rendered", f"partial diagnostic status differs: {summary!r}")
    scene = summary.get("scene")
    require(
        isinstance(scene, dict)
        and scene.get("capture_status") == "partial"
        and scene.get("capture_code") == UNSUPPORTED_CAPTURE_CODE,
        f"partial diagnostic scene differs: {scene!r}",
    )
    artifacts_value = summary.get("artifacts")
    require(isinstance(artifacts_value, str), "partial diagnostic has no artifact directory")
    artifacts = Path(artifacts_value)
    require(artifacts.is_dir(), "partial diagnostic artifact directory does not exist")
    layout = artifacts / "layout-debug.json"
    require(layout.is_file(), "partial diagnostic did not retain layout diagnostics")
    return read_json(str(layout), "partial diagnostic layout debug artifact")


def run(binary: Path, fixture: Path) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="pliego-table-header-footer-") as temp:
        result = invoke(binary, fixture, temp)
        require(result.returncode == 0, f"Pliego exited with {result.returncode}: {result.stderr[-4000:]}")
        summary = final_summary(result)
        require(summary.get("status") == "rendered", f"Pliego did not render: {summary!r}")
        return {
            "summary": summary,
            "layout": read_json(summary.get("layout_debug"), "layout debug artifact"),
            "scene": read_json(summary.get("scene_artifact"), "scene artifact"),
            "structure": read_json(summary.get("pdf_structure"), "PDF structure artifact"),
            "pdf_text": extract_pdf_text(Path(summary["document_pdf"])),
        }


def run_unsupported(binary: Path, fixture: Path) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="pliego-table-header-footer-unsupported-") as temp:
        require_unsupported_failure(invoke(binary, fixture, temp))
    with tempfile.TemporaryDirectory(prefix="pliego-table-header-footer-diagnostic-") as temp:
        return read_diagnostic_layout(invoke(binary, fixture, temp, allow_partial_scene=True))


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("table header/footer self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    fixture = Path(__file__).resolve().parent / "fixtures/table-header-footer"
    supported = run(binary, fixture / "index.html")
    verify_supported(
        supported["scene"],
        supported["layout"],
        supported["structure"],
        supported["pdf_text"],
    )
    verify_unsupported(run_unsupported(binary, fixture / "unsupported.html"))
    print("table header/footer check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
