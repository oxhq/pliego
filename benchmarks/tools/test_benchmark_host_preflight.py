#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fixture-backed checks for the dedicated benchmark-host gate."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
PREFLIGHT = TOOLS / "benchmark_host_preflight.py"
VALIDATOR = TOOLS / "validate_host_proof.py"
WORKFLOW = TOOLS.parents[1] / ".github" / "workflows" / "pliego-dedicated-benchmark.yml"
SHA = "a" * 40
LABELS = ["Linux", "X64", "pliego-benchmark-pinned-v1", "self-hosted"]
SECRET = "must-not-reach-child"
EXPECTED_COMMAND = ["python3", "benchmarks/tools/test_process_tree_sampler.py", "--acceptance-overhead"]

sys.path.insert(0, str(TOOLS))
import benchmark_publication  # noqa: E402
import validate_host_proof  # noqa: E402


def write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def write_json(path: Path, value: object) -> None:
    write(path, json.dumps(value, indent=2) + "\n")


def make_fixture(root: Path) -> Path:
    fixture = root / "fixture"
    runner = {
        "id": 17,
        "name": "pliego-pinned-01",
        "os": "linux",
        "status": "online",
        "busy": True,
        "labels": [{"name": label} for label in LABELS],
    }
    write_json(
        fixture / "env.json",
        {
            "GITHUB_REPOSITORY": "OxHQ/pliego",
            "GITHUB_EVENT_NAME": "workflow_dispatch",
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_REF_PROTECTED": "true",
            "GITHUB_SHA": SHA,
            "GITHUB_RUN_ID": "123",
            "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_JOB": "dedicated",
            "GITHUB_WORKFLOW_REF": benchmark_publication.TRUSTED_WORKFLOW_REF,
            "RUNNER_NAME": "pliego-pinned-01",
            "RUNNER_OS": "Linux",
            "RUNNER_ARCH": "X64",
        },
    )
    runtime = {"checkout_sha": SHA, "worktree_clean": True, "self_pid": 999, "ancestor_pids": [999, 1]}
    runtime["fixture_command"] = [
        sys.executable,
        "-c",
        "import os; print('PLIEGO_BENCHMARK_PROOF_TOKEN' in os.environ, "
        "'PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX' in os.environ)",
    ]
    write_json(fixture / "runtime.json", runtime)
    write_json(
        fixture / "api.json",
        {
            "branch": {"name": "main", "protected": True, "commit": {"sha": SHA}},
            "runner_group": {"id": 7, "name": "Pliego dedicated benchmarks"},
            "group_runners": {"runners": [runner]},
        },
    )
    topology = {
        "2": {"package": 0, "core": 1, "siblings": "2-3"},
        "3": {"package": 0, "core": 1, "siblings": "2-3"},
    }
    write_json(
        fixture / "config.json",
        {
            "schema": "pliego.benchmark-host-config",
            "version": 1,
            "runner": {"name": "pliego-pinned-01", "group_id": 7},
            "cpu": {"set": "2-3", "topology": topology},
            "controls": {"boost": "disabled", "smt": "enabled", "aslr": 2},
            "sensors": {
                "thermal_paths": ["class/thermal/thermal_zone0/temp"],
                "throttle_paths": ["devices/system/cpu/cpu2/thermal_throttle/core_throttle_count"],
            },
            "limits": {
                "max_load_one_per_cpu": 0.25,
                "max_cpu_psi_avg10": 0.1,
                "max_memory_psi_avg10": 0.1,
                "max_io_psi_avg10": 0.1,
                "max_temperature_millic": 70000,
                "max_temperature_drift_millic": 5000,
                "max_interrupt_delta": 10000,
            },
            "lock_path": "benchmark.lock",
        },
    )

    write(fixture / "proc/self/status", "Name:\tpython\nCpus_allowed_list:\t2-3\n")
    write(fixture / "proc/loadavg", "0.10 0.05 0.01 1/100 999\n")
    pressure = "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"
    for kind in ("cpu", "memory", "io"):
        write(fixture / "proc/pressure" / kind, pressure)
    write(fixture / "proc/interrupts", "           CPU0 CPU1\n  0:          1          2 timer\n")
    write(fixture / "proc/sys/kernel/randomize_va_space", "2\n")

    write(fixture / "sys/devices/system/cpu/online", "0-3\n")
    write(fixture / "sys/devices/system/cpu/isolated", "2-3\n")
    write(fixture / "sys/devices/system/cpu/cpufreq/boost", "0\n")
    write(fixture / "sys/devices/system/cpu/smt/control", "on\n")
    for cpu in (2, 3):
        cpu_root = fixture / "sys/devices/system/cpu" / f"cpu{cpu}"
        write(cpu_root / "topology/physical_package_id", "0\n")
        write(cpu_root / "topology/core_id", "1\n")
        write(cpu_root / "topology/thread_siblings_list", "2-3\n")
        write(cpu_root / "cpufreq/scaling_governor", "performance\n")
    write(fixture / "sys/class/thermal/thermal_zone0/temp", "40000\n")
    write(fixture / "sys/devices/system/cpu/cpu2/thermal_throttle/core_throttle_count", "0\n")
    return fixture


