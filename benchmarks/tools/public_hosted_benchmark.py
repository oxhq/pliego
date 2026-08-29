#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Stage and verify the public view of one packaged hosted benchmark series.

No value in the buyer-facing README is accepted by hand.  Publication copies
the exact series and report from a checksum-verified evidence archive, then
derives the compact README table from that sealed series.

The staging gate consumes the exact, still-live Actions run attempt and artifact
ZIP, proves that ZIP derives the released archive, and commits a provenance
receipt.  Durable verification deliberately does not require that expiring
Actions artifact: it rechecks the receipt, lightweight tag, immutable two-asset
release, and downloaded release bytes.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Iterator, NoReturn, Sequence

import package_hosted_evidence
import run_benchmark
import summarize_comparisons


ROOT = Path(__file__).resolve().parents[2]
RESULT_RELATIVE = Path("docs/benchmarks/results/v0.3.3-minimal-static-github-hosted")
EVIDENCE_MANIFEST = "evidence-manifest.v1.json"
SERIES_FILE = summarize_comparisons.SERIES_FILE
REPORT_FILE = summarize_comparisons.REPORT_FILE
ARCHIVE_CHECKSUM_FILE = "archive.sha256"
SOURCE_PROVENANCE_FILE = "source-provenance.v1.json"
PUBLICATION_ENTRIES = frozenset(
    {EVIDENCE_MANIFEST, SOURCE_PROVENANCE_FILE, SERIES_FILE, REPORT_FILE, ARCHIVE_CHECKSUM_FILE}
)
README_START = "<!-- pliego-hosted-benchmark:start -->"
README_END = "<!-- pliego-hosted-benchmark:end -->"
CLAIM_BOUNDARY = summarize_comparisons.CLAIM_BOUNDARY
RELEASE_TAG = re.compile(r"^benchmark-v0\.3\.3-minimal-static-gh-([1-9][0-9]*)-a([1-9][0-9]*)$")
ARCHIVE_ROOT = re.compile(r"^pliego-benchmark-v0\.3\.3-minimal-static-gh-run-([1-9][0-9]*)-attempt-([1-9][0-9]*)$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
ARTIFACT_DIGEST = re.compile(r"^sha256:([0-9a-f]{64})$")
SOURCE_PROVENANCE_CONTRACT = "pliego.github-actions-source-provenance.v1"
PERFORMANCE_WORKFLOW_PATH = ".github/workflows/pliego-performance.yml"
GITHUB_API = "https://api.github.com"
GITHUB_WEB = "https://github.com"
MAX_ACTIONS_ARTIFACT_BYTES = 8 * 1024 * 1024 * 1024
EXPECTED_ARCHIVE_FILES = frozenset(
    {
        "series/SHA256SUMS",
        "series/all-repeats.md",
        "series/hosted-series.v1.json",
        *(
            f"repeats/repeat-{repeat}/{filename}"
            for repeat in (1, 2, 3)
            for filename in (
                "SHA256SUMS",
                "all-metrics.md",
                "hosted-comparison.v1.json",
                "interleaved-run.v1.json",
                "verified-release.json",
            )
        ),
    }
)


class PublicationError(RuntimeError):
    pass


@dataclass(frozen=True)
class PathIdentity:
    device: int
    inode: int
    mode: int
    links: int
    size: int
    modified_ns: int


@dataclass(frozen=True)
class PublicationTreeIdentity:
    root: PathIdentity
    files: tuple[tuple[str, PathIdentity, str], ...]


def fail(message: str) -> NoReturn:
    raise PublicationError(message)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def load_json_bytes(payload: bytes, label: str) -> dict[str, Any]:
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"cannot decode {label}: {error}")
    require(isinstance(value, dict), f"{label} must contain one JSON object")
    return value


def load_json(path: Path) -> dict[str, Any]:
    payload, _ = _stable_regular_bytes(path, str(path))
    return load_json_bytes(payload, str(path))


def sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _lexists(path: Path) -> bool:
    return os.path.lexists(path)


def _is_reparse_point(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    marker = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & marker)


def _path_identity(path: Path, *, directory: bool) -> PathIdentity:
    try:
        metadata = path.lstat()
    except OSError as error:
        fail(f"cannot inspect transaction path {path}: {error}")
    expected = stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode)
    require(
        expected and not stat.S_ISLNK(metadata.st_mode) and not _is_reparse_point(metadata),
        f"transaction path is not a plain {'directory' if directory else 'file'}: {path}",
    )
    return _metadata_identity(metadata)


def _metadata_identity(metadata: os.stat_result) -> PathIdentity:
    return PathIdentity(
        device=metadata.st_dev,
        inode=metadata.st_ino,
        mode=metadata.st_mode,
        links=metadata.st_nlink,
        size=metadata.st_size,
        modified_ns=metadata.st_mtime_ns,
    )


def _stable_regular_bytes(path: Path, label: str, *, require_single_link: bool = True) -> tuple[bytes, PathIdentity]:
    before = _path_identity(path, directory=False)
    require(not require_single_link or before.links == 1, f"{label} must not be hard-linked")
    try:
        payload = path.read_bytes()
    except OSError as error:
        fail(f"cannot read {label}: {error}")
    after = _path_identity(path, directory=False)
    require(before == after and len(payload) == after.size, f"{label} changed while it was read")
    return payload, after


def _same_plain_file(path: Path, identity: PathIdentity, digest: str) -> bool:
    try:
        payload, observed = _stable_regular_bytes(path, str(path), require_single_link=False)
    except PublicationError:
        return False
    return observed == identity and sha256_bytes(payload) == digest


def _publication_tree_identity(directory: Path) -> PublicationTreeIdentity:
    root = _path_identity(directory, directory=True)
    try:
        entries = list(directory.iterdir())
    except OSError as error:
        fail(f"cannot inspect staged publication directory {directory}: {error}")
    require({entry.name for entry in entries} == PUBLICATION_ENTRIES, "staged publication file set changed")
    files: list[tuple[str, PathIdentity, str]] = []
    for entry in sorted(entries, key=lambda item: item.name.encode("utf-8")):
        payload, identity = _stable_regular_bytes(entry, f"staged {entry.name}")
        files.append((entry.name, identity, sha256_bytes(payload)))
    return PublicationTreeIdentity(root=root, files=tuple(files))


def _ensure_plain_directory_chain(root: Path, relative: Path) -> Path:
    current = root
    _path_identity(current, directory=True)
    for component in relative.parts:
        require(component not in ("", ".", ".."), f"unsafe publication directory component: {component!r}")
        current = current / component
        if _lexists(current):
            _path_identity(current, directory=True)
        else:
            try:
                current.mkdir()
            except OSError as error:
                fail(f"cannot create publication directory {current}: {error}")
            _path_identity(current, directory=True)
    return current


def _same_publication_tree(directory: Path, expected: PublicationTreeIdentity) -> bool:
    try:
        return _publication_tree_identity(directory) == expected
    except PublicationError:
        return False


def _same_directory_object(path: Path, expected: PathIdentity) -> bool:
    """Compare stable directory identity while ignoring entry-driven metadata."""

    try:
        observed = _path_identity(path, directory=True)
    except PublicationError:
        return False
    return (
        observed.device,
        observed.inode,
        observed.mode,
        observed.links,
    ) == (
        expected.device,
        expected.inode,
        expected.mode,
        expected.links,
    )


def _same_directory_identity(observed: PathIdentity, expected: PathIdentity) -> bool:
    return (
        observed.device,
        observed.inode,
        observed.mode,
        observed.links,
    ) == (
        expected.device,
        expected.inode,
        expected.mode,
        expected.links,
    )


@contextmanager
def _bound_directory_linker(directory: Path, expected: PathIdentity) -> Iterator[Callable[[Path, str], None]]:
    """Bind no-clobber entry creation to the reserved directory object."""

    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        class FileId128(ctypes.Structure):
            _fields_ = [("identifier", ctypes.c_ubyte * 16)]

        class FileIdInfo(ctypes.Structure):
            _fields_ = [("volume_serial_number", ctypes.c_ulonglong), ("file_id", FileId128)]

        class FileAttributeTagInfo(ctypes.Structure):
            _fields_ = [("file_attributes", wintypes.DWORD), ("reparse_tag", wintypes.DWORD)]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        create_file = kernel32.CreateFileW
        create_file.argtypes = (
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.LPVOID,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HANDLE,
        )
        create_file.restype = wintypes.HANDLE
        get_information = kernel32.GetFileInformationByHandleEx
        get_information.argtypes = (wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD)
        get_information.restype = wintypes.BOOL
        close_handle = kernel32.CloseHandle
        close_handle.argtypes = (wintypes.HANDLE,)
        close_handle.restype = wintypes.BOOL
        handle = create_file(
            str(directory),
            0x0080,  # FILE_READ_ATTRIBUTES
            0x0001 | 0x0002,  # FILE_SHARE_READ | FILE_SHARE_WRITE; deliberately no DELETE sharing
            None,
            3,  # OPEN_EXISTING
            0x02000000 | 0x00200000,  # BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            None,
        )
        invalid_handle = ctypes.c_void_p(-1).value
        if handle == invalid_handle:
            fail(f"cannot bind reserved publication directory {directory}: Windows error {ctypes.get_last_error()}")
        try:
            identity = FileIdInfo()
            attributes = FileAttributeTagInfo()
            require(
                bool(get_information(handle, 18, ctypes.byref(identity), ctypes.sizeof(identity))),
                f"cannot read reserved publication directory identity: Windows error {ctypes.get_last_error()}",
            )
            require(
                bool(get_information(handle, 9, ctypes.byref(attributes), ctypes.sizeof(attributes))),
                f"cannot read reserved publication directory attributes: Windows error {ctypes.get_last_error()}",
            )
            observed_inode = int.from_bytes(bytes(identity.file_id.identifier), "little")
            require(
                identity.volume_serial_number == expected.device
                and observed_inode == expected.inode
                and bool(attributes.file_attributes & 0x0010)
                and not bool(attributes.file_attributes & 0x0400),
                "opened publication directory is not the exclusively reserved directory object",
            )

            def link_entry(source: Path, filename: str) -> None:
                os.link(source, directory / filename)

            yield link_entry
        finally:
            require(bool(close_handle(handle)), "cannot close the reserved publication directory handle")
        return

    require(
        hasattr(os, "O_DIRECTORY") and hasattr(os, "O_NOFOLLOW") and os.link in os.supports_dir_fd,
        "platform lacks no-follow directory-relative hard-link support",
    )
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    try:
        descriptor = os.open(directory, flags)
    except OSError as error:
        fail(f"cannot bind reserved publication directory {directory}: {error}")
    try:
        observed_metadata = os.fstat(descriptor)
        require(
            stat.S_ISDIR(observed_metadata.st_mode)
            and _same_directory_identity(_metadata_identity(observed_metadata), expected),
            "opened publication directory is not the exclusively reserved directory object",
        )

        def link_entry(source: Path, filename: str) -> None:
            os.link(source, filename, dst_dir_fd=descriptor, follow_symlinks=False)

        yield link_entry
    finally:
        os.close(descriptor)


