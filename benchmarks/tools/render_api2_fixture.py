#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Render one prepared benchmark fixture through Pliego API 2.

This helper is for the hosted correctness smoke. It deliberately records no timing
or resource metric; publishable measurement remains owned by the cgroup benchmark
runner.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any

PORTABLE_PATH = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9._/-]*[A-Za-z0-9_-])?$")
SHA256 = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
SEMVER = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")
TARGET = re.compile(
    r"^[a-z0-9]+(?:_[a-z0-9]+)*-[a-z0-9]+(?:_[a-z0-9]+)*-[a-z0-9]+(?:_[a-z0-9]+)*(?:-[a-z0-9]+(?:_[a-z0-9]+)*)?$"
)
DIAGNOSTIC_PATH = re.compile(r"^diagnostics/[A-Za-z0-9](?:[A-Za-z0-9._/-]*[A-Za-z0-9_-])?$")


class FixtureError(RuntimeError):
    pass


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=True, separators=(",", ":")) + "\n").encode("utf-8")


def digest_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def digest_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def media_type(path: str) -> str:
    extension = PurePosixPath(path).suffix.lower()
    return {
        ".css": "text/css;charset=utf-8",
        ".html": "text/html;charset=utf-8",
        ".htm": "text/html;charset=utf-8",
        ".js": "text/javascript;charset=utf-8",
        ".mjs": "text/javascript;charset=utf-8",
        ".json": "application/json",
        ".svg": "image/svg+xml",
        ".gif": "image/gif",
        ".jpg": "image/jpeg",
        ".jpeg": "image/jpeg",
        ".png": "image/png",
        ".webp": "image/webp",
        ".woff": "font/woff",
        ".woff2": "font/woff2",
        ".ttf": "font/ttf",
        ".otf": "font/otf",
    }.get(extension, "application/octet-stream")


def portable_path(value: str) -> str:
    path = PurePosixPath(value)
    if (
        not value
        or not PORTABLE_PATH.fullmatch(value)
        or path.is_absolute()
        or ".." in path.parts
        or len(path.parts) > 32
        or len(value.encode("utf-8")) > 240
    ):
        raise FixtureError(f"unsafe API 2 input path: {value!r}")
    return value


def copy_input(fixture: Path, job_root: Path, paths: list[str]) -> bytes:
    input_root = job_root / "input"
    input_root.mkdir(mode=0o700)
    entries = []
    for relative in sorted({portable_path(path) for path in paths}, key=lambda value: value.encode("ascii")):
        source = fixture.joinpath(*PurePosixPath(relative).parts)
        if source.is_symlink() or not source.is_file():
            raise FixtureError(f"fixture input is absent or not a regular file: {relative}")
        payload = source.read_bytes()
        destination = input_root.joinpath(*PurePosixPath(relative).parts)
        destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        destination.write_bytes(payload)
        destination.chmod(0o600)
        entries.append(
            {
                "path": relative,
                "media_type": media_type(relative),
                "sha256": digest_bytes(payload),
                "bytes": len(payload),
            }
        )
    manifest = {
        "schema": "pliego.input-manifest",
        "version": 1,
        "url_root": "pliego-input:///",
        "entries": entries,
    }
    manifest_bytes = canonical_json(manifest)
    (job_root / "input-manifest.json").write_bytes(manifest_bytes)
    (job_root / "input-manifest.json").chmod(0o600)
    return manifest_bytes


def render_request(entrypoint: str, manifest_bytes: bytes) -> bytes:
    request = {
        "schema": "pliego.render-request",
        "version": 1,
        "api": 2,
        "profile": None,
        "input": {
            "entrypoint": portable_path(entrypoint),
            "manifest": {
                "path": "input-manifest.json",
                "media_type": "application/vnd.pliego.input-manifest+json",
                "sha256": digest_bytes(manifest_bytes),
                "bytes": len(manifest_bytes),
            },
        },
        "environment": {"locale": "en-US", "timezone": "UTC"},
        "page": {
            "size": {"name": "A4"},
            "margins_app_units": {"top": 0, "right": 0, "bottom": 0, "left": 0},
            "geometry_authority": "request-only-v1",
        },
        "resources": {"network": "deny", "host_fonts": "deny"},
        "time": {"policy_version": 1, "epoch_unix_ms": 946684800000, "initial_offset_ns": 0},
        "settlement": {
            "policy_version": 1,
            "infinite_source_policy": "fail",
            "empty_checkpoints": 2,
            "limits": {
                "virtual_span_ms": 86400000,
                "ordinary_tasks": 100000,
                "microtasks": 1000000,
                "rendering_opportunities": 10000,
                "mutations": 1000000,
                "host_wall_ms": 60000,
            },
        },
        "diagnostics": {"retention": "always"},
    }
    return canonical_json(request)


