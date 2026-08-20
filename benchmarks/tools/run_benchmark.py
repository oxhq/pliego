#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Pliego benchmark orchestrator.

Reads `benchmarks/manifest.toml`, drives the target-neutral single-sample runner
for each enabled fixture, aggregates raw samples into a result document, and
validates it against
`schema/benchmark-result.v1.json` before writing it.

Usage:
    python3 benchmarks/tools/run_benchmark.py \
        [--binary /path/to/pliego] \
        [--target pliego-0.2.0|dompdf-3.1.6|browsershot-...] \
        [--out benchmarks/baselines/pliego-0.2.0-linux-x86_64.json] \
        [--fixture invoice-showcase] [--samples N] [--warmup N] \
        [--php /usr/bin/php] [--dedicated]

Design notes:
* The PHP runner owns each target launch and delegates Linux execution to the
  cgroup-v2 sampler; this script verifies target and oracle identity.
* Results are validated against the schema before being written; a result
  that fails validation is not saved.
* Throughput is serial renders/minute (concurrency 1); concurrent 2/4/8
  sampling is a later increment.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import random
import re
import statistics
import subprocess
import sys
import tomllib
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Callable, NoReturn

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "benchmarks" / "manifest.toml"
RUNNER = ROOT / "benchmarks" / "runners" / "pliego.php"
PDF_ORACLE = ROOT / "benchmarks" / "tools" / "pdf_oracle.py"
SAMPLER = ROOT / "benchmarks" / "tools" / "process_tree_sampler.py"
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"
POPPLER_TOOLS = ("pdfinfo", "pdftotext", "pdffonts", "pdftoppm")
ORACLE_IDENTITY_REASON = "manifest does not pin canonical Poppler tool identities"
ADAPTER_ATTESTATION_REASON = "out-of-process OCI image attestation is unavailable"
CROSS_TARGET_SCHEDULE = "pliego.cross-target-schedule.v1"

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_result  # noqa: E402


def fail(message: str) -> NoReturn:
    print(f"run_benchmark: {message}", file=sys.stderr)
    raise SystemExit(1)


def cross_target_phase_schedule(
    target_ids: list[str],
    fixture_id: str,
    iterations: int,
    seed: int,
    phase: str,
) -> list[dict[str, Any]]:
    """Return one seeded, target-complete order for every phase iteration.

    Ranking uses SHA-256 over a compact JSON tuple instead of Python's random
    implementation, making the schedule reproducible in other languages and
    across Python versions. Every target appears exactly once per iteration.
    """

    if len(target_ids) < 2:
        raise ValueError("cross-target scheduling requires at least two targets")
    if any(not isinstance(target_id, str) or not target_id for target_id in target_ids):
        raise ValueError("cross-target scheduling requires unique nonempty target ids")
    if len(set(target_ids)) != len(target_ids):
        raise ValueError("cross-target scheduling requires unique nonempty target ids")
    if not isinstance(fixture_id, str) or not fixture_id:
        raise ValueError("cross-target scheduling requires a fixture id")
    if not isinstance(iterations, int) or isinstance(iterations, bool) or iterations < 1:
        raise ValueError("cross-target scheduling requires at least one iteration")
    if not isinstance(seed, int) or isinstance(seed, bool):
        raise ValueError("cross-target scheduling requires an integer seed")
    if phase not in {"preflight", "warmup", "timed"}:
        raise ValueError("cross-target scheduling phase must be preflight, warmup, or timed")

    canonical_targets = sorted(target_ids, key=lambda target_id: target_id.encode("utf-8"))
    schedule: list[dict[str, Any]] = []
    for iteration in range(iterations):
        ranked: list[tuple[bytes, bytes, str]] = []
        for target_id in canonical_targets:
            rank_input = json.dumps(
                [CROSS_TARGET_SCHEDULE, seed, fixture_id, phase, iteration, target_id],
                ensure_ascii=True,
                separators=(",", ":"),
            ).encode("ascii")
            ranked.append((hashlib.sha256(rank_input).digest(), target_id.encode("utf-8"), target_id))
        for _, _, target_id in sorted(ranked):
            schedule.append(
                {
                    "position": len(schedule),
                    "iteration": iteration,
                    "target_id": target_id,
                }
            )
    return schedule


def execute_interleaved_run(
    target_ids: list[str],
    fixture_id: str,
    warmup_iterations: int,
    sample_count: int,
    seed: int,
    run_preflight: Callable[[str], None],
    run_warmup: Callable[[str, int], None],
    run_timed: Callable[[str, int], dict[str, Any]],
) -> tuple[dict[str, Any], dict[str, list[dict[str, Any]]]]:
    """Execute callbacks in the canonical cross-target phase schedules.

    This is deliberately not wired into the public single-target CLI yet. A
    future comparison coordinator must first construct fully attested target
    contexts, then use this primitive and retain the returned transcript.
    """

    if not isinstance(warmup_iterations, int) or isinstance(warmup_iterations, bool) or warmup_iterations < 0:
        raise ValueError("interleaved execution requires a nonnegative integer warmup count")
    if not isinstance(sample_count, int) or isinstance(sample_count, bool) or sample_count < 1:
        raise ValueError("interleaved execution requires a positive integer sample count")

    preflight = cross_target_phase_schedule(target_ids, fixture_id, 1, seed, "preflight")
    warmup = (
        cross_target_phase_schedule(target_ids, fixture_id, warmup_iterations, seed, "warmup")
        if warmup_iterations > 0
        else []
    )
    timed = cross_target_phase_schedule(target_ids, fixture_id, sample_count, seed, "timed")
    samples_by_target = {target_id: [] for target_id in sorted(target_ids, key=lambda value: value.encode("utf-8"))}

    for entry in preflight:
        run_preflight(entry["target_id"])
    for entry in warmup:
        run_warmup(entry["target_id"], entry["iteration"])
    for entry in timed:
        sample = run_timed(entry["target_id"], entry["iteration"])
        sample_index = sample.get("index") if isinstance(sample, dict) else None
        if not isinstance(sample_index, int) or isinstance(sample_index, bool) or sample_index != entry["iteration"]:
            fail(
                "interleaved runner returned a sample whose index does not match "
                f"the schedule for {entry['target_id']!r}"
            )
        samples_by_target[entry["target_id"]].append(sample)

    expected_indices = list(range(sample_count))
    for target_id, samples in samples_by_target.items():
        if [sample["index"] for sample in samples] != expected_indices:
            fail(f"interleaved runner did not retain one ordered sample per round for {target_id!r}")

    transcript = {
        "contract": CROSS_TARGET_SCHEDULE,
        "seed": seed,
        "fixture_id": fixture_id,
        "targets": sorted(target_ids, key=lambda value: value.encode("utf-8")),
        "preflight": preflight,
        "warmup": warmup,
        "timed": timed,
    }
    return transcript, samples_by_target