def source_provenance_sha256(data: dict[str, Any]) -> str:
    return run_benchmark.canonical_json_sha256(
        {key: value for key, value in data.items() if key != "provenance_sha256"}
    )


def normalized_utf8_text(payload: bytes, label: str) -> str:
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError as error:
        fail(f"{label} is not UTF-8: {error}")
    return text.replace("\r\n", "\n").replace("\r", "\n")


def manifest_sha256(data: dict[str, Any]) -> str:
    return run_benchmark.canonical_json_sha256({key: value for key, value in data.items() if key != "manifest_sha256"})


def _release_identity(manifest: dict[str, Any]) -> tuple[str, str, str, str]:
    release = manifest.get("release")
    require(isinstance(release, dict), "evidence manifest release identity is absent")
    require(
        set(release) == {"tag", "archive_root", "archive_filename", "checksum_filename"},
        "evidence manifest release identity shape changed",
    )
    tag = release["tag"]
    root = release["archive_root"]
    archive = release["archive_filename"]
    checksum = release["checksum_filename"]
    require(all(isinstance(value, str) for value in (tag, root, archive, checksum)), "release names must be strings")
    tag_match = RELEASE_TAG.fullmatch(tag)
    root_match = ARCHIVE_ROOT.fullmatch(root)
    require(tag_match is not None and root_match is not None, "release tag or archive root is not canonical")
    assert tag_match is not None and root_match is not None
    run_id, attempt = tag_match.groups()
    require(root_match.groups() == (run_id, attempt), "release tag and archive root identify different runs")
    require(archive == f"{root}.tar.gz", "archive filename does not match its canonical root")
    require(checksum == f"{archive}.sha256", "checksum filename does not match its archive")
    return tag, root, archive, checksum


def validate_evidence_manifest(manifest: dict[str, Any]) -> None:
    require(manifest.get("schema") == "pliego.benchmark-evidence-archive", "evidence manifest schema changed")
    require(manifest.get("version") == 1, "evidence manifest version changed")
    require(manifest.get("evidence_class") == "github-hosted-exploratory", "evidence class is not hosted exploratory")
    require(manifest.get("publication_status") == "packaged-hosted-series", "evidence archive is not publishable")
    require(manifest.get("claim_boundary") == CLAIM_BOUNDARY, "evidence manifest weakened the claim boundary")
    digest = manifest.get("manifest_sha256")
    require(isinstance(digest, str) and SHA256.fullmatch(digest) is not None, "manifest SHA-256 is invalid")
    require(digest == manifest_sha256(manifest), "evidence manifest SHA-256 does not match its canonical content")

    tag, root, _, _ = _release_identity(manifest)
    tag_match = RELEASE_TAG.fullmatch(tag)
    root_match = ARCHIVE_ROOT.fullmatch(root)
    assert tag_match is not None and root_match is not None
    run_id, attempt = (int(value) for value in tag_match.groups())

    source = manifest.get("source")
    require(isinstance(source, dict), "evidence manifest source is absent")
    require(source.get("repository", "").lower() == "oxhq/pliego", "evidence source is not oxhq/pliego")
    require(source.get("run_id") == run_id, "evidence source run does not match the release tag")
    require(source.get("run_attempt") == attempt, "evidence source attempt does not match the release tag")
    require(source.get("ref") == "refs/heads/main", "evidence source is not main")
    require(source.get("event_name") == "workflow_dispatch", "evidence source is not the manual measurement lane")
    require(
        source.get("workflow_ref") == "oxhq/pliego/.github/workflows/pliego-performance.yml@refs/heads/main",
        "evidence source workflow is not the main performance lane",
    )

    target = manifest.get("target")
    require(
        target
        == {
            "id": "pliego-0.3.3",
            "version": "0.3.3",
            "release_tag": "v0.3.3",
            "fixture": "minimal-static",
            "runner_environment": "github-hosted",
        },
        "evidence target is not the exact v0.3.3 minimal-static hosted slice",
    )
    series = manifest.get("series")
    require(isinstance(series, dict), "evidence series binding is absent")
    require(series.get("path") == "series", "evidence series path changed")
    require(
        isinstance(series.get("series_sha256"), str) and SHA256.fullmatch(series["series_sha256"]) is not None,
        "evidence series SHA-256 is invalid",
    )
    repeats = manifest.get("repeats")
    require(isinstance(repeats, list), "evidence repeat bindings are absent")
    require(
        [repeat.get("repeat") for repeat in repeats if isinstance(repeat, dict)] == [1, 2, 3],
        "evidence must retain repeats 1, 2, and 3 in order",
    )

    descriptors = manifest.get("files")
    require(isinstance(descriptors, list) and len(descriptors) == 18, "evidence manifest must bind exactly 18 files")
    paths: list[str] = []
    for descriptor in descriptors:
        require(isinstance(descriptor, dict), "evidence file descriptor is not an object")
        require(set(descriptor) == {"path", "bytes", "sha256"}, "evidence file descriptor shape changed")
        path = descriptor["path"]
        require(isinstance(path, str), "evidence file path is not a string")
        require(isinstance(descriptor["bytes"], int) and descriptor["bytes"] > 0, f"invalid byte count for {path!r}")
        require(
            isinstance(descriptor["sha256"], str) and SHA256.fullmatch(descriptor["sha256"]) is not None,
            f"invalid SHA-256 for {path!r}",
        )
        paths.append(path)
    require(len(paths) == len(set(paths)), "evidence manifest contains duplicate file paths")
    require(set(paths) == EXPECTED_ARCHIVE_FILES, "evidence manifest file inventory changed")


def _expected_release_assets(manifest: dict[str, Any]) -> dict[str, str]:
    tag, _, archive, checksum = _release_identity(manifest)
    base = f"{GITHUB_WEB}/oxhq/pliego/releases/download/{tag}"
    return {
        "release_tag": tag,
        "archive_name": archive,
        "archive_url": f"{base}/{archive}",
        "checksum_name": checksum,
        "checksum_url": f"{base}/{checksum}",
    }


def _actions_run_attestation(metadata: dict[str, Any], manifest: dict[str, Any]) -> dict[str, Any]:
    source = manifest["source"]
    run_id = source["run_id"]
    attempt = source["run_attempt"]
    revision = source["revision"]
    require(metadata.get("id") == run_id, "Actions run metadata identifies a different run")
    require(metadata.get("run_attempt") == attempt, "Actions run metadata identifies a different attempt")
    require(metadata.get("head_sha") == revision, "Actions run head SHA differs from the evidence revision")
    require(metadata.get("head_branch") == "main", "Actions run did not execute from main")
    require(metadata.get("event") == "workflow_dispatch", "Actions run was not manually dispatched")
    require(metadata.get("status") == "completed", "Actions run is not completed")
    require(metadata.get("conclusion") == "success", "Actions run did not conclude successfully")
    require(
        metadata.get("path") in (PERFORMANCE_WORKFLOW_PATH, f"{PERFORMANCE_WORKFLOW_PATH}@main"),
        "Actions run used a different workflow path",
    )
    require(metadata.get("name") == source["workflow"], "Actions run workflow name differs from retained evidence")
    workflow_id = metadata.get("workflow_id")
    require(isinstance(workflow_id, int) and workflow_id > 0, "Actions run workflow ID is invalid")
    repository = metadata.get("repository")
    head_repository = metadata.get("head_repository")
    require(
        isinstance(repository, dict) and repository.get("full_name", "").lower() == "oxhq/pliego",
        "Actions run repository is not oxhq/pliego",
    )
    require(
        isinstance(head_repository, dict) and head_repository.get("full_name", "").lower() == "oxhq/pliego",
        "Actions run head repository is not oxhq/pliego",
    )
    repository_id = repository.get("id")
    require(
        isinstance(repository_id, int) and repository_id > 0 and head_repository.get("id") == repository_id,
        "Actions run repository IDs are invalid or differ",
    )
    return {
        "id": run_id,
        "attempt": attempt,
        "head_sha": revision,
        "head_branch": "main",
        "event": "workflow_dispatch",
        "status": "completed",
        "conclusion": "success",
        "workflow_name": source["workflow"],
        "workflow_path": PERFORMANCE_WORKFLOW_PATH,
        "workflow_id": workflow_id,
        "repository": "oxhq/pliego",
        "repository_id": repository_id,
    }


