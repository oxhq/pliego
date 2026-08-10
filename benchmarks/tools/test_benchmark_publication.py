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
ATTESTATION_KEY = "e" * 64


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


def attestation(candidate_sha256: str = "c" * 64) -> dict:
    unsigned = {
        "schema": "pliego.benchmark-publication-attestation",
        "version": 1,
        "status": "authorized",
        "authority": "github-protected-environment-hmac-v1",
        "repository": "OxHQ/pliego",
        "ref": "refs/heads/main",
        "revision": "a" * 40,
        "workflow_ref": benchmark_publication.TRUSTED_WORKFLOW_REF,
        "run_id": 123,
        "run_attempt": 1,
        "job": "dedicated",
        "runner": {"id": 7, "name": "pliego-pinned-01"},
        "artifact_name": f"benchmark-publication-{'a' * 40}",
        "subject": {
            "candidate_sha256": candidate_sha256,
            "host_proof_bundle_sha256": "b" * 64,
            "observer_proof_sha256": "d" * 64,
            "output_basename": "fixture.json",
            "operation": "replace",
        },
    }
    return benchmark_publication.authenticate_publication_attestation(unsigned, ATTESTATION_KEY)


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
        completed_attempts = [path for path in (directory / "failures").iterdir() if not path.name.startswith(".")]
        quarantines = list((directory / "failures").glob(".quarantine-*"))
        assert len(completed_attempts) == 2
        assert len(quarantines) == 1 and (quarantines[0] / "retained-samples" / "first").is_file()
        assert not list((directory / "failures").glob(".building-*"))
        assert baseline.read_bytes() == original

        completed_before = set(completed_attempts)
        quarantines_before = set(quarantines)
        original_fsync_directory = benchmark_publication._fsync_directory
        fail_final_fsync = True

        def interrupted_final_fsync(path: Path) -> None:
            nonlocal fail_final_fsync
            if path == (directory / "failures").resolve() and fail_final_fsync:
                fail_final_fsync = False
                raise OSError("simulated final evidence fsync failure")
            original_fsync_directory(path)

        benchmark_publication._fsync_directory = interrupted_final_fsync
        try:
            try:
                benchmark_publication.write_failure_evidence(
                    directory / "failures",
                    failed,
                    "interrupted finalization",
                )
            except benchmark_publication.PublicationError:
                pass
            else:
                raise AssertionError("failure evidence survived an interrupted final fsync as a completed attempt")
        finally:
            benchmark_publication._fsync_directory = original_fsync_directory
        completed_after = {path for path in (directory / "failures").iterdir() if not path.name.startswith(".")}
        quarantines_after = set((directory / "failures").glob(".quarantine-*"))
        assert completed_after == completed_before
        assert len(quarantines_after - quarantines_before) == 1
        assert not list((directory / "failures").glob(".building-*"))

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
            "identity": {
                "sha": "a" * 40,
                "run_id": 123,
                "run_attempt": 1,
                "job": "dedicated",
                "workflow_ref": benchmark_publication.TRUSTED_WORKFLOW_REF,
            },
            "runner": {"id": 7, "name": "pliego-pinned-01"},
        }
        authorization_document = attestation()
        authorization_path = (directory / "123-1.json").resolve()
        authorization_path.write_bytes(benchmark_publication.canonical_json_bytes(authorization_document))
        trusted_digest = benchmark_publication.sha256_file(authorization_path)
        authorization = benchmark_publication.load_trusted_publication_attestation(
            authorization_path,
            ATTESTATION_KEY,
            allow_fixture_authority=True,
        )
        forged = dict(authorization_document, artifact_name="benchmark-publication-forged")
        forged = benchmark_publication.authenticate_publication_attestation(forged, "d" * 64)
        authorization_path.write_bytes(benchmark_publication.canonical_json_bytes(forged))
        try:
            benchmark_publication.load_trusted_publication_attestation(
                authorization_path,
                ATTESTATION_KEY,
                allow_fixture_authority=True,
            )
        except benchmark_publication.PublicationError as error:
            assert "protected workflow context" in str(error)
        else:
            raise AssertionError("candidate-mutated publication attestation authenticated itself")
        authorization_path.write_bytes(benchmark_publication.canonical_json_bytes(authorization_document))
        self_asserted = result()
        self_asserted["host"]["dedicated"] = True
        try:
            benchmark_publication.bind_publication(
                self_asserted,
                proof,
                "b" * 64,
                "d" * 64,
                "c" * 64,
                "fixture.json",
                "replace",
                authorization,
            )
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("self-asserted dedicated candidate was accepted")
        try:
            benchmark_publication.bind_publication(
                result(),
                proof,
                "b" * 64,
                "d" * 64,
                "c" * 64,
                "fixture.json",
                "bootstrap",
                authorization,
            )
        except benchmark_publication.PublicationError as error:
            assert "does not bind" in str(error)
        else:
            raise AssertionError("normal replacement authority silently authorized bootstrap")
        bound = benchmark_publication.bind_publication(
            result(),
            proof,
            "b" * 64,
            "d" * 64,
            "c" * 64,
            "fixture.json",
            "replace",
            authorization,
        )
        assert bound["publication"]["host_proof_bundle_sha256"] == "b" * 64
        assert bound["publication"]["attestation_sha256"] == trusted_digest
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

            late_parent = directory / "late-bound-baselines"
            late_parent.mkdir()
            late_baseline = late_parent / "fixture.json"
            late_baseline.write_bytes(original)
            moved_late_parent = directory / "late-bound-baselines-original"
            with benchmark_publication.bind_publication_directory(late_parent.resolve()) as bound_directory:

                def replace_parent_after_backup() -> None:
                    late_parent.rename(moved_late_parent)
                    late_parent.mkdir()
                    (late_parent / late_baseline.name).write_bytes(replacement_sentinel)

                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        late_baseline.name,
                        b'{"official":"detached-write"}\n',
                        after_backup=replace_parent_after_backup,
                    )
                except benchmark_publication.PublicationError as error:
                    assert "directory identity changed" in str(error)
                else:
                    raise AssertionError("late parent replacement was accepted")
            assert (moved_late_parent / late_baseline.name).read_bytes() == original
            assert (late_parent / late_baseline.name).read_bytes() == replacement_sentinel
            assert not list(moved_late_parent.glob(".*.rollback"))
            assert not list(moved_late_parent.glob(".*.tmp"))

            unlinked_parent = directory / "unlinked-after-journal-baselines"
            unlinked_parent.mkdir()
            unlinked_baseline = unlinked_parent / "fixture.json"
            unlinked_baseline.write_bytes(original)
            with benchmark_publication.bind_publication_directory(unlinked_parent.resolve()) as bound_directory:

                def unlink_destination_after_backup() -> None:
                    unlinked_baseline.unlink()

                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        unlinked_baseline.name,
                        b'{"official":"must-not-survive-unlink-race"}\n',
                        after_backup=unlink_destination_after_backup,
                    )
                except benchmark_publication.PublicationError as error:
                    assert "must already exist" in str(error)
                else:
                    raise AssertionError("destination unlink after rollback creation was accepted")
            assert unlinked_baseline.read_bytes() == original
            assert not list(unlinked_parent.glob(".*.rollback"))
            assert not list(unlinked_parent.glob(".*.tmp"))

            swapped_parent = directory / "swapped-after-journal-baselines"
            swapped_parent.mkdir()
            swapped_baseline = swapped_parent / "fixture.json"
            swapped_baseline.write_bytes(original)
            with benchmark_publication.bind_publication_directory(swapped_parent.resolve()) as bound_directory:

                def swap_destination_after_backup() -> None:
                    replacement = swapped_parent / "replacement.json"
                    replacement.write_bytes(b"raced destination\n")
                    os.replace(replacement, swapped_baseline)

                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        swapped_baseline.name,
                        b'{"official":"must-not-survive-swap-race"}\n',
                        after_backup=swap_destination_after_backup,
                    )
                except benchmark_publication.PublicationError as error:
                    assert "identity changed" in str(error)
                else:
                    raise AssertionError("destination swap after rollback creation was accepted")
            assert swapped_baseline.read_bytes() == original
            assert not list(swapped_parent.glob(".*.rollback"))
            assert not list(swapped_parent.glob(".*.tmp"))

            prepared_parent = directory / "interrupted-after-journal-baselines"
            prepared_parent.mkdir()
            prepared_baseline = prepared_parent / "fixture.json"
            prepared_baseline.write_bytes(original)
            with benchmark_publication.bind_publication_directory(prepared_parent.resolve()) as bound_directory:
                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        prepared_baseline.name,
                        b'{"official":"must-not-survive-prepared-crash"}\n',
                        after_backup=lambda: (_ for _ in ()).throw(
                            benchmark_publication.PublicationInterrupted("simulated crash after rollback journal")
                        ),
                    )
                except benchmark_publication.PublicationInterrupted:
                    pass
                else:
                    raise AssertionError("post-journal interruption was accepted")
            assert prepared_baseline.read_bytes() == original
            assert not list(prepared_parent.glob(".*.rollback"))
            assert not list(prepared_parent.glob(".*.tmp"))

            rollback_parent = directory / "rollback-baselines"
            rollback_parent.mkdir()
            rollback_baseline = rollback_parent / "fixture.json"
            rollback_baseline.write_bytes(original)
            with benchmark_publication.bind_publication_directory(rollback_parent.resolve()) as bound_directory:
                try:
                    benchmark_publication.atomic_publish_bytes(
                        bound_directory,
                        rollback_baseline.name,
                        b'{"official":"interrupted-after-replace"}\n',
                        after_replace=lambda: (_ for _ in ()).throw(
                            benchmark_publication.PublicationInterrupted("simulated crash after replacement")
                        ),
                    )
                except benchmark_publication.PublicationInterrupted:
                    pass
                else:
                    raise AssertionError("post-replacement interruption was accepted")
            assert rollback_baseline.read_bytes() == original
            assert not list(rollback_parent.glob(".*.rollback"))
            assert not list(rollback_parent.glob(".*.tmp"))

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

            bootstrap_parent = directory / "bootstrap-baselines"
            bootstrap_parent.mkdir()
            bootstrap_baseline = bootstrap_parent / "fixture.json"
            with benchmark_publication.bind_publication_directory(bootstrap_parent.resolve()) as bound_directory:
                benchmark_publication.atomic_bootstrap_bytes(
                    bound_directory,
                    bootstrap_baseline.name,
                    b'{"official":"bootstrapped"}\n',
                )
                try:
                    benchmark_publication.atomic_bootstrap_bytes(
                        bound_directory,
                        bootstrap_baseline.name,
                        b'{"official":"bootstrapped-twice"}\n',
                    )
                except benchmark_publication.PublicationError as error:
                    assert "must not already exist" in str(error)
                else:
                    raise AssertionError("one-time baseline bootstrap was repeated")
                benchmark_publication.atomic_publish_bytes(
                    bound_directory,
                    bootstrap_baseline.name,
                    b'{"official":"normal-replacement"}\n',
                )
            assert bootstrap_baseline.read_bytes() == b'{"official":"normal-replacement"}\n'
            assert not list(bootstrap_parent.glob(".*.bootstrap"))

            interrupted_bootstrap_parent = directory / "interrupted-bootstrap-baselines"
            interrupted_bootstrap_parent.mkdir()
            interrupted_bootstrap = interrupted_bootstrap_parent / "fixture.json"
            with benchmark_publication.bind_publication_directory(
                interrupted_bootstrap_parent.resolve()
            ) as bound_directory:
                try:
                    benchmark_publication.atomic_bootstrap_bytes(
                        bound_directory,
                        interrupted_bootstrap.name,
                        b'{"official":"must-not-survive-staged-bootstrap-crash"}\n',
                        simulate_crash=True,
                    )
                except benchmark_publication.PublicationInterrupted:
                    pass
                else:
                    raise AssertionError("pre-link bootstrap interruption was accepted")
                assert not interrupted_bootstrap.exists()
                try:
                    benchmark_publication.atomic_bootstrap_bytes(
                        bound_directory,
                        interrupted_bootstrap.name,
                        b'{"official":"must-not-survive-bootstrap-crash"}\n',
                        after_create=lambda: (_ for _ in ()).throw(
                            benchmark_publication.PublicationInterrupted("simulated crash after bootstrap link")
                        ),
                    )
                except benchmark_publication.PublicationInterrupted:
                    pass
                else:
                    raise AssertionError("post-link bootstrap interruption was accepted")
            assert not interrupted_bootstrap.exists()
            assert not list(interrupted_bootstrap_parent.glob(".*.bootstrap"))

            bootstrap_outside = directory / "bootstrap-outside-sentinel.json"
            bootstrap_outside.write_bytes(b"outside bootstrap sentinel\n")
            symlink_bootstrap_parent = directory / "symlink-bootstrap-baselines"
            symlink_bootstrap_parent.mkdir()
            symlink_bootstrap = symlink_bootstrap_parent / "fixture.json"
            symlink_bootstrap.symlink_to(bootstrap_outside)
            with benchmark_publication.bind_publication_directory(
                symlink_bootstrap_parent.resolve()
            ) as bound_directory:
                try:
                    benchmark_publication.atomic_bootstrap_bytes(
                        bound_directory,
                        symlink_bootstrap.name,
                        b'{"official":"escaped-bootstrap"}\n',
                    )
                except benchmark_publication.PublicationError as error:
                    assert "must not already exist" in str(error)
                else:
                    raise AssertionError("symlink bootstrap destination was accepted")
            assert bootstrap_outside.read_bytes() == b"outside bootstrap sentinel\n"

            raced_bootstrap_parent = directory / "raced-bootstrap-baselines"
            raced_bootstrap_parent.mkdir()
            raced_bootstrap = raced_bootstrap_parent / "fixture.json"
            with benchmark_publication.bind_publication_directory(raced_bootstrap_parent.resolve()) as bound_directory:

                def race_bootstrap_destination() -> None:
                    raced_bootstrap.symlink_to(bootstrap_outside)

                try:
                    benchmark_publication.atomic_bootstrap_bytes(
                        bound_directory,
                        raced_bootstrap.name,
                        b'{"official":"raced-bootstrap"}\n',
                        before_create=race_bootstrap_destination,
                    )
                except benchmark_publication.PublicationError as error:
                    assert "must not already exist" in str(error)
                else:
                    raise AssertionError("bootstrap destination creation race was accepted")
            assert raced_bootstrap.is_symlink()
            assert bootstrap_outside.read_bytes() == b"outside bootstrap sentinel\n"
            assert not list(raced_bootstrap_parent.glob(".*.bootstrap"))

            detached_bootstrap_parent = directory / "detached-bootstrap-baselines"
            detached_bootstrap_parent.mkdir()
            moved_detached_bootstrap_parent = directory / "detached-bootstrap-baselines-original"
            detached_bootstrap = detached_bootstrap_parent / "fixture.json"
            with benchmark_publication.bind_publication_directory(
                detached_bootstrap_parent.resolve()
            ) as bound_directory:

                def replace_bootstrap_parent() -> None:
                    detached_bootstrap_parent.rename(moved_detached_bootstrap_parent)
                    detached_bootstrap_parent.mkdir()
                    (detached_bootstrap_parent / detached_bootstrap.name).write_bytes(replacement_sentinel)

                try:
                    benchmark_publication.atomic_bootstrap_bytes(
                        bound_directory,
                        detached_bootstrap.name,
                        b'{"official":"detached-bootstrap"}\n',
                        before_create=replace_bootstrap_parent,
                    )
                except benchmark_publication.PublicationError as error:
                    assert "directory identity changed" in str(error)
                else:
                    raise AssertionError("bootstrap through a replaced parent was accepted")
            assert not (moved_detached_bootstrap_parent / detached_bootstrap.name).exists()
            assert (detached_bootstrap_parent / detached_bootstrap.name).read_bytes() == replacement_sentinel
            assert not list(moved_detached_bootstrap_parent.glob(".*.bootstrap"))
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
