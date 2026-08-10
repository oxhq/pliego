#!/usr/bin/env python3

"""Adversarial checks for staging/failure separation and atomic publication."""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
import importlib.util

import benchmark_publication

PUBLISH = Path(__file__).with_name("publish_benchmark.py")
SPEC = importlib.util.spec_from_file_location("publish_benchmark", PUBLISH)
assert SPEC is not None and SPEC.loader is not None
publish_benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publish_benchmark)


def result(status: str = "supported", passed: bool = True) -> dict:
    return {
        "schema": "pliego.benchmark-result",
        "version": 1,
        "status": status,
        "host": {"dedicated": False},
        "toolchain": {"harness_revision": "a" * 40},
        "fixture": {"id": "fixture"},
        "aggregates": {"correctness": {"passed": passed}},
    }


def main() -> None:
    try:
        benchmark_publication.require_unprivileged(0, 0)
    except benchmark_publication.PublicationError:
        pass
    else:
        raise AssertionError("root publication was accepted")

    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        baseline = directory / "baseline.json"
        original = b'{"official":"unchanged"}\n'
        baseline.write_bytes(original)

        failed = result("failed", False)
        try:
            benchmark_publication.require_supported(failed)
        except benchmark_publication.PublicationError:
            evidence = benchmark_publication.write_failure_evidence(directory / "failures", failed, "correctness")
        else:
            raise AssertionError("failed correctness was accepted")
        assert baseline.read_bytes() == original
        assert (evidence / "candidate.json").is_file()
        assert (evidence / "SHA256SUMS").read_text(encoding="ascii").split()[0] == benchmark_publication.sha256_file(
            evidence / "candidate.json"
        )

        retained = directory / "failures" / ".in-progress-fixture"
        retained.mkdir(mode=0o700)
        (retained / "engine.stderr").write_text("failure output", encoding="utf-8")
        retained_evidence = benchmark_publication.write_failure_evidence(
            directory / "failures",
            failed,
            "correctness with artifacts",
            retained,
        )
        retained_file = retained_evidence / "retained-samples" / "engine.stderr"
        assert retained_file.read_text(encoding="utf-8") == "failure output"
        assert "retained-samples/engine.stderr" in (retained_evidence / "SHA256SUMS").read_text(encoding="ascii")
        retained_document = json.loads((retained_evidence / "candidate.json").read_text(encoding="utf-8"))
        assert retained_document["retained_samples"] == {
            "directory": "retained-samples",
            "original_path_prefix": str(retained.resolve(strict=False)),
        }
        assert not retained.exists()

        linked = directory / "failures" / ".in-progress-linked"
        linked.mkdir(mode=0o700)
        (linked / "first").write_bytes(b"linked evidence")
        os.link(linked / "first", linked / "second")
        try:
            benchmark_publication.write_failure_evidence(
                directory / "failures",
                failed,
                "unsafe linked artifacts",
                linked,
            )
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("hard-linked failure artifact was accepted")
        assert baseline.read_bytes() == original

        try:
            benchmark_publication.atomic_write_bytes(
                baseline,
                b'{"official":"new"}\n',
                simulate_crash=True,
            )
        except benchmark_publication.PublicationInterrupted:
            pass
        else:
            raise AssertionError("simulated publication crash did not interrupt")
        assert baseline.read_bytes() == original
        assert not list(directory.glob(".baseline.json.*.tmp"))

        proof = {
            "identity": {"sha": "a" * 40},
            "runner": {"id": 7, "name": "pliego-pinned-01"},
        }
        self_asserted = result()
        self_asserted["host"]["dedicated"] = True
        try:
            benchmark_publication.bind_publication(self_asserted, proof, "b" * 64)
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("self-asserted dedicated candidate was accepted")
        bound = benchmark_publication.bind_publication(result(), proof, "b" * 64)
        assert bound["publication"]["host_proof_bundle_sha256"] == "b" * 64
        assert bound["host"]["dedicated"] is True
        benchmark_publication.atomic_write_bytes(baseline, benchmark_publication.canonical_json_bytes(bound))
        assert json.loads(baseline.read_text(encoding="utf-8"))["status"] == "supported"

        candidate = directory / "candidate.json"
        candidate.write_text('{"candidate":1}\n', encoding="utf-8")
        command = [
            "python3",
            "benchmarks/tools/run_benchmark.py",
            "--binary",
            str((directory / "pliego").resolve()),
            "--frozen-fixture-root",
            str((directory / "fixtures").resolve()),
            "--staging-out",
            str(candidate.resolve()),
            "--failure-evidence-dir",
            str((directory / "failures").resolve()),
        ]
        digest_proof = {
            "status": "accepted",
            "mode": "production",
            "evidence_source": "live",
            "command": {
                "argv": command,
                "candidate": {
                    "path": str(candidate.resolve()),
                    "sha256": benchmark_publication.sha256_file(candidate),
                    "bytes": candidate.stat().st_size,
                },
            },
        }
        assert publish_benchmark.require_benchmark_command(digest_proof, candidate) == tuple(command)
        noncanonical = json.loads(json.dumps(digest_proof))
        noncanonical["command"]["argv"][3] = str(directory / "fixtures" / ".." / "pliego")
        try:
            publish_benchmark.require_benchmark_command(noncanonical, candidate)
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("noncanonical benchmark command path was accepted")
        publish_benchmark.require_accepted_host_proof(digest_proof, allow_fixture=False)
        rejected_proof = dict(digest_proof, status="failed")
        try:
            publish_benchmark.require_accepted_host_proof(rejected_proof, allow_fixture=False)
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("failed host proof was accepted for publication")
        bound_candidate = publish_benchmark.load_bound_candidate(candidate)
        publish_benchmark.require_candidate_binding(digest_proof, bound_candidate.binding)
        candidate.write_text('{"candidate":2}\n', encoding="utf-8")
        try:
            publish_benchmark.require_candidate_binding(
                digest_proof,
                publish_benchmark.load_bound_candidate(candidate).binding,
            )
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("candidate replacement after host proof was accepted")

        try:
            publish_benchmark.require_baseline_output(directory / "not-an-official-baseline.json")
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("publication outside benchmarks/baselines was accepted")
        official = publish_benchmark.ROOT / "benchmarks" / "baselines" / "fixture.json"
        assert publish_benchmark.require_baseline_output(official) == official.resolve(strict=False)

    print("benchmark publication adversarial checks passed")


if __name__ == "__main__":
    main()
