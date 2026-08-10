#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fetch, verify, and safely extract the immutable Linux benchmark runtime."""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import re
import shutil
import sys
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.request
from pathlib import Path, PurePosixPath
from typing import Any, IO

import benchmark_publication

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "benchmarks" / "manifest.toml"
RUNTIMES = ROOT / "sdk" / "laravel" / "resources" / "runtimes.json"

sys.path.insert(0, str(ROOT / "python"))
from check_pliego_release_archive import check_archive  # noqa: E402


class ReleaseError(RuntimeError):
    pass


def sha256_stream(source: IO[bytes]) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    for chunk in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(chunk)
        size += len(chunk)
    return digest.hexdigest(), size


def sha256_file(path: Path) -> tuple[str, int]:
    with path.open("rb") as source:
        return sha256_stream(source)


def load_release(target_id: str) -> tuple[dict[str, Any], dict[str, Any]]:
    targets = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))["targets"]
    target = targets.get(target_id)
    if not target or not target.get("enabled"):
        raise ReleaseError(f"target {target_id!r} is not enabled")

    required = {
        "version",
        "repository",
        "release_tag",
        "commit",
        "servo_build",
        "servo_base",
        "platform",
        "profile",
        "archive",
        "archive_bytes",
        "archive_sha256",
        "binary",
        "binary_bytes",
        "binary_sha256",
    }
    missing = sorted(required - target.keys())
    if missing:
        raise ReleaseError(f"target {target_id!r} lacks immutable release fields: {missing}")
    if target["release_tag"] != f"v{target['version']}":
        raise ReleaseError("release tag is not the target's exact version")
    if target["platform"] != "linux-x86_64" or target["profile"] != "checked-release":
        raise ReleaseError("benchmark runtime must be linux-x86_64 checked-release")
    if target["archive"] != f"pliego-{target['version']}-linux-x86_64.tar.gz":
        raise ReleaseError("archive name is mutable, unversioned, or for the wrong platform")
    if target["binary"] != "pliego":
        raise ReleaseError("Linux release binary must be named pliego")
    if not isinstance(target["repository"], str) or not target["repository"].startswith("https://github.com/"):
        raise ReleaseError("release repository must be an HTTPS GitHub URL")
    if not all(isinstance(target[key], int) and target[key] > 0 for key in ("archive_bytes", "binary_bytes")):
        raise ReleaseError("release byte counts must be positive integers")
    for key, length in (("commit", 40), ("servo_base", 40), ("archive_sha256", 64), ("binary_sha256", 64)):
        if not isinstance(target[key], str) or re.fullmatch(f"[0-9a-f]{{{length}}}", target[key]) is None:
            raise ReleaseError(f"target {key} is not a pinned digest")

    runtimes = benchmark_publication.strict_json_loads(RUNTIMES.read_bytes(), "Laravel runtime manifest")
    if runtimes.get("version") != target["version"]:
        raise ReleaseError("Laravel runtime manifest version differs from benchmark target")
    runtime = runtimes.get("assets", {}).get("linux-x86_64")
    if not isinstance(runtime, dict):
        raise ReleaseError("Laravel runtime manifest lacks linux-x86_64")
    if runtime.get("bytes") != target["archive_bytes"] or runtime.get("sha256") != target["archive_sha256"]:
        raise ReleaseError("benchmark target differs from the published Laravel runtime manifest")
    if not isinstance(runtime.get("files"), list) or not runtime["files"]:
        raise ReleaseError("Laravel runtime manifest has no immutable file list")
    if len(runtime["files"]) != len(set(runtime["files"])):
        raise ReleaseError("Laravel runtime manifest contains duplicate files")
    if not all(
        isinstance(path, str)
        and path
        and not PurePosixPath(path).is_absolute()
        and ".." not in PurePosixPath(path).parts
        for path in runtime["files"]
    ):
        raise ReleaseError("Laravel runtime manifest contains an unsafe file path")
    return target, runtime


def member_path(name: str, expected_root: str) -> str:
    path = PurePosixPath(name.replace("\\", "/"))
    if path.is_absolute() or ".." in path.parts or not path.parts or path.parts[0] != expected_root:
        raise ReleaseError(f"unsafe archive member: {name}")
    if len(path.parts) == 1:
        return ""
    return PurePosixPath(*path.parts[1:]).as_posix()


def verify_archive(archive: Path, target: dict[str, Any], runtime: dict[str, Any]) -> dict[str, Any]:
    if archive.name != target["archive"]:
        raise ReleaseError(f"archive must be named {target['archive']}")
    archive_hash, archive_bytes = sha256_file(archive)
    if archive_bytes != target["archive_bytes"] or archive_hash != target["archive_sha256"]:
        raise ReleaseError("release archive size or SHA-256 differs from the immutable target")

    errors = check_archive(
        archive,
        version=target["version"],
        bundle=target["platform"],
        repository_url=target["repository"],
    )
    if errors:
        raise ReleaseError(errors[0])

    root = f"pliego-{target['version']}-{target['platform']}"
    with tarfile.open(archive, "r:gz") as source:
        files: dict[str, tarfile.TarInfo] = {}
        for member in source.getmembers():
            relative = member_path(member.name, root)
            if member.isdir():
                continue
            if not member.isfile() or not relative or relative in files:
                raise ReleaseError(f"unsupported or duplicate archive member: {member.name}")
            files[relative] = member
        if set(files) != set(runtime["files"]):
            raise ReleaseError("release archive file set differs from the published runtime manifest")

        binary = source.extractfile(files[target["binary"]])
        if binary is None:
            raise ReleaseError("release archive binary cannot be read")
        binary_hash, binary_bytes = sha256_stream(binary)
        if binary_bytes != target["binary_bytes"] or binary_hash != target["binary_sha256"]:
            raise ReleaseError("release binary size or SHA-256 differs from the immutable target")

        version_file = source.extractfile(files["VERSION.txt"])
        if version_file is None:
            raise ReleaseError("VERSION.txt cannot be read")
        version_text = version_file.read().decode("utf-8").replace("\r\n", "\n")
        expected_lines = {
            f"pliego {target['version']}",
            f"Servo {target['servo_build']}",
            f"Servo base {target['servo_base']}",
        }
        if not expected_lines.issubset(set(version_text.splitlines())):
            raise ReleaseError("VERSION.txt differs from the pinned native/Servo identity")

    return {
        "archive_sha256": archive_hash,
        "archive_bytes": archive_bytes,
        "binary_sha256": binary_hash,
        "binary_bytes": binary_bytes,
    }


