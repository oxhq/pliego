#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


def fail(message: str, code: int = 1) -> None:
    print(f"paged reference check: {message}", file=sys.stderr)
    raise SystemExit(code)


def extract_reference(
    scene: dict[str, Any], layout: dict[str, Any]
) -> dict[str, Any]:
    pages = scene.get("pages")
    if not isinstance(pages, list):
        fail("scene.json has no pages array")
    page_sequence = layout.get("page_sequence")
    if not isinstance(page_sequence, dict) or not isinstance(
        page_sequence.get("pages"), list
    ):
        fail("layout-debug.json has no page sequence")
    layout_pages = page_sequence["pages"]
    if len(layout_pages) != len(pages):
        fail(
            "layout/scene page count differs: "
            f"layout has {len(layout_pages)}, scene has {len(pages)}"
        )
    extracted = []
    for index, page in enumerate(pages):
        if not isinstance(page, dict) or not isinstance(page.get("size"), dict):
            fail(f"scene page {index} has no size geometry")
        if index >= len(layout_pages) or not isinstance(layout_pages[index], dict):
            fail(f"layout-debug.json has no geometry for scene page {index}")
        layout_page = layout_pages[index]
        operations = page.get("operations")
        if not isinstance(operations, list):
            fail(f"scene page {index} has no operations array")
        text_operations = [
            operation
            for operation in operations
            if isinstance(operation, dict) and operation.get("type") == "text"
        ]
        if not text_operations or not isinstance(text_operations[0].get("glyphs"), list):
            fail(f"scene page {index} has no text glyphs")
        glyphs = text_operations[0]["glyphs"]
        if not glyphs or not isinstance(glyphs[0], dict):
            fail(f"scene page {index} first text operation has no glyphs")
        extracted.append(
            {
                "geometry_css_px": page["size"],
                "content_box_css_px": {
                    "x": layout_page.get("margin_left"),
                    "y": layout_page.get("margin_top"),
                    "available_inline_size": layout_page.get("available_inline_size"),
                    "available_block_size": layout_page.get("available_block_size"),
                },
                "first_glyph_css_px": {
                    "x": glyphs[0].get("x"),
                    "y": glyphs[0].get("y"),
                },
                "ordered_operations": [
                    operation.get("type")
                    for operation in operations
                    if isinstance(operation, dict)
                ],
                "ordered_text": [operation["text"] for operation in text_operations],
            }
        )
    continuations = []
    raw_continuations = page_sequence.get("continuations", [])
    if not isinstance(raw_continuations, list):
        fail("layout-debug.json page continuations are not an array")
    for index, continuation in enumerate(raw_continuations):
        if not isinstance(continuation, dict) or not isinstance(
            continuation.get("token"), dict
        ):
            fail(f"layout-debug.json continuation {index} is invalid")
        token = continuation["token"]
        kind = token.get("kind")
        normalized = {
            "page_index": continuation.get("page_index"),
            "kind": kind,
            "resume_page_index": token.get("resume_page_index"),
        }
        if kind == "block":
            normalized["next_child_index"] = token.get("next_child_index")
            normalized["forced"] = token.get("forced")
        elif kind == "inline":
            normalized.update(
                {
                    "child_index": token.get("child_index"),
                    "next_line_index": token.get("next_line_index"),
                    "inline_item_index": token.get("inline_item_index"),
                    "text_offset": token.get("text_offset"),
                    "shaping_result_index": token.get("shaping_result_index"),
                }
            )
        else:
            fail(f"layout-debug.json continuation {index} has unknown kind {kind!r}")
        continuations.append(normalized)
    return {
        "page_count": len(pages),
        "pages": extracted,
        "continuations": continuations,
    }


