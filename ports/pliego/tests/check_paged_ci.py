#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import json
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

PAGE_SIZE = "180x80"
PAGE_MARGINS = "10,10,10,10"
PROCESS_TIMEOUT_SECONDS = 60
REFERENCE_SCHEMA = "pliego.paged-ci-reference.v1"


def fail(message: str, code: int = 1) -> None:
    print(f"paged CI check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_object(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"artifact does not exist: {path}")
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read JSON artifact {path}: {error}")
    require(isinstance(value, dict), f"JSON artifact is not an object: {path}")
    return value


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    return value.decode(errors="replace") if isinstance(value, bytes) else value


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the paged CI gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-nopgbrk", str(pdf), "-"],
        capture_output=True,
        timeout=20,
        check=False,
    )
    require(result.returncode == 0, f"pdftotext failed: {text(result.stderr)}")
    return result.stdout.decode("utf-8-sig")


def run_once(
    binary: Path,
    fixture: Path,
    reference_path: Path,
    destination: Path,
) -> dict[str, Any]:
    destination.mkdir(parents=True)
    inputs = destination / "inputs"
    inputs.mkdir()
    for source in (fixture, reference_path, fixture.with_name("Ahem.ttf")):
        shutil.copy2(source, inputs / source.name)
    command = [
        str(binary),
        "render",
        fixture.name,
        "--output",
        str(destination / "document.pdf"),
        "--artifacts",
        str(destination / "artifacts"),
        "--page-size",
        PAGE_SIZE,
        "--page-margins",
        PAGE_MARGINS,
    ]
    try:
        result = subprocess.run(
            command,
            cwd=fixture.parent,
            capture_output=True,
            text=True,
            timeout=PROCESS_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        (destination / "process.stdout.log").write_text(text(error.stdout), encoding="utf-8")
        (destination / "process.stderr.log").write_text(text(error.stderr), encoding="utf-8")
        fail(f"render exceeded {PROCESS_TIMEOUT_SECONDS}s; artifacts retained at {destination}")
    (destination / "process.stdout.log").write_text(result.stdout, encoding="utf-8")
    (destination / "process.stderr.log").write_text(result.stderr, encoding="utf-8")
    require(
        result.returncode == 0,
        f"render exited with {result.returncode}; artifacts retained at {destination}",
    )
    summary = final_json(result)
    require(summary.get("status") == "rendered", f"render did not complete: {summary!r}")
    scene_path = Path(summary["scene_artifact"])
    pdf_path = Path(summary["document_pdf"])
    return {
        "summary": summary,
        "scene": read_object(scene_path),
        "scene_bytes": scene_path.read_bytes(),
        "layout": read_object(Path(summary["layout_debug"])),
        "structure": read_object(Path(summary["pdf_structure"])),
        "pdf_path": pdf_path,
        "pdf_bytes": pdf_path.read_bytes(),
    }


def normalize_continuations(layout: dict[str, Any]) -> list[dict[str, Any]]:
    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "layout debug has no page sequence")
    continuations = page_sequence.get("continuations")
    require(isinstance(continuations, list), "layout continuations are absent")
    fragments = layout.get("fragments")
    require(isinstance(fragments, list), "layout fragments are absent")
    tag_positions = {
        fragment["tag_id"]: index
        for index, fragment in reversed(list(enumerate(fragments)))
        if isinstance(fragment, dict)
        and fragment.get("kind") == "box"
        and isinstance(fragment.get("tag_id"), int)
    }
    block_nodes = []
    for continuation in continuations:
        token = continuation.get("token", {}) if isinstance(continuation, dict) else {}
        if token.get("kind") != "block":
            continue
        node = token.get("next_node")
        require(isinstance(node, int) and node in tag_positions, f"unknown block token node: {node!r}")
        block_nodes.append(node)
    require(len(block_nodes) == len(set(block_nodes)), f"block token nodes repeat: {block_nodes!r}")
    ordered_nodes = sorted(block_nodes, key=tag_positions.__getitem__)
    require(block_nodes == ordered_nodes, f"block token nodes are out of DOM box order: {block_nodes!r}")

    normalized = []
    inline_progress = []
    for continuation in continuations:
        require(isinstance(continuation, dict), f"invalid continuation: {continuation!r}")
        token = continuation.get("token")
        require(isinstance(token, dict), f"continuation has no token: {continuation!r}")
        kind = token.get("kind")
        row = {
            "page_index": continuation.get("page_index"),
            "kind": kind,
            "resume_page_index": token.get("resume_page_index"),
        }
        if kind == "block":
            row.update(
                {
                    "next_child_index": token.get("next_child_index"),
                    "node_order": ordered_nodes.index(token.get("next_node")),
                    "forced": token.get("forced"),
                    "break_inside_avoid": token.get("break_inside_avoid"),
                    "retry_count": token.get("retry_count"),
                }
            )
        elif kind == "inline":
            progress = (
                token.get("inline_item_index"),
                token.get("shaping_result_index"),
                token.get("text_offset"),
            )
            require(all(isinstance(value, int) for value in progress), f"invalid inline progress: {token!r}")
            inline_progress.append(progress)
            row.update(
                {
                    "child_index": token.get("child_index"),
                    "next_line_index": token.get("next_line_index"),
                    "text_offset": token.get("text_offset"),
                }
            )
        else:
            fail(f"unknown continuation kind: {kind!r}")
        normalized.append(row)
    require(
        all(next_ > previous for previous, next_ in zip(inline_progress, inline_progress[1:])),
        f"inline token progress is not monotonic: {inline_progress!r}",
    )
    return normalized


