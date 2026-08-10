#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
from dataclasses import replace
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import benchmark_publication
import validate_result

SCHEMA = json.loads(
    (Path(__file__).resolve().parents[1] / "schema" / "benchmark-result.v1.json").read_text(encoding="utf-8")
)
HASH = "0" * 64
ROOT = Path(__file__).resolve().parents[2]
CONTEXT: validate_result.ValidationContext | None = None


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
        "method": "linux-cgroup-v2-v2",
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
            "executable": {"path": "/tmp/pliego", "sha256": HASH, "device": 1, "inode": 2, "bytes": 1},
            "command_identity": {
                "cwd": {"path": "/tmp", "device": 1, "inode": 3},
                "input": {
                    "manifest_path": "/tmp/input.html",
                    "argv_relative_path": "input.html",
                    "sha256": HASH,
                    "device": 1,
                    "inode": 4,
                },
                "workspace_directory": {"path": "/tmp/workspace", "device": 1, "inode": 6},
                "output_directory": {"path": "/tmp", "device": 1, "inode": 3},
                "artifact_directory": {"path": "/tmp/artifacts", "device": 1, "inode": 5},
            },
            "execution_binding": "sealed-memfd-execveat",
            "parent_death_signal": 9,
            "network_namespace_isolated": True,
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
            "migration_fd_probes": {
                label: {"cgroup.procs": "EACCES", "cgroup.threads": "EACCES"}
                for label in ("parent", "harness", "staging", "measurement")
            },
        },
        "root_wall_ms": 1,
        "tree_wall_ms": 1,
        "measurement_complete_ms": 2,
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
        "drain_ms": 0,
        "accounting_settle": {
            "poll_interval_ms": 1,
            "timeout_ms": 10,
            "duration_ms": 1,
            "reads": 2,
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
                "sequential pidfd-identity and membership-verified procfs diagnostics; values are "
                "time-smeared observations, not simultaneous bounds or authoritative accounting"
            ),
            "observation_enabled": True,
            "sample_interval_ms": 1,
            "pss_interval_ms": 2,
            "page_size_bytes": 4096,
            "max_sample_gap_ms": 1,
            "sequential_sampled_peak_summed_rss_kib_diagnostic": 4,
            "sequential_sampled_peak_summed_pss_kib_diagnostic": 3,
            "observed_processes": [{"pid": 10, "start_ticks": 100, "first_seen_ms": 0, "last_seen_ms": 0}],
            "samples": [
                {
                    "elapsed_ms": 0,
                    "cgroup_memory_current_bytes": 4096,
                    "sequential_sampled_summed_rss_kib_diagnostic": 4,
                    "sequential_sampled_summed_pss_kib_diagnostic": 3,
                    "processes": [{"pid": 10, "start_ticks": 100, "rss_pages": 1, "pss_kib": 3}],
                },
                {
                    "elapsed_ms": 1,
                    "cgroup_memory_current_bytes": 0,
                    "sequential_sampled_summed_rss_kib_diagnostic": 0,
                    "sequential_sampled_summed_pss_kib_diagnostic": None,
                    "processes": [],
                },
            ],
        },
        "engine_stdout": "",
        "engine_stderr": "",
        "sampler_cpu_user_ms": 0,
        "sampler_cpu_sys_ms": 0,
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
            "dedicated": False,
        },
        "toolchain": {
            "engine": {
                "name": "pliego",
                "version": "0.1.1",
                "binary_path": "/tmp/pliego",
                "binary_sha256": HASH,
            },
            "python_version": "3.11",
            "php_version": "8.3",
            "harness_revision": "4f0beb41a4d00000000000000000000000000000",
        },
        "protocol": {
            "warmup_iterations": 1,
            "sample_count": 1,
            "sample_order": "sequential",
            "network": "disabled",
            "binary_profile": "checked-release",
            "measurement_method": "linux-cgroup-v2-v2",
            "percentile_method": "nearest-rank-v1",
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
                "memory_current_bytes": 0,
                "memory_peak_bytes": 4096,
                "sequential_sampled_peak_rss_kib_diagnostic": 4,
                "sequential_sampled_peak_pss_kib_diagnostic": 3,
                "read_bytes": 0,
                "write_bytes": 1,
                "read_operations": 0,
                "write_operations": 1,
                "measurement_method": "linux-cgroup-v2-v2",
                "signal": None,
                "resource_usage": usage,
                "phase_timings_ms": {"capture": 1},
                "output": {
                    "pdf_bytes": 1,
                    "pdf_sha256": HASH,
                    "page_count": 1,
                    "artifact_bytes": 0,
                    "published_pdf": True,
                },
                "correctness": {"pass": True, "checks": [{"name": "pdf_published", "status": "pass"}]},
                "failure": {"code": None, "published_pdf": True},
            }
        ],
        "aggregates": {
            "latency": pct(1),
            "cpu": {"user_ms": pct(1), "sys_ms": pct(0), "wall_ms": pct(1)},
            "memory": {
                "cgroup_peak_bytes": pct(4096),
                "sequential_sampled_peak_rss_kib_diagnostic": pct(4),
                "sequential_sampled_peak_pss_kib_diagnostic": pct(3),
            },
            "io": {
                "read_bytes": pct(0),
                "write_bytes": pct(1),
                "read_operations": pct(0),
                "write_operations": pct(1),
            },
            "scaling": {"per_page_wall_ms": 1, "per_page_memory_peak_bytes": 4096},
            "throughput": {"renders_per_minute": 60000, "concurrency": 1},
            "output": {"pdf_bytes": pct(1), "page_count": 1},
            "correctness": {"pass_count": 1, "total": 1, "passed": True},
            "determinism": {"identical_pdf_sha256": 1, "total": 1, "pdf_sha256_variants": 1},
            "failures": {"count": 0, "codes": {}},
        },
    }


