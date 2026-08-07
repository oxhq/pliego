#!/usr/bin/env python3

"""B0 benchmark orchestrator.

Reads `benchmarks/manifest.toml`, drives `benchmarks/runners/pliego.php` for
each enabled fixture against a published Pliego binary, aggregates raw samples
into a result document, and validates it against
`schema/benchmark-result.v1.json` before writing it.

Usage:
    python3 benchmarks/tools/run_benchmark.py \
        --binary /path/to/pliego \
        [--out benchmarks/baselines/pliego-0.1.1-linux-x86_64.json] \
        [--fixture invoice-showcase] [--samples N] [--warmup N] \
        [--php /usr/bin/php] [--dedicated]

Design notes:
* The PHP runner is the only component that spawns the engine; this script
  never touches the binary directly except to read its version.
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
import tempfile
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "benchmarks" / "manifest.toml"
RUNNER = ROOT / "benchmarks" / "runners" / "pliego.php"
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_result  # noqa: E402


def fail(message: str) -> None:
    print(f"run_benchmark: {message}", file=sys.stderr)
    raise SystemExit(1)


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, round(p / 100.0 * (len(ordered) - 1))))
    return float(ordered[index])


def percentiles(values: list[float]) -> dict[str, float]:
    values = [v for v in values if v is not None]
    if not values:
        return {"min": 0.0, "p50": 0.0, "p95": 0.0, "p99": 0.0, "max": 0.0, "mean": 0.0}
    return {
        "min": min(values),
        "p50": percentile(values, 50),
        "p95": percentile(values, 95),
        "p99": percentile(values, 99),
        "max": max(values),
        "mean": statistics.fmean(values),
    }


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


def engine_identity(binary: Path) -> dict[str, Any]:
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
    return identity


def tool_version(command: list[str]) -> str:
    try:
        result = run(command)
        return (result.stdout + result.stderr).strip().splitlines()[0] if result.stdout.strip() else ""
    except (OSError, subprocess.TimeoutExpired):
        return ""


def check_prep(fixture_id: str, fixture: dict[str, Any]) -> None:
    if fixture_id == "chartjs-showcase":
        base = ROOT / "ports" / "pliego" / "tests" / "fixtures" / "chartjs-report"
        missing = [
            p for p in (base / "node_modules" / "chart.js" / "dist" / "chart.umd.js", base / "ReportSans.ttf")
            if not p.is_file()
        ]
        if missing:
            fail(
                f"fixture {fixture_id!r} is not prepared; missing {missing}. "
                f"Run: npm ci in {base} and copy DejaVuSans.ttf to ReportSans.ttf there."
            )
    for generated in ("ledger-20-pages", "statement-100-pages", "font-image-heavy"):
        if fixture_id == generated:
            fixture_input = ROOT / fixture["input"]
            if not fixture_input.is_file():
                fail(
                    f"fixture {fixture_id!r} is missing {fixture_input.relative_to(ROOT)}; "
                    "run: python3 benchmarks/tools/generate_fixtures.py"
                )


def build_command(
    php: Path, fixture_id: str, fixture: dict[str, Any], binary: Path, samples: int, warmup: int
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
    if "page_count" in correctness:
        command += ["--page-count", str(correctness["page_count"])]
    if "text_contains" in correctness:
        command += ["--text-contains", ",".join(correctness["text_contains"])]
    if fixture.get("expect_failure"):
        command.append("--expect-failure")
        if "failure_code" in correctness:
            command += ["--expected-code", correctness["failure_code"]]
    return command


def collect_samples(
    php: Path, fixture_id: str, fixture: dict[str, Any], binary: Path, samples: int, warmup: int
) -> list[dict[str, Any]]:
    command = build_command(php, fixture_id, fixture, binary, samples, warmup)
    started = time.monotonic()
    result = subprocess.run(command, capture_output=True, text=True, timeout=max(600, samples * 60))
    wall_s = time.monotonic() - started
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
    if not samples_out:
        fail(
            f"runner produced no samples for {fixture_id!r}: {result.stderr[-2000:]}"
        )
    return samples_out


def aggregates(samples: list[dict[str, Any]], page_count: int | None) -> dict[str, Any]:
    walls = [s["wall_ms"] for s in samples]
    users = [s.get("user_ms") for s in samples]
    syss = [s.get("sys_ms") for s in samples]
    rss = [s.get("peak_rss_kib") for s in samples]
    pdfs = [s.get("output", {}).get("pdf_bytes") for s in samples]
    sha256s = [s.get("output", {}).get("pdf_sha256") for s in samples]
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
        "latency": percentiles(walls),
        "cpu": {
            "user_ms": percentiles(users),
            "sys_ms": percentiles(syss),
            "wall_ms": percentiles(walls),
        },
        "memory": {"peak_rss_kib": percentiles(rss)},
        "output": {
            "pdf_bytes": percentiles(pdfs),
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
    if page_count:
        agg["scaling"] = {
            "per_page_wall_ms": round(mean_wall / page_count, 3),
            "per_page_peak_rss_kib": (
                round(percentiles(rss)["mean"] / page_count, 1) if rss else 0.0
            ),
        }
    if mean_wall > 0:
        agg["throughput"] = {"renders_per_minute": round(60_000 / mean_wall, 2), "concurrency": 1}
    return agg


def main() -> int:
    parser = argparse.ArgumentParser(description="Pliego B0 benchmark orchestrator")
    parser.add_argument("--binary", required=True, help="path to the published pliego binary")
    parser.add_argument("--out", help="result file path (default: baselines/pliego-<target>-<host>.json)")
    parser.add_argument("--fixture", action="append", help="restrict to these fixture ids (repeatable)")
    parser.add_argument("--samples", type=int, help="override samples per fixture")
    parser.add_argument("--warmup", type=int, help="override warmup iterations")
    parser.add_argument("--php", default="php", help="php-cli binary (default: php)")
    parser.add_argument("--target", default="pliego-0.1.1", help="manifest target id")
    parser.add_argument("--dedicated", action="store_true", help="host is dedicated to the run")
    parser.add_argument("--schema", default=str(DEFAULT_SCHEMA), help="result schema path")
    args = parser.parse_args()

    binary = Path(args.binary)
    if not binary.is_file():
        fail(f"binary not found: {binary}")

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

    print(f"host: {platform.system()} {platform.machine()} ({os_cpu_count()} cores)")
    print(f"engine: {engine_version(binary)}")

    results: list[dict[str, Any]] = []
    for fixture_id in fixture_ids:
        fixture = manifest["fixtures"].get(fixture_id)
        if fixture is None:
            fail(f"unknown fixture {fixture_id!r}")
        check_prep(fixture_id, fixture)
        samples_n = args.samples if args.samples else fixture.get("samples", protocol["samples_short"])
        warmup_n = args.warmup if args.warmup is not None else protocol["warmup_iterations"]
        print(f"[{fixture_id}] {fixture['purpose']} ({samples_n} samples, {warmup_n} warmup)")
        samples = collect_samples(php, fixture_id, fixture, binary, samples_n, warmup_n)
        correctness = fixture.get("correctness", {})
        page_count = correctness.get("page_count")
        measured_pages = [
            sample.get("output", {}).get("page_count")
            for sample in samples
            if sample.get("output", {}).get("page_count")
        ]
        if page_count is None and measured_pages:
            page_count = int(statistics.median(measured_pages))
        result: dict[str, Any] = {
            "schema": "pliego.benchmark-result",
            "version": 1,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "host": host_info(args.dedicated),
            "toolchain": {
                "engine": engine_identity(binary),
                "python_version": platform.python_version(),
                "php_version": tool_version([str(php), "--version"]),
            },
            "protocol": {
                "warmup_iterations": warmup_n,
                "sample_count": len(samples),
                "sample_order": protocol["sample_order"],
                "seed": protocol.get("seed"),
                "network": protocol["network"],
                "binary_profile": protocol["binary_profile"],
                "rss_method": samples[0].get("rss_method", "unavailable"),
            },
            "target": {"id": args.target, "label": target["label"]},
            "fixture": {
                "id": fixture_id,
                "purpose": fixture["purpose"],
                "category": fixture["category"],
                "input": fixture["input"],
                "expected_page_count": page_count,
                "expected_failure_code": correctness.get("failure_code"),
            },
            "samples": samples,
            "aggregates": aggregates(samples, page_count),
        }
        violations: list[Any] = []
        validate_result.validate(result, json.loads(schema.read_text(encoding="utf-8")), "$", violations)
        if violations:
            fail(f"result for {fixture_id!r} failed validation: {violations[0]}")
        results.append(result)
        print(f"  -> p50 {result['aggregates']['latency']['p50']:.1f} ms, "
              f"p95 {result['aggregates']['latency']['p95']:.1f} ms, "
              f"correctness {result['aggregates']['correctness']['pass_count']}/{result['aggregates']['correctness']['total']}")

    if args.out:
        out = Path(args.out)
    else:
        safe_host = platform.system().lower().replace(" ", "-")
        out = ROOT / "benchmarks" / "baselines" / f"pliego-{args.target}-{safe_host}-{platform.machine().lower()}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(results if len(results) > 1 else results[0], indent=2) + "\n", encoding="utf-8")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