def verify_run(run: dict[str, Any], expected: dict[str, Any]) -> dict[str, Any]:
    summary = run["summary"]
    readiness = summary.get("readiness")
    require(
        isinstance(readiness, dict) and readiness.get("fixture") == "paged-ci",
        f"fixture readiness differs: {readiness!r}",
    )
    source = expected["source_unicode"]
    require(readiness.get("source") == source, "ordered live DOM source differs from reference")

    scene = run["scene"]
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages array")
    layout = run["layout"]
    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "pagination was not retained during layout")
    layout_pages = page_sequence.get("pages")
    reference_pages = expected["pages"]
    require(
        isinstance(layout_pages, list)
        and len(pages) == len(layout_pages) == len(reference_pages)
        and len(pages) > 1,
        "layout/scene/reference page counts differ",
    )

    page_geometry = expected["page"]
    page_text = []
    for index, (page, layout_page, reference_page) in enumerate(
        zip(pages, layout_pages, reference_pages)
    ):
        require(isinstance(page, dict) and isinstance(layout_page, dict), f"invalid page {index}")
        require(page.get("size") == {"width": 180.0, "height": 80.0}, f"scene page {index} size differs")
        require(layout_page.get("index") == index, f"layout page {index} index differs")
        for field, value in page_geometry.items():
            require(layout_page.get(field) == value, f"layout page {index} {field} differs")
        operations = page.get("operations")
        require(isinstance(operations, list), f"scene page {index} has no operations")
        require(
            all(isinstance(operation, dict) and operation.get("type") == "text" for operation in operations),
            f"scene page {index} contains unexpected operations: {operations!r}",
        )
        actual_text = "".join(str(operation.get("text", "")) for operation in operations)
        require(
            reference_page == {"index": index, "text": actual_text},
            f"scene page {index} fixed reference differs: {actual_text!r}",
        )
        glyphs = operations[0].get("glyphs") if operations else None
        require(isinstance(glyphs, list) and glyphs, f"scene page {index} has no glyphs")
        first = glyphs[0]
        require(
            first.get("x") == 10.0 and isinstance(first.get("y"), (int, float)) and 10.0 <= first["y"] < 30.0,
            f"scene page {index} first glyph is outside the content origin: {first!r}",
        )
        page_text.append(actual_text)
    require("".join(page_text) == source, "scene text was lost, duplicated, or reordered")

    pages_artifact = read_object(Path(summary["pages_artifact"]))
    previews = pages_artifact.get("pages")
    require(isinstance(previews, list) and len(previews) == len(pages), "preview page count differs")
    for preview in previews:
        require(isinstance(preview, dict), f"invalid preview entry: {preview!r}")
        artifact = preview.get("artifact")
        require(isinstance(artifact, str), f"preview has no artifact path: {preview!r}")
        preview_path = Path(summary["artifacts"]) / artifact
        require(preview_path.read_bytes().startswith(b"\x89PNG\r\n\x1a\n"), f"invalid preview: {preview_path}")

    structure = run["structure"]
    pdf_pages = structure.get("pages")
    require(isinstance(pdf_pages, list) and len(pdf_pages) == len(pages), "PDF page count differs")
    require(structure.get("page_count") == len(pages), "PDF page count field differs")
    expected_pdf_pages = [str(page.get("expected_extracted_unicode", "")) for page in pdf_pages]
    require(expected_pdf_pages == page_text, "PDF page-by-page source reference differs")
    pdf_bytes = run["pdf_bytes"]
    require(pdf_bytes.startswith(b"%PDF-") and b"/ToUnicode" in pdf_bytes, "PDF is invalid or unselectable")
    extracted = extract_pdf_text(run["pdf_path"])
    require(canonical_text(extracted) == canonical_text(source), f"pdftotext differs: {extracted!r}")

    continuations = normalize_continuations(layout)
    require(continuations == expected["continuations"], f"continuation reference differs: {continuations!r}")
    require(page_sequence.get("warnings") == expected["warnings"], f"pagination warnings differ: {page_sequence!r}")
    require(max(row.get("retry_count", 0) for row in continuations) <= 1, "pagination retry is unbounded")
    scene_summary = summary.get("scene")
    require(isinstance(scene_summary, dict) and isinstance(scene_summary.get("hash"), str), "scene hash is absent")
    return {
        "pages": page_text,
        "continuations": continuations,
        "scene_hash": scene_summary["hash"],
    }


