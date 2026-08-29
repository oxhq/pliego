#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Aggregate comparison metrics from an interleaved benchmark artifact.

The source artifact remains the authority for target and sample ordering.  This
module only accepts a fully validated artifact and deliberately requires at
least 100 timed samples per target so that a nearest-rank p99 represents a
distinct tail observation rather than an undersampled maximum.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import run_benchmark
import validate_result


MINIMUM_SAMPLES_FOR_P99 = 100
STATISTICS = ("min", "p50", "p95", "p99", "max", "mean")


@dataclass(frozen=True)
class MetricDefinition:
    """One ordered metric contract and its location in a retained sample."""

    name: str
    unit: str
    path: tuple[str, ...] | None = None
    components: tuple[str, ...] = ()


RAW_ONLY_COMPARATIVE_METRICS = frozenset({"sampled_peak_pss_kib_lower_bound"})


METRIC_REGISTRY = (
    MetricDefinition("wall_ms", "ms", ("wall_ms",)),
    MetricDefinition("one_shot_wall_ms", "ms", ("one_shot_wall_ms",)),
    MetricDefinition("cpu_user_ms", "ms", ("user_ms",)),
    MetricDefinition("cpu_system_ms", "ms", ("sys_ms",)),
    MetricDefinition("cpu_total_ms", "ms", components=("user_ms", "sys_ms")),
    MetricDefinition("memory_current_bytes", "bytes", ("memory_current_bytes",)),
    MetricDefinition("memory_peak_bytes", "bytes", ("memory_peak_bytes",)),
    MetricDefinition(
        "sampled_peak_rss_kib_lower_bound",
        "KiB",
        ("sampled_peak_rss_kib_lower_bound",),
    ),
    # RAW_ONLY_COMPARATIVE_METRICS fields and cadence observations remain in
    # the raw samples. Their slower cadence can miss short processes, so
    # aggregating only the observed subset would make availability and
    # percentiles depend on runner speed.
    MetricDefinition("read_bytes", "bytes", ("read_bytes",)),
    MetricDefinition("write_bytes", "bytes", ("write_bytes",)),
    MetricDefinition("read_operations", "operations", ("read_operations",)),
    MetricDefinition("write_operations", "operations", ("write_operations",)),
    MetricDefinition("pdf_bytes", "bytes", ("output", "pdf_bytes")),
    MetricDefinition("artifact_bytes", "bytes", ("output", "artifact_bytes")),
)

BRIDGE_TIMING_REGISTRY = (
    MetricDefinition("bridge_timings.total_ms", "ms", ("bridge_timings", "total_ms")),
    MetricDefinition(
        "bridge_timings.native_engine_ms",
        "ms",
        ("bridge_timings", "native_engine_ms"),
    ),
    MetricDefinition(
        "bridge_timings.bridge_overhead_ms",
        "ms",
        ("bridge_timings", "bridge_overhead_ms"),
    ),
    MetricDefinition(
        "bridge_timings.setup_ms.runtime_resolution",
        "ms",
        ("bridge_timings", "setup_ms", "runtime_resolution"),
    ),
    MetricDefinition(
        "bridge_timings.setup_ms.runtime_install",
        "ms",
        ("bridge_timings", "setup_ms", "runtime_install"),
    ),
    *(
        MetricDefinition(
            f"bridge_timings.phases_ms.{phase}",
            "ms",
            ("bridge_timings", "phases_ms", phase),
        )
        for phase in (
            "laravel_setup",
            "view_render",
            "bundle_staging",
            "asset_manifest_hash",
            "process_launch",
            "stdin_stdout",
            "native_wait",
            "result_parse",
            "publication_copy",
            "cleanup",
            "unattributed",
        )
    ),
)


def _value_at_path(sample: dict[str, Any], path: tuple[str, ...]) -> int | float | None:
    value: Any = sample
    for key in path:
        if not isinstance(value, dict) or key not in value:
            return None
        value = value[key]
    return value


def _metric_value(sample: dict[str, Any], metric: MetricDefinition) -> int | float | None:
    if metric.components:
        values = [sample[name] for name in metric.components]
        if all(value is None for value in values):
            return None
        if any(value is None for value in values):
            raise ValueError(
                f"metric {metric.name!r} has partial-null components "
                f"{metric.components!r} in sample index {sample['index']}"
            )
        return sum(values)

    assert metric.path is not None
    return _value_at_path(sample, metric.path)


def _dynamic_timing_metrics(
    records: list[dict[str, Any]],
    field: str,
) -> tuple[MetricDefinition, ...]:
    """Discover ordered scalar timings while treating missing keys as nulls."""

    keys: set[str] = set()
    for record in records:
        timings = record["sample"].get(field)
        if timings is not None:
            if not isinstance(timings, dict):
                raise ValueError(f"metric group {field!r} is not an object in sample index {record['sample']['index']}")
            keys.update(timings)
    return tuple(MetricDefinition(f"{field}.{key}", "ms", (field, key)) for key in sorted(keys))


