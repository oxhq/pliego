#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "benchmarks" / "tools"))
sys.path.insert(0, str(ROOT / "python"))

from check_pliego_release_archive import _fixture_files, _write_fixture  # noqa: E402
from resolve_release import ReleaseError, install_archive, sha256_file, verify_archive  # noqa: E402


def main() -> None:
    version = "9.9.9"
    platform = "linux-x86_64"
    repository = "https://github.com/oxhq/pliego"
    root = f"pliego-{version}-{platform}"
    files = _fixture_files(version, platform, repository)
    files["pliego"] = b"verified fake binary\n"
    files["VERSION.txt"] = b"pliego 9.9.9\npliego-api 1\nServo test-build\nServo base " + b"a" * 40 + b"\n"

    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        archive = directory / f"{root}.tar.gz"
        _write_fixture(archive, root, files)
        archive_hash, archive_bytes = sha256_file(archive)
        target = {
            "version": version,
            "repository": repository,
            "release_tag": f"v{version}",
            "commit": "b" * 40,
            "servo_build": "test-build",
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
