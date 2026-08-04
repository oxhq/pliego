#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import copy
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
from typing import Any

MODE_PREFIXES = {"collapsed": "C", "separate": "S"}
EXPECTED_BODY_TEXT = {
    mode: {f"{prefix}-R{row}-{column}" for row in range(7) for column in "AB"}
    for mode, prefix in MODE_PREFIXES.items()
}
EXPECTED_VERTICAL_X = (10.0, 120.0, 210.0)
EXPECTED_INLINE_END = 212.0
BORDER_WIDTH = 2.0
ROUND_DIGITS = 6
EPSILON = 10**-ROUND_DIGITS
FALLBACK_LEAD_TOKEN = "A-LEAD"
FALLBACK_FITTING_TOKENS = Counter(
    {
        "A-H-A": 1,
        "A-H-B": 1,
        "A-R-A": 1,
        "A-R-B": 1,
    }
)
FALLBACK_UNSUPPORTED_TOKEN = "U"
FALLBACK_FITTING_PAGE_INDEX = 1
FALLBACK_UNSUPPORTED_PAGE_INDEX = 2
FALLBACK_FITTING_CHILD_INDEX = 1
FALLBACK_UNSUPPORTED_CHILD_INDEX = 2
FALLBACK_BORDER_COLOR = (0.0, 87.0 / 255.0, 184.0 / 255.0, 1.0)
ROW_BACKGROUND_COLOR = (232.0 / 255.0, 240.0 / 255.0, 251.0 / 255.0, 1.0)
STRIPED_SEPARATE_ROWS = {1, 3, 5}
FALLBACK_BORDER_RECTANGLES = {
    (10.0, 10.0, 202.0, 2.0),
    (10.0, 10.0, 2.0, 26.0),
    (110.0, 12.0, 2.0, 24.0),
    (210.0, 12.0, 2.0, 24.0),
    (12.0, 34.0, 100.0, 2.0),
    (112.0, 34.0, 100.0, 2.0),
    (10.0, 36.0, 2.0, 32.0),
    (110.0, 36.0, 2.0, 32.0),
    (210.0, 36.0, 2.0, 32.0),
    (12.0, 66.0, 100.0, 2.0),
    (112.0, 66.0, 100.0, 2.0),
}


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


def text_x(operation: dict[str, Any]) -> float:
    glyphs = operation.get("glyphs")
    require(isinstance(glyphs, list) and glyphs, "table text has no glyph geometry")
    glyph = glyphs[0]
    require(isinstance(glyph, dict), "table text glyph is not an object")
    return number(glyph.get("x"), "table text x")


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


def rectangle_path_data(bounds: tuple[float, float, float, float]) -> str:
    x, y, width, height = bounds
    return f"M{x:g} {y:g}h{width:g}v{height:g}h-{width:g}z"


def semantic_label(operation: dict[str, Any]) -> Any:
    return operation.get("meta", {}).get("semantics", {}).get("label")


def is_table_border(operation: dict[str, Any]) -> bool:
    return operation.get("type") == "path" and semantic_label(operation) in {
        "table-border",
        "repeated-table-header",
    }


def is_row_background(operation: dict[str, Any], page_index: int) -> bool:
    if operation.get("type") != "path" or semantic_label(operation) != "background":
        return False
    fill = operation.get("fill")
    require(isinstance(fill, dict), f"page {page_index} background is not filled")
    actual = tuple(
        number(fill.get(channel), f"page {page_index} background {channel}")
        for channel in ("r", "g", "b", "a")
    )
    return all(
        math.isclose(actual_channel, expected_channel, abs_tol=EPSILON)
        for actual_channel, expected_channel in zip(actual, ROW_BACKGROUND_COLOR)
    )


def operation_bounds(operation: dict[str, Any], page_index: int) -> tuple[float, float, float, float]:
    bounds = operation.get("bounds")
    require(isinstance(bounds, dict), f"page {page_index} paint operation has no bounds")
    return tuple(
        number(bounds.get(field), f"page {page_index} paint operation {field}")
        for field in ("x", "y", "width", "height")
    )


def overlap(first: tuple[float, float, float, float], second: tuple[float, float, float, float]) -> bool:
    return (
        first[0] < second[0] + second[2]
        and second[0] < first[0] + first[2]
        and first[1] < second[1] + second[3]
        and second[1] < first[1] + first[3]
    )


