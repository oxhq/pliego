#!/usr/bin/env python3
"""Check the production WebCrypto host policy with synthetic, offline keys."""

from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from check_api2_contract import (
    build_execution_fixture,
    canonical_json,
    create_private_job_root,
    run_probe,
    sha256_bytes,
    sha256_file,
    validate_probe,
    verify_artifact,
)

REPOSITORY = Path(__file__).resolve().parents[3]
FIXTURE = REPOSITORY / "ports/pliego/tests/fixtures/webcrypto-policy/index.html"
COMMON = ["secure-context", "crypto-random-values", "servo-internals-absent-top-level"]
SUPPORTS = ["supports-private-rsa", "supports-unwrap-overloads", "supports-public-rsa", "supports-key-handling"]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def expected_payload(secure: bool, blocked: bool) -> dict:
    checks = list(COMMON)
    if not secure:
        checks.append("subtle-absent-insecure")
    else:
        checks.append("subtle-exposed")
        checks += [
            "digest-sha256",
            "aes-gcm-roundtrip",
            "public-rsa-oaep-encrypt",
            "public-rsa-pkcs1-verify",
            "public-rsa-pss-verify",
            *SUPPORTS,
        ]
        for label in ("oaep", "pkcs1", "pss"):
            checks.append(f"{label}-key-handling")
            for source in ("generated", "imported", "cloned"):
                if label == "oaep":
                    checks += [f"oaep-decrypt-{source}", f"oaep-unwrap-{source}"]
                else:
                    checks.append(f"{label}-sign-{source}")
    return {
        "fixture": "webcrypto-policy-v1",
        "secureContext": secure,
        "privateRsaBlocked": blocked,
        "supportsAvailable": secure,
        "checks": checks,
    }


def check_payload(payload: object, secure: bool, blocked: bool) -> None:
    # Pinned Servo exposes supports(); silently skipping it would weaken the oracle.
    require(payload == expected_payload(secure, blocked), f"incomplete/wrong WebCrypto evidence: {payload!r}")


def html(secure: bool, blocked: bool) -> str:
    value = FIXTURE.read_text(encoding="utf-8")
    for token, expected in (("__EXPECT_PRIVATE_RSA_BLOCKED__", blocked), ("__EXPECT_SECURE_CONTEXT__", secure)):
        require(value.count(token) == 1, f"missing/duplicate fixture token {token}")
        value = value.replace(token, str(expected).lower())
    # Nested browsing-context execution is outside the production controlled
    # profile. The native realtime DocumentSession test exercises srcdoc exposure.
    require(value.count("__CHECK_SRCDOC__") == 1, "missing/duplicate srcdoc fixture token")
    value = value.replace("__CHECK_SRCDOC__", "false")
    return value


