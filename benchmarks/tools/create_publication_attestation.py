#!/usr/bin/env python3

"""Authenticate one benchmark publication subject with protected workflow context."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import benchmark_publication  # noqa: E402


def build_attestation(
    candidate: Path,
    host_proof: Path,
    observer_proof: Path,
    output_basename: str,
    operation: str,
    key_hex: str,
) -> dict[str, Any]:
    _, candidate_binding = benchmark_publication.load_bound_json_object(candidate, "staged candidate")
    proof, _ = benchmark_publication.load_bound_json_object(host_proof, "host proof")
    _, observer_binding = benchmark_publication.load_bound_json_object(observer_proof, "observer proof")
    identity = proof.get("identity")
    runner = proof.get("runner")
    if not isinstance(identity, dict) or not isinstance(runner, dict):
        raise benchmark_publication.PublicationError("host proof omitted its run/runner identity")
    unsigned = {
        "schema": "pliego.benchmark-publication-attestation",
        "version": 1,
        "status": "authorized",
        "authority": "github-protected-environment-hmac-v1",
        "repository": benchmark_publication.CANONICAL_REPOSITORY,
        "ref": "refs/heads/main",
        "revision": identity.get("sha"),
        "workflow_ref": identity.get("workflow_ref"),
        "run_id": identity.get("run_id"),
        "run_attempt": identity.get("run_attempt"),
        "job": identity.get("job"),
        "runner": {"id": runner.get("id"), "name": runner.get("name")},
        "artifact_name": f"benchmark-publication-{identity.get('sha')}",
        "subject": {
            "candidate_sha256": candidate_binding["sha256"],
            "host_proof_bundle_sha256": benchmark_publication.host_proof_bundle_digest(host_proof),
            "observer_proof_sha256": observer_binding["sha256"],
            "output_basename": output_basename,
            "operation": operation,
        },
    }
    return benchmark_publication.authenticate_publication_attestation(unsigned, key_hex)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--host-proof", required=True, type=Path)
    parser.add_argument("--observer-proof", required=True, type=Path)
    parser.add_argument("--output-basename", required=True)
    parser.add_argument("--operation", required=True, choices=("bootstrap", "replace"))
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    trusted_key = benchmark_publication.consume_trusted_attestation_key()
    try:
        benchmark_publication.require_unprivileged()
        document = build_attestation(
            args.candidate,
            args.host_proof,
            args.observer_proof,
            args.output_basename,
            args.operation,
            trusted_key,
        )
        benchmark_publication.atomic_write_bytes(
            args.out,
            benchmark_publication.json_bytes(document, indent=2),
        )
    except (OSError, benchmark_publication.PublicationError) as error:
        print(f"create_publication_attestation: {error}", file=sys.stderr)
        return 1
    print(f"created authenticated publication attestation {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
