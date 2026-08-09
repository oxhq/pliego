import hashlib
import json
import shutil
import subprocess
import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path
from unittest import mock

import compare_parity

PDF = b"%PDF-1.7\nfixture\n%%EOF\n"


def successful_outcome() -> dict:
    pdf_hash = hashlib.sha256(PDF).hexdigest()
    return {
        "exit_code": 0,
        "status": "rendered",
        "error_code": None,
        "render_id": "sha256:render",
        "artifacts_reported": True,
        "artifacts_match_requested": True,
        "artifact_path_matches": {
            "scene_artifact": True,
            "fonts_artifact": True,
            "pdf_structure": True,
        },
        "artifact_json_valid": {
            "scene_artifact": True,
            "fonts_artifact": True,
            "pdf_structure": True,
        },
        "scene_schema": "pliego.document-scene",
        "scene_version": 1,
        "scene_hash": "sha256:scene",
        "scene_capture_status": "complete",
        "scene_capture_code": None,
        "scene_preview_status": "rendered",
        "scene_unsupported_event_count": 0,
        "scene_text_mapping_gap_count": 0,
        "scene_artifact_schema": "pliego.document-scene",
        "scene_artifact_version": 1,
        "scene_artifact_page_count": 1,
        "fonts_artifact_schema": "pliego.font-report",
        "fonts_artifact_version": 1,
        "document_pdf_status": "rendered",
        "pdf_structure_status": "rendered",
        "requested_output_exists": True,
        "requested_output_is_regular": True,
        "document_pdf_reported": True,
        "document_pdf_matches_output": True,
        "pdf_envelope_valid": True,
        "retained_pdf_bytes": len(PDF),
        "pdf_structure_schema": "pliego.pdf-structure",
        "pdf_structure_version": 1,
        "pdf_structure_pdf_artifact": "document.pdf",
        "pdf_structure_pdf_sha256": f"sha256:{pdf_hash}",
        "pdf_structure_pdf_bytes": len(PDF),
        "pdf_structure_pages_count": 1,
        "summary_preview_page_count": 1,
        "page_count": 1,
        "text_contains": {"Minimal": True},
        "link_count": 0,
        "link_targets_sha256": compare_parity.canonical_sha256([]),
        "pdf_sha256": pdf_hash,
        "pdf_bytes": len(PDF),
        "artifact_hashes": {
            "scene_artifact": "scene",
            "fonts_artifact": "fonts",
            "pdf_structure": "structure",
            "document_pdf_artifact": pdf_hash,
        },
    }


