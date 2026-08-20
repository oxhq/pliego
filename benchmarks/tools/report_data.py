#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Generate, validate, and render prerequisite-only benchmark latency cells."""

from __future__ import annotations

import argparse
import html
import json
import sys
from pathlib import Path
from typing import Any

import run_benchmark
import validate_result

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-report-data.v1.json"
REPORT_SCHEMA = "pliego.benchmark-report-data"
REPORT_VERSION = 1
CELL_ID_CONTRACT = "pliego.benchmark-report-cell-id.v1"
AGGREGATION_CONTRACT = "pliego.benchmark-latency-cells.v1"
STATISTICS = ("min", "p50", "p95", "p99", "max", "mean")


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {path}: {error}") from error


def cell_id(
    artifact_sha256: str,
    fixture_id: str,
    target_id: str,
    statistic: str,
) -> str:
    return run_benchmark.canonical_json_sha256(
        [CELL_ID_CONTRACT, artifact_sha256, fixture_id, target_id, "wall_ms", statistic]
    )


def _artifact_error(violations: list[validate_result.Violation]) -> ValueError:
    return ValueError(f"source interleaved artifact failed validation: {violations[0]}")


def _report_from_valid_artifact(artifact: dict[str, Any]) -> dict[str, Any]:
    schedule = artifact["schedule"]
    artifact_sha256 = artifact["artifact_sha256"]
    fixture_id = schedule["fixture_id"]
    targets = schedule["targets"]
    records_by_target: dict[str, list[dict[str, Any]]] = {target_id: [] for target_id in targets}
    for record in artifact["raw_samples"]:
        records_by_target[record["target_id"]].append(record)

    cells: list[dict[str, Any]] = []
    for target_id in targets:
        records = records_by_target[target_id]
        if len(records) != schedule["sample_count"]:
            raise ValueError(f"target {target_id!r} does not retain every timed sample")
        if any(not record["sample"]["ok"] or not record["sample"]["correctness"]["pass"] for record in records):
            raise ValueError(f"target {target_id!r} has a timed sample that failed correctness")
        sample_ids = [record["sample_id"] for record in records]
        values = [record["sample"]["wall_ms"] for record in records]
        aggregate = validate_result.percentiles(values)
        for statistic in STATISTICS:
            cells.append(
                {
                    "cell_id": cell_id(artifact_sha256, fixture_id, target_id, statistic),
                    "artifact_sha256": artifact_sha256,
                    "fixture_id": fixture_id,
                    "target_id": target_id,
                    "metric": "wall_ms",
                    "statistic": statistic,
                    "unit": "ms",
                    "value": aggregate[statistic],
                    "sample_ids": list(sample_ids),
                }
            )

    return {
        "schema": REPORT_SCHEMA,
        "version": REPORT_VERSION,
        "publication_status": "prerequisite-only",
        "cell_id_contract": CELL_ID_CONTRACT,
        "source": {
            "artifact_schema": artifact["schema"],
            "artifact_version": artifact["version"],
            "artifact_sha256": artifact_sha256,
            "schedule_contract": schedule["contract"],
            "schedule_sha256": run_benchmark.canonical_json_sha256(schedule),
            "fixture_id": fixture_id,
            "targets": list(targets),
        },
        "aggregation": {
            "contract": AGGREGATION_CONTRACT,
            "metric": "wall_ms",
            "unit": "ms",
            "sample_policy": "all-timed-samples-must-pass",
            "percentile_method": validate_result.PERCENTILE_METHOD,
            "statistics": list(STATISTICS),
        },
        "cells": cells,
    }


def build_report_data(artifact: Any) -> dict[str, Any]:
    violations = run_benchmark.validate_interleaved_artifact(artifact)
    if violations:
        raise _artifact_error(violations)
    assert isinstance(artifact, dict)
    report = _report_from_valid_artifact(artifact)
    report_violations = validate_report_data(report, artifact)
    if report_violations:
        raise ValueError(f"generated report data failed validation: {report_violations[0]}")
    return report


def _sample_id_violation(
    path: str,
    actual: list[str],
    expected: list[str],
) -> validate_result.Violation:
    if len(actual) != len(set(actual)):
        return validate_result.Violation(path, "must not contain duplicate sample ids")
    missing = [sample_id for sample_id in expected if sample_id not in actual]
    if missing:
        return validate_result.Violation(path, f"is missing contributing sample ids {missing!r}")
    extra = [sample_id for sample_id in actual if sample_id not in expected]
    if extra:
        return validate_result.Violation(path, f"contains extra sample ids {extra!r}")
    return validate_result.Violation(path, "must preserve contributing sample ids in timed schedule order")


