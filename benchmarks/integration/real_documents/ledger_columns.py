"""Pinned Aureus ledger numeric-column oracle using actual PDF word geometry."""

from __future__ import annotations

import argparse
import copy
import importlib.metadata
import re
from pathlib import Path

import pdfplumber
from pdfplumber.page import Page

from ledger_checks import require

LABELS = ("ACCOUNT", "DATE", "COMMUNICATION", "PARTNER", "DEBIT", "CREDIT", "BALANCE")
MONEY = re.compile(r"-?(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2}\Z")


def money(cents: int) -> str:
    return ("-" if cents < 0 else "") + f"{abs(cents) // 100:,}.{abs(cents) % 100:02d}"


def painted_rectangles(page: Page) -> list[dict]:
    result = list(page.rects)
    # Krilla may encode an axis-aligned rectangle using a closed four-edge
    # path rather than PDF's re operator. Require its actual corner geometry.
    for curve in page.curves:
        points = curve.get("pts", [])
        corners = {
            (curve["x0"], curve["top"]),
            (curve["x1"], curve["top"]),
            (curve["x1"], curve["bottom"]),
            (curve["x0"], curve["bottom"]),
        }
        if len(points) == 5 and points[0] == points[-1] and set(points[:-1]) == corners and curve.get("fill"):
            if all(a[0] == b[0] or a[1] == b[1] for a, b in zip(points, points[1:])):
                result.append(curve)
    return result


def validate_numeric(
    words: list[dict], bands: list[tuple[float, float]], rights: list[float], expected: list[str]
) -> list[list[str]]:
    cells = [[], [], []]
    for word in words:
        if not MONEY.fullmatch(word["text"]):
            require(
                not any((word["x1"] > left + 0.5 and word["x0"] < right - 0.5 for left, right in bands)),
                f"Unexpected non-money text in numeric column: {word}",
            )
            continue
        owners = [i for i, (left, right) in enumerate(bands) if word["x0"] >= left + 0.5 and word["x1"] <= right - 0.5]
        require(len(owners) == 1, f"Numeric word crosses or misses a column: {word}")
        column = owners[0]
        require(
            abs(word["x1"] - rights[column]) <= 0.75,
            f"Numeric word is not right-aligned with its reviewed column: {word}",
        )
        cells[column].append(word["text"])
    require(cells == [[value] if value else [] for value in expected], (cells, expected))
    return cells


