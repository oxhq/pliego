#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import binascii
import json
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

DECLARED_DIMENSION_LIMIT = 16_384


def fail(message: str, code: int = 1) -> None:
    print(f"image limit check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def png(width: int, height: int) -> bytes:
    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", binascii.crc32(kind + body))

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    pixels = b"".join(b"\0" + b"\0\0\0\0" * width for _ in range(height))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b"")


def document(image: bytes, fixture: str) -> str:
    encoded = base64.b64encode(image).decode("ascii")
    return f"""<!doctype html>
<meta charset="utf-8">
<title>{fixture}</title>
<style>html, body {{ margin: 0 }} img {{ display: block; width: 8px; height: 8px }}</style>
<img alt="" src="data:image/png;base64,{encoded}">
<script>
  addEventListener('load', () => window.pliego?.ready({{
    fixture: '{fixture}',
    width: document.images[0].naturalWidth,
    height: document.images[0].naturalHeight,
  }}));
</script>
"""


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, object]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def run(binary: Path, root: Path, fixture: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            str(binary),
            "render",
            f"{fixture}.html",
            "--output",
            str(root / "document.pdf"),
            "--artifacts",
            str(root / "artifacts"),
        ],
        cwd=root,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )


def retain(root: Path, result: subprocess.CompletedProcess[str], destination: Path) -> None:
    destination.mkdir(parents=True)
    (destination / "process.stdout.log").write_text(result.stdout, encoding="utf-8")
    (destination / "process.stderr.log").write_text(result.stderr, encoding="utf-8")
    if (root / "artifacts").is_dir():
        shutil.copytree(root / "artifacts", destination / "artifacts")
    if (root / "document.pdf").is_file():
        shutil.copy2(root / "document.pdf", destination / "document.pdf")


def verify_failure(result: subprocess.CompletedProcess[str], summary: dict[str, object], root: Path) -> None:
    require(result.returncode != 0, "over-limit render unexpectedly succeeded")
    require(summary.get("status") == "failed", repr(summary))
    error = summary.get("error")
    require(isinstance(error, dict), f"failure summary has no typed error: {summary!r}")
    require(error.get("code") == "SCENE_CAPTURE_PREVIEW_FAILED", repr(error))
    message = error.get("message")
    require(isinstance(message, str), repr(error))
    require("declared-width limit" in message, message)
    require(f"configured {DECLARED_DIMENSION_LIMIT}" in message, message)
    require(f"observed {DECLARED_DIMENSION_LIMIT + 1}" in message, message)
    failure_path = root / "artifacts/failure.json"
    require(failure_path.is_file(), f"failure artifact does not exist: {failure_path}")
    failure = json.loads(failure_path.read_bytes())
    require(failure.get("status") == "failed", repr(failure))
    require(failure.get("render_id") == summary.get("render_id"), repr(failure))
    require(failure.get("error") == error, "failure.json differs from terminal error")
    require(not (root / "document.pdf").exists(), "failed render published a PDF")


def verify_healthy(result: subprocess.CompletedProcess[str], summary: dict[str, object], root: Path) -> None:
    require(result.returncode == 0, f"healthy render exited with {result.returncode}: {result.stderr[-2000:]}")
    require(summary.get("status") == "rendered", repr(summary))
    readiness = summary.get("readiness")
    require(
        isinstance(readiness, dict)
        and readiness.get("fixture") == "within-limit"
        and readiness.get("width") == 1
        and readiness.get("height") == 1,
        repr(readiness),
    )
    require((root / "document.pdf").read_bytes().startswith(b"%PDF-"), "healthy PDF is invalid")
    require(
        (root / "artifacts/scene-preview.png").read_bytes().startswith(b"\x89PNG\r\n\x1a\n"),
        "healthy preview is invalid",
    )
    require(not (root / "artifacts/failure.json").exists(), "healthy render wrote failure.json")


def self_test() -> None:
    over = png(DECLARED_DIMENSION_LIMIT + 1, 1)
    healthy = png(1, 1)
    require(over[16:24] == struct.pack(">II", DECLARED_DIMENSION_LIMIT + 1, 1), "bad over-limit IHDR")
    require(healthy[16:24] == struct.pack(">II", 1, 1), "bad healthy IHDR")
    require(len(over) < 1024, f"dimension fixture is unexpectedly large: {len(over)} bytes")


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        print("image limit checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)

    binary = Path(sys.argv[1]).expanduser().resolve()
    output = Path(sys.argv[2]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(not output.exists(), f"output directory already exists: {output}")

    with tempfile.TemporaryDirectory(prefix="pliego-image-limits-") as temp:
        root = Path(temp)
        over_root = root / "over-limit"
        healthy_root = root / "healthy"
        over_root.mkdir()
        healthy_root.mkdir()
        (over_root / "over-limit.html").write_text(
            document(png(DECLARED_DIMENSION_LIMIT + 1, 1), "over-limit"), encoding="utf-8"
        )
        (healthy_root / "within-limit.html").write_text(document(png(1, 1), "within-limit"), encoding="utf-8")

        failed_result = run(binary, over_root, "over-limit")
        retain(over_root, failed_result, output / "over-limit")
        failed_summary = final_json(failed_result)
        verify_failure(failed_result, failed_summary, over_root)
        healthy_result = run(binary, healthy_root, "within-limit")
        retain(healthy_root, healthy_result, output / "healthy")
        healthy_summary = final_json(healthy_result)
        verify_healthy(healthy_result, healthy_summary, healthy_root)

    print(f"image limit check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
