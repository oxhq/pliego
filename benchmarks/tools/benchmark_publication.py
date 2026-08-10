#!/usr/bin/env python3

"""Fail-closed staging and atomic publication helpers for benchmark evidence."""

from __future__ import annotations

import hashlib
import json
import os
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


def canonical_json_bytes(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode("utf-8")


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


def _require_regular_destination(directory: BoundPublicationDirectory, name: str) -> None:
    try:
        metadata = os.stat(name, dir_fd=directory.descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return
    except OSError as error:
        raise PublicationError(f"cannot inspect baseline destination: {error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise PublicationError("official baseline destination must be a regular non-symlink file")


def atomic_publish_bytes(
    directory: BoundPublicationDirectory,
    name: str,
    payload: bytes,
    *,
    simulate_crash: bool = False,
    before_replace: Callable[[], None] | None = None,
) -> None:
    """Durably replace one basename relative to the already-bound directory."""

    if not name or name in {".", ".."} or Path(name).name != name:
        raise PublicationError("official baseline destination must be one basename")
    _require_current_publication_directory(directory)
    _require_regular_destination(directory, name)
    temporary = f".{name}.{secrets.token_hex(8)}.tmp"
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
        _require_regular_destination(directory, name)
        os.rename(
            temporary,
            name,
            src_dir_fd=directory.descriptor,
            dst_dir_fd=directory.descriptor,
        )
        _require_current_publication_directory(directory)
        os.fsync(directory.descriptor)
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


def bind_publication(payload: dict[str, Any], proof: dict[str, Any], proof_digest: str) -> dict[str, Any]:
    identity = proof.get("identity", {})
    runner = proof.get("runner", {})
    revision = identity.get("sha")
    if not isinstance(revision, str) or len(revision) != 40:
        raise PublicationError("host proof does not bind an exact 40-character revision")
    publication = {
        "host_proof_schema": "pliego.benchmark-host-proof.v1",
        "host_proof_bundle_sha256": proof_digest,
        "harness_revision": revision,
        "runner_id": runner.get("id"),
        "runner_name": runner.get("name"),
    }
    copy = json.loads(json.dumps(payload))
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
    attempt = root / (
        datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ") + f"-{os.getpid()}-{secrets.token_hex(4)}"
    )
    attempt.mkdir(mode=0o700)
    retained_destination: dict[str, str] | None = None
    if retained_path is not None and retained_path.exists():
        retained = retained_path.resolve(strict=True)
        if retained.parent != resolved_root or not retained.name.startswith(".in-progress-") or retained.is_symlink():
            raise PublicationError("retained sample workspace escaped the failure-evidence root")
        retained_destination = {
            "directory": "retained-samples",
            "original_path_prefix": str(retained),
        }
        os.replace(retained, attempt / retained_destination["directory"])
    document = {
        "schema": "pliego.benchmark-failure-evidence",
        "version": 1,
        "reason": reason,
        "candidate": payload,
        "retained_samples": retained_destination,
    }
    candidate_path = attempt / "candidate.json"
    atomic_write_bytes(candidate_path, canonical_json_bytes(document))
    manifest_path = attempt / "SHA256SUMS"
    retained_files, retained_directories = _regular_evidence_tree(attempt, manifest_path)
    retained_files.sort(key=lambda path: path.relative_to(attempt).as_posix())
    manifest = "".join(
        f"{sha256_file(path)}  {path.relative_to(attempt).as_posix()}\n" for path in retained_files
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
    os.chmod(attempt, stat.S_IRUSR | stat.S_IXUSR | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH)
    return attempt


def die(message: str) -> NoReturn:
    raise PublicationError(message)
