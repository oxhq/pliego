#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Run the correctness-gated GitHub-hosted cross-renderer benchmark.

This command deliberately produces ``github-hosted-exploratory`` evidence.
It cannot mint a dedicated-host result and does not weaken the authoritative
single-target publication path in ``run_benchmark.py``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import tomllib
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, NoReturn

ROOT = Path(__file__).resolve().parents[2]
TOOLS = Path(__file__).resolve().parent
MANIFEST = ROOT / "benchmarks" / "manifest.toml"
SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-hosted-comparison.v1.json"
TARGET_IDS = (
    "pliego-0.3.3",
    "dompdf-3.1.6",
    "browsershot-5.4.0-puppeteer-25.8.0",
)
FIXTURE_ID = "minimal-static"
SAMPLE_COUNT = 100
WARMUP_ITERATIONS = 10
SEED = 1
EVIDENCE_CLASS = "github-hosted-exploratory"
PUBLICATION_STATUS = "hosted-snapshot"
CLAIM_BOUNDARY = (
    "Directional comparison on one GitHub-hosted VM; not dedicated-host evidence "
    "and not a general production-performance ranking."
)
WORKFLOW_REF = "oxhq/pliego/.github/workflows/pliego-performance.yml@refs/heads/main"
REQUIRED_BUNDLE_FILES = frozenset(
    {
        "SHA256SUMS",
        "all-metrics.md",
        "hosted-comparison.v1.json",
        "interleaved-run.v1.json",
        "verified-release.json",
    }
)

sys.path.insert(0, str(TOOLS))
import comparison_metrics  # noqa: E402
import run_benchmark  # noqa: E402
import validate_result  # noqa: E402


def fail(message: str) -> NoReturn:
    print(f"run_comparison: {message}", file=sys.stderr)
    raise SystemExit(1)


def read_text(path: str) -> str | None:
    try:
        return Path(path).read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def command_text(command: list[str]) -> str | None:
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    text = (result.stdout + result.stderr).strip()
    return text if result.returncode == 0 and text else None


def ambient_snapshot() -> dict[str, Any]:
    return {
        "loadavg": read_text("/proc/loadavg"),
        "pressure_cpu": read_text("/proc/pressure/cpu"),
        "pressure_memory": read_text("/proc/pressure/memory"),
        "pressure_io": read_text("/proc/pressure/io"),
        "meminfo": read_text("/proc/meminfo"),
        "process_count": len([entry for entry in Path("/proc").iterdir() if entry.name.isdigit()]),
    }


def cgroup_snapshot() -> dict[str, str | None]:
    return {
        "self": read_text("/proc/self/cgroup"),
        "mount": command_text(["findmnt", "-n", "-o", "TARGET,FSTYPE,OPTIONS", "/sys/fs/cgroup"]),
        "controllers": read_text("/sys/fs/cgroup/cgroup.controllers"),
        "cpu_max": read_text("/sys/fs/cgroup/cpu.max"),
        "cpuset_cpus_effective": read_text("/sys/fs/cgroup/cpuset.cpus.effective"),
        "memory_max": read_text("/sys/fs/cgroup/memory.max"),
        "pids_max": read_text("/sys/fs/cgroup/pids.max"),
        "clocksource": read_text("/sys/devices/system/clocksource/clocksource0/current_clocksource"),
    }


def github_source(revision: str) -> dict[str, Any]:
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    github_sha = os.environ.get("GITHUB_SHA", "")
    runner_environment = os.environ.get("PLIEGO_RUNNER_ENVIRONMENT", "")
    github_ref = os.environ.get("GITHUB_REF", "")
    event_name = os.environ.get("GITHUB_EVENT_NAME", "")
    workflow_ref = os.environ.get("GITHUB_WORKFLOW_REF", "")
    if os.environ.get("GITHUB_ACTIONS") != "true":
        fail("hosted comparison must run in GitHub Actions")
    if repository.casefold() != "oxhq/pliego":
        fail(f"hosted comparison must run in oxhq/pliego, got {repository!r}")
    if github_sha != revision:
        fail(f"GITHUB_SHA {github_sha!r} does not identify checked-out revision {revision!r}")
    if runner_environment != "github-hosted":
        fail(f"runner environment must be github-hosted, got {runner_environment!r}")
    if github_ref != "refs/heads/main":
        fail(f"public hosted snapshots must run from refs/heads/main, got {github_ref!r}")
    if event_name != "workflow_dispatch":
        fail(f"hosted snapshot event must be workflow_dispatch, got {event_name!r}")
    if workflow_ref.casefold() != WORKFLOW_REF.casefold():
        fail(f"hosted snapshot must use the reviewed main workflow, got {workflow_ref!r}")
    try:
        run_id = int(os.environ["GITHUB_RUN_ID"])
        run_attempt = int(os.environ["GITHUB_RUN_ATTEMPT"])
        repeat = int(os.environ["PLIEGO_SNAPSHOT_REPEAT"])
    except (KeyError, ValueError) as error:
        fail(f"GitHub run identity is incomplete: {error}")
    server = os.environ.get("GITHUB_SERVER_URL", "https://github.com").rstrip("/")
    return {
        "repository": repository,
        "revision": revision,
        "ref": github_ref,
        "event_name": event_name,
        "workflow": os.environ.get("GITHUB_WORKFLOW", ""),
        "workflow_ref": workflow_ref,
        "job": os.environ.get("GITHUB_JOB", ""),
        "run_id": run_id,
        "run_attempt": run_attempt,
        "repeat": repeat,
        "run_url": f"{server}/{repository}/actions/runs/{run_id}/attempts/{run_attempt}",
    }