def compare_reference(actual: dict[str, Any], expected: dict[str, Any]) -> list[str]:
    errors = []
    if actual.get("page_count") != expected.get("page_count"):
        errors.append(
            f"page count: expected {expected.get('page_count')}, got {actual.get('page_count')}"
        )
    actual_pages = actual.get("pages", [])
    expected_pages = expected.get("pages", [])
    for index, (actual_page, expected_page) in enumerate(zip(actual_pages, expected_pages)):
        actual_geometry = actual_page["geometry_css_px"]
        expected_geometry = expected_page["geometry_css_px"]
        for dimension in ("width", "height"):
            if actual_geometry.get(dimension) != expected_geometry.get(dimension):
                errors.append(
                    f"page {index} {dimension}: expected {expected_geometry.get(dimension)}, "
                    f"got {actual_geometry.get(dimension)}"
                )
        actual_content = actual_page["content_box_css_px"]
        expected_content = expected_page["content_box_css_px"]
        for dimension, description in (
            ("x", "content x"),
            ("y", "content y"),
            ("available_inline_size", "available inline size"),
            ("available_block_size", "available block size"),
        ):
            if actual_content.get(dimension) != expected_content.get(dimension):
                errors.append(
                    f"page {index} {description}: expected {expected_content.get(dimension)}, "
                    f"got {actual_content.get(dimension)}"
                )
        actual_glyph = actual_page["first_glyph_css_px"]
        expected_glyph = expected_page["first_glyph_css_px"]
        if actual_glyph.get("x") != expected_glyph.get("x"):
            errors.append(
                f"page {index} first glyph x: expected {expected_glyph.get('x')}, "
                f"got {actual_glyph.get('x')}"
            )
        glyph_y = actual_glyph.get("y")
        minimum_y = expected_glyph.get("minimum_y")
        maximum_y = expected_glyph.get("maximum_y")
        if not isinstance(glyph_y, (int, float)) or not (
            isinstance(minimum_y, (int, float))
            and isinstance(maximum_y, (int, float))
            and minimum_y <= glyph_y < maximum_y
        ):
            errors.append(
                f"page {index} first glyph y: expected {minimum_y} <= y < {maximum_y}, "
                f"got {glyph_y}"
            )
        if actual_page["ordered_operations"] != expected_page["ordered_operations"]:
            errors.append(
                f"page {index} operation order: expected "
                f"{expected_page['ordered_operations']!r}, "
                f"got {actual_page['ordered_operations']!r}"
            )
        if actual_page["ordered_text"] != expected_page["ordered_text"]:
            errors.append(
                f"page {index} text order: expected {expected_page['ordered_text']!r}, "
                f"got {actual_page['ordered_text']!r}"
            )
    if "continuations" in expected and actual.get("continuations") != expected["continuations"]:
        errors.append(
            f"continuations: expected {expected['continuations']!r}, "
            f"got {actual.get('continuations')!r}"
        )
    return errors


def deliberate_mismatch_self_test(verbose: bool) -> None:
    expected = {
        "page_count": 1,
        "pages": [
            {
                "geometry_css_px": {"width": 320.0, "height": 480.0},
                "content_box_css_px": {
                    "x": 50.0,
                    "y": 20.0,
                    "available_inline_size": 240.0,
                    "available_block_size": 420.0,
                },
                "first_glyph_css_px": {
                    "x": 50.0,
                    "minimum_y": 20.0,
                    "maximum_y": 40.0,
                },
                "ordered_operations": ["text", "text"],
                "ordered_text": ["Alpha.", "Beta."],
            }
        ],
    }
    wrong = {
        "page_count": 2,
        "pages": [
            {
                "geometry_css_px": {"width": 321.0, "height": 481.0},
                "content_box_css_px": {
                    "x": 49.0,
                    "y": 19.0,
                    "available_inline_size": 241.0,
                    "available_block_size": 421.0,
                },
                "first_glyph_css_px": {"x": 49.0, "y": 19.0},
                "ordered_operations": ["text", "path", "text"],
                "ordered_text": ["Beta.", "Alpha."],
            }
        ],
    }
    errors = compare_reference(wrong, expected)
    required = (
        "page count:",
        "page 0 width:",
        "page 0 height:",
        "page 0 content x:",
        "page 0 content y:",
        "page 0 available inline size:",
        "page 0 available block size:",
        "page 0 first glyph x:",
        "page 0 first glyph y:",
        "page 0 operation order:",
        "page 0 text order:",
    )
    if not all(any(error.startswith(prefix) for error in errors) for prefix in required):
        fail(f"deliberate mismatch was not fully detected: {errors!r}")
    if verbose:
        print("deliberate mismatch detected:")
        for error in errors:
            print(f"- {error}")


def final_summary(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if not lines:
        fail("Pliego produced no stdout JSON")
    try:
        summary = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}")
    if not isinstance(summary, dict):
        fail("final stdout JSON is not an object")
    return summary


