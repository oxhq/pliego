#!/usr/bin/env python3

"""Adversarial checks for baseline artifact verification and PR-only promotion."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
from argparse import Namespace
from collections.abc import Callable
from pathlib import Path

import benchmark_publication
import publication_promotion
import validate_host_proof

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "pliego-dedicated-benchmark.yml"


def git(root: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return result.stdout.strip()


def initialize_repository(root: Path) -> str:
    root.mkdir()
    git(root, "init", "--quiet")
    git(root, "config", "user.name", "promotion-test")
    git(root, "config", "user.email", "promotion-test@example.invalid")
    baseline_root = root / "benchmarks" / "baselines"
    baseline_root.mkdir(parents=True)
    (baseline_root / ".gitkeep").write_bytes(b"")
    git(root, "add", "benchmarks/baselines/.gitkeep")
    git(root, "commit", "--quiet", "-m", "fixture")
    return git(root, "rev-parse", "HEAD")


def source_args(root: Path, revision: str, run_id: int = 123, run_attempt: int = 1) -> Namespace:
    candidate = root / "candidate.json"
    observer = root / "observer.json"
    attestation = root / f"{run_id}-{run_attempt}.json"
    host = root / "host-proof"
    host.mkdir()
    candidate.write_text('{"candidate":true}\n', encoding="utf-8")
    observer.write_text('{"observer":true}\n', encoding="utf-8")
    attestation.write_text('{"attestation":true}\n', encoding="utf-8")
    for name in validate_host_proof.ARTIFACT_NAMES:
        (host / name).write_bytes(
            b"" if name in {validate_host_proof.STDOUT_NAME, validate_host_proof.STDERR_NAME} else b"{}\n"
        )
    (host / validate_host_proof.MANIFEST_NAME).write_text("0" * 64 + "  fixture\n", encoding="ascii")
    baseline = root / publication_promotion.OFFICIAL_OUTPUT
    baseline.write_text('{"official":"first"}\n', encoding="utf-8")
    return Namespace(
        repository=benchmark_publication.CANONICAL_REPOSITORY,
        repository_root=root,
        revision=revision,
        run_id=run_id,
        run_attempt=run_attempt,
        operation="bootstrap",
        output_basename=publication_promotion.OFFICIAL_OUTPUT.name,
        candidate=candidate,
        host_proof_dir=host,
        observer_proof=observer,
        attestation=attestation,
        baseline=baseline,
        out=root / "publication-source",
    )


def expect_rejection(callable_object: Callable[[], object], contains: str) -> None:
    try:
        callable_object()
    except benchmark_publication.PublicationError as error:
        assert contains in str(error), error
    else:
        raise AssertionError(f"expected publication rejection containing {contains!r}")


def test_canonical_repository_identity() -> None:
    assert benchmark_publication.CANONICAL_REPOSITORY == "oxhq/pliego"
    assert benchmark_publication.TRUSTED_WORKFLOW_REF == (
        "oxhq/pliego/.github/workflows/pliego-dedicated-benchmark.yml@refs/heads/main"
    )
    attestation_schema = json.loads(
        (ROOT / "benchmarks" / "schema" / "benchmark-publication-attestation.v1.json").read_bytes()
    )
    result_schema = json.loads((ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json").read_bytes())
    assert attestation_schema["properties"]["repository"]["const"] == benchmark_publication.CANONICAL_REPOSITORY
    assert attestation_schema["properties"]["workflow_ref"]["const"] == benchmark_publication.TRUSTED_WORKFLOW_REF
    assert (
        result_schema["definitions"]["publication"]["properties"]["github_workflow_ref"]["const"]
        == benchmark_publication.TRUSTED_WORKFLOW_REF
    )


def test_fork_safe_pull_request_selection() -> None:
    revision = "a" * 40
    branch = f"benchmark-baseline/{revision}/123-1-bootstrap"
    exact = {
        "number": 3,
        "url": "https://github.com/oxhq/pliego/pull/3",
        "state": "OPEN",
        "headRefOid": "b" * 40,
        "baseRefName": "main",
        "headRefName": branch,
        "headRepository": {"nameWithOwner": benchmark_publication.CANONICAL_REPOSITORY},
        "isCrossRepository": False,
    }
    public_fork = {
        **exact,
        "number": 1,
        "url": "https://github.com/attacker/pliego/pull/1",
        "headRepository": {"nameWithOwner": "attacker/pliego"},
        "isCrossRepository": True,
    }
    case_substitution = {
        **exact,
        "number": 2,
        "headRepository": {"nameWithOwner": "OxHQ/pliego"},
    }
    missing_cross_repository = dict(exact)
    missing_cross_repository.pop("isCrossRepository")
    wrong_branch = {**exact, "headRefName": f"{branch}-attacker"}
    assert publication_promotion.select_promotion_pull_requests(
        [public_fork, case_substitution, missing_cross_repository, wrong_branch, exact],
        benchmark_publication.CANONICAL_REPOSITORY,
        branch,
    ) == [exact]
    assert (
        publication_promotion.select_promotion_pull_requests(
            [public_fork], benchmark_publication.CANONICAL_REPOSITORY, branch
        )
        == []
    )
    expect_rejection(
        lambda: publication_promotion.select_promotion_pull_requests([exact], "OxHQ/pliego", branch),
        "noncanonical repository",
    )


def test_exact_github_commit_contract() -> None:
    revision = "a" * 40
    commit = "b" * 40
    blob = "c" * 40
    tree_sha = "d" * 40
    benchmarks_tree_sha = "e" * 40
    baselines_tree_sha = "f" * 40
    path = publication_promotion.OFFICIAL_OUTPUT.as_posix()
    compare = {
        "base_commit": {"sha": revision},
        "merge_base_commit": {"sha": revision},
        "status": "ahead",
        "ahead_by": 1,
        "behind_by": 0,
        "total_commits": 1,
        "commits": [{"sha": commit}],
        "files": [{"filename": path, "status": "added"}],
    }
    commit_record = {"sha": commit, "parents": [{"sha": revision}], "tree": {"sha": tree_sha}}
    trees = [
        {
            "sha": tree_sha,
            "truncated": False,
            "tree": [{"path": "benchmarks", "mode": "040000", "type": "tree", "sha": benchmarks_tree_sha}],
        },
        {
            "sha": benchmarks_tree_sha,
            "truncated": False,
            "tree": [{"path": "baselines", "mode": "040000", "type": "tree", "sha": baselines_tree_sha}],
        },
        {
            "sha": baselines_tree_sha,
            "truncated": False,
            "tree": [
                {
                    "path": publication_promotion.OFFICIAL_OUTPUT.name,
                    "mode": "100644",
                    "type": "blob",
                    "sha": blob,
                }
            ],
        },
    ]

    def validate(candidate_compare: object, candidate_commit: object, candidate_tree: object) -> None:
        publication_promotion.validate_github_commit(
            candidate_compare,
            candidate_commit,
            candidate_tree,
            revision=revision,
            commit=commit,
            expected_path=path,
            expected_status="added",
            expected_blob=blob,
        )

    validate(compare, commit_record, trees)

    extra_parent = json.loads(json.dumps(commit_record))
    extra_parent["parents"].append({"sha": "e" * 40})
    expect_rejection(lambda: validate(compare, extra_parent, trees), "sole parent")

    wrong_parent = json.loads(json.dumps(commit_record))
    wrong_parent["parents"][0]["sha"] = "e" * 40
    expect_rejection(lambda: validate(compare, wrong_parent, trees), "sole parent")

    executable = json.loads(json.dumps(trees))
    executable[-1]["tree"][0]["mode"] = "100755"
    expect_rejection(lambda: validate(compare, commit_record, executable), "tree entry mode")

    substituted_tree = json.loads(json.dumps(trees))
    substituted_tree[-1]["tree"][0]["sha"] = "9" * 40
    expect_rejection(lambda: validate(compare, commit_record, substituted_tree), "tree entry mode")

    extra_path = json.loads(json.dumps(compare))
    extra_path["files"].append({"filename": "README.md", "status": "modified"})
    expect_rejection(lambda: validate(extra_path, commit_record, trees), "parent or path drift")

    wrong_parent_tree = json.loads(json.dumps(trees))
    wrong_parent_tree[0]["tree"][0]["sha"] = "9" * 40
    expect_rejection(lambda: validate(compare, commit_record, wrong_parent_tree), "another tree")

    truncated = json.loads(json.dumps(trees))
    truncated[1]["truncated"] = True
    expect_rejection(lambda: validate(compare, commit_record, truncated), "tree response is incomplete")


def test_source_artifact_contract(root: Path) -> None:
    revision = initialize_repository(root)
    args = source_args(root, revision)
    variable = benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV
    os.environ[variable] = "e" * 64
    expect_rejection(lambda: publication_promotion.stage(args), "must not inherit publication HMAC authority")
    os.environ.pop(variable)

    noncanonical = Namespace(**{**vars(args), "repository_root": root / ".." / root.name})
    expect_rejection(lambda: publication_promotion.stage(noncanonical), "canonical non-symlink directory")
    expect_rejection(
        lambda: publication_promotion._new_output(root / ".." / root.name / "alias-output", "fixture output"),
        "canonical non-symlink directory",
    )
    publication_promotion.stage(args)
    expected = publication_promotion._identity(
        repository=args.repository,
        revision=revision,
        run_id=args.run_id,
        run_attempt=args.run_attempt,
        operation=args.operation,
        output_basename=args.output_basename,
    )
    manifest = publication_promotion._source_manifest(args.out.resolve(), expected)
    assert manifest["expected_old_blob"] is None
    assert manifest["artifact_name"] == f"benchmark-publication-source-{revision}-bootstrap-123-1"

    original_manifest = (args.out / publication_promotion.SOURCE_MANIFEST).read_bytes()
    tampered = json.loads(original_manifest)
    tampered["revision"] = "f" * 40
    (args.out / publication_promotion.SOURCE_MANIFEST).write_bytes(benchmark_publication.json_bytes(tampered, indent=2))
    expect_rejection(
        lambda: publication_promotion._source_manifest(args.out.resolve(), expected),
        "identity was substituted",
    )
    (args.out / publication_promotion.SOURCE_MANIFEST).write_bytes(original_manifest)

    candidate = args.out / "candidate.json"
    original_candidate = candidate.read_bytes()
    candidate.write_bytes(b'{"candidate":"substituted"}\n')
    expect_rejection(
        lambda: publication_promotion._source_manifest(args.out.resolve(), expected),
        "file was substituted",
    )
    candidate.write_bytes(original_candidate)

    hard_link = root / "candidate-hard-link.json"
    os.link(args.candidate, hard_link)
    expect_rejection(
        lambda: publication_promotion._binding(args.candidate, "hard-linked candidate"),
        "single-link regular file",
    )
    hard_link.unlink()

    substituted = dict(expected, operation="replace")
    expect_rejection(
        lambda: publication_promotion._source_manifest(args.out.resolve(), substituted),
        "identity was substituted",
    )
    expect_rejection(
        lambda: publication_promotion._identity(
            repository=benchmark_publication.CANONICAL_REPOSITORY,
            revision=revision,
            run_id=123,
            run_attempt=1,
            operation="bootstrap",
            output_basename="attacker.json",
        ),
        "not the one authorized baseline",
    )
    expect_rejection(
        lambda: publication_promotion._identity(
            repository="OxHQ/pliego",
            revision=revision,
            run_id=123,
            run_attempt=1,
            operation="bootstrap",
            output_basename=publication_promotion.OFFICIAL_OUTPUT.name,
        ),
        "canonical oxhq/pliego repository",
    )


def test_old_blob_transition(root: Path) -> None:
    revision = initialize_repository(root)
    assert publication_promotion._expected_old_blob(root, revision, "bootstrap") is None
    expect_rejection(
        lambda: publication_promotion._expected_old_blob(root, revision, "replace"),
        "requires an existing baseline blob",
    )
    baseline = root / publication_promotion.OFFICIAL_OUTPUT
    baseline.write_text('{"official":"tracked"}\n', encoding="utf-8")
    git(root, "add", publication_promotion.OFFICIAL_OUTPUT.as_posix())
    git(root, "commit", "--quiet", "-m", "baseline")
    tracked_revision = git(root, "rev-parse", "HEAD")
    blob = git(root, "rev-parse", f"{tracked_revision}:{publication_promotion.OFFICIAL_OUTPUT.as_posix()}")
    assert publication_promotion._expected_old_blob(root, tracked_revision, "replace") == blob
    expect_rejection(
        lambda: publication_promotion._expected_old_blob(root, tracked_revision, "bootstrap"),
        "requires the exact baseline path to be absent",
    )
    git(root, "update-index", "--chmod=+x", publication_promotion.OFFICIAL_OUTPUT.as_posix())
    git(root, "commit", "--quiet", "-m", "wrong baseline mode")
    executable_revision = git(root, "rev-parse", "HEAD")
    expect_rejection(
        lambda: publication_promotion._expected_old_blob(root, executable_revision, "replace"),
        "mode-100644 Git blob",
    )


def test_verified_artifact_contract(root: Path) -> None:
    root.mkdir()
    baseline = root / publication_promotion.OFFICIAL_OUTPUT.name
    baseline.write_bytes(b'{"official":"verified"}\n')
    expected = publication_promotion._identity(
        repository=benchmark_publication.CANONICAL_REPOSITORY,
        revision="a" * 40,
        run_id=456,
        run_attempt=2,
        operation="bootstrap",
        output_basename=baseline.name,
    )
    manifest = {
        "schema": publication_promotion.VERIFIED_SCHEMA,
        "version": 1,
        **expected,
        "artifact_name": f"verified-benchmark-baseline-{'a' * 40}-bootstrap-456-2",
        "source_artifact_name": f"benchmark-publication-source-{'a' * 40}-bootstrap-456-2",
        "source_manifest_sha256": "b" * 64,
        "attestation_sha256": "c" * 64,
        "expected_old_blob": None,
        "baseline": publication_promotion._binding(baseline, "fixture baseline"),
    }
    manifest_path = root / publication_promotion.VERIFIED_MANIFEST
    manifest_path.write_bytes(benchmark_publication.json_bytes(manifest, indent=2))
    publication_promotion._verified_manifest(root.resolve(), expected)

    baseline.write_bytes(b'{"official":"artifact-substitution"}\n')
    expect_rejection(
        lambda: publication_promotion._verified_manifest(root.resolve(), expected),
        "baseline bytes were substituted",
    )
    baseline.write_bytes(b'{"official":"verified"}\n')
    tampered = dict(manifest, run_attempt=3)
    manifest_path.write_bytes(benchmark_publication.json_bytes(tampered, indent=2))
    expect_rejection(
        lambda: publication_promotion._verified_manifest(root.resolve(), expected),
        "identity was substituted",
    )
    tampered = dict(manifest, expected_old_blob="d" * 40)
    manifest_path.write_bytes(benchmark_publication.json_bytes(tampered, indent=2))
    expect_rejection(
        lambda: publication_promotion._verified_manifest(root.resolve(), expected),
        "must bind an absent old blob",
    )
    tampered = dict(manifest, artifact_name="verified-benchmark-baseline-substituted")
    manifest_path.write_bytes(benchmark_publication.json_bytes(tampered, indent=2))
    expect_rejection(
        lambda: publication_promotion._verified_manifest(root.resolve(), expected),
        "artifact name was substituted",
    )


def test_hmac_reverification_and_verified_emission(root: Path) -> None:
    revision = initialize_repository(root)
    args = source_args(root, revision, run_id=789, run_attempt=4)
    key = "e" * 64
    candidate = {
        "schema": "pliego.benchmark-result",
        "version": 1,
        "status": "supported",
        "host": {"dedicated": False},
        "toolchain": {"harness_revision": revision},
        "fixture": {"id": "fixture"},
        "aggregates": {"correctness": {"passed": True}},
    }
    args.candidate.write_bytes(benchmark_publication.json_bytes(candidate))
    candidate_binding = publication_promotion._binding(args.candidate, "fixture candidate")
    measurements: dict[str, object] = {}
    measurements_binding = {
        "path": str(root / "measurements.json"),
        "sha256": "1" * 64,
        "bytes": 2,
        "canonical_sha256": hashlib.sha256(publication_promotion.observer_ab.canonical_bytes(measurements)).hexdigest(),
    }
    command_paths = [
        root / "engine",
        root / "frozen",
        args.candidate,
        root / "failures",
        root / "measurements.json",
    ]
    command_argv = ["python3", "benchmarks/tools/run_benchmark.py"]
    for flag, path in zip(validate_host_proof.BENCHMARK_COMMAND_FLAGS, command_paths, strict=True):
        command_argv.extend((flag, str(path)))
    proof_path = args.host_proof_dir / validate_host_proof.PROOF_NAME
    proof = {
        "status": "accepted",
        "mode": "production",
        "identity": {
            "repository": benchmark_publication.CANONICAL_REPOSITORY,
            "sha": revision,
            "run_id": args.run_id,
            "run_attempt": args.run_attempt,
            "job": "dedicated",
            "workflow_ref": benchmark_publication.TRUSTED_WORKFLOW_REF,
        },
        "runner": {"id": 7, "name": "pliego-pinned-01"},
        "host_config": {"sha256": "2" * 64},
        "command": {
            "argv": command_argv,
            "candidate": candidate_binding,
            "observer_measurements": measurements_binding,
        },
    }
    proof_path.write_bytes(benchmark_publication.json_bytes(proof))
    proof_digest = benchmark_publication.host_proof_bundle_digest(proof_path)
    source_broker_digest = publication_promotion._sha256(publication_promotion.observer_ab.SOURCE_BROKER.resolve())
    observer = {
        "binding": {
            "harness_revision": revision,
            "source_broker_sha256": source_broker_digest,
            "installed_broker_sha256": source_broker_digest,
            "host_config_sha256": proof["host_config"]["sha256"],
            "host_proof_bundle_sha256": proof_digest,
            "runner_id": proof["runner"]["id"],
            "runner_name": proof["runner"]["name"],
        },
        "measurements_binding": measurements_binding,
        "measurements": measurements,
    }
    args.observer_proof.write_bytes(benchmark_publication.json_bytes(observer))
    observer_binding = publication_promotion._binding(args.observer_proof, "fixture observer")
    unsigned = {
        "schema": "pliego.benchmark-publication-attestation",
        "version": 1,
        "status": "authorized",
        "authority": "github-protected-environment-hmac-v1",
        "repository": benchmark_publication.CANONICAL_REPOSITORY,
        "ref": "refs/heads/main",
        "revision": revision,
        "workflow_ref": benchmark_publication.TRUSTED_WORKFLOW_REF,
        "run_id": args.run_id,
        "run_attempt": args.run_attempt,
        "job": "dedicated",
        "runner": proof["runner"],
        "artifact_name": f"benchmark-publication-{revision}",
        "subject": {
            "candidate_sha256": candidate_binding["sha256"],
            "host_proof_bundle_sha256": proof_digest,
            "observer_proof_sha256": observer_binding["sha256"],
            "output_basename": args.output_basename,
            "operation": args.operation,
        },
    }
    attestation = benchmark_publication.authenticate_publication_attestation(unsigned, key)
    args.attestation.write_bytes(benchmark_publication.json_bytes(attestation))
    trusted = benchmark_publication.load_trusted_publication_attestation(
        args.attestation,
        key,
        allow_fixture_authority=True,
    )
    baseline = benchmark_publication.bind_publication(
        candidate,
        proof,
        proof_digest,
        observer_binding["sha256"],
        candidate_binding["sha256"],
        args.output_basename,
        args.operation,
        trusted,
    )
    args.baseline.write_bytes(benchmark_publication.json_bytes(baseline, indent=2))
    publication_promotion.stage(args)

    validation_labels: list[str] = []
    original_schema_validation = publication_promotion._validate_schema
    original_host_validation = publication_promotion.validate_host_proof.validate_document
    original_measurement_validation = publication_promotion.observer_ab.validate_measurements

    def record_schema_validation(_document: object, _schema: Path, label: str) -> None:
        validation_labels.append(label)

    def accept_fixture_host(*_args: object, **kwargs: object) -> list[str]:
        assert kwargs["allow_fixture_evidence"] is False
        return []

    publication_promotion._validate_schema = record_schema_validation
    publication_promotion.validate_host_proof.validate_document = accept_fixture_host
    publication_promotion.observer_ab.validate_measurements = lambda _document: []
    try:
        verify_args = Namespace(
            repository=args.repository,
            revision=revision,
            run_id=args.run_id,
            run_attempt=args.run_attempt,
            operation=args.operation,
            output_basename=args.output_basename,
            bundle=args.out.resolve(),
            out=root / "verified",
        )
        publication_promotion.verify(verify_args, key)
        assert validation_labels == ["publication attestation", "observer proof", "published baseline"]
        expected = publication_promotion._identity(
            repository=args.repository,
            revision=revision,
            run_id=args.run_id,
            run_attempt=args.run_attempt,
            operation=args.operation,
            output_basename=args.output_basename,
        )
        verified = publication_promotion._verified_manifest(verify_args.out.resolve(), expected)
        assert verified["expected_old_blob"] is None
        assert verified["baseline"] == publication_promotion._binding(
            verify_args.out / args.output_basename, "emitted verified baseline"
        )

        attestation_name = f"{args.run_id}-{args.run_attempt}.json"
        staged_attestation = args.out / attestation_name
        forged = json.loads(staged_attestation.read_bytes())
        forged["authentication"]["mac"] = "f" * 64
        staged_attestation.write_bytes(benchmark_publication.json_bytes(forged))
        source_manifest_path = args.out / publication_promotion.SOURCE_MANIFEST
        source_manifest = json.loads(source_manifest_path.read_bytes())
        source_manifest["files"][attestation_name] = publication_promotion._binding(
            staged_attestation, "forged staged attestation"
        )
        source_manifest_path.write_bytes(benchmark_publication.json_bytes(source_manifest, indent=2))
        forged_args = Namespace(**{**vars(verify_args), "out": root / "forged-verified"})
        expect_rejection(
            lambda: publication_promotion.verify(forged_args, key),
            "not authenticated by protected workflow context",
        )
    finally:
        publication_promotion._validate_schema = original_schema_validation
        publication_promotion.validate_host_proof.validate_document = original_host_validation
        publication_promotion.observer_ab.validate_measurements = original_measurement_validation


def test_exact_verifier_process_consumes_hmac_before_child(root: Path) -> None:
    variable = benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV
    key = "e" * 64
    previous_argv = sys.argv
    original_verify = publication_promotion.verify
    observed = False

    def observe_boundary(_args: Namespace, trusted_key: str) -> None:
        nonlocal observed
        assert trusted_key == key
        assert variable not in os.environ
        child = subprocess.run(
            [sys.executable, "-c", f"import os; raise SystemExit({variable!r} in os.environ)"],
            check=False,
            env=os.environ.copy(),
        )
        assert child.returncode == 0
        observed = True

    try:
        os.environ[variable] = key
        publication_promotion.verify = observe_boundary
        sys.argv = [
            "publication_promotion.py",
            "verify",
            "--repository",
            benchmark_publication.CANONICAL_REPOSITORY,
            "--revision",
            "a" * 40,
            "--run-id",
            "1",
            "--run-attempt",
            "1",
            "--operation",
            "bootstrap",
            "--output-basename",
            publication_promotion.OFFICIAL_OUTPUT.name,
            "--bundle",
            str(root),
            "--out",
            str(root / "out"),
        ]
        assert publication_promotion.main() == 0
        assert observed

        os.environ[variable] = key
        expect_rejection(
            lambda: publication_promotion._git(root, "status"),
            "must not reach a child process",
        )
    finally:
        os.environ.pop(variable, None)
        publication_promotion.verify = original_verify
        sys.argv = previous_argv


def test_workflow_authority_split() -> None:
    workflow = WORKFLOW.read_text(encoding="utf-8")
    dedicated = workflow.split("  dedicated:", 1)[1].split("  verify-publication:", 1)[0]
    verifier = workflow.split("  verify-publication:", 1)[1].split("  promote-publication:", 1)[0]
    promoter = workflow.split("  promote-publication:", 1)[1].split("  github-hosted-negative:", 1)[0]

    assert "CANONICAL_REPOSITORY: oxhq/pliego" in workflow
    assert "OxHQ/pliego" not in workflow
    assert "contents: write" not in dedicated
    assert "pull-requests: write" not in dedicated
    assert "publication_source_artifact_id" in dedicated
    assert "publication_source_artifact_digest" in dedicated
    assert "artifact-id" in dedicated and "artifact-digest" in dedicated
    assert "publication_promotion.py stage" in dedicated

    assert "actions: read\n      contents: read" in verifier
    assert "contents: write" not in verifier
    assert "PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX" in verifier
    assert "publication_promotion.py verify" in verifier
    assert "needs.dedicated.outputs.publication_source_artifact_id" in verifier
    assert "needs.dedicated.outputs.publication_source_artifact_digest" in verifier
    assert "verified_baseline_artifact_id" in verifier
    assert "verified_baseline_artifact_digest" in verifier
    assert '[[ "$ARTIFACT_DIGEST" =~ ^[0-9a-f]{64}$ ]]' in verifier
    assert '--arg digest "sha256:$ARTIFACT_DIGEST"' in verifier
    assert ".workflow_run.id == $run_id" in verifier
    assert ".workflow_run.head_sha == $revision" in verifier
    assert ".workflow_run.head_branch == $branch" in verifier

    assert "actions: read\n      contents: read" in promoter
    assert "contents: write" not in promoter
    assert "pull-requests: write" not in promoter
    assert "environment: Pliego benchmark promotion" in promoter
    assert "secrets.PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX" not in promoter
    assert "secrets.OXHQ_BENCHMARK_PROMOTION_TOKEN" in promoter
    assert promoter.count("secrets.OXHQ_BENCHMARK_PROMOTION_TOKEN") == 1
    assert "PLIEGO_BENCHMARK_PROOF_TOKEN" not in promoter
    assert 'test -z "${PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX:-}"' in promoter
    assert "needs.verify-publication.outputs.verified_baseline_artifact_id" in promoter
    assert "publication_promotion.py prepare" in promoter
    assert 'branch="benchmark-baseline/$GITHUB_SHA/$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT-$OPERATION"' in promoter
    assert 'expected_worktree_status=" M $BASELINE_PATH"' in promoter
    assert 'expected_worktree_status="?? $BASELINE_PATH"' in promoter
    assert "git status --short --untracked-files=all" in promoter
    assert "gh pr list" in promoter and "--state all" in promoter
    assert "headRefName,headRepository,isCrossRepository" in promoter
    assert "publication_promotion.py select-promotion-prs" in promoter
    assert '--repository "$CANONICAL_REPOSITORY"' in promoter
    assert "gh pr create" in promoter and '--base "$DEFAULT_BRANCH"' in promoter
    assert 'git push origin "HEAD:refs/heads/$branch"' in promoter
    assert "git push origin main" not in promoter
    assert "HEAD:refs/heads/$DEFAULT_BRANCH" not in promoter
    assert "main advanced after the benchmark" in promoter
    assert 'test "$DEFAULT_BRANCH" = main' in verifier and 'test "$DEFAULT_BRANCH" = main' in promoter
    assert "publication_promotion.py verify-github-commit" in promoter
    assert 'test "$(git rev-parse HEAD^)" = "$GITHUB_SHA"' in promoter
    assert "OPEN|MERGED" in promoter and "existing exact promotion PR was closed" in promoter
    assert "if ! git push origin" in promoter
    assert "if ! created_pr=" in promoter
    assert promoter.count('existing_pr="$(list_exact_promotion_prs)"') == 3
    post_create = promoter.split("# A successful create can still race", 1)[1]
    assert 'existing_pr="$(list_exact_promotion_prs)"' in post_create
    assert 'verified_pr="$(verify_single_pr "$existing_pr")"' in post_create
    assert 'test "$created_pr" = "$verified_pr"' in post_create
    assert "artifact-ids:" in verifier and "artifact-ids:" in promoter
    assert '--arg digest "sha256:$ARTIFACT_DIGEST"' in promoter
    assert "expected_old_blob" not in workflow  # old-blob authority lives in the verified artifact, not shell input
    assert workflow.count("secrets.PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX") == 3

    verify_sensitive = verifier.split("- name: Revalidate HMAC-bound publication source", 1)[1].split(
        "- name: Retain newly verified baseline only", 1
    )[0]
    assert verify_sensitive.count("secrets.PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX") == 1
    assert "publication_promotion.py verify" in verify_sensitive
    prepare_step = promoter.split("- name: Prepare exact old-blob-bound baseline change", 1)[1].split(
        "- name: Create or reuse review-only promotion", 1
    )[0]
    mutation_step = promoter.split("- name: Create or reuse review-only promotion", 1)[1]
    assert "OXHQ_BENCHMARK_PROMOTION_TOKEN" not in prepare_step
    assert "secrets.PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX" not in mutation_step
    assert "OXHQ_BENCHMARK_PROMOTION_TOKEN" in mutation_step


def main() -> None:
    previous = os.environ.pop(benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV, None)
    try:
        test_canonical_repository_identity()
        test_fork_safe_pull_request_selection()
        test_exact_github_commit_contract()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw).resolve()
            test_source_artifact_contract(root / "source")
            test_old_blob_transition(root / "old-blob")
            test_verified_artifact_contract(root / "verified")
            test_hmac_reverification_and_verified_emission(root / "hmac-reverification")
            process_root = root / "process-boundary"
            process_root.mkdir()
            test_exact_verifier_process_consumes_hmac_before_child(process_root)
        test_workflow_authority_split()
    finally:
        if previous is not None:
            os.environ[benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV] = previous
    print("publication promotion adversarial checks passed")


if __name__ == "__main__":
    main()