def windows_ram_bytes() -> int:
    try:
        import ctypes

        class MemoryStatusEx(ctypes.Structure):
            _fields_ = [
                ("dwLength", ctypes.c_ulong),
                ("dwMemoryLoad", ctypes.c_ulong),
                ("ullTotalPhys", ctypes.c_ulonglong),
                ("ullAvailPhys", ctypes.c_ulonglong),
                ("ullTotalPageFile", ctypes.c_ulonglong),
                ("ullAvailPageFile", ctypes.c_ulonglong),
                ("ullTotalVirtual", ctypes.c_ulonglong),
                ("ullAvailVirtual", ctypes.c_ulonglong),
                ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
            ]

        status = MemoryStatusEx()
        status.dwLength = ctypes.sizeof(MemoryStatusEx)
        if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status)):
            return int(status.ullTotalPhys)
    except Exception:
        pass
    return 0


def host_info(dedicated: bool) -> dict[str, Any]:
    info: dict[str, Any] = {
        "os": platform.system(),
        "arch": platform.machine(),
        "kernel": platform.release(),
        "cpu_model": platform.processor() or "",
        "cores": os_cpu_count(),
        "ram_bytes": 0,
        "dedicated": dedicated,
    }
    if platform.system() == "Linux":
        cpuinfo = Path("/proc/cpuinfo")
        if cpuinfo.is_file():
            for line in cpuinfo.read_text(encoding="utf-8", errors="replace").splitlines():
                if line.startswith("model name"):
                    info["cpu_model"] = line.split(":", 1)[1].strip()
                    break
        meminfo = Path("/proc/meminfo")
        if meminfo.is_file():
            for line in meminfo.read_text(encoding="utf-8", errors="replace").splitlines():
                if line.startswith("MemTotal"):
                    kib = int(line.split()[1])
                    info["ram_bytes"] = kib * 1024
                    break
    elif platform.system() == "Windows":
        info["ram_bytes"] = windows_ram_bytes()
    return info


def os_cpu_count() -> int:
    try:
        count = os.cpu_count()
        return count if count and count > 0 else 1
    except Exception:
        return 1


def run(command: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=600)


def engine_version(binary: Path) -> str:
    result = run([str(binary), "--version"])
    text = (result.stdout + result.stderr).strip()
    return text.splitlines()[0] if text else "unknown"


def engine_identity(binary: Path, target: dict[str, Any]) -> dict[str, Any]:
    reported_version = engine_version(binary)
    expected_version = f"pliego {target['version']}"
    if reported_version != expected_version:
        fail(f"binary reports {reported_version!r}, expected {expected_version!r}")
    identity: dict[str, Any] = {
        "name": "pliego",
        "version": target["version"],
        "binary_path": str(binary.resolve()),
    }
    digest = hashlib.sha256()
    size = 0
    with open(binary, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
            size += len(chunk)
    identity["binary_sha256"] = digest.hexdigest()
    identity["binary_bytes"] = size
    if identity["binary_sha256"] != target["binary_sha256"] or size != target["binary_bytes"]:
        fail(f"binary does not match the immutable {target['release_tag']} Linux release")
    identity.update(
        {
            "commit": target["commit"],
            "release_tag": target["release_tag"],
            "servo_build": target["servo_build"],
            "servo_base": target["servo_base"],
            "bundle": target["archive"],
            "bundle_sha256": target["archive_sha256"],
            "bundle_bytes": target["archive_bytes"],
            "profile": target["profile"],
        }
    )
    return identity


def json_command(command: list[str], label: str) -> dict[str, str]:
    result = run(command)
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"{label} returned invalid JSON: {error}")
    if (
        result.returncode != 0
        or not isinstance(value, dict)
        or not all(isinstance(key, str) and isinstance(item, str) for key, item in value.items())
    ):
        fail(f"{label} identity failed: {(result.stderr or result.stdout)[-2000:]}")
    return value


