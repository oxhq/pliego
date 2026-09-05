#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Portable correctness gate for the frozen, shared-repaired 300-entry Aureus ledger.

Runs outside benchmark timing. A machine pass is not visual, accessible-PDF,
application migration or performance qualification. Candidate fonts require the
exact scene/bundle/PDF closure in addition to independent original-font proof.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import re
import subprocess
from html.parser import HTMLParser
from pathlib import Path

import pdfplumber
from pypdf import PdfReader

from ledger_checks import closed_path, plain_path, require
from ledger_columns import money, painted_rectangles, qualify_pdf
from ledger_fonts import qualify_fonts

INPUT_SHA256 = "ccb11d4c713a66d71fe7798e03bfbeb3b7fe90b74229790c7e9c1a512a7aa105"
FOOTER = "Generated on March 31, 2026 at 12:00 PM"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def verify_fixture(fixture: Path) -> dict:
    fixture = plain_path(fixture, directory=True)
    closure = json.loads(closed_path(fixture, "closure.json").read_bytes())
    require(
        closure["schema"] == "pliego.real-document-closure.v1",
        "Ledger qualification requirement failed: closure['schema'] == 'pliego.real-document-closure.v1'",
    )
    seen = set()
    names = set()
    for entry in closure["entries"]:
        path = closed_path(fixture, entry["path"])
        name = entry["path"].casefold()
        require(name not in names, "Duplicate or case-aliased fixture path")
        names.add(name)
        seen.add(entry["path"])
        require(
            path.stat().st_size == entry["bytes"] and sha256(path) == entry["sha256"],
            "Ledger qualification requirement failed: path.stat().st_size == entry['bytes'] and sha256(path) == entry['sha256']",
        )
    for path in fixture.rglob("*"):
        plain_path(path, directory=path.is_dir())
    require(
        seen == {p.relative_to(fixture).as_posix() for p in fixture.rglob("*") if p.is_file()} - {"closure.json"},
        "Ledger qualification requirement failed: seen == {p.relative_to(fixture).as_posix() for p in fixture.rglob('*') if p.is_file()} - {'closure.json'}",
    )
    require(
        sha256(fixture / "document.blade.html") == INPUT_SHA256,
        "Ledger qualification requirement failed: sha256(fixture / 'document.blade.html') == INPUT_SHA256",
    )
    return closure


