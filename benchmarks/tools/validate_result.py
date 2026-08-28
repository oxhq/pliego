#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Dependency-free JSON Schema validation for benchmark results.

Implements the subset of draft-07 used by `schema/benchmark-result.v1.json`
(no `jsonschema` required): type (including null unions), const, enum, allOf, oneOf,
required, properties, additionalProperties, minimum, minLength, minItems,
maxItems, $ref
resolution into `definitions`, and pattern.

Usage:
    python3 benchmarks/tools/validate_result.py <result.json> [<schema.json>]
"""

from __future__ import annotations

import json
import hashlib
import math
import re
import statistics
import subprocess
import sys
import tomllib
from collections import Counter
from pathlib import Path, PurePosixPath
from typing import Any

from benchmark_runtime import BROWSERSHOT_TARGET, runtime_contract, runtime_target

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"
MANIFEST = ROOT / "benchmarks" / "manifest.toml"
PDF_ORACLE = ROOT / "benchmarks" / "tools" / "pdf_oracle.py"
PERCENTILE_METHOD = "nearest-rank-v1"
ENGINE_ACCOUNT_HOME = "/nonexistent/pliego-benchmark-engine"
RUNTIME_DIRECTORY_NAMES = {
    "HOME": "home",
    "XDG_CACHE_HOME": "xdg-cache",
    "XDG_CONFIG_HOME": "xdg-config",
    "XDG_DATA_HOME": "xdg-data",
    "XDG_RUNTIME_DIR": "xdg-runtime",
    "XDG_STATE_HOME": "xdg-state",
}
POPPLER_TOOLS = ("pdfinfo", "pdftotext", "pdffonts", "pdftoppm")
ORACLE_IDENTITY_REASON = "manifest does not pin canonical Poppler tool identities"
ADAPTER_ATTESTATION_REASON = "out-of-process OCI image attestation is unavailable"


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


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_fixture_hashes(fixture: dict[str, Any]) -> tuple[str, str]:
    input_path = (ROOT / fixture["input"]).resolve(strict=True)
    paths = [input_path, *(input_path.parent / asset for asset in fixture.get("assets", []))]
    digest = hashlib.sha256()
    for path in sorted(paths, key=lambda candidate: candidate.relative_to(input_path.parent).as_posix()):
        relative = path.relative_to(input_path.parent).as_posix()
        digest.update(relative.encode("utf-8") + b"\0")
        digest.update(bytes.fromhex(file_sha256(path)))
    return file_sha256(input_path), digest.hexdigest()


def clean_harness_revision() -> str | None:
    try:
        status = subprocess.run(
            [
                "git",
                "status",
                "--porcelain",
                "--",
                "benchmarks",
                ".github/workflows/pliego-benchmark.yml",
                ":(exclude)benchmarks/baselines",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=30,
        )
        revision = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    value = revision.stdout.strip()
    if status.returncode != 0 or status.stdout.strip() or revision.returncode != 0:
        return None
    return value if re.fullmatch(r"[0-9a-f]{40}", value) else None


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

    if "allOf" in schema:
        for branch in schema["allOf"]:
            validate(data, branch, path, violations, root)

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
        if "exclusiveMinimum" in schema and data <= schema["exclusiveMinimum"]:
            violations.append(Violation(path, f"{data} <= exclusive minimum {schema['exclusiveMinimum']}"))

    if isinstance(data, list):
        if "minItems" in schema and len(data) < schema["minItems"]:
            violations.append(Violation(path, f"expected >= {schema['minItems']} items, got {len(data)}"))
        if "maxItems" in schema and len(data) > schema["maxItems"]:
            violations.append(Violation(path, f"expected <= {schema['maxItems']} items, got {len(data)}"))
        if "items" in schema and isinstance(schema["items"], dict):
            for index, item in enumerate(data):
                validate(item, schema["items"], f"{path}[{index}]", violations, root)

    if isinstance(data, dict):
        if "minProperties" in schema and len(data) < schema["minProperties"]:
            violations.append(Violation(path, f"expected >= {schema['minProperties']} properties, got {len(data)}"))
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


def oracle_manifest_pins_complete(manifest: dict[str, Any]) -> bool:
    oracle = manifest.get("oracle", {})
    if oracle.get("contract") != "pliego.pdf-oracle.v1":
        return False
    for tool in POPPLER_TOOLS:
        path = oracle.get(f"{tool}_path")
        digest = oracle.get(f"{tool}_sha256")
        version = oracle.get(f"{tool}_version")
        if (
            not isinstance(path, str)
            or not PurePosixPath(path).is_absolute()
            or ".." in PurePosixPath(path).parts
            or str(PurePosixPath(path)) != path
            or not isinstance(digest, str)
            or re.fullmatch(r"[0-9a-f]{64}", digest) is None
            or not isinstance(version, str)
            or not version.strip()
        ):
            return False
    return True


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
    temporary_storage = launch["temporary_storage"]
    for field, expected in {
        "access_time": "FS_NOATIME_FL",
        "directory_sync": "FS_DIRSYNC_FL",
        "file_sync": "FS_SYNC_FL",
        "filesystem": "ext4",
        "scope": "per-invocation-private",
    }.items():
        require_equal(
            f"{path}.resource_usage.launch_security.temporary_storage.{field}",
            temporary_storage[field],
            expected,
            violations,
        )
    launch_argv = launch["argv"]
    target_classification = runtime_target(launch_argv)
    require_equal(
        f"{path}.resource_usage.launch_security.temporary_storage.runtime_environment",
        temporary_storage["runtime_environment"],
        runtime_contract(target_classification),
        violations,
    )
    require_equal(
        f"{path}.resource_usage.launch_security.temporary_storage.runtime_target",
        temporary_storage["runtime_target"],
        target_classification,
        violations,
    )
    account_home = temporary_storage["account_home"]
    require_equal(
        f"{path}.resource_usage.launch_security.temporary_storage.account_home.path",
        account_home["path"],
        ENGINE_ACCOUNT_HOME,
        violations,
    )
    bindings_proof = temporary_storage["runtime_path_bindings"]
    if target_classification == BROWSERSHOT_TARGET:
        if not isinstance(bindings_proof, dict):
            violations.append(
                Violation(
                    f"{path}.resource_usage.launch_security.temporary_storage.runtime_path_bindings",
                    "Browsershot must retain private runtime path identities",
                )
            )
        else:
            pre = bindings_proof["pre"]
            post = bindings_proof["post"]
            require_equal(
                f"{path}.resource_usage.launch_security.temporary_storage.runtime_path_bindings.post",
                post,
                pre,
                violations,
            )
            root_path = PurePosixPath(pre["temporary_root"]["path"])
            if (
                not root_path.is_absolute()
                or ".." in root_path.parts
                or str(root_path) != pre["temporary_root"]["path"]
            ):
                violations.append(
                    Violation(
                        f"{path}.resource_usage.launch_security.temporary_storage.runtime_path_bindings.pre.temporary_root.path",
                        "must be an absolute canonical path",
                    )
                )
            root_identity_data = pre["temporary_root"]["identity"]
            root_identity = (root_identity_data["device"], root_identity_data["inode"])
            identities: set[tuple[int, int]] = set()
            for variable, relative_path in RUNTIME_DIRECTORY_NAMES.items():
                binding = pre["bindings"][variable]
                require_equal(
                    f"{path}.resource_usage.launch_security.temporary_storage.runtime_path_bindings.pre.bindings.{variable}.relative_path",
                    binding["relative_path"],
                    relative_path,
                    violations,
                )
                identity = binding["identity"]
                identities.add((identity["device"], identity["inode"]))
            if len(identities) != len(RUNTIME_DIRECTORY_NAMES) or root_identity in identities:
                violations.append(
                    Violation(
                        f"{path}.resource_usage.launch_security.temporary_storage.runtime_path_bindings.pre.bindings",
                        "must bind unique child directory identities distinct from the temporary root",
                    )
                )
    elif bindings_proof is not None:
        violations.append(
            Violation(
                f"{path}.resource_usage.launch_security.temporary_storage.runtime_path_bindings",
                "non-browser targets must not claim private browser runtime roots",
            )
        )
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
    minimum_one_shot_wall = round(usage["wall_ms"] + usage["drain_ms"] + settle["duration_ms"], 3)
    if sample["one_shot_wall_ms"] + 0.003 < minimum_one_shot_wall:
        violations.append(
            Violation(
                f"{path}.one_shot_wall_ms",
                "must cover engine wall, descendant drain, and accounting settle "
                f"({minimum_one_shot_wall!r} ms minimum)",
            )
        )
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


def validate_manifest_contract(
    data: dict[str, Any],
    path: str,
    violations: list[Violation],
    manifest: dict[str, Any],
    expected_harness_revision: str | None,
) -> None:
    target_id = data["target"]["id"]
    fixture_id = data["fixture"]["id"]
    target = manifest.get("targets", {}).get(target_id)
    fixture = manifest.get("fixtures", {}).get(fixture_id)
    if target is None or not target.get("enabled", False):
        violations.append(Violation(f"{path}.target.id", "must name an enabled canonical manifest target"))
        return
    if fixture is None:
        violations.append(Violation(f"{path}.fixture.id", "must name a canonical manifest fixture"))
        return

    require_equal(f"{path}.target.label", data["target"]["label"], target["label"], violations)
    correctness = fixture.get("correctness", {})
    canonical_fixture = {
        "purpose": fixture["purpose"],
        "category": fixture["category"],
        "input": fixture["input"],
        "expected_page_count": correctness.get("page_count"),
        "expected_page_width_points": correctness.get("page_width_points"),
        "expected_page_height_points": correctness.get("page_height_points"),
        "dimension_tolerance_points": correctness.get("dimension_tolerance_points"),
        "expected_text_contains": correctness.get("text_contains", []),
        "expected_text": correctness.get("text_equals"),
        "expected_font_families": correctness.get("font_families", []),
        "expected_normalized_raster_sha256": correctness.get("normalized_raster_sha256"),
        "expected_link_targets": correctness.get("link_targets", []),
        "expected_failure_code": correctness.get("failure_code"),
    }
    for field, expected in canonical_fixture.items():
        require_equal(f"{path}.fixture.{field}", data["fixture"].get(field), expected, violations)

    protocol = data["protocol"]
    canonical_protocol = manifest["protocol"]
    expected_protocol = {
        "sample_order": canonical_protocol["sample_order"],
        "seed": canonical_protocol.get("seed"),
        "network": canonical_protocol["network"],
        "binary_profile": target.get("profile", canonical_protocol["binary_profile"]),
    }
    if data.get("status") == "not-applicable":
        expected_protocol.update(warmup_iterations=0, sample_count=0)
    else:
        expected_protocol.update(
            warmup_iterations=canonical_protocol["warmup_iterations"],
            sample_count=fixture.get("samples", canonical_protocol["samples_short"]),
            correctness_preflight_iterations=1,
            execution_order="untimed-correctness-preflight,warmup,timed",
        )
    for field, expected in expected_protocol.items():
        require_equal(f"{path}.protocol.{field}", protocol.get(field), expected, violations)

    supported_fixtures = (
        set(manifest["fixtures"])
        if target.get("kind", "pliego") == "pliego"
        else set(target.get("supported_fixtures", []))
    )
    if data.get("status") != "not-applicable" and target.get("kind", "pliego") == "adapter":
        if fixture_id not in supported_fixtures:
            violations.append(Violation(f"{path}.fixture.id", "is not supported by the canonical adapter target"))

    if data.get("status") == "not-applicable":
        if fixture_id not in supported_fixtures:
            expected_reason = target.get("not_applicable", {}).get(fixture_id, target.get("unsupported_reason"))
            require_equal(f"{path}.reason", data["reason"], expected_reason, violations)
        else:
            if not oracle_manifest_pins_complete(manifest) and ORACLE_IDENTITY_REASON not in data["reason"]:
                violations.append(Violation(f"{path}.reason", "must identify missing canonical Poppler pins"))
            if target.get("kind", "pliego") == "adapter":
                if ADAPTER_ATTESTATION_REASON not in data["reason"]:
                    violations.append(Violation(f"{path}.reason", "must identify missing external image attestation"))
                if (
                    not target.get("image_digest")
                    and "manifest does not pin an immutable OCI image digest" not in data["reason"]
                ):
                    violations.append(Violation(f"{path}.reason", "must identify the missing immutable image digest"))
        if target.get("kind", "pliego") == "adapter" and fixture_id in supported_fixtures:
            runtime_names = ["php", *(["node", "chrome"] if target.get("requires_network_isolation") else [])]
            required_hashes = [
                *target.get("identity_keys", []),
                *(f"{name}_sha256" for name in runtime_names),
            ]
            required_paths = [
                "adapter_path",
                *(key.removesuffix("_sha256") + "_path" for key in target.get("identity_keys", [])),
                *(f"{name}_path" for name in runtime_names),
            ]
            pins = target.get("identity_sha256", {})
            if any(
                not isinstance(pins.get(key), str) or re.fullmatch(r"[0-9a-f]{64}", pins[key]) is None
                for key in required_hashes
            ) and ("manifest does not pin canonical dependency and runtime SHA-256 values" not in data["reason"]):
                violations.append(Violation(f"{path}.reason", "must identify missing canonical identity hashes"))
            paths = target.get("identity_paths", {})
            if any(
                not isinstance(paths.get(key), str) or not PurePosixPath(paths[key]).is_absolute()
                for key in required_paths
            ) and ("manifest does not pin canonical dependency and runtime paths" not in data["reason"]):
                violations.append(Violation(f"{path}.reason", "must identify missing canonical identity paths"))
        return

    host = data["host"]
    require_equal(f"{path}.host.os", host["os"], "Linux", violations)
    require_equal(f"{path}.host.arch", host["arch"], "x86_64", violations)
    require_equal(f"{path}.host.dedicated", host["dedicated"], True, violations)
    revision = data["toolchain"].get("harness_revision")
    if not isinstance(revision, str) or re.fullmatch(r"[0-9a-f]{40}", revision) is None:
        violations.append(Violation(f"{path}.toolchain.harness_revision", "must retain the exact clean harness commit"))
    elif expected_harness_revision is None:
        violations.append(
            Violation(
                f"{path}.toolchain.harness_revision",
                "must be validated from a clean checkout of the recorded harness commit",
            )
        )
    else:
        require_equal(
            f"{path}.toolchain.harness_revision",
            revision,
            expected_harness_revision,
            violations,
        )
    competitors = data["toolchain"].get("competitors")
    if not isinstance(competitors, dict):
        violations.append(Violation(f"{path}.toolchain.competitors", "must retain PDF oracle identity"))
    else:
        require_equal(
            f"{path}.toolchain.competitors.oracle.contract",
            competitors.get("oracle.contract"),
            "pliego.pdf-oracle.v1",
            violations,
        )
        oracle_path = competitors.get("oracle.oracle_path")
        if not isinstance(oracle_path, str) or not PurePosixPath(oracle_path).is_absolute():
            violations.append(Violation(f"{path}.toolchain.competitors.oracle.oracle_path", "must be absolute"))
        oracle_digest = competitors.get("oracle.oracle_sha256")
        if not isinstance(oracle_digest, str) or re.fullmatch(r"[0-9a-f]{64}", oracle_digest) is None:
            violations.append(Violation(f"{path}.toolchain.competitors.oracle.oracle_sha256", "must be SHA-256"))
        else:
            require_equal(
                f"{path}.toolchain.competitors.oracle.oracle_sha256",
                oracle_digest,
                file_sha256(PDF_ORACLE),
                violations,
            )
        oracle_manifest = manifest.get("oracle", {})
        if not oracle_manifest_pins_complete(manifest):
            violations.append(Violation(f"{path}.toolchain.competitors", ORACLE_IDENTITY_REASON))
        for tool in POPPLER_TOOLS:
            tool_path = competitors.get(f"oracle.{tool}_path")
            if not isinstance(tool_path, str) or not PurePosixPath(tool_path).is_absolute():
                violations.append(
                    Violation(f"{path}.toolchain.competitors.oracle.{tool}_path", "must be an absolute path")
                )
            digest = competitors.get(f"oracle.{tool}_sha256")
            if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
                violations.append(Violation(f"{path}.toolchain.competitors.oracle.{tool}_sha256", "must be SHA-256"))
            version = competitors.get(f"oracle.{tool}_version")
            if not isinstance(version, str) or not version.strip():
                violations.append(Violation(f"{path}.toolchain.competitors.oracle.{tool}_version", "is required"))
            for field, actual in (("path", tool_path), ("sha256", digest), ("version", version)):
                expected = oracle_manifest.get(f"{tool}_{field}")
                if isinstance(expected, str) and expected:
                    require_equal(
                        f"{path}.toolchain.competitors.oracle.{tool}_{field}",
                        actual,
                        expected,
                        violations,
                    )

    try:
        input_hash, bundle_hash = canonical_fixture_hashes(fixture)
    except OSError as error:
        violations.append(Violation(f"{path}.fixture", f"cannot verify canonical fixture files: {error}"))
    else:
        require_equal(f"{path}.fixture.input_sha256", data["fixture"].get("input_sha256"), input_hash, violations)
        require_equal(f"{path}.fixture.bundle_sha256", data["fixture"].get("bundle_sha256"), bundle_hash, violations)

    engine = data["toolchain"]["engine"]
    if target.get("kind", "pliego") == "pliego":
        expected_engine = {
            "name": "pliego",
            "version": target["version"],
            "commit": target["commit"],
            "release_tag": target["release_tag"],
            "servo_build": target["servo_build"],
            "servo_base": target["servo_base"],
            "bundle": target["archive"],
            "bundle_sha256": target["archive_sha256"],
            "bundle_bytes": target["archive_bytes"],
            "binary_sha256": target["binary_sha256"],
            "binary_bytes": target["binary_bytes"],
            "profile": target["profile"],
        }
        for field, expected in expected_engine.items():
            require_equal(f"{path}.toolchain.engine.{field}", engine.get(field), expected, violations)
    else:
        violations.append(
            Violation(
                f"{path}.target.id",
                "adapter target must be N/A until out-of-process OCI image attestation is implemented",
            )
        )
        adapter = (ROOT / target["adapter"]).resolve()
        expected_adapter_hash = file_sha256(adapter)
        for field, expected in {
            "name": target["package"].split("/")[-1],
            "version": target["version"],
            "package": target["package"],
            "binary_sha256": expected_adapter_hash,
            "binary_bytes": adapter.stat().st_size,
            "profile": target["profile"],
        }.items():
            require_equal(f"{path}.toolchain.engine.{field}", engine.get(field), expected, violations)
        competitors = data["toolchain"].get("competitors", {})
        expected_image = target.get("image_digest")
        hash_pins = target.get("identity_sha256", {})
        path_pins = target.get("identity_paths", {})
        expected_adapter_path = path_pins.get("adapter_path") if isinstance(path_pins, dict) else None
        if not isinstance(expected_adapter_path, str) or not PurePosixPath(expected_adapter_path).is_absolute():
            violations.append(Violation(f"{path}.target.id", "manifest must pin adapter path 'adapter_path'"))
        else:
            require_equal(
                f"{path}.toolchain.engine.binary_path",
                engine.get("binary_path"),
                expected_adapter_path,
                violations,
            )
        if not isinstance(expected_image, str) or re.fullmatch(r"sha256:[0-9a-f]{64}", expected_image) is None:
            violations.append(
                Violation(f"{path}.target.id", "adapter target has no pinned immutable image and must be N/A")
            )
        for field, expected in {
            "adapter.contract": "pliego.benchmark-adapter.v1",
            "adapter.target": target_id,
            "adapter.package": target["package"],
            "adapter.package_version": target["version"],
            "adapter.adapter_path": expected_adapter_path,
            "adapter.adapter_sha256": expected_adapter_hash,
            "environment.contract": target.get("runtime_identity_contract"),
            "environment.image_digest": expected_image,
            "environment.rootfs_read_only": "true",
            "environment.read_only.adapter_path": "true",
        }.items():
            require_equal(f"{path}.toolchain.competitors.{field}", competitors.get(field), expected, violations)
        for lock_name in target.get("lockfiles", []):
            lock = (ROOT / lock_name).resolve()
            key = f"adapter.{lock.name.removesuffix('.json').replace('-', '_').replace('.', '_')}_sha256"
            require_equal(f"{path}.toolchain.competitors.{key}", competitors.get(key), file_sha256(lock), violations)
        for key in target.get("identity_keys", []):
            value = competitors.get(f"adapter.{key}")
            if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
                violations.append(
                    Violation(f"{path}.toolchain.competitors.adapter.{key}", "must retain dependency tree SHA-256")
                )
            expected_hash = hash_pins.get(key) if isinstance(hash_pins, dict) else None
            if not isinstance(expected_hash, str) or re.fullmatch(r"[0-9a-f]{64}", expected_hash) is None:
                violations.append(Violation(f"{path}.target.id", f"manifest must pin adapter identity {key!r}"))
            else:
                require_equal(
                    f"{path}.toolchain.competitors.adapter.{key}",
                    value,
                    expected_hash,
                    violations,
                )
            path_key = key.removesuffix("_sha256") + "_path"
            expected_path = path_pins.get(path_key) if isinstance(path_pins, dict) else None
            if not isinstance(expected_path, str) or not PurePosixPath(expected_path).is_absolute():
                violations.append(Violation(f"{path}.target.id", f"manifest must pin adapter path {path_key!r}"))
            else:
                require_equal(
                    f"{path}.toolchain.competitors.adapter.{path_key}",
                    competitors.get(f"adapter.{path_key}"),
                    expected_path,
                    violations,
                )
            require_equal(
                f"{path}.toolchain.competitors.environment.read_only.{path_key}",
                competitors.get(f"environment.read_only.{path_key}"),
                "true",
                violations,
            )
        runtime_names = ["php", *(["node", "chrome"] if target.get("requires_network_isolation") else [])]
        for runtime in runtime_names:
            for suffix in ("path", "version"):
                key = f"adapter.{runtime}_{suffix}"
                if not competitors.get(key):
                    violations.append(Violation(f"{path}.toolchain.competitors.{key}", "is required"))
            digest = competitors.get(f"adapter.{runtime}_sha256")
            if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
                violations.append(
                    Violation(f"{path}.toolchain.competitors.adapter.{runtime}_sha256", "must be SHA-256")
                )
            for suffix, pins in (("sha256", hash_pins), ("path", path_pins)):
                key = f"{runtime}_{suffix}"
                expected = pins.get(key) if isinstance(pins, dict) else None
                if (
                    not isinstance(expected, str)
                    or (suffix == "sha256" and re.fullmatch(r"[0-9a-f]{64}", expected) is None)
                    or (suffix == "path" and not PurePosixPath(expected).is_absolute())
                ):
                    violations.append(Violation(f"{path}.target.id", f"manifest must pin adapter identity {key!r}"))
                else:
                    require_equal(
                        f"{path}.toolchain.competitors.adapter.{key}",
                        competitors.get(f"adapter.{key}"),
                        expected,
                        violations,
                    )
            require_equal(
                f"{path}.toolchain.competitors.environment.read_only.{runtime}_path",
                competitors.get(f"environment.read_only.{runtime}_path"),
                "true",
                violations,
            )
        if target.get("requires_network_isolation"):
            require_equal(
                f"{path}.toolchain.competitors.environment.network_isolation",
                competitors.get("environment.network_isolation"),
                "linux-private-network-namespace-v1",
                violations,
            )


def validate_semantics(
    data: dict[str, Any],
    path: str,
    violations: list[Violation],
    manifest: dict[str, Any],
    expected_harness_revision: str | None,
) -> None:
    if data["schema"] == "pliego.benchmark-bundle":
        for index, result in enumerate(data["results"]):
            validate_semantics(
                result,
                f"{path}.results[{index}]",
                violations,
                manifest,
                expected_harness_revision,
            )
        return
    validate_manifest_contract(data, path, violations, manifest, expected_harness_revision)
    if data.get("status") == "not-applicable":
        return

    samples = data["samples"]
    require_equal(
        f"{path}.protocol.correctness_preflight_iterations",
        data["protocol"]["correctness_preflight_iterations"],
        1,
        violations,
    )
    require_equal(
        f"{path}.protocol.execution_order",
        data["protocol"]["execution_order"],
        "untimed-correctness-preflight,warmup,timed",
        violations,
    )
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
    target_manifest = manifest.get("targets", {}).get(data["target"]["id"], {})
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
            if target_manifest.get("requires_network_isolation"):
                network = launch.get("network_isolation")
                if not isinstance(network, dict):
                    violations.append(
                        Violation(
                            f"{sample_path}.resource_usage.launch_security",
                            "must retain private network namespace proof",
                        )
                    )
                elif network["host_namespace"] == network["engine_namespace"]:
                    violations.append(
                        Violation(
                            f"{sample_path}.resource_usage.launch_security.network_isolation",
                            "engine namespace must differ from host namespace",
                        )
                    )
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
            target_kind = target_manifest.get("kind")
            expected_runtime_argv = [
                engine.get("binary_path", ""),
                "render" if target_kind == "adapter" else "render-api2",
            ]
            require_equal(
                f"{sample_path}.resource_usage.launch_security.temporary_storage.runtime_target",
                launch["temporary_storage"]["runtime_target"],
                runtime_target(expected_runtime_argv),
                violations,
            )
            if target_kind == "pliego":
                require_equal(
                    f"{sample_path}.resource_usage.launch_security.argv",
                    argv,
                    [executable["path"], "render-api2"],
                    violations,
                )
            elif target_kind == "adapter":
                require_equal(
                    f"{sample_path}.resource_usage.launch_security.argv[1]",
                    argv[1],
                    "render",
                    violations,
                )
                if len(argv) < 3:
                    violations.append(
                        Violation(
                            f"{sample_path}.resource_usage.launch_security.argv",
                            "adapter launch must include the bare fixture input as argv[2]",
                        )
                    )
                else:
                    require_equal(
                        f"{sample_path}.resource_usage.launch_security.argv[2]",
                        argv[2],
                        PurePosixPath(data["fixture"]["input"]).name,
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
            else:
                violations.append(
                    Violation(
                        f"{sample_path}.resource_usage.launch_security.argv",
                        f"cannot validate command shape for target kind {target_kind!r}",
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
            for field in (
                "pdf_bytes",
                "pdf_sha256",
                "page_count",
                "page_dimensions_points",
                "normalized_text_sha256",
                "normalized_raster_sha256",
            ):
                if output.get(field) is not None:
                    violations.append(
                        Violation(f"{sample_path}.output.{field}", "must be null when no PDF was published")
                    )
            if output["font_families"]:
                violations.append(
                    Violation(f"{sample_path}.output.font_families", "must be empty when no PDF was published")
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
                expected_width = data["fixture"].get("expected_page_width_points")
                expected_height = data["fixture"].get("expected_page_height_points")
                tolerance = data["fixture"].get("dimension_tolerance_points")
                if expected_width is not None and expected_height is not None:
                    dimensions = output.get("page_dimensions_points")
                    if not isinstance(dimensions, list) or len(dimensions) != output["page_count"]:
                        violations.append(
                            Violation(
                                f"{sample_path}.output.page_dimensions_points",
                                "must retain one width/height pair for every passing page",
                            )
                        )
                    else:
                        allowed = float(tolerance or 0)
                        for page_index, (width, height) in enumerate(dimensions):
                            if abs(width - expected_width) > allowed or abs(height - expected_height) > allowed:
                                violations.append(
                                    Violation(
                                        f"{sample_path}.output.page_dimensions_points[{page_index}]",
                                        "must be within the declared fixture dimension tolerance",
                                    )
                                )
                expected_checks = {"pdf_envelope", "pdf_parse"}
                if expected_page_count is not None:
                    expected_checks.add("page_count")
                if expected_width is not None and expected_height is not None:
                    expected_checks.add("page_dimensions")
                expected_checks.update(
                    f"text:{fragment}" for fragment in data["fixture"].get("expected_text_contains", [])
                )
                expected_text = data["fixture"].get("expected_text")
                if expected_text is not None:
                    expected_checks.add("text_exact")
                    require_equal(
                        f"{sample_path}.output.normalized_text_sha256",
                        output["normalized_text_sha256"],
                        hashlib.sha256(" ".join(expected_text.split()).encode()).hexdigest(),
                        violations,
                    )
                expected_fonts = data["fixture"].get("expected_font_families", [])
                if expected_fonts:
                    expected_checks.add("fonts_exact")
                    require_equal(
                        f"{sample_path}.output.font_families",
                        sorted(output["font_families"]),
                        sorted(expected_fonts),
                        violations,
                    )
                expected_raster = data["fixture"].get("expected_normalized_raster_sha256")
                if expected_raster is not None:
                    expected_checks.add("raster_normalized")
                    expected_checks.add("raster_parity")
                    if output["normalized_raster_sha256"] is None:
                        violations.append(Violation(f"{sample_path}.output.normalized_raster_sha256", "is required"))
                    require_equal(
                        f"{sample_path}.output.normalized_raster_sha256",
                        output["normalized_raster_sha256"],
                        expected_raster,
                        violations,
                    )
                expected_checks.update(f"link:{target}" for target in data["fixture"].get("expected_link_targets", []))
                check_names = [check["name"] for check in correctness["checks"]]
                missing_checks = sorted(name for name in expected_checks if check_names.count(name) != 1)
                if missing_checks:
                    violations.append(
                        Violation(
                            f"{sample_path}.correctness.checks",
                            f"must contain each shared PDF oracle check exactly once; missing={missing_checks!r}",
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
    one_shot_walls = [sample["one_shot_wall_ms"] for sample in passing_samples]
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
    if passing_samples and "throughput" not in aggregates:
        violations.append(
            Violation(
                f"{path}.aggregates.throughput",
                "is required when correctness-passing timed samples exist",
            )
        )
    elif "throughput" in aggregates:
        mean_one_shot_wall = statistics.fmean(one_shot_walls) if one_shot_walls else 0.0
        expected_throughput = {
            "renders_per_minute": round(60_000 / mean_one_shot_wall, 2) if mean_one_shot_wall else 0.0,
            "concurrency": 1,
            "mean_one_shot_wall_ms": round(mean_one_shot_wall, 3),
            "measurement_boundary": "runner-process-open-through-sampler-exit",
        }
        if aggregates["throughput"] != expected_throughput:
            violations.append(
                Violation(
                    f"{path}.aggregates.throughput",
                    f"must equal drain-inclusive serial passing-sample throughput {expected_throughput!r}",
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
    expected_raster = data["fixture"].get("expected_normalized_raster_sha256")
    if expected_raster is not None:
        raster_hashes = [
            sample["output"]["normalized_raster_sha256"]
            for sample in passing_samples
            if sample["output"]["normalized_raster_sha256"]
        ]
        if len(raster_hashes) != len(passing_samples) or len(set(raster_hashes)) != 1:
            violations.append(
                Violation(f"{path}.samples", "passing samples must share one normalized raster signature")
            )
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


def validate_document(
    data: Any,
    schema: dict[str, Any],
    manifest: dict[str, Any] | None = None,
    expected_harness_revision: str | None = None,
) -> list[Violation]:
    violations: list[Violation] = []
    validate(data, schema, "$", violations)
    if not violations:
        validate_semantics(
            data,
            "$",
            violations,
            manifest or tomllib.loads(MANIFEST.read_text(encoding="utf-8")),
            expected_harness_revision or clean_harness_revision(),
        )
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