def verify_output_contract(summary: dict[str, Any], reference: dict[str, Any]) -> None:
    page_count = reference["page_count"]
    artifacts = Path(summary["artifacts"])
    pages_path = Path(summary["pages_artifact"])
    pages = json.loads(pages_path.read_text(encoding="utf-8"))
    if pages.get("schema") != "pliego.pages" or pages.get("page_count") != page_count:
        fail(f"invalid pages.json: {pages!r}")
    preview_pages = pages.get("pages")
    if not isinstance(preview_pages, list) or len(preview_pages) != page_count:
        fail(f"pages.json preview count differs: {preview_pages!r}")
    previews = summary.get("scene_previews")
    if not isinstance(previews, list) or len(previews) != page_count:
        fail(f"terminal preview count differs: {previews!r}")
    if summary.get("scene_preview") != previews[0]:
        fail("legacy scene_preview is not the first page preview")

    required_bundle_paths = {"document.pdf", "pages.json"}
    for index, (page, expected_page, preview_value) in enumerate(
        zip(preview_pages, reference["pages"], previews)
    ):
        if page.get("index") != index or page.get("page_size") != expected_page["geometry_css_px"]:
            fail(f"preview page {index} geometry differs: {page!r}")
        preview = Path(preview_value)
        if not preview.read_bytes().startswith(b"\x89PNG\r\n\x1a\n"):
            fail(f"preview page {index} is not PNG: {preview}")
        relative_preview = preview.relative_to(artifacts).as_posix()
        if page.get("artifact") != relative_preview:
            fail(f"preview page {index} artifact differs: {page!r}")
        required_bundle_paths.add(relative_preview)

    structure = json.loads(Path(summary["pdf_structure"]).read_text(encoding="utf-8"))
    if structure.get("page_count") != page_count:
        fail(f"PDF structure page count differs: {structure!r}")
    if [page.get("scene_page_size_css_px") for page in structure.get("pages", [])] != [
        page["geometry_css_px"] for page in reference["pages"]
    ]:
        fail(f"PDF structure page geometry differs: {structure!r}")
    pdf = Path(summary["document_pdf"])
    if not pdf.read_bytes().startswith(b"%PDF-"):
        fail(f"published document is not PDF: {pdf}")

    bundle = json.loads(Path(summary["bundle"]).read_text(encoding="utf-8"))
    entries = {
        entry.get("path"): entry
        for entry in bundle.get("entries", [])
        if isinstance(entry, dict)
    }
    if not required_bundle_paths <= entries.keys():
        fail(f"bundle omits multi-page artifacts: {required_bundle_paths - entries.keys()!r}")
    for path in required_bundle_paths:
        contents = (artifacts / path).read_bytes()
        if entries[path].get("sha256") != f"sha256:{hashlib.sha256(contents).hexdigest()}":
            fail(f"bundle hash differs for {path}")


def check_reference(
    binary: Path,
    fixture_dir: Path,
    expected_name: str,
    page_size: str,
    page_margins: str,
) -> None:
    expected = json.loads((fixture_dir / expected_name).read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="pliego-paged-reference-") as temp:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp, "TMP": temp, "TEMP": temp})
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
            cwd=fixture_dir,
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
        scene_path = summary.get("scene_artifact")
        if summary.get("status") != "rendered" or not isinstance(scene_path, str):
            fail(f"Pliego did not report a rendered scene artifact: {summary!r}")
        scene = json.loads(Path(scene_path).read_text(encoding="utf-8"))
        layout_path = summary.get("layout_debug")
        if not isinstance(layout_path, str):
            fail(f"Pliego did not report a layout debug artifact: {summary!r}")
        layout = json.loads(Path(layout_path).read_text(encoding="utf-8"))
        reference = extract_reference(scene, layout)
        verify_output_contract(summary, reference)
        errors = compare_reference(reference, expected)
        if errors:
            fail("reference mismatch:\n- " + "\n- ".join(errors))


