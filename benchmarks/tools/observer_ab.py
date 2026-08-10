#!/usr/bin/env python3

"""Build and verify the host-bound 20-pair procfs observer A/B proof."""

from __future__ import annotations

import argparse
import functools
import hashlib
import math
import random
import re
import sys
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parents[1]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-observer-proof.v1.json"
SOURCE_BROKER = TOOLS / "process_tree_sampler.py"
INSTALLED_BROKER = Path("/usr/local/libexec/pliego-cgroup-broker")
PAIR_COUNT = 20
SEED = 20260808
THRESHOLD_PERCENT_EXCLUSIVE = 2.0
MAX_DOCUMENT_BYTES = 128 * 1024 * 1024
RESULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"
ALLOWED_DIFFERENCES = [
    "observation_enabled_and_sampled_diagnostics",
    "timing_and_accounting_values",
    "fresh_cgroup_process_and_workspace_identities",
]

sys.path.insert(0, str(TOOLS))
import benchmark_publication  # noqa: E402
import validate_host_proof  # noqa: E402
import validate_result  # noqa: E402


def canonical_bytes(value: Any) -> bytes:
    return benchmark_publication.json_bytes(value, trailing_newline=False)


def load_bound_object(path: Path, label: str) -> tuple[dict[str, Any], dict[str, Any]]:
    return benchmark_publication.load_bound_json_object(path, label, max_bytes=MAX_DOCUMENT_BYTES)


def normalized_contract(proof: dict[str, Any]) -> dict[str, Any]:
    launch = proof["launch_security"]
    identity = launch["command_identity"]
    counters = proof["counters"]
    argv = list(launch["argv"])
    output_index = argv.index("--output") + 1
    artifacts_index = argv.index("--artifacts") + 1
    argv[output_index] = f"<fresh-output-directory>/{PurePosixPath(argv[output_index]).name}"
    argv[artifacts_index] = "<fresh-artifact-directory>"
    return {
        "cgroup_boundary": {
            "method": proof["method"],
            "scope": proof["scope"],
            "delegated_parent": proof["delegated_parent"],
        },
        "launch_security": {
            "account": launch["account"],
            "uid": launch["uid"],
            "gid": launch["gid"],
            "status": launch["status"],
            "execution_binding": launch["execution_binding"],
            "parent_death_signal": launch["parent_death_signal"],
            "network_namespace_isolated": launch["network_namespace_isolated"],
            "migration_write_probes": launch["migration_write_probes"],
            "migration_fd_probes": launch["migration_fd_probes"],
            "executable": launch["executable"],
        },
        "frozen_command": {
            "argv": argv,
            "cwd": identity["cwd"],
            "input": identity["input"],
        },
        "outcome": {
            "exit_code": proof["exit_code"],
            "signal": proof["signal"],
            "engine_stdout": proof["engine_stdout"],
            "engine_stderr": proof["engine_stderr"],
        },
        "static_controls": {
            "cleanup_kill_used": proof["cleanup"]["kill_used"],
            "cleanup_kill_grace_ms": proof["cleanup"]["kill_grace_ms"],
            "cleanup_lingering_before_kill": proof["cleanup"]["lingering_before_kill"],
            "accounting_poll_interval_ms": proof["accounting_settle"]["poll_interval_ms"],
            "empty_cgroup_events": counters["empty"]["cgroup_events"],
            "after_join_cgroup_events": counters["after_join"]["cgroup_events"],
            "final_cgroup_events": counters["final"]["cgroup_events"],
        },
    }


def observation(proof: dict[str, Any]) -> dict[str, Any]:
    identity = proof["launch_security"]["command_identity"]
    diagnostics = proof["sampled_diagnostics"]
    return {
        "observation_enabled": diagnostics["observation_enabled"],
        "tree_wall_ms": proof["tree_wall_ms"],
        "sampler_cpu_ms": proof["sampler_cpu_user_ms"] + proof["sampler_cpu_sys_ms"],
        "cgroup_path": proof["cgroup_path"],
        "root_pid": proof["root_pid"],
        "root_start_ticks": proof["root_start_ticks"],
        "workspace_directory": identity["workspace_directory"],
        "output_directory": identity["output_directory"],
        "artifact_directory": identity["artifact_directory"],
    }