class Rows(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.rows, self.row, self.cell = [], None, None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag == "tr":
            require(self.row is None, "Ledger qualification requirement failed: self.row is None")
            self.row = []
        elif tag in ("td", "th") and self.row is not None:
            require(self.cell is None, "Ledger qualification requirement failed: self.cell is None")
            self.cell = []

    def handle_data(self, data: str) -> None:
        if self.cell is not None:
            self.cell.append(data)

    def handle_endtag(self, tag: str) -> None:
        if tag in ("td", "th") and self.cell is not None:
            self.row.append(" ".join("".join(self.cell).split()))
            self.cell = None
        elif tag == "tr" and self.row is not None:
            require(self.cell is None, "Ledger qualification requirement failed: self.cell is None")
            self.rows.append(self.row)
            self.row = None


def independent_inputs(manifest: dict, primary: dict, html: str) -> tuple[dict, list]:
    require(
        manifest["count"] == len(manifest["entries"]) == 300,
        "Ledger qualification requirement failed: manifest['count'] == len(manifest['entries']) == 300",
    )
    running, debit_total, credit_total = -125000, 0, 0
    for i, entry in enumerate(manifest["entries"], 1):
        amount = 10000 + 25 * i
        debit, credit = (amount, 0) if i % 5 == 0 else (0, amount)
        running += debit - credit
        debit_total += debit
        credit_total += credit
        require(
            entry
            == {
                "index": i,
                "marker": f"GL300-{i:04d}",
                "date": f"2026-03-{(i + 9) // 10:02d}",
                "type": "out_refund" if debit else "out_invoice",
                "cents": amount,
                "incomeDebitCents": debit,
                "incomeCreditCents": credit,
                "incomeRunningCents": running,
            },
            "Frozen row facts do not follow the independent accounting equation",
        )
    require(
        manifest["expectedIncome"]
        == {"openingCents": -125000, "debitCents": debit_total, "creditCents": credit_total, "closingCents": running},
        "Ledger qualification requirement failed: manifest['expectedIncome'] == {'openingCents': -125000, 'debitCents': debit_total, 'creditCents': credit_total, 'closingCents': running}",
    )
    require(
        (debit_total, credit_total, running) == (828750, 3300000, -2596250),
        "Ledger qualification requirement failed: (debit_total, credit_total, running) == (828750, 3300000, -2596250)",
    )
    parsed = Rows()
    parsed.feed(html)
    rows = [row for row in parsed.rows if len(row) == 7 and re.fullmatch(r"GL300-\d{4}", row[2])]
    require(len(rows) == 300, "Ledger qualification requirement failed: len(rows) == 300")
    for row, entry in zip(rows, manifest["entries"], strict=True):
        year, month, day = entry["date"].split("-")
        require(
            row[1:4] == [f"{month}/{day}/{year}", entry["marker"], "GL300 Synthetic Customer"],
            "Ledger qualification requirement failed: row[1:4] == [f'{month}/{day}/{year}', entry['marker'], 'GL300 Synthetic Customer']",
        )
        require(
            row[4:]
            == [
                money(entry["incomeDebitCents"]) if entry["incomeDebitCents"] else "",
                money(entry["incomeCreditCents"]) if entry["incomeCreditCents"] else "",
                money(entry["incomeRunningCents"]),
            ],
            "Ledger qualification requirement failed: row[4:] == [money(entry['incomeDebitCents']) if entry['incomeDebitCents'] else '', money(entry['incomeCreditCents']) if entry['incomeCreditCents'] else '', money(entry['incomeRunningCents'])]",
        )
    for control in [manifest["opening"], *manifest["controls"]]:
        require(control["marker"] not in html, "Ledger qualification requirement failed: control['marker'] not in html")
    summary = [r for r in parsed.rows if r[0] == f"{primary['code']} {primary['name']}"]
    opening = [r for r in parsed.rows if r[0] == "Opening Balance"]
    total = [r for r in parsed.rows if r[0] == "Total"]
    require(
        len(summary) == len(opening) == len(total) == 1,
        "Ledger qualification requirement failed: len(summary) == len(opening) == len(total) == 1",
    )
    require(
        summary[0][4:] == [money(debit_total), money(credit_total), money(running)],
        "Ledger qualification requirement failed: summary[0][4:] == [money(debit_total), money(credit_total), money(running)]",
    )
    require(
        opening[0] == ["Opening Balance", "03/01/2026", "", "", "", "", money(-125000)],
        "Ledger qualification requirement failed: opening[0] == ['Opening Balance', '03/01/2026', '', '', '', '', money(-125000)]",
    )
    require(
        total[0][4:] == [money(debit_total + credit_total)] * 2 + [""],
        "Ledger qualification requirement failed: total[0][4:] == [money(debit_total + credit_total)] * 2 + ['']",
    )
    return {r[2]: r for r in rows}, [summary[0], opening[0], total[0]]


def footer_and_text(pdf_path: Path, manifest: dict, summary_rows: list, columns: dict) -> tuple[list, str]:
    pages, texts = [], []
    with pdfplumber.open(pdf_path) as pdf:
        require(2 <= len(pdf.pages) <= 100, "Expanded ledger is missing pagination or exceeds the bounded oracle")
        for number, page in enumerate(pdf.pages, 1):
            require(abs(page.width - 841.89) <= 0.02 and abs(page.height - 595.28) <= 0.02, "Not authored A4 landscape")
            text = page.extract_text(x_tolerance=2, y_tolerance=2) or ""
            texts.append(text)
            check_footer(text, number)
            table = [c for c in page.chars if abs(c["size"] - 9) < 0.02 and c["text"].strip()]
            footer = []
            for char in page.chars:
                color = char.get("non_stroking_color")
                if (
                    isinstance(color, (tuple, list))
                    and len(color) == 3
                    and all(abs(a - b / 255) < 0.001 for a, b in zip(color, (156, 163, 175)))
                ):
                    if char["text"].strip():
                        footer.append(char)
            require(table and footer, "Ledger qualification requirement failed: table and footer")
            rule_candidates = [line for line in page.lines if line["top"] > 450 and line["x1"] - line["x0"] > 780]
            rule_candidates += [
                r
                for r in painted_rectangles(page)
                if r["top"] > 450 and r["x1"] - r["x0"] > 780 and 0 < r["bottom"] - r["top"] <= 0.76
            ]
            # A footer rule is below every detail/table row and immediately
            # before the uniquely colored footer, not an arbitrary table line.
            footer_top = min(c["top"] for c in footer)
            rule_candidates = [r for r in rule_candidates if 0 < footer_top - r["top"] < 20]
            require(len(rule_candidates) == 1, "Missing/ambiguous original full-width footer rule")
            rule = rule_candidates[0]
            table_bottom = max(c["bottom"] for c in table)
            require(table_bottom < rule["top"], "Table content overlaps the reserved footer band")
            require(
                0 <= min((c["x0"] for c in footer)) and max((c["x1"] for c in footer)) <= page.width,
                "Ledger qualification requirement failed: 0 <= min((c['x0'] for c in footer)) and max((c['x1'] for c in footer)) <= page.width",
            )
            require(max((c["bottom"] for c in footer)) < page.height, "Footer escaped page")
            require(len(columns["pages"][number - 1]["rows"]) > 0, "Unexplained blank/summary-only page")
            pages.append(
                {
                    "page": number,
                    "widthPt": page.width,
                    "heightPt": page.height,
                    "detailRows": len(columns["pages"][number - 1]["rows"]),
                    "tableBottomPt": table_bottom,
                    "footerRuleTopPt": rule["top"],
                    "footerTextTopPt": footer_top,
                    "footerTextBottomPt": max(c["bottom"] for c in footer),
                }
            )
    all_text = "\n\f\n".join(texts)
    require(
        re.findall("GL300-\\d{4}", all_text) == [e["marker"] for e in manifest["entries"]],
        "Missing/duplicate/reordered PDF rows",
    )
    require(
        all((c["marker"] not in all_text for c in [manifest["opening"], *manifest["controls"]])),
        "Ledger qualification requirement failed: all((c['marker'] not in all_text for c in [manifest['opening'], *manifest['controls']]))",
    )
    normalized_lines = [" ".join(line.split()) for line in all_text.splitlines()]
    for row in summary_rows:
        require(" ".join((cell for cell in row if cell)) in normalized_lines, "Missing independent PDF summary row")
    return pages, all_text


def check_footer(text: str, number: int) -> None:
    require(re.findall("\\bPage\\s+(\\d+)\\b", text) == [str(number)], "Wrong/missing/duplicate footer page counter")
    require(text.count(FOOTER) == 1, "Original generated-at footer content was lost or duplicated")


def qualify(
    fixture: Path, pdf: Path, provider: str, scene: Path | None = None, bundle: Path | None = None
) -> tuple[dict, str]:
    fixture = plain_path(fixture, directory=True)
    pdf = plain_path(pdf)
    verify_fixture(fixture)
    require(0 < pdf.stat().st_size <= 32 * 1024 * 1024, "PDF exceeds the bounded ledger oracle size")
    manifest = json.loads((fixture / "synthetic-input.json").read_bytes())
    primary = json.loads((fixture / "primary-account.json").read_bytes())
    rows, summaries = independent_inputs(
        manifest, primary, (fixture / "document.blade.html").read_text(encoding="utf-8")
    )
    columns = qualify_pdf(pdf, manifest, primary, rows)
    pages, text = footer_and_text(pdf, manifest, summaries, columns)
    fonts = qualify_fonts(PdfReader(pdf, strict=True), fixture, provider, scene, bundle, pdf)
    layout = {"columns": columns["pages"], "pages": pages}
    fingerprint = hashlib.sha256(json.dumps(layout, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return {
        "schema": "pliego.aureus-ledger-oracle.v1",
        "outcome": "passed",
        "provider": provider,
        "fixtureSha256": INPUT_SHA256,
        "closureSha256": sha256(fixture / "closure.json"),
        "pdfSha256": sha256(pdf),
        "detailRows": 300,
        "pages": pages,
        "columns": columns,
        "fonts": fonts,
        "layoutFingerprint": fingerprint,
        "independentBusinessOracle": {
            "openingCents": -125000,
            "debitCents": 828750,
            "creditCents": 3300000,
            "closingCents": -2596250,
            "sales": 240,
            "creditNotes": 60,
        },
        "dependencies": {
            name: importlib.metadata.version(name) for name in ("pypdf", "pdfplumber", "pdfminer.six", "fonttools")
        },
        "visualQualified": False,
        "benchmarkQualified": False,
        "boundary": "Frozen input, all ordered rows/dates/accounts/partners/amount columns, summaries, page counters, footer clearance and cryptographic font closure. Actual visual review, accessibility, app/store integration and benchmark qualification are separate.",
    }, text


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--pdf", type=Path, required=True)
    parser.add_argument("--provider", choices=("dompdf", "pliego"), required=True)
    parser.add_argument("--scene", type=Path)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--render-pages", action="store_true")
    parser.add_argument("--pdftoppm", type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    require(
        not output.exists() and (not output.is_relative_to(args.fixture.resolve())),
        "Output must be a fresh separate directory",
    )
    if args.render_pages:
        require(args.pdftoppm is not None and args.pdftoppm.is_file(), "Explicit pdftoppm executable required")
    output.mkdir(parents=True)
    report = {
        "schema": "pliego.aureus-ledger-oracle.v1",
        "outcome": "failed",
        "provider": args.provider,
        "visualQualified": False,
        "benchmarkQualified": False,
    }
    try:
        report, text = qualify(args.fixture, args.pdf, args.provider, args.scene, args.bundle)
        (output / "pdf-text.txt").write_text(text, encoding="utf-8")
        if args.render_pages:
            count = len(report["pages"])
            selected = sorted({1, (count + 1) // 2, count})
            for number in selected:
                command = [
                    str(args.pdftoppm.resolve()),
                    "-f",
                    str(number),
                    "-singlefile",
                    "-scale-to",
                    "1600",
                    "-png",
                    str(args.pdf.resolve()),
                    str(output / f"page-{number}"),
                ]
                run = subprocess.run(command, capture_output=True, timeout=60)
                require(run.returncode == 0, run.stderr.decode(errors="replace"))
            report["renderedPages"] = selected
            report["pdftoppmSha256"] = sha256(args.pdftoppm)
    except (AssertionError, OSError, ValueError, KeyError, TypeError) as error:
        report.update(outcome="failed", error=str(error))
        raise
    finally:
        save(output / "report.json", report)
    print(
        json.dumps(
            {
                "outcome": report["outcome"],
                "pages": len(report["pages"]),
                "detailRows": report["detailRows"],
                "visualQualified": False,
            }
        )
    )


if __name__ == "__main__":
    main()
