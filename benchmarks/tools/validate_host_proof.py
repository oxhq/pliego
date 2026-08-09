#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Validate a versioned Pliego dedicated-host proof and retained evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-host-proof.v1.json"
PROOF_NAME = "benchmark-host-proof.v1.json"
CHRONOLOGY_NAME = "benchmark-host-chronology.ndjson"
DIAGNOSTICS_NAME = "benchmark-host-diagnostics.json"
STDOUT_NAME = "benchmark-command.stdout"
STDERR_NAME = "benchmark-command.stderr"
MANIFEST_NAME = "SHA256SUMS"
ARTIFACT_NAMES = {PROOF_NAME, CHRONOLOGY_NAME, DIAGNOSTICS_NAME, STDOUT_NAME, STDERR_NAME}
EXPECTED_PRODUCTION_COMMAND = (
    "python3",
    "benchmarks/tools/test_process_tree_sampler.py",
    "--acceptance-overhead",
)

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_result  # noqa: E402


def sha256_file(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def load_chronology(path: Path, violations: list[str]) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        violations.append(f"chronology unreadable: {error}")
        return events
    for line_number, line in enumerate(lines, 1):
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            violations.append(f"chronology line {line_number} is invalid JSON: {error}")
            continue
        if not isinstance(event, dict):
            violations.append(f"chronology line {line_number} is not an object")
            continue
        events.append(event)
    sequences = [event.get("sequence") for event in events]
    if sequences != list(range(len(events))):
        violations.append("chronology sequence must be contiguous and ordered from zero")
    if not events or events[0].get("event") != "preflight.started":
        violations.append("chronology must begin with preflight.started")
    return events


def parse_manifest(path: Path, violations: list[str]) -> dict[str, str]:
    entries: dict[str, str] = {}
    try:
        lines = path.read_text(encoding="ascii").splitlines()
    except OSError as error:
        violations.append(f"SHA256SUMS unreadable: {error}")
        return entries
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9._-]+)", line)
        if not match:
            violations.append(f"invalid SHA256SUMS line: {line!r}")
            continue
        digest, name = match.groups()
        if name in entries:
            violations.append(f"duplicate SHA256SUMS entry: {name}")
        entries[name] = digest
    return entries