def errors(value: object) -> list[validate_result.Violation]:
    assert CONTEXT is not None
    return validate_result.validate_document(value, SCHEMA, CONTEXT)


def must_fail(value: object, expected: str) -> None:
    found = errors(value)
    assert found and expected in "\n".join(map(str, found)), found


def changed(value: dict, operation: object, expected: str) -> None:
    broken = deepcopy(value)
    operation(broken)  # type: ignore[operator]
    must_fail(broken, expected)


def bind_real_context(value: dict, raw_workspace: str) -> validate_result.ValidationContext:
    global CONTEXT
    workspace = Path(raw_workspace)
    frozen_root = workspace / "frozen"
    failure_root = workspace / "failures"
    staging = workspace / "candidate.json"
    binary = workspace / "pliego"
    source_directory = ROOT / "benchmarks" / "fixtures" / "minimal-static"
    frozen_directory = frozen_root / "benchmarks" / "fixtures" / "minimal-static"
    frozen_directory.mkdir(parents=True)
    for name in ("input.html", "Ahem.ttf"):
        shutil.copyfile(source_directory / name, frozen_directory / name)
    failure_root.mkdir(mode=0o700)
    if os.name == "posix":
        os.chmod(failure_root, 0o700)
    binary.write_bytes(b"test engine\n")
    revision = value["toolchain"]["harness_revision"]
    CONTEXT = validate_result.build_validation_context(
        mode="staged",
        repository_root=ROOT,
        manifest_path=ROOT / "benchmarks" / "manifest.toml",
        frozen_fixture_root=frozen_root.resolve(),
        staging_output=staging.resolve(),
        failure_evidence_root=failure_root.resolve(),
        engine_binary=binary.resolve(),
        harness_revision=revision,
    )
    contract = CONTEXT.fixtures["minimal-static"]
    frozen_input = frozen_directory / "input.html"
    frozen_font = frozen_directory / "Ahem.ttf"
    input_digest, input_metadata = validate_result._bound_regular_file(frozen_input.resolve())
    font_digest, _ = validate_result._bound_regular_file(frozen_font.resolve())
    bundle_digest = validate_result._bundle_digest([("input.html", input_digest), ("Ahem.ttf", font_digest)])
    engine_digest, engine_metadata = validate_result._bound_regular_file(binary.resolve())
    cwd_metadata = frozen_directory.stat()
    sample_root = failure_root / ".in-progress-123-0000000000000000"
    output_directory = sample_root / "sample-0-0000000000000000-output"
    artifact_directory = sample_root / "sample-0-0000000000000000-artifacts"
    launch = value["samples"][0]["resource_usage"]["launch_security"]
    value["fixture"].update(
        id="minimal-static",
        purpose=contract["purpose"],
        category=contract["category"],
        input=contract["input"],
        input_sha256=input_digest,
        bundle_sha256=bundle_digest,
    )
    value["toolchain"]["engine"].update(binary_path=str(binary.resolve()), binary_sha256=engine_digest)
    launch["argv"] = [
        str(binary.resolve()),
        "render",
        frozen_input.name,
        "--output",
        str(output_directory / "document.pdf"),
        "--artifacts",
        str(artifact_directory),
    ]
    launch["executable"] = {
        "path": str(binary.resolve()),
        "sha256": engine_digest,
        "device": engine_metadata.st_dev,
        "inode": engine_metadata.st_ino,
        "bytes": engine_metadata.st_size,
    }
    launch["command_identity"] = {
        "cwd": {"path": str(frozen_directory.resolve()), "device": cwd_metadata.st_dev, "inode": cwd_metadata.st_ino},
        "input": {
            "manifest_path": str(frozen_input.resolve()),
            "argv_relative_path": frozen_input.name,
            "sha256": input_digest,
            "device": input_metadata.st_dev,
            "inode": input_metadata.st_ino,
        },
        "workspace_directory": {"path": str(sample_root), "device": 7, "inode": 6},
        "output_directory": {"path": str(output_directory), "device": 7, "inode": 8},
        "artifact_directory": {"path": str(artifact_directory), "device": 7, "inode": 9},
    }
    return CONTEXT


