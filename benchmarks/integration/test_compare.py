# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Focused tests for honest comparison denominators and untimed PDF checks."""

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
from argparse import Namespace

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run_comparison import input_identity, load_fixture, markdown, node_preflight, oracle_command, summarize


def sample(provider: str, outcome: str = "correct", wall: float = 100, fixture: str = "invoice") -> dict:
    return {"fixture": fixture, "provider": provider, "outcome": outcome, "execution": {"processWallMs": wall}}


class ComparisonTests(unittest.TestCase):
    def test_failed_attempts_remain_in_denominator_and_suppress_ratio(self) -> None:
        samples = [
            sample("pliego"),
            sample("pliego", "render_failure", 1),
            sample("browsershot", wall=200),
            sample("browsershot", wall=200),
        ]
        result = summarize(samples)[0]
        self.assertEqual(result["providers"]["pliego"]["attempts"], 2)
        self.assertEqual(result["providers"]["pliego"]["notCorrectFraction"], 0.5)
        self.assertEqual(result["providers"]["pliego"]["correctOnlyProcessWallMs"]["p50"], 100)
        self.assertIsNone(result["legacyOverPliegoMedianRatio"])

    def test_every_failure_class_suppresses_ratio_for_either_provider(self) -> None:
        for outcome in (
            "unsupported",
            "incorrect_output",
            "setup_failure",
            "transport_failure",
            "storage_failure",
            "timeout",
            "oracle_failure",
            "input_changed",
        ):
            for provider in ("pliego", "browsershot"):
                with self.subTest(outcome=outcome, provider=provider):
                    samples = [sample("pliego"), sample("browsershot"), sample(provider, outcome)]
                    self.assertIsNone(summarize(samples)[0]["legacyOverPliegoMedianRatio"])

    def test_ratio_requires_both_providers(self) -> None:
        self.assertIsNone(summarize([sample("pliego")])[0]["legacyOverPliegoMedianRatio"])

    def test_bootstrap_probe_suppresses_ratio_even_when_both_pdfs_pass(self) -> None:
        legacy = sample("browsershot")
        legacy["adapter"] = {"dependencyPreflightWallMs": 50}
        result = summarize([sample("pliego"), legacy])[0]
        self.assertIsNone(result["legacyOverPliegoMedianRatio"])
        self.assertIn("visual/document acceptance", result["ratioNote"])

    def test_all_pdf_facts_pass_still_defers_ratio_and_retains_diagnostic_p95(self) -> None:
        result = summarize(
            [sample("pliego", wall=value) for value in (10, 20, 30)]
            + [sample("browsershot", wall=value) for value in (30, 40, 50)]
        )[0]
        self.assertIsNone(result["legacyOverPliegoMedianRatio"])
        self.assertIn("not established", result["acceptance"])
        self.assertEqual(result["providers"]["pliego"]["correctOnlyProcessWallMs"]["p95NearestRank"], 30)

    def test_document_families_are_not_pooled(self) -> None:
        result = summarize(
            [sample("pliego"), sample("browsershot", wall=200), sample("pliego", "render_failure", fixture="long")]
        )
        self.assertEqual(len(result), 2)
        self.assertEqual(result[0]["providers"]["pliego"]["attempts"], 1)
        self.assertIsNone(result[0]["legacyOverPliegoMedianRatio"])
        self.assertIsNone(result[1]["legacyOverPliegoMedianRatio"])

    def test_pdf_facts_are_passed_to_existing_oracle(self) -> None:
        command = oracle_command(
            Path("out.pdf"),
            {
                "expected": {
                    "textContains": ["Total 100.00"],
                    "fontFamilies": ["Nunito"],
                    "linkTargets": ["https://example.test/terms"],
                    "pageCount": 2,
                }
            },
        )
        self.assertIn("--text-contains", command)
        self.assertIn("Total 100.00", command)
        self.assertIn("--font-family", command)
        self.assertIn("--link-target", command)
        self.assertIn("--page-count", command)

    def test_report_states_visual_and_scope_boundary(self) -> None:
        text = markdown({"summary": summarize([sample("pliego", "render_failure"), sample("browsershot")])})
        self.assertIn("visual review", text)
        self.assertIn("no speed ratio", text)
        self.assertIn("not release", text)
        self.assertIn("PDF facts passed / attempts", text)
        self.assertNotIn("Correct / attempts", text)

    def test_missing_node_dependency_is_setup_failure_without_samples(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = node_preflight(Namespace(node=None, node_modules=None, timeout_seconds=30), root)
            self.assertEqual(result["status"], "setup_failure")
            self.assertTrue(result["untimed"])
            text = markdown({"summary": [], "setupFailure": "node_dependency_preflight"})
            self.assertIn("No samples were attempted", text)

    def test_node_preflight_uses_exact_node_and_module_environment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            node = root / "node.exe"
            node.touch()
            modules = root / "node_modules"
            (modules / "puppeteer").mkdir(parents=True)
            (modules / "puppeteer/package.json").write_text("{}")

            def run(command: list[str], output: Path, timeout: float, *, env: dict) -> dict:
                self.assertEqual(command[0], str(node.resolve()))
                self.assertEqual(env["NODE_PATH"], str(modules.resolve()))
                (output / "stdout.txt").write_text(json.dumps({"launch": "function", "node": "v25.9.0"}))
                return {"exitCode": 0, "timedOut": False}

            with patch("run_comparison.run_process", side_effect=run):
                result = node_preflight(Namespace(node=str(node), node_modules=str(modules), timeout_seconds=30), root)
            self.assertEqual(result["status"], "pass")
            self.assertEqual(result["identity"]["node"], "v25.9.0")

    def test_fixture_requires_real_text_expectations(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = {
                "schema": "pliego.application-fixture.v1",
                "id": "invoice",
                "assets": [],
                "expected": {"textContains": []},
            }
            (root / "fixture.json").write_text(json.dumps(metadata), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "textContains"):
                load_fixture(root)

    def test_asset_traversal_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "fixture"
            fixture.mkdir()
            for name in ("fixture.json", "input.html"):
                (fixture / name).write_text("fixture", encoding="utf-8")
            (root / "outside.css").write_text("css", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "escapes"):
                input_identity(fixture, {"assets": [{"path": "../outside.css"}]})


if __name__ == "__main__":
    unittest.main()
