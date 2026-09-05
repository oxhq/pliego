#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fixed minimal-static candidate/v0.3.3 comparison. Preflight is the default.

The two-job population measures ONE launcher/tree/cgroup, not two samplers.
No engine, sampler lock, existing result schema, or historical record changes.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path[:0] = [str(HERE), str(ROOT / "benchmarks/tools"), str(ROOT / "ports/pliego/tests")]
import run_real_document_comparison as common  # noqa: E402
import run_benchmark as harness  # noqa: E402
import validate_result  # noqa: E402
import check_api2_session_diagnostics as contracts  # noqa: E402
import native_batch_launcher as batch  # noqa: E402

sampler = batch.sampler

require, digest, read, save = common.require, common.digest, common.read, common.save
SCHEMA = "pliego.native-regression.v1"
TARGETS = ["candidate", "v0.3.3"]
FIXTURE = "minimal-static"
FIXTURE_HASHES = (
    "657b662eee0d7df8ae89db504712d2aadff1add2a740fc79b3c49000f1fc8cc8",
    "0c478804dfbfd66346799e87eb8d1beafd926ecaa252832ff5d31d63fc8d6ce7",
)
COUNTS = {
    "serial": {"preflight": 1, "warmup": 10, "timed": 100},
    "concurrency-2": {"preflight": 1, "warmup": 5, "timed": 50},
}
METRICS = (
    "wall_ms",
    "one_shot_wall_ms",
    "user_ms",
    "sys_ms",
    "memory_peak_bytes",
    "memory_current_bytes",
    "read_bytes",
    "write_bytes",
    "read_operations",
    "write_operations",
)


def schedule(repeat: int, mode: str) -> list[dict]:
    require(type(repeat) is int and repeat in (1, 2, 3), "Expected repeat 1, 2 or 3")
    require(mode in ("preflight", "timed"), "Invalid mode")
    result = []
    # Complete BOTH populations' preflights before any warmup/timed work.
    for phase in ["preflight"] if mode == "preflight" else ["preflight", "warmup", "timed"]:
        for population, counts in COUNTS.items():
            for entry in harness.cross_target_phase_schedule(
                TARGETS, FIXTURE + "/" + population, counts[phase], repeat, phase
            ):
                result.append({**entry, "position": len(result), "phase": phase, "population": population})
    return result


def invoke(command: list[str], root: Path, label: str, payload: dict | None = None) -> subprocess.CompletedProcess:
    save(root / f"{label}.command.json", command)
    raw = harness.canonical_json_bytes(payload) + b"\n" if payload is not None else None
    if raw is not None:
        (root / f"{label}.stdin").write_bytes(raw)
    try:
        process = subprocess.run(command, input=raw, capture_output=True, timeout=180)
    except subprocess.TimeoutExpired as failure:
        (root / f"{label}.stdout").write_bytes(failure.stdout or b"")
        (root / f"{label}.stderr").write_bytes(failure.stderr or b"")
        save(root / f"{label}.process.json", {"outcome": "outer-timeout", "cleanup_verified": False})
        raise common.FatalInfrastructureError("Outer deadline: no subsequent subprocess is permitted") from failure
    (root / f"{label}.stdout").write_bytes(process.stdout)
    (root / f"{label}.stderr").write_bytes(process.stderr)
    save(root / f"{label}.process.json", {"exit_code": process.returncode, "timeout_seconds": 180})
    return process


def fixture() -> dict:
    manifest = tomllib.loads((ROOT / "benchmarks/manifest.toml").read_text())
    value = manifest["fixtures"][FIXTURE]
    require(
        value["input"] == "benchmarks/fixtures/minimal-static/comparator.html" and value["assets"] == ["Ahem.ttf"],
        "Native regression scope is fixed minimal-static",
    )
    require(harness.fixture_identity(value) == FIXTURE_HASHES, "Frozen minimal input/font changed")
    return value


def batch_state(value: dict, identity: tuple[str, str]) -> dict:
    source = ROOT / value["input"]
    return {
        "cwd": str(source.parent),
        "input": source.name,
        "fixtureAssets": value["assets"],
        "fixtureInputSha256": identity[0],
        "fixtureBundleSha256": identity[1],
        "pageSize": value["page_size"],
        "pageMargins": value["page_margins"],
    }


