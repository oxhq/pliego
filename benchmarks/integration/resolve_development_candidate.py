#!/usr/bin/env python3
"""Verify one development Linux/macOS Intel package; never a release promotion proof."""

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


BUNDLES = {"linux-x86_64": "x86_64-unknown-linux-gnu", "macos-x86_64": "x86_64-apple-darwin"}


def api(path: str) -> bytes:
    # gh owns authentication and signed-redirect handling; never copy the token
    # into evidence, URLs, a command argument, or a redirected HTTP header.
    result = subprocess.run(["gh", "api", "repos/oxhq/pliego/" + path], capture_output=True, timeout=180)
    require(result.returncode == 0, "GitHub artifact retrieval failed")
    return result.stdout


def resolve(artifact_id: int, source: str, output: Path, *, bundle: str = "linux-x86_64") -> Path:
    require(artifact_id > 0 and re.fullmatch(r"[0-9a-f]{40}", source), "Exact artifact id/source SHA required")
    require(bundle in BUNDLES, "Unsupported development bundle")
    require(not output.exists(), "Candidate output must be fresh")
    output.mkdir(parents=True)
    metadata = json.loads(api(f"actions/artifacts/{artifact_id}"))
    require(metadata["name"] == f"pliego-{bundle}" and metadata["expired"] is False, "Wrong/expired package artifact")
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
    with zipfile.ZipFile(io.BytesIO(zipped)) as artifact_zip:
        names = artifact_zip.namelist()
        archives = [
            name for name in names if re.fullmatch(rf"pliego-[0-9]+\.[0-9]+\.[0-9]+-{re.escape(bundle)}\.tar\.gz", name)
        ]
        require(
            len(archives) == 1 and set(names) == {archives[0], archives[0] + ".sha256"} and len(names) == 2,
            "Unexpected artifact files",
        )
        name = archives[0]
        require(artifact_zip.getinfo(name).file_size <= 200 * 1024 * 1024, "Unbounded package archive")
        archive = output / name
        archive.write_bytes(artifact_zip.read(name))
        checksum = artifact_zip.read(name + ".sha256").decode("ascii").strip().split()
        require(checksum == [digest(archive), name], "Package checksum mismatch")
    version = name.removeprefix("pliego-").removesuffix(f"-{bundle}.tar.gz")
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
        bundle=bundle,
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
            "bundle": bundle,
            "target": BUNDLES[bundle],
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


def self_test() -> None:
    import copy
    import tempfile
    from unittest.mock import patch

    from check_pliego_release_archive import _fixture_files, _write_fixture

    source = "0123456789" * 4
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        for bundle in BUNDLES:
            name = f"pliego-0.3.3-{bundle}.tar.gz"
            archive = root / name
            files = _fixture_files(
                "0.3.3",
                bundle,
                "https://github.com/oxhq/pliego",
                servo_version="0.4.0",
                git_sha=source[:9],
                servo_base_sha="a" * 40,
            )
            _write_fixture(archive, name.removesuffix(".tar.gz"), files)
            zipped = io.BytesIO()
            with zipfile.ZipFile(zipped, "w") as target:
                target.writestr(name, archive.read_bytes())
                target.writestr(name + ".sha256", f"{digest(archive)}  {name}\n")
            zip_path = root / f"{bundle}.zip"
            zip_path.write_bytes(zipped.getvalue())
            metadata = {
                "name": f"pliego-{bundle}",
                "expired": False,
                "size_in_bytes": zip_path.stat().st_size,
                "digest": "sha256:" + digest(zip_path),
                "workflow_run": {
                    "id": 42,
                    "head_sha": source,
                    "repository_id": 1301051584,
                    "head_repository_id": 1301051584,
                },
            }
            run = {"id": 42, "head_sha": source, "path": ".github/workflows/pliego-package.yml", "run_attempt": 1}

            def responses(path: str) -> bytes:
                return {
                    "actions/artifacts/7": json.dumps(current_metadata).encode(),
                    "actions/runs/42": json.dumps(current_run).encode(),
                    "actions/artifacts/7/zip": zipped.getvalue(),
                }[path]

            current_metadata, current_run = metadata, run
            with patch(__name__ + ".api", side_effect=responses):
                options = {} if bundle == "linux-x86_64" else {"bundle": bundle}
                binary = resolve(7, source, root / (bundle + "-valid"), **options)
            require(binary.read_bytes() == b"binary", "Verified executable bytes changed")
            identity = json.loads((root / (bundle + "-valid") / "candidate-identity.json").read_bytes())
            require(identity["bundle"] == bundle and identity["target"] == BUNDLES[bundle], "Candidate target lost")
            for index, corruption in enumerate(
                ("name", "source", "repository", "workflow", "digest", "size", "expired")
            ):
                current_metadata, current_run = copy.deepcopy(metadata), copy.deepcopy(run)
                if corruption == "name":
                    current_metadata["name"] = "pliego-" + next(value for value in BUNDLES if value != bundle)
                elif corruption == "source":
                    current_metadata["workflow_run"]["head_sha"] = "f" * 40
                elif corruption == "repository":
                    current_metadata["workflow_run"]["head_repository_id"] = 1
                elif corruption == "workflow":
                    current_run["path"] = ".github/workflows/not-package.yml"
                elif corruption == "digest":
                    current_metadata["digest"] = "sha256:" + "0" * 64
                elif corruption == "size":
                    current_metadata["size_in_bytes"] += 1
                else:
                    current_metadata["expired"] = True
                with patch(__name__ + ".api", side_effect=responses):
                    try:
                        resolve(7, source, root / f"{bundle}-invalid-{index}", bundle=bundle)
                    except ValueError:
                        pass
                    else:
                        raise AssertionError(f"Resolver accepted {bundle}/{corruption}")
    print("Development resolver self-test: ok (Linux default and macOS Intel; 14 corruptions rejected)")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--artifact-id", type=int)
    parser.add_argument("--source-sha")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--bundle", choices=tuple(BUNDLES), default="linux-x86_64")
    args = parser.parse_args()
    if args.self_test:
        self_test()
    else:
        if args.artifact_id is None or args.source_sha is None or args.out is None:
            parser.error("--artifact-id, --source-sha and --out are required")
        print(resolve(args.artifact_id, args.source_sha, args.out.resolve(), bundle=args.bundle))