def runner_identity() -> dict[str, Any]:
    return {
        "name": os.environ.get("RUNNER_NAME", ""),
        "os": os.environ.get("RUNNER_OS", ""),
        "arch": os.environ.get("RUNNER_ARCH", ""),
        "environment": os.environ.get("PLIEGO_RUNNER_ENVIRONMENT", ""),
        "image_os": os.environ.get("ImageOS", "unavailable"),
        "image_version": os.environ.get("ImageVersion", "unavailable"),
    }


def host_identity() -> dict[str, Any]:
    identity = run_benchmark.host_info(False)
    identity.update(
        {
            "dedicated": False,
            "uname": command_text(["uname", "-a"]),
            "os_release": read_text("/etc/os-release"),
            "lscpu_json": command_text(["lscpu", "--json"]),
            "virtualization": command_text(["systemd-detect-virt"]),
            "cgroup": cgroup_snapshot(),
        }
    )
    return identity


def pliego_runtime_identity(engine: dict[str, Any]) -> dict[str, str]:
    """Return the immutable published-runtime subset used by this comparison."""

    return {
        "contract": "pliego.published-runtime.v1",
        "release_tag": engine["release_tag"],
        "commit": engine["commit"],
        "servo_build": engine["servo_build"],
        "servo_base": engine["servo_base"],
        "bundle": engine["bundle"],
        "bundle_sha256": engine["bundle_sha256"],
        "profile": engine["profile"],
    }


def target_identity_sha256(identity: dict[str, Any]) -> str:
    return run_benchmark.canonical_json_sha256({key: identity[key] for key in ("id", "label", "engine", "runtime")})


def seal_target_identity(identity: dict[str, Any]) -> dict[str, Any]:
    sealed = dict(identity)
    sealed["identity_sha256"] = target_identity_sha256(sealed)
    return sealed


def target_contexts(
    manifest: dict[str, Any],
    binary: Path,
) -> tuple[dict[str, dict[str, Any]], list[dict[str, Any]]]:
    contexts: dict[str, dict[str, Any]] = {}
    identities: list[dict[str, Any]] = []
    for target_id in TARGET_IDS:
        target = manifest["targets"].get(target_id)
        if not isinstance(target, dict) or not target.get("enabled"):
            fail(f"comparison target {target_id!r} is not enabled")
        if target.get("kind") == "pliego":
            engine = run_benchmark.engine_identity(binary, target)
            target_binary = binary
            details = pliego_runtime_identity(engine)
            require_scene_report = True
            native_api2 = True
        elif target.get("kind") == "adapter":
            target_binary = (ROOT / target["adapter"]).resolve(strict=True)
            engine, details = run_benchmark.adapter_identity(target_binary, target_id, target)
            require_scene_report = False
            native_api2 = False
        else:
            fail(f"comparison target {target_id!r} has unsupported kind {target.get('kind')!r}")
        contexts[target_id] = {
            "binary": target_binary,
            "target": target,
            "require_scene_report": require_scene_report,
            "native_api2": native_api2,
        }
        identities.append(
            seal_target_identity(
                {
                    "id": target_id,
                    "label": target["label"],
                    "engine": engine,
                    "runtime": details,
                }
            )
        )
    return contexts, identities


def revalidate_target_contexts(
    manifest: dict[str, Any],
    contexts: dict[str, dict[str, Any]],
    identities: list[dict[str, Any]],
) -> None:
    expected = {identity["id"]: identity for identity in identities}
    for target_id in TARGET_IDS:
        target = manifest["targets"][target_id]
        binary = contexts[target_id]["binary"]
        if target.get("kind") == "pliego":
            engine = run_benchmark.engine_identity(binary, target)
            runtime = pliego_runtime_identity(engine)
        else:
            engine, runtime = run_benchmark.adapter_identity(binary, target_id, target)
        actual = seal_target_identity(
            {
                "id": target_id,
                "label": target["label"],
                "engine": engine,
                "runtime": runtime,
            }
        )
        if expected[target_id] != actual:
            fail(f"target identity changed during comparison: {target_id}")


