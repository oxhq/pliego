# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Separate Chrome encoding checks; optional retained PDF is read-only."""

import argparse
import io
from pathlib import Path
import unittest

from pypdf import PdfReader
from pypdf.generic import ContentStream, DecodedStreamObject, FloatObject, NameObject

from invobook_corpus import DEFAULT_CORPUS, FONT_SHA256
from invobook_fonts import qualify_chrome_fonts, unicode_map
from ledger_fonts import font_from_bytes

LEGACY_PDF = None


class ChromeFontTests(unittest.TestCase):
    def test_counted_scalar_unicode_maps(self) -> None:
        prefix = b"1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n"
        valid = prefix + b"1 beginbfchar\n<0003> <0020>\nendbfchar\n1 beginbfrange\n<0013> <0015> <0030>\nendbfrange"
        self.assertEqual(unicode_map(valid), {3: " ", 19: "0", 20: "1", 21: "2"})
        for bad in (
            valid.replace(b"<0013> <0015>", b"<0015> <0013>"),
            valid.replace(b"1 beginbfchar", b"2 beginbfchar"),
            valid.replace(b"<0020>", b"<D800>"),
            valid.replace(b"<0020>", b"<00660069>"),
            valid.replace(b"<0030>", b"[<0030> <0031> <0032>]"),
            valid.replace(b"<0013>", b"<0003>"),
            valid + b"\n/Other usecmap",
        ):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                unicode_map(bad)

    def test_original_and_twelve_retained_pdf_corruptions(self) -> None:
        if LEGACY_PDF is None:
            self.skipTest("Pass --legacy-pdf for retained Chrome PDF proof")
        sources = {name: (DEFAULT_CORPUS / "fonts" / name, sha) for name, sha in FONT_SHA256.items()}
        proof = qualify_chrome_fonts(PdfReader(LEGACY_PDF, strict=True), sources)
        self.assertEqual(proof["paintedGlyphMappings"], 89)
        for case in (
            "metric",
            "outline",
            "width",
            "unicode",
            "missing-map",
            "cid-map",
            "encoding",
            "unembedded",
            "face",
            "active-font",
            "unknown-operator",
            "gs-font",
        ):
            reader = PdfReader(LEGACY_PDF, strict=True)
            font = next(iter(reader.pages[0]["/Resources"]["/Font"].values())).get_object()
            descendant = font["/DescendantFonts"][0].get_object()
            descriptor = descendant["/FontDescriptor"].get_object()
            if case in {"metric", "outline"}:
                reduced = font_from_bytes(descriptor["/FontFile2"].get_object().get_data())
                if case == "metric":
                    width, bearing = reduced["hmtx"].metrics["A"]
                    reduced["hmtx"].metrics["A"] = (width + 1, bearing)
                else:
                    x, y = reduced["glyf"]["A"].coordinates[0]
                    reduced["glyf"]["A"].coordinates[0] = (x + 1, y)
                buffer = io.BytesIO()
                reduced.save(buffer)
                stream = DecodedStreamObject()
                stream.set_data(buffer.getvalue())
                descriptor[NameObject("/FontFile2")] = stream
            elif case == "width":
                descendant["/W"][1][3] = FloatObject(float(descendant["/W"][1][3]) + 1)
            elif case in {"unicode", "missing-map"}:
                raw = font["/ToUnicode"].get_object().get_data()
                raw = raw.replace(b"<002C> <0049>", b"<002C> <004A>" if case == "unicode" else b"", 1)
                if case == "missing-map":
                    raw = raw.replace(b"6 beginbfchar", b"5 beginbfchar", 1)
                stream = DecodedStreamObject()
                stream.set_data(raw)
                font[NameObject("/ToUnicode")] = stream
            elif case == "cid-map":
                descendant[NameObject("/CIDToGIDMap")] = NameObject("/Other")
            elif case == "encoding":
                font[NameObject("/Encoding")] = NameObject("/Identity-V")
            elif case == "unembedded":
                del descriptor[NameObject("/FontFile2")]
            elif case == "face":
                font[NameObject("/BaseFont")] = NameObject("/AAAAAA+DejaVuSans-Bold")
            elif case in {"active-font", "unknown-operator"}:
                content = ContentStream(reader.pages[0].get_contents(), reader)
                if case == "active-font":
                    next(arguments for arguments, op in content.operations if op == b"Tf")[0] = NameObject("/Missing")
                else:
                    content.operations.append(([], b"Do"))
                reader.pages[0][NameObject("/Contents")] = content
            else:
                state = next(iter(reader.pages[0]["/Resources"]["/ExtGState"].values())).get_object()
                state[NameObject("/Font")] = NameObject("/Unexpected")
            with self.subTest(case=case), self.assertRaises((AssertionError, ValueError)):
                qualify_chrome_fonts(reader, sources)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--legacy-pdf", type=Path)
    args, remaining = parser.parse_known_args()
    LEGACY_PDF = args.legacy_pdf
    unittest.main(argv=[__file__, *remaining])
