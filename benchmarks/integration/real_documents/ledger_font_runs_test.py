# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Used Krilla text/ActualText proof, including resealed real-font corruptions."""

from __future__ import annotations

import ast
import io
from pathlib import Path
import tempfile
import unittest

from pypdf import PdfReader, PdfWriter
from pypdf.generic import ContentStream, DecodedStreamObject, NameObject

import invobook_corpus
from ledger_fonts import painted_text_runs, qualify_fonts, unicode_map
from manufacturing_font_test import test_bundle, update_bundle


class PaintedRunsTests(unittest.TestCase):
    def parse(self, source: bytes, fonts: dict | None = None) -> tuple[list, int]:
        stream = DecodedStreamObject()
        stream.set_data(source)
        return painted_text_runs(
            ContentStream(stream, None).operations,
            fonts
            or {
                "/F0": {"source": "regular", "order": [".notdef", "zero", "A"], "mapping": {1: " 0", 2: "A"}},
                "/F1": {"source": "bold", "order": [".notdef", "zero", "A"], "mapping": {1: " 0", 2: "A"}},
            },
        )

    def test_scoped_alias_and_legitimate_tj_segmentation(self) -> None:
        actual, overrides = self.parse(
            b"BT /F0 12 Tf <0001> Tj /Span << /ActualText (0) >> BDC <0001> Tj EMC [<0002> 10 <0002>] TJ ET"
        )
        self.assertEqual(
            actual, [("regular", "zero", " 0"), ("regular", "zero", "0"), ("regular", "A", "A"), ("regular", "A", "A")]
        )
        self.assertEqual(overrides, 1)
        whole, _ = self.parse(b"BT /F0 12 Tf <00020002> Tj ET")
        split, _ = self.parse(b"BT /F0 12 Tf <0002> Tj <0002> Tj ET")
        self.assertEqual(whole, split)

    def test_graphics_restore_preserves_actual_active_font(self) -> None:
        actual, _ = self.parse(b"BT /F0 12 Tf <0002> Tj ET q BT /F1 12 Tf <0002> Tj ET Q BT <0002> Tj ET")
        self.assertEqual([row[0] for row in actual], ["regular", "bold", "regular"])

    def test_unscoped_nested_unclosed_and_segmented_override_reject(self) -> None:
        for source in (
            b"/Span << /ActualText (0) >> BDC BT /F0 12 Tf <0001> Tj ET EMC",
            b"BT /F0 12 Tf /Span << /ActualText (0) >> BDC /Span << /ActualText (0) >> BDC <0001> Tj EMC EMC ET",
            b"BT /F0 12 Tf /Span << /ActualText (0) >> BDC <0001> Tj ET",
            b"BT /F0 12 Tf /Span << /ActualText (0) >> BDC EMC ET",
            b"BT /F0 12 Tf /Span << /ActualText (0) >> BDC <0001> Tj <0001> Tj EMC ET",
            b"BT /F0 12 Tf /Span << /ActualText (0) >> BDC <00010001> Tj EMC ET",
            b"BT /F0 12 Tf /Span << /ActualText (0) >> BDC [<0001> 0 <>] TJ EMC ET",
            b"BT /F0 12 Tf /Span /Props BDC <0001> Tj EMC ET",
        ):
            with self.subTest(source=source), self.assertRaises(AssertionError):
                self.parse(source)

    def test_resource_scope_form_and_invisible_text_reject(self) -> None:
        for source in (
            b"BT /OtherPageFont 12 Tf <0001> Tj ET",
            b"BT <0001> Tj ET",
            b"/Form0 Do",
            b"BT /F0 12 Tf 3 Tr <0001> Tj ET",
            b"BT /F0 12 Tf <0000> Tj ET",
            b"BT /F0 12 Tf <0003> Tj ET",
            b"BT /F0 12 Tf <0001> ' ET",
            b"q BT /F0 12 Tf <0001> Tj ET",
        ):
            with self.subTest(source=source), self.assertRaises(AssertionError):
                self.parse(source)

    def test_unused_mapping_does_not_manufacture_painted_evidence(self) -> None:
        actual, _ = self.parse(b"BT /F0 12 Tf <0002> Tj ET")
        self.assertEqual(actual, [("regular", "A", "A")])
        self.assertNotIn(("regular", "zero", " 0"), actual)

    def test_authoritative_checks_survive_optimization(self) -> None:
        tree = ast.parse(Path(__file__).with_name("ledger_fonts.py").read_text(encoding="utf-8"))
        self.assertFalse(any(isinstance(node, ast.Assert) for node in ast.walk(tree)))


