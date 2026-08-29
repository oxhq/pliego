#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Seal three GitHub-hosted benchmark repeats without choosing a winner.

Each input directory must be the output of ``run_comparison.py``.  The
summarizer validates every comparison against its exact interleaved raw-sample
artifact before it computes repeat-to-repeat spread.  It deliberately cannot
upgrade hosted evidence into a dedicated-host claim.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import statistics
import sys
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, NoReturn, Sequence

TOOLS = Path(__file__).resolve().parent
ROOT = Path(__file__).resolve().parents[2]
SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-hosted-series.v1.json"
COMPARISON_FILE = "hosted-comparison.v1.json"
INTERLEAVED_FILE = "interleaved-run.v1.json"
SERIES_FILE = "hosted-series.v1.json"
REPORT_FILE = "all-repeats.md"
CHECKSUM_FILE = "SHA256SUMS"
SERIES_BUNDLE_ENTRIES = frozenset({SERIES_FILE, REPORT_FILE, CHECKSUM_FILE})
EXPECTED_REPEATS = (1, 2, 3)
EVIDENCE_CLASS = "github-hosted-exploratory"
PUBLICATION_STATUS = "hosted-series"
SELECTION_POLICY = "all-repeats-no-selection"
CLAIM_BOUNDARY = (
    "Three correctness-gated repeats on GitHub-hosted VMs; repeat-to-repeat spread is retained, "
    "no best or canonical repeat is selected, and the series is not dedicated-host evidence or a "
    "general production-performance ranking."
)

sys.path.insert(0, str(TOOLS))
import run_benchmark  # noqa: E402
import run_comparison  # noqa: E402
import validate_result  # noqa: E402


@dataclass(frozen=True)
class HostedRun:
    """One validated comparison and its exact raw artifact."""

    directory: Path
    comparison: dict[str, Any]
    artifact: dict[str, Any]


def fail(message: str) -> NoReturn:
    print(f"summarize_comparisons: {message}", file=sys.stderr)
    raise SystemExit(1)


def _read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def load_hosted_run(directory: Path) -> HostedRun:
    """Load and fully validate one ``run_comparison.py`` output directory."""

    bundle_violations = run_comparison.validate_bundle(directory)
    if bundle_violations:
        raise ValueError(f"{directory}: hosted comparison bundle failed validation: {bundle_violations[0]}")
    comparison = _read_json(directory / COMPARISON_FILE)
    artifact = _read_json(directory / INTERLEAVED_FILE)
    violations = run_comparison.validate_comparison(comparison, artifact)
    if violations:
        raise ValueError(f"{directory}: hosted comparison failed validation: {violations[0]}")
    return HostedRun(directory=directory, comparison=comparison, artifact=artifact)


def _require_same(label: str, actual: Any, expected: Any) -> None:
    if actual != expected:
        raise ValueError(f"repeat mismatch for {label}: expected {expected!r}, got {actual!r}")


def _shared_source(source: dict[str, Any]) -> dict[str, Any]:
    return {key: deepcopy(value) for key, value in source.items() if key != "repeat"}


def validate_run_group(runs: Sequence[HostedRun]) -> list[HostedRun]:
    """Require the three repeats to be one identity-stable workflow series."""

    if len(runs) != len(EXPECTED_REPEATS):
        raise ValueError(f"expected exactly three comparison directories, got {len(runs)}")
    repeats = [run.comparison["source"]["repeat"] for run in runs]
    if set(repeats) != set(EXPECTED_REPEATS) or len(set(repeats)) != len(EXPECTED_REPEATS):
        raise ValueError(f"comparison repeats must be exactly {set(EXPECTED_REPEATS)!r}, got {repeats!r}")
    ordered = sorted(runs, key=lambda run: run.comparison["source"]["repeat"])
    artifact_sha256s = [run.artifact["artifact_sha256"] for run in ordered]
    if len(set(artifact_sha256s)) != len(EXPECTED_REPEATS):
        raise ValueError("hosted series requires three distinct interleaved artifact SHA-256s")
    sample_populations = [frozenset(record["sample_id"] for record in run.artifact["raw_samples"]) for run in ordered]
    if len(set(sample_populations)) != len(EXPECTED_REPEATS):
        raise ValueError("hosted series requires three distinct raw-sample-ID populations")
    reference = ordered[0].comparison
    reference_source = _shared_source(reference["source"])
    reference_image = {
        "image_os": reference["runner"]["image_os"],
        "image_version": reference["runner"]["image_version"],
    }

    for run in ordered[1:]:
        comparison = run.comparison
        _require_same("workflow source", _shared_source(comparison["source"]), reference_source)
        _require_same("fixture", comparison["fixture"], reference["fixture"])
        _require_same("protocol", comparison["protocol"], reference["protocol"])
        _require_same("target identity records", comparison["targets"], reference["targets"])
        _require_same("oracle", comparison["oracle"], reference["oracle"])
        _require_same(
            "runner image",
            {
                "image_os": comparison["runner"]["image_os"],
                "image_version": comparison["runner"]["image_version"],
            },
            reference_image,
        )

    reference_aggregates = {target["target_id"]: target for target in reference["aggregates"]["targets"]}
    for run in ordered[1:]:
        aggregates = {target["target_id"]: target for target in run.comparison["aggregates"]["targets"]}
        _require_same("aggregate target ids", list(aggregates), list(reference_aggregates))
        for target_id, reference_target in reference_aggregates.items():
            target = aggregates[target_id]
            _require_same(
                f"metric availability for {target_id}",
                list(target["metrics"]),
                list(reference_target["metrics"]),
            )
            for metric_name, reference_metric in reference_target["metrics"].items():
                _require_same(
                    f"metric unit for {target_id}/{metric_name}",
                    target["metrics"][metric_name]["unit"],
                    reference_metric["unit"],
                )
            _require_same(
                f"throughput unit for {target_id}",
                target["serial_throughput"]["unit"],
                reference_target["serial_throughput"]["unit"],
            )
    return ordered


