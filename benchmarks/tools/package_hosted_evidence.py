#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Build or validate one canonical hosted benchmark evidence archive.

The build command accepts the final GitHub Actions artifact tree containing one
hosted-series bundle and its three hosted-comparison source bundles. It validates
all four bundles with their existing validators, normalizes the tree, and emits
one deterministic ``tar.gz`` plus its SHA-256 sidecar. The validate command does
the reverse: it rejects unsafe tar members, validates all retained bundles, and
requires a byte-identical rebuild under the canonical archive contract.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import stat
import sys
import tarfile
import tempfile
import unicodedata
from copy import deepcopy
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, NoReturn, Sequence

TOOLS = Path(__file__).resolve().parent
ROOT = Path(__file__).resolve().parents[2]
SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-evidence-archive.v1.json"

sys.path.insert(0, str(TOOLS))
import run_benchmark  # noqa: E402
import run_comparison  # noqa: E402
import summarize_comparisons  # noqa: E402
import validate_result  # noqa: E402

MANIFEST_FILE = "evidence-manifest.v1.json"
ARCHIVE_CONTRACT = "pliego.canonical-tar-gzip.v1"
ARCHIVE_ROOT_PATTERN = re.compile(
    r"pliego-benchmark-v0\.3\.3-minimal-static-gh-run-([1-9][0-9]*)-attempt-([1-9][0-9]*)"
)
EXPECTED_REPEATS = (1, 2, 3)
MAX_ARCHIVE_MEMBERS = 32
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
FILE_MODE = 0o644
DIRECTORY_MODE = 0o755
CANONICAL_MTIME = 0
CANONICAL_UID = 0
CANONICAL_GID = 0
GZIP_COMPRESSION_LEVEL = 9


class EvidenceError(ValueError):
    """The input cannot be accepted as canonical benchmark evidence."""


@dataclass(frozen=True)
class SourceTree:
    directories: frozenset[Path]
    files: frozenset[Path]


@dataclass(frozen=True)
class HostedEvidence:
    root: Path
    series_directory: Path
    repeat_directories: tuple[Path, Path, Path]
    runs: tuple[summarize_comparisons.HostedRun, ...]
    series: dict[str, Any]


@dataclass(frozen=True)
class ArchiveAssets:
    archive: Path
    checksum: Path
    manifest: dict[str, Any]


def fail(message: str) -> NoReturn:
    print(f"package_hosted_evidence: {message}", file=sys.stderr)
    raise SystemExit(1)