def check_paragraph_continuation(binary: Path) -> None:
    fixture_dir = Path(__file__).resolve().parent / "fixtures/paged-paragraph"
    expected = json.loads((fixture_dir / "expected.json").read_text(encoding="utf-8"))
    source = expected["source_unicode"]
    with tempfile.TemporaryDirectory(prefix="pliego-paged-paragraph-") as temp:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp, "TMP": temp, "TEMP": temp})
        result = subprocess.run(
            [
                str(binary),
                "--allow-host-fonts",
                "--page-size",
                "180x80",
                "--page-margins",
                "10,10,10,10",
                "index.html",
            ],
            cwd=fixture_dir,
            env=environment,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "no process output"
            fail(f"paragraph fixture exited with {result.returncode}: {detail[-4000:]}")
        summary = final_summary(result)
        if summary.get("status") != "rendered":
            fail(f"paragraph fixture did not render: {summary!r}")
        scene = json.loads(Path(summary["scene_artifact"]).read_text(encoding="utf-8"))
        layout = json.loads(Path(summary["layout_debug"]).read_text(encoding="utf-8"))
        reference = extract_reference(scene, layout)
        verify_output_contract(summary, reference)
        if reference["page_count"] < 2:
            fail("paragraph fixture did not cross a page boundary")

        page_text = []
        for page_index, page in enumerate(scene["pages"]):
            text_operations = [
                operation
                for operation in page["operations"]
                if operation.get("type") == "text"
            ]
            text = "".join(operation["text"] for operation in text_operations)
            if not text:
                fail(f"paragraph scene page {page_index} has no text")
            page_text.append(text)
            for operation_index, operation in enumerate(text_operations):
                run = operation["text"]
                boundaries = {
                    len(run[:character].encode("utf-8"))
                    for character in range(len(run) + 1)
                }
                previous = (0, 0)
                for glyph_index, glyph in enumerate(operation.get("glyphs", [])):
                    text_range = glyph.get("text_range")
                    if not isinstance(text_range, dict):
                        fail(
                            f"paragraph page {page_index} operation {operation_index} "
                            f"glyph {glyph_index} has no text range"
                        )
                    current = (text_range.get("start"), text_range.get("end"))
                    if (
                        not all(isinstance(value, int) for value in current)
                        or current[0] not in boundaries
                        or current[1] not in boundaries
                        or current[0] >= current[1]
                        or current < previous
                    ):
                        fail(
                            f"paragraph page {page_index} operation {operation_index} "
                            f"glyph ranges are invalid or non-monotonic: {current!r}"
                        )
                    previous = current
        actual = "".join(page_text)
        if actual != source:
            fail(f"paragraph text differs: expected {source!r}, got {actual!r}")

        page_sequence = layout.get("page_sequence", {})
        continuations = page_sequence.get("continuations")
        if not isinstance(continuations, list) or len(continuations) != len(page_text) - 1:
            fail(f"paragraph continuation count differs: {continuations!r}")
        previous_progress = None
        source_boundaries = {
            len(source[:character].encode("utf-8"))
            for character in range(len(source) + 1)
        }
        for boundary, continuation in enumerate(continuations):
            token = continuation.get("token", {})
            progress = (
                token.get("next_line_index"),
                token.get("text_offset"),
                token.get("shaping_result_index"),
            )
            if (
                continuation.get("page_index") != boundary
                or token.get("kind") != "inline"
                or token.get("resume_page_index") != boundary + 1
                or not all(isinstance(value, int) for value in progress)
                or token.get("text_offset") not in source_boundaries
                or (
                    previous_progress is not None
                    and (
                        progress[0] <= previous_progress[0]
                        or progress[1] <= previous_progress[1]
                        or progress[2] < previous_progress[2]
                    )
                )
            ):
                fail(f"paragraph continuation is invalid or non-monotonic: {continuation!r}")
            previous_progress = progress

        structure = json.loads(Path(summary["pdf_structure"]).read_text(encoding="utf-8"))
        pdf_text = "".join(
            page.get("expected_extracted_unicode", "") for page in structure.get("pages", [])
        )
        if pdf_text != source:
            fail(f"PDF source text differs: expected {source!r}, got {pdf_text!r}")


def check_binary(binary: Path) -> None:
    fixture_dir = Path(__file__).resolve().parent / "fixtures/paged-root"
    check_reference(binary, fixture_dir, "expected.json", "320x480", "20,30,40,50")
    check_reference(binary, fixture_dir, "expected-small.json", "320x40", "5,10,5,10")
    check_paragraph_continuation(binary)


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        deliberate_mismatch_self_test(True)
        print("paged reference self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    if not binary.is_file():
        fail(f"Pliego binary does not exist: {binary}", 2)
    deliberate_mismatch_self_test(False)
    check_binary(binary)
    print("paged reference check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