def validate_semantics(
    proof: dict[str, Any],
    root: Path,
    *,
    allow_fixture_evidence: bool,
    forbidden_event: str | None,
) -> list[str]:
    violations: list[str] = []
    checks = proof.get("checks", [])
    check_ids = [check.get("id") for check in checks if isinstance(check, dict)]
    if len(check_ids) != len(set(check_ids)):
        violations.append("check ids must be unique")

    status = proof.get("status")
    identity = proof.get("identity", {})
    runner = proof.get("runner", {})
    if status == "accepted":
        if proof.get("mode") != "production":
            violations.append("accepted proof must use production mode")
        if proof.get("evidence_source") != "live" and not allow_fixture_evidence:
            violations.append("accepted evidence must come from a live host")
        if any(not check.get("passed", False) for check in checks):
            violations.append("accepted proof contains a failed check")
        if identity.get("ref_protected") is not True:
            violations.append("accepted proof requires protected main")
        if identity.get("worktree_clean") is not True:
            violations.append("accepted proof requires an unmodified checkout")
        if (
            identity.get("repository") != "OxHQ/pliego"
            or identity.get("event") != "workflow_dispatch"
            or identity.get("ref") != "refs/heads/main"
        ):
            violations.append("accepted proof has the wrong repository, event, or ref")
        shas = {identity.get("sha"), identity.get("checkout_sha"), identity.get("default_branch_sha")}
        if len(shas) != 1 or None in shas:
            violations.append("accepted proof must bind workflow, checkout, and main to one immutable SHA")
        required_labels = {"self-hosted", "Linux", "X64", "pliego-benchmark-pinned-v1"}
        if runner.get("group") != "Pliego dedicated benchmarks":
            violations.append("accepted proof has the wrong runner group")
        if not required_labels.issubset(set(runner.get("labels", []))):
            violations.append("accepted proof is missing a required runner label")
        if runner.get("os") != "Linux" or runner.get("arch") != "X64":
            violations.append("accepted proof requires Linux/X64")
        if runner.get("status") != "online" or runner.get("busy") is not True:
            violations.append("accepted proof requires the current online, busy runner")
        if proof.get("post") is None or proof.get("drift") is None:
            violations.append("accepted proof requires postflight evidence")
        elif proof["drift"].get("controls_equal") is not True:
            violations.append("accepted proof contains host-control drift")
        elif not proof["drift"].get("throttle_delta") or any(
            delta != 0 for delta in proof["drift"]["throttle_delta"].values()
        ):
            violations.append("accepted proof contains missing or changed throttle counters")
        pre = proof.get("pre", {})
        cpu = pre.get("cpu", {})
        if cpu.get("process_scan_complete") is not True or cpu.get("competing_processes"):
            violations.append("accepted proof did not establish an idle user-space CPU set")
        controls = pre.get("controls", {})
        if not controls.get("governors") or any(
            governor != "performance" for governor in controls.get("governors", {}).values()
        ):
            violations.append("accepted proof did not establish performance governors")
        post = proof.get("post") or {}
        if post.get("control_fingerprint") != pre.get("control_fingerprint"):
            violations.append("accepted proof snapshot fingerprints differ")
        if proof.get("command", {}).get("started") is not True:
            violations.append("accepted proof did not start the benchmark command")
        if proof.get("command", {}).get("argv") != list(EXPECTED_PRODUCTION_COMMAND):
            violations.append("accepted proof did not run the dedicated acceptance workload")
        if proof.get("command", {}).get("exit_code") != 0:
            violations.append("accepted proof command did not exit zero")
        if proof.get("failure") is not None:
            violations.append("accepted proof cannot contain a failure")
    elif status == "rejected":
        if proof.get("command", {}).get("started") is not False:
            violations.append("rejected preflight must not start samples")
        if proof.get("post") is not None or proof.get("drift") is not None:
            violations.append("rejected preflight cannot contain postflight evidence")
        if proof.get("failure", {}).get("code") != "HOST_PREFLIGHT_REJECTED":
            violations.append("rejected proof needs HOST_PREFLIGHT_REJECTED")
        if all(check.get("passed", False) for check in checks):
            violations.append("rejected proof must retain at least one failed check")
    elif status == "failed":
        if proof.get("command", {}).get("started") is not True:
            violations.append("postflight/command failure requires a started command")
        if proof.get("post") is None or proof.get("drift") is None:
            violations.append("postflight/command failure requires postflight evidence")
        if proof.get("failure") is None:
            violations.append("failed proof requires a typed failure")

    events = load_chronology(root / CHRONOLOGY_NAME, violations)
    names = [event.get("event") for event in events]
    if forbidden_event and forbidden_event in names:
        violations.append(f"chronology contains forbidden event {forbidden_event}")
    if status == "rejected":
        if "samples.started" in names:
            violations.append("rejected preflight reached samples.started")
        if not names or names[-1] != "preflight.rejected":
            violations.append("rejected chronology must end with preflight.rejected")
    else:
        required_order = ["preflight.passed", "samples.started", "samples.finished", "postflight.finished"]
        positions = [names.index(name) if name in names else -1 for name in required_order]
        if -1 in positions or positions != sorted(positions):
            violations.append("started chronology is missing ordered benchmark/postflight events")

    diagnostics_path = root / DIAGNOSTICS_NAME
    if diagnostics_path.is_file():
        try:
            diagnostics = json.loads(diagnostics_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            violations.append(f"diagnostics unreadable: {error}")
        else:
            if diagnostics.get("status") != status or diagnostics.get("mode") != proof.get("mode"):
                violations.append("diagnostics status/mode do not match proof")
            expected_failures = [check["id"] for check in checks if not check["passed"]]
            if diagnostics.get("failed_checks") != expected_failures:
                violations.append("diagnostics failed_checks do not match proof checks")

    manifest = parse_manifest(root / MANIFEST_NAME, violations)
    if set(manifest) != ARTIFACT_NAMES:
        violations.append(
            f"SHA256SUMS entries must be exact: expected {sorted(ARTIFACT_NAMES)}, got {sorted(manifest)}"
        )
    for name, expected in manifest.items():
        candidate = root / name
        if not candidate.is_file():
            violations.append(f"SHA256SUMS file is missing: {name}")
        elif sha256_file(candidate) != expected:
            violations.append(f"SHA256SUMS mismatch: {name}")
    return violations


def validate_document(
    proof: dict[str, Any],
    schema: dict[str, Any],
    root: Path,
    *,
    allow_fixture_evidence: bool = False,
    forbidden_event: str | None = None,
) -> list[str]:
    schema_violations: list[validate_result.Violation] = []
    validate_result.validate(proof, schema, "$", schema_violations)
    if schema_violations:
        return [str(violation) for violation in schema_violations]
    return validate_semantics(
        proof,
        root,
        allow_fixture_evidence=allow_fixture_evidence,
        forbidden_event=forbidden_event,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("proof", type=Path)
    parser.add_argument("--schema", type=Path, default=DEFAULT_SCHEMA)
    parser.add_argument("--expect-status", choices=("accepted", "rejected", "failed"))
    parser.add_argument("--forbid-event")
    parser.add_argument("--allow-fixture-evidence", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()

    try:
        proof = json.loads(args.proof.read_text(encoding="utf-8"))
        schema = json.loads(args.schema.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"validate_host_proof: {error}", file=sys.stderr)
        return 1
    if not isinstance(proof, dict) or not isinstance(schema, dict):
        print("validate_host_proof: proof and schema must be JSON objects", file=sys.stderr)
        return 1
    violations = validate_document(
        proof,
        schema,
        args.proof.parent,
        allow_fixture_evidence=args.allow_fixture_evidence,
        forbidden_event=args.forbid_event,
    )
    if args.expect_status and proof.get("status") != args.expect_status:
        violations.append(f"expected status {args.expect_status!r}, got {proof.get('status')!r}")
    if violations:
        for violation in violations:
            print(f"validate_host_proof: {violation}", file=sys.stderr)
        return 1
    print(f"validated {args.proof}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