def _actions_artifact_attestation(
    listing: dict[str, Any], artifact_zip: Path, manifest: dict[str, Any], run_attestation: dict[str, Any]
) -> tuple[dict[str, Any], bytes]:
    source = manifest["source"]
    run_id = source["run_id"]
    attempt = source["run_attempt"]
    revision = source["revision"]
    expected_name = f"pliego-hosted-performance-series-{revision}-{run_id}-{attempt}"
    artifacts = listing.get("artifacts")
    require(listing.get("total_count") == 1, "Actions artifact query did not return exactly one match")
    require(isinstance(artifacts, list) and len(artifacts) == 1, "Actions artifact query result is not exact")
    metadata = artifacts[0]
    require(isinstance(metadata, dict), "Actions artifact query returned an invalid object")
    artifact_id = metadata.get("id")
    require(isinstance(artifact_id, int) and artifact_id > 0, "Actions artifact ID is invalid")
    require(metadata.get("name") == expected_name, "Actions artifact name differs from the exact hosted series")
    require(metadata.get("expired") is False, "Actions artifact is expired")
    digest = metadata.get("digest")
    digest_match = ARTIFACT_DIGEST.fullmatch(digest) if isinstance(digest, str) else None
    require(digest_match is not None, "Actions artifact server digest is not an exact SHA-256")
    expected_url = f"{GITHUB_API}/repos/oxhq/pliego/actions/artifacts/{artifact_id}/zip"
    require(
        metadata.get("archive_download_url") == expected_url,
        "Actions artifact download URL does not match its exact ID",
    )
    workflow_run = metadata.get("workflow_run")
    require(isinstance(workflow_run, dict), "Actions artifact omits its workflow-run identity")
    require(workflow_run.get("id") == run_id, "Actions artifact belongs to a different workflow run")
    require(workflow_run.get("head_sha") == revision, "Actions artifact belongs to a different head SHA")
    require(workflow_run.get("head_branch") == "main", "Actions artifact did not originate from main")
    require(
        workflow_run.get("repository_id") == run_attestation["repository_id"]
        and workflow_run.get("head_repository_id") == run_attestation["repository_id"],
        "Actions artifact repository IDs differ from the run",
    )
    payload, _ = _stable_regular_bytes(artifact_zip, "downloaded Actions artifact ZIP")
    require(len(payload) <= MAX_ACTIONS_ARTIFACT_BYTES, "Actions artifact ZIP exceeds the publication limit")
    require(
        metadata.get("size_in_bytes") == len(payload), "Actions artifact byte count differs from the downloaded ZIP"
    )
    actual_digest = sha256_bytes(payload)
    assert digest_match is not None
    require(actual_digest == digest_match.group(1), "downloaded Actions artifact ZIP differs from its server digest")
    created_at = metadata.get("created_at")
    expires_at = metadata.get("expires_at")
    require(isinstance(created_at, str) and created_at, "Actions artifact creation timestamp is absent")
    require(isinstance(expires_at, str) and expires_at, "Actions artifact expiry timestamp is absent")
    try:
        expires = datetime.fromisoformat(expires_at.replace("Z", "+00:00"))
    except ValueError as error:
        fail(f"Actions artifact expiry timestamp is invalid: {error}")
    require(
        expires.tzinfo is not None and expires > datetime.now(timezone.utc), "Actions artifact expiry is not future"
    )
    return (
        {
            "id": artifact_id,
            "name": expected_name,
            "server_digest": digest,
            "zip_sha256": actual_digest,
            "size_in_bytes": len(payload),
            "expired_at_publication": False,
            "created_at": created_at,
            "expires_at": expires_at,
            "workflow_run_id": run_id,
            "head_sha": revision,
        },
        payload,
    )


def _benchmark_tag_attestation(metadata: dict[str, Any], manifest: dict[str, Any]) -> dict[str, str]:
    tag, _, _, _ = _release_identity(manifest)
    revision = manifest["source"]["revision"]
    require(metadata.get("ref") == f"refs/tags/{tag}", "benchmark tag metadata identifies a different ref")
    target = metadata.get("object")
    require(isinstance(target, dict), "benchmark tag target metadata is absent")
    require(target.get("type") == "commit", "benchmark tag is annotated rather than lightweight")
    require(target.get("sha") == revision, "benchmark tag does not target the evidence source revision")
    require(
        target.get("url") == f"{GITHUB_API}/repos/oxhq/pliego/git/commits/{revision}",
        "benchmark tag target URL differs from the evidence revision",
    )
    return {"ref": f"refs/tags/{tag}", "object_type": "commit", "target_sha": revision}


def _github_release_attestation(
    metadata: dict[str, Any], manifest: dict[str, Any], payloads: dict[str, bytes] | None = None
) -> dict[str, Any]:
    assets = _expected_release_assets(manifest)
    release_id = metadata.get("id")
    require(isinstance(release_id, int) and release_id > 0, "GitHub benchmark release ID is invalid")
    require(metadata.get("tag_name") == assets["release_tag"], "GitHub release tag differs from committed evidence")
    require(metadata.get("immutable") is True, "GitHub benchmark release is not immutable")
    require(metadata.get("draft") is False, "GitHub benchmark release is still a draft")
    require(metadata.get("prerelease") is False, "GitHub benchmark release is marked as a prerelease")
    require(
        metadata.get("html_url") == f"{GITHUB_WEB}/oxhq/pliego/releases/tag/{assets['release_tag']}",
        "GitHub benchmark release URL differs from its tag",
    )

    released_assets = metadata.get("assets")
    require(isinstance(released_assets, list), "GitHub benchmark release assets are absent")
    expected = {
        assets["archive_name"]: assets["archive_url"],
        assets["checksum_name"]: assets["checksum_url"],
    }
    observed: dict[str, tuple[int, str, int, str]] = {}
    for item in released_assets:
        require(isinstance(item, dict), "GitHub benchmark release contains an invalid asset")
        name = item.get("name")
        url = item.get("browser_download_url")
        asset_id = item.get("id")
        size = item.get("size")
        digest = item.get("digest")
        require(isinstance(name, str) and isinstance(url, str), "GitHub benchmark release asset identity is invalid")
        require(isinstance(asset_id, int) and asset_id > 0, f"GitHub benchmark release asset {name!r} has no exact ID")
        require(isinstance(size, int) and size > 0, f"GitHub benchmark release asset {name!r} has no exact size")
        require(
            isinstance(digest, str) and ARTIFACT_DIGEST.fullmatch(digest) is not None,
            f"GitHub benchmark release asset {name!r} has no exact SHA-256 digest",
        )
        require(item.get("state") == "uploaded", f"GitHub benchmark release asset {name!r} is not uploaded")
        require(name not in observed, f"GitHub benchmark release repeats asset {name!r}")
        observed[name] = (asset_id, url, size, digest)
    require(
        {name: url for name, (_, url, _, _) in observed.items()} == expected,
        "GitHub benchmark release does not contain exactly the two committed evidence assets",
    )
    if payloads is not None:
        require(
            set(payloads) == set(expected), "release byte verification did not receive exactly both evidence assets"
        )
        for name, payload in payloads.items():
            _, _, size, digest = observed[name]
            require(size == len(payload), f"GitHub benchmark release asset {name!r} size differs from its bytes")
            require(
                digest == f"sha256:{sha256_bytes(payload)}", f"GitHub benchmark release asset {name!r} digest differs"
            )
    return {
        "id": release_id,
        "tag_name": assets["release_tag"],
        "immutable": True,
        "draft": False,
        "prerelease": False,
        "assets": [
            {
                "id": observed[name][0],
                "name": name,
                "url": expected[name],
                "state": "uploaded",
                "size": observed[name][2],
                "digest": observed[name][3],
            }
            for name in sorted(expected, key=lambda value: value.encode("utf-8"))
        ],
    }


def _zip_member_path(member: zipfile.ZipInfo) -> tuple[PurePosixPath, bool]:
    name = member.filename
    require(name != "" and "\\" not in name and "\x00" not in name, "Actions artifact ZIP has an invalid path")
    directory = member.is_dir()
    canonical_name = name[:-1] if directory and name.endswith("/") else name
    path = PurePosixPath(canonical_name)
    require(not path.is_absolute(), f"Actions artifact ZIP contains an absolute path: {name!r}")
    require(
        canonical_name == path.as_posix() and all(part not in ("", ".", "..") for part in path.parts),
        f"Actions artifact ZIP contains a non-canonical path: {name!r}",
    )
    unix_mode = member.external_attr >> 16
    kind = stat.S_IFMT(unix_mode)
    allowed = (0, stat.S_IFDIR) if directory else (0, stat.S_IFREG)
    require(kind in allowed, f"Actions artifact ZIP contains a linked or special entry: {name!r}")
    require(member.flag_bits & 0x1 == 0, f"Actions artifact ZIP contains an encrypted entry: {name!r}")
    return path, directory


