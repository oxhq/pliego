#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
import json
from pathlib import Path
import tomllib

import validate_result

SCHEMA = json.loads(
    (Path(__file__).resolve().parents[1] / "schema" / "benchmark-result.v1.json").read_text(encoding="utf-8")
)
HASH = "0" * 64
TEXT = (
    "Minimal Hello, Pliego. This fixture measures pure startup: one small page, one bundled font, no scripts or images."
)
RASTER_HASH = "0f9edcf2b796a110d1a68efed00ce684921fec9ff7b52ccdc68bb718d1b93444"
TEST_MANIFEST = tomllib.loads(validate_result.MANIFEST.read_text(encoding="utf-8"))
TEST_MANIFEST["protocol"]["warmup_iterations"] = 1
TEST_MANIFEST["fixtures"]["minimal-static"]["samples"] = 1
for tool in validate_result.POPPLER_TOOLS:
    TEST_MANIFEST["oracle"].update(
        {
            f"{tool}_path": f"/usr/bin/{tool}",
            f"{tool}_sha256": HASH,
            f"{tool}_version": f"{tool} 1.0",
        }
    )
TARGET = TEST_MANIFEST["targets"]["pliego-0.2.0"]
FIXTURE = TEST_MANIFEST["fixtures"]["minimal-static"]
INPUT_HASH, BUNDLE_HASH = validate_result.canonical_fixture_hashes(FIXTURE)


def pct(value: int | float) -> dict[str, int | float]:
    return {key: value for key in ("min", "p50", "p95", "p99", "max", "mean")}


def counters(
    *,
    user_usec: int = 0,
    memory_current: int = 0,
    memory_peak: int = 0,
    pids_peak: int = 0,
    populated: int = 0,
    final_io: bool = False,
) -> dict:
    io_stat = {"8:0": {"rbytes": 0, "wbytes": 1, "rios": 0, "wios": 1}} if final_io else {}
    return {
        "cpu_stat": {"usage_usec": user_usec, "user_usec": user_usec, "system_usec": 0},
        "io_stat": io_stat,
        "memory_current_bytes": memory_current,
        "memory_peak_bytes": memory_peak,
        "memory_file_dirty_bytes": 0,
        "memory_file_writeback_bytes": 0,
        "pids_peak": pids_peak,
        "cgroup_events": {"populated": populated, "frozen": 0},
    }


