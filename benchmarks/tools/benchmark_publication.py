#!/usr/bin/env python3

"""Fail-closed staging and atomic publication helpers for benchmark evidence."""

from __future__ import annotations

import hashlib
import hmac
import json
import math
import os
import re
import secrets
import stat
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, NoReturn


class PublicationError(RuntimeError):
    """A candidate cannot become an official baseline."""


class PublicationInterrupted(PublicationError):
    """Test-only simulated interruption before the atomic replacement."""


MAX_BOUND_JSON_BYTES = 512 * 1024 * 1024
TRUSTED_ATTESTATION_KEY_ENV = "PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX"
TRUSTED_ATTESTATION_ROOT = Path("/var/lib/pliego-benchmark-attestations")
CANONICAL_REPOSITORY = "oxhq/pliego"
TRUSTED_WORKFLOW_REF = f"{CANONICAL_REPOSITORY}/.github/workflows/pliego-dedicated-benchmark.yml@refs/heads/main"


@dataclass(frozen=True)
class TrustedPublicationAttestation:
    document: dict[str, Any]
    sha256: str
    path: Path


@dataclass
class BoundPublicationDirectory:
    """One non-following directory descriptor used as publication authority."""

    path: Path
    descriptor: int
    device: int
    inode: int

    def close(self) -> None:
        if self.descriptor >= 0:
            os.close(self.descriptor)
            self.descriptor = -1

    def __enter__(self) -> BoundPublicationDirectory:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def require_unprivileged(uid: int | None = None, euid: int | None = None) -> None:
    if uid is None or euid is None:
        if os.name != "posix":
            return
        real = os.getuid()
        effective = os.geteuid()
    else:
        real = uid
        effective = euid
    if real == 0 or effective == 0:
        raise PublicationError("benchmark orchestration and publication must never run as root")