def _sha256_bytes(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _canonical_json_bytes(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n").encode("utf-8")


def _normalized_path_key(value: str) -> str:
    return unicodedata.normalize("NFC", value).casefold()


def _is_reparse_point(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    return bool(reparse_flag and attributes & reparse_flag)


def _require_safe_component(name: str) -> None:
    if not name or name in {".", ".."} or "/" in name or "\\" in name or "\0" in name:
        raise EvidenceError(f"unsafe filesystem entry name: {name!r}")


def scan_source_tree(root: Path) -> SourceTree:
    """Scan without following links and reject aliases or special files."""

    root = root.absolute()
    try:
        root_metadata = root.lstat()
    except OSError as error:
        raise EvidenceError(f"cannot inspect source tree {root}: {error}") from error
    if (
        not stat.S_ISDIR(root_metadata.st_mode)
        or stat.S_ISLNK(root_metadata.st_mode)
        or _is_reparse_point(root_metadata)
    ):
        raise EvidenceError("source tree must be a real directory, not a link or reparse point")

    directories: set[Path] = {root}
    files: set[Path] = set()
    normalized_paths: dict[str, str] = {}
    file_identities: dict[tuple[int, int], str] = {}

    def visit(directory: Path, relative: PurePosixPath | None = None) -> None:
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name.encode("utf-8"))
        except (OSError, UnicodeEncodeError) as error:
            raise EvidenceError(f"cannot enumerate source tree directory {directory}: {error}") from error
        for entry in entries:
            _require_safe_component(entry.name)
            child_relative = PurePosixPath(entry.name) if relative is None else relative / entry.name
            child_text = child_relative.as_posix()
            normalized = _normalized_path_key(child_text)
            previous = normalized_paths.setdefault(normalized, child_text)
            if previous != child_text:
                raise EvidenceError(f"source tree contains colliding paths: {previous!r} and {child_text!r}")
            try:
                # ``DirEntry.stat().st_nlink`` is reported as zero by some
                # Windows filesystems even when a direct lstat reports the
                # real link count. Use the path syscall for the security gate.
                metadata = os.lstat(entry.path)
            except OSError as error:
                raise EvidenceError(f"cannot inspect source tree entry {child_text}: {error}") from error
            child = Path(entry.path)
            if stat.S_ISLNK(metadata.st_mode) or _is_reparse_point(metadata):
                raise EvidenceError(f"source tree contains a link or reparse point: {child_text}")
            if stat.S_ISDIR(metadata.st_mode):
                directories.add(child)
                visit(child, child_relative)
                continue
            if not stat.S_ISREG(metadata.st_mode):
                raise EvidenceError(f"source tree contains a non-regular file: {child_text}")
            if metadata.st_nlink != 1:
                raise EvidenceError(f"source tree contains a hard-linked file: {child_text}")
            identity = (metadata.st_dev, metadata.st_ino)
            if identity in file_identities:
                raise EvidenceError(
                    f"source tree files share one filesystem identity: {file_identities[identity]!r} and {child_text!r}"
                )
            file_identities[identity] = child_text
            files.add(child)

    visit(root)
    return SourceTree(directories=frozenset(directories), files=frozenset(files))


def _ancestors_through(path: Path, root: Path) -> set[Path]:
    ancestors: set[Path] = set()
    current = path
    while True:
        ancestors.add(current)
        if current == root:
            return ancestors
        if current.parent == current:
            raise EvidenceError(f"bundle directory {path} is outside source root {root}")
        current = current.parent


def _load_json_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise EvidenceError(f"{path} must contain a JSON object")
    return value


def _validate_product_identity(series: dict[str, Any], runs: Sequence[summarize_comparisons.HostedRun]) -> None:
    source = series["source"]
    if series["fixture"]["id"] != "minimal-static" or series["protocol"]["fixture_id"] != "minimal-static":
        raise EvidenceError("evidence archive requires the minimal-static fixture")
    if set(series["protocol"]["targets"]) != set(run_comparison.TARGET_IDS):
        raise EvidenceError(f"evidence archive requires targets {run_comparison.TARGET_IDS!r}")
    if not re.fullmatch(r"[0-9a-f]{40}", source["revision"]):
        raise EvidenceError("evidence archive requires one exact 40-character source revision")
    if source["run_id"] < 1 or source["run_attempt"] < 1:
        raise EvidenceError("evidence archive requires positive GitHub run and attempt identifiers")
    pliego = next((target for target in series["targets"] if target["id"] == "pliego-0.3.3"), None)
    if pliego is None:
        raise EvidenceError("evidence archive requires the pliego-0.3.3 target")
    engine = pliego["engine"]
    if engine.get("version") != "0.3.3" or engine.get("release_tag") != "v0.3.3":
        raise EvidenceError("evidence archive requires the immutable Pliego v0.3.3 target")
    if [run.comparison["source"]["repeat"] for run in runs] != list(EXPECTED_REPEATS):
        raise EvidenceError("evidence archive requires repeats 1, 2, and 3 in order")
    if any(run.comparison["runner"]["environment"] != "github-hosted" for run in runs):
        raise EvidenceError("evidence archive requires GitHub-hosted runner evidence")


def load_source_evidence(root: Path) -> HostedEvidence:
    """Discover and validate exactly one series plus exactly three source runs."""

    root = root.absolute()
    scanned = scan_source_tree(root)
    files_by_directory: dict[Path, set[str]] = {}
    for path in scanned.files:
        files_by_directory.setdefault(path.parent, set()).add(path.name)
    series_candidates = sorted(
        (
            directory
            for directory, names in files_by_directory.items()
            if names == summarize_comparisons.SERIES_BUNDLE_ENTRIES
        ),
        key=lambda path: str(path).encode("utf-8"),
    )
    repeat_candidates = sorted(
        (directory for directory, names in files_by_directory.items() if names == run_comparison.REQUIRED_BUNDLE_FILES),
        key=lambda path: str(path).encode("utf-8"),
    )
    if len(series_candidates) != 1:
        raise EvidenceError(
            f"source tree must contain exactly one hosted-series bundle, found {len(series_candidates)}"
        )
    if len(repeat_candidates) != len(EXPECTED_REPEATS):
        raise EvidenceError(
            f"source tree must contain exactly three hosted-comparison bundles, found {len(repeat_candidates)}"
        )

    try:
        runs = tuple(summarize_comparisons.load_and_validate_runs(repeat_candidates))
    except ValueError as error:
        raise EvidenceError(str(error)) from error
    series_directory = series_candidates[0]
    series_violations = summarize_comparisons.validate_series_bundle(series_directory, runs)
    if series_violations:
        raise EvidenceError(f"hosted series bundle failed validation: {series_violations[0]}")
    series = _load_json_object(series_directory / summarize_comparisons.SERIES_FILE)
    _validate_product_identity(series, runs)

    ordered_directories = tuple(run.directory for run in runs)
    bundle_directories = (series_directory, *ordered_directories)
    expected_files = {
        *(series_directory / name for name in summarize_comparisons.SERIES_BUNDLE_ENTRIES),
        *(directory / name for directory in ordered_directories for name in run_comparison.REQUIRED_BUNDLE_FILES),
    }
    if set(scanned.files) != expected_files:
        extras = sorted(str(path.relative_to(root)) for path in set(scanned.files) - expected_files)
        missing = sorted(str(path.relative_to(root)) for path in expected_files - set(scanned.files))
        raise EvidenceError(f"source tree file set is not exact; extras={extras!r}, missing={missing!r}")
    expected_directories = {root}
    for directory in bundle_directories:
        expected_directories.update(_ancestors_through(directory, root))
    if set(scanned.directories) != expected_directories:
        extras = sorted(str(path.relative_to(root)) for path in set(scanned.directories) - expected_directories)
        raise EvidenceError(f"source tree contains unexpected directories: {extras!r}")
    return HostedEvidence(
        root=root,
        series_directory=series_directory,
        repeat_directories=ordered_directories,
        runs=runs,
        series=series,
    )


def _read_stable_regular_file(path: Path) -> bytes:
    try:
        before = path.lstat()
        if (
            not stat.S_ISREG(before.st_mode)
            or stat.S_ISLNK(before.st_mode)
            or _is_reparse_point(before)
            or before.st_nlink != 1
        ):
            raise EvidenceError(f"source changed into a linked or non-regular file: {path}")
        contents = path.read_bytes()
        after = path.lstat()
    except OSError as error:
        raise EvidenceError(f"cannot read source file {path}: {error}") from error
    identity_before = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_nlink)
    identity_after = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_nlink)
    if identity_before != identity_after or len(contents) != after.st_size:
        raise EvidenceError(f"source file changed while it was read: {path}")
    return contents


