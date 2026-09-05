# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Corpus identity and independent observation-corruption tests (no render)."""

import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from manufacturing_corpus import DEFAULT_CORPUS, load_corpus, member
from manufacturing_oracle import validate


class ManufacturingOracleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest, cls.expected = load_corpus()
        cls.reference = json.loads((DEFAULT_CORPUS / "reference-observation.json").read_text(encoding="utf-8"))

    def test_original_legacy_observation(self) -> None:
        facts = validate(self.reference, self.expected)
        self.assertEqual(len(facts["barcodes"]), 3)
        self.assertEqual(len(facts["componentRowsInDocumentOrder"]), 3)

    def test_document_corruptions(self) -> None:
        cases = [
            "wrong-component-quantity",
            "missing-barcode",
            "wrong-barcode-content",
            "extra-page",
            "operation-row-reorder",
            "barcode-checksum",
            "barcode-format",
            "barcode-offpage",
            "text-offpage",
            "wrong-font",
            "unembedded-font",
            "missing-bold",
            "duplicate-row",
            "component-row-reorder",
            "wrong-page-size",
            "missing-header-footer",
            "wrong-production-quantity",
            "wrong-duration",
        ]
        facts = validate(self.reference, self.expected)
        for case in cases:
            with self.subTest(case=case):
                observed = copy.deepcopy(self.reference)
                words = observed["pages"][0]["words"]
                if case == "wrong-component-quantity":
                    next(w for w in words if w["text"] == "10.00")["text"] = "10.01"
                elif case == "missing-barcode":
                    observed["barcodes"].pop()
                elif case == "wrong-barcode-content":
                    observed["barcodes"][0]["text"] = "WRONG"
                elif case == "extra-page":
                    observed["pages"].append(copy.deepcopy(observed["pages"][0]))
                elif case in {"operation-row-reorder", "component-row-reorder"}:
                    key = "operationRows" if case.startswith("operation") else "componentRowsInDocumentOrder"
                    first, second = facts[key][:2]
                    delta = second["bounds"][1] - first["bounds"][1]
                    for word in words:
                        if abs(word["y0"] - first["bounds"][1]) < 0.7:
                            word["y0"] += delta
                            word["y1"] += delta
                        elif abs(word["y0"] - second["bounds"][1]) < 0.7:
                            word["y0"] -= delta
                            word["y1"] -= delta
                elif case == "barcode-checksum":
                    observed["barcodes"][0]["valid"] = False
                elif case == "barcode-format":
                    observed["barcodes"][0]["format"] = "BarcodeFormat.Code39"
                elif case == "barcode-offpage":
                    observed["barcodes"][0]["points"][0][0] = observed["imageWidth"]
                elif case == "text-offpage":
                    words[0]["x0"] = -1
                elif case == "wrong-font":
                    observed["fonts"][0]["name"] = "DejaVuSans-Fake"
                elif case == "unembedded-font":
                    observed["fonts"][0]["embedded"] = False
                elif case == "missing-bold":
                    observed["fonts"] = [font for font in observed["fonts"] if "Bold" not in font["name"]]
                elif case == "duplicate-row":
                    row = facts["componentRowsInDocumentOrder"][0]
                    copied = [copy.deepcopy(word) for word in words if abs(word["y0"] - row["bounds"][1]) < 0.7]
                    for word in copied:
                        word["y0"] += 5
                        word["y1"] += 5
                    words.extend(copied)
                elif case == "wrong-page-size":
                    observed["pages"][0]["width"] = 612
                elif case == "missing-header-footer":
                    next(w for w in words if w["text"] == "1")["text"] = "2"
                elif case == "wrong-production-quantity":
                    row = facts["productionQuantityRow"]
                    next(w for w in words if w["text"] == "5.00" and abs(w["y0"] - row["bounds"][1]) < 0.7)["text"] = (
                        "4.00"
                    )
                else:
                    next(w for w in words if w["text"] == "12.5")["text"] = "12.6"
                with self.assertRaises(ValueError):
                    validate(observed, self.expected)

    def test_pdf_subset_prefix_does_not_change_face_name(self) -> None:
        observed = copy.deepcopy(self.reference)
        for font in observed["fonts"]:
            font["name"] = "ABCDEF+" + font["name"]
        validate(observed, self.expected)

    def test_corpus_corruptions(self) -> None:
        for relative in [
            "document.blade.html",
            "synthetic-input.json",
            "resources/DejaVuSans.ttf",
            "resources/DejaVuSans-Bold.ttf",
            "licenses/Aureus-MIT.txt",
        ]:
            with self.subTest(path=relative), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary) / "fixture"
                shutil.copytree(DEFAULT_CORPUS, root)
                path = root / relative
                data = path.read_bytes()
                path.write_bytes(bytes([data[0] ^ 1]) + data[1:])
                with self.assertRaisesRegex(ValueError, "Corpus hash differs"):
                    load_corpus(root)

    def test_separately_supplied_assets_are_exact(self) -> None:
        load_corpus(assets=DEFAULT_CORPUS / "resources")
        with tempfile.TemporaryDirectory() as temporary:
            assets = Path(temporary)
            shutil.copytree(DEFAULT_CORPUS / "resources", assets, dirs_exist_ok=True)
            path = assets / "DejaVuSans.ttf"
            path.write_bytes(b"not the original font")
            with self.assertRaisesRegex(ValueError, "Supplied font bytes changed"):
                load_corpus(assets=assets)

    def test_manifest_path_escapes_rejected(self) -> None:
        for relative in ["../secret", "/secret", "C:/secret", "resources\\font.ttf"]:
            with self.subTest(path=relative), self.assertRaises(ValueError):
                member(DEFAULT_CORPUS, relative)


if __name__ == "__main__":
    unittest.main()