def _extract_actions_artifact(payload: bytes, destination: Path) -> None:
    total = 0
    seen: set[str] = set()
    try:
        with zipfile.ZipFile(io.BytesIO(payload), mode="r") as bundle:
            for member in bundle.infolist():
                relative, directory = _zip_member_path(member)
                key = relative.as_posix().casefold()
                require(key not in seen, f"Actions artifact ZIP repeats or case-collides at {member.filename!r}")
                seen.add(key)
                total += member.file_size
                require(total <= MAX_ACTIONS_ARTIFACT_BYTES, "Actions artifact expands beyond the publication limit")
                target = destination.joinpath(*relative.parts)
                if directory:
                    target.mkdir(parents=True, exist_ok=False)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                require(not _lexists(target), f"Actions artifact extraction path already exists: {relative.as_posix()}")
                written = 0
                with bundle.open(member, mode="r") as source, target.open("xb") as output:
                    while chunk := source.read(1024 * 1024):
                        output.write(chunk)
                        written += len(chunk)
                require(written == member.file_size, f"Actions artifact member size changed: {member.filename!r}")
    except (OSError, zipfile.BadZipFile, zipfile.LargeZipFile) as error:
        fail(f"cannot extract downloaded Actions artifact ZIP: {error}")


def _verify_artifact_derivation(payload: bytes, archive: Path, checksum: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="pliego-actions-artifact-") as temporary:
        temporary_root = Path(temporary)
        extracted = temporary_root / "extracted"
        extracted.mkdir()
        _extract_actions_artifact(payload, extracted)
        try:
            regenerated = package_hosted_evidence.build_evidence_archive(extracted, temporary_root / "evidence")
        except package_hosted_evidence.EvidenceError as error:
            fail(f"downloaded Actions artifact is not the exact hosted evidence source: {error}")
        supplied_archive, _ = _stable_regular_bytes(archive, "supplied evidence archive")
        supplied_checksum, _ = _stable_regular_bytes(checksum, "supplied evidence checksum")
        require(
            regenerated.archive.read_bytes() == supplied_archive,
            "Actions artifact does not derive the supplied archive",
        )
        require(
            regenerated.checksum.read_bytes() == supplied_checksum,
            "Actions artifact does not derive the supplied archive checksum",
        )


def build_source_provenance(
    manifest: dict[str, Any],
    archive: Path,
    checksum: Path,
    run_metadata: dict[str, Any],
    artifact_metadata: dict[str, Any],
    artifact_zip: Path,
    tag_metadata: dict[str, Any],
    release_metadata: dict[str, Any],
) -> dict[str, Any]:
    validate_evidence_manifest(manifest)
    run = _actions_run_attestation(run_metadata, manifest)
    artifact, artifact_payload = _actions_artifact_attestation(artifact_metadata, artifact_zip, manifest, run)
    tag = _benchmark_tag_attestation(tag_metadata, manifest)
    _, _, archive_name, checksum_name = _release_identity(manifest)
    archive_payload, _ = _stable_regular_bytes(archive, "supplied evidence archive")
    checksum_payload, _ = _stable_regular_bytes(checksum, "supplied evidence checksum")
    require(
        parse_archive_checksum(checksum_payload, archive_name) == sha256_bytes(archive_payload),
        "supplied evidence archive differs from its checksum",
    )
    release = _github_release_attestation(
        release_metadata,
        manifest,
        {archive_name: archive_payload, checksum_name: checksum_payload},
    )
    _verify_artifact_derivation(artifact_payload, archive, checksum)
    provenance: dict[str, Any] = {
        "schema": "pliego.benchmark-source-provenance",
        "version": 1,
        "contract": SOURCE_PROVENANCE_CONTRACT,
        "provenance_sha256": "",
        "source": dict(manifest["source"]),
        "actions_run": run,
        "actions_artifact": artifact,
        "benchmark_tag": tag,
        "release": release,
        "evidence": {
            "manifest_sha256": manifest["manifest_sha256"],
            "archive_name": archive_name,
            "archive_sha256": sha256_bytes(archive_payload),
            "archive_bytes": len(archive_payload),
            "checksum_name": checksum_name,
            "checksum_sha256": sha256_bytes(checksum_payload),
            "checksum_bytes": len(checksum_payload),
        },
    }
    provenance["provenance_sha256"] = source_provenance_sha256(provenance)
    validate_source_provenance(provenance, manifest)
    return provenance


def validate_source_provenance(provenance: dict[str, Any], manifest: dict[str, Any]) -> None:
    require(
        set(provenance)
        == {
            "schema",
            "version",
            "contract",
            "provenance_sha256",
            "source",
            "actions_run",
            "actions_artifact",
            "benchmark_tag",
            "release",
            "evidence",
        },
        "source provenance shape changed",
    )
    require(provenance.get("schema") == "pliego.benchmark-source-provenance", "source provenance schema changed")
    require(provenance.get("version") == 1, "source provenance version changed")
    require(provenance.get("contract") == SOURCE_PROVENANCE_CONTRACT, "source provenance contract changed")
    digest = provenance.get("provenance_sha256")
    require(isinstance(digest, str) and SHA256.fullmatch(digest) is not None, "source provenance SHA-256 is invalid")
    require(digest == source_provenance_sha256(provenance), "source provenance SHA-256 does not match its content")
    require(provenance.get("source") == manifest["source"], "source provenance differs from the evidence manifest")
    source = manifest["source"]
    run = provenance.get("actions_run")
    require(isinstance(run, dict), "committed Actions run provenance is absent")
    require(
        set(run)
        == {
            "id",
            "attempt",
            "head_sha",
            "head_branch",
            "event",
            "status",
            "conclusion",
            "workflow_name",
            "workflow_path",
            "workflow_id",
            "repository",
            "repository_id",
        },
        "committed Actions run provenance shape changed",
    )
    require(
        {key: run[key] for key in run if key not in ("workflow_id", "repository_id")}
        == {
            "id": source["run_id"],
            "attempt": source["run_attempt"],
            "head_sha": source["revision"],
            "head_branch": "main",
            "event": "workflow_dispatch",
            "status": "completed",
            "conclusion": "success",
            "workflow_name": source["workflow"],
            "workflow_path": PERFORMANCE_WORKFLOW_PATH,
            "repository": "oxhq/pliego",
        },
        "committed Actions run provenance is not exact",
    )
    require(isinstance(run["workflow_id"], int) and run["workflow_id"] > 0, "committed workflow ID is invalid")
    require(isinstance(run["repository_id"], int) and run["repository_id"] > 0, "committed repository ID is invalid")
    artifact = provenance.get("actions_artifact")
    require(isinstance(artifact, dict), "committed Actions artifact provenance is absent")
    require(
        set(artifact)
        == {
            "id",
            "name",
            "server_digest",
            "zip_sha256",
            "size_in_bytes",
            "expired_at_publication",
            "created_at",
            "expires_at",
            "workflow_run_id",
            "head_sha",
        },
        "committed Actions artifact provenance shape changed",
    )
    artifact_id = artifact.get("id")
    require(isinstance(artifact_id, int) and artifact_id > 0, "committed Actions artifact ID is invalid")
    expected_artifact_name = (
        f"pliego-hosted-performance-series-{source['revision']}-{source['run_id']}-{source['run_attempt']}"
    )
    require(artifact.get("name") == expected_artifact_name, "committed Actions artifact name is not canonical")
    server_digest = artifact.get("server_digest")
    match = ARTIFACT_DIGEST.fullmatch(server_digest) if isinstance(server_digest, str) else None
    require(match is not None and artifact.get("zip_sha256") == match.group(1), "committed artifact digests differ")
    require(artifact.get("expired_at_publication") is False, "source artifact was expired at publication")
    require(
        isinstance(artifact.get("size_in_bytes"), int) and artifact["size_in_bytes"] > 0,
        "committed Actions artifact byte count is invalid",
    )
    require(artifact.get("workflow_run_id") == source["run_id"], "committed artifact run ID differs")
    require(artifact.get("head_sha") == source["revision"], "committed artifact head SHA differs")
    require(
        isinstance(artifact.get("created_at"), str)
        and artifact["created_at"]
        and isinstance(artifact.get("expires_at"), str)
        and artifact["expires_at"],
        "committed Actions artifact timestamps are absent",
    )
    tag, _, archive_name, checksum_name = _release_identity(manifest)
    require(
        provenance.get("benchmark_tag")
        == {"ref": f"refs/tags/{tag}", "object_type": "commit", "target_sha": source["revision"]},
        "committed benchmark tag provenance is not a lightweight source-revision tag",
    )
    release = provenance.get("release")
    require(isinstance(release, dict), "committed release provenance is absent")
    require(
        set(release) == {"id", "tag_name", "immutable", "draft", "prerelease", "assets"},
        "committed release provenance shape changed",
    )
    require(release.get("tag_name") == tag, "committed release provenance tag differs")
    require(
        release.get("immutable") is True and release.get("draft") is False and release.get("prerelease") is False,
        "committed release provenance is not an immutable published release",
    )
    require(isinstance(release.get("id"), int) and release["id"] > 0, "committed release ID is invalid")
    release_assets = release.get("assets")
    require(isinstance(release_assets, list) and len(release_assets) == 2, "committed release does not bind two assets")
    expected_assets = _expected_release_assets(manifest)
    expected_urls = {
        expected_assets["archive_name"]: expected_assets["archive_url"],
        expected_assets["checksum_name"]: expected_assets["checksum_url"],
    }
    observed_assets: dict[str, str] = {}
    ids: set[int] = set()
    for item in release_assets:
        require(
            isinstance(item, dict) and set(item) == {"id", "name", "url", "state", "size", "digest"},
            "committed release asset provenance shape changed",
        )
        require(
            isinstance(item["id"], int) and item["id"] > 0 and item["id"] not in ids,
            "committed release asset ID is invalid or repeated",
        )
        ids.add(item["id"])
        require(isinstance(item["size"], int) and item["size"] > 0, "committed release asset size is invalid")
        require(
            isinstance(item["digest"], str) and ARTIFACT_DIGEST.fullmatch(item["digest"]) is not None,
            "committed release asset digest is invalid",
        )
        require(item["state"] == "uploaded", "committed release asset is not uploaded")
        require(item["name"] not in observed_assets, "committed release asset name is repeated")
        observed_assets[item["name"]] = item["url"]
    require(observed_assets == expected_urls, "committed release provenance does not bind exactly the evidence assets")
    evidence = provenance.get("evidence")
    require(isinstance(evidence, dict), "source provenance evidence binding is absent")
    require(
        set(evidence)
        == {
            "manifest_sha256",
            "archive_name",
            "archive_sha256",
            "archive_bytes",
            "checksum_name",
            "checksum_sha256",
            "checksum_bytes",
        },
        "source provenance evidence binding shape changed",
    )
    require(
        evidence.get("manifest_sha256") == manifest["manifest_sha256"]
        and evidence.get("archive_name") == archive_name
        and isinstance(evidence.get("archive_sha256"), str)
        and SHA256.fullmatch(evidence["archive_sha256"]) is not None
        and isinstance(evidence.get("archive_bytes"), int)
        and evidence["archive_bytes"] > 0
        and evidence.get("checksum_name") == checksum_name
        and isinstance(evidence.get("checksum_sha256"), str)
        and SHA256.fullmatch(evidence["checksum_sha256"]) is not None
        and isinstance(evidence.get("checksum_bytes"), int)
        and evidence["checksum_bytes"] > 0,
        "source provenance evidence binding is invalid",
    )
    release_assets_by_name = {item["name"]: item for item in release_assets}
    require(
        release_assets_by_name[archive_name]["digest"] == f"sha256:{evidence['archive_sha256']}"
        and release_assets_by_name[archive_name]["size"] == evidence["archive_bytes"],
        "committed archive asset metadata differs from the evidence binding",
    )
    require(
        release_assets_by_name[checksum_name]["digest"] == f"sha256:{evidence['checksum_sha256']}"
        and release_assets_by_name[checksum_name]["size"] == evidence["checksum_bytes"],
        "committed checksum asset metadata differs from the evidence binding",
    )


