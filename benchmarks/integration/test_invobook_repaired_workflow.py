"""Opt-in real-application regressions; no renderer, external HTTP or production DB.

Set PLIEGO_INVOBOOK_REPAIRED_APP and PLIEGO_INVOBOOK_PHP to installed, pinned
paths. The application must contain exactly invobook-currency.patch. Each test
owns a new evidence directory and the HTML action rolls back its invoice rows.
"""

from __future__ import annotations

import json
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
        self, template: str, name: str = "case", provider: str = "html"
    ) -> tuple[subprocess.CompletedProcess, dict]:
        output = self.root / name
        result = subprocess.run(
            [
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
            ],
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
