#!/usr/bin/env python3

"""Build and verify the host-bound 20-pair procfs observer A/B proof."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import re
import stat
import sys
from datetime import datetime, timezone
from pathlib import Path
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
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def load_bound_object(path: Path, label: str) -> tuple[dict[str, Any], dict[str, Any]]:
    resolved = path.resolve(strict=True)
    if path != resolved or path.is_symlink():
        raise benchmark_publication.PublicationError(f"{label} must be one canonical non-symlink file")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size <= 0 or before.st_size > MAX_DOCUMENT_BYTES:
            raise benchmark_publication.PublicationError(f"{label} must be a bounded nonempty regular file")
        raw = bytearray()
        while len(raw) < before.st_size:
            chunk = os.read(descriptor, min(1024 * 1024, before.st_size - len(raw)))
            if not chunk:
                raise benchmark_publication.PublicationError(f"{label} shortened while binding")
            raw.extend(chunk)
        after = os.fstat(descriptor)
        current = os.stat(path, follow_symlinks=False)
        identity = (before.st_dev, before.st_ino, before.st_size)
        if identity != (after.st_dev, after.st_ino, after.st_size) or identity != (
            current.st_dev,
            current.st_ino,
            current.st_size,
        ):
            raise benchmark_publication.PublicationError(f"{label} identity changed while binding")
    finally:
        os.close(descriptor)
    try:
        document = json.loads(raw)
    except json.JSONDecodeError as error:
        raise benchmark_publication.PublicationError(f"{label} is invalid JSON: {error}") from error
    if not isinstance(document, dict):
        raise benchmark_publication.PublicationError(f"{label} must be a JSON object")
    return document, {"path": str(resolved), "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}


def normalized_contract(proof: dict[str, Any]) -> dict[str, Any]:
    launch = proof["launch_security"]
    identity = launch["command_identity"]
    counters = proof["counters"]
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
            "cwd": identity["cwd"],
            "input": identity["input"],
        },
        "static_controls": {
            "cleanup_kill_used": proof["cleanup"]["kill_used"],
            "cleanup_kill_grace_ms": proof["cleanup"]["kill_grace_ms"],
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
        "observation_off": off_observation,
        "observation_on": on_observation,
        "normalized_off": off_contract,
        "normalized_on": on_contract,
        "overhead_percent": (on_ms - off_ms) * 100.0 / off_ms,
        "sampler_cpu_percent_of_tree_wall": sampler_cpu_ms * 100.0 / on_ms,
    }


def build_measurements(pairs: list[dict[str, Any]], *, duration_seconds: float) -> dict[str, Any]:
    overhead = [float(pair["overhead_percent"]) for pair in pairs]
    p95 = validate_result.percentile(overhead, 95)
    return {
        "schema": "pliego.observer-ab-measurements",
        "version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "method": "randomized-paired-observation-v3",
        "seed": SEED,
        "pair_count": PAIR_COUNT,
        "workload_duration_seconds": duration_seconds,
        "percentile_method": validate_result.PERCENTILE_METHOD,
        "threshold_percent_exclusive": THRESHOLD_PERCENT_EXCLUSIVE,
        "p95_overhead_percent": round(p95, 3),
        "passed": p95 < THRESHOLD_PERCENT_EXCLUSIVE,
        "allowed_differences": ALLOWED_DIFFERENCES,
        "pairs": pairs,
    }


def _normalized_violations(contract: Any, path: str) -> list[str]:
    violations: list[str] = []
    if not isinstance(contract, dict):
        return [f"{path} must be an object"]
    try:
        boundary = contract["cgroup_boundary"]
        launch = contract["launch_security"]
        status = launch["status"]
        frozen = contract["frozen_command"]
        controls = contract["static_controls"]
        expected_top = {"cgroup_boundary", "launch_security", "frozen_command", "static_controls"}
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
        if (
            set(frozen) != {"cwd", "input"}
            or set(cwd) != {"path", "device", "inode"}
            or set(input_identity) != {"manifest_path", "argv_relative_path", "sha256", "device", "inode"}
        ):
            violations.append(f"{path}.frozen_command keys must be exact")
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
        if (
            controls.get("cleanup_kill_used") is not False
            or controls.get("cleanup_kill_grace_ms") != 1000.0
            or controls.get("accounting_poll_interval_ms", 0) <= 0
            or controls.get("empty_cgroup_events") != {"populated": 0, "frozen": 0}
            or controls.get("after_join_cgroup_events") != {"populated": 1, "frozen": 0}
            or controls.get("final_cgroup_events") != {"populated": 0, "frozen": 0}
        ):
            violations.append(f"{path}.static_controls are invalid")
        if set(controls) != {
            "cleanup_kill_used",
            "cleanup_kill_grace_ms",
            "accounting_poll_interval_ms",
            "empty_cgroup_events",
            "after_join_cgroup_events",
            "final_cgroup_events",
        }:
            violations.append(f"{path}.static_controls keys must be exact")
    except (KeyError, TypeError, ValueError) as error:
        violations.append(f"{path} is malformed: {error}")
    return violations


def validate_measurements(document: Any) -> list[str]:
    violations: list[str] = []
    if not isinstance(document, dict):
        return ["measurements must be a JSON object"]
    expected_keys = {
        "schema",
        "version",
        "generated_at",
        "method",
        "seed",
        "pair_count",
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
        ("version", 1),
        ("method", "randomized-paired-observation-v3"),
        ("seed", SEED),
        ("pair_count", PAIR_COUNT),
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
            "observation_off",
            "observation_on",
            "normalized_off",
            "normalized_on",
            "overhead_percent",
            "sampler_cpu_percent_of_tree_wall",
        }:
            violations.append(f"{path} keys must be exact")
        off = pair.get("observation_off", {})
        on = pair.get("observation_on", {})
        observation_keys = {
            "observation_enabled",
            "tree_wall_ms",
            "sampler_cpu_ms",
            "cgroup_path",
            "root_pid",
            "root_start_ticks",
            "output_directory",
            "artifact_directory",
        }
        if (
            not isinstance(off, dict)
            or not isinstance(on, dict)
            or set(off) != observation_keys
            or set(on) != observation_keys
        ):
            violations.append(f"{path} observation keys must be exact")
            continue
        if off.get("observation_enabled") is not False or on.get("observation_enabled") is not True:
            violations.append(f"{path} observation modes are invalid")
        if off.get("cgroup_path") == on.get("cgroup_path"):
            violations.append(f"{path} did not use fresh cgroup identities")
        if off.get("root_pid") == on.get("root_pid") and off.get("root_start_ticks") == on.get("root_start_ticks"):
            violations.append(f"{path} did not use fresh process identities")
        if off.get("output_directory") == on.get("output_directory") or off.get("artifact_directory") == on.get(
            "artifact_directory"
        ):
            violations.append(f"{path} did not use fresh workspace identities")
        off_contract = pair.get("normalized_off")
        on_contract = pair.get("normalized_on")
        violations.extend(_normalized_violations(off_contract, f"{path}.normalized_off"))
        violations.extend(_normalized_violations(on_contract, f"{path}.normalized_on"))
        if off_contract != on_contract:
            violations.append(f"{path} normalized security/control contracts differ")
        try:
            off_ms = float(off["tree_wall_ms"])
            on_ms = float(on["tree_wall_ms"])
            overhead = (on_ms - off_ms) * 100.0 / off_ms
            sampler = float(on["sampler_cpu_ms"]) * 100.0 / on_ms
            if abs(float(pair["overhead_percent"]) - overhead) > 1e-9:
                violations.append(f"{path}.overhead_percent is not derived from retained walls")
            if abs(float(pair["sampler_cpu_percent_of_tree_wall"]) - sampler) > 1e-9:
                violations.append(f"{path}.sampler_cpu_percent_of_tree_wall is not derived")
            overheads.append(overhead)
        except (KeyError, TypeError, ValueError, ZeroDivisionError) as error:
            violations.append(f"{path} timing evidence is invalid: {error}")
    p95 = validate_result.percentile(overheads, 95)
    if document.get("p95_overhead_percent") != round(p95, 3):
        violations.append("measurements.p95_overhead_percent is not the retained nearest-rank p95")
    if document.get("passed") is not (p95 < THRESHOLD_PERCENT_EXCLUSIVE):
        violations.append("measurements.passed differs from the exclusive p95 threshold")
    if p95 >= THRESHOLD_PERCENT_EXCLUSIVE:
        violations.append("observer p95 overhead must remain below 2 percent")
    return violations


def load_host_proof(path: Path, *, allow_fixture_evidence: bool) -> tuple[dict[str, Any], str]:
    proof, _ = load_bound_object(path, "host proof")
    schema = json.loads(validate_host_proof.DEFAULT_SCHEMA.read_text(encoding="utf-8"))
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
            schema = json.loads(DEFAULT_SCHEMA.read_text(encoding="utf-8"))
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
                json.dumps(document, indent=2, ensure_ascii=False).encode("utf-8") + b"\n",
            )
            print(f"bound observer proof {args.out}")
            return 0
        document, _ = load_bound_object(args.proof, "observer proof")
        schema = json.loads(DEFAULT_SCHEMA.read_text(encoding="utf-8"))
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