def _descriptor_index(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {descriptor["path"]: descriptor for descriptor in manifest["files"]}


def _require_bound_bytes(payload: bytes, descriptor: dict[str, Any], label: str) -> None:
    require(len(payload) == descriptor["bytes"], f"{label} byte count differs from the evidence archive")
    require(sha256_bytes(payload) == descriptor["sha256"], f"{label} SHA-256 differs from the evidence archive")


def _number(value: float) -> str:
    require(math.isfinite(value) and value >= 0, f"public benchmark value is not finite and nonnegative: {value!r}")
    return f"{value:.6f}".rstrip("0").rstrip(".")


def _markdown_cell(value: str) -> str:
    return value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace("|", "&#124;")


def render_readme_body(series: dict[str, Any], manifest: dict[str, Any]) -> str:
    violations = summarize_comparisons.validate_series(series)
    require(not violations, f"hosted series failed validation: {violations[0] if violations else ''}")
    validate_evidence_manifest(manifest)
    require(series["claim_boundary"] == CLAIM_BOUNDARY, "hosted series weakened the claim boundary")
    require(
        series["series_sha256"] == manifest["series"]["series_sha256"], "series seal differs from the archive manifest"
    )
    require(series["source"] == manifest["source"], "series source differs from the archive manifest")
    require(
        series["fixture"]["id"] == manifest["target"]["fixture"], "series fixture differs from the archive manifest"
    )
    labels = {target["id"]: target["label"] for target in series["targets"]}
    tag, _, _, _ = _release_identity(manifest)
    report = RESULT_RELATIVE.as_posix() + f"/{REPORT_FILE}"
    release_url = f"https://github.com/oxhq/pliego/releases/tag/{tag}"
    total_samples = len(series["summary"]["repeats"]) * len(series["targets"]) * series["protocol"]["sample_count"]

    lines = [
        "The published `minimal-static` snapshot measures the released Pliego v0.3.3 bundle and the",
        "version-locked adapter dependency graphs for dompdf 3.1.6 and Browsershot 5.4.0 with Puppeteer",
        "25.8.0. Every displayed value comes from the sealed three-repeat series; the table shows each",
        "fresh VM's p50 wall time and the complete between-run range.",
        "",
        "| Renderer | Repeat 1 p50 (ms) | Repeat 2 p50 (ms) | Repeat 3 p50 (ms) | p50 range (ms) |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for target in series["summary"]["targets"]:
        target_id = target["target_id"]
        wall = target["metrics"].get("wall_ms")
        require(isinstance(wall, dict), f"hosted series omits wall_ms for {target_id!r}")
        values = [entry["p50"] for entry in wall["per_repeat"]]
        require(len(values) == 3, f"hosted series does not retain three wall_ms p50s for {target_id!r}")
        label = _markdown_cell(labels[target_id])
        lines.append(
            f"| {label} | {_number(values[0])} | {_number(values[1])} | {_number(values[2])} | "
            f"{_number(wall['min'])} to {_number(wall['max'])} |"
        )
    lines.extend(
        [
            "",
            "Browsershot's Node/Chromium generic temporary storage uses a disclosed private tmpfs `TMPDIR`; "
            "it remains charged to cgroup memory but is outside block-device I/O. Its PHP `TMPDIR`, "
            "HOME/XDG roots, explicit Chromium profile, artifacts, and PDF remain on measured ext4. "
            "This target-specific storage accommodation can affect wall time; see the methodology.",
            "",
            f"All {total_samples} timed samples passed the shared PDF oracle. " + CLAIM_BOUNDARY,
            "",
            f"[Full comparative aggregate report and spread]({report}) · [Immutable evidence release]({release_url})",
            "",
            "Authoritative tables and production rankings remain N/A until the stricter dedicated-host,",
            "immutable-runtime, and canonical-oracle gates pass. Read the exact boundary and reproduction",
            "commands in the [benchmark methodology](docs/benchmarks/README.md).",
        ]
    )
    return "\n".join(lines)


def _marker_bounds(readme: str) -> tuple[int, int]:
    require(readme.count(README_START) == 1, "README must contain exactly one hosted benchmark start marker")
    require(readme.count(README_END) == 1, "README must contain exactly one hosted benchmark end marker")
    start = readme.index(README_START) + len(README_START)
    end = readme.index(README_END)
    require(start < end, "README hosted benchmark markers are out of order")
    return start, end


def readme_body(readme: str) -> str:
    start, end = _marker_bounds(readme)
    return readme[start:end].strip("\n")


def replace_readme_body(readme: str, body: str) -> str:
    start, end = _marker_bounds(readme)
    return readme[:start] + "\n" + body.rstrip("\n") + "\n" + readme[end:]


def _regular_public_file(path: Path, root: Path) -> bytes:
    require(_lexists(path), f"required public benchmark file is absent: {path.relative_to(root)}")
    resolved = path.resolve(strict=True)
    canonical_root = root.resolve(strict=True)
    require(
        resolved == canonical_root or canonical_root in resolved.parents,
        f"public benchmark path escapes the repository: {path}",
    )
    payload, _ = _stable_regular_bytes(path, f"public benchmark file {path.relative_to(root)}")
    return payload


def _public_utf8_file(path: Path, root: Path) -> str:
    payload = _regular_public_file(path, root)
    return normalized_utf8_text(payload, f"public benchmark file {path.relative_to(root)}")


def parse_archive_checksum(payload: bytes, archive_filename: str) -> str:
    try:
        text = payload.decode("ascii")
    except UnicodeDecodeError as error:
        fail(f"archive checksum is not ASCII: {error}")
    match = re.fullmatch(r"([0-9a-f]{64})  ([^\r\n]+)\n", text)
    require(match is not None, "archive checksum must be one canonical sha256sum line")
    assert match is not None
    require(match.group(2) == archive_filename, "archive checksum names a different asset")
    return match.group(1)


def publication_directory(root: Path = ROOT) -> Path:
    return root / RESULT_RELATIVE


def verify_unpublished(root: Path = ROOT) -> None:
    result = publication_directory(root)
    require(not _lexists(result), f"unpublished state contains a benchmark result path: {result.relative_to(root)}")
    body = readme_body(_public_utf8_file(root / "README.md", root))
    for phrase in (
        "There is no committed performance snapshot yet.",
        "directional `github-hosted-exploratory` timing/resource evidence",
        "Authoritative\ntables and production rankings remain N/A",
    ):
        require(phrase in body, f"unpublished README omitted its benchmark boundary: {phrase!r}")
    require("docs/benchmarks/results/" not in body, "unpublished README links to a result that does not exist")


def load_publication(
    root: Path = ROOT,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any], bytes, bytes]:
    directory = publication_directory(root)
    require(directory.exists(), f"public benchmark result directory is absent: {directory.relative_to(root)}")
    require(
        not directory.is_symlink() and directory.is_dir(), "public benchmark result path is not a regular directory"
    )
    actual = {path.name for path in directory.iterdir()}
    require(actual == PUBLICATION_ENTRIES, f"public benchmark result file set changed: {sorted(actual)!r}")
    manifest_payload = _regular_public_file(directory / EVIDENCE_MANIFEST, root)
    provenance_payload = _regular_public_file(directory / SOURCE_PROVENANCE_FILE, root)
    series_payload = _regular_public_file(directory / SERIES_FILE, root)
    report_payload = _regular_public_file(directory / REPORT_FILE, root)
    checksum_payload = _regular_public_file(directory / ARCHIVE_CHECKSUM_FILE, root)
    manifest = load_json_bytes(manifest_payload, EVIDENCE_MANIFEST)
    provenance = load_json_bytes(provenance_payload, SOURCE_PROVENANCE_FILE)
    series = load_json_bytes(series_payload, SERIES_FILE)
    validate_evidence_manifest(manifest)
    validate_source_provenance(provenance, manifest)
    descriptors = _descriptor_index(manifest)
    _require_bound_bytes(series_payload, descriptors[f"series/{SERIES_FILE}"], SERIES_FILE)
    _require_bound_bytes(report_payload, descriptors[f"series/{REPORT_FILE}"], REPORT_FILE)
    _, _, archive_filename, _ = _release_identity(manifest)
    archive_sha256 = parse_archive_checksum(checksum_payload, archive_filename)
    require(
        provenance["evidence"]["archive_sha256"] == archive_sha256,
        "committed provenance archive SHA-256 differs from the checksum",
    )
    require(
        provenance["evidence"]["checksum_sha256"] == sha256_bytes(checksum_payload),
        "committed provenance checksum SHA-256 differs from the checksum file",
    )
    return manifest, provenance, series, report_payload, checksum_payload


def verify_publication(root: Path = ROOT) -> bool:
    directory = publication_directory(root)
    if not directory.exists():
        verify_unpublished(root)
        return False
    manifest, _, series, report_payload, _ = load_publication(root)
    violations = summarize_comparisons.validate_series(series)
    require(not violations, f"committed hosted series failed validation: {violations[0] if violations else ''}")
    expected_report = summarize_comparisons.render_markdown(series)
    require(
        normalized_utf8_text(report_payload, REPORT_FILE) == expected_report,
        "committed hosted report is not the deterministic rendering of its series",
    )
    expected_body = render_readme_body(series, manifest)
    actual_body = readme_body(_public_utf8_file(root / "README.md", root))
    require(
        actual_body == expected_body, "README benchmark table is not the exact rendering of the committed hosted series"
    )
    return True


def release_assets(root: Path = ROOT) -> dict[str, str] | None:
    if not verify_publication(root):
        return None
    manifest, provenance, _, _, _ = load_publication(root)
    assets = _expected_release_assets(manifest)
    assets.update(
        {
            "source_revision": manifest["source"]["revision"],
            "source_run_id": str(manifest["source"]["run_id"]),
            "source_run_attempt": str(manifest["source"]["run_attempt"]),
            "source_artifact_id": str(provenance["actions_artifact"]["id"]),
            "source_artifact_name": provenance["actions_artifact"]["name"],
            "source_artifact_digest": provenance["actions_artifact"]["server_digest"],
        }
    )
    return assets


def release_urls(root: Path = ROOT) -> tuple[str, str] | None:
    assets = release_assets(root)
    if assets is None:
        return None
    return assets["archive_url"], assets["checksum_url"]


def verify_github_release(metadata: Path, root: Path = ROOT) -> None:
    require(verify_publication(root), "no committed public benchmark release exists")
    manifest, provenance, _, _, _ = load_publication(root)
    observed = _github_release_attestation(load_json(metadata), manifest)
    require(observed == provenance["release"], "live GitHub release metadata differs from committed provenance")


def verify_github_tag(metadata: Path, root: Path = ROOT) -> None:
    require(verify_publication(root), "no committed public benchmark release exists")
    manifest, provenance, _, _, _ = load_publication(root)
    observed = _benchmark_tag_attestation(load_json(metadata), manifest)
    require(observed == provenance["benchmark_tag"], "live benchmark tag differs from committed provenance")


def _validate_packaged_archive(archive: Path, checksum: Path) -> None:
    script = Path(__file__).with_name("package_hosted_evidence.py")
    require(script.is_file(), "package_hosted_evidence.py is required before public benchmark staging")
    process = subprocess.run(
        [sys.executable, str(script), "validate", str(archive), "--checksum", str(checksum)],
        capture_output=True,
        text=True,
    )
    require(process.returncode == 0, f"packaged benchmark evidence failed validation: {process.stderr.strip()}")


def _archive_payloads(archive: Path) -> tuple[dict[str, Any], dict[str, bytes]]:
    archive_payload, _ = _stable_regular_bytes(archive, "evidence archive")
    try:
        with tarfile.open(fileobj=io.BytesIO(archive_payload), mode="r:gz") as bundle:
            manifest_members = [
                member
                for member in bundle.getmembers()
                if member.isfile() and member.name.count("/") == 1 and member.name.endswith(f"/{EVIDENCE_MANIFEST}")
            ]
            require(len(manifest_members) == 1, "evidence archive must contain one root manifest")
            manifest_stream = bundle.extractfile(manifest_members[0])
            require(manifest_stream is not None, "cannot read the evidence archive manifest")
            manifest_payload = manifest_stream.read()
            manifest = load_json_bytes(manifest_payload, EVIDENCE_MANIFEST)
            validate_evidence_manifest(manifest)
            _, archive_root, _, _ = _release_identity(manifest)
            require(manifest_members[0].name == f"{archive_root}/{EVIDENCE_MANIFEST}", "archive manifest root changed")
            wanted = {
                EVIDENCE_MANIFEST: f"{archive_root}/{EVIDENCE_MANIFEST}",
                SERIES_FILE: f"{archive_root}/series/{SERIES_FILE}",
                REPORT_FILE: f"{archive_root}/series/{REPORT_FILE}",
            }
            indexed = {member.name: member for member in bundle.getmembers()}
            payloads: dict[str, bytes] = {}
            for public_name, member_name in wanted.items():
                member = indexed.get(member_name)
                require(member is not None and member.isfile(), f"evidence archive omits {member_name}")
                stream = bundle.extractfile(member)
                require(stream is not None, f"cannot read {member_name} from the evidence archive")
                payloads[public_name] = stream.read()
            return manifest, payloads
    except (OSError, tarfile.TarError) as error:
        fail(f"cannot read evidence archive {archive}: {error}")


def verify_against_archive(archive: Path, checksum: Path, root: Path = ROOT) -> None:
    _validate_packaged_archive(archive, checksum)
    manifest, payloads = _archive_payloads(archive)
    committed_manifest, provenance, _, _, committed_checksum = load_publication(root)
    require(manifest == committed_manifest, "committed evidence manifest differs from the released archive")
    directory = publication_directory(root)
    for filename, payload in payloads.items():
        require(
            (directory / filename).read_bytes() == payload, f"committed {filename} differs from the released archive"
        )
    require(committed_checksum == checksum.read_bytes(), "committed archive checksum differs from the released asset")
    _, _, archive_filename, _ = _release_identity(manifest)
    expected_archive_sha256 = parse_archive_checksum(committed_checksum, archive_filename)
    archive_payload, _ = _stable_regular_bytes(archive, "released archive")
    checksum_payload, _ = _stable_regular_bytes(checksum, "released checksum")
    require(
        sha256_bytes(archive_payload) == expected_archive_sha256,
        "released archive bytes differ from the committed checksum",
    )
    require(
        len(archive_payload) == provenance["evidence"]["archive_bytes"]
        and sha256_bytes(archive_payload) == provenance["evidence"]["archive_sha256"],
        "released archive bytes differ from committed provenance",
    )
    require(
        len(checksum_payload) == provenance["evidence"]["checksum_bytes"]
        and sha256_bytes(checksum_payload) == provenance["evidence"]["checksum_sha256"],
        "released checksum bytes differ from committed provenance",
    )
    release_assets_by_name = {item["name"]: item for item in provenance["release"]["assets"]}
    for name, payload in ((archive.name, archive_payload), (checksum.name, checksum_payload)):
        asset = release_assets_by_name[name]
        require(
            asset["size"] == len(payload) and asset["digest"] == f"sha256:{sha256_bytes(payload)}",
            f"released {name} differs from committed release-asset provenance",
        )
    verify_publication(root)


def _write_transaction_file(parent: Path, prefix: str, payload: bytes, mode: int) -> tuple[Path, PathIdentity, str]:
    descriptor, name = tempfile.mkstemp(prefix=prefix, dir=parent)
    path = Path(name)
    try:
        os.fchmod(descriptor, stat.S_IMODE(mode))
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
    except Exception:
        try:
            os.close(descriptor)
        except OSError:
            pass
        try:
            path.unlink()
        except OSError:
            pass
        raise
    identity = _path_identity(path, directory=False)
    return path, identity, sha256_bytes(payload)


def _same_file_object(path: Path, expected: PathIdentity, digest: str) -> bool:
    try:
        payload, observed = _stable_regular_bytes(path, str(path), require_single_link=False)
    except PublicationError:
        return False
    return (
        observed.device,
        observed.inode,
        observed.mode,
        observed.size,
        observed.modified_ns,
        sha256_bytes(payload),
    ) == (
        expected.device,
        expected.inode,
        expected.mode,
        expected.size,
        expected.modified_ns,
        digest,
    )


def _remove_private_transaction(path: Path) -> None:
    _path_identity(path, directory=True)
    for entry in path.rglob("*"):
        metadata = entry.lstat()
        require(
            not stat.S_ISLNK(metadata.st_mode) and not _is_reparse_point(metadata),
            f"private transaction contains a linked path and was preserved: {entry}",
        )
        require(
            stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode),
            f"private transaction contains a special path and was preserved: {entry}",
        )
        if stat.S_ISREG(metadata.st_mode):
            require(
                metadata.st_nlink == 1, f"private transaction contains a hard-linked file and was preserved: {entry}"
            )
    shutil.rmtree(path)