def measurement_pair(index: int, order: list[str], off: dict[str, Any], on: dict[str, Any]) -> dict[str, Any]:
    off_contract = normalized_contract(off)
    on_contract = normalized_contract(on)
    if off_contract != on_contract:
        raise AssertionError(f"observer A/B security contract changed in pair {index}")
    off_observation = observation(off)
    on_observation = observation(on)
    off_ms = float(off_observation["tree_wall_ms"])
    on_ms = float(on_observation["tree_wall_ms"])
    sampler_cpu_ms = float(on_observation["sampler_cpu_ms"])
    return {
        "index": index,
        "order": order,
        "observation_off_record": off,
        "observation_on_record": on,
        "overhead_percent": (on_ms - off_ms) * 100.0 / off_ms,
        "sampler_cpu_percent_of_tree_wall": sampler_cpu_ms * 100.0 / on_ms,
    }


def build_measurements(pairs: list[dict[str, Any]], *, duration_seconds: float) -> dict[str, Any]:
    overhead = [float(pair["overhead_percent"]) for pair in pairs]
    p95 = validate_result.percentile(overhead, 95)
    return {
        "schema": "pliego.observer-ab-measurements",
        "version": 2,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "method": "randomized-paired-full-broker-record-v4",
        "seed": SEED,
        "pair_count": PAIR_COUNT,
        "execution_count": PAIR_COUNT * 2,
        "workload_duration_seconds": duration_seconds,
        "percentile_method": validate_result.PERCENTILE_METHOD,
        "threshold_percent_exclusive": THRESHOLD_PERCENT_EXCLUSIVE,
        "p95_overhead_percent": round(p95, 3),
        "passed": p95 < THRESHOLD_PERCENT_EXCLUSIVE,
        "allowed_differences": ALLOWED_DIFFERENCES,
        "pairs": pairs,
    }


@functools.cache
def _result_schema() -> dict[str, Any]:
    schema = benchmark_publication.strict_json_loads(RESULT_SCHEMA.read_bytes(), "benchmark result schema")
    if not isinstance(schema, dict):
        raise benchmark_publication.PublicationError("benchmark result schema must be a JSON object")
    return schema


def _record_violations(record: Any, path: str) -> list[str]:
    if not isinstance(record, dict):
        return [f"{path} must be a full broker JSON object"]
    schema = _result_schema()
    failures: list[validate_result.Violation] = []
    validate_result.validate(record, schema["definitions"]["resource_usage"], path, failures, schema)
    if failures:
        return [str(failure) for failure in failures]
    diagnostics = record["sampled_diagnostics"]
    sample = {
        "resource_usage": record,
        "exit_code": record["exit_code"],
        "signal": record["signal"],
        "wall_ms": record["tree_wall_ms"],
        "user_ms": record["cpu_user_ms"],
        "sys_ms": record["cpu_sys_ms"],
        "memory_current_bytes": record["memory_current_bytes"],
        "memory_peak_bytes": record["memory_peak_bytes"],
        "read_bytes": record["read_bytes"],
        "write_bytes": record["write_bytes"],
        "read_operations": record["read_operations"],
        "write_operations": record["write_operations"],
        "measurement_method": record["method"],
        "sequential_sampled_peak_rss_kib_diagnostic": diagnostics["sequential_sampled_peak_summed_rss_kib_diagnostic"],
        "sequential_sampled_peak_pss_kib_diagnostic": diagnostics["sequential_sampled_peak_summed_pss_kib_diagnostic"],
        "ok": record["exit_code"] == 0 and record["signal"] is None,
    }
    semantic_failures: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(sample, path, semantic_failures)
    if sys.platform == "win32":
        semantic_failures = [
            failure
            for failure in semantic_failures
            if not (
                failure.path.endswith(".launch_security.executable.path")
                and failure.message == "must be an absolute canonical path"
            )
        ]
    failures.extend(semantic_failures)
    identity = record["launch_security"]["command_identity"]
    argv = record["launch_security"]["argv"]
    try:
        if argv.count("--output") != 1 or argv.count("--artifacts") != 1:
            raise ValueError("raw argv must contain one output and artifact option")
        output = Path(argv[argv.index("--output") + 1])
        artifacts = Path(argv[argv.index("--artifacts") + 1])
        workspace = Path(identity["workspace_directory"]["path"])
        recorded_output = Path(identity["output_directory"]["path"])
        recorded_artifacts = Path(identity["artifact_directory"]["path"])
        if output.parent != recorded_output or artifacts != recorded_artifacts:
            failures.append(validate_result.Violation(path, "raw argv differs from its FD-bound output/artifact paths"))
        if recorded_output.parent != workspace or recorded_artifacts.parent != workspace:
            failures.append(
                validate_result.Violation(path, "raw output/artifact directories are not workspace siblings")
            )
        identities = {
            (identity[label]["device"], identity[label]["inode"])
            for label in ("cwd", "input", "workspace_directory", "output_directory", "artifact_directory")
        }
        if len(identities) != 5:
            failures.append(validate_result.Violation(path, "raw FD-bound command identities are not disjoint"))
    except (KeyError, ValueError, IndexError, TypeError) as error:
        failures.append(validate_result.Violation(path, f"raw command identity is malformed: {error}"))
    return [str(failure) for failure in failures]