def adapter_identity(adapter: Path, target_id: str, target: dict[str, Any]) -> tuple[dict[str, Any], dict[str, str]]:
    resolved = adapter.resolve(strict=True)
    digest = file_sha256(resolved)
    identity = json_command([str(resolved), "identity"], f"adapter {target_id!r}")
    expected = {
        "contract": "pliego.benchmark-adapter.v1",
        "target": target_id,
        "package": target["package"],
        "package_version": target["version"],
        "adapter_path": str(resolved),
        "adapter_sha256": digest,
    }
    for key, value in expected.items():
        if identity.get(key) != value:
            fail(f"adapter {target_id!r} identity {key!r} must be {value!r}, got {identity.get(key)!r}")
    for lock_name in target.get("lockfiles", []):
        lock = (ROOT / lock_name).resolve(strict=True)
        key = f"{lock.name.removesuffix('.json').replace('-', '_').replace('.', '_')}_sha256"
        expected_hash = file_sha256(lock)
        if identity.get(key) != expected_hash:
            fail(f"adapter {target_id!r} did not identify exact lock file {lock_name!r}")
    for key in target.get("identity_keys", []):
        value = identity.get(key)
        if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
            fail(f"adapter {target_id!r} omitted dependency tree snapshot {key!r}")
        path_key = key.removesuffix("_sha256") + "_path"
        dependency_path = identity.get(path_key)
        if (
            not isinstance(dependency_path, str)
            or not Path(dependency_path).is_absolute()
            or Path(dependency_path).resolve(strict=True) != Path(dependency_path)
        ):
            fail(f"adapter {target_id!r} omitted canonical dependency path {path_key!r}")
    runtime_names = ["php", *(["node", "chrome"] if target.get("requires_network_isolation") else [])]
    for runtime in runtime_names:
        path = identity.get(f"{runtime}_path")
        runtime_digest = identity.get(f"{runtime}_sha256")
        version = identity.get(f"{runtime}_version")
        if not isinstance(path, str) or not Path(path).is_absolute() or not version:
            fail(f"adapter {target_id!r} omitted canonical {runtime} runtime identity")
        if not isinstance(runtime_digest, str) or re.fullmatch(r"[0-9a-f]{64}", runtime_digest) is None:
            fail(f"adapter {target_id!r} omitted {runtime} runtime SHA-256")
        if Path(path).resolve(strict=True) != Path(path):
            fail(f"adapter {target_id!r} {runtime} runtime path is not canonical")
    for key, expected in target.get("identity_sha256", {}).items():
        if identity.get(key) != expected:
            fail(f"adapter {target_id!r} identity {key!r} does not match the immutable image contract")
    for key, expected in target.get("identity_paths", {}).items():
        if identity.get(key) != expected:
            fail(f"adapter {target_id!r} path {key!r} does not match the immutable image contract")
    engine = {
        "name": target["package"].split("/")[-1],
        "version": target["version"],
        "package": target["package"],
        "binary_path": str(resolved),
        "binary_sha256": digest,
        "binary_bytes": resolved.stat().st_size,
        "profile": target["profile"],
    }
    return engine, identity


def mountinfo_path_is_read_only(path: PurePosixPath, mountinfo: str) -> bool:
    match: tuple[int, bool] | None = None
    for line in mountinfo.splitlines():
        before, _, _ = line.partition(" - ")
        fields = before.split()
        if len(fields) < 6:
            continue
        decoded = re.sub(r"\\([0-7]{3})", lambda value: chr(int(value.group(1), 8)), fields[4])
        mount = PurePosixPath(decoded)
        if path == mount or mount in path.parents:
            candidate = (len(mount.parts), "ro" in fields[5].split(","))
            if match is None or candidate[0] > match[0]:
                match = candidate
    return match[1] if match is not None else False


def path_mount_is_read_only(path: Path) -> bool:
    try:
        resolved = PurePosixPath(path.resolve(strict=True).as_posix())
        mountinfo = Path("/proc/self/mountinfo").read_text(encoding="utf-8")
    except OSError:
        return False
    return mountinfo_path_is_read_only(resolved, mountinfo)


def adapter_environment_identity(target: dict[str, Any]) -> tuple[dict[str, str], str | None]:
    contract = target.get("runtime_identity_contract")
    expected_image = target.get("image_digest")
    actual_image = os.environ.get("PLIEGO_BENCHMARK_IMAGE_DIGEST")
    root_read_only = path_mount_is_read_only(Path("/"))
    identity = {
        "contract": str(contract or "unavailable"),
        "image_digest": actual_image or "unavailable",
        "image_attestation": "unavailable",
        "rootfs_read_only": "true" if root_read_only else "false",
    }
    reasons: list[str] = [ADAPTER_ATTESTATION_REASON]
    if contract != "pliego.benchmark-runtime-image.v1":
        reasons.append("target omits the immutable runtime image contract")
    if not isinstance(expected_image, str) or re.fullmatch(r"sha256:[0-9a-f]{64}", expected_image) is None:
        reasons.append("manifest does not pin an immutable OCI image digest")
    elif actual_image != expected_image:
        reasons.append("running OCI image digest does not match the manifest")
    runtime_names = ["php", *(["node", "chrome"] if target.get("requires_network_isolation") else [])]
    required_hashes = [*target.get("identity_keys", []), *(f"{name}_sha256" for name in runtime_names)]
    required_paths = [
        "adapter_path",
        *(key.removesuffix("_sha256") + "_path" for key in target.get("identity_keys", [])),
        *(f"{name}_path" for name in runtime_names),
    ]
    hash_pins = target.get("identity_sha256", {})
    path_pins = target.get("identity_paths", {})
    if any(
        not isinstance(hash_pins.get(key), str) or re.fullmatch(r"[0-9a-f]{64}", hash_pins[key]) is None
        for key in required_hashes
    ):
        reasons.append("manifest does not pin canonical dependency and runtime SHA-256 values")
    if any(not isinstance(path_pins.get(key), str) or not path_pins[key].startswith("/") for key in required_paths):
        reasons.append("manifest does not pin canonical dependency and runtime paths")
    if not root_read_only:
        reasons.append("runtime root filesystem is not read-only")
    if target.get("requires_network_isolation"):
        probe = run([sys.executable, "-I", str(SAMPLER), "--probe-network-isolation"])
        if probe.returncode != 0:
            reasons.append("private network namespace is unavailable")
        else:
            try:
                proof = json.loads(probe.stdout)
            except json.JSONDecodeError:
                reasons.append("private network namespace probe returned invalid evidence")
            else:
                if proof.get("mode") != "linux-private-network-namespace-v1" or proof.get("interfaces") != ["lo"]:
                    reasons.append("private network namespace probe retained an external interface")
                else:
                    identity["network_isolation"] = proof["mode"]
    return identity, "; ".join(reasons) or None


