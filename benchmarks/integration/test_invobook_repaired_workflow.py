# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Opt-in real-application regressions; no renderer, external HTTP or production DB.

Set PLIEGO_INVOBOOK_REPAIRED_APP and PLIEGO_INVOBOOK_PHP to installed, pinned
paths. The application must contain exactly invobook-currency.patch. Each test
owns a new evidence directory and the HTML action rolls back its invoice rows.
"""

from __future__ import annotations

import json
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("invobook_repaired_workflow.php")
APP = os.environ.get("PLIEGO_INVOBOOK_REPAIRED_APP")
PHP = os.environ.get("PLIEGO_INVOBOOK_PHP")


@unittest.skipUnless(APP and PHP, "set explicit installed Invobook and PHP paths for real-application tests")
class RepairedWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="pliego-invobook-action-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def invoke(
        self,
        template: str,
        name: str = "case",
        provider: str = "html",
        *,
        repair_media: bool = False,
        repair_full: bool = False,
    ) -> tuple[subprocess.CompletedProcess, dict]:
        output = self.root / name
        command = [
            str(PHP),
            str(SCRIPT),
            "--app",
            str(APP),
            "--output",
            str(output),
            "--template",
            template,
            "--provider",
            provider,
        ]
        if repair_media:
            command += ["--simple-pdf-media", "all"]
        if repair_full:
            command += ["--simple-pdf-repair", "quantity-fonts"]
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=30,
        )
        report = output / "report.json"
        self.assertTrue(report.is_file(), result.stdout + result.stderr)
        return result, json.loads(report.read_text(encoding="utf-8"))

    def assert_rolled_back(self, report: dict) -> None:
        self.assertEqual(report["persistedInvoiceCount"], 0)
        self.assertEqual(report["persistedInvoiceItemCount"], 0)
        self.assertTrue(report["sourceFilesUnchangedDuringRun"])
        self.assertFalse(report["performanceQualified"])

    def test_simple_action_preserves_totals_currency_and_deterministic_html(self) -> None:
        first, first_report = self.invoke("simple", "first")
        second, second_report = self.invoke("simple", "second")
        for result, report in [(first, first_report), (second, second_report)]:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(report["status"], "html_action_passed")
            self.assertEqual(
                report["databaseBeforeDelivery"],
                {
                    "subtotal_in_cents": 37500,
                    "vat_in_cents": 7500,
                    "total_in_cents": 45000,
                },
            )
            self.assert_rolled_back(report)
        self.assertEqual(first_report["inputSha256"], second_report["inputSha256"])
        self.assertEqual((self.root / "first/input.html").read_bytes(), (self.root / "second/input.html").read_bytes())

    def test_simple_media_override_changes_only_the_allowlisted_attribute(self) -> None:
        original_result, original = self.invoke("simple", "original")
        first_result, first = self.invoke("simple", "all-first", repair_media=True)
        second_result, second = self.invoke("simple", "all-second", repair_media=True)
        for result, report in [(original_result, original), (first_result, first), (second_result, second)]:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assert_rolled_back(report)
        before = b'<style type="text/css" media="screen">'
        after = b'<style type="text/css" media="all">'
        original_html = (self.root / "original/input.html").read_bytes()
        repaired_html = (self.root / "all-first/input.html").read_bytes()
        self.assertEqual(original_html.count(before), 1)
        self.assertEqual(original_html.replace(before, after), repaired_html)
        self.assertEqual(first["inputSha256"], second["inputSha256"])
        self.assertNotEqual(first["inputSha256"], original["inputSha256"])
        self.assertEqual(first["track"], "shared-currency-and-simple-media-repair-html-delivery")
        self.assertTrue(first["provenance"]["templateChanged"])
        self.assertFalse(first["provenance"]["generateActionChanged"])
        repair = first["provenance"]["templateRepair"]
        self.assertTrue(repair["applicationTemplateUnchanged"])
        self.assertEqual(repair["replacementCount"], 1)
        source = (self.root / "all-first/template.original.blade.php").read_bytes()
        target = (self.root / "all-first/template-override/templates/simple.blade.php").read_bytes()
        self.assertEqual(source.replace(before, after), target)
        self.assertEqual(hashlib.sha256(source).hexdigest(), repair["originalSha256"])
        self.assertEqual(hashlib.sha256(target).hexdigest(), repair["repairedSha256"])
        patch = (self.root / "all-first/template-repair.patch").read_bytes()
        self.assertEqual(hashlib.sha256(patch).hexdigest(), repair["patchSha256"])

    def test_media_repair_rejects_non_simple_templates(self) -> None:
        for template, option, value in (
            ("default", "--simple-pdf-media", "all"),
            ("elegant", "--simple-pdf-media", "all"),
            ("default", "--simple-pdf-repair", "quantity-fonts"),
            ("elegant", "--simple-pdf-repair", "quantity-fonts"),
        ):
            result = subprocess.run(
                [
                    str(PHP),
                    str(SCRIPT),
                    "--app",
                    str(APP),
                    "--output",
                    str(self.root / template),
                    "--template",
                    template,
                    "--provider",
                    "html",
                    option,
                    value,
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=30,
            )
            self.assertEqual(result.returncode, 1)
            report = json.loads(result.stdout)
            self.assertEqual(report["status"], "setup_failure")
            self.assertIn("allowed only for the simple template", report["error"]["message"])
            self.assertFalse((self.root / template).exists())

    def test_quantity_font_repair_preserves_original_action_and_closes_resources(self) -> None:
        first_result, first = self.invoke("simple", "full-first", repair_full=True)
        second_result, second = self.invoke("simple", "full-second", repair_full=True)
        for result, report in [(first_result, first), (second_result, second)]:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assert_rolled_back(report)
            self.assertEqual(report["track"], "shared-currency-media-quantity-font-repair-html-delivery")
            self.assertTrue(report["provenance"]["generateActionChanged"])
            self.assertEqual(
                report["invoiceItemFacts"],
                [
                    {"title": "Fixture consultation", "quantity": 1, "unitPrice": 125, "subtotal": 125},
                    {"title": "Fixture implementation", "quantity": 2, "unitPrice": 125, "subtotal": 250},
                ],
            )
            self.assertEqual(report["databaseBeforeDelivery"]["total_in_cents"], 45000)
        self.assertEqual(first["inputSha256"], second["inputSha256"])
        root = self.root / "full-first"
        source = (root / "action.original.php").read_bytes()
        target = (root / "action-override/GenerateInvoicePdf.php").read_bytes()
        before = b"                    ->pricePerUnit($item->rate_in_cents / 100)"
        addition = b"                    ->quantity($item->total_duration / 3600)"
        newline = b"\r\n" if b"\r\n" in source else b"\n"
        self.assertEqual(source.count(before), 1)
        self.assertEqual(source.replace(before, before + newline + addition), target)
        for patch_name in ("action-repair.patch", "template-repair.patch"):
            patch_check = subprocess.run(
                ["git", "-C", str(APP), "apply", "--check", "--unidiff-zero", str(root / patch_name)],
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(patch_check.returncode, 0, patch_check.stdout + patch_check.stderr)
        repair = first["provenance"]["actionRepair"]
        self.assertTrue(repair["applicationActionUnchanged"])
        self.assertEqual(repair["replacementCount"], 1)
        for bytes_, field in [
            (source, "originalSha256"),
            (target, "repairedSha256"),
            ((root / "action-repair.patch").read_bytes(), "patchSha256"),
        ]:
            self.assertEqual(hashlib.sha256(bytes_).hexdigest(), repair[field])
        fonts = first["provenance"]["fontRepair"]["fonts"]
        self.assertEqual(len(fonts), 2)
        for font in fonts:
            self.assertEqual(font["version"], "Version 2.37")
            file = root / font["path"]
            self.assertEqual(hashlib.sha256(file.read_bytes()).hexdigest(), font["sha256"])
            self.assertEqual(file.read_bytes(), (Path(str(APP)) / font["sourcePath"]).read_bytes())
            notice = file.with_name(file.name + ".LICENSE.txt").read_bytes()
            self.assertEqual(hashlib.sha256(notice).hexdigest(), font["licenseSha256"])
            self.assertIn(b"Bitstream", notice)
            self.assertIn(b"Arev", notice)
        original_template = (root / "template.original.blade.php").read_bytes()
        repaired_template = (root / "template-override/templates/simple.blade.php").read_bytes()
        template_newline = b"\r\n" if b"\r\n" in original_template else b"\n"
        rules = [rule.encode() for rule in first["provenance"]["templateRepair"]["fontFaceRules"]]
        self.assertEqual(len(rules), 2)
        replacement = b'<style type="text/css" media="all">' + template_newline + template_newline.join(rules)
        self.assertEqual(
            original_template.replace(b'<style type="text/css" media="screen">', replacement), repaired_template
        )

    def test_default_action_is_not_silently_replaced(self) -> None:
        result, report = self.invoke("default")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(report["status"], "html_action_passed")
        self.assertFalse(report["provenance"]["templateChanged"])
        self.assertFalse(report["provenance"]["generateActionChanged"])
        self.assertIn("fonts.googleapis.com", (self.root / "case/input.html").read_text(encoding="utf-8"))
        self.assert_rolled_back(report)

    def test_elegant_contract_failure_rolls_back(self) -> None:
        result, report = self.invoke("elegant")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(report["failurePhase"], "view_render")
        self.assertIn("issued_on", report["error"]["message"])
        self.assert_rolled_back(report)

    def test_existing_evidence_is_not_overwritten(self) -> None:
        self.invoke("simple")
        path = self.root / "case/report.json"
        before = path.read_bytes()
        result, _ = self.invoke("simple")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(path.read_bytes(), before)
        self.assertIn("Output must be a fresh directory", result.stdout)


if __name__ == "__main__":
    unittest.main()