def sample_evidence_violations(
    artifact: dict[str, Any],
    targets: list[dict[str, Any]] | None = None,
) -> list[validate_result.Violation]:
    violations: list[validate_result.Violation] = []
    identities = {target["id"]: target for target in targets or []}
    for index, record in enumerate(artifact["raw_samples"]):
        path = f"$artifact.raw_samples[{index}].sample"
        sample = record["sample"]
        usage = sample.get("resource_usage")
        if sample.get("measurement_method") != "linux-cgroup-v2-v1" or not isinstance(usage, dict):
            violations.append(validate_result.Violation(path, "lacks exact cgroup-v2 accounting"))
            continue
        validate_result.validate_resource_usage(sample, path, violations)
        if usage.get("cgroup_drained") is not True:
            violations.append(validate_result.Violation(path, "did not drain its descendant cgroup"))
        network = (usage.get("launch_security") or {}).get("network_isolation")
        if not isinstance(network, dict) or network.get("mode") != "linux-private-network-namespace-v1":
            violations.append(validate_result.Violation(path, "lacks private-network proof"))
            continue
        if network.get("interfaces") != ["lo"] or network.get("host_namespace") == network.get("engine_namespace"):
            violations.append(validate_result.Violation(path, "retained host networking"))
        identity = identities.get(record["target_id"])
        if identity is not None:
            executable = usage["launch_security"]["executable"]
            validate_result.require_equal(
                f"{path}.resource_usage.launch_security.executable.path",
                executable["path"],
                identity["engine"]["binary_path"],
                violations,
            )
            validate_result.require_equal(
                f"{path}.resource_usage.launch_security.executable.sha256",
                executable["sha256"],
                identity["engine"]["binary_sha256"],
                violations,
            )
    return violations


