"""Untimed PDF row/column/fact oracle for the frozen repaired Invobook invoice."""

from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
import re
import subprocess
import time
import xml.etree.ElementTree as ET

from invobook_corpus import DEFAULT_CORPUS, FONT_SHA256, load_corpus
from manufacturing_corpus import digest, member, require
from manufacturing_fonts import layout_fingerprint, qualify_pdf_fonts


def money(cents: int) -> str:
    return f"€{cents // 100}.{cents % 100:02d}"


def bounds(words: list[dict]) -> list[float]:
    return [
        min(w["x0"] for w in words),
        min(w["y0"] for w in words),
        max(w["x1"] for w in words),
        max(w["y1"] for w in words),
    ]


def center(word: dict) -> float:
    return (word["y0"] + word["y1"]) / 2


def rows(words: list[dict]) -> list[list[dict]]:
    grouped: list[list[dict]] = []
    for word in sorted(words, key=lambda w: (center(w), w["x0"])):
        # The authored total is 12px bold while labels are 10px. Group actual
        # intersecting line boxes by their center, not equal glyph-top guesses.
        if not grouped or abs(center(word) - center(grouped[-1][0])) > 1.0:
            grouped.append([word])
        else:
            grouped[-1].append(word)
    return [sorted(row, key=lambda w: w["x0"]) for row in grouped]


def validate(observed: dict, expected: dict) -> dict:
    require(len(observed["pages"]) == 1, "Expected one-page invoice")
    page = observed["pages"][0]
    require(
        abs(page["width"] - 595.2755905511812) <= 1 and abs(page["height"] - 841.8897637795276) <= 1,
        "Expected A4 portrait within original 1pt tolerance",
    )
    words = page["words"]
    for word in words:
        require(
            0 <= word["x0"] < word["x1"] <= page["width"] and 0 <= word["y0"] < word["y1"] <= page["height"],
            "Invoice text is clipped/offpage",
        )
    grouped = rows(words)
    strings = [[word["text"] for word in row] for row in grouped]
    text = " ".join(word["text"] for row in grouped for word in row)
    for phrase in [
        expected["seller"],
        expected["buyer"],
        expected["serial"],
        expected["invoiceDate"],
        expected["dueDate"],
        expected["notes"],
        expected["amountInWords"],
        expected["buyerAddress"],
    ]:
        require(phrase in text, "Missing original invoice text: " + phrase)
    require(text.count("Seller") == 3 and text.count("Buyer") == 3, "Original extra Seller/Buyer line changed")

    def exact_row(tokens: list[str]) -> dict:
        indices = [i for i, row in enumerate(strings) if row == tokens]
        require(len(indices) == 1, "Missing, duplicated or detached numeric row: " + " ".join(tokens))
        selected = grouped[indices[0]]
        for previous, following in zip(selected, selected[1:]):
            require(previous["x1"] <= following["x0"], "Invoice row words overlap")
        return {"text": tokens, "words": selected, "bounds": bounds(selected)}

    header = exact_row(["Description", "Qty", "Price", "Sub", "total"])
    description, quantity_header, price_header, *subtotal_header = header["words"]
    price_edge = price_header["x1"]
    subtotal_edge = bounds(subtotal_header)[2]
    quantity_center = (quantity_header["x0"] + quantity_header["x1"]) / 2
    require(
        abs(description["x0"] - 36) <= 1 and abs(subtotal_edge - (page["width"] - 36)) <= 1,
        "Invoice table moved outside its authored 36pt body margins",
    )
    item_rows = []
    for item in expected["items"]:
        row = exact_row(
            item["title"].split() + [str(item["quantity"]), money(item["unitPriceCents"]), money(item["subtotalCents"])]
        )
        quantity, price, subtotal = row["words"][-3:]
        require(
            abs((quantity["x0"] + quantity["x1"]) / 2 - quantity_center) <= 0.75, "Quantity is not centered under Qty"
        )
        require(
            abs(price["x1"] - price_edge) <= 0.75 and abs(subtotal["x1"] - subtotal_edge) <= 0.75,
            "Item amount is outside its Price/Sub total column",
        )
        require(abs(row["words"][0]["x0"] - description["x0"]) <= 0.75, "Item description left edge moved")
        require(item["quantity"] * item["unitPriceCents"] == item["subtotalCents"], "Invalid independent item equation")
        item_rows.append(row)
    totals = []
    for label, amount in [
        ("Taxable amount", expected["subtotalCents"]),
        ("Total taxes", expected["taxCents"]),
        ("Total amount", expected["totalCents"]),
    ]:
        row = exact_row(label.split() + [money(amount)])
        require(
            abs(row["words"][-1]["x1"] - subtotal_edge) <= 0.75 and abs(row["words"][-2]["x1"] - price_edge) <= 0.75,
            "Summary label/amount detached from the original columns",
        )
        totals.append(row)
    ordered = [header, *item_rows, *totals]
    for previous, following in zip(ordered, ordered[1:]):
        require(previous["bounds"][3] < following["bounds"][1], "Invoice numeric rows overlap or were reordered")
    notes = exact_row(expected["notes"].split())
    require(totals[-1]["bounds"][3] < notes["bounds"][1], "Totals collide with or follow the original notes")
    monetary_words = [w for w in words if re.fullmatch(r"€[0-9]+\.[0-9]{2}", w["text"])]
    require(len(monetary_words) == 7, "Unexpected missing/duplicated monetary cell")
    require(
        sum(item["subtotalCents"] for item in expected["items"]) == expected["subtotalCents"]
        and expected["subtotalCents"] + expected["taxCents"] == expected["totalCents"],
        "Invalid independent totals",
    )
    require(
        sorted(re.sub(r"^[A-Z]{6}\+", "", font["name"]) for font in observed["fonts"])
        == ["DejaVuSans", "DejaVuSans-Bold"]
        and all(font["embedded"] and font["unicode"] for font in observed["fonts"]),
        "Expected exactly original regular/bold embedded Unicode DejaVu faces",
    )
    return {
        "header": header,
        "items": item_rows,
        "totals": totals,
        "columns": {"quantityCenter": quantity_center, "priceRight": price_edge, "subtotalRight": subtotal_edge},
        "equationsCents": [
            [item["quantity"], item["unitPriceCents"], item["subtotalCents"]] for item in expected["items"]
        ],
        "subtotalTaxTotalCents": [expected["subtotalCents"], expected["taxCents"], expected["totalCents"]],
    }