def rendered_outcome(mutate: Callable[[Path, Path, Path, dict], None] | None = None) -> dict:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        fixture = root / "fixture"
        fixture.mkdir()
        input_path = fixture / "input.html"
        input_path.write_text("<p>fixture</p>", encoding="utf-8")

        def fake_run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            output = Path(command[command.index("--output") + 1])
            artifacts = Path(command[command.index("--artifacts") + 1])
            artifacts.mkdir()
            output.write_bytes(PDF)
            (artifacts / "document.pdf").write_bytes(PDF)
            scene = {"schema": "pliego.document-scene", "version": 1, "pages": [{"operations": []}]}
            scene_bytes = json.dumps(scene, separators=(",", ":")).encode()
            (artifacts / "scene.json").write_bytes(scene_bytes)
            (artifacts / "fonts.json").write_text(
                json.dumps({"schema": "pliego.font-report", "version": 1}), encoding="utf-8"
            )
            structure = {
                "schema": "pliego.pdf-structure",
                "version": 1,
                "pdf": {
                    "artifact": "document.pdf",
                    "sha256": f"sha256:{hashlib.sha256(PDF).hexdigest()}",
                    "bytes": len(PDF),
                },
                "page_count": 1,
                "pages": [{"expected_extracted_unicode": "Minimal"}],
            }
            (artifacts / "pdf-structure.json").write_text(json.dumps(structure), encoding="utf-8")
            summary = {
                "status": "rendered",
                "render_id": "sha256:render",
                "artifacts": str(artifacts),
                "document_pdf": str(output),
                "document_pdf_status": "rendered",
                "pdf_structure_status": "rendered",
                "scene_artifact": str(artifacts / "scene.json"),
                "fonts_artifact": str(artifacts / "fonts.json"),
                "pdf_structure": str(artifacts / "pdf-structure.json"),
                "scene_previews": [str(artifacts / "scene-preview.png")],
                "scene": {
                    "schema": "pliego.document-scene",
                    "version": 1,
                    "hash": f"sha256:{hashlib.sha256(scene_bytes).hexdigest()}",
                    "capture_status": "complete",
                    "capture_code": None,
                    "preview_status": "rendered",
                    "unsupported_event_count": 0,
                    "text_mapping_gap_count": 0,
                },
            }
            if mutate is not None:
                mutate(root, output, artifacts, summary)
            return subprocess.CompletedProcess(command, 0, json.dumps(summary) + "\n", "")

        with mock.patch.object(compare_parity.subprocess, "run", side_effect=fake_run):
            return compare_parity.stable_outcome(
                root / "pliego", input_path, root / "run", {"text_contains": ["Minimal"]}
            )


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

    def test_linux_symlink_loop_is_invalid_path_evidence(self) -> None:
        with mock.patch.object(Path, "resolve", side_effect=RuntimeError("Symlink loop from '/tmp/a'")):
            self.assertIsNone(compare_parity.reported_path("/tmp/a", Path("/tmp")))

    def test_matching_failures_do_not_pass_a_success_fixture(self) -> None:
        fixture = {"correctness": {"page_count": 1, "text_contains": ["Minimal"]}}
        outcome = successful_outcome()
        outcome.update(exit_code=1, status="failed", error_code="SAME_FAILURE")
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
            "render_id": "sha256:render",
            "artifacts_reported": True,
            "artifacts_match_requested": True,
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

    def test_exact_artifact_contract_is_accepted(self) -> None:
        outcome = rendered_outcome()
        fixture = {"correctness": {"page_count": 1, "text_contains": ["Minimal"]}}
        self.assertEqual(compare_parity.correctness_problems("fixture", fixture, outcome), [])

    def test_external_artifact_root_and_files_are_rejected(self) -> None:
        def external(root: Path, _output: Path, artifacts: Path, summary: dict) -> None:
            external_root = root / "external"
            shutil.copytree(artifacts, external_root)
            summary["artifacts"] = str(external_root)
            for key, filename in compare_parity.ARTIFACT_FILES.items():
                summary[key] = str(external_root / filename)

        outcome = rendered_outcome(external)
        problems = compare_parity.correctness_problems("fixture", {"correctness": {"page_count": 1}}, outcome)
        self.assertTrue(any("requested artifact root" in problem for problem in problems))
        self.assertTrue(any("artifact path mismatch" in problem for problem in problems))

    def test_mismatched_scene_summary_hash_is_rejected(self) -> None:
        def mismatch(_root: Path, _output: Path, _artifacts: Path, summary: dict) -> None:
            summary["scene"]["hash"] = "sha256:not-the-scene"

        outcome = rendered_outcome(mismatch)
        problems = compare_parity.correctness_problems("fixture", {"correctness": {"page_count": 1}}, outcome)
        self.assertTrue(any("exact scene artifact" in problem for problem in problems))

    def test_invalid_fonts_json_is_a_retained_correctness_failure(self) -> None:
        def invalid(_root: Path, _output: Path, artifacts: Path, _summary: dict) -> None:
            (artifacts / "fonts.json").write_text("{", encoding="utf-8")

        outcome = rendered_outcome(invalid)
        problems = compare_parity.correctness_problems("fixture", {"correctness": {"page_count": 1}}, outcome)
        self.assertFalse(outcome["artifact_json_valid"]["fonts_artifact"])
        self.assertTrue(any("invalid artifact JSON" in problem for problem in problems))

    def test_pdf_structure_pointing_to_other_bytes_is_rejected(self) -> None:
        def other_bytes(_root: Path, _output: Path, artifacts: Path, _summary: dict) -> None:
            other = b"%PDF-1.7\nother\n%%EOF\n"
            (artifacts / "other.pdf").write_bytes(other)
            structure = json.loads((artifacts / "pdf-structure.json").read_text(encoding="utf-8"))
            structure["pdf"] = {
                "artifact": "other.pdf",
                "sha256": f"sha256:{hashlib.sha256(other).hexdigest()}",
                "bytes": len(other),
            }
            (artifacts / "pdf-structure.json").write_text(json.dumps(structure), encoding="utf-8")

        outcome = rendered_outcome(other_bytes)
        problems = compare_parity.correctness_problems("fixture", {"correctness": {"page_count": 1}}, outcome)
        self.assertTrue(any("exact requested and retained PDF bytes" in problem for problem in problems))

    def test_non_pdf_bytes_are_rejected_even_when_hashes_match(self) -> None:
        def non_pdf(_root: Path, output: Path, artifacts: Path, _summary: dict) -> None:
            payload = b"requested"
            output.write_bytes(payload)
            (artifacts / "document.pdf").write_bytes(payload)
            structure = json.loads((artifacts / "pdf-structure.json").read_text(encoding="utf-8"))
            structure["pdf"]["sha256"] = f"sha256:{hashlib.sha256(payload).hexdigest()}"
            structure["pdf"]["bytes"] = len(payload)
            (artifacts / "pdf-structure.json").write_text(json.dumps(structure), encoding="utf-8")

        outcome = rendered_outcome(non_pdf)
        problems = compare_parity.correctness_problems("fixture", {"correctness": {"page_count": 1}}, outcome)
        self.assertFalse(outcome["pdf_envelope_valid"])
        self.assertTrue(any("rendered PDF" in problem for problem in problems))

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

    def test_success_requires_complete_summary_evidence(self) -> None:
        fixture = {"correctness": {"page_count": 1, "text_contains": ["Minimal"]}}
        mutations = {
            "render_id": None,
            "scene_version": True,
            "fonts_artifact_version": True,
            "pdf_structure_version": True,
            "scene_capture_status": "partial",
            "scene_preview_status": "unsupported",
            "scene_unsupported_event_count": 1,
            "scene_text_mapping_gap_count": 1,
            "pdf_structure_status": "failed",
        }
        for key, value in mutations.items():
            with self.subTest(key=key):
                outcome = successful_outcome()
                outcome[key] = value
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
