#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Verify the files and source pointer in a packaged Pliego archive."""

from __future__ import annotations

import argparse
from collections.abc import Callable
import io
from pathlib import Path, PurePosixPath
import sys
import tarfile
import tempfile
import zipfile


KRILLA_FILES = (
    "licenses/krilla-0.8.2/ICC_CC0-1.0.txt",
    "licenses/krilla-0.8.2/LICENSE_APACHE",
    "licenses/krilla-0.8.2/LICENSE_MIT",
    "licenses/krilla-0.8.2/NOTICE.md",
)
MOZANGLE_FILES = (
    "licenses/mozangle-0.6.0/ANGLE_LICENSE",
    "licenses/mozangle-0.6.0/LICENSE",
    "licenses/mozangle-0.6.0/THIRD_PARTY_NOTICES.md",
)
SOURCE_ASSETS = (
    "LICENSE",
    "LICENSE_WHATWG_SPECS",
    "docs/pliego/third-party-notices.md",
    "resources/resource_protocol/license.html",
    *KRILLA_FILES,
    *MOZANGLE_FILES,
)
COMMON_FILES = (
    "INSTALL.txt",
    "LICENSE",
    "LICENSE_WHATWG_SPECS",
    "NATIVE-LIBRARIES.txt",
    "SOURCE.txt",
    "THIRD_PARTY_LICENSES.html",
    "THIRD_PARTY_NOTICES.md",
    "VERSION.txt",
    *KRILLA_FILES,
)
REPORT_COMPONENTS = (b"krilla 0.8.2", b"mozangle 0.6.0")
SOURCE_IDENTIFIERS = {
    "docs/pliego/third-party-notices.md": (
        b"`krilla` 0.8.2",
        b"3ffdf0588cf98050aad6edba51ca70162e1fb5b5",
    ),
    "resources/resource_protocol/license.html": REPORT_COMPONENTS,
    "licenses/krilla-0.8.2/LICENSE_MIT": (b"Copyright (c) 2024 Laurenz Stampfl",),
    "licenses/krilla-0.8.2/LICENSE_APACHE": (b"Apache License", b"Version 2.0"),
    "licenses/krilla-0.8.2/NOTICE.md": (b"`resvg`", b"`typst`", b"`svg2pdf`", b"`vello`"),
    "licenses/krilla-0.8.2/ICC_CC0-1.0.txt": (b"CC0 1.0 Universal",),
    "licenses/mozangle-0.6.0/LICENSE": (b"2002-2013 The ANGLE Project Authors",),
    "licenses/mozangle-0.6.0/ANGLE_LICENSE": (b"2018 The ANGLE Project Authors",),
    "licenses/mozangle-0.6.0/THIRD_PARTY_NOTICES.md": (
        b"mozangle 0.6.0",
        b"60b428c032f0af701a3ca440c92e7b552c25b2a5af2b08a85077ee8e9d2ae699",
        b"7be30d4be68583169ced927e7b5dab7cca6f185f",
        b"libEGL.dll",
        b"libGLESv2.dll",
    ),
}
MAX_NOTICE_SIZE = 16 * 1024 * 1024


def _source_text(repository_url: str, version: str) -> str:
    repository_url = repository_url.rstrip("/")
    return f"Repository: {repository_url}\nSource: {repository_url}/tree/v{version}\n"


def _native_libraries_text(bundle: str) -> str:
    if bundle.startswith("windows-"):
        notice = "ANGLE via mozangle 0.6.0; notices: licenses/mozangle-0.6.0/"
        libraries = f"- libEGL.dll ({notice})\n- libGLESv2.dll ({notice})\n"
    else:
        libraries = "none\n"
    return f"Artifact: {bundle}\nBundled native libraries: {libraries}"


def _member_parts(name: str) -> tuple[str, ...]:
    path = PurePosixPath(name.replace("\\", "/"))
    if path.is_absolute() or ".." in path.parts:
        raise ValueError(f"unsafe archive member: {name}")
    return tuple(part for part in path.parts if part not in ("", "."))