def target_identity(target: str, metadata: dict, probe: dict, binary_hash: str, binary_bytes: int) -> dict:
    engine = probe["engine"]
    require(
        engine["api"] == 2 and engine["runtime"]["target"] == "x86_64-unknown-linux-gnu",
        "Expected native Linux x64 API 2",
    )
    source = metadata.get("source_sha") if target == "candidate" else metadata.get("commit")
    try:
        contracts.validate_probe(
            probe,
            binary_sha256="sha256:" + binary_hash,
            expected_source_commit=source,
            expected_version="0.4.0" if target == "candidate" else "0.3.3",
            expected_target="x86_64-unknown-linux-gnu",
            expected_servo_base=metadata["provenance"]["servo_base"]
            if target == "candidate"
            else metadata["servo_base"],
        )
    except AssertionError as failure:
        raise ValueError(str(failure)) from failure
    require(
        engine["source_commit"] == source and engine["runtime"]["binary_sha256"] == "sha256:" + binary_hash,
        "Native source/probe/binary identity differs",
    )
    require(
        metadata["binary_sha256"] == binary_hash
        and metadata["binary_bytes"] == binary_bytes
        and metadata["profile"] == "release",
        "Unverified release-profile binary",
    )
    if target == "v0.3.3":
        expected = tomllib.loads((ROOT / "benchmarks/manifest.toml").read_text())["targets"]["pliego-0.3.3"]
        require(
            metadata["schema"] == "pliego.verified-release"
            and metadata["release_tag"] == "v0.3.3"
            and source == expected["commit"]
            and binary_hash == expected["binary_sha256"]
            and binary_bytes == expected["binary_bytes"]
            and engine["version"] == "0.3.3",
            "Not the published immutable v0.3.3 target",
        )
    else:
        require(
            metadata["bundle"] == "linux-x86_64" and engine["version"] == "0.4.0" and metadata["version"] == "0.4.0",
            "Not a 0.4.0 candidate package",
        )
    return {"source_sha": source, "binary_sha256": binary_hash, "binary_bytes": binary_bytes, "probe": probe}


def verify_measurement(value: dict, *, require_success: bool = True) -> None:
    harness.canonical_json_bytes(value)
    schema = read(harness.DEFAULT_SCHEMA)
    violations = []
    validate_result.validate(
        value["resource_usage"], schema["definitions"]["resource_usage"], "$.resource_usage", violations, schema
    )
    require(not violations, f"Invalid resource structure: {violations[:1]}")
    validate_result.validate_resource_usage({**value, "ok": require_success}, "$.measurement", violations)
    require(not violations, f"Invalid resource proof: {violations[:1]}")
    usage = value["resource_usage"]
    require(
        usage["root_wall_deadline"]["limit_ms"] == 65_000
        and usage["cgroup_drained"] is True
        and usage["cleanup"]["kill_used"] is False,
        "Incomplete/unclean bounded execution",
    )
    require(not require_success or value["exit_code"] == 0, "Measured root failed")


def verify_batch_envelope(prepared: dict, measured: dict, expected_binary: dict) -> dict:
    require(
        "error" not in measured and measured["stderr"] == "" and measured["sampler_stderr"] == "",
        "Batch broker/sampler failed",
    )
    verify_measurement(measured)
    require(json.loads(measured["sampler_stdout"]) == measured["resource_usage"], "Raw sampler differs")
    result = json.loads(measured["stdout"])
    require(result["schema"] == batch.CONTRACT and result["error"] is None, "Launcher failed")
    plan = prepared["plan"]
    # Only the LF-terminated canonical bytes are actually sent by the PHP broker.
    require(
        result["plan_sha256"] == hashlib.sha256(harness.canonical_json_bytes(plan) + b"\n").hexdigest(),
        "Noncanonical batch input",
    )
    require(plan["binary"] == expected_binary, "Batch used a different native executable")
    workers, witness = result["workers"], result["overlap"]
    require(len(workers) == 2 and [worker["index"] for worker in workers] == [0, 1], "Wrong batch outputs")
    require(witness["observed"] is True and len(witness["workers"]) == 2, "No actual native overlap")
    require(len({worker["pid"] for worker in workers}) == 2, "Duplicate native PID")
    usage = measured["resource_usage"]
    group = "/" + PurePosixPath(usage["cgroup_path"]).relative_to("/sys/fs/cgroup").as_posix()
    launch = usage["launch_security"]
    require(
        launch["argv"][1:] == ["render", "paired-minimal-static", "--artifacts", prepared["artifacts"]]
        and launch["executable"]["sha256"] == digest(HERE / "native_batch_launcher.py"),
        "Wrong measured batch root",
    )
    require(
        usage["launch_security"]["temporary_storage"]["native_api2_path_bindings"] is None,
        "Batch launcher is not a direct native root",
    )
    for worker, observed in zip(workers, witness["workers"]):
        require(
            all(
                type(worker[key]) is int and worker[key] > 0
                for key in ("pid", "started_monotonic_ns", "exited_monotonic_ns")
            )
            and type(observed["start_ticks"]) is int
            and type(witness["at_monotonic_ns"]) is int,
            "Invalid native process identity/timestamp",
        )
        require(
            worker["pid"] == observed["pid"]
            and observed["start_ticks"] > 0
            and observed["cgroup"] == group
            and worker["pid"] != usage["root_pid"]
            and (observed["executable_device"], observed["executable_inode"])
            == (expected_binary["device"], expected_binary["inode"]),
            "Unbound worker exec/cgroup",
        )
        require(
            worker["exit_code"] == 0
            and worker["streams_complete"] is True
            and worker["started_monotonic_ns"] <= witness["at_monotonic_ns"] < worker["exited_monotonic_ns"],
            "Missing successful overlapping native lifetime",
        )
        for stream in ("stdout", "stderr"):
            raw = base64.b64decode(worker[stream + "_base64"], validate=True)
            require(base64.b64encode(raw).decode("ascii") == worker[stream + "_base64"], "Noncanonical captured bytes")
            require(
                len(raw) <= batch.MAX_STREAM and hashlib.sha256(raw).hexdigest() == worker[stream + "_sha256"],
                "Changed worker output",
            )
        require(worker["stderr_base64"] == "", "Native worker stderr is not empty")
    return result


