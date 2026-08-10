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
            previous_umask = os.umask(0o022)
            try:
                evidence = benchmark_publication.write_failure_evidence(directory / "failures", failed, "correctness")
            finally:
                os.umask(previous_umask)
        else:
            raise AssertionError("failed correctness was accepted")
        if os.name == "posix":
            assert (directory / "failures").stat().st_mode & 0o777 == 0o700
        assert baseline.read_bytes() == original

        permissive_root = directory / "permissive-failures"
        permissive_root.mkdir(mode=0o755)
        if os.name == "posix":
            os.chmod(permissive_root, 0o755)
            try:
                benchmark_publication.require_private_failure_root(permissive_root)
            except benchmark_publication.PublicationError as error:
                assert "mode 0700" in str(error)
            else:
                raise AssertionError("an existing permissive failure root was accepted")
            outside_failures = directory / "outside-failures"
            outside_failures.mkdir()
            linked_parent = directory / "linked-failure-parent"
            linked_parent.symlink_to(outside_failures, target_is_directory=True)
            try:
                benchmark_publication.write_failure_evidence(
                    linked_parent / "attempts",
                    failed,
                    "symlink parent",
                )
            except benchmark_publication.PublicationError:
                pass
            else:
                raise AssertionError("a symlinked failure parent was accepted")
            assert not (outside_failures / "attempts").exists()
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

        publication_parent = directory / "bound-baselines"
        publication_parent.mkdir()
        published = publication_parent / "fixture.json"
        published.write_bytes(original)
        if os.name == "posix":
            with benchmark_publication.bind_publication_directory(publication_parent.resolve()) as bound_directory:
                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        published.name,
                        b'{"official":"interrupted"}\n',
                        simulate_crash=True,
                    )
                except benchmark_publication.PublicationInterrupted:
                    pass
                else:
                    raise AssertionError("FD-bound simulated crash did not interrupt")
            assert published.read_bytes() == original
            assert not list(publication_parent.glob(".*.tmp"))

            outside = directory / "outside-sentinel.json"
            outside.write_bytes(b"outside sentinel\n")
            published.unlink()
            published.symlink_to(outside)
            with benchmark_publication.bind_publication_directory(publication_parent.resolve()) as bound_directory:
                try:
                    benchmark_publication.atomic_publish_bytes(bound_directory, published.name, b"replacement\n")
                except benchmark_publication.PublicationError:
                    pass
                else:
                    raise AssertionError("symlink baseline destination was accepted")
            assert outside.read_bytes() == b"outside sentinel\n"

            published.unlink()
            published.write_bytes(original)
            with benchmark_publication.bind_publication_directory(publication_parent.resolve()) as bound_directory:

                def replace_destination() -> None:
                    published.unlink()
                    published.symlink_to(outside)

                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        published.name,
                        b"raced replacement\n",
                        before_replace=replace_destination,
                    )
                except benchmark_publication.PublicationError:
                    pass
                else:
                    raise AssertionError("destination symlink replacement race was accepted")
            assert outside.read_bytes() == b"outside sentinel\n"

            published.unlink()
            published.write_bytes(original)
            moved_parent = directory / "bound-baselines-original"
            replacement_sentinel = b"replacement directory sentinel\n"
            with benchmark_publication.bind_publication_directory(publication_parent.resolve()) as bound_directory:

                def replace_parent() -> None:
                    publication_parent.rename(moved_parent)
                    publication_parent.mkdir()
                    (publication_parent / published.name).write_bytes(replacement_sentinel)

                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        published.name,
                        b'{"official":"escaped"}\n',
                        before_replace=replace_parent,
                    )
                except benchmark_publication.PublicationError as error:
                    assert "directory identity changed" in str(error)
                else:
                    raise AssertionError("parent replacement was accepted")
            assert (moved_parent / published.name).read_bytes() == original
            assert (publication_parent / published.name).read_bytes() == replacement_sentinel
            assert not list(moved_parent.glob(".*.tmp"))

            successful_parent = directory / "successful-baselines"
            successful_parent.mkdir()
            successful_baseline = successful_parent / "fixture.json"
            successful_baseline.write_bytes(original)
            with benchmark_publication.bind_publication_directory(successful_parent.resolve()) as bound_directory:
                benchmark_publication.atomic_publish_bytes(
                    bound_directory,
                    successful_baseline.name,
                    b'{"official":"published"}\n',
                )
            assert successful_baseline.read_bytes() == b'{"official":"published"}\n'
            assert not list(successful_parent.glob(".*.tmp"))
        else:
            try:
                benchmark_publication.bind_publication_directory(publication_parent.resolve())
            except benchmark_publication.PublicationError as error:
                assert "requires POSIX" in str(error)
            else:
                raise AssertionError("Windows accepted a publication path without directory-FD authority")

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
            "--observer-measurements-out",
            str((directory / "observer-measurements.json").resolve()),
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
        destination = publish_benchmark.require_baseline_output(official)
        assert destination.directory == official.parent.resolve(strict=True)
        assert destination.name == official.name

    print("benchmark publication adversarial checks passed")


if __name__ == "__main__":
    main()
