#!/usr/bin/env python3

"""Stage, verify, and prepare one benchmark-baseline promotion artifact."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parents[1]
sys.path.insert(0, str(TOOLS))

import benchmark_publication  # noqa: E402
import observer_ab  # noqa: E402
import validate_host_proof  # noqa: E402
import validate_result  # noqa: E402


OFFICIAL_OUTPUT = Path("benchmarks/baselines/pliego-0.1.1-linux-x86_64.json")
SOURCE_MANIFEST = "publication-source.v1.json"
VERIFIED_MANIFEST = "baseline-promotion.v1.json"
SOURCE_SCHEMA = "pliego.benchmark-publication-source"
VERIFIED_SCHEMA = "pliego.benchmark-baseline-promotion"
SOURCE_ARTIFACT_PREFIX = "benchmark-publication-source"
VERIFIED_ARTIFACT_PREFIX = "verified-benchmark-baseline"
ATTESTATION_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-publication-attestation.v1.json"
RESULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"
HOST_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-host-proof.v1.json"
OBSERVER_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-observer-proof.v1.json"


def _error(message: str) -> benchmark_publication.PublicationError:
    return benchmark_publication.PublicationError(message)


def _sha256(path: Path) -> str:
    return benchmark_publication.sha256_file(path)


def _binding(path: Path, label: str) -> dict[str, Any]:
    resolved = _regular(path, label)
    return {"sha256": _sha256(resolved), "bytes": resolved.stat().st_size}


def _regular(path: Path, label: str, *, nonempty: bool = True) -> Path:
    try:
        resolved = path.resolve(strict=True)
        metadata = path.stat(follow_symlinks=False)
    except OSError as error:
        raise _error(f"cannot bind {label}: {error}") from error
    if path != resolved or path.is_symlink() or not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise _error(f"{label} must be one canonical single-link regular file")
    if nonempty and metadata.st_size <= 0:
        raise _error(f"{label} must not be empty")
    return resolved


def _directory(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise _error(f"{label} must be an absolute path")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise _error(f"cannot bind {label}: {error}") from error
    if path != resolved or path.is_symlink() or not path.is_dir():
        raise _error(f"{label} must be one canonical non-symlink directory")
    return resolved


def _new_output(path: Path, label: str) -> Path:
    if not path.is_absolute() or not path.name or path.name in {".", ".."}:
        raise _error(f"{label} must be one absolute output basename")
    parent = _directory(path.parent, f"{label} parent")
    if path != parent / path.name:
        raise _error(f"{label} parent is not canonical")
    if path.exists() or path.is_symlink():
        raise _error(f"{label} must not already exist")
    return path


def _exact_entries(root: Path, files: set[str], directories: set[str] | None = None) -> None:
    directories = set() if directories is None else directories
    actual_files = {entry.name for entry in root.iterdir() if entry.is_file() and not entry.is_symlink()}
    actual_directories = {entry.name for entry in root.iterdir() if entry.is_dir() and not entry.is_symlink()}
    actual_other = {entry.name for entry in root.iterdir()} - actual_files - actual_directories
    if actual_files != files or actual_directories != directories or actual_other:
        raise _error(
            "artifact surface is not exact: "
            f"files={sorted(actual_files)}, directories={sorted(actual_directories)}, other={sorted(actual_other)}"
        )


def _load_object(path: Path, label: str) -> dict[str, Any]:
    document, _ = benchmark_publication.load_bound_json_object(path, label, max_bytes=4 * 1024 * 1024)
    return document


def _validate_schema(document: dict[str, Any], schema_path: Path, label: str) -> None:
    schema = _load_object(schema_path.resolve(), f"{label} schema")
    violations: list[validate_result.Violation] = []
    validate_result.validate(document, schema, "$", violations)
    if violations:
        raise _error(f"{label} schema violation: {violations[0]}")


def _identity(
    *, repository: str, revision: str, run_id: int, run_attempt: int, operation: str, output_basename: str
) -> dict[str, Any]:
    if repository != "OxHQ/pliego":
        raise _error("publication promotion requires the canonical OxHQ/pliego repository")
    if re.fullmatch(r"[0-9a-f]{40}", revision) is None:
        raise _error("publication promotion requires one exact 40-character revision")
    if type(run_id) is not int or run_id <= 0 or type(run_attempt) is not int or run_attempt <= 0:
        raise _error("publication promotion requires positive run identity integers")
    if operation not in {"bootstrap", "replace"}:
        raise _error("publication promotion operation must be bootstrap or replace")
    if output_basename != OFFICIAL_OUTPUT.name:
        raise _error("publication promotion output basename is not the one authorized baseline")
    return {
        "repository": repository,
        "revision": revision,
        "run_id": run_id,
        "run_attempt": run_attempt,
        "operation": operation,
        "output_path": OFFICIAL_OUTPUT.as_posix(),
        "output_basename": output_basename,
    }


def _artifact_name(prefix: str, identity: dict[str, Any]) -> str:
    return f"{prefix}-{identity['revision']}-{identity['operation']}-{identity['run_id']}-{identity['run_attempt']}"


def _require_identity(document: dict[str, Any], expected: dict[str, Any], schema: str) -> None:
    fixed_keys = {"schema", "version", *expected}
    if document.get("schema") != schema or document.get("version") != 1:
        raise _error("promotion artifact schema/version is invalid")
    if any(document.get(key) != value for key, value in expected.items()):
        raise _error("promotion artifact run/revision/operation/path identity was substituted")
    if not fixed_keys.issubset(document):
        raise _error("promotion artifact identity keys are incomplete")


def _git(root: Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    if benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV in os.environ:
        raise _error("publication HMAC authority must not reach a child process")
    result = subprocess.run(
        ["git", *arguments],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if check and result.returncode != 0:
        raise _error(f"git {' '.join(arguments)} failed: {result.stderr.strip()}")
    return result


def _require_revision(root: Path, revision: str) -> None:
    result = _git(root, "cat-file", "-e", f"{revision}^{{commit}}", check=False)
    if result.returncode != 0:
        raise _error("benchmarked revision is not a local commit")


def _blob_at(root: Path, revision: str, relative: Path) -> str | None:
    _require_revision(root, revision)
    entry = _git(root, "ls-tree", revision, "--", relative.as_posix()).stdout.rstrip("\n")
    if not entry:
        return None
    try:
        metadata, entry_path = entry.split("\t", 1)
        mode, object_type, digest = metadata.split(" ", 2)
    except ValueError as error:
        raise _error("official baseline revision entry is malformed") from error
    if (
        entry_path != relative.as_posix()
        or mode != "100644"
        or object_type != "blob"
        or re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", digest) is None
    ):
        raise _error("official baseline revision entry must be one mode-100644 Git blob at the exact path")
    return digest


def _expected_old_blob(root: Path, revision: str, operation: str) -> str | None:
    current = _blob_at(root, revision, OFFICIAL_OUTPUT)
    if operation == "bootstrap":
        if current is not None:
            raise _error(
                "bootstrap authority requires the exact baseline path to be absent at the benchmarked revision"
            )
        return None
    if current is None:
        raise _error("replace authority requires an existing baseline blob at the benchmarked revision")
    return current


def _copy_file(source: Path, target: Path, label: str, *, nonempty: bool = True) -> None:
    source = _regular(source, label, nonempty=nonempty)
    if target.exists() or target.is_symlink():
        raise _error(f"refusing to overwrite staged {label}")
    shutil.copyfile(source, target, follow_symlinks=False)
    os.chmod(target, 0o644)


def _publish_directory(temporary: Path, output: Path) -> None:
    if output.exists() or output.is_symlink():
        raise _error("promotion artifact output must not already exist")
    os.replace(temporary, output)
    benchmark_publication._fsync_directory(output.parent)


def stage(args: argparse.Namespace) -> None:
    if benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV in os.environ:
        raise _error("source staging must not inherit publication HMAC authority")
    root = _directory(args.repository_root, "repository root")
    expected = _identity(
        repository=args.repository,
        revision=args.revision,
        run_id=args.run_id,
        run_attempt=args.run_attempt,
        operation=args.operation,
        output_basename=args.output_basename,
    )
    supplied_baseline = args.baseline if args.baseline.is_absolute() else root / args.baseline
    if supplied_baseline != root / OFFICIAL_OUTPUT:
        raise _error("published baseline argument is not the exact authorized repository path")
    baseline = _regular(supplied_baseline, "published baseline")
    if baseline != root / OFFICIAL_OUTPUT:
        raise _error("published baseline path is not the exact authorized repository path")
    expected_old_blob = _expected_old_blob(root, args.revision, args.operation)
    attestation_name = f"{args.run_id}-{args.run_attempt}.json"
    if args.attestation.name != attestation_name:
        raise _error("publication attestation basename differs from this run/attempt")
    host_root = _directory(args.host_proof_dir, "host-proof bundle")
    _exact_entries(
        host_root,
        set(validate_host_proof.ARTIFACT_NAMES) | {validate_host_proof.MANIFEST_NAME},
    )
    output = _new_output(args.out, "publication source artifact")
    temporary = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        host_target = temporary / "host-proof"
        host_target.mkdir(mode=0o755)
        sources = {
            "candidate.json": args.candidate,
            "observer-proof.json": args.observer_proof,
            attestation_name: args.attestation,
            args.output_basename: baseline,
        }
        for name, source in sources.items():
            _copy_file(source, temporary / name, name)
        for name in sorted(validate_host_proof.ARTIFACT_NAMES | {validate_host_proof.MANIFEST_NAME}):
            _copy_file(
                host_root / name,
                host_target / name,
                f"host proof {name}",
                nonempty=name not in {validate_host_proof.STDOUT_NAME, validate_host_proof.STDERR_NAME},
            )
        artifact_name = _artifact_name(SOURCE_ARTIFACT_PREFIX, expected)
        bindings = {name: _binding(temporary / name, name) for name in sources}
        bindings["host-proof/SHA256SUMS"] = _binding(
            host_target / validate_host_proof.MANIFEST_NAME, "host proof manifest"
        )
        manifest = {
            "schema": SOURCE_SCHEMA,
            "version": 1,
            **expected,
            "artifact_name": artifact_name,
            "expected_old_blob": expected_old_blob,
            "files": bindings,
        }
        benchmark_publication.atomic_write_bytes(
            temporary / SOURCE_MANIFEST,
            benchmark_publication.json_bytes(manifest, indent=2),
        )
        _publish_directory(temporary, output)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def _source_manifest(bundle: Path, expected: dict[str, Any]) -> dict[str, Any]:
    attestation_name = f"{expected['run_id']}-{expected['run_attempt']}.json"
    expected_files = {
        SOURCE_MANIFEST,
        "candidate.json",
        "observer-proof.json",
        attestation_name,
        expected["output_basename"],
    }
    _exact_entries(bundle, expected_files, {"host-proof"})
    manifest = _load_object(bundle / SOURCE_MANIFEST, "publication source manifest")
    required = {
        "schema",
        "version",
        *expected,
        "artifact_name",
        "expected_old_blob",
        "files",
    }
    if set(manifest) != required:
        raise _error("publication source manifest keys are not exact")
    _require_identity(manifest, expected, SOURCE_SCHEMA)
    expected_artifact = _artifact_name(SOURCE_ARTIFACT_PREFIX, expected)
    if manifest["artifact_name"] != expected_artifact:
        raise _error("publication source artifact name was substituted")
    old = manifest["expected_old_blob"]
    if expected["operation"] == "bootstrap":
        if old is not None:
            raise _error("bootstrap source must bind an absent old blob")
    elif not isinstance(old, str) or re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", old) is None:
        raise _error("replace source must bind one exact old blob")
    expected_binding_names = {
        "candidate.json",
        "observer-proof.json",
        attestation_name,
        expected["output_basename"],
        "host-proof/SHA256SUMS",
    }
    if not isinstance(manifest["files"], dict) or set(manifest["files"]) != expected_binding_names:
        raise _error("publication source file bindings are not exact")
    for name in expected_binding_names:
        value = manifest["files"][name]
        if not isinstance(value, dict) or set(value) != {"sha256", "bytes"}:
            raise _error(f"publication source binding is invalid: {name}")
        actual = _binding(bundle / Path(name), f"publication source {name}")
        if value != actual:
            raise _error(f"publication source file was substituted: {name}")
    return manifest


def _validate_observer_copy(document: dict[str, Any], proof: dict[str, Any], proof_digest: str) -> None:
    _validate_schema(document, OBSERVER_SCHEMA, "observer proof")
    violations = observer_ab.validate_measurements(document["measurements"])
    canonical_measurements = hashlib.sha256(observer_ab.canonical_bytes(document["measurements"])).hexdigest()
    if canonical_measurements != document["measurements_binding"]["canonical_sha256"]:
        violations.append("observer measurements canonical digest differs from the embedded document")
    source_digest = _sha256(observer_ab.SOURCE_BROKER.resolve())
    expected_binding = {
        "harness_revision": proof["identity"]["sha"],
        "source_broker_sha256": source_digest,
        # The protected publisher already proved the installed copy equals the
        # source. The clean verifier has no privileged host installation.
        "installed_broker_sha256": source_digest,
        "host_config_sha256": proof["host_config"]["sha256"],
        "host_proof_bundle_sha256": proof_digest,
        "runner_id": proof["runner"]["id"],
        "runner_name": proof["runner"]["name"],
    }
    if document["binding"] != expected_binding:
        violations.append("observer proof binding differs from the transported host proof")
    if document["measurements_binding"] != proof["command"].get("observer_measurements"):
        violations.append("observer measurement binding differs from the host proof command")
    if violations:
        raise _error(f"downloaded observer proof is invalid: {violations[0]}")


def verify(args: argparse.Namespace, trusted_key: str) -> None:
    expected = _identity(
        repository=args.repository,
        revision=args.revision,
        run_id=args.run_id,
        run_attempt=args.run_attempt,
        operation=args.operation,
        output_basename=args.output_basename,
    )
    bundle = _directory(args.bundle, "downloaded publication source")
    manifest = _source_manifest(bundle, expected)
    candidate, candidate_binding = benchmark_publication.load_bound_json_object(
        bundle / "candidate.json", "downloaded staged candidate"
    )
    observer_document, observer_binding = observer_ab.load_bound_object(
        bundle / "observer-proof.json", "downloaded observer proof"
    )
    host_root = _directory(bundle / "host-proof", "downloaded host-proof bundle")
    _exact_entries(
        host_root,
        set(validate_host_proof.ARTIFACT_NAMES) | {validate_host_proof.MANIFEST_NAME},
    )
    proof = _load_object(host_root / validate_host_proof.PROOF_NAME, "host proof")
    argv = proof.get("command", {}).get("argv")
    if not isinstance(argv, list) or not validate_host_proof.canonical_benchmark_command(argv):
        raise _error("host proof omitted the canonical full benchmark command")
    proof_candidate = proof.get("command", {}).get("candidate")
    transported_candidate = {"sha256": candidate_binding["sha256"], "bytes": candidate_binding["bytes"]}
    if (
        not isinstance(proof_candidate, dict)
        or {
            "sha256": proof_candidate.get("sha256"),
            "bytes": proof_candidate.get("bytes"),
        }
        != transported_candidate
    ):
        raise _error("downloaded staged candidate differs from the host-bound digest")
    host_schema = _load_object(HOST_SCHEMA.resolve(), "host-proof schema")
    host_violations = validate_host_proof.validate_document(
        proof,
        host_schema,
        host_root,
        allow_fixture_evidence=False,
        expected_command=tuple(argv),
    )
    if host_violations:
        raise _error(f"downloaded host proof is invalid: {host_violations[0]}")
    identity = proof.get("identity", {})
    if (
        proof.get("status") != "accepted"
        or proof.get("mode") != "production"
        or identity.get("repository") != args.repository
        or identity.get("sha") != args.revision
        or identity.get("run_id") != args.run_id
        or identity.get("run_attempt") != args.run_attempt
    ):
        raise _error("host proof identity was substituted")
    attestation_name = f"{args.run_id}-{args.run_attempt}.json"
    # Artifact transport cannot preserve the original root ownership. This
    # switch skips only that local ownership check; the MAC and every signed
    # run/subject field are still revalidated below.
    trusted = benchmark_publication.load_trusted_publication_attestation(
        bundle / attestation_name,
        trusted_key,
        allow_fixture_authority=True,
    )
    _validate_schema(trusted.document, ATTESTATION_SCHEMA, "publication attestation")
    proof_digest = benchmark_publication.host_proof_bundle_digest(host_root / validate_host_proof.PROOF_NAME)
    _validate_observer_copy(observer_document, proof, proof_digest)
    expected_subject = {
        "candidate_sha256": candidate_binding["sha256"],
        "host_proof_bundle_sha256": proof_digest,
        "observer_proof_sha256": observer_binding["sha256"],
        "output_basename": args.output_basename,
        "operation": args.operation,
    }
    authorization = trusted.document
    if authorization["subject"] != expected_subject:
        raise _error("publication attestation subject digest/operation/basename was substituted")
    if (
        authorization["revision"] != args.revision
        or authorization["run_id"] != args.run_id
        or authorization["run_attempt"] != args.run_attempt
    ):
        raise _error("publication attestation run/revision identity was substituted")
    benchmark_publication.require_supported(candidate)
    baseline, baseline_binding = benchmark_publication.load_bound_json_object(
        bundle / args.output_basename, "downloaded published baseline"
    )
    _validate_schema(baseline, RESULT_SCHEMA, "published baseline")
    expected_baseline = benchmark_publication.bind_publication(
        candidate,
        proof,
        proof_digest,
        observer_binding["sha256"],
        candidate_binding["sha256"],
        args.output_basename,
        args.operation,
        trusted,
    )
    expected_bytes = benchmark_publication.json_bytes(expected_baseline, indent=2)
    expected_binding = {"sha256": hashlib.sha256(expected_bytes).hexdigest(), "bytes": len(expected_bytes)}
    if {"sha256": baseline_binding["sha256"], "bytes": baseline_binding["bytes"]} != expected_binding:
        raise _error("published baseline bytes differ from the HMAC-bound candidate publication")
    output = _new_output(args.out, "verified promotion artifact")
    temporary = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        # Emit the bytes just reconstructed from the authenticated subject,
        # rather than reopening the untrusted downloaded path after validation.
        benchmark_publication.atomic_write_bytes(temporary / args.output_basename, expected_bytes)
        verified = {
            "schema": VERIFIED_SCHEMA,
            "version": 1,
            **expected,
            "artifact_name": _artifact_name(VERIFIED_ARTIFACT_PREFIX, expected),
            "source_artifact_name": manifest["artifact_name"],
            "source_manifest_sha256": _sha256(bundle / SOURCE_MANIFEST),
            "attestation_sha256": trusted.sha256,
            "expected_old_blob": manifest["expected_old_blob"],
            "baseline": _binding(temporary / args.output_basename, "verified baseline"),
        }
        benchmark_publication.atomic_write_bytes(
            temporary / VERIFIED_MANIFEST,
            benchmark_publication.json_bytes(verified, indent=2),
        )
        _publish_directory(temporary, output)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def _verified_manifest(bundle: Path, expected: dict[str, Any]) -> dict[str, Any]:
    _exact_entries(bundle, {VERIFIED_MANIFEST, expected["output_basename"]})
    document = _load_object(bundle / VERIFIED_MANIFEST, "verified promotion manifest")
    required = {
        "schema",
        "version",
        *expected,
        "artifact_name",
        "source_artifact_name",
        "source_manifest_sha256",
        "attestation_sha256",
        "expected_old_blob",
        "baseline",
    }
    if set(document) != required:
        raise _error("verified promotion manifest keys are not exact")
    _require_identity(document, expected, VERIFIED_SCHEMA)
    if document["artifact_name"] != _artifact_name(VERIFIED_ARTIFACT_PREFIX, expected):
        raise _error("verified promotion artifact name was substituted")
    if document["source_artifact_name"] != _artifact_name(SOURCE_ARTIFACT_PREFIX, expected):
        raise _error("verified source artifact name was substituted")
    for digest in (document["source_manifest_sha256"], document["attestation_sha256"]):
        if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise _error("verified promotion digest is invalid")
    old = document["expected_old_blob"]
    if expected["operation"] == "bootstrap":
        if old is not None:
            raise _error("verified bootstrap promotion must bind an absent old blob")
    elif not isinstance(old, str) or re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", old) is None:
        raise _error("verified replacement promotion omitted its exact old blob")
    actual = _binding(bundle / expected["output_basename"], "verified promotion baseline")
    if document["baseline"] != actual:
        raise _error("verified promotion baseline bytes were substituted")
    return document


def prepare(args: argparse.Namespace) -> None:
    if benchmark_publication.TRUSTED_ATTESTATION_KEY_ENV in os.environ:
        raise _error("repository-write promotion must not inherit publication HMAC authority")
    expected = _identity(
        repository=args.repository,
        revision=args.revision,
        run_id=args.run_id,
        run_attempt=args.run_attempt,
        operation=args.operation,
        output_basename=args.output_basename,
    )
    bundle = _directory(args.bundle, "verified promotion artifact")
    manifest = _verified_manifest(bundle, expected)
    root = _directory(args.repository_root, "promotion checkout")
    head = _git(root, "rev-parse", "HEAD").stdout.strip()
    if head != args.revision:
        raise _error("promotion checkout is not the exact benchmarked main revision")
    actual_old = _expected_old_blob(root, args.revision, args.operation)
    if actual_old != manifest["expected_old_blob"]:
        raise _error("promotion old-blob precondition differs from the verified artifact")
    changed_before = [
        line for line in _git(root, "status", "--short", "--untracked-files=all").stdout.splitlines() if line
    ]
    if changed_before:
        raise _error(f"promotion checkout must start clean: {changed_before}")
    baseline, baseline_binding = benchmark_publication.load_bound_json_object(
        bundle / args.output_basename, "verified promotion baseline"
    )
    _validate_schema(baseline, RESULT_SCHEMA, "verified promotion baseline")
    binding = {"sha256": baseline_binding["sha256"], "bytes": baseline_binding["bytes"]}
    if binding != manifest["baseline"]:
        raise _error("verified promotion baseline changed before preparation")
    payload = benchmark_publication.json_bytes(baseline, indent=2)
    if len(payload) != binding["bytes"] or hashlib.sha256(payload).hexdigest() != binding["sha256"]:
        raise _error("verified promotion baseline is not the exact canonical publication bytes")
    destination = root / OFFICIAL_OUTPUT
    with benchmark_publication.bind_publication_directory(destination.parent) as directory:
        if args.operation == "bootstrap":
            benchmark_publication.atomic_bootstrap_bytes(directory, args.output_basename, payload)
        else:
            current_blob = _git(root, "hash-object", OFFICIAL_OUTPUT.as_posix()).stdout.strip()
            if current_blob != actual_old:
                raise _error("checked-out replacement baseline differs from its exact expected old blob")
            benchmark_publication.atomic_publish_bytes(directory, args.output_basename, payload)
    changed = [line for line in _git(root, "status", "--short", "--untracked-files=all").stdout.splitlines() if line]
    if len(changed) != 1 or changed[0][3:].replace("\\", "/") != OFFICIAL_OUTPUT.as_posix():
        raise _error(f"promotion prepared an unexpected worktree surface: {changed}")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    def identity_arguments(command: argparse.ArgumentParser) -> None:
        command.add_argument("--repository", required=True)
        command.add_argument("--revision", required=True)
        command.add_argument("--run-id", required=True, type=int)
        command.add_argument("--run-attempt", required=True, type=int)
        command.add_argument("--operation", required=True, choices=("bootstrap", "replace"))
        command.add_argument("--output-basename", required=True)

    source = commands.add_parser("stage")
    identity_arguments(source)
    source.add_argument("--repository-root", required=True, type=Path)
    source.add_argument("--candidate", required=True, type=Path)
    source.add_argument("--host-proof-dir", required=True, type=Path)
    source.add_argument("--observer-proof", required=True, type=Path)
    source.add_argument("--attestation", required=True, type=Path)
    source.add_argument("--baseline", required=True, type=Path)
    source.add_argument("--out", required=True, type=Path)

    verifier = commands.add_parser("verify")
    identity_arguments(verifier)
    verifier.add_argument("--bundle", required=True, type=Path)
    verifier.add_argument("--out", required=True, type=Path)

    promotion = commands.add_parser("prepare")
    identity_arguments(promotion)
    promotion.add_argument("--repository-root", required=True, type=Path)
    promotion.add_argument("--bundle", required=True, type=Path)
    return root


def main() -> int:
    args = parser().parse_args()
    trusted_key = benchmark_publication.consume_trusted_attestation_key() if args.command == "verify" else ""
    try:
        if args.command == "stage":
            stage(args)
        elif args.command == "verify":
            verify(args, trusted_key)
        else:
            prepare(args)
    except (OSError, subprocess.SubprocessError, benchmark_publication.PublicationError) as error:
        print(f"publication_promotion: {error}", file=sys.stderr)
        return 1
    print(f"publication promotion {args.command} contract passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
