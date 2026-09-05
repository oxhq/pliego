#!/usr/bin/env python3
"""Portable frozen-ledger oracle tests; optional real legacy PDF is read-only."""

from __future__ import annotations

import argparse
import ast
import copy
import io
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from fontTools import subset
from pypdf import PdfReader
from pypdf.generic import DecodedStreamObject, NameObject

import ledger_columns
import ledger_fonts
import ledger_oracle

FIXTURE = Path(__file__).with_name("ledger")
LEGACY_PDF = None


class LedgerTests(unittest.TestCase):
    def test_authoritative_checks_are_not_optimized_away(self) -> None:
        for name in ("ledger_checks.py", "ledger_columns.py", "ledger_fonts.py", "ledger_oracle.py"):
            tree = ast.parse(Path(__file__).with_name(name).read_text(encoding="utf-8"))
            self.assertFalse(any(isinstance(node, ast.Assert) for node in ast.walk(tree)), name)

    def test_optimized_helpers_and_corrupt_fixture_cli(self) -> None:
        selected = [
            "test_closed_inputs_and_independent_equations",
            "test_numeric_column_corruptions",
            "test_footer_counter_and_content_corruptions",
            "test_subset_outlines_metrics_and_wrong_face",
            "test_source_font_rechecks_bytes_before_cached_parse",
            "test_bundle_hash_and_path_corruptions",
        ]
        run = subprocess.run(
            [sys.executable, "-O", __file__, *(f"LedgerTests.{name}" for name in selected)],
            capture_output=True,
            timeout=45,
        )
        self.assertEqual(run.returncode, 0, run.stderr.decode(errors="replace"))
        with tempfile.TemporaryDirectory(prefix="ledger-optimized-test-") as temporary:
            root = Path(temporary)
            fixture = root / "fixture"
            shutil.copytree(FIXTURE, fixture)
            # Do not update its original closure after corrupting the HTML.
            html = fixture / "document.blade.html"
            html.write_bytes(html.read_bytes().replace(b"GL300-0001", b"WRONG-0001", 1))
            pdf = root / "not-reached.pdf"
            pdf.write_bytes(b"PDF parsing must not run after fixture corruption")
            output = root / "oracle"
            run = subprocess.run(
                [
                    sys.executable,
                    "-O",
                    str(Path(__file__).with_name("ledger_oracle.py")),
                    "--fixture",
                    str(fixture),
                    "--pdf",
                    str(pdf),
                    "--provider",
                    "dompdf",
                    "--output",
                    str(output),
                ],
                capture_output=True,
                timeout=15,
            )
            self.assertNotEqual(run.returncode, 0)
            self.assertIn(b"AssertionError", run.stderr)
            report = json.loads((output / "report.json").read_bytes())
            self.assertEqual(report["outcome"], "failed")
            self.assertFalse(report["visualQualified"] or report["benchmarkQualified"])

    def test_closed_inputs_and_independent_equations(self) -> None:
        ledger_oracle.verify_fixture(FIXTURE)
        manifest = json.loads((FIXTURE / "synthetic-input.json").read_bytes())
        primary = json.loads((FIXTURE / "primary-account.json").read_bytes())
        html = (FIXTURE / "document.blade.html").read_text(encoding="utf-8")
        rows, summaries = ledger_oracle.independent_inputs(manifest, primary, html)
        self.assertEqual(len(rows), 300)
        self.assertEqual(len(summaries), 3)
        for field in ("cents", "incomeDebitCents", "incomeCreditCents", "incomeRunningCents"):
            invalid = copy.deepcopy(manifest)
            invalid["entries"][14][field] += 1
            with self.subTest(field=field), self.assertRaises(AssertionError):
                ledger_oracle.independent_inputs(invalid, primary, html)
        for bad in (
            html.replace("GL300-0001", "GL300-0002", 1),
            html.replace("GL300-0001", "MISSING-ROW", 1),
            html.replace("GL300 Synthetic Customer", "WRONG PARTNER", 1),
            html.replace("03/01/2026", "03/02/2026", 1),
            html.replace("100.25", "100.26", 1),
            "41,287.51".join(html.rsplit("41,287.50", 1)),
        ):
            with self.assertRaises(AssertionError):
                ledger_oracle.independent_inputs(manifest, primary, bad)

    def test_numeric_column_corruptions(self) -> None:
        ledger_columns.self_test()

    def test_footer_counter_and_content_corruptions(self) -> None:
        valid = "Page 8\n" + ledger_oracle.FOOTER
        ledger_oracle.check_footer(valid, 8)
        for bad in (
            valid.replace("Page 8", "Page 7"),
            valid.replace("Page 8", ""),
            valid + " Page 8",
            valid.replace("12:00", "12:01"),
            valid + ledger_oracle.FOOTER,
        ):
            with self.assertRaises(AssertionError):
                ledger_oracle.check_footer(bad, 8)

    def test_font_repaired_bbox_is_not_a_changed_outline(self) -> None:
        font = ledger_fonts.font_from_bytes((FIXTURE / "resources/DejaVuSans.ttf").read_bytes())
        original = ledger_fonts.semantic_font_identity(font, ledger_fonts.glyph_fingerprints(font))
        font["glyf"]["uni2D00"].xMin -= 42
        self.assertEqual(original, ledger_fonts.semantic_font_identity(font, ledger_fonts.glyph_fingerprints(font)))
        font["glyf"]["H"].coordinates[0] = (
            font["glyf"]["H"].coordinates[0][0] + 1,
            font["glyf"]["H"].coordinates[0][1],
        )
        self.assertNotEqual(original, ledger_fonts.semantic_font_identity(font, ledger_fonts.glyph_fingerprints(font)))

    def test_source_font_rechecks_bytes_before_cached_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ledger-font-cache-") as temporary:
            path = Path(temporary) / "font.ttf"
            raw = (FIXTURE / "resources/DejaVuSans.ttf").read_bytes()
            path.write_bytes(raw)
            identity = ledger_fonts.digest(raw)
            ledger_fonts.source_font(str(path), identity)
            path.write_bytes(raw[:-1] + bytes([raw[-1] ^ 1]))
            with self.assertRaisesRegex(AssertionError, "byte identity"):
                ledger_fonts.source_font(str(path), identity)

    def test_subset_outlines_metrics_and_wrong_face(self) -> None:
        original = ledger_fonts.font_from_bytes((FIXTURE / "resources/DejaVuSans.ttf").read_bytes())
        source = ledger_fonts.glyph_fingerprints(original)
        reduced = copy.deepcopy(original)
        options = subset.Options()
        options.glyph_names = True
        options.notdef_outline = True
        options.drop_tables += ["FFTM"]
        job = subset.Subsetter(options=options)
        job.populate(text="Ledger 123.45")
        job.subset(reduced)
        raw = io.BytesIO()
        reduced.save(raw)
        source_record = ledger_fonts.source_font(
            str(FIXTURE / "resources/DejaVuSans.ttf"),
            ledger_fonts.digest((FIXTURE / "resources/DejaVuSans.ttf").read_bytes()),
        )
        source_name, _ = ledger_fonts.match_subset_program(
            raw.getvalue(), {"DejaVuSans.ttf": source_record}, {"DejaVuSans.ttf"}
        )
        self.assertEqual(source_name, "DejaVuSans.ttf")
        with self.assertRaises(AssertionError):
            ledger_fonts.match_subset_program(raw.getvalue(), {"DejaVuSans.ttf": source_record}, {"Other.ttf"})
        reduced = ledger_fonts.font_from_bytes(raw.getvalue())
        glyphs = ledger_fonts.glyph_fingerprints(reduced)
        self.assertTrue(all(source[name] == value for name, value in glyphs.items()))
        advance, bearing = reduced["hmtx"].metrics["L"]
        reduced["hmtx"].metrics["L"] = (advance + 1, bearing)
        self.assertNotEqual(source["L"], ledger_fonts.glyph_fingerprints(reduced)["L"])
        changed = io.BytesIO()
        reduced.save(changed)
        with self.assertRaises(AssertionError):
            ledger_fonts.match_subset_program(changed.getvalue(), {"DejaVuSans.ttf": source_record}, {"DejaVuSans.ttf"})
        bold = ledger_fonts.font_from_bytes((FIXTURE / "resources/DejaVuSans-Bold.ttf").read_bytes())
        self.assertNotEqual(source["L"], ledger_fonts.glyph_fingerprints(bold)["L"])

    def test_bundle_hash_and_path_corruptions(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ledger-bundle-test-") as temporary:
            root = Path(temporary)
            pdf, scene = root / "document.pdf", root / "scene.json"
            pdf.write_bytes(b"unit-test-only-not-a-pdf")
            scene.write_text(json.dumps({"schema": "pliego.document-scene", "version": 2, "pages": []}))
            entries = [
                {"path": p.name, "bytes": p.stat().st_size, "sha256": "sha256:" + ledger_oracle.sha256(p)}
                for p in (pdf, scene)
            ]
            bundle = {"schema": "pliego.bundle-manifest", "version": 1, "entries": entries}
            manifest = root / "bundle.json"
            manifest.write_text(json.dumps(bundle))
            ledger_fonts.checked_bundle(manifest, scene, pdf)
            for bad in ("hash", "escape", "duplicate"):
                invalid = copy.deepcopy(bundle)
                if bad == "hash":
                    invalid["entries"][0]["sha256"] = "sha256:" + "0" * 64
                elif bad == "escape":
                    invalid["entries"][0]["path"] = "../document.pdf"
                else:
                    invalid["entries"].append(invalid["entries"][0])
                manifest.write_text(json.dumps(invalid))
                with self.subTest(bad=bad), self.assertRaises(AssertionError):
                    ledger_fonts.checked_bundle(manifest, scene, pdf)
            for name in (
                "",
                "../document.pdf",
                "/document.pdf",
                "C:/document.pdf",
                "C:\\document.pdf",
                "\\\\server\\share\\document.pdf",
                "./document.pdf",
                "resources//font.ttf",
                "resources/../font.ttf",
                "NUL",
                "resources/CON.ttf",
                "document.pdf.",
                "document.pdf:stream",
                "resources/ font.ttf",
                "document.pdf\0",
            ):
                invalid = copy.deepcopy(bundle)
                invalid["entries"][0]["path"] = name
                manifest.write_text(json.dumps(invalid))
                with self.subTest(path=name), self.assertRaises(AssertionError):
                    ledger_fonts.checked_bundle(manifest, scene, pdf)
            alias = root / "SCENE.JSON"
            if not alias.exists():
                alias.write_bytes(scene.read_bytes())
            invalid = copy.deepcopy(bundle)
            invalid["entries"].append(dict(entries[1], path="SCENE.JSON"))
            manifest.write_text(json.dumps(invalid))
            with self.assertRaisesRegex(AssertionError, "case-aliased"):
                ledger_fonts.checked_bundle(manifest, scene, pdf)

    def test_bundle_lexical_symlinks_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ledger-bundle-links-") as temporary:
            base = Path(temporary)
            root = base / "bundle"
            root.mkdir()
            pdf, scene, manifest = (root / name for name in ("document.pdf", "scene.json", "bundle.json"))
            pdf.write_bytes(b"unit-test-only-not-a-pdf")
            scene.write_text(json.dumps({"schema": "pliego.document-scene", "version": 2, "pages": []}))
            entries = [
                {"path": p.name, "bytes": p.stat().st_size, "sha256": "sha256:" + ledger_oracle.sha256(p)}
                for p in (pdf, scene)
            ]
            bundle = {"schema": "pliego.bundle-manifest", "version": 1, "entries": entries}
            manifest.write_text(json.dumps(bundle))
            outside = base / "outside"
            outside.mkdir()
            font = outside / "font.ttf"
            font.write_bytes(b"hash-correct but symlinked artifact")
            link = root / "font.ttf"
            try:
                link.symlink_to(font)
            except OSError as error:
                self.skipTest(f"Host cannot create test symlinks: {error}")
            linked = copy.deepcopy(bundle)
            linked["entries"].append(
                {"path": "font.ttf", "bytes": font.stat().st_size, "sha256": "sha256:" + ledger_oracle.sha256(font)}
            )
            manifest.write_text(json.dumps(linked))
            with self.assertRaisesRegex(AssertionError, "[Ll]ink|[Rr]eparse"):
                ledger_fonts.checked_bundle(manifest, scene, pdf)
            (root / "resources").symlink_to(outside, target_is_directory=True)
            linked["entries"][-1]["path"] = "resources/font.ttf"
            manifest.write_text(json.dumps(linked))
            with self.assertRaisesRegex(AssertionError, "[Ll]ink|[Rr]eparse"):
                ledger_fonts.checked_bundle(manifest, scene, pdf)
            manifest.write_text(json.dumps(bundle))
            (base / "alias").symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(AssertionError, "[Ll]ink|[Rr]eparse"):
                ledger_fonts.checked_bundle(base / "alias/bundle.json", scene, pdf)
            (root / "manifest-link.json").symlink_to(manifest)
            with self.assertRaisesRegex(AssertionError, "[Ll]ink|[Rr]eparse"):
                ledger_fonts.checked_bundle(root / "manifest-link.json", scene, pdf)
            (root / "scene-link.json").symlink_to(scene)
            with self.assertRaisesRegex(AssertionError, "[Ll]ink|[Rr]eparse"):
                ledger_fonts.checked_bundle(manifest, root / "scene-link.json", pdf)

    def test_real_legacy_and_foreign_embedded_font(self) -> None:
        if LEGACY_PDF is None:
            self.skipTest("Pass --legacy-pdf for retained original-output proof")
        report, _ = ledger_oracle.qualify(FIXTURE, LEGACY_PDF, "dompdf")
        self.assertEqual(report["detailRows"], 300)
        self.assertEqual(len(report["pages"]), 15)
        reader = PdfReader(LEGACY_PDF, strict=True)
        assets = json.loads((FIXTURE / "font-closure.json").read_bytes())["assets"]
        source_fonts = {a["file"]: (FIXTURE / "resources" / a["file"], a["sha256"]) for a in assets}
        explicit = ledger_fonts.qualify_fonts(
            reader,
            Path("unused-fixture-path"),
            "dompdf",
            None,
            None,
            LEGACY_PDF,
            required_faces=ledger_fonts.USED_FACES,
            source_fonts=source_fonts,
        )
        self.assertEqual(explicit["embedded"], report["fonts"]["embedded"])
        wrong_sources = dict(source_fonts)
        wrong_sources["DejaVuSans.ttf"] = (source_fonts["DejaVuSans.ttf"][0], "0" * 64)
        with self.assertRaisesRegex(AssertionError, "byte identity"):
            ledger_fonts.qualify_fonts(reader, FIXTURE, "dompdf", None, None, LEGACY_PDF, source_fonts=wrong_sources)
        with self.assertRaises(AssertionError):
            ledger_fonts.qualify_fonts(
                reader, FIXTURE, "dompdf", None, None, LEGACY_PDF, required_faces={"DejaVuSans.ttf"}
            )
        with self.assertRaises(AssertionError):
            ledger_fonts.qualify_fonts(reader, FIXTURE, "browsershot", None, None, LEGACY_PDF)
        font = next(iter(reader.pages[0]["/Resources"]["/Font"].values())).get_object()
        descendant = font.get("/DescendantFonts", [font])[0].get_object()
        descriptor = descendant["/FontDescriptor"].get_object()
        bad = DecodedStreamObject()
        bad.set_data(b"not the cryptographically bound original font")
        descriptor[NameObject("/FontFile2")] = bad
        with self.assertRaises(AssertionError):
            ledger_fonts.qualify_fonts(reader, FIXTURE, "dompdf", None, None, LEGACY_PDF)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--legacy-pdf", type=Path)
    arguments, remaining = parser.parse_known_args()
    LEGACY_PDF = arguments.legacy_pdf.resolve(strict=True) if arguments.legacy_pdf else None
    unittest.main(argv=[__file__, *remaining])
