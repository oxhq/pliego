#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import hashlib
import json
import math
import subprocess
import sys
import tempfile
from copy import deepcopy
from pathlib import Path
from typing import Any, Callable

import run_comparison
import run_benchmark
import summarize_comparisons
from test_run_comparison import artifact, comparison, reseal, write_bundle


SCRIPT = Path(__file__).with_name("summarize_comparisons.py")


def artifact_for_repeat(repeat: int) -> dict[str, Any]:
    source = artifact()
    offset = float((repeat - 1) * 10)
    for record in source["raw_samples"]:
        record["sample"]["one_shot_wall_ms"] += offset
    reseal(source)
    return source


def comparison_for_repeat(source: dict[str, Any], repeat: int) -> dict[str, Any]:
    data = comparison(source)
    data["generated_at"] = f"2026-08-28T00:00:0{repeat}+00:00"
    data["source"]["repeat"] = repeat
    data["runner"]["name"] = f"GitHub Actions {repeat}"
    data["host"].update(
        {
            "kernel": f"6.11.{repeat}",
            "cpu_model": f"synthetic-{repeat}",
            "cores": repeat + 3,
            "ram_bytes": (16 + repeat) * 1024 * 1024 * 1024,
            "virtualization": f"kvm-{repeat}",
        }
    )
    data["ambient"] = {"before": {"repeat": repeat, "phase": "before"}, "after": {"repeat": repeat, "phase": "after"}}
    data["evidence_binding"]["sha256"] = run_comparison.evidence_binding_sha256(data)
    data["comparison_sha256"] = run_comparison.comparison_sha256(data)
    assert not run_comparison.validate_comparison(data, source)
    return data


def write_input(directory: Path, source: dict[str, Any], data: dict[str, Any]) -> None:
    directory.mkdir(parents=True)
    write_bundle(directory, source, data)


def make_inputs(root: Path) -> tuple[list[Path], list[summarize_comparisons.HostedRun]]:
    directories: list[Path] = []
    for repeat in summarize_comparisons.EXPECTED_REPEATS:
        source = artifact_for_repeat(repeat)
        data = comparison_for_repeat(source, repeat)
        directory = root / f"repeat-{repeat}"
        write_input(directory, source, data)
        directories.append(directory)
    return directories, summarize_comparisons.load_and_validate_runs(directories)


def expect_value_error(operation: Callable[[], object], expected: str) -> None:
    try:
        operation()
    except ValueError as error:
        assert expected in str(error), str(error)
    else:
        raise AssertionError(f"expected ValueError containing {expected!r}")


def write_changed_comparison(directory: Path, change: Callable[[dict[str, Any]], None]) -> None:
    path = directory / summarize_comparisons.COMPARISON_FILE
    data = json.loads(path.read_text(encoding="utf-8"))
    source = json.loads((directory / summarize_comparisons.INTERLEAVED_FILE).read_text(encoding="utf-8"))
    change(data)
    for identity in data["targets"]:
        identity["identity_sha256"] = run_comparison.target_identity_sha256(identity)
    data["evidence_binding"]["sha256"] = run_comparison.evidence_binding_sha256(data)
    data["comparison_sha256"] = run_comparison.comparison_sha256(data)
    write_bundle(directory, source, data)


