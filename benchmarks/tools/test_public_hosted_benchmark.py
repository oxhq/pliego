#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import hashlib
import json
import os
import shutil
import stat
import tempfile
import zipfile
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
    checksum_payload = (destination / public_hosted_benchmark.ARCHIVE_CHECKSUM_FILE).read_bytes()
    assets = public_hosted_benchmark._expected_release_assets(manifest)
    source = manifest["source"]
    provenance: dict[str, Any] = {
        "schema": "pliego.benchmark-source-provenance",
        "version": 1,
        "contract": public_hosted_benchmark.SOURCE_PROVENANCE_CONTRACT,
        "provenance_sha256": "",
        "source": deepcopy(source),
        "actions_run": {
            "id": source["run_id"],
            "attempt": source["run_attempt"],
            "head_sha": source["revision"],
            "head_branch": "main",
            "event": "workflow_dispatch",
            "status": "completed",
            "conclusion": "success",
            "workflow_name": source["workflow"],
            "workflow_path": public_hosted_benchmark.PERFORMANCE_WORKFLOW_PATH,
            "workflow_id": 8001,
            "repository": "oxhq/pliego",
            "repository_id": 7001,
        },
        "actions_artifact": {
            "id": 6001,
            "name": (
                f"pliego-hosted-performance-series-{source['revision']}-{source['run_id']}-{source['run_attempt']}"
            ),
            "server_digest": f"sha256:{'b' * 64}",
            "zip_sha256": "b" * 64,
            "size_in_bytes": 1,
            "expired_at_publication": False,
            "created_at": "2026-08-28T00:03:00Z",
            "expires_at": "2099-08-28T00:03:00Z",
            "workflow_run_id": source["run_id"],
            "head_sha": source["revision"],
        },
        "benchmark_tag": {
            "ref": f"refs/tags/{manifest['release']['tag']}",
            "object_type": "commit",
            "target_sha": source["revision"],
        },
        "release": {
            "id": 5001,
            "tag_name": manifest["release"]["tag"],
            "immutable": True,
            "draft": False,
            "prerelease": False,
            "assets": [
                {
                    "id": 5002,
                    "name": assets["archive_name"],
                    "url": assets["archive_url"],
                    "state": "uploaded",
                    "size": 1,
                    "digest": f"sha256:{'a' * 64}",
                },
                {
                    "id": 5003,
                    "name": assets["checksum_name"],
                    "url": assets["checksum_url"],
                    "state": "uploaded",
                    "size": len(checksum_payload),
                    "digest": f"sha256:{hashlib.sha256(checksum_payload).hexdigest()}",
                },
            ],
        },
        "evidence": {
            "manifest_sha256": manifest["manifest_sha256"],
            "archive_name": assets["archive_name"],
            "archive_sha256": "a" * 64,
            "archive_bytes": 1,
            "checksum_name": assets["checksum_name"],
            "checksum_sha256": hashlib.sha256(checksum_payload).hexdigest(),
            "checksum_bytes": len(checksum_payload),
        },
    }
    provenance["provenance_sha256"] = public_hosted_benchmark.source_provenance_sha256(provenance)
    (destination / public_hosted_benchmark.SOURCE_PROVENANCE_FILE).write_text(
        json.dumps(provenance, indent=2, allow_nan=False) + "\n", encoding="utf-8"
    )
    body = public_hosted_benchmark.render_readme_body(series, manifest)
    (root / "README.md").write_text(readme(body), encoding="utf-8")
    return manifest, series_payloads


