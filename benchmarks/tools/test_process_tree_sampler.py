#!/usr/bin/env python3

"""Focused Linux proof for the process-tree sampler."""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import random
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import process_tree_sampler

SAMPLER = Path(__file__).with_name("process_tree_sampler.py")


def child_workload(mebibytes: int, duration: float) -> int:
    allocation = bytearray(mebibytes * 1024 * 1024)
    for offset in range(0, len(allocation), 4096):
        allocation[offset] = (offset // 4096) % 251
    with tempfile.TemporaryFile() as output:
        output.write(b"x" * 1024 * 1024)
        output.flush()
        os.fsync(output.fileno())
        time.sleep(duration)
    return allocation[0]


def parent_workload(mebibytes: int, duration: float) -> int:
    children = [
        subprocess.Popen(
            [sys.executable, __file__, "--workload-child", str(mebibytes), str(duration)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        for _ in range(2)
    ]
    return max(child.wait() for child in children)


def workload_command(mebibytes: int = 32, duration: float = 0.8) -> list[str]:
    return [sys.executable, __file__, "--workload-parent", str(mebibytes), str(duration)]


def sampled(command: list[str]) -> dict:
    result = subprocess.run(
        [
            sys.executable,
            str(SAMPLER),
            "--interval-ms",
            "75",
            "--pss-interval-ms",
            "250",
            "--",
            *command,
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def direct_wall_ms(command: list[str]) -> float:
    started = time.monotonic()
    result = subprocess.run(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
    assert result.returncode == 0
    return (time.monotonic() - started) * 1000.0


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    rank = (len(ordered) - 1) * fraction
    lower = int(rank)
    upper = min(lower + 1, len(ordered) - 1)
    weight = rank - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def recycled_root_pid_proof() -> None:
    root_pid = 4242
    original_start_ticks = 100
    with tempfile.TemporaryDirectory() as directory:
        proc = Path(directory)
        recycled = proc / str(root_pid)
        recycled.mkdir()
        fields = [
            "S", "1", str(root_pid), str(root_pid + 1), "0", "0", "0", "0", "0", "0", "0",
            "1", "1", "0", "0", "20", "0", "1", "0", str(original_start_ticks + 1), "4096", "1",
        ]
        (recycled / "stat").write_text(f"{root_pid} (recycled) {' '.join(fields)}\n", encoding="ascii")
        (recycled / "io").write_text("read_bytes: 0\nwrite_bytes: 0\n", encoding="ascii")

        original_proc = process_tree_sampler.PROC
        process_tree_sampler.PROC = proc
        try:
            assert process_tree_sampler.scan_session(root_pid, include_pss=False) == []
        finally:
            process_tree_sampler.PROC = original_proc


def unsupported_platform_proof() -> None:
    parser = argparse.ArgumentParser(prog="process_tree_sampler")
    error = io.StringIO()
    with contextlib.redirect_stderr(error):
        try:
            process_tree_sampler.require_linux(parser, "win32")
        except SystemExit as exit_error:
            assert exit_error.code == 2
        else:
            raise AssertionError("unsupported platform was accepted")
    assert "this sampler requires Linux procfs" in error.getvalue()


def acceptance_overhead_proof() -> tuple[dict, dict]:
    acceptance_duration_seconds = 1.5
    acceptance_command = workload_command(duration=acceptance_duration_seconds)
    pair_count = 20
    seed = 20260808
    rng = random.Random(seed)
    pairs: list[dict] = []
    direct_wall_ms(acceptance_command)
    sampled(acceptance_command)
    for index in range(pair_count):
        order = ["direct", "sampled"]
        rng.shuffle(order)
        direct_ms = 0.0
        measurement: dict = {}
        for mode in order:
            if mode == "direct":
                direct_ms = direct_wall_ms(acceptance_command)
            else:
                measurement = sampled(acceptance_command)
        sampled_ms = float(measurement["wall_ms"])
        pairs.append(
            {
                "index": index,
                "order": order,
                "direct_wall_ms": direct_ms,
                "sampled_wall_ms": sampled_ms,
                "overhead_percent": (sampled_ms - direct_ms) * 100.0 / direct_ms,
                "sampler_cpu_percent_of_wall": float(measurement["sampler_cpu_percent_of_wall"]),
            }
        )
    overhead = [float(pair["overhead_percent"]) for pair in pairs]
    sampler_cpu = [float(pair["sampler_cpu_percent_of_wall"]) for pair in pairs]
    overhead_p95 = percentile(overhead, 0.95)
    observer_effect = {
        "method": "randomized-paired-wall-v1",
        "seed": seed,
        "pair_count": pair_count,
        "workload_duration_seconds": acceptance_duration_seconds,
        "percentile_method": "linear interpolation at rank (n - 1) * 0.95",
        "p95_overhead_percent": round(overhead_p95, 3),
        "threshold_percent_exclusive": 2.0,
        "passed": overhead_p95 < 2.0,
        "pairs": pairs,
    }
    sampler_cpu_diagnostic = {
        "method": "sampler-process-cpu-divided-by-engine-wall",
        "median_percent_of_wall": round(statistics.median(sampler_cpu), 3),
        "p95_percent_of_wall": round(percentile(sampler_cpu, 0.95), 3),
        "raw_percent_of_wall": sampler_cpu,
    }
    return observer_effect, sampler_cpu_diagnostic


def main(acceptance_overhead: bool) -> None:
    recycled_root_pid_proof()
    unsupported_platform_proof()
    command = workload_command()
    proof = sampled(command)
    assert proof["exit_code"] == 0 and proof["signal"] is None
    assert proof["tree_complete"] is True
    assert len(proof["observed_pids"]) >= 3, proof["observed_pids"]
    assert proof["peak_summed_rss_kib"] >= 48 * 1024, proof["peak_summed_rss_kib"]
    assert max(len(sample["processes"]) for sample in proof["samples"]) >= 3
    assert proof["cpu_user_ms"] > 0 and proof["cpu_sys_ms"] >= 0
    assert proof["read_bytes"] is not None and proof["write_bytes"] > 0

    signaled = sampled([sys.executable, "-c", "import os,signal; os.kill(os.getpid(), signal.SIGTERM)"])
    assert signaled["exit_code"] == 128 + signal.SIGTERM
    assert signaled["signal"] == signal.SIGTERM

    report = {
        "proof": "parent-plus-two-memory-heavy-children",
        "observed_pids": proof["observed_pids"],
        "peak_summed_rss_kib": proof["peak_summed_rss_kib"],
        "peak_summed_pss_kib": proof["peak_summed_pss_kib"],
        "sampler_cpu_diagnostic": {
            "method": "sampler-process-cpu-divided-by-engine-wall",
            "functional_sample_percent_of_wall": proof["sampler_cpu_percent_of_wall"],
        },
    }
    observer_effect: dict | None = None
    if acceptance_overhead:
        observer_effect, sampler_cpu_diagnostic = acceptance_overhead_proof()
        report["observer_effect"] = observer_effect
        report["sampler_cpu_diagnostic"] = sampler_cpu_diagnostic
    print(
        json.dumps(
            report,
            indent=2,
        )
    )
    if observer_effect is not None and not observer_effect["passed"]:
        raise AssertionError(
            f"p95 paired observer overhead {observer_effect['p95_overhead_percent']}% is not below 2%"
        )


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--workload-child":
        raise SystemExit(child_workload(int(sys.argv[2]), float(sys.argv[3])))
    if len(sys.argv) == 4 and sys.argv[1] == "--workload-parent":
        raise SystemExit(parent_workload(int(sys.argv[2]), float(sys.argv[3])))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--acceptance-overhead",
        action="store_true",
        help="run the 20-pair p95 observer-effect gate on a dedicated Linux host",
    )
    main(parser.parse_args().acceptance_overhead)
