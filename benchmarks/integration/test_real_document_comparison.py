#!/usr/bin/env python3
"""Pure campaign guards; these are not native/Linux measurement evidence."""

from __future__ import annotations

import copy
import json
import subprocess
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

import run_real_document_comparison as campaign


def replace_json(path: Path, value: object) -> None:
    """Reseal deliberately corrupted synthetic evidence, not a production writer."""
    path.write_bytes(campaign.harness.canonical_json_bytes(value) + b"\n")


def measured_sample(index: int, binary_sha: str) -> dict:
    from test_comparison_metrics import passing_sample
    from test_validate_result import resource_usage

    sample = passing_sample("synthetic-campaign", 0)
    usage = resource_usage()
    sample.update(index=index, measurement_method=usage["method"], resource_usage=usage)
    for field, resource_field in {
        "wall_ms": "wall_ms",
        "user_ms": "cpu_user_ms",
        "sys_ms": "cpu_sys_ms",
        "memory_current_bytes": "memory_current_bytes",
        "memory_peak_bytes": "memory_peak_bytes",
        "read_bytes": "read_bytes",
        "write_bytes": "write_bytes",
        "read_operations": "read_operations",
        "write_operations": "write_operations",
    }.items():
        sample[field] = usage[resource_field]
    for kind in ("rss", "pss"):
        sample[f"sampled_peak_{kind}_kib_lower_bound"] = usage["sampled_diagnostics"][
            f"sampled_peak_summed_{kind}_kib_lower_bound"
        ]
    usage["launch_security"]["executable"]["sha256"] = binary_sha
    usage["launch_security"]["network_isolation"] = {
        "mode": "linux-private-network-namespace-v1",
        "host_namespace": "net:[1]",
        "engine_namespace": "net:[2]",
        "interfaces": ["lo"],
    }
    return sample