def write_actions_zip(source: Path, destination: Path) -> bytes:
    with zipfile.ZipFile(destination, mode="x", compression=zipfile.ZIP_DEFLATED) as archive:
        for path in sorted((item for item in source.rglob("*") if item.is_file()), key=lambda item: item.as_posix()):
            relative = path.relative_to(source).as_posix()
            info = zipfile.ZipInfo(relative, date_time=(2026, 8, 28, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            archive.writestr(info, path.read_bytes())
    return destination.read_bytes()


def write_live_metadata(
    root: Path, assets: package_hosted_evidence.ArchiveAssets, artifact_zip: Path
) -> dict[str, Path]:
    source = assets.manifest["source"]
    release = assets.manifest["release"]
    repository_id = 7001
    workflow_id = 8001
    artifact_id = 6001
    artifact_payload = artifact_zip.read_bytes()
    expected = public_hosted_benchmark._expected_release_assets(assets.manifest)
    payloads: dict[str, dict[str, Any]] = {
        "run": {
            "id": source["run_id"],
            "run_attempt": source["run_attempt"],
            "head_sha": source["revision"],
            "head_branch": "main",
            "event": "workflow_dispatch",
            "status": "completed",
            "conclusion": "success",
            "path": f"{public_hosted_benchmark.PERFORMANCE_WORKFLOW_PATH}@main",
            "name": source["workflow"],
            "workflow_id": workflow_id,
            "repository": {"id": repository_id, "full_name": "oxhq/pliego"},
            "head_repository": {"id": repository_id, "full_name": "oxhq/pliego"},
        },
        "artifact": {
            "total_count": 1,
            "artifacts": [
                {
                    "id": artifact_id,
                    "name": (
                        f"pliego-hosted-performance-series-{source['revision']}-{source['run_id']}-{source['run_attempt']}"
                    ),
                    "expired": False,
                    "digest": f"sha256:{hashlib.sha256(artifact_payload).hexdigest()}",
                    "size_in_bytes": len(artifact_payload),
                    "archive_download_url": (
                        f"https://api.github.com/repos/oxhq/pliego/actions/artifacts/{artifact_id}/zip"
                    ),
                    "created_at": "2026-08-28T00:03:00Z",
                    "expires_at": "2099-08-28T00:03:00Z",
                    "workflow_run": {
                        "id": source["run_id"],
                        "repository_id": repository_id,
                        "head_repository_id": repository_id,
                        "head_branch": "main",
                        "head_sha": source["revision"],
                    },
                }
            ],
        },
        "tag": {
            "ref": f"refs/tags/{release['tag']}",
            "object": {
                "type": "commit",
                "sha": source["revision"],
                "url": f"https://api.github.com/repos/oxhq/pliego/git/commits/{source['revision']}",
            },
        },
        "release": {
            "id": 5001,
            "tag_name": release["tag"],
            "immutable": True,
            "draft": False,
            "prerelease": False,
            "html_url": f"https://github.com/oxhq/pliego/releases/tag/{release['tag']}",
            "assets": [
                {
                    "id": 5002,
                    "name": expected["archive_name"],
                    "browser_download_url": expected["archive_url"],
                    "state": "uploaded",
                    "size": assets.archive.stat().st_size,
                    "digest": f"sha256:{hashlib.sha256(assets.archive.read_bytes()).hexdigest()}",
                },
                {
                    "id": 5003,
                    "name": expected["checksum_name"],
                    "browser_download_url": expected["checksum_url"],
                    "state": "uploaded",
                    "size": assets.checksum.stat().st_size,
                    "digest": f"sha256:{hashlib.sha256(assets.checksum.read_bytes()).hexdigest()}",
                },
            ],
        },
    }
    paths: dict[str, Path] = {}
    for name, payload in payloads.items():
        path = root / f"github-{name}.json"
        path.write_text(json.dumps(payload), encoding="utf-8")
        paths[name] = path
    return paths


def expect_error(operation: Callable[[], object], expected: str) -> None:
    try:
        operation()
    except public_hosted_benchmark.PublicationError as error:
        assert expected in str(error), str(error)
    else:
        raise AssertionError(f"expected PublicationError containing {expected!r}")


def packaged_publication_fixture(
    root: Path,
) -> tuple[package_hosted_evidence.ArchiveAssets, Path, dict[str, Path], Path]:
    root.mkdir()
    source = make_final_artifact_tree(root)
    assets = package_hosted_evidence.build_evidence_archive(source, root / "assets")
    artifact_zip = root / "series-artifact.zip"
    write_actions_zip(source, artifact_zip)
    metadata = write_live_metadata(root, assets, artifact_zip)
    publication = root / "publication"
    publication.mkdir()
    (publication / "README.md").write_text(readme(), encoding="utf-8")
    return assets, artifact_zip, metadata, publication


def stage_fixture(
    assets: package_hosted_evidence.ArchiveAssets,
    artifact_zip: Path,
    metadata: dict[str, Path],
    publication: Path,
    *,
    hook: Callable[[str], None] | None = None,
) -> None:
    public_hosted_benchmark.stage_publication(
        assets.archive,
        assets.checksum,
        metadata["run"],
        metadata["artifact"],
        artifact_zip,
        metadata["tag"],
        metadata["release"],
        publication,
        _transaction_hook=hook,
    )


def verify_origin_fixture(
    assets: package_hosted_evidence.ArchiveAssets,
    artifact_zip: Path,
    metadata: dict[str, Path],
) -> dict[str, Any]:
    return public_hosted_benchmark.build_source_provenance(
        assets.manifest,
        assets.archive,
        assets.checksum,
        public_hosted_benchmark.load_json(metadata["run"]),
        public_hosted_benchmark.load_json(metadata["artifact"]),
        artifact_zip,
        public_hosted_benchmark.load_json(metadata["tag"]),
        public_hosted_benchmark.load_json(metadata["release"]),
    )


def transaction_template(root: Path) -> tuple[dict[str, bytes], str]:
    root.mkdir()
    (root / "README.md").write_text(readme(), encoding="utf-8")
    write_publication(root)
    destination = public_hosted_benchmark.publication_directory(root)
    payloads = {path.name: path.read_bytes() for path in destination.iterdir()}
    body = public_hosted_benchmark.readme_body((root / "README.md").read_text(encoding="utf-8"))
    return payloads, body


def commit_template(
    payloads: dict[str, bytes],
    body: str,
    publication: Path,
    hook: Callable[[str], None] | None = None,
) -> None:
    public_hosted_benchmark._commit_publication(
        payloads,
        body,
        publication,
        lambda: public_hosted_benchmark.verify_publication(publication),
        hook,
    )


def test_packaged_archive_integration(root: Path) -> None:
    assets, artifact_zip, metadata, publication = packaged_publication_fixture(root)

    stage_fixture(assets, artifact_zip, metadata, publication)
    public_hosted_benchmark.verify_against_archive(assets.archive, assets.checksum, publication)
    assert public_hosted_benchmark.verify_publication(publication)

    manifest, provenance, _, _, committed_checksum = public_hosted_benchmark.load_publication(publication)
    assert manifest == assets.manifest
    assert committed_checksum == assets.checksum.read_bytes()
    assert provenance["actions_artifact"]["id"] == 6001
    assert provenance["actions_artifact"]["expired_at_publication"] is False
    assert (
        public_hosted_benchmark.verify_live_source(
            assets.archive,
            assets.checksum,
            metadata["run"],
            metadata["artifact"],
            artifact_zip,
            metadata["tag"],
            metadata["release"],
            publication,
        )
        == provenance
    )
    forged_provenance = deepcopy(provenance)
    forged_provenance["actions_artifact"]["id"] += 1
    forged_provenance["provenance_sha256"] = public_hosted_benchmark.source_provenance_sha256(forged_provenance)
    expect_error(
        lambda: public_hosted_benchmark._require_committed_source_provenance(
            assets.manifest, forged_provenance, publication
        ),
        "differs from the committed provenance receipt",
    )

    # Durable verification remains valid after the original Actions artifact expires or disappears.
    artifact_zip.unlink()
    artifact_listing = json.loads(metadata["artifact"].read_text(encoding="utf-8"))
    artifact_listing["artifacts"][0]["expired"] = True
    metadata["artifact"].write_text(json.dumps(artifact_listing), encoding="utf-8")
    assert public_hosted_benchmark.verify_publication(publication)
    public_hosted_benchmark.verify_github_tag(metadata["tag"], publication)
    public_hosted_benchmark.verify_github_release(metadata["release"], publication)


def test_transaction_rollback_and_retry(root: Path) -> None:
    root.mkdir()
    payloads, body = transaction_template(root / "template")
    for phase in (
        "destination-reserved",
        "destination-partially-installed",
        "destination-installed",
        "readme-backed-up",
        "readme-installed",
        "verified",
    ):
        publication = root / f"transaction-{phase}"
        publication.mkdir()
        original = readme().encode()
        (publication / "README.md").write_bytes(original)

        def inject(observed: str, expected: str = phase) -> None:
            if observed == expected:
                raise OSError(f"injected failure at {expected}")

        try:
            commit_template(payloads, body, publication, inject)
        except OSError as error:
            assert f"injected failure at {phase}" in str(error)
        else:
            raise AssertionError(f"expected injected failure at {phase}")
        assert (publication / "README.md").read_bytes() == original
        assert not os.path.lexists(public_hosted_benchmark.publication_directory(publication))
        assert not list(publication.glob(".*transaction-*"))

        commit_template(payloads, body, publication)
        assert public_hosted_benchmark.verify_publication(publication)


def test_destination_reservation_race(root: Path) -> None:
    root.mkdir()
    payloads, body = transaction_template(root / "template")
    publication = root / "publication"
    publication.mkdir()
    original = readme().encode()
    (publication / "README.md").write_bytes(original)
    victim = root / "victim"
    victim.mkdir()
    sentinel = victim / "sentinel.txt"
    sentinel.write_text("do not overwrite", encoding="utf-8")

    def install_competing_symlink(phase: str) -> None:
        if phase == "before-destination-reserve":
            destination = public_hosted_benchmark.publication_directory(publication)
            destination.symlink_to(victim, target_is_directory=True)

    expect_error(
        lambda: commit_template(payloads, body, publication, install_competing_symlink),
        "appeared before exclusive reservation",
    )
    destination = public_hosted_benchmark.publication_directory(publication)
    assert destination.is_symlink()
    assert sentinel.read_text(encoding="utf-8") == "do not overwrite"
    assert (publication / "README.md").read_bytes() == original
    assert not list(publication.glob(".*transaction-*"))

    destination.unlink()
    commit_template(payloads, body, publication)
    assert public_hosted_benchmark.verify_publication(publication)


def test_symlink_replacement_rollback(root: Path) -> None:
    root.mkdir()
    payloads, body = transaction_template(root / "template")

    directory_case = root / "directory-symlink"
    directory_case.mkdir()
    (directory_case / "README.md").write_text(readme(), encoding="utf-8")
    victim_directory = root / "directory-victim"
    victim_directory.mkdir()
    sentinel = victim_directory / "sentinel.txt"
    sentinel.write_text("do not delete", encoding="utf-8")

    def replace_directory(phase: str) -> None:
        if phase != "destination-installed":
            return
        destination = public_hosted_benchmark.publication_directory(directory_case)
        shutil.rmtree(destination)
        destination.symlink_to(victim_directory, target_is_directory=True)
        raise OSError("injected directory symlink replacement")

    expect_error(
        lambda: commit_template(payloads, body, directory_case, replace_directory),
        "installed publication directory was replaced or modified",
    )
    destination = public_hosted_benchmark.publication_directory(directory_case)
    assert destination.is_symlink()
    assert sentinel.read_text(encoding="utf-8") == "do not delete"
    preserved = list(directory_case.glob(".*transaction-*"))
    assert len(preserved) == 1
    destination.unlink()
    shutil.rmtree(preserved[0])
    commit_template(payloads, body, directory_case)
    assert public_hosted_benchmark.verify_publication(directory_case)

    readme_case = root / "readme-symlink"
    readme_case.mkdir()
    original = readme().encode()
    (readme_case / "README.md").write_bytes(original)
    victim_readme = root / "readme-victim.txt"
    victim_readme.write_text("do not overwrite", encoding="utf-8")

    def replace_readme(phase: str) -> None:
        if phase != "readme-installed":
            return
        readme_path = readme_case / "README.md"
        readme_path.unlink()
        readme_path.symlink_to(victim_readme)
        raise OSError("injected README symlink replacement")

    expect_error(
        lambda: commit_template(payloads, body, readme_case, replace_readme),
        "installed README was replaced or modified",
    )
    assert (readme_case / "README.md").is_symlink()
    assert victim_readme.read_text(encoding="utf-8") == "do not overwrite"
    assert not os.path.lexists(public_hosted_benchmark.publication_directory(readme_case))
    preserved = list(readme_case.glob(".*transaction-*"))
    assert len(preserved) == 1
    backups = list(preserved[0].glob("README.original"))
    assert len(backups) == 1 and backups[0].read_bytes() == original
    (readme_case / "README.md").unlink()
    os.replace(backups[0], readme_case / "README.md")
    shutil.rmtree(preserved[0])
    commit_template(payloads, body, readme_case)
    assert public_hosted_benchmark.verify_publication(readme_case)

    install_race = root / "readme-install-race"
    install_race.mkdir()
    (install_race / "README.md").write_bytes(original)
    race_victim = root / "readme-install-race-victim.txt"
    race_victim.write_text("do not overwrite", encoding="utf-8")

    def recreate_readme_before_install(phase: str) -> None:
        if phase == "readme-backed-up":
            (install_race / "README.md").symlink_to(race_victim)

    expect_error(
        lambda: commit_template(payloads, body, install_race, recreate_readme_before_install),
        "README path was recreated",
    )
    assert (install_race / "README.md").is_symlink()
    assert race_victim.read_text(encoding="utf-8") == "do not overwrite"
    assert not os.path.lexists(public_hosted_benchmark.publication_directory(install_race))
    preserved = list(install_race.glob(".*transaction-*"))
    assert len(preserved) == 1
    backup = preserved[0] / "README.original"
    assert backup.read_bytes() == original
    (install_race / "README.md").unlink()
    os.replace(backup, install_race / "README.md")
    shutil.rmtree(preserved[0])
    commit_template(payloads, body, install_race)
    assert public_hosted_benchmark.verify_publication(install_race)


def test_publication_origin_fail_closed(root: Path) -> None:
    assets, artifact_zip, metadata, publication = packaged_publication_fixture(root)
    original_readme = (publication / "README.md").read_bytes()

    cases = (
        ("run", lambda data: data.__setitem__("conclusion", "failure"), "did not conclude successfully"),
        (
            "artifact",
            lambda data: data["artifacts"][0].__setitem__("expired", True),
            "Actions artifact is expired",
        ),
        (
            "artifact",
            lambda data: data["artifacts"][0].__setitem__("digest", f"sha256:{'0' * 64}"),
            "differs from its server digest",
        ),
        ("tag", lambda data: data["object"].__setitem__("type", "tag"), "annotated"),
        ("release", lambda data: data.__setitem__("immutable", False), "is not immutable"),
    )
    for name, mutation, expected in cases:
        path = metadata[name]
        canonical = path.read_bytes()
        data = json.loads(canonical)
        mutation(data)
        path.write_text(json.dumps(data), encoding="utf-8")
        expect_error(
            lambda: verify_origin_fixture(assets, artifact_zip, metadata),
            expected,
        )
        path.write_bytes(canonical)
        assert (publication / "README.md").read_bytes() == original_readme
        assert not os.path.lexists(public_hosted_benchmark.publication_directory(publication))

    duplicate = json.loads(metadata["artifact"].read_text(encoding="utf-8"))
    duplicate["total_count"] = 2
    duplicate["artifacts"].append(deepcopy(duplicate["artifacts"][0]))
    canonical_artifact = metadata["artifact"].read_bytes()
    metadata["artifact"].write_text(json.dumps(duplicate), encoding="utf-8")
    expect_error(
        lambda: verify_origin_fixture(assets, artifact_zip, metadata),
        "did not return exactly one",
    )
    metadata["artifact"].write_bytes(canonical_artifact)

    tampered_zip = artifact_zip.with_name("tampered-series-artifact.zip")
    tampered_zip.write_bytes(artifact_zip.read_bytes() + b"tampered")
    expect_error(
        lambda: verify_origin_fixture(assets, tampered_zip, metadata),
        "byte count differs",
    )
    assert (publication / "README.md").read_bytes() == original_readme
    assert not os.path.lexists(public_hosted_benchmark.publication_directory(publication))


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
        assert "Browsershot's Node/Chromium generic temporary storage uses a disclosed private tmpfs `TMPDIR`" in body
        assert "This target-specific storage accommodation can affect wall time" in body

        release_metadata = root / "github-release.json"
        _, provenance, _, _, _ = public_hosted_benchmark.load_publication(root)
        release_assets_by_name = {item["name"]: item for item in provenance["release"]["assets"]}
        canonical_release = {
            "id": provenance["release"]["id"],
            "tag_name": assets["release_tag"],
            "immutable": True,
            "draft": False,
            "prerelease": False,
            "html_url": f"https://github.com/oxhq/pliego/releases/tag/{assets['release_tag']}",
            "assets": [
                {
                    "id": release_assets_by_name[assets["archive_name"]]["id"],
                    "name": assets["archive_name"],
                    "browser_download_url": assets["archive_url"],
                    "state": "uploaded",
                    "size": release_assets_by_name[assets["archive_name"]]["size"],
                    "digest": release_assets_by_name[assets["archive_name"]]["digest"],
                },
                {
                    "id": release_assets_by_name[assets["checksum_name"]]["id"],
                    "name": assets["checksum_name"],
                    "browser_download_url": assets["checksum_url"],
                    "state": "uploaded",
                    "size": release_assets_by_name[assets["checksum_name"]]["size"],
                    "digest": release_assets_by_name[assets["checksum_name"]]["digest"],
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
            {
                "id": 5999,
                "name": "extra.txt",
                "browser_download_url": "https://example.invalid/extra.txt",
                "state": "uploaded",
                "size": 1,
                "digest": f"sha256:{'f' * 64}",
            }
        )
        release_metadata.write_text(json.dumps(extra_asset_release), encoding="utf-8")
        expect_error(
            lambda: public_hosted_benchmark.verify_github_release(release_metadata, root),
            "exactly the two committed evidence assets",
        )

        tag_metadata = root / "github-tag.json"
        canonical_tag = {
            "ref": f"refs/tags/{assets['release_tag']}",
            "object": {
                "type": "commit",
                "sha": manifest["source"]["revision"],
                "url": (f"https://api.github.com/repos/oxhq/pliego/git/commits/{manifest['source']['revision']}"),
            },
        }
        tag_metadata.write_text(json.dumps(canonical_tag), encoding="utf-8")
        public_hosted_benchmark.verify_github_tag(tag_metadata, root)
        annotated_tag = deepcopy(canonical_tag)
        annotated_tag["object"]["type"] = "tag"
        tag_metadata.write_text(json.dumps(annotated_tag), encoding="utf-8")
        expect_error(lambda: public_hosted_benchmark.verify_github_tag(tag_metadata, root), "annotated")

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
        test_transaction_rollback_and_retry(root / "transactional-publication")
        test_destination_reservation_race(root / "destination-reservation-race")
        test_symlink_replacement_rollback(root / "symlink-replacement")
        test_publication_origin_fail_closed(root / "origin-fail-closed")


if __name__ == "__main__":
    main()