def _normalized_violations(contract: Any, path: str) -> list[str]:
    violations: list[str] = []
    if not isinstance(contract, dict):
        return [f"{path} must be an object"]
    try:
        boundary = contract["cgroup_boundary"]
        launch = contract["launch_security"]
        status = launch["status"]
        frozen = contract["frozen_command"]
        outcome = contract["outcome"]
        controls = contract["static_controls"]
        expected_top = {"cgroup_boundary", "launch_security", "frozen_command", "outcome", "static_controls"}
        if set(contract) != expected_top:
            violations.append(f"{path} keys must be exact")
        if set(boundary) != {"method", "scope", "delegated_parent"}:
            violations.append(f"{path}.cgroup_boundary keys must be exact")
        expected_launch = {
            "account",
            "uid",
            "gid",
            "status",
            "execution_binding",
            "parent_death_signal",
            "network_namespace_isolated",
            "migration_write_probes",
            "migration_fd_probes",
            "executable",
        }
        if set(launch) != expected_launch:
            violations.append(f"{path}.launch_security keys must be exact")
        expected_status = {
            "uid",
            "gid",
            "supplementary_groups",
            "cap_inheritable_hex",
            "cap_permitted_hex",
            "cap_effective_hex",
            "cap_bounding_hex",
            "cap_ambient_hex",
            "no_new_privs",
        }
        if set(status) != expected_status:
            violations.append(f"{path}.launch_security.status keys must be exact")
        if boundary != {
            "method": "linux-cgroup-v2-v2",
            "scope": "fresh-root-owned-cgroup-subtree",
            "delegated_parent": boundary.get("delegated_parent"),
        } or not re.fullmatch(r"/[^\0]+", str(boundary.get("delegated_parent", ""))):
            violations.append(f"{path}.cgroup_boundary is invalid")
        if launch.get("account") != "pliego-benchmark-engine" or launch.get("uid", 0) <= 0 or launch.get("gid", 0) <= 0:
            violations.append(f"{path}.launch_security account/IDs are invalid")
        if status.get("uid") != [launch.get("uid")] * 4 or status.get("gid") != [launch.get("gid")] * 4:
            violations.append(f"{path}.launch_security.status IDs differ from the launch IDs")
        if status.get("supplementary_groups") != [] or status.get("no_new_privs") != 1:
            violations.append(f"{path}.launch_security.status privilege removal is invalid")
        for name in (
            "cap_inheritable_hex",
            "cap_permitted_hex",
            "cap_effective_hex",
            "cap_bounding_hex",
            "cap_ambient_hex",
        ):
            if int(status.get(name, "1"), 16) != 0:
                violations.append(f"{path}.launch_security.status.{name} must be zero")
        if (
            launch.get("execution_binding") != "sealed-memfd-execveat"
            or launch.get("parent_death_signal") != 9
            or launch.get("network_namespace_isolated") is not True
        ):
            violations.append(f"{path}.launch_security execution isolation is invalid")
        for probe_name in ("migration_write_probes", "migration_fd_probes"):
            probes = launch.get(probe_name, {})
            if set(probes) != {"parent", "harness", "staging", "measurement"}:
                violations.append(f"{path}.launch_security.{probe_name} labels are invalid")
            for results in probes.values():
                if set(results) != {"cgroup.procs", "cgroup.threads"} or any(
                    result not in {"EACCES", "EPERM"} for result in results.values()
                ):
                    violations.append(f"{path}.launch_security.{probe_name} omitted a denial")
        executable = launch["executable"]
        if set(executable) != {"path", "sha256", "device", "inode", "bytes"}:
            violations.append(f"{path}.launch_security.executable keys must be exact")
        if (
            not isinstance(executable.get("path"), str)
            or not executable["path"].startswith("/")
            or re.fullmatch(r"[0-9a-f]{64}", str(executable.get("sha256"))) is None
            or not isinstance(executable.get("device"), int)
            or executable["device"] < 0
            or executable.get("inode", 0) <= 0
            or executable.get("bytes", 0) <= 0
        ):
            violations.append(f"{path}.launch_security.executable is invalid")
        cwd = frozen["cwd"]
        input_identity = frozen["input"]
        argv = frozen["argv"]
        if (
            set(frozen) != {"argv", "cwd", "input"}
            or set(cwd) != {"path", "device", "inode"}
            or set(input_identity) != {"manifest_path", "argv_relative_path", "sha256", "device", "inode"}
        ):
            violations.append(f"{path}.frozen_command keys must be exact")
        if (
            not isinstance(argv, list)
            or any(not isinstance(value, str) for value in argv)
            or len(argv) < 7
            or argv[0] != launch["executable"]["path"]
            or argv[1] != "render"
            or argv[2] != input_identity.get("argv_relative_path")
            or argv.count("--output") != 1
            or argv[argv.index("--output") + 1] != "<fresh-output-directory>/document.pdf"
            or argv.count("--artifacts") != 1
            or argv[argv.index("--artifacts") + 1] != "<fresh-artifact-directory>"
        ):
            violations.append(f"{path}.frozen_command.argv is invalid")
        if (
            not str(input_identity.get("manifest_path", "")).startswith(str(cwd.get("path", "")) + "/")
            or input_identity.get("argv_relative_path") != Path(input_identity.get("manifest_path", "")).name
            or re.fullmatch(r"[0-9a-f]{64}", str(input_identity.get("sha256"))) is None
            or not isinstance(cwd.get("device"), int)
            or cwd["device"] < 0
            or not isinstance(input_identity.get("device"), int)
            or input_identity["device"] < 0
            or cwd.get("inode", 0) <= 0
            or input_identity.get("inode", 0) <= 0
        ):
            violations.append(f"{path}.frozen_command is invalid")
        if set(outcome) != {"exit_code", "signal", "engine_stdout", "engine_stderr"} or outcome != {
            "exit_code": 0,
            "signal": None,
            "engine_stdout": "",
            "engine_stderr": "",
        }:
            violations.append(f"{path}.outcome must be one quiet successful execution")
        if (
            controls.get("cleanup_kill_used") is not False
            or controls.get("cleanup_kill_grace_ms") != 1000.0
            or controls.get("cleanup_lingering_before_kill") != []
            or controls.get("accounting_poll_interval_ms", 0) <= 0
            or controls.get("empty_cgroup_events") != {"populated": 0, "frozen": 0}
            or controls.get("after_join_cgroup_events") != {"populated": 1, "frozen": 0}
            or controls.get("final_cgroup_events") != {"populated": 0, "frozen": 0}
        ):
            violations.append(f"{path}.static_controls are invalid")
        if set(controls) != {
            "cleanup_kill_used",
            "cleanup_kill_grace_ms",
            "cleanup_lingering_before_kill",
            "accounting_poll_interval_ms",
            "empty_cgroup_events",
            "after_join_cgroup_events",
            "final_cgroup_events",
        }:
            violations.append(f"{path}.static_controls keys must be exact")
    except (IndexError, KeyError, TypeError, ValueError) as error:
        violations.append(f"{path} is malformed: {error}")
    return violations