def qualify_pdf(path: Path, manifest: dict, primary: dict, html_rows: dict | None = None) -> dict:
    evidence = []
    observed = []
    summary_checks = []
    by_marker = {e["marker"]: e for e in manifest["entries"]}
    require(len(by_marker) == 300, "Ledger qualification requirement failed: len(by_marker) == 300")
    with pdfplumber.open(path) as pdf:
        for number, page in enumerate(pdf.pages, 1):
            words = page.extract_words(x_tolerance=2, y_tolerance=2)
            header = []
            for label in LABELS:
                matching = [w for w in words if w["text"] == label]
                require(len(matching) == 1, (number, label, matching))
                header.append(matching[0])
            require(
                max((w["top"] for w in header)) - min((w["top"] for w in header)) <= 0.1,
                "Ledger qualification requirement failed: max((w['top'] for w in header)) - min((w['top'] for w in header)) <= 0.1",
            )
            rectangles = []
            for rect in painted_rectangles(page):
                color = rect.get("non_stroking_color")
                if (
                    isinstance(color, (tuple, list))
                    and len(color) == 3
                    and all(abs(a - b / 255) < 0.001 for a, b in zip(color, (243, 244, 246)))
                ):
                    if rect["top"] <= header[0]["top"] and rect["bottom"] >= header[0]["bottom"]:
                        rectangles.append(rect)
            rectangles.sort(key=lambda r: r["x0"])
            require(len(rectangles) == 7, (number, "Seven original header cell backgrounds are required"))
            for index, (word, rect) in enumerate(zip(header, rectangles, strict=True)):
                require(
                    word["x0"] >= rect["x0"] + 0.5 and word["x1"] <= rect["x1"] - 0.5,
                    "Ledger qualification requirement failed: word['x0'] >= rect['x0'] + 0.5 and word['x1'] <= rect['x1'] - 0.5",
                )
                if index:
                    require(
                        abs(rect["x0"] - rectangles[index - 1]["x1"]) <= 0.1,
                        "Ledger qualification requirement failed: abs(rect['x0'] - rectangles[index - 1]['x1']) <= 0.1",
                    )
            bands = [(r["x0"], r["x1"]) for r in rectangles[4:]]
            rights = [word["x1"] for word in header[4:]]
            income = manifest["expectedIncome"]
            summary_specs = {
                str(primary["code"]): [
                    money(income["debitCents"]),
                    money(income["creditCents"]),
                    money(income["closingCents"]),
                ],
                "Opening": ["", "", money(income["openingCents"])],
                "Total": [money(income["debitCents"] + income["creditCents"])] * 2 + [""],
            }
            for label, expected in summary_specs.items():
                for anchor in [w for w in words if w["text"] == label]:
                    row_words = [w for w in words if abs(w["top"] - anchor["top"]) <= 0.75]
                    validate_numeric(row_words, bands, rights, expected)
                    summary_checks.append(label)
            markers = [w for w in words if re.fullmatch(r"GL300-\d{4}", w["text"])]
            previous_bottom = None
            row_facts = []
            for marker in markers:
                expected = by_marker[marker["text"]]
                require(
                    marker["x0"] >= rectangles[2]["x0"] + 0.5 and marker["x1"] <= rectangles[2]["x1"] - 0.5,
                    "Ledger qualification requirement failed: marker['x0'] >= rectangles[2]['x0'] + 0.5 and marker['x1'] <= rectangles[2]['x1'] - 0.5",
                )
                if previous_bottom is not None:
                    require(marker["top"] > previous_bottom + 0.5, "Detail rows overlap vertically")
                previous_bottom = marker["bottom"]
                row_words = [w for w in words if abs(w["top"] - marker["top"]) <= 0.75]
                if html_rows is not None:
                    expected_row = html_rows[marker["text"]]
                    for column in range(4):
                        rect = rectangles[column]
                        cells = [w for w in row_words if w["x0"] >= rect["x0"] + 0.5 and w["x1"] <= rect["x1"] - 0.5]
                        actual = " ".join(w["text"] for w in sorted(cells, key=lambda w: w["x0"]))
                        require(
                            actual == expected_row[column],
                            (number, marker["text"], column, actual, expected_row[column]),
                        )
                expected_cells = [
                    money(expected["incomeDebitCents"]) if expected["incomeDebitCents"] else "",
                    money(expected["incomeCreditCents"]) if expected["incomeCreditCents"] else "",
                    money(expected["incomeRunningCents"]),
                ]
                cells = validate_numeric(row_words, bands, rights, expected_cells)
                observed.append(marker["text"])
                row_facts.append(
                    {
                        "marker": marker["text"],
                        "top": marker["top"],
                        "bottom": marker["bottom"],
                        "numericColumns": cells,
                    }
                )
            evidence.append(
                {
                    "page": number,
                    "numericBands": bands,
                    "headerRightAnchors": rights,
                    "tableBounds": [rectangles[0]["x0"], rectangles[-1]["x1"]],
                    "headerTop": header[0]["top"],
                    "rows": row_facts,
                }
            )
    require(
        observed == [e["marker"] for e in manifest["entries"]], "All 300 rows must have unique ordered geometric proof"
    )
    require(
        sorted(summary_checks) == sorted([str(primary["code"]), "Opening", "Total"]),
        "Ledger qualification requirement failed: sorted(summary_checks) == sorted([str(primary['code']), 'Opening', 'Total'])",
    )
    return {
        "schema": "aureus.pdf-money-columns.v1",
        "outcome": "passed",
        "pdfplumberVersion": importlib.metadata.version("pdfplumber"),
        "detailRows": len(observed),
        "summaryRows": summary_checks,
        "sales": sum(e["type"] == "out_invoice" for e in manifest["entries"]),
        "creditNotes": sum(e["type"] == "out_refund" for e in manifest["entries"]),
        "pdfMoneyLayoutQualified": True,
        "scope": "Every detail debit/credit and running balance plus primary/opening/total rows is inside its original reviewed header-cell band, right aligned and not crossing another band. Not a full visual or accessible-PDF gate.",
        "pages": evidence,
    }


def self_test() -> None:
    bands = [(0, 100), (100, 200), (200, 300)]
    rights = [97, 197, 297]
    words = [{"text": "10.00", "x0": 170, "x1": 197}, {"text": "-20.00", "x0": 267, "x1": 297}]
    expected = ["", "10.00", "-20.00"]
    validate_numeric(words, bands, rights, expected)
    variants = []
    wrong_column = copy.deepcopy(words)
    wrong_column[0].update(x0=70, x1=97)
    variants.append(wrong_column)
    overlap = copy.deepcopy(words)
    overlap[0].update(x0=95, x1=197)
    variants.append(overlap)
    variants.append(words[1:])
    variants.append(words + [words[0]])
    wrong_balance = copy.deepcopy(words)
    wrong_balance[1]["text"] = "-19.00"
    variants.append(wrong_balance)
    wrong_alignment = copy.deepcopy(words)
    wrong_alignment[0].update(x0=165, x1=192)
    variants.append(wrong_alignment)
    bad_text = copy.deepcopy(words)
    bad_text[0]["text"] = "ERROR"
    variants.append(bad_text)
    for variant in variants:
        try:
            validate_numeric(variant, bands, rights, expected)
        except AssertionError:
            continue
        raise AssertionError("Negative numeric geometry sample unexpectedly passed")
    print(f"numeric-column self-test: 1 positive, {len(variants)} negatives passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true", required=True)
    parser.parse_args()
    self_test()
