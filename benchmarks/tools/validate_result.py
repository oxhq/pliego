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

Validation is authority-aware: staged results require the exact benchmark
command context, while published results additionally require the external
dedicated-host proof bundle that authorized publication.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import stat
import statistics
import sys
import tomllib
from collections import Counter
from dataclasses import dataclass
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


@dataclass(frozen=True)
class ExternalPublication:
    host_proof_schema: str
    host_proof_bundle_sha256: str
    harness_revision: str
    runner_id: int
    runner_name: str

    def document(self) -> dict[str, Any]:
        return {
            "host_proof_schema": self.host_proof_schema,
            "host_proof_bundle_sha256": self.host_proof_bundle_sha256,
            "harness_revision": self.harness_revision,
            "runner_id": self.runner_id,
            "runner_name": self.runner_name,
        }


@dataclass(frozen=True)
class ValidationContext:
    mode: str
    repository_root: Path
    manifest_path: Path
    frozen_fixture_root: Path
    staging_output: Path
    failure_evidence_root: Path
    engine_binary: Path
    harness_revision: str
    fixtures: dict[str, dict[str, Any]]
    external_publication: ExternalPublication | None = None


def _canonical_path(path: Path, label: str, *, strict: bool) -> Path:
    try:
        resolved = path.resolve(strict=strict)
    except OSError as error:
        raise ValueError(f"{label} cannot be resolved: {error}") from error
    if not path.is_absolute() or path != resolved:
        raise ValueError(f"{label} must be absolute and canonical")
    return resolved


def _overlap(first: Path, second: Path) -> bool:
    return first == second or first in second.parents or second in first.parents


def build_validation_context(
    *,
    mode: str,
    repository_root: Path,
    manifest_path: Path,
    frozen_fixture_root: Path,
    staging_output: Path,
    failure_evidence_root: Path,
    engine_binary: Path,
    harness_revision: str,
    external_publication: ExternalPublication | None = None,
) -> ValidationContext:
    """Bind result validation to the already-authorized benchmark command."""

    if mode not in {"staged", "published"}:
        raise ValueError("validation mode must be staged or published")
    if (mode == "published") is not (external_publication is not None):
        raise ValueError("published mode requires exactly one external publication proof")
    if re.fullmatch(r"[0-9a-f]{40}", harness_revision) is None:
        raise ValueError("validation context requires one exact 40-character revision")
    repository = _canonical_path(repository_root, "repository root", strict=True)
    manifest = _canonical_path(manifest_path, "benchmark manifest", strict=True)
    if manifest != repository / "benchmarks" / "manifest.toml":
        raise ValueError("benchmark manifest must be the canonical repository manifest")
    frozen = _canonical_path(frozen_fixture_root, "frozen fixture root", strict=True)
    staging = _canonical_path(staging_output, "staging output", strict=False)
    failures = _canonical_path(failure_evidence_root, "failure-evidence root", strict=True)
    binary = _canonical_path(engine_binary, "engine binary", strict=True)
    for first, second, label in (
        (frozen, staging, "frozen fixture root and staging output"),
        (frozen, failures, "frozen fixture root and failure-evidence root"),
        (staging, failures, "staging output and failure-evidence root"),
    ):
        if _overlap(first, second):
            raise ValueError(f"{label} must be disjoint")
    if not frozen.is_dir() or not failures.is_dir() or not binary.is_file():
        raise ValueError("validation context paths do not identify the required directory/file types")
    try:
        manifest_document = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"benchmark manifest is unreadable: {error}") from error
    fixtures = manifest_document.get("fixtures")
    if not isinstance(fixtures, dict):
        raise ValueError("benchmark manifest omitted its fixture contracts")
    if external_publication is not None and external_publication.harness_revision != harness_revision:
        raise ValueError("external publication proof revision differs from the benchmark command")
    return ValidationContext(
        mode=mode,
        repository_root=repository,
        manifest_path=manifest,
        frozen_fixture_root=frozen,
        staging_output=staging,
        failure_evidence_root=failures,
        engine_binary=binary,
        harness_revision=harness_revision,
        fixtures=fixtures,
        external_publication=external_publication,
    )


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


