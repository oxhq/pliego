#!/usr/bin/env python3

"""Validate a staged benchmark plus host proof, then atomically publish it."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path
from typing import Any, NamedTuple

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parents[1]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"
DEFAULT_HOST_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-host-proof.v1.json"

sys.path.insert(0, str(TOOLS))
import benchmark_publication  # noqa: E402
import validate_host_proof  # noqa: E402
import validate_result  # noqa: E402

MAX_CANDIDATE_BYTES = 512 * 1024 * 1024


class BoundCandidate(NamedTuple):
    document: dict[str, Any]
    binding: dict[str, Any]


def load_bound_candidate(path: Path) -> BoundCandidate:
    resolved = path.resolve(strict=True)
    if path != resolved:
        raise benchmark_publication.PublicationError("staged candidate path must be absolute and canonical")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise benchmark_publication.PublicationError(f"cannot bind staged candidate: {error}") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size <= 0 or before.st_size > MAX_CANDIDATE_BYTES:
            raise benchmark_publication.PublicationError("staged candidate must be a bounded nonempty regular file")
        chunks: list[bytes] = []
        remaining = before.st_size
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                raise benchmark_publication.PublicationError("staged candidate shortened while binding")
            chunks.append(chunk)
            remaining -= len(chunk)
        raw = b"".join(chunks)
        after = os.fstat(descriptor)
        current = os.stat(path, follow_symlinks=False)
        identity = (before.st_dev, before.st_ino, before.st_size)
        if identity != (after.st_dev, after.st_ino, after.st_size) or identity != (
            current.st_dev,
            current.st_ino,
            current.st_size,
        ):
            raise benchmark_publication.PublicationError("staged candidate identity changed while binding")
    finally:
        os.close(descriptor)
    try:
        document = json.loads(raw)
    except json.JSONDecodeError as error:
        raise benchmark_publication.PublicationError(f"staged candidate is invalid JSON: {error}") from error
    if not isinstance(document, dict):
        raise benchmark_publication.PublicationError("staged candidate must be a JSON object")
    return BoundCandidate(
        document,
        {"path": str(resolved), "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)},
    )


def load_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise benchmark_publication.PublicationError(f"cannot read {label}: {error}") from error
    if not isinstance(value, dict):
        raise benchmark_publication.PublicationError(f"{label} must be a JSON object")
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


def require_baseline_output(path: Path) -> Path:
    baselines = (ROOT / "benchmarks" / "baselines").resolve(strict=True)
    output = path.resolve(strict=False)
    if output.parent != baselines or output.suffix != ".json":
        raise benchmark_publication.PublicationError("official output must be one JSON file in benchmarks/baselines")
    if path.is_symlink() or (output.exists() and not output.is_file()):
        raise benchmark_publication.PublicationError("official baseline output is not a regular non-symlink path")
    return output


def publish(
    candidate_path: Path,
    proof_path: Path,
    output_path: Path,
    result_schema_path: Path,
    host_schema_path: Path,
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

    benchmark_publication.require_supported(candidate)
    digest = benchmark_publication.host_proof_bundle_digest(proof_path)
    bound = benchmark_publication.bind_publication(candidate, proof, digest)
    result_schema = load_object(result_schema_path, "result schema")
    result_violations = validate_result.validate_document(bound, result_schema)
    if result_violations:
        raise benchmark_publication.PublicationError(f"bound candidate is invalid: {result_violations[0]}")
    output = require_baseline_output(output_path)
    benchmark_publication.atomic_write_bytes(
        output,
        json.dumps(bound, indent=2, ensure_ascii=False).encode("utf-8") + b"\n",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--host-proof", required=True, type=Path)
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
