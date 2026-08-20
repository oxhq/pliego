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
        stdout = "Minimal\nHello, Pliego.\n"
    else:
        raise AssertionError(command)
    return subprocess.CompletedProcess(command, 0, stdout, "")


def main() -> None:
    pages, dimensions = oracle.parse_pdfinfo("Pages: 2\nPage 1 size: 100 x 200 pts\nPage 2 size: 100 x 200 pts\n")
    assert pages == 2 and dimensions == [(100.0, 200.0), (100.0, 200.0)]

    with tempfile.TemporaryDirectory() as raw:
        pdf = Path(raw) / "sample.pdf"
        pdf.write_bytes(b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\nstartxref\n0\n%%EOF\n")
        with patch.object(oracle.subprocess, "run", side_effect=fake_tool):
            result = oracle.inspect_pdf(
                pdf,
                expected_page_count=1,
                expected_width_points=595.276,
                expected_height_points=841.89,
                dimension_tolerance_points=0.01,
                text_contains=["Minimal", "Hello, Pliego."],
                link_targets=["https://pliego.dev/docs"],
                pdfinfo=Path("pdfinfo"),
                pdftotext=Path("pdftotext"),
            )
            assert result["pass"], result
            missing = oracle.inspect_pdf(
                pdf,
                expected_page_count=1,
                expected_width_points=595.276,
                expected_height_points=841.89,
                dimension_tolerance_points=0.01,
                text_contains=["Minimal"],
                link_targets=["https://example.invalid/missing"],
                pdfinfo=Path("pdfinfo"),
                pdftotext=Path("pdftotext"),
            )
            assert not missing["pass"]

        pdf.write_bytes(b"not a pdf")
        envelope_ok, _ = oracle.pdf_envelope(pdf)
        assert not envelope_ok

    print("PDF correctness oracle self-check passed")


if __name__ == "__main__":
    main()
