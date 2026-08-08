#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
import json
from pathlib import Path

import validate_result


SCHEMA = json.loads(
    (Path(__file__).resolve().parents[1] / "schema" / "benchmark-result.v1.json").read_text(encoding="utf-8")
)
HASH = "0" * 64
PERCENTILES = {key: 1 for key in ("min", "p50", "p95", "p99", "max", "mean")}
ZERO_PERCENTILES = {key: 0 for key in ("min", "p50", "p95", "p99", "max", "mean")}


def process_tree() -> dict:
    return {
        "method": "linux-procfs-session-poll-v1",
        "scope": "new-session-and-inherited-descendants",
        "root_pid": 10,
        "session_id": 10,
        "sample_interval_ms": 75,
        "pss_interval_ms": 250,
        "max_sample_gap_ms": 51,
        "clock_ticks_per_second": 100,
        "page_size_bytes": 4096,
        "wall_ms": 1,
        "exit_code": 0,
        "signal": None,
        "peak_summed_rss_kib": 4,
        "peak_summed_pss_kib": 3,
        "pss_method": "procfs-smaps_rollup-summed",
        "cpu_user_ticks": 1,
        "cpu_sys_ticks": 0,
        "cpu_user_ms": 10,
        "cpu_sys_ms": 0,
        "read_bytes": 0,
        "write_bytes": 1,
        "observed_pids": [10],
        "tree_complete": True,
        "lingering_pids": [],
        "discovery": "test",
        "sampler_cpu_user_ms": 0,
        "sampler_cpu_sys_ms": 0,
        "sampler_cpu_percent_of_wall": 0,
        "processes": [
            {
                "pid": 10,
                "start_ticks": 1,
                "first_seen_ms": 0,
                "last_seen_ms": 1,
                "ppid": 1,
                "process_group": 10,
                "user_ticks": 1,
                "sys_ticks": 0,
                "read_bytes": 0,
                "write_bytes": 1,
            }
        ],
        "samples": [
            {
                "elapsed_ms": 0,
                "summed_rss_kib": 4,
                "summed_pss_kib": 3,
                "processes": [{"pid": 10, "start_ticks": 1, "rss_pages": 1, "pss_kib": 3}],
            }
        ],
    }


def result() -> dict:
    return {
        "schema": "pliego.benchmark-result",
        "version": 1,
        "status": "supported",
        "generated_at": "2026-08-08T00:00:00+00:00",
        "host": {
            "os": "Linux",
            "arch": "x86_64",
            "kernel": "test",
            "cpu_model": "test",
            "cores": 1,
            "ram_bytes": 1,
            "dedicated": True,
        },
        "toolchain": {
            "engine": {
                "name": "pliego",
                "version": "0.1.1",
                "binary_path": "/tmp/pliego",
            },
            "python_version": "3.11",
            "php_version": "8.3",
            "harness_revision": "4f0beb41a4d",
        },
        "protocol": {
            "warmup_iterations": 1,
            "sample_count": 1,
            "sample_order": "sequential",
            "network": "disabled",
            "binary_profile": "checked-release",
            "rss_method": "linux-procfs-session-poll-v1",
        },
        "target": {"id": "pliego-0.1.1", "label": "Pliego 0.1.1"},
        "fixture": {
            "id": "minimal-static",
            "purpose": "startup",
            "category": "static",
            "input": "input.html",
            "input_sha256": HASH,
            "bundle_sha256": HASH,
            "expected_page_count": 1,
            "expected_failure_code": None,
        },
        "samples": [
            {
                "index": 0,
                "ok": True,
                "exit_code": 0,
                "wall_ms": 1,
                "user_ms": 1,
                "sys_ms": 0,
                "peak_rss_kib": 1,
                "peak_pss_kib": 1,
                "read_bytes": 0,
                "write_bytes": 1,
                "rss_method": "linux-procfs-session-poll-v1",
                "signal": None,
                "process_tree": process_tree(),
                "phase_timings_ms": {"capture": 1},
                "output": {
                    "pdf_bytes": 1,
                    "pdf_sha256": HASH,
                    "page_count": 1,
                    "artifact_bytes": 0,
                    "published_pdf": True,
                },
                "correctness": {
                    "pass": True,
                    "checks": [{"name": "pdf_published", "status": "pass"}],
                },
                "failure": {"code": None, "published_pdf": True},
            }
        ],
        "aggregates": {
            "latency": PERCENTILES,
            "cpu": {
                "user_ms": PERCENTILES,
                "sys_ms": ZERO_PERCENTILES,
                "wall_ms": PERCENTILES,
            },
            "memory": {"peak_rss_kib": PERCENTILES, "peak_pss_kib": PERCENTILES},
            "io": {"read_bytes": PERCENTILES, "write_bytes": PERCENTILES},
            "scaling": {"per_page_wall_ms": 1, "per_page_peak_rss_kib": 1},
            "throughput": {"renders_per_minute": 60000, "concurrency": 1},
            "output": {"pdf_bytes": PERCENTILES, "page_count": 1},
            "correctness": {"pass_count": 1, "total": 1, "passed": True},
            "determinism": {
                "identical_pdf_sha256": 1,
                "total": 1,
                "pdf_sha256_variants": 1,
            },
            "failures": {"count": 0, "codes": {}},
        },
    }