def verify_storage(prepared: dict, before: list, after: list) -> None:
    require(len(before) == 2 and before == after, "Native storage identity changed")
    seen = set()
    controlled = None
    for worker, snapshot in zip(prepared["plan"]["workers"], before):
        job, temporary = PurePosixPath(worker["job"]), PurePosixPath(worker["temporary"])
        require(
            job.is_absolute()
            and temporary.is_absolute()
            and ".." not in job.parts
            and ".." not in temporary.parts
            and job.name == "job"
            and temporary.name == "temporary"
            and job.parent == temporary.parent,
            "Invalid worker topology",
        )
        expected = {"controlled_root": job.parent.parent, "sandbox": job.parent, "job": job, "temporary": temporary}
        require(
            snapshot["contract"] == "native-api2-storage-bindings-v1" and set(snapshot["bindings"]) == set(expected),
            "Wrong storage binding contract",
        )
        if controlled is None:
            controlled = snapshot["bindings"]["controlled_root"]
        require(snapshot["bindings"]["controlled_root"] == controlled, "Different controlled roots")
        for key, path in expected.items():
            bound = snapshot["bindings"][key]
            pair = bound["identity"]["device"], bound["identity"]["inode"]
            require(
                bound["path"] == path.as_posix() and all(type(value) is int and value > 0 for value in pair),
                "Wrong path or native inode",
            )
            require(pair[0] == controlled["identity"]["device"], "Split native filesystem")
            if key != "controlled_root":
                require(pair not in seen and pair != tuple(controlled["identity"].values()), "Aliased worker storage")
                seen.add(pair)


def oracle_command(value: dict, pdf: Path) -> list[str]:
    expected = value["correctness"]
    result = [sys.executable, str(ROOT / "benchmarks/tools/pdf_oracle.py"), "--pdf", str(pdf)]
    for key in ("page_count", "page_width_points", "page_height_points", "dimension_tolerance_points", "text_equals"):
        result += ["--" + key.replace("_", "-"), str(expected[key])]
    for key, option in (("text_contains", "text-contains"), ("font_families", "font-family")):
        for item in expected[key]:
            result += ["--" + option, item]
    return result + ["--raster-sha256", expected["normalized_raster_sha256"]]


def qualify_worker(root: Path, job: Path, payload: bytes, raw: bytes, code: int, probe: dict, value: dict) -> dict:
    verify_input(payload, job)
    protocol = contracts.validate_result(raw, code, payload, probe, job)
    save(root / "protocol.json", protocol)
    require(protocol["status"] == "success", "Native PDF failed: " + str(protocol))
    pdf = job / "delivery/document.pdf"
    process = invoke(oracle_command(value, pdf), root, "oracle")
    facts = json.loads(process.stdout)
    require(process.returncode == 0 and process.stderr == b"" and facts["pass"] is True, "PDF oracle failed")
    return {
        "pdf_sha256": digest(pdf),
        "pdf_bytes": pdf.stat().st_size,
        "protocol": protocol,
        "oracle": facts,
        "request_sha256": hashlib.sha256(payload).hexdigest(),
    }


def verify_input(payload: bytes, job: Path) -> None:
    contracts.validate_input(payload, job)
    request = contracts.strict_json(payload)
    require(
        request["page"]
        == {
            "size": {"width_app_units": 47622, "height_app_units": 67351},
            "margins_app_units": dict.fromkeys(("top", "right", "bottom", "left"), 0),
            "geometry_authority": "request-only-v1",
        },
        "Changed minimal page geometry",
    )
    require(
        request["settlement"]["limits"]["host_wall_ms"] == 60000
        and request["environment"] == {"locale": "en-US", "timezone": "UTC"}
        and request["resources"] == {"network": "deny", "host_fonts": "deny"}
        and request["input"]["entrypoint"] == "comparator.html",
        "Changed native fixture policy",
    )
    manifest = read(job / "input-manifest.json")
    expected = {
        "comparator.html": FIXTURE_HASHES[0],
        "Ahem.ttf": "b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448",
    }
    require(
        {entry["path"]: entry["sha256"] for entry in manifest["entries"]}
        == {key: "sha256:" + value for key, value in expected.items()},
        "Different frozen input manifest",
    )


