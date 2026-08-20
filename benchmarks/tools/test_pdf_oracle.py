#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import importlib.util
import subprocess
import tempfile
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("pdf_oracle.py")
SPEC = importlib.util.spec_from_file_location("pdf_oracle", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
oracle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(oracle)


def fake_tool(command: list[str], **_: object) -> subprocess.CompletedProcess[str]:
    name = Path(command[0]).name
    if name == "pdfinfo" and "-url" in command:
        stdout = "Page  Type          URL\n1     Annotation    https://pliego.dev/docs\n"
    elif name == "pdfinfo":
        stdout = "Pages:          1\nPage size:      595.276 x 841.89 pts (A4)\n"
    elif name == "pdftotext":
        stdout = "Minimal\nHello, Pliego. This fixture measures pure startup: one small page, one bundled font, no scripts or images.\n"
    elif name == "pdffonts":
        stdout = "name type encoding emb sub uni object ID\n------\nABCDEF+Ahem TrueType WinAnsi yes yes yes 1 0\n"
    else:
        raise AssertionError(command)
    return subprocess.CompletedProcess(command, 0, stdout, "")


def mixed_embedding_tool(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    if Path(command[0]).name != "pdffonts":
        return fake_tool(command, **kwargs)
    stdout = (
        "name type encoding emb sub uni object ID\n------\n"
        "ABCDEF+Ahem TrueType WinAnsi yes yes yes 1 0\n"
        "GHIJKL+Ahem TrueType WinAnsi no no yes 2 0\n"
    )
    return subprocess.CompletedProcess(command, 0, stdout, "")


def main() -> None:
    pages, dimensions = oracle.parse_pdfinfo("Pages: 2\nPage 1 size: 100 x 200 pts\nPage 2 size: 100 x 200 pts\n")
    assert pages == 2 and dimensions == [(100.0, 200.0), (100.0, 200.0)]

    with tempfile.TemporaryDirectory() as raw:
        pdf = Path(raw) / "sample.pdf"
        pbm = Path(raw) / "sample.pbm"
        pbm.write_bytes(b"P4\n8 1\n\x0a")
        assert oracle.read_pbm(pbm) == (8, 1, b"\x0a")

        def raster(pixels: set[tuple[int, int]]) -> bytes:
            value = bytearray(12 * 128)
            for x, y in pixels:
                value[y * 12 + x // 8] |= 0x80 >> (x % 8)
            return bytes(value)

        block = {(x, y) for x in range(4, 24) for y in range(4, 24)}
        translated = {(x + 32, y + 32) for x, y in block}
        scaled = {(x, y) for x in range(4, 34) for y in range(4, 34)}
        signature = oracle.normalized_page_raster(96, 128, raster(block))
        assert signature != oracle.normalized_page_raster(96, 128, raster(translated))
        assert signature != oracle.normalized_page_raster(96, 128, raster(scaled))

        pdf.write_bytes(b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\nstartxref\n0\n%%EOF\n")
        with (
            patch.object(oracle.subprocess, "run", side_effect=fake_tool),
            patch.object(oracle, "normalized_raster_sha256", return_value="0" * 64),
        ):
            result = oracle.inspect_pdf(
                pdf,
                expected_page_count=1,
                expected_width_points=595.276,
                expected_height_points=841.89,
                dimension_tolerance_points=0.01,
                text_contains=["Minimal", "Hello, Pliego."],
                text_equals=(
                    "Minimal Hello, Pliego. This fixture measures pure startup: one small page, "
                    "one bundled font, no scripts or images."
                ),
                font_families=["Ahem"],
                expected_raster_sha256="0" * 64,
                link_targets=["https://pliego.dev/docs"],
                pdfinfo=Path("pdfinfo"),
                pdftotext=Path("pdftotext"),
                pdffonts=Path("pdffonts"),
                pdftoppm=Path("pdftoppm"),
            )
            assert result["pass"], result
            assert result["normalized_text"].startswith("Minimal Hello")
            assert result["fonts"] == [{"name": "Ahem", "embedded": True, "subset": True, "unicode": True}]
            assert result["normalized_raster_sha256"] == "0" * 64
            missing = oracle.inspect_pdf(
                pdf,
                expected_page_count=1,
                expected_width_points=595.276,
                expected_height_points=841.89,
                dimension_tolerance_points=0.01,
                text_contains=["Minimal"],
                text_equals="wrong",
                font_families=["Ahem"],
                expected_raster_sha256="1" * 64,
                link_targets=["https://example.invalid/missing"],
                pdfinfo=Path("pdfinfo"),
                pdftotext=Path("pdftotext"),
                pdffonts=Path("pdffonts"),
                pdftoppm=Path("pdftoppm"),
            )
            assert not missing["pass"]

        with (
            patch.object(oracle.subprocess, "run", side_effect=mixed_embedding_tool),
            patch.object(oracle, "normalized_raster_sha256", return_value="0" * 64),
        ):
            mixed = oracle.inspect_pdf(
                pdf,
                expected_page_count=1,
                expected_width_points=595.276,
                expected_height_points=841.89,
                dimension_tolerance_points=0.01,
                text_contains=["Minimal"],
                text_equals=(
                    "Minimal Hello, Pliego. This fixture measures pure startup: one small page, "
                    "one bundled font, no scripts or images."
                ),
                font_families=["Ahem"],
                expected_raster_sha256="0" * 64,
                link_targets=["https://pliego.dev/docs"],
                pdfinfo=Path("pdfinfo"),
                pdftotext=Path("pdftotext"),
                pdffonts=Path("pdffonts"),
                pdftoppm=Path("pdftoppm"),
            )
        assert not mixed["pass"]
        assert next(check for check in mixed["checks"] if check["name"] == "fonts_exact")["status"] == "fail"

        pdf.write_bytes(b"not a pdf")
        envelope_ok, _ = oracle.pdf_envelope(pdf)
        assert not envelope_ok

    print("PDF correctness oracle self-check passed")


if __name__ == "__main__":
    main()
