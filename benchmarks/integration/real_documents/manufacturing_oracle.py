# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Portable, untimed PDF oracle for the exact frozen Aureus work order.

Source: the retained original check-work-order.py. Document/row/barcode gates
are independent of the provider; visual and engine artifact closure remain
separate qualification gates. No application, database or network access.
"""

from __future__ import annotations

import argparse
import copy
import importlib.metadata
import json
from pathlib import Path
import re
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

from manufacturing_corpus import DEFAULT_CORPUS, FONT_SHA256, digest, load_corpus, member, require
from manufacturing_fonts import layout_fingerprint, qualify_pdf_fonts


def rows(words: list[dict]) -> list[list[dict]]:
    grouped: list[list[dict]] = []
    for word in sorted(words, key=lambda item: (item["y0"], item["x0"])):
        if not grouped or abs(word["y0"] - grouped[-1][0]["y0"]) > 0.7:
            grouped.append([word])
        else:
            grouped[-1].append(word)
    return [sorted(row, key=lambda item: item["x0"]) for row in grouped]


def validate(observed: dict, expected: dict) -> dict:
    require(
        len(observed["pages"]) == 1,
        "The unchanged 1/1 template requires exactly one page",
    )
    page = observed["pages"][0]
    require(
        abs(page["width"] - 595.28) < 0.5 and abs(page["height"] - 841.89) < 0.5,
        "Expected portrait A4",
    )
    for word in page["words"]:
        require(
            0 <= word["x0"] < word["x1"] <= page["width"] and 0 <= word["y0"] < word["y1"] <= page["height"],
            "Text extends outside page bounds",
        )
    grouped = rows(page["words"])
    strings = [[word["text"] for word in row] for row in grouped]
    all_text = " ".join(word["text"] for row in grouped for word in row)
    for text in [
        expected["orderName"],
        expected["company"],
        expected["operator"],
        "Operations Done",
        "Components",
    ]:
        require(text in all_text, "Missing required text: " + text)
    require(
        all_text.count("1 / 1") == 2 or all_text.count("1/1") == 2,
        "Original header/footer page counts must both remain",
    )

    def exact_row(tokens: list[str]) -> dict:
        matches = [i for i, actual in enumerate(strings) if actual == tokens]
        require(len(matches) == 1, "Expected exactly one aligned row: " + " ".join(tokens))
        row = grouped[matches[0]]
        return {
            "text": tokens,
            "bounds": [
                min(w["x0"] for w in row),
                min(w["y0"] for w in row),
                max(w["x1"] for w in row),
                max(w["y1"] for w in row),
            ],
        }

    quantity = f"{expected['quantity']:.2f}"
    quantity_row = exact_row(expected["product"].split() + [quantity, "Units", quantity, "Units"])
    operations = [
        exact_row(item["name"].split() + item["workCenter"].split() + [f"{item['minutes']:.1f}"])
        for item in expected["operations"]
    ]
    require(
        operations[0]["bounds"][3] < operations[1]["bounds"][1],
        "Operation rows reordered or overlapping",
    )
    components = [
        exact_row(item["name"].split() + [f"{item['required']:.2f}", f"{item['required']:.2f}", "Units"])
        for item in expected["components"]
    ]
    component_order = sorted(components, key=lambda item: item["bounds"][1])
    require(component_order == components, "Frozen capture008 component order changed")
    for previous, following in zip(component_order, component_order[1:]):
        require(previous["bounds"][3] < following["bounds"][1], "Component rows overlap")
    require(
        operations[-1]["bounds"][3] < component_order[0]["bounds"][1],
        "Operations/components overlap or reordered",
    )
    expected_codes = [
        expected["orderName"],
        *[item["name"] for item in expected["operations"]],
    ]
    codes = observed["barcodes"]
    require(
        len(codes) == 3 and sorted(code["text"] for code in codes) == sorted(expected_codes),
        "Barcode count/content differs from source business identifiers",
    )
    for code in codes:
        require(
            code["valid"] and code["format"] == "BarcodeFormat.Code128",
            "Barcode format/checksum invalid",
        )
        for x, y in code["points"]:
            require(
                0 <= x < observed["imageWidth"] and 0 <= y < observed["imageHeight"],
                "Barcode bounds outside raster",
            )
    require(
        all(font["embedded"] for font in observed["fonts"])
        and sorted(re.sub(r"^[A-Z]{6}\+", "", font["name"]) for font in observed["fonts"])
        == ["DejaVuSans", "DejaVuSans-Bold"],
        "Expected only the two embedded original DejaVu faces",
    )
    require(
        any("Bold" in font["name"] for font in observed["fonts"]),
        "Original bold face missing",
    )
    return {
        "productionQuantityRow": quantity_row,
        "operationRows": operations,
        "componentRowsInDocumentOrder": component_order,
        "barcodes": codes,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument(
        "--assets", type=Path, help="Optional separately staged directory containing the two exact TTF files"
    )
    parser.add_argument("--pdf", type=Path, required=True)
    parser.add_argument("--provider", choices=("dompdf", "pliego"), default="dompdf")
    parser.add_argument("--scene", type=Path)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--poppler-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = args.corpus.resolve()
    pdf = args.pdf.resolve()
    output = args.output.resolve()
    require(not output.exists(), "Oracle output must be fresh")
    require(not output.is_relative_to(source), "Output must not mutate the corpus")
    require(not pdf.is_relative_to(output), "Output must not contain the input PDF")
    output.mkdir(parents=True)
    commands = []

    def run(exe: str, arguments: list[str], name: str) -> str:
        candidate = args.poppler_dir.resolve() / exe
        binary = candidate if candidate.is_file() else candidate.with_suffix(".exe")
        require(binary.is_file(), "Missing explicit Poppler executable: " + exe)
        started = time.perf_counter()
        record = {
            "exe": str(binary),
            "sha256": digest(binary),
            "arguments": arguments,
            "deadlineSeconds": 30,
        }
        commands.append(record)
        try:
            result = subprocess.run([str(binary), *arguments], capture_output=True, timeout=30)
        except subprocess.TimeoutExpired as error:
            (output / f"{name}.stdout").write_bytes(error.stdout or b"")
            (output / f"{name}.stderr").write_bytes(error.stderr or b"")
            record.update(outcome="timeout", wallSeconds=time.perf_counter() - started)
            raise ValueError(f"{exe} exceeded its 30-second oracle deadline") from error
        (output / f"{name}.stdout").write_bytes(result.stdout)
        (output / f"{name}.stderr").write_bytes(result.stderr)
        record.update(exit=result.returncode, wallSeconds=time.perf_counter() - started)
        require(result.returncode == 0, f"{exe} failed")
        return result.stdout.decode("utf-8", errors="strict")

    try:
        from PIL import Image, _imaging
        import zxingcpp

        require(importlib.metadata.version("zxing-cpp") == "2.3.0", "Expected pinned zxing-cpp 2.3.0")
        require(importlib.metadata.version("Pillow") == "11.3.0", "Expected pinned Pillow 11.3.0")
        manifest, expected = load_corpus(source, args.assets)
        require(pdf.is_file(), "Missing explicit PDF input")
        font_proof = qualify_pdf_fonts(
            source,
            {name: (member(source, "resources/" + name), sha) for name, sha in FONT_SHA256.items()},
            pdf,
            args.provider,
            args.scene,
            args.bundle,
        )
        for executable in ["pdftotext", "pdftoppm", "pdffonts"]:
            run(executable, ["-v"], executable + "-version")
        run(
            "pdftotext",
            [
                "-bbox-layout",
                "-enc",
                "UTF-8",
                str(pdf),
                str(output / "document.bbox.xhtml"),
            ],
            "text",
        )
        xml = ET.parse(output / "document.bbox.xhtml")
        pages = []
        for page in xml.iter("{http://www.w3.org/1999/xhtml}page"):
            words = [
                {
                    "text": word.text,
                    "x0": float(word.attrib["xMin"]),
                    "y0": float(word.attrib["yMin"]),
                    "x1": float(word.attrib["xMax"]),
                    "y1": float(word.attrib["yMax"]),
                }
                for word in page.iter("{http://www.w3.org/1999/xhtml}word")
            ]
            pages.append(
                {
                    "width": float(page.attrib["width"]),
                    "height": float(page.attrib["height"]),
                    "words": words,
                }
            )
        run(
            "pdftoppm",
            [
                "-r",
                "200",
                "-png",
                "-singlefile",
                "-f",
                "1",
                "-l",
                "1",
                str(pdf),
                str(output / "page"),
            ],
            "raster",
        )
        font_output = run("pdffonts", [str(pdf)], "fonts")
        fonts = []
        for line in font_output.splitlines()[2:]:
            fields = line.split()
            if fields:
                fonts.append({"name": fields[0], "embedded": fields[-5] == "yes", "line": line})
        with Image.open(output / "page.png") as picture:
            codes = zxingcpp.read_barcodes(picture, formats=zxingcpp.BarcodeFormat.Code128, return_errors=True)
            observed = {
                "pages": pages,
                "imageWidth": picture.width,
                "imageHeight": picture.height,
                "fonts": fonts,
                "barcodes": [
                    {
                        "text": code.text,
                        "valid": code.valid,
                        "format": str(code.format),
                        "points": [
                            [
                                getattr(code.position, point).x,
                                getattr(code.position, point).y,
                            ]
                            for point in [
                                "top_left",
                                "top_right",
                                "bottom_right",
                                "bottom_left",
                            ]
                        ],
                    }
                    for code in codes
                ],
            }
        (output / "observed.json").write_text(json.dumps(observed, indent=2) + "\n", encoding="utf-8")
        facts = validate(observed, expected)
        negative_tests = []
        for name in [
            "wrong-component-quantity",
            "missing-barcode",
            "wrong-barcode-content",
            "extra-page",
            "operation-row-reorder",
        ]:
            corrupt = copy.deepcopy(observed)
            if name == "wrong-component-quantity":
                next(word for word in corrupt["pages"][0]["words"] if word["text"] == "10.00")["text"] = "10.01"
            elif name == "missing-barcode":
                corrupt["barcodes"].pop()
            elif name == "wrong-barcode-content":
                corrupt["barcodes"][0]["text"] = "WRONG"
            elif name == "extra-page":
                corrupt["pages"].append(copy.deepcopy(corrupt["pages"][0]))
            else:
                first, second = facts["operationRows"]
                delta = second["bounds"][1] - first["bounds"][1]
                for word in corrupt["pages"][0]["words"]:
                    if abs(word["y0"] - first["bounds"][1]) < 0.7:
                        word["y0"] += delta
                        word["y1"] += delta
                    elif abs(word["y0"] - second["bounds"][1]) < 0.7:
                        word["y0"] -= delta
                        word["y1"] -= delta
            try:
                validate(corrupt, expected)
            except ValueError as error:
                negative_tests.append({"case": name, "outcome": "rejected", "reason": str(error)})
            else:
                raise ValueError("Corruption was not detected: " + name)
        report = {
            "schema": "pliego.real-document.manufacturing-oracle.v1",
            "outcome": "pass",
            "track": manifest["track"],
            "provider": args.provider,
            "layoutFingerprint": layout_fingerprint(observed),
            "layoutFingerprintPolicy": "extracted-text-geometry-faces-and-barcode-positions-v1",
            "corpusManifestSha256": digest(source / "manifest.json"),
            "inputHtmlSha256": digest(source / manifest["input"]),
            "inputResources": [item for item in manifest["files"] if item["path"].endswith(".ttf")],
            "pdfSha256": digest(pdf),
            "pdfBytes": pdf.stat().st_size,
            "syntheticInputSha256": digest(source / "synthetic-input.json"),
            "oracleSha256": digest(Path(__file__)),
            "corpusVerifierSha256": digest(Path(__file__).with_name("manufacturing_corpus.py")),
            "fontBridgeSha256": digest(Path(__file__).with_name("manufacturing_fonts.py")),
            "fontVerifierSha256": digest(Path(__file__).with_name("ledger_fonts.py")),
            "fontProof": font_proof,
            "dependencyRequirementsSha256": digest(Path(__file__).with_name("manufacturing_requirements.txt")),
            "zxingVersion": importlib.metadata.version("zxing-cpp"),
            "zxingBinarySha256": digest(Path(zxingcpp.__file__)),
            "pillowVersion": importlib.metadata.version("Pillow"),
            "pillowBinarySha256": digest(Path(_imaging.__file__)),
            "python": {"version": sys.version, "executable": sys.executable},
            "pages": len(pages),
            "retainedArtifacts": [
                {"path": path.name, "sha256": digest(path), "bytes": path.stat().st_size}
                for path in [output / "document.bbox.xhtml", output / "page.png", output / "observed.json"]
            ],
            "facts": facts,
            "negativeTests": negative_tests,
            "visualReview": "separate human/agent image inspection required",
            "fontProofBoundary": "Exact original whole-font bytes for dompdf; hash-bound source/scene/subset glyph closure for Pliego. Neither proves visual equivalence or publication readiness.",
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
