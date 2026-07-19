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
    return {"page_count": len(pages), "pages": extracted}


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


def check_binary(binary: Path) -> None:
    fixture_dir = Path(__file__).resolve().parent / "fixtures/paged-root"
    expected = json.loads((fixture_dir / "expected.json").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="pliego-paged-reference-") as temp:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp, "TMP": temp, "TEMP": temp})
        result = subprocess.run(
            [
                str(binary),
                "--page-size",
                "320x480",
                "--page-margins",
                "20,30,40,50",
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
        errors = compare_reference(extract_reference(scene, layout), expected)
        if errors:
            fail("reference mismatch:\n- " + "\n- ".join(errors))


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
