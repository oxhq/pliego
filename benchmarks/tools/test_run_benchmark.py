#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("run_benchmark.py")
SPEC = importlib.util.spec_from_file_location("run_benchmark", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def main() -> None:
    adapters = benchmark.ROOT / "benchmarks" / "adapters"
    php = shutil.which("php")
    assert php is not None
    for adapter in (adapters / "dompdf" / "adapter.php", adapters / "browsershot" / "adapter.php"):
        command = [php, str(adapter), "self-test"]
        if os.name == "nt" and Path(php).suffix.lower() in {".bat", ".cmd"}:
            command = ["cmd.exe", "/d", "/c", *command]
        self_test = subprocess.run(command, capture_output=True, text=True, timeout=30)
        assert self_test.returncode == 0, self_test.stderr

    dompdf_lock = json.loads((adapters / "dompdf" / "composer.lock").read_text(encoding="utf-8"))
    dompdf_versions = {package["name"]: package["version"].lstrip("v") for package in dompdf_lock["packages"]}
    assert dompdf_versions["dompdf/dompdf"] == "3.1.6"
    browsershot_lock = json.loads((adapters / "browsershot" / "composer.lock").read_text(encoding="utf-8"))
    browsershot_versions = {package["name"]: package["version"].lstrip("v") for package in browsershot_lock["packages"]}
    assert browsershot_versions["spatie/browsershot"] == "5.4.0"
    npm_lock = json.loads((adapters / "browsershot" / "package-lock.json").read_text(encoding="utf-8"))
    assert npm_lock["packages"]["node_modules/puppeteer"]["version"] == "25.8.0"

    oracle_test = subprocess.run(
        [sys.executable, str(SCRIPT.with_name("test_pdf_oracle.py"))],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert oracle_test.returncode == 0, oracle_test.stderr

    with patch.object(
        benchmark.subprocess,
        "run",
        return_value=subprocess.CompletedProcess(args=[], returncode=0, stdout='{"wall_ms":1}\n', stderr=""),
    ):
        try:
            benchmark.collect_samples(
                Path("php"),
                "minimal-static",
                {"input": "benchmarks/fixtures/minimal-static/input.html"},
                Path("pliego"),
                2,
                0,
            )
        except SystemExit as error:
            assert error.code == 1
        else:
            raise AssertionError("partial runner output was accepted")

    with patch.object(
        benchmark,
        "run",
        return_value=subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr="PHP 8.4.0\n"),
    ):
        assert benchmark.tool_version(["php", "--version"]) == "PHP 8.4.0"

    command = benchmark.build_command(
        Path("php"),
        "comma-text",
        {
            "input": "benchmarks/fixtures/minimal-static/input.html",
            "correctness": {"text_contains": ["Revenue, net", "Total"]},
        },
        Path("pliego"),
        1,
        0,
    )
    fragments = [command[index + 1] for index, value in enumerate(command) if value == "--text-contains"]
    assert fragments == ["Revenue, net", "Total"]

    neutral = benchmark.build_command(
        Path("php"),
        "minimal-static",
        {
            "input": "benchmarks/fixtures/minimal-static/input.html",
            "page_size": "793.7008x1122.52",
            "page_margins": "0,0,0,0",
            "correctness": {
                "page_count": 1,
                "page_width_points": 595.276,
                "page_height_points": 841.89,
                "dimension_tolerance_points": 0.05,
                "text_contains": ["Minimal"],
                "link_targets": ["https://pliego.dev/docs"],
            },
        },
        Path("adapter"),
        1,
        0,
        require_scene_report=False,
    )
    assert neutral[neutral.index("--input") + 1] == "input.html"
    assert Path(neutral[neutral.index("--cwd") + 1]).name == "minimal-static"
    assert "--require-scene-report" not in neutral
    assert neutral[neutral.index("--link-target") + 1] == "https://pliego.dev/docs"
    assert neutral[neutral.index("--page-size") + 1] == "793.7008x1122.52"

    manifest = tomllib.loads(benchmark.MANIFEST.read_text(encoding="utf-8"))
    target = manifest["targets"]["dompdf-3.1.6"]
    fixture = manifest["fixtures"]["chartjs-showcase"]
    not_applicable = benchmark.not_applicable_result(
        generated_at="2026-08-19T00:00:00+00:00",
        host={
            "os": "Linux",
            "arch": "x86_64",
            "kernel": "test",
            "cpu_model": "test",
            "cores": 1,
            "ram_bytes": 1,
            "dedicated": True,
        },
        toolchain={"engine": {"name": "dompdf", "version": "3.1.6"}, "python_version": "3.11", "php_version": "8.3"},
        protocol=manifest["protocol"],
        target_id="dompdf-3.1.6",
        target=target,
        fixture_id="chartjs-showcase",
        fixture=fixture,
    )
    assert not_applicable["status"] == "not-applicable"
    assert "dynamic Canvas" in not_applicable["reason"]
    assert not_applicable["protocol"]["sample_count"] == 0
    assert not_applicable["protocol"]["sample_order"] == manifest["protocol"]["sample_order"]
    schema = json.loads(benchmark.DEFAULT_SCHEMA.read_text(encoding="utf-8"))
    assert benchmark.validate_result.validate_document(not_applicable, schema) == []

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        adapter = root / "adapter"
        lock = root / "composer.lock"
        adapter.write_text("adapter", encoding="utf-8")
        lock.write_text("lock", encoding="utf-8")
        identity = {
            "contract": "pliego.benchmark-adapter.v1",
            "target": "fixture-adapter",
            "package": "fixture/package",
            "package_version": "1.2.3",
            "adapter_path": str(adapter.resolve()),
            "adapter_sha256": benchmark.file_sha256(adapter),
            "composer_lock_sha256": benchmark.file_sha256(lock),
        }
        completed = subprocess.CompletedProcess([], 0, json.dumps(identity), "")
        with patch.object(benchmark, "ROOT", root), patch.object(benchmark, "run", return_value=completed):
            engine, recorded = benchmark.adapter_identity(
                adapter,
                "fixture-adapter",
                {
                    "package": "fixture/package",
                    "version": "1.2.3",
                    "profile": "locked",
                    "lockfiles": ["composer.lock"],
                },
            )
        assert engine["binary_sha256"] == identity["adapter_sha256"]
        assert recorded == identity

    aggregate = benchmark.aggregates(
        [
            {
                "ok": True,
                "correctness": {"pass": True},
                "wall_ms": 1,
                "user_ms": 1,
                "sys_ms": 0,
                "memory_peak_bytes": 2048,
                "sampled_peak_rss_kib_lower_bound": 2,
                "sampled_peak_pss_kib_lower_bound": 1,
                "read_bytes": 3,
                "write_bytes": 4,
                "read_operations": 5,
                "write_operations": 6,
                "output": {"pdf_bytes": 5, "pdf_sha256": "0" * 64},
                "failure": {"code": None},
            }
        ],
        1,
    )
    assert aggregate["memory"]["cgroup_peak_bytes"]["mean"] == 2048
    assert aggregate["memory"]["sampled_peak_pss_kib_lower_bound"]["mean"] == 1
    assert aggregate["io"]["read_bytes"]["mean"] == 3
    assert aggregate["io"]["write_bytes"]["mean"] == 4
    assert aggregate["io"]["read_operations"]["mean"] == 5
    assert aggregate["io"]["write_operations"]["mean"] == 6
    assert benchmark.validate_result.percentiles([1, 3])["p50"] == 1

    print("Pliego benchmark runner-count self-test passed")


if __name__ == "__main__":
    main()