def verify_install(directory: Path, target: dict[str, Any], runtime: dict[str, Any]) -> Path:
    files = {path.relative_to(directory).as_posix() for path in directory.rglob("*") if path.is_file()}
    if files != set(runtime["files"]):
        raise ReleaseError("cached runtime file set differs from the published manifest")
    binary = directory / target["binary"]
    digest, size = sha256_file(binary)
    if size != target["binary_bytes"] or digest != target["binary_sha256"]:
        raise ReleaseError("cached release binary is corrupt")
    return binary


def install_archive(archive: Path, cache: Path, target: dict[str, Any], runtime: dict[str, Any]) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    root = f"pliego-{target['version']}-{target['platform']}"
    installed = cache / root
    if installed.exists():
        return verify_install(installed, target, runtime)

    stage_parent = Path(tempfile.mkdtemp(prefix=".extract-", dir=cache))
    stage = stage_parent / root
    try:
        with tarfile.open(archive, "r:gz") as source:
            for member in source.getmembers():
                relative = member_path(member.name, root)
                if member.isdir():
                    continue
                if not member.isfile() or relative not in runtime["files"]:
                    raise ReleaseError(f"refusing archive member: {member.name}")
                destination = stage / PurePosixPath(relative)
                destination.parent.mkdir(parents=True, exist_ok=True)
                payload = source.extractfile(member)
                if payload is None:
                    raise ReleaseError(f"cannot read archive member: {member.name}")
                with destination.open("wb") as output:
                    shutil.copyfileobj(payload, output)
        binary = verify_install(stage, target, runtime)
        binary.chmod(0o755)
        os.replace(stage, installed)
        return installed / target["binary"]
    finally:
        shutil.rmtree(stage_parent, ignore_errors=True)


def obtain_archive(
    cache: Path, supplied: Path | None, offline: bool, target: dict[str, Any], runtime: dict[str, Any]
) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    cached = cache / target["archive"]
    if supplied is not None:
        supplied = supplied.resolve()
        verify_archive(supplied, target, runtime)
        if not cached.exists():
            with tempfile.TemporaryDirectory(prefix=".download-", dir=cache) as stage:
                temporary = Path(stage) / target["archive"]
                shutil.copyfile(supplied, temporary)
                verify_archive(temporary, target, runtime)
                os.replace(temporary, cached)
    if cached.exists():
        verify_archive(cached, target, runtime)
        return cached
    if offline:
        raise ReleaseError("verified release archive is not cached and --offline forbids download")

    url = f"{target['repository']}/releases/download/{target['release_tag']}/{target['archive']}"
    request = urllib.request.Request(url, headers={"User-Agent": "pliego-benchmark-release-verifier"})
    with tempfile.TemporaryDirectory(prefix=".download-", dir=cache) as stage:
        temporary = Path(stage) / target["archive"]
        with urllib.request.urlopen(request, timeout=120) as response, temporary.open("wb") as output:
            shutil.copyfileobj(response, output)
        verify_archive(temporary, target, runtime)
        os.replace(temporary, cached)
    return cached


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="pliego-0.1.1")
    parser.add_argument("--cache", type=Path, default=Path.home() / ".cache" / "pliego-benchmarks")
    parser.add_argument("--archive", type=Path, help="verify this exact archive instead of downloading")
    parser.add_argument("--offline", action="store_true", help="refuse network access")
    parser.add_argument("--metadata-out", type=Path, help="write verified release metadata as JSON")
    args = parser.parse_args()

    try:
        if platform.system() != "Linux" or platform.machine().lower() not in ("x86_64", "amd64"):
            raise ReleaseError("published benchmark releases require Linux x86_64")
        target, runtime = load_release(args.target)
        archive = obtain_archive(args.cache.resolve(), args.archive, args.offline, target, runtime)
        evidence = verify_archive(archive, target, runtime)
        binary = install_archive(archive, args.cache.resolve(), target, runtime)
        metadata = {
            "schema": "pliego.verified-release",
            "version": 1,
            "target": args.target,
            "release_tag": target["release_tag"],
            "commit": target["commit"],
            "servo_build": target["servo_build"],
            "servo_base": target["servo_base"],
            "platform": target["platform"],
            "profile": target["profile"],
            "archive": str(archive),
            "binary": str(binary),
            **evidence,
        }
        if args.metadata_out:
            args.metadata_out.parent.mkdir(parents=True, exist_ok=True)
            benchmark_publication.atomic_write_bytes(
                args.metadata_out,
                benchmark_publication.json_bytes(metadata, indent=2),
            )
        print(binary)
        return 0
    except (
        KeyError,
        OSError,
        ReleaseError,
        TypeError,
        ValueError,
        tarfile.TarError,
        UnicodeError,
        urllib.error.URLError,
    ) as error:
        print(f"resolve_release: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