def validate_measurements(document: Any) -> list[str]:
    violations: list[str] = []
    if not isinstance(document, dict):
        return ["measurements must be a JSON object"]
    try:
        benchmark_publication.require_finite_json(document)
    except ValueError as error:
        return [f"measurements strict numeric contract failed: {error}"]
    expected_keys = {
        "schema",
        "version",
        "generated_at",
        "method",
        "seed",
        "pair_count",
        "execution_count",
        "workload_duration_seconds",
        "percentile_method",
        "threshold_percent_exclusive",
        "p95_overhead_percent",
        "passed",
        "allowed_differences",
        "pairs",
    }
    if set(document) != expected_keys:
        violations.append("measurement keys must be exact")
    for field, expected in (
        ("schema", "pliego.observer-ab-measurements"),
        ("version", 2),
        ("method", "randomized-paired-full-broker-record-v4"),
        ("seed", SEED),
        ("pair_count", PAIR_COUNT),
        ("execution_count", PAIR_COUNT * 2),
        ("workload_duration_seconds", 1.5),
        ("percentile_method", validate_result.PERCENTILE_METHOD),
        ("threshold_percent_exclusive", THRESHOLD_PERCENT_EXCLUSIVE),
        ("allowed_differences", ALLOWED_DIFFERENCES),
    ):
        if document.get(field) != expected:
            violations.append(f"measurements.{field} must equal {expected!r}")
    pairs = document.get("pairs")
    if not isinstance(pairs, list) or len(pairs) != PAIR_COUNT:
        return [*violations, f"measurements.pairs must contain exactly {PAIR_COUNT} pairs"]
    rng = random.Random(SEED)
    overheads: list[float] = []
    cgroup_paths: set[str] = set()
    process_identities: set[tuple[int, int]] = set()
    directory_paths: set[str] = set()
    directory_fd_identities: set[tuple[int, int]] = set()

    def unique(value: Any, seen: set[Any], label: str, path: str) -> None:
        try:
            duplicate = value in seen
        except TypeError:
            violations.append(f"{path}.{label} is not a hashable identity")
            return
        if duplicate:
            violations.append(f"{path}.{label} was replayed across the 40 executions")
        seen.add(value)

    for index, pair in enumerate(pairs):
        path = f"measurements.pairs[{index}]"
        expected_order = ["observation-off", "observation-on"]
        rng.shuffle(expected_order)
        if not isinstance(pair, dict) or pair.get("index") != index or pair.get("order") != expected_order:
            violations.append(f"{path} index/order differs from the seeded protocol")
            continue
        if set(pair) != {
            "index",
            "order",
            "observation_off_record",
            "observation_on_record",
            "overhead_percent",
            "sampler_cpu_percent_of_tree_wall",
        }:
            violations.append(f"{path} keys must be exact")
        off_record = pair.get("observation_off_record")
        on_record = pair.get("observation_on_record")
        record_failures = [
            *_record_violations(off_record, f"{path}.observation_off_record"),
            *_record_violations(on_record, f"{path}.observation_on_record"),
        ]
        violations.extend(record_failures)
        if record_failures:
            continue
        assert isinstance(off_record, dict) and isinstance(on_record, dict)
        off = observation(off_record)
        on = observation(on_record)
        if off.get("observation_enabled") is not False or on.get("observation_enabled") is not True:
            violations.append(f"{path} observation modes are invalid")
        off_contract = normalized_contract(off_record)
        on_contract = normalized_contract(on_record)
        violations.extend(_normalized_violations(off_contract, f"{path}.normalized_off"))
        violations.extend(_normalized_violations(on_contract, f"{path}.normalized_on"))
        if off_contract != on_contract:
            violations.append(f"{path} normalized security/control contracts differ")
        for mode, current in (("off", off), ("on", on)):
            execution_path = f"{path}.{mode}"
            unique(current["cgroup_path"], cgroup_paths, "cgroup_path", execution_path)
            unique(
                (current["root_pid"], current["root_start_ticks"]),
                process_identities,
                "root_process_identity",
                execution_path,
            )
            for label in ("workspace_directory", "output_directory", "artifact_directory"):
                directory = current[label]
                unique(directory["path"], directory_paths, f"{label}.path", execution_path)
                unique(
                    (directory["device"], directory["inode"]),
                    directory_fd_identities,
                    f"{label}.fd_identity",
                    execution_path,
                )
        try:
            off_ms = float(off["tree_wall_ms"])
            on_ms = float(on["tree_wall_ms"])
            if not math.isfinite(off_ms) or not math.isfinite(on_ms) or off_ms <= 0 or on_ms <= 0:
                raise ValueError("tree walls must be positive and finite")
            overhead = (on_ms - off_ms) * 100.0 / off_ms
            sampler = float(on["sampler_cpu_ms"]) * 100.0 / on_ms
            retained_overhead = float(pair["overhead_percent"])
            retained_sampler = float(pair["sampler_cpu_percent_of_tree_wall"])
            if not all(math.isfinite(value) for value in (overhead, sampler, retained_overhead, retained_sampler)):
                raise ValueError("derived observer ratios must be finite")
            if sampler < 0:
                raise ValueError("sampler CPU ratio must be non-negative")
            if abs(retained_overhead - overhead) > 1e-9:
                violations.append(f"{path}.overhead_percent is not derived from retained walls")
            if abs(retained_sampler - sampler) > 1e-9:
                violations.append(f"{path}.sampler_cpu_percent_of_tree_wall is not derived")
            overheads.append(overhead)
        except (KeyError, TypeError, ValueError, ZeroDivisionError) as error:
            violations.append(f"{path} timing evidence is invalid: {error}")
    if len(overheads) != PAIR_COUNT:
        violations.append("measurements did not retain 20 finite derived overheads")
        return violations
    p95 = validate_result.percentile(overheads, 95)
    if not math.isfinite(p95):
        violations.append("measurements derived a non-finite p95")
        return violations
    if document.get("p95_overhead_percent") != round(p95, 3):
        violations.append("measurements.p95_overhead_percent is not the retained nearest-rank p95")
    if document.get("passed") is not (p95 < THRESHOLD_PERCENT_EXCLUSIVE):
        violations.append("measurements.passed differs from the exclusive p95 threshold")
    if p95 >= THRESHOLD_PERCENT_EXCLUSIVE:
        violations.append("observer p95 overhead must remain below 2 percent")
    return violations


