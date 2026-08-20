#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
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
REPORT_SCRIPT = SCRIPT.with_name("report_data.py")
REPORT_SPEC = importlib.util.spec_from_file_location("report_data", REPORT_SCRIPT)
assert REPORT_SPEC is not None and REPORT_SPEC.loader is not None
report_data = importlib.util.module_from_spec(REPORT_SPEC)
REPORT_SPEC.loader.exec_module(report_data)


def retained_sample(index: int) -> dict[str, object]:
    """Return the smallest structurally valid benchmark-result.v1 sample."""

    return {
        "index": index,
        "ok": False,
        "exit_code": 1,
        "wall_ms": float(index + 1),
        "one_shot_wall_ms": float(index + 2),
        "user_ms": None,
        "sys_ms": None,
        "memory_current_bytes": None,
        "memory_peak_bytes": None,
        "sampled_peak_rss_kib_lower_bound": None,
        "sampled_peak_pss_kib_lower_bound": None,
        "read_bytes": None,
        "write_bytes": None,
        "read_operations": None,
        "write_operations": None,
        "measurement_method": "unavailable",
        "signal": None,
        "resource_usage": None,
        "output": {
            "pdf_bytes": None,
            "pdf_sha256": None,
            "page_count": None,
            "artifact_bytes": 0,
            "published_pdf": False,
            "normalized_text_sha256": None,
            "font_families": [],
            "normalized_raster_sha256": None,
        },
        "correctness": {"pass": False, "checks": [{"name": "synthetic", "status": "fail"}]},
        "failure": {"code": "synthetic_failure", "message": "test sample", "published_pdf": False},
    }


def passing_sample(index: int, wall_ms: float) -> dict[str, object]:
    sample = retained_sample(index)
    sample.update(
        {
            "ok": True,
            "exit_code": 0,
            "wall_ms": wall_ms,
            "one_shot_wall_ms": wall_ms + 1,
            "output": {
                "pdf_bytes": 1,
                "pdf_sha256": "0" * 64,
                "page_count": 1,
                "artifact_bytes": 0,
                "published_pdf": True,
                "normalized_text_sha256": "1" * 64,
                "font_families": ["Synthetic"],
                "normalized_raster_sha256": "2" * 64,
            },
            "correctness": {"pass": True, "checks": [{"name": "synthetic", "status": "pass"}]},
            "failure": {"code": None, "message": None, "published_pdf": True},
        }
    )
    return sample