def _canonical_absolute_path(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    path = PurePosixPath(value)
    return path.is_absolute() and ".." not in path.parts and str(path) == value


def _require_hash(path: str, value: Any, violations: list[validate_result.Violation]) -> None:
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        violations.append(validate_result.Violation(path, "must be a lowercase SHA-256"))


def validate_target_identities(
    targets: list[dict[str, Any]],
    violations: list[validate_result.Violation],
) -> None:
    expected_ids = sorted(TARGET_IDS)
    validate_result.require_equal("$.targets", [target["id"] for target in targets], expected_ids, violations)
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    for index, identity in enumerate(targets):
        path = f"$.targets[{index}]"
        target_id = identity["id"]
        if target_id not in manifest["targets"]:
            violations.append(validate_result.Violation(f"{path}.id", "is not a benchmark manifest target"))
            continue
        target = manifest["targets"][target_id]
        engine = identity["engine"]
        runtime = identity["runtime"]
        validate_result.require_equal(f"{path}.label", identity["label"], target["label"], violations)
        validate_result.require_equal(
            f"{path}.identity_sha256",
            identity["identity_sha256"],
            target_identity_sha256(identity),
            violations,
        )
        expected_engine = {
            "name": "pliego" if target["kind"] == "pliego" else target["package"].split("/")[-1],
            "version": target["version"],
            "profile": target["profile"],
        }
        if target["kind"] == "adapter":
            expected_engine["package"] = target["package"]
        for key, expected in expected_engine.items():
            validate_result.require_equal(f"{path}.engine.{key}", engine.get(key), expected, violations)
        if not _canonical_absolute_path(engine.get("binary_path")):
            violations.append(
                validate_result.Violation(f"{path}.engine.binary_path", "must be an absolute canonical path")
            )
        _require_hash(f"{path}.engine.binary_sha256", engine.get("binary_sha256"), violations)
        if (
            not isinstance(engine.get("binary_bytes"), int)
            or isinstance(engine.get("binary_bytes"), bool)
            or engine["binary_bytes"] <= 0
        ):
            violations.append(validate_result.Violation(f"{path}.engine.binary_bytes", "must be a positive integer"))

        if target["kind"] == "pliego":
            expected_release = {
                "binary_sha256": target["binary_sha256"],
                "binary_bytes": target["binary_bytes"],
                "commit": target["commit"],
                "release_tag": target["release_tag"],
                "servo_build": target["servo_build"],
                "servo_base": target["servo_base"],
                "bundle": target["archive"],
                "bundle_sha256": target["archive_sha256"],
                "bundle_bytes": target["archive_bytes"],
            }
            for key, expected in expected_release.items():
                validate_result.require_equal(f"{path}.engine.{key}", engine.get(key), expected, violations)
            validate_result.require_equal(f"{path}.runtime", runtime, pliego_runtime_identity(engine), violations)
            continue

        adapter = (ROOT / target["adapter"]).resolve()
        validate_result.require_equal(
            f"{path}.engine.binary_sha256", engine.get("binary_sha256"), run_benchmark.file_sha256(adapter), violations
        )
        validate_result.require_equal(
            f"{path}.engine.binary_bytes", engine.get("binary_bytes"), adapter.stat().st_size, violations
        )
        expected_runtime = {
            "contract": "pliego.benchmark-adapter.v1",
            "target": target_id,
            "package": target["package"],
            "package_version": target["version"],
            "adapter_path": engine["binary_path"],
            "adapter_sha256": engine["binary_sha256"],
        }
        for lock_name in target.get("lockfiles", []):
            lock = (ROOT / lock_name).resolve()
            key = f"{lock.name.removesuffix('.json').replace('-', '_').replace('.', '_')}_sha256"
            expected_runtime[key] = run_benchmark.file_sha256(lock)
        for key, expected in expected_runtime.items():
            validate_result.require_equal(f"{path}.runtime.{key}", runtime.get(key), expected, violations)
        required_hashes = ["composer_vendor_sha256", "php_sha256"]
        required_paths = ["composer_vendor_path", "php_path"]
        required_text = ["php_version"]
        if target.get("requires_network_isolation"):
            required_hashes.extend(("node_modules_sha256", "node_sha256", "chrome_sha256"))
            required_paths.extend(("node_modules_path", "node_path", "chrome_path"))
            required_text.extend(("node_version", "chrome_version", "puppeteer_version"))
            validate_result.require_equal(
                f"{path}.runtime.puppeteer_version", runtime.get("puppeteer_version"), "25.8.0", violations
            )
        for key in required_hashes:
            _require_hash(f"{path}.runtime.{key}", runtime.get(key), violations)
        for key in required_paths:
            if not _canonical_absolute_path(runtime.get(key)):
                violations.append(
                    validate_result.Violation(f"{path}.runtime.{key}", "must be an absolute canonical path")
                )
        for key in required_text:
            if not isinstance(runtime.get(key), str) or not runtime[key].strip():
                violations.append(validate_result.Violation(f"{path}.runtime.{key}", "must be a nonempty string"))


def evidence_binding_sha256(data: dict[str, Any]) -> str:
    payload = {
        "contract": "pliego.hosted-comparison-evidence-binding.v1",
        "source": data["source"],
        "protocol": data["protocol"],
        "fixture": data["fixture"],
        "oracle": data["oracle"],
        "target_identity_sha256": [target["identity_sha256"] for target in data["targets"]],
        "interleaved_artifact": data["interleaved_artifact"],
    }
    return run_benchmark.canonical_json_sha256(payload)


def validate_oracle_identity(
    oracle: dict[str, Any],
    violations: list[validate_result.Violation],
) -> None:
    validate_result.require_equal("$.oracle.contract", oracle.get("contract"), "pliego.pdf-oracle.v1", violations)
    oracle_path = oracle.get("oracle_path")
    if not _canonical_absolute_path(oracle_path) or not str(oracle_path).endswith("/benchmarks/tools/pdf_oracle.py"):
        violations.append(
            validate_result.Violation(
                "$.oracle.oracle_path", "must be the absolute canonical checked-out PDF oracle path"
            )
        )
    validate_result.require_equal(
        "$.oracle.oracle_sha256",
        oracle.get("oracle_sha256"),
        run_benchmark.file_sha256(run_benchmark.PDF_ORACLE),
        violations,
    )
    for tool in run_benchmark.POPPLER_TOOLS:
        path_key = f"{tool}_path"
        hash_key = f"{tool}_sha256"
        version_key = f"{tool}_version"
        if not _canonical_absolute_path(oracle.get(path_key)):
            violations.append(validate_result.Violation(f"$.oracle.{path_key}", "must be an absolute canonical path"))
        _require_hash(f"$.oracle.{hash_key}", oracle.get(hash_key), violations)
        if not isinstance(oracle.get(version_key), str) or not oracle[version_key].strip():
            violations.append(validate_result.Violation(f"$.oracle.{version_key}", "must be a nonempty string"))


def comparison_sha256(data: dict[str, Any]) -> str:
    return run_benchmark.canonical_json_sha256(
        {key: value for key, value in data.items() if key != "comparison_sha256"}
    )


def validate_comparison(data: Any, artifact: Any) -> list[validate_result.Violation]:
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    violations: list[validate_result.Violation] = []
    validate_result.validate(data, schema, "$", violations)
    if violations:
        return violations
    artifact_violations = run_benchmark.validate_interleaved_artifact(artifact)
    if artifact_violations:
        return [
            validate_result.Violation(f"$artifact{violation.path[1:]}", violation.message)
            for violation in artifact_violations
        ]
    assert isinstance(data, dict) and isinstance(artifact, dict)
    validate_target_identities(data["targets"], violations)
    validate_oracle_identity(data["oracle"], violations)
    violations.extend(sample_evidence_violations(artifact, data["targets"]))
    validate_result.require_equal("$.claim_boundary", data["claim_boundary"], CLAIM_BOUNDARY, violations)
    validate_result.require_equal("$.comparison_sha256", data["comparison_sha256"], comparison_sha256(data), violations)
    validate_result.require_equal(
        "$.evidence_binding.sha256",
        data["evidence_binding"]["sha256"],
        evidence_binding_sha256(data),
        violations,
    )
    validate_result.require_equal(
        "$.interleaved_artifact.artifact_sha256",
        data["interleaved_artifact"]["artifact_sha256"],
        artifact["artifact_sha256"],
        violations,
    )
    validate_result.require_equal(
        "$.interleaved_artifact.schedule_sha256",
        data["interleaved_artifact"]["schedule_sha256"],
        run_benchmark.canonical_json_sha256(artifact["schedule"]),
        violations,
    )
    validate_result.require_equal(
        "$.protocol.sample_count", data["protocol"]["sample_count"], artifact["schedule"]["sample_count"], violations
    )
    validate_result.require_equal(
        "$.protocol.warmup_iterations",
        data["protocol"]["warmup_iterations"],
        artifact["schedule"]["warmup_iterations"],
        violations,
    )
    validate_result.require_equal(
        "$.protocol.targets",
        data["protocol"]["targets"],
        artifact["schedule"]["targets"],
        violations,
    )
    validate_result.require_equal(
        "$.protocol.fixture_id", data["protocol"]["fixture_id"], artifact["schedule"]["fixture_id"], violations
    )
    validate_result.require_equal("$.protocol.seed", data["protocol"]["seed"], artifact["schedule"]["seed"], violations)
    validate_result.require_equal(
        "$.targets",
        [target["id"] for target in data["targets"]],
        artifact["schedule"]["targets"],
        violations,
    )
    try:
        expected_aggregates = comparison_metrics.aggregate_interleaved_artifact(artifact)
    except ValueError as error:
        violations.append(validate_result.Violation("$artifact.raw_samples", str(error)))
    else:
        validate_result.require_equal("$.aggregates", data["aggregates"], expected_aggregates, violations)
    return violations


def atomic_json(path: Path, value: Any) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        stream.write(json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def write_checksums(output: Path, files: list[Path]) -> None:
    lines = []
    for path in sorted(files, key=lambda item: item.name):
        lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}")
    checksum = output / "SHA256SUMS"
    checksum.write_text("\n".join(lines) + "\n", encoding="ascii")


def validate_verified_release(value: Any, data: dict[str, Any]) -> list[validate_result.Violation]:
    violations: list[validate_result.Violation] = []
    if not isinstance(value, dict):
        return [validate_result.Violation("$.verified_release", "must be a JSON object")]
    pliego = next(target for target in data["targets"] if target["id"] == "pliego-0.3.3")
    engine = pliego["engine"]
    expected = {
        "schema": "pliego.verified-release",
        "version": 1,
        "target": "pliego-0.3.3",
        "release_tag": engine["release_tag"],
        "commit": engine["commit"],
        "servo_build": engine["servo_build"],
        "servo_base": engine["servo_base"],
        "platform": "linux-x86_64",
        "profile": engine["profile"],
        "runtime_manifest": "benchmarks/releases/v0.3.3/runtimes.json",
        "runtime_manifest_bytes": 3290,
        "runtime_manifest_sha256": "e4dc42db44d534d857cfb0a2e1c5f442a47ed913b6fcb999accaf50a4335412b",
        "archive_sha256": engine["bundle_sha256"],
        "archive_bytes": engine["bundle_bytes"],
        "binary_sha256": engine["binary_sha256"],
        "binary_bytes": engine["binary_bytes"],
    }
    for key, expected_value in expected.items():
        validate_result.require_equal(f"$.verified_release.{key}", value.get(key), expected_value, violations)
    for key in ("archive", "binary"):
        if not _canonical_absolute_path(value.get(key)):
            violations.append(
                validate_result.Violation(f"$.verified_release.{key}", "must be an absolute canonical path")
            )
    return violations


def validate_bundle(directory: Path) -> list[validate_result.Violation]:
    violations: list[validate_result.Violation] = []
    if not directory.is_dir():
        return [validate_result.Violation("$bundle", "directory does not exist")]
    entries = list(directory.iterdir())
    actual_files = {path.name for path in entries}
    validate_result.require_equal("$bundle.files", actual_files, REQUIRED_BUNDLE_FILES, violations)
    for entry in entries:
        if not entry.is_file() or entry.is_symlink():
            violations.append(validate_result.Violation(f"$bundle.{entry.name}", "must be a regular file"))
    if violations:
        return violations
    try:
        data = json.loads((directory / "hosted-comparison.v1.json").read_text(encoding="utf-8"))
        artifact = json.loads((directory / "interleaved-run.v1.json").read_text(encoding="utf-8"))
        release = json.loads((directory / "verified-release.json").read_text(encoding="utf-8"))
        report = (directory / "all-metrics.md").read_text(encoding="utf-8")
        checksum_lines = (directory / "SHA256SUMS").read_text(encoding="ascii").splitlines()
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return [validate_result.Violation("$bundle", f"cannot read retained bundle: {error}")]
    comparison_violations = validate_comparison(data, artifact)
    violations.extend(comparison_violations)
    if not comparison_violations and isinstance(data, dict):
        violations.extend(validate_verified_release(release, data))
        validate_result.require_equal("$bundle.all-metrics.md", report, render_markdown(data), violations)
    expected_checksummed = REQUIRED_BUNDLE_FILES - {"SHA256SUMS"}
    parsed: dict[str, str] = {}
    for index, line in enumerate(checksum_lines):
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)", line)
        if match is None:
            violations.append(validate_result.Violation(f"$bundle.SHA256SUMS[{index}]", "has invalid syntax"))
            continue
        digest, filename = match.groups()
        if filename in parsed:
            violations.append(validate_result.Violation(f"$bundle.SHA256SUMS[{index}]", "duplicates a filename"))
            continue
        parsed[filename] = digest
    validate_result.require_equal("$bundle.SHA256SUMS.files", set(parsed), expected_checksummed, violations)
    for filename in sorted(set(parsed) & expected_checksummed):
        actual = hashlib.sha256((directory / filename).read_bytes()).hexdigest()
        validate_result.require_equal(f"$bundle.SHA256SUMS.{filename}", parsed[filename], actual, violations)
    return violations


