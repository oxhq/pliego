#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import gzip
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from copy import deepcopy
from pathlib import Path
from typing import Callable

import package_hosted_evidence
import summarize_comparisons
from test_summarize_comparisons import make_inputs


SCRIPT = Path(__file__).with_name("package_hosted_evidence.py")
ARCHIVE_NAME = "pliego-benchmark-v0.3.3-minimal-static-gh-run-123-attempt-1.tar.gz"
CHECKSUM_NAME = f"{ARCHIVE_NAME}.sha256"
ARCHIVE_ROOT = "pliego-benchmark-v0.3.3-minimal-static-gh-run-123-attempt-1"


def expect_evidence_error(operation: Callable[[], object], expected: str) -> None:
    try:
        operation()
    except package_hosted_evidence.EvidenceError as error:
        assert expected in str(error), str(error)
    else:
        raise AssertionError(f"expected EvidenceError containing {expected!r}")


def make_final_artifact_tree(root: Path) -> Path:
    artifact = root / "downloaded-series-artifact"
    repeats_root = artifact / "pliego-hosted-performance-inputs"
    directories, _ = make_inputs(repeats_root)
    series_root = artifact / "pliego-hosted-performance-series"
    summarize_comparisons.write_hosted_series(directories, series_root)
    return artifact


def checksum_for(archive: Path) -> Path:
    checksum = archive.with_name(f"{archive.name}.sha256")
    checksum.write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n", encoding="ascii")
    return checksum


def canonical_specs(root: str, files: dict[str, bytes]) -> list[tuple[str, bytes | None, bytes | None, str]]:
    specs: list[tuple[str, bytes | None, bytes | None, str]] = [(root, tarfile.DIRTYPE, None, "")]
    for directory in package_hosted_evidence._archive_directories(list(files)):
        specs.append((f"{root}/{directory}", tarfile.DIRTYPE, None, ""))
    for relative, contents in files.items():
        specs.append((f"{root}/{relative}", tarfile.REGTYPE, contents, ""))
    return sorted(specs, key=lambda entry: entry[0].encode("utf-8"))


def write_specs(
    path: Path,
    specs: list[tuple[str, bytes | None, bytes | None, str]],
    *,
    compression_level: int = 9,
) -> None:
    with path.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", compresslevel=compression_level, fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT) as archive:
                for name, member_type, contents, linkname in specs:
                    info = tarfile.TarInfo(name)
                    info.type = member_type
                    info.mode = 0o755 if member_type == tarfile.DIRTYPE else 0o644
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mtime = 0
                    info.linkname = linkname
                    if contents is not None:
                        info.size = len(contents)
                        archive.addfile(info, io.BytesIO(contents))
                    else:
                        archive.addfile(info)


def malicious_archive(
    root: Path,
    files: dict[str, bytes],
    mutation: Callable[[list[tuple[str, bytes | None, bytes | None, str]]], None],
    *,
    compression_level: int = 9,
) -> tuple[Path, Path]:
    root.mkdir()
    archive = root / ARCHIVE_NAME
    specs = canonical_specs(ARCHIVE_ROOT, files)
    mutation(specs)
    write_specs(archive, specs, compression_level=compression_level)
    return archive, checksum_for(archive)


def semantic_tamper_archive(root: Path, files: dict[str, bytes]) -> tuple[Path, Path]:
    root.mkdir()
    archive = root / ARCHIVE_NAME
    package_hosted_evidence._write_canonical_archive(archive, ARCHIVE_ROOT, files)
    return archive, checksum_for(archive)