def load_and_validate_runs(directories: Sequence[Path]) -> list[HostedRun]:
    """Load exactly three directories and enforce cross-repeat invariants."""

    if len(directories) != len(EXPECTED_REPEATS):
        raise ValueError(f"expected exactly three comparison directories, got {len(directories)}")
    return validate_run_group([load_hosted_run(directory) for directory in directories])


def _spread(values: Sequence[float]) -> dict[str, float]:
    if len(values) != len(EXPECTED_REPEATS) or any(not math.isfinite(value) or value < 0 for value in values):
        raise ValueError(f"spread requires three finite nonnegative values, got {values!r}")
    minimum = min(values)
    maximum = max(values)
    mean = statistics.fmean(values)
    relative = 0.0 if mean == 0 else (maximum - minimum) / mean * 100.0
    return {
        "min": minimum,
        "max": maximum,
        "mean": mean,
        "relative_spread_percent": relative,
    }


def _target_aggregate(comparison: dict[str, Any], target_id: str) -> dict[str, Any]:
    return next(target for target in comparison["aggregates"]["targets"] if target["target_id"] == target_id)


def _summary_targets(runs: Sequence[HostedRun]) -> list[dict[str, Any]]:
    target_summaries: list[dict[str, Any]] = []
    reference = runs[0].comparison
    for identity in reference["targets"]:
        target_id = identity["id"]
        aggregates = [_target_aggregate(run.comparison, target_id) for run in runs]
        metrics: dict[str, Any] = {}
        for metric_name, metric in aggregates[0]["metrics"].items():
            values = [float(aggregate["metrics"][metric_name]["statistics"]["p50"]) for aggregate in aggregates]
            metrics[metric_name] = {
                "unit": metric["unit"],
                "per_repeat": [
                    {"repeat": repeat, "p50": value} for repeat, value in zip(EXPECTED_REPEATS, values, strict=True)
                ],
                **_spread(values),
            }
        throughput_values = [float(aggregate["serial_throughput"]["renders_per_minute"]) for aggregate in aggregates]
        target_summaries.append(
            {
                "target_id": target_id,
                "metrics": metrics,
                "serial_throughput": {
                    "unit": aggregates[0]["serial_throughput"]["unit"],
                    "per_repeat": [
                        {"repeat": repeat, "renders_per_minute": value}
                        for repeat, value in zip(EXPECTED_REPEATS, throughput_values, strict=True)
                    ],
                    **_spread(throughput_values),
                },
            }
        )
    return target_summaries


def series_sha256(data: dict[str, Any]) -> str:
    """Return the canonical series seal with its digest field omitted."""

    return run_benchmark.canonical_json_sha256({key: value for key, value in data.items() if key != "series_sha256"})


