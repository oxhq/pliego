#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Development-only session failure census, NOT a release gate or benchmark.

Run the existing unsafe-javascript link fixture and href-only HTTPS/no-href
controls, three interleaved times each. No activation, warmup,
retry-on-failure, or successful-render timing claim. The API 2 result/closure
and private failure-only stderr are checked independently. Empty stderr on a
success or artifact rejection does not imply any unobserved session milestone.
An outer process timeout retains partial streams and stops the entire census;
subprocess.run reaps its direct child, but descendant cleanup is not proven.

The optional package-order census runs the exact link and table-background
fixture lists in their original order three times. It observes session outcomes,
not their PDF geometry or the intervening package suites; no result is retried.

The gradient-pairs modes run three rotated orders of a HTTPS control or an
original gradient fixture followed immediately by the same no-href probe.
The primary mode preserves 10s engine / 30s outer budgets. The separately
selected default-budget counterfactual changes only host_wall_ms to the API 2
default 60s, with a 65s outer bound; it is not a release waiver, automatic retry,
or runtime policy change. An interrupted census retains only an incomplete
prefix, never a complete comparison.
"""

from __future__ import annotations

import argparse
import copy
import importlib.util
import importlib.metadata
import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from check_api2_contract import (
    COMMIT_RE,
    PROBE_TIMEOUT_SECONDS,
    canonical_json,
    create_private_job_root,
    sha256_bytes,
    sha256_file,
    validate_probe,
    verify_artifact,
)
from check_api2_links import CASES, HTTPS, build_fixture, require, verify_delivery
from check_api2_table_backgrounds import CASES as TABLE_BACKGROUND_CASES


REPOSITORY = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "session_diagnostic_contract", REPOSITORY / "contracts/api2/test_contract.py"
)
require(SPEC is not None and SPEC.loader is not None, "contract validator unavailable")
CONTRACT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CONTRACT
SPEC.loader.exec_module(CONTRACT)
CONTRACT.load_schemas()

DIAGNOSTIC = "pliego-session-diagnostics-v1"
OUTER_SECONDS = 30
HOST_MS = 10000
MAX_LINE = 8192
REPEATS = 3
NAMES = ("unsafe-javascript", "https-control", "no-href-control")
PAIR_MODES = ("gradient-pairs", "gradient-pairs-default-budget")
MODES = ("matched-controls", "package-order", *PAIR_MODES)
PACKAGE_CASES = {"links": CASES, "table-backgrounds": TABLE_BACKGROUND_CASES}
PREDECESSORS = (
    (None, "https-control"),
    ("links", "unsupported-background-gradient"),
    ("table-backgrounds", "row-image-layer"),
)
UNSAFE = next(case for case in CASES if case["name"] == NAMES[0])
TIMEOUT_CLASSES = {"SETTLEMENT_TIMEOUT", "READINESS_TIMEOUT", "CONTROLLED_CAPTURE_TIMEOUT"}
REJECTIONS = {"event-loop-unavailable", "target-changed", "advance-state-changed", "timer-rejected", "other-rejection"}
TIMEOUT_SITES = {
    "startup-render-context",
    "startup-servo",
    "startup-webview",
    "webview-ready",
    "readiness-pending-deadline",
    "settlement-before-command",
    "producer-wait-before-check",
    "producer-owner-after-spin",
    "producer-wake-deadline",
    "response-before-spin",
    "response-after-spin",
    "response-wake-deadline",
}
READINESS = {
    "DocumentUnavailable",
    "NotFullyActive",
    "Loading",
    "RenderBlocked",
    "WaitMarker",
    "FontsLoading",
    "FontReadyPromise",
    "LayoutImages",
    "RasterImages",
    "RenderingUpdate",
    "CanvasImageUpdates",
}
FIELDS = {
    "diagnostic",
    "timeout_site",
    "failure_class",
    "host_elapsed_ms",
    "host_limit_ms",
    "startup_ms",
    "commands",
    "responses",
    "rejections",
    "generation_reobservations",
    "last_command",
    "last_outcome",
    "last_observation",
    "last_rejection",
    "last_observation_response",
    "load_complete",
    "resource_events",
    "resource_entries",
    "resource_loaded",
    "resource_destinations",
}
STARTUP = ("render_context_ready", "servo_ready", "webview_ready")


class OuterTimeout(RuntimeError):
    """Fatal census interruption; do not continue with another child process."""


def strict_json(raw: bytes, *, floats: bool = False) -> Any:
    return json.loads(
        raw.decode("utf-8"),
        object_pairs_hook=CONTRACT.reject_duplicate_keys,
        parse_int=CONTRACT.parse_integer,
        parse_float=float if floats else CONTRACT.reject_float,
        parse_constant=CONTRACT.reject_nonfinite_json,
    )


def uint(value: Any, name: str) -> None:
    require(type(value) is int and 0 <= value <= 2**64 - 1, f"invalid unsigned counter: {name}")


def milliseconds(value: Any, name: str) -> None:
    require(type(value) in (int, float) and math.isfinite(value) and value >= 0, f"invalid milliseconds: {name}")


def exact(value: Any, fields: set[str], name: str) -> None:
    require(isinstance(value, dict) and set(value) == fields, f"unexpected {name} fields")


def validate_observation(value: Any) -> None:
    exact(
        value,
        {
            "virtual_time_ns",
            "top_level_epoch",
            "fully_active_pipelines",
            "pending_events",
            "input_batch_saturated",
            "has_next_deadline",
            "producer_stability",
            "producer_pending",
            "producer_pending_by_kind",
            "documents",
            "first_document_readiness",
            "first_document_readiness_total",
        },
        "observation",
    )
    require(
        isinstance(value["virtual_time_ns"], str) and re.fullmatch(r"0|[1-9][0-9]{0,38}", value["virtual_time_ns"]),
        "invalid virtual time",
    )
    require(int(value["virtual_time_ns"]) <= 2**128 - 1, "virtual time overflow")
    for key in ("top_level_epoch", "fully_active_pipelines", "pending_events", "producer_pending", "documents"):
        uint(value[key], key)
    for key in ("input_batch_saturated", "has_next_deadline"):
        require(type(value[key]) is bool, f"invalid observation boolean: {key}")
    require(
        value["producer_stability"] in {"NotCheckpointed", "Busy", "FirstEmpty", "StableEmpty", "UnchangedCheckpoint"},
        "unknown producer stability",
    )
    pending = value["producer_pending_by_kind"]
    require(isinstance(pending, list) and len(pending) == 5, "incomplete producer snapshot")
    for entry, kind in zip(pending, ("Task", "Resource", "Font", "Image", "ExternalCallback")):
        exact(entry, {"kind", "pending"}, "producer")
        require(entry["kind"] == kind, "producer kinds/order changed")
        uint(entry["pending"], "producer pending")
    require(
        value["producer_pending"] == min(2**64 - 1, sum(item["pending"] for item in pending)), "producer total differs"
    )
    blockers, total = value["first_document_readiness"], value["first_document_readiness_total"]
    if value["documents"] == 0:
        require(blockers is None and total is None, "readiness without a document")
    else:
        uint(total, "readiness total")
        require(isinstance(blockers, list) and len(blockers) == min(total, 8), "invalid bounded readiness list")
        require(all(isinstance(item, str) and item in READINESS for item in blockers), "unknown readiness blocker")


def validate_diagnostic(raw: bytes, *, host_ms: int = HOST_MS) -> dict[str, Any] | None:
    require(type(host_ms) is int and host_ms in (HOST_MS, 60000), "unknown diagnostic budget")
    if not raw:
        return None
    require(
        len(raw) <= MAX_LINE and raw.endswith(b"\n") and b"\r" not in raw and b"\n" not in raw[:-1],
        "diagnostic must be one LF-terminated line of at most 8192 bytes",
    )
    value = strict_json(raw, floats=True)
    require(isinstance(value, dict) and value.get("diagnostic") == DIAGNOSTIC, "unknown diagnostic schema")
    if set(value) == {"diagnostic", "encoding_failed"}:
        require(value["encoding_failed"] is True, "invalid diagnostic fallback")
        return value
    exact(value, FIELDS, "diagnostic")
    require(value["failure_class"] in TIMEOUT_CLASSES | {"OTHER_SESSION_FAILURE"}, "unknown failure class")
    if value["failure_class"] in {"SETTLEMENT_TIMEOUT", "READINESS_TIMEOUT"}:
        require(
            isinstance(value["timeout_site"], str) and value["timeout_site"] in TIMEOUT_SITES, "unknown timeout site"
        )
    else:
        require(value["timeout_site"] is None, "non-timeout carries timeout site")
    for key in ("host_elapsed_ms", "host_limit_ms"):
        milliseconds(value[key], key)
    require(value["host_limit_ms"] == host_ms, "session budget changed")
    exact(value["startup_ms"], set(STARTUP), "startup")
    previous, missing = 0.0, False
    for key in STARTUP:
        mark = value["startup_ms"][key]
        if mark is None:
            missing = True
        else:
            milliseconds(mark, key)
            require(not missing and previous <= mark <= value["host_elapsed_ms"], "unordered startup milestone")
            previous = mark
    for key in (
        "commands",
        "responses",
        "rejections",
        "generation_reobservations",
        "resource_entries",
        "resource_loaded",
    ):
        uint(value[key], key)
    require(value["rejections"] <= value["responses"] <= value["commands"], "inconsistent command counters")
    require(
        value["last_command"] is None
        or value["last_command"]
        in {"Observe", "DriveOneTurn", "AdvanceTo", "PrepareCapture", "ConsumeCapture", "WaitForWake"},
        "unknown command",
    )
    require((value["last_command"] is None) == (value["commands"] == 0), "command identity absent")
    require(
        value["last_outcome"] is None
        or value["last_outcome"] in REJECTIONS | {"completed", "indeterminate", "transport-failure-or-indeterminate"},
        "unknown outcome",
    )
    require((value["last_outcome"] is None) == (value["responses"] == 0), "response identity absent")
    require(value["last_rejection"] is None or value["last_rejection"] in REJECTIONS, "unknown rejection")
    require((value["last_rejection"] is None) == (value["rejections"] == 0), "rejection identity absent")
    if value["last_observation"] is None:
        require(value["last_observation_response"] is None, "observation generation without observation")
    else:
        validate_observation(value["last_observation"])
        uint(value["last_observation_response"], "observation response")
        require(0 < value["last_observation_response"] <= value["responses"], "observation response outside history")
    require(value["load_complete"] is None or type(value["load_complete"]) is bool, "invalid load state")
    if value["resource_events"] is not None:
        uint(value["resource_events"], "resource events")
    else:
        require(value["resource_entries"] == 0 and value["load_complete"] is None, "resources without delegate")
    require(value["resource_loaded"] <= value["resource_entries"], "loaded resource count exceeds entries")
    destinations = value["resource_destinations"]
    require(
        isinstance(destinations, list) and len(destinations) == min(8, value["resource_entries"]),
        "invalid bounded destination list",
    )
    require(
        all(
            isinstance(item, str) and item in {"Document", "Style", "Font", "Image", "Script", "Empty", "other"}
            for item in destinations
        ),
        "unknown resource destination",
    )
    return value


def checked_artifact(root: Path, descriptor: dict[str, Any]) -> Path:
    relative = descriptor["path"]
    require(CONTRACT.safe_relative_path(relative), "nonportable artifact path")
    current = root
    require(not current.is_symlink() and not current.is_junction(), "linked artifact root")
    for part in relative.split("/"):
        current /= part
        require(not current.is_symlink() and not current.is_junction(), "linked artifact component")
    return verify_artifact(root, descriptor)


def validate_contract(kind: str, value: Any, filename: str) -> None:
    CONTRACT.assert_valid(f"diagnostic {kind}", kind, value)
    require(
        not CONTRACT.member_order_semantics(value, CONTRACT.SCHEMAS[filename], filename),
        f"noncanonical {kind} member order",
    )


def validate_input(payload: bytes, root: Path) -> None:
    request = strict_json(payload)
    validate_contract("request", request, "render-request.v1.json")
    manifest_path = checked_artifact(root, request["input"]["manifest"])
    manifest = strict_json(manifest_path.read_bytes())
    validate_contract("input_manifest", manifest, "input-manifest.v1.json")
    require(not CONTRACT.input_manifest_semantics(manifest, root / "input"), "invalid frozen input closure")
    for entry in manifest["entries"]:
        checked_artifact(root / "input", entry)


def validate_result(raw: bytes, code: int, payload: bytes, probe: dict[str, Any], job: Path) -> dict[str, Any]:
    result = strict_json(raw)
    require(canonical_json(result) == raw, "noncanonical RenderResult stdout")
    validate_contract("result", result, "render-result.v1.json")
    require(
        result["request"] == strict_json(payload) and result["engine"] == probe["engine"], "result identity changed"
    )
    require(
        result["conformance"] == {"requested": None, "status": "not-requested", "evidence": None},
        "unexpected conformance claim",
    )
    diagnostics = result["diagnostics"]
    require(diagnostics["retained"] is True and diagnostics["artifacts"], "missing retained diagnostics")
    descriptors = diagnostics["artifacts"]
    paths = [entry["path"] for entry in descriptors]
    require(not CONTRACT.path_set_semantics(paths, "diagnostics"), "invalid diagnostic paths")
    records = []
    for descriptor in descriptors:
        path = checked_artifact(job, descriptor)
        if path.suffix == ".json":
            records.append(strict_json(path.read_bytes(), floats=True))
    require(
        CONTRACT.listed_files(job / "diagnostics") == sorted(path.removeprefix("diagnostics/") for path in paths),
        "unlisted diagnostic file",
    )
    codes = [record["code"] for record in records if isinstance(record, dict) and isinstance(record.get("code"), str)]
    if result["status"] == "failed":
        require(
            code == 1 and result["delivery"] is None and not (job / "delivery").exists(), "failure published delivery"
        )
        require(codes and all(codes), "failure lacks retained typed cause")
        require(not list(job.rglob("*.pdf")), "failed fixture left a partial PDF")
        facts = {"status": "failed", "error_kind": result["error"]["kind"], "codes": codes}
    else:
        require(code == 0, "success process exit differs")
        for descriptor in result["delivery"].values():
            checked_artifact(job / "delivery", descriptor)
        bundle = strict_json((job / "delivery/bundle.json").read_bytes())
        validate_contract("bundle_manifest", bundle, "bundle-manifest.v1.json")
        for descriptor in bundle["entries"]:
            checked_artifact(job / "delivery", descriptor)
        scene, pdf = verify_delivery(job, result)
        # Validate every operation, not only fields read by the link helper.
        strict_scene = strict_json((job / "delivery/scene.json").read_bytes())
        validate_contract("scene", strict_scene, "document-scene.v2.json")
        require(not CONTRACT.scene_semantics(scene, result["request"]), "invalid scene semantics")
        require(not CONTRACT.bundle_manifest_semantics(bundle, scene, root=job / "delivery"), "invalid bundle closure")
        entries = {entry["path"]: entry for entry in bundle["entries"]}
        require(
            all(entries[result["delivery"][kind]["path"]] == result["delivery"][kind] for kind in ("pdf", "scene")),
            "result descriptor differs from bundle",
        )
        require(pdf.read_bytes().startswith(b"%PDF-"), "delivery is not a PDF")
        facts = {
            "status": "success",
            "error_kind": None,
            "codes": [],
            "pdf_sha256": sha256_file(pdf),
            "pages": len(scene["pages"]),
            "pdf_geometry_qualified": False,
        }
    allowed = {"input-manifest.json", "input", "diagnostics"} | (
        {"delivery"} if result["status"] == "success" else set()
    )
    require({path.name for path in job.iterdir()} == allowed, "unexpected public job-root entry")
    return {"valid": True, **facts}


def diagnostic_facts(raw: bytes, protocol: dict[str, Any], *, host_ms: int = HOST_MS) -> dict[str, Any]:
    record = validate_diagnostic(raw, host_ms=host_ms)
    codes = protocol.get("codes", [])
    if record is None:
        require(not TIMEOUT_CLASSES.intersection(codes), "session timeout omitted opt-in diagnostic")
        return {"valid": True, "available": False, "disposition": "not-emitted", "milestones": None}
    require(protocol["status"] == "failed", "success emitted failure-only diagnostic")
    if record.get("encoding_failed") is True:
        return {"valid": True, "available": False, "disposition": "encoding-failed", "milestones": None}
    if record["failure_class"] != "OTHER_SESSION_FAILURE":
        require(record["failure_class"] in codes, "diagnostic failure differs from retained typed result")
    else:
        require(not TIMEOUT_CLASSES.intersection(codes), "diagnostic lost known timeout class")
    return {"valid": True, "available": True, "disposition": "emitted", "milestones": record}


def case_for(name: str, suite: str | None = None) -> dict[str, Any]:
    if suite is not None:
        require(suite in PACKAGE_CASES, "unknown diagnostic suite")
        matches = [case for case in PACKAGE_CASES[suite] if case["name"] == name]
        require(len(matches) == 1, "unknown or ambiguous diagnostic suite case")
        return copy.deepcopy(matches[0])
    require(name in NAMES, "unknown diagnostic case")
    if name == NAMES[0]:
        return copy.deepcopy(UNSAFE)
    body = UNSAFE["body"].replace(' href="javascript:alert(1)"', f' href="{HTTPS}"' if name == NAMES[1] else "")
    return {"name": name, "body": body, "text": ["UNSAFE"]}


def schedule(mode: str = "matched-controls") -> list[dict[str, Any]]:
    require(mode in MODES, "unknown census mode")
    if mode in PAIR_MODES:
        entries = []
        for repeat in range(REPEATS):
            for suite, predecessor in PREDECESSORS[repeat:] + PREDECESSORS[:repeat]:
                pair = {"repeat": repeat + 1, "pair": predecessor}
                entries.append({**pair, "suite": suite, "name": predecessor, "pair_role": "predecessor"})
                entries.append({**pair, "name": "no-href-control", "pair_role": "probe"})
        return entries
    if mode == "package-order":
        return [
            {"repeat": repeat + 1, "suite": suite, "name": case["name"]}
            for repeat in range(REPEATS)
            for suite, cases in PACKAGE_CASES.items()
            for case in cases
        ]
    # Rotate first position; this is one predetermined census, not retries.
    return [
        {"repeat": repeat + 1, "name": name} for repeat in range(REPEATS) for name in NAMES[repeat:] + NAMES[:repeat]
    ]


def budgets(mode: str) -> tuple[int, int]:
    require(mode in MODES, "unknown census mode")
    return (60000, 65) if mode == "gradient-pairs-default-budget" else (HOST_MS, OUTER_SECONDS)


def build_census_fixture(output: Path, item: dict[str, Any], mode: str) -> tuple[bytes, Path]:
    host_ms, _ = budgets(mode)
    payload, source = build_fixture(REPOSITORY, output, case_for(item["name"], item.get("suite")))
    request = strict_json(payload)
    require(request["settlement"]["limits"]["host_wall_ms"] == HOST_MS, "original fixture budget changed")
    if host_ms != HOST_MS:
        request["settlement"]["limits"]["host_wall_ms"] = host_ms
        payload = canonical_json(request)
    return payload, source


def execute(
    binary: Path, args: list[str], root: Path, *, cwd: Path, payload: bytes | None, timeout: int, diagnostics: bool
) -> tuple[bytes, bytes, int]:
    environment = os.environ.copy()
    environment.pop("PLIEGO_SESSION_DIAGNOSTICS", None)
    if diagnostics:
        environment["PLIEGO_SESSION_DIAGNOSTICS"] = "1"
    started = time.monotonic()
    process: dict[str, Any] = {
        "arguments": args,
        "limit_seconds": timeout,
        "session_diagnostics_enabled": diagnostics,
        "direct_child_reaped": False,
        "descendant_cleanup_verified": False,
    }
    try:
        completed = subprocess.run(
            [str(binary), *args],
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL if payload is None else None,
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        (root / "stdout.json").write_bytes(error.stdout or b"")
        (root / "stderr.txt").write_bytes(error.stderr or b"")
        process.update(
            outcome="outer-process-timeout", wall_seconds=time.monotonic() - started, direct_child_reaped=True
        )
        (root / "process.json").write_bytes(canonical_json(process))
        raise OuterTimeout("outer process deadline: census aborted; descendant cleanup unverified") from error
    (root / "stdout.json").write_bytes(completed.stdout)
    (root / "stderr.txt").write_bytes(completed.stderr)
    process.update(
        outcome="exited",
        exit_code=completed.returncode,
        wall_seconds=time.monotonic() - started,
        direct_child_reaped=True,
    )
    (root / "process.json").write_bytes(canonical_json(process))
    return completed.stdout, completed.stderr, completed.returncode


CHECK_ERRORS = (AssertionError, ValueError, TypeError, KeyError, OSError)


def run_attempt(
    binary: Path, output: Path, item: dict[str, Any], probe: dict[str, Any], mode: str = "matched-controls"
) -> dict[str, Any]:
    host_ms, outer_seconds = budgets(mode)
    payload, source = build_census_fixture(output, item, mode)
    (output / "request.json").write_bytes(payload)
    job = output / "job"
    create_private_job_root(job)
    shutil.copy2(source / "input-manifest.json", job / "input-manifest.json")
    shutil.copytree(source / "input", job / "input")
    validate_input(payload, source)
    validate_input(payload, job)
    raw, stderr, code = execute(
        binary, ["render-api2"], output, cwd=job, payload=payload, timeout=outer_seconds, diagnostics=True
    )
    try:
        validate_input(payload, job)
        protocol = validate_result(raw, code, payload, probe, job)
    except CHECK_ERRORS as error:
        protocol = {"valid": False, "failure": str(error)}
    try:
        # Parse even when stdout is invalid, retaining diagnostic-only evidence
        # without pretending it is bound to a valid terminal result.
        if protocol["valid"]:
            private = diagnostic_facts(stderr, protocol, host_ms=host_ms)
        else:
            record = validate_diagnostic(stderr, host_ms=host_ms)
            private = {
                "valid": True,
                "available": record is not None,
                "disposition": "unbound-result",
                "milestones": record,
            }
    except CHECK_ERRORS as error:
        private = {"valid": False, "available": False, "failure": str(error), "milestones": None}
    return {
        **item,
        "protocol": protocol,
        "session_diagnostic": private,
        "input_sha256": sha256_file(source / "input/document.html"),
        "request_sha256": sha256_bytes(payload),
    }


def run_census(binary: Path, output: Path, probe: dict[str, Any], report: dict[str, Any]) -> None:
    for index, item in enumerate(report["schedule"], 1):
        target = output / f"{index:02}-{item['name']}"
        try:
            row = run_attempt(binary, target, item, probe, report.get("census_mode", "matched-controls"))
        except OuterTimeout as error:
            report["attempts"].append({**item, "fatal": str(error), "process_path": f"{target.name}/process.json"})
            report["census_status"] = "aborted-outer-timeout"
            (output / "report.json").write_bytes(canonical_json(report))
            raise
        except CHECK_ERRORS as error:
            report["attempts"].append({**item, "fatal": str(error)})
            report["census_status"] = "aborted-harness-error"
            (output / "report.json").write_bytes(canonical_json(report))
            raise
        report["attempts"].append(row)
        (output / "report.json").write_bytes(canonical_json(report))
        print(
            f"{index}/{len(report['schedule'])} {item['name']}: result={row['protocol'].get('status', 'invalid')}, "
            f"diagnostic={row['session_diagnostic'].get('disposition', 'invalid')}",
            flush=True,
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--proof-directory", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--pypdf-version", default="6.16.2")
    parser.add_argument("--census", choices=MODES, default="matched-controls")
    args = parser.parse_args()
    require(importlib.metadata.version("pypdf") == args.pypdf_version, "pypdf differs from required parser pin")
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SelfTest)
        require(unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful(), "session diagnostic self-test failed")
        return
    if not args.binary or not args.proof_directory or not args.source_commit:
        parser.error("--binary, --proof-directory and --source-commit are required")
    require(COMMIT_RE.fullmatch(args.source_commit), "source commit must be full lowercase Git SHA")
    require(platform.system() == "Darwin" and platform.machine() == "x86_64", "native census requires macOS Intel")
    binary = args.binary.resolve(strict=True)
    output = args.proof_directory.resolve()
    output.mkdir(parents=True, exist_ok=False)
    probe_dir = output / "probe"
    probe_dir.mkdir()
    host_ms, outer_seconds = budgets(args.census)
    report = {
        "schema": "pliego.session-diagnostic-census",
        "version": 1,
        "source_commit": args.source_commit,
        "release_acceptance": False,
        "benchmark_qualified": False,
        "activation_performed": False,
        "host_wall_ms": host_ms,
        "outer_process_seconds": outer_seconds,
        "census_mode": args.census,
        "budget_counterfactual": args.census == "gradient-pairs-default-budget",
        "schedule": schedule(args.census),
        "attempts": [],
        "census_status": "probing",
        "host": {"system": platform.system(), "machine": platform.machine(), "release": platform.release()},
        "checker_sha256": sha256_file(Path(__file__)),
        "pypdf_version": importlib.metadata.version("pypdf"),
        "binary_sha256": sha256_file(binary),
    }
    (output / "report.json").write_bytes(canonical_json(report))
    try:
        raw, stderr, code = execute(
            binary,
            ["--contract-probe"],
            probe_dir,
            cwd=probe_dir,
            payload=None,
            timeout=PROBE_TIMEOUT_SECONDS,
            diagnostics=False,
        )
        require(code == 0 and not stderr, "contract probe framing failed")
        probe = strict_json(raw)
        require(canonical_json(probe) == raw, "noncanonical contract probe")
        engine = probe["engine"]
        validate_probe(
            probe,
            binary_sha256=report["binary_sha256"],
            expected_version=engine["version"],
            expected_source_commit=args.source_commit,
            expected_target="x86_64-apple-darwin",
            expected_servo_base=engine["runtime"]["servo_base"],
        )
        report.update(engine=engine, census_status="running")
        run_census(binary, output, probe, report)
        valid = all(
            row["protocol"]["valid"]
            and row["session_diagnostic"]["valid"]
            and row["session_diagnostic"].get("disposition") != "encoding-failed"
            for row in report["attempts"]
        )
        require(sha256_file(binary) == report["binary_sha256"], "binary changed during census")
        report["census_status"] = "complete" if valid else "complete-with-invalid-or-incomplete-evidence"
    except (OuterTimeout, *CHECK_ERRORS, subprocess.SubprocessError) as error:
        report["failure"] = str(error)
        if not report["census_status"].startswith("aborted"):
            report["census_status"] = "aborted"
        raise
    finally:
        report["files"] = [
            {"path": path.relative_to(output).as_posix(), "bytes": path.stat().st_size, "sha256": sha256_file(path)}
            for path in sorted(output.rglob("*"))
            if path.is_file() and path != output / "report.json"
        ]
        (output / "report.json").write_bytes(canonical_json(report))
    require(valid, "census retained invalid or incomplete evidence; no acceptance claim")


class SelfTest(unittest.TestCase):
    def diagnostic(self) -> dict[str, Any]:
        return {
            "diagnostic": DIAGNOSTIC,
            "timeout_site": "response-wake-deadline",
            "failure_class": "SETTLEMENT_TIMEOUT",
            "host_elapsed_ms": 10001.5,
            "host_limit_ms": 10000.0,
            "startup_ms": dict(zip(STARTUP, (100.5, 200.5, 300.5))),
            "commands": 2,
            "responses": 1,
            "rejections": 0,
            "generation_reobservations": 0,
            "last_command": "Observe",
            "last_outcome": "completed",
            "last_rejection": None,
            "last_observation_response": 1,
            "load_complete": False,
            "resource_events": 0,
            "resource_entries": 0,
            "resource_loaded": 0,
            "resource_destinations": [],
            "last_observation": {
                "virtual_time_ns": "0",
                "top_level_epoch": 1,
                "fully_active_pipelines": 1,
                "pending_events": 0,
                "input_batch_saturated": False,
                "has_next_deadline": False,
                "producer_stability": "StableEmpty",
                "producer_pending": 0,
                "producer_pending_by_kind": [
                    {"kind": kind, "pending": 0} for kind in ("Task", "Resource", "Font", "Image", "ExternalCallback")
                ],
                "documents": 1,
                "first_document_readiness": ["Loading"],
                "first_document_readiness_total": 1,
            },
        }

    def test_diagnostic_schema(self) -> None:
        good = self.diagnostic()
        self.assertEqual(validate_diagnostic(canonical_json(good)), good)
        self.assertIsNone(validate_diagnostic(b""))
        fallback = {"diagnostic": DIAGNOSTIC, "encoding_failed": True}
        self.assertEqual(validate_diagnostic(canonical_json(fallback)), fallback)
        mutations = (
            ("diagnostic", "other"),
            ("timeout_site", "secret/url"),
            ("commands", True),
            ("responses", 3),
            ("host_limit_ms", 30000),
            ("host_elapsed_ms", float("nan")),
            ("last_command", "Evaluate"),
            ("resource_destinations", ["secret/url"]),
            ("last_observation_response", 2),
            ("raw_url", "https://private.test/"),
            ("startup_ms", dict(zip(STARTUP, (100, None, 300)))),
        )
        for key, value in mutations:
            bad = copy.deepcopy(good)
            bad[key] = value
            with self.subTest(key=key), self.assertRaises(CHECK_ERRORS):
                validate_diagnostic(canonical_json(bad))
        for raw in (
            canonical_json(good) + b"\n",
            canonical_json(good) * 2,
            b"x" * 8192 + b"\n",
            b'{"diagnostic":"a","diagnostic":"b"}\n',
            canonical_json(good).replace(b"\n", b"\r\n"),
        ):
            with self.assertRaises(CHECK_ERRORS):
                validate_diagnostic(raw)
        for key, value in (
            ("pending_events", -1),
            ("producer_pending", 1),
            ("virtual_time_ns", "00"),
            ("first_document_readiness", ["raw authored readiness"]),
            ("raw", "private"),
        ):
            bad = copy.deepcopy(good)
            bad["last_observation"][key] = value
            with self.subTest(key=key), self.assertRaises(CHECK_ERRORS):
                validate_diagnostic(canonical_json(bad))

    def test_diagnostic_result_binding(self) -> None:
        failure = {"status": "failed", "codes": ["SETTLEMENT_TIMEOUT"]}
        self.assertTrue(diagnostic_facts(canonical_json(self.diagnostic()), failure)["available"])
        for raw, result in (
            (b"", failure),
            (canonical_json(self.diagnostic()), {"status": "success", "codes": []}),
            (canonical_json(self.diagnostic()), {"status": "failed", "codes": ["READINESS_TIMEOUT"]}),
        ):
            with self.assertRaises(CHECK_ERRORS):
                diagnostic_facts(raw, result)
        self.assertFalse(diagnostic_facts(b"", {"status": "success", "codes": []})["available"])
        self.assertFalse(diagnostic_facts(b"", {"status": "failed", "codes": ["SCENE_ENCODING_FAILED"]})["available"])

    def failure_fixture(self, root: Path) -> tuple[dict[str, Any], bytes, dict[str, Any]]:
        result = strict_json((REPOSITORY / "contracts/api2/goldens/accepted/render-result.failure.json").read_bytes())
        (root / "input").mkdir()
        (root / "input-manifest.json").write_bytes(b"fixture\n")
        (root / "diagnostics").mkdir()
        raw = canonical_json({"code": "SETTLEMENT_TIMEOUT", "message": "controlled fixture failure"})
        (root / "diagnostics/failure.json").write_bytes(raw)
        result["request"]["settlement"]["limits"]["host_wall_ms"] = HOST_MS
        result["error"]["kind"] = "settlement"
        result["diagnostics"]["artifacts"][0].update(sha256=sha256_bytes(raw), bytes=len(raw))
        return result, canonical_json(result["request"]), {"engine": result["engine"]}

    def test_result_and_artifact_corruptions(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            job = Path(temp)
            result, payload, probe = self.failure_fixture(job)
            self.assertEqual(
                validate_result(canonical_json(result), 1, payload, probe, job)["codes"], ["SETTLEMENT_TIMEOUT"]
            )
            for field, value in (("extra", 1), ("api", 3), ("delivery", {}), ("status", "success")):
                bad = copy.deepcopy(result)
                bad[field] = value
                with self.subTest(field=field), self.assertRaises(CHECK_ERRORS):
                    validate_result(canonical_json(bad), 1, payload, probe, job)
            with self.assertRaises(CHECK_ERRORS):
                validate_result(canonical_json(result), 0, payload, probe, job)
            wrong_probe = copy.deepcopy(probe)
            wrong_probe["engine"]["source_commit"] = "0" * 40
            with self.assertRaises(CHECK_ERRORS):
                validate_result(canonical_json(result), 1, payload, wrong_probe, job)
            (job / "diagnostics/failure.json").write_bytes(b"corrupt\n")
            with self.assertRaises(CHECK_ERRORS):
                validate_result(canonical_json(result), 1, payload, probe, job)

    def test_matched_inputs_and_schedule(self) -> None:
        self.assertEqual(len(schedule()), 9)
        self.assertEqual([sum(item["name"] == name for item in schedule()) for name in NAMES], [3, 3, 3])
        with tempfile.TemporaryDirectory() as temp:
            frozen = []
            for name in NAMES:
                payload, source = build_fixture(REPOSITORY, Path(temp) / name, case_for(name))
                request = strict_json(payload)
                validate_input(payload, source)
                self.assertEqual(request["settlement"]["limits"]["host_wall_ms"], HOST_MS)
                self.assertEqual(request["resources"], {"network": "deny", "host_fonts": "deny"})
                frozen.append((payload, source, request))
            original, source, request = frozen[0]
            # Exact retained historical request and HTML, not a new approximation.
            self.assertEqual(
                sha256_bytes(original), "sha256:935debe31996d376e6c86f49ab0b8379de662aac6ae07da3ab465a926d7a0af3"
            )
            self.assertEqual(
                sha256_file(source / "input/document.html"),
                "sha256:0ebace7c9361db48005214d33428c9a6d922dc6decdd6ab9fdadefdba33b835c",
            )
            for _, control, other in frozen[1:]:
                for file in ("styles.css", "font.ttf"):
                    self.assertEqual((source / "input" / file).read_bytes(), (control / "input" / file).read_bytes())
                other["input"] = request["input"]
                self.assertEqual(other, request)
                expected = (
                    (source / "input/document.html")
                    .read_bytes()
                    .replace(
                        b' href="javascript:alert(1)"',
                        f' href="{HTTPS}"'.encode() if control.parent.name == NAMES[1] else b"",
                    )
                )
                self.assertEqual((control / "input/document.html").read_bytes(), expected)

    def test_package_order_uses_exact_existing_fixtures_and_budgets(self) -> None:
        entries = schedule("package-order")
        original = [(suite, case) for suite, cases in PACKAGE_CASES.items() for case in cases]
        self.assertEqual(len(original), 36)
        self.assertEqual(len(entries), 108)
        self.assertEqual(
            [(row["repeat"], row["suite"], row["name"]) for row in entries],
            [(repeat, suite, case["name"]) for repeat in (1, 2, 3) for suite, case in original],
        )
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for index, (suite, case) in enumerate(original):
                copied = case_for(case["name"], suite)
                self.assertEqual(copied, case)
                self.assertIsNot(copied, case)
                payload, source = build_fixture(REPOSITORY, root / f"copy-{index}", copied)
                expected, original_source = build_fixture(REPOSITORY, root / f"original-{index}", case)
                self.assertEqual(payload, expected)
                validate_input(payload, source)
                self.assertEqual(strict_json(payload)["settlement"]["limits"]["host_wall_ms"], HOST_MS)
                for file in ("document.html", "styles.css", "font.ttf"):
                    self.assertEqual(
                        (source / "input" / file).read_bytes(), (original_source / "input" / file).read_bytes()
                    )
        with self.assertRaises(AssertionError):
            schedule("retry-until-pass")
        with self.assertRaises(AssertionError):
            case_for("unsafe-javascript", "unknown")
        with self.assertRaises(AssertionError):
            case_for("unknown", "links")

    def test_gradient_pairs_have_fixed_rotations_and_identical_probe(self) -> None:
        entries = schedule(PAIR_MODES[0])
        self.assertEqual(entries, schedule(PAIR_MODES[1]))
        self.assertEqual(len(entries), 18)
        self.assertEqual(
            [row["pair"] for row in entries[::2]],
            [
                "https-control",
                "unsupported-background-gradient",
                "row-image-layer",
                "unsupported-background-gradient",
                "row-image-layer",
                "https-control",
                "row-image-layer",
                "https-control",
                "unsupported-background-gradient",
            ],
        )
        for index in range(0, len(entries), 2):
            predecessor, probe = entries[index : index + 2]
            self.assertEqual(predecessor["repeat"], index // 6 + 1)
            self.assertEqual(predecessor["pair_role"], "predecessor")
            self.assertEqual(predecessor["pair"], predecessor["name"])
            self.assertEqual(
                probe,
                {
                    "repeat": predecessor["repeat"],
                    "pair": predecessor["name"],
                    "name": "no-href-control",
                    "pair_role": "probe",
                },
            )
        with tempfile.TemporaryDirectory() as temp:
            root, probe_payloads = Path(temp), []
            for index, item in enumerate(entries):
                primary, source = build_census_fixture(root / f"primary-{index}", item, PAIR_MODES[0])
                other, other_source = build_census_fixture(root / f"counterfactual-{index}", item, PAIR_MODES[1])
                original, original_source = build_fixture(
                    REPOSITORY, root / f"original-{index}", case_for(item["name"], item.get("suite"))
                )
                self.assertEqual(primary, original)
                validate_input(primary, source)
                validate_input(other, other_source)
                request = strict_json(other)
                self.assertEqual(request["settlement"]["limits"]["host_wall_ms"], 60000)
                request["settlement"]["limits"]["host_wall_ms"] = HOST_MS
                self.assertEqual(canonical_json(request), primary)
                # No resource, manifest, page, or non-wall settlement change.
                for file in ("input-manifest.json", "input/document.html", "input/styles.css", "input/font.ttf"):
                    self.assertEqual((source / file).read_bytes(), (original_source / file).read_bytes())
                    self.assertEqual((source / file).read_bytes(), (other_source / file).read_bytes())
                if item["pair_role"] == "probe":
                    probe_payloads.append(primary)
            self.assertEqual(len(probe_payloads), 9)
            self.assertEqual(len(set(probe_payloads)), 1)

    def test_diagnostic_binds_exact_selected_budget(self) -> None:
        for mode in MODES:
            self.assertEqual(budgets(mode), (60000, 65) if mode == PAIR_MODES[1] else (10000, 30))
        with self.assertRaises(CHECK_ERRORS):
            budgets("retry-with-more-time")
        default_request = strict_json(
            (REPOSITORY / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes()
        )
        self.assertEqual(default_request["settlement"]["limits"]["host_wall_ms"], budgets(PAIR_MODES[1])[0])
        failure = {"status": "failed", "codes": ["SETTLEMENT_TIMEOUT"]}
        for host_ms in (HOST_MS, 60000):
            diagnostic = self.diagnostic()
            diagnostic.update(host_limit_ms=host_ms, host_elapsed_ms=host_ms + 1.5)
            raw = canonical_json(diagnostic)
            self.assertTrue(diagnostic_facts(raw, failure, host_ms=host_ms)["available"])
            for incorrect in (True, float(host_ms), 30000, 60000 if host_ms == HOST_MS else HOST_MS):
                with self.subTest(expected=host_ms, incorrect=incorrect), self.assertRaises(CHECK_ERRORS):
                    diagnostic_facts(raw, failure, host_ms=incorrect)

    def test_counterfactual_executes_once_with_declared_budgets(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            item = schedule(PAIR_MODES[1])[1]
            diagnostic = self.diagnostic()
            diagnostic.update(host_limit_ms=60000, host_elapsed_ms=60001.5)
            protocol = {"valid": True, "status": "failed", "codes": ["SETTLEMENT_TIMEOUT"]}
            with (
                patch(
                    __name__ + ".execute", return_value=(b"retained-stdout", canonical_json(diagnostic), 1)
                ) as execute_mock,
                patch(__name__ + ".validate_result", return_value=protocol),
            ):
                row = run_attempt(Path("dummy"), Path(temp) / "attempt", item, {}, PAIR_MODES[1])
            self.assertEqual(execute_mock.call_count, 1)
            self.assertEqual(execute_mock.call_args.kwargs["timeout"], 65)
            self.assertTrue(execute_mock.call_args.kwargs["diagnostics"])
            request = strict_json(execute_mock.call_args.kwargs["payload"])
            self.assertEqual(request["settlement"]["limits"]["host_wall_ms"], 60000)
            self.assertEqual(row["protocol"]["status"], "failed")
            self.assertTrue(row["session_diagnostic"]["available"])

    def test_successful_closure_and_corruptions(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            job = Path(temp)
            result = strict_json(
                (REPOSITORY / "contracts/api2/goldens/accepted/render-result.success.json").read_bytes()
            )
            shutil.copytree(REPOSITORY / "contracts/api2/fixtures/delivery", job / "delivery")
            (job / "input").mkdir()
            (job / "input-manifest.json").write_bytes(b"fixture\n")
            (job / "diagnostics").mkdir()
            for descriptor in result["diagnostics"]["artifacts"]:
                shutil.copy2(
                    REPOSITORY / "contracts/api2/fixtures/diagnostics" / descriptor["path"], job / descriptor["path"]
                )
            payload, probe = canonical_json(result["request"]), {"engine": result["engine"]}
            self.assertEqual(validate_result(canonical_json(result), 0, payload, probe, job)["status"], "success")
            self.assertFalse(validate_result(canonical_json(result), 0, payload, probe, job)["pdf_geometry_qualified"])
            for kind in ("pdf", "scene", "bundle"):
                bad = copy.deepcopy(result)
                bad["delivery"][kind]["bytes"] += 1
                with self.subTest(kind=kind), self.assertRaises(CHECK_ERRORS):
                    validate_result(canonical_json(bad), 0, payload, probe, job)
            extra = job / "delivery/extra.txt"
            extra.write_bytes(b"unlisted")
            with self.assertRaises(CHECK_ERRORS):
                validate_result(canonical_json(result), 0, payload, probe, job)
            extra.unlink()
            (job / "delivery/document.pdf").write_bytes(b"corrupt")
            with self.assertRaises(CHECK_ERRORS):
                validate_result(canonical_json(result), 0, payload, probe, job)

    def test_outer_timeout_aborts_and_retains(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            error = subprocess.TimeoutExpired(["dummy"], OUTER_SECONDS, output=b"partial", stderr=b"private partial")
            with patch.object(subprocess, "run", side_effect=error):
                with self.assertRaises(OuterTimeout):
                    execute(
                        Path("dummy"),
                        ["render-api2"],
                        root,
                        cwd=root,
                        payload=b"request",
                        timeout=OUTER_SECONDS,
                        diagnostics=True,
                    )
            self.assertEqual((root / "stdout.json").read_bytes(), b"partial")
            self.assertEqual((root / "stderr.txt").read_bytes(), b"private partial")
            record = strict_json((root / "process.json").read_bytes(), floats=True)
            self.assertFalse(record["descendant_cleanup_verified"])
            report = {"attempts": [], "census_status": "running", "schedule": schedule()}
            with patch(__name__ + ".run_attempt", side_effect=OuterTimeout("stop")) as attempt:
                with self.assertRaises(OuterTimeout):
                    run_census(Path("dummy"), root, {}, report)
            self.assertEqual(attempt.call_count, 1)
            self.assertEqual(report["census_status"], "aborted-outer-timeout")

    def test_artifact_path_guards(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "actual").write_bytes(b"data")
            descriptor = {
                "path": "actual",
                "media_type": "application/octet-stream",
                "sha256": sha256_bytes(b"data"),
                "bytes": 4,
            }
            checked_artifact(root, descriptor)
            for path in ("../actual", "./actual", "a//actual", "/actual", "C:/actual", "a\\actual"):
                with self.assertRaises(CHECK_ERRORS):
                    checked_artifact(root, {**descriptor, "path": path})
            try:
                (root / "linked").symlink_to(root / "actual")
            except OSError:
                return  # Unprivileged Windows may not permit symlink creation.
            with self.assertRaises(CHECK_ERRORS):
                checked_artifact(root, {**descriptor, "path": "linked"})


if __name__ == "__main__":
    try:
        main()
    except (OuterTimeout, *CHECK_ERRORS, subprocess.SubprocessError) as error:
        print(f"Session diagnostic census failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