def normalized_payload(evidence: HostedEvidence) -> dict[str, bytes]:
    payload: dict[str, bytes] = {}
    for name in sorted(summarize_comparisons.SERIES_BUNDLE_ENTRIES):
        payload[f"series/{name}"] = _read_stable_regular_file(evidence.series_directory / name)
    for repeat, directory in zip(EXPECTED_REPEATS, evidence.repeat_directories, strict=True):
        for name in sorted(run_comparison.REQUIRED_BUNDLE_FILES):
            payload[f"repeats/repeat-{repeat}/{name}"] = _read_stable_regular_file(directory / name)
    return payload


def _release_identity(source: dict[str, Any]) -> dict[str, str]:
    run_id = source["run_id"]
    attempt = source["run_attempt"]
    archive_root = f"pliego-benchmark-v0.3.3-minimal-static-gh-run-{run_id}-attempt-{attempt}"
    archive_filename = f"{archive_root}.tar.gz"
    return {
        "tag": f"benchmark-v0.3.3-minimal-static-gh-{run_id}-a{attempt}",
        "archive_root": archive_root,
        "archive_filename": archive_filename,
        "checksum_filename": f"{archive_filename}.sha256",
    }


def manifest_sha256(data: dict[str, Any]) -> str:
    return run_benchmark.canonical_json_sha256({key: value for key, value in data.items() if key != "manifest_sha256"})


