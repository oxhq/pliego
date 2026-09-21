#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Deterministically generate the scale and image benchmark fixtures.

Generates `ledger-20-pages/input.html`, `statement-100-pages/input.html`, and
`font-image-heavy/input.html` under `benchmarks/fixtures/`. The output is a
pure function of this script and the fixture constants: no randomness, no
clock. Regenerating on the same revision is a no-op (byte-identical).

Usage:
    python3 benchmarks/tools/generate_fixtures.py [--check]
"""

from __future__ import annotations

import argparse
import base64
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "benchmarks" / "fixtures"

# Counts were pinned on Linux v0.1.1 and revalidated unchanged on published v0.2.0.
LEDGER_ROWS = 250
STATEMENT_ROWS = 1300
FONT_IMAGE_PAGES = 6
IMAGES_PER_PAGE = 8
IMAGE_WIDTH = 320
IMAGE_HEIGHT = 180

TABLE_CSS = """
      table { border-collapse: collapse; table-layout: fixed; width: 560px; }
      caption { height: 32px; text-align: left; }
      th, td {
        border: 1px solid #243447;
        box-sizing: border-box;
        padding: 4px;
        text-align: left;
      }
      thead tr { height: 32px; }
      tbody tr { height: 36px; }
      th:nth-child(1), td:nth-child(1) { width: 96px; }
      th:nth-child(2), td:nth-child(2) { width: 96px; }
      th:nth-child(3), td:nth-child(3) { width: 200px; }
      th:nth-child(4), td:nth-child(4) { width: 84px; }
      th:nth-child(5), td:nth-child(5) { width: 84px; }
"""


def chunk(kind: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def validate_png(png: bytes, width: int, height: int) -> None:
    """Decode enough PNG structure to catch invalid generated fixtures."""
    if not png.startswith(b"\x89PNG\r\n\x1a\n"):
        raise ValueError("invalid PNG signature")

    offset = 8
    chunks = []
    compressed = bytearray()
    while offset < len(png):
        if offset + 12 > len(png):
            raise ValueError("truncated PNG chunk")
        length = struct.unpack(">I", png[offset : offset + 4])[0]
        kind = png[offset + 4 : offset + 8]
        data_start = offset + 8
        data_end = data_start + length
        crc_end = data_end + 4
        if crc_end > len(png):
            raise ValueError("PNG chunk exceeds file length")
        data = png[data_start:data_end]
        expected_crc = struct.unpack(">I", png[data_end:crc_end])[0]
        if zlib.crc32(kind + data) & 0xFFFFFFFF != expected_crc:
            raise ValueError(f"invalid {kind!r} chunk CRC")
        chunks.append(kind)
        if kind == b"IHDR":
            expected = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
            if data != expected:
                raise ValueError("unexpected PNG header")
        elif kind == b"IDAT":
            compressed.extend(data)
        offset = crc_end

    if chunks != [b"IHDR", b"IDAT", b"IEND"]:
        raise ValueError(f"unexpected PNG chunks: {chunks!r}")
    raw = zlib.decompress(compressed)
    row_bytes = 1 + width * 3
    if len(raw) != height * row_bytes:
        raise ValueError("unexpected decoded PNG size")
    if any(raw[row * row_bytes] != 0 for row in range(height)):
        raise ValueError("unexpected PNG row filter")


def make_png(width: int, height: int, index: int) -> bytes:
    """Deterministic chart-like truecolor PNG, no external dependencies."""
    background = (248, 250, 252)
    grid = (203, 213, 225)
    accent = PALETTE[index % len(PALETTE)]
    bars = 8
    bar_width = width // bars
    heights = [24 + ((index * 29 + bar * 37) % (height - 48)) for bar in range(bars)]
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            bar = min(x // bar_width, bars - 1)
            inside_bar = 7 <= x % bar_width < bar_width - 7
            if inside_bar and y >= height - heights[bar]:
                color = accent
            elif x % 64 == 0 or y % 45 == 0:
                color = grid
            else:
                color = background
            raw.extend(color)
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    validate_png(png, width, height)
    return png


PALETTE = [
    (37, 99, 235),  # blue
    (245, 158, 11),  # amber
    (71, 85, 105),  # slate
    (16, 185, 129),  # emerald
    (220, 38, 38),  # red
    (124, 58, 237),  # violet
    (14, 165, 233),  # sky
    (234, 88, 12),  # orange
]


def image_pages() -> str:
    pages = []
    for page in range(FONT_IMAGE_PAGES):
        cards = []
        for slot in range(IMAGES_PER_PAGE):
            index = page * IMAGES_PER_PAGE + slot
            uri = base64.b64encode(make_png(IMAGE_WIDTH, IMAGE_HEIGHT, index)).decode("ascii")
            cards.append(
                "          <figure>"
                f'<img src="data:image/png;base64,{uri}" alt="Chart {index + 1}">'
                f"<figcaption>Chart {index + 1:02d} / page {page + 1}</figcaption>"
                "</figure>"
            )
        pages.append(
            '    <section class="page">\n'
            f"      <h1>Font and image heavy - page {page + 1}</h1>\n"
            "      <p>Deterministic embedded-font labels and unique decoded chart images.</p>\n"
            '      <div class="grid">\n' + "\n".join(cards) + "\n      </div>\n"
            "    </section>"
        )
    return "\n".join(pages)


def statement_document(rows: int, title: str, caption: str) -> str:
    body_rows = []
    for index in range(1, rows + 1):
        ref = f"REF-{index:06d}"
        description = f"LEDGER LINE {index:05d} — deterministic generated row"
        body_rows.append(
            "        <tr>"
            f"<td>2026-{index % 12 + 1:02d}-{index % 28 + 1:02d}</td>"
            f"<td>{ref}</td>"
            f"<td>{description}</td>"
            "<td>1,234.56</td><td>0.00</td>"
            "</tr>"
        )
    return f"""<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>{title}</title>
    <style>
      @font-face {{
        font-family: Ahem;
        src: url("Ahem.ttf");
      }}
      html, body {{ margin: 0; }}
      body {{ font: 12px/16px Ahem; }}
{TABLE_CSS}    </style>
  </head>
  <body>
    <table>
      <caption>{caption}</caption>
      <thead>
        <tr><th>DATE</th><th>REF</th><th>DESCRIPTION</th><th>DEBIT</th><th>CREDIT</th></tr>
      </thead>
      <tbody>
{chr(10).join(body_rows)}
      </tbody>
    </table>
  </body>