def sha256_file(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def stable_stat_identity(metadata: os.stat_result) -> tuple[int, ...]:
    stable = (metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns)
    return (*stable, metadata.st_ctime_ns) if os.name == "posix" else stable


def _reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON constant {value!r} is forbidden")


def require_finite_json(value: Any, path: str = "$") -> None:
    """Reject every non-finite number before evidence is trusted or serialized."""

    if isinstance(value, float) and not math.isfinite(value):
        raise ValueError(f"{path} contains a non-finite number")
    if isinstance(value, dict):
        for key, child in value.items():
            require_finite_json(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            require_finite_json(child, f"{path}[{index}]")


def strict_json_loads(raw: str | bytes | bytearray, label: str) -> Any:
    try:
        value = json.loads(raw, parse_constant=_reject_json_constant)
        require_finite_json(value)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise PublicationError(f"{label} is invalid strict JSON: {error}") from error
    return value


def json_bytes(value: Any, *, indent: int | None = None, trailing_newline: bool = True) -> bytes:
    try:
        require_finite_json(value)
        rendered = json.dumps(
            value,
            indent=indent,
            sort_keys=indent is None,
            separators=(",", ":") if indent is None else None,
            ensure_ascii=False,
            allow_nan=False,
        )
    except (TypeError, ValueError) as error:
        raise PublicationError(f"cannot serialize strict JSON: {error}") from error
    return (rendered + ("\n" if trailing_newline else "")).encode("utf-8")


def canonical_json_bytes(value: Any) -> bytes:
    return json_bytes(value)


def _attestation_key(value: str) -> bytes:
    if re.fullmatch(r"[0-9a-f]{64}", value or "") is None:
        raise PublicationError("protected publication authority omitted its 256-bit authentication key")
    return bytes.fromhex(value)


def consume_trusted_attestation_key() -> str:
    """Remove protected HMAC authority before any later child can inherit it."""

    return os.environ.pop(TRUSTED_ATTESTATION_KEY_ENV, "")


def authenticate_publication_attestation(document: dict[str, Any], key_hex: str) -> dict[str, Any]:
    """Attach a MAC from protected workflow context that candidate bytes never receive."""

    unsigned = {key: value for key, value in document.items() if key != "authentication"}
    mac = hmac.new(_attestation_key(key_hex), canonical_json_bytes(unsigned), hashlib.sha256).hexdigest()
    return {**unsigned, "authentication": {"method": "hmac-sha256-v1", "mac": mac}}


def load_bound_json_object(
    path: Path,
    label: str,
    *,
    max_bytes: int = MAX_BOUND_JSON_BYTES,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Read one canonical regular JSON object through a non-following file FD."""

    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise PublicationError(f"cannot bind {label}: {error}") from error
    if path != resolved or path.is_symlink():
        raise PublicationError(f"{label} must be one absolute canonical non-symlink file")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise PublicationError(f"cannot bind {label}: {error}") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size <= 0 or before.st_size > max_bytes:
            raise PublicationError(f"{label} must be a bounded nonempty regular file")
        raw = bytearray()
        while len(raw) < before.st_size:
            chunk = os.read(descriptor, min(1024 * 1024, before.st_size - len(raw)))
            if not chunk:
                raise PublicationError(f"{label} shortened while binding")
            raw.extend(chunk)
        after = os.fstat(descriptor)
        current = os.stat(path, follow_symlinks=False)
        before_identity = stable_stat_identity(before)
        if before_identity != stable_stat_identity(after) or before_identity != stable_stat_identity(current):
            raise PublicationError(f"{label} identity changed while binding")
    finally:
        os.close(descriptor)
    document = strict_json_loads(raw, label)
    if not isinstance(document, dict):
        raise PublicationError(f"{label} must be a JSON object")
    return document, {"path": str(resolved), "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}


def load_trusted_publication_attestation(
    path: Path,
    trusted_key_hex: str,
    *,
    allow_fixture_authority: bool = False,
) -> TrustedPublicationAttestation:
    """Bind authorization whose digest and filesystem authority come from outside the candidate bundle."""

    key = _attestation_key(trusted_key_hex)
    document, binding = load_bound_json_object(path, "publication attestation", max_bytes=64 * 1024)
    expected_keys = {
        "schema",
        "version",
        "status",
        "authority",
        "repository",
        "ref",
        "revision",
        "workflow_ref",
        "run_id",
        "run_attempt",
        "job",
        "runner",
        "artifact_name",
        "subject",
        "authentication",
    }
    if set(document) != expected_keys:
        raise PublicationError("publication attestation keys are not exact")
    fixed = {
        "schema": "pliego.benchmark-publication-attestation",
        "version": 1,
        "status": "authorized",
        "authority": "github-protected-environment-hmac-v1",
        "repository": CANONICAL_REPOSITORY,
        "ref": "refs/heads/main",
        "workflow_ref": TRUSTED_WORKFLOW_REF,
        "job": "dedicated",
    }
    if any(document.get(key) != value for key, value in fixed.items()):
        raise PublicationError("publication attestation is not for the protected Pliego workflow authority")
    authentication = document.get("authentication")
    if (
        not isinstance(authentication, dict)
        or set(authentication) != {"method", "mac"}
        or authentication.get("method") != "hmac-sha256-v1"
        or re.fullmatch(r"[0-9a-f]{64}", str(authentication.get("mac", ""))) is None
    ):
        raise PublicationError("publication attestation authentication is invalid")
    unsigned = {key: value for key, value in document.items() if key != "authentication"}
    expected_mac = hmac.new(key, canonical_json_bytes(unsigned), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(authentication["mac"], expected_mac):
        raise PublicationError("publication attestation was not authenticated by protected workflow context")
    if re.fullmatch(r"[0-9a-f]{40}", str(document.get("revision", ""))) is None:
        raise PublicationError("publication attestation omitted an exact revision")
    for field in ("run_id", "run_attempt"):
        if type(document.get(field)) is not int or document[field] <= 0:
            raise PublicationError(f"publication attestation {field} must be a positive integer")
    runner = document.get("runner")
    if (
        not isinstance(runner, dict)
        or set(runner) != {"id", "name"}
        or type(runner.get("id")) is not int
        or runner["id"] <= 0
        or not isinstance(runner.get("name"), str)
        or not runner["name"]
    ):
        raise PublicationError("publication attestation runner identity is invalid")
    subject = document.get("subject")
    if not isinstance(subject, dict) or set(subject) != {
        "candidate_sha256",
        "host_proof_bundle_sha256",
        "observer_proof_sha256",
        "output_basename",
        "operation",
    }:
        raise PublicationError("publication attestation subject keys are not exact")
    for field in ("candidate_sha256", "host_proof_bundle_sha256", "observer_proof_sha256"):
        if re.fullmatch(r"[0-9a-f]{64}", str(subject.get(field, ""))) is None:
            raise PublicationError(f"publication attestation subject {field} is invalid")
    output_basename = subject.get("output_basename")
    if (
        not isinstance(output_basename, str)
        or Path(output_basename).name != output_basename
        or not output_basename.endswith(".json")
    ):
        raise PublicationError("publication attestation output basename is invalid")
    if subject.get("operation") not in {"bootstrap", "replace"}:
        raise PublicationError("publication attestation operation is invalid")
    if document.get("artifact_name") != f"benchmark-publication-{document['revision']}":
        raise PublicationError("publication attestation artifact name differs from its revision")
    if path.name != f"{document['run_id']}-{document['run_attempt']}.json":
        raise PublicationError("publication attestation basename differs from its run identity")
    if not allow_fixture_authority:
        if os.name != "posix":
            raise PublicationError("trusted publication attestation requires POSIX protected-host authority")
        try:
            root = TRUSTED_ATTESTATION_ROOT.resolve(strict=True)
            root_metadata = root.stat(follow_symlinks=False)
            file_metadata = path.stat(follow_symlinks=False)
        except OSError as error:
            raise PublicationError(f"cannot verify protected publication attestation authority: {error}") from error
        if path.parent != root or root != TRUSTED_ATTESTATION_ROOT or root.is_symlink() or not root.is_dir():
            raise PublicationError("publication attestation must come from the canonical protected-host authority root")
        if (
            root_metadata.st_uid != 0
            or root_metadata.st_gid != 0
            or stat.S_IMODE(root_metadata.st_mode) & 0o022
            or file_metadata.st_uid != 0
            or file_metadata.st_gid != 0
            or stat.S_IMODE(file_metadata.st_mode) & 0o022
            or file_metadata.st_nlink != 1
        ):
            raise PublicationError("publication attestation authority must be root-owned and immutable to the runner")
    return TrustedPublicationAttestation(document=document, sha256=binding["sha256"], path=path)


def _fsync_directory(path: Path) -> None:
    if os.name != "posix":
        return
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_write_bytes(path: Path, payload: bytes, *, simulate_crash: bool = False) -> None:
    """Replace ``path`` only after a complete same-directory durable write."""

    path = path.resolve(strict=False)
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, raw_temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    temporary = Path(raw_temporary)
    try:
        os.chmod(temporary, 0o644)
        with os.fdopen(descriptor, "wb", closefd=True) as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        if simulate_crash:
            raise PublicationInterrupted("simulated interruption before baseline replacement")
        os.replace(temporary, path)
        _fsync_directory(path.parent)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def bind_publication_directory(path: Path) -> BoundPublicationDirectory:
    """Bind a canonical directory without following its final component."""

    if os.name != "posix" or os.open not in os.supports_dir_fd or os.rename not in os.supports_dir_fd:
        raise PublicationError("official baseline publication requires POSIX directory-FD operations")
    if not path.is_absolute():
        raise PublicationError("publication directory must be absolute")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise PublicationError(f"cannot bind publication directory: {error}") from error
    if path != resolved or path.is_symlink() or not path.is_dir():
        raise PublicationError("publication directory must be one canonical non-symlink directory")
    flags = os.O_RDONLY | os.O_DIRECTORY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise PublicationError(f"cannot bind publication directory: {error}") from error
    metadata = os.fstat(descriptor)
    current = os.stat(path, follow_symlinks=False)
    if not stat.S_ISDIR(metadata.st_mode) or (metadata.st_dev, metadata.st_ino) != (
        current.st_dev,
        current.st_ino,
    ):
        os.close(descriptor)
        raise PublicationError("publication directory identity changed while binding")
    return BoundPublicationDirectory(path, descriptor, metadata.st_dev, metadata.st_ino)


def _require_current_publication_directory(directory: BoundPublicationDirectory) -> None:
    try:
        descriptor_metadata = os.fstat(directory.descriptor)
        path_metadata = os.stat(directory.path, follow_symlinks=False)
    except OSError as error:
        raise PublicationError(f"publication directory is no longer bound at its canonical path: {error}") from error
    expected = (directory.device, directory.inode)
    if (descriptor_metadata.st_dev, descriptor_metadata.st_ino) != expected or (
        path_metadata.st_dev,
        path_metadata.st_ino,
    ) != expected:
        raise PublicationError("publication directory identity changed before replacement")


def _require_regular_destination(
    directory: BoundPublicationDirectory,
    name: str,
    *,
    require_single_link: bool = True,
) -> os.stat_result:
    try:
        metadata = os.stat(name, dir_fd=directory.descriptor, follow_symlinks=False)
    except FileNotFoundError:
        raise PublicationError("official baseline destination must already exist") from None
    except OSError as error:
        raise PublicationError(f"cannot inspect baseline destination: {error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise PublicationError("official baseline destination must be a regular non-symlink file")
    if require_single_link and metadata.st_nlink != 1:
        raise PublicationError("official baseline destination must have exactly one link before publication")
    return metadata


def _require_absent_destination(directory: BoundPublicationDirectory, name: str) -> None:
    try:
        os.stat(name, dir_fd=directory.descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return
    except OSError as error:
        raise PublicationError(f"cannot inspect baseline bootstrap destination: {error}") from error
    raise PublicationError("official baseline bootstrap destination must not already exist")


def atomic_publish_bytes(
    directory: BoundPublicationDirectory,
    name: str,
    payload: bytes,
    *,
    simulate_crash: bool = False,
    before_replace: Callable[[], None] | None = None,
    after_backup: Callable[[], None] | None = None,
    after_replace: Callable[[], None] | None = None,
) -> None:
    """Journal and durably replace one basename relative to a bound directory."""

    if not name or name in {".", ".."} or Path(name).name != name:
        raise PublicationError("official baseline destination must be one basename")
    _require_current_publication_directory(directory)
    _require_regular_destination(directory, name)
    temporary = f".{name}.{secrets.token_hex(8)}.tmp"
    rollback = f".{name}.{secrets.token_hex(8)}.rollback"
    backup_created = False
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(temporary, flags, 0o644, dir_fd=directory.descriptor)
    try:
        try:
            os.fchmod(descriptor, 0o644)
            offset = 0
            while offset < len(payload):
                written = os.write(descriptor, payload[offset:])
                if written <= 0:
                    raise PublicationError("short write while staging official baseline")
                offset += written
            os.fsync(descriptor)
            if simulate_crash:
                raise PublicationInterrupted("simulated interruption before baseline replacement")
        finally:
            os.close(descriptor)
        if before_replace is not None:
            before_replace()
        _require_current_publication_directory(directory)
        destination_metadata = _require_regular_destination(directory, name)
        os.link(
            name,
            rollback,
            src_dir_fd=directory.descriptor,
            dst_dir_fd=directory.descriptor,
            follow_symlinks=False,
        )
        backup_created = True
        try:
            # From the moment the rollback link exists, every rejected
            # transaction must restore through that link.  In particular, the
            # post-link destination revalidation belongs inside this recovery
            # boundary: an unlink or swap at that point must not cause the
            # only durable copy of the previous baseline to be discarded.
            os.fsync(directory.descriptor)
            if after_backup is not None:
                after_backup()
            linked_metadata = _require_regular_destination(directory, name, require_single_link=False)
            if (linked_metadata.st_dev, linked_metadata.st_ino) != (
                destination_metadata.st_dev,
                destination_metadata.st_ino,
            ) or linked_metadata.st_nlink != 2:
                raise PublicationError("official baseline destination identity changed after rollback journal creation")
            os.rename(
                temporary,
                name,
                src_dir_fd=directory.descriptor,
                dst_dir_fd=directory.descriptor,
            )
            if after_replace is not None:
                after_replace()
            _require_current_publication_directory(directory)
            os.fsync(directory.descriptor)
        except BaseException:
            if backup_created:
                try:
                    rollback_metadata = os.stat(
                        rollback,
                        dir_fd=directory.descriptor,
                        follow_symlinks=False,
                    )
                    try:
                        current_metadata = os.stat(
                            name,
                            dir_fd=directory.descriptor,
                            follow_symlinks=False,
                        )
                    except FileNotFoundError:
                        current_metadata = None
                    if current_metadata is not None and (
                        current_metadata.st_dev,
                        current_metadata.st_ino,
                    ) == (rollback_metadata.st_dev, rollback_metadata.st_ino):
                        # POSIX rename is a no-op when both hard-link names
                        # identify the same file, so remove the journal name.
                        os.unlink(rollback, dir_fd=directory.descriptor)
                    else:
                        os.rename(
                            rollback,
                            name,
                            src_dir_fd=directory.descriptor,
                            dst_dir_fd=directory.descriptor,
                        )
                    backup_created = False
                    os.fsync(directory.descriptor)
                except OSError as rollback_error:
                    raise PublicationError(
                        f"publication failed and previous baseline remains only in rollback journal {rollback!r}: "
                        f"{rollback_error}"
                    ) from rollback_error
            raise
        # The replacement and parent directory are already durable. Cleanup is
        # deliberately best-effort: losing authority to remove the old journal
        # must not turn a committed publication into a reported failure.
        try:
            os.unlink(rollback, dir_fd=directory.descriptor)
        except OSError:
            pass
        else:
            backup_created = False
            try:
                os.fsync(directory.descriptor)
            except OSError:
                pass
    finally:
        try:
            os.unlink(temporary, dir_fd=directory.descriptor)
        except FileNotFoundError:
            pass


def atomic_bootstrap_bytes(
    directory: BoundPublicationDirectory,
    name: str,
    payload: bytes,
    *,
    simulate_crash: bool = False,
    before_create: Callable[[], None] | None = None,
    after_create: Callable[[], None] | None = None,
) -> None:
    """Durably create one explicitly authorized baseline without replacement."""

    if not name or name in {".", ".."} or Path(name).name != name:
        raise PublicationError("official baseline bootstrap destination must be one basename")
    _require_current_publication_directory(directory)
    _require_absent_destination(directory, name)
    temporary = f".{name}.{secrets.token_hex(8)}.bootstrap"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(temporary, flags, 0o644, dir_fd=directory.descriptor)
    try:
        try:
            os.fchmod(descriptor, 0o644)
            offset = 0
            while offset < len(payload):
                written = os.write(descriptor, payload[offset:])
                if written <= 0:
                    raise PublicationError("short write while staging official baseline bootstrap")
                offset += written
            os.fsync(descriptor)
            staged_metadata = os.fstat(descriptor)
            if simulate_crash:
                raise PublicationInterrupted("simulated interruption before baseline bootstrap")
        finally:
            os.close(descriptor)
        if before_create is not None:
            before_create()
        _require_current_publication_directory(directory)
        _require_absent_destination(directory, name)
        try:
            os.link(
                temporary,
                name,
                src_dir_fd=directory.descriptor,
                dst_dir_fd=directory.descriptor,
                follow_symlinks=False,
            )
        except FileExistsError:
            raise PublicationError("official baseline bootstrap destination must remain absent") from None
        try:
            if after_create is not None:
                after_create()
            _require_current_publication_directory(directory)
            destination_metadata = _require_regular_destination(directory, name, require_single_link=False)
            if (destination_metadata.st_dev, destination_metadata.st_ino) != (
                staged_metadata.st_dev,
                staged_metadata.st_ino,
            ) or destination_metadata.st_nlink != 2:
                raise PublicationError("official baseline bootstrap destination identity changed after creation")
            os.fsync(directory.descriptor)
        except BaseException as failure:
            try:
                current = os.stat(name, dir_fd=directory.descriptor, follow_symlinks=False)
            except FileNotFoundError:
                pass
            except OSError as inspect_error:
                raise PublicationError(
                    f"bootstrap failed and created destination cannot be inspected safely: {inspect_error}"
                ) from failure
            else:
                if (current.st_dev, current.st_ino) != (staged_metadata.st_dev, staged_metadata.st_ino):
                    raise PublicationError(
                        "bootstrap failed after destination identity changed; refusing to unlink a raced target"
                    ) from failure
                try:
                    os.unlink(name, dir_fd=directory.descriptor)
                    os.fsync(directory.descriptor)
                except OSError as rollback_error:
                    raise PublicationError(
                        f"bootstrap failed and the created destination could not be removed safely: {rollback_error}"
                    ) from failure
            raise
        # The destination is now a durable link to the fully written payload.
        # Cleanup cannot invalidate it, so an unremovable staging link is only
        # stale debris and must not turn a committed bootstrap into a failure.
        try:
            os.unlink(temporary, dir_fd=directory.descriptor)
        except OSError:
            pass
        else:
            try:
                os.fsync(directory.descriptor)
            except OSError:
                pass
    finally:
        try:
            os.unlink(temporary, dir_fd=directory.descriptor)
        except FileNotFoundError:
            pass


def result_documents(payload: dict[str, Any]) -> list[dict[str, Any]]:
    if payload.get("schema") == "pliego.benchmark-result":
        return [payload]
    if payload.get("schema") == "pliego.benchmark-bundle" and isinstance(payload.get("results"), list):
        return [value for value in payload["results"] if isinstance(value, dict)]
    raise PublicationError("candidate is neither a benchmark result nor a benchmark bundle")


def require_supported(payload: dict[str, Any]) -> None:
    documents = result_documents(payload)
    if not documents:
        raise PublicationError("candidate bundle has no result documents")
    failed = [
        str(document.get("fixture", {}).get("id", "unknown"))
        for document in documents
        if document.get("status") != "supported"
        or document.get("aggregates", {}).get("correctness", {}).get("passed") is not True
    ]
    if failed:
        raise PublicationError(f"failed correctness cannot replace a baseline: {', '.join(failed)}")


def host_proof_bundle_digest(proof_path: Path) -> str:
    manifest = proof_path.parent / "SHA256SUMS"
    if proof_path.name != "benchmark-host-proof.v1.json" or not manifest.is_file():
        raise PublicationError("host proof must be accompanied by its retained SHA256SUMS bundle")
    return sha256_file(manifest)


def bind_publication(
    payload: dict[str, Any],
    proof: dict[str, Any],
    proof_digest: str,
    observer_proof_sha256: str,
    candidate_sha256: str,
    output_basename: str,
    operation: str,
    attestation: TrustedPublicationAttestation,
) -> dict[str, Any]:
    identity = proof.get("identity", {})
    runner = proof.get("runner", {})
    revision = identity.get("sha")
    if not isinstance(revision, str) or len(revision) != 40:
        raise PublicationError("host proof does not bind an exact 40-character revision")
    authorization = attestation.document
    expected_subject = {
        "candidate_sha256": candidate_sha256,
        "host_proof_bundle_sha256": proof_digest,
        "observer_proof_sha256": observer_proof_sha256,
        "output_basename": output_basename,
        "operation": operation,
    }
    if authorization["subject"] != expected_subject:
        raise PublicationError("protected publication attestation does not bind this candidate/proof/observer subject")
    if (
        authorization["revision"] != revision
        or authorization["run_id"] != identity.get("run_id")
        or authorization["run_attempt"] != identity.get("run_attempt")
        or authorization["job"] != identity.get("job")
        or authorization["workflow_ref"] != identity.get("workflow_ref")
        or authorization["runner"] != {"id": runner.get("id"), "name": runner.get("name")}
    ):
        raise PublicationError("protected publication attestation differs from the host run/runner identity")
    publication = {
        "host_proof_schema": "pliego.benchmark-host-proof.v1",
        "host_proof_bundle_sha256": proof_digest,
        "attestation_schema": "pliego.benchmark-publication-attestation.v1",
        "attestation_sha256": attestation.sha256,
        "observer_proof_sha256": observer_proof_sha256,
        "authority": authorization["authority"],
        "harness_revision": revision,
        "github_run_id": authorization["run_id"],
        "github_run_attempt": authorization["run_attempt"],
        "github_job": authorization["job"],
        "github_workflow_ref": authorization["workflow_ref"],
        "runner_id": runner.get("id"),
        "runner_name": runner.get("name"),
    }
    copy = strict_json_loads(json_bytes(payload, trailing_newline=False), "staged candidate")
    for document in result_documents(copy):
        if "publication" in document or document.get("host", {}).get("dedicated") is not False:
            raise PublicationError("staged candidate must not self-assert dedicated publication")
        existing_revision = document.get("toolchain", {}).get("harness_revision")
        if existing_revision != revision:
            raise PublicationError(
                f"candidate harness revision {existing_revision!r} does not match host proof {revision!r}"
            )
        document["publication"] = publication
        document["host"]["dedicated"] = True
    return copy


def _regular_evidence_tree(root: Path, manifest_path: Path) -> tuple[list[Path], list[Path]]:
    files: list[Path] = []
    directories: list[Path] = []
    for raw_directory, raw_directories, raw_files in os.walk(root, followlinks=False):
        directory = Path(raw_directory)
        raw_directories.sort()
        raw_files.sort()
        for name in raw_directories:
            path = directory / name
            if not stat.S_ISDIR(path.stat(follow_symlinks=False).st_mode):
                raise PublicationError(f"failure evidence contains a symlink or special directory: {path}")
            directories.append(path)
        for name in raw_files:
            path = directory / name
            if path == manifest_path:
                continue
            metadata = path.stat(follow_symlinks=False)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                raise PublicationError(f"failure evidence contains a symlink, special file, or hard link: {path}")
            files.append(path)
    return files, directories


def _chmod_if_owned(path: Path, mode: int) -> None:
    if os.name != "posix":
        os.chmod(path, mode)
    elif path.stat(follow_symlinks=False).st_uid == os.geteuid():
        os.chmod(path, mode, follow_symlinks=False)


def require_private_failure_root(root: Path) -> Path:
    """Create a missing root as 0700 or strictly validate an existing one."""

    if not root.is_absolute():
        raise PublicationError("failure-evidence root must be absolute")
    try:
        parent = root.parent.resolve(strict=False)
    except OSError as error:
        raise PublicationError(f"failure-evidence parent cannot be resolved: {error}") from error
    if root.parent != parent:
        raise PublicationError("failure-evidence parent path must be canonical and non-symlinked")
    created = False
    try:
        root.mkdir(parents=True, mode=0o700)
        created = True
    except FileExistsError:
        pass
    if created and os.name == "posix":
        os.chmod(root, 0o700, follow_symlinks=False)
    resolved_root = root.resolve(strict=True)
    if root != resolved_root or root.is_symlink() or not root.is_dir():
        raise PublicationError("failure-evidence root must be a canonical directory")
    if os.name == "posix":
        metadata = root.stat(follow_symlinks=False)
        if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
            raise PublicationError("failure-evidence root must be runner-owned with mode 0700")
    return resolved_root


def write_failure_evidence(
    root: Path,
    payload: dict[str, Any],
    reason: str,
    retained_path: Path | None = None,
) -> Path:
    """Retain immutable-by-mode failure evidence without touching a baseline."""

    resolved_root = require_private_failure_root(root)
    basename = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ") + f"-{os.getpid()}-{secrets.token_hex(4)}"
    building = root / f".building-{basename}"
    final = root / basename
    quarantine = root / f".quarantine-{basename}"
    building.mkdir(mode=0o700)
    try:
        retained_destination: dict[str, str] | None = None
        if retained_path is not None and retained_path.exists():
            retained = retained_path.resolve(strict=True)
            if (
                retained.parent != resolved_root
                or not retained.name.startswith(".in-progress-")
                or retained.is_symlink()
            ):
                raise PublicationError("retained sample workspace escaped the failure-evidence root")
            retained_destination = {
                "directory": "retained-samples",
                "original_path_prefix": str(retained),
            }
            os.replace(retained, building / retained_destination["directory"])
        document = {
            "schema": "pliego.benchmark-failure-evidence",
            "version": 1,
            "reason": reason,
            "candidate": payload,
            "retained_samples": retained_destination,
        }
        candidate_path = building / "candidate.json"
        atomic_write_bytes(candidate_path, canonical_json_bytes(document))
        manifest_path = building / "SHA256SUMS"
        retained_files, retained_directories = _regular_evidence_tree(building, manifest_path)
        retained_files.sort(key=lambda path: path.relative_to(building).as_posix())
        manifest = "".join(
            f"{sha256_file(path)}  {path.relative_to(building).as_posix()}\n" for path in retained_files
        ).encode("utf-8")
        atomic_write_bytes(manifest_path, manifest)
        for path in retained_files:
            _chmod_if_owned(path, stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
        os.chmod(manifest_path, stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
        for path in sorted(retained_directories, key=lambda candidate: len(candidate.parts), reverse=True):
            _chmod_if_owned(
                path,
                stat.S_IRUSR | stat.S_IXUSR | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH,
            )
        os.chmod(building, stat.S_IRUSR | stat.S_IXUSR | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH)
        os.replace(building, final)
        _fsync_directory(resolved_root)
        return final
    except BaseException as error:
        try:
            partial = final if final.exists() or final.is_symlink() else building
            if partial.exists():
                os.replace(partial, quarantine)
                _fsync_directory(resolved_root)
        except OSError as quarantine_error:
            raise PublicationError(
                f"failure evidence is incomplete at {building}; quarantine failed: {quarantine_error}"
            ) from error
        if isinstance(error, PublicationError):
            raise
        raise PublicationError(f"failure evidence attempt quarantined at {quarantine}: {error}") from error


def die(message: str) -> NoReturn:
    raise PublicationError(message)
