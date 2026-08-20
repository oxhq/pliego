#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Prove the unavailable-but-discoverable API 2 executable foundation."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


REQUEST_MAX_BYTES = 1_048_576
INPUT_MANIFEST_MAX_BYTES = 16 * 1024 * 1024
PROBE_TIMEOUT_SECONDS = 180
INVOCATION_TIMEOUT_SECONDS = 30
SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
TARGET_COMPONENT = r"[a-z0-9]+(?:_[a-z0-9]+)*"
TARGET_RE = re.compile(rf"^{TARGET_COMPONENT}(?:-{TARGET_COMPONENT}){{2,3}}$")
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")


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

    if root["contracts"] != []:
        raise AssertionError("the executable foundation must not advertise an API 2 render tuple")
    invocation = exact_keys(
        root["invocation"],
        [
            "request_transport",
            "request_max_bytes",
            "job_root_transport",
            "input_manifest_max_bytes",
            "result_transport",
            "invocation_error_transport",
            "success_exit_code",
            "failed_exit_code",
            "invocation_error_exit_code",
        ],
        "probe.invocation",
    )
    expected_invocation = {
        "request_transport": "stdin-single-json",
        "request_max_bytes": REQUEST_MAX_BYTES,
        "job_root_transport": "cwd-v1",
        "input_manifest_max_bytes": INPUT_MANIFEST_MAX_BYTES,
        "result_transport": "stdout-single-json",
        "invocation_error_transport": "stderr-utf8-line",
        "success_exit_code": 0,
        "failed_exit_code": 1,
        "invocation_error_exit_code": 64,
    }
    if invocation != expected_invocation:
        raise AssertionError("probe invocation contract differs from API 2 framing")
    for key in [
        "request_max_bytes",
        "input_manifest_max_bytes",
        "success_exit_code",
        "failed_exit_code",
        "invocation_error_exit_code",
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
    completed = subprocess.run(
        [str(binary), "render-api2", *extra_args],
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=INVOCATION_TIMEOUT_SECONDS,
    )
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
        (
            "valid-but-unadvertised",
            valid_request,
            "no complete API 2 contract tuple is advertised",
            (),
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
            "contracts": [],
            "invocation": {
                "request_transport": "stdin-single-json",
                "request_max_bytes": REQUEST_MAX_BYTES,
                "job_root_transport": "cwd-v1",
                "input_manifest_max_bytes": INPUT_MANIFEST_MAX_BYTES,
                "result_transport": "stdout-single-json",
                "invocation_error_transport": "stderr-utf8-line",
                "success_exit_code": 0,
                "failed_exit_code": 1,
                "invocation_error_exit_code": 64,
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
        advertised["contracts"] = [{"api": 2}]
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
            if "must not advertise" not in str(error):
                raise
        else:
            raise AssertionError("self-test accepted a falsely advertised API 2 tuple")
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
            "valid-but-unadvertised",
        ]:
            raise AssertionError("self-test lost an executable framing proof case")
        if len(cases[7][1]) != REQUEST_MAX_BYTES or len(cases[8][1]) != REQUEST_MAX_BYTES + 1:
            raise AssertionError("self-test lost the inclusive request-size boundary")
        if PROBE_TIMEOUT_SECONDS <= INVOCATION_TIMEOUT_SECONDS:
            raise AssertionError("self-test lost the separate debug-binary probe budget")
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
    }
    write_proof(args.proof_output.resolve(), proof)
    print(f"API 2 executable foundation verified: {binary}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"API 2 executable contract check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
