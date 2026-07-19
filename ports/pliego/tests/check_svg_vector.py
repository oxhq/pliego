#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

INLINE_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
    validate=True,
)


def fail(message: str, code: int = 1) -> None:
    print(f"retained SVG vector check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_json(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"missing artifact: {path}")
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"artifact is not an object: {path}")
    return value


def artifact(summary: dict[str, Any], key: str) -> Path:
    value = summary.get(key)
    require(isinstance(value, str), f"summary has no {key}")
    path = Path(value)
    require(path.is_file(), f"missing {key}: {path}")
    return path


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    value = json.loads(lines[-1])
    require(isinstance(value, dict), "terminal output is not an object")
    return value


def close(left: float, right: float, tolerance: float = 0.02) -> bool:
    return abs(left - right) <= tolerance


def require_rect(actual: dict[str, Any], expected: dict[str, float], label: str) -> None:
    require(
        all(close(float(actual.get(key, float("nan"))), value) for key, value in expected.items()),
        f"{label} geometry differs: expected {expected!r}, got {actual!r}",
    )


def mapped_rect(fragment: dict[str, Any], item: dict[str, Any]) -> dict[str, float]:
    vector = fragment["vector_image"]
    rect = fragment["rect"]
    scale_x = rect["width"] / vector["viewport_width"]
    scale_y = rect["height"] / vector["viewport_height"]
    return {
        "x": rect["x"] + item["x"] * scale_x,
        "y": rect["y"] + item["y"] * scale_y,
        "width": item["width"] * scale_x,
        "height": item["height"] * scale_y,
    }