def build_manifest(evidence: HostedEvidence, payload: dict[str, bytes]) -> dict[str, Any]:
    pliego = next(target for target in evidence.series["targets"] if target["id"] == "pliego-0.3.3")
    data: dict[str, Any] = {
        "schema": "pliego.benchmark-evidence-archive",
        "version": 1,
        "evidence_class": "github-hosted-exploratory",
        "publication_status": "packaged-hosted-series",
        "claim_boundary": summarize_comparisons.CLAIM_BOUNDARY,
        "manifest_sha256": "",
        "generated_at": evidence.series["generated_at"],
        "archive_format": {
            "contract": ARCHIVE_CONTRACT,
            "container": "tar.gz",
            "tar_format": "ustar",
            "gzip_compression_level": GZIP_COMPRESSION_LEVEL,
            "mtime": CANONICAL_MTIME,
            "uid": CANONICAL_UID,
            "gid": CANONICAL_GID,
            "file_mode": "0644",
            "directory_mode": "0755",
        },
        "release": _release_identity(evidence.series["source"]),
        "source": deepcopy(evidence.series["source"]),
        "target": {
            "id": "pliego-0.3.3",
            "version": pliego["engine"]["version"],
            "release_tag": pliego["engine"]["release_tag"],
            "fixture": evidence.series["fixture"]["id"],
            "runner_environment": "github-hosted",
        },
        "series": {
            "path": "series",
            "series_sha256": evidence.series["series_sha256"],
        },
        "repeats": [
            {
                "repeat": run["repeat"],
                "path": f"repeats/repeat-{run['repeat']}",
                "comparison_sha256": run["comparison_sha256"],
                "interleaved_artifact_sha256": run["interleaved_artifact_sha256"],
                "schedule_sha256": run["schedule_sha256"],
            }
            for run in evidence.series["runs"]
        ],
        "files": [
            {"path": path, "bytes": len(contents), "sha256": _sha256_bytes(contents)}
            for path, contents in sorted(payload.items(), key=lambda item: item[0].encode("utf-8"))
        ],
    }
    data["manifest_sha256"] = manifest_sha256(data)
    return data


def validate_manifest(data: Any) -> list[validate_result.Violation]:
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    violations: list[validate_result.Violation] = []
    validate_result.validate(data, schema, "$", violations)
    if violations:
        return violations
    assert isinstance(data, dict)
    validate_result.require_equal("$.manifest_sha256", data["manifest_sha256"], manifest_sha256(data), violations)
    validate_result.require_equal(
        "$.repeats", [entry["repeat"] for entry in data["repeats"]], list(EXPECTED_REPEATS), violations
    )
    paths = [entry["path"] for entry in data["files"]]
    if len(paths) != len(set(paths)):
        violations.append(validate_result.Violation("$.files", "paths must be unique"))
    validate_result.require_equal(
        "$.files.paths", paths, sorted(paths, key=lambda path: path.encode("utf-8")), violations
    )
    expected_release = _release_identity(data["source"])
    validate_result.require_equal("$.release", data["release"], expected_release, violations)
    return violations


def _archive_directories(payload_paths: Sequence[str]) -> set[str]:
    directories: set[str] = set()
    for value in payload_paths:
        parent = PurePosixPath(value).parent
        while str(parent) != ".":
            directories.add(parent.as_posix())
            parent = parent.parent
    return directories


def _tar_info(name: str, *, directory: bool, size: int = 0) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE if directory else tarfile.REGTYPE
    info.mode = DIRECTORY_MODE if directory else FILE_MODE
    info.uid = CANONICAL_UID
    info.gid = CANONICAL_GID
    info.uname = ""
    info.gname = ""
    info.mtime = CANONICAL_MTIME
    info.size = 0 if directory else size
    return info


