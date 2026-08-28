#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Stage and verify the public view of one packaged hosted benchmark series.

No value in the buyer-facing README is accepted by hand.  Publication copies
the exact series and report from a checksum-verified evidence archive, then
derives the compact README table from that sealed series.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path
from typing import Any, NoReturn, Sequence

import run_benchmark
import summarize_comparisons


ROOT = Path(__file__).resolve().parents[2]
RESULT_RELATIVE = Path("docs/benchmarks/results/v0.3.3-minimal-static-github-hosted")
EVIDENCE_MANIFEST = "evidence-manifest.v1.json"
SERIES_FILE = summarize_comparisons.SERIES_FILE
REPORT_FILE = summarize_comparisons.REPORT_FILE
ARCHIVE_CHECKSUM_FILE = "archive.sha256"
PUBLICATION_ENTRIES = frozenset({EVIDENCE_MANIFEST, SERIES_FILE, REPORT_FILE, ARCHIVE_CHECKSUM_FILE})
README_START = "<!-- pliego-hosted-benchmark:start -->"
README_END = "<!-- pliego-hosted-benchmark:end -->"
CLAIM_BOUNDARY = summarize_comparisons.CLAIM_BOUNDARY
RELEASE_TAG = re.compile(r"^benchmark-v0\.3\.3-minimal-static-gh-([1-9][0-9]*)-a([1-9][0-9]*)$")
ARCHIVE_ROOT = re.compile(r"^pliego-benchmark-v0\.3\.3-minimal-static-gh-run-([1-9][0-9]*)-attempt-([1-9][0-9]*)$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
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
    try:
        return load_json_bytes(path.read_bytes(), str(path))
    except OSError as error:
        fail(f"cannot read {path}: {error}")


def sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


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
            f"All {total_samples} timed samples passed the shared PDF oracle. " + CLAIM_BOUNDARY,
            "",
            f"[Full report with all retained metrics and spread]({report}) · "
            f"[Checksum-bound evidence release]({release_url})",
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
    require(path.exists(), f"required public benchmark file is absent: {path.relative_to(root)}")
    require(
        not path.is_symlink() and path.is_file(),
        f"public benchmark path is not a regular file: {path.relative_to(root)}",
    )
    resolved = path.resolve(strict=True)
    canonical_root = root.resolve(strict=True)
    require(
        resolved == canonical_root or canonical_root in resolved.parents,
        f"public benchmark path escapes the repository: {path}",
    )
    return path.read_bytes()


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
    require(not result.exists(), f"unpublished state contains a benchmark result directory: {result.relative_to(root)}")
    body = readme_body((root / "README.md").read_text(encoding="utf-8"))
    for phrase in (
        "There is no committed performance snapshot yet.",
        "directional `github-hosted-exploratory` timing/resource evidence",
        "Authoritative\ntables and production rankings remain N/A",
    ):
        require(phrase in body, f"unpublished README omitted its benchmark boundary: {phrase!r}")
    require("docs/benchmarks/results/" not in body, "unpublished README links to a result that does not exist")


def load_publication(root: Path = ROOT) -> tuple[dict[str, Any], dict[str, Any], bytes, bytes]:
    directory = publication_directory(root)
    require(directory.exists(), f"public benchmark result directory is absent: {directory.relative_to(root)}")
    require(
        not directory.is_symlink() and directory.is_dir(), "public benchmark result path is not a regular directory"
    )
    actual = {path.name for path in directory.iterdir()}
    require(actual == PUBLICATION_ENTRIES, f"public benchmark result file set changed: {sorted(actual)!r}")
    manifest_payload = _regular_public_file(directory / EVIDENCE_MANIFEST, root)
    series_payload = _regular_public_file(directory / SERIES_FILE, root)
    report_payload = _regular_public_file(directory / REPORT_FILE, root)
    checksum_payload = _regular_public_file(directory / ARCHIVE_CHECKSUM_FILE, root)
    manifest = load_json_bytes(manifest_payload, EVIDENCE_MANIFEST)
    series = load_json_bytes(series_payload, SERIES_FILE)
    validate_evidence_manifest(manifest)
    descriptors = _descriptor_index(manifest)
    _require_bound_bytes(series_payload, descriptors[f"series/{SERIES_FILE}"], SERIES_FILE)
    _require_bound_bytes(report_payload, descriptors[f"series/{REPORT_FILE}"], REPORT_FILE)
    _, _, archive_filename, _ = _release_identity(manifest)
    parse_archive_checksum(checksum_payload, archive_filename)
    return manifest, series, report_payload, checksum_payload


def verify_publication(root: Path = ROOT) -> bool:
    directory = publication_directory(root)
    if not directory.exists():
        verify_unpublished(root)
        return False
    manifest, series, report_payload, _ = load_publication(root)
    violations = summarize_comparisons.validate_series(series)
    require(not violations, f"committed hosted series failed validation: {violations[0] if violations else ''}")
    expected_report = summarize_comparisons.render_markdown(series)
    require(
        normalized_utf8_text(report_payload, REPORT_FILE) == expected_report,
        "committed hosted report is not the deterministic rendering of its series",
    )
    expected_body = render_readme_body(series, manifest)
    actual_body = readme_body((root / "README.md").read_text(encoding="utf-8"))
    require(
        actual_body == expected_body, "README benchmark table is not the exact rendering of the committed hosted series"
    )
    return True


def release_assets(root: Path = ROOT) -> dict[str, str] | None:
    if not verify_publication(root):
        return None
    manifest, _, _, _ = load_publication(root)
    tag, _, archive, checksum = _release_identity(manifest)
    base = f"https://github.com/oxhq/pliego/releases/download/{tag}"
    return {
        "archive_name": archive,
        "archive_url": f"{base}/{archive}",
        "checksum_name": checksum,
        "checksum_url": f"{base}/{checksum}",
    }


def release_urls(root: Path = ROOT) -> tuple[str, str] | None:
    assets = release_assets(root)
    if assets is None:
        return None
    return assets["archive_url"], assets["checksum_url"]


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
    try:
        with tarfile.open(archive, mode="r:gz") as bundle:
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
    committed_manifest, _, _, committed_checksum = load_publication(root)
    require(manifest == committed_manifest, "committed evidence manifest differs from the released archive")
    directory = publication_directory(root)
    for filename, payload in payloads.items():
        require(
            (directory / filename).read_bytes() == payload, f"committed {filename} differs from the released archive"
        )
    require(committed_checksum == checksum.read_bytes(), "committed archive checksum differs from the released asset")
    _, _, archive_filename, _ = _release_identity(manifest)
    expected_archive_sha256 = parse_archive_checksum(committed_checksum, archive_filename)
    require(
        sha256_bytes(archive.read_bytes()) == expected_archive_sha256,
        "released archive bytes differ from the committed checksum",
    )
    verify_publication(root)


def stage_publication(archive: Path, checksum: Path, root: Path = ROOT) -> None:
    _validate_packaged_archive(archive, checksum)
    manifest, payloads = _archive_payloads(archive)
    _, _, archive_filename, _ = _release_identity(manifest)
    checksum_payload = checksum.read_bytes()
    expected_archive_sha256 = parse_archive_checksum(checksum_payload, archive_filename)
    require(
        sha256_bytes(archive.read_bytes()) == expected_archive_sha256, "archive bytes differ from the supplied checksum"
    )
    series = load_json_bytes(payloads[SERIES_FILE], SERIES_FILE)
    expected_report = summarize_comparisons.render_markdown(series)
    require(
        normalized_utf8_text(payloads[REPORT_FILE], REPORT_FILE) == expected_report,
        "archive report is not the deterministic rendering of its series",
    )
    body = render_readme_body(series, manifest)

    destination = publication_directory(root)
    require(not destination.exists(), f"public benchmark result directory already exists: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f".{destination.name}-", dir=destination.parent) as temporary:
        staged = Path(temporary)
        for filename in (EVIDENCE_MANIFEST, SERIES_FILE, REPORT_FILE):
            (staged / filename).write_bytes(payloads[filename])
        (staged / ARCHIVE_CHECKSUM_FILE).write_bytes(checksum_payload)
        os.replace(staged, destination)
    readme_path = root / "README.md"
    readme_path.write_text(
        replace_readme_body(readme_path.read_text(encoding="utf-8"), body),
        encoding="utf-8",
        newline="\n",
    )
    verify_against_archive(archive, checksum, root)


def _write_github_outputs(path: Path, assets: dict[str, str] | None) -> None:
    published = assets is not None
    lines = [f"published={'true' if published else 'false'}"]
    if assets is not None:
        lines.extend(f"{key}={value}" for key, value in assets.items())
    with path.open("a", encoding="utf-8", newline="\n") as output:
        output.write("\n".join(lines) + "\n")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("verify", help="verify the committed public benchmark state")
    stage = commands.add_parser("stage", help="stage public files from one validated evidence archive")
    stage.add_argument("archive", type=Path)
    stage.add_argument("--checksum", type=Path, required=True)
    archive_verify = commands.add_parser("verify-archive", help="compare committed files with exact release assets")
    archive_verify.add_argument("archive", type=Path)
    archive_verify.add_argument("--checksum", type=Path, required=True)
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
            stage_publication(args.archive, args.checksum)
            print(f"staged public hosted benchmark under {RESULT_RELATIVE}")
        elif args.command == "verify-archive":
            verify_against_archive(args.archive, args.checksum)
            print("committed public benchmark matches the exact released evidence archive")
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