def resource_usage() -> dict:
    final = counters(user_usec=1000, memory_peak=4096, pids_peak=1, final_io=True)
    stable = {
        "cpu_stat": deepcopy(final["cpu_stat"]),
        "io_stat": deepcopy(final["io_stat"]),
        "memory_file_dirty_bytes": 0,
        "memory_file_writeback_bytes": 0,
    }
    return {
        "method": "linux-cgroup-v2-v1",
        "scope": "fresh-root-owned-cgroup-subtree",
        "delegated_parent": "/sys/fs/cgroup/pliego",
        "cgroup_path": "/sys/fs/cgroup/pliego/pliego-render-10-0000000000000000",
        "root_pid": 10,
        "root_start_ticks": 100,
        "launch_security": {
            "account": "pliego-benchmark-engine",
            "uid": 991,
            "gid": 991,
            "argv": [
                "/tmp/pliego",
                "render",
                "input.html",
                "--output",
                "/tmp/output.pdf",
                "--artifacts",
                "/tmp/artifacts",
            ],
            "executable": {
                "path": "/tmp/pliego",
                "sha256": TARGET["binary_sha256"],
                "device": 1,
                "inode": 2,
            },
            "status": {
                "uid": [991, 991, 991, 991],
                "gid": [991, 991, 991, 991],
                "supplementary_groups": [],
                "cap_inheritable_hex": "0000000000000000",
                "cap_permitted_hex": "0000000000000000",
                "cap_effective_hex": "0000000000000000",
                "cap_bounding_hex": "0000000000000000",
                "cap_ambient_hex": "0000000000000000",
                "no_new_privs": 1,
            },
            "migration_write_probes": {
                label: {"cgroup.procs": "EACCES", "cgroup.threads": "EACCES"}
                for label in ("parent", "harness", "staging", "measurement")
            },
        },
        "wall_ms": 1,
        "exit_code": 0,
        "signal": None,
        "cpu_user_ms": 1,
        "cpu_sys_ms": 0,
        "memory_current_bytes": 0,
        "memory_peak_bytes": 4096,
        "read_bytes": 0,
        "write_bytes": 1,
        "read_operations": 0,
        "write_operations": 1,
        "cgroup_drained": True,
        "drain_ms": 0,
        "accounting_settle": {
            "poll_interval_ms": 1,
            "timeout_ms": 10,
            "duration_ms": 1,
            "reads": 2,
            "stable_reads": 2,
            "stable_observations": [deepcopy(stable), deepcopy(stable)],
        },
        "cleanup": {"kill_used": False, "kill_grace_ms": 1000, "lingering_before_kill": []},
        "counters": {
            "empty": counters(),
            "after_join": counters(pids_peak=1, populated=1),
            "final": final,
        },
        "sampled_diagnostics": {
            "coverage": (
                "periodic post-exec cgroup membership snapshots; RSS and PSS peaks are lower bounds "
                "and may miss processes shorter than the interval"
            ),
            "sample_interval_ms": 1,
            "pss_interval_ms": 2,
            "page_size_bytes": 4096,
            "max_sample_gap_ms": 1,
            "sampled_peak_summed_rss_kib_lower_bound": 4,
            "sampled_peak_summed_pss_kib_lower_bound": 3,
            "observed_processes": [{"pid": 10, "start_ticks": 100, "first_seen_ms": 0, "last_seen_ms": 0}],
            "samples": [
                {
                    "elapsed_ms": 0,
                    "cgroup_memory_current_bytes": 4096,
                    "sampled_summed_rss_kib_lower_bound": 4,
                    "sampled_summed_pss_kib_lower_bound": 3,
                    "processes": [{"pid": 10, "start_ticks": 100, "rss_pages": 1, "pss_kib": 3}],
                },
                {
                    "elapsed_ms": 1,
                    "cgroup_memory_current_bytes": 0,
                    "sampled_summed_rss_kib_lower_bound": 0,
                    "sampled_summed_pss_kib_lower_bound": None,
                    "processes": [],
                },
            ],
        },
        "sampler_cpu_user_ms": 0,
        "sampler_cpu_sys_ms": 0,
        "sampler_cpu_percent_of_wall": 0,
    }


