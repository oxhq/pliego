#!/usr/bin/env python3

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

# Row heights match the invoice fixture (36px rows, 32px header) so page
# estimates are comparable. Targets are estimates; the first signed baseline pins them.
LEDGER_ROWS = 500  # ~20 pages at ~25 rows/page
STATEMENT_ROWS = 2500  # ~100 pages
FONT_IMAGE_PAGES = 6
IMAGES_PER_PAGE = 10

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


def chunk(data: bytes, kind: bytes) -> bytes:
    return kind + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF) + data


def make_png(width: int, height: int, rgb: tuple[int, int, int]) -> bytes:
    """Minimal deterministic truecolor PNG, no external dependencies."""
    pixel = bytes(rgb)
    row = b"\x00" + pixel * width
    raw = row * height
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(ihdr, b"IHDR")
        + chunk(zlib.compress(raw, 9), b"IDAT")
        + chunk(b"", b"IEND")
    )


PALETTE = [
    (37, 99, 235),    # blue
    (245, 158, 11),   # amber
    (71, 85, 105),    # slate
    (16, 185, 129),   # emerald
    (220, 38, 38),    # red
    (124, 58, 237),   # violet
    (14, 165, 233),   # sky
    (234, 88, 12),    # orange
]


def image_tags(total: int) -> str:
    pngs = [make_png(64, 48, color) for color in PALETTE]
    uris = [base64.b64encode(png).decode("ascii") for png in pngs]
    tags = []
    for index in range(total):
        uri = uris[index % len(uris)]
        alt = f"Chart color {index % len(uris) + 1}"
        tags.append(f'        <img src="data:image/png;base64,{uri}" alt="{alt}">')
    return "\n".join(tags)


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
      html, body {{ margin: 0; }}
      body {{ font: 12px/16px sans-serif; }}
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
    total_images = FONT_IMAGE_PAGES * IMAGES_PER_PAGE
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
      h1 {{ font-size: 18px; }}
      .grid {{ display: flex; flex-wrap: wrap; gap: 8px; }}
      .grid img {{ width: 64px; height: 48px; }}
    </style>
  </head>
  <body>
    <h1>Font and image heavy</h1>
    <div class="grid">
{image_tags(total_images)}
    </div>
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