class ResealedFontTests(unittest.TestCase):
    def sources(self) -> dict:
        return {
            name: (invobook_corpus.DEFAULT_CORPUS / "fonts" / name, sha)
            for name, sha in invobook_corpus.FONT_SHA256.items()
        }

    def mutate(self, root: Path, change: str) -> None:
        pdf = root / "document.pdf"
        writer = PdfWriter(clone_from=PdfReader(pdf, strict=True))
        page = writer.pages[0]
        fonts = page["/Resources"]["/Font"]
        first, second = fonts["/F0"].get_object(), fonts["/F1"].get_object()
        first_cid, second_cid = next(iter(unicode_map(first))), next(iter(unicode_map(second)))
        mapping = DecodedStreamObject()
        mapping.set_data(f"1 beginbfchar\n<{first_cid:04X}> <00200041>\nendbfchar".encode())
        first[NameObject("/ToUnicode")] = writer._add_object(mapping)
        first_run = f"BT /F0 12 Tf /Span << /ActualText (A) >> BDC <{first_cid:04X}> Tj EMC ET"
        second_run = f"BT /F1 12 Tf <{second_cid:04X}> Tj ET"
        source = first_run + "\n" + second_run
        if change == "wrong-actual-text":
            source = source.replace("/ActualText (A)", "/ActualText (B)")
        elif change == "unscoped-actual-text":
            source = source.replace(
                "BT /F0 12 Tf /Span << /ActualText (A) >> BDC", "/Span << /ActualText (A) >> BDC BT /F0 12 Tf"
            )
        elif change == "wrong-cid":
            source = source.replace(f"<{first_cid:04X}> Tj", "<FFFF> Tj", 1)
        elif change == "wrong-cid-to-gid":
            first["/DescendantFonts"][0].get_object()[NameObject("/CIDToGIDMap")] = NameObject("/Wrong")
        elif change in {"vertical-encoding", "custom-encoding", "wrong-type0", "wrong-descendant"}:
            if change == "wrong-type0":
                first[NameObject("/Subtype")] = NameObject("/TrueType")
            elif change == "wrong-descendant":
                first["/DescendantFonts"][0].get_object()[NameObject("/Subtype")] = NameObject("/CIDFontType0")
            else:
                first[NameObject("/Encoding")] = NameObject(
                    "/Identity-V" if change == "vertical-encoding" else "/Custom"
                )
        elif change == "unused-cmap-laundering":
            source = second_run
        elif change == "two-shows-in-override":
            source = source.replace(f"<{first_cid:04X}> Tj", f"<{first_cid:04X}> Tj <{first_cid:04X}> Tj", 1)
        elif change == "two-cids-in-override":
            source = source.replace(f"<{first_cid:04X}> Tj", f"<{first_cid:04X}{first_cid:04X}> Tj", 1)
        elif change == "removed-override":
            source = source.replace("/Span << /ActualText (A) >> BDC ", "").replace("EMC ", "")
        elif change == "reordered":
            source = second_run + "\n" + first_run
        elif change == "duplicate":
            source += "\n" + second_run
        elif change != "valid":
            raise AssertionError(change)
        contents = DecodedStreamObject()
        contents.set_data(source.encode("ascii"))
        page[NameObject("/Contents")] = writer._add_object(contents)
        encoded = io.BytesIO()
        writer.write(encoded)
        pdf.write_bytes(encoded.getvalue())
        update_bundle(root, root / "bundle.json")

    def qualify(self, root: Path) -> dict:
        return qualify_fonts(
            PdfReader(root / "document.pdf", strict=True),
            invobook_corpus.DEFAULT_CORPUS,
            "pliego",
            root / "scene.json",
            root / "bundle.json",
            root / "document.pdf",
            required_faces=set(self.sources()),
            source_fonts=self.sources(),
        )

    def test_actual_scoped_alias_preserves_original_font_closure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            test_bundle(root, self.sources())
            self.mutate(root, "valid")
            proof = self.qualify(root)
            self.assertEqual(proof["scopedActualTextOverrides"], 1)
            self.assertEqual(proof["paintedGlyphCount"], 2)
            self.assertEqual(proof["scenePdfGlyphMappings"], 2)

    def test_resealed_actual_use_corruptions_reject(self) -> None:
        for change in (
            "wrong-actual-text",
            "unscoped-actual-text",
            "wrong-cid",
            "wrong-cid-to-gid",
            "vertical-encoding",
            "custom-encoding",
            "wrong-type0",
            "wrong-descendant",
            "unused-cmap-laundering",
            "two-shows-in-override",
            "two-cids-in-override",
            "removed-override",
            "reordered",
            "duplicate",
        ):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                test_bundle(root, self.sources())
                self.mutate(root, change)
                with self.assertRaises(AssertionError):
                    self.qualify(root)


if __name__ == "__main__":
    unittest.main()