def result() -> dict:
    usage = resource_usage()
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
                "version": TARGET["version"],
                "binary_path": "/tmp/pliego",
                "binary_sha256": TARGET["binary_sha256"],
                "binary_bytes": TARGET["binary_bytes"],
                "commit": TARGET["commit"],
                "release_tag": TARGET["release_tag"],
                "servo_build": TARGET["servo_build"],
                "servo_base": TARGET["servo_base"],
                "bundle": TARGET["archive"],
                "bundle_sha256": TARGET["archive_sha256"],
                "bundle_bytes": TARGET["archive_bytes"],
                "profile": TARGET["profile"],
            },
            "python_version": "3.11",
            "php_version": "8.3",
            "harness_revision": "4" * 40,
            "competitors": {
                "oracle.contract": "pliego.pdf-oracle.v1",
                "oracle.oracle_path": "/workspace/benchmarks/tools/pdf_oracle.py",
                "oracle.oracle_sha256": validate_result.file_sha256(validate_result.PDF_ORACLE),
                **{
                    f"oracle.{tool}_{field}": value
                    for tool in ("pdfinfo", "pdftotext", "pdffonts", "pdftoppm")
                    for field, value in (
                        ("path", f"/usr/bin/{tool}"),
                        ("sha256", HASH),
                        ("version", f"{tool} 1.0"),
                    )
                },
            },
        },
        "protocol": {
            "warmup_iterations": 1,
            "correctness_preflight_iterations": 1,
            "execution_order": "untimed-correctness-preflight,warmup,timed",
            "sample_count": 1,
            "sample_order": "random",
            "seed": 1,
            "network": "disabled",
            "binary_profile": "checked-release",
            "measurement_method": "linux-cgroup-v2-v1",
            "percentile_method": "nearest-rank-v1",
        },
        "target": {"id": "pliego-0.2.0", "label": TARGET["label"]},
        "fixture": {
            "id": "minimal-static",
            "purpose": FIXTURE["purpose"],
            "category": "static",
            "input": "benchmarks/fixtures/minimal-static/input.html",
            "input_sha256": INPUT_HASH,
            "bundle_sha256": BUNDLE_HASH,
            "expected_page_count": 1,
            "expected_page_width_points": 595.276,
            "expected_page_height_points": 841.89,
            "dimension_tolerance_points": 0.75,
            "expected_text_contains": ["Minimal"],
            "expected_text": TEXT,
            "expected_font_families": ["Ahem"],
            "expected_normalized_raster_sha256": RASTER_HASH,
            "expected_link_targets": ["https://pliego.dev/docs"],
            "expected_failure_code": None,
        },
        "samples": [
            {
                "index": 0,
                "ok": True,
                "exit_code": 0,
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
                "measurement_method": "linux-cgroup-v2-v1",
                "signal": None,
                "resource_usage": usage,
                "phase_timings_ms": {"capture": 1},
                "output": {
                    "pdf_bytes": 1,
                    "pdf_sha256": HASH,
                    "page_count": 1,
                    "page_dimensions_points": [[595.276, 841.89]],
                    "normalized_text_sha256": validate_result.hashlib.sha256(TEXT.encode()).hexdigest(),
                    "font_families": ["Ahem"],
                    "normalized_raster_sha256": RASTER_HASH,
                    "artifact_bytes": 0,
                    "published_pdf": True,
                },
                "correctness": {
                    "pass": True,
                    "checks": [
                        {"name": "pdf_published", "status": "pass"},
                        {"name": "pdf_envelope", "status": "pass"},
                        {"name": "pdf_parse", "status": "pass"},
                        {"name": "page_count", "status": "pass"},
                        {"name": "page_dimensions", "status": "pass"},
                        {"name": "text:Minimal", "status": "pass"},
                        {"name": "text_exact", "status": "pass"},
                        {"name": "fonts_exact", "status": "pass"},
                        {"name": "raster_normalized", "status": "pass"},
                        {"name": "raster_parity", "status": "pass"},
                        {"name": "link:https://pliego.dev/docs", "status": "pass"},
                    ],
                },
                "failure": {"code": None, "published_pdf": True},
            }
        ],
        "aggregates": {
            "latency": pct(1),
            "cpu": {"user_ms": pct(1), "sys_ms": pct(0), "wall_ms": pct(1)},
            "memory": {
                "cgroup_peak_bytes": pct(4096),
                "sampled_peak_rss_kib_lower_bound": pct(4),
                "sampled_peak_pss_kib_lower_bound": pct(3),
            },
            "io": {
                "read_bytes": pct(0),
                "write_bytes": pct(1),
                "read_operations": pct(0),
                "write_operations": pct(1),
            },
            "scaling": {"per_page_wall_ms": 1, "per_page_memory_peak_bytes": 4096},
            "throughput": {
                "renders_per_minute": 30000,
                "concurrency": 1,
                "mean_one_shot_wall_ms": 2,
                "measurement_boundary": "runner-process-open-through-sampler-exit",
            },
            "output": {"pdf_bytes": pct(1), "page_count": 1},
            "correctness": {"pass_count": 1, "total": 1, "passed": True},
            "determinism": {"identical_pdf_sha256": 1, "total": 1, "pdf_sha256_variants": 1},
            "failures": {"count": 0, "codes": {}},
        },
    }


def errors(value: object) -> list[validate_result.Violation]:
    return validate_result.validate_document(value, SCHEMA, TEST_MANIFEST, "4" * 40)


def must_fail(value: object, expected: str) -> None:
    found = errors(value)
    assert found and expected in "\n".join(map(str, found)), found


def changed(value: dict, operation: object, expected: str) -> None:
    broken = deepcopy(value)
    operation(broken)  # type: ignore[operator]
    must_fail(broken, expected)