def self_test(fixture: Path, reference_path: Path) -> None:
    expected = read_object(reference_path)
    require(expected.get("schema") == REFERENCE_SCHEMA, "unexpected paged CI reference schema")
    require(
        "".join(page["text"] for page in expected["pages"]) == expected["source_unicode"],
        "fixed page text does not reconstruct the DOM source",
    )
    kinds = [row["kind"] for row in expected["continuations"]]
    require(kinds == ["block", "block", "inline", "inline", "block"], f"bad fixture coverage: {kinds!r}")
    require(max(row.get("retry_count", 0) for row in expected["continuations"]) == 1, "bad retry oracle")
    node_ids = [101, 102, 103]
    node_index = 0
    continuations = []
    for row in expected["continuations"]:
        token = {
            key: value
            for key, value in row.items()
            if key not in ("page_index", "node_order")
        }
        if row["kind"] == "block":
            token["next_node"] = node_ids[node_index]
            node_index += 1
        else:
            token.update({"inline_item_index": 0, "shaping_result_index": 0})
        continuations.append({"page_index": row["page_index"], "token": token})
    synthetic_layout = {
        "fragments": [
            {"kind": "box", "tag_id": node_id}
            for node_id in node_ids
        ],
        "page_sequence": {"continuations": continuations},
    }
    require(
        normalize_continuations(synthetic_layout) == expected["continuations"],
        "continuation normalizer differs from the fixed reference",
    )
    markup = fixture.read_text(encoding="utf-8")
    require("break-before: page" in markup and "break-inside: avoid" in markup, "authored breaks are absent")
    require("page-break" not in markup, "legacy page-break alias entered the fixture")


def main() -> int:
    fixture = Path(__file__).resolve().parent / "fixtures/text-scene/paged-ci.html"
    reference_path = fixture.with_name("paged-ci.expected.json")
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test(fixture, reference_path)
        print("paged CI checker self-test: ok")
        return 0
    if len(arguments) != 2:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    output = Path(arguments[1]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(not output.exists(), f"output directory already exists: {output}")
    output.mkdir(parents=True)
    expected = read_object(reference_path)
    require(expected.get("schema") == REFERENCE_SCHEMA, "unexpected paged CI reference schema")

    first = run_once(binary, fixture, reference_path, output / "run-1")
    first_reference = verify_run(first, expected)
    second = run_once(binary, fixture, reference_path, output / "run-2")
    second_reference = verify_run(second, expected)
    require(first_reference == second_reference, "repeat layout reference differs")
    require(first["scene_bytes"] == second["scene_bytes"], "repeat canonical scene bytes differ")
    require(first["pdf_bytes"] == second["pdf_bytes"], "repeat PDF bytes differ")
    print(f"paged CI check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