def build_series(runs: Sequence[HostedRun], generated_at: str | None = None) -> dict[str, Any]:
    """Build one no-selection hosted series from three validated repeats."""

    ordered = validate_run_group(runs)
    reference = ordered[0].comparison
    data: dict[str, Any] = {
        "schema": "pliego.benchmark-hosted-series",
        "version": 1,
        "evidence_class": EVIDENCE_CLASS,
        "publication_status": PUBLICATION_STATUS,
        "selection_policy": SELECTION_POLICY,
        "claim_boundary": CLAIM_BOUNDARY,
        "series_sha256": "",
        "generated_at": generated_at or datetime.now(timezone.utc).isoformat(),
        "source": _shared_source(reference["source"]),
        "runner_image": {
            "image_os": reference["runner"]["image_os"],
            "image_version": reference["runner"]["image_version"],
        },
        "fixture": deepcopy(reference["fixture"]),
        "protocol": deepcopy(reference["protocol"]),
        "oracle": deepcopy(reference["oracle"]),
        "targets": deepcopy(reference["targets"]),
        "runs": [
            {
                "repeat": run.comparison["source"]["repeat"],
                "generated_at": run.comparison["generated_at"],
                "comparison_sha256": run.comparison["comparison_sha256"],
                "interleaved_artifact_sha256": run.comparison["interleaved_artifact"]["artifact_sha256"],
                "schedule_sha256": run.comparison["interleaved_artifact"]["schedule_sha256"],
                "runner": deepcopy(run.comparison["runner"]),
                "host": deepcopy(run.comparison["host"]),
                "ambient": deepcopy(run.comparison["ambient"]),
            }
            for run in ordered
        ],
        "summary": {
            "basis": "three-per-run-p50s",
            "repeats": list(EXPECTED_REPEATS),
            "targets": _summary_targets(ordered),
        },
    }
    data["series_sha256"] = series_sha256(data)
    return data


def _validate_spread(path: str, value: dict[str, Any], key: str, violations: list[validate_result.Violation]) -> None:
    repeats = [entry["repeat"] for entry in value["per_repeat"]]
    validate_result.require_equal(f"{path}.per_repeat.repeats", repeats, list(EXPECTED_REPEATS), violations)
    values = [entry[key] for entry in value["per_repeat"]]
    if any(not math.isfinite(item) for item in values):
        violations.append(validate_result.Violation(f"{path}.per_repeat", "values must be finite"))
        return
    expected = _spread(values)
    for statistic, result in expected.items():
        validate_result.require_equal(f"{path}.{statistic}", value[statistic], result, violations)


def validate_series(data: Any) -> list[validate_result.Violation]:
    """Validate the schema, seal, ordering, and all derived spread values."""

    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    violations: list[validate_result.Violation] = []
    validate_result.validate(data, schema, "$", violations)
    if violations:
        return violations
    assert isinstance(data, dict)
    try:
        expected_sha256 = series_sha256(data)
    except (TypeError, ValueError) as error:
        violations.append(validate_result.Violation("$.series_sha256", f"cannot seal series: {error}"))
        return violations
    validate_result.require_equal("$.series_sha256", data["series_sha256"], expected_sha256, violations)
    validate_result.require_equal("$.claim_boundary", data["claim_boundary"], CLAIM_BOUNDARY, violations)
    validate_result.require_equal(
        "$.runs.repeats", [run["repeat"] for run in data["runs"]], list(EXPECTED_REPEATS), violations
    )
    validate_result.require_equal("$.summary.repeats", data["summary"]["repeats"], list(EXPECTED_REPEATS), violations)
    target_ids = [target["id"] for target in data["targets"]]
    validate_result.require_equal("$.protocol.targets", data["protocol"]["targets"], target_ids, violations)
    validate_result.require_equal(
        "$.summary.targets",
        [target["target_id"] for target in data["summary"]["targets"]],
        target_ids,
        violations,
    )
    run_comparison.validate_target_identities(data["targets"], violations)
    for target_index, target in enumerate(data["summary"]["targets"]):
        for metric_name, metric in target["metrics"].items():
            _validate_spread(f"$.summary.targets[{target_index}].metrics.{metric_name}", metric, "p50", violations)
        _validate_spread(
            f"$.summary.targets[{target_index}].serial_throughput",
            target["serial_throughput"],
            "renders_per_minute",
            violations,
        )
    return violations


def validate_series_against_runs(
    data: Any,
    runs: Sequence[HostedRun],
) -> list[validate_result.Violation]:
    """Rebuild a series from its three source artifacts and compare exactly."""

    violations = validate_series(data)
    if violations:
        return violations
    assert isinstance(data, dict)
    try:
        expected = build_series(runs, generated_at=data["generated_at"])
    except ValueError as error:
        return [validate_result.Violation("$sources", str(error))]
    validate_result.require_equal("$", data, expected, violations)
    return violations


def _number(value: float) -> str:
    return f"{value:.6f}".rstrip("0").rstrip(".")


