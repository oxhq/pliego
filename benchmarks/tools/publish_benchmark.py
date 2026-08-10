#!/usr/bin/env python3

"""Validate a staged benchmark plus host proof, then atomically publish it."""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path
from typing import Any, NamedTuple

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parents[1]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"
DEFAULT_HOST_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-host-proof.v1.json"
DEFAULT_OBSERVER_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-observer-proof.v1.json"

sys.path.insert(0, str(TOOLS))
import benchmark_publication  # noqa: E402
import observer_ab  # noqa: E402
import validate_host_proof  # noqa: E402
import validate_result  # noqa: E402

MAX_CANDIDATE_BYTES = 512 * 1024 * 1024


class BoundCandidate(NamedTuple):
    document: dict[str, Any]
    binding: dict[str, Any]


class BenchmarkCommandContext(NamedTuple):
    engine_binary: Path
    frozen_fixture_root: Path
    staging_output: Path
    failure_evidence_root: Path


class BaselineDestination(NamedTuple):
    directory: Path
    name: str


def load_bound_candidate(path: Path) -> BoundCandidate:
    document, binding = benchmark_publication.load_bound_json_object(
        path, "staged candidate", max_bytes=MAX_CANDIDATE_BYTES
    )
    return BoundCandidate(document, binding)


def load_object(path: Path, label: str) -> dict[str, Any]:
    value, _ = benchmark_publication.load_bound_json_object(path, label)
    return value


def require_benchmark_command(proof: dict[str, Any], candidate: Path) -> tuple[str, ...]:
    argv = proof.get("command", {}).get("argv")
    if not isinstance(argv, list) or any(not isinstance(value, str) for value in argv):
        raise benchmark_publication.PublicationError("host proof omitted its exact benchmark command")
    if not validate_host_proof.canonical_benchmark_command(argv):
        raise benchmark_publication.PublicationError("host proof command is not the canonical full benchmark command")
    staged_path = validate_host_proof.benchmark_candidate_path(argv)
    assert staged_path is not None
    staged = staged_path.resolve(strict=False)
    if staged != candidate.resolve(strict=True):
        raise benchmark_publication.PublicationError("host proof command is not bound to this staged candidate")
    return tuple(argv)


def benchmark_command_context(argv: tuple[str, ...]) -> BenchmarkCommandContext:
    if not validate_host_proof.canonical_benchmark_command(argv):
        raise benchmark_publication.PublicationError("cannot extract paths from a noncanonical benchmark command")

    def value(flag: str) -> Path:
        return Path(argv[argv.index(flag) + 1])

    return BenchmarkCommandContext(
        engine_binary=value("--binary"),
        frozen_fixture_root=value("--frozen-fixture-root"),
        staging_output=value("--staging-out"),
        failure_evidence_root=value("--failure-evidence-dir"),
    )


def require_accepted_host_proof(proof: dict[str, Any], *, allow_fixture: bool) -> None:
    if proof.get("status") != "accepted" or proof.get("mode") != "production":
        raise benchmark_publication.PublicationError("only an accepted production host proof can publish")
    if proof.get("evidence_source") != "live" and not allow_fixture:
        raise benchmark_publication.PublicationError("publishable host proof must come from the live dedicated host")


def require_canonical_host_proof(path: Path) -> Path:
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise benchmark_publication.PublicationError(f"cannot bind host proof: {error}") from error
    if path != resolved or path.is_symlink() or not path.is_file():
        raise benchmark_publication.PublicationError("host proof path must be a canonical regular file")
    return resolved


def require_candidate_binding(proof: dict[str, Any], binding: dict[str, Any]) -> None:
    proof_binding = proof.get("command", {}).get("candidate")
    if not isinstance(proof_binding, dict):
        raise benchmark_publication.PublicationError("host proof omitted the staged candidate digest")
    if binding != proof_binding:
        raise benchmark_publication.PublicationError("staged candidate differs from the host-bound digest")


def require_baseline_output(path: Path) -> BaselineDestination:
    baselines = (ROOT / "benchmarks" / "baselines").resolve(strict=True)
    output = path if path.is_absolute() else Path.cwd() / path
    try:
        parent = output.parent.resolve(strict=True)
    except OSError as error:
        raise benchmark_publication.PublicationError(f"cannot bind official baselines directory: {error}") from error
    if output.parent != parent or parent != baselines or Path(output.name).suffix != ".json":
        raise benchmark_publication.PublicationError("official output must be one JSON file in benchmarks/baselines")
    if not output.name or Path(output.name).name != output.name:
        raise benchmark_publication.PublicationError("official baseline output must be one basename")
    return BaselineDestination(parent, output.name)


