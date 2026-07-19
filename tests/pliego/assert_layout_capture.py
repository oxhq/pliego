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


def fail(message: str, code: int = 1) -> None:
    print(f"layout capture smoke test: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def number(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def positive(value: object) -> bool:
    return number(value) and value > 0


def positive_rects(fragments: list[object], kind: str) -> list[object]:
    rects = [
        fragment.get("rect")
        for fragment in fragments
        if isinstance(fragment, dict) and fragment.get("kind") == kind
    ]
    return [
        rect
        for rect in rects
        if isinstance(rect, dict) and positive(rect.get("width")) and positive(rect.get("height"))
    ]


def output_path(summary: dict[str, object], key: str, root: Path) -> Path:
    value = summary.get(key)
    require(isinstance(value, str) and bool(value), f"final output has no {key!r} path")
    path = Path(value)
    return path if path.is_absolute() else root / path


def main() -> int:
    if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != "text"):
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> [text]", 2)

    root = Path(__file__).resolve().parents[2]
    binary = Path(sys.argv[1]).expanduser().resolve()
    text_mode = len(sys.argv) == 3
    fixture_name = "text-capture" if text_mode else "session"
    fixture = Path(f"tests/pliego/fixtures/{fixture_name}/index.html")
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require((root / fixture).is_file(), f"{fixture_name} fixture does not exist: {root / fixture}")

    with tempfile.TemporaryDirectory(prefix="pliego-layout-capture-") as temp_dir:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp_dir, "TMP": temp_dir, "TEMP": temp_dir})
        try:
            result = subprocess.run(
                [str(binary), str(fixture)],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
        except OSError as error:
            fail(f"could not execute {binary}: {error}")
        except subprocess.TimeoutExpired:
            fail(f"Pliego did not finish the {fixture_name} fixture within 60 seconds")

        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "no process output"
            fail(f"Pliego exited with {result.returncode}: {detail[-2000:]}")

        lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
        require(bool(lines), "Pliego produced no stdout JSON")
        try:
            summary = json.loads(lines[-1])
        except json.JSONDecodeError as error:
            fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
        require(isinstance(summary, dict), "final stdout JSON is not an object")

        layout_path = output_path(summary, "layout_debug", root)
        require(layout_path.is_file(), f"layout debug artifact does not exist: {layout_path}")
        try:
            snapshot = json.loads(layout_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            fail(f"could not load layout debug artifact {layout_path}: {error}")
        require(isinstance(snapshot, dict), "layout debug artifact is not a JSON object")

        boxes = snapshot.get("boxes")
        require(isinstance(boxes, list), "layout debug artifact has no boxes array")

        fragments = snapshot.get("fragments")
        require(isinstance(fragments, list) and bool(fragments), "fragment array is empty or missing")
        if text_mode:
            text_runs = [
                fragment.get("text_run")
                for fragment in fragments
                if isinstance(fragment, dict) and isinstance(fragment.get("text_run"), dict)
            ]
            require(bool(text_runs), "text fixture produced no captured text runs")
            texts = [run.get("text") for run in text_runs]
            require(all(isinstance(text, str) for text in texts), "a text run has no text string")
            captured_text = "".join(texts)
            require(captured_text == "CAFÉ OFFICE", f"captured text was {captured_text!r}")
            for run_index, run in enumerate(text_runs):
                require(
                    bool(run.get("font_identifier")),
                    f"text run {run_index} has an empty font_identifier",
                )
                require(run.get("font_size") == 20, f"text run {run_index} font_size is not 20")
                glyphs = run.get("glyphs")
                require(
                    isinstance(glyphs, list) and bool(glyphs),
                    f"text run {run_index} has no glyphs",
                )
                for glyph_index, glyph in enumerate(glyphs):
                    require(
                        isinstance(glyph, dict),
                        f"text run {run_index} glyph {glyph_index} is not an object",
                    )
                    for key in ("id", "x", "y", "advance"):
                        require(
                            number(glyph.get(key)),
                            f"text run {run_index} glyph {glyph_index} has nonnumeric {key}",
                        )
        else:
            box_kinds = [box.get("kind") for box in boxes if isinstance(box, dict)]
            require(
                "independent" in box_kinds,
                f"expected an independent box; got kinds {box_kinds}",
            )
            for kind in ("text", "image"):
                require(
                    bool(positive_rects(fragments, kind)),
                    f"expected a {kind} fragment with positive width and height",
                )

        for key in ("paint_content_width", "paint_content_height"):
            require(positive(snapshot.get(key)), f"{key} must be positive; got {snapshot.get(key)!r}")
        scroll_nodes = snapshot.get("paint_scroll_node_count")
        require(
            isinstance(scroll_nodes, int) and not isinstance(scroll_nodes, bool) and scroll_nodes >= 2,
            f"paint_scroll_node_count must be at least 2; got {scroll_nodes!r}",
        )
        for key in ("paintable", "contentful"):
            require(snapshot.get(key) is True, f"{key} must be true; got {snapshot.get(key)!r}")
        epoch = snapshot.get("paint_epoch")
        require(
            isinstance(epoch, int) and not isinstance(epoch, bool) and epoch >= 0,
            f"paint_epoch must be a nonnegative integer; got {epoch!r}",
        )
        require(
            isinstance(snapshot.get("first_reflow"), bool),
            f"first_reflow must be boolean; got {snapshot.get('first_reflow')!r}",
        )

        rendered_path = output_path(summary, "rendered_image", root)
        require(rendered_path.is_file(), f"rendered image does not exist: {rendered_path}")
        require(rendered_path.stat().st_size > 0, f"rendered image is empty: {rendered_path}")

    print("layout capture smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
