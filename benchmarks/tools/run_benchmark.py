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
        [--target pliego-0.1.1|dompdf-3.1.6|browsershot-...] \
        [--out benchmarks/baselines/pliego-0.1.1-linux-x86_64.json] \
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
import statistics
import subprocess
import sys
import tomllib
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, NoReturn

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "benchmarks" / "manifest.toml"
RUNNER = ROOT / "benchmarks" / "runners" / "pliego.php"
PDF_ORACLE = ROOT / "benchmarks" / "tools" / "pdf_oracle.py"
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_result  # noqa: E402


def fail(message: str) -> NoReturn:
    print(f"run_benchmark: {message}", file=sys.stderr)
    raise SystemExit(1)


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
    identity: dict[str, Any] = {
        "name": "pliego",
        "version": engine_version(binary),
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


def pdf_oracle_identity() -> dict[str, str]:
    return json_command([sys.executable, "-I", str(PDF_ORACLE), "--identity"], "PDF oracle")


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def harness_revision() -> str | None:
    result = run(["git", "rev-parse", "HEAD"], ROOT)
    revision = result.stdout.strip()
    return revision if result.returncode == 0 and 7 <= len(revision) <= 64 else None


def benchmark_tree_is_clean() -> bool:
    result = run(["git", "status", "--porcelain", "--", "benchmarks"], ROOT)
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
    samples: int,
    warmup: int,
    require_scene_report: bool = True,
) -> list[str]:
    # The engine resolves the input relative to the process cwd and rejects
    # absolute or parent-traversing paths (mirroring the PHP SDK). Run with
    # cwd = the input's own directory and pass the bare file name.
    input_path = (ROOT / fixture["input"]).resolve()
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
        "--samples",
        str(samples),
        "--warmup",
        str(warmup),
        "--cwd",
        str(input_path.parent),
    ]
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


def collect_samples(
    php: Path,
    fixture_id: str,
    fixture: dict[str, Any],
    binary: Path,
    samples: int,
    warmup: int,
    require_scene_report: bool = True,
) -> list[dict[str, Any]]:
    command = build_command(php, fixture_id, fixture, binary, samples, warmup, require_scene_report)
    result = subprocess.run(command, capture_output=True, text=True, timeout=max(600, samples * 60))
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    samples_out: list[dict[str, Any]] = []
    for line in lines:
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            print(f"runner emitted a non-JSON line: {line[:200]}", file=sys.stderr)
            continue
        if isinstance(parsed, dict) and "wall_ms" in parsed:
            samples_out.append(parsed)
    if result.returncode != 0:
        fail(f"runner failed for {fixture_id!r}: {result.stderr[-2000:]}")
    if len(samples_out) != samples:
        fail(f"runner produced {len(samples_out)} of {samples} samples for {fixture_id!r}: {result.stderr[-2000:]}")
    return samples_out


def aggregates(samples: list[dict[str, Any]], page_count: int | None) -> dict[str, Any]:
    valid = [s for s in samples if s.get("ok") and (s.get("correctness") or {}).get("pass")]
    walls = [s["wall_ms"] for s in valid]
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
    if mean_wall > 0:
        agg["throughput"] = {"renders_per_minute": round(60_000 / mean_wall, 2), "concurrency": 1}
    return agg


def fixture_record(fixture_id: str, fixture: dict[str, Any], include_hashes: bool) -> dict[str, Any]:
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
        "expected_link_targets": correctness.get("link_targets", []),
        "expected_failure_code": correctness.get("failure_code"),
    }
    if include_hashes:
        input_sha256, bundle_sha256 = fixture_identity(fixture)
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
) -> dict[str, Any]:
    reasons = target.get("not_applicable", {})
    reason = reasons.get(fixture_id, target.get("unsupported_reason"))
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
    parser.add_argument("--target", default="pliego-0.1.1", help="manifest target id")
    parser.add_argument("--dedicated", action="store_true", help="host is dedicated to the run")
    parser.add_argument("--schema", default=str(DEFAULT_SCHEMA), help="result schema path")
    args = parser.parse_args()

    if args.samples is not None and args.samples < 1:
        fail("--samples must be at least 1")
    if args.warmup is not None and args.warmup < 0:
        fail("--warmup cannot be negative")
    benchmark_clean = benchmark_tree_is_clean()
    if args.dedicated and not benchmark_clean:
        fail(
            "--dedicated requires a clean benchmarks tree so the recorded revision identifies the harness and fixtures"
        )

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
    if target_kind == "pliego":
        if args.binary is None:
            fail(f"target {args.target!r} requires --binary")
        binary = Path(args.binary)
        if not binary.is_file():
            fail(f"binary not found: {binary}")
        actual_version = engine_version(binary)
        if actual_version != f"pliego {target['version']}":
            fail(f"target {args.target!r} requires pliego {target['version']}, got {actual_version!r}")
        engine = engine_identity(binary, target)
        supported_fixtures = set(manifest["fixtures"])
        require_scene_report = True
    elif target_kind == "adapter":
        if args.binary is not None:
            fail(f"target {args.target!r} uses its committed adapter; --binary is not accepted")
        binary = (ROOT / target["adapter"]).resolve()
        if not binary.is_file():
            fail(f"adapter not found: {binary}")
        engine, adapter_details = adapter_identity(binary, args.target, target)
        identity_details.update({f"adapter.{key}": value for key, value in adapter_details.items()})
        actual_version = f"{engine['name']} {engine['version']}"
        supported_fixtures = set(target.get("supported_fixtures", []))
        require_scene_report = False
    else:
        fail(f"target {args.target!r} has unsupported kind {target_kind!r}")

    if any(fixture_id in supported_fixtures for fixture_id in fixture_ids):
        oracle_details = pdf_oracle_identity()
        identity_details.update({f"oracle.{key}": value for key, value in oracle_details.items()})

    generated_at = datetime.now(timezone.utc).isoformat()
    host = host_info(args.dedicated)
    revision = harness_revision() if benchmark_clean else None
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
            result = not_applicable_result(
                generated_at=generated_at,
                host=host,
                toolchain=toolchain,
                protocol=protocol,
                target_id=args.target,
                target=target,
                fixture_id=fixture_id,
                fixture=fixture,
            )
            results.append(result)
            print(f"[{fixture_id}] not applicable: {result['reason']}")
            continue
        check_prep(fixture_id, fixture)
        samples_n = args.samples if args.samples else fixture.get("samples", protocol["samples_short"])
        warmup_n = args.warmup if args.warmup is not None else protocol["warmup_iterations"]
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
        )
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
            "fixture": fixture_record(fixture_id, fixture, include_hashes=True),
            "samples": samples,
            "aggregates": aggregate,
        }
        results.append(result)
        print(
            f"  -> p50 {result['aggregates']['latency']['p50']:.1f} ms, "
            f"p95 {result['aggregates']['latency']['p95']:.1f} ms, "
            f"correctness {result['aggregates']['correctness']['pass_count']}/{result['aggregates']['correctness']['total']}"
        )

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