def _bound_regular_file(path: Path) -> tuple[str, os.stat_result]:
    if path != path.resolve(strict=True) or path.is_symlink():
        raise ValueError(f"bound file is not canonical and non-following: {path}")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    descriptor = os.open(path, flags)
    digest = hashlib.sha256()
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise ValueError(f"bound path is not a regular file: {path}")
        while chunk := os.read(descriptor, 1024 * 1024):
            digest.update(chunk)
        after = os.fstat(descriptor)
        current = os.stat(path, follow_symlinks=False)
    finally:
        os.close(descriptor)
    identity = (before.st_dev, before.st_ino, before.st_size)
    if identity != (after.st_dev, after.st_ino, after.st_size) or identity != (
        current.st_dev,
        current.st_ino,
        current.st_size,
    ):
        raise ValueError(f"bound file identity changed while hashing: {path}")
    return digest.hexdigest(), before


def _manifest_relative_path(value: str, label: str) -> Path:
    relative = PurePosixPath(value)
    if relative.is_absolute() or not relative.parts or any(part in {"", ".", ".."} for part in relative.parts):
        raise ValueError(f"{label} is not one safe manifest-relative path")
    return Path(*relative.parts)


def _bundle_digest(paths: list[tuple[str, str]]) -> str:
    digest = hashlib.sha256()
    for relative, file_digest in sorted(paths):
        digest.update(relative.encode("utf-8") + b"\0")
        digest.update(bytes.fromhex(file_digest))
    return digest.hexdigest()


def _context_violation(path: str, message: str, violations: list[Violation]) -> None:
    violations.append(Violation(path, message))