def contains_point(bounds: tuple[float, float, float, float], x: float, y: float) -> bool:
    return (
        bounds[0] - EPSILON <= x <= bounds[0] + bounds[2] + EPSILON
        and bounds[1] - EPSILON <= y <= bounds[1] + bounds[3] + EPSILON
    )


def require_row_backgrounds_before_borders(objects: list[dict[str, Any]], page_index: int) -> int:
    borders = [
        (index, operation_bounds(operation, page_index))
        for index, operation in enumerate(objects)
        if is_table_border(operation)
    ]
    checked = 0
    for background_index, background in enumerate(objects):
        if not is_row_background(background, page_index):
            continue
        background_bounds = operation_bounds(background, page_index)
        for border_index, border_bounds in borders:
            if overlap(background_bounds, border_bounds):
                checked += 1
                require(
                    background_index < border_index,
                    f"page {page_index} background paints over a table border",
                )
    return checked


def fallback_rectangle(
    operation: dict[str, Any],
    page_index: int,
) -> tuple[float, float, float, float]:
    bounds = rectangle(operation, page_index)
    require(
        operation.get("data") == rectangle_path_data(bounds),
        f"page {page_index} fallback border path data differs",
    )
    require(
        operation.get("fill_rule") == "non_zero",
        f"page {page_index} fallback border fill rule differs",
    )
    require(
        operation.get("meta")
        == {
            "semantics": {
                "role": "artifact",
                "label": "table-border",
            }
        },
        f"page {page_index} fallback border semantics differ",
    )
    fill = operation["fill"]
    require(
        set(fill) == {"r", "g", "b", "a"}
        and all(
            math.isclose(
                number(fill.get(channel), f"page {page_index} fallback border {channel}"),
                expected,
                abs_tol=EPSILON,
            )
            for channel, expected in zip(("r", "g", "b", "a"), FALLBACK_BORDER_COLOR)
        ),
        f"page {page_index} fallback border color differs: {fill!r}",
    )
    return bounds