def mutable_adapter_identity_paths(identity: dict[str, str], target: dict[str, Any]) -> list[str]:
    keys = ["adapter_path"]
    keys.extend(key.removesuffix("_sha256") + "_path" for key in target.get("identity_keys", []))
    keys.append("php_path")
    if target.get("requires_network_isolation"):
        keys.extend(("node_path", "chrome_path"))
    return [key for key in keys if not path_mount_is_read_only(Path(identity[key]))]


def pdf_oracle_identity() -> dict[str, str]:
    return json_command([sys.executable, "-I", str(PDF_ORACLE), "--identity"], "PDF oracle")


def pdf_oracle_manifest_pins_complete(manifest: dict[str, Any]) -> bool:
    expected = manifest.get("oracle", {})
    if expected.get("contract") != "pliego.pdf-oracle.v1":
        return False
    for tool in POPPLER_TOOLS:
        path = expected.get(f"{tool}_path")
        digest = expected.get(f"{tool}_sha256")
        version = expected.get(f"{tool}_version")
        if (
            not isinstance(path, str)
            or not PurePosixPath(path).is_absolute()
            or ".." in PurePosixPath(path).parts
            or str(PurePosixPath(path)) != path
            or not isinstance(digest, str)
            or re.fullmatch(r"[0-9a-f]{64}", digest) is None
            or not isinstance(version, str)
            or not version.strip()
        ):
            return False
    return True


def pdf_oracle_identity_reason(identity: dict[str, str], manifest: dict[str, Any]) -> str | None:
    if not pdf_oracle_manifest_pins_complete(manifest):
        return ORACLE_IDENTITY_REASON
    expected = manifest["oracle"]
    required = [
        "contract",
        *(f"{tool}_{field}" for tool in POPPLER_TOOLS for field in ("path", "sha256", "version")),
    ]
    if any(identity.get(key) != expected[key] for key in required):
        return "running Poppler identity does not match the canonical manifest pins"
    return None


def append_reason(current: str | None, added: str | None) -> str | None:
    return "; ".join(reason for reason in (current, added) if reason) or None


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def harness_revision() -> str | None:
    result = run(["git", "rev-parse", "HEAD"], ROOT)
    revision = result.stdout.strip()
    return revision if result.returncode == 0 and re.fullmatch(r"[0-9a-f]{40}", revision) else None


def benchmark_tree_is_clean() -> bool:
    result = run(
        [
            "git",
            "status",
            "--porcelain",
            "--",
            "benchmarks",
            ".github/workflows/pliego-benchmark.yml",
            ":(exclude)benchmarks/baselines",
        ],
        ROOT,
    )
    return result.returncode == 0 and not result.stdout.strip()


def fixture_identity(fixture: dict[str, Any]) -> tuple[str, str]:
    input_path = (ROOT / fixture["input"]).resolve()
    paths = [input_path, *(input_path.parent / asset for asset in fixture.get("assets", []))]
    digest = hashlib.sha256()
    for path in sorted(paths, key=lambda candidate: candidate.relative_to(input_path.parent).as_posix()):
        relative = path.relative_to(input_path.parent).as_posix()
        digest.update(relative.encode("utf-8") + b"\0")
        digest.update(bytes.fromhex(file_sha256(path)))
    return file_sha256(input_path), digest.hexdigest()


def tool_version(command: list[str]) -> str:
    try:
        result = run(command)
        text = (result.stdout + result.stderr).strip()
        return text.splitlines()[0] if text else ""
    except (OSError, subprocess.TimeoutExpired):
        return ""


def check_prep(fixture_id: str, fixture: dict[str, Any]) -> None:
    fixture_input = (ROOT / fixture["input"]).resolve()
    required = [fixture_input, *(fixture_input.parent / asset for asset in fixture.get("assets", []))]
    missing = [path for path in required if not path.is_file()]
    if missing:
        fail(f"fixture {fixture_id!r} is not prepared; missing {missing}")


