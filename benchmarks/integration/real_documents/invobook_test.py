# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Pure frozen-invoice row/column and corpus-corruption checks."""

import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from invobook_corpus import DEFAULT_CORPUS, load_corpus
from invobook_oracle import corruptions, validate


class InvobookOracleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest, cls.expected = load_corpus()
        cls.observed = json.loads((DEFAULT_CORPUS / "reference-observation.json").read_text(encoding="utf-8"))

    def test_original_browser_observation_and_equations(self) -> None:
        facts = validate(self.observed, self.expected)
        self.assertEqual(facts["equationsCents"], [[1, 12500, 12500], [2, 12500, 25000]])
        self.assertEqual(facts["subtotalTaxTotalCents"], [37500, 7500, 45000])

    def test_seventeen_document_corruptions(self) -> None:
        results = corruptions(self.observed, self.expected)
        self.assertEqual(len(results), 17)
        self.assertTrue(all(result["outcome"] == "rejected" for result in results))

    def test_source_font_and_license_corruptions(self) -> None:
        for relative in [
            "input.html",
            "expected-facts.json",
            "fonts/DejaVuSans.ttf",
            "fonts/DejaVuSans-Bold.ttf",
            "licenses/LaravelDaily-GPL-3.0-only.txt",
        ]:
            with self.subTest(path=relative), tempfile.TemporaryDirectory() as temporary:
                corpus = Path(temporary) / "fixture"
                shutil.copytree(DEFAULT_CORPUS, corpus)
                path = corpus / relative
                value = path.read_bytes()
                path.write_bytes(bytes([value[0] ^ 1]) + value[1:])
                with self.assertRaisesRegex(ValueError, "Corpus bytes differ"):
                    load_corpus(corpus)

    def test_independent_staged_fonts(self) -> None:
        load_corpus(assets=DEFAULT_CORPUS / "fonts")
        with tempfile.TemporaryDirectory() as temporary:
            assets = Path(temporary) / "fonts with spaces"
            shutil.copytree(DEFAULT_CORPUS / "fonts", assets)
            (assets / "DejaVuSans.ttf").write_bytes(b"not the source face")
            with self.assertRaisesRegex(ValueError, "Staged invoice font changed"):
                load_corpus(assets=assets)

    def test_missing_or_extra_corpus_member(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            corpus = Path(temporary) / "fixture"
            shutil.copytree(DEFAULT_CORPUS, corpus)
            (corpus / "unlisted.txt").write_text("not part of frozen input", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "Unlisted or missing"):
                load_corpus(corpus)

    def test_frozen_request_geometry(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            corpus = Path(temporary) / "fixture"
            shutil.copytree(DEFAULT_CORPUS, corpus)
            manifest = copy.deepcopy(self.manifest)
            manifest["request"]["pageMargins"] = "1,1,1,1au"
            (corpus / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "geometry changed"):
                load_corpus(corpus)

    def test_header_and_all_totals_cannot_move_off_authored_edge(self) -> None:
        observed = copy.deepcopy(self.observed)
        facts = validate(observed, self.expected)
        right = facts["columns"]["subtotalRight"]
        for word in observed["pages"][0]["words"]:
            if word["x0"] > right - 50:
                word["x0"] -= 10
                word["x1"] -= 10
        with self.assertRaises(ValueError):
            validate(observed, self.expected)


if __name__ == "__main__":
    unittest.main()
