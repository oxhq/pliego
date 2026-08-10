#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Dependency-free JSON Schema validation for benchmark results.

Implements the subset of draft-07 used by `schema/benchmark-result.v1.json`
(no `jsonschema` required): type (including null unions), const, enum, oneOf,
required, properties, additionalProperties, minimum, minLength, minItems,
maxItems, $ref
resolution into `definitions`, and pattern.

Usage:
    python3 benchmarks/tools/validate_result.py <result.json> [<schema.json>]
"""

from __future__ import annotations

import json
import math
import re
import statistics
import sys
from collections import Counter
from pathlib import Path, PurePosixPath
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"
PERCENTILE_METHOD = "nearest-rank-v1"


class Violation:
    def __init__(self, path: str, message: str) -> None:
        self.path = path
        self.message = message

    def __str__(self) -> str:
        return f"{self.path}: {self.message}"


def percentile(values: list[int | float], p: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, math.ceil(p / 100.0 * len(ordered)) - 1))
    return float(ordered[index])


def percentiles(values: list[int | float | None]) -> dict[str, float]:
    measured = [value for value in values if value is not None]
    if not measured:
        return {key: 0.0 for key in ("min", "p50", "p95", "p99", "max", "mean")}
    return {
        "min": min(measured),
        "p50": percentile(measured, 50),
        "p95": percentile(measured, 95),
        "p99": percentile(measured, 99),
        "max": max(measured),
        "mean": statistics.fmean(measured),
    }


def resolve(schema: dict[str, Any], ref: str) -> dict[str, Any]:
    if not ref.startswith("#/definitions/"):
        raise ValueError(f"unsupported $ref: {ref}")
    definitions = schema.get("definitions", {})
    name = ref[len("#/definitions/") :]
    if name not in definitions:
        raise ValueError(f"unknown definition in $ref: {ref}")
    return definitions[name]


def type_matches(value: Any, expected: str) -> bool:
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if expected == "string":
        return isinstance(value, str)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "null":
        return value is None
    return True


def validate(
    data: Any,
    schema: dict[str, Any],
    path: str,
    violations: list[Violation],
    root: dict[str, Any] | None = None,
) -> None:
    if root is None:
        root = schema
    if "$ref" in schema:
        validate(data, resolve(root, schema["$ref"]), path, violations, root)
        return

    if "oneOf" in schema:
        branch_violations: list[list[Violation]] = []
        for branch in schema["oneOf"]:
            failures: list[Violation] = []
            validate(data, branch, path, failures, root)
            branch_violations.append(failures)
        matches = sum(not failures for failures in branch_violations)
        if matches != 1:
            closest = min(
                branch_violations,
                key=lambda failures: (len(failures), -max((len(failure.path) for failure in failures), default=0)),
            )
            detail = f"; closest: {closest[0]}" if closest else ""
            violations.append(Violation(path, f"expected exactly one oneOf match, got {matches}{detail}"))
        return

    if "type" in schema:
        types = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(type_matches(data, t) for t in types):
            violations.append(Violation(path, f"expected type {types!r}, got {type(data).__name__}"))
            return

    if "const" in schema and data != schema["const"]:
        violations.append(Violation(path, f"expected const {schema['const']!r}, got {data!r}"))

    if "enum" in schema and data not in schema["enum"]:
        violations.append(Violation(path, f"expected one of {schema['enum']!r}, got {data!r}"))

    if "pattern" in schema and isinstance(data, str):
        if re.fullmatch(schema["pattern"], data) is None:
            violations.append(Violation(path, f"does not match pattern {schema['pattern']!r}"))
    if isinstance(data, str) and "minLength" in schema and len(data) < schema["minLength"]:
        violations.append(Violation(path, f"expected >= {schema['minLength']} characters, got {len(data)}"))

    if isinstance(data, (int, float)) and not isinstance(data, bool):
        if "minimum" in schema and data < schema["minimum"]:
            violations.append(Violation(path, f"{data} < minimum {schema['minimum']}"))

    if isinstance(data, list):
        if "minItems" in schema and len(data) < schema["minItems"]:
            violations.append(Violation(path, f"expected >= {schema['minItems']} items, got {len(data)}"))
        if "maxItems" in schema and len(data) > schema["maxItems"]:
            violations.append(Violation(path, f"expected <= {schema['maxItems']} items, got {len(data)}"))
        if "items" in schema and isinstance(schema["items"], dict):
            for index, item in enumerate(data):
                validate(item, schema["items"], f"{path}[{index}]", violations, root)

    if isinstance(data, dict):
        properties = schema.get("properties", {})
        for required in schema.get("required", []):
            if required not in data:
                violations.append(Violation(path, f"missing required property {required!r}"))
        for key, value in data.items():
            child = f"{path}.{key}"
            if key in properties:
                validate(value, properties[key], child, violations, root)
            elif schema.get("additionalProperties") is False:
                violations.append(Violation(path, f"unexpected property {key!r}"))
            elif isinstance(schema.get("additionalProperties"), dict):
                validate(value, schema["additionalProperties"], child, violations, root)


def require_equal(path: str, actual: Any, expected: Any, violations: list[Violation]) -> None:
    if actual != expected:
        violations.append(Violation(path, f"must equal {expected!r}, got {actual!r}"))


def io_total(io_stat: dict[str, dict[str, int]], field: str) -> int:
    return sum(device.get(field, 0) for device in io_stat.values())


def validate_zero_counters(
    counters: dict[str, Any],
    path: str,
    populated: int,
    pids_peak: int,
    violations: list[Violation],
) -> None:
    for field in ("usage_usec", "user_usec", "system_usec"):
        require_equal(f"{path}.cpu_stat.{field}", counters["cpu_stat"][field], 0, violations)
    for device, values in counters["io_stat"].items():
        for field, value in values.items():
            require_equal(f"{path}.io_stat.{device}.{field}", value, 0, violations)
    for field in (
        "memory_current_bytes",
        "memory_peak_bytes",
        "memory_file_dirty_bytes",
        "memory_file_writeback_bytes",
    ):
        require_equal(f"{path}.{field}", counters[field], 0, violations)
    require_equal(f"{path}.pids_peak", counters["pids_peak"], pids_peak, violations)
    require_equal(f"{path}.cgroup_events.populated", counters["cgroup_events"]["populated"], populated, violations)
    require_equal(f"{path}.cgroup_events.frozen", counters["cgroup_events"]["frozen"], 0, violations)


def validate_resource_usage(sample: dict[str, Any], path: str, violations: list[Violation]) -> None:
    usage = sample["resource_usage"]
    if not isinstance(usage, dict):
        violations.append(Violation(f"{path}.resource_usage", "cgroup-v2 samples require retained resource usage"))
        return

    exact_fields = {
        "exit_code": "exit_code",
        "signal": "signal",
        "wall_ms": "wall_ms",
        "user_ms": "cpu_user_ms",
        "sys_ms": "cpu_sys_ms",
        "memory_current_bytes": "memory_current_bytes",
        "memory_peak_bytes": "memory_peak_bytes",
        "read_bytes": "read_bytes",
        "write_bytes": "write_bytes",
        "read_operations": "read_operations",
        "write_operations": "write_operations",
        "measurement_method": "method",
    }
    for sample_field, usage_field in exact_fields.items():
        require_equal(f"{path}.{sample_field}", sample[sample_field], usage[usage_field], violations)

    parent = PurePosixPath(usage["delegated_parent"])
    cgroup = PurePosixPath(usage["cgroup_path"])
    canonical_parent = parent.is_absolute() and ".." not in parent.parts and str(parent) == usage["delegated_parent"]
    leaf_name = cgroup.name
    if not canonical_parent:
        violations.append(Violation(f"{path}.resource_usage.delegated_parent", "must be an absolute canonical path"))
    if (
        cgroup.parent != parent
        or re.fullmatch(r"pliego-render-[1-9][0-9]*-[0-9a-f]{16}", leaf_name) is None
        or str(cgroup) != usage["cgroup_path"]
    ):
        violations.append(Violation(f"{path}.resource_usage.cgroup_path", "must be the named fresh direct child"))
    if usage["root_start_ticks"] <= 0:
        violations.append(Violation(f"{path}.resource_usage.root_start_ticks", "must bind a positive Linux start time"))
    if usage["signal"] is not None:
        require_equal(f"{path}.resource_usage.exit_code", usage["exit_code"], 128 + usage["signal"], violations)

    launch = usage["launch_security"]
    status = launch["status"]
    require_equal(f"{path}.resource_usage.launch_security.status.uid", status["uid"], [launch["uid"]] * 4, violations)
    require_equal(f"{path}.resource_usage.launch_security.status.gid", status["gid"], [launch["gid"]] * 4, violations)
    require_equal(
        f"{path}.resource_usage.launch_security.status.supplementary_groups",
        status["supplementary_groups"],
        [],
        violations,
    )
    for field in (
        "cap_inheritable_hex",
        "cap_permitted_hex",
        "cap_effective_hex",
        "cap_bounding_hex",
        "cap_ambient_hex",
    ):
        if int(status[field], 16) != 0:
            violations.append(Violation(f"{path}.resource_usage.launch_security.status.{field}", "must be zero"))
    require_equal(
        f"{path}.resource_usage.launch_security.status.no_new_privs",
        status["no_new_privs"],
        1,
        violations,
    )
    for label, results in launch["migration_write_probes"].items():
        for interface, result in results.items():
            if result not in {"EACCES", "EPERM"}:
                violations.append(
                    Violation(
                        f"{path}.resource_usage.launch_security.migration_write_probes.{label}.{interface}",
                        "must prove a permission-denied write",
                    )
                )
    executable = launch["executable"]
    executable_path = PurePosixPath(executable["path"])
    if not executable_path.is_absolute() or ".." in executable_path.parts or str(executable_path) != executable["path"]:
        violations.append(
            Violation(f"{path}.resource_usage.launch_security.executable.path", "must be an absolute canonical path")
        )
    require_equal(f"{path}.resource_usage.launch_security.argv[0]", launch["argv"][0], executable["path"], violations)
    require_equal(f"{path}.resource_usage.launch_security.argv[1]", launch["argv"][1], "render", violations)

    counters = usage["counters"]
    empty = counters["empty"]
    after_join = counters["after_join"]
    final = counters["final"]
    validate_zero_counters(empty, f"{path}.resource_usage.counters.empty", 0, 0, violations)
    validate_zero_counters(after_join, f"{path}.resource_usage.counters.after_join", 1, 1, violations)
    require_equal(
        f"{path}.resource_usage.counters.final.cgroup_events.populated",
        final["cgroup_events"]["populated"],
        0,
        violations,
    )
    require_equal(
        f"{path}.resource_usage.counters.final.cgroup_events.frozen",
        final["cgroup_events"]["frozen"],
        0,
        violations,
    )
    for field in ("memory_file_dirty_bytes", "memory_file_writeback_bytes"):
        require_equal(f"{path}.resource_usage.counters.final.{field}", final[field], 0, violations)
    if final["memory_peak_bytes"] < final["memory_current_bytes"]:
        violations.append(
            Violation(
                f"{path}.resource_usage.counters.final.memory_peak_bytes",
                "must be at least final memory_current_bytes",
            )
        )
    if final["pids_peak"] < 1:
        violations.append(Violation(f"{path}.resource_usage.counters.final.pids_peak", "must observe the root process"))

    cpu = final["cpu_stat"]
    if cpu["usage_usec"] + 2 < cpu["user_usec"] + cpu["system_usec"]:
        violations.append(
            Violation(f"{path}.resource_usage.counters.final.cpu_stat.usage_usec", "must cover user plus system CPU")
        )
    derived = {
        "cpu_user_ms": round(cpu["user_usec"] / 1000.0, 3),
        "cpu_sys_ms": round(cpu["system_usec"] / 1000.0, 3),
        "memory_current_bytes": final["memory_current_bytes"],
        "memory_peak_bytes": final["memory_peak_bytes"],
        "read_bytes": io_total(final["io_stat"], "rbytes"),
        "write_bytes": io_total(final["io_stat"], "wbytes"),
        "read_operations": io_total(final["io_stat"], "rios"),
        "write_operations": io_total(final["io_stat"], "wios"),
    }
    for field, expected in derived.items():
        require_equal(f"{path}.resource_usage.{field}", usage[field], expected, violations)

    settle = usage["accounting_settle"]
    observations = settle["stable_observations"]
    if len(observations) != 2:
        violations.append(
            Violation(
                f"{path}.resource_usage.accounting_settle.stable_observations", "must retain exactly two stable reads"
            )
        )
    elif observations[0] != observations[1]:
        violations.append(
            Violation(
                f"{path}.resource_usage.accounting_settle.stable_observations",
                "cpu.stat and io.stat reads must be identical",
            )
        )
    elif observations[1]["cpu_stat"] != final["cpu_stat"] or observations[1]["io_stat"] != final["io_stat"]:
        violations.append(
            Violation(
                f"{path}.resource_usage.accounting_settle.stable_observations",
                "last stable read must equal final counters",
            )
        )
    if settle["duration_ms"] > settle["timeout_ms"] + settle["poll_interval_ms"]:
        violations.append(
            Violation(f"{path}.resource_usage.accounting_settle.duration_ms", "exceeds bounded settle budget")
        )
    if settle["duration_ms"] < settle["poll_interval_ms"]:
        violations.append(
            Violation(
                f"{path}.resource_usage.accounting_settle.duration_ms", "two stable reads were not interval-separated"
            )
        )

    diagnostics = usage["sampled_diagnostics"]
    require_equal(
        f"{path}.resource_usage.accounting_settle.poll_interval_ms",
        settle["poll_interval_ms"],
        diagnostics["sample_interval_ms"],
        violations,
    )
    if diagnostics["pss_interval_ms"] < diagnostics["sample_interval_ms"]:
        violations.append(
            Violation(f"{path}.resource_usage.sampled_diagnostics.pss_interval_ms", "must cover the sample interval")
        )
    elapsed = [float(row["elapsed_ms"]) for row in diagnostics["samples"]]
    if elapsed != sorted(elapsed):
        violations.append(
            Violation(f"{path}.resource_usage.sampled_diagnostics.samples", "elapsed_ms must be monotonic")
        )
    gaps = [later - earlier for earlier, later in zip(elapsed, elapsed[1:])]
    expected_drained_at = round(usage["wall_ms"] + usage["drain_ms"], 3)
    if abs(elapsed[-1] - expected_drained_at) > 0.002:
        violations.append(
            Violation(
                f"{path}.resource_usage.sampled_diagnostics.samples[-1].elapsed_ms",
                f"must equal engine wall plus drain duration {expected_drained_at!r}",
            )
        )
    require_equal(
        f"{path}.resource_usage.sampled_diagnostics.max_sample_gap_ms",
        diagnostics["max_sample_gap_ms"],
        round(max(gaps, default=0.0), 3),
        violations,
    )

    rss_peaks: list[int] = []
    pss_peaks: list[int] = []
    observed: dict[tuple[int, int], dict[str, int | float]] = {}
    page_size = diagnostics["page_size_bytes"]
    for index, raw_sample in enumerate(diagnostics["samples"]):
        sample_path = f"{path}.resource_usage.sampled_diagnostics.samples[{index}]"
        if raw_sample["cgroup_memory_current_bytes"] > final["memory_peak_bytes"]:
            violations.append(
                Violation(f"{sample_path}.cgroup_memory_current_bytes", "must not exceed cgroup memory.peak")
            )
        identities: set[tuple[int, int]] = set()
        for process in raw_sample["processes"]:
            identity = (process["pid"], process["start_ticks"])
            if identity in identities:
                violations.append(Violation(f"{sample_path}.processes", f"duplicate identity {identity!r}"))
            identities.add(identity)
            if process["pid"] == usage["root_pid"] and process["start_ticks"] != usage["root_start_ticks"]:
                violations.append(Violation(f"{sample_path}.processes", "root PID has a different start time"))
            aggregate = observed.setdefault(
                identity,
                {
                    "pid": identity[0],
                    "start_ticks": identity[1],
                    "first_seen_ms": raw_sample["elapsed_ms"],
                    "last_seen_ms": raw_sample["elapsed_ms"],
                },
            )
            aggregate["last_seen_ms"] = raw_sample["elapsed_ms"]
        rss = sum(process["rss_pages"] * page_size // 1024 for process in raw_sample["processes"])
        require_equal(
            f"{sample_path}.sampled_summed_rss_kib_lower_bound",
            raw_sample["sampled_summed_rss_kib_lower_bound"],
            rss,
            violations,
        )
        rss_peaks.append(rss)
        pss_values = [process["pss_kib"] for process in raw_sample["processes"]]
        expected_pss = (
            sum(pss_values) if raw_sample["processes"] and all(value is not None for value in pss_values) else None
        )
        require_equal(
            f"{sample_path}.sampled_summed_pss_kib_lower_bound",
            raw_sample["sampled_summed_pss_kib_lower_bound"],
            expected_pss,
            violations,
        )
        if expected_pss is not None:
            pss_peaks.append(expected_pss)

    expected_observed = sorted(observed.values(), key=lambda row: (int(row["pid"]), int(row["start_ticks"])))
    require_equal(
        f"{path}.resource_usage.sampled_diagnostics.observed_processes",
        diagnostics["observed_processes"],
        expected_observed,
        violations,
    )
    require_equal(
        f"{path}.sampled_peak_rss_kib_lower_bound",
        sample["sampled_peak_rss_kib_lower_bound"],
        max(rss_peaks, default=0),
        violations,
    )
    require_equal(
        f"{path}.sampled_peak_pss_kib_lower_bound",
        sample["sampled_peak_pss_kib_lower_bound"],
        max(pss_peaks) if pss_peaks else None,
        violations,
    )
    require_equal(
        f"{path}.resource_usage.sampled_diagnostics.sampled_peak_summed_rss_kib_lower_bound",
        diagnostics["sampled_peak_summed_rss_kib_lower_bound"],
        max(rss_peaks, default=0),
        violations,
    )
    require_equal(
        f"{path}.resource_usage.sampled_diagnostics.sampled_peak_summed_pss_kib_lower_bound",
        diagnostics["sampled_peak_summed_pss_kib_lower_bound"],
        max(pss_peaks) if pss_peaks else None,
        violations,
    )
    cleanup = usage["cleanup"]
    require_equal(f"{path}.resource_usage.cleanup.kill_grace_ms", cleanup["kill_grace_ms"], 1000.0, violations)
    if not cleanup["kill_used"] and cleanup["lingering_before_kill"]:
        violations.append(
            Violation(f"{path}.resource_usage.cleanup", "cannot retain lingering identities without cgroup.kill")
        )
    if sample["ok"] and cleanup["kill_used"]:
        violations.append(
            Violation(f"{path}.resource_usage.cleanup.kill_used", "passing samples must drain without cgroup.kill")
        )
    sampler_cpu_ms = usage["sampler_cpu_user_ms"] + usage["sampler_cpu_sys_ms"]
    sampler_cpu_percent = round(sampler_cpu_ms * 100.0 / usage["wall_ms"], 3) if usage["wall_ms"] else 0.0
    require_equal(
        f"{path}.resource_usage.sampler_cpu_percent_of_wall",
        usage["sampler_cpu_percent_of_wall"],
        sampler_cpu_percent,
        violations,
    )


def validate_semantics(data: dict[str, Any], path: str, violations: list[Violation]) -> None:
    if data["schema"] == "pliego.benchmark-bundle":
        for index, result in enumerate(data["results"]):
            validate_semantics(result, f"{path}.results[{index}]", violations)
        return
    if data.get("status") == "not-applicable":
        return

    samples = data["samples"]
    if data["protocol"]["sample_count"] != len(samples):
        violations.append(
            Violation(
                f"{path}.protocol.sample_count",
                f"must equal the {len(samples)} retained samples",
            )
        )

    indices = [sample["index"] for sample in samples]
    if indices != list(range(len(samples))):
        violations.append(Violation(f"{path}.samples", "sample indices must be contiguous and ordered from zero"))

    protocol_measurement_method = data["protocol"]["measurement_method"]
    if protocol_measurement_method != "linux-cgroup-v2-v1":
        violations.append(
            Violation(
                f"{path}.protocol.measurement_method",
                "publishable benchmark results require linux-cgroup-v2-v1",
            )
        )
    engine = data["toolchain"]["engine"]
    for field in ("binary_path", "binary_sha256"):
        if field not in engine:
            violations.append(Violation(f"{path}.toolchain.engine.{field}", "is required for a publishable engine"))
    cgroup_paths: list[str] = []
    launch_accounts: list[tuple[int, int]] = []
    executable_identities: list[tuple[int, int]] = []
    for index, sample in enumerate(samples):
        sample_path = f"{path}.samples[{index}]"
        if sample["measurement_method"] != protocol_measurement_method:
            violations.append(
                Violation(
                    f"{sample_path}.measurement_method",
                    f"must equal protocol measurement_method {protocol_measurement_method!r}",
                )
            )
        validate_resource_usage(sample, sample_path, violations)
        if isinstance(sample["resource_usage"], dict):
            usage = sample["resource_usage"]
            cgroup_paths.append(usage["cgroup_path"])
            launch = usage["launch_security"]
            executable = launch["executable"]
            launch_accounts.append((launch["uid"], launch["gid"]))
            executable_identities.append((executable["device"], executable["inode"]))
            if "binary_path" in engine:
                require_equal(
                    f"{sample_path}.resource_usage.launch_security.executable.path",
                    executable["path"],
                    engine["binary_path"],
                    violations,
                )
            if "binary_sha256" in engine:
                require_equal(
                    f"{sample_path}.resource_usage.launch_security.executable.sha256",
                    executable["sha256"],
                    engine["binary_sha256"],
                    violations,
                )
            argv = launch["argv"]
            require_equal(
                f"{sample_path}.resource_usage.launch_security.argv[2]",
                argv[2],
                data["fixture"]["input"],
                violations,
            )
            for flag in ("--output", "--artifacts"):
                if argv.count(flag) != 1:
                    violations.append(
                        Violation(
                            f"{sample_path}.resource_usage.launch_security.argv",
                            f"must contain exactly one {flag!r}",
                        )
                    )
        correctness = sample["correctness"]
        output = sample["output"]
        failure = sample["failure"]
        expected_pass = bool(correctness["checks"]) and all(
            check["status"] == "pass" for check in correctness["checks"]
        )
        if correctness["pass"] != expected_pass:
            violations.append(
                Violation(
                    f"{sample_path}.correctness.pass",
                    "must be true exactly when no correctness check failed",
                )
            )
        if sample["ok"] != correctness["pass"]:
            violations.append(Violation(f"{sample_path}.ok", "must equal correctness.pass"))
        bridge_timings = sample.get("bridge_timings")
        if bridge_timings is not None:
            phase_total = math.fsum(value for value in bridge_timings["phases_ms"].values() if value is not None)
            if abs(phase_total - bridge_timings["total_ms"]) >= 0.02:
                violations.append(
                    Violation(
                        f"{sample_path}.bridge_timings.total_ms",
                        f"must equal the additive phase total {phase_total!r}",
                    )
                )
            native_engine = bridge_timings["native_engine_ms"]
            bridge_overhead = bridge_timings["bridge_overhead_ms"]
            if (native_engine is None) != (bridge_overhead is None):
                violations.append(
                    Violation(
                        f"{sample_path}.bridge_timings.bridge_overhead_ms",
                        "must be null exactly when native_engine_ms is null",
                    )
                )
            elif (
                native_engine is not None and abs(native_engine + bridge_overhead - bridge_timings["total_ms"]) >= 0.002
            ):
                violations.append(
                    Violation(
                        f"{sample_path}.bridge_timings.bridge_overhead_ms",
                        "must reconcile native_engine_ms to total_ms",
                    )
                )
        if output["published_pdf"] != failure["published_pdf"]:
            violations.append(
                Violation(
                    f"{sample_path}.failure.published_pdf",
                    "must equal output.published_pdf",
                )
            )
        if output["published_pdf"]:
            if output["pdf_bytes"] is None or output["pdf_bytes"] <= 0:
                violations.append(Violation(f"{sample_path}.output.pdf_bytes", "must be positive for a published PDF"))
            for field in ("pdf_sha256", "page_count"):
                if output[field] is None:
                    violations.append(Violation(f"{sample_path}.output.{field}", "must be present for a published PDF"))
        else:
            for field in ("pdf_bytes", "pdf_sha256", "page_count"):
                if output[field] is not None:
                    violations.append(
                        Violation(f"{sample_path}.output.{field}", "must be null when no PDF was published")
                    )

        if not sample["ok"] and "retained" not in sample:
            violations.append(Violation(sample_path, "failed samples must retain output and artifacts"))
        if sample["ok"]:
            expected_failure_code = data["fixture"].get("expected_failure_code")
            if expected_failure_code is None:
                if sample["exit_code"] != 0:
                    violations.append(Violation(f"{sample_path}.exit_code", "passing normal fixtures must exit zero"))
                if not output["published_pdf"]:
                    violations.append(
                        Violation(
                            f"{sample_path}.output.published_pdf",
                            "passing normal fixtures must publish a PDF",
                        )
                    )
                if failure["code"] is not None:
                    violations.append(
                        Violation(f"{sample_path}.failure.code", "passing normal fixtures cannot report a failure")
                    )
                expected_page_count = data["fixture"].get("expected_page_count")
                if expected_page_count is not None and output["page_count"] != expected_page_count:
                    violations.append(
                        Violation(
                            f"{sample_path}.output.page_count",
                            f"must equal expected page count {expected_page_count}",
                        )
                    )
            else:
                if sample["exit_code"] == 0:
                    violations.append(
                        Violation(f"{sample_path}.exit_code", "passing expected failures must exit nonzero")
                    )
                if output["published_pdf"]:
                    violations.append(
                        Violation(
                            f"{sample_path}.output.published_pdf",
                            "passing expected failures must not publish a PDF",
                        )
                    )
                if failure["code"] != expected_failure_code:
                    violations.append(
                        Violation(
                            f"{sample_path}.failure.code",
                            f"must equal expected failure code {expected_failure_code!r}",
                        )
                    )

    if len(cgroup_paths) != len(set(cgroup_paths)):
        violations.append(Violation(f"{path}.samples", "each render must retain a unique fresh cgroup_path"))
    if len(set(launch_accounts)) > 1:
        violations.append(Violation(f"{path}.samples", "all renders must use the same dedicated engine UID and GID"))
    if len(set(executable_identities)) > 1:
        violations.append(
            Violation(f"{path}.samples", "engine executable device/inode identity changed between renders")
        )

    aggregates = data["aggregates"]
    passing_samples = [sample for sample in samples if sample["ok"] and sample["correctness"]["pass"]]
    walls = [sample["wall_ms"] for sample in passing_samples]
    aggregate_series = {
        f"{path}.aggregates.latency": (aggregates["latency"], percentiles(walls)),
        f"{path}.aggregates.cpu.user_ms": (
            aggregates["cpu"]["user_ms"],
            percentiles([sample["user_ms"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.cpu.sys_ms": (
            aggregates["cpu"]["sys_ms"],
            percentiles([sample["sys_ms"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.cpu.wall_ms": (aggregates["cpu"]["wall_ms"], percentiles(walls)),
        f"{path}.aggregates.memory.cgroup_peak_bytes": (
            aggregates["memory"]["cgroup_peak_bytes"],
            percentiles([sample["memory_peak_bytes"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.memory.sampled_peak_rss_kib_lower_bound": (
            aggregates["memory"]["sampled_peak_rss_kib_lower_bound"],
            percentiles([sample["sampled_peak_rss_kib_lower_bound"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.io.read_bytes": (
            aggregates["io"]["read_bytes"],
            percentiles([sample["read_bytes"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.io.write_bytes": (
            aggregates["io"]["write_bytes"],
            percentiles([sample["write_bytes"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.io.read_operations": (
            aggregates["io"]["read_operations"],
            percentiles([sample["read_operations"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.io.write_operations": (
            aggregates["io"]["write_operations"],
            percentiles([sample["write_operations"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.output.pdf_bytes": (
            aggregates["output"]["pdf_bytes"],
            percentiles([sample["output"]["pdf_bytes"] for sample in passing_samples]),
        ),
    }
    for aggregate_path, (actual, expected) in aggregate_series.items():
        if actual != expected:
            violations.append(Violation(aggregate_path, f"must equal aggregate of passing samples {expected!r}"))

    pss_values = [
        sample["sampled_peak_pss_kib_lower_bound"]
        for sample in passing_samples
        if sample["sampled_peak_pss_kib_lower_bound"] is not None
    ]
    expected_pss = percentiles(pss_values) if passing_samples and len(pss_values) == len(passing_samples) else None
    actual_pss = aggregates["memory"].get("sampled_peak_pss_kib_lower_bound")
    if actual_pss != expected_pss:
        violations.append(
            Violation(
                f"{path}.aggregates.memory.sampled_peak_pss_kib_lower_bound",
                f"must equal aggregate of passing samples {expected_pss!r}",
            )
        )

    if "expected_page_count" in data["fixture"]:
        expected_page_count = data["fixture"]["expected_page_count"]
        if aggregates["output"]["page_count"] != expected_page_count:
            violations.append(
                Violation(
                    f"{path}.aggregates.output.page_count",
                    f"must equal fixture expected_page_count {expected_page_count!r}",
                )
            )

    mean_wall = statistics.fmean(walls) if walls else 0.0
    if "scaling" in aggregates:
        page_count = aggregates["output"]["page_count"]
        memory_peak = percentiles([sample["memory_peak_bytes"] for sample in passing_samples])
        expected_scaling = {
            "per_page_wall_ms": round(mean_wall / page_count, 3) if page_count else 0.0,
            "per_page_memory_peak_bytes": round(memory_peak["mean"] / page_count, 1) if page_count else 0.0,
        }
        if aggregates["scaling"] != expected_scaling:
            violations.append(
                Violation(f"{path}.aggregates.scaling", f"must equal passing-sample scaling {expected_scaling!r}")
            )
    if "throughput" in aggregates:
        expected_throughput = {
            "renders_per_minute": round(60_000 / mean_wall, 2) if mean_wall else 0.0,
            "concurrency": 1,
        }
        if aggregates["throughput"] != expected_throughput:
            violations.append(
                Violation(
                    f"{path}.aggregates.throughput",
                    f"must equal serial passing-sample throughput {expected_throughput!r}",
                )
            )

    pass_count = sum(sample["correctness"]["pass"] for sample in samples)
    all_passed = pass_count == len(samples)
    expected_correctness = {
        "pass_count": pass_count,
        "total": len(samples),
        "passed": all_passed,
    }
    aggregate_correctness = aggregates["correctness"]
    for field, expected in expected_correctness.items():
        if aggregate_correctness[field] != expected:
            violations.append(
                Violation(
                    f"{path}.aggregates.correctness.{field}",
                    f"must equal {expected!r} from retained samples",
                )
            )

    expected_status = "supported" if all_passed else "failed"
    if "status" in data and data["status"] != expected_status:
        violations.append(Violation(f"{path}.status", f"must be {expected_status!r}"))

    failed_samples = [sample for sample in samples if not sample["ok"]]
    expected_codes = Counter((sample["failure"]["code"] or "unknown") for sample in failed_samples)
    aggregate_failures = aggregates["failures"]
    if aggregate_failures["count"] != len(failed_samples):
        violations.append(
            Violation(
                f"{path}.aggregates.failures.count",
                f"must equal the {len(failed_samples)} failed samples",
            )
        )
    if "codes" in aggregate_failures and aggregate_failures["codes"] != dict(expected_codes):
        violations.append(
            Violation(
                f"{path}.aggregates.failures.codes",
                f"must equal the failed-sample code counts {dict(expected_codes)!r}",
            )
        )

    hashes = [
        sample["output"]["pdf_sha256"]
        for sample in samples
        if sample["ok"] and sample["correctness"]["pass"] and sample["output"]["pdf_sha256"]
    ]
    hash_counts = Counter(hashes)
    expected_determinism = {
        "identical_pdf_sha256": max(hash_counts.values(), default=0),
        "total": len(hashes),
        "pdf_sha256_variants": len(hash_counts),
    }
    determinism = aggregates["determinism"]
    for field, expected in expected_determinism.items():
        if determinism[field] != expected:
            violations.append(
                Violation(
                    f"{path}.aggregates.determinism.{field}",
                    f"must equal {expected} from passing sample hashes",
                )
            )


def validate_document(data: Any, schema: dict[str, Any]) -> list[Violation]:
    violations: list[Violation] = []
    validate(data, schema, "$", violations)
    if not violations:
        validate_semantics(data, "$", violations)
    return violations


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print("usage: validate_result.py <result.json> [<schema.json>]", file=sys.stderr)
        return 2
    data = load_json(Path(sys.argv[1]))
    schema = load_json(Path(sys.argv[2]) if len(sys.argv) == 3 else DEFAULT_SCHEMA)
    violations = validate_document(data, schema)
    if violations:
        for violation in violations:
            print(f"violation {violation}", file=sys.stderr)
        print(f"result failed validation with {len(violations)} violation(s)", file=sys.stderr)
        return 1
    print("result validated against schema")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