def build_command(
    php: Path,
    fixture_id: str,
    fixture: dict[str, Any],
    binary: Path,
    samples: int | None,
    warmup: int | None,
    require_scene_report: bool = True,
    expected_fixture_identity: tuple[str, str] | None = None,
    isolate_network: bool = False,
    runner_phase: str = "full",
    sample_index: int | None = None,
) -> list[str]:
    # The engine resolves the input relative to the process cwd and rejects
    # absolute or parent-traversing paths (mirroring the PHP SDK). Run with
    # cwd = the input's own directory and pass the bare file name.
    input_path = (ROOT / fixture["input"]).resolve()
    input_sha256, bundle_sha256 = expected_fixture_identity or fixture_identity(fixture)
    if runner_phase not in {"full", "preflight", "warmup", "timed"}:
        raise ValueError(f"unsupported runner phase {runner_phase!r}")
    if runner_phase == "full":
        if samples is None or warmup is None or sample_index is not None:
            raise ValueError("full runner phase requires sample and warmup counts and no sample index")
        if not isinstance(samples, int) or isinstance(samples, bool) or samples < 1:
            raise ValueError("full runner phase requires a positive integer sample count")
        if not isinstance(warmup, int) or isinstance(warmup, bool) or warmup < 0:
            raise ValueError("full runner phase requires a nonnegative integer warmup count")
    elif samples is not None or warmup is not None:
        raise ValueError(f"{runner_phase} runner phase does not accept sample or warmup counts")
    if runner_phase in {"warmup", "timed"}:
        if not isinstance(sample_index, int) or isinstance(sample_index, bool) or sample_index < 0:
            raise ValueError(f"{runner_phase} runner phase requires a nonnegative sample index")
    elif sample_index is not None:
        raise ValueError(f"{runner_phase} runner phase does not accept a sample index")

    command = [
        str(php),
        str(RUNNER),
        "--binary",
        str(binary.resolve()),
        "--input",
        input_path.name,
        "--output",
        "document.pdf",
        "--artifacts",
        "artifacts",
        "--cwd",
        str(input_path.parent),
        "--fixture-input-sha256",
        input_sha256,
        "--fixture-bundle-sha256",
        bundle_sha256,
    ]
    if samples is not None:
        command += ["--samples", str(samples)]
    if warmup is not None:
        command += ["--warmup", str(warmup)]
    if runner_phase != "full":
        command += ["--runner-phase", runner_phase]
    if sample_index is not None:
        command += ["--sample-index", str(sample_index)]
    for asset in fixture.get("assets", []):
        command += ["--fixture-asset", asset]
    if isolate_network:
        command.append("--isolate-network")
    correctness = fixture.get("correctness", {})
    if require_scene_report:
        command.append("--require-scene-report")
    if "page_size" in fixture:
        command += ["--page-size", fixture["page_size"]]
    if "page_margins" in fixture:
        command += ["--page-margins", fixture["page_margins"]]
    if "page_count" in correctness:
        command += ["--page-count", str(correctness["page_count"])]
    if "text_contains" in correctness:
        for fragment in correctness["text_contains"]:
            command += ["--text-contains", fragment]
    if "text_equals" in correctness:
        command += ["--text-equals", correctness["text_equals"]]
    for family in correctness.get("font_families", []):
        command += ["--font-family", family]
    if "normalized_raster_sha256" in correctness:
        command += ["--raster-sha256", correctness["normalized_raster_sha256"]]
    for target in correctness.get("link_targets", []):
        command += ["--link-target", target]
    if "page_width_points" in correctness and "page_height_points" in correctness:
        command += [
            "--page-width-points",
            str(correctness["page_width_points"]),
            "--page-height-points",
            str(correctness["page_height_points"]),
            "--dimension-tolerance-points",
            str(correctness.get("dimension_tolerance_points", 0.5)),
        ]
    if fixture.get("expect_failure"):
        command.append("--expect-failure")
        if "failure_code" in correctness:
            command += ["--expected-code", correctness["failure_code"]]
    return command


def invoke_runner(
    command: list[str],
    fixture_id: str,
    fixture: dict[str, Any],
    original_identity: tuple[str, str],
    expected_indices: list[int],
    timeout: int,
) -> list[dict[str, Any]]:
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        fail(f"runner failed for {fixture_id!r}: {result.stderr[-2000:]}")

    samples_out: list[dict[str, Any]] = []
    for line in (line for line in result.stdout.splitlines() if line.strip()):
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            fail(f"runner emitted a non-JSON stdout line for {fixture_id!r}: {line[:200]}")
        if not isinstance(parsed, dict) or "wall_ms" not in parsed:
            fail(f"runner emitted a non-sample stdout value for {fixture_id!r}")
        samples_out.append(parsed)

    actual_indices = [sample.get("index") for sample in samples_out]
    indices_are_integers = all(isinstance(index, int) and not isinstance(index, bool) for index in actual_indices)
    if not indices_are_integers or actual_indices != expected_indices:
        fail(
            f"runner produced sample indices {actual_indices!r}, expected {expected_indices!r} "
            f"for {fixture_id!r}: {result.stderr[-2000:]}"
        )
    if fixture_identity(fixture) != original_identity:
        fail(f"fixture {fixture_id!r} changed during rendering")
    return samples_out


def collect_samples(
    php: Path,
    fixture_id: str,
    fixture: dict[str, Any],
    binary: Path,
    samples: int,
    warmup: int,
    require_scene_report: bool = True,
    expected_fixture_identity: tuple[str, str] | None = None,
    isolate_network: bool = False,
) -> list[dict[str, Any]]:
    original_identity = expected_fixture_identity or fixture_identity(fixture)
    command = build_command(
        php,
        fixture_id,
        fixture,
        binary,
        samples,
        warmup,
        require_scene_report,
        original_identity,
        isolate_network,
    )
    return invoke_runner(
        command,
        fixture_id,
        fixture,
        original_identity,
        list(range(samples)),
        max(600, samples * 60),
    )


def run_runner_phase(
    php: Path,
    fixture_id: str,
    fixture: dict[str, Any],
    binary: Path,
    runner_phase: str,
    sample_index: int | None = None,
    require_scene_report: bool = True,
    expected_fixture_identity: tuple[str, str] | None = None,
    isolate_network: bool = False,
) -> dict[str, Any] | None:
    """Run exactly one internal preflight, warmup, or timed phase."""

    if runner_phase not in {"preflight", "warmup", "timed"}:
        raise ValueError("run_runner_phase accepts only preflight, warmup, or timed")
    original_identity = expected_fixture_identity or fixture_identity(fixture)
    command = build_command(
        php,
        fixture_id,
        fixture,
        binary,
        None,
        None,
        require_scene_report,
        original_identity,
        isolate_network,
        runner_phase,
        sample_index,
    )
    expected_indices = [sample_index] if runner_phase == "timed" and sample_index is not None else []
    samples = invoke_runner(command, fixture_id, fixture, original_identity, expected_indices, 600)
    return samples[0] if samples else None