def api2_fixture(root: Path, blocked: bool) -> tuple[bytes, Path]:
    template = (REPOSITORY / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes()
    payload, source = build_execution_fixture(REPOSITORY, root, template)
    (source / "input/document.html").write_text(html(False, blocked), encoding="utf-8", newline="\n")
    manifest = json.loads((source / "input-manifest.json").read_bytes())
    for entry in manifest["entries"]:
        path = source / "input" / entry["path"]
        entry.update(sha256=sha256_file(path), bytes=path.stat().st_size)
    manifest_bytes = canonical_json(manifest)
    (source / "input-manifest.json").write_bytes(manifest_bytes)
    request = json.loads(payload)
    request["input"]["manifest"].update(sha256=sha256_bytes(manifest_bytes), bytes=len(manifest_bytes))
    request["diagnostics"]["retention"] = "always"
    return canonical_json(request), source


def execute(command: list[str], cwd: Path, root: Path, timeout: int, payload: bytes | None = None):
    started = time.monotonic()
    process_info = {"command": command, "limit_seconds": timeout, "outcome": "failed"}
    try:
        result = subprocess.run(command, cwd=cwd, input=payload, capture_output=True, timeout=timeout, check=False)
        (root / "stdout.json").write_bytes(result.stdout)
        (root / "stderr.txt").write_bytes(result.stderr)
        process_info.update(exit_code=result.returncode, outcome="exited")
        return result
    except subprocess.TimeoutExpired as error:
        (root / "stdout.json").write_bytes(error.stdout or b"")
        (root / "stderr.txt").write_bytes(error.stderr or b"")
        process_info["outcome"] = "host-timeout"
        raise
    finally:
        process_info["wall_seconds"] = time.monotonic() - started
        (root / "process.json").write_bytes(canonical_json(process_info))


def run_case(binary: Path, root: Path, command: str, blocked: bool, probe: dict, timeout: int) -> dict:
    root.mkdir()
    if command == "render-api2":
        payload, source = api2_fixture(root, blocked)
        (root / "request.json").write_bytes(payload)
        job = root / "job"
        create_private_job_root(job)
        shutil.copy2(source / "input-manifest.json", job / "input-manifest.json")
        shutil.copytree(source / "input", job / "input")
        process = execute([str(binary), command], job, root, timeout, payload)
        require(not process.stderr, "API 2 emitted stderr")
        result = json.loads(process.stdout)
        require(canonical_json(result) == process.stdout, "noncanonical API 2 result")
        require(
            result.get("engine") == probe["engine"] and result.get("request") == json.loads(payload),
            "API 2 executable/request identity changed",
        )
        require(
            process.returncode == 0 and result.get("status") == "success" and result.get("error") is None,
            f"API 2 fixture failed: {result.get('error')}",
        )
        diagnostics = result["diagnostics"]
        require(diagnostics["retained"] and len(diagnostics["artifacts"]) == 1, "missing readiness diagnostics")
        diagnostic = verify_artifact(job, diagnostics["artifacts"][0])
        ready = json.loads(diagnostic.read_bytes())["readiness"]
        require(ready["status"] == "ready", "API 2 fixture did not become ready")
        check_payload(ready["payload"], False, blocked)
        delivery = job / "delivery"
        for kind, filename in (("pdf", "document.pdf"), ("scene", "scene.json"), ("bundle", "bundle.json")):
            verify_artifact(delivery, result["delivery"][kind], filename)
        for item in json.loads((delivery / "bundle.json").read_bytes())["entries"]:
            verify_artifact(delivery, item)
        pdf = delivery / "document.pdf"
        evidence = ready["payload"]
    else:
        (root / "input.html").write_text(html(True, blocked), encoding="utf-8", newline="\n")
        pdf = root / "document.pdf"
        process = execute(
            [
                str(binary),
                command,
                "input.html",
                "--output",
                str(pdf),
                "--artifacts",
                str(root / "artifacts"),
                "--page-size",
                "240x160",
                "--page-margins",
                "0,0,0,0",
            ],
            root,
            root,
            timeout,
        )
        result = json.loads(process.stdout)
        require(process.returncode == 0 and result.get("status") == "rendered", f"API 1 fixture failed: {result!r}")
        evidence = result.get("readiness")
        check_payload(evidence, True, blocked)
        ready = json.loads((root / "artifacts/readiness.json").read_bytes())
        require(
            ready["status"] == "ready" and ready["payload"] == evidence and ready["render_id"] == result["render_id"],
            "API 1 retained readiness identity changed",
        )
        require(not (root / "artifacts/failure.json").exists(), "success retained a failure")
    require(pdf.read_bytes().startswith(b"%PDF-"), "fixture has no PDF")
    return {
        "passed": True,
        "readiness": evidence,
        "pdf_sha256": sha256_file(pdf),
        "scope": "insecure-context exposure only" if command == "render-api2" else "secure-context operations",
    }


def self_test() -> None:
    for secure in (False, True):
        for blocked in (False, True):
            expected = expected_payload(secure, blocked)
            check_payload(expected, secure, blocked)
            for mutation in ("missing-check", "duplicate-check", "wrong-context", "wrong-policy", "skipped-supports"):
                invalid = copy.deepcopy(expected)
                if mutation == "missing-check":
                    invalid["checks"].pop()
                elif mutation == "duplicate-check":
                    invalid["checks"].append(invalid["checks"][0])
                elif mutation == "wrong-context":
                    invalid["secureContext"] = not secure
                elif mutation == "wrong-policy":
                    invalid["privateRsaBlocked"] = not blocked
                else:
                    invalid["supportsAvailable"] = not secure
                try:
                    check_payload(invalid, secure, blocked)
                except AssertionError:
                    pass
                else:
                    raise AssertionError(f"oracle accepted {mutation}")
    with tempfile.TemporaryDirectory(prefix="pliego-crypto-selftest-") as temporary:
        first, _ = api2_fixture(Path(temporary) / "first", True)
        second, _ = api2_fixture(Path(temporary) / "second", True)
        require(first == second, "fixture input closure is not stable")
        request = json.loads(first)
        require(request["resources"] == {"network": "deny", "host_fonts": "deny"}, "fixture is not offline")
        require(request["settlement"]["limits"]["host_wall_ms"] == 60000, "fixture changed native wall limit")
    print("WebCrypto policy checker self-test: passed (pure oracle/fixture checks, no native execution)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--proof-directory", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--process-timeout-seconds", type=int, default=90)
    parser.add_argument(
        "--reference-unrestricted",
        action="store_true",
        help="check an old, unrestricted reference only; NOT host-policy qualification",
    )
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not args.binary or not args.proof_directory or not args.source_commit:
        parser.error("--binary, --proof-directory and --source-commit are required")
    if not 65 <= args.process_timeout_seconds <= 300:
        parser.error("--process-timeout-seconds must be in 65..300 (does not change native limits)")
    binary = args.binary.resolve(strict=True)
    output = args.proof_directory.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {
        "schema": "pliego.webcrypto-policy-proof",
        "version": 1,
        "passed": False,
        "qualification": "unrestricted-reference-only" if args.reference_unrestricted else "production-host-policy",
        "fixture_sha256": sha256_file(FIXTURE),
        "cases": [],
    }
    try:
        probe, raw = run_probe(binary)
        (output / "probe.json").write_bytes(raw)
        engine = probe["engine"]
        validate_probe(
            probe,
            binary_sha256=sha256_file(binary),
            expected_version=engine["version"],
            expected_source_commit=args.source_commit,
            expected_target=engine["runtime"]["target"],
            expected_servo_base=engine["runtime"]["servo_base"],
        )
        report["engine"] = engine
        for command in ("render", "render-controlled", "render-api2"):
            row = {"command": command, "passed": False}
            try:
                row.update(
                    run_case(
                        binary,
                        output / command,
                        command,
                        not args.reference_unrestricted,
                        probe,
                        args.process_timeout_seconds,
                    )
                )
            except (AssertionError, OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
                row["failure"] = str(error)
            report["cases"].append(row)
            (output / "report.json").write_bytes(canonical_json(report))
            print(f"{command}: {'passed' if row['passed'] else row['failure']}", flush=True)
        report["passed"] = all(row["passed"] for row in report["cases"])
        require(report["passed"], f"WebCrypto checks failed; retained evidence: {output}")
    finally:
        (output / "report.json").write_bytes(canonical_json(report))


if __name__ == "__main__":
    main()
