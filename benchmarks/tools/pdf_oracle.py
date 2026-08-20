#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Untimed, target-neutral PDF correctness oracle for benchmark samples."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
from pathlib import Path
from typing import Any


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def resolve_tool(name: str) -> Path:
    found = shutil.which(name)
    if found is None:
        raise RuntimeError(f"required Poppler tool is unavailable: {name}")
    path = Path(found).resolve(strict=True)
    if not path.is_file():
        raise RuntimeError(f"required Poppler tool is not a file: {path}")
    return path


def tool_version(path: Path) -> str:
    result = subprocess.run([str(path), "-v"], capture_output=True, text=True, timeout=30)
    text = (result.stdout + result.stderr).strip()
    if result.returncode != 0 or not text:
        raise RuntimeError(f"cannot identify {path}: {text or f'exit {result.returncode}'}")
    return text.splitlines()[0]


def oracle_identity() -> dict[str, str]:
    identity = {"contract": "pliego.pdf-oracle.v1"}
    for name in ("pdfinfo", "pdftotext"):
        path = resolve_tool(name)
        identity[f"{name}_path"] = str(path)
        identity[f"{name}_sha256"] = file_sha256(path)
        identity[f"{name}_version"] = tool_version(path)
    return identity


def check(name: str, passed: bool, detail: str | None = None) -> dict[str, str]:
    value = {"name": name, "status": "pass" if passed else "fail"}
    if detail:
        value["detail"] = detail
    return value


def pdf_envelope(path: Path) -> tuple[bool, str]:
    try:
        size = path.stat().st_size
        with path.open("rb") as stream:
            head = stream.read(16)
            stream.seek(max(0, size - 4096))
            tail = stream.read()
    except OSError as error:
        return False, str(error)
    header = re.match(rb"%PDF-[12]\.[0-9]", head) is not None
    trailer = re.search(rb"%%EOF[\x00\x09\x0a\x0c\x0d\x20]*\Z", tail) is not None
    return header and trailer, f"bytes={size} header={'yes' if header else 'no'} eof={'yes' if trailer else 'no'}"


def run_tool(command: list[str]) -> str:
    result = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise RuntimeError(f"{Path(command[0]).name} failed: {detail or f'exit {result.returncode}'}")
    return result.stdout


def parse_pdfinfo(text: str) -> tuple[int, list[tuple[float, float]]]:
    pages_match = re.search(r"(?m)^Pages:\s+([1-9][0-9]*)\s*$", text)
    if pages_match is None:
        raise RuntimeError("pdfinfo omitted a positive page count")
    page_count = int(pages_match.group(1))
    numbered = [
        (int(index), float(width), float(height))
        for index, width, height in re.findall(
            r"(?m)^Page\s+([1-9][0-9]*)\s+size:\s+([0-9.]+)\s+x\s+([0-9.]+)\s+pts(?:\s|$)",
            text,
        )
    ]
    if numbered:
        numbered.sort()
        dimensions = [(width, height) for _, width, height in numbered]
    else:
        size_match = re.search(r"(?m)^Page size:\s+([0-9.]+)\s+x\s+([0-9.]+)\s+pts(?:\s|$)", text)
        if size_match is None:
            raise RuntimeError("pdfinfo omitted page dimensions")
        dimensions = [(float(size_match.group(1)), float(size_match.group(2)))] * page_count
    if len(dimensions) != page_count:
        raise RuntimeError(f"pdfinfo returned dimensions for {len(dimensions)} of {page_count} pages")
    return page_count, dimensions


def normalize_text(value: str) -> str:
    return " ".join(value.split())