def validate_fixture_context(
    data: dict[str, Any],
    path: str,
    violations: list[Violation],
    context: ValidationContext,
) -> None:
    fixture = data["fixture"]
    fixture_id = fixture["id"]
    contract = context.fixtures.get(fixture_id)
    if not isinstance(contract, dict):
        _context_violation(f"{path}.fixture.id", "is absent from the frozen benchmark manifest", violations)
        return
    for field in ("purpose", "category", "input"):
        require_equal(f"{path}.fixture.{field}", fixture.get(field), contract.get(field), violations)
    try:
        input_relative = _manifest_relative_path(contract["input"], "fixture input")
        source_input = context.repository_root / input_relative
        frozen_input = context.frozen_fixture_root / input_relative
        assets = [_manifest_relative_path(value, "fixture asset") for value in contract.get("assets", [])]
        source_paths = [source_input, *(source_input.parent / asset for asset in assets)]
        frozen_paths = [frozen_input, *(frozen_input.parent / asset for asset in assets)]
        source_bundle: list[tuple[str, str]] = []
        frozen_bundle: list[tuple[str, str]] = []
        input_metadata: os.stat_result | None = None
        input_digest = ""
        for index, (source, frozen) in enumerate(zip(source_paths, frozen_paths)):
            source_digest, _ = _bound_regular_file(source)
            frozen_digest, frozen_metadata = _bound_regular_file(frozen)
            relative = frozen.relative_to(frozen_input.parent).as_posix()
            source_bundle.append((relative, source_digest))
            frozen_bundle.append((relative, frozen_digest))
            if source_digest != frozen_digest:
                raise ValueError(f"frozen fixture asset differs from committed source: {relative}")
            if index == 0:
                input_digest = frozen_digest
                input_metadata = frozen_metadata
        source_bundle_digest = _bundle_digest(source_bundle)
        frozen_bundle_digest = _bundle_digest(frozen_bundle)
        if source_bundle_digest != frozen_bundle_digest:
            raise ValueError("frozen fixture bundle differs from committed source")
        engine_digest, engine_metadata = _bound_regular_file(context.engine_binary)
        cwd_metadata = os.stat(frozen_input.parent, follow_symlinks=False)
        if not stat.S_ISDIR(cwd_metadata.st_mode) or frozen_input.parent != frozen_input.parent.resolve(strict=True):
            raise ValueError("frozen fixture cwd is not one canonical directory")
        assert input_metadata is not None
    except (KeyError, OSError, ValueError) as error:
        _context_violation(f"{path}.fixture", f"cannot bind frozen fixture context: {error}", violations)
        return
    require_equal(f"{path}.fixture.input_sha256", fixture.get("input_sha256"), input_digest, violations)
    require_equal(f"{path}.fixture.bundle_sha256", fixture.get("bundle_sha256"), frozen_bundle_digest, violations)
    engine = data["toolchain"]["engine"]
    require_equal(
        f"{path}.toolchain.engine.binary_path", engine.get("binary_path"), str(context.engine_binary), violations
    )
    require_equal(f"{path}.toolchain.engine.binary_sha256", engine.get("binary_sha256"), engine_digest, violations)
    if "binary_bytes" in engine:
        require_equal(
            f"{path}.toolchain.engine.binary_bytes", engine["binary_bytes"], engine_metadata.st_size, violations
        )

    for index, sample in enumerate(data["samples"]):
        sample_path = f"{path}.samples[{index}].resource_usage.launch_security"
        usage = sample.get("resource_usage")
        if not isinstance(usage, dict):
            continue
        launch = usage["launch_security"]
        argv = launch["argv"]
        identity = launch["command_identity"]
        malformed_flags = False
        for flag in ("--output", "--artifacts"):
            if argv.count(flag) != 1:
                _context_violation(f"{sample_path}.argv", f"must contain exactly one {flag!r}", violations)
                malformed_flags = True
        if malformed_flags:
            continue
        require_equal(f"{sample_path}.argv[0]", argv[0], str(context.engine_binary), violations)
        require_equal(f"{sample_path}.argv[2]", argv[2], frozen_input.name, violations)
        require_equal(
            f"{sample_path}.executable.path", launch["executable"]["path"], str(context.engine_binary), violations
        )
        require_equal(f"{sample_path}.executable.sha256", launch["executable"]["sha256"], engine_digest, violations)
        for field, expected in (
            ("device", engine_metadata.st_dev),
            ("inode", engine_metadata.st_ino),
            ("bytes", engine_metadata.st_size),
        ):
            require_equal(f"{sample_path}.executable.{field}", launch["executable"][field], expected, violations)
        command_input = identity["input"]
        require_equal(
            f"{sample_path}.command_identity.input.manifest_path",
            command_input["manifest_path"],
            str(frozen_input),
            violations,
        )
        require_equal(
            f"{sample_path}.command_identity.input.argv_relative_path",
            command_input["argv_relative_path"],
            frozen_input.name,
            violations,
        )
        require_equal(f"{sample_path}.command_identity.input.sha256", command_input["sha256"], input_digest, violations)
        for field, expected in (("device", input_metadata.st_dev), ("inode", input_metadata.st_ino)):
            require_equal(f"{sample_path}.command_identity.input.{field}", command_input[field], expected, violations)
        cwd = identity["cwd"]
        require_equal(f"{sample_path}.command_identity.cwd.path", cwd["path"], str(frozen_input.parent), violations)
        for field, expected in (("device", cwd_metadata.st_dev), ("inode", cwd_metadata.st_ino)):
            require_equal(f"{sample_path}.command_identity.cwd.{field}", cwd[field], expected, violations)
        try:
            output_flag = argv.index("--output")
            artifact_flag = argv.index("--artifacts")
            output_file = Path(argv[output_flag + 1])
            artifact_directory = Path(argv[artifact_flag + 1])
        except (ValueError, IndexError):
            _context_violation(f"{sample_path}.argv", "omits its exact output/artifact values", violations)
            continue
        output_directory = output_file.parent
        recorded_output = Path(identity["output_directory"]["path"])
        recorded_artifacts = Path(identity["artifact_directory"]["path"])
        require_equal(
            f"{sample_path}.command_identity.output_directory.path",
            str(recorded_output),
            str(output_directory),
            violations,
        )
        require_equal(
            f"{sample_path}.command_identity.artifact_directory.path",
            str(recorded_artifacts),
            str(artifact_directory),
            violations,
        )
        workspace = output_directory.parent
        if (
            not output_file.is_absolute()
            or not artifact_directory.is_absolute()
            or ".." in output_file.parts
            or ".." in artifact_directory.parts
            or workspace.parent != context.failure_evidence_root
            or re.fullmatch(r"\.in-progress-[1-9][0-9]*-[0-9a-f]{16}", workspace.name) is None
            or artifact_directory.parent != workspace
            or output_directory == artifact_directory
        ):
            _context_violation(
                f"{sample_path}.command_identity",
                "output and artifact directories must be disjoint siblings below the exact failure-evidence root",
                violations,
            )
        prefix = f"sample-{sample['index']}-"
        if (
            re.fullmatch(re.escape(prefix) + r"[0-9a-f]{16}-output", output_directory.name) is None
            or artifact_directory.name != output_directory.name.removesuffix("-output") + "-artifacts"
            or output_file.name != "document.pdf"
        ):
            _context_violation(f"{sample_path}.argv", "does not bind the indexed retained sample paths", violations)
        output_fd_identity = (identity["output_directory"]["device"], identity["output_directory"]["inode"])
        artifact_fd_identity = (identity["artifact_directory"]["device"], identity["artifact_directory"]["inode"])
        bound_identities = {
            (cwd["device"], cwd["inode"]),
            (command_input["device"], command_input["inode"]),
            output_fd_identity,
            artifact_fd_identity,
        }
        if len(bound_identities) != 4:
            _context_violation(
                f"{sample_path}.command_identity",
                "cwd, input, output, and artifact FD identities must be disjoint",
                violations,
            )


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
        "wall_ms": "tree_wall_ms",
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
    for probe_field in ("migration_write_probes", "migration_fd_probes"):
        for label, results in launch[probe_field].items():
            for interface, result in results.items():
                if result not in {"EACCES", "EPERM"}:
                    violations.append(
                        Violation(
                            f"{path}.resource_usage.launch_security.{probe_field}.{label}.{interface}",
                            "must prove a permission-denied write",
                        )
                    )
    executable = launch["executable"]
    executable_path = Path(executable["path"])
    if not executable_path.is_absolute() or ".." in executable_path.parts or str(executable_path) != executable["path"]:
        violations.append(
            Violation(f"{path}.resource_usage.launch_security.executable.path", "must be an absolute canonical path")
        )
    require_equal(f"{path}.resource_usage.launch_security.argv[0]", launch["argv"][0], executable["path"], violations)
    require_equal(f"{path}.resource_usage.launch_security.argv[1]", launch["argv"][1], "render", violations)
    require_equal(
        f"{path}.resource_usage.launch_security.command_identity.input.argv_relative_path",
        launch["command_identity"]["input"]["argv_relative_path"],
        launch["argv"][2],
        violations,
    )
    if not (usage["root_wall_ms"] <= usage["tree_wall_ms"] <= usage["measurement_complete_ms"]):
        violations.append(
            Violation(
                f"{path}.resource_usage.tree_wall_ms",
                "must include root lifetime and precede measurement completion",
            )
        )
    require_equal(
        f"{path}.resource_usage.drain_ms",
        usage["drain_ms"],
        round(usage["tree_wall_ms"] - usage["root_wall_ms"], 3),
        violations,
    )

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
    if usage["measurement_complete_ms"] + 0.002 < usage["tree_wall_ms"] + settle["duration_ms"]:
        violations.append(
            Violation(
                f"{path}.resource_usage.measurement_complete_ms",
                "must include the full post-drain accounting settle interval",
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
    if diagnostics["observation_enabled"] is not bool(diagnostics["samples"]):
        violations.append(
            Violation(
                f"{path}.resource_usage.sampled_diagnostics.observation_enabled",
                "must be true exactly when sequential procfs observations were retained",
            )
        )
    gaps = [later - earlier for earlier, later in zip(elapsed, elapsed[1:])]
    if elapsed and abs(elapsed[-1] - usage["tree_wall_ms"]) > 0.002:
        violations.append(
            Violation(
                f"{path}.resource_usage.sampled_diagnostics.samples[-1].elapsed_ms",
                f"must equal tree wall {usage['tree_wall_ms']!r}",
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
            f"{sample_path}.sequential_sampled_summed_rss_kib_diagnostic",
            raw_sample["sequential_sampled_summed_rss_kib_diagnostic"],
            rss,
            violations,
        )
        rss_peaks.append(rss)
        pss_values = [process["pss_kib"] for process in raw_sample["processes"]]
        expected_pss = (
            sum(pss_values) if raw_sample["processes"] and all(value is not None for value in pss_values) else None
        )
        require_equal(
            f"{sample_path}.sequential_sampled_summed_pss_kib_diagnostic",
            raw_sample["sequential_sampled_summed_pss_kib_diagnostic"],
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
        f"{path}.sequential_sampled_peak_rss_kib_diagnostic",
        sample["sequential_sampled_peak_rss_kib_diagnostic"],
        max(rss_peaks, default=0),
        violations,
    )
    require_equal(
        f"{path}.sequential_sampled_peak_pss_kib_diagnostic",
        sample["sequential_sampled_peak_pss_kib_diagnostic"],
        max(pss_peaks) if pss_peaks else None,
        violations,
    )
    require_equal(
        f"{path}.resource_usage.sampled_diagnostics.sequential_sampled_peak_summed_rss_kib_diagnostic",
        diagnostics["sequential_sampled_peak_summed_rss_kib_diagnostic"],
        max(rss_peaks, default=0),
        violations,
    )
    require_equal(
        f"{path}.resource_usage.sampled_diagnostics.sequential_sampled_peak_summed_pss_kib_diagnostic",
        diagnostics["sequential_sampled_peak_summed_pss_kib_diagnostic"],
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


def validate_semantics(
    data: dict[str, Any],
    path: str,
    violations: list[Violation],
    context: ValidationContext | None,
) -> None:
    if data["schema"] == "pliego.benchmark-bundle":
        for index, result in enumerate(data["results"]):
            validate_semantics(result, f"{path}.results[{index}]", violations, context)
        return

    publication = data.get("publication")
    dedicated = data["host"]["dedicated"]
    if context is None:
        violations.append(
            Violation(
                path,
                "applicable benchmark validation requires a typed staged or external-publication context",
            )
        )
    elif context.mode == "staged":
        if publication is not None or dedicated is not False:
            violations.append(
                Violation(
                    f"{path}.publication",
                    "staged validation forbids dedicated or publication provenance",
                )
            )
    else:
        assert context.external_publication is not None
        require_equal(
            f"{path}.publication",
            publication,
            context.external_publication.document(),
            violations,
        )
        require_equal(f"{path}.host.dedicated", dedicated, True, violations)
    if context is not None:
        require_equal(
            f"{path}.toolchain.harness_revision",
            data["toolchain"].get("harness_revision"),
            context.harness_revision,
            violations,
        )
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
    if protocol_measurement_method != "linux-cgroup-v2-v2":
        violations.append(
            Violation(
                f"{path}.protocol.measurement_method",
                "publishable benchmark results require linux-cgroup-v2-v2",
            )
        )
    engine = data["toolchain"]["engine"]
    harness_revision = data["toolchain"].get("harness_revision")
    if not isinstance(harness_revision, str) or re.fullmatch(r"[0-9a-f]{40}", harness_revision) is None:
        violations.append(Violation(f"{path}.toolchain.harness_revision", "must be one exact 40-character revision"))
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
    if context is not None:
        validate_fixture_context(data, path, violations, context)

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
        f"{path}.aggregates.memory.sequential_sampled_peak_rss_kib_diagnostic": (
            aggregates["memory"]["sequential_sampled_peak_rss_kib_diagnostic"],
            percentiles([sample["sequential_sampled_peak_rss_kib_diagnostic"] for sample in passing_samples]),
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
        sample["sequential_sampled_peak_pss_kib_diagnostic"]
        for sample in passing_samples
        if sample["sequential_sampled_peak_pss_kib_diagnostic"] is not None
    ]
    expected_pss = percentiles(pss_values) if passing_samples and len(pss_values) == len(passing_samples) else None
    actual_pss = aggregates["memory"].get("sequential_sampled_peak_pss_kib_diagnostic")
    if actual_pss != expected_pss:
        violations.append(
            Violation(
                f"{path}.aggregates.memory.sequential_sampled_peak_pss_kib_diagnostic",
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


def validate_document(
    data: Any,
    schema: dict[str, Any],
    context: ValidationContext | None = None,
) -> list[Violation]:
    violations: list[Violation] = []
    validate(data, schema, "$", violations)
    if not violations:
        validate_semantics(data, "$", violations, context)
    return violations


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("result", type=Path)
    parser.add_argument("--schema", type=Path, default=DEFAULT_SCHEMA)
    parser.add_argument("--mode", choices=("staged", "published"), required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--frozen-fixture-root", type=Path)
    parser.add_argument("--staging-out", type=Path)
    parser.add_argument("--failure-evidence-dir", type=Path)
    parser.add_argument("--revision")
    parser.add_argument("--host-proof", type=Path)
    args = parser.parse_args()

    data = load_json(args.result)
    schema = load_json(args.schema)
    try:
        if args.mode == "staged":
            required = {
                "--binary": args.binary,
                "--frozen-fixture-root": args.frozen_fixture_root,
                "--staging-out": args.staging_out,
                "--failure-evidence-dir": args.failure_evidence_dir,
                "--revision": args.revision,
            }
            missing = [flag for flag, value in required.items() if value is None]
            if missing or args.host_proof is not None:
                raise ValueError(f"staged validation requires its command context only; invalid flags: {missing}")
            context = build_validation_context(
                mode="staged",
                repository_root=ROOT,
                manifest_path=ROOT / "benchmarks" / "manifest.toml",
                frozen_fixture_root=args.frozen_fixture_root,
                staging_output=args.staging_out,
                failure_evidence_root=args.failure_evidence_dir,
                engine_binary=args.binary,
                harness_revision=args.revision,
            )
        else:
            if args.host_proof is None or any(
                value is not None
                for value in (
                    args.binary,
                    args.frozen_fixture_root,
                    args.staging_out,
                    args.failure_evidence_dir,
                    args.revision,
                )
            ):
                raise ValueError("published validation derives its exact context only from --host-proof")
            proof_path = args.host_proof.resolve(strict=True)
            if args.host_proof != proof_path or args.host_proof.is_symlink() or not proof_path.is_file():
                raise ValueError("host proof must be one canonical non-symlink file")
            proof = load_json(proof_path)
            if not isinstance(proof, dict):
                raise ValueError("host proof must be a JSON object")
            tools = Path(__file__).resolve().parent
            sys.path.insert(0, str(tools))
            import benchmark_publication  # noqa: PLC0415
            import validate_host_proof  # noqa: PLC0415

            argv = proof.get("command", {}).get("argv")
            if not isinstance(argv, list) or not validate_host_proof.canonical_benchmark_command(argv):
                raise ValueError("host proof omitted the canonical full benchmark command")
            host_schema = load_json(ROOT / "benchmarks" / "schema" / "benchmark-host-proof.v1.json")
            host_violations = validate_host_proof.validate_document(
                proof,
                host_schema,
                proof_path.parent,
                allow_fixture_evidence=False,
                expected_command=tuple(argv),
            )
            if host_violations:
                raise ValueError(f"external host proof is invalid: {host_violations[0]}")
            if proof.get("status") != "accepted" or proof.get("mode") != "production":
                raise ValueError("published validation requires an accepted production host proof")
            values = {flag: Path(argv[argv.index(flag) + 1]) for flag in validate_host_proof.BENCHMARK_COMMAND_FLAGS}
            revision = proof["identity"]["sha"]
            runner = proof["runner"]
            external = ExternalPublication(
                host_proof_schema="pliego.benchmark-host-proof.v1",
                host_proof_bundle_sha256=benchmark_publication.host_proof_bundle_digest(proof_path),
                harness_revision=revision,
                runner_id=runner["id"],
                runner_name=runner["name"],
            )
            context = build_validation_context(
                mode="published",
                repository_root=ROOT,
                manifest_path=ROOT / "benchmarks" / "manifest.toml",
                frozen_fixture_root=values["--frozen-fixture-root"],
                staging_output=values["--staging-out"],
                failure_evidence_root=values["--failure-evidence-dir"],
                engine_binary=values["--binary"],
                harness_revision=revision,
                external_publication=external,
            )
    except (OSError, ValueError) as error:
        print(f"validation context rejected: {error}", file=sys.stderr)
        return 2
    violations = validate_document(data, schema, context)
    if violations:
        for violation in violations:
            print(f"violation {violation}", file=sys.stderr)
        print(f"result failed validation with {len(violations)} violation(s)", file=sys.stderr)
        return 1
    print("result validated against schema")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