def load_host_proof(path: Path, *, allow_fixture_evidence: bool) -> tuple[dict[str, Any], str]:
    proof, _ = load_bound_object(path, "host proof")
    schema = benchmark_publication.strict_json_loads(
        validate_host_proof.DEFAULT_SCHEMA.read_bytes(), "host-proof schema"
    )
    argv = proof.get("command", {}).get("argv")
    if not isinstance(argv, list) or not validate_host_proof.canonical_benchmark_command(argv):
        raise benchmark_publication.PublicationError("host proof omitted the canonical full benchmark command")
    violations = validate_host_proof.validate_document(
        proof,
        schema,
        path.parent,
        allow_fixture_evidence=allow_fixture_evidence,
        expected_command=tuple(argv),
    )
    if violations:
        raise benchmark_publication.PublicationError(f"host proof is invalid: {violations[0]}")
    if proof.get("status") != "accepted" or proof.get("mode") != "production":
        raise benchmark_publication.PublicationError("observer proof requires an accepted production host proof")
    return proof, benchmark_publication.host_proof_bundle_digest(path)


def expected_binding(proof: dict[str, Any], proof_digest: str, source: Path, installed: Path) -> dict[str, Any]:
    source_digest = benchmark_publication.sha256_file(source)
    installed_digest = benchmark_publication.sha256_file(installed)
    if source_digest != installed_digest:
        raise benchmark_publication.PublicationError("installed broker differs from the host-bound source broker")
    return {
        "harness_revision": proof["identity"]["sha"],
        "source_broker_sha256": source_digest,
        "installed_broker_sha256": installed_digest,
        "host_config_sha256": proof["host_config"]["sha256"],
        "host_proof_bundle_sha256": proof_digest,
        "runner_id": proof["runner"]["id"],
        "runner_name": proof["runner"]["name"],
    }