class SyntheticArchive:
    """Real sealed files with synthetic metrics/font facts; never native proof."""

    def __init__(self, root: Path, family: str = "ledger") -> None:
        self.root = root
        self.plan = campaign.schedule("synthetic", "legacy", 1)
        self.identity = {
            "track": "synthetic",
            "family": family,
            "fixture_identity": ["b" * 64, "c" * 64],
            "targets": {campaign.TARGET: {"binary_sha256": "a" * 64}, "legacy": {"binary_sha256": "d" * 64}},
        }
        self.records: list[dict] = []
        (root / "attempts").mkdir()
        replace_json(root / "identity.json", self.identity)
        replace_json(root / "schedule.json", self.plan)

    def append(self, phase: str, entry: dict) -> Path:
        root = self.root / "attempts" / f"{phase}-{entry['position']:03d}"
        root.mkdir()
        index = (
            -1000000 if phase == "preflight" else (-1 - entry["iteration"] if phase == "warmup" else entry["iteration"])
        )
        retained = root / f"retained/sample-{index}"
        native = entry["target_id"] == campaign.TARGET
        pdf = retained / ("job/delivery/document.pdf" if native else "output/document.pdf")
        pdf.parent.mkdir(parents=True)
        pdf.write_bytes(b"%PDF-synthetic-archive-guard-test-only\n")
        sample = measured_sample(index, self.identity["targets"][entry["target_id"]]["binary_sha256"])
        sample["output"].update(pdf_sha256=campaign.digest(pdf), pdf_bytes=pdf.stat().st_size)
        replace_json(retained / "sample.json", sample)
        timing = {
            "root_wall_ms": sample["wall_ms"],
            "sampler_lifecycle_wall_ms": sample["one_shot_wall_ms"],
            "tree_wall_ms": sample["wall_ms"] + sample["resource_usage"]["drain_ms"],
        }
        manifest = {
            "schema": "pliego.benchmark-retained-sample.v1",
            "phase": phase,
            "index": index,
            "binary_sha256": self.identity["targets"][entry["target_id"]]["binary_sha256"],
            "fixture_input_sha256": self.identity["fixture_identity"][0],
            "fixture_bundle_sha256": self.identity["fixture_identity"][1],
            "root_wall_timeout_ms": campaign.DEADLINE_MS,
            "timing": timing,
            "files": {},
        }
        replace_json(retained / "manifest.json", manifest)
        for label in ("runner", "oracle"):
            replace_json(root / f"{label}.process.json", {"exit_code": 0, "timeout_seconds": 180})
            (root / f"{label}.stdout").write_bytes(b"")
        if phase == "timed":
            replace_json(root / "runner.stdout", sample)
        family = self.identity["family"]
        provider = "pliego" if native else ("browsershot" if family == "invobook" else "dompdf")
        policies = {
            "pliego": "source-outline-metric-cmap-style-and-scene-subset-glyph-closure-v1",
            "dompdf": "original-whole-font-bytes",
            "browsershot": "chrome-identity-h-painted-cid-source-unicode-outline-metric-style-v1",
        }
        font_proof = {
            "outcome": "passed",
            "policy": policies[provider],
            "embedded": [{"synthetic": True}],
            "sceneResources": [{"synthetic": True}] if native else [],
            "scenePdfGlyphMappings": 1 if native else 0,
            "paintedGlyphMappings": 1,
        }
        report = {
            "schema": "pliego.aureus-ledger-oracle.v1"
            if family == "ledger"
            else f"pliego.real-document.{family}-oracle.v1",
            "outcome": "passed",
            "provider": provider,
            "pdfSha256": campaign.digest(pdf),
            "pdfBytes": pdf.stat().st_size,
            "layoutFingerprint": "e" * 64,
            "fixtureSha256" if family == "ledger" else "inputHtmlSha256": self.identity["fixture_identity"][0],
            "fonts" if family == "ledger" else "fontProof": font_proof,
        }
        (root / "oracle").mkdir()
        replace_json(root / "oracle/report.json", report)
        record = {
            "phase": phase,
            **entry,
            "outcome": "correct",
            "sample": sample,
            "timing": timing,
            "retention_sha256": "",
            "oracle": {
                "report_sha256": "",
                "pdf_sha256": report["pdfSha256"],
                "layout_fingerprint": report["layoutFingerprint"],
            },
        }
        self.records.append(record)
        replace_json(root / "attempt.json", record)
        self.reseal_attempt(root)
        return root

    def reseal_attempt(self, root: Path) -> None:
        record = campaign.read(root / "attempt.json")
        if record["sample"] is not None:
            retained = next((root / "retained").iterdir())
            manifest = campaign.read(retained / "manifest.json")
            manifest["files"] = {k: v for k, v in campaign.inventory(retained).items() if k != "manifest.json"}
            replace_json(retained / "manifest.json", manifest)
            record["sample"] = campaign.read(retained / "sample.json")
            record["retention_sha256"] = campaign.digest(retained / "manifest.json")
            record["oracle"]["report_sha256"] = campaign.digest(root / "oracle/report.json")
        replace_json(root / "attempt.json", record)
        for position, old in enumerate(self.records):
            if (old["phase"], old["position"]) == (record["phase"], record["position"]):
                self.records[position] = record

    def seal(self, mode: str = "preflight") -> None:
        replace_json(
            self.root / "campaign.json",
            {
                "schema": campaign.SCHEMA,
                "track": "synthetic",
                "repeat": 1,
                "mode": mode,
                "identity_sha256": campaign.harness.canonical_json_sha256(self.identity),
                "counts": campaign.counts(self.plan, self.records),
                "comparison_qualified": False,
                "aggregate": None,
            },
        )
        replace_json(
            self.root / "files.json", {k: v for k, v in campaign.inventory(self.root).items() if k != "files.json"}
        )