def _validate_cgroup_measurements(artifact: dict[str, Any]) -> None:
    for index, record in enumerate(artifact["raw_samples"]):
        sample = record["sample"]
        if sample.get("measurement_method") != "linux-cgroup-v2-v1":
            continue
        violations: list[validate_result.Violation] = []
        validate_result.validate_resource_usage(sample, f"$.raw_samples[{index}].sample", violations)
        if violations:
            raise ValueError(f"resource-usage validation failed: {violations[0]}")


def _available_metric(
    target_id: str,
    metric: MetricDefinition,
    records: list[dict[str, Any]],
) -> dict[str, Any] | None:
    values = [_metric_value(record["sample"], metric) for record in records]
    null_count = sum(value is None for value in values)
    if null_count == len(values):
        return None
    if null_count:
        raise ValueError(
            f"target {target_id!r} has a partial-null metric {metric.name!r}: "
            f"{null_count} of {len(values)} timed samples are null"
        )

    measured = [value for value in values if value is not None]
    aggregate = validate_result.percentiles(measured)
    return {
        "unit": metric.unit,
        "statistics": {statistic: aggregate[statistic] for statistic in STATISTICS},
        "sample_ids": [record["sample_id"] for record in records],
    }


def aggregate_interleaved_artifact(artifact: Any) -> dict[str, Any]:
    """Return deterministic per-target aggregates with exact sample provenance.

    All target order comes from ``schedule.targets`` and every metric retains
    the sample identifiers that contributed to it in timed schedule order.
    Metrics that are null for every sample are omitted; partial availability is
    rejected because it would silently change the population being compared.
    """

    violations = run_benchmark.validate_interleaved_artifact(artifact)
    if violations:
        raise ValueError(f"source interleaved artifact failed validation: {violations[0]}")

    assert isinstance(artifact, dict)
    _validate_cgroup_measurements(artifact)
    schedule = artifact["schedule"]
    sample_count = schedule["sample_count"]
    if sample_count < MINIMUM_SAMPLES_FOR_P99:
        raise ValueError(
            f"interleaved artifact retains {sample_count} timed samples per target; "
            f"at least {MINIMUM_SAMPLES_FOR_P99} are required for p99"
        )

    target_ids = schedule["targets"]
    records_by_target: dict[str, list[dict[str, Any]]] = {target_id: [] for target_id in target_ids}
    for record in artifact["raw_samples"]:
        target_id = record["target_id"]
        if target_id not in records_by_target:
            raise ValueError(f"timed sample references missing target {target_id!r}")
        records_by_target[target_id].append(record)

    target_aggregates: list[dict[str, Any]] = []
    for target_id in target_ids:
        records = records_by_target[target_id]
        if len(records) != sample_count:
            raise ValueError(
                f"target {target_id!r} retains {len(records)} timed samples; expected exactly {sample_count}"
            )
        if any(not record["sample"]["ok"] or not record["sample"]["correctness"]["pass"] for record in records):
            raise ValueError(f"target {target_id!r} has a timed sample that failed correctness")

        metrics: dict[str, dict[str, Any]] = {}
        metric_definitions = (
            *METRIC_REGISTRY,
            *_dynamic_timing_metrics(records, "phase_timings_ms"),
            *_dynamic_timing_metrics(records, "bridge_timings_ms"),
            *BRIDGE_TIMING_REGISTRY,
        )
        for metric in metric_definitions:
            aggregate = _available_metric(target_id, metric, records)
            if aggregate is not None:
                metrics[metric.name] = aggregate

        one_shot = metrics["one_shot_wall_ms"]
        mean_one_shot_wall_ms = one_shot["statistics"]["mean"]
        if mean_one_shot_wall_ms <= 0:
            raise ValueError(f"target {target_id!r} has a non-positive mean one-shot wall time")

        sample_ids = [record["sample_id"] for record in records]
        pdf_hashes = [record["sample"]["output"]["pdf_sha256"] for record in records]
        target_aggregates.append(
            {
                "target_id": target_id,
                "sample_count": len(records),
                "sample_ids": sample_ids,
                "correctness_pass_count": len(records),
                "pdf_sha256_variants": len(set(pdf_hashes)),
                "metrics": metrics,
                "serial_throughput": {
                    "unit": "renders/minute",
                    "renders_per_minute": 60000.0 / mean_one_shot_wall_ms,
                    "mean_one_shot_wall_ms": mean_one_shot_wall_ms,
                    "sample_ids": list(sample_ids),
                },
            }
        )

    return {
        "artifact_sha256": artifact["artifact_sha256"],
        "fixture_id": schedule["fixture_id"],
        "sample_count_per_target": sample_count,
        "minimum_samples_for_p99": MINIMUM_SAMPLES_FOR_P99,
        "percentile_method": validate_result.PERCENTILE_METHOD,
        "statistics": list(STATISTICS),
        "targets": target_aggregates,
    }