def _write_canonical_archive(path: Path, archive_root: str, payload: dict[str, bytes]) -> None:
    entries: list[tuple[str, bool, bytes | None]] = [(archive_root, True, None)]
    for directory in _archive_directories(list(payload)):
        entries.append((f"{archive_root}/{directory}", True, None))
    for relative, contents in payload.items():
        entries.append((f"{archive_root}/{relative}", False, contents))
    entries.sort(key=lambda entry: entry[0].encode("utf-8"))

    with path.open("xb") as raw:
        with gzip.GzipFile(
            filename="",
            mode="wb",
            compresslevel=GZIP_COMPRESSION_LEVEL,
            fileobj=raw,
            mtime=CANONICAL_MTIME,
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT) as archive:
                for name, directory, contents in entries:
                    if directory:
                        archive.addfile(_tar_info(name, directory=True))
                    else:
                        assert contents is not None
                        archive.addfile(_tar_info(name, directory=False, size=len(contents)), io.BytesIO(contents))
        raw.flush()
        os.fsync(raw.fileno())


def _write_checksum(path: Path, archive: Path) -> None:
    contents = f"{_sha256_file(archive)}  {archive.name}\n".encode("ascii")
    with path.open("xb") as stream:
        stream.write(contents)
        stream.flush()
        os.fsync(stream.fileno())


def _require_plain_file(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise EvidenceError(f"cannot inspect {label} {path}: {error}") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse_point(metadata)
        or metadata.st_nlink != 1
    ):
        raise EvidenceError(f"{label} must be one non-linked regular file")


def _read_and_validate_checksum(checksum: Path, archive: Path) -> None:
    _require_plain_file(checksum, "checksum")
    if checksum.name != f"{archive.name}.sha256":
        raise EvidenceError(f"checksum filename must be {archive.name}.sha256")
    try:
        text = checksum.read_text(encoding="ascii")
    except (OSError, UnicodeError) as error:
        raise EvidenceError(f"cannot read checksum {checksum}: {error}") from error
    match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)\n", text)
    if match is None:
        raise EvidenceError("checksum must contain one canonical SHA-256 line")
    digest, filename = match.groups()
    if filename != archive.name:
        raise EvidenceError(f"checksum names {filename!r}, expected {archive.name!r}")
    if digest != _sha256_file(archive):
        raise EvidenceError("archive SHA-256 does not match its checksum")


def _safe_member_path(name: str) -> PurePosixPath:
    if not name or "\\" in name or "\0" in name:
        raise EvidenceError(f"archive contains an unsafe member path: {name!r}")
    path = PurePosixPath(name)
    if path.is_absolute() or path.as_posix() != name or any(part in {"", ".", ".."} for part in path.parts):
        raise EvidenceError(f"archive contains a non-canonical or traversing member path: {name!r}")
    return path


def _validate_member_metadata(member: tarfile.TarInfo) -> None:
    expected_mode = DIRECTORY_MODE if member.isdir() else FILE_MODE
    if member.mode != expected_mode:
        raise EvidenceError(f"archive member {member.name!r} has non-canonical mode {member.mode:o}")
    if member.uid != CANONICAL_UID or member.gid != CANONICAL_GID or member.uname or member.gname:
        raise EvidenceError(f"archive member {member.name!r} has non-canonical ownership metadata")
    if member.mtime != CANONICAL_MTIME:
        raise EvidenceError(f"archive member {member.name!r} has non-canonical modification time")
    if member.pax_headers:
        raise EvidenceError(f"archive member {member.name!r} carries non-canonical PAX metadata")
    if getattr(member, "sparse", None):
        raise EvidenceError(f"archive member {member.name!r} is sparse")