class CampaignTests(unittest.TestCase):
    def test_frozen_tracks(self) -> None:
        manifest = campaign.HERE / "real_documents.json"
        for name in campaign.read(manifest)["tracks"]:
            track, fixture, corpus = campaign.configuration(manifest, name)
            self.assertEqual(campaign.harness.fixture_identity(fixture)[0], track["input_sha256"])
            self.assertTrue(corpus.is_dir())
            self.assertTrue(fixture["page_size"].endswith("au"))

    def test_seeded_schedule_and_missing_denominators(self) -> None:
        plan = campaign.schedule("synthetic", "legacy", 1)
        self.assertEqual(plan, campaign.schedule("synthetic", "legacy", 1))
        self.assertNotEqual(plan["timed"], campaign.schedule("synthetic", "legacy", 2)["timed"])
        records = [{"phase": "preflight", **plan["preflight"][0], "outcome": "oracle-failure"}]
        counts = campaign.counts(plan, records)
        self.assertEqual(sum(v["attempted"] for v in counts["preflight"].values()), 1)
        for value in counts["timed"].values():
            self.assertEqual(value, {"scheduled": 100, "attempted": 0, "not_attempted": 100, "outcomes": {}})
        self.assertIsNone(campaign.aggregate(plan, records, False))
        with self.assertRaises(ValueError):
            campaign.aggregate(plan, records, True)

    def test_reject_changed_or_extra_retention_bytes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="campaign-paths-") as temporary:
            root = Path(temporary)
            (root / "payload").write_bytes(b"synthetic")
            expected = campaign.inventory(root)
            campaign.check_inventory(root, expected)
            for bad in ("../payload", "/payload", "a//b", "a/./b", "C:/payload", "a\\b"):
                with self.subTest(path=bad), self.assertRaises(ValueError):
                    campaign.relative_file(root, bad)
            (root / "payload").write_bytes(b"changed")
            with self.assertRaises(ValueError):
                campaign.check_inventory(root, expected)
            (root / "payload").write_bytes(b"synthetic")
            (root / "extra").write_bytes(b"unlisted")
            with self.assertRaises(ValueError):
                campaign.check_inventory(root, expected)

    def test_retention_schema_binding(self) -> None:
        with tempfile.TemporaryDirectory(prefix="campaign-retained-") as temporary:
            root = Path(temporary)
            retained = root / "retained/sample--1000000"
            retained.mkdir(parents=True)
            sample = {"index": -1000000, "ok": False, "error": "ROOT_WALL_TIMEOUT"}
            campaign.save(retained / "sample.json", sample)
            manifest = {
                "schema": "pliego.benchmark-retained-sample.v1",
                "phase": "preflight",
                "index": -1000000,
                "binary_sha256": "a" * 64,
                "fixture_input_sha256": "b" * 64,
                "fixture_bundle_sha256": "c" * 64,
                "root_wall_timeout_ms": campaign.DEADLINE_MS,
                "timing": {"root_wall_ms": None, "tree_wall_ms": None},
                "files": campaign.inventory(retained),
            }
            campaign.save(retained / "manifest.json", manifest)
            _, observed, _ = campaign.inspect_retention(root, -1000000, "preflight", "a" * 64, ("b" * 64, "c" * 64))
            self.assertEqual(observed, sample)
            for field, wrong in (("binary_sha256", "d" * 64), ("phase", "timed"), ("root_wall_timeout_ms", 0)):
                changed = {**manifest, field: wrong}
                (retained / "manifest.json").write_text(json.dumps(changed))
                with self.subTest(field=field), self.assertRaises(ValueError):
                    campaign.inspect_retention(root, -1000000, "preflight", "a" * 64, ("b" * 64, "c" * 64))

    def test_acceptance_cannot_move_between_identities(self) -> None:
        identity = {"targets": {"legacy": {}, campaign.TARGET: {}}, "fixture_identity": ["a" * 64, "b" * 64]}
        value = {
            "schema": "pliego.real-document-visual-acceptance.v1",
            "identity_sha256": campaign.harness.canonical_json_sha256(identity),
            "targets": {
                name: {
                    "reviewed": True,
                    "notes": "Synthetic test only",
                    "evidence": "unit test",
                    "layout_fingerprints": ["c" * 64],
                    "pdf_sha256": ["d" * 64],
                }
                for name in identity["targets"]
            },
        }
        with tempfile.TemporaryDirectory(prefix="campaign-acceptance-") as temporary:
            path = Path(temporary) / "acceptance.json"
            campaign.save(path, value)
            self.assertEqual(campaign.acceptance(path, identity), value)
            changed = copy.deepcopy(identity)
            changed["fixture_identity"][0] = "e" * 64
            with self.assertRaises(ValueError):
                campaign.acceptance(path, changed)
            for name in value["targets"]:
                bad = copy.deepcopy(value)
                bad["targets"][name]["reviewed"] = False
                path.write_text(json.dumps(bad))
                with self.assertRaises(ValueError):
                    campaign.acceptance(path, identity)

    def test_complete_population_uses_existing_validated_metrics(self) -> None:
        from test_comparison_metrics import passing_sample

        plan = campaign.schedule("synthetic", "legacy", 1)
        attempts = []
        for phase in ("preflight", "warmup", "timed"):
            for entry in plan[phase]:
                sample = passing_sample("alpha", entry["iteration"])
                attempts.append(
                    {
                        "phase": phase,
                        **entry,
                        "outcome": "correct",
                        "sample": sample,
                        "timing": {"tree_wall_ms": sample["wall_ms"] + 0.5},
                    }
                )
        # Synthetic helper samples explicitly have unavailable accounting; mock
        # only that live-resource gate while exercising real schema/statistics.
        with self.assertRaisesRegex(ValueError, "Missing cgroup"):
            campaign.aggregate(plan, attempts, True)
        with patch.object(campaign, "validate_measurement"):
            result = campaign.aggregate(plan, attempts, True)
        self.assertEqual(result["metrics"]["sample_count_per_target"], 100)
        self.assertEqual(result["tree_wall_ms"][campaign.TARGET]["p50"], 50.5)
        corrupted = copy.deepcopy(attempts)
        corrupted[-1]["sample"]["retained"] = {
            "output_dir": "test",
            "artifacts_dir": "test",
            "evidence_dir": "wrong-extra-key",
        }
        with patch.object(campaign, "validate_measurement"), self.assertRaises(ValueError):
            campaign.aggregate(plan, corrupted, True)
        corrupted = copy.deepcopy(attempts)
        corrupted[-1]["outcome"] = "incorrect"
        with self.assertRaises(ValueError):
            campaign.aggregate(plan, corrupted, True)

    def test_outer_timeout_is_not_safe_to_continue(self) -> None:
        import subprocess

        with tempfile.TemporaryDirectory(prefix="campaign-timeout-") as temporary:
            root = Path(temporary)
            with patch.object(
                campaign.subprocess, "run", side_effect=subprocess.TimeoutExpired(["fake"], 1, output=b"partial")
            ):
                with self.assertRaisesRegex(ValueError, "cleanup unverified"):
                    campaign.capture(["fake"], root, "runner", 1)
            self.assertFalse(campaign.read(root / "runner.process.json")["cleanup_verified"])
            self.assertEqual((root / "runner.stdout").read_bytes(), b"partial")

    def test_oracle_outer_timeout_is_fatal_even_during_preflight(self) -> None:
        with tempfile.TemporaryDirectory(prefix="campaign-oracle-timeout-") as temporary:
            root = Path(temporary)
            binary = root / "candidate"
            binary.write_bytes(b"synthetic")
            binary_sha = campaign.digest(binary)
            sample = measured_sample(-1000000, binary_sha)
            retained = root / "retained"
            retained.mkdir()
            replace_json(retained / "manifest.json", {})
            args = Namespace(candidate_binary=binary, php=Path("php"), track="synthetic", poppler_dir=Path("poppler"))
            identity = {
                "fixture_identity": ["b" * 64, "c" * 64],
                "targets": {campaign.TARGET: {"binary_sha256": binary_sha}},
            }
            with (
                patch.object(campaign.harness, "build_command", return_value=["synthetic"]),
                patch.object(campaign.harness, "fixture_identity", return_value=tuple(identity["fixture_identity"])),
                patch.object(campaign, "inspect_retention", return_value=(retained, sample, {"timing": {}})),
                patch.object(
                    campaign,
                    "capture",
                    side_effect=[
                        subprocess.CompletedProcess(["synthetic"], 0, b"", b""),
                        campaign.FatalInfrastructureError("Oracle cleanup unverified"),
                    ],
                ) as captured,
            ):
                record = campaign.execute_attempt(
                    args,
                    {"family": "ledger"},
                    {},
                    root,
                    identity,
                    {"target_id": campaign.TARGET, "iteration": 0, "position": 0},
                    "preflight",
                    root,
                    None,
                )
            self.assertEqual(captured.call_count, 2)
            self.assertEqual(record["outcome"], "infrastructure-failure")
            self.assertIs(record["cleanup_verified"], False)
            self.assertTrue(campaign.stop_after_attempt("preflight", record))

    def test_archives_bind_all_provider_pdf_and_font_reports(self) -> None:
        for family in ("ledger", "manufacturing", "invobook"):
            with self.subTest(family=family), tempfile.TemporaryDirectory(prefix="campaign-valid-") as temporary:
                archive = SyntheticArchive(Path(temporary), family)
                for entry in archive.plan["preflight"]:
                    archive.append("preflight", entry)
                archive.seal()
                result = campaign.verify(archive.root)
                self.assertTrue(result["verified"])
                self.assertFalse(result["comparison_qualified"])

    def test_resealed_corrupt_success_archives_are_rejected(self) -> None:
        # Every mutation refreshes both inventory layers and record hashes. A
        # rejection must come from semantic evidence, not a stale outer digest.
        cases = [
            ("pdf", "bytes", b"%PDF-corrupt-but-resealed\n", "delivered PDF"),
            ("sample", "ok", False, "Failed raw sample"),
            ("sample", "correctness.pass", False, "Failed raw sample"),
            ("sample", "correctness.checks", [{"name": "synthetic", "status": "fail"}], "Failed raw sample"),
            ("sample", "correctness.checks", [], "Failed raw sample"),
            ("sample", "exit_code", 1, "Invalid resource accounting"),
            ("sample", "signal", 9, "Invalid resource accounting"),
            ("sample", "output.published_pdf", False, "delivered PDF"),
            ("sample", "output.pdf_sha256", "f" * 64, "delivered PDF"),
            ("sample", "output.pdf_bytes", 1, "delivered PDF"),
            ("sample", "resource_usage.launch_security.executable.sha256", "f" * 64, "Measured executable"),
            (
                "sample",
                "resource_usage.launch_security.network_isolation.engine_namespace",
                "net:[1]",
                "network namespace",
            ),
            ("runner", "exit_code", 1, "runner exit"),
            ("stdout", "bytes", b"unexpected", "untimed runner stdout"),
            ("oracle", "exit_code", 1, "Oracle did not exit"),
            ("report", "provider", "wrong", "Wrong or unsuccessful"),
            ("report", "outcome", "failed", "Wrong or unsuccessful"),
            ("report", "fixtureSha256", "f" * 64, "bound to the delivered PDF"),
            ("report", "pdfSha256", "f" * 64, "bound to the delivered PDF"),
            ("report", "pdfBytes", 1, "Oracle PDF size"),
            ("report", "fonts.outcome", "failed", "font proof"),
            ("report", "fonts.policy", "name-only-font-check", "font proof"),
            ("report", "fonts.embedded", [], "font proof"),
            ("report", "fonts.sceneResources", [], "scene/subset font closure"),
            ("report", "fonts.scenePdfGlyphMappings", 0, "scene/subset font closure"),
            ("record", "oracle.pdf_sha256", "f" * 64, "oracle identity facts"),
        ]
        for location, key, wrong, error in cases:
            with (
                self.subTest(location=location, key=key),
                tempfile.TemporaryDirectory(prefix="campaign-corrupt-") as temporary,
            ):
                archive = SyntheticArchive(Path(temporary))
                roots = {entry["target_id"]: archive.append("preflight", entry) for entry in archive.plan["preflight"]}
                root = roots[campaign.TARGET]
                retained = root / "retained/sample--1000000"
                paths = {
                    "pdf": retained / "job/delivery/document.pdf",
                    "sample": retained / "sample.json",
                    "runner": root / "runner.process.json",
                    "stdout": root / "runner.stdout",
                    "oracle": root / "oracle.process.json",
                    "report": root / "oracle/report.json",
                    "record": root / "attempt.json",
                }
                path = paths[location]
                if key == "bytes":
                    path.write_bytes(wrong)
                else:
                    value = campaign.read(path)
                    owner = value
                    parts = key.split(".")
                    for part in parts[:-1]:
                        owner = owner[part]
                    owner[parts[-1]] = wrong
                    replace_json(path, value)
                archive.reseal_attempt(root)
                archive.seal()
                with self.assertRaisesRegex(ValueError, error):
                    campaign.verify(archive.root)

    def test_browser_painted_font_mapping_is_required(self) -> None:
        with tempfile.TemporaryDirectory(prefix="campaign-browser-font-") as temporary:
            archive = SyntheticArchive(Path(temporary), "invobook")
            roots = {entry["target_id"]: archive.append("preflight", entry) for entry in archive.plan["preflight"]}
            root = roots["legacy"]
            path = root / "oracle/report.json"
            report = campaign.read(path)
            report["fontProof"]["paintedGlyphMappings"] = 0
            replace_json(path, report)
            archive.reseal_attempt(root)
            archive.seal()
            with self.assertRaisesRegex(ValueError, "painted Chrome glyph proof"):
                campaign.verify(archive.root)

    def test_timed_stdout_must_equal_retained_raw_sample(self) -> None:
        with tempfile.TemporaryDirectory(prefix="campaign-timed-stdout-") as temporary:
            archive = SyntheticArchive(Path(temporary))
            entry = archive.plan["timed"][0]
            root = archive.append("timed", entry)
            retained = root / f"retained/sample-{entry['iteration']}"
            sample = campaign.read(retained / "sample.json")
            campaign.check_success_evidence(root, "timed", entry["target_id"], archive.identity, sample, retained)
            for output in (b"", b"{}\n", (root / "runner.stdout").read_bytes() * 2):
                (root / "runner.stdout").write_bytes(output)
                with self.subTest(output=output[:20]), self.assertRaisesRegex(ValueError, "Timed stdout"):
                    campaign.check_success_evidence(
                        root, "timed", entry["target_id"], archive.identity, sample, retained
                    )

    def test_resealed_archives_cannot_continue_after_stop(self) -> None:
        for failure, warmup, error in (
            ("infrastructure-failure", False, "mandatory stop"),
            ("oracle-failure", True, "failed phase"),
        ):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(prefix="campaign-stop-") as temporary:
                archive = SyntheticArchive(Path(temporary))
                roots = [archive.append("preflight", entry) for entry in archive.plan["preflight"]]
                record = campaign.read(roots[0] / "attempt.json")
                record.update(outcome=failure)
                if failure == "infrastructure-failure":
                    record["cleanup_verified"] = False
                replace_json(roots[0] / "attempt.json", record)
                archive.reseal_attempt(roots[0])
                if warmup:
                    archive.append("warmup", archive.plan["warmup"][0])
                archive.seal("timed")
                with self.assertRaisesRegex(ValueError, error):
                    campaign.verify(archive.root)

    def test_malformed_cgroup_sample_is_a_typed_validation_failure(self) -> None:
        with self.assertRaisesRegex(ValueError, "Invalid sample structure"):
            campaign.validate_measurement({"index": -1000000, "measurement_method": "linux-cgroup-v2-v1"})
        for number in (float("nan"), float("inf"), float("-inf")):
            sample = measured_sample(0, "a" * 64)
            sample["phase_timings_ms"]["render"] = number
            with self.subTest(number=number), self.assertRaisesRegex(ValueError, "Out of range float"):
                campaign.validate_measurement(sample)


if __name__ == "__main__":
    unittest.main()