def aggregates(samples: list[dict[str, Any]], page_count: int | None) -> dict[str, Any]:
    valid = [s for s in samples if s.get("ok") and (s.get("correctness") or {}).get("pass")]
    walls = [s["wall_ms"] for s in valid]
    one_shot_walls = [s["one_shot_wall_ms"] for s in valid]
    users = [value for s in valid if (value := s.get("user_ms")) is not None]
    syss = [value for s in valid if (value := s.get("sys_ms")) is not None]
    memory_peaks = [value for s in valid if (value := s.get("memory_peak_bytes")) is not None]
    rss = [value for s in valid if (value := s.get("sampled_peak_rss_kib_lower_bound")) is not None]
    pss = [value for s in valid if (value := s.get("sampled_peak_pss_kib_lower_bound")) is not None]
    reads = [value for s in valid if (value := s.get("read_bytes")) is not None]
    writes = [value for s in valid if (value := s.get("write_bytes")) is not None]
    read_operations = [value for s in valid if (value := s.get("read_operations")) is not None]
    write_operations = [value for s in valid if (value := s.get("write_operations")) is not None]
    pdfs = [s.get("output", {}).get("pdf_bytes") for s in valid]
    sha256s = [s.get("output", {}).get("pdf_sha256") for s in valid]
    pdf_hashes = [h for h in sha256s if h]
    identical = max(
        (sum(1 for h in pdf_hashes if h == candidate) for candidate in set(pdf_hashes)),
        default=0,
    )
    failures = [s for s in samples if s.get("ok") is False]
    codes: dict[str, int] = {}
    for sample in failures:
        code = (sample.get("failure") or {}).get("code") or "unknown"
        codes[code] = codes.get(code, 0) + 1
    passed = sum(1 for s in samples if (s.get("correctness") or {}).get("pass"))
    variants = len(set(pdf_hashes))
    mean_wall = statistics.fmean(walls) if walls else 0.0
    mean_one_shot_wall = statistics.fmean(one_shot_walls) if one_shot_walls else 0.0
    agg: dict[str, Any] = {
        "latency": validate_result.percentiles(walls),
        "cpu": {
            "user_ms": validate_result.percentiles(users),
            "sys_ms": validate_result.percentiles(syss),
            "wall_ms": validate_result.percentiles(walls),
        },
        "memory": {
            "cgroup_peak_bytes": validate_result.percentiles(memory_peaks),
            "sampled_peak_rss_kib_lower_bound": validate_result.percentiles(rss),
        },
        "io": {
            "read_bytes": validate_result.percentiles(reads),
            "write_bytes": validate_result.percentiles(writes),
            "read_operations": validate_result.percentiles(read_operations),
            "write_operations": validate_result.percentiles(write_operations),
        },
        "output": {
            "pdf_bytes": validate_result.percentiles(pdfs),
            "page_count": page_count,
        },
        "correctness": {
            "pass_count": passed,
            "total": len(samples),
            "passed": passed == len(samples),
        },
        "determinism": {
            "identical_pdf_sha256": identical,
            "total": len(pdf_hashes),
            "pdf_sha256_variants": variants,
        },
        "failures": {"count": len(failures), "codes": codes},
    }
    if pss and len(pss) == len(valid):
        agg["memory"]["sampled_peak_pss_kib_lower_bound"] = validate_result.percentiles(pss)
    if page_count:
        agg["scaling"] = {
            "per_page_wall_ms": round(mean_wall / page_count, 3),
            "per_page_memory_peak_bytes": (
                round(validate_result.percentiles(memory_peaks)["mean"] / page_count, 1) if memory_peaks else 0.0
            ),
        }
    if mean_one_shot_wall > 0:
        agg["throughput"] = {
            "renders_per_minute": round(60_000 / mean_one_shot_wall, 2),
            "concurrency": 1,
            "mean_one_shot_wall_ms": round(mean_one_shot_wall, 3),
            "measurement_boundary": "runner-process-open-through-sampler-exit",
        }
    return agg


def fixture_record(
    fixture_id: str,
    fixture: dict[str, Any],
    include_hashes: bool,
    identity: tuple[str, str] | None = None,
) -> dict[str, Any]:
    correctness = fixture.get("correctness", {})
    record: dict[str, Any] = {
        "id": fixture_id,
        "purpose": fixture["purpose"],
        "category": fixture["category"],
        "input": fixture["input"],
        "expected_page_count": correctness.get("page_count"),
        "expected_page_width_points": correctness.get("page_width_points"),
        "expected_page_height_points": correctness.get("page_height_points"),
        "dimension_tolerance_points": correctness.get("dimension_tolerance_points"),
        "expected_text_contains": correctness.get("text_contains", []),
        "expected_text": correctness.get("text_equals"),
        "expected_font_families": correctness.get("font_families", []),
        "expected_normalized_raster_sha256": correctness.get("normalized_raster_sha256"),
        "expected_link_targets": correctness.get("link_targets", []),
        "expected_failure_code": correctness.get("failure_code"),
    }
    if include_hashes:
        input_sha256, bundle_sha256 = identity or fixture_identity(fixture)
        record["input_sha256"] = input_sha256
        record["bundle_sha256"] = bundle_sha256
    return record