def corruptions(observed: dict, expected: dict) -> list[dict]:
    facts = validate(observed, expected)
    results = []
    cases = [
        "quantity",
        "item-subtotal",
        "total",
        "missing-row",
        "duplicate-row",
        "row-overlap",
        "row-reorder",
        "price-column",
        "quantity-column",
        "total-column",
        "swapped-amount-columns",
        "wrong-font",
        "unembedded-font",
        "nonunicode-font",
        "offpage",
        "extra-page",
        "missing-notes",
    ]
    for case in cases:
        corrupt = copy.deepcopy(observed)
        words = corrupt["pages"][0]["words"]

        def matching(original: dict) -> dict:
            return next(word for word in words if word == original)

        if case == "quantity":
            matching(facts["items"][1]["words"][-3])["text"] = "1"
        elif case == "item-subtotal":
            matching(facts["items"][1]["words"][-1])["text"] = "€249.00"
        elif case == "total":
            matching(facts["totals"][-1]["words"][-1])["text"] = "€449.00"
        elif case == "missing-row":
            words[:] = [w for w in words if w not in facts["items"][1]["words"]]
        elif case == "duplicate-row":
            for original in facts["items"][0]["words"]:
                word = copy.deepcopy(original)
                word["y0"] += 12
                word["y1"] += 12
                words.append(word)
        elif case in {"row-overlap", "row-reorder"}:
            delta = facts["items"][1]["bounds"][1] - facts["items"][0]["bounds"][1]
            for original in facts["items"][1]["words"]:
                word = matching(original)
                word["y0"] -= delta
                word["y1"] -= delta
            if case == "row-reorder":
                for original in facts["items"][0]["words"]:
                    word = matching(original)
                    word["y0"] += delta
                    word["y1"] += delta
        elif case in {"price-column", "quantity-column", "total-column"}:
            original = (
                facts["totals"][-1]["words"][-1]
                if case == "total-column"
                else facts["items"][0]["words"][-2 if case == "price-column" else -3]
            )
            word = matching(original)
            word["x0"] -= 10
            word["x1"] -= 10
        elif case == "swapped-amount-columns":
            price = matching(facts["items"][1]["words"][-2])
            subtotal = matching(facts["items"][1]["words"][-1])
            price["text"], subtotal["text"] = subtotal["text"], price["text"]
        elif case == "wrong-font":
            corrupt["fonts"][0]["name"] = "Helvetica"
        elif case == "unembedded-font":
            corrupt["fonts"][0]["embedded"] = False
        elif case == "nonunicode-font":
            corrupt["fonts"][0]["unicode"] = False
        elif case == "offpage":
            words[0]["x0"] = -1
        elif case == "extra-page":
            corrupt["pages"].append(copy.deepcopy(corrupt["pages"][0]))
        else:
            next(w for w in words if w["text"] == "Notes:")["text"] = "Removed:"
        try:
            validate(corrupt, expected)
        except ValueError as error:
            results.append({"case": case, "outcome": "rejected", "reason": str(error)})
        else:
            raise ValueError("Corruption was accepted: " + case)
    return results


