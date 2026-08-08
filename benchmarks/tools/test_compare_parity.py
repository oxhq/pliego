import unittest
from unittest import mock

import compare_parity


class CorrectnessGateTest(unittest.TestCase):
    def test_missing_generated_fixture_is_fatal(self) -> None:
        fixture = {"input": "benchmarks/fixtures/__parity_missing__/input.html"}
        with mock.patch.object(compare_parity, "fail", side_effect=RuntimeError):
            with self.assertRaises(RuntimeError):
                compare_parity.check_prep("ledger-20-pages", fixture)

    def test_matching_failures_do_not_pass_a_success_fixture(self) -> None:
        fixture = {"correctness": {"page_count": 1, "text_contains": ["Minimal"]}}
        outcome = {
            "exit_code": 1,
            "status": "failed",
            "error_code": "SAME_FAILURE",
            "document_pdf_status": None,
            "page_count": 0,
            "text_contains": {"Minimal": False},
            "pdf_sha256": None,
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
            "pdf_sha256": None,
        }
        self.assertEqual(compare_parity.correctness_problems("fixture", fixture, outcome), [])
        outcome["pdf_sha256"] = "unexpected"
        self.assertTrue(compare_parity.correctness_problems("fixture", fixture, outcome))


if __name__ == "__main__":
    unittest.main()