def main() -> None:
    target_ids = [
        "pliego-0.2.0",
        "dompdf-3.1.6",
        "browsershot-5.4.0-puppeteer-25.8.0",
    ]
    timed_schedule = benchmark.cross_target_phase_schedule(target_ids, "minimal-static", 3, 1, "timed")
    assert [entry["target_id"] for entry in timed_schedule] == [
        "dompdf-3.1.6",
        "pliego-0.2.0",
        "browsershot-5.4.0-puppeteer-25.8.0",
        "pliego-0.2.0",
        "dompdf-3.1.6",
        "browsershot-5.4.0-puppeteer-25.8.0",
        "dompdf-3.1.6",
        "browsershot-5.4.0-puppeteer-25.8.0",
        "pliego-0.2.0",
    ]
    assert timed_schedule == benchmark.cross_target_phase_schedule(
        list(reversed(target_ids)), "minimal-static", 3, 1, "timed"
    )
    assert timed_schedule != benchmark.cross_target_phase_schedule(target_ids, "minimal-static", 3, 2, "timed")
    for iteration in range(3):
        round_entries = [entry for entry in timed_schedule if entry["iteration"] == iteration]
        assert {entry["target_id"] for entry in round_entries} == set(target_ids)
    for invalid_targets, invalid_iterations, invalid_seed in (
        (["only-one"], 1, 1),
        (["duplicate", "duplicate"], 1, 1),
        (["one", "two"], 0, 1),
        (["one", "two"], True, 1),
        (["one", "two"], 1, True),
    ):
        try:
            benchmark.cross_target_phase_schedule(
                invalid_targets,
                "minimal-static",
                invalid_iterations,
                invalid_seed,
                "timed",
            )
        except ValueError:
            pass
        else:
            raise AssertionError("invalid cross-target schedule input was accepted")

    calls: list[tuple[str, str, int | None]] = []

    def record_preflight(target_id: str) -> None:
        calls.append(("preflight", target_id, None))

    def record_warmup(target_id: str, iteration: int) -> None:
        calls.append(("warmup", target_id, iteration))

    def record_timed(target_id: str, iteration: int) -> dict[str, object]:
        calls.append(("timed", target_id, iteration))
        return retained_sample(iteration)

    artifact = benchmark.execute_interleaved_run(
        target_ids,
        "minimal-static",
        2,
        3,
        1,
        record_preflight,
        record_warmup,
        record_timed,
    )
    transcript = artifact["schedule"]
    assert artifact["schema"] == benchmark.INTERLEAVED_RUN_SCHEMA
    assert artifact["version"] == benchmark.INTERLEAVED_RUN_VERSION
    assert artifact["publication_status"] == "prerequisite-only"
    assert artifact["sample_contract"] == benchmark.BENCHMARK_SAMPLE_CONTRACT
    assert transcript["contract"] == benchmark.CROSS_TARGET_SCHEDULE
    assert calls == [
        *[("preflight", entry["target_id"], None) for entry in transcript["preflight"]],
        *[("warmup", entry["target_id"], entry["iteration"]) for entry in transcript["warmup"]],
        *[("timed", entry["target_id"], entry["iteration"]) for entry in transcript["timed"]],
    ]
    assert [record["schedule_position"] for record in artifact["raw_samples"]] == list(range(9))
    assert [record["target_id"] for record in artifact["raw_samples"]] == [
        entry["target_id"] for entry in transcript["timed"]
    ]
    assert len({record["sample_id"] for record in artifact["raw_samples"]}) == 9
    assert artifact["artifact_sha256"] == "35cbb1e012072e54e9cef490c4bd21c74d4538e1f603db9994acbd70d3d645eb"
    assert artifact["raw_samples"][0]["sample_sha256"] == (
        "c8f3ae5f1cdbfa83db37a5d97283a3e8f7ad9a95d4b09b1fa31dbab978a698fd"
    )
    assert artifact["raw_samples"][0]["sample_id"] == (
        "ff69974c11d51cfad63707c51fbd5eb85360734e0c0f0f9335ce7aded35081bf"
    )
    assert not benchmark.validate_interleaved_artifact(artifact)

    changed_sample = deepcopy(artifact)
    changed_sample["raw_samples"][0]["sample"]["wall_ms"] = 999
    changed_errors = "\n".join(map(str, benchmark.validate_interleaved_artifact(changed_sample)))
    assert "sample_sha256" in changed_errors and "artifact_sha256" in changed_errors

    changed_schedule = deepcopy(artifact)
    changed_schedule["schedule"]["timed"][0]["target_id"] = "pliego-0.2.0"
    schedule_errors = "\n".join(map(str, benchmark.validate_interleaved_artifact(changed_schedule)))
    assert "must equal the canonical pliego.cross-target-schedule.v1 order" in schedule_errors

    missing_sample = deepcopy(artifact)
    missing_sample["raw_samples"].pop()
    missing_errors = "\n".join(map(str, benchmark.validate_interleaved_artifact(missing_sample)))
    assert "must retain exactly one sample" in missing_errors

    unknown_sample_field = deepcopy(artifact)
    unknown_sample_field["raw_samples"][0]["sample"]["uncontracted"] = True
    unknown_errors = "\n".join(map(str, benchmark.validate_interleaved_artifact(unknown_sample_field)))
    assert "unexpected property 'uncontracted'" in unknown_errors

    with tempfile.TemporaryDirectory() as directory:
        artifact_path = Path(directory) / "interleaved.json"
        artifact_path.write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
        validation = subprocess.run(
            [sys.executable, str(SCRIPT.with_name("validate_interleaved_run.py")), str(artifact_path)],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert validation.returncode == 0, validation.stderr
        assert "artifact validated" in validation.stdout

    synthetic_targets = ["synthetic-alpha", "synthetic-beta"]
    synthetic_walls = {
        "synthetic-alpha": [10.0, 20.0, 30.0],
        "synthetic-beta": [15.0, 25.0, 35.0],
    }
    synthetic_artifact = benchmark.execute_interleaved_run(
        synthetic_targets,
        "synthetic-fixture",
        1,
        3,
        7,
        lambda _target: None,
        lambda _target, _iteration: None,
        lambda target, iteration: passing_sample(iteration, synthetic_walls[target][iteration]),
    )
    report = report_data.build_report_data(synthetic_artifact)
    assert synthetic_artifact["artifact_sha256"] == ("f65bc80e2b9fd98ed988d0ad217837b7fff7ead1f34c073f55d0994297cc02ae")
    assert report["schema"] == report_data.REPORT_SCHEMA
    assert report["publication_status"] == "prerequisite-only"
    assert report["source"]["artifact_sha256"] == synthetic_artifact["artifact_sha256"]
    assert report["source"]["fixture_id"] == "synthetic-fixture"
    assert report["source"]["targets"] == synthetic_targets
    assert report["source"]["schedule_sha256"] == ("149b14c99c6c30477e3bde01da8f0d837c6de672168abf93e3ed332741d1b81f")
    assert len(report["cells"]) == 12
    alpha_cells = [cell for cell in report["cells"] if cell["target_id"] == "synthetic-alpha"]
    assert [cell["statistic"] for cell in alpha_cells] == list(report_data.STATISTICS)
    assert [cell["value"] for cell in alpha_cells] == [10.0, 20.0, 30.0, 30.0, 30.0, 20.0]
    assert all(cell["artifact_sha256"] == synthetic_artifact["artifact_sha256"] for cell in report["cells"])
    assert all(len(cell["sample_ids"]) == 3 for cell in report["cells"])
    assert alpha_cells[0]["cell_id"] == "0f617e80fc7355b2d5956f184f33c17a63d5b6ab4a90ec5e18b1efbc1638d50b"
    assert not report_data.validate_report_data(report, synthetic_artifact)

    for operation, expected_error in (
        (lambda value: value["cells"][0]["sample_ids"].pop(), "is missing contributing sample ids"),
        (lambda value: value["cells"][0]["sample_ids"].append("f" * 64), "contains extra sample ids"),
        (lambda value: value["cells"][0]["sample_ids"].reverse(), "must preserve contributing sample ids"),
        (
            lambda value: value["cells"][0]["sample_ids"].__setitem__(1, value["cells"][0]["sample_ids"][0]),
            "must not contain duplicate sample ids",
        ),
        (lambda value: value["cells"][0].__setitem__("value", 999.0), "must equal the recomputed min"),
        (lambda value: value["source"].__setitem__("schedule_sha256", "f" * 64), "source.schedule_sha256"),
        (lambda value: value["source"].__setitem__("artifact_sha256", "f" * 64), "source.artifact_sha256"),
        (lambda value: value["source"].__setitem__("fixture_id", "other-fixture"), "source.fixture_id"),
        (lambda value: value["cells"][0].__setitem__("target_id", "synthetic-beta"), "cells[0].target_id"),
        (lambda value: value["cells"][0].__setitem__("fixture_id", "other-fixture"), "cells[0].fixture_id"),
        (lambda value: value["cells"][0].__setitem__("artifact_sha256", "f" * 64), "cells[0].artifact_sha256"),
    ):
        broken_report = deepcopy(report)
        operation(broken_report)
        report_errors = "\n".join(map(str, report_data.validate_report_data(broken_report, synthetic_artifact)))
        assert expected_error in report_errors, report_errors

    missing_cell = deepcopy(report)
    missing_cell["cells"].pop()
    assert "expected >= 12 items" in "\n".join(
        map(str, report_data.validate_report_data(missing_cell, synthetic_artifact))
    )
    reordered_cells = deepcopy(report)
    reordered_cells["cells"][0], reordered_cells["cells"][1] = (
        reordered_cells["cells"][1],
        reordered_cells["cells"][0],
    )
    reordered_errors = "\n".join(map(str, report_data.validate_report_data(reordered_cells, synthetic_artifact)))
    assert "cells[0].cell_id" in reordered_errors and "cells[1].cell_id" in reordered_errors

    broken_artifact = deepcopy(synthetic_artifact)
    broken_artifact["artifact_sha256"] = "f" * 64
    assert "$artifact.artifact_sha256" in "\n".join(map(str, report_data.validate_report_data(report, broken_artifact)))
    alternate_schedule_artifact = benchmark.execute_interleaved_run(
        synthetic_targets,
        "synthetic-fixture",
        1,
        3,
        8,
        lambda _target: None,
        lambda _target, _iteration: None,
        lambda target, iteration: passing_sample(iteration, synthetic_walls[target][iteration]),
    )
    alternate_errors = "\n".join(map(str, report_data.validate_report_data(report, alternate_schedule_artifact)))
    assert "source.artifact_sha256" in alternate_errors
    assert "source.schedule_sha256" in alternate_errors
    assert "sample_ids" in alternate_errors
    try:
        report_data.build_report_data(artifact)
    except ValueError as error:
        assert "failed correctness" in str(error)
    else:
        raise AssertionError("report cells were generated from correctness-failing samples")

    with tempfile.TemporaryDirectory() as directory:
        artifact_path = Path(directory) / "synthetic-interleaved.json"
        report_path = Path(directory) / "synthetic-report.json"
        artifact_path.write_text(json.dumps(synthetic_artifact, indent=2) + "\n", encoding="utf-8")
        generated = subprocess.run(
            [sys.executable, str(REPORT_SCRIPT), "generate", str(artifact_path), "--out", str(report_path)],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert generated.returncode == 0, generated.stderr
        validated = subprocess.run(
            [
                sys.executable,
                str(REPORT_SCRIPT),
                "validate",
                str(report_path),
                "--artifact",
                str(artifact_path),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert validated.returncode == 0, validated.stderr
        assert "exact raw samples" in validated.stdout
        invalid_report = json.loads(report_path.read_text(encoding="utf-8"))
        invalid_report["cells"][0]["value"] = 999.0
        report_path.write_text(json.dumps(invalid_report, indent=2) + "\n", encoding="utf-8")
        rejected = subprocess.run(
            [
                sys.executable,
                str(REPORT_SCRIPT),
                "validate",
                str(report_path),
                "--artifact",
                str(artifact_path),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert rejected.returncode == 1
        assert "must equal the recomputed min" in rejected.stderr
    try:
        benchmark.execute_interleaved_run(
            ["one", "two"],
            "minimal-static",
            0,
            1,
            1,
            lambda _target: None,
            lambda _target, _iteration: None,
            lambda _target, iteration: {"index": iteration + 1},
        )
    except SystemExit as error:
        assert error.code == 1
    else:
        raise AssertionError("interleaved sample/index mismatch was accepted")
    try:
        benchmark.execute_interleaved_run(
            ["one", "two"],
            "minimal-static",
            0,
            1,
            1,
            lambda _target: None,
            lambda _target, _iteration: None,
            lambda _target, iteration: {**retained_sample(iteration), "wall_ms": float("nan")},
        )
    except SystemExit as error:
        assert error.code == 1
    else:
        raise AssertionError("non-finite raw sample was accepted by the identity contract")

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
    invalid_phase_command = [
        php,
        str(benchmark.RUNNER),
        "--binary",
        "missing-adapter",
        "--input",
        "input.html",
        "--runner-phase",
        "timed",
        "--sample-index",
        "01",
    ]
    if os.name == "nt" and Path(php).suffix.lower() in {".bat", ".cmd"}:
        invalid_phase_command = ["cmd.exe", "/d", "/c", *invalid_phase_command]
    invalid_phase = subprocess.run(invalid_phase_command, capture_output=True, text=True, timeout=30)
    assert invalid_phase.returncode == 2
    assert "--sample-index must be a canonical nonnegative integer" in invalid_phase.stderr
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

    timed = benchmark.build_command(
        Path("php"),
        "minimal-static",
        {"input": "benchmarks/fixtures/minimal-static/input.html"},
        Path("adapter"),
        None,
        None,
        runner_phase="timed",
        sample_index=7,
    )
    assert timed[timed.index("--runner-phase") + 1] == "timed"
    assert timed[timed.index("--sample-index") + 1] == "7"
    assert "--samples" not in timed and "--warmup" not in timed
    try:
        benchmark.build_command(
            Path("php"),
            "minimal-static",
            {"input": "benchmarks/fixtures/minimal-static/input.html"},
            Path("adapter"),
            1,
            0,
            runner_phase="timed",
            sample_index=0,
        )
    except ValueError:
        pass
    else:
        raise AssertionError("phase-specific runner command accepted aggregate counts")
    try:
        benchmark.build_command(
            Path("php"),
            "minimal-static",
            {"input": "benchmarks/fixtures/minimal-static/input.html"},
            Path("adapter"),
            None,
            None,
            runner_phase="timed",
            sample_index=True,
        )
    except ValueError:
        pass
    else:
        raise AssertionError("phase-specific runner command accepted a boolean sample index")

    phase_fixture = {"input": "benchmarks/fixtures/minimal-static/input.html"}
    with patch.object(
        benchmark.subprocess,
        "run",
        return_value=subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),
    ):
        assert (
            benchmark.run_runner_phase(
                Path("php"),
                "minimal-static",
                phase_fixture,
                Path("adapter"),
                "preflight",
            )
            is None
        )
    with patch.object(
        benchmark.subprocess,
        "run",
        return_value=subprocess.CompletedProcess(args=[], returncode=0, stdout='{"index":3,"wall_ms":1}\n', stderr=""),
    ):
        assert benchmark.run_runner_phase(
            Path("php"),
            "minimal-static",
            phase_fixture,
            Path("adapter"),
            "timed",
            sample_index=3,
        ) == {"index": 3, "wall_ms": 1}
    with patch.object(
        benchmark.subprocess,
        "run",
        return_value=subprocess.CompletedProcess(args=[], returncode=0, stdout="not-json\n", stderr=""),
    ):
        try:
            benchmark.run_runner_phase(
                Path("php"),
                "minimal-static",
                phase_fixture,
                Path("adapter"),
                "timed",
                sample_index=0,
            )
        except SystemExit as error:
            assert error.code == 1
        else:
            raise AssertionError("non-JSON phase stdout was accepted")

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