def not_applicable_result(
    *,
    generated_at: str,
    host: dict[str, Any],
    toolchain: dict[str, Any],
    protocol: dict[str, Any],
    target_id: str,
    target: dict[str, Any],
    fixture_id: str,
    fixture: dict[str, Any],
    reason: str | None = None,
) -> dict[str, Any]:
    reasons = target.get("not_applicable", {})
    reason = reason or reasons.get(fixture_id, target.get("unsupported_reason"))
    if not isinstance(reason, str) or not reason.strip():
        fail(f"target {target_id!r} must declare a reason for unsupported fixture {fixture_id!r}")
    return {
        "schema": "pliego.benchmark-result",
        "version": 1,
        "status": "not-applicable",
        "reason": reason,
        "generated_at": generated_at,
        "host": host,
        "toolchain": toolchain,
        "protocol": {
            "warmup_iterations": 0,
            "sample_count": 0,
            "sample_order": protocol["sample_order"],
            "seed": protocol.get("seed"),
            "network": protocol["network"],
            "binary_profile": target.get("profile", protocol["binary_profile"]),
            "measurement_method": "unavailable",
            "percentile_method": validate_result.PERCENTILE_METHOD,
        },
        "target": {"id": target_id, "label": target["label"]},
        "fixture": fixture_record(fixture_id, fixture, include_hashes=False),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Pliego benchmark orchestrator")
    parser.add_argument("--binary", help="path to the published Pliego binary (Pliego targets only)")
    parser.add_argument("--out", help="result file path (default: baselines/pliego-<target>-<host>.json)")
    parser.add_argument("--fixture", action="append", help="restrict to these fixture ids (repeatable)")
    parser.add_argument("--samples", type=int, help="override samples per fixture")
    parser.add_argument("--warmup", type=int, help="override warmup iterations")
    parser.add_argument("--php", default="php", help="php-cli binary (default: php)")
    parser.add_argument("--target", default="pliego-0.2.0", help="manifest target id")
    parser.add_argument("--dedicated", action="store_true", help="host is dedicated to the run")
    parser.add_argument("--schema", default=str(DEFAULT_SCHEMA), help="result schema path")
    args = parser.parse_args()

    if args.samples is not None and args.samples < 1:
        fail("--samples must be at least 1")
    if args.warmup is not None and args.warmup < 0:
        fail("--warmup cannot be negative")
    benchmark_clean = benchmark_tree_is_clean()
    original_harness_revision = harness_revision() if benchmark_clean else None
    if args.dedicated and not benchmark_clean:
        fail(
            "--dedicated requires a clean benchmarks tree so the recorded revision identifies the harness and fixtures"
        )
    if args.dedicated and original_harness_revision is None:
        fail("--dedicated requires an exact 40-character harness commit")

    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    target = manifest["targets"].get(args.target)
    if not target or not target.get("enabled", False):
        fail(f"target {args.target!r} is not enabled in {MANIFEST}")

    protocol = manifest["protocol"]
    php = Path(args.php)
    schema = Path(args.schema)
    fixture_ids = args.fixture or sorted(manifest["fixtures"].keys())
    if args.fixture is None and protocol.get("seed") is not None:
        random.Random(protocol["seed"]).shuffle(fixture_ids)

    target_kind = target.get("kind", "pliego")
    identity_details: dict[str, str] = {}
    unavailable_reason: str | None = None
    adapter_identity_snapshot: dict[str, str] | None = None
    if target_kind == "pliego":
        if args.binary is None:
            fail(f"target {args.target!r} requires --binary")
        binary = Path(args.binary)
        if not binary.is_file():
            fail(f"binary not found: {binary}")
        engine = engine_identity(binary, target)
        mapped_supported_fixtures = set(manifest["fixtures"])
        supported_fixtures = set(mapped_supported_fixtures)
        require_scene_report = True
    elif target_kind == "adapter":
        if args.binary is not None:
            fail(f"target {args.target!r} uses its committed adapter; --binary is not accepted")
        binary = (ROOT / target["adapter"]).resolve()
        if not binary.is_file():
            fail(f"adapter not found: {binary}")
        environment, unavailable_reason = adapter_environment_identity(target)
        identity_details.update({f"environment.{key}": value for key, value in environment.items()})
        if unavailable_reason is None:
            engine, adapter_details = adapter_identity(binary, args.target, target)
            adapter_identity_snapshot = adapter_details
            identity_details.update({f"adapter.{key}": value for key, value in adapter_details.items()})
            mutable_paths = mutable_adapter_identity_paths(adapter_details, target)
            protected_keys = ["adapter_path"]
            protected_keys.extend(key.removesuffix("_sha256") + "_path" for key in target.get("identity_keys", []))
            protected_keys.append("php_path")
            if target.get("requires_network_isolation"):
                protected_keys.extend(("node_path", "chrome_path"))
            for key in protected_keys:
                identity_details[f"environment.read_only.{key}"] = (
                    "true" if path_mount_is_read_only(Path(adapter_details[key])) else "false"
                )
            if mutable_paths:
                unavailable_reason = "adapter identity paths are not read-only: " + ", ".join(mutable_paths)
        else:
            engine = {
                "name": target["package"].split("/")[-1],
                "version": target["version"],
                "package": target["package"],
                "profile": target["profile"],
            }
        mapped_supported_fixtures = set(target.get("supported_fixtures", []))
        supported_fixtures = set(mapped_supported_fixtures) if unavailable_reason is None else set()
        require_scene_report = False
    else:
        fail(f"target {args.target!r} has unsupported kind {target_kind!r}")
    actual_version = f"{engine['name']} {engine['version']}"

    oracle_identity_snapshot: dict[str, str] | None = None
    if any(fixture_id in mapped_supported_fixtures for fixture_id in fixture_ids):
        oracle_details: dict[str, str] | None = None
        if pdf_oracle_manifest_pins_complete(manifest):
            oracle_details = pdf_oracle_identity()
            identity_details.update({f"oracle.{key}": value for key, value in oracle_details.items()})
        oracle_reason = pdf_oracle_identity_reason(oracle_details or {}, manifest)
        unavailable_reason = append_reason(unavailable_reason, oracle_reason)
        if unavailable_reason is None:
            assert oracle_details is not None
            oracle_identity_snapshot = oracle_details
        else:
            supported_fixtures.clear()

    generated_at = datetime.now(timezone.utc).isoformat()
    host = host_info(args.dedicated)
    revision = original_harness_revision
    toolchain = {
        "engine": engine,
        "python_version": platform.python_version(),
        "php_version": tool_version([str(php), "--version"]),
    }
    if identity_details:
        toolchain["competitors"] = identity_details
    if revision:
        toolchain["harness_revision"] = revision

    print(f"host: {platform.system()} {platform.machine()} ({os_cpu_count()} cores)")
    print(f"target: {actual_version}")

    results: list[dict[str, Any]] = []
    for fixture_id in fixture_ids:
        fixture = manifest["fixtures"].get(fixture_id)
        if fixture is None:
            fail(f"unknown fixture {fixture_id!r}")
        if fixture_id not in supported_fixtures:
            reason = unavailable_reason if fixture_id in mapped_supported_fixtures else None
            result = not_applicable_result(
                generated_at=generated_at,
                host=host,
                toolchain=toolchain,
                protocol=protocol,
                target_id=args.target,
                target=target,
                fixture_id=fixture_id,
                fixture=fixture,
                reason=reason,
            )
            results.append(result)
            print(f"[{fixture_id}] not applicable: {result['reason']}")
            continue
        check_prep(fixture_id, fixture)
        if not args.dedicated or not benchmark_clean or platform.system() != "Linux" or platform.machine() != "x86_64":
            fail("supported benchmark results require --dedicated on clean Linux x86_64")
        original_fixture_identity = fixture_identity(fixture)
        samples_n = fixture.get("samples", protocol["samples_short"])
        warmup_n = protocol["warmup_iterations"]
        if args.samples not in (None, samples_n) or args.warmup not in (None, warmup_n):
            fail("sample and warmup overrides cannot produce canonical benchmark results")
        print(
            f"[{fixture_id}] {fixture['purpose']} "
            f"(1 untimed correctness preflight, {warmup_n} warmup, {samples_n} timed samples)"
        )
        samples = collect_samples(
            php,
            fixture_id,
            fixture,
            binary,
            samples_n,
            warmup_n,
            require_scene_report,
            original_fixture_identity,
            bool(target.get("requires_network_isolation")),
        )
        if adapter_identity_snapshot is not None:
            _, after_identity = adapter_identity(binary, args.target, target)
            if after_identity != adapter_identity_snapshot:
                fail(f"adapter {args.target!r} runtime or dependency identity changed during rendering")
        correctness = fixture.get("correctness", {})
        page_count = correctness.get("page_count")
        measured_pages = [
            sample.get("output", {}).get("page_count")
            for sample in samples
            if sample.get("output", {}).get("page_count")
        ]
        if page_count is None and measured_pages:
            page_count = int(statistics.median(measured_pages))
        aggregate = aggregates(samples, page_count)
        result: dict[str, Any] = {
            "schema": "pliego.benchmark-result",
            "version": 1,
            "status": "supported" if aggregate["correctness"]["passed"] else "failed",
            "generated_at": generated_at,
            "host": host,
            "toolchain": toolchain,
            "protocol": {
                "warmup_iterations": warmup_n,
                "correctness_preflight_iterations": 1,
                "execution_order": "untimed-correctness-preflight,warmup,timed",
                "sample_count": len(samples),
                "sample_order": protocol["sample_order"],
                "seed": protocol.get("seed"),
                "network": protocol["network"],
                "binary_profile": target.get("profile", protocol["binary_profile"]),
                "measurement_method": samples[0].get("measurement_method", "unavailable"),
                "percentile_method": validate_result.PERCENTILE_METHOD,
            },
            "target": {"id": args.target, "label": target["label"]},
            "fixture": fixture_record(
                fixture_id,
                fixture,
                include_hashes=True,
                identity=original_fixture_identity,
            ),
            "samples": samples,
            "aggregates": aggregate,
        }
        results.append(result)
        print(
            f"  -> p50 {result['aggregates']['latency']['p50']:.1f} ms, "
            f"p95 {result['aggregates']['latency']['p95']:.1f} ms, "
            f"correctness {result['aggregates']['correctness']['pass_count']}/{result['aggregates']['correctness']['total']}"
        )

    if oracle_identity_snapshot is not None:
        if not benchmark_tree_is_clean() or harness_revision() != original_harness_revision:
            fail("benchmark harness changed during execution")
        if pdf_oracle_identity() != oracle_identity_snapshot:
            fail("PDF oracle identity changed during execution")

    if args.out:
        out = Path(args.out)
    else:
        safe_host = platform.system().lower().replace(" ", "-")
        out = ROOT / "benchmarks" / "baselines" / f"{args.target}-{safe_host}-{platform.machine().lower()}.json"
    payload = (
        results[0]
        if len(results) == 1
        else {
            "schema": "pliego.benchmark-bundle",
            "version": 1,
            "generated_at": generated_at,
            "results": results,
        }
    )
    violations = validate_result.validate_document(payload, json.loads(schema.read_text(encoding="utf-8")))
    if violations:
        fail(f"benchmark output failed validation: {violations[0]}")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {out}")
    failed = [
        result["fixture"]["id"]
        for result in results
        if result.get("status") == "failed"
        or (result.get("status") == "supported" and not result["aggregates"]["correctness"]["passed"])
    ]
    if failed:
        print(f"correctness gate failed: {', '.join(failed)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
