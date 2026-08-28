#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import json
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "benchmarks" / "tools"))
sys.path.insert(0, str(ROOT / "python"))

from check_pliego_release_archive import _fixture_files, _write_fixture  # noqa: E402
import resolve_release  # noqa: E402
from resolve_release import ReleaseError, install_archive, load_release, sha256_file, verify_archive  # noqa: E402


def assert_committed_manifest_binding() -> None:
    published_target, published_runtime = load_release(resolve_release.DEFAULT_TARGET)
    assert published_target["version"] == "0.3.2"
    assert published_target["api"] == 2
    assert published_target["runtime_manifest_sha256"] == (
        "6d48a02bf8b60c3e947a8fd3784593f8a841e79694ba82eb36835532588ab2d9"
    )
    assert published_runtime["sha256"] == published_target["archive_sha256"]

    manifest_text = resolve_release.MANIFEST.read_text(encoding="utf-8")
    runtime_bytes = (resolve_release.ROOT / published_target["runtime_manifest"]).read_bytes()
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        copied_manifest = root / "benchmarks" / "manifest.toml"
        copied_runtime = root / published_target["runtime_manifest"]
        copied_manifest.parent.mkdir(parents=True)
        copied_runtime.parent.mkdir(parents=True)
        copied_manifest.write_text(manifest_text, encoding="utf-8")
        copied_runtime.write_bytes(runtime_bytes)

        with patch.object(resolve_release, "ROOT", root), patch.object(resolve_release, "MANIFEST", copied_manifest):
            copied_runtime.write_bytes(runtime_bytes[:-1] + b" ")
            try:
                load_release(resolve_release.DEFAULT_TARGET)
            except ReleaseError as error:
                assert "differs from the promoted release asset" in str(error)
            else:
                raise AssertionError("altered promoted runtime manifest passed its pinned hash")

            altered = json.loads(runtime_bytes)
            altered["release_ready"] = False
            altered_bytes = (json.dumps(altered, indent=4) + "\n").encode()
            altered_sha256 = hashlib.sha256(altered_bytes).hexdigest()
            altered_manifest = manifest_text.replace(
                f"runtime_manifest_bytes = {len(runtime_bytes)}",
                f"runtime_manifest_bytes = {len(altered_bytes)}",
            ).replace(published_target["runtime_manifest_sha256"], altered_sha256)
            copied_manifest.write_text(altered_manifest, encoding="utf-8")
            copied_runtime.write_bytes(altered_bytes)
            try:
                load_release(resolve_release.DEFAULT_TARGET)
            except ReleaseError as error:
                assert "contract differs from benchmark target" in str(error)
            else:
                raise AssertionError("non-finalized runtime manifest passed release semantics")


def main() -> None:
    assert_committed_manifest_binding()

    version = "9.9.9"
    platform = "linux-x86_64"
    repository = "https://github.com/oxhq/pliego"
    root = f"pliego-{version}-{platform}"
    commit = "b" * 40
    identity = {
        "servo_version": "0.4.0",
        "git_sha": commit[:7],
        "servo_base_sha": "a" * 40,
    }
    files = _fixture_files(version, platform, repository, **identity)
    files["pliego"] = b"verified fake binary\n"

    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        archive = directory / f"{root}.tar.gz"
        _write_fixture(archive, root, files)
        archive_hash, archive_bytes = sha256_file(archive)
        target = {
            "version": version,
            "repository": repository,
            "release_tag": f"v{version}",
            "commit": commit,
            "servo_build": f"{identity['servo_version']}-{identity['git_sha']}",
            "servo_base": "a" * 40,
            "platform": platform,
            "profile": "checked-release",
            "archive": archive.name,
            "archive_bytes": archive_bytes,
            "archive_sha256": archive_hash,
            "binary": "pliego",
            "binary_bytes": len(files["pliego"]),
            "binary_sha256": hashlib.sha256(files["pliego"]).hexdigest(),
        }
        runtime = {"bytes": archive_bytes, "sha256": archive_hash, "files": sorted(files)}
        verify_archive(archive, target, runtime)
        binary = install_archive(archive, directory / "cache", target, runtime)
        assert binary.read_bytes() == files["pliego"]

        corrupt = bytearray(archive.read_bytes())
        corrupt[len(corrupt) // 2] ^= 1
        archive.write_bytes(corrupt)
        try:
            verify_archive(archive, target, runtime)
        except ReleaseError as error:
            assert "SHA-256" in str(error)
        else:
            raise AssertionError("one-byte archive corruption passed verification")

    print("release resolver self-check passed")


if __name__ == "__main__":
    main()