def read_observation(bbox: Path, font_output: str) -> dict:
    pages = []
    for page in ET.parse(bbox).iter("{http://www.w3.org/1999/xhtml}page"):
        pages.append(
            {
                "width": float(page.attrib["width"]),
                "height": float(page.attrib["height"]),
                "words": [
                    {
                        "text": word.text,
                        "x0": float(word.attrib["xMin"]),
                        "x1": float(word.attrib["xMax"]),
                        "y0": float(word.attrib["yMin"]),
                        "y1": float(word.attrib["yMax"]),
                    }
                    for word in page.iter("{http://www.w3.org/1999/xhtml}word")
                ],
            }
        )
    fonts = []
    for line in font_output.splitlines()[2:]:
        fields = line.split()
        if fields:
            require(len(fields) >= 8, "Invalid pdffonts row")
            fonts.append(
                {
                    "name": fields[0],
                    "embedded": fields[-5] == "yes",
                    "subset": fields[-4] == "yes",
                    "unicode": fields[-3] == "yes",
                }
            )
    return {"pages": pages, "fonts": fonts}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--pdf", type=Path, required=True)
    parser.add_argument("--provider", choices=("browsershot", "pliego"), default="browsershot")
    parser.add_argument("--scene", type=Path)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--poppler-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output, pdf, corpus = args.output.resolve(), args.pdf.resolve(), args.corpus.resolve()
    require(not output.exists() and not output.is_relative_to(corpus), "Output must be fresh and outside corpus")
    require(not pdf.is_relative_to(output), "Output must not contain input PDF")
    output.mkdir(parents=True)
    commands = []

    def run(name: str, arguments: list[str], log: str) -> str:
        binary = args.poppler_dir.resolve() / name
        if not binary.is_file():
            binary = binary.with_suffix(".exe")
        require(binary.is_file(), "Missing explicit Poppler executable: " + name)
        record = {"path": str(binary), "sha256": digest(binary), "arguments": arguments, "deadlineSeconds": 30}
        commands.append(record)
        start = time.perf_counter()
        try:
            result = subprocess.run([str(binary), *arguments], capture_output=True, timeout=30)
        except subprocess.TimeoutExpired as error:
            (output / (log + ".stdout")).write_bytes(error.stdout or b"")
            (output / (log + ".stderr")).write_bytes(error.stderr or b"")
            record.update(outcome="timeout", wallSeconds=time.perf_counter() - start)
            raise ValueError("Poppler exceeded its 30-second oracle deadline") from error
        (output / (log + ".stdout")).write_bytes(result.stdout)
        (output / (log + ".stderr")).write_bytes(result.stderr)
        record.update(exit=result.returncode, wallSeconds=time.perf_counter() - start)
        require(result.returncode == 0, "Poppler extraction failed: " + name)
        return result.stdout.decode("utf-8", errors="strict")

    try:
        manifest, expected = load_corpus(corpus, args.assets)
        require(pdf.is_file(), "Missing explicit PDF")
        font_proof = qualify_pdf_fonts(
            corpus,
            {name: (member(corpus, "fonts/" + name), sha) for name, sha in FONT_SHA256.items()},
            pdf,
            args.provider,
            args.scene,
            args.bundle,
        )
        for tool in ["pdftotext", "pdffonts", "pdftoppm"]:
            run(tool, ["-v"], tool + "-version")
        run("pdftotext", ["-bbox-layout", "-enc", "UTF-8", str(pdf), str(output / "document.bbox.xhtml")], "text")
        font_output = run("pdffonts", [str(pdf)], "fonts")
        run(
            "pdftoppm",
            ["-r", "120", "-png", "-singlefile", "-f", "1", "-l", "1", str(pdf), str(output / "page")],
            "raster",
        )
        observed = read_observation(output / "document.bbox.xhtml", font_output)
        (output / "observed.json").write_text(json.dumps(observed, indent=2) + "\n", encoding="utf-8")
        facts = validate(observed, expected)
        negatives = corruptions(observed, expected)
        report = {
            "schema": "pliego.real-document.invobook-oracle.v1",
            "outcome": "pass",
            "track": manifest["track"],
            "provider": args.provider,
            "layoutFingerprint": layout_fingerprint(observed),
            "layoutFingerprintPolicy": "extracted-text-geometry-and-faces-v1",
            "pdfSha256": digest(pdf),
            "pdfBytes": pdf.stat().st_size,
            "pages": len(observed["pages"]),
            "inputHtmlSha256": digest(corpus / "input.html"),
            "corpusManifestSha256": digest(corpus / "manifest.json"),
            "expectedFactsSha256": digest(corpus / "expected-facts.json"),
            "oracleSha256": digest(Path(__file__)),
            "corpusVerifierSha256": digest(Path(__file__).with_name("invobook_corpus.py")),
            "sharedPathVerifierSha256": digest(Path(__file__).with_name("manufacturing_corpus.py")),
            "fontBridgeSha256": digest(Path(__file__).with_name("manufacturing_fonts.py")),
            "fontVerifierSha256": digest(
                Path(__file__).with_name("ledger_fonts.py" if args.provider == "pliego" else "invobook_fonts.py")
            ),
            "sharedFontProgramVerifierSha256": digest(Path(__file__).with_name("ledger_fonts.py")),
            "fontProof": font_proof,
            "inputResources": [record for record in manifest["files"] if record["path"].endswith(".ttf")],
            "facts": facts,
            "negativeTests": negatives,
            "retainedArtifacts": [
                {"path": path.name, "sha256": digest(path), "bytes": path.stat().st_size}
                for path in [output / "document.bbox.xhtml", output / "page.png", output / "observed.json"]
            ],
            "visualReview": "Separate actual-PDF image review required",
            "fontProofBoundary": "Pliego requires source/scene/subset closure. Browsershot separately checks actual painted Identity-H CIDs, Unicode-to-original outline/metric/style and PDF advance widths. No arbitrary Chromium encoding or visual/release equivalence is implied.",
            "benchmarkQualified": False,
        }
    except Exception as error:
        report = {"outcome": "fail", "type": type(error).__name__, "reason": str(error)}
    report["commands"] = commands
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: report[key] for key in ["outcome", "reason", "pages", "pdfBytes"] if key in report}))
    if report["outcome"] != "pass":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
