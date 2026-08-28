#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import hashlib
import json
import tempfile
from copy import deepcopy
from pathlib import Path
from typing import Any, Callable

import package_hosted_evidence
import public_hosted_benchmark
import summarize_comparisons
from test_package_hosted_evidence import make_final_artifact_tree
from test_summarize_comparisons import make_inputs


UNPUBLISHED_BODY = """The comparator is correctness-gated.

There is no committed performance snapshot yet. A manual lane can now produce
directional `github-hosted-exploratory` timing/resource evidence. Authoritative
tables and production rankings remain N/A."""


def readme(body: str = UNPUBLISHED_BODY) -> str:
    return "\n".join(
        (
            "# Fixture",
            "",
            "## Benchmark evidence",
            "",
            public_hosted_benchmark.README_START,
            body,
            public_hosted_benchmark.README_END,
            "",
        )
    )


def descriptor(path: str, payload: bytes) -> dict[str, Any]:
    return {"path": path, "bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}


def evidence_manifest(series: dict[str, Any], series_payloads: dict[str, bytes]) -> dict[str, Any]:
    run_id = series["source"]["run_id"]
    attempt = series["source"]["run_attempt"]
    archive_root = f"pliego-benchmark-v0.3.3-minimal-static-gh-run-{run_id}-attempt-{attempt}"
    files = [descriptor(path, series_payloads[path]) for path in sorted(series_payloads)]
    for path in sorted(public_hosted_benchmark.EXPECTED_ARCHIVE_FILES - set(series_payloads)):
        files.append(descriptor(path, f"fixture:{path}\n".encode()))
    files.sort(key=lambda item: item["path"])
    manifest: dict[str, Any] = {
        "schema": "pliego.benchmark-evidence-archive",
        "version": 1,
        "evidence_class": "github-hosted-exploratory",
        "publication_status": "packaged-hosted-series",
        "claim_boundary": public_hosted_benchmark.CLAIM_BOUNDARY,
        "manifest_sha256": "",
        "generated_at": "2026-08-28T00:02:00+00:00",
        "archive_format": {
            "contract": "pliego.canonical-tar-gzip.v1",
            "container": "tar.gz",
            "tar_format": "ustar",
            "gzip_compression_level": 9,
            "mtime": 0,
            "uid": 0,
            "gid": 0,
            "file_mode": "0644",
            "directory_mode": "0755",
        },
        "release": {
            "tag": f"benchmark-v0.3.3-minimal-static-gh-{run_id}-a{attempt}",
            "archive_root": archive_root,
            "archive_filename": f"{archive_root}.tar.gz",
            "checksum_filename": f"{archive_root}.tar.gz.sha256",
        },
        "source": deepcopy(series["source"]),
        "target": {
            "id": "pliego-0.3.3",
            "version": "0.3.3",
            "release_tag": "v0.3.3",
            "fixture": "minimal-static",
            "runner_environment": "github-hosted",
        },
        "series": {"path": "series", "series_sha256": series["series_sha256"]},
        "repeats": [
            {
                "repeat": run["repeat"],
                "path": f"repeats/repeat-{run['repeat']}",
                "comparison_sha256": run["comparison_sha256"],
                "interleaved_artifact_sha256": run["interleaved_artifact_sha256"],
                "schedule_sha256": run["schedule_sha256"],
            }
            for run in series["runs"]
        ],
        "files": files,
    }
    manifest["manifest_sha256"] = public_hosted_benchmark.manifest_sha256(manifest)
    return manifest


def write_publication(root: Path) -> tuple[dict[str, Any], dict[str, bytes]]:
    inputs, runs = make_inputs(root / "inputs")
    del inputs
    series = summarize_comparisons.build_series(runs, generated_at="2026-08-28T00:01:00+00:00")
    series_payload = (json.dumps(series, indent=2, allow_nan=False) + "\n").encode()
    report_payload = summarize_comparisons.render_markdown(series).encode()
    series_payloads = {
        "series/SHA256SUMS": b"fixture checksums\n",
        "series/all-repeats.md": report_payload,
        "series/hosted-series.v1.json": series_payload,
    }
    manifest = evidence_manifest(series, series_payloads)
    destination = public_hosted_benchmark.publication_directory(root)
    destination.mkdir(parents=True)
    (destination / public_hosted_benchmark.EVIDENCE_MANIFEST).write_text(
        json.dumps(manifest, indent=2, allow_nan=False) + "\n", encoding="utf-8"
    )
    (destination / public_hosted_benchmark.SERIES_FILE).write_bytes(series_payload)
    (destination / public_hosted_benchmark.REPORT_FILE).write_bytes(report_payload)
    archive = manifest["release"]["archive_filename"]
    (destination / public_hosted_benchmark.ARCHIVE_CHECKSUM_FILE).write_bytes(
        f"{'a' * 64}  {archive}\n".encode("ascii")
    )
    body = public_hosted_benchmark.render_readme_body(series, manifest)
    (root / "README.md").write_text(readme(body), encoding="utf-8")
    return manifest, series_payloads


def expect_error(operation: Callable[[], object], expected: str) -> None:
    try:
        operation()
    except public_hosted_benchmark.PublicationError as error:
        assert expected in str(error), str(error)
    else:
        raise AssertionError(f"expected PublicationError containing {expected!r}")


def test_packaged_archive_integration(root: Path) -> None:
    root.mkdir()
    source = make_final_artifact_tree(root)
    assets = package_hosted_evidence.build_evidence_archive(source, root / "assets")
    publication = root / "publication"
    publication.mkdir()
    (publication / "README.md").write_text(readme(), encoding="utf-8")

    public_hosted_benchmark.stage_publication(assets.archive, assets.checksum, publication)
    public_hosted_benchmark.verify_against_archive(assets.archive, assets.checksum, publication)
    assert public_hosted_benchmark.verify_publication(publication)

    manifest, _, _, committed_checksum = public_hosted_benchmark.load_publication(publication)
    assert manifest == assets.manifest
    assert committed_checksum == assets.checksum.read_bytes()


def main() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        (root / "README.md").write_text(readme(), encoding="utf-8")
        assert not public_hosted_benchmark.verify_publication(root)
        expect_error(
            lambda: public_hosted_benchmark.replace_readme_body(readme() + public_hosted_benchmark.README_START, "x"),
            "exactly one hosted benchmark start marker",
        )

        manifest, _ = write_publication(root)
        assert public_hosted_benchmark.verify_publication(root)
        assets = public_hosted_benchmark.release_assets(root)
        assert assets is not None
        assert assets["release_tag"] == manifest["release"]["tag"]
        assert assets["archive_name"] == manifest["release"]["archive_filename"]
        assert assets["checksum_name"] == manifest["release"]["checksum_filename"]
        urls = public_hosted_benchmark.release_urls(root)
        assert urls is not None and manifest["release"]["tag"] in urls[0] and urls[1].endswith(".sha256")
        body = public_hosted_benchmark.readme_body((root / "README.md").read_text(encoding="utf-8"))
        assert "Repeat 1 p50 (ms)" in body
        assert "No best or canonical repeat is selected" not in body
        assert public_hosted_benchmark.CLAIM_BOUNDARY in body
        assert "to" in body and "Full report with all retained metrics and spread" in body
        assert "Immutable evidence release" in body

        release_metadata = root / "github-release.json"
        canonical_release = {
            "tag_name": assets["release_tag"],
            "immutable": True,
            "draft": False,
            "prerelease": False,
            "assets": [
                {
                    "name": assets["archive_name"],
                    "browser_download_url": assets["archive_url"],
                    "state": "uploaded",
                },
                {
                    "name": assets["checksum_name"],
                    "browser_download_url": assets["checksum_url"],
                    "state": "uploaded",
                },
            ],
        }
        release_metadata.write_text(json.dumps(canonical_release), encoding="utf-8")
        public_hosted_benchmark.verify_github_release(release_metadata, root)
        mutable_release = deepcopy(canonical_release)
        mutable_release["immutable"] = False
        release_metadata.write_text(json.dumps(mutable_release), encoding="utf-8")
        expect_error(
            lambda: public_hosted_benchmark.verify_github_release(release_metadata, root),
            "is not immutable",
        )
        extra_asset_release = deepcopy(canonical_release)
        extra_asset_release["assets"].append(
            {"name": "extra.txt", "browser_download_url": "https://example.invalid/extra.txt", "state": "uploaded"}
        )
        release_metadata.write_text(json.dumps(extra_asset_release), encoding="utf-8")
        expect_error(
            lambda: public_hosted_benchmark.verify_github_release(release_metadata, root),
            "exactly the two committed evidence assets",
        )

        report = public_hosted_benchmark.publication_directory(root) / public_hosted_benchmark.REPORT_FILE
        original_report = report.read_bytes()
        report.write_text("tampered\n", encoding="utf-8")
        expect_error(lambda: public_hosted_benchmark.verify_publication(root), "differs from the evidence archive")
        report.write_bytes(original_report)

        readme_path = root / "README.md"
        original_readme = readme_path.read_text(encoding="utf-8")
        readme_path.write_text(original_readme.replace("Repeat 1 p50 (ms)", "Chosen p50"), encoding="utf-8")
        expect_error(lambda: public_hosted_benchmark.verify_publication(root), "not the exact rendering")
        readme_path.write_text(original_readme, encoding="utf-8")

        checksum = public_hosted_benchmark.publication_directory(root) / public_hosted_benchmark.ARCHIVE_CHECKSUM_FILE
        checksum.write_bytes(f"{'a' * 64}  wrong.tar.gz\n".encode("ascii"))
        expect_error(lambda: public_hosted_benchmark.verify_publication(root), "names a different asset")

        test_packaged_archive_integration(root / "packaged-archive-integration")


if __name__ == "__main__":
    main()