</html>
"""


def font_image_document() -> str:
    return f"""<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Font and image heavy</title>
    <style>
      @font-face {{
        font-family: Ahem;
        src: url("Ahem.ttf");
      }}
      html, body {{ margin: 0; }}
      body {{ font: 12px/16px Ahem; padding: 16px; }}
      .page {{ page-break-after: always; break-after: page; }}
      .page:last-child {{ page-break-after: auto; break-after: auto; }}
      h1 {{ font-size: 18px; margin: 0 0 8px; }}
      p {{ margin: 0 0 12px; }}
      .grid {{ display: flex; flex-wrap: wrap; gap: 8px; }}
      figure {{ margin: 0; width: 160px; }}
      .grid img {{ display: block; width: 160px; height: 90px; }}
      figcaption {{ margin-top: 2px; }}
    </style>
  </head>
  <body>
{image_pages()}
  </body>
</html>
"""


def write(path: Path, content: str) -> None:
    expected = content.encode("utf-8")
    if path.exists() and path.read_bytes() == expected:
        print(f"unchanged  {path.relative_to(ROOT)}")
        return
    path.write_bytes(expected)
    print(f"written    {path.relative_to(ROOT)}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if a fixture would change")
    args = parser.parse_args()

    outputs = {
        FIXTURES / "ledger-20-pages" / "input.html": statement_document(
            LEDGER_ROWS, "Ledger — 20 pages", "LEDGER PLG-2026-01"
        ),
        FIXTURES / "statement-100-pages" / "input.html": statement_document(
            STATEMENT_ROWS, "Statement — 100 pages", "STATEMENT PLG-2026-01"
        ),
        FIXTURES / "font-image-heavy" / "input.html": font_image_document(),
    }

    changed = []
    for path, content in outputs.items():
        expected = content.encode("utf-8")
        if args.check:
            if not path.exists() or path.read_bytes() != expected:
                changed.append(path)
        elif not path.exists() or path.read_bytes() != expected:
            write(path, content)

    if args.check:
        if changed:
            print("fixtures are stale; run generate_fixtures.py", file=sys.stderr)
            for path in changed:
                print(f"  {path.relative_to(ROOT)}", file=sys.stderr)
            return 1
        print("fixtures are current")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