def _rollback_publication_directory(
    destination: Path, expected: PublicationTreeIdentity, transaction_root: Path
) -> None:
    require(
        _same_publication_tree(destination, expected),
        "installed publication directory was replaced or modified; it was preserved",
    )
    quarantine = transaction_root / "result.failed"
    require(not _lexists(quarantine), "private result rollback path was unexpectedly occupied")
    os.replace(destination, quarantine)
    if not _same_publication_tree(quarantine, expected):
        fail(f"publication directory identity changed during rollback; moved path preserved at {quarantine}")
    shutil.rmtree(quarantine)


def _same_partial_publication_tree(
    directory: Path,
    expected_root: PathIdentity,
    expected_files: dict[str, tuple[PathIdentity, str]],
) -> bool:
    if not _same_directory_object(directory, expected_root):
        return False
    try:
        entries = list(directory.iterdir())
    except OSError:
        return False
    if {entry.name for entry in entries} != set(expected_files):
        return False
    return all(_same_plain_file(entry, *expected_files[entry.name]) for entry in entries)


def _rollback_partial_publication_directory(
    destination: Path,
    expected_root: PathIdentity,
    expected_files: dict[str, tuple[PathIdentity, str]],
    transaction_root: Path,
) -> None:
    require(
        _same_partial_publication_tree(destination, expected_root, expected_files),
        "reserved publication directory was replaced or modified; it was preserved",
    )
    quarantine = transaction_root / "result.failed"
    require(not _lexists(quarantine), "private result rollback path was unexpectedly occupied")
    os.replace(destination, quarantine)
    if not _same_partial_publication_tree(quarantine, expected_root, expected_files):
        fail(f"publication directory identity changed during rollback; moved path preserved at {quarantine}")
    for entry in quarantine.iterdir():
        entry.unlink()
    quarantine.rmdir()


