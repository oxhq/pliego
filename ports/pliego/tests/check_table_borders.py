#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import copy
import json
import math
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

MODE_PREFIXES = {"collapsed": "C", "separate": "S"}
ROUND_DIGITS = 6
EPSILON = 10**-ROUND_DIGITS


def fail(message: str, code: int = 1) -> None:
    print(f"table border check: {message}", file=sys.stderr)
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


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def number(value: Any, description: str) -> float:
    require(
        isinstance(value, (int, float)) and not isinstance(value, bool),
        f"{description} is not numeric",
    )
    result = float(value)
    require(math.isfinite(result), f"{description} is not finite")
    return result


def text_y(operation: dict[str, Any]) -> float:
    glyphs = operation.get("glyphs")
    require(isinstance(glyphs, list) and glyphs, "table text has no glyph geometry")
    glyph = glyphs[0]
    require(isinstance(glyph, dict), "table text glyph is not an object")
    return number(glyph.get("y"), "table text y")


def rectangle(operation: dict[str, Any], page_index: int) -> tuple[float, float, float, float]:
    bounds = operation.get("bounds")
    require(isinstance(bounds, dict), f"page {page_index} border has no bounds")
    values = tuple(
        round(number(bounds.get(field), f"page {page_index} border {field}"), ROUND_DIGITS)
        for field in ("x", "y", "width", "height")
    )
    require(values[2] > 0 and values[3] > 0, f"page {page_index} border is empty: {values!r}")
    require(
        not math.isclose(values[2], values[3], abs_tol=EPSILON),
        f"page {page_index} border is not an edge rectangle: {values!r}",
    )
    fill = operation.get("fill")
    require(isinstance(fill, dict), f"page {page_index} border is not filled")
    require(number(fill.get("a"), f"page {page_index} border alpha") > 0, "border is invisible")
    require(operation.get("stroke") is None, f"page {page_index} border unexpectedly uses a stroke")
    require(isinstance(operation.get("data"), str) and operation["data"], "border path data is absent")
    return values


def page_mode(operations: list[dict[str, Any]], page_index: int) -> tuple[str, str]:
    text = [
        canonical_text(str(operation.get("text", ""))) for operation in operations if operation.get("type") == "text"
    ]
    modes = {
        (mode, prefix)
        for mode, prefix in MODE_PREFIXES.items()
        if any(token.startswith(f"{prefix}-") for token in text)
    }
    require(len(modes) == 1, f"page {page_index} does not identify one border mode: {text!r}")
    return next(iter(modes))


def require_one_seam(
    rectangles: list[tuple[float, float, float, float]],
    header_y: float,
    body_y: float,
    page_index: int,
) -> None:
    seam = [item for item in rectangles if item[2] > item[3] and header_y < item[1] + item[3] / 2 < body_y]
    levels = {(item[1], item[3]) for item in seam}
    require(len(levels) == 1 and seam, f"page {page_index} has {len(levels)} header/body seams")
    intervals = sorted((item[0], item[0] + item[2]) for item in seam)
    for previous, current in zip(intervals, intervals[1:]):
        require(
            math.isclose(previous[1], current[0], abs_tol=EPSILON),
            f"page {page_index} header/body seam overlaps or has a gap: {intervals!r}",
        )