def publish(
    candidate_path: Path,
    proof_path: Path,
    output_path: Path,
    result_schema_path: Path,
    host_schema_path: Path,
    observer_proof_path: Path,
    observer_schema_path: Path,
    attestation_path: Path,
    trusted_attestation_key_hex: str,
    *,
    bound_candidate: BoundCandidate | None = None,
) -> None:
    benchmark_publication.require_unprivileged()
    bound_candidate = bound_candidate or load_bound_candidate(candidate_path)
    candidate = bound_candidate.document
    require_canonical_host_proof(proof_path)
    proof = load_object(proof_path, "host proof")
    require_accepted_host_proof(proof, allow_fixture=False)
    command = require_benchmark_command(proof, candidate_path)
    require_candidate_binding(proof, bound_candidate.binding)

    host_schema = load_object(host_schema_path, "host-proof schema")
    host_violations = validate_host_proof.validate_document(
        proof,
        host_schema,
        proof_path.parent,
        allow_fixture_evidence=False,
        expected_command=command,
    )
    if host_violations:
        raise benchmark_publication.PublicationError(f"host proof is not publishable: {host_violations[0]}")
    command_context = benchmark_command_context(command)
    benchmark_publication.require_private_failure_root(command_context.failure_evidence_root)
    observer_document, observer_file_binding = observer_ab.load_bound_object(observer_proof_path, "observer proof")
    observer_schema = load_object(observer_schema_path, "observer-proof schema")
    observer_violations = observer_ab.validate_proof(
        observer_document,
        observer_schema,
        proof_path,
        observer_ab.SOURCE_BROKER,
        observer_ab.INSTALLED_BROKER,
    )
    if observer_violations:
        raise benchmark_publication.PublicationError(
            f"observer A/B prerequisite is not publishable: {observer_violations[0]}"
        )

    revision = proof["identity"]["sha"]
    try:
        staged_context = validate_result.build_validation_context(
            mode="staged",
            repository_root=ROOT,
            manifest_path=ROOT / "benchmarks" / "manifest.toml",
            frozen_fixture_root=command_context.frozen_fixture_root,
            staging_output=command_context.staging_output,
            failure_evidence_root=command_context.failure_evidence_root,
            engine_binary=command_context.engine_binary,
            harness_revision=revision,
        )
    except ValueError as error:
        raise benchmark_publication.PublicationError(f"host-bound validation context is invalid: {error}") from error
    result_schema = load_object(result_schema_path, "result schema")
    staged_violations = validate_result.validate_document(candidate, result_schema, staged_context)
    if staged_violations:
        raise benchmark_publication.PublicationError(f"staged candidate is invalid: {staged_violations[0]}")
    benchmark_publication.require_supported(candidate)
    digest = benchmark_publication.host_proof_bundle_digest(proof_path)
    output = require_baseline_output(output_path)
    attestation = benchmark_publication.load_trusted_publication_attestation(
        attestation_path,
        trusted_attestation_key_hex,
    )
    bound = benchmark_publication.bind_publication(
        candidate,
        proof,
        digest,
        observer_file_binding["sha256"],
        bound_candidate.binding["sha256"],
        output.name,
        attestation,
    )
    external = validate_result.ExternalPublication(**bound["publication"])
    try:
        published_context = validate_result.build_validation_context(
            mode="published",
            repository_root=ROOT,
            manifest_path=ROOT / "benchmarks" / "manifest.toml",
            frozen_fixture_root=command_context.frozen_fixture_root,
            staging_output=command_context.staging_output,
            failure_evidence_root=command_context.failure_evidence_root,
            engine_binary=command_context.engine_binary,
            harness_revision=revision,
            external_publication=external,
        )
    except ValueError as error:
        raise benchmark_publication.PublicationError(f"external publication context is invalid: {error}") from error
    result_violations = validate_result.validate_document(bound, result_schema, published_context)
    if result_violations:
        raise benchmark_publication.PublicationError(f"bound candidate is invalid: {result_violations[0]}")
    with benchmark_publication.bind_publication_directory(output.directory) as directory:
        benchmark_publication.atomic_publish_bytes(
            directory,
            output.name,
            benchmark_publication.json_bytes(bound, indent=2),
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--host-proof", required=True, type=Path)
    parser.add_argument("--observer-proof", required=True, type=Path)
    parser.add_argument("--attestation", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--failure-evidence", required=True, type=Path)
    args = parser.parse_args()

    try:
        benchmark_publication.require_unprivileged()
    except benchmark_publication.PublicationError as error:
        print(f"publish_benchmark: {error}", file=sys.stderr)
        return 1

    bound_candidate: BoundCandidate | None = None
    try:
        bound_candidate = load_bound_candidate(args.candidate)
        publish(
            args.candidate,
            args.host_proof,
            args.out,
            DEFAULT_SCHEMA,
            DEFAULT_HOST_SCHEMA,
            args.observer_proof,
            DEFAULT_OBSERVER_SCHEMA,
            args.attestation,
            os.environ.get(benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV, ""),
            bound_candidate=bound_candidate,
        )
    except benchmark_publication.PublicationError as error:
        candidate = bound_candidate.document if bound_candidate is not None else {}
        try:
            evidence = benchmark_publication.write_failure_evidence(args.failure_evidence, candidate, str(error))
        except benchmark_publication.PublicationError as evidence_error:
            print(f"publish_benchmark: {error}; evidence rejected: {evidence_error}", file=sys.stderr)
        else:
            print(f"publish_benchmark: {error}; retained {evidence}", file=sys.stderr)
        return 1
    print(f"published {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