def serial_attempt(args: argparse.Namespace, root: Path, entry: dict, value: dict, target: dict) -> dict:
    phase, index = entry["phase"], entry["iteration"]
    sample_index = -1_000_000 if phase == "preflight" else (-index - 1 if phase == "warmup" else index)
    command = harness.build_command(
        args.php,
        FIXTURE,
        value,
        Path(target["path"]),
        None,
        None,
        runner_phase=phase,
        sample_index=None if phase == "preflight" else index,
        isolate_network=True,
        native_api2=True,
    )
    command += ["--retain-root", str(root / "retained"), "--root-wall-timeout-ms", "65000"]
    process = invoke(command, root, "runner")
    retained, sample, _ = common.inspect_retention(
        root, sample_index, phase, target["identity"]["binary_sha256"], harness.fixture_identity(value)
    )
    require(process.returncode == 0 and sample["ok"] is True, "Serial runner failed")
    require((retained / "stderr.txt").read_bytes() == b"", "Native serial stderr is not empty")
    if phase == "timed":
        require(json.loads(process.stdout) == sample, "Raw serial sample changed")
    facts = qualify_worker(
        root,
        retained / "job",
        (retained / "request.json").read_bytes(),
        (retained / "stdout.txt").read_bytes(),
        sample["exit_code"],
        target["identity"]["probe"],
        value,
    )
    verify_measurement(sample)
    return {"measurement": sample, "documents": [facts], "overlap": None}


def copy_job(source: Path, destination: Path) -> None:
    before = common.inventory(source)
    require(
        sum(fact["bytes"] for fact in before.values()) <= 64 * 1024 * 1024 and len(before) <= 2048,
        "Unbounded worker output tree",
    )
    shutil.copytree(source, destination)
    require(common.inventory(destination) == before and common.inventory(source) == before, "Changed worker retention")


def retain_worker_streams(root: Path, measured: dict) -> None:
    """Retain available bounded outcomes before requiring either child to pass."""
    result = json.loads(measured["stdout"])
    require(
        result["schema"] == batch.CONTRACT and isinstance(result["workers"], list) and len(result["workers"]) <= 2,
        "Invalid worker output envelope",
    )
    seen = set()
    for worker in result["workers"]:
        index = worker["index"]
        require(type(index) is int and index in (0, 1) and index not in seen, "Invalid/duplicate output worker")
        seen.add(index)
        output = root / f"worker-{index}"
        for name in ("stdout", "stderr"):
            raw = base64.b64decode(worker[name + "_base64"], validate=True)
            require(
                len(raw) <= batch.MAX_STREAM and hashlib.sha256(raw).hexdigest() == worker[name + "_sha256"],
                "Unbounded or changed captured worker bytes",
            )
            (output / f"{name}.txt").write_bytes(raw)
        save(output / "process.json", worker)