def _read_archive_payload(archive_path: Path) -> tuple[str, dict[str, bytes]]:
    _require_plain_file(archive_path, "archive")
    try:
        with tarfile.open(archive_path, mode="r:gz", errorlevel=2) as archive:
            members = archive.getmembers()
            if not members or len(members) > MAX_ARCHIVE_MEMBERS:
                raise EvidenceError(f"archive must contain 1-{MAX_ARCHIVE_MEMBERS} members")
            names: set[str] = set()
            normalized_names: dict[str, str] = {}
            paths: dict[str, PurePosixPath] = {}
            total_size = 0
            for member in members:
                path = _safe_member_path(member.name)
                if member.name in names:
                    raise EvidenceError(f"archive contains duplicate member path: {member.name!r}")
                names.add(member.name)
                normalized = _normalized_path_key(member.name)
                previous = normalized_names.setdefault(normalized, member.name)
                if previous != member.name:
                    raise EvidenceError(f"archive contains colliding member paths: {previous!r} and {member.name!r}")
                paths[member.name] = path
                if not member.isdir() and not member.isreg():
                    raise EvidenceError(
                        f"archive member {member.name!r} is a link, device, FIFO, or other special type"
                    )
                _validate_member_metadata(member)
                if member.isreg():
                    total_size += member.size
                    if member.size < 1 or total_size > MAX_ARCHIVE_BYTES:
                        raise EvidenceError(
                            "archive regular-file payload is empty or exceeds the retained-evidence limit"
                        )

            roots = {path.parts[0] for path in paths.values()}
            if len(roots) != 1:
                raise EvidenceError(f"archive must have exactly one root directory, found {sorted(roots)!r}")
            archive_root = next(iter(roots))
            if ARCHIVE_ROOT_PATTERN.fullmatch(archive_root) is None:
                raise EvidenceError(f"archive root has an invalid evidence identity: {archive_root!r}")

            relative_files: dict[str, bytes] = {}
            actual_directories: set[str] = set()
            for member in members:
                path = paths[member.name]
                if len(path.parts) == 1:
                    if not member.isdir():
                        raise EvidenceError("archive root must be a directory")
                    continue
                relative = PurePosixPath(*path.parts[1:]).as_posix()
                if member.isdir():
                    actual_directories.add(relative)
                    continue
                stream = archive.extractfile(member)
                if stream is None:
                    raise EvidenceError(f"cannot read archive member {member.name!r}")
                contents = stream.read(member.size + 1)
                if len(contents) != member.size:
                    raise EvidenceError(f"archive member {member.name!r} is truncated or oversized")
                relative_files[relative] = contents
    except EvidenceError:
        raise
    except (OSError, EOFError, tarfile.TarError) as error:
        raise EvidenceError(f"cannot parse evidence archive: {error}") from error

    expected_files = {
        MANIFEST_FILE,
        *(f"series/{name}" for name in summarize_comparisons.SERIES_BUNDLE_ENTRIES),
        *(
            f"repeats/repeat-{repeat}/{name}"
            for repeat in EXPECTED_REPEATS
            for name in run_comparison.REQUIRED_BUNDLE_FILES
        ),
    }
    if set(relative_files) != expected_files:
        extras = sorted(set(relative_files) - expected_files)
        missing = sorted(expected_files - set(relative_files))
        raise EvidenceError(f"archive file set is not exact; extras={extras!r}, missing={missing!r}")
    expected_directories = _archive_directories(list(expected_files))
    if actual_directories != expected_directories:
        extras = sorted(actual_directories - expected_directories)
        missing = sorted(expected_directories - actual_directories)
        raise EvidenceError(f"archive directory set is not exact; extras={extras!r}, missing={missing!r}")
    return archive_root, relative_files


def _materialize_payload(payload: dict[str, bytes], destination: Path) -> None:
    for relative, contents in payload.items():
        path = destination / Path(PurePosixPath(relative))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)


def _files_equal(left: Path, right: Path) -> bool:
    if left.stat().st_size != right.stat().st_size:
        return False
    with left.open("rb") as left_stream, right.open("rb") as right_stream:
        while True:
            left_chunk = left_stream.read(1024 * 1024)
            right_chunk = right_stream.read(1024 * 1024)
            if left_chunk != right_chunk:
                return False
            if not left_chunk:
                return True