def page_mode(operations: list[dict[str, Any]], page_index: int) -> tuple[str, str]:
    text = [
        canonical_text(str(operation.get("text", "")))
        for operation in operations
        if operation.get("type") == "text"
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


def require_exact_grid(
    rectangles: list[tuple[float, float, float, float]],
    mode: str,
    header_y: float,
    body_rows: int,
    page_index: int,
) -> set[float]:
    vertical = [item for item in rectangles if item[3] > item[2]]
    horizontal = [item for item in rectangles if item[2] > item[3]]
    require(
        all(math.isclose(item[2], BORDER_WIDTH, abs_tol=EPSILON) for item in vertical),
        f"page {page_index} vertical border width drifted",
    )
    vertical_x = {item[0] for item in vertical}
    require(
        sorted(vertical_x) == list(EXPECTED_VERTICAL_X),
        f"page {page_index} vertical border x coordinates differ: {sorted(vertical_x)!r}",
    )
    require(
        all(math.isclose(item[3], BORDER_WIDTH, abs_tol=EPSILON) for item in horizontal),
        f"page {page_index} horizontal border width drifted",
    )
    require(
        math.isclose(min(item[0] for item in rectangles), EXPECTED_VERTICAL_X[0], abs_tol=EPSILON)
        and math.isclose(
            max(item[0] + item[2] for item in rectangles),
            EXPECTED_INLINE_END,
            abs_tol=EPSILON,
        ),
        f"page {page_index} table inline extent drifted",
    )

    top = min(item[1] for item in rectangles)
    bottom = top + 24.0 + 32.0 * body_rows + BORDER_WIDTH
    require(
        math.isclose(max(item[1] + item[3] for item in rectangles), bottom, abs_tol=EPSILON),
        f"page {page_index} table block extent drifted",
    )
    for x in EXPECTED_VERTICAL_X:
        intervals = sorted(
            (item[1], item[1] + item[3]) for item in vertical if math.isclose(item[0], x)
        )
        expected_start = top + (BORDER_WIDTH if mode == "separate" and x != EXPECTED_VERTICAL_X[0] else 0)
        first_height = 24.0 if mode == "separate" and x != EXPECTED_VERTICAL_X[0] else 26.0
        expected_heights = [first_height] + [32.0] * body_rows
        require(
            len(intervals) == body_rows + 1
            and math.isclose(intervals[0][0], expected_start, abs_tol=EPSILON)
            and math.isclose(intervals[-1][1], bottom, abs_tol=EPSILON)
            and all(
                math.isclose(end - start, expected_height, abs_tol=EPSILON)
                for (start, end), expected_height in zip(intervals, expected_heights)
            ),
            f"page {page_index} vertical edge {x:g} has incomplete extent: {intervals!r}",
        )
        for previous, current in zip(intervals, intervals[1:]):
            require(
                math.isclose(previous[1], current[0], abs_tol=EPSILON),
                f"page {page_index} vertical edge {x:g} overlaps or has a gap: {intervals!r}",
            )

    horizontal_levels: dict[tuple[float, float], set[tuple[float, float]]] = {}
    for x, y, width, height in horizontal:
        horizontal_levels.setdefault((y, height), set()).add((x, width))
    expected_levels = {top, top + 24.0}
    expected_levels.update(top + 24.0 + 32.0 * index for index in range(1, body_rows + 1))
    require(
        {y for y, _ in horizontal_levels} == expected_levels,
        f"page {page_index} horizontal border levels differ",
    )
    for (y, _), segments in horizontal_levels.items():
        if mode == "collapsed":
            expected = {(10.0, 112.0), (122.0, 90.0)}
        elif y + BORDER_WIDTH / 2 < header_y:
            expected = {(10.0, 202.0)}
        else:
            expected = {(12.0, 110.0), (122.0, 90.0)}
        require(
            segments == expected,
            f"page {page_index} horizontal edge at {y:g} differs: {sorted(segments)!r}",
        )
    return vertical_x


def border_geometry(scene: dict[str, Any]) -> list[tuple[Any, ...]]:
    pages = scene.get("pages")
    require(isinstance(pages, list), "scene has no pages")
    fragments: dict[str, list[tuple[int, set[float]]]] = {mode: [] for mode in MODE_PREFIXES}
    body_text: dict[str, list[str]] = {mode: [] for mode in MODE_PREFIXES}
    geometry: list[tuple[Any, ...]] = []
    paint_order_checks = 0
    striped_rows_seen: set[int] = set()

    for page_index, page in enumerate(pages):
        require(isinstance(page, dict), f"scene page {page_index} is not an object")
        operations = page.get("operations")
        require(isinstance(operations, list), f"scene page {page_index} has no operations")
        objects = [operation for operation in operations if isinstance(operation, dict)]
        paths = [
            operation
            for operation in objects
            if is_table_border(operation)
        ]
        text = [
            canonical_text(str(operation.get("text", "")))
            for operation in objects
            if operation.get("type") == "text"
        ]
        if not any(token.startswith(("C-", "S-")) for token in text):
            require(not paths, f"page {page_index} contains paths without table fixture text")
            continue

        mode, prefix = page_mode(objects, page_index)
        require(bool(paths), f"page {page_index} {mode} table fragment contains no borders")
        paint_order_checks += require_row_backgrounds_before_borders(objects, page_index)
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
        header_text = Counter(token for token in text if token.startswith(f"{prefix}-H-"))
        expected_headers = Counter({f"{prefix}-H-A": 1, f"{prefix}-H-B": 1})
        require(
            header_text == expected_headers,
            f"page {page_index} repeated header identity differs: {header_text!r}",
        )
        require(bool(body) and len(body) % 2 == 0, f"page {page_index} contains incomplete body rows")
        body_text[mode].extend(canonical_text(str(operation.get("text", ""))) for operation in body)
        row_backgrounds = [
            operation_bounds(operation, page_index)
            for operation in objects
            if is_row_background(operation, page_index)
        ]
        if mode == "collapsed":
            require(not row_backgrounds, f"page {page_index} collapsed table unexpectedly has row stripes")
        else:
            for operation in body:
                token = canonical_text(str(operation.get("text", "")))
                match = re.fullmatch(r"S-R([0-6])-[AB]", token)
                require(match is not None, f"page {page_index} has malformed separate row token: {token!r}")
                row = int(match.group(1))
                covered = any(
                    contains_point(bounds, text_x(operation), text_y(operation))
                    for bounds in row_backgrounds
                )
                require(
                    covered == (row in STRIPED_SEPARATE_ROWS),
                    f"page {page_index} row background coverage differs for {token}",
                )
                if covered:
                    striped_rows_seen.add(row)

        rectangles = [rectangle(operation, page_index) for operation in paths]
        require(
            len(rectangles) == len(set(rectangles)),
            f"page {page_index} contains duplicate border rectangles",
        )
        vertical_x = require_exact_grid(
            rectangles,
            mode,
            max(text_y(operation) for operation in headers),
            len(body) // 2,
            page_index,
        )
        require(
            min(vertical_x) < min(text_x(operation) for operation in headers + body),
            f"page {page_index} has no authored outer inline-start border",
        )
        if mode == "collapsed":
            require(
                all(
                    math.isclose(min(item[2], item[3]), BORDER_WIDTH, abs_tol=EPSILON)
                    for item in rectangles
                ),
                f"page {page_index} collapsed border width drifted",
            )
        else:
            require(
                any(
                    item[2] > item[3]
                    and item[1] + item[3] / 2 < min(text_y(operation) for operation in headers)
                    for item in rectangles
                ),
                f"page {page_index} has no grid-authored table top border",
            )
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
        require(
            len(body_text[mode]) == len(EXPECTED_BODY_TEXT[mode])
            and set(body_text[mode]) == EXPECTED_BODY_TEXT[mode],
            f"{mode} table body text differs: {sorted(body_text[mode])!r}",
        )
    require(
        striped_rows_seen == STRIPED_SEPARATE_ROWS,
        f"row-level table stripes differ: {sorted(striped_rows_seen)!r}",
    )
    require(paint_order_checks > 0, "row backgrounds did not exercise background/border order")
    return sorted(geometry)


def verify(
    first_scene: dict[str, Any],
    second_scene: dict[str, Any],
    first_page_hashes: tuple[str, ...],
    second_page_hashes: tuple[str, ...],
) -> None:
    first = border_geometry(first_scene)
    second = border_geometry(second_scene)
    require(first == second, "identical runs changed paged table border geometry")
    require(bool(first_page_hashes), "render produced no page previews")
    require(first_page_hashes == second_page_hashes, "identical runs changed page raster output")


def verify_fallback_capture(scene: dict[str, Any], layout: dict[str, Any]) -> None:
    pages = scene.get("pages")
    require(isinstance(pages, list), "fallback scene has no pages")
    fitting_text = Counter()
    lead_pages = []
    unsupported_pages = []
    for page_index, page in enumerate(pages):
        require(isinstance(page, dict), f"fallback page {page_index} is not an object")
        operations = page.get("operations")
        require(isinstance(operations, list), f"fallback page {page_index} has no operations")
        objects = [operation for operation in operations if isinstance(operation, dict)]
        text = Counter(
            canonical_text(str(operation.get("text", ""))) for operation in objects if operation.get("type") == "text"
        )
        paths = [operation for operation in objects if operation.get("type") == "path"]
        page_fitting_text = Counter(
            {
                token: count
                for token, count in text.items()
                if token.startswith(("A-H-", "A-R-"))
            }
        )
        if page_fitting_text:
            fitting_text.update(page_fitting_text)
            require(
                page_index == FALLBACK_FITTING_PAGE_INDEX,
                f"fitting break-inside:avoid table remained on page {page_index}",
            )
            require(
                page_fitting_text == FALLBACK_FITTING_TOKENS,
                f"fitting table text differs: {page_fitting_text!r}",
            )
            rectangles = [fallback_rectangle(operation, page_index) for operation in paths]
            require(
                len(rectangles) == len(FALLBACK_BORDER_RECTANGLES) and len(rectangles) == len(set(rectangles)),
                f"fitting table borders are duplicated or incomplete: {rectangles!r}",
            )
            require(
                set(rectangles) == FALLBACK_BORDER_RECTANGLES,
                f"fitting table border geometry differs: {sorted(rectangles)!r}",
            )
        else:
            require(not paths, f"fallback page {page_index} contains unexpected paths")
        if text[FALLBACK_LEAD_TOKEN]:
            require(
                text[FALLBACK_LEAD_TOKEN] == 1,
                f"fallback page {page_index} duplicates lead text",
            )
            lead_pages.append(page_index)
        if text[FALLBACK_UNSUPPORTED_TOKEN]:
            require(
                text[FALLBACK_UNSUPPORTED_TOKEN] == 1,
                f"fallback page {page_index} duplicates unsupported text",
            )
            unsupported_pages.append(page_index)
    require(fitting_text == FALLBACK_FITTING_TOKENS, f"fitting table text differs: {fitting_text!r}")
    require(lead_pages == [0], f"lead content page differs: {lead_pages!r}")
    require(
        unsupported_pages == [FALLBACK_UNSUPPORTED_PAGE_INDEX],
        f"unsupported text page differs: {unsupported_pages!r}",
    )

    page_sequence = layout.get("page_sequence")
    require(isinstance(page_sequence, dict), "fallback layout has no page sequence")
    continuations = page_sequence.get("continuations")
    require(isinstance(continuations, list), "fallback layout has no typed continuations")
    require(
        len(continuations) == 2,
        f"fallback layout continuation count differs: {continuations!r}",
    )

    def block_continuation(
        source_page_index: int,
        child_index: int,
        resume_page_index: int,
        *,
        forced: bool,
        break_inside_avoid: bool,
        retry_count: int,
    ) -> int:
        matches = [
            continuation
            for continuation in continuations
            if isinstance(continuation, dict) and continuation.get("page_index") == source_page_index
        ]
        require(
            len(matches) == 1,
            f"fallback page {source_page_index} continuation differs: {matches!r}",
        )
        token = matches[0].get("token")
        require(isinstance(token, dict), f"fallback page {source_page_index} token is absent")
        node = token.get("next_node")
        require(
            isinstance(node, int) and not isinstance(node, bool),
            f"fallback child {child_index} has no node identity",
        )
        require(
            token
            == {
                "kind": "block",
                "next_child_index": child_index,
                "next_node": node,
                "forced": forced,
                "break_inside_avoid": break_inside_avoid,
                "retry_count": retry_count,
                "resume_page_index": resume_page_index,
            },
            f"fallback child {child_index} continuation differs: {token!r}",
        )
        return node

    fitting_node = block_continuation(
        0,
        FALLBACK_FITTING_CHILD_INDEX,
        FALLBACK_FITTING_PAGE_INDEX,
        forced=False,
        break_inside_avoid=True,
        retry_count=1,
    )
    unsupported_node = block_continuation(
        FALLBACK_FITTING_PAGE_INDEX,
        FALLBACK_UNSUPPORTED_CHILD_INDEX,
        FALLBACK_UNSUPPORTED_PAGE_INDEX,
        forced=True,
        break_inside_avoid=False,
        retry_count=0,
    )
    require(fitting_node != unsupported_node, "fallback tables share one node identity")

    warnings = page_sequence.get("warnings")
    require(isinstance(warnings, list), "fallback layout has no typed warnings")
    unsupported_warnings = [
        warning
        for warning in warnings
        if isinstance(warning, dict) and warning.get("kind") == "unsupported-table-group-pagination"
    ]
    require(
        unsupported_warnings
        == [
            {
                "kind": "unsupported-table-group-pagination",
                "child_index": FALLBACK_UNSUPPORTED_CHILD_INDEX,
                "table_node": unsupported_node,
                "reason": "unsupported-layout",
            }
        ],
        f"unsupported table warning is absent or targets another table: {unsupported_warnings!r}",
    )


def self_test() -> None:
    fixture = Path(__file__).resolve().parent / "fixtures/table-borders/index.html"
    html = fixture.read_text(encoding="utf-8")
    require(
        "#separate tbody tr:nth-child(even) { background: #e8f0fb; }" in html,
        "fixture does not exercise row-level table backgrounds",
    )
    require(
        "#separate tbody tr:nth-child(even) td" not in html,
        "fixture stripes cells directly instead of exercising row background propagation",
    )

    def text(value: str, y: float) -> dict[str, Any]:
        return {"type": "text", "text": value, "glyphs": [{"x": 20.0, "y": y}]}

    def path(x: float, y: float, width: float, height: float) -> dict[str, Any]:
        return {
            "type": "path",
            "bounds": {"x": x, "y": y, "width": width, "height": height},
            "data": f"M{x} {y}h{width}v{height}h-{width}z",
            "fill": {"r": 0.1, "g": 0.2, "b": 0.3, "a": 1.0},
            "meta": {"semantics": {"role": "artifact", "label": "table-border"}},
        }

    def background(x: float, y: float, width: float, height: float) -> dict[str, Any]:
        return {
            "type": "path",
            "bounds": {"x": x, "y": y, "width": width, "height": height},
            "data": f"M{x} {y}h{width}v{height}h-{width}z",
            "fill": dict(zip(("r", "g", "b", "a"), ROW_BACKGROUND_COLOR)),
            "meta": {"semantics": {"role": "artifact", "label": "background"}},
        }

    def fallback_path(bounds: tuple[float, float, float, float]) -> dict[str, Any]:
        x, y, width, height = bounds
        return {
            "type": "path",
            "bounds": {"x": x, "y": y, "width": width, "height": height},
            "data": rectangle_path_data(bounds),
            "fill": dict(zip(("r", "g", "b", "a"), FALLBACK_BORDER_COLOR)),
            "fill_rule": "non_zero",
            "meta": {
                "semantics": {
                    "role": "artifact",
                    "label": "table-border",
                }
            },
        }

    def fragment(prefix: str, top: float, rows: list[int]) -> dict[str, Any]:
        mode = "collapsed" if prefix == "C" else "separate"
        operations = [
            text(f"{prefix}-H-A", top + 12.0),
            text(f"{prefix}-H-B", top + 12.0),
        ]
        for position, row in enumerate(rows):
            y = top + 38.0 + 32.0 * position
            operations.extend([text(f"{prefix}-R{row}-A", y), text(f"{prefix}-R{row}-B", y)])
        if mode == "collapsed":
            for x in EXPECTED_VERTICAL_X:
                operations.append(path(x, top, 2.0, 26.0))
                for index in range(len(rows)):
                    operations.append(path(x, top + 26.0 + 32.0 * index, 2.0, 32.0))
            for y in [top, top + 24.0] + [top + 24.0 + 32.0 * index for index in range(1, len(rows) + 1)]:
                operations.extend([path(10.0, y, 112.0, 2.0), path(122.0, y, 90.0, 2.0)])
        else:
            operations[0:0] = [
                background(10.0, top + 26.0 + 32.0 * position, 202.0, 32.0)
                for position, row in enumerate(rows)
                if row in STRIPED_SEPARATE_ROWS
            ]
            operations.extend([path(10.0, top, 2.0, 26.0), path(10.0, top, 202.0, 2.0)])
            for x in EXPECTED_VERTICAL_X[1:]:
                operations.append(path(x, top + 2.0, 2.0, 24.0))
            for index in range(len(rows)):
                y = top + 26.0 + 32.0 * index
                for x in EXPECTED_VERTICAL_X:
                    operations.append(path(x, y, 2.0, 32.0))
            for y in [top + 24.0] + [top + 24.0 + 32.0 * index for index in range(1, len(rows) + 1)]:
                operations.extend([path(12.0, y, 110.0, 2.0), path(122.0, y, 90.0, 2.0)])
        return {
            "operations": operations
        }

    first = {
        "pages": [
            fragment("C", 20.0, [0, 1]),
            fragment("C", 0.0, [2, 3, 4]),
            fragment("C", 0.0, [5, 6]),
            fragment("S", 0.0, [0, 1, 2]),
            fragment("S", 0.0, [3, 4, 5]),
            fragment("S", 0.0, [6]),
        ]
    }
    verify(first, copy.deepcopy(first), ("same",), ("same",))

    zero_path = copy.deepcopy(first)
    zero_path["pages"][1]["operations"] = [
        operation
        for operation in zero_path["pages"][1]["operations"]
        if operation.get("type") != "path"
    ]
    shifted = copy.deepcopy(first)
    for page in shifted["pages"][:3]:
        for operation in page["operations"]:
            bounds = operation.get("bounds", {})
            if operation.get("type") == "path" and bounds.get("x") == 210.0:
                bounds["x"] -= 60.0
                bounds["height"] -= 60.0
    duplicate_header = copy.deepcopy(first)
    for operation in duplicate_header["pages"][0]["operations"]:
        if operation.get("type") == "text" and operation.get("text") == "C-H-B":
            operation["text"] = "C-H-A"
    covered_border = copy.deepcopy(first)
    covered_operations = covered_border["pages"][3]["operations"]
    covered_operations.append(covered_operations.pop(0))

    for broken, description in [
        (zero_path, "zero-path table continuation"),
        (shifted, "shifted and shortened right edge"),
        (duplicate_header, "duplicate H-A and missing H-B"),
        (covered_border, "background painted after its table borders"),
    ]:
        with redirect_stderr(StringIO()):
            try:
                border_geometry(broken)
            except SystemExit:
                continue
        fail(f"self-test accepted {description}")

    fallback_scene = {
        "pages": [
            {"operations": [text(FALLBACK_LEAD_TOKEN, 10.0)]},
            {
                "operations": [
                    text("A-H-A", 12.0),
                    text("A-H-B", 12.0),
                    text("A-R-A", 38.0),
                    text("A-R-B", 38.0),
                    *[fallback_path(bounds) for bounds in sorted(FALLBACK_BORDER_RECTANGLES)],
                ]
            },
            {"operations": [text(FALLBACK_UNSUPPORTED_TOKEN, 10.0)]},
            {"operations": []},
        ]
    }
    fitting_node = 101
    unsupported_node = 102
    fallback_layout = {
        "page_sequence": {
            "continuations": [
                {
                    "page_index": 0,
                    "token": {
                        "kind": "block",
                        "next_child_index": FALLBACK_FITTING_CHILD_INDEX,
                        "next_node": fitting_node,
                        "forced": False,
                        "break_inside_avoid": True,
                        "retry_count": 1,
                        "resume_page_index": FALLBACK_FITTING_PAGE_INDEX,
                    },
                },
                {
                    "page_index": FALLBACK_FITTING_PAGE_INDEX,
                    "token": {
                        "kind": "block",
                        "next_child_index": FALLBACK_UNSUPPORTED_CHILD_INDEX,
                        "next_node": unsupported_node,
                        "forced": True,
                        "break_inside_avoid": False,
                        "retry_count": 0,
                        "resume_page_index": FALLBACK_UNSUPPORTED_PAGE_INDEX,
                    },
                },
            ],
            "warnings": [
                {
                    "kind": "unsupported-table-group-pagination",
                    "child_index": FALLBACK_UNSUPPORTED_CHILD_INDEX,
                    "table_node": unsupported_node,
                    "reason": "unsupported-layout",
                }
            ],
        }
    }
    verify_fallback_capture(fallback_scene, fallback_layout)

    malformed_fallback = copy.deepcopy(fallback_scene)
    malformed_fallback["pages"][FALLBACK_FITTING_PAGE_INDEX]["operations"] = [
        text("A-H-A", 12.0),
        *[{"type": "path"} for _ in range(11)],
    ]
    wrong_position = copy.deepcopy(fallback_scene)
    wrong_position_path = wrong_position["pages"][FALLBACK_FITTING_PAGE_INDEX]["operations"][4]
    wrong_position_path["bounds"]["x"] += 1.0
    wrong_position_path["data"] = rectangle_path_data(
        tuple(wrong_position_path["bounds"][field] for field in ("x", "y", "width", "height"))
    )
    wrong_thickness = copy.deepcopy(fallback_scene)
    wrong_thickness_path = wrong_thickness["pages"][FALLBACK_FITTING_PAGE_INDEX]["operations"][4]
    wrong_thickness_path["bounds"]["height"] = 1.0
    wrong_thickness_path["data"] = rectangle_path_data(
        tuple(wrong_thickness_path["bounds"][field] for field in ("x", "y", "width", "height"))
    )
    wrong_color = copy.deepcopy(fallback_scene)
    wrong_color["pages"][FALLBACK_FITTING_PAGE_INDEX]["operations"][4]["fill"]["g"] = 0.0
    wrong_semantics = copy.deepcopy(fallback_scene)
    wrong_semantics["pages"][FALLBACK_FITTING_PAGE_INDEX]["operations"][4]["meta"]["semantics"]["label"] = (
        "not-a-table-border"
    )
    duplicate_border = copy.deepcopy(fallback_scene)
    duplicate_border["pages"][FALLBACK_FITTING_PAGE_INDEX]["operations"][4] = copy.deepcopy(
        duplicate_border["pages"][FALLBACK_FITTING_PAGE_INDEX]["operations"][5]
    )
    wrong_warning = copy.deepcopy(fallback_layout)
    wrong_warning["page_sequence"]["warnings"][0]["table_node"] = fitting_node
    wrong_continuation = copy.deepcopy(fallback_layout)
    wrong_continuation["page_sequence"]["continuations"][0]["token"]["break_inside_avoid"] = False

    for broken_scene, broken_layout, description in [
        (malformed_fallback, fallback_layout, "missing fitting cells plus arbitrary paths"),
        (wrong_position, fallback_layout, "shifted fitting border"),
        (wrong_thickness, fallback_layout, "wrong fitting border thickness"),
        (wrong_color, fallback_layout, "wrong fitting border color"),
        (wrong_semantics, fallback_layout, "wrong fitting border semantics"),
        (duplicate_border, fallback_layout, "duplicate fitting border"),
        (fallback_scene, wrong_warning, "warning for another table"),
        (fallback_scene, wrong_continuation, "missing generic avoid placement"),
    ]:
        with redirect_stderr(StringIO()):
            try:
                verify_fallback_capture(broken_scene, broken_layout)
            except SystemExit:
                continue
        fail(f"self-test accepted {description}")

    leaked_fallback = copy.deepcopy(fallback_scene)
    leaked_fallback["pages"][FALLBACK_UNSUPPORTED_PAGE_INDEX]["operations"].append(
        fallback_path((10.0, 0.0, 2.0, 120.0))
    )
    with redirect_stderr(StringIO()):
        try:
            verify_fallback_capture(leaked_fallback, fallback_layout)
        except SystemExit:
            pass
        else:
            fail("self-test accepted unsupported fallback borders")


def page_raster_hashes(summary: dict[str, Any]) -> tuple[str, ...]:
    manifest = read_json(summary.get("pages_artifact"), "pages artifact")
    pages = manifest.get("pages")
    require(isinstance(pages, list) and pages, "pages artifact contains no previews")
    artifact_root = summary.get("artifacts")
    require(isinstance(artifact_root, str), "terminal result has no artifacts path")
    artifacts = Path(artifact_root)
    hashes = []
    for expected_index, page in enumerate(pages):
        require(isinstance(page, dict), f"preview {expected_index} is not an object")
        require(page.get("index") == expected_index, f"preview index differs: {page!r}")
        artifact = page.get("artifact")
        require(isinstance(artifact, str), f"preview {expected_index} has no artifact")
        contents = (artifacts / artifact).read_bytes()
        require(contents.startswith(b"\x89PNG\r\n\x1a\n"), f"preview {expected_index} is not PNG")
        hashes.append(hashlib.sha256(contents).hexdigest())
    return tuple(hashes)


def render(binary: Path, temp: Path, fixture_name: str) -> dict[str, Any]:
    fixture = Path(__file__).resolve().parent / f"fixtures/{fixture_name}"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(temp), "TMP": str(temp), "TEMP": str(temp)})
    result = subprocess.run(
        [
            str(binary),
            *(["--allow-partial-scene"] if fixture_name == "table-border-fallbacks" else []),
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
    return summary


def run(binary: Path, temp: Path) -> tuple[dict[str, Any], tuple[str, ...]]:
    summary = render(binary, temp, "table-borders")
    return (
        read_json(summary.get("scene_artifact"), "scene artifact"),
        page_raster_hashes(summary),
    )


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
        first_scene, first_hashes = run(binary, Path(first))
    with tempfile.TemporaryDirectory(prefix="pliego-table-borders-second-") as second:
        second_scene, second_hashes = run(binary, Path(second))
    verify(first_scene, second_scene, first_hashes, second_hashes)
    with tempfile.TemporaryDirectory(prefix="pliego-table-border-fallbacks-") as fallback:
        summary = render(binary, Path(fallback), "table-border-fallbacks")
        verify_fallback_capture(
            read_json(summary.get("scene_artifact"), "fallback scene artifact"),
            read_json(summary.get("layout_debug"), "fallback layout debug"),
        )
    print("table border check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
