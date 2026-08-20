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
from pathlib import Path, PurePosixPath
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("run_benchmark.py")
SPEC = importlib.util.spec_from_file_location("run_benchmark", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def main() -> None:
    mounts = "\n".join(
        [
            "1 0 0:1 / / ro - ext4 root ro",
            "2 1 0:2 / /workspace/vendor rw - bind vendor rw",
        ]
    )
    assert benchmark.mountinfo_path_is_read_only(PurePosixPath("/workspace/app"), mounts)
    assert not benchmark.mountinfo_path_is_read_only(PurePosixPath("/workspace/vendor/package.php"), mounts)

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
    assert "--text-equals" not in neutral
    assert "--fixture-input-sha256" in neutral and "--fixture-bundle-sha256" in neutral

    manifest = tomllib.loads(benchmark.MANIFEST.read_text(encoding="utf-8"))
    target = manifest["targets"]["dompdf-3.1.6"]
    _, unavailable = benchmark.adapter_environment_identity(target)
    assert benchmark.ADAPTER_ATTESTATION_REASON in (unavailable or "")
    assert benchmark.pdf_oracle_identity_reason({"contract": "pliego.pdf-oracle.v1"}, manifest) == (
        benchmark.ORACLE_IDENTITY_REASON
    )
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
            "php_path": str(Path(sys.executable).resolve()),
            "php_sha256": "0" * 64,
            "php_version": "PHP 8.4.0",
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

        with patch.object(benchmark, "ROOT", root), patch.object(benchmark, "run", return_value=completed):
            try:
                benchmark.adapter_identity(
                    adapter,
                    "fixture-adapter",
                    {
                        "package": "fixture/package",
                        "version": "1.2.3",
                        "profile": "locked",
                        "lockfiles": ["composer.lock"],
                        "identity_keys": ["composer_vendor_sha256"],
                    },
                )
            except SystemExit as error:
                assert error.code == 1
            else:
                raise AssertionError("missing dependency tree identity was accepted")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        release = root / "pliego"
        release.write_bytes(b"release")
        release_target = {
            "version": "0.1.1",
            "binary_sha256": benchmark.file_sha256(release),
            "binary_bytes": release.stat().st_size,
            "release_tag": "v0.1.1",
            "commit": "1" * 40,
            "servo_build": "fixture",
            "servo_base": "2" * 40,
            "archive": "fixture.tar.gz",
            "archive_sha256": "3" * 64,
            "archive_bytes": 1,
            "profile": "checked-release",
        }
        with patch.object(benchmark, "engine_version", return_value="pliego 0.1.1"):
            release_identity = benchmark.engine_identity(release, release_target)
        assert release_identity["version"] == "0.1.1"
        with patch.object(benchmark, "engine_version", return_value="pliego 9.9.9"):
            try:
                benchmark.engine_identity(release, release_target)
            except SystemExit as error:
                assert error.code == 1
            else:
                raise AssertionError("wrong reported engine version was accepted")

        fixture_dir = root / "fixture"
        fixture_dir.mkdir()
        source = fixture_dir / "input.html"
        source.write_text("before", encoding="utf-8")
        fixture = {"input": "fixture/input.html"}
        with patch.object(benchmark, "ROOT", root):
            original = benchmark.fixture_identity(fixture)

            def mutate_fixture(*_: object, **__: object) -> subprocess.CompletedProcess[str]:
                source.write_text("after", encoding="utf-8")
                return subprocess.CompletedProcess([], 0, '{"wall_ms":1}\n', "")

            with patch.object(benchmark.subprocess, "run", side_effect=mutate_fixture):
                try:
                    benchmark.collect_samples(
                        Path("php"),
                        "fixture",
                        fixture,
                        Path("adapter"),
                        1,
                        0,
                        expected_fixture_identity=original,
                    )
                except SystemExit as error:
                    assert error.code == 1
                else:
                    raise AssertionError("fixture mutation during rendering was accepted")

    aggregate = benchmark.aggregates(
        [
            {
                "ok": True,
                "correctness": {"pass": True},
                "wall_ms": 1,
                "one_shot_wall_ms": 4,
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
    assert aggregate["throughput"] == {
        "renders_per_minute": 15000.0,
        "concurrency": 1,
        "mean_one_shot_wall_ms": 4.0,
        "measurement_boundary": "runner-process-open-through-sampler-exit",
    }
    assert benchmark.validate_result.percentiles([1, 3])["p50"] == 1

    print("Pliego benchmark runner-count self-test passed")


if __name__ == "__main__":
    main()