def concurrent_attempt(args: argparse.Namespace, root: Path, entry: dict, value: dict, target: dict) -> dict:
    broker = [str(args.php), str(HERE / "native_batch_broker.php")]
    process = invoke(
        broker + ["prepare"],
        root,
        "prepare",
        {"state": batch_state(value, harness.fixture_identity(value)), "binary": target["binary"]},
    )
    require(process.returncode == 0 and not process.stderr, "Batch staging failed")
    prepared = json.loads(process.stdout)
    batch.validate_plan(prepared["plan"])
    account = sampler.resolve_engine_account()
    snapshots, bindings = [], []
    for worker in prepared["plan"]["workers"]:
        job, temporary = Path(worker["job"]), Path(worker["temporary"])
        binding = sampler.bind_native_api2_storage(job, temporary, account, require_pristine_job=True)
        bindings.append(binding)
        snapshots.append(sampler.native_api2_storage_snapshot(job, temporary, binding))
    save(root / "storage-pre.json", snapshots)
    process = invoke(broker + ["measure"], root, "measure", {"prepared": prepared})
    # Preserve raw failures first. Never launch an oracle after unknown cleanup.
    require(process.returncode == 0 and not process.stderr, "Batch measurement broker failed")
    measured = json.loads(process.stdout)
    require("error" not in measured, "Sampler could not establish bounded accounting")
    # A nonzero child/root status is not a successful observation, but a fully
    # validated clean drain makes immutable failure-tree retention safe.
    verify_measurement(measured, require_success=False)
    require(json.loads(measured["sampler_stdout"]) == measured["resource_usage"], "Raw sampler differs")
    after = []
    for worker, binding in zip(prepared["plan"]["workers"], bindings):
        job, temporary = Path(worker["job"]), Path(worker["temporary"])
        sampler.bind_native_api2_storage(job, temporary, account, expected=binding)
        after.append(sampler.native_api2_storage_snapshot(job, temporary, binding))
        output = root / f"worker-{worker['index']}"
        output.mkdir()
        copy_job(job, output / "job")
        (output / "request.json").write_bytes(worker["request"].encode())
    save(root / "storage-post.json", after)
    verify_storage(prepared, snapshots, after)
    retain_worker_streams(root, measured)
    result = verify_batch_envelope(prepared, measured, target["binary"])
    documents = []
    for worker in result["workers"]:
        output = root / f"worker-{worker['index']}"
        raw = (output / "stdout.txt").read_bytes()
        documents.append(
            qualify_worker(
                output,
                output / "job",
                (output / "request.json").read_bytes(),
                raw,
                worker["exit_code"],
                target["identity"]["probe"],
                value,
            )
        )
    # Safe cleanup only after identity-bound post-snapshot and exact retention.
    # Failure paths leave original storage for scoped service teardown/review.
    for worker, binding in zip(prepared["plan"]["workers"], bindings):
        job, temporary = Path(worker["job"]), Path(worker["temporary"])
        sampler.bind_native_api2_storage(job, temporary, account, expected=binding)
        require(job.parent.parent == Path(os.environ["PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT"]), "Unsafe cleanup root")
        shutil.rmtree(job.parent)
    outer = Path(prepared["cwd"])
    require(outer.parent == Path(os.environ["PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT"]), "Unsafe launcher cleanup root")
    sampler.validate_engine_temporary_directory(outer, account)
    shutil.rmtree(outer)
    return {"measurement": measured, "documents": documents, "overlap": result["overlap"]}


def aggregate(entries: list[dict], attempts: list[dict]) -> dict | None:
    if len(attempts) != len(entries) or any(not item["correct"] for item in attempts):
        return None
    result = {}
    for population in COUNTS:
        groups = {}
        for target in TARGETS:
            samples = [
                item["result"]
                for item in attempts
                if item["entry"]["phase"] == "timed"
                and item["entry"]["target_id"] == target
                and item["entry"]["population"] == population
            ]
            if len(samples) != COUNTS[population]["timed"]:
                return None
            multiplier = 1 if population == "serial" else 2
            require(all(len(sample["documents"]) == multiplier for sample in samples), "Wrong PDF denominator")
            metrics = {}
            for key in METRICS:
                values = [sample["measurement"][key] for sample in samples]
                require(all(type(v) in (float, int) and math.isfinite(v) and v >= 0 for v in values), "Invalid metric")
                metrics[key] = {
                    "p50": validate_result.percentile(values, 50),
                    "p95": validate_result.percentile(values, 95),
                    "min": min(values),
                    "max": max(values),
                    "mean": sum(values) / len(values),
                }
                if population == "serial":
                    metrics[key]["p99"] = validate_result.percentile(values, 99)
            groups[target] = {
                "observations": len(samples),
                "documents": len(samples) * multiplier,
                "metrics": metrics,
                "documents_per_sampler_lifecycle_second": len(samples)
                * multiplier
                * 1000
                / sum(sample["measurement"]["one_shot_wall_ms"] for sample in samples),
            }
        ratios = {
            key: (
                groups["v0.3.3"]["metrics"][key]["p50"] / groups["candidate"]["metrics"][key]["p50"]
                if groups["candidate"]["metrics"][key]["p50"] > 0
                else None
            )
            for key in METRICS
        }
        result[population] = {"targets": groups, "v033_over_candidate_p50": ratios}
    return result


def source_closure() -> dict:
    paths = {
        path
        for directory in (ROOT / "benchmarks/tools", ROOT / "benchmarks/integration", ROOT / "ports/pliego/tests")
        for path in directory.glob("*.py")
    }
    paths.update(
        (
            HERE / "native_batch_broker.php",
            ROOT / "benchmarks/runners/pliego.php",
            harness.DEFAULT_SCHEMA,
            ROOT / "benchmarks/manifest.toml",
            ROOT / "contracts/api2/test_contract.py",
        )
    )
    paths.update((ROOT / "contracts/api2").rglob("*.json"))
    return {path.relative_to(ROOT).as_posix(): digest(path) for path in sorted(paths)}


