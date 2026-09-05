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
import tempfile
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
    result = subprocess.run([str(path), "-v"], capture_output=True, text=True, encoding="utf-8", timeout=30)
    text = (result.stdout + result.stderr).strip()
    if result.returncode != 0 or not text:
        raise RuntimeError(f"cannot identify {path}: {text or f'exit {result.returncode}'}")
    return text.splitlines()[0]


def oracle_identity() -> dict[str, str]:
    oracle = Path(__file__).resolve(strict=True)
    identity = {
        "contract": "pliego.pdf-oracle.v1",
        "oracle_path": str(oracle),
        "oracle_sha256": file_sha256(oracle),
    }
    for name in ("pdfinfo", "pdftotext", "pdffonts", "pdftoppm"):
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
    # pdftotext is explicitly invoked with -enc UTF-8. Do not decode that
    # output with Windows' locale code page and corrupt currencies/names.
    result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", timeout=60)
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


def parse_pdffonts(text: str) -> list[dict[str, Any]]:
    fonts: list[dict[str, Any]] = []
    for line in text.splitlines():
        parts = line.split()
        if len(parts) < 8 or parts[-5] not in {"yes", "no"}:
            continue
        name = re.sub(r"^[A-Z]{6}\+", "", parts[0])
        fonts.append(
            {
                "name": name,
                "embedded": parts[-5] == "yes",
                "subset": parts[-4] == "yes",
                "unicode": parts[-3] == "yes",
            }
        )
    return fonts