def main() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        source = make_final_artifact_tree(root)
        evidence = package_hosted_evidence.load_source_evidence(source)
        assert evidence.series["source"]["run_id"] == 123
        assert [run.comparison["source"]["repeat"] for run in evidence.runs] == [1, 2, 3]

        first = package_hosted_evidence.build_evidence_archive(source, root / "assets-1")
        second = package_hosted_evidence.build_evidence_archive(source, root / "assets-2")
        assert first.archive.name == ARCHIVE_NAME
        assert first.checksum.name == CHECKSUM_NAME
        assert first.archive.read_bytes() == second.archive.read_bytes()
        assert first.checksum.read_bytes() == second.checksum.read_bytes()
        assert package_hosted_evidence.validate_evidence_archive(first.archive, first.checksum) == first.manifest
        assert first.manifest["release"] == {
            "tag": "benchmark-v0.3.3-minimal-static-gh-123-a1",
            "archive_root": ARCHIVE_ROOT,
            "archive_filename": ARCHIVE_NAME,
            "checksum_filename": CHECKSUM_NAME,
        }
        assert first.manifest["source"]["revision"] == "2" * 40
        assert first.manifest["target"] == {
            "id": "pliego-0.3.3",
            "version": "0.3.3",
            "release_tag": "v0.3.3",
            "fixture": "minimal-static",
            "runner_environment": "github-hosted",
        }
        assert [entry["repeat"] for entry in first.manifest["repeats"]] == [1, 2, 3]
        assert len(first.manifest["files"]) == 18
        assert not package_hosted_evidence.validate_manifest(first.manifest)

        archive_root, files = package_hosted_evidence._read_archive_payload(first.archive)
        assert archive_root == ARCHIVE_ROOT
        assert set(files) == {
            package_hosted_evidence.MANIFEST_FILE,
            *(f"series/{name}" for name in summarize_comparisons.SERIES_BUNDLE_ENTRIES),
            *(
                f"repeats/repeat-{repeat}/{name}"
                for repeat in package_hosted_evidence.EXPECTED_REPEATS
                for name in package_hosted_evidence.run_comparison.REQUIRED_BUNDLE_FILES
            ),
        }

        cli = subprocess.run(
            [sys.executable, str(SCRIPT), "validate", str(first.archive), "--checksum", str(first.checksum)],
            capture_output=True,
            text=True,
        )
        assert cli.returncode == 0, cli.stderr
        assert "validated canonical hosted evidence" in cli.stdout
        cli_output = root / "cli-assets"
        cli_build = subprocess.run(
            [sys.executable, str(SCRIPT), "build", str(source), "--out", str(cli_output)],
            capture_output=True,
            text=True,
        )
        assert cli_build.returncode == 0, cli_build.stderr
        assert (cli_output / ARCHIVE_NAME).read_bytes() == first.archive.read_bytes()
        assert (cli_output / CHECKSUM_NAME).read_bytes() == first.checksum.read_bytes()

        expect_evidence_error(
            lambda: package_hosted_evidence.build_evidence_archive(source, root / "assets-1"),
            "must be empty",
        )

        extra = source / "unexpected.txt"
        extra.write_text("unexpected\n", encoding="utf-8")
        expect_evidence_error(lambda: package_hosted_evidence.load_source_evidence(source), "file set is not exact")
        extra.unlink()

        hardlink = source / "hardlink"
        linked_source = evidence.series_directory / summarize_comparisons.REPORT_FILE
        os.link(linked_source, hardlink)
        expect_evidence_error(lambda: package_hosted_evidence.load_source_evidence(source), "hard-linked file")
        hardlink.unlink()

        symlink = source / "symlink"
        try:
            symlink.symlink_to(linked_source)
        except OSError:
            pass
        else:
            expect_evidence_error(lambda: package_hosted_evidence.load_source_evidence(source), "link or reparse")
            symlink.unlink()

        fifo = source / "fifo"
        if hasattr(os, "mkfifo"):
            try:
                os.mkfifo(fifo)
            except OSError:
                pass
            else:
                expect_evidence_error(lambda: package_hosted_evidence.load_source_evidence(source), "non-regular")
                fifo.unlink()

        broken_checksum = root / "broken.sha256"
        broken_checksum.write_text(f"{'0' * 64}  {first.archive.name}\n", encoding="ascii")
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(first.archive, broken_checksum),
            "checksum filename",
        )
        expected_checksum_name = root / CHECKSUM_NAME
        expected_checksum_name.write_text(f"{'0' * 64}  {first.archive.name}\n", encoding="ascii")
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(first.archive, expected_checksum_name),
            "does not match",
        )

        duplicate_archive, duplicate_checksum = malicious_archive(
            root / "duplicate-member",
            files,
            lambda specs: specs.append(specs[-1]),
        )
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(duplicate_archive, duplicate_checksum),
            "duplicate member path",
        )

        traversal_archive, traversal_checksum = malicious_archive(
            root / "traversal-member",
            files,
            lambda specs: specs.append((f"{ARCHIVE_ROOT}/../escape", tarfile.REGTYPE, b"escape", "")),
        )
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(traversal_archive, traversal_checksum),
            "traversing member path",
        )

        collision_archive, collision_checksum = malicious_archive(
            root / "colliding-member",
            files,
            lambda specs: specs.append((f"{ARCHIVE_ROOT}/SERIES", tarfile.DIRTYPE, None, "")),
        )
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(collision_archive, collision_checksum),
            "colliding member paths",
        )

        for label, member_type in (
            ("symlink", tarfile.SYMTYPE),
            ("hardlink", tarfile.LNKTYPE),
            ("device", tarfile.CHRTYPE),
            ("fifo", tarfile.FIFOTYPE),
        ):
            unsafe_archive, unsafe_checksum = malicious_archive(
                root / f"unsafe-{label}",
                files,
                lambda specs, kind=member_type: specs.append(
                    (
                        f"{ARCHIVE_ROOT}/unsafe-{label}",
                        kind,
                        None,
                        f"{ARCHIVE_ROOT}/{package_hosted_evidence.MANIFEST_FILE}",
                    )
                ),
            )
            expect_evidence_error(
                lambda archive=unsafe_archive, checksum=unsafe_checksum: (
                    package_hosted_evidence.validate_evidence_archive(archive, checksum)
                ),
                "special type",
            )

        noncanonical_archive, noncanonical_checksum = malicious_archive(
            root / "noncanonical-gzip",
            files,
            lambda _specs: None,
            compression_level=1,
        )
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(noncanonical_archive, noncanonical_checksum),
            "not the canonical reproducible",
        )

        series_tamper = dict(files)
        series_tamper[f"series/{summarize_comparisons.REPORT_FILE}"] += b"tampered\n"
        series_archive, series_checksum = semantic_tamper_archive(root / "series-tamper", series_tamper)
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(series_archive, series_checksum),
            "hosted series bundle failed validation",
        )

        repeat_tamper = dict(files)
        repeat_tamper["repeats/repeat-2/all-metrics.md"] += b"tampered\n"
        repeat_archive, repeat_checksum = semantic_tamper_archive(root / "repeat-tamper", repeat_tamper)
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(repeat_archive, repeat_checksum),
            "hosted comparison bundle failed validation",
        )

        manifest_tamper = dict(files)
        changed_manifest = json.loads(manifest_tamper[package_hosted_evidence.MANIFEST_FILE])
        changed_manifest["source"]["revision"] = "f" * 40
        changed_manifest["manifest_sha256"] = package_hosted_evidence.manifest_sha256(changed_manifest)
        manifest_tamper[package_hosted_evidence.MANIFEST_FILE] = package_hosted_evidence._canonical_json_bytes(
            changed_manifest
        )
        manifest_archive, manifest_checksum = semantic_tamper_archive(root / "manifest-tamper", manifest_tamper)
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(manifest_archive, manifest_checksum),
            "not the exact derivation",
        )

        wrong_name_root = root / "wrong-name"
        wrong_name_root.mkdir()
        wrong_name = wrong_name_root / "renamed.tar.gz"
        shutil.copyfile(first.archive, wrong_name)
        wrong_name_checksum = checksum_for(wrong_name)
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(wrong_name, wrong_name_checksum),
            "filename is not bound",
        )

        hardlinked_archive_root = root / "hardlinked-archive"
        hardlinked_archive_root.mkdir()
        hardlinked_archive = hardlinked_archive_root / ARCHIVE_NAME
        os.link(first.archive, hardlinked_archive)
        hardlinked_checksum = checksum_for(hardlinked_archive)
        expect_evidence_error(
            lambda: package_hosted_evidence.validate_evidence_archive(hardlinked_archive, hardlinked_checksum),
            "non-linked regular file",
        )
        hardlinked_archive.unlink()

        broken_manifest = deepcopy(first.manifest)
        broken_manifest["files"][0]["sha256"] = "0" * 64
        errors = "\n".join(map(str, package_hosted_evidence.validate_manifest(broken_manifest)))
        assert "manifest_sha256" in errors


if __name__ == "__main__":
    main()