def verified_artifact(root: Path, descriptor: Any, expected_path: str) -> Path:
    if not isinstance(descriptor, dict) or set(descriptor) != {"path", "media_type", "sha256", "bytes"}:
        raise FixtureError(f"invalid API 2 artifact descriptor for {expected_path}")
    if descriptor["path"] != expected_path or not SHA256.fullmatch(str(descriptor["sha256"])):
        raise FixtureError(f"API 2 artifact identity differs for {expected_path}")
    path = root / expected_path
    if path.is_symlink() or not path.is_file():
        raise FixtureError(f"API 2 artifact is absent: {expected_path}")
    payload = path.read_bytes()
    if descriptor["bytes"] != len(payload) or descriptor["sha256"] != digest_bytes(payload):
        raise FixtureError(f"API 2 artifact bytes differ for {expected_path}")
    return path


def verified_diagnostics(job_root: Path, diagnostics: Any) -> None:
    if (
        not isinstance(diagnostics, dict)
        or set(diagnostics) != {"retained", "artifacts"}
        or diagnostics["retained"] is not True
        or not isinstance(diagnostics["artifacts"], list)
    ):
        raise FixtureError("API 2 success omitted retained diagnostics")
    described: set[str] = set()
    for descriptor in diagnostics["artifacts"]:
        if not isinstance(descriptor, dict) or set(descriptor) != {"path", "media_type", "sha256", "bytes"}:
            raise FixtureError("invalid API 2 diagnostic descriptor")
        relative = descriptor["path"]
        if (
            not isinstance(relative, str)
            or not DIAGNOSTIC_PATH.fullmatch(relative)
            or relative in described
            or not isinstance(descriptor["media_type"], str)
            or not 3 <= len(descriptor["media_type"]) <= 255
            or not SHA256.fullmatch(str(descriptor["sha256"]))
            or not isinstance(descriptor["bytes"], int)
            or isinstance(descriptor["bytes"], bool)
            or descriptor["bytes"] < 1
        ):
            raise FixtureError("invalid API 2 diagnostic identity")
        parts = PurePosixPath(relative).parts
        if ".." in parts:
            raise FixtureError("unsafe API 2 diagnostic path")
        path = job_root.joinpath(*parts)
        if path.is_symlink() or not path.is_file():
            raise FixtureError(f"API 2 diagnostic is absent: {relative}")
        payload = path.read_bytes()
        if descriptor["bytes"] != len(payload) or descriptor["sha256"] != digest_bytes(payload):
            raise FixtureError(f"API 2 diagnostic bytes differ: {relative}")
        described.add(relative)
    root = job_root / "diagnostics"
    if root.is_symlink() or not root.is_dir():
        raise FixtureError("API 2 retained diagnostics directory is absent")
    actual = {
        path.relative_to(job_root).as_posix() for path in root.rglob("*") if path.is_file() and not path.is_symlink()
    }
    if any(path.is_symlink() for path in root.rglob("*")) or actual != described:
        raise FixtureError("API 2 diagnostic descriptor set differs from retained files")


