# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Synthetic font-boundary units, not actual native document qualification."""

from __future__ import annotations

import copy
import io
import json
from pathlib import Path
import shutil
import tempfile
from types import ModuleType
import unittest
from unittest.mock import patch

from fontTools import subset
from pypdf import PdfReader, PdfWriter
from pypdf.generic import ArrayObject, DecodedStreamObject, DictionaryObject, NameObject

import invobook_corpus
import ledger_fonts
import manufacturing_corpus
from manufacturing_fonts import layout_fingerprint, qualify_pdf_fonts


def test_bundle(root: Path, sources: dict[str, tuple[Path, str]]) -> tuple[Path, Path, Path]:
    """Minimal valid font objects with synthetic scene; never a renderer fixture."""
    writer = PdfWriter()
    page = writer.add_blank_page(width=595.28, height=841.89)
    fonts = DictionaryObject()
    operations = []
    painted_runs = []
    for index, (name, (source, _)) in enumerate(sources.items()):
        raw = source.read_bytes()
        resource = ledger_fonts.digest(raw)
        (root / "resources").mkdir(exist_ok=True)
        (root / "resources" / resource).write_bytes(raw)
        original = ledger_fonts.font_from_bytes(raw)
        reduced = copy.deepcopy(original)
        options = subset.Options()
        options.glyph_names = True
        options.notdef_outline = True
        options.drop_tables += ["FFTM"]
        job = subset.Subsetter(options=options)
        job.populate(text="A")
        job.subset(reduced)
        buffer = io.BytesIO()
        reduced.save(buffer)
        stream = DecodedStreamObject()
        stream.set_data(buffer.getvalue())
        descriptor = DictionaryObject({NameObject("/FontFile2"): writer._add_object(stream)})
        descendant = DictionaryObject(
            {
                NameObject("/Subtype"): NameObject("/CIDFontType2"),
                NameObject("/FontDescriptor"): writer._add_object(descriptor),
                NameObject("/BaseFont"): NameObject("/ABCDEF+" + name.removesuffix(".ttf")),
                NameObject("/CIDToGIDMap"): NameObject("/Identity"),
            }
        )
        mapping = DecodedStreamObject()
        mapping.set_data(f"1 beginbfchar\n<{reduced.getGlyphID('A'):04X}> <0041>\nendbfchar".encode())
        font = DictionaryObject(
            {
                NameObject("/Type"): NameObject("/Font"),
                NameObject("/Subtype"): NameObject("/Type0"),
                NameObject("/Encoding"): NameObject("/Identity-H"),
                NameObject("/ToUnicode"): writer._add_object(mapping),
                NameObject("/DescendantFonts"): ArrayObject([writer._add_object(descendant)]),
            }
        )
        fonts[NameObject(f"/F{index}")] = writer._add_object(font)
        painted_runs.append(f"BT /F{index} 12 Tf <{reduced.getGlyphID('A'):04X}> Tj ET")
        operations.append(
            {
                "type": "text",
                "text": "A",
                "font": {
                    "resource": "sha256:" + resource,
                    "face_index": 0,
                    "variations": [],
                    "synthetic_bold": False,
                },
                "glyphs": [{"id": original.getGlyphID("A"), "text_range": {"start": 0, "end": 1}}],
            }
        )
    page[NameObject("/Resources")] = DictionaryObject({NameObject("/Font"): fonts})
    contents = DecodedStreamObject()
    contents.set_data("\n".join(painted_runs).encode("ascii"))
    page[NameObject("/Contents")] = writer._add_object(contents)
    pdf = root / "document.pdf"
    writer.write(pdf)
    scene = root / "scene.json"
    scene.write_text(
        json.dumps({"schema": "pliego.document-scene", "version": 2, "pages": [{"operations": operations}]})
    )
    bundle = root / "bundle.json"
    update_bundle(root, bundle)
    return pdf, scene, bundle


def update_bundle(root: Path, bundle: Path) -> None:
    bundle.write_text(
        json.dumps(
            {
                "schema": "pliego.bundle-manifest",
                "version": 1,
                "entries": [
                    {
                        "path": path.relative_to(root).as_posix(),
                        "bytes": path.stat().st_size,
                        "sha256": "sha256:" + manufacturing_corpus.digest(path),
                    }
                    for path in sorted(root.rglob("*"))
                    if path.is_file() and path != bundle
                ],
            }
        )
    )


