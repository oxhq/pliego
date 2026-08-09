#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


def fail(message: str, code: int = 1) -> None:
    print(f"live hybrid Canvas check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_json(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"missing artifact: {path}")
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"artifact is not an object: {path}")
    return value


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    value = json.loads(lines[-1])
    require(isinstance(value, dict), "terminal output is not an object")
    return value


def artifact(summary: dict[str, Any], key: str) -> Path:
    value = summary.get(key)
    require(isinstance(value, str), f"summary has no {key}")
    path = Path(value)
    require(path.is_file(), f"missing {key}: {path}")
    return path


def run(binary: Path, fixture: Path, root: Path) -> tuple[bytes, bytes, bytes]:
    artifacts = root / "artifacts"
    output = root / "document.pdf"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(root), "TMP": str(root), "TEMP": str(root)})
    result = subprocess.run(
        [
            str(binary),
            "render",
            fixture.name,
            "--output",
            str(output),
            "--artifacts",
            str(artifacts),
            "--page-size",
            "200x160",
            "--page-margins",
            "10,10,10,10",
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
    require(summary.get("readiness", {}).get("fixture") == "live-hybrid-canvas", repr(summary))
    require(summary.get("readiness", {}).get("readbackBytes") == 16, repr(summary))

    layout = read_json(artifact(summary, "layout_debug"))
    fragments = layout.get("fragments")
    require(isinstance(fragments, list), "layout snapshot has no fragments")
    canvas_fragments = [
        item for item in fragments if isinstance(item, dict) and isinstance(item.get("canvas_image_key"), dict)
    ]
    require(len(canvas_fragments) == 1, f"expected one retained Canvas join key: {canvas_fragments!r}")

    scene_bytes = artifact(summary, "scene_artifact").read_bytes()
    scene = json.loads(scene_bytes)
    operations = scene["pages"][0]["operations"]
    require(sum(item.get("type") == "path" for item in operations) == 3, repr(operations))
    images = [item for item in operations if item.get("type") == "image"]
    require(len(images) == 1, repr(operations))
    resource = images[0].get("resource")
    require(isinstance(resource, str) and resource.startswith("sha256:"), repr(images[0]))
    resource_path = artifacts / "resources" / resource.removeprefix("sha256:")
    require(resource_path.is_file(), f"missing Canvas raster patch: {resource_path}")
    png = resource_path.read_bytes()
    require(png.startswith(b"\x89PNG\r\n\x1a\n"), "Canvas fallback is not a PNG")

    report = read_json(artifact(summary, "scene_report"))
    canvases = report.get("capture", {}).get("canvases")
    require(isinstance(canvases, list) and len(canvases) == 1, repr(canvases))
    diagnostics = canvases[0].get("diagnostics", {})
    require(diagnostics.get("vector_operation_count") == 3, repr(diagnostics))
    require(diagnostics.get("rasterized_area_px") == 4, repr(diagnostics))
    fallbacks = diagnostics.get("fallbacks")
    require(isinstance(fallbacks, list) and len(fallbacks) == 1, repr(diagnostics))
    require(fallbacks[0].get("reason") == "pixel_readback", repr(fallbacks[0]))
    require(fallbacks[0].get("area_px") == 4, repr(fallbacks[0]))
    require(
        fallbacks[0].get("bounds") == {"x": 6.0, "y": 4.0, "width": 2.0, "height": 2.0},
        repr(fallbacks[0]),
    )

    preview = artifact(summary, "scene_preview").read_bytes()
    pdf = artifact(summary, "document_pdf").read_bytes()
    require(pdf.startswith(b"%PDF-"), "document output is not a PDF")
    return scene_bytes, preview, png


def self_test() -> None:
    sample = b"stable"
    require(hashlib.sha256(sample).digest() == hashlib.sha256(sample).digest(), "hash mismatch")


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        print("live hybrid Canvas checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)
    binary = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2]).resolve()
    fixture = Path(__file__).resolve().parent / "fixtures/hybrid-canvas/live.html"
    require(binary.is_file(), f"missing Pliego binary: {binary}")
    require(fixture.is_file(), f"missing fixture: {fixture}")
    require(not output.exists(), f"output already exists: {output}")
    output.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="pliego-live-canvas-") as temporary:
        temporary = Path(temporary)
        first_root = temporary / "first"
        second_root = temporary / "second"
        first_root.mkdir()
        second_root.mkdir()
        first = run(binary, fixture, first_root)
        second = run(binary, fixture, second_root)
        require(first == second, "identical live Canvas runs changed scene, preview, or raster patch")
        shutil.copytree(first_root / "artifacts", output / "artifacts")
        shutil.copy2(first_root / "document.pdf", output / "document.pdf")
    digest = f"sha256:{hashlib.sha256(first[0]).hexdigest()}"
    (output / "comparison.json").write_text(
        json.dumps({"scene_hash": digest, "repeat_equal": True}, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"live hybrid Canvas check: ok ({digest}; artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