def verify_acceptance(acceptance: dict, identity_hash: str, preflight: Path) -> dict:
    require(
        isinstance(acceptance, dict)
        and set(acceptance)
        == {
            "schema",
            "identity_sha256",
            "preflight_directory",
            "preflight_manifest_sha256",
            "reviewed",
            "linux_lifecycle_controls_reviewed",
            "evidence",
        },
        "Wrong acceptance fields",
    )
    require(
        acceptance["schema"] == SCHEMA + ".acceptance"
        and acceptance["identity_sha256"] == identity_hash
        and acceptance["reviewed"] is True
        and acceptance["linux_lifecycle_controls_reviewed"] is True
        and isinstance(acceptance["evidence"], str)
        and acceptance["evidence"].strip(),
        "Unreviewed eligibility",
    )
    require(
        isinstance(acceptance["preflight_directory"], str)
        and PurePosixPath(acceptance["preflight_directory"]).is_absolute()
        and ".." not in PurePosixPath(acceptance["preflight_directory"]).parts,
        "Invalid preflight source pointer",
    )
    require(
        preflight.is_dir() and not preflight.is_symlink() and not preflight.is_junction(), "Missing or linked preflight"
    )
    require(digest(preflight / "manifest.json") == acceptance["preflight_manifest_sha256"], "Changed preflight")
    # Check the kind BEFORE recursion. A timed archive cannot reference itself
    # or another timed archive as its authority, even after hash resealing.
    require(read(preflight / "summary.json")["mode"] == "preflight", "Acceptance must reference a preflight")
    prior = verify(preflight)
    require(
        prior["identity_sha256"] == identity_hash and prior["completed"] is True,
        "Acceptance does not bind exact correct overlapping preflight",
    )
    return acceptance


def run(args: argparse.Namespace) -> None:
    require(sys.flags.optimize == 0, "Native qualification requires Python without -O")
    require(sys.platform == "linux" and os.getuid() == os.geteuid() == 0, "Linux root broker required")
    require(not args.out.exists() and args.out.parent.is_dir(), "Output must be fresh")
    args.out.mkdir(mode=0o700)
    account = sampler.resolve_engine_account()
    value, targets = fixture(), {}
    for name in TARGETS:
        binary = getattr(args, "candidate" if name == "candidate" else "baseline").resolve(strict=True)
        metadata_path = getattr(args, "candidate_metadata" if name == "candidate" else "baseline_metadata")
        destination = args.out / name
        destination.mkdir()
        metadata = read(metadata_path)
        save(destination / "package-identity.json", metadata)
        _, executable = sampler.command_identity([str(binary), "render-api2"], account)
        process = invoke([str(binary), "--contract-probe"], destination, "probe")
        require(process.returncode == 0 and not process.stderr, "Native contract probe failed")
        identity = target_identity(name, metadata, json.loads(process.stdout), digest(binary), binary.stat().st_size)
        info = binary.stat()
        targets[name] = {
            "path": str(binary),
            "identity": identity,
            "binary": {
                "path": str(binary),
                "sha256": identity["binary_sha256"],
                "bytes": info.st_size,
                "device": info.st_dev,
                "inode": info.st_ino,
            },
        }
        require(executable.sha256 == identity["binary_sha256"], "Native identity changed after hashing")
    require(
        targets["candidate"]["identity"]["source_sha"] != targets["v0.3.3"]["identity"]["source_sha"],
        "Candidate and baseline are the same source",
    )
    identity = {
        "targets": {name: target["identity"] for name, target in targets.items()},
        "fixture": value,
        "fixture_hashes": harness.fixture_identity(value),
        "source_files": source_closure(),
        "oracle_tools": harness.pdf_oracle_identity(),
        "python": harness.tool_version([sys.executable, "--version"]),
        "php": harness.tool_version([str(args.php), "--version"]),
        "interpreter_sha256": digest(Path("/usr/bin/python3").resolve()),
    }
    identity_hash = harness.canonical_json_sha256(identity)
    if args.mode == "timed":
        require(args.acceptance is not None, "Timed mode requires reviewed acceptance")
        acceptance = read(args.acceptance)
        preflight = Path(acceptance["preflight_directory"])
        verify_acceptance(acceptance, identity_hash, preflight)
        # Keep the exact small six-PDF preflight inside the timed archive;
        # original absolute source paths are provenance, not offline dependencies.
        copy_job(preflight, args.out / "acceptance-preflight")
        save(args.out / "acceptance.json", acceptance)
    entries = schedule(args.repeat, args.mode)
    save(args.out / "identity.json", identity)
    save(args.out / "targets.json", targets)
    save(args.out / "schedule.json", entries)
    attempts = []
    request_sha = None
    host = harness.host_info(False)  # platform.processor() may spawn uname.
    for entry in entries:
        root = args.out / f"attempt-{entry['position']:04d}"
        root.mkdir()
        item = {"entry": entry, "correct": False, "result": None, "error": None}
        try:
            action = serial_attempt if entry["population"] == "serial" else concurrent_attempt
            item["result"] = action(args, root, entry, value, targets[entry["target_id"]])
            hashes = {document["request_sha256"] for document in item["result"]["documents"]}
            require(
                len(hashes) == 1 and (request_sha is None or hashes == {request_sha}), "Requests are not byte-identical"
            )
            request_sha = next(iter(hashes))
            item["correct"] = True
        except (ValueError, OSError, KeyError, TypeError, RuntimeError, AssertionError) as failure:
            item["error"] = f"{type(failure).__name__}: {failure}"
        save(root / "attempt.json", item)
        attempts.append(item)
        if not item["correct"]:
            break  # No retry, no continuation after any failed/ambiguous attempt.
    completed = len(attempts) == len(entries) and all(item["correct"] for item in attempts)
    summary = {
        "schema": SCHEMA,
        "mode": args.mode,
        "repeat": args.repeat,
        "identity_sha256": identity_hash,
        "completed": completed,
        "scheduled_observations": len(entries),
        "attempted_observations": len(attempts),
        "scheduled_documents": sum(1 if e["population"] == "serial" else 2 for e in entries),
        "attempted_documents": sum(1 if i["entry"]["population"] == "serial" else 2 for i in attempts),
        "aggregate": aggregate(entries, attempts),
        "host": host,
        "evidence_class": "github-hosted-exploratory"
        if os.environ.get("GITHUB_ACTIONS") == "true"
        else "local-development",
        "github_run_id": os.environ.get("GITHUB_RUN_ID"),
        "scaling_claim": False,
    }
    save(args.out / "summary.json", summary)
    save(args.out / "manifest.json", common.inventory(args.out))
    print(json.dumps({"completed": completed, "output": str(args.out)}))
    require(completed, "Campaign stopped; full failed evidence retained")


