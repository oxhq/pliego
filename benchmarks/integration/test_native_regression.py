#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Portable source guards; NOT Linux lifecycle, overlap or performance proof."""

from __future__ import annotations

import base64
import copy
import ast
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from argparse import Namespace
from collections import Counter
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import run_native_regression as campaign
from test_real_document_comparison import measured_sample


def envelope() -> tuple[dict, dict, dict]:
    binary = {"path": "/opt/native/pliego", "sha256": "b" * 64, "bytes": 200, "device": 1, "inode": 91}
    request = '{"synthetic":"not a render fixture"}\n'
    workers = [
        {
            "index": index,
            "job": f"/controlled/worker-{index}/job",
            "temporary": f"/controlled/worker-{index}/temporary",
            "request": request,
            "request_sha256": hashlib.sha256(request.encode()).hexdigest(),
        }
        for index in range(2)
    ]
    prepared = {
        "plan": {"schema": campaign.batch.CONTRACT, "binary": binary, "workers": workers},
        "artifacts": "/batch/artifacts",
    }
    measured = measured_sample(0, campaign.digest(campaign.HERE / "native_batch_launcher.py"))
    launch = measured["resource_usage"]["launch_security"]
    launch["argv"] = ["/tmp/pliego", "render", "paired-minimal-static", "--artifacts", prepared["artifacts"]]
    launch["temporary_storage"]["native_api2_path_bindings"] = None
    records, observed = [], []
    group = (
        "/" + campaign.PurePosixPath(measured["resource_usage"]["cgroup_path"]).relative_to("/sys/fs/cgroup").as_posix()
    )
    for index in range(2):
        record = {
            "index": index,
            "pid": 600 + index,
            "started_monotonic_ns": 10 + index,
            "exited_monotonic_ns": 40 + index,
            "exit_code": 0,
            "streams_complete": True,
        }
        for name, raw in (("stdout", b'{"synthetic":true}\n'), ("stderr", b"")):
            record[name + "_base64"] = base64.b64encode(raw).decode()
            record[name + "_sha256"] = hashlib.sha256(raw).hexdigest()
        records.append(record)
        observed.append(
            {
                "pid": 600 + index,
                "start_ticks": 500 + index,
                "cgroup": group,
                "executable_device": 1,
                "executable_inode": 91,
            }
        )
    result = {
        "schema": campaign.batch.CONTRACT,
        "error": None,
        "workers": records,
        "overlap": {"observed": True, "at_monotonic_ns": 20, "workers": observed},
        "plan_sha256": hashlib.sha256(campaign.harness.canonical_json_bytes(prepared["plan"]) + b"\n").hexdigest(),
    }
    measured.update(
        stdout=json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n",
        stderr="",
        sampler_stdout=json.dumps(measured["resource_usage"]),
        sampler_stderr="",
    )
    return prepared, measured, binary