def _read_archive(archive: Path) -> tuple[str, dict[str, int], dict[str, bytes]]:
    entries: list[tuple[str, int, object]] = []
    if archive.name.endswith(".tar.gz"):
        with tarfile.open(archive, "r:gz") as source:
            for member in source.getmembers():
                if member.isfile():
                    entries.append((member.name, member.size, member))
            return _collect_entries(
                entries,
                lambda member: source.extractfile(member).read(),  # type: ignore[union-attr]
            )
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as source:
            for member in source.infolist():
                if not member.is_dir():
                    entries.append((member.filename, member.file_size, member))
            return _collect_entries(entries, source.read)
    raise ValueError("archive must end in .tar.gz or .zip")


def _collect_entries(
    entries: list[tuple[str, int, object]], read: Callable[[object], bytes]
) -> tuple[str, dict[str, int], dict[str, bytes]]:
    roots: set[str] = set()
    sizes: dict[str, int] = {}
    payloads: dict[str, bytes] = {}
    prepared: list[tuple[str, int, object]] = []

    for name, size, member in entries:
        parts = _member_parts(name)
        if len(parts) < 2:
            raise ValueError(f"archive member must be inside one bundle directory: {name}")
        roots.add(parts[0])
        relative = "/".join(parts[1:])
        if relative in sizes:
            raise ValueError(f"duplicate archive member: {relative}")
        sizes[relative] = size
        prepared.append((relative, size, member))

    if len(roots) != 1:
        raise ValueError("archive must contain exactly one top-level bundle directory")

    wanted_payloads = set(COMMON_FILES) | set(MOZANGLE_FILES)
    for relative, size, member in prepared:
        if relative not in wanted_payloads:
            continue
        if size > MAX_NOTICE_SIZE:
            raise ValueError(f"notice file is unexpectedly large: {relative}")
        payloads[relative] = read(member)

    return roots.pop(), sizes, payloads


def check_archive(archive: Path, *, version: str, bundle: str, repository_url: str) -> list[str]:
    errors: list[str] = []
    expected_root = f"pliego-{version}-{bundle}"
    extension = ".zip" if bundle.startswith("windows-") else ".tar.gz"
    if archive.name != f"{expected_root}{extension}":
        errors.append(f"archive name must be {expected_root}{extension}")

    try:
        root, sizes, payloads = _read_archive(archive)
    except (OSError, tarfile.TarError, zipfile.BadZipFile, ValueError) as error:
        return [str(error)]

    if root != expected_root:
        errors.append(f"archive root must be {expected_root}, got {root}")

    required = set(COMMON_FILES)
    required.add("pliego.exe" if bundle.startswith("windows-") else "pliego")
    if bundle.startswith("windows-"):
        required.update(("libEGL.dll", "libGLESv2.dll", *MOZANGLE_FILES))

    for relative in sorted(required - sizes.keys()):
        errors.append(f"missing required file: {relative}")
    for relative in sorted(required & sizes.keys()):
        if sizes[relative] == 0:
            errors.append(f"required file is empty: {relative}")

    expected_text = {
        "SOURCE.txt": _source_text(repository_url, version),
        "NATIVE-LIBRARIES.txt": _native_libraries_text(bundle),
    }
    for relative, expected in expected_text.items():
        payload = payloads.get(relative)
        if payload is None:
            continue
        try:
            actual = payload.decode("utf-8").replace("\r\n", "\n")
        except UnicodeDecodeError:
            errors.append(f"{relative} must be UTF-8")
            continue
        if actual != expected:
            errors.append(f"{relative} does not match the release contract")

    report = payloads.get("THIRD_PARTY_LICENSES.html")
    if report is not None:
        for component in REPORT_COMPONENTS:
            if component not in report:
                errors.append(f"THIRD_PARTY_LICENSES.html does not identify {component.decode()}")

    return errors


def check_source_assets(source_root: Path) -> list[str]:
    errors = []
    for relative in SOURCE_ASSETS:
        path = source_root / relative
        if not path.is_file():
            errors.append(f"missing source asset: {relative}")
        elif path.stat().st_size == 0:
            errors.append(f"source asset is empty: {relative}")
    for relative, identifiers in SOURCE_IDENTIFIERS.items():
        path = source_root / relative
        if not path.is_file():
            continue
        payload = path.read_bytes()
        for identifier in identifiers:
            if identifier not in payload:
                errors.append(f"{relative} does not identify {identifier.decode()}")
    return errors


