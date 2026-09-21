#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Prove the advertised API 2 profile-null tuple and its executable closure."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


REQUEST_MAX_BYTES = 1_048_576
INPUT_MANIFEST_MAX_BYTES = 16 * 1024 * 1024
INPUT_CONTENT_MAX_BYTES = 64 * 1024 * 1024
PROBE_TIMEOUT_SECONDS = 180
INVOCATION_TIMEOUT_SECONDS = 30
WINDOWS_ACL_TOOL_TIMEOUT_SECONDS = 15
WINDOWS_ACL_PROOF_MAX_BYTES = 64 * 1024
SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
TARGET_COMPONENT = r"[a-z0-9]+(?:_[a-z0-9]+)*"
TARGET_RE = re.compile(rf"^{TARGET_COMPONENT}(?:-{TARGET_COMPONENT}){{2,3}}$")
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")
WINDOWS_SID_RE = re.compile(rb"(?<![A-Za-z0-9-])(S-1-(?:[0-9]+-)+[0-9]+)(?![A-Za-z0-9-])")
WINDOWS_LOCAL_ADMINISTRATOR_SID_RE = re.compile(r"^S-1-5-21-(?:[0-9]+-){3}500$")
WINDOWS_LOCAL_ACCOUNT_TOKEN_SID = "S-1-5-113"
WINDOWS_FIXED_SDDL_ALIASES = {
    "S-1-5-18": "SY",  # Local System
    "S-1-5-19": "LS",  # Local Service
    "S-1-5-20": "NS",  # Network Service
}


