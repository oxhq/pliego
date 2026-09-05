#!/usr/bin/env python3
"""One frozen real-document track/repeat over the existing cgroup PHP runner.

Preflight is the default. Timed execution needs a separately reviewed acceptance
record. All outputs, including stopped/failed populations, remain evidence; no
ratio is produced from incomplete or incorrect populations.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import os
import platform
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path, PurePosixPath
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "benchmarks/tools"))
import comparison_metrics  # noqa: E402
import run_benchmark as harness  # noqa: E402
import validate_result  # noqa: E402

SCHEMA = "pliego.real-document-campaign.v1"
TARGET = "pliego-candidate"
HASH = re.compile(r"[0-9a-f]{64}\Z")
DEADLINE_MS = 65_000


class FatalInfrastructureError(ValueError):
    """No further subprocess may start after unverified outer cleanup."""


def require(condition: Any, message: str) -> None:
    if not condition:
        raise ValueError(message)


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def read(path: Path) -> Any:
    return json.loads(path.read_bytes())


def save(path: Path, value: Any) -> None:
    # Exclusive, read-back-verified evidence publication; not crash-atomic across
    # the campaign. An interrupted run retains its already published attempts.
    data = harness.canonical_json_bytes(value) + b"\n"
    with path.open("xb") as stream:
        require(stream.write(data) == len(data), "Short evidence write")
        stream.flush()
    require(path.read_bytes() == data, "Evidence readback mismatch")


def relative_file(root: Path, relative: str) -> Path:
    require(isinstance(relative, str) and "\\" not in relative and ":" not in relative, "Nonportable path")
    parts = relative.split("/")
    require(all(part not in {"", ".", ".."} for part in parts), "Unsafe relative path")
    require(not PurePosixPath(relative).is_absolute(), "Absolute evidence path")
    path = root
    for part in parts:
        path = path / part
        require(not path.is_symlink(), "Symlink in evidence path")
    require(path.resolve(strict=True).is_relative_to(root.resolve(strict=True)), "Escaped evidence")
    require(path.is_file(), "Missing evidence file")
    return path


def inventory(root: Path) -> dict[str, dict]:
    files = {}
    for path in sorted(root.rglob("*")):
        require(not path.is_symlink(), "Cannot seal a symlink")
        if path.is_file():
            require(path.stat().st_nlink == 1, "Cannot seal a hardlink")
            files[path.relative_to(root).as_posix()] = {"sha256": digest(path), "bytes": path.stat().st_size}
    return files


def check_inventory(root: Path, expected: dict, *, exclude: set[str] | None = None) -> None:
    require(isinstance(expected, dict) and expected, "Empty evidence inventory")
    for name, fact in expected.items():
        path = relative_file(root, name)
        require(fact == {"sha256": digest(path), "bytes": path.stat().st_size}, f"Changed evidence: {name}")
    actual = set(inventory(root)) - (exclude or set())
    require(actual == set(expected), "Missing or extra evidence files")


def capture(command: list[str], root: Path, label: str, timeout: int) -> subprocess.CompletedProcess:
    save(root / f"{label}.command.json", command)
    try:
        result = subprocess.run(command, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        (root / f"{label}.stdout").write_bytes(error.stdout or b"")
        (root / f"{label}.stderr").write_bytes(error.stderr or b"")
        save(root / f"{label}.process.json", {"outcome": "outer-timeout", "cleanup_verified": False})
        # A systemd KillMode=control-group service owns ultimate cleanup. Never
        # resume sampling after this outer harness bound fires.
        raise FatalInfrastructureError("Outer process deadline; cleanup unverified, abort campaign") from error
    (root / f"{label}.stdout").write_bytes(result.stdout)
    (root / f"{label}.stderr").write_bytes(result.stderr)
    save(root / f"{label}.process.json", {"exit_code": result.returncode, "timeout_seconds": timeout})
    return result


def configuration(manifest: Path, track_id: str) -> tuple[dict, dict, Path]:
    data = read(manifest)
    require(data.get("schema") == "pliego.real-document-tracks.v1", "Wrong track schema")
    track = data["tracks"][track_id]
    corpus = (manifest.parent / track["corpus"]).resolve(strict=True)
    fixture = {key: track[key] for key in ("assets", "page_size", "page_margins", "correctness")}
    fixture["input"] = str(relative_file(corpus, track["input"]))
    require(digest(Path(fixture["input"])) == track["input_sha256"], "Changed frozen HTML")
    for asset in fixture["assets"]:
        relative_file(corpus, asset)
    return track, fixture, corpus


def schedule(track_id: str, legacy: str, repeat: int) -> dict:
    require(type(repeat) is int and 1 <= repeat <= 3, "Repeat must be 1, 2 or 3")
    targets = sorted([TARGET, legacy])
    result = {
        "contract": harness.CROSS_TARGET_SCHEDULE,
        "seed": 40400 + repeat,
        "fixture_id": track_id,
        "targets": targets,
        "warmup_iterations": 10,
        "sample_count": 100,
    }
    for phase, count in (("preflight", 1), ("warmup", 10), ("timed", 100)):
        result[phase] = harness.cross_target_phase_schedule(targets, track_id, count, result["seed"], phase)
    return result


def oracle_command(track: dict, corpus: Path, provider: str, retained: Path, output: Path, poppler: Path) -> list[str]:
    native = provider == TARGET
    delivered = retained / ("job/delivery" if native else "output")
    family = track["family"]
    command = [sys.executable, str(HERE / f"real_documents/{family}_oracle.py")]
    command += ["--fixture" if family == "ledger" else "--corpus", str(corpus)]
    command += ["--pdf", str(delivered / "document.pdf"), "--output", str(output)]
    command += ["--provider", "pliego" if native else ("browsershot" if family == "invobook" else "dompdf")]
    if family == "ledger":
        command += ["--render-pages", "--pdftoppm", str(poppler / "pdftoppm")]
    else:
        command += ["--poppler-dir", str(poppler)]
    if native:
        command += ["--scene", str(delivered / "scene.json"), "--bundle", str(delivered / "bundle.json")]
    return command


def inspect_retention(
    root: Path, index: int, phase: str, binary_sha: str, fixture_id: tuple[str, str]
) -> tuple[Path, dict, dict]:
    retained = root / f"retained/sample-{index}"
    manifest = read(retained / "manifest.json")
    require(manifest.get("schema") == "pliego.benchmark-retained-sample.v1", "Wrong retention schema")
    require(manifest["index"] == index and manifest["phase"] == phase, "Wrong retained phase/index")
    require(manifest["binary_sha256"] == binary_sha, "Wrong retained executable")
    require(
        (manifest["fixture_input_sha256"], manifest["fixture_bundle_sha256"]) == fixture_id, "Wrong retained fixture"
    )
    require(manifest["root_wall_timeout_ms"] == DEADLINE_MS, "Wrong engine process bound")
    check_inventory(retained, manifest["files"], exclude={"manifest.json"})
    sample = read(retained / "sample.json")
    require(sample.get("index") == index, "Wrong sample index")
    if sample.get("measurement_method") == "linux-cgroup-v2-v1":
        validate_measurement(sample)
        timing = manifest["timing"]
        require(
            timing["root_wall_ms"] == sample["wall_ms"]
            and timing["sampler_lifecycle_wall_ms"] == sample["one_shot_wall_ms"]
            and abs(timing["tree_wall_ms"] - sample["wall_ms"] - sample["resource_usage"]["drain_ms"]) <= 0.000501,
            "Retained timing boundaries disagree with measured resource accounting",
        )
    return retained, sample, manifest


def validate_measurement(sample: dict) -> None:
    require(sample.get("measurement_method") == "linux-cgroup-v2-v1", "Missing cgroup accounting")
    # The historical structural validator accepts Python floats; canonical JSON
    # additionally rejects NaN/Infinity anywhere in the retained measurement.
    harness.canonical_json_bytes(sample)
    violations = []
    schema = read(harness.DEFAULT_SCHEMA)
    require(type(sample.get("index")) is int, "Invalid sample index")
    # The historical timed-sample schema excludes the runner's negative untimed
    # indices. Their exact phase/index is checked separately, all other fields
    # must obey the same schema before semantic accounting accesses them.
    validate_result.validate(
        {**sample, "index": max(0, sample["index"])}, schema["definitions"]["sample"], "$.sample", violations, schema
    )
    require(not violations, f"Invalid sample structure: {violations[:1]}")
    validate_result.validate_resource_usage(sample, "$.sample", violations)
    require(not violations, f"Invalid resource accounting: {violations[:1]}")


def check_success_evidence(root: Path, phase: str, target: str, identity: dict, sample: dict, retained: Path) -> dict:
    """Recheck live-run invariants from bytes, also when verifying a moved archive."""
    validate_measurement(sample)
    launch = sample["resource_usage"]["launch_security"]
    require(
        launch["executable"]["sha256"] == identity["targets"][target]["binary_sha256"],
        "Measured executable differs from target identity",
    )
    network = launch.get("network_isolation")
    require(
        isinstance(network, dict) and network["host_namespace"] != network["engine_namespace"],
        "Missing private network namespace proof",
    )
    process = read(root / "runner.process.json")
    require(process == {"exit_code": 0, "timeout_seconds": 180}, "Successful attempt lacks successful runner exit")
    require(
        sample.get("ok") is True
        and sample.get("correctness", {}).get("pass") is True
        and bool(sample["correctness"]["checks"])
        and all(check["status"] != "fail" for check in sample["correctness"]["checks"])
        and sample.get("exit_code") == 0
        and sample.get("signal") is None,
        "Failed raw sample counted successful",
    )
    stdout = (root / "runner.stdout").read_bytes()
    if phase == "timed":
        lines = stdout.splitlines()
        require(len(lines) == 1 and json.loads(lines[0]) == sample, "Timed stdout and retained sample differ")
    else:
        require(not stdout, "Unexpected untimed runner stdout")
    native = target == TARGET
    pdf = relative_file(retained, ("job/delivery" if native else "output") + "/document.pdf")
    require(
        sample["output"].get("published_pdf") is True
        and digest(pdf) == sample["output"]["pdf_sha256"]
        and pdf.stat().st_size == sample["output"]["pdf_bytes"],
        "Retained delivered PDF differs from successful sample",
    )
    require(
        read(root / "oracle.process.json") == {"exit_code": 0, "timeout_seconds": 180},
        "Oracle did not exit successfully",
    )
    report = read(root / "oracle/report.json")
    family = identity["family"]
    provider = "pliego" if native else ("browsershot" if family == "invobook" else "dompdf")
    expected_schema = (
        "pliego.aureus-ledger-oracle.v1" if family == "ledger" else f"pliego.real-document.{family}-oracle.v1"
    )
    require(
        report.get("schema") == expected_schema
        and report.get("provider") == provider
        and report.get("outcome") in {"pass", "passed"},
        "Wrong or unsuccessful document oracle",
    )
    require(
        report.get("pdfSha256") == digest(pdf)
        and report.get("fixtureSha256" if family == "ledger" else "inputHtmlSha256") == identity["fixture_identity"][0],
        "Oracle is not bound to the delivered PDF and frozen input",
    )
    if "pdfBytes" in report:
        require(report["pdfBytes"] == pdf.stat().st_size, "Oracle PDF size differs")
    font_proof = report.get("fonts" if family == "ledger" else "fontProof", {})
    policies = {
        "pliego": "source-outline-metric-cmap-style-and-scene-subset-glyph-closure-v1",
        "dompdf": "original-whole-font-bytes",
        "browsershot": "chrome-identity-h-painted-cid-source-unicode-outline-metric-style-v1",
    }
    require(
        font_proof.get("outcome") == "passed"
        and font_proof.get("policy") == policies[provider]
        and bool(font_proof.get("embedded")),
        "Missing qualified provider-specific font proof",
    )
    if native:
        require(
            bool(font_proof.get("sceneResources")) and font_proof.get("scenePdfGlyphMappings", 0) > 0,
            "Missing scene/subset font closure",
        )
    if provider == "browsershot":
        require(font_proof.get("paintedGlyphMappings", 0) > 0, "Missing painted Chrome glyph proof")
    fingerprint = report.get("layoutFingerprint")
    require(isinstance(fingerprint, str) and HASH.fullmatch(fingerprint), "Oracle omitted stable layout fingerprint")
    return report


def stop_after_attempt(phase: str, record: dict) -> bool:
    return record["outcome"] != "correct" and (
        phase != "preflight"
        or record["outcome"] in {"infrastructure-failure", "measurement-incomplete"}
        or record.get("cleanup_verified") is False
    )


def execute_attempt(
    args: argparse.Namespace,
    track: dict,
    fixture: dict,
    corpus: Path,
    identity: dict,
    entry: dict,
    phase: str,
    root: Path,
    accepted: dict | None,
) -> dict:
    target = entry["target_id"]
    binary = args.candidate_binary if target == TARGET else ROOT / track["adapter"]
    fixture_id = tuple(identity["fixture_identity"])
    expected_index = (
        -1000000 if phase == "preflight" else (-1 - entry["iteration"] if phase == "warmup" else entry["iteration"])
    )
    record = {"phase": phase, **entry, "outcome": "infrastructure-failure", "sample": None}
    try:
        require(digest(binary) == identity["targets"][target]["binary_sha256"], "Executable changed between attempts")
        command = harness.build_command(
            args.php,
            args.track,
            fixture,
            binary,
            None,
            None,
            require_scene_report=target == TARGET,
            expected_fixture_identity=fixture_id,
            isolate_network=True,
            runner_phase=phase,
            sample_index=None if phase == "preflight" else entry["iteration"],
            native_api2=target == TARGET,
        )
        command += ["--retain-root", str(root / "retained"), "--root-wall-timeout-ms", str(DEADLINE_MS)]
        result = capture(command, root, "runner", 180)
        retained, sample, retained_manifest = inspect_retention(
            root, expected_index, phase, identity["targets"][target]["binary_sha256"], fixture_id
        )
        record.update(
            sample=sample, timing=retained_manifest["timing"], retention_sha256=digest(retained / "manifest.json")
        )
        require(harness.fixture_identity(fixture) == fixture_id, "Frozen fixture changed during rendering")
        if sample.get("measurement_method") != "linux-cgroup-v2-v1":
            record["outcome"] = "measurement-incomplete"
            raise ValueError("No complete cgroup sample")
        validate_measurement(sample)
        lines = result.stdout.splitlines()
        if phase == "timed":
            require(len(lines) == 1 and json.loads(lines[0]) == sample, "Timed stdout and retained sample differ")
        else:
            require(not result.stdout, "Unexpected untimed runner stdout")
        if sample.get("ok") is not True:
            record["outcome"] = "renderer-or-correctness-failure"
            record["failure"] = sample.get("failure", sample.get("error"))
            return record
        require(
            result.returncode == 0 and sample.get("correctness", {}).get("pass") is True,
            "Runner/sample success mismatch",
        )
        record["outcome"] = "oracle-failure"
        oracle = capture(
            oracle_command(track, corpus, target, retained, root / "oracle", args.poppler_dir), root, "oracle", 180
        )
        report = read(root / "oracle/report.json")
        record["oracle"] = {
            "report_sha256": digest(root / "oracle/report.json"),
            "pdf_sha256": report.get("pdfSha256"),
            "layout_fingerprint": report.get("layoutFingerprint"),
        }
        require(oracle.returncode == 0 and report.get("outcome") in {"pass", "passed"}, "Document oracle rejected PDF")
        check_success_evidence(root, phase, target, identity, sample, retained)
        fingerprint = report.get("layoutFingerprint")
        require(
            isinstance(fingerprint, str) and HASH.fullmatch(fingerprint), "Oracle omitted stable layout fingerprint"
        )
        if accepted is not None:
            record["outcome"] = "unreviewed-layout"
            require(
                fingerprint in accepted["targets"][target]["layout_fingerprints"], "Unreviewed output layout variant"
            )
        record["outcome"] = "correct"
    except FatalInfrastructureError as error:
        record.update(
            outcome="infrastructure-failure", cleanup_verified=False, error=f"{type(error).__name__}: {error}"
        )
    except (ValueError, OSError, KeyError, TypeError) as error:
        record["error"] = f"{type(error).__name__}: {error}"
    return record


def counts(schedule_data: dict, attempts: list[dict]) -> dict:
    result = {}
    for phase in ("preflight", "warmup", "timed"):
        result[phase] = {}
        for target in schedule_data["targets"]:
            scheduled = sum(e["target_id"] == target for e in schedule_data[phase])
            actual = [a for a in attempts if a["phase"] == phase and a["target_id"] == target]
            result[phase][target] = {
                "scheduled": scheduled,
                "attempted": len(actual),
                "not_attempted": scheduled - len(actual),
                "outcomes": dict(sorted(Counter(a["outcome"] for a in actual).items())),
            }
    return result


def aggregate(schedule_data: dict, attempts: list[dict], eligible: bool) -> dict | None:
    if not eligible:
        return None
    timed = [a for a in attempts if a["phase"] == "timed"]
    require(
        len(timed) == 200 and all(a["outcome"] == "correct" for a in attempts),
        "Incomplete joint all-correct population",
    )
    for attempt in timed:
        validate_measurement(attempt["sample"])
    artifact = {
        "schema": harness.INTERLEAVED_RUN_SCHEMA,
        "version": 1,
        "publication_status": "prerequisite-only",
        "sample_contract": harness.BENCHMARK_SAMPLE_CONTRACT,
        "sample_id_contract": harness.RAW_SAMPLE_ID_CONTRACT,
        "schedule": schedule_data,
        "raw_samples": [harness.raw_sample_record(schedule_data["fixture_id"], a, a["sample"]) for a in timed],
    }
    artifact["artifact_sha256"] = harness.interleaved_artifact_sha256(artifact)
    metrics = comparison_metrics.aggregate_interleaved_artifact(artifact)
    # The historical metric registry deliberately means root wall. Add the
    # distinct observed root-plus-descendant-drain boundary without relabeling it.
    tree_wall = {}
    for target in schedule_data["targets"]:
        values = [a["timing"]["tree_wall_ms"] for a in timed if a["target_id"] == target]
        require(all(type(v) in (int, float) and v >= 0 for v in values), "Unavailable whole-tree wall")
        tree_wall[target] = validate_result.percentiles(values)
    return {"interleaved": artifact, "metrics": metrics, "tree_wall_ms": tree_wall}


def acceptance(path: Path | None, identity: dict) -> dict | None:
    if path is None:
        return None
    value = read(path)
    require(value.get("schema") == "pliego.real-document-visual-acceptance.v1", "Wrong acceptance schema")
    require(
        value.get("identity_sha256") == harness.canonical_json_sha256(identity),
        "Acceptance is for a different runtime/corpus/oracle closure",
    )
    require(set(value["targets"]) == set(identity["targets"]), "Missing provider acceptance")
    for target in value["targets"].values():
        require(target.get("reviewed") is True and target.get("notes"), "Actual-PDF visual review required")
        require(
            target.get("layout_fingerprints") and all(HASH.fullmatch(v) for v in target["layout_fingerprints"]),
            "Missing reviewed layouts",
        )
        require(
            target.get("pdf_sha256") and all(HASH.fullmatch(v) for v in target["pdf_sha256"]), "Missing reviewed PDFs"
        )
        require(target.get("evidence"), "Missing retained visual review reference")
    return value


def run_campaign(args: argparse.Namespace) -> None:
    require(sys.flags.optimize == 0, "Optimized Python is not a qualified oracle runtime")
    require(
        platform.system() == "Linux" and os.environ.get("PLIEGO_BENCHMARK_CGROUP_PARENT"),
        "Run inside the reviewed Linux delegated-cgroup service",
    )
    track, fixture, corpus = configuration(args.manifest, args.track)
    output = args.out.resolve()
    require(not output.exists() and not output.is_relative_to(corpus), "Output must be fresh and outside the corpus")
    output.mkdir()
    (output / "attempts").mkdir()
    native = read(args.candidate_identity)
    require(
        HASH.fullmatch(native["binary_sha256"]) and re.fullmatch(r"[0-9a-f]{40}", native["source_sha"]),
        "Candidate needs exact source and executable hashes",
    )
    require(
        native.get("profile") == "release" and native.get("provenance"),
        "Candidate needs packaged release-profile provenance",
    )
    require(digest(args.candidate_binary) == native["binary_sha256"], "Candidate executable hash mismatch")
    adapter = ROOT / track["adapter"]
    legacy_run = capture([str(args.php), str(adapter), "identity"], output, "legacy-identity", 120)
    require(legacy_run.returncode == 0, "Legacy dependency identity failed")
    legacy = json.loads(legacy_run.stdout)
    require(
        legacy.get("target") == track["legacy"] and legacy.get("adapter_sha256") == digest(adapter),
        "Wrong legacy adapter identity",
    )
    probe = capture([str(args.candidate_binary), "--contract-probe"], output, "candidate-probe", 30)
    require(probe.returncode == 0, "Candidate contract discovery failed")
    contract = json.loads(probe.stdout)
    require(
        contract.get("schema") == "pliego.runtime-contract"
        and contract["engine"]["source_commit"] == native["source_sha"]
        and contract["engine"]["runtime"]["binary_sha256"] == "sha256:" + native["binary_sha256"]
        and contract["engine"]["runtime"]["target"] == "x86_64-unknown-linux-gnu",
        "Contract probe does not bind the supplied candidate identity",
    )
    # Actual render negotiation is independently enforced by the existing runner.
    native = {**native, "contract_probe": contract}
    code = {
        str(path.relative_to(ROOT)).replace("\\", "/"): digest(path)
        for path in sorted((HERE / "real_documents").glob("*.py"))
    }
    for path in [
        Path(__file__),
        args.manifest,
        harness.RUNNER,
        harness.SAMPLER,
        ROOT / "benchmarks/tools/benchmark_runtime.py",
        Path(harness.__file__),
        Path(comparison_metrics.__file__),
        Path(validate_result.__file__),
        HERE / "real_document_requirements.txt",
        HERE / "real_documents/manufacturing_requirements.txt",
    ]:
        code[path.relative_to(ROOT).as_posix()] = digest(path)
    identity = {
        "track": args.track,
        "family": track["family"],
        "fixture_identity": list(harness.fixture_identity(fixture)),
        "corpus_files": inventory(corpus),
        "code": code,
        "targets": {TARGET: native, track["legacy"]: {"binary_sha256": digest(adapter), "adapter": legacy}},
        "oracle_tools": {name: digest(args.poppler_dir / name) for name in harness.POPPLER_TOOLS},
        "oracle_runtime": {
            "python": sys.version,
            "executable_sha256": digest(Path(sys.executable)),
            "distributions": {
                name: importlib.metadata.version(name)
                for name in (
                    "pypdf",
                    "pdfplumber",
                    "fonttools",
                    "pdfminer.six",
                    "charset-normalizer",
                    "cryptography",
                    "cffi",
                    "pypdfium2",
                    "pycparser",
                    "Pillow",
                    "zxing-cpp",
                )
            },
        },
    }
    accepted = acceptance(args.acceptance, identity)
    require(args.mode == "preflight" or accepted is not None, "Timed execution requires reviewed preflight acceptance")
    save(output / "identity.json", identity)
    if accepted is not None:
        save(output / "acceptance.json", accepted)
    schedule_data = schedule(args.track, track["legacy"], args.repeat)
    save(output / "schedule.json", schedule_data)
    save(
        output / "host.json",
        {
            "host": harness.host_info(False),
            "python": sys.version,
            "environment": {
                key: os.environ.get(key)
                for key in (
                    "GITHUB_RUN_ID",
                    "GITHUB_RUN_ATTEMPT",
                    "GITHUB_SHA",
                    "GITHUB_WORKFLOW_REF",
                    "ImageOS",
                    "ImageVersion",
                )
            },
            "population": "github-hosted-exploratory"
            if os.environ.get("GITHUB_ACTIONS") == "true"
            else "local-prerequisite-only",
        },
    )
    attempts = []
    stopped = False
    phases = ("preflight",) if args.mode == "preflight" else ("preflight", "warmup", "timed")
    for phase in phases:
        if stopped:
            break
        for entry in schedule_data[phase]:
            root = output / "attempts" / f"{phase}-{entry['position']:03d}"
            root.mkdir()
            record = execute_attempt(args, track, fixture, corpus, identity, entry, phase, root, accepted)
            save(root / "attempt.json", record)
            attempts.append(record)
            print(
                json.dumps(
                    {
                        "phase": phase,
                        "target": entry["target_id"],
                        "iteration": entry["iteration"],
                        "outcome": record["outcome"],
                    }
                ),
                flush=True,
            )
            if record["outcome"] != "correct":
                stopped = True
                # Complete both preflights only when the first failure is an
                # accounted renderer/oracle result, never after infrastructure.
                if stop_after_attempt(phase, record):
                    break
    qualified = args.mode == "timed" and not stopped and len(attempts) == 222
    result = {
        "schema": SCHEMA,
        "track": args.track,
        "repeat": args.repeat,
        "mode": args.mode,
        "identity_sha256": harness.canonical_json_sha256(identity),
        "counts": counts(schedule_data, attempts),
        "comparison_qualified": qualified,
        "stop_policy": "Stop on first failed warmup/timed attempt; preserve not-attempted denominators. Infrastructure or unverified cleanup stops immediately.",
        "publication_status": "prerequisite-only",
        "aggregate": aggregate(schedule_data, attempts, qualified),
    }
    save(output / "campaign.json", result)
    save(output / "files.json", inventory(output))
    verify(output)
    if stopped:
        raise SystemExit(1)


def verify(root: Path) -> dict:
    require(sys.flags.optimize == 0, "Optimized Python is not a qualified verifier")
    check_inventory(root, read(root / "files.json"), exclude={"files.json"})
    identity, plan, campaign = (read(root / name) for name in ("identity.json", "schedule.json", "campaign.json"))
    require(campaign.get("schema") == SCHEMA, "Wrong campaign schema")
    require(campaign["mode"] in {"preflight", "timed"}, "Wrong campaign mode")
    require(identity["track"] == campaign["track"], "Wrong campaign track identity")
    require(TARGET in identity["targets"] and len(identity["targets"]) == 2, "Wrong campaign targets")
    require(campaign["identity_sha256"] == harness.canonical_json_sha256(identity), "Changed campaign identity")
    legacy = next(target for target in identity["targets"] if target != TARGET)
    require(plan == schedule(campaign["track"], legacy, campaign["repeat"]), "Changed schedule")
    attempts = []
    allowed = [(phase, entry) for phase in ("preflight", "warmup", "timed") for entry in plan[phase]]
    paths = sorted(
        (root / "attempts").glob("*/attempt.json"),
        key=lambda p: ({"preflight": 0, "warmup": 1, "timed": 2}[p.parent.name.split("-")[0]], p.parent.name),
    )
    require(len(paths) <= (2 if campaign["mode"] == "preflight" else len(allowed)), "Excess campaign attempts")
    for position, path in enumerate(paths):
        record = read(path)
        phase, entry = allowed[position]
        require(
            record["phase"] == phase and all(record[key] == value for key, value in entry.items()),
            "Attempt omitted/reordered within scheduled prefix",
        )
        require(
            not any(a["phase"] != phase and a["outcome"] != "correct" for a in attempts),
            "Campaign advanced past a failed phase",
        )
        if record["sample"] is not None:
            index = (
                -1000000
                if phase == "preflight"
                else (-1 - entry["iteration"] if phase == "warmup" else entry["iteration"])
            )
            retained, sample, manifest = inspect_retention(
                path.parent,
                index,
                phase,
                identity["targets"][entry["target_id"]]["binary_sha256"],
                tuple(identity["fixture_identity"]),
            )
            require(sample == record["sample"] and manifest["timing"] == record["timing"], "Changed raw sample/timing")
            require(digest(retained / "manifest.json") == record["retention_sha256"], "Changed retention manifest")
        if record["outcome"] == "correct":
            require(record["sample"] is not None, "Correct attempt has no raw sample")
            report = check_success_evidence(path.parent, phase, entry["target_id"], identity, sample, retained)
            require(
                digest(path.parent / "oracle/report.json") == record["oracle"]["report_sha256"], "Changed oracle report"
            )
            require(
                report.get("outcome") in {"pass", "passed"}
                and report["pdfSha256"] == record["sample"]["output"]["pdf_sha256"],
                "Incorrect PDF counted successful",
            )
            require(
                report["layoutFingerprint"] == record["oracle"]["layout_fingerprint"]
                and report["pdfSha256"] == record["oracle"]["pdf_sha256"],
                "Changed oracle identity facts",
            )
        attempts.append(record)
        if stop_after_attempt(phase, record):
            require(
                position == len(paths) - 1,
                "Campaign continued after mandatory stop",
            )
    require(campaign["counts"] == counts(plan, attempts), "Incorrect failure denominators")
    eligible = campaign["mode"] == "timed" and len(attempts) == 222 and all(a["outcome"] == "correct" for a in attempts)
    if eligible:
        accepted = acceptance(root / "acceptance.json", identity)
        require(accepted is not None, "Missing reviewed acceptance")
        require(
            all(
                a["oracle"]["layout_fingerprint"] in accepted["targets"][a["target_id"]]["layout_fingerprints"]
                for a in attempts
            ),
            "Unreviewed layout counted qualified",
        )
    require(campaign["comparison_qualified"] is eligible, "Incorrect comparison qualification")
    require(campaign["aggregate"] == aggregate(plan, attempts, eligible), "Aggregate cannot be recomputed")
    return {"verified": True, "comparison_qualified": eligible, "counts": campaign["counts"]}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", type=Path)
    parser.add_argument("--manifest", type=Path, default=HERE / "real_documents.json")
    parser.add_argument("--track")
    parser.add_argument("--candidate-binary", type=Path)
    parser.add_argument("--candidate-identity", type=Path)
    parser.add_argument("--php", type=Path)
    parser.add_argument("--poppler-dir", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--mode", choices=("preflight", "timed"), default="preflight")
    parser.add_argument("--acceptance", type=Path)
    args = parser.parse_args()
    if args.verify:
        print(json.dumps(verify(args.verify.resolve(strict=True))))
    else:
        require(
            all(
                getattr(args, name) is not None
                for name in ("track", "candidate_binary", "candidate_identity", "php", "poppler_dir", "out")
            ),
            "Missing run arguments",
        )
        run_campaign(args)


if __name__ == "__main__":
    main()