def build_proof(
    measurements_path: Path,
    host_proof_path: Path,
    source_broker: Path,
    installed_broker: Path,
    *,
    allow_fixture_evidence: bool = False,
) -> dict[str, Any]:
    measurements, measurements_binding = load_bound_object(measurements_path, "observer measurements")
    measurement_violations = validate_measurements(measurements)
    if measurement_violations:
        raise benchmark_publication.PublicationError(f"observer measurements are invalid: {measurement_violations[0]}")
    proof, proof_digest = load_host_proof(host_proof_path, allow_fixture_evidence=allow_fixture_evidence)
    measurements_binding = finalize_measurements_binding(measurements, measurements_binding)
    expected_measurements = proof["command"].get("observer_measurements")
    if measurements_binding != expected_measurements:
        raise benchmark_publication.PublicationError("observer measurements differ from the host-bound digest")
    return {
        "schema": "pliego.benchmark-observer-proof",
        "version": 1,
        "status": "accepted",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "binding": expected_binding(proof, proof_digest, source_broker, installed_broker),
        "measurements_binding": measurements_binding,
        "measurements": measurements,
    }


def validate_proof(
    document: dict[str, Any],
    schema: dict[str, Any],
    host_proof_path: Path,
    source_broker: Path,
    installed_broker: Path,
    *,
    allow_fixture_evidence: bool = False,
) -> list[str]:
    schema_violations: list[validate_result.Violation] = []
    validate_result.validate(document, schema, "$", schema_violations)
    if schema_violations:
        return [str(violation) for violation in schema_violations]
    violations = validate_measurements(document["measurements"])
    if (
        hashlib.sha256(canonical_bytes(document["measurements"])).hexdigest()
        != document["measurements_binding"]["canonical_sha256"]
    ):
        violations.append("measurements canonical digest differs from the embedded document")
    try:
        proof, proof_digest = load_host_proof(host_proof_path, allow_fixture_evidence=allow_fixture_evidence)
        binding = expected_binding(proof, proof_digest, source_broker, installed_broker)
    except (OSError, benchmark_publication.PublicationError) as error:
        violations.append(str(error))
        return violations
    if document["binding"] != binding:
        violations.append("observer proof binding differs from the external host proof/broker identities")
    if document["measurements_binding"] != proof["command"].get("observer_measurements"):
        violations.append("observer measurement binding differs from the host proof command")
    return violations