SUPPORTED_CONTRACT = {
    "api": 2,
    "input_manifest": {"schema": "pliego.input-manifest", "version": 1},
    "request": {"schema": "pliego.render-request", "version": 1},
    "result": {"schema": "pliego.render-result", "version": 1},
    "document_scene": {"schema": "pliego.document-scene", "version": 2},
    "bundle_manifest": {"schema": "pliego.bundle-manifest", "version": 1},
    "profiles": [],
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def sha256_bytes(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def canonical_json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"


def windows_user_sid(payload: bytes) -> str:
    matches = {match.group(1).decode("ascii") for match in WINDOWS_SID_RE.finditer(payload)}
    if len(matches) != 1:
        raise OSError("whoami.exe did not return exactly one current-user SID")
    return matches.pop()


def windows_token_has_sid(payload: bytes, expected_sid: str) -> bool:
    return expected_sid in {match.group(1).decode("ascii") for match in WINDOWS_SID_RE.finditer(payload)}


def windows_acl_descriptor(payload: bytes) -> str:
    if not payload or len(payload) % 2 != 0 or len(payload) > WINDOWS_ACL_PROOF_MAX_BYTES:
        raise OSError("icacls.exe returned an invalid or oversized ACL proof")
    try:
        text = payload.decode("utf-16-le")
    except UnicodeDecodeError as error:
        raise OSError("icacls.exe ACL proof is not UTF-16LE") from error
    descriptors = [line.strip() for line in text.lstrip("\ufeff").splitlines() if line.strip().startswith("D:")]
    if len(descriptors) != 1:
        raise OSError("icacls.exe ACL proof did not contain exactly one DACL")
    return descriptors[0]


def validate_windows_owner_only_dacl(payload: bytes, sid: str, *, local_account: bool = False) -> None:
    descriptor = windows_acl_descriptor(payload)
    match = re.fullmatch(
        r"D:P(?P<control>(?:(?:AI|AR))*)"
        r"\(A;(?P<flags>[A-Z]*);FA;;;(?P<trustee>S-1-(?:[0-9]+-)+[0-9]+|[A-Z]{2})\)",
        descriptor,
    )
    expected_trustees = {sid}
    fixed_alias = WINDOWS_FIXED_SDDL_ALIASES.get(sid)
    if fixed_alias is not None:
        expected_trustees.add(fixed_alias)
    if local_account and WINDOWS_LOCAL_ADMINISTRATOR_SID_RE.fullmatch(sid):
        # Windows writes the local Administrator SID as its canonical SDDL alias.
        expected_trustees.add("LA")
    if (
        match is None
        or match.group("trustee") not in expected_trustees
        or sorted(match.group("flags")[index : index + 2] for index in range(0, len(match.group("flags")), 2))
        != ["CI", "OI"]
    ):
        raise OSError("Windows job root does not have one protected owner-only full-access DACL")


def windows_system_tool(name: str) -> Path:
    system_root = os.environ.get("SystemRoot")
    if not system_root or not re.fullmatch(r"[A-Za-z]:[\\/].+", system_root):
        raise OSError("SystemRoot does not identify an absolute Windows directory")
    candidate = Path(system_root) / "System32" / name
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise OSError(f"required Windows ACL tool is unavailable: {name}") from error
    if not resolved.is_file() or resolved.name.lower() != name.lower() or resolved.suffix.lower() != ".exe":
        raise OSError(f"required Windows ACL tool is not a native executable: {name}")
    return resolved


def run_windows_acl_tool(arguments: list[str], operation: str) -> bytes:
    try:
        completed = subprocess.run(
            arguments,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            shell=False,
            timeout=WINDOWS_ACL_TOOL_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise OSError(f"Windows API 2 job-root {operation} could not run") from error
    if completed.returncode != 0:
        diagnostic = (
            completed.stderr[:512].decode("utf-8", errors="backslashreplace").replace("\r", "\\r").replace("\n", "\\n")
        )
        raise OSError(
            f"Windows API 2 job-root {operation} failed with exit {completed.returncode}: {diagnostic or '<empty stderr>'}"
        )
    return completed.stdout


def create_private_job_root(path: Path) -> None:
    path.mkdir(mode=0o700)
    try:
        if os.name != "nt":
            path.chmod(0o700)
            if stat.S_IMODE(path.stat().st_mode) != 0o700:
                raise OSError("API 2 job root does not have Unix mode 0700")
            return

        whoami = windows_system_tool("whoami.exe")
        icacls = windows_system_tool("icacls.exe")
        sid = windows_user_sid(run_windows_acl_tool([str(whoami), "/user", "/fo", "csv", "/nh"], "current-user lookup"))
        local_account = False
        if WINDOWS_LOCAL_ADMINISTRATOR_SID_RE.fullmatch(sid):
            local_account = windows_token_has_sid(
                run_windows_acl_tool(
                    [str(whoami), "/groups", "/fo", "csv", "/nh"],
                    "current-token group lookup",
                ),
                WINDOWS_LOCAL_ACCOUNT_TOKEN_SID,
            )
        run_windows_acl_tool(
            [str(icacls), str(path), "/setowner", f"*{sid}", "/q"],
            "owner assignment",
        )
        run_windows_acl_tool(
            [str(icacls), str(path), "/reset", "/q"],
            "DACL reset",
        )
        run_windows_acl_tool(
            [str(icacls), str(path), "/inheritance:r", "/q"],
            "DACL inheritance removal",
        )
        run_windows_acl_tool(
            [str(icacls), str(path), "/grant:r", f"*{sid}:(OI)(CI)F", "/q"],
            "owner-only DACL assignment",
        )
        descriptor_fd, descriptor_name = tempfile.mkstemp(
            prefix=".pliego-api2-acl-",
            suffix=".txt",
            dir=path.parent,
        )
        os.close(descriptor_fd)
        descriptor_path = Path(descriptor_name)
        try:
            run_windows_acl_tool(
                [str(icacls), str(path), "/save", str(descriptor_path), "/q"],
                "DACL verification",
            )
            validate_windows_owner_only_dacl(
                descriptor_path.read_bytes(),
                sid,
                local_account=local_account,
            )
        finally:
            descriptor_path.unlink(missing_ok=True)
        if list(path.iterdir()):
            raise OSError("Windows API 2 job root changed before input staging")
    except BaseException:
        try:
            path.rmdir()
        except OSError:
            pass
        raise


def exact_keys(value: Any, expected: list[str], name: str) -> dict[str, Any]:
    if not isinstance(value, dict) or list(value) != expected:
        raise AssertionError(f"{name} keys must be exactly {expected!r}")
    return value


def require_int(value: Any, expected: int, name: str) -> None:
    if type(value) is not int or value != expected:
        raise AssertionError(f"{name} must be the integer {expected}")


def validate_probe(
    probe: Any,
    *,
    binary_sha256: str,
    expected_version: str,
    expected_source_commit: str,
    expected_target: str,
    expected_servo_base: str,
) -> None:
    root = exact_keys(
        probe,
        ["schema", "version", "engine", "contracts", "invocation"],
        "probe",
    )
    if root["schema"] != "pliego.runtime-contract":
        raise AssertionError("probe schema identity is not runtime-contract v1")
    require_int(root["version"], 1, "probe.version")

    engine = exact_keys(
        root["engine"],
        ["name", "version", "api", "source_commit", "runtime"],
        "probe.engine",
    )
    if engine != {
        "name": "pliego",
        "version": expected_version,
        "api": 2,
        "source_commit": expected_source_commit,
        "runtime": engine["runtime"],
    }:
        raise AssertionError("probe engine identity differs from the expected executable")
    require_int(engine["api"], 2, "probe.engine.api")
    if not VERSION_RE.fullmatch(engine["version"]):
        raise AssertionError("probe engine version is not canonical SemVer")
    if not COMMIT_RE.fullmatch(engine["source_commit"]):
        raise AssertionError("probe source commit is not full lowercase Git identity")

    runtime = exact_keys(
        engine["runtime"],
        ["mode", "target", "binary_sha256", "servo_base"],
        "probe.engine.runtime",
    )
    if runtime != {
        "mode": "one-shot",
        "target": expected_target,
        "binary_sha256": binary_sha256,
        "servo_base": expected_servo_base,
    }:
        raise AssertionError("probe runtime identity differs from the packaged binary")
    if not TARGET_RE.fullmatch(runtime["target"]):
        raise AssertionError("probe target triple is not canonical")
    if not SHA256_RE.fullmatch(runtime["binary_sha256"]):
        raise AssertionError("probe binary hash is not canonical")
    if not COMMIT_RE.fullmatch(runtime["servo_base"]):
        raise AssertionError("probe Servo base is not full lowercase Git identity")

    if root["contracts"] != [SUPPORTED_CONTRACT]:
        raise AssertionError("the executable must advertise exactly the supported profile-null tuple")
    invocation = exact_keys(
        root["invocation"],
        [
            "request_transport",
            "request_max_bytes",
            "job_root_transport",
            "input_manifest_max_bytes",
            "input_content_max_bytes",
            "result_transport",
            "invocation_error_transport",
            "transport_error_transport",
            "success_exit_code",
            "failed_exit_code",
            "invocation_error_exit_code",
            "transport_error_exit_code",
        ],
        "probe.invocation",
    )
    expected_invocation = {
        "request_transport": "stdin-single-json",
        "request_max_bytes": REQUEST_MAX_BYTES,
        "job_root_transport": "cwd-v1",
        "input_manifest_max_bytes": INPUT_MANIFEST_MAX_BYTES,
        "input_content_max_bytes": INPUT_CONTENT_MAX_BYTES,
        "result_transport": "stdout-single-json",
        "invocation_error_transport": "stderr-utf8-line",
        "transport_error_transport": "stderr-utf8-line",
        "success_exit_code": 0,
        "failed_exit_code": 1,
        "invocation_error_exit_code": 64,
        "transport_error_exit_code": 74,
    }
    if invocation != expected_invocation:
        raise AssertionError("probe invocation contract differs from API 2 framing")
    for key in [
        "request_max_bytes",
        "input_manifest_max_bytes",
        "input_content_max_bytes",
        "success_exit_code",
        "failed_exit_code",
        "invocation_error_exit_code",
        "transport_error_exit_code",
    ]:
        require_int(invocation[key], expected_invocation[key], f"probe.invocation.{key}")


def run_probe(binary: Path) -> tuple[dict[str, Any], bytes]:
    completed = subprocess.run(
        [str(binary), "--contract-probe"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        # Debug Servo executables are hundreds of MiB. The probe hashes its own
        # bytes before emitting identity, so give that bounded operation a
        # separate budget from ordinary request framing.
        timeout=PROBE_TIMEOUT_SECONDS,
    )
    if completed.returncode != 0 or completed.stderr != b"":
        raise AssertionError(f"contract probe failed: exit={completed.returncode}, stderr={completed.stderr!r}")
    if completed.stdout.startswith(b"\xef\xbb\xbf"):
        raise AssertionError("contract probe stdout contains a UTF-8 BOM")
    try:
        probe = json.loads(completed.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AssertionError("contract probe did not emit one UTF-8 JSON value") from error
    if canonical_json(probe) != completed.stdout:
        raise AssertionError("contract probe stdout is not canonical compact JSON plus one LF")
    if not isinstance(probe, dict):
        raise AssertionError("contract probe root must be an object")
    return probe, completed.stdout


def assert_invocation_error(
    binary: Path,
    name: str,
    payload: bytes,
    expected_diagnostic: str,
    *extra_args: str,
) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            [str(binary), "render-api2", *extra_args],
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=INVOCATION_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raise AssertionError(
            f"{name}: invocation framing did not terminate within {INVOCATION_TIMEOUT_SECONDS} seconds"
        ) from error
    if completed.returncode != 64:
        raise AssertionError(f"{name}: expected exit 64, got {completed.returncode}")
    if completed.stdout != b"":
        raise AssertionError(f"{name}: invocation error wrote stdout")
    try:
        diagnostic = completed.stderr.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AssertionError(f"{name}: diagnostic is not UTF-8") from error
    if (
        not diagnostic.endswith("\n")
        or diagnostic.endswith("\n\n")
        or "\r" in diagnostic
        or "\n" in diagnostic[:-1]
        or diagnostic[:-1] == ""
    ):
        raise AssertionError(f"{name}: diagnostic must be exactly one nonempty LF-terminated line")
    if expected_diagnostic not in diagnostic:
        raise AssertionError(f"{name}: diagnostic does not prove the expected decoder branch: {diagnostic!r}")
    return {
        "name": name,
        "arguments": ["render-api2", *extra_args],
        "input_bytes": len(payload),
        "input_sha256": sha256_bytes(payload),
        "exit_code": completed.returncode,
        "expected_diagnostic": expected_diagnostic,
        "stderr_sha256": sha256_bytes(completed.stderr),
    }


def verify_artifact(job_root: Path, descriptor: Any, expected_path: str | None = None) -> Path:
    artifact = exact_keys(
        descriptor,
        ["path", "media_type", "sha256", "bytes"],
        f"artifact {expected_path or '<dynamic>'}",
    )
    if expected_path is not None and artifact["path"] != expected_path:
        raise AssertionError(f"artifact path must be {expected_path!r}")
    if not isinstance(artifact["path"], str) or not isinstance(artifact["media_type"], str):
        raise AssertionError("artifact path and media type must be strings")
    if not SHA256_RE.fullmatch(artifact["sha256"]):
        raise AssertionError("artifact hash is not canonical SHA-256")
    if type(artifact["bytes"]) is not int or artifact["bytes"] < 1:
        raise AssertionError("artifact byte count must be a positive integer")
    path = (job_root / artifact["path"]).resolve(strict=True)
    if job_root.resolve() not in path.parents:
        raise AssertionError("artifact path escaped the cwd-v1 job root")
    if not path.is_file():
        raise AssertionError(f"artifact is not a regular file: {path}")
    if path.stat().st_size != artifact["bytes"] or sha256_file(path) != artifact["sha256"]:
        raise AssertionError(f"artifact descriptor does not bind the published bytes: {path}")
    return path


def assert_render_success(
    binary: Path,
    payload: bytes,
    fixture_root: Path,
    job_root: Path,
    probe: dict[str, Any],
) -> dict[str, Any]:
    create_private_job_root(job_root)
    shutil.copy2(fixture_root / "input-manifest.json", job_root / "input-manifest.json")
    shutil.copytree(fixture_root / "input", job_root / "input")
    completed = subprocess.run(
        [str(binary), "render-api2"],
        cwd=job_root,
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=PROBE_TIMEOUT_SECONDS,
    )
    if completed.returncode != 0 or completed.stderr != b"":
        raise AssertionError(f"valid API 2 render failed: exit={completed.returncode}, stderr={completed.stderr!r}")
    try:
        result = json.loads(completed.stdout)
        request = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AssertionError("valid API 2 render did not emit one UTF-8 JSON result") from error
    if canonical_json(result) != completed.stdout:
        raise AssertionError("render result stdout is not canonical compact JSON plus one LF")
    root = exact_keys(
        result,
        [
            "schema",
            "version",
            "api",
            "status",
            "request",
            "engine",
            "delivery",
            "conformance",
            "diagnostics",
            "error",
        ],
        "render result",
    )
    if root["schema"] != "pliego.render-result" or root["status"] != "success":
        raise AssertionError("valid request did not produce a successful RenderResult v1")
    require_int(root["version"], 1, "render result.version")
    require_int(root["api"], 2, "render result.api")
    if root["request"] != request or root["engine"] != probe["engine"] or root["error"] is not None:
        raise AssertionError("render result lost the accepted request or exact engine identity")
    if root["conformance"] != {
        "requested": None,
        "status": "not-requested",
        "evidence": None,
    }:
        raise AssertionError("profile-null result has an invalid conformance disposition")

    delivery = exact_keys(root["delivery"], ["pdf", "scene", "bundle"], "render result.delivery")
    delivery_root = job_root / "delivery"
    pdf_path = verify_artifact(delivery_root, delivery["pdf"], "document.pdf")
    scene_path = verify_artifact(delivery_root, delivery["scene"], "scene.json")
    bundle_path = verify_artifact(delivery_root, delivery["bundle"], "bundle.json")
    if not pdf_path.read_bytes().startswith(b"%PDF-"):
        raise AssertionError("published document.pdf has no PDF header")
    scene = json.loads(scene_path.read_bytes())
    if scene.get("schema") != "pliego.document-scene" or scene.get("version") != 2:
        raise AssertionError("published scene is not DocumentScene v2")

    bundle_bytes = bundle_path.read_bytes()
    bundle = json.loads(bundle_bytes)
    if canonical_json(bundle) != bundle_bytes:
        raise AssertionError("bundle manifest is not canonical compact JSON plus one LF")
    bundle_root = exact_keys(bundle, ["schema", "version", "entries"], "bundle manifest")
    if bundle_root["schema"] != "pliego.bundle-manifest":
        raise AssertionError("published bundle is not bundle-manifest v1")
    require_int(bundle_root["version"], 1, "bundle manifest.version")
    entries = bundle_root["entries"]
    if not isinstance(entries, list) or not entries:
        raise AssertionError("bundle manifest has no delivery entries")
    entry_paths = [entry.get("path") for entry in entries if isinstance(entry, dict)]
    if len(entry_paths) != len(entries) or entry_paths != sorted(entry_paths) or len(set(entry_paths)) != len(entries):
        raise AssertionError("bundle entries are not unique canonical path order")
    if "bundle.json" in entry_paths or "document.pdf" not in entry_paths or "scene.json" not in entry_paths:
        raise AssertionError("bundle manifest does not have the required self-excluding closure")
    for entry in entries:
        verify_artifact(delivery_root, entry)

    diagnostics = exact_keys(root["diagnostics"], ["retained", "artifacts"], "render result.diagnostics")
    if diagnostics["retained"] is not True or not diagnostics["artifacts"]:
        raise AssertionError("the always-retained request did not publish diagnostic evidence")
    for artifact in diagnostics["artifacts"]:
        verify_artifact(job_root, artifact)
    if {path.name for path in job_root.iterdir()} != {
        "input-manifest.json",
        "input",
        "delivery",
        "diagnostics",
    }:
        raise AssertionError("render left an unexpected public job-root entry")
    return {
        "exit_code": completed.returncode,
        "stdout_sha256": sha256_bytes(completed.stdout),
        "result": result,
        "bundle_sha256": sha256_bytes(bundle_bytes),
    }


def build_execution_fixture(
    repository_root: Path,
    root: Path,
    request_template: bytes,
) -> tuple[bytes, Path]:
    font_source = repository_root / "ports" / "pliego" / "tests" / "fixtures" / "text-scene" / "Ahem.ttf"
    if not font_source.is_file():
        raise AssertionError("renderer-backed API 2 fixture font is missing")
    fixture_root = root / "source"
    input_root = fixture_root / "input"
    input_root.mkdir(parents=True)
    bodies = {
        "document.html": (
            b'<!doctype html><html lang="en-US"><head><meta charset="utf-8">'
            b'<link rel="stylesheet" href="styles.css"><title>API 2 executable fixture</title>'
            b"</head><body><p>API 2</p></body></html>\n"
        ),
        "font.ttf": font_source.read_bytes(),
        "styles.css": (
            b'@font-face{font-family:Ahem;src:url("font.ttf") format("truetype");}'
            b"html,body{margin:0}body{font-family:Ahem;font-size:16px;color:#000}\n"
        ),
    }
    media_types = {
        "document.html": "text/html;charset=utf-8",
        "font.ttf": "application/octet-stream",
        "styles.css": "text/css;charset=utf-8",
    }
    entries = []
    for path, body in bodies.items():
        (input_root / path).write_bytes(body)
        entries.append(
            {
                "path": path,
                "media_type": media_types[path],
                "sha256": sha256_bytes(body),
                "bytes": len(body),
            }
        )
    manifest_bytes = canonical_json(
        {
            "schema": "pliego.input-manifest",
            "version": 1,
            "url_root": "pliego-input:///",
            "entries": entries,
        }
    )
    (fixture_root / "input-manifest.json").write_bytes(manifest_bytes)
    request = json.loads(request_template)
    request["input"] = {
        "entrypoint": "document.html",
        "manifest": {
            "path": "input-manifest.json",
            "media_type": "application/vnd.pliego.input-manifest+json",
            "sha256": sha256_bytes(manifest_bytes),
            "bytes": len(manifest_bytes),
        },
    }
    return canonical_json(request), fixture_root


def framing_cases(valid_request: bytes) -> list[tuple[str, bytes, str, tuple[str, ...]]]:
    return [
        ("empty", b"", "stdin is empty", ()),
        ("bom", b"\xef\xbb\xbf{}\n", "UTF-8 BOM", ()),
        (
            "duplicate-member",
            b'{"schema":"a","schema":"b"}\n',
            "duplicate JSON object member",
            (),
        ),
        ("floating-point", b'{"value":1.0}\n', "floating-point JSON numbers", ()),
        ("negative-zero", b'{"value":-0}\n', "negative zero", ()),
        ("invalid-utf8", b"\xff", "invalid request JSON", ()),
        ("trailing-json", b"{}\n{}\n", "invalid request framing", ()),
        (
            "at-limit-invalid-json",
            b" " * REQUEST_MAX_BYTES,
            "invalid request JSON",
            (),
        ),
        (
            "over-limit",
            b" " * (REQUEST_MAX_BYTES + 1),
            "exceeds request_max_bytes",
            (),
        ),
        (
            "extra-argument",
            b"",
            "accepts no command-line options or paths",
            ("--unexpected",),
        ),
    ]


def write_proof(path: Path, proof: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        raise AssertionError(f"proof output already exists: {path}")
    path.write_bytes(canonical_json(proof))


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="pliego-api2-check-") as temporary:
        binary = Path(temporary) / "pliego"
        binary.write_bytes(b"synthetic executable bytes")
        binary_sha256 = sha256_file(binary)
        probe = {
            "schema": "pliego.runtime-contract",
            "version": 1,
            "engine": {
                "name": "pliego",
                "version": "0.1.1",
                "api": 2,
                "source_commit": "1" * 40,
                "runtime": {
                    "mode": "one-shot",
                    "target": "x86_64-unknown-linux-gnu",
                    "binary_sha256": binary_sha256,
                    "servo_base": "2" * 40,
                },
            },
            "contracts": [SUPPORTED_CONTRACT],
            "invocation": {
                "request_transport": "stdin-single-json",
                "request_max_bytes": REQUEST_MAX_BYTES,
                "job_root_transport": "cwd-v1",
                "input_manifest_max_bytes": INPUT_MANIFEST_MAX_BYTES,
                "input_content_max_bytes": INPUT_CONTENT_MAX_BYTES,
                "result_transport": "stdout-single-json",
                "invocation_error_transport": "stderr-utf8-line",
                "transport_error_transport": "stderr-utf8-line",
                "success_exit_code": 0,
                "failed_exit_code": 1,
                "invocation_error_exit_code": 64,
                "transport_error_exit_code": 74,
            },
        }
        validate_probe(
            probe,
            binary_sha256=binary_sha256,
            expected_version="0.1.1",
            expected_source_commit="1" * 40,
            expected_target="x86_64-unknown-linux-gnu",
            expected_servo_base="2" * 40,
        )
        advertised = json.loads(json.dumps(probe))
        advertised["contracts"] = []
        try:
            validate_probe(
                advertised,
                binary_sha256=binary_sha256,
                expected_version="0.1.1",
                expected_source_commit="1" * 40,
                expected_target="x86_64-unknown-linux-gnu",
                expected_servo_base="2" * 40,
            )
        except AssertionError as error:
            if "exactly the supported" not in str(error):
                raise
        else:
            raise AssertionError("self-test accepted a missing API 2 tuple")
        wrong_job_root = json.loads(json.dumps(probe))
        wrong_job_root["invocation"]["job_root_transport"] = "argument-v1"
        try:
            validate_probe(
                wrong_job_root,
                binary_sha256=binary_sha256,
                expected_version="0.1.1",
                expected_source_commit="1" * 40,
                expected_target="x86_64-unknown-linux-gnu",
                expected_servo_base="2" * 40,
            )
        except AssertionError as error:
            if "invocation contract differs" not in str(error):
                raise
        else:
            raise AssertionError("self-test accepted a different API 2 job-root transport")
        wrong_manifest_limit = json.loads(json.dumps(probe))
        wrong_manifest_limit["invocation"]["input_manifest_max_bytes"] -= 1
        try:
            validate_probe(
                wrong_manifest_limit,
                binary_sha256=binary_sha256,
                expected_version="0.1.1",
                expected_source_commit="1" * 40,
                expected_target="x86_64-unknown-linux-gnu",
                expected_servo_base="2" * 40,
            )
        except AssertionError as error:
            if "invocation contract differs" not in str(error):
                raise
        else:
            raise AssertionError("self-test accepted a different input-manifest byte limit")
        wrong_content_limit = json.loads(json.dumps(probe))
        wrong_content_limit["invocation"]["input_content_max_bytes"] -= 1
        try:
            validate_probe(
                wrong_content_limit,
                binary_sha256=binary_sha256,
                expected_version="0.1.1",
                expected_source_commit="1" * 40,
                expected_target="x86_64-unknown-linux-gnu",
                expected_servo_base="2" * 40,
            )
        except AssertionError as error:
            if "invocation contract differs" not in str(error):
                raise
        else:
            raise AssertionError("self-test accepted a different aggregate input-content byte limit")
        wrong_transport_exit = json.loads(json.dumps(probe))
        wrong_transport_exit["invocation"]["transport_error_exit_code"] = 75
        try:
            validate_probe(
                wrong_transport_exit,
                binary_sha256=binary_sha256,
                expected_version="0.1.1",
                expected_source_commit="1" * 40,
                expected_target="x86_64-unknown-linux-gnu",
                expected_servo_base="2" * 40,
            )
        except AssertionError as error:
            if "invocation contract differs" not in str(error):
                raise
        else:
            raise AssertionError("self-test accepted a different transport-error exit code")
        boolean_version = json.loads(json.dumps(probe))
        boolean_version["version"] = True
        try:
            validate_probe(
                boolean_version,
                binary_sha256=binary_sha256,
                expected_version="0.1.1",
                expected_source_commit="1" * 40,
                expected_target="x86_64-unknown-linux-gnu",
                expected_servo_base="2" * 40,
            )
        except AssertionError as error:
            if "must be the integer 1" not in str(error):
                raise
        else:
            raise AssertionError("self-test accepted a Boolean as an integer probe version")
        cases = framing_cases(b"{}\n")
        if [case[0] for case in cases] != [
            "empty",
            "bom",
            "duplicate-member",
            "floating-point",
            "negative-zero",
            "invalid-utf8",
            "trailing-json",
            "at-limit-invalid-json",
            "over-limit",
            "extra-argument",
        ]:
            raise AssertionError("self-test lost an executable framing proof case")
        if len(cases[7][1]) != REQUEST_MAX_BYTES or len(cases[8][1]) != REQUEST_MAX_BYTES + 1:
            raise AssertionError("self-test lost the inclusive request-size boundary")
        if PROBE_TIMEOUT_SECONDS <= INVOCATION_TIMEOUT_SECONDS:
            raise AssertionError("self-test lost the separate debug-binary probe budget")
        sid = "S-1-5-21-4080267330-3575100508-2019971957-1001"
        if windows_user_sid(f'"DOMAIN\\user","{sid}"\r\n'.encode()) != sid:
            raise AssertionError("self-test did not parse one current-user SID")
        if not windows_token_has_sid(b'"Local account","S-1-5-113"\r\n', WINDOWS_LOCAL_ACCOUNT_TOKEN_SID):
            raise AssertionError("self-test did not recognize the local-account token SID")
        if windows_token_has_sid(b'"Domain Users","S-1-5-21-1-2-3-513"\r\n', WINDOWS_LOCAL_ACCOUNT_TOKEN_SID):
            raise AssertionError("self-test misclassified a domain-only token as a local account")
        valid_acl = f"fixture\r\nD:PAI(A;OICI;FA;;;{sid})\r\n".encode("utf-16-le")
        validate_windows_owner_only_dacl(valid_acl, sid)
        system_acl = "fixture\r\nD:PAI(A;OICI;FA;;;SY)\r\n".encode("utf-16-le")
        validate_windows_owner_only_dacl(system_acl, "S-1-5-18")
        administrator_sid = "S-1-5-21-51256336-3298027356-2228789493-500"
        administrator_acl = "fixture\r\nD:PAI(A;OICI;FA;;;LA)\r\n".encode("utf-16-le")
        validate_windows_owner_only_dacl(administrator_acl, administrator_sid, local_account=True)
        invalid_acls = [
            f"fixture\r\nD:AI(A;OICI;FA;;;{sid})\r\n".encode("utf-16-le"),
            f"fixture\r\nD:PAI(A;OICI;FA;;;{sid})(A;OICI;FA;;;S-1-5-18)\r\n".encode("utf-16-le"),
            f"fixture\r\nD:PAI(A;OI;FA;;;{sid})\r\n".encode("utf-16-le"),
            "fixture\r\nD:PAI(A;OICI;FA;;;LA)\r\n".encode("utf-16-le"),
        ]
        for invalid_acl in invalid_acls:
            try:
                validate_windows_owner_only_dacl(invalid_acl, sid)
            except OSError:
                pass
            else:
                raise AssertionError("self-test accepted a non-owner-only Windows DACL")
        try:
            validate_windows_owner_only_dacl(administrator_acl, administrator_sid)
        except OSError:
            pass
        else:
            raise AssertionError("self-test accepted LA for a nonlocal Administrator SID")
        try:
            windows_user_sid(b'"DOMAIN\\user","not-a-sid"\r\n')
        except OSError:
            pass
        else:
            raise AssertionError("self-test accepted a missing Windows user SID")
        private_root = Path(temporary) / "private-job-root"
        create_private_job_root(private_root)
        if list(private_root.iterdir()):
            raise AssertionError("private job-root creation introduced public entries")
        if os.name != "nt" and stat.S_IMODE(private_root.stat().st_mode) != 0o700:
            raise AssertionError("self-test lost Unix mode 0700")
        private_root.rmdir()
    print("API 2 executable contract checker self-test: ok")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-source-commit")
    parser.add_argument("--expected-target")
    parser.add_argument("--expected-servo-base")
    parser.add_argument("--valid-request", type=Path)
    parser.add_argument("--proof-output", type=Path)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--bundle-version-file", type=Path)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return

    required = {
        "--binary": args.binary,
        "--expected-version": args.expected_version,
        "--expected-source-commit": args.expected_source_commit,
        "--expected-target": args.expected_target,
        "--expected-servo-base": args.expected_servo_base,
        "--valid-request": args.valid_request,
        "--proof-output": args.proof_output,
    }
    missing = [name for name, value in required.items() if value is None]
    if missing:
        parser.error(f"required outside --self-test: {', '.join(missing)}")

    binary = args.binary.resolve(strict=True)
    valid_request_path = args.valid_request.resolve(strict=True)
    if not binary.is_file() or not valid_request_path.is_file():
        raise AssertionError("binary and valid request must be regular files")
    if (args.archive is None) != (args.bundle_version_file is None):
        raise AssertionError("archive and bundle VERSION must be supplied together")
    archive = args.archive.resolve(strict=True) if args.archive is not None else None
    bundle_version_file = (
        args.bundle_version_file.resolve(strict=True) if args.bundle_version_file is not None else None
    )
    if archive is not None and (
        not archive.is_file() or bundle_version_file is None or not bundle_version_file.is_file()
    ):
        raise AssertionError("archive and bundle VERSION must be regular files")
    if not COMMIT_RE.fullmatch(args.expected_source_commit):
        raise AssertionError("expected source commit must be 40 lowercase hex characters")
    if not TARGET_RE.fullmatch(args.expected_target):
        raise AssertionError("expected target must be a canonical target triple")
    if not COMMIT_RE.fullmatch(args.expected_servo_base):
        raise AssertionError("expected Servo base must be 40 lowercase hex characters")

    binary_sha256 = sha256_file(binary)
    probe, probe_bytes = run_probe(binary)
    validate_probe(
        probe,
        binary_sha256=binary_sha256,
        expected_version=args.expected_version,
        expected_source_commit=args.expected_source_commit,
        expected_target=args.expected_target,
        expected_servo_base=args.expected_servo_base,
    )
    valid_request = valid_request_path.read_bytes()
    cases = [
        assert_invocation_error(binary, name, payload, diagnostic, *extra_args)
        for name, payload, diagnostic, extra_args in framing_cases(valid_request)
    ]
    with tempfile.TemporaryDirectory(prefix="pliego-api2-execute-") as temporary:
        execution_request, fixture_root = build_execution_fixture(
            valid_request_path.parents[4],
            Path(temporary),
            valid_request,
        )
        execution = assert_render_success(
            binary,
            execution_request,
            fixture_root,
            Path(temporary) / "job",
            probe,
        )
    archive_evidence = None
    if archive is not None and bundle_version_file is not None:
        archive_evidence = {
            "name": archive.name,
            "sha256": sha256_file(archive),
            "bundle_version_file_sha256": sha256_file(bundle_version_file),
            "bundle_version": bundle_version_file.read_text(encoding="utf-8").strip(),
        }
    proof = {
        "schema": "pliego.api2-executable-proof",
        "version": 1,
        "archive": archive_evidence,
        "binary_sha256": binary_sha256,
        "probe_sha256": sha256_bytes(probe_bytes),
        "probe": probe,
        "invocation_cases": cases,
        "execution": execution,
    }
    write_proof(args.proof_output.resolve(), proof)
    print(f"API 2 advertised tuple and executable closure verified: {binary}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"API 2 executable contract check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
