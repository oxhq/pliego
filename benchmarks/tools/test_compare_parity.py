import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import compare_parity


def successful_outcome() -> dict:
    return {
        "exit_code": 0,
        "status": "rendered",
        "error_code": None,
        "document_pdf_status": "rendered",
        "requested_output_exists": True,
        "document_pdf_reported": True,
        "document_pdf_matches_output": True,
        "page_count": 1,
        "text_contains": {"Minimal": True},
        "link_count": 0,
        "link_targets_sha256": compare_parity.canonical_sha256([]),
        "pdf_sha256": "pdf",
        "artifact_hashes": {
            "scene_artifact": "scene",
            "fonts_artifact": "fonts",
            "pdf_structure": "structure",
        },
    }


class CorrectnessGateTest(unittest.TestCase):
    def test_binary_identity_retains_hash_size_and_version(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "pliego"
            binary.write_bytes(b"candidate-bytes")
            completed = subprocess.CompletedProcess([str(binary), "--version"], 0, "pliego 0.1.1\n", "")
            with mock.patch.object(compare_parity.subprocess, "run", return_value=completed):
                identity = compare_parity.binary_identity(binary)
        self.assertEqual(identity["bytes"], 15)
        self.assertEqual(
            identity["sha256"],
            "e8b471c16cc972d5e5e5be6ae2f93fecaf4e6c3dacbe15334bbcfc0b9a9d8eec",
        )
        self.assertEqual(identity["version"], "pliego 0.1.1")

    def test_missing_generated_fixture_is_fatal(self) -> None:
        fixture = {"input": "benchmarks/fixtures/__parity_missing__/input.html"}
        with mock.patch.object(compare_parity, "fail", side_effect=RuntimeError):
            with self.assertRaises(RuntimeError):
                compare_parity.check_prep("ledger-20-pages", fixture)

    def test_fixture_identity_hashes_input_and_prepared_assets(self) -> None:
        with tempfile.TemporaryDirectory(dir=compare_parity.ROOT) as directory:
            root = Path(directory)
            input_path = root / "input.html"
            input_path.write_bytes(b"fixture")
            (root / "font.ttf").write_bytes(b"font")
            identity = compare_parity.fixture_identity(input_path, {"assets": ["font.ttf"]})
        self.assertEqual(identity["input_sha256"], hashlib.sha256(b"fixture").hexdigest())
        self.assertEqual(identity["assets"], {"font.ttf": hashlib.sha256(b"font").hexdigest()})

    def test_matching_failures_do_not_pass_a_success_fixture(self) -> None:
        fixture = {"correctness": {"page_count": 1, "text_contains": ["Minimal"]}}
        outcome = {
            "exit_code": 1,
            "status": "failed",
            "error_code": "SAME_FAILURE",
            "document_pdf_status": None,
            "requested_output_exists": False,
            "document_pdf_reported": False,
            "document_pdf_matches_output": None,
            "page_count": 0,
            "text_contains": {"Minimal": False},
            "pdf_sha256": None,
            "artifact_hashes": {},
        }
        self.assertTrue(compare_parity.correctness_problems("fixture", fixture, outcome))

    def test_expected_failure_requires_code_and_no_pdf(self) -> None:
        fixture = {
            "expect_failure": True,
            "correctness": {"failure_code": "EXPECTED", "pdf_published": False},
        }
        outcome = {
            "exit_code": 1,
            "status": "failed",
            "error_code": "EXPECTED",
            "requested_output_exists": False,
            "document_pdf_reported": False,
            "document_pdf_matches_output": None,
            "pdf_sha256": None,
        }
        self.assertEqual(compare_parity.correctness_problems("fixture", fixture, outcome), [])
        outcome["requested_output_exists"] = True
        self.assertTrue(compare_parity.correctness_problems("fixture", fixture, outcome))

    def test_requested_output_is_hashed_even_when_summary_reports_another_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "fixture"
            fixture.mkdir()
            input_path = fixture / "input.html"
            input_path.write_text("<p>fixture</p>", encoding="utf-8")
            reported = root / "reported.pdf"
            reported.write_bytes(b"reported")

            def fake_run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
                output = Path(command[command.index("--output") + 1])
                output.write_bytes(b"requested")
                summary = {"status": "rendered", "document_pdf": str(reported)}
                return subprocess.CompletedProcess(command, 0, json.dumps(summary) + "\n", "")

            with mock.patch.object(compare_parity.subprocess, "run", side_effect=fake_run):
                outcome = compare_parity.stable_outcome(root / "pliego", input_path, root / "run", {})

        self.assertEqual(outcome["pdf_sha256"], hashlib.sha256(b"requested").hexdigest())
        self.assertFalse(outcome["document_pdf_matches_output"])

    def test_success_requires_each_retained_artifact(self) -> None:
        fixture = {"correctness": {"page_count": 1, "text_contains": ["Minimal"]}}
        for missing in ("scene_artifact", "fonts_artifact", "pdf_structure"):
            with self.subTest(missing=missing):
                outcome = successful_outcome()
                del outcome["artifact_hashes"][missing]
                problems = compare_parity.correctness_problems("fixture", fixture, outcome)
                self.assertTrue(any(missing in problem for problem in problems))

    def test_success_requires_reported_pdf_to_match_requested_output(self) -> None:
        fixture = {"correctness": {"page_count": 1, "text_contains": ["Minimal"]}}
        outcome = successful_outcome()
        outcome["document_pdf_matches_output"] = False
        self.assertTrue(compare_parity.correctness_problems("fixture", fixture, outcome))

    def test_expected_link_target_is_bound_by_count_and_digest(self) -> None:
        target = "https://pliego.dev/docs"
        fixture = {
            "correctness": {
                "page_count": 1,
                "text_contains": ["Minimal"],
                "link_targets": [target],
            }
        }
        outcome = successful_outcome()
        outcome["link_count"] = 1
        outcome["link_targets_sha256"] = compare_parity.canonical_sha256([target])
        self.assertEqual(compare_parity.correctness_problems("fixture", fixture, outcome), [])
        outcome["link_targets_sha256"] = compare_parity.canonical_sha256(["https://example.invalid"])
        self.assertTrue(compare_parity.correctness_problems("fixture", fixture, outcome))


if __name__ == "__main__":
    unittest.main()
