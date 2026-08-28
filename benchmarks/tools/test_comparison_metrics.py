#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
from typing import Any, Callable

import comparison_metrics
import run_benchmark
from test_validate_result import resource_usage as valid_resource_usage


TARGETS = ["zeta", "alpha"]


def passing_sample(target_id: str, index: int) -> dict[str, Any]:
    offset = 0 if target_id == "alpha" else 1000
    wall_ms = float(index + 1 + offset)
    sample = {
        "index": index,
        "ok": True,
        "exit_code": 0,
        "wall_ms": wall_ms,
        "one_shot_wall_ms": wall_ms * 2,
        "user_ms": float(index),
        "sys_ms": float(99 - index),
        "memory_current_bytes": 9_000 + offset + index,
        "memory_peak_bytes": 10_000 + offset + index,
        "sampled_peak_rss_kib_lower_bound": 20_000 + offset + index,
        "sampled_peak_pss_kib_lower_bound": 15_000 + offset + index,
        "read_bytes": 30_000 + offset + index,
        "write_bytes": 40_000 + offset + index,
        "read_operations": 30 + offset + index,
        "write_operations": 40 + offset + index,
        "measurement_method": "unavailable",
        "signal": None,
        "resource_usage": None,
        "phase_timings_ms": {
            "capture": float(index) / 10,
            "render": float(index) / 5,
        },
        "bridge_timings_ms": {"legacy_total": float(index) / 4},
        "bridge_timings": {
            "schema": "pliego.php-bridge-timings",
            "version": 1,
            "measurement_boundary": "render-invocation-before-timing-diagnostics",
            "total_ms": 10.0 + index,
            "native_engine_ms": 6.0 + index,
            "bridge_overhead_ms": 4.0,
            "setup_ms": {
                "runtime_resolution": 1.0,
                "runtime_install": None,
            },
            "phases_ms": {
                "laravel_setup": None,
                "view_render": 1.0,
                "bundle_staging": 1.0,
                "asset_manifest_hash": 1.0,
                "process_launch": 1.0,
                "stdin_stdout": 1.0,
                "native_wait": 1.0 + index,
                "result_parse": 1.0,
                "publication_copy": None,
                "cleanup": 1.0,
                "unattributed": 2.0,
            },
            "unavailable": {
                "laravel_setup": "outside-this-render-path",
                "runtime_install": "explicit-artisan-command",
                "publication_copy": "native-engine-publishes-directly",
            },
            "notes": {},
            "diagnostics": {"retained": True},
        },
        "output": {
            "pdf_bytes": 50_000 + offset + index,
            "pdf_sha256": "0" * 64,
            "page_count": 1,
            "artifact_bytes": 60_000 + offset + index,
            "published_pdf": True,
            "normalized_text_sha256": "1" * 64,
            "font_families": ["Synthetic"],
            "normalized_raster_sha256": "2" * 64,
        },
        "correctness": {"pass": True, "checks": [{"name": "synthetic", "status": "pass"}]},
        "failure": {"code": None, "message": None, "published_pdf": True},
    }
    if target_id.startswith("synthetic-"):
        sample.update(
            {
                "wall_ms": 1,
                "one_shot_wall_ms": 2,
                "user_ms": 1,
                "sys_ms": 0,
                "memory_current_bytes": 0,
                "memory_peak_bytes": 4096,
                "sampled_peak_rss_kib_lower_bound": 4,
                "sampled_peak_pss_kib_lower_bound": 3,
                "read_bytes": 0,
                "write_bytes": 1,
                "read_operations": 0,
                "write_operations": 1,
            }
        )
    return sample


def artifact_with(sample_count: int = 100) -> dict[str, Any]:
    return run_benchmark.execute_interleaved_run(
        TARGETS,
        "synthetic-fixture",
        0,
        sample_count,
        17,
        lambda _target: None,
        lambda _target, _iteration: None,
        passing_sample,
    )


def reseal(artifact: dict[str, Any]) -> None:
    fixture_id = artifact["schedule"]["fixture_id"]
    artifact["raw_samples"] = [
        run_benchmark.raw_sample_record(fixture_id, entry, record["sample"])
        for entry, record in zip(artifact["schedule"]["timed"], artifact["raw_samples"], strict=True)
    ]
    artifact["artifact_sha256"] = run_benchmark.interleaved_artifact_sha256(artifact)
    assert not run_benchmark.validate_interleaved_artifact(artifact)


def expect_value_error(operation: Callable[[], object], message: str) -> None:
    try:
        operation()
    except ValueError as error:
        assert message in str(error), str(error)
    else:
        raise AssertionError(f"expected ValueError containing {message!r}")


