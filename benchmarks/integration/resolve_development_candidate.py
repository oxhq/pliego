#!/usr/bin/env python3
"""Verify one development Linux package artifact; never a release promotion proof."""

from __future__ import annotations

import argparse
import io
import json
import re
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

from run_real_document_comparison import ROOT, digest, require, save

sys.path.insert(0, str(ROOT / "python"))
from check_pliego_release_archive import check_archive  # noqa: E402


def api(path: str) -> bytes:
    # gh owns authentication and signed-redirect handling; never copy the token
    # into evidence, URLs, a command argument, or a redirected HTTP header.
    result = subprocess.run(["gh", "api", "repos/oxhq/pliego/" + path], capture_output=True, timeout=180)
    require(result.returncode == 0, "GitHub artifact retrieval failed")
    return result.stdout


def resolve(artifact_id: int, source: str, output: Path) -> Path:
    require(artifact_id > 0 and re.fullmatch(r"[0-9a-f]{40}", source), "Exact artifact id/source SHA required")
    require(not output.exists(), "Candidate output must be fresh")
    output.mkdir(parents=True)
    metadata = json.loads(api(f"actions/artifacts/{artifact_id}"))
    require(
        metadata["name"] == "pliego-linux-x86_64" and metadata["expired"] is False, "Wrong/expired package artifact"
    )
    origin = metadata["workflow_run"]
    require(
        origin["head_sha"] == source and origin["repository_id"] == origin["head_repository_id"] == 1301051584,
        "Wrong artifact repository/source",
    )
    run = json.loads(api(f"actions/runs/{origin['id']}"))
    require(
        run["head_sha"] == source and run["path"] == ".github/workflows/pliego-package.yml",
        "Wrong package workflow/source",
    )
    save(output / "artifact.json", metadata)
    save(output / "run.json", run)
    zipped = api(f"actions/artifacts/{artifact_id}/zip")
    require(
        len(zipped) == metadata["size_in_bytes"] and len(zipped) <= 200 * 1024 * 1024, "Wrong/unbounded artifact size"
    )
    zipped_path = output / "github-artifact.zip"
    zipped_path.write_bytes(zipped)
    require("sha256:" + digest(zipped_path) == metadata["digest"], "GitHub artifact digest mismatch")
    with zipfile.ZipFile(io.BytesIO(zipped)) as bundle:
        names = bundle.namelist()
        archives = [
            name for name in names if re.fullmatch(r"pliego-[0-9]+\.[0-9]+\.[0-9]+-linux-x86_64\.tar\.gz", name)
        ]
        require(
            len(archives) == 1 and set(names) == {archives[0], archives[0] + ".sha256"} and len(names) == 2,
            "Unexpected artifact files",
        )
        name = archives[0]
        require(bundle.getinfo(name).file_size <= 200 * 1024 * 1024, "Unbounded package archive")
        archive = output / name
        archive.write_bytes(bundle.read(name))
        checksum = bundle.read(name + ".sha256").decode("ascii").strip().split()
        require(checksum == [digest(archive), name], "Package checksum mismatch")
    version = name.removeprefix("pliego-").removesuffix("-linux-x86_64.tar.gz")
    bundle_root = name.removesuffix(".tar.gz")
    with tarfile.open(archive, "r:gz") as packed:
        versions = packed.extractfile(bundle_root + "/VERSION.txt")
        require(versions is not None, "Missing VERSION contract")
        version_lines = versions.read(4096).decode("utf-8").splitlines()
    require(len(version_lines) == 4 and version_lines[2].startswith("Servo "), "Wrong version contract")
    servo_build = version_lines[2].removeprefix("Servo ")
    require(servo_build.endswith("-" + source[:9]), "Version does not bind the exact package source")
    servo_version = servo_build[: -(len(source[:9]) + 1)]
    servo_base = version_lines[3].removeprefix("Servo base ")
    require(re.fullmatch(r"[0-9a-f]{40}", servo_base), "Wrong Servo base")
    errors = check_archive(
        archive,
        version=version,
        bundle="linux-x86_64",
        repository_url="https://github.com/oxhq/pliego",
        servo_version=servo_version,
        git_sha=source[:9],
        servo_base_sha=servo_base,
    )
    require(not errors, f"Package archive contract failed: {errors}")
    extracted = output / "extracted"
    extracted.mkdir()
    with tarfile.open(archive, "r:gz") as packed:
        members = packed.getmembers()
        require(sum(member.size for member in members) <= 600 * 1024 * 1024, "Unbounded extracted package")
        require(all(member.isfile() or member.isdir() for member in members), "Package contains links/special entries")
        packed.extractall(extracted, members=members, filter="data")
    binary = extracted / bundle_root / "pliego"
    require(binary.is_file() and not binary.is_symlink(), "Missing candidate executable")
    binary.chmod(0o755)
    save(
        output / "candidate-identity.json",
        {
            "source_sha": source,
            "binary_sha256": digest(binary),
            "binary_bytes": binary.stat().st_size,
            "version": version,
            "profile": "release",
            "promotion_eligible": False,
            "provenance": {
                "kind": "development-package-artifact",
                "workflow": run["path"],
                "run_id": run["id"],
                "run_attempt": run["run_attempt"],
                "artifact_id": artifact_id,
                "artifact_sha256": metadata["digest"],
                "archive_sha256": digest(archive),
                "servo_base": servo_base,
            },
            "boundary": "Retained optimized package for diagnosis/comparison preflight, not successful matrix/release/public-package evidence.",
        },
    )
    return binary


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-id", required=True, type=int)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    print(resolve(args.artifact_id, args.source_sha, args.out.resolve()))