def errors(value: object) -> list[validate_result.Violation]:
    return validate_result.validate_document(value, SCHEMA)


def must_fail(value: object, expected: str) -> None:
    found = errors(value)
    assert found and expected in "\n".join(map(str, found)), found


def main() -> None:
    valid = result()
    assert not errors(valid)
    legacy = deepcopy(valid)
    del legacy["status"]
    del legacy["aggregates"]["failures"]["codes"]
    del legacy["fixture"]["expected_page_count"]
    del legacy["fixture"]["expected_failure_code"]
    assert not errors(legacy)

    supported = deepcopy(valid)
    supported["toolchain"]["engine"] = {
        "name": "dompdf",
        "version": "3.1.0",
        "package": "dompdf/dompdf",
    }
    failed = deepcopy(valid)
    failed["status"] = "failed"
    failed["samples"][0]["ok"] = False
    failed["samples"][0]["correctness"] = {
        "pass": False,
        "checks": [{"name": "render", "status": "fail"}],
    }
    failed["samples"][0]["failure"]["code"] = "RENDER_FAILED"
    failed["aggregates"]["correctness"] = {"pass_count": 0, "total": 1, "passed": False}
    failed["aggregates"]["latency"] = ZERO_PERCENTILES
    failed["aggregates"]["cpu"] = {
        "user_ms": ZERO_PERCENTILES,
        "sys_ms": ZERO_PERCENTILES,
        "wall_ms": ZERO_PERCENTILES,
    }
    failed["aggregates"]["memory"] = {"peak_rss_kib": ZERO_PERCENTILES}
    failed["aggregates"]["scaling"] = {"per_page_wall_ms": 0, "per_page_peak_rss_kib": 0}
    del failed["aggregates"]["throughput"]
    failed["aggregates"]["output"]["pdf_bytes"] = ZERO_PERCENTILES
    failed["aggregates"]["determinism"] = {
        "identical_pdf_sha256": 0,
        "total": 0,
        "pdf_sha256_variants": 0,
    }
    failed["aggregates"]["failures"] = {"count": 1, "codes": {"RENDER_FAILED": 1}}
    failed["samples"][0]["retained"] = {
        "output_dir": "/tmp/pliego-output",
        "artifacts_dir": "/tmp/pliego-artifacts",
    }
    expected_failure = deepcopy(valid)
    expected_failure["fixture"]["expected_page_count"] = None
    expected_failure["fixture"]["expected_failure_code"] = "UNSUPPORTED_PAINT"
    expected_failure["samples"][0]["exit_code"] = 1
    expected_failure["samples"][0]["correctness"]["checks"] = [
        {"name": "render_failed_closed", "status": "pass"},
        {"name": "failure_code", "status": "pass"},
        {"name": "pdf_not_published", "status": "pass"},
    ]
    expected_failure["samples"][0]["output"] = {
        "pdf_bytes": None,
        "pdf_sha256": None,
        "page_count": None,
        "artifact_bytes": 1,
        "published_pdf": False,
    }
    expected_failure["samples"][0]["failure"] = {
        "code": "UNSUPPORTED_PAINT",
        "published_pdf": False,
    }
    expected_failure["aggregates"]["output"]["pdf_bytes"] = ZERO_PERCENTILES
    expected_failure["aggregates"]["output"]["page_count"] = None
    del expected_failure["aggregates"]["scaling"]
    expected_failure["aggregates"]["determinism"] = {
        "identical_pdf_sha256": 0,
        "total": 0,
        "pdf_sha256_variants": 0,
    }
    assert not errors(expected_failure)
    not_applicable = {
        key: deepcopy(valid[key])
        for key in ("schema", "version", "generated_at", "host", "toolchain", "target", "fixture")
    }
    not_applicable.update(
        status="not-applicable",
        reason="fixture requires JavaScript",
        protocol={
            "warmup_iterations": 0,
            "sample_count": 0,
            "sample_order": "sequential",
            "network": "disabled",
            "binary_profile": "package",
            "rss_method": "unavailable",
        },
    )
    bundle = {
        "schema": "pliego.benchmark-bundle",
        "version": 1,
        "generated_at": valid["generated_at"],
        "results": [legacy, supported, failed, expected_failure, not_applicable],
    }
    assert not errors(bundle)

    timed = deepcopy(valid)
    timed["samples"][0]["bridge_timings"] = {
        "schema": "pliego.php-bridge-timings",
        "version": 1,
        "measurement_boundary": "render-invocation-before-timing-diagnostics",
        "total_ms": 1.0,
        "native_engine_ms": 0.25,
        "bridge_overhead_ms": 0.75,
        "setup_ms": {
            "runtime_resolution": None,
            "runtime_install": None,
        },
        "phases_ms": {
            "laravel_setup": None,
            "view_render": None,
            "bundle_staging": 0.1,
            "asset_manifest_hash": 0.1,
            "process_launch": 0.1,
            "stdin_stdout": 0.1,
            "native_wait": 0.3,
            "result_parse": 0.1,
            "publication_copy": None,
            "cleanup": 0.1,
            "unattributed": 0.1,
        },
        "unavailable": {
            "laravel_setup": "outside-this-render-path",
            "view_render": "outside-this-render-path",
            "runtime_resolution": "outside-this-render-path",
            "runtime_install": "explicit-artisan-command",
            "publication_copy": "native-engine-publishes-directly",
        },
        "notes": {"native_engine_ms": "contained within native_wait"},
        "diagnostics": {"retained": True},
    }
    assert not errors(timed)
    legacy_timed = deepcopy(valid)
    legacy_timed["samples"][0]["bridge_timings_ms"] = {"php_bridge": 1.0}
    assert not errors(legacy_timed)

    must_fail([valid], "oneOf")
    for field in ("rss_method", "output", "correctness", "failure"):
        broken = deepcopy(valid)
        del broken["samples"][0][field]
        must_fail(broken, field)
    broken = deepcopy(valid)
    broken["samples"][0]["phase_timings_ms"]["capture"] = "fast"
    must_fail(broken, "phase_timings_ms.capture")
    broken = deepcopy(valid)
    broken["samples"][0]["process_tree"] = "opaque"
    must_fail(broken, "process_tree")
    broken = deepcopy(valid)
    del broken["samples"][0]["process_tree"]["samples"]
    must_fail(broken, "samples")
    broken = deepcopy(valid)
    broken["toolchain"]["competitors"] = {"dompdf": 3}
    must_fail(broken, "competitors.dompdf")
    broken = deepcopy(valid)
    broken["aggregates"]["failures"]["codes"]["TIMEOUT"] = 0
    must_fail(broken, "minimum")
    broken = deepcopy(valid)
    broken["fixture"]["input_sha256"] = "unknown"
    must_fail(broken, "pattern")
    broken = deepcopy(timed)
    broken["samples"][0]["bridge_timings"]["native_engine_ms"] = -1
    must_fail(broken, "minimum")
    broken = deepcopy(timed)
    broken["samples"][0]["bridge_timings"]["phases_ms"]["bundle_staging"] = 99
    must_fail(broken, "bridge_timings.total_ms")
    broken = deepcopy(timed)
    broken["samples"][0]["bridge_timings"]["bridge_overhead_ms"] = 999
    must_fail(broken, "bridge_timings.bridge_overhead_ms")
    broken = deepcopy(timed)
    broken["samples"][0]["bridge_timings"]["native_engine_ms"] = None
    must_fail(broken, "bridge_timings.bridge_overhead_ms")
    broken = deepcopy(timed)
    broken["samples"][0]["bridge_timings"]["phases_ms"]["invented"] = 0
    must_fail(broken, "unexpected property")
    broken = deepcopy(not_applicable)
    broken["samples"] = []
    must_fail(broken, "samples")

    broken = deepcopy(valid)
    broken["protocol"]["sample_count"] = 2
    must_fail(broken, "protocol.sample_count")
    broken = deepcopy(valid)
    broken["samples"][0]["ok"] = False
    must_fail(broken, "samples[0].ok")
    broken = deepcopy(valid)
    broken["samples"][0]["correctness"]["pass"] = False
    must_fail(broken, "samples[0].correctness.pass")
    broken = deepcopy(valid)
    broken["samples"][0]["failure"]["published_pdf"] = False
    must_fail(broken, "samples[0].failure.published_pdf")
    broken = deepcopy(valid)
    broken["aggregates"]["correctness"]["total"] = 2
    must_fail(broken, "aggregates.correctness.total")
    broken = deepcopy(valid)
    broken["status"] = "failed"
    must_fail(broken, "$.status")
    broken = deepcopy(valid)
    broken["aggregates"]["failures"]["codes"] = {"GHOST": 1}
    must_fail(broken, "aggregates.failures.codes")
    broken = deepcopy(valid)
    broken["aggregates"]["determinism"]["total"] = 0
    must_fail(broken, "aggregates.determinism.total")
    broken = deepcopy(valid)
    broken["aggregates"]["latency"]["p50"] = 999
    must_fail(broken, "aggregates.latency")
    broken = deepcopy(valid)
    broken["aggregates"]["memory"]["peak_rss_kib"]["max"] = 999
    must_fail(broken, "aggregates.memory.peak_rss_kib")
    broken = deepcopy(valid)
    broken["aggregates"]["output"]["pdf_bytes"]["mean"] = 999
    must_fail(broken, "aggregates.output.pdf_bytes")
    broken = deepcopy(valid)
    broken["aggregates"]["scaling"]["per_page_wall_ms"] = 999
    must_fail(broken, "aggregates.scaling")
    broken = deepcopy(valid)
    broken["aggregates"]["throughput"]["renders_per_minute"] = 999
    must_fail(broken, "aggregates.throughput")
    broken = deepcopy(valid)
    broken["samples"][0]["correctness"]["checks"][0]["status"] = "unverified"
    must_fail(broken, "samples[0].correctness.pass")
    broken = deepcopy(valid)
    broken["samples"][0]["index"] = 1
    must_fail(broken, "sample indices")
    broken = deepcopy(valid)
    broken["samples"][0]["rss_method"] = "unavailable"
    must_fail(broken, "protocol rss_method")
    broken = deepcopy(valid)
    broken["samples"][0]["failure"]["code"] = "PANIC"
    must_fail(broken, "cannot report a failure")
    broken = deepcopy(valid)
    broken["samples"][0]["output"]["page_count"] = 2
    must_fail(broken, "expected page count 1")
    broken = deepcopy(valid)
    broken["aggregates"]["output"]["page_count"] = 2
    must_fail(broken, "fixture expected_page_count 1")

    no_pdf = deepcopy(valid)
    no_pdf["samples"][0]["output"]["published_pdf"] = False
    no_pdf["samples"][0]["failure"]["published_pdf"] = False
    must_fail(no_pdf, "passing normal fixtures must publish a PDF")

    missing_retained = deepcopy(failed)
    del missing_retained["samples"][0]["retained"]
    must_fail(missing_retained, "failed samples must retain")

    contradiction = deepcopy(valid)
    contradiction["samples"][0]["correctness"]["pass"] = False
    contradiction["samples"][0]["correctness"]["checks"][0]["status"] = "fail"
    must_fail(contradiction, "aggregates.correctness.pass_count")

    nested = deepcopy(bundle)
    nested["results"][1]["protocol"]["sample_count"] = 2
    must_fail(nested, "$.results[1].protocol.sample_count")
    print("benchmark result validator self-check passed")


if __name__ == "__main__":
    main()