def main() -> None:
    valid = result()
    workspace = tempfile.TemporaryDirectory()
    bind_real_context(valid, workspace.name)
    assert not errors(valid), errors(valid)
    for non_finite in (math.nan, math.inf, -math.inf):
        broken = deepcopy(valid)
        broken["samples"][0]["wall_ms"] = non_finite
        assert any("non-finite" in str(violation) for violation in errors(broken))
        try:
            benchmark_publication.json_bytes(broken)
        except benchmark_publication.PublicationError as error:
            assert "non-finite" in str(error)
        else:
            raise AssertionError("non-finite result serialization was accepted")
    for literal in ("NaN", "Infinity", "-Infinity"):
        try:
            benchmark_publication.strict_json_loads(f'{{"value":{literal}}}', "result adversary")
        except benchmark_publication.PublicationError as error:
            assert "non-finite" in str(error)
        else:
            raise AssertionError(f"literal {literal} result JSON was accepted")
    assert validate_result.percentile([1, 3], 50) == 1
    assert validate_result.PERCENTILE_METHOD == "nearest-rank-v1"

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

    changed(valid, lambda value: value["host"].update(dedicated=True), "staged validation forbids")
    changed(valid, lambda value: value["samples"][0].update(resource_usage=None), "resource_usage")
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
        lambda value: value["samples"][0]["resource_usage"].update(measurement_complete_ms=1.5),
        "full post-drain accounting settle interval",
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
            sequential_sampled_peak_summed_rss_kib_diagnostic=99
        ),
        "sequential_sampled_peak_summed_rss_kib_diagnostic",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["sampled_diagnostics"]["samples"][-1].update(elapsed_ms=2),
        "tree wall",
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
        lambda value: value["samples"][0]["resource_usage"].update(tree_wall_ms=0.5),
        "must include root lifetime",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["migration_fd_probes"][
            "measurement"
        ].update({"cgroup.procs": "WRITABLE"}),
        "migration_fd_probes.measurement.cgroup.procs",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"].update(parent_death_signal=0),
        "parent_death_signal",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["command_identity"]["input"].update(
            sha256="1" * 64
        ),
        "command_identity.input.sha256",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["command_identity"]["input"].update(
            manifest_path=str((Path(tempfile.gettempdir()) / "attacker-input.html").resolve())
        ),
        "command_identity.input.manifest_path",
    )
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["command_identity"]["cwd"].update(
            path=str(Path(tempfile.gettempdir()).resolve())
        ),
        "command_identity.cwd.path",
    )

    def escape_outputs(value: dict) -> None:
        launch = value["samples"][0]["resource_usage"]["launch_security"]
        outside = Path(tempfile.gettempdir()).resolve() / "attacker-output"
        launch["argv"][launch["argv"].index("--output") + 1] = str(outside / "document.pdf")
        launch["argv"][launch["argv"].index("--artifacts") + 1] = str(outside.parent / "attacker-artifacts")
        launch["command_identity"]["workspace_directory"]["path"] = str(outside.parent)
        launch["command_identity"]["output_directory"]["path"] = str(outside)
        launch["command_identity"]["artifact_directory"]["path"] = str(outside.parent / "attacker-artifacts")

    changed(valid, escape_outputs, "exact failure-evidence root")
    changed(
        valid,
        lambda value: value["samples"][0]["resource_usage"]["launch_security"]["command_identity"][
            "artifact_directory"
        ].update(
            device=value["samples"][0]["resource_usage"]["launch_security"]["command_identity"]["output_directory"][
                "device"
            ],
            inode=value["samples"][0]["resource_usage"]["launch_security"]["command_identity"]["output_directory"][
                "inode"
            ],
        ),
        "FD identities must be disjoint",
    )
    changed(valid, lambda value: value["fixture"].update(bundle_sha256="1" * 64), "fixture.bundle_sha256")
    changed(valid, lambda value: value["fixture"].update(input="attacker/input.html"), "fixture.input")
    changed(valid, lambda value: value["protocol"].update(percentile_method="linear"), "percentile_method")
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
    changed(valid, lambda value: value["protocol"].update(sample_count=2), "protocol.sample_count")
    changed(valid, lambda value: value["samples"][0]["correctness"].update({"pass": False}), "correctness.pass")
    changed(valid, lambda value: value["samples"][0]["failure"].update(published_pdf=False), "failure.published_pdf")
    changed(valid, lambda value: value["samples"][0]["output"].update(page_count=2), "expected page count 1")
    changed(
        timed, lambda value: value["samples"][0]["bridge_timings"].update(bridge_overhead_ms=99), "bridge_overhead_ms"
    )
    changed(
        bundle, lambda value: value["results"][0]["protocol"].update(sample_count=2), "results[0].protocol.sample_count"
    )

    assert CONTEXT is not None
    external = validate_result.ExternalPublication(
        host_proof_schema="pliego.benchmark-host-proof.v1",
        host_proof_bundle_sha256="b" * 64,
        attestation_schema="pliego.benchmark-publication-attestation.v1",
        attestation_sha256="c" * 64,
        observer_proof_sha256="d" * 64,
        authority="github-protected-environment-hmac-v1",
        harness_revision=CONTEXT.harness_revision,
        github_run_id=123,
        github_run_attempt=1,
        github_job="dedicated",
        github_workflow_ref=benchmark_publication.TRUSTED_WORKFLOW_REF,
        runner_id=7,
        runner_name="pliego-pinned-01",
    )
    published_context = validate_result.build_validation_context(
        mode="published",
        repository_root=CONTEXT.repository_root,
        manifest_path=CONTEXT.manifest_path,
        frozen_fixture_root=CONTEXT.frozen_fixture_root,
        staging_output=CONTEXT.staging_output,
        failure_evidence_root=CONTEXT.failure_evidence_root,
        engine_binary=CONTEXT.engine_binary,
        harness_revision=CONTEXT.harness_revision,
        external_publication=external,
    )
    try:
        validate_result.build_validation_context(
            mode="published",
            repository_root=CONTEXT.repository_root,
            manifest_path=CONTEXT.manifest_path,
            frozen_fixture_root=CONTEXT.frozen_fixture_root,
            staging_output=CONTEXT.staging_output,
            failure_evidence_root=CONTEXT.failure_evidence_root,
            engine_binary=CONTEXT.engine_binary,
            harness_revision=CONTEXT.harness_revision,
            external_publication=replace(external, authority="self-rehashed-live-bundle"),
        )
    except ValueError as error:
        assert "protected GitHub attestation" in str(error)
    else:
        raise AssertionError("self-asserted external publication authority was accepted")
    published = deepcopy(valid)
    published["host"]["dedicated"] = True
    published["publication"] = external.document()
    assert not validate_result.validate_document(published, SCHEMA, published_context)
    invented = deepcopy(published)
    invented["publication"]["runner_name"] = "invented-runner"
    must_be_external = validate_result.validate_document(invented, SCHEMA, published_context)
    assert must_be_external and "publication" in str(must_be_external[0])
    no_external_context = validate_result.validate_document(published, SCHEMA)
    assert no_external_context and "typed staged or external-publication context" in str(no_external_context[0])
    staged_path = Path(workspace.name) / "staged-result.json"
    staged_path.write_text(json.dumps(valid), encoding="utf-8")
    staged_cli = subprocess.run(
        [
            sys.executable,
            str(Path(validate_result.__file__).resolve()),
            str(staged_path),
            "--mode",
            "staged",
            "--binary",
            str(CONTEXT.engine_binary),
            "--frozen-fixture-root",
            str(CONTEXT.frozen_fixture_root),
            "--staging-out",
            str(CONTEXT.staging_output),
            "--failure-evidence-dir",
            str(CONTEXT.failure_evidence_root),
            "--revision",
            CONTEXT.harness_revision,
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert staged_cli.returncode == 0, staged_cli.stderr
    invented_path = Path(workspace.name) / "invented-publication.json"
    invented_path.write_text(json.dumps(invented), encoding="utf-8")
    invented_cli = subprocess.run(
        [sys.executable, str(Path(validate_result.__file__).resolve()), str(invented_path), "--mode", "published"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert invented_cli.returncode == 2
    assert "host proof plus protected attestation" in invented_cli.stderr

    frozen_input = CONTEXT.frozen_fixture_root / "benchmarks" / "fixtures" / "minimal-static" / "input.html"
    retained_identity = frozen_input.with_suffix(".retained-identity")
    os.link(frozen_input, retained_identity)
    replacement = frozen_input.with_suffix(".replacement")
    replacement.write_bytes(frozen_input.read_bytes())
    os.replace(replacement, frozen_input)
    replaced_identity = validate_result.validate_document(valid, SCHEMA, CONTEXT)
    assert replaced_identity and "command_identity.input.inode" in "\n".join(map(str, replaced_identity))
    retained_identity.unlink()

    workspace.cleanup()

    print("benchmark result validator self-check passed")


if __name__ == "__main__":
    main()