def main() -> None:
    valid = result()
    assert not errors(valid), errors(valid)
    assert validate_result.percentile([1, 3], 50) == 1
    assert validate_result.PERCENTILE_METHOD == "nearest-rank-v1"

    not_applicable = {
        key: deepcopy(valid[key])
        for key in ("schema", "version", "generated_at", "host", "toolchain", "target", "fixture")
    }
    na_target = TEST_MANIFEST["targets"]["dompdf-3.1.6"]
    na_fixture = TEST_MANIFEST["fixtures"]["chartjs-showcase"]
    na_correctness = na_fixture["correctness"]
    not_applicable["target"] = {"id": "dompdf-3.1.6", "label": na_target["label"]}
    not_applicable["toolchain"]["engine"] = {
        "name": "dompdf",
        "version": na_target["version"],
        "package": na_target["package"],
        "profile": na_target["profile"],
    }
    not_applicable["fixture"] = {
        "id": "chartjs-showcase",
        "purpose": na_fixture["purpose"],
        "category": na_fixture["category"],
        "input": na_fixture["input"],
        "expected_page_count": na_correctness.get("page_count"),
        "expected_page_width_points": na_correctness.get("page_width_points"),
        "expected_page_height_points": na_correctness.get("page_height_points"),
        "dimension_tolerance_points": na_correctness.get("dimension_tolerance_points"),
        "expected_text_contains": na_correctness.get("text_contains", []),
        "expected_text": na_correctness.get("text_equals"),
        "expected_font_families": na_correctness.get("font_families", []),
        "expected_normalized_raster_sha256": na_correctness.get("normalized_raster_sha256"),
        "expected_link_targets": na_correctness.get("link_targets", []),
        "expected_failure_code": na_correctness.get("failure_code"),
    }
    not_applicable.update(
        status="not-applicable",
        reason=na_target["not_applicable"]["chartjs-showcase"],
        protocol={
            "warmup_iterations": 0,
            "sample_count": 0,
            "sample_order": "random",
            "seed": 1,
            "network": "disabled",
            "binary_profile": na_target["profile"],
            "measurement_method": "unavailable",
            "percentile_method": "nearest-rank-v1",
        },
    )
    bundle = {
        "schema": "pliego.benchmark-bundle",
        "version": 1,
        "generated_at": valid["generated_at"],
        "results": [valid, not_applicable],
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
        "setup_ms": {"runtime_resolution": None, "runtime_install": None},
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

    changed(valid, lambda value: value["samples"][0].update(resource_usage=None), "resource_usage")
    changed(
        valid,
        lambda value: value["samples"][0].update(one_shot_wall_ms=0),
        "must cover engine wall, descendant drain, and accounting settle",
    )
    changed(valid, lambda value: value["samples"][0].update(user_ms=99), "samples[0].user_ms")
    changed(valid, lambda value: value["samples"][0].update(memory_peak_bytes=99), "memory_peak_bytes")
    changed(valid, lambda value: value["samples"][0].update(write_bytes=99), "write_bytes")
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"].update(
            argv=["/usr/bin/true", "render", "input.html", "--output", "/tmp/o", "--artifacts", "/tmp/a"]
        ),
        "launch_security.argv[0]",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["argv"].__setitem__(2, "other.html"),
        "launch_security.argv[2]",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["status"].update(
            cap_effective_hex="0000000000000001"
        ),
        "cap_effective_hex",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["status"].update(
            supplementary_groups=[10]
        ),
        "expected <= 0 items",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["migration_write_probes"][
            "parent"
        ].update({"cgroup.procs": "WRITABLE"}),
        "migration_write_probes.parent.cgroup.procs",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["executable"].update(sha256="1" * 64),
        "launch_security.executable.sha256",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"].update(
            cgroup_path="/sys/fs/cgroup/pliego/pliego-render-10-0000000000000000/nested"
        ),
        "fresh direct child",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["counters"]["empty"].update(memory_peak_bytes=1),
        "counters.empty.memory_peak_bytes",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["counters"]["final"]["cgroup_events"].update(populated=1),
        "counters.final.cgroup_events.populated",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["counters"]["final"].update(memory_file_dirty_bytes=1),
        "memory_file_dirty_bytes",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["counters"]["final"]["cpu_stat"].update(usage_usec=1),
        "cover user plus system CPU",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["accounting_settle"].update(duration_ms=0),
        "interval-separated",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["accounting_settle"]["stable_observations"][1][
            "cpu_stat"
        ].update(user_usec=2),
        "stable_observations",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["sampled_diagnostics"]["samples"][0]["processes"][0].update(
            start_ticks=101
        ),
        "root PID has a different start time",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["sampled_diagnostics"].update(
            sampled_peak_summed_rss_kib_lower_bound=99
        ),
        "sampled_peak_summed_rss_kib_lower_bound",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["sampled_diagnostics"]["samples"][-1].update(elapsed_ms=2),
        "engine wall plus drain duration",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["sampled_diagnostics"]["samples"][0].update(
            cgroup_memory_current_bytes=4097
        ),
        "must not exceed cgroup memory.peak",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["cleanup"].update(kill_used=True),
        "passing samples must drain without cgroup.kill",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["cleanup"].update(kill_grace_ms=0),
        "kill_grace_ms",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"].update(sampler_cpu_percent_of_wall=1),
        "sampler_cpu_percent_of_wall",
    )
    changed(valid, lambda value: value["protocol"].update(percentile_method="linear"), "percentile_method")
    changed(valid, lambda value: value.pop("status"), "missing required property 'status'")
    changed(valid, lambda value: value["host"].update(dedicated=False), "host.dedicated")
    changed(valid, lambda value: value["toolchain"].update(harness_revision="d" * 40), "harness_revision")
    changed(valid, lambda value: value["toolchain"].pop("competitors"), "missing required property 'competitors'")
    changed(
        valid,
        lambda value: value["toolchain"]["competitors"].pop("oracle.pdftoppm_sha256"),
        "oracle.pdftoppm_sha256",
    )
    changed(
        valid,
        lambda value: value["toolchain"]["competitors"].update(
            {
                "oracle.pdftoppm_path": "/definitely/missing/pdftoppm",
                "oracle.pdftoppm_sha256": "f" * 64,
                "oracle.pdftoppm_version": "forged",
            }
        ),
        "oracle.pdftoppm_path",
    )
    changed(
        valid,
        lambda value: value["toolchain"]["competitors"].update({"oracle.oracle_sha256": "f" * 64}),
        "oracle.oracle_sha256",
    )
    changed(valid, lambda value: value["target"].update(label="invented"), "target.label")
    changed(valid, lambda value: value["target"].update(id="invented"), "canonical manifest target")
    changed(
        valid,
        lambda value: value["target"].update(
            id="dompdf-3.1.6",
            label=TEST_MANIFEST["targets"]["dompdf-3.1.6"]["label"],
        ),
        "adapter target must be N/A",
    )
    changed(
        valid,
        lambda value: (
            value["target"].update(id="dompdf-3.1.6", label=TEST_MANIFEST["targets"]["dompdf-3.1.6"]["label"]),
            value["fixture"].update(id="chartjs-showcase"),
        ),
        "is not supported by the canonical adapter target",
    )
    changed(valid, lambda value: value["fixture"].update(purpose="invented"), "fixture.purpose")
    changed(valid, lambda value: value["fixture"].update(expected_text="short"), "fixture.expected_text")
    changed(
        valid,
        lambda value: value["samples"][0]["output"].update(normalized_raster_sha256="2" * 64),
        "normalized_raster_sha256",
    )
    changed(not_applicable, lambda value: value.update(reason="invented"), "reason")
    changed(
        valid,
        lambda value: value["aggregates"]["io"]["write_operations"].update(max=99),
        "aggregates.io.write_operations",
    )
    changed(
        valid,
        lambda value: value["aggregates"]["memory"]["cgroup_peak_bytes"].update(max=99),
        "aggregates.memory.cgroup_peak_bytes",
    )
    changed(
        valid,
        lambda value: value["aggregates"]["throughput"].update(renders_per_minute=60000),
        "drain-inclusive serial passing-sample throughput",
    )
    changed(
        valid,
        lambda value: value["aggregates"].pop("throughput"),
        "is required when correctness-passing timed samples exist",
    )
    changed(valid, lambda value: value["protocol"].update(sample_count=2), "protocol.sample_count")
    changed(
        valid,
        lambda value: value["protocol"].update(correctness_preflight_iterations=0),
        "correctness_preflight_iterations",
    )
    changed(
        valid,
        lambda value: value["protocol"].pop("execution_order"),
        "missing required property 'execution_order'",
    )
    changed(valid, lambda value: value["samples"][0]["correctness"].update({"pass": False}), "correctness.pass")
    changed(valid, lambda value: value["samples"][0]["failure"].update(published_pdf=False), "failure.published_pdf")
    changed(valid, lambda value: value["samples"][0]["output"].update(page_count=2), "expected page count 1")
    changed(
        valid,
        lambda value: value["samples"][0]["output"].update(page_dimensions_points=[[600, 842]]),
        "dimension tolerance",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["correctness"]["checks"].pop(),
        "shared PDF oracle check exactly once",
    )
    changed(
        timed, lambda value: value["samples"][0]["bridge_timings"].update(bridge_overhead_ms=99), "bridge_overhead_ms"
    )
    changed(
        bundle, lambda value: value["results"][0]["protocol"].update(sample_count=2), "results[0].protocol.sample_count"
    )

    print("benchmark result validator self-check passed")


if __name__ == "__main__":
    main()