def _fixture_files(version: str, bundle: str, repository_url: str) -> dict[str, bytes]:
    files = {relative: b"fixture\n" for relative in COMMON_FILES}
    files["SOURCE.txt"] = _source_text(repository_url, version).encode()
    files["NATIVE-LIBRARIES.txt"] = _native_libraries_text(bundle).encode()
    files["THIRD_PARTY_LICENSES.html"] = b"\n".join(REPORT_COMPONENTS)
    files["pliego.exe" if bundle.startswith("windows-") else "pliego"] = b"binary"
    if bundle.startswith("windows-"):
        files.update({relative: b"fixture\n" for relative in MOZANGLE_FILES})
        files["libEGL.dll"] = b"binary"
        files["libGLESv2.dll"] = b"binary"
    return files


def _write_fixture(archive: Path, root: str, files: dict[str, bytes]) -> None:
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive, "w") as target:
            for relative, payload in files.items():
                target.writestr(f"{root}/{relative}", payload)
        return
    with tarfile.open(archive, "w:gz") as target:
        for relative, payload in files.items():
            member = tarfile.TarInfo(f"{root}/{relative}")
            member.size = len(payload)
            target.addfile(member, io.BytesIO(payload))


def self_test() -> None:
    version = "0.0.0-test"
    repository_url = "https://github.com/oxhq/pliego"
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        for bundle, extension in (("linux-x86_64", ".tar.gz"), ("windows-x86_64", ".zip")):
            root = f"pliego-{version}-{bundle}"
            archive = directory / f"{root}{extension}"
            files = _fixture_files(version, bundle, repository_url)
            _write_fixture(archive, root, files)
            assert not check_archive(
                archive,
                version=version,
                bundle=bundle,
                repository_url=repository_url,
            )

        root = f"pliego-{version}-windows-x86_64"
        archive = directory / f"{root}.zip"
        cases = (
            (
                "licenses/mozangle-0.6.0/ANGLE_LICENSE",
                None,
                "missing required file: licenses/mozangle-0.6.0/ANGLE_LICENSE",
            ),
            ("INSTALL.txt", None, "missing required file: INSTALL.txt"),
            (
                "SOURCE.txt",
                b"https://example.invalid/source\n",
                "SOURCE.txt does not match the release contract",
            ),
            (
                "THIRD_PARTY_LICENSES.html",
                b"krilla 0.8.2\n",
                "THIRD_PARTY_LICENSES.html does not identify mozangle 0.6.0",
            ),
        )
        for relative, replacement, expected in cases:
            files = _fixture_files(version, "windows-x86_64", repository_url)
            if replacement is None:
                del files[relative]
            else:
                files[relative] = replacement
            _write_fixture(archive, root, files)
            errors = check_archive(
                archive,
                version=version,
                bundle="windows-x86_64",
                repository_url=repository_url,
            )
            assert errors == [expected]

        files = _fixture_files(version, "windows-x86_64", repository_url)
        _write_fixture(archive, root, files)
        with zipfile.ZipFile(archive, "a") as target:
            target.writestr("another-root/file.txt", b"fixture")
        assert check_archive(
            archive,
            version=version,
            bundle="windows-x86_64",
            repository_url=repository_url,
        ) == ["archive must contain exactly one top-level bundle directory"]

        _write_fixture(archive, root, files)
        with zipfile.ZipFile(archive, "a") as target:
            target.writestr("../escape.txt", b"fixture")
        assert check_archive(
            archive,
            version=version,
            bundle="windows-x86_64",
            repository_url=repository_url,
        ) == ["unsafe archive member: ../escape.txt"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", nargs="?", type=Path)
    parser.add_argument("--bundle")
    parser.add_argument("--version")
    parser.add_argument("--repository-url", default="https://github.com/oxhq/pliego")
    parser.add_argument("--check-source-assets", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        print("release archive checker self-test: ok")
    if args.check_source_assets is not None:
        errors = check_source_assets(args.check_source_assets)
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        if errors:
            return 1
        print("release archive source assets: ok")
    if args.self_test or args.check_source_assets is not None:
        return 0
    if args.archive is None or args.bundle is None or args.version is None:
        parser.error("archive, --bundle, and --version are required")

    errors = check_archive(
        args.archive,
        version=args.version,
        bundle=args.bundle,
        repository_url=args.repository_url,
    )
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"release archive verified: {args.archive}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