def verify(root: Path) -> dict:
    require(sys.flags.optimize == 0, "Native qualification requires Python without -O")
    common.check_inventory(root, read(root / "manifest.json"), exclude={"manifest.json"})
    summary, identity, targets = read(root / "summary.json"), read(root / "identity.json"), read(root / "targets.json")
    require(
        summary["schema"] == SCHEMA and harness.canonical_json_sha256(identity) == summary["identity_sha256"],
        "Wrong campaign identity",
    )
    if summary["mode"] == "timed":
        verify_acceptance(read(root / "acceptance.json"), summary["identity_sha256"], root / "acceptance-preflight")
    else:
        require(
            not (root / "acceptance.json").exists() and not (root / "acceptance-preflight").exists(),
            "A preflight cannot carry recursive acceptance evidence",
        )
    require(
        identity["fixture"] == fixture() and tuple(identity["fixture_hashes"]) == FIXTURE_HASHES,
        "Wrong frozen corpus/correctness contract",
    )
    require(identity["source_files"] == source_closure(), "Use the campaign's exact pinned verifier/source")
    for name in TARGETS:
        current = targets[name]
        package = read(root / name / "package-identity.json")
        require(
            current["identity"] == identity["targets"][name]
            and read(root / name / "probe.stdout") == current["identity"]["probe"],
            "Unbound target identity",
        )
        require(
            current["identity"]
            == target_identity(
                name, package, current["identity"]["probe"], current["binary"]["sha256"], current["binary"]["bytes"]
            ),
            "Changed native package/probe binding",
        )
    entries = schedule(summary["repeat"], summary["mode"])
    require(entries == read(root / "schedule.json"), "Changed schedule")
    require(
        type(summary["attempted_observations"]) is int and 0 <= summary["attempted_observations"] <= len(entries),
        "Invalid attempted denominator",
    )
    attempts = []
    groups = set()
    request_hashes = set()
    for position in range(summary["attempted_observations"]):
        folder = root / f"attempt-{position:04d}"
        item = read(folder / "attempt.json")
        require(item["entry"] == entries[position], "Attempt is not the scheduled prefix")
        require(not attempts or attempts[-1]["correct"], "Campaign continued after failure")
        if item["correct"]:
            measurement = item["result"]["measurement"]
            verify_measurement(measurement)
            group = measurement["resource_usage"]["cgroup_path"]
            require(group not in groups, "Reused measurement cgroup")
            groups.add(group)
            target = targets[item["entry"]["target_id"]]
            if item["entry"]["population"] == "concurrency-2":
                prepared, measured = read(folder / "prepare.stdout"), read(folder / "measure.stdout")
                require(measured == measurement, "Raw batch measurement changed")
                result = verify_batch_envelope(prepared, measured, target["binary"])
                require(result["overlap"] == item["result"]["overlap"], "Changed overlap summary")
                verify_storage(prepared, read(folder / "storage-pre.json"), read(folder / "storage-post.json"))
                roots = [folder / f"worker-{worker['index']}" for worker in result["workers"]]
                for output, native, planned in zip(roots, result["workers"], prepared["plan"]["workers"]):
                    require(
                        (output / "request.json").read_bytes() == planned["request"].encode()
                        and (output / "stdout.txt").read_bytes()
                        == base64.b64decode(native["stdout_base64"], validate=True)
                        and read(output / "process.json") == native,
                        "Changed native worker evidence",
                    )
            else:
                index = measurement["index"]
                retained, sample, _ = common.inspect_retention(
                    folder,
                    index,
                    item["entry"]["phase"],
                    target["identity"]["binary_sha256"],
                    tuple(identity["fixture_hashes"]),
                )
                require(sample == measurement, "Raw serial sample changed")
                require(
                    measurement["resource_usage"]["launch_security"]["argv"] == [target["path"], "render-api2"]
                    and measurement["resource_usage"]["launch_security"]["executable"]["sha256"]
                    == target["identity"]["binary_sha256"],
                    "Wrong measured native root",
                )
                require(read(folder / "runner.process.json")["exit_code"] == 0, "Raw serial process failed")
                if item["entry"]["phase"] == "timed":
                    require(read(folder / "runner.stdout") == sample, "Raw serial runner sample differs")
                roots = [retained]
            require(len(roots) == len(item["result"]["documents"]), "Wrong document count")
            for worker, facts in zip(roots, item["result"]["documents"]):
                payload, raw = (worker / "request.json").read_bytes(), (worker / "stdout.txt").read_bytes()
                verify_input(payload, worker / "job")
                require(hashlib.sha256(payload).hexdigest() == facts["request_sha256"], "Changed request identity")
                request_hashes.add(facts["request_sha256"])
                require(len(request_hashes) == 1, "Requests are not byte-identical across targets/populations")
                protocol = contracts.validate_result(raw, 0, payload, target["identity"]["probe"], worker / "job")
                pdf = worker / "job/delivery/document.pdf"
                require(
                    (worker / "stderr.txt").read_bytes() == b""
                    and protocol == facts["protocol"]
                    and digest(pdf) == facts["pdf_sha256"]
                    and pdf.stat().st_size == facts["pdf_bytes"],
                    "Changed native output",
                )
                oracle_root = folder if len(roots) == 1 else worker
                require(
                    read(oracle_root / "oracle.stdout") == facts["oracle"]
                    and facts["oracle"]["pass"] is True
                    and read(oracle_root / "oracle.process.json")["exit_code"] == 0
                    and (oracle_root / "oracle.stderr").read_bytes() == b"",
                    "Wrong retained PDF oracle",
                )
                # Re-sealing hashes cannot turn a corrupted PDF into a passing
                # oracle record. This executes only the pinned read-only oracle.
                checked = subprocess.run(oracle_command(identity["fixture"], pdf), capture_output=True, timeout=180)
                require(
                    checked.returncode == 0 and not checked.stderr and json.loads(checked.stdout) == facts["oracle"],
                    "Retained PDF no longer passes the exact oracle",
                )
        attempts.append(item)
    require(
        summary["completed"] == (len(attempts) == len(entries) and all(item["correct"] for item in attempts)),
        "Incorrect completion claim",
    )
    require(summary["aggregate"] == aggregate(entries, attempts), "Changed aggregate or denominator")
    require(
        summary["scheduled_observations"] == len(entries)
        and summary["scheduled_documents"] == sum(1 if e["population"] == "serial" else 2 for e in entries)
        and summary["attempted_documents"] == sum(1 if i["entry"]["population"] == "serial" else 2 for i in attempts)
        and summary["scaling_claim"] is False,
        "Incorrect denominator or scaling claim",
    )
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", type=Path)
    parser.add_argument("--mode", choices=("preflight", "timed"), default="preflight")
    parser.add_argument("--repeat", type=int, choices=(1, 2, 3), default=1)
    for name in ("candidate", "candidate-metadata", "baseline", "baseline-metadata", "php", "out", "acceptance"):
        parser.add_argument("--" + name, type=Path)
    args = parser.parse_args()
    if args.verify:
        print(json.dumps(verify(args.verify)))
    else:
        require(
            all(
                getattr(args, name) is not None
                for name in ("candidate", "candidate_metadata", "baseline", "baseline_metadata", "php", "out")
            ),
            "Missing run paths",
        )
        args.out = args.out.absolute()
        run(args)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, RuntimeError) as failure:
        print(f"native-regression: {failure}", file=sys.stderr)
        raise SystemExit(1)