def _rollback_readme(
    readme_path: Path,
    backup: Path,
    original_identity: PathIdentity,
    original_digest: str,
    installed_identity: PathIdentity | None,
    installed_digest: str,
    backup_moved: bool,
    transaction_root: Path,
) -> None:
    if installed_identity is not None:
        require(
            _same_plain_file(readme_path, installed_identity, installed_digest),
            "installed README was replaced or modified; it and the original backup were preserved",
        )
        quarantine = transaction_root / "README.failed"
        require(not _lexists(quarantine), "private README rollback path was unexpectedly occupied")
        os.replace(readme_path, quarantine)
        if not _same_plain_file(quarantine, installed_identity, installed_digest):
            fail(f"README identity changed during rollback; moved path preserved at {quarantine}")
        if not _same_plain_file(backup, original_identity, original_digest):
            fail("original README backup changed during rollback")
        require(not _lexists(readme_path), "README path was recreated during rollback; original backup was preserved")
        try:
            os.link(backup, readme_path)
        except FileExistsError:
            fail("README path was recreated during rollback; original backup was preserved")
        require(
            _same_file_object(readme_path, original_identity, original_digest),
            "original README object was not linked back during rollback",
        )
        backup.unlink()
        require(
            _same_plain_file(readme_path, original_identity, original_digest),
            "original README identity was not restored",
        )
        quarantine.unlink()
        return
    if backup_moved:
        require(not _lexists(readme_path), "README path was replaced while its original backup was held")
        require(_same_plain_file(backup, original_identity, original_digest), "original README backup changed")
        try:
            os.link(backup, readme_path)
        except FileExistsError:
            fail("README path was recreated during rollback; original backup was preserved")
        require(
            _same_file_object(readme_path, original_identity, original_digest),
            "original README object was not linked back during rollback",
        )
        backup.unlink()
        require(
            _same_plain_file(readme_path, original_identity, original_digest),
            "original README identity was not restored",
        )
        return
    require(
        _same_plain_file(readme_path, original_identity, original_digest),
        "README changed before publication and was preserved",
    )


def _commit_publication(
    public_payloads: dict[str, bytes],
    body: str,
    root: Path,
    verify: Callable[[], None],
    transaction_hook: Callable[[str], None] | None = None,
) -> None:
    require(set(public_payloads) == PUBLICATION_ENTRIES, "publication transaction payload set is not exact")
    destination = publication_directory(root)
    require(not _lexists(destination), f"public benchmark result directory already exists: {destination}")
    parent = _ensure_plain_directory_chain(root, RESULT_RELATIVE.parent)
    require(parent == destination.parent, "publication directory chain resolved unexpectedly")
    readme_path = root / "README.md"
    original_readme, original_identity = _stable_regular_bytes(readme_path, "README")
    try:
        original_text = original_readme.decode("utf-8")
    except UnicodeDecodeError as error:
        fail(f"README is not UTF-8: {error}")
    new_readme = replace_readme_body(original_text, body).replace("\r\n", "\n").replace("\r", "\n").encode("utf-8")
    transaction_prefix = f".{destination.name}-transaction-"
    transaction_root = Path(tempfile.mkdtemp(prefix=transaction_prefix, dir=root))
    transaction_root.chmod(0o700)
    staged = transaction_root / "result.staged"
    staged.mkdir(mode=0o700)
    destination_identity: PublicationTreeIdentity | None = None
    destination_reserved_identity: PathIdentity | None = None
    destination_files: dict[str, tuple[PathIdentity, str]] = {}
    destination_installed = False
    readme_temp: Path | None = None
    readme_temp_identity: PathIdentity | None = None
    readme_temp_digest = ""
    backup = transaction_root / "README.original"
    backup_moved = False
    installed_readme_identity: PathIdentity | None = None
    original_digest = sha256_bytes(original_readme)
    preserve_transaction = False
    try:
        for filename, payload in public_payloads.items():
            (staged / filename).write_bytes(payload)
        _publication_tree_identity(staged)
        readme_temp, readme_temp_identity, readme_temp_digest = _write_transaction_file(
            transaction_root,
            "README.new-",
            new_readme,
            original_identity.mode,
        )
        if transaction_hook is not None:
            transaction_hook("before-destination-reserve")
        try:
            destination.mkdir()
        except FileExistsError:
            fail(f"public benchmark result directory appeared before exclusive reservation: {destination}")
        except OSError as error:
            fail(f"cannot reserve public benchmark result directory {destination}: {error}")
        destination_reserved_identity = _path_identity(destination, directory=True)
        if transaction_hook is not None:
            transaction_hook("destination-reserved")
        with _bound_directory_linker(destination, destination_reserved_identity) as link_entry:
            for index, filename in enumerate(sorted(public_payloads, key=lambda item: item.encode("utf-8"))):
                require(
                    _same_directory_object(destination, destination_reserved_identity),
                    "reserved publication directory was replaced before it could be populated",
                )
                source = staged / filename
                target = destination / filename
                payload, source_identity = _stable_regular_bytes(source, f"staged {filename}")
                digest = sha256_bytes(payload)
                try:
                    link_entry(source, filename)
                except FileExistsError:
                    fail(f"reserved publication entry was recreated before installation: {target}")
                except OSError as error:
                    fail(f"cannot install public benchmark entry {target}: {error}")
                linked_identity = _path_identity(target, directory=False)
                destination_files[filename] = (linked_identity, digest)
                require(
                    (
                        linked_identity.device,
                        linked_identity.inode,
                        linked_identity.mode,
                        linked_identity.size,
                        linked_identity.modified_ns,
                    )
                    == (
                        source_identity.device,
                        source_identity.inode,
                        source_identity.mode,
                        source_identity.size,
                        source_identity.modified_ns,
                    )
                    and linked_identity.links == source_identity.links + 1,
                    f"installed publication entry is not the staged file: {target}",
                )
                require(
                    _same_directory_object(destination, destination_reserved_identity),
                    "reserved publication directory was replaced while it was populated",
                )
                source.unlink()
                expected_installed_identity = PathIdentity(
                    device=linked_identity.device,
                    inode=linked_identity.inode,
                    mode=linked_identity.mode,
                    links=source_identity.links,
                    size=linked_identity.size,
                    modified_ns=linked_identity.modified_ns,
                )
                destination_files[filename] = (expected_installed_identity, digest)
                installed_payload, installed_identity = _stable_regular_bytes(target, f"installed {filename}")
                require(
                    installed_identity == expected_installed_identity and installed_payload == payload,
                    f"installed publication entry differs from the staged file: {target}",
                )
                require(
                    _same_directory_object(destination, destination_reserved_identity),
                    "reserved publication directory was replaced while it was populated",
                )
                if index == 0 and transaction_hook is not None:
                    transaction_hook("destination-partially-installed")
            staged.rmdir()
            destination_identity = _publication_tree_identity(destination)
            require(
                _same_directory_object(destination, destination_reserved_identity),
                "installed publication directory identity changed",
            )
            require(
                destination_identity.files
                == tuple(
                    (filename, *destination_files[filename])
                    for filename in sorted(destination_files, key=lambda item: item.encode("utf-8"))
                ),
                "installed publication directory differs from the staged payload identities",
            )
        destination_installed = True
        if transaction_hook is not None:
            transaction_hook("destination-installed")
        require(
            _same_plain_file(readme_path, original_identity, original_digest),
            "README changed before the transaction could install its replacement",
        )
        os.replace(readme_path, backup)
        backup_moved = True
        require(
            _same_plain_file(backup, original_identity, original_digest),
            "README identity changed while it was moved into the private transaction",
        )
        if transaction_hook is not None:
            transaction_hook("readme-backed-up")
        require(readme_temp is not None and readme_temp_identity is not None, "new README transaction file is absent")
        try:
            os.link(readme_temp, readme_path)
        except FileExistsError:
            fail("README path was recreated before the new README could be installed")
        readme_temp.unlink()
        installed_readme_identity = _path_identity(readme_path, directory=False)
        require(
            _same_plain_file(readme_path, installed_readme_identity, readme_temp_digest),
            "installed README differs from the staged transaction file",
        )
        if transaction_hook is not None:
            transaction_hook("readme-installed")
        verify()
        if transaction_hook is not None:
            transaction_hook("verified")
        require(_same_plain_file(backup, original_identity, original_digest), "original README backup changed")
        backup.unlink()
        backup_moved = False
    except Exception as error:
        rollback_errors: list[str] = []
        try:
            _rollback_readme(
                readme_path,
                backup,
                original_identity,
                original_digest,
                installed_readme_identity,
                readme_temp_digest,
                backup_moved,
                transaction_root,
            )
        except (OSError, PublicationError) as rollback_error:
            rollback_errors.append(f"README: {rollback_error}")
        if destination_installed and destination_identity is not None:
            try:
                _rollback_publication_directory(destination, destination_identity, transaction_root)
            except (OSError, PublicationError) as rollback_error:
                rollback_errors.append(f"result directory: {rollback_error}")
        elif destination_reserved_identity is not None:
            try:
                _rollback_partial_publication_directory(
                    destination,
                    destination_reserved_identity,
                    destination_files,
                    transaction_root,
                )
            except (OSError, PublicationError) as rollback_error:
                rollback_errors.append(f"result directory: {rollback_error}")
        if rollback_errors:
            preserve_transaction = True
            fail(
                f"publication failed ({error}); rollback incomplete: {'; '.join(rollback_errors)}; "
                f"private transaction preserved at {transaction_root}"
            )
        raise
    finally:
        if _lexists(transaction_root) and not preserve_transaction:
            _remove_private_transaction(transaction_root)