def invoke(fixture: Path, output: Path, *command: str, mode: str = "production") -> subprocess.CompletedProcess[str]:
    environment = dict(os.environ)
    environment["PLIEGO_BENCHMARK_PROOF_TOKEN"] = SECRET
    environment[benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV] = "e" * 64
    return subprocess.run(
        [
            sys.executable,
            str(PREFLIGHT),
            "--mode",
            mode,
            "--fixture-root",
            str(fixture),
            "--output-dir",
            str(output),
            "--",
            *command,
        ],
        capture_output=True,
        env=environment,
        text=True,
        timeout=30,
    )


def proof(output: Path) -> dict:
    return json.loads((output / "benchmark-host-proof.v1.json").read_text(encoding="utf-8"))


def events(output: Path) -> list[str]:
    return [
        json.loads(line)["event"]
        for line in (output / "benchmark-host-chronology.ndjson").read_text(encoding="utf-8").splitlines()
    ]


class BenchmarkHostPreflightTests(unittest.TestCase):
    def test_full_benchmark_proof_binds_staged_candidate_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            output = root / "out"
            candidate = (root / "candidate.json").resolve()
            observer = (root / "observer-measurements.json").resolve()
            runtime_path = fixture / "runtime.json"
            runtime = json.loads(runtime_path.read_text(encoding="utf-8"))
            runtime["fixture_command"] = [
                sys.executable,
                "-c",
                (
                    "import pathlib,sys; "
                    "pathlib.Path(sys.argv[1]).write_text('{\"candidate\":true}\\n'); "
                    "pathlib.Path(sys.argv[2]).write_text('{}\\n')"
                ),
                str(candidate),
                str(observer),
            ]
            write_json(runtime_path, runtime)
            command = [
                "python3",
                "benchmarks/tools/run_benchmark.py",
                "--binary",
                str((root / "pliego").resolve()),
                "--frozen-fixture-root",
                str((root / "frozen").resolve()),
                "--staging-out",
                str(candidate),
                "--failure-evidence-dir",
                str((root / "failures").resolve()),
                "--observer-measurements-out",
                str(observer),
            ]
            result = invoke(fixture, output, *command)
            self.assertEqual(result.returncode, 0, result.stderr)
            document = proof(output)
            self.assertEqual(document["status"], "accepted")
            self.assertEqual(document["command"]["candidate"]["path"], str(candidate))
            self.assertEqual(
                document["command"]["candidate"]["sha256"],
                validate_host_proof.sha256_file(candidate),
            )
            self.assertEqual(document["command"]["observer_measurements"]["path"], str(observer))
            schema = json.loads((TOOLS.parent / "schema" / "benchmark-host-proof.v1.json").read_text(encoding="utf-8"))
            self.assertEqual(
                validate_host_proof.validate_document(
                    document,
                    schema,
                    output,
                    allow_fixture_evidence=True,
                    expected_command=tuple(command),
                ),
                [],
            )
            validation = subprocess.run(
                [
                    sys.executable,
                    str(VALIDATOR),
                    str(output / "benchmark-host-proof.v1.json"),
                    "--allow-fixture-evidence",
                    "--expect-status",
                    "accepted",
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(validation.returncode, 0, validation.stderr)

    def test_fixture_host_is_accepted_and_retained(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 0, result.stderr)
            document = proof(output)
            self.assertEqual(document["status"], "accepted")
            self.assertEqual(document["evidence_source"], "fixture")
            self.assertEqual(document["command"]["exit_code"], 0)
            self.assertEqual((output / "benchmark-command.stdout").read_text(encoding="utf-8"), "False False\n")
            self.assertIn("samples.started", events(output))
            validation = subprocess.run(
                [
                    sys.executable,
                    str(VALIDATOR),
                    str(output / "benchmark-host-proof.v1.json"),
                    "--allow-fixture-evidence",
                    "--expect-status",
                    "accepted",
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(validation.returncode, 0, validation.stderr)
            self.assertEqual(len((output / "SHA256SUMS").read_text(encoding="ascii").splitlines()), 5)
            self.assertNotIn(SECRET.encode(), b"".join(path.read_bytes() for path in output.iterdir()))
            document["command"]["argv"] = ["true"]
            violations = validate_host_proof.validate_semantics(
                document, output, allow_fixture_evidence=True, forbidden_event=None
            )
            self.assertIn(
                "accepted proof did not run the expected canonical workload",
                violations,
            )

    def test_noop_production_command_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            output = root / "out"
            result = invoke(fixture, output, "true")
            self.assertEqual(result.returncode, 1, result.stderr)
            document = proof(output)
            self.assertEqual(document["status"], "rejected")
            self.assertNotIn("samples.started", events(output))
            command_check = next(check for check in document["checks"] if check["id"] == "command.identity")
            self.assertFalse(command_check["passed"])

    def test_non_main_ref_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            env = json.loads((fixture / "env.json").read_text(encoding="utf-8"))
            env.update({"GITHUB_REF": "refs/heads/feature", "GITHUB_REF_PROTECTED": "false"})
            write_json(fixture / "env.json", env)
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(proof(output)["status"], "rejected")
            self.assertNotIn("samples.started", events(output))

    def test_wrong_architecture_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            env = json.loads((fixture / "env.json").read_text(encoding="utf-8"))
            env["RUNNER_ARCH"] = "ARM64"
            write_json(fixture / "env.json", env)
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(proof(output)["status"], "rejected")
            self.assertNotIn("samples.started", events(output))

    def test_missing_revision_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            runtime = json.loads((fixture / "runtime.json").read_text(encoding="utf-8"))
            runtime["checkout_sha"] = None
            write_json(fixture / "runtime.json", runtime)
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(proof(output)["status"], "rejected")
            self.assertNotIn("samples.started", events(output))

    def test_shared_runner_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            api = json.loads((fixture / "api.json").read_text(encoding="utf-8"))
            api["group_runners"]["runners"][0]["busy"] = False
            write_json(fixture / "api.json", api)
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(proof(output)["status"], "rejected")
            self.assertNotIn("samples.started", events(output))

    def test_missing_thermal_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            output = root / "out"
            result = invoke(
                fixture,
                output,
                sys.executable,
                "-c",
                "raise SystemExit('must not run')",
                mode="negative-missing-thermal",
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(proof(output)["status"], "rejected")
            self.assertNotIn("samples.started", events(output))
            self.assertEqual((output / "benchmark-command.stderr").read_bytes(), b"")
            validation = subprocess.run(
                [
                    sys.executable,
                    str(VALIDATOR),
                    str(output / "benchmark-host-proof.v1.json"),
                    "--expect-status",
                    "rejected",
                    "--forbid-event",
                    "samples.started",
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(validation.returncode, 0, validation.stderr)

    def test_github_hosted_negative_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            env = json.loads((fixture / "env.json").read_text(encoding="utf-8"))
            env.update({"RUNNER_NAME": "GitHub Actions 1", "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64"})
            write_json(fixture / "env.json", env)
            output = root / "out"
            result = invoke(
                fixture,
                output,
                sys.executable,
                "-c",
                "raise SystemExit('must not run')",
                mode="negative-github-hosted",
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(proof(output)["status"], "rejected")
            self.assertNotIn("samples.started", events(output))

    def test_throttle_change_fails_postflight(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            throttle = fixture / "sys/devices/system/cpu/cpu2/thermal_throttle/core_throttle_count"
            runtime = json.loads((fixture / "runtime.json").read_text(encoding="utf-8"))
            runtime["fixture_command"] = [
                sys.executable,
                "-c",
                f"from pathlib import Path; Path({str(throttle)!r}).write_text('1')",
            ]
            write_json(fixture / "runtime.json", runtime)
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 1, result.stderr)
            document = proof(output)
            self.assertEqual(document["status"], "failed")
            self.assertEqual(document["failure"]["code"], "HOST_POSTFLIGHT_REJECTED")
            self.assertIn("samples.started", events(output))
            self.assertEqual(next(iter(document["drift"]["throttle_delta"].values())), 1)

    def test_existing_lock_is_not_removed_and_rejects(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            lock = fixture / "benchmark.lock"
            write(lock, "owned elsewhere\n")
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertTrue(lock.is_file())
            self.assertNotIn("samples.started", events(output))

    def test_incomplete_process_scan_rejects_before_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            write(fixture / "proc/42/status", "Name:\tunknown\n")
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertNotIn("samples.started", events(output))
            self.assertFalse(
                next(check for check in proof(output)["checks"] if check["id"] == "host.process_scan")["passed"]
            )

    def test_secret_in_command_is_redacted_and_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            output = root / "out"
            result = invoke(fixture, output, sys.executable, "-c", "pass", SECRET)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertNotIn("samples.started", events(output))
            self.assertIn("<redacted>", proof(output)["command"]["argv"])
            self.assertNotIn(SECRET.encode(), b"".join(path.read_bytes() for path in output.iterdir()))

    def test_tampered_chronology_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = make_fixture(root)
            output = root / "out"
            result = invoke(fixture, output, *EXPECTED_COMMAND)
            self.assertEqual(result.returncode, 0, result.stderr)
            with (output / "benchmark-host-chronology.ndjson").open("a", encoding="utf-8") as handle:
                handle.write("{}\n")
            validation = subprocess.run(
                [
                    sys.executable,
                    str(VALIDATOR),
                    str(output / "benchmark-host-proof.v1.json"),
                    "--allow-fixture-evidence",
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertNotEqual(validation.returncode, 0)
            self.assertIn("SHA256SUMS mismatch", validation.stderr)

    def test_workflow_is_manual_and_environment_bound(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("on:\n  workflow_dispatch:", workflow)
        self.assertNotIn("\n  push:", workflow)
        self.assertNotIn("\n  pull_request:", workflow)
        self.assertIn("group: Pliego dedicated benchmarks", workflow)
        self.assertIn("labels: [self-hosted, Linux, X64, pliego-benchmark-pinned-v1]", workflow)
        self.assertIn("environment: Pliego dedicated benchmarks", workflow)
        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertEqual(workflow.count("secrets.OXHQ_BENCHMARK_PROOF_TOKEN"), 1)
        self.assertEqual(workflow.count("secrets.PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX"), 3)
        dedicated_header = workflow.split("  dedicated:", 1)[1].split("    steps:", 1)[0]
        self.assertNotIn("\n    env:\n", dedicated_header)
        self.assertIn("negative-github-hosted", workflow)
        self.assertIn("negative-missing-thermal", workflow)
        self.assertIn("publication:\n        description:", workflow)
        self.assertIn("PUBLICATION_OPERATION: ${{ inputs.publication }}", workflow)
        self.assertEqual(workflow.count('--operation "$PUBLICATION_OPERATION"'), 2)
        self.assertIn("--forbid-event samples.started", workflow)
        self.assertIn("-- python3 benchmarks/tools/test_process_tree_sampler.py --acceptance-overhead", workflow)
        self.assertIn('--observer-measurements-out "$OBSERVER_MEASUREMENTS"', workflow)
        self.assertIn("python3 benchmarks/tools/observer_ab.py bind", workflow)
        self.assertIn("python3 benchmarks/tools/create_publication_attestation.py", workflow)
        self.assertIn('--observer-proof "$OBSERVER_PROOF"', workflow)
        self.assertIn('--attestation "$ATTESTATION"', workflow)
        evidence_step = workflow.split("- name: Gate host and produce benchmark evidence", 1)[1].split(
            "- name: Authenticate benchmark publication subject", 1
        )[0]
        attestation_step = workflow.split("- name: Authenticate benchmark publication subject", 1)[1].split(
            "- name: Publish authenticated benchmark baseline", 1
        )[0]
        publisher_step = workflow.split("- name: Publish authenticated benchmark baseline", 1)[1].split(
            "- name: Stage exact publication source artifact", 1
        )[0]
        source_stage_step = workflow.split("- name: Stage exact publication source artifact", 1)[1].split(
            "- name: Remove frozen fixture mirror", 1
        )[0]
        verifier_step = workflow.split("- name: Revalidate HMAC-bound publication source", 1)[1].split(
            "- name: Retain newly verified baseline only", 1
        )[0]
        self.assertNotIn("PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX", evidence_step)
        self.assertIn("create_publication_attestation.py", attestation_step)
        self.assertNotIn("publish_benchmark.py", attestation_step)
        self.assertIn("${PUBLISHED_BASELINE##*/}", attestation_step)
        self.assertNotIn("$(basename", attestation_step)
        self.assertIn("publish_benchmark.py", publisher_step)
        self.assertNotIn("create_publication_attestation.py", publisher_step)
        self.assertNotIn("secrets.PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX", source_stage_step)
        self.assertIn('test -z "${PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX:-}"', source_stage_step)
        for sensitive_step in (attestation_step, publisher_step, verifier_step):
            self.assertEqual(sensitive_step.count("secrets.PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX"), 1)
            self.assertIn("unset PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX", sensitive_step)
        self.assertIn("Initialize setup diagnostics before checkout", workflow)
        self.assertIn("benchmark_setup_evidence.py", workflow)
        self.assertIn("pliego-setup-evidence-${{ github.run_id }}-${{ github.run_attempt }}", workflow)
        self.assertIn("if: success() && inputs.mode == 'production'", workflow)
        always_upload = workflow.split("- name: Retain host proof and failure evidence", 1)[1].split(
            "- name: Retain exact publication source", 1
        )[0]
        self.assertNotIn("benchmarks/baselines/", always_upload)
        self.assertNotIn("pliego-candidate-", always_upload)


if __name__ == "__main__":
    unittest.main()