def border_geometry(scene: dict[str, Any]) -> list[tuple[Any, ...]]:
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages")
    fragments: dict[str, list[tuple[int, set[float]]]] = {mode: [] for mode in MODE_PREFIXES}
    geometry: list[tuple[Any, ...]] = []

    for page_index, page in enumerate(pages):
        require(isinstance(page, dict), f"scene page {page_index} is not an object")
        operations = page.get("operations")
        require(isinstance(operations, list), f"scene page {page_index} has no operations")
        objects = [operation for operation in operations if isinstance(operation, dict)]
        paths = [operation for operation in objects if operation.get("type") == "path"]
        if not paths:
            continue

        mode, prefix = page_mode(objects, page_index)
        headers = [
            operation
            for operation in objects
            if operation.get("type") == "text"
            and re.fullmatch(rf"{prefix}-H-[AB]", canonical_text(str(operation.get("text", ""))))
        ]
        body = [
            operation
            for operation in objects
            if operation.get("type") == "text"
            and re.fullmatch(rf"{prefix}-R[0-6]-[AB]", canonical_text(str(operation.get("text", ""))))
        ]
        require(len(headers) == 2, f"page {page_index} does not contain one repeated header")
        require(bool(body), f"page {page_index} contains no body rows")

        rectangles = [rectangle(operation, page_index) for operation in paths]
        require(
            len(rectangles) == len(set(rectangles)),
            f"page {page_index} contains duplicate border rectangles",
        )
        vertical_x = {item[0] for item in rectangles if item[3] > item[2]}
        require(len(vertical_x) >= 3, f"page {page_index} has no complete column border grid")
        require_one_seam(
            rectangles,
            max(text_y(operation) for operation in headers),
            min(text_y(operation) for operation in body),
            page_index,
        )
        fragments[mode].append((page_index, vertical_x))
        geometry.extend((page_index, mode, *item) for item in rectangles)

    for mode, mode_fragments in fragments.items():
        require(len(mode_fragments) >= 2, f"{mode} table did not span two fragments")
        expected = mode_fragments[0][1]
        require(
            all(vertical_x == expected for _, vertical_x in mode_fragments),
            f"{mode} vertical border x coordinates drifted: {mode_fragments!r}",
        )
    return sorted(geometry)


def verify(first_scene: dict[str, Any], second_scene: dict[str, Any]) -> None:
    first = border_geometry(first_scene)
    second = border_geometry(second_scene)
    require(first == second, "identical runs changed paged table border geometry")


def self_test() -> None:
    def text(value: str, y: float) -> dict[str, Any]:
        return {"type": "text", "text": value, "glyphs": [{"x": 20.0, "y": y}]}

    def path(x: float, y: float, width: float, height: float) -> dict[str, Any]:
        return {
            "type": "path",
            "bounds": {"x": x, "y": y, "width": width, "height": height},
            "data": f"M{x} {y}h{width}v{height}h-{width}z",
            "fill": {"r": 0.1, "g": 0.2, "b": 0.3, "a": 1.0},
        }

    def fragment(prefix: str, top: float) -> dict[str, Any]:
        return {
            "operations": [
                text(f"{prefix}-H-A", top + 12.0),
                text(f"{prefix}-H-B", top + 12.0),
                text(f"{prefix}-R0-A", top + 38.0),
                text(f"{prefix}-R0-B", top + 38.0),
                path(10.0, top, 2.0, 80.0),
                path(110.0, top, 2.0, 80.0),
                path(210.0, top, 2.0, 80.0),
                path(10.0, top, 202.0, 2.0),
                path(10.0, top + 24.0, 202.0, 2.0),
                path(10.0, top + 78.0, 202.0, 2.0),
            ]
        }

    first = {
        "pages": [
            fragment("C", 20.0),
            fragment("C", 0.0),
            fragment("S", 0.0),
            fragment("S", 0.0),
        ]
    }
    verify(first, copy.deepcopy(first))


def run(binary: Path, temp: Path) -> dict[str, Any]:
    fixture = Path(__file__).resolve().parent / "fixtures/table-borders"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(temp), "TMP": str(temp), "TEMP": str(temp)})
    result = subprocess.run(
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
        timeout=60,
        check=False,
    )
    require(result.returncode == 0, f"Pliego exited with {result.returncode}: {result.stderr[-4000:]}")
    summary = final_summary(result)
    require(summary.get("status") == "rendered", f"Pliego did not render: {summary!r}")
    return read_json(summary.get("scene_artifact"), "scene artifact")


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("table border self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    with tempfile.TemporaryDirectory(prefix="pliego-table-borders-first-") as first:
        first_scene = run(binary, Path(first))
    with tempfile.TemporaryDirectory(prefix="pliego-table-borders-second-") as second:
        second_scene = run(binary, Path(second))
    verify(first_scene, second_scene)
    print("table border check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