def validate_evidence_archive(archive: Path, checksum: Path | None = None) -> dict[str, Any]:
    """Validate safety, nested evidence, provenance, and canonical bytes."""

    archive = archive.absolute()
    checksum = checksum.absolute() if checksum is not None else archive.with_name(f"{archive.name}.sha256")
    _read_and_validate_checksum(checksum, archive)
    archive_root, files = _read_archive_payload(archive)
    try:
        manifest = json.loads(files[MANIFEST_FILE].decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"cannot parse {MANIFEST_FILE}: {error}") from error
    manifest_violations = validate_manifest(manifest)
    if manifest_violations:
        raise EvidenceError(f"archive manifest failed validation: {manifest_violations[0]}")
    assert isinstance(manifest, dict)

    payload = {path: contents for path, contents in files.items() if path != MANIFEST_FILE}
    with tempfile.TemporaryDirectory(prefix="pliego-evidence-validate-") as temporary:
        materialized = Path(temporary) / "source"
        materialized.mkdir()
        _materialize_payload(payload, materialized)
        evidence = load_source_evidence(materialized)
        expected_manifest = build_manifest(evidence, normalized_payload(evidence))
        if manifest != expected_manifest:
            raise EvidenceError("archive manifest is not the exact derivation of its retained source bundles")
        expected_release = expected_manifest["release"]
        if archive_root != expected_release["archive_root"]:
            raise EvidenceError("archive root is not bound to its retained GitHub run identity")
        if archive.name != expected_release["archive_filename"]:
            raise EvidenceError("archive filename is not bound to its retained GitHub run identity")
        if checksum.name != expected_release["checksum_filename"]:
            raise EvidenceError("checksum filename is not bound to its retained GitHub run identity")

        canonical = Path(temporary) / expected_release["archive_filename"]
        _write_canonical_archive(canonical, archive_root, files)
        if not _files_equal(archive, canonical):
            raise EvidenceError("archive bytes are not the canonical reproducible tar.gz encoding")
    return manifest


def build_evidence_archive(source: Path, output: Path) -> ArchiveAssets:
    """Build, checksum, and independently revalidate a canonical archive."""

    evidence = load_source_evidence(source)
    payload = normalized_payload(evidence)
    manifest = build_manifest(evidence, payload)
    manifest_violations = validate_manifest(manifest)
    if manifest_violations:
        raise EvidenceError(f"generated manifest failed validation: {manifest_violations[0]}")
    payload_with_manifest = {MANIFEST_FILE: _canonical_json_bytes(manifest), **payload}
    release = manifest["release"]

    output = output.absolute()
    if output.exists():
        if output.is_symlink() or not output.is_dir():
            raise EvidenceError("output must be a real directory")
        if any(output.iterdir()):
            raise EvidenceError("output directory must be empty")
    else:
        output.mkdir(parents=True)
    archive = output / release["archive_filename"]
    checksum = output / release["checksum_filename"]
    temporary_archive = output / f".{archive.name}.tmp"
    temporary_checksum = output / f".{checksum.name}.tmp"
    try:
        _write_canonical_archive(temporary_archive, release["archive_root"], payload_with_manifest)
        os.replace(temporary_archive, archive)
        _write_checksum(temporary_checksum, archive)
        os.replace(temporary_checksum, checksum)
        validated = validate_evidence_archive(archive, checksum)
        if validated != manifest:
            raise EvidenceError("post-build validation returned different manifest metadata")
    except Exception:
        for path in (temporary_archive, temporary_checksum, archive, checksum):
            try:
                path.unlink()
            except FileNotFoundError:
                pass
        raise
    return ArchiveAssets(archive=archive, checksum=checksum, manifest=manifest)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build_parser = commands.add_parser("build", help="validate and package one final hosted-series artifact tree")
    build_parser.add_argument("source", type=Path, help="artifact tree containing the series and all three repeats")
    build_parser.add_argument("--out", required=True, type=Path, help="new or empty output directory for two assets")
    validate_parser = commands.add_parser("validate", help="validate one canonical archive and checksum")
    validate_parser.add_argument("archive", type=Path, help="canonical .tar.gz evidence asset")
    validate_parser.add_argument("--checksum", type=Path, help="SHA-256 sidecar; defaults to ARCHIVE.sha256")
    args = parser.parse_args(argv)
    try:
        if args.command == "build":
            assets = build_evidence_archive(args.source, args.out)
            print(f"wrote {assets.archive}")
            print(f"wrote {assets.checksum}")
            print(f"manifest {assets.manifest['manifest_sha256']}")
        else:
            manifest = validate_evidence_archive(args.archive, args.checksum)
            print(
                "validated canonical hosted evidence "
                f"{manifest['release']['tag']} at revision {manifest['source']['revision']}"
            )
    except EvidenceError as error:
        fail(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