class NativeRegressionTests(unittest.TestCase):
    def test_failed_native_batch_retains_both_trees_and_streams_before_rejection(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            output = base / "retained"
            output.mkdir()
            prepared, measured, binary = envelope()
            for worker in prepared["plan"]["workers"]:
                owned = base / f"native-{worker['index']}"
                for name in ("job", "temporary"):
                    path = owned / name
                    path.mkdir(parents=True)
                    worker[name] = str(path)
                (owned / "job/diagnostic.txt").write_bytes(f"native-{worker['index']} retained failure".encode())
            measured["exit_code"] = measured["resource_usage"]["exit_code"] = 1
            measured["sampler_stdout"] = json.dumps(measured["resource_usage"])
            result = json.loads(measured["stdout"])
            result["workers"][0]["exit_code"] = 1
            measured["stdout"] = json.dumps(result)
            responses = [
                subprocess.CompletedProcess([], 0, json.dumps(value).encode(), b"") for value in (prepared, measured)
            ]
            fake_sampler = SimpleNamespace(
                resolve_engine_account=lambda: object(),
                bind_native_api2_storage=lambda *a, **kw: {"synthetic": (1, 2)},
                native_api2_storage_snapshot=lambda job, temporary, binding: {
                    "job": str(job),
                    "temporary": str(temporary),
                },
            )
            with (
                patch.object(campaign, "sampler", fake_sampler),
                patch.object(campaign, "invoke", side_effect=responses),
                patch.object(campaign, "verify_storage"),
                patch.object(campaign, "qualify_worker") as oracle,
            ):
                with self.assertRaisesRegex(ValueError, "Measured root failed"):
                    campaign.concurrent_attempt(
                        Namespace(php=Path("php")),
                        output,
                        {},
                        campaign.fixture(),
                        {"binary": binary, "identity": {"probe": {}}},
                    )
                oracle.assert_not_called()
            for index in (0, 1):
                retained = output / f"worker-{index}"
                self.assertEqual(
                    (retained / "job/diagnostic.txt").read_bytes(), f"native-{index} retained failure".encode()
                )
                self.assertEqual(campaign.read(retained / "process.json")["exit_code"], 1 if index == 0 else 0)
                self.assertEqual((retained / "stdout.txt").read_bytes(), b'{"synthetic":true}\n')
                self.assertTrue((retained / "request.json").is_file())
                self.assertTrue((base / f"native-{index}/job").is_dir())

    def test_unknown_cleanup_still_prevents_tree_reads_and_oracles(self) -> None:
        prepared, measured, binary = envelope()
        measured["resource_usage"]["cgroup_drained"] = False
        responses = [
            subprocess.CompletedProcess([], 0, json.dumps(value).encode(), b"") for value in (prepared, measured)
        ]
        fake_sampler = SimpleNamespace(
            resolve_engine_account=lambda: object(),
            bind_native_api2_storage=lambda *a, **kw: {},
            native_api2_storage_snapshot=lambda *a: {},
        )
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(campaign, "sampler", fake_sampler),
            patch.object(campaign.batch, "validate_plan"),
            patch.object(campaign, "invoke", side_effect=responses),
            patch.object(campaign, "copy_job") as copy_tree,
            patch.object(campaign, "qualify_worker") as oracle,
        ):
            with self.assertRaises(ValueError):
                campaign.concurrent_attempt(
                    Namespace(php=Path("php")),
                    Path(temporary),
                    {},
                    campaign.fixture(),
                    {"binary": binary, "identity": {"probe": {}}},
                )
            copy_tree.assert_not_called()
            oracle.assert_not_called()

    def test_timed_acceptance_is_revalidated_offline_with_relocation_safe_preflight(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            preflight = base / "acceptance-preflight"
            preflight.mkdir()
            (preflight / "manifest.json").write_bytes(b"{}\n")
            prior = {"mode": "preflight", "identity_sha256": "a" * 64, "completed": True}
            campaign.save(preflight / "summary.json", prior)
            accepted = {
                "schema": campaign.SCHEMA + ".acceptance",
                "identity_sha256": "a" * 64,
                "preflight_directory": "/original-host/preflight-no-longer-present",
                "preflight_manifest_sha256": campaign.digest(preflight / "manifest.json"),
                "reviewed": True,
                "linux_lifecycle_controls_reviewed": True,
                "evidence": "reviewed lifecycle proof",
            }
            with patch.object(campaign, "verify", return_value=prior) as verifier:
                campaign.verify_acceptance(accepted, "a" * 64, preflight)
                verifier.assert_called_once_with(preflight)
            for field, value in (
                ("schema", "unknown"),
                ("identity_sha256", "b" * 64),
                ("reviewed", False),
                ("linux_lifecycle_controls_reviewed", False),
                ("evidence", " "),
                ("preflight_manifest_sha256", "c" * 64),
            ):
                mutated = {**accepted, field: value}
                with (
                    self.subTest(field=field),
                    patch.object(campaign, "verify", return_value=prior),
                    self.assertRaises(ValueError),
                ):
                    campaign.verify_acceptance(mutated, "a" * 64, preflight)
            for changed in ({**prior, "completed": False}, {**prior, "identity_sha256": "c" * 64}):
                with patch.object(campaign, "verify", return_value=changed), self.assertRaises(ValueError):
                    campaign.verify_acceptance(accepted, "a" * 64, preflight)
            # The actual top-level offline verifier must read acceptance, not
            # just trust a resealed timed summary/manifest.
            identity = {}
            campaign.save(base / "identity.json", identity)
            campaign.save(base / "targets.json", {})
            campaign.save(
                base / "summary.json",
                {
                    "schema": campaign.SCHEMA,
                    "mode": "timed",
                    "identity_sha256": campaign.harness.canonical_json_sha256(identity),
                },
            )
            for variant in (None, {**accepted, "reviewed": False}):
                acceptance_file = base / "acceptance.json"
                if variant is not None:
                    campaign.save(acceptance_file, variant)
                manifest = base / "manifest.json"
                if manifest.exists():
                    manifest.unlink()
                campaign.save(manifest, campaign.common.inventory(base))
                with self.assertRaises((ValueError, FileNotFoundError)):
                    campaign.verify(base)
            # A timed/self-referential predecessor is rejected before recursion.
            (preflight / "summary.json").write_text(json.dumps({**prior, "mode": "timed"}))
            with (
                patch.object(campaign, "verify") as recursive,
                self.assertRaisesRegex(ValueError, "must reference a preflight"),
            ):
                campaign.verify_acceptance(accepted, "a" * 64, preflight)
            recursive.assert_not_called()

    def test_host_discovery_precedes_all_attempts_and_cannot_follow_outer_timeout(self) -> None:
        tree = ast.parse(Path(campaign.__file__).read_text())
        run = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "run")
        host_calls = [
            node
            for node in ast.walk(run)
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "host_info"
        ]
        attempt_loop = next(
            node
            for node in run.body
            if isinstance(node, ast.For) and isinstance(node.iter, ast.Name) and node.iter.id == "entries"
        )
        self.assertEqual(len(host_calls), 1)
        self.assertLess(host_calls[0].lineno, attempt_loop.lineno)

    def test_schedule_counts_and_no_timing_before_both_preflights(self) -> None:
        schedules = [campaign.schedule(repeat, "timed") for repeat in (1, 2, 3)]
        self.assertNotEqual(schedules[0], schedules[1])
        for entries in schedules:
            self.assertEqual(len(entries), 334)
            self.assertEqual([entry["phase"] for entry in entries[:4]], ["preflight"] * 4)
            counts = Counter((e["population"], e["target_id"], e["phase"]) for e in entries)
            for population, phases in campaign.COUNTS.items():
                for target in campaign.TARGETS:
                    for phase, count in phases.items():
                        self.assertEqual(counts[population, target, phase], count)
        self.assertEqual(len(campaign.schedule(1, "preflight")), 4)
        for value in (0, 4, True):
            with self.assertRaises(ValueError):
                campaign.schedule(value, "timed")

    def test_batch_envelope_retains_one_sampler_and_two_native_workers(self) -> None:
        prepared, measured, binary = envelope()
        result = campaign.verify_batch_envelope(prepared, measured, binary)
        self.assertEqual(len(result["workers"]), 2)

    def test_resealed_overlap_stream_root_and_worker_corruptions(self) -> None:
        mutations = {
            "missing-overlap": lambda r: r["overlap"].update(observed=False),
            "duplicate-pid": lambda r: r["workers"][1].update(pid=600),
            "wrong-native-inode": lambda r: r["overlap"]["workers"][0].update(executable_inode=1),
            "wrong-cgroup": lambda r: r["overlap"]["workers"][0].update(cgroup="/other"),
            "non-overlap": lambda r: r["workers"][0].update(exited_monotonic_ns=15),
            "failed-worker": lambda r: r["workers"][0].update(exit_code=1),
            "partial-stream": lambda r: r["workers"][0].update(streams_complete=False),
            "changed-stream": lambda r: r["workers"][0].update(stdout_base64="eA=="),
            "missing-worker": lambda r: r["workers"].pop(),
            "changed-plan": lambda r: r.update(plan_sha256="f" * 64),
        }
        for name, change in mutations.items():
            with self.subTest(name=name):
                prepared, measured, binary = envelope()
                result = json.loads(measured["stdout"])
                change(result)
                measured["stdout"] = json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n"
                with self.assertRaises((ValueError, KeyError, TypeError)):
                    campaign.verify_batch_envelope(prepared, measured, binary)
        prepared, measured, binary = envelope()
        measured["resource_usage"]["launch_security"]["executable"]["sha256"] = "f" * 64
        measured["sampler_stdout"] = json.dumps(measured["resource_usage"])
        with self.assertRaisesRegex(ValueError, "Wrong measured"):
            campaign.verify_batch_envelope(prepared, measured, binary)

    def test_measurement_failure_or_raised_budget_does_not_qualify(self) -> None:
        for kind in ("budget", "kill", "raw-sampler", "unknown-field"):
            prepared, measured, binary = envelope()
            if kind == "budget":
                measured["resource_usage"]["root_wall_deadline"]["limit_ms"] = 130000
            elif kind == "kill":
                measured["resource_usage"]["cleanup"]["kill_used"] = True
            elif kind == "raw-sampler":
                measured["sampler_stdout"] = "{}"
            else:
                measured["resource_usage"]["unvalidated"] = True
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                campaign.verify_batch_envelope(prepared, measured, binary)

    def test_storage_aliases_and_resealed_path_changes_fail(self) -> None:
        prepared, _, _ = envelope()
        before = []
        for worker in prepared["plan"]["workers"]:
            job = campaign.PurePosixPath(worker["job"])
            paths = {
                "controlled_root": job.parent.parent,
                "sandbox": job.parent,
                "job": job,
                "temporary": campaign.PurePosixPath(worker["temporary"]),
            }
            before.append(
                {
                    "contract": "native-api2-storage-bindings-v1",
                    "bindings": {
                        name: {
                            "path": str(path),
                            "identity": {
                                "device": 1,
                                "inode": 1 if name == "controlled_root" else 2 + offset + worker["index"] * 10,
                            },
                        }
                        for offset, (name, path) in enumerate(paths.items())
                    },
                }
            )
        campaign.verify_storage(prepared, before, copy.deepcopy(before))
        for name in ("inode", "path", "split-device", "missing"):
            corrupted = copy.deepcopy(before)
            if name == "inode":
                corrupted[1]["bindings"]["job"]["identity"] = corrupted[0]["bindings"]["job"]["identity"]
            elif name == "path":
                corrupted[0]["bindings"]["job"]["path"] = "/not-owned/job"
            elif name == "split-device":
                corrupted[0]["bindings"]["temporary"]["identity"]["device"] = 2
            else:
                corrupted.pop()
            with self.subTest(name=name), self.assertRaises(ValueError):
                campaign.verify_storage(prepared, corrupted, copy.deepcopy(corrupted))

    def test_launcher_plan_paths_requests_and_exact_two(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            prepared, _, _ = envelope()
            for worker in prepared["plan"]["workers"]:
                root = Path(temporary).resolve() / str(worker["index"])
                for name in ("job", "temporary"):
                    path = root / name
                    path.mkdir(parents=True)
                    worker[name] = str(path)
            plan = prepared["plan"]
            campaign.batch.validate_plan(plan)
            for name in ("duplicate", "request", "arity", "bool-index"):
                corrupted = copy.deepcopy(plan)
                if name == "duplicate":
                    corrupted["workers"][1].update(
                        job=plan["workers"][0]["job"], temporary=plan["workers"][0]["temporary"]
                    )
                elif name == "request":
                    corrupted["workers"][0]["request"] += "x"
                elif name == "bool-index":
                    corrupted["workers"][0]["index"] = False
                else:
                    corrupted["workers"].pop()
                with self.subTest(name=name), self.assertRaises(ValueError):
                    campaign.batch.validate_plan(corrupted)

    def test_overlap_witness_requires_both_pidfds_still_live(self) -> None:
        native = SimpleNamespace(st_dev=1, st_ino=2)
        mock_sampler = SimpleNamespace(cgroup_relative=lambda *a: "/owned", read_stat=lambda pid: {"start_ticks": pid})
        with (
            patch.object(campaign.batch, "sampler", mock_sampler),
            patch.object(Path, "stat", return_value=native),
            patch.object(campaign.batch.select, "POLLIN", 1, create=True),
            patch.object(campaign.batch.select, "poll", create=True) as poll,
        ):
            poll.return_value.poll.return_value = []
            processes = [SimpleNamespace(pid=101), SimpleNamespace(pid=102)]
            self.assertTrue(campaign.batch.overlap_witness(processes, [7, 8], (1, 2))["observed"])
            poll.return_value.poll.return_value = [(7, 1)]
            with self.assertRaises(campaign.batch.OverlapUnavailable):
                campaign.batch.overlap_witness(processes, [7, 8], (1, 2))

    def test_denominators_and_batch_percentiles_are_not_p99_or_divided_memory(self) -> None:
        entries = campaign.schedule(1, "timed")
        attempts = []
        for entry in entries:
            value = entry["iteration"] + 1
            measurement = {metric: value for metric in campaign.METRICS}
            attempts.append(
                {
                    "entry": entry,
                    "correct": True,
                    "result": {
                        "measurement": measurement,
                        "documents": [{}] * (1 if entry["population"] == "serial" else 2),
                    },
                }
            )
        result = campaign.aggregate(entries, attempts)
        single = result["serial"]["targets"]["candidate"]
        paired = result["concurrency-2"]["targets"]["candidate"]
        self.assertEqual((single["observations"], single["documents"]), (100, 100))
        self.assertEqual((paired["observations"], paired["documents"]), (50, 100))
        self.assertEqual(single["metrics"]["wall_ms"]["p99"], 99)
        self.assertEqual(paired["metrics"]["memory_peak_bytes"]["p50"], 25)
        self.assertNotIn("p99", paired["metrics"]["wall_ms"])
        self.assertIsNone(campaign.aggregate(entries, attempts[:-1]))
        attempts[-1]["correct"] = False
        self.assertIsNone(campaign.aggregate(entries, attempts))
        attempts[-1]["correct"] = True
        attempts[-1]["result"]["documents"].pop()
        with self.assertRaisesRegex(ValueError, "denominator"):
            campaign.aggregate(entries, attempts)

    def test_outer_timeout_is_fatal_and_raw_evidence_survives(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(
                campaign.subprocess, "run", side_effect=subprocess.TimeoutExpired(["fake"], 180, b"partial", b"failure")
            ):
                with self.assertRaises(campaign.common.FatalInfrastructureError):
                    campaign.invoke(["fake"], root, "measure")
            self.assertEqual((root / "measure.stdout").read_bytes(), b"partial")
            self.assertFalse(campaign.read(root / "measure.process.json")["cleanup_verified"])

    def test_optimized_python_still_rejects_corrupt_schedule(self) -> None:
        command = [
            sys.executable,
            "-O",
            "-c",
            "import sys;sys.path.insert(0,sys.argv[1]);import run_native_regression as n;n.schedule(True,'timed')",
            str(campaign.HERE),
        ]
        process = subprocess.run(command, capture_output=True)
        self.assertNotEqual(process.returncode, 0)
        self.assertIn(b"Expected repeat", process.stderr)

    def test_optimized_runtime_and_archive_verifier_reject_before_any_work(self) -> None:
        for call in ("n.run(None)", "n.verify(None)"):
            process = subprocess.run(
                [
                    sys.executable,
                    "-O",
                    "-c",
                    "import sys;sys.path.insert(0,sys.argv[1]);import run_native_regression as n;" + call,
                    str(campaign.HERE),
                ],
                capture_output=True,
            )
            self.assertNotEqual(process.returncode, 0)
            self.assertIn(b"requires Python without -O", process.stderr)

    def test_frozen_fixture_and_corrupted_probe_fail_closed(self) -> None:
        self.assertEqual(campaign.harness.fixture_identity(campaign.fixture()), campaign.FIXTURE_HASHES)
        # A version string alone cannot identify a target; no malformed probe
        # can bypass the exact schema/source/contract checks.
        with self.assertRaises((ValueError, KeyError)):
            campaign.target_identity(
                "v0.3.3", {}, {"engine": {"api": 2, "runtime": {"target": "x86_64-unknown-linux-gnu"}}}, "a" * 64, 1
            )


if __name__ == "__main__":
    unittest.main()
