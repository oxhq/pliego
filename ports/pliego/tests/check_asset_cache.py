#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

MANIFEST_SCHEMA = "pliego.asset-manifest"
RESOURCE_POLICY = "pliego.resource-policy.v1"
CACHE_SCOPE = "pliego.asset-cache.v1"
CACHE_DIRECTORY = ".pliego-asset-cache-v1"
ASSET_URLS = (
    "https://assets.pliego.test/first.js",
    "https://assets.pliego.test/renamed.js",
)
ASSET_FILES = ("first.js", "renamed.js")
CHANGED_BODY = (
    b"window.assetExecutions = (window.assetExecutions || 0) + 1;\n"
    b'document.documentElement.dataset.assetTheme = "orange";\n'
)
BAD_DIGEST = "0" * 64
PROCESS_TIMEOUT_SECONDS = 60


def fail(message: str, code: int = 1) -> None:
    print(f"asset cache check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def digest(body: bytes) -> str:
    return hashlib.sha256(body).hexdigest()


def read_object(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"missing JSON artifact: {path}")
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read JSON artifact {path}: {error}")
    require(isinstance(value, dict), f"JSON artifact is not an object: {path}")
    return value


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def resource_rows(artifacts: Path) -> list[dict[str, Any]]:
    path = artifacts / "resources.jsonl"
    require(path.is_file(), f"missing resource log: {path}")
    rows: list[dict[str, Any]] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        try:
            row = json.loads(line)
        except json.JSONDecodeError as error:
            fail(f"invalid resources.jsonl line {line_number}: {error}")
        require(isinstance(row, dict), f"resource log line {line_number} is not an object")
        rows.append(row)
    return rows


def manifest_value(body: bytes) -> dict[str, Any]:
    content_hash = digest(body)
    return {
        "schema": MANIFEST_SCHEMA,
        "version": 1,
        "assets": [
            {"url": url, "path": path, "sha256": content_hash}
            for url, path in zip(ASSET_URLS, ASSET_FILES, strict=True)
        ],
    }


def write_asset_set(root: Path, body: bytes) -> str:
    for name in ASSET_FILES:
        (root / name).write_bytes(body)
    content_hash = digest(body)
    (root / "assets.json").write_text(
        json.dumps(manifest_value(body), indent=2) + "\n",
        encoding="utf-8",
    )
    return content_hash


def write_bad_manifest(root: Path) -> None:
    value = {
        "schema": MANIFEST_SCHEMA,
        "version": 1,
        "assets": [
            {
                "url": "https://assets.pliego.test/bad.js",
                "path": ASSET_FILES[0],
                "sha256": BAD_DIGEST,
            }
        ],
    }
    (root / "assets.json").write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def retain(
    root: Path,
    artifacts: Path,
    document_pdf: Path,
    result: subprocess.CompletedProcess[str],
    destination: Path,
) -> None:
    destination.mkdir(parents=True)
    (destination / "process.stdout.log").write_text(result.stdout, encoding="utf-8")
    (destination / "process.stderr.log").write_text(result.stderr, encoding="utf-8")
    inputs = destination / "inputs"
    inputs.mkdir()
    for source in sorted((*root.glob("*.html"), *root.glob("*.js"), *root.glob("*.json"))):
        shutil.copy2(source, inputs / source.name)
    cache = root / CACHE_DIRECTORY
    if cache.is_dir():
        shutil.copytree(cache, destination / "cache")
    if artifacts.is_dir():
        shutil.copytree(artifacts, destination / "artifacts")
    if document_pdf.is_file():
        shutil.copy2(document_pdf, destination / "document.pdf")


def run(
    binary: Path,
    root: Path,
    output: Path,
    name: str,
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any], Path, Path]:
    artifacts = root / f"{name}-artifacts"
    document_pdf = root / f"{name}.pdf"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(root), "TMP": str(root), "TEMP": str(root)})
    command = [
        str(binary),
        "render",
        "index.html",
        "--output",
        str(document_pdf),
        "--artifacts",
        str(artifacts),
        "--asset-manifest",
        "assets.json",
        "--page-size",
        "200x100",
        "--page-margins",
        "8,8,8,8",
    ]
    try:
        result = subprocess.run(
            command,
            cwd=root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=PROCESS_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else (error.stdout or "")
        stderr = error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else (error.stderr or "")
        result = subprocess.CompletedProcess(command, 124, stdout, stderr)
    destination = output / name
    retain(root, artifacts, document_pdf, result, destination)
    summary = final_json(result)
    (destination / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return result, summary, artifacts, document_pdf


def verify_asset_artifact(
    artifacts: Path,
    expected_hash: str,
    expected_results: dict[str, str],
    expected_metrics: dict[str, int],
    expected_bytes: int,
) -> dict[str, Any]:
    environment = read_object(artifacts / "environment.json")
    policy = environment.get("resource_policy")
    require(isinstance(policy, dict), f"environment omitted resource policy: {environment!r}")
    require(policy.get("schema") == RESOURCE_POLICY and policy.get("version") == 1, repr(policy))
    manifest = policy.get("asset_manifest")
    require(isinstance(manifest, dict), f"resource policy omitted asset manifest: {policy!r}")
    require(manifest.get("schema") == MANIFEST_SCHEMA and manifest.get("version") == 1, repr(manifest))
    require(manifest.get("status") == "verified", repr(manifest))
    require(Path(str(manifest.get("manifest"))).name == "assets.json", repr(manifest))

    cache = manifest.get("cache")
    require(isinstance(cache, dict), f"manifest artifact omitted cache metrics: {manifest!r}")
    require(cache.get("scope") == CACHE_SCOPE, repr(cache))
    require(cache.get("policy") == "bounded-lexicographic-sha256", repr(cache))
    require(cache.get("max_entries") == 128 and cache.get("max_bytes") == 64 * 1024 * 1024, repr(cache))
    for metric, expected in {**expected_metrics, "invalidations": 0, "evictions": 0}.items():
        require(cache.get(metric) == expected, f"expected cache {metric}={expected}: {cache!r}")

    assets = manifest.get("assets")
    require(isinstance(assets, list) and len(assets) == len(ASSET_URLS), repr(assets))
    by_url = {asset.get("url"): asset for asset in assets if isinstance(asset, dict)}
    require(set(by_url) == set(ASSET_URLS), f"unexpected manifest asset rows: {assets!r}")
    for url in ASSET_URLS:
        asset = by_url[url]
        require(asset.get("content_hash") == f"sha256:{expected_hash}", repr(asset))
        require(asset.get("cache_result") == expected_results[url], repr(asset))
        require(asset.get("bytes") == expected_bytes, repr(asset))
    return cache


def verify_loaded_assets(
    artifacts: Path,
    render_id: str,
    expected_hash: str,
    expected_results: dict[str, str],
    expected_body: bytes,
) -> list[dict[str, Any]]:
    rows = resource_rows(artifacts)
    loaded = [row for row in rows if row.get("status") == "loaded" and row.get("url") in ASSET_URLS]
    require(len(loaded) == len(ASSET_URLS), f"expected exactly two loaded manifest assets: {rows!r}")
    by_url = {row["url"]: row for row in loaded}
    require(set(by_url) == set(ASSET_URLS), repr(by_url))
    normalized = []
    for url in ASSET_URLS:
        row = by_url[url]
        expected_address = f"sha256:{expected_hash}"
        require(row.get("render_id") == render_id and row.get("policy") == RESOURCE_POLICY, repr(row))
        require(row.get("content_hash") == expected_address, repr(row))
        require(row.get("resource") == expected_address and row.get("sha256") == expected_hash, repr(row))
        require(row.get("cache_result") == expected_results[url], repr(row))
        require(row.get("bytes") == len(expected_body), repr(row))
        require(row.get("content_type") == "text/javascript", repr(row))
        artifact = row.get("artifact")
        require(artifact == f"resources/{expected_hash}", repr(row))
        require((artifacts / artifact).read_bytes() == expected_body, f"resource bytes differ for {url}")
        normalized.append(
            {
                "url": url,
                "content_hash": row["content_hash"],
                "cache_result": row["cache_result"],
                "bytes": row["bytes"],
                "content_type": row["content_type"],
            }
        )
    return normalized


def verify_success(
    result: subprocess.CompletedProcess[str],
    summary: dict[str, Any],
    artifacts: Path,
    document_pdf: Path,
    body: bytes,
    theme: str,
    expected_results: dict[str, str],
    expected_metrics: dict[str, int],
) -> dict[str, Any]:
    require(result.returncode == 0, f"render exited with {result.returncode}: {result.stderr[-3000:]}")
    require(summary.get("status") == "rendered", repr(summary))
    require(
        summary.get("readiness") == {"fixture": "asset-cache", "executions": 2, "theme": theme},
        repr(summary.get("readiness")),
    )
    render_id = summary.get("render_id")
    require(isinstance(render_id, str) and render_id.startswith("sha256:"), repr(render_id))
    scene = summary.get("scene")
    require(isinstance(scene, dict), f"summary omitted scene: {summary!r}")
    scene_hash = scene.get("hash")
    require(isinstance(scene_hash, str) and scene_hash.startswith("sha256:"), repr(scene))
    expected_hash = digest(body)
    cache = verify_asset_artifact(
        artifacts,
        expected_hash,
        expected_results,
        expected_metrics,
        len(body),
    )
    normalized_rows = verify_loaded_assets(artifacts, render_id, expected_hash, expected_results, body)
    scene_bytes = (artifacts / "scene.json").read_bytes()
    preview = (artifacts / "scene-preview.png").read_bytes()
    pdf = document_pdf.read_bytes()
    require(preview.startswith(b"\x89PNG\r\n\x1a\n"), "scene preview is not a PNG")
    require(pdf.startswith(b"%PDF-"), "document output is not a PDF")
    return {
        "render_id": render_id,
        "scene_hash": scene_hash,
        "scene_bytes": scene_bytes,
        "preview": preview,
        "pdf": pdf,
        "metrics": {key: cache[key] for key in ("hits", "misses", "invalidations", "evictions")},
        "asset_logs": normalized_rows,
    }


def verify_bad_manifest(
    result: subprocess.CompletedProcess[str],
    summary: dict[str, Any],
    artifacts: Path,
    document_pdf: Path,
    actual_hash: str,
) -> dict[str, Any]:
    require(result.returncode == 1, f"bad manifest exited with {result.returncode}: {result.stderr[-3000:]}")
    require(summary.get("status") == "failed", repr(summary))
    error = summary.get("error")
    require(isinstance(error, dict) and error.get("code") == "ASSET_HASH_MISMATCH", repr(error))
    policy = read_object(artifacts / "environment.json").get("resource_policy")
    require(isinstance(policy, dict), repr(policy))
    manifest = policy.get("asset_manifest")
    require(isinstance(manifest, dict) and manifest.get("status") == "failed", repr(manifest))
    manifest_error = manifest.get("error")
    require(isinstance(manifest_error, dict), repr(manifest))
    require(manifest_error.get("code") == "ASSET_HASH_MISMATCH", repr(manifest_error))
    require(manifest_error.get("expected") == f"sha256:{BAD_DIGEST}", repr(manifest_error))
    require(manifest_error.get("actual") == f"sha256:{actual_hash}", repr(manifest_error))
    rows = resource_rows(artifacts)
    require(len(rows) == 1, f"hash mismatch should fail before Servo resource events: {rows!r}")
    row = rows[0]
    require(row.get("policy") == CACHE_SCOPE and row.get("status") == "failed", repr(row))
    require(row.get("code") == "ASSET_HASH_MISMATCH", repr(row))
    require(row.get("expected") == f"sha256:{BAD_DIGEST}", repr(row))
    require(row.get("actual") == f"sha256:{actual_hash}", repr(row))
    require(row.get("cache_result") is None and row.get("bytes") is None, repr(row))
    require(not document_pdf.exists(), "bad manifest published the requested PDF")
    require(not (artifacts / "document.pdf").exists(), "bad manifest retained a diagnostic PDF")
    require(not (artifacts / "scene.json").exists(), "bad manifest reached scene capture")
    return {
        "code": row["code"],
        "expected": row["expected"],
        "actual": row["actual"],
        "failed_before_scene": True,
    }


def cache_objects(root: Path) -> set[str]:
    cache = root / CACHE_DIRECTORY
    require(cache.is_dir(), f"cache directory was not created: {cache}")
    return {entry.name for entry in cache.iterdir() if entry.is_file()}


def self_test() -> None:
    fixture = Path(__file__).resolve().parent / "fixtures/asset-cache"
    first = (fixture / ASSET_FILES[0]).read_bytes()
    second = (fixture / ASSET_FILES[1]).read_bytes()
    require(first == second, "same-content fixture bytes differ")
    manifest = read_object(fixture / "assets.json")
    require(manifest == manifest_value(first), "checked-in asset manifest does not match fixture bytes")
    require(digest(first) != digest(CHANGED_BODY), "changed fixture did not change its content identity")
    with tempfile.TemporaryDirectory(prefix="pliego-asset-cache-self-test-") as temp:
        root = Path(temp)
        changed_hash = write_asset_set(root, CHANGED_BODY)
        require(changed_hash == digest(CHANGED_BODY), "changed manifest digest is incorrect")
        require((root / ASSET_FILES[0]).read_bytes() == (root / ASSET_FILES[1]).read_bytes(), "copy drift")


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        print("asset cache checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)

    binary = Path(sys.argv[1]).expanduser().resolve()
    output = Path(sys.argv[2]).expanduser().resolve()
    fixture = Path(__file__).resolve().parent / "fixtures/asset-cache"
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(fixture.is_dir(), f"asset cache fixture does not exist: {fixture}")
    require(not output.exists(), f"output directory already exists: {output}")
    output.mkdir(parents=True)

    with tempfile.TemporaryDirectory(prefix="pliego-asset-cache-") as temp:
        root = Path(temp) / "fixture"
        shutil.copytree(fixture, root)
        original_body = (root / ASSET_FILES[0]).read_bytes()
        original_hash = digest(original_body)

        first_results = {ASSET_URLS[0]: "miss", ASSET_URLS[1]: "hit"}
        all_hits = {url: "hit" for url in ASSET_URLS}
        first_run = run(binary, root, output, "first")
        first = verify_success(*first_run, original_body, "blue", first_results, {"hits": 1, "misses": 1})
        require(cache_objects(root) == {original_hash}, "same bytes under two names created duplicate cache objects")

        second_run = run(binary, root, output, "second")
        second = verify_success(*second_run, original_body, "blue", all_hits, {"hits": 2, "misses": 0})
        require(first["render_id"] == second["render_id"], "cache state changed the stable render ID")
        require(first["scene_bytes"] == second["scene_bytes"], "cache hit changed canonical scene bytes")
        require(first["preview"] == second["preview"], "cache hit changed preview bytes")
        require(first["pdf"] == second["pdf"], "cache hit changed PDF bytes")

        changed_hash = write_asset_set(root, CHANGED_BODY)
        changed_run = run(binary, root, output, "changed")
        changed = verify_success(
            *changed_run,
            CHANGED_BODY,
            "orange",
            first_results,
            {"hits": 1, "misses": 1},
        )
        require(cache_objects(root) == {original_hash, changed_hash}, "changed bytes did not create one new cache object")
        require(first["render_id"] != changed["render_id"], "changed asset bytes did not change render identity")
        require(first["scene_bytes"] != changed["scene_bytes"], "changed asset bytes did not change scene bytes")
        require(first["preview"] != changed["preview"], "changed asset bytes did not change preview bytes")
        require(first["pdf"] != changed["pdf"], "changed asset bytes did not change PDF bytes")

        write_bad_manifest(root)
        bad_run = run(binary, root, output, "bad-manifest")
        bad = verify_bad_manifest(*bad_run, changed_hash)
        require(BAD_DIGEST not in cache_objects(root), "hash mismatch populated an unverified cache object")

    comparison = {
        "schema": "pliego.asset-cache-check",
        "version": 1,
        "content_identity": {
            "same_bytes_different_names": True,
            "original": f"sha256:{original_hash}",
            "changed": f"sha256:{changed_hash}",
        },
        "first": {key: first[key] for key in ("render_id", "scene_hash", "metrics", "asset_logs")},
        "second": {key: second[key] for key in ("render_id", "scene_hash", "metrics", "asset_logs")},
        "changed": {key: changed[key] for key in ("render_id", "scene_hash", "metrics", "asset_logs")},
        "repeat_equal": {"scene": True, "preview": True, "pdf": True},
        "changed_invalidated_render": {"render_id": True, "scene": True, "preview": True, "pdf": True},
        "bad_manifest": bad,
    }
    (output / "comparison.json").write_text(
        json.dumps(comparison, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"asset cache check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