def render(
    binary: Path,
    fixture: Path,
    entrypoint: str,
    assets: list[str],
    output: Path,
    expected_version: str | None,
    expected_commit: str | None,
) -> dict[str, Any]:
    binary = binary.resolve(strict=True)
    fixture = fixture.resolve(strict=True)
    if not binary.is_file() or not fixture.is_dir():
        raise FixtureError("binary and fixture must be regular existing paths")
    with tempfile.TemporaryDirectory(prefix="pliego-api2-smoke-") as temporary:
        job_root = Path(temporary)
        job_root.chmod(0o700)
        manifest_bytes = copy_input(fixture, job_root, [entrypoint, *assets])
        request_bytes = render_request(entrypoint, manifest_bytes)
        completed = subprocess.run(
            [str(binary), "render-api2"],
            cwd=job_root,
            input=request_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=120,
        )
        if completed.returncode != 0 or completed.stderr:
            failure_path = job_root / "diagnostics" / "failure.json"
            failure = failure_path.read_bytes() if failure_path.is_file() else b""
            raise FixtureError(
                "API 2 render failed: "
                f"exit={completed.returncode}, stdout={completed.stdout[-4096:]!r}, "
                f"stderr={completed.stderr[-4096:]!r}, failure={failure[-4096:]!r}"
            )
        try:
            result = json.loads(completed.stdout)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise FixtureError("API 2 render returned invalid JSON") from error
        if canonical_json(result) != completed.stdout:
            raise FixtureError("API 2 render result is not canonical JSON plus LF")
        if (
            not isinstance(result, dict)
            or result.get("schema") != "pliego.render-result"
            or result.get("version") != 1
            or result.get("api") != 2
            or result.get("status") != "success"
            or result.get("request") != json.loads(request_bytes)
            or result.get("error") is not None
        ):
            raise FixtureError("API 2 render result is not the accepted success contract")
        engine = result.get("engine")
        if not isinstance(engine, dict) or set(engine) != {"name", "version", "api", "source_commit", "runtime"}:
            raise FixtureError("API 2 render result omitted engine identity")
        runtime = engine.get("runtime")
        if (
            engine.get("name") != "pliego"
            or engine.get("api") != 2
            or not SEMVER.fullmatch(str(engine.get("version")))
            or not COMMIT.fullmatch(str(engine.get("source_commit")))
            or not isinstance(runtime, dict)
            or set(runtime) != {"mode", "target", "binary_sha256", "servo_base"}
            or runtime.get("mode") != "one-shot"
            or not TARGET.fullmatch(str(runtime.get("target")))
            or runtime.get("binary_sha256") != digest_file(binary)
            or not COMMIT.fullmatch(str(runtime.get("servo_base")))
        ):
            raise FixtureError("API 2 engine identity differs from the measured binary")
        if expected_version is not None and engine.get("version") != expected_version:
            raise FixtureError("API 2 engine version differs from the expected release")
        if expected_commit is not None and engine.get("source_commit") != expected_commit:
            raise FixtureError("API 2 engine commit differs from the expected release")
        if result.get("conformance") != {"requested": None, "status": "not-requested", "evidence": None}:
            raise FixtureError("API 2 result is not the profile-null conformance tuple")
        verified_diagnostics(job_root, result.get("diagnostics"))
        delivery = result.get("delivery")
        if not isinstance(delivery, dict) or set(delivery) != {"pdf", "scene", "bundle"}:
            raise FixtureError("API 2 success omitted the complete delivery")
        delivery_root = job_root / "delivery"
        pdf = verified_artifact(delivery_root, delivery["pdf"], "document.pdf")
        verified_artifact(delivery_root, delivery["scene"], "scene.json")
        verified_artifact(delivery_root, delivery["bundle"], "bundle.json")
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(pdf, output)
        return {
            "schema": "pliego.api2-fixture-smoke",
            "version": 1,
            "engine_version": engine["version"],
            "source_commit": engine["source_commit"],
            "input_manifest_sha256": digest_bytes(manifest_bytes),
            "request_sha256": digest_bytes(request_bytes),
            "pdf_sha256": digest_bytes(output.read_bytes()),
            "pdf_bytes": output.stat().st_size,
        }


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="pliego-api2-helper-test-") as temporary:
        root = Path(temporary)
        fixture = root / "fixture"
        job = root / "job"
        fixture.mkdir()
        job.mkdir(mode=0o700)
        (fixture / "document.html").write_text("<p>ok</p>\n", encoding="utf-8")
        (fixture / "Ahem.ttf").write_bytes(b"font")
        manifest = copy_input(fixture, job, ["document.html", "Ahem.ttf"])
        decoded = json.loads(manifest)
        assert [entry["path"] for entry in decoded["entries"]] == ["Ahem.ttf", "document.html"]
        request = json.loads(render_request("document.html", manifest))
        assert request["api"] == 2
        assert request["input"]["manifest"]["sha256"] == digest_bytes(manifest)
        assert request["page"]["margins_app_units"] == {"top": 0, "right": 0, "bottom": 0, "left": 0}
        diagnostics = job / "diagnostics"
        diagnostics.mkdir()
        payload = b'{"fixture":true}\n'
        (diagnostics / "environment.json").write_bytes(payload)
        descriptor = {
            "path": "diagnostics/environment.json",
            "media_type": "application/json",
            "sha256": digest_bytes(payload),
            "bytes": len(payload),
        }
        verified_diagnostics(job, {"retained": True, "artifacts": [descriptor]})
        broken = dict(descriptor)
        broken["bytes"] += 1
        try:
            verified_diagnostics(job, {"retained": True, "artifacts": [broken]})
        except FixtureError:
            pass
        else:
            raise AssertionError("diagnostic descriptor drift was accepted")
    print("API 2 fixture helper self-test passed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--fixture", type=Path)
    parser.add_argument("--entrypoint", default="input.html")
    parser.add_argument("--asset", action="append", default=[])
    parser.add_argument("--output", type=Path)
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-commit")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        if args.self_test:
            self_test()
            return 0
        if args.binary is None or args.fixture is None or args.output is None:
            parser.error("--binary, --fixture, and --output are required")
        summary = render(
            args.binary,
            args.fixture,
            args.entrypoint,
            args.asset,
            args.output,
            args.expected_version,
            args.expected_commit,
        )
        print(canonical_json(summary).decode("utf-8"), end="")
        return 0
    except (FixtureError, OSError, subprocess.TimeoutExpired, ValueError) as error:
        print(f"render_api2_fixture: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