def read_pbm(path: Path) -> tuple[int, int, bytes]:
    data = path.read_bytes()
    offset = 0

    def token() -> bytes:
        nonlocal offset
        while offset < len(data):
            if data[offset : offset + 1] == b"#":
                offset = data.find(b"\n", offset)
                if offset < 0:
                    raise RuntimeError(f"invalid PBM comment in {path}")
            elif data[offset] in b" \t\r\n":
                offset += 1
            else:
                break
        start = offset
        while offset < len(data) and data[offset] not in b" \t\r\n":
            offset += 1
        if start == offset:
            raise RuntimeError(f"truncated PBM header in {path}")
        return data[start:offset]

    if token() != b"P4":
        raise RuntimeError(f"pdftoppm did not produce binary PBM: {path}")
    width, height = int(token()), int(token())
    if data[offset : offset + 2] == b"\r\n":
        offset += 2
    elif offset < len(data) and data[offset] in b" \t\r\n":
        offset += 1
    else:
        raise RuntimeError(f"PBM header omitted raster separator in {path}")
    raster = data[offset:]
    expected = ((width + 7) // 8) * height
    if width <= 0 or height <= 0 or len(raster) != expected:
        raise RuntimeError(f"invalid PBM raster length in {path}")
    return width, height, raster


def normalized_page_raster(width: int, height: int, raster: bytes) -> bytes:
    grid_width = 24
    grid_height = 32
    row_bytes = (width + 7) // 8
    normalized = bytearray((grid_width * grid_height + 7) // 8)
    ink_count = 0
    for y in range(height):
        for x in range(width):
            if not raster[y * row_bytes + x // 8] & (0x80 >> (x % 8)):
                continue
            ink_count += 1
            normalized_x = min(grid_width - 1, x * grid_width // width)
            normalized_y = min(grid_height - 1, y * grid_height // height)
            bit = normalized_y * grid_width + normalized_x
            normalized[bit // 8] |= 0x80 >> (bit % 8)
    if ink_count == 0:
        raise RuntimeError("page has no rasterized ink")
    ink_bucket = ((ink_count + 128) // 256) * 256
    return f"{width}x{height}:{grid_width}x{grid_height}:ink={ink_bucket}\0".encode("ascii") + normalized


def normalized_raster_sha256(pdf: Path, pdftoppm: Path, page_count: int) -> str:
    digest = hashlib.sha256()
    with tempfile.TemporaryDirectory(prefix="pliego-pdf-oracle-") as raw:
        prefix = Path(raw) / "page"
        run_tool(
            [
                str(pdftoppm),
                "-f",
                "1",
                "-l",
                str(page_count),
                "-r",
                "72",
                "-mono",
                str(pdf),
                str(prefix),
            ]
        )
        pages = sorted(
            prefix.parent.glob(f"{prefix.name}-*.pbm"),
            key=lambda path: int(path.stem.rsplit("-", 1)[1]),
        )
        if len(pages) != page_count:
            raise RuntimeError(f"pdftoppm produced {len(pages)} of {page_count} pages")
        for index, page in enumerate(pages, 1):
            width, height, raster = read_pbm(page)
            digest.update(f"page={index}\0".encode("ascii"))
            digest.update(normalized_page_raster(width, height, raster))
    return digest.hexdigest()


def inspect_pdf(
    pdf: Path,
    *,
    expected_page_count: int | None,
    expected_width_points: float | None,
    expected_height_points: float | None,
    dimension_tolerance_points: float,
    text_contains: list[str],
    text_equals: str | None,
    font_families: list[str],
    expected_raster_sha256: str | None,
    link_targets: list[str],
    pdfinfo: Path,
    pdftotext: Path,
    pdffonts: Path,
    pdftoppm: Path,
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

    normalized_document_text: str | None = None
    if text_contains or text_equals is not None:
        try:
            extracted = normalize_text(run_tool([str(pdftotext), "-layout", "-enc", "UTF-8", str(pdf), "-"]))
            normalized_document_text = extracted
            for fragment in text_contains:
                normalized = normalize_text(fragment)
                checks.append(check(f"text:{fragment}", normalized in extracted))
            if text_equals is not None:
                expected = normalize_text(text_equals)
                checks.append(
                    check(
                        "text_exact",
                        extracted == expected,
                        f"expected_sha256={hashlib.sha256(expected.encode()).hexdigest()} actual_sha256={hashlib.sha256(extracted.encode()).hexdigest()}",
                    )
                )
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            checks.append(check("text", False, str(error)))

    fonts: list[dict[str, Any]] = []
    if font_families:
        try:
            fonts = parse_pdffonts(run_tool([str(pdffonts), str(pdf)]))
            observed = {font["name"] for font in fonts}
            observed_folded = {name.casefold() for name in observed}
            expected = set(font_families)
            expected_folded = {name.casefold() for name in expected}
            checks.append(
                check(
                    "fonts_exact",
                    bool(fonts) and observed_folded == expected_folded and all(font["embedded"] for font in fonts),
                    f"expected={sorted(expected)} observed={sorted(observed)}",
                )
            )
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            checks.append(check("fonts_exact", False, str(error)))

    raster_sha256: str | None = None
    if expected_raster_sha256 is not None and page_count is not None:
        try:
            raster_sha256 = normalized_raster_sha256(pdf, pdftoppm, page_count)
            checks.append(check("raster_normalized", True, f"sha256={raster_sha256}"))
            checks.append(
                check(
                    "raster_parity",
                    raster_sha256 == expected_raster_sha256,
                    f"expected={expected_raster_sha256} actual={raster_sha256}",
                )
            )
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            checks.append(check("raster_normalized", False, str(error)))
    elif expected_raster_sha256 is not None:
        checks.append(check("raster_normalized", False, "page count is unavailable"))

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
        "normalized_text": normalized_document_text,
        "fonts": fonts,
        "normalized_raster_sha256": raster_sha256,
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
    parser.add_argument("--text-equals")
    parser.add_argument("--font-family", action="append", default=[])
    parser.add_argument("--raster-sha256")
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
        if args.raster_sha256 is not None and re.fullmatch(r"[0-9a-f]{64}", args.raster_sha256) is None:
            parser.error("--raster-sha256 must be a lowercase SHA-256 value")
        result = inspect_pdf(
            Path(args.pdf),
            expected_page_count=args.page_count,
            expected_width_points=args.page_width_points,
            expected_height_points=args.page_height_points,
            dimension_tolerance_points=args.dimension_tolerance_points,
            text_contains=args.text_contains,
            text_equals=args.text_equals,
            font_families=args.font_family,
            expected_raster_sha256=args.raster_sha256,
            link_targets=args.link_target,
            pdfinfo=resolve_tool("pdfinfo"),
            pdftotext=resolve_tool("pdftotext"),
            pdffonts=resolve_tool("pdffonts"),
            pdftoppm=resolve_tool("pdftoppm"),
        )
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"contract": "pliego.pdf-oracle.v1", "pass": False, "error": str(error)}))
        return 2
    print(json.dumps(result, separators=(",", ":")))
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