def main() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        directories, runs = make_inputs(root)
        series = summarize_comparisons.build_series(runs, generated_at="2026-08-28T00:01:00+00:00")
        assert not summarize_comparisons.validate_series(series)
        assert not summarize_comparisons.validate_series_against_runs(series, runs)
        assert [run["repeat"] for run in series["runs"]] == [1, 2, 3]
        assert [run["comparison_sha256"] for run in series["runs"]] == [
            run.comparison["comparison_sha256"] for run in runs
        ]
        assert [run["interleaved_artifact_sha256"] for run in series["runs"]] == [
            run.artifact["artifact_sha256"] for run in runs
        ]
        assert len({run["interleaved_artifact_sha256"] for run in series["runs"]}) == 3
        for retained, source_run in zip(series["runs"], runs, strict=True):
            assert retained["runner"] == source_run.comparison["runner"]
            assert retained["host"] == source_run.comparison["host"]
            assert retained["ambient"] == source_run.comparison["ambient"]
        assert series["selection_policy"] == "all-repeats-no-selection"

        pliego = next(target for target in series["summary"]["targets"] if target["target_id"] == "pliego-0.3.3")
        one_shot = pliego["metrics"]["one_shot_wall_ms"]
        assert [entry["p50"] for entry in one_shot["per_repeat"]] == [2100.0, 2110.0, 2120.0]
        assert one_shot["min"] == 2100.0 and one_shot["max"] == 2120.0 and one_shot["mean"] == 2110.0
        assert math.isclose(one_shot["relative_spread_percent"], 20.0 / 2110.0 * 100.0)
        throughput = pliego["serial_throughput"]
        assert len(throughput["per_repeat"]) == 3
        assert throughput["max"] > throughput["mean"] > throughput["min"]

        markdown = summarize_comparisons.render_markdown(series)
        assert "Repeat 1 p50" in markdown
        assert "Relative spread" in markdown
        assert "No best or canonical repeat is selected" in markdown
        assert "Per-repeat hosted environments" in markdown
        assert (
            "For `read_bytes` and `write_bytes`, values come from cgroup `io.stat`; memory-backed stdout/stderr capture is excluded."
            in markdown
        )
        for run in series["runs"]:
            assert run["comparison_sha256"] in markdown
            assert run["interleaved_artifact_sha256"] in markdown
            assert run["runner"]["name"] in markdown
            assert run["host"]["cpu_model"] in markdown
            assert run["host"]["kernel"] in markdown
            assert run["host"]["virtualization"] in markdown

        output = root / "series"
        written = summarize_comparisons.write_hosted_series(directories, output)
        assert not summarize_comparisons.validate_series(written)
        assert not summarize_comparisons.validate_series_bundle(output, runs)
        assert (output / summarize_comparisons.SERIES_FILE).is_file()
        assert (output / summarize_comparisons.REPORT_FILE).is_file()
        assert {path.name for path in output.iterdir()} == summarize_comparisons.SERIES_BUNDLE_ENTRIES
        checksum_lines = (output / "SHA256SUMS").read_text(encoding="ascii").splitlines()
        assert len(checksum_lines) == 2
        for line in checksum_lines:
            digest, name = line.split("  ", 1)
            assert digest == hashlib.sha256((output / name).read_bytes()).hexdigest()
        expect_value_error(
            lambda: summarize_comparisons.write_hosted_series(directories, output),
            "output directory already exists",
        )

        cli_validation = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                *(str(directory) for directory in directories),
                "--validate",
                str(output),
            ],
            capture_output=True,
            text=True,
        )
        assert cli_validation.returncode == 0, cli_validation.stderr
        assert "validated against all three exact source bundles" in cli_validation.stdout
        conflicting_cli = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                *(str(directory) for directory in directories),
                "--out",
                str(root / "conflict"),
                "--validate",
                str(output),
            ],
            capture_output=True,
            text=True,
        )
        assert conflicting_cli.returncode == 2

        original_bundle = {name: (output / name).read_bytes() for name in summarize_comparisons.SERIES_BUNDLE_ENTRIES}

        def restore_series_bundle() -> None:
            for name, contents in original_bundle.items():
                (output / name).write_bytes(contents)

        (output / summarize_comparisons.REPORT_FILE).write_text("tampered\n", encoding="utf-8")
        errors = "\n".join(map(str, summarize_comparisons.validate_series_bundle(output, runs)))
        assert summarize_comparisons.REPORT_FILE in errors
        restore_series_bundle()

        (output / summarize_comparisons.CHECKSUM_FILE).write_text("not a checksum\n", encoding="ascii")
        errors = "\n".join(map(str, summarize_comparisons.validate_series_bundle(output, runs)))
        assert "invalid syntax" in errors
        restore_series_bundle()

        (output / summarize_comparisons.REPORT_FILE).unlink()
        errors = "\n".join(map(str, summarize_comparisons.validate_series_bundle(output, runs)))
        assert "$bundle.entries" in errors
        restore_series_bundle()

        (output / "unexpected.txt").write_text("extra\n", encoding="utf-8")
        errors = "\n".join(map(str, summarize_comparisons.validate_series_bundle(output, runs)))
        assert "$bundle.entries" in errors
        (output / "unexpected.txt").unlink()

        broken_seal = deepcopy(series)
        broken_seal["summary"]["targets"][0]["metrics"]["wall_ms"]["per_repeat"][0]["p50"] += 1
        errors = "\n".join(map(str, summarize_comparisons.validate_series(broken_seal)))
        assert "series_sha256" in errors

        wrong_derived_value = deepcopy(series)
        wrong_derived_value["summary"]["targets"][0]["metrics"]["wall_ms"]["mean"] += 1
        wrong_derived_value["series_sha256"] = summarize_comparisons.series_sha256(wrong_derived_value)
        errors = "\n".join(map(str, summarize_comparisons.validate_series(wrong_derived_value)))
        assert ".mean" in errors

        source_unbound = deepcopy(series)
        metric = source_unbound["summary"]["targets"][0]["metrics"]["wall_ms"]
        metric["per_repeat"][0]["p50"] += 1
        values = [entry["p50"] for entry in metric["per_repeat"]]
        metric.update(summarize_comparisons._spread(values))
        source_unbound["series_sha256"] = summarize_comparisons.series_sha256(source_unbound)
        assert not summarize_comparisons.validate_series(source_unbound)
        errors = "\n".join(map(str, summarize_comparisons.validate_series_against_runs(source_unbound, runs)))
        assert "must equal" in errors

        schema_tamper = deepcopy(series)
        schema_tamper["publication_status"] = "dedicated"
        schema_tamper["series_sha256"] = summarize_comparisons.series_sha256(schema_tamper)
        errors = "\n".join(map(str, summarize_comparisons.validate_series(schema_tamper)))
        assert "publication_status" in errors and "hosted-series" in errors

        claim_tamper = deepcopy(series)
        claim_tamper["claim_boundary"] = "Authoritative ranking."
        claim_tamper["series_sha256"] = summarize_comparisons.series_sha256(claim_tamper)
        errors = "\n".join(map(str, summarize_comparisons.validate_series(claim_tamper)))
        assert "claim_boundary" in errors and "expected const" in errors

        comparison_tamper_root = root / "comparison-tamper"
        tamper_dirs, _ = make_inputs(comparison_tamper_root)

        def alter_aggregate(data: dict[str, Any]) -> None:
            data["aggregates"]["targets"][0]["metrics"]["wall_ms"]["statistics"]["p50"] += 1

        write_changed_comparison(tamper_dirs[1], alter_aggregate)
        expect_value_error(
            lambda: summarize_comparisons.load_and_validate_runs(tamper_dirs),
            "hosted comparison bundle failed validation",
        )

        identity_tamper_root = root / "identity-tamper"
        identity_dirs, _ = make_inputs(identity_tamper_root)
        write_changed_comparison(identity_dirs[1], lambda data: data["targets"][0]["engine"].update(extra="changed"))
        expect_value_error(
            lambda: summarize_comparisons.load_and_validate_runs(identity_dirs),
            "target identity records",
        )

        repeat_tamper_root = root / "repeat-tamper"
        repeat_dirs, _ = make_inputs(repeat_tamper_root)
        write_changed_comparison(repeat_dirs[1], lambda data: data["source"].update(repeat=1))
        expect_value_error(
            lambda: summarize_comparisons.load_and_validate_runs(repeat_dirs),
            "repeats must be exactly",
        )

        run_tamper_root = root / "run-tamper"
        run_dirs, _ = make_inputs(run_tamper_root)
        write_changed_comparison(run_dirs[1], lambda data: data["source"].update(run_id=124))
        expect_value_error(
            lambda: summarize_comparisons.load_and_validate_runs(run_dirs),
            "workflow source",
        )

        copied_population = deepcopy(runs[0].artifact)
        copied_population["artifact_sha256"] = "f" * 64
        population_runs = list(runs)
        population_runs[1] = summarize_comparisons.HostedRun(
            directory=runs[1].directory,
            comparison=runs[1].comparison,
            artifact=copied_population,
        )
        expect_value_error(
            lambda: summarize_comparisons.validate_run_group(population_runs),
            "distinct raw-sample-ID populations",
        )

        process = subprocess.run(
            [sys.executable, str(SCRIPT), str(directories[0]), str(directories[1]), "--out", str(root / "short")],
            capture_output=True,
            text=True,
        )
        assert process.returncode == 2
        assert "comparisons" in process.stderr

        source_report = directories[0] / "all-metrics.md"
        source_report_contents = source_report.read_bytes()
        source_report.write_text("source drift\n", encoding="utf-8")
        errors = "\n".join(map(str, summarize_comparisons.validate_series_bundle(output, runs)))
        assert "$sources" in errors
        source_report.write_bytes(source_report_contents)

        copied_artifact = json.loads(
            (directories[0] / summarize_comparisons.INTERLEAVED_FILE).read_text(encoding="utf-8")
        )
        copied_comparison_path = directories[1] / summarize_comparisons.COMPARISON_FILE
        copied_comparison = json.loads(copied_comparison_path.read_text(encoding="utf-8"))
        copied_comparison["interleaved_artifact"] = {
            "file": summarize_comparisons.INTERLEAVED_FILE,
            "artifact_sha256": copied_artifact["artifact_sha256"],
            "schedule_sha256": run_benchmark.canonical_json_sha256(copied_artifact["schedule"]),
        }
        copied_comparison["aggregates"] = run_comparison.comparison_metrics.aggregate_interleaved_artifact(
            copied_artifact
        )
        copied_comparison["evidence_binding"]["sha256"] = run_comparison.evidence_binding_sha256(copied_comparison)
        copied_comparison["comparison_sha256"] = run_comparison.comparison_sha256(copied_comparison)
        write_bundle(directories[1], copied_artifact, copied_comparison)
        assert not run_comparison.validate_bundle(directories[1])
        expect_value_error(
            lambda: summarize_comparisons.load_and_validate_runs(directories),
            "three distinct interleaved artifact SHA-256s",
        )


if __name__ == "__main__":
    main()