def run(binary: Path, fixture: Path, root: Path) -> tuple[bytes, bytes, bytes, bytes]:
    artifacts = root / "artifacts"
    output = root / "document.pdf"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(root), "TMP": str(root), "TEMP": str(root)})
    result = subprocess.run(
        [
            str(binary), "render", fixture.name, "--output", str(output),
            "--artifacts", str(artifacts), "--page-size", "200x120",
            "--page-margins", "0,0,0,0",
        ],
        cwd=fixture.parent,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    if result.returncode != 0:
        fail(result.stderr[-4000:] or result.stdout[-4000:])
    summary = final_json(result)
    require(summary.get("status") == "rendered", repr(summary))
    require(summary.get("readiness", {}).get("fixture") == "live-svg-vector", repr(summary))

    layout = read_json(artifact(summary, "layout_debug"))
    fragments = layout.get("fragments")
    require(isinstance(fragments, list), "layout snapshot has no fragments")
    vectors = [value for value in fragments if isinstance(value, dict) and value.get("vector_image")]
    require(len(vectors) == 1, f"expected one retained SVG fragment: {vectors!r}")
    fragment = vectors[0]
    items = fragment["vector_image"]["items"]
    kinds = [item.get("kind") for item in items]
    require(kinds.count("path") == 1, repr(items))
    require(kinds.count("image") == 1, repr(items))
    require(kinds.count("text") >= 1, repr(items))
    require(
        any(item.get("kind") == "unsupported" and item.get("reason") == "compositing" for item in items),
        f"filtered SVG group was not diagnosed: {items!r}",
    )

    scene_bytes = artifact(summary, "scene_artifact").read_bytes()
    scene = json.loads(scene_bytes)
    operations = scene["pages"][0]["operations"]
    paths = [item for item in operations if item.get("type") == "path"]
    images = [item for item in operations if item.get("type") == "image"]
    texts = [item for item in operations if item.get("type") == "text"]
    require(len(paths) == len(images) == 1 and bool(texts), repr(operations))
    path_item = next(item for item in items if item.get("kind") == "path")
    points = [
        (segment["x"], segment["y"])
        for segment in path_item["segments"]
        if "x" in segment and "y" in segment
    ]
    source_bounds = {
        "x": min(x for x, _ in points), "y": min(y for _, y in points),
        "width": max(x for x, _ in points) - min(x for x, _ in points),
        "height": max(y for _, y in points) - min(y for _, y in points),
    }
    require_rect(paths[0]["bounds"], mapped_rect(fragment, source_bounds), "path")
    require(paths[0].get("stroke", {}).get("width") == 2.0, repr(paths[0]))
    image_item = next(item for item in items if item.get("kind") == "image")
    require_rect(images[0]["bounds"], mapped_rect(fragment, image_item), "image")
    expected_image = f"sha256:{hashlib.sha256(INLINE_PNG).hexdigest()}"
    require(images[0].get("resource") == expected_image, repr(images[0]))
    require((artifacts / "resources" / expected_image.removeprefix("sha256:")).read_bytes() == INLINE_PNG,
            "embedded SVG PNG sidecar differs")

    font_path = fixture.parent.parent / "text-scene" / "Ahem.ttf"
    expected_font = f"sha256:{hashlib.sha256(font_path.read_bytes()).hexdigest()}"
    fonts_bytes = artifact(summary, "fonts_artifact").read_bytes()
    fonts = json.loads(fonts_bytes)
    require(any(resource.get("resource") == expected_font for resource in fonts["font_resources"]),
            f"exact bundled Ahem bytes are absent: {fonts!r}")
    text_fonts = {item["font"] for item in texts}
    require(text_fonts <= {item["instance"] for item in fonts["selections"]}, repr(fonts))

    report = read_json(artifact(summary, "scene_report"))
    capture = report.get("capture", {})
    require(capture.get("status") == "partial", repr(capture))
    require(any(event.get("kind") == "svg-compositing" for event in capture.get("unsupported_events", [])),
            f"typed SVG diagnostic is absent: {capture!r}")
    preview = artifact(summary, "scene_preview").read_bytes()
    require(preview.startswith(b"\x89PNG\r\n\x1a\n"), "preview is not PNG")
    preview_size = (int.from_bytes(preview[16:20], "big"), int.from_bytes(preview[20:24], "big"))
    page_size = scene["pages"][0]["size"]
    require_rect(
        {"x": 0.0, "y": 0.0, "width": preview_size[0], "height": preview_size[1]},
        {"x": 0.0, "y": 0.0, "width": page_size["width"], "height": page_size["height"]},
        "preview page",
    )

    pdf = artifact(summary, "document_pdf").read_bytes()
    require(pdf.startswith(b"%PDF-") and b"/ToUnicode" in pdf, "PDF lacks header or text mapping")
    structure = read_json(artifact(summary, "pdf_structure"))
    page = structure["pages"][0]
    require("SVG" in page.get("expected_extracted_unicode", ""), repr(page))
    require(page.get("operation_counts") == {"text": len(texts), "vector": 1, "image": 1, "link": 0}, repr(page))
    require(all(close(value, expected) for value, expected in zip(page["media_box_pt"], [0, 0, 150, 90])),
            f"PDF media box differs: {page['media_box_pt']!r}")
    return scene_bytes, preview, pdf, fonts_bytes


def self_test() -> None:
    fixture_root = Path(__file__).resolve().parent / "fixtures"
    require((fixture_root / "svg-vector/graphic.svg").is_file(), "SVG fixture is missing")
    require((fixture_root / "text-scene/Ahem.ttf").is_file(), "bundled Ahem fixture is missing")
    require(close(10.0, 10.01) and not close(10.0, 10.03), "geometry tolerance is broken")
    require(INLINE_PNG.startswith(b"\x89PNG\r\n\x1a\n"), "inline PNG fixture is invalid")


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("retained SVG vector checker self-test: ok")
        return 0
    if len(arguments) != 2:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory> | --self-test", 2)
    binary = Path(arguments[0]).resolve()
    output = Path(arguments[1]).resolve()
    fixture = Path(__file__).resolve().parent / "fixtures/svg-vector/live.html"
    require(binary.is_file(), f"missing Pliego binary: {binary}")
    require(not output.exists(), f"output already exists: {output}")
    output.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="pliego-svg-vector-") as temporary:
        root = Path(temporary)
        first_root, second_root = root / "first", root / "second"
        first_root.mkdir()
        second_root.mkdir()
        first = run(binary, fixture, first_root)
        second = run(binary, fixture, second_root)
        require(first == second, "identical live SVG runs changed scene, preview, PDF, or font report")
        shutil.copytree(first_root / "artifacts", output / "artifacts")
        shutil.copy2(first_root / "document.pdf", output / "document.pdf")
    digest = f"sha256:{hashlib.sha256(first[0]).hexdigest()}"
    (output / "comparison.json").write_text(
        json.dumps({"scene_hash": digest, "repeat_equal": True}, indent=2) + "\n", encoding="utf-8"
    )
    print(f"retained SVG vector check: ok ({digest}; artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