def validate_report_data(
    data: Any,
    artifact: Any,
    schema: dict[str, Any] | None = None,
) -> list[validate_result.Violation]:
    artifact_violations = run_benchmark.validate_interleaved_artifact(artifact)
    if artifact_violations:
        return [
            validate_result.Violation(f"$artifact{violation.path[1:]}", violation.message)
            for violation in artifact_violations
        ]

    schema = schema or json.loads(DEFAULT_SCHEMA.read_text(encoding="utf-8"))
    violations: list[validate_result.Violation] = []
    validate_result.validate(data, schema, "$", violations)
    if violations:
        return violations

    assert isinstance(data, dict) and isinstance(artifact, dict)
    try:
        expected = _report_from_valid_artifact(artifact)
    except ValueError as error:
        violations.append(validate_result.Violation("$artifact.raw_samples", str(error)))
        return violations

    for field, expected_value in expected["source"].items():
        validate_result.require_equal(f"$.source.{field}", data["source"][field], expected_value, violations)
    for field, expected_value in expected["aggregation"].items():
        validate_result.require_equal(f"$.aggregation.{field}", data["aggregation"][field], expected_value, violations)

    cells = data["cells"]
    expected_cells = expected["cells"]
    if len(cells) != len(expected_cells):
        violations.append(
            validate_result.Violation("$.cells", f"must contain exactly the {len(expected_cells)} canonical cells")
        )
    cell_ids = [cell["cell_id"] for cell in cells]
    if len(cell_ids) != len(set(cell_ids)):
        violations.append(validate_result.Violation("$.cells", "must not contain duplicate cell ids"))

    for index, cell in enumerate(cells[: len(expected_cells)]):
        path = f"$.cells[{index}]"
        expected_cell = expected_cells[index]
        for field in (
            "cell_id",
            "artifact_sha256",
            "fixture_id",
            "target_id",
            "metric",
            "statistic",
            "unit",
        ):
            validate_result.require_equal(f"{path}.{field}", cell[field], expected_cell[field], violations)
        if cell["sample_ids"] != expected_cell["sample_ids"]:
            violations.append(
                _sample_id_violation(f"{path}.sample_ids", cell["sample_ids"], expected_cell["sample_ids"])
            )
        if cell["value"] != expected_cell["value"]:
            violations.append(
                validate_result.Violation(
                    f"{path}.value",
                    f"must equal the recomputed {expected_cell['statistic']} {expected_cell['value']!r}",
                )
            )
    return violations


def markdown_text(value: str) -> str:
    """Escape one untrusted value for a Markdown table cell."""

    return (
        html.escape(value, quote=False)
        .replace("|", "&#124;")
        .replace("\r\n", "<br>")
        .replace("\r", "<br>")
        .replace("\n", "<br>")
    )


def render_markdown(data: Any, artifact: Any) -> str:
    """Render validated latency cells without deriving new claims or values."""

    violations = validate_report_data(data, artifact)
    if violations:
        raise ValueError(f"report data failed validation: {violations[0]}")

    assert isinstance(data, dict)
    cells = {(cell["target_id"], cell["statistic"]): cell for cell in data["cells"]}
    statistics = data["aggregation"]["statistics"]
    targets = data["source"]["targets"]
    lines = [
        "# Benchmark latency table",
        "",
        "> Status: prerequisite-only. This table is not publishable benchmark evidence.",
        "",
        f"- Fixture: <code>{markdown_text(data['source']['fixture_id'])}</code>",
        f"- Interleaved artifact SHA-256: `{data['source']['artifact_sha256']}`",
        f"- Schedule SHA-256: `{data['source']['schedule_sha256']}`",
        "- Metric: `wall_ms` (`ms`), nearest-rank-v1 percentiles",
        "",
        "| Target | " + " | ".join(f"{statistic} (ms)" for statistic in statistics) + " |",
        "| --- | " + " | ".join("---:" for _ in statistics) + " |",
    ]
    for target_id in targets:
        values = []
        for statistic in statistics:
            cell = cells[(target_id, statistic)]
            value = json.dumps(cell["value"], allow_nan=False, separators=(",", ":"))
            values.append(f"[{value}](#cell-{cell['cell_id']})")
        lines.append(f"| <code>{markdown_text(target_id)}</code> | " + " | ".join(values) + " |")

    lines.extend(["", "## Cell provenance", ""])
    for target_id in targets:
        lines.append(f"### <code>{markdown_text(target_id)}</code>")
        lines.append("")
        target_cells = [cells[(target_id, statistic)] for statistic in statistics]
        for cell in target_cells:
            lines.append(
                f"- <a id=\"cell-{cell['cell_id']}\"></a>`{cell['statistic']}` cell: `{cell['cell_id']}`"
            )
        lines.append(
            "- Timed sample IDs, in schedule order: "
            + ", ".join(f"`{sample_id}`" for sample_id in target_cells[0]["sample_ids"])
        )
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    generate = commands.add_parser("generate", help="generate canonical report data from an interleaved artifact")
    generate.add_argument("artifact")
    generate.add_argument("--out", required=True)
    validate = commands.add_parser("validate", help="validate report data against its source artifact")
    validate.add_argument("report")
    validate.add_argument("--artifact", required=True)
    render = commands.add_parser("render", help="render a validated provenance-bearing Markdown table")
    render.add_argument("report")
    render.add_argument("--artifact", required=True)
    render.add_argument("--out", required=True)
    args = parser.parse_args()

    try:
        artifact_path = Path(args.artifact)
        artifact = load_json(artifact_path)
        if args.command == "generate":
            out = Path(args.out)
            if out.resolve() == artifact_path.resolve() or (out.exists() and out.samefile(artifact_path)):
                raise ValueError("report output must not overwrite its source artifact")
            report = build_report_data(artifact)
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
            print(f"wrote prerequisite-only report data to {out}")
            return 0
        report_path = Path(args.report)
        report = load_json(report_path)
        violations = validate_report_data(report, artifact)
        if violations:
            for violation in violations:
                print(f"violation {violation}", file=sys.stderr)
            print(f"report data failed validation with {len(violations)} violation(s)", file=sys.stderr)
            return 1
        if args.command == "render":
            out = Path(args.out)
            for source in (artifact_path, report_path):
                if out.resolve() == source.resolve() or (out.exists() and out.samefile(source)):
                    raise ValueError("Markdown output must not overwrite its report data or source artifact")
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(render_markdown(report, artifact), encoding="utf-8")
            print(f"wrote prerequisite-only Markdown table to {out}")
            return 0
    except (OSError, ValueError) as error:
        print(f"report_data: {error}", file=sys.stderr)
        return 1
    print("report data validated against its source artifact and exact raw samples")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