def with_one_cgroup_sample(source: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    artifact = deepcopy(source)
    sample = artifact["raw_samples"][0]["sample"]
    usage = valid_resource_usage()
    sample.update(
        {
            "wall_ms": usage["wall_ms"],
            "one_shot_wall_ms": 2,
            "user_ms": usage["cpu_user_ms"],
            "sys_ms": usage["cpu_sys_ms"],
            "memory_current_bytes": usage["memory_current_bytes"],
            "memory_peak_bytes": usage["memory_peak_bytes"],
            "sampled_peak_rss_kib_lower_bound": usage["sampled_diagnostics"]["sampled_peak_summed_rss_kib_lower_bound"],
            "sampled_peak_pss_kib_lower_bound": usage["sampled_diagnostics"]["sampled_peak_summed_pss_kib_lower_bound"],
            "read_bytes": usage["read_bytes"],
            "write_bytes": usage["write_bytes"],
            "read_operations": usage["read_operations"],
            "write_operations": usage["write_operations"],
            "measurement_method": "linux-cgroup-v2-v1",
            "resource_usage": usage,
        }
    )
    reseal(artifact)
    return artifact, sample


def main() -> None:
    artifact = artifact_with()
    retained_source = deepcopy(artifact)
    aggregated = comparison_metrics.aggregate_interleaved_artifact(artifact)
    assert artifact == retained_source

    # The validated schedule, not caller insertion order or a set, is authoritative.
    assert artifact["schedule"]["targets"] == ["alpha", "zeta"]
    assert [target["target_id"] for target in aggregated["targets"]] == artifact["schedule"]["targets"]
    assert aggregated["sample_count_per_target"] == 100
    assert aggregated["percentile_method"] == "nearest-rank-v1"

    alpha = aggregated["targets"][0]
    wall = alpha["metrics"]["wall_ms"]
    assert wall["statistics"] == {
        "min": 1.0,
        "p50": 50.0,
        "p95": 95.0,
        "p99": 99.0,
        "max": 100.0,
        "mean": 50.5,
    }
    expected_sample_ids = [record["sample_id"] for record in artifact["raw_samples"] if record["target_id"] == "alpha"]
    assert alpha["sample_ids"] == expected_sample_ids
    assert alpha["correctness_pass_count"] == 100
    assert alpha["pdf_sha256_variants"] == 1
    assert wall["sample_ids"] == expected_sample_ids
    assert all(metric["sample_ids"] == expected_sample_ids for metric in alpha["metrics"].values())

    expected_metrics = {
        "wall_ms",
        "one_shot_wall_ms",
        "cpu_user_ms",
        "cpu_system_ms",
        "cpu_total_ms",
        "memory_current_bytes",
        "memory_peak_bytes",
        "sampled_peak_rss_kib_lower_bound",
        "sampled_peak_pss_kib_lower_bound",
        "read_bytes",
        "write_bytes",
        "read_operations",
        "write_operations",
        "pdf_bytes",
        "artifact_bytes",
        "phase_timings_ms.capture",
        "phase_timings_ms.render",
        "bridge_timings_ms.legacy_total",
        "bridge_timings.total_ms",
        "bridge_timings.native_engine_ms",
        "bridge_timings.bridge_overhead_ms",
        "bridge_timings.setup_ms.runtime_resolution",
        "bridge_timings.phases_ms.view_render",
        "bridge_timings.phases_ms.bundle_staging",
        "bridge_timings.phases_ms.asset_manifest_hash",
        "bridge_timings.phases_ms.process_launch",
        "bridge_timings.phases_ms.stdin_stdout",
        "bridge_timings.phases_ms.native_wait",
        "bridge_timings.phases_ms.result_parse",
        "bridge_timings.phases_ms.cleanup",
        "bridge_timings.phases_ms.unattributed",
    }
    assert set(alpha["metrics"]) == expected_metrics
    assert "bridge_timings.setup_ms.runtime_install" not in alpha["metrics"]
    assert "bridge_timings.phases_ms.laravel_setup" not in alpha["metrics"]
    assert "bridge_timings.phases_ms.publication_copy" not in alpha["metrics"]

    # CPU must be summed for each sample before percentiles are selected.  Both
    # component tails are 98 ms, while every paired total is exactly 99 ms.
    cpu = alpha["metrics"]["cpu_total_ms"]["statistics"]
    assert cpu == {"min": 99.0, "p50": 99.0, "p95": 99.0, "p99": 99.0, "max": 99.0, "mean": 99.0}
    assert alpha["metrics"]["cpu_user_ms"]["statistics"] == {
        "min": 0.0,
        "p50": 49.0,
        "p95": 94.0,
        "p99": 98.0,
        "max": 99.0,
        "mean": 49.5,
    }
    assert alpha["metrics"]["phase_timings_ms.capture"]["statistics"]["p99"] == 9.8
    assert alpha["metrics"]["bridge_timings.phases_ms.native_wait"]["statistics"]["max"] == 100.0
    assert alpha["serial_throughput"]["renders_per_minute"] == 60000.0 / 101.0
    assert alpha["serial_throughput"]["sample_ids"] == expected_sample_ids

    partial_null = deepcopy(artifact)
    next(record for record in partial_null["raw_samples"] if record["target_id"] == "alpha")["sample"][
        "memory_peak_bytes"
    ] = None
    reseal(partial_null)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(partial_null),
        "partial-null metric 'memory_peak_bytes'",
    )

    partial_cpu = deepcopy(artifact)
    cpu_sample = next(record for record in partial_cpu["raw_samples"] if record["target_id"] == "alpha")["sample"]
    cpu_sample["sys_ms"] = None
    reseal(partial_cpu)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(partial_cpu),
        "partial-null metric 'cpu_system_ms'",
    )

    all_null = deepcopy(artifact)
    for record in all_null["raw_samples"]:
        if record["target_id"] == "alpha":
            record["sample"]["sampled_peak_pss_kib_lower_bound"] = None
    reseal(all_null)
    all_null_aggregate = comparison_metrics.aggregate_interleaved_artifact(all_null)
    assert "sampled_peak_pss_kib_lower_bound" not in all_null_aggregate["targets"][0]["metrics"]

    partial_dynamic_timing = deepcopy(artifact)
    dynamic_sample = next(record for record in partial_dynamic_timing["raw_samples"] if record["target_id"] == "alpha")[
        "sample"
    ]
    dynamic_sample["phase_timings_ms"].pop("capture")
    reseal(partial_dynamic_timing)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(partial_dynamic_timing),
        "partial-null metric 'phase_timings_ms.capture'",
    )

    partial_bridge_timing = deepcopy(artifact)
    bridge_sample = next(record for record in partial_bridge_timing["raw_samples"] if record["target_id"] == "alpha")[
        "sample"
    ]
    bridge_sample["bridge_timings"]["native_engine_ms"] = None
    reseal(partial_bridge_timing)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(partial_bridge_timing),
        "partial-null metric 'bridge_timings.native_engine_ms'",
    )

    invalid_wall, invalid_wall_sample = with_one_cgroup_sample(artifact)
    invalid_wall_sample["wall_ms"] = 2
    reseal(invalid_wall)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(invalid_wall),
        "resource-usage validation failed: $.raw_samples[0].sample.wall_ms",
    )

    invalid_cpu, invalid_cpu_sample = with_one_cgroup_sample(artifact)
    invalid_cpu_sample["resource_usage"]["counters"]["final"]["cpu_stat"]["user_usec"] = 2_000
    invalid_cpu_sample["resource_usage"]["counters"]["final"]["cpu_stat"]["usage_usec"] = 2_000
    reseal(invalid_cpu)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(invalid_cpu),
        ".resource_usage.cpu_user_ms",
    )

    invalid_io, invalid_io_sample = with_one_cgroup_sample(artifact)
    invalid_io_sample["resource_usage"]["counters"]["final"]["io_stat"]["8:0"]["wbytes"] = 2
    reseal(invalid_io)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(invalid_io),
        ".resource_usage.write_bytes",
    )

    failed_correctness = deepcopy(artifact)
    failed_sample = next(record for record in failed_correctness["raw_samples"] if record["target_id"] == "alpha")[
        "sample"
    ]
    failed_sample["correctness"] = {"pass": False, "checks": [{"name": "synthetic", "status": "fail"}]}
    reseal(failed_correctness)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(failed_correctness),
        "failed correctness",
    )

    undersampled = artifact_with(99)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(undersampled),
        "at least 100 are required for p99",
    )

    missing_sample = deepcopy(artifact)
    missing_sample["raw_samples"].pop()
    missing_sample["artifact_sha256"] = run_benchmark.interleaved_artifact_sha256(missing_sample)
    expect_value_error(
        lambda: comparison_metrics.aggregate_interleaved_artifact(missing_sample),
        "source interleaved artifact failed validation",
    )


if __name__ == "__main__":
    main()