def stage_publication(
    archive: Path,
    checksum: Path,
    run_metadata: Path,
    artifact_metadata: Path,
    artifact_zip: Path,
    tag_metadata: Path,
    release_metadata: Path,
    root: Path = ROOT,
    *,
    _transaction_hook: Callable[[str], None] | None = None,
) -> None:
    _validate_packaged_archive(archive, checksum)
    manifest, payloads = _archive_payloads(archive)
    _, _, archive_filename, _ = _release_identity(manifest)
    checksum_payload, _ = _stable_regular_bytes(checksum, "supplied evidence checksum")
    archive_payload, _ = _stable_regular_bytes(archive, "supplied evidence archive")
    expected_archive_sha256 = parse_archive_checksum(checksum_payload, archive_filename)
    require(sha256_bytes(archive_payload) == expected_archive_sha256, "archive bytes differ from the supplied checksum")
    series = load_json_bytes(payloads[SERIES_FILE], SERIES_FILE)
    expected_report = summarize_comparisons.render_markdown(series)
    require(
        normalized_utf8_text(payloads[REPORT_FILE], REPORT_FILE) == expected_report,
        "archive report is not the deterministic rendering of its series",
    )
    body = render_readme_body(series, manifest)
    provenance = build_source_provenance(
        manifest,
        archive,
        checksum,
        load_json(run_metadata),
        load_json(artifact_metadata),
        artifact_zip,
        load_json(tag_metadata),
        load_json(release_metadata),
    )
    provenance_payload = (json.dumps(provenance, indent=2, ensure_ascii=True, allow_nan=False) + "\n").encode("utf-8")
    public_payloads = {
        EVIDENCE_MANIFEST: payloads[EVIDENCE_MANIFEST],
        SOURCE_PROVENANCE_FILE: provenance_payload,
        SERIES_FILE: payloads[SERIES_FILE],
        REPORT_FILE: payloads[REPORT_FILE],
        ARCHIVE_CHECKSUM_FILE: checksum_payload,
    }
    _commit_publication(
        public_payloads,
        body,
        root,
        lambda: verify_against_archive(archive, checksum, root),
        _transaction_hook,
    )


def verify_live_source(
    archive: Path,
    checksum: Path,
    run_metadata: Path,
    artifact_metadata: Path,
    artifact_zip: Path,
    tag_metadata: Path,
    release_metadata: Path,
    root: Path = ROOT,
) -> dict[str, Any]:
    _validate_packaged_archive(archive, checksum)
    manifest, _ = _archive_payloads(archive)
    provenance = build_source_provenance(
        manifest,
        archive,
        checksum,
        load_json(run_metadata),
        load_json(artifact_metadata),
        artifact_zip,
        load_json(tag_metadata),
        load_json(release_metadata),
    )
    _require_committed_source_provenance(manifest, provenance, root)
    return provenance


def _require_committed_source_provenance(
    manifest: dict[str, Any], provenance: dict[str, Any], root: Path = ROOT
) -> None:
    committed_manifest, committed_provenance, _, _, _ = load_publication(root)
    require(manifest == committed_manifest, "live source archive differs from the committed evidence manifest")
    require(
        provenance == committed_provenance,
        "live GitHub source provenance differs from the committed provenance receipt",
    )


def _write_github_outputs(path: Path, assets: dict[str, str] | None) -> None:
    published = assets is not None
    lines = [f"published={'true' if published else 'false'}"]
    if assets is not None:
        lines.extend(f"{key}={value}" for key, value in assets.items())
    with path.open("a", encoding="utf-8", newline="\n") as output:
        output.write("\n".join(lines) + "\n")


def _add_source_origin_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--run-metadata", required=True, type=Path, help="exact Actions run-attempt API response")
    parser.add_argument(
        "--artifact-metadata", required=True, type=Path, help="exact-name Actions artifact-list API response"
    )
    parser.add_argument("--artifact-zip", required=True, type=Path, help="downloaded exact Actions artifact ZIP")
    parser.add_argument("--tag-metadata", required=True, type=Path, help="benchmark Git-ref API response")
    parser.add_argument("--release-metadata", required=True, type=Path, help="immutable GitHub release API response")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("verify", help="verify the committed public benchmark state")
    stage = commands.add_parser("stage", help="stage public files from one validated evidence archive")
    stage.add_argument("archive", type=Path)
    stage.add_argument("--checksum", type=Path, required=True)
    _add_source_origin_arguments(stage)
    source_verify = commands.add_parser(
        "verify-source", help="verify the live publication-time Actions, tag, and release origin"
    )
    source_verify.add_argument("archive", type=Path)
    source_verify.add_argument("--checksum", type=Path, required=True)
    _add_source_origin_arguments(source_verify)
    archive_verify = commands.add_parser("verify-archive", help="compare committed files with exact release assets")
    archive_verify.add_argument("archive", type=Path)
    archive_verify.add_argument("--checksum", type=Path, required=True)
    release_verify = commands.add_parser("verify-release", help="verify immutable GitHub release metadata")
    release_verify.add_argument("metadata", type=Path)
    tag_verify = commands.add_parser("verify-tag", help="verify the live lightweight benchmark tag target")
    tag_verify.add_argument("metadata", type=Path)
    assets = commands.add_parser("release-assets", help="emit canonical release URLs for CI")
    assets.add_argument("--github-output", type=Path)
    args = parser.parse_args(argv)

    try:
        if args.command == "verify":
            published = verify_publication()
            print(
                "public hosted benchmark publication verified"
                if published
                else "public hosted benchmark remains unpublished"
            )
        elif args.command == "stage":
            stage_publication(
                args.archive,
                args.checksum,
                args.run_metadata,
                args.artifact_metadata,
                args.artifact_zip,
                args.tag_metadata,
                args.release_metadata,
            )
            print(f"staged public hosted benchmark under {RESULT_RELATIVE}")
        elif args.command == "verify-source":
            provenance = verify_live_source(
                args.archive,
                args.checksum,
                args.run_metadata,
                args.artifact_metadata,
                args.artifact_zip,
                args.tag_metadata,
                args.release_metadata,
            )
            print(f"verified publication-time source provenance {provenance['provenance_sha256']}")
        elif args.command == "verify-archive":
            verify_against_archive(args.archive, args.checksum)
            print("committed public benchmark matches the exact released evidence archive")
        elif args.command == "verify-release":
            verify_github_release(args.metadata)
            print("committed public benchmark is attached to an immutable GitHub release")
        elif args.command == "verify-tag":
            verify_github_tag(args.metadata)
            print("committed public benchmark tag is lightweight at the evidence source revision")
        elif args.command == "release-assets":
            assets = release_assets()
            if args.github_output is not None:
                _write_github_outputs(args.github_output, assets)
            if assets is None:
                print("unpublished")
            else:
                print("\n".join(f"{key}={value}" for key, value in assets.items()))
    except (OSError, PublicationError, ValueError) as error:
        print(f"public_hosted_benchmark: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
