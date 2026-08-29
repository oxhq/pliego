#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
import json
from pathlib import Path
import tomllib
from unittest.mock import patch

import validate_result

SCHEMA = json.loads(
    (Path(__file__).resolve().parents[1] / "schema" / "benchmark-result.v1.json").read_text(encoding="utf-8")
)
HASH = "0" * 64
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
TARGET = TEST_MANIFEST["targets"]["pliego-0.3.3"]
FIXTURE = TEST_MANIFEST["fixtures"]["minimal-static"]
FIXTURE_CORRECTNESS = FIXTURE["correctness"]
TEXT = FIXTURE_CORRECTNESS["text_equals"]
RASTER_HASH = FIXTURE_CORRECTNESS["normalized_raster_sha256"]
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
                "render-api2",
            ],
            "executable": {
                "path": "/tmp/pliego",
                "sha256": TARGET["binary_sha256"],
                "device": 1,
                "inode": 2,
            },
            "launch_context": {
                "browser_tmpdir": None,
                "cwd": "/var/lib/pliego-benchmark-engine-temp/pliego-bench-api2-fixture/job",
                "tmpdir": "/var/lib/pliego-benchmark-engine-temp/pliego-bench-api2-fixture/temporary",
            },
            "output_capture": {
                "contract": "root-bound-tmpfs-engine-output-v1",
                "filesystem": "tmpfs",
                "max_bytes_per_stream": 16777216,
                "write_sync": "O_SYNC",
                "pre": {
                    "root": {
                        "path": "/dev/shm",
                        "identity": {"device": 2, "inode": 10},
                        "owner_uid": 0,
                        "owner_gid": 0,
                        "mode": 0o1777,
                    },
                    "streams": {
                        "stdout": {
                            "path": "/dev/shm/pliego-bench-out-fixture",
                            "identity": {"device": 2, "inode": 11},
                            "owner_uid": 0,
                            "owner_gid": 0,
                            "mode": 0o600,
                            "link_count": 1,
                            "size_bytes": 0,
                        },
                        "stderr": {
                            "path": "/dev/shm/pliego-bench-err-fixture",
                            "identity": {"device": 2, "inode": 12},
                            "owner_uid": 0,
                            "owner_gid": 0,
                            "mode": 0o600,
                            "link_count": 1,
                            "size_bytes": 0,
                        },
                    },
                },
                "post": {
                    "root": {
                        "path": "/dev/shm",
                        "identity": {"device": 2, "inode": 10},
                        "owner_uid": 0,
                        "owner_gid": 0,
                        "mode": 0o1777,
                    },
                    "streams": {
                        "stdout": {
                            "path": "/dev/shm/pliego-bench-out-fixture",
                            "identity": {"device": 2, "inode": 11},
                            "owner_uid": 0,
                            "owner_gid": 0,
                            "mode": 0o600,
                            "link_count": 1,
                            "size_bytes": 100,
                        },
                        "stderr": {
                            "path": "/dev/shm/pliego-bench-err-fixture",
                            "identity": {"device": 2, "inode": 12},
                            "owner_uid": 0,
                            "owner_gid": 0,
                            "mode": 0o600,
                            "link_count": 1,
                            "size_bytes": 10,
                        },
                    },
                },
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
            "temporary_storage": {
                "access_time": "FS_NOATIME_FL",
                "account_home": {
                    "path": "/nonexistent/pliego-benchmark-engine",
                    "state": "absent",
                },
                "browser_shared_memory": None,
                "directory_sync": "FS_DIRSYNC_FL",
                "file_sync": "FS_SYNC_FL",
                "filesystem": "ext4",
                "native_api2_path_bindings": {
                    phase: {
                        "contract": "native-api2-storage-bindings-v1",
                        "bindings": {
                            "controlled_root": {
                                "path": "/var/lib/pliego-benchmark-engine-temp",
                                "identity": {"device": 1, "inode": 19},
                            },
                            "sandbox": {
                                "path": "/var/lib/pliego-benchmark-engine-temp/pliego-bench-api2-fixture",
                                "identity": {"device": 1, "inode": 20},
                            },
                            "job": {
                                "path": "/var/lib/pliego-benchmark-engine-temp/pliego-bench-api2-fixture/job",
                                "identity": {"device": 1, "inode": 21},
                            },
                            "temporary": {
                                "path": "/var/lib/pliego-benchmark-engine-temp/pliego-bench-api2-fixture/temporary",
                                "identity": {"device": 1, "inode": 22},
                            },
                        },
                    }
                    for phase in ("pre", "post")
                },
                "runtime_environment": "fixed-account-home-v1",
                "runtime_path_bindings": None,
                "runtime_target": "generic-benchmark-engine-v1",
                "scope": "per-invocation-private",
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
        "target": {"id": "pliego-0.3.3", "label": TARGET["label"]},
        "fixture": {
            "id": "minimal-static",
            "purpose": FIXTURE["purpose"],
            "category": FIXTURE["category"],
            "input": FIXTURE["input"],
            "input_sha256": INPUT_HASH,
            "bundle_sha256": BUNDLE_HASH,
            "expected_page_count": FIXTURE_CORRECTNESS["page_count"],
            "expected_page_width_points": FIXTURE_CORRECTNESS["page_width_points"],
            "expected_page_height_points": FIXTURE_CORRECTNESS["page_height_points"],
            "dimension_tolerance_points": FIXTURE_CORRECTNESS["dimension_tolerance_points"],
            "expected_text_contains": FIXTURE_CORRECTNESS["text_contains"],
            "expected_text": TEXT,
            "expected_font_families": FIXTURE_CORRECTNESS["font_families"],
            "expected_normalized_raster_sha256": RASTER_HASH,
            "expected_link_targets": FIXTURE_CORRECTNESS.get("link_targets", []),
            "expected_failure_code": FIXTURE_CORRECTNESS.get("failure_code"),
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
                    "page_count": FIXTURE_CORRECTNESS["page_count"],
                    "page_dimensions_points": [
                        [
                            FIXTURE_CORRECTNESS["page_width_points"],
                            FIXTURE_CORRECTNESS["page_height_points"],
                        ]
                    ],
                    "normalized_text_sha256": validate_result.hashlib.sha256(TEXT.encode()).hexdigest(),
                    "font_families": FIXTURE_CORRECTNESS["font_families"],
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
                        {"name": f"text:{FIXTURE_CORRECTNESS['text_contains'][0]}", "status": "pass"},
                        {"name": "text_exact", "status": "pass"},
                        {"name": "fonts_exact", "status": "pass"},
                        {"name": "raster_normalized", "status": "pass"},
                        {"name": "raster_parity", "status": "pass"},
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
    browser_sample = deepcopy(valid["samples"][0])
    browser_launch = browser_sample["resource_usage"]["launch_security"]
    browser_path = "/repo/benchmarks/adapters/browsershot/adapter.php"
    browser_launch["argv"] = [browser_path, "render"]
    browser_launch["executable"]["path"] = browser_path
    browser_launch["temporary_storage"]["runtime_environment"] = "fresh-private-home-xdg-v1"
    browser_launch["temporary_storage"]["native_api2_path_bindings"] = None
    browser_launch["temporary_storage"]["runtime_target"] = "browsershot-adapter-v1"
    runtime_bindings = {
        "contract": "runtime-path-bindings-v1",
        "temporary_root": {
            "path": "/var/tmp/pliego/invocation",
            "identity": {"device": 1, "inode": 10},
        },
        "bindings": {
            variable: {
                "relative_path": relative,
                "identity": {"device": 1, "inode": index + 11},
            }
            for index, (variable, relative) in enumerate(validate_result.RUNTIME_DIRECTORY_NAMES.items())
        },
    }
    browser_launch["temporary_storage"]["runtime_path_bindings"] = {
        "pre": deepcopy(runtime_bindings),
        "post": deepcopy(runtime_bindings),
    }
    browser_shared_memory = {
        "root": deepcopy(browser_launch["output_capture"]["pre"]["root"]),
        "container": {
            "path": "/dev/shm/pliego-bench-shm-0123456789abcdef0123456789abcdef",
            "identity": {"device": 2, "inode": 13},
            "owner_uid": 0,
            "owner_gid": 0,
            "mode": 0o711,
            "link_count": 3,
        },
        "directory": {
            "path": "/dev/shm/pliego-bench-shm-0123456789abcdef0123456789abcdef/tmp",
            "identity": {"device": 2, "inode": 14},
            "owner_uid": 991,
            "owner_gid": 991,
            "mode": 0o700,
            "link_count": 2,
        },
        "container_entries": ["tmp"],
        "directory_entries": [],
    }
    browser_launch["temporary_storage"]["browser_shared_memory"] = {
        "contract": "bound-private-tmpfs-browser-shared-memory-v1",
        "filesystem": "tmpfs",
        "semantics": "puppeteer-node-chrome-temporary-storage-v1",
        "pre": deepcopy(browser_shared_memory),
        "post": deepcopy(browser_shared_memory),
    }
    browser_launch["launch_context"]["browser_tmpdir"] = browser_shared_memory["directory"]["path"]
    browser_schema_violations: list[validate_result.Violation] = []
    validate_result.validate(
        browser_launch["temporary_storage"],
        SCHEMA["definitions"]["launch_security"]["properties"]["temporary_storage"],
        "$.temporary_storage",
        browser_schema_violations,
        SCHEMA,
    )
    assert not browser_schema_violations, browser_schema_violations
    browser_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(browser_sample, "$.sample", browser_violations)
    assert not browser_violations, browser_violations
    false_native_browser = deepcopy(browser_sample)
    false_native_browser["resource_usage"]["launch_security"]["temporary_storage"]["native_api2_path_bindings"] = (
        deepcopy(
            valid["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"]["native_api2_path_bindings"]
        )
    )
    false_native_browser_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(false_native_browser, "$.sample", false_native_browser_violations)
    assert any("non-native targets" in str(item) for item in false_native_browser_violations)
    tampered_browser = deepcopy(browser_sample)
    tampered_browser["resource_usage"]["launch_security"]["temporary_storage"]["runtime_path_bindings"]["post"][
        "bindings"
    ]["HOME"]["identity"]["inode"] += 1
    tampered_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(tampered_browser, "$.sample", tampered_violations)
    assert any("runtime_path_bindings.post" in str(item) for item in tampered_violations)
    escaped_browser = deepcopy(browser_sample)
    escaped_browser["resource_usage"]["launch_security"]["temporary_storage"]["runtime_path_bindings"]["pre"][
        "bindings"
    ]["HOME"]["relative_path"] = "../home"
    escaped_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(escaped_browser, "$.sample", escaped_violations)
    assert any("HOME.relative_path" in str(item) for item in escaped_violations)
    missing_browser_tmpfs = deepcopy(browser_sample)
    missing_browser_tmpfs["resource_usage"]["launch_security"]["temporary_storage"]["browser_shared_memory"] = None
    missing_browser_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(missing_browser_tmpfs, "$.sample", missing_browser_violations)
    assert any("Browsershot must retain" in str(item) for item in missing_browser_violations)
    changed_browser_tmpfs = deepcopy(browser_sample)
    changed_browser_tmpfs["resource_usage"]["launch_security"]["temporary_storage"]["browser_shared_memory"]["post"][
        "directory"
    ]["identity"]["inode"] += 1
    changed_browser_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(changed_browser_tmpfs, "$.sample", changed_browser_violations)
    assert any("browser_shared_memory.post" in str(item) for item in changed_browser_violations)
    escaped_browser_tmpfs = deepcopy(browser_sample)
    escaped_browser_tmpfs["resource_usage"]["launch_security"]["temporary_storage"]["browser_shared_memory"]["pre"][
        "container"
    ]["path"] = "/dev/shm/../tmp/pliego-bench-shm-0123456789abcdef0123456789abcdef"
    escaped_tmpfs_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(escaped_browser_tmpfs, "$.sample", escaped_tmpfs_violations)
    assert any("canonical direct /dev/shm child" in str(item) for item in escaped_tmpfs_violations)
    short_nonce_browser_tmpfs = deepcopy(browser_sample)
    short_nonce_browser_tmpfs["resource_usage"]["launch_security"]["temporary_storage"]["browser_shared_memory"]["pre"][
        "container"
    ]["path"] = "/dev/shm/pliego-bench-shm-0123456789abcdef"
    short_nonce_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(short_nonce_browser_tmpfs, "$.sample", short_nonce_violations)
    assert any("32-character lowercase-hex nonce" in str(item) for item in short_nonce_violations)
    noncanonical_browser_tmpfs = deepcopy(browser_sample)
    noncanonical_browser_tmpfs["resource_usage"]["launch_security"]["temporary_storage"]["browser_shared_memory"][
        "pre"
    ]["directory"]["path"] = "/dev/shm/pliego-bench-shm-0123456789abcdef0123456789abcdef/./tmp"
    noncanonical_tmpfs_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(
        noncanonical_browser_tmpfs,
        "$.sample",
        noncanonical_tmpfs_violations,
    )
    assert any("pre.directory.path" in str(item) for item in noncanonical_tmpfs_violations)
    mismatched_browser_tmpdir = deepcopy(browser_sample)
    mismatched_browser_tmpdir["resource_usage"]["launch_security"]["launch_context"]["browser_tmpdir"] = (
        "/dev/shm/pliego-bench-shm-deadbeefdeadbeefdeadbeefdeadbeef/tmp"
    )
    mismatched_tmpdir_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(mismatched_browser_tmpdir, "$.sample", mismatched_tmpdir_violations)
    assert any("launch_context.browser_tmpdir" in str(item) for item in mismatched_tmpdir_violations)
    unsafe_browser_mode = deepcopy(browser_sample)
    unsafe_browser_mode["resource_usage"]["launch_security"]["temporary_storage"]["browser_shared_memory"]["pre"][
        "container"
    ]["mode"] = 0o777
    unsafe_mode_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(unsafe_browser_mode, "$.sample", unsafe_mode_violations)
    assert any("pre.container.mode" in str(item) for item in unsafe_mode_violations)
    generic_claims_browser_tmpfs = deepcopy(valid["samples"][0])
    generic_claims_browser_tmpfs["resource_usage"]["launch_security"]["temporary_storage"]["browser_shared_memory"] = (
        deepcopy(browser_launch["temporary_storage"]["browser_shared_memory"])
    )
    generic_tmpfs_violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(generic_claims_browser_tmpfs, "$.sample", generic_tmpfs_violations)
    assert any("non-browser targets" in str(item) for item in generic_tmpfs_violations)
    assert validate_result.percentile([1, 3], 50) == 1
    assert validate_result.PERCENTILE_METHOD == "nearest-rank-v1"

    with patch.object(validate_result.subprocess, "run", side_effect=OSError("git unavailable")):
        assert validate_result.clean_harness_revision() is None
    successful_status = validate_result.subprocess.CompletedProcess(
        args=["git", "status"],
        returncode=0,
        stdout="",
        stderr="",
    )
    with patch.object(
        validate_result.subprocess,
        "run",
        side_effect=[
            successful_status,
            validate_result.subprocess.TimeoutExpired(cmd="git", timeout=30),
        ],
    ):
        assert validate_result.clean_harness_revision() is None

    schema_violations: list[validate_result.Violation] = []
    validate_result.validate({}, {"type": "object", "minProperties": 1}, "$", schema_violations)
    assert schema_violations and "expected >= 1 properties" in str(schema_violations[0])
    schema_violations = []
    validate_result.validate(0, {"type": "number", "exclusiveMinimum": 0}, "$", schema_violations)
    assert schema_violations and "exclusive minimum 0" in str(schema_violations[0])
    changed(valid, lambda value: value["toolchain"]["competitors"].clear(), "expected >= 1 properties")
    changed(
        valid,
        lambda value: value["aggregates"]["throughput"].update(mean_one_shot_wall_ms=0),
        "exclusive minimum 0",
    )

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
            argv=["/usr/bin/true", "render-api2"]
        ),
        "launch_security.argv[0]",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["argv"].__setitem__(1, "render"),
        "launch_security.argv",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["argv"].append("unexpected"),
        "launch_security.argv",
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
        lambda value: value["samples"][0]["resource_usage"]["launch_security"].pop("output_capture"),
        "missing required property 'output_capture'",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"].update(
            filesystem="ext4"
        ),
        "launch_security.output_capture.filesystem",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["post"]["streams"][
            "stdout"
        ]["identity"].update(inode=99),
        "launch_security.output_capture.post",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["pre"]["root"].update(
            mode=0o777
        ),
        "output_capture.pre.root.mode",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["pre"]["streams"][
            "stdout"
        ].update(path="/tmp/pliego-bench-out-escaped"),
        "output_capture.pre.streams.stdout.path",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["pre"]["streams"][
            "stderr"
        ]["identity"].update(inode=11),
        "must bind distinct stdout and stderr",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["pre"]["streams"][
            "stdout"
        ].update(size_bytes=1),
        "output_capture.pre.streams.stdout.size_bytes",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["post"]["streams"][
            "stdout"
        ].update(size_bytes=16777217),
        "must not exceed max_bytes_per_stream",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["post"]["streams"][
            "stdout"
        ].update(link_count=2),
        "output_capture.post.streams.stdout",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["output_capture"]["pre"]["streams"][
            "stdout"
        ]["identity"].update(device=3),
        "output_capture.pre.streams.stdout.identity.device",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"].update(
            filesystem="tmpfs"
        ),
        "launch_security.temporary_storage",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"].update(
            file_sync="none"
        ),
        "launch_security.temporary_storage",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"].update(
            runtime_environment="host-home"
        ),
        "runtime_environment",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"].update(
            runtime_environment="fresh-private-home-xdg-v1"
        ),
        "runtime_environment",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"].update(
            access_time="relatime"
        ),
        "launch_security.temporary_storage",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"].update(
            native_api2_path_bindings=None
        ),
        "native API 2 must retain",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"][
            "native_api2_path_bindings"
        ]["post"]["bindings"]["job"]["identity"].update(inode=99),
        "native_api2_path_bindings.post",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["launch_context"].update(
            cwd="/var/lib/pliego-benchmark-engine-temp/other/job"
        ),
        "bindings.job.path",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"][
            "native_api2_path_bindings"
        ]["pre"]["bindings"]["job"].update(path="/var/lib/pliego-benchmark-engine-temp/wrong"),
        "job/temporary siblings",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"][
            "native_api2_path_bindings"
        ]["pre"]["bindings"]["job"]["identity"].update(inode=20),
        "four distinct directory identities",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["temporary_storage"][
            "native_api2_path_bindings"
        ]["pre"]["bindings"]["temporary"]["identity"].update(device=2),
        "on one device",
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
    changed(
        valid,
        lambda value: value["samples"][0]["output"].update(page_count=FIXTURE_CORRECTNESS["page_count"] + 1),
        f"expected page count {FIXTURE_CORRECTNESS['page_count']}",
    )
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