def inspect_pdf(
    pdf: Path,
    *,
    expected_page_count: int | None,
    expected_width_points: float | None,
    expected_height_points: float | None,
    dimension_tolerance_points: float,
    text_contains: list[str],
    link_targets: list[str],
    pdfinfo: Path,
    pdftotext: Path,
) -> dict[str, Any]:
    checks: list[dict[str, str]] = []
    envelope_ok, envelope_detail = pdf_envelope(pdf)
    checks.append(check("pdf_envelope", envelope_ok, envelope_detail))
    page_count: int | None = None
    dimensions: list[tuple[float, float]] = []

    if envelope_ok:
        try:
            info = run_tool([str(pdfinfo), "-box", "-f", "1", "-l", "999999", str(pdf)])
            page_count, dimensions = parse_pdfinfo(info)
            checks.append(check("pdf_parse", True))
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            checks.append(check("pdf_parse", False, str(error)))
    else:
        checks.append(check("pdf_parse", False, "PDF envelope failed"))

    if expected_page_count is not None:
        checks.append(
            check(
                "page_count",
                page_count == expected_page_count,
                f"expected={expected_page_count} actual={page_count if page_count is not None else 'n/a'}",
            )
        )

    if expected_width_points is not None and expected_height_points is not None:
        wrong = [
            (index + 1, width, height)
            for index, (width, height) in enumerate(dimensions)
            if abs(width - expected_width_points) > dimension_tolerance_points
            or abs(height - expected_height_points) > dimension_tolerance_points
        ]
        checks.append(
            check(
                "page_dimensions",
                bool(dimensions) and not wrong,
                (
                    f"expected={expected_width_points}x{expected_height_points}pt "
                    f"tolerance={dimension_tolerance_points}pt wrong={wrong or 'none'}"
                ),
            )
        )

    if text_contains:
        try:
            extracted = normalize_text(run_tool([str(pdftotext), "-enc", "UTF-8", str(pdf), "-"]))
            for fragment in text_contains:
                normalized = normalize_text(fragment)
                checks.append(check(f"text:{fragment}", normalized in extracted))
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            checks.append(check("text", False, str(error)))

    if link_targets:
        try:
            links = run_tool([str(pdfinfo), "-url", str(pdf)])
            tokens = {token for line in links.splitlines() for token in line.split() if "://" in token}
            for target in link_targets:
                checks.append(check(f"link:{target}", target in tokens, f"observed={sorted(tokens)}"))
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            checks.append(check("links", False, str(error)))

    return {
        "contract": "pliego.pdf-oracle.v1",
        "pass": all(item["status"] == "pass" for item in checks),
        "page_count": page_count,
        "page_dimensions_points": [[width, height] for width, height in dimensions],
        "checks": checks,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", action="store_true")
    parser.add_argument("--pdf")
    parser.add_argument("--page-count", type=int)
    parser.add_argument("--page-width-points", type=float)
    parser.add_argument("--page-height-points", type=float)
    parser.add_argument("--dimension-tolerance-points", type=float, default=0.5)
    parser.add_argument("--text-contains", action="append", default=[])
    parser.add_argument("--link-target", action="append", default=[])
    args = parser.parse_args()

    try:
        if args.identity:
            print(json.dumps(oracle_identity(), separators=(",", ":")))
            return 0
        if not args.pdf:
            parser.error("--pdf is required unless --identity is used")
        if (args.page_width_points is None) != (args.page_height_points is None):
            parser.error("page width and height must be provided together")
        if args.dimension_tolerance_points < 0:
            parser.error("dimension tolerance cannot be negative")
        result = inspect_pdf(
            Path(args.pdf),
            expected_page_count=args.page_count,
            expected_width_points=args.page_width_points,
            expected_height_points=args.page_height_points,
            dimension_tolerance_points=args.dimension_tolerance_points,
            text_contains=args.text_contains,
            link_targets=args.link_target,
            pdfinfo=resolve_tool("pdfinfo"),
            pdftotext=resolve_tool("pdftotext"),
        )
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"contract": "pliego.pdf-oracle.v1", "pass": False, "error": str(error)}))
        return 2
    print(json.dumps(result, separators=(",", ":")))
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