def render_markdown(data: dict[str, Any]) -> str:
    """Render every repeat and its between-run spread without selection."""

    labels = {target["id"]: target["label"] for target in data["targets"]}
    lines = [
        "# Pliego v0.3.3 hosted benchmark series",
        "",
        "> Evidence class: `github-hosted-exploratory`; publication status: `hosted-series`.",
        "> All three repeats are retained. No best or canonical repeat is selected.",
        "",
        f"- Run: [{data['source']['run_id']} attempt {data['source']['run_attempt']}]({data['source']['run_url']})",
        f"- Revision: `{data['source']['revision']}`",
        f"- Runner image: `{data['runner_image']['image_os']}` `{data['runner_image']['image_version']}`",
        f"- Fixture: `{data['fixture']['id']}`",
        f"- Repeats: 3 fresh GitHub-hosted jobs; {data['protocol']['sample_count']} timed samples per renderer per repeat",
        f"- Per repeat: 1 correctness preflight, {data['protocol']['warmup_iterations']} discarded warmups, seed {data['protocol']['seed']}",
        f"- Correctness: all {data['protocol']['sample_count']} timed samples per renderer passed the shared PDF oracle in every repeat",
        "",
        "## Retained source artifacts",
        "",
        "| Repeat | Comparison SHA-256 | Interleaved artifact SHA-256 | Schedule SHA-256 |",
        "| ---: | --- | --- | --- |",
    ]
    for run in data["runs"]:
        lines.append(
            f"| {run['repeat']} | `{run['comparison_sha256']}` | "
            f"`{run['interleaved_artifact_sha256']}` | `{run['schedule_sha256']}` |"
        )
    lines.extend(
        [
            "",
            "## Per-repeat hosted environments",
            "",
            "| Repeat | Runner name | CPU model | Cores | RAM bytes | Kernel | Virtualization |",
            "| ---: | --- | --- | ---: | ---: | --- | --- |",
        ]
    )
    for run in data["runs"]:
        lines.append(
            f"| {run['repeat']} | `{run['runner']['name']}` | `{run['host']['cpu_model']}` | "
            f"{run['host']['cores']} | {run['host']['ram_bytes']} | `{run['host']['kernel']}` | "
            f"`{run['host']['virtualization']}` |"
        )
    lines.extend(
        [
            "",
            "## Per-run p50s and between-run spread",
            "",
            "For `read_bytes` and `write_bytes`, values come from cgroup `io.stat`. Memory-backed stdout/stderr capture is excluded for every target. Browsershot's private tmpfs Node/Chrome `TMPDIR` is also excluded from block I/O but remains charged to cgroup memory; its PHP `TMPDIR`, HOME/XDG roots, explicit Chromium profile, artifacts, and PDF stay on the measured ext4 storage.",
            "",
            "| Renderer | Metric | Unit | Repeat 1 p50 | Repeat 2 p50 | Repeat 3 p50 | Min | Max | Mean | Relative spread |",
            "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for target in data["summary"]["targets"]:
        for metric_name in sorted(target["metrics"]):
            metric = target["metrics"][metric_name]
            values = [entry["p50"] for entry in metric["per_repeat"]]
            lines.append(
                f"| {labels[target['target_id']]} | `{metric_name}` | {metric['unit']} | "
                f"{_number(values[0])} | {_number(values[1])} | {_number(values[2])} | "
                f"{_number(metric['min'])} | {_number(metric['max'])} | {_number(metric['mean'])} | "
                f"{_number(metric['relative_spread_percent'])}% |"
            )
    lines.extend(
        [
            "",
            "## Serial throughput by repeat",
            "",
            "| Renderer | Repeat 1 renders/min | Repeat 2 renders/min | Repeat 3 renders/min | Min | Max | Mean | Relative spread |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for target in data["summary"]["targets"]:
        throughput = target["serial_throughput"]
        values = [entry["renders_per_minute"] for entry in throughput["per_repeat"]]
        lines.append(
            f"| {labels[target['target_id']]} | {_number(values[0])} | {_number(values[1])} | "
            f"{_number(values[2])} | {_number(throughput['min'])} | {_number(throughput['max'])} | "
            f"{_number(throughput['mean'])} | {_number(throughput['relative_spread_percent'])}% |"
        )
    lines.extend(
        [
            "",
            "## Claim boundary",
            "",
            data["claim_boundary"],
            "",
            "Relative spread is `(max - min) / mean * 100` across the three per-run p50s, or across the",
            "three serial-throughput values. The three source comparisons and raw interleaved artifacts are the retained",
            "verification sources for this summary.",
            "",
        ]
    )
    return "\n".join(lines)


def validate_series_bundle(output: Path, runs: Sequence[HostedRun]) -> list[validate_result.Violation]:
    """Validate one exact three-file series bundle against current source bundles."""

    violations: list[validate_result.Violation] = []
    if not output.is_dir():
        return [validate_result.Violation("$bundle", "directory does not exist")]
    entries = list(output.iterdir())
    actual_entries = {path.name for path in entries}
    validate_result.require_equal("$bundle.entries", actual_entries, SERIES_BUNDLE_ENTRIES, violations)
    for entry in entries:
        if not entry.is_file() or entry.is_symlink():
            violations.append(validate_result.Violation(f"$bundle.{entry.name}", "must be a regular file"))
    if violations:
        return violations
    try:
        current_runs = load_and_validate_runs([run.directory for run in runs])
    except ValueError as error:
        return [validate_result.Violation("$sources", str(error))]
    try:
        data = json.loads((output / SERIES_FILE).read_text(encoding="utf-8"))
        report = (output / REPORT_FILE).read_text(encoding="utf-8")
        checksum_lines = (output / CHECKSUM_FILE).read_text(encoding="ascii").splitlines()
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return [validate_result.Violation("$bundle", f"cannot read retained series bundle: {error}")]
    violations.extend(validate_series_against_runs(data, current_runs))
    if isinstance(data, dict):
        try:
            expected_report = render_markdown(data)
        except (KeyError, TypeError, ValueError) as error:
            violations.append(validate_result.Violation(f"$bundle.{REPORT_FILE}", f"cannot render report: {error}"))
        else:
            validate_result.require_equal(f"$bundle.{REPORT_FILE}", report, expected_report, violations)

    expected_checksummed = SERIES_BUNDLE_ENTRIES - {CHECKSUM_FILE}
    parsed: dict[str, str] = {}
    for index, line in enumerate(checksum_lines):
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)", line)
        if match is None:
            violations.append(validate_result.Violation(f"$bundle.{CHECKSUM_FILE}[{index}]", "has invalid syntax"))
            continue
        digest, filename = match.groups()
        if filename in parsed:
            violations.append(validate_result.Violation(f"$bundle.{CHECKSUM_FILE}[{index}]", "duplicates a filename"))
            continue
        parsed[filename] = digest
    validate_result.require_equal(f"$bundle.{CHECKSUM_FILE}.files", set(parsed), expected_checksummed, violations)
    for filename in sorted(set(parsed) & expected_checksummed):
        actual = hashlib.sha256((output / filename).read_bytes()).hexdigest()
        validate_result.require_equal(f"$bundle.{CHECKSUM_FILE}.{filename}", parsed[filename], actual, violations)
    return violations


def write_hosted_series(directories: Sequence[Path], output: Path) -> dict[str, Any]:
    """Validate inputs and write a new sealed output directory."""

    if output.exists():
        raise ValueError(f"output directory already exists: {output}")
    runs = load_and_validate_runs(directories)
    data = build_series(runs)
    violations = validate_series_against_runs(data, runs)
    if violations:
        raise ValueError(f"hosted series failed validation: {violations[0]}")
    output.mkdir(parents=True)
    series_path = output / SERIES_FILE
    report_path = output / REPORT_FILE
    run_comparison.atomic_json(series_path, data)
    report_path.write_text(render_markdown(data), encoding="utf-8")
    run_comparison.write_checksums(output, [series_path, report_path])
    bundle_violations = validate_series_bundle(output, runs)
    if bundle_violations:
        raise ValueError(f"hosted series bundle failed validation: {bundle_violations[0]}")
    return data


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("comparisons", nargs=3, type=Path, help="three hosted comparison output directories")
    destination = parser.add_mutually_exclusive_group(required=True)
    destination.add_argument("--out", type=Path, help="new hosted-series output directory")
    destination.add_argument("--validate", type=Path, help="validate an existing hosted-series output directory")
    args = parser.parse_args(argv)
    if args.validate is not None:
        try:
            runs = load_and_validate_runs(args.comparisons)
            violations = validate_series_bundle(args.validate, runs)
        except ValueError as error:
            fail(str(error))
        if violations:
            fail(f"existing hosted series bundle failed validation: {violations[0]}")
        print("hosted series bundle validated against all three exact source bundles")
        return 0
    assert args.out is not None
    try:
        data = write_hosted_series(args.comparisons, args.out)
    except ValueError as error:
        fail(str(error))
    print(f"wrote hosted series {data['series_sha256']} to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