def finalize_bundle(directory: Path, release_path: Path) -> None:
    destination = directory / "verified-release.json"
    if not directory.is_dir() or destination.exists():
        fail("finalization requires an existing unfinalized comparison directory")
    destination.write_bytes(release_path.resolve(strict=True).read_bytes())
    write_checksums(directory, [directory / name for name in sorted(REQUIRED_BUNDLE_FILES - {"SHA256SUMS"})])
    violations = validate_bundle(directory)
    if violations:
        fail(f"finalized hosted comparison failed validation: {violations[0]}")


def render_markdown(data: dict[str, Any]) -> str:
    metrics = {target["target_id"]: target for target in data["aggregates"]["targets"]}
    lines = [
        "# Pliego v0.3.3 hosted comparative benchmark",
        "",
        "> Evidence class: `github-hosted-exploratory`. These are measured, correctness-gated",
        "> results from one GitHub-hosted VM, not dedicated-host production claims.",
        "",
        f"- Run: [{data['source']['run_id']} attempt {data['source']['run_attempt']}, "
        f"repeat {data['source']['repeat']}]({data['source']['run_url']})",
        f"- Revision: `{data['source']['revision']}`",
        f"- Runner image: `{data['runner']['image_os']}` `{data['runner']['image_version']}`",
        f"- CPU: `{data['host']['cpu_model']}`; logical cores: `{data['host']['cores']}`; RAM: `{data['host']['ram_bytes']}` bytes",
        f"- Protocol: 1 correctness preflight, {data['protocol']['warmup_iterations']} discarded warmups, "
        f"{data['protocol']['sample_count']} timed cold-process samples per renderer, seed {data['protocol']['seed']}",
        "- Input: identical local `minimal-static` fixture; network namespace contains loopback only for every renderer",
        "- Percentiles: nearest-rank; p99 is backed by 100 timed samples per renderer",
        f"- Raw artifact: `{data['interleaved_artifact']['artifact_sha256']}`",
        "",
        "## Latency, CPU, and memory",
        "",
        "| Renderer | Wall p50 | Wall p95 | Wall p99 | CPU p50 | Peak memory p50 | RSS p50 lower bound | Serial renders/min |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for target in data["targets"]:
        aggregate = metrics[target["id"]]
        values = aggregate["metrics"]
        wall = values["wall_ms"]["statistics"]
        cpu = values["cpu_total_ms"]["statistics"]
        memory = values["memory_peak_bytes"]["statistics"]
        rss = values["sampled_peak_rss_kib_lower_bound"]["statistics"]
        lines.append(
            f"| {target['label']} | {wall['p50']:.3f} ms | {wall['p95']:.3f} ms | {wall['p99']:.3f} ms | "
            f"{cpu['p50']:.3f} ms | {memory['p50'] / 1048576:.2f} MiB | {rss['p50'] / 1024:.2f} MiB | "
            f"{aggregate['serial_throughput']['renders_per_minute']:.2f} |"
        )
    lines.extend(
        [
            "",
            "## Block-device I/O and output",
            "",
            "`read_bytes` and `write_bytes` come from cgroup `io.stat`; memory-backed stdout/stderr capture is "
            "excluded for all targets. Browsershot's disclosed protected Node/Chrome tmpfs `TMPDIR` is also "
            "excluded from block I/O but charged to cgroup memory; its PHP `TMPDIR`, profile/XDG state, "
            "artifacts, and PDF remain on measured ext4.",
            "",
            "| Renderer | Block read p50 (`io.stat`) | Block write p50 (`io.stat`) | PDF bytes p50 | Correct samples | PDF hash variants |",
            "| --- | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for target in data["targets"]:
        aggregate = metrics[target["id"]]
        values = aggregate["metrics"]
        read = values["read_bytes"]["statistics"]["p50"]
        write = values["write_bytes"]["statistics"]["p50"]
        pdf = values["pdf_bytes"]["statistics"]["p50"]
        lines.append(
            f"| {target['label']} | {read / 1048576:.2f} MiB | {write / 1048576:.2f} MiB | "
            f"{pdf:.0f} | {aggregate['correctness_pass_count']}/{aggregate['sample_count']} | "
            f"{aggregate['pdf_sha256_variants']} |"
        )
    lines.extend(
        [
            "",
            "## Full aggregate table",
            "",
            "| Renderer | Metric | Unit | Min | p50 | p95 | p99 | Max | Mean |",
            "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for target in data["targets"]:
        aggregate = metrics[target["id"]]
        for metric_name in sorted(aggregate["metrics"]):
            metric = aggregate["metrics"][metric_name]
            statistics = metric["statistics"]
            lines.append(
                f"| {target['label']} | `{metric_name}` | {metric['unit']} | "
                f"{statistics['min']:.3f} | {statistics['p50']:.3f} | {statistics['p95']:.3f} | "
                f"{statistics['p99']:.3f} | {statistics['max']:.3f} | {statistics['mean']:.3f} |"
            )
    lines.extend(
        [
            "",
            "## Claim boundary",
            "",
            data["claim_boundary"],
            "",
            "The exact schedule, all raw samples, cgroup descendant accounting, runtime hashes, oracle hashes,",
            "and pre/post host-pressure snapshots are retained beside this report.",
            "",
        ]
    )
    return "\n".join(lines)


def run_comparison(binary: Path, output: Path) -> dict[str, Any]:
    if sys.platform != "linux" or platform.machine() != "x86_64":
        fail("hosted comparison requires Linux x86_64")
    if os.getuid() != 0 or os.geteuid() != 0:
        fail("hosted comparison requires the root cgroup broker")
    revision = run_benchmark.harness_revision()
    if revision is None or not run_benchmark.benchmark_tree_is_clean():
        fail("hosted comparison requires a clean exact benchmark revision")
    source = github_source(revision)
    if output.exists():
        fail(f"output directory already exists: {output}")
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    fixture = manifest["fixtures"][FIXTURE_ID]
    run_benchmark.check_prep(FIXTURE_ID, fixture)
    fixture_identity = run_benchmark.fixture_identity(fixture)
    oracle_before = run_benchmark.pdf_oracle_identity()
    contexts, target_identities = target_contexts(manifest, binary.resolve(strict=True))
    before = ambient_snapshot()

    def run_phase(target_id: str, phase: str, index: int | None = None) -> dict[str, Any] | None:
        context = contexts[target_id]
        try:
            return run_benchmark.run_runner_phase(
                Path("/usr/bin/php"),
                FIXTURE_ID,
                fixture,
                context["binary"],
                phase,
                index,
                context["require_scene_report"],
                fixture_identity,
                True,
                context["native_api2"],
            )
        except SystemExit:
            sample = "" if index is None else f" sample={index}"
            print(f"run_comparison: target={target_id} phase={phase}{sample} failed", file=sys.stderr)
            raise

    artifact = run_benchmark.execute_interleaved_run(
        list(TARGET_IDS),
        FIXTURE_ID,
        WARMUP_ITERATIONS,
        SAMPLE_COUNT,
        SEED,
        lambda target_id: run_phase(target_id, "preflight"),
        lambda target_id, index: run_phase(target_id, "warmup", index),
        lambda target_id, index: run_phase(target_id, "timed", index) or {},
    )
    if any(
        not record["sample"].get("ok") or not (record["sample"].get("correctness") or {}).get("pass")
        for record in artifact["raw_samples"]
    ):
        fail("a timed comparison sample failed rendering or correctness")
    evidence_violations = sample_evidence_violations(artifact, target_identities)
    if evidence_violations:
        fail(f"raw comparison evidence failed validation: {evidence_violations[0]}")
    after = ambient_snapshot()
    if run_benchmark.fixture_identity(fixture) != fixture_identity:
        fail("fixture changed during comparison")
    if run_benchmark.pdf_oracle_identity() != oracle_before:
        fail("PDF oracle identity changed during comparison")
    revalidate_target_contexts(manifest, contexts, target_identities)
    if not run_benchmark.benchmark_tree_is_clean() or run_benchmark.harness_revision() != revision:
        fail("benchmark harness changed during comparison")
    aggregates = comparison_metrics.aggregate_interleaved_artifact(artifact)
    identities_by_id = {target["id"]: target for target in target_identities}
    target_identities = [identities_by_id[target_id] for target_id in artifact["schedule"]["targets"]]
    generated_at = datetime.now(timezone.utc).isoformat()
    data: dict[str, Any] = {
        "schema": "pliego.benchmark-hosted-comparison",
        "version": 1,
        "evidence_class": EVIDENCE_CLASS,
        "publication_status": PUBLICATION_STATUS,
        "claim_boundary": CLAIM_BOUNDARY,
        "comparison_sha256": "",
        "generated_at": generated_at,
        "source": source,
        "runner": runner_identity(),
        "host": host_identity(),
        "ambient": {"before": before, "after": after},
        "protocol": {
            "fixture_id": FIXTURE_ID,
            "targets": list(artifact["schedule"]["targets"]),
            "correctness_preflight_iterations": 1,
            "warmup_iterations": WARMUP_ITERATIONS,
            "sample_count": SAMPLE_COUNT,
            "seed": SEED,
            "schedule_contract": run_benchmark.CROSS_TARGET_SCHEDULE,
            "sample_order": "globally-interleaved-seeded",
            "network": "private-loopback-only",
            "measurement_method": "linux-cgroup-v2-v1",
            "percentile_method": validate_result.PERCENTILE_METHOD,
        },
        "fixture": {
            "id": FIXTURE_ID,
            "input": fixture["input"],
            "input_sha256": fixture_identity[0],
            "bundle_sha256": fixture_identity[1],
        },
        "oracle": oracle_before,
        "targets": target_identities,
        "interleaved_artifact": {
            "file": "interleaved-run.v1.json",
            "artifact_sha256": artifact["artifact_sha256"],
            "schedule_sha256": run_benchmark.canonical_json_sha256(artifact["schedule"]),
        },
        "aggregates": aggregates,
        "evidence_binding": {
            "contract": "pliego.hosted-comparison-evidence-binding.v1",
            "sha256": "",
        },
    }
    data["evidence_binding"]["sha256"] = evidence_binding_sha256(data)
    data["comparison_sha256"] = comparison_sha256(data)
    violations = validate_comparison(data, artifact)
    if violations:
        fail(f"hosted comparison failed validation: {violations[0]}")

    output.mkdir(parents=True)
    artifact_path = output / "interleaved-run.v1.json"
    comparison_path = output / "hosted-comparison.v1.json"
    report_path = output / "all-metrics.md"
    atomic_json(artifact_path, artifact)
    atomic_json(comparison_path, data)
    report_path.write_text(render_markdown(data), encoding="utf-8")
    write_checksums(output, [artifact_path, comparison_path, report_path])
    print(f"wrote hosted comparison {data['comparison_sha256']} to {output}")
    return data


def run_preflight_all(binary: Path) -> None:
    """Exercise every comparison target through the publishable hosted sampler."""

    if sys.platform != "linux" or platform.machine() != "x86_64":
        fail("hosted comparison preflight requires Linux x86_64")
    if os.getuid() != 0 or os.geteuid() != 0:
        fail("hosted comparison preflight requires the root cgroup broker")
    revision = run_benchmark.harness_revision()
    if revision is None or not run_benchmark.benchmark_tree_is_clean():
        fail("hosted comparison preflight requires a clean exact benchmark revision")
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    fixture = manifest["fixtures"][FIXTURE_ID]
    run_benchmark.check_prep(FIXTURE_ID, fixture)
    expected_fixture_identity = run_benchmark.fixture_identity(fixture)
    expected_oracle_identity = run_benchmark.pdf_oracle_identity()
    contexts, identities = target_contexts(manifest, binary.resolve(strict=True))

    for target_id in TARGET_IDS:
        context = contexts[target_id]
        print(f"hosted comparison preflight: {target_id}", flush=True)
        result = run_benchmark.run_runner_phase(
            Path("/usr/bin/php"),
            FIXTURE_ID,
            fixture,
            context["binary"],
            "preflight",
            require_scene_report=context["require_scene_report"],
            expected_fixture_identity=expected_fixture_identity,
            isolate_network=True,
            native_api2=context["native_api2"],
        )
        if result is not None:
            fail(f"preflight for {target_id} unexpectedly emitted a timed sample")

    if run_benchmark.fixture_identity(fixture) != expected_fixture_identity:
        fail("fixture changed during hosted comparison preflight")
    if run_benchmark.pdf_oracle_identity() != expected_oracle_identity:
        fail("PDF oracle identity changed during hosted comparison preflight")
    revalidate_target_contexts(manifest, contexts, identities)
    if not run_benchmark.benchmark_tree_is_clean() or run_benchmark.harness_revision() != revision:
        fail("benchmark harness changed during hosted comparison preflight")
    print("all hosted comparison targets passed the exact sampler and correctness preflight")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="verified published Pliego v0.3.3 binary")
    parser.add_argument("--out", type=Path, help="new output directory")
    parser.add_argument("--validate", type=Path, help="validate an existing hosted comparison directory")
    parser.add_argument("--finalize", type=Path, help="comparison directory to finalize and validate")
    parser.add_argument("--verified-release", type=Path, help="verified release metadata used with --finalize")
    parser.add_argument(
        "--preflight-all",
        action="store_true",
        help="run one correctness-gated publishable sampler preflight for every comparison target",
    )
    args = parser.parse_args()
    if args.preflight_all:
        if args.binary is None or any(
            value is not None for value in (args.out, args.validate, args.finalize, args.verified_release)
        ):
            parser.error("--preflight-all requires --binary and cannot be combined with other operations")
        run_preflight_all(args.binary)
        return 0
    if args.validate is not None:
        if any(value is not None for value in (args.binary, args.out, args.finalize, args.verified_release)):
            parser.error("--validate cannot be combined with generation or finalization arguments")
        violations = validate_bundle(args.validate)
        if violations:
            fail(f"existing hosted comparison failed validation: {violations[0]}")
        print("hosted comparison bundle validated against exact retained evidence")
        return 0
    if args.finalize is not None:
        if args.verified_release is None or any(value is not None for value in (args.binary, args.out)):
            parser.error("--finalize requires --verified-release and cannot be combined with generation arguments")
        finalize_bundle(args.finalize, args.verified_release)
        print("hosted comparison bundle finalized and validated")
        return 0
    if args.verified_release is not None:
        parser.error("--verified-release requires --finalize")
    if args.binary is None or args.out is None:
        parser.error("--binary and --out are required unless --validate is used")
    run_comparison(args.binary, args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