def finalize_measurements_binding(document: dict[str, Any], file_binding: dict[str, Any]) -> dict[str, Any]:
    return {
        **file_binding,
        "canonical_sha256": hashlib.sha256(canonical_bytes(document)).hexdigest(),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    bind = subparsers.add_parser("bind")
    bind.add_argument("--measurements", required=True, type=Path)
    bind.add_argument("--host-proof", required=True, type=Path)
    bind.add_argument("--source-broker", type=Path, default=SOURCE_BROKER)
    bind.add_argument("--installed-broker", type=Path, default=INSTALLED_BROKER)
    bind.add_argument("--out", required=True, type=Path)
    verify = subparsers.add_parser("validate")
    verify.add_argument("proof", type=Path)
    verify.add_argument("--host-proof", required=True, type=Path)
    verify.add_argument("--source-broker", type=Path, default=SOURCE_BROKER)
    verify.add_argument("--installed-broker", type=Path, default=INSTALLED_BROKER)
    args = parser.parse_args()
    try:
        if args.command == "bind":
            benchmark_publication.require_unprivileged()
            document = build_proof(args.measurements, args.host_proof, args.source_broker, args.installed_broker)
            schema = benchmark_publication.strict_json_loads(DEFAULT_SCHEMA.read_bytes(), "observer-proof schema")
            violations = validate_proof(
                document,
                schema,
                args.host_proof,
                args.source_broker,
                args.installed_broker,
            )
            if violations:
                raise benchmark_publication.PublicationError(f"observer proof is invalid: {violations[0]}")
            benchmark_publication.atomic_write_bytes(
                args.out,
                benchmark_publication.json_bytes(document, indent=2),
            )
            print(f"bound observer proof {args.out}")
            return 0
        document, _ = load_bound_object(args.proof, "observer proof")
        schema = benchmark_publication.strict_json_loads(DEFAULT_SCHEMA.read_bytes(), "observer-proof schema")
        violations = validate_proof(
            document,
            schema,
            args.host_proof,
            args.source_broker,
            args.installed_broker,
        )
        if violations:
            for violation in violations:
                print(f"observer proof violation: {violation}", file=sys.stderr)
            return 1
        print("observer proof validated")
        return 0
    except (OSError, benchmark_publication.PublicationError) as error:
        print(f"observer_ab: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