class TwoCorpusFontTests(unittest.TestCase):
    def sources(self, corpus_module: ModuleType) -> dict:
        folder = "resources" if corpus_module is manufacturing_corpus else "fonts"
        return {
            name: (corpus_module.DEFAULT_CORPUS / folder / name, sha) for name, sha in corpus_module.FONT_SHA256.items()
        }

    def test_callers_supply_exact_original_two_faces(self) -> None:
        for module in (manufacturing_corpus, invobook_corpus):
            module.load_corpus()
            sources = self.sources(module)
            with tempfile.TemporaryDirectory() as temporary:
                pdf, scene, bundle = test_bundle(Path(temporary), sources)
                proof = qualify_pdf_fonts(module.DEFAULT_CORPUS, sources, pdf, "pliego", scene, bundle)
                self.assertEqual(proof["outcome"], "passed")
                self.assertEqual({entry["source"] for entry in proof["embedded"]}, set(module.FONT_SHA256))
                self.assertEqual(proof["scenePdfGlyphMappings"], 2)

    def test_missing_and_wrong_scene_bundle_and_pdf(self) -> None:
        sources = self.sources(invobook_corpus)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pdf, scene, bundle = test_bundle(root, sources)
            for scene_arg, bundle_arg in (
                (None, bundle),
                (scene, None),
                (None, None),
                (root / "missing.json", bundle),
                (scene, root / "missing.json"),
            ):
                with (
                    self.subTest(scene=scene_arg, bundle=bundle_arg),
                    self.assertRaises((ValueError, AssertionError, OSError)),
                ):
                    qualify_pdf_fonts(invobook_corpus.DEFAULT_CORPUS, sources, pdf, "pliego", scene_arg, bundle_arg)
            original = scene.read_bytes()
            altered = json.loads(original)
            altered["pages"][0]["operations"][0]["text"] = "B"
            scene.write_text(json.dumps(altered))
            # Stale bundle hash fails, then a consistently rehashed wrong scene
            # must still fail against the real embedded Unicode/glyph mapping.
            for rehash in (False, True):
                if rehash:
                    update_bundle(root, bundle)
                with self.subTest(rehash=rehash), self.assertRaises(AssertionError):
                    qualify_pdf_fonts(invobook_corpus.DEFAULT_CORPUS, sources, pdf, "pliego", scene, bundle)
            scene.write_bytes(original)
            update_bundle(root, bundle)
            wrong_pdf = root / "other.pdf"
            wrong_pdf.write_bytes(pdf.read_bytes() + b"\n% changed identity\n")
            with self.assertRaises(AssertionError):
                qualify_pdf_fonts(invobook_corpus.DEFAULT_CORPUS, sources, wrong_pdf, "pliego", scene, bundle)

    def test_wrong_embedded_program_and_scene_resource(self) -> None:
        sources = self.sources(manufacturing_corpus)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pdf, scene, bundle = test_bundle(root, sources)
            original_pdf = pdf.read_bytes()
            reader = PdfReader(pdf, strict=True)
            font = next(iter(reader.pages[0]["/Resources"]["/Font"].values())).get_object()
            descriptor = font["/DescendantFonts"][0].get_object()["/FontDescriptor"].get_object()
            del descriptor[NameObject("/FontFile2")]
            writer = PdfWriter(clone_from=reader)
            writer.write(pdf)
            update_bundle(root, bundle)
            with self.assertRaisesRegex(AssertionError, "Unembedded"):
                qualify_pdf_fonts(manufacturing_corpus.DEFAULT_CORPUS, sources, pdf, "pliego", scene, bundle)
            pdf.write_bytes(original_pdf)
            reader = PdfReader(pdf, strict=True)
            font = next(iter(reader.pages[0]["/Resources"]["/Font"].values())).get_object()
            descriptor = font["/DescendantFonts"][0].get_object()["/FontDescriptor"].get_object()
            reduced = ledger_fonts.font_from_bytes(descriptor["/FontFile2"].get_object().get_data())
            width, bearing = reduced["hmtx"].metrics["A"]
            reduced["hmtx"].metrics["A"] = (width + 1, bearing)
            buffer = io.BytesIO()
            reduced.save(buffer)
            replacement = DecodedStreamObject()
            replacement.set_data(buffer.getvalue())
            descriptor[NameObject("/FontFile2")] = replacement
            writer = PdfWriter(clone_from=reader)
            writer.write(pdf)
            update_bundle(root, bundle)
            with self.assertRaisesRegex(AssertionError, "outline/metric"):
                qualify_pdf_fonts(manufacturing_corpus.DEFAULT_CORPUS, sources, pdf, "pliego", scene, bundle)
            pdf.write_bytes(original_pdf)
            altered = json.loads(scene.read_bytes())
            altered["pages"][0]["operations"][0]["font"]["resource"] = "sha256:" + "0" * 64
            scene.write_text(json.dumps(altered))
            update_bundle(root, bundle)
            with self.assertRaises((AssertionError, KeyError)):
                qualify_pdf_fonts(manufacturing_corpus.DEFAULT_CORPUS, sources, pdf, "pliego", scene, bundle)

    def test_source_replacement_is_not_hidden_by_parse_cache(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "DejaVuSans.ttf"
            raw = next(iter(self.sources(invobook_corpus).values()))[0].read_bytes()
            path.write_bytes(raw)
            sources = {"DejaVuSans.ttf": (path, manufacturing_corpus.digest(path))}
            with (
                patch("pypdf.PdfReader"),
                patch("invobook_fonts.qualify_chrome_fonts", return_value={"outcome": "passed"}),
            ):
                proof = qualify_pdf_fonts(invobook_corpus.DEFAULT_CORPUS, sources, path, "browsershot", None, None)
            self.assertEqual(proof["outcome"], "passed")
            path.write_bytes(raw + b"changed")
            with self.assertRaisesRegex(ValueError, "original font bytes changed"):
                qualify_pdf_fonts(invobook_corpus.DEFAULT_CORPUS, sources, path, "browsershot", None, None)

    def test_chromium_path_never_assumes_krilla_encoding(self) -> None:
        sources = self.sources(invobook_corpus)
        with (
            patch("ledger_fonts.qualify_fonts", side_effect=AssertionError("must not call Krilla")),
            patch("pypdf.PdfReader"),
            patch("invobook_fonts.qualify_chrome_fonts", return_value={"outcome": "passed"}) as chrome,
        ):
            proof = qualify_pdf_fonts(
                invobook_corpus.DEFAULT_CORPUS, sources, Path("unused.pdf"), "browsershot", None, None
            )
        self.assertEqual(proof["outcome"], "passed")
        self.assertEqual(chrome.call_args.args[1], sources)
        with self.assertRaises(ValueError):
            qualify_pdf_fonts(
                invobook_corpus.DEFAULT_CORPUS,
                sources,
                Path("unused.pdf"),
                "browsershot",
                Path("scene"),
                Path("bundle"),
            )

    def test_corrupt_source_cannot_self_rehash_manifest(self) -> None:
        for module in (manufacturing_corpus, invobook_corpus):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary) / "fixture"
                shutil.copytree(module.DEFAULT_CORPUS, root)
                manifest = json.loads((root / "manifest.json").read_bytes())
                font = next(item for item in manifest["files"] if item["path"].endswith("DejaVuSans.ttf"))
                path = root / font["path"]
                raw = path.read_bytes()
                path.write_bytes(raw[:-1] + bytes([raw[-1] ^ 1]))
                font["sha256"] = manufacturing_corpus.digest(path)
                (root / "manifest.json").write_text(json.dumps(manifest))
                with self.assertRaisesRegex(ValueError, "Original .* font changed"):
                    module.load_corpus(root)

    def test_layout_fingerprint_excludes_metadata_not_content(self) -> None:
        for module in (manufacturing_corpus, invobook_corpus):
            observed = json.loads((module.DEFAULT_CORPUS / "reference-observation.json").read_bytes())
            original = layout_fingerprint(observed)
            changed = copy.deepcopy(observed)
            changed.update(path="elsewhere", wallSeconds=999, pdfSha256="different")
            for font in changed["fonts"]:
                font["name"] = "ZYXWVU+" + font["name"].split("+")[-1]
                font["line"] = "different PDF object IDs"
            self.assertEqual(original, layout_fingerprint(changed))
            for field, value in (("text", "Wrong"), ("x0", 999)):
                changed = copy.deepcopy(observed)
                changed["pages"][0]["words"][0][field] = value
                self.assertNotEqual(original, layout_fingerprint(changed))
            changed = copy.deepcopy(observed)
            changed["fonts"][0]["name"] = "WrongFace"
            self.assertNotEqual(original, layout_fingerprint(changed))


if __name__ == "__main__":
    unittest.main()
