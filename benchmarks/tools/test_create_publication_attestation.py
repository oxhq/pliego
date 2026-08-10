#!/usr/bin/env python3

"""Adversarial self-checks for protected-context publication authentication."""

from __future__ import annotations

import tempfile
from pathlib import Path

import benchmark_publication
import create_publication_attestation


def main() -> None:
    key = "e" * 64
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw).resolve()
        candidate = root / "candidate.json"
        observer = root / "observer.json"
        proof_root = root / "host-proof"
        proof_root.mkdir()
        proof = proof_root / "benchmark-host-proof.v1.json"
        candidate.write_bytes(benchmark_publication.canonical_json_bytes({"candidate": True}))
        observer.write_bytes(benchmark_publication.canonical_json_bytes({"observer": True}))
        proof.write_bytes(
            benchmark_publication.canonical_json_bytes(
                {
                    "identity": {
                        "sha": "a" * 40,
                        "run_id": 123,
                        "run_attempt": 1,
                        "job": "dedicated",
                        "workflow_ref": benchmark_publication.TRUSTED_WORKFLOW_REF,
                    },
                    "runner": {"id": 7, "name": "pliego-pinned-01"},
                }
            )
        )
        (proof_root / "SHA256SUMS").write_text(
            f"{benchmark_publication.sha256_file(proof)}  {proof.name}\n",
            encoding="ascii",
        )
        document = create_publication_attestation.build_attestation(
            candidate,
            proof,
            observer,
            "fixture.json",
            "bootstrap",
            key,
        )
        attestation = root / "123-1.json"
        attestation.write_bytes(benchmark_publication.canonical_json_bytes(document))
        trusted = benchmark_publication.load_trusted_publication_attestation(
            attestation,
            key,
            allow_fixture_authority=True,
        )
        assert trusted.document["subject"]["candidate_sha256"] == benchmark_publication.sha256_file(candidate)
        assert trusted.document["subject"]["operation"] == "bootstrap"

        forged_unsigned = {name: value for name, value in document.items() if name != "authentication"}
        forged_unsigned["subject"] = {**forged_unsigned["subject"], "candidate_sha256": "f" * 64}
        forged = benchmark_publication.authenticate_publication_attestation(forged_unsigned, "d" * 64)
        attestation.write_bytes(benchmark_publication.canonical_json_bytes(forged))
        try:
            benchmark_publication.load_trusted_publication_attestation(
                attestation,
                key,
                allow_fixture_authority=True,
            )
        except benchmark_publication.PublicationError as error:
            assert "protected workflow context" in str(error)
        else:
            raise AssertionError("attestation authenticated with a candidate-controlled key")
    print("publication attestation authentication checks passed")


if __name__ == "__main__":
    main()
