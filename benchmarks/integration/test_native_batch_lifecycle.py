#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Finite Linux lifecycle controls for the fixed launcher, never native/PDF timing.

The bound executable is the actual immutable CPython ELF. Its required
`render-api2` argument names a generated synthetic script in each owned cwd.
The script is not a renderer: no successful PDF/contract claim is made here.
"""

from __future__ import annotations

import argparse
import base64
import contextlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest

import run_native_regression as campaign

require, read, save = campaign.require, campaign.read, campaign.save
CASES = ("one-failure", "one-hang", "early-root-death", "overflow", "busy", "recovery")
SCHEMA = "pliego.native-batch-lifecycle.v1"


def worker_script(case: str, index: int) -> str:
    require(case in CASES and type(index) is int and index in (0, 1), "Unknown fixed lifecycle case")
    return f"""import json, os, signal, subprocess, sys, time
from pathlib import Path
def remember(pid, name):
    raw = Path('/proc', str(pid), 'stat').read_text()
    fields = raw[raw.rfind(')') + 2:].split()
    with Path(name).open('x') as stream:
        json.dump({{'pid':pid, 'start_ticks':int(fields[19])}}, stream)
        stream.flush()
        os.fsync(stream.fileno())
remember(os.getpid(), 'worker.pid.json')
sys.stdin.buffer.read()
case, index = {case!r}, {index}
if case == 'early-root-death' and index == 0:
    child = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(30)'],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    remember(child.pid, 'descendant.pid.json')
    os.kill(os.getppid(), signal.SIGKILL)
    time.sleep(30)
elif case == 'one-hang' and index == 0:
    time.sleep(30)
elif case == 'overflow' and index == 0:
    remaining = {campaign.batch.MAX_STREAM + 65536}
    while remaining:
        remaining -= os.write(1, b'x' * min(65536, remaining))
else:
    # Deliberate synthetic duration only; real-native preflight has no delay
    # injection, retry, fixture change or manufactured overlap.
    time.sleep(0.1)
    print('synthetic-worker-ok', flush=True)
    raise SystemExit(1 if case == 'one-failure' and index == 0 else 0)
"""


def process_gone(identity: dict) -> bool:
    actual = campaign.sampler.read_stat(identity["pid"])
    if actual is None or actual["start_ticks"] != identity["start_ticks"]:
        return True
    try:
        raw = Path("/proc", str(identity["pid"]), "stat").read_text()
    except FileNotFoundError:
        return True
    fields = raw[raw.rfind(")") + 2 :].split()
    return int(fields[19]) != identity["start_ticks"] or fields[0] == "Z"


def run_case(case: str, root: Path, php: Path, binary: Path) -> dict:
    sampler = campaign.sampler
    from test_process_tree_sampler import live_output_capture_paths, assert_exact_counters, assert_validator_accepts

    account = sampler.resolve_engine_account()
    parent = Path(os.environ["PLIEGO_BENCHMARK_CGROUP_PARENT"])
    before = sorted(path.name for path in parent.iterdir() if path.is_dir())
    info = binary.stat()
    native = {
        "path": str(binary),
        "sha256": campaign.digest(binary),
        "bytes": info.st_size,
        "device": info.st_dev,
        "inode": info.st_ino,
    }
    process = campaign.invoke(
        [str(php), str(campaign.HERE / "native_batch_broker.php"), "prepare"],
        root,
        "prepare",
        {"state": campaign.batch_state(campaign.fixture(), campaign.FIXTURE_HASHES), "binary": native},
    )
    require(process.returncode == 0 and not process.stderr, "Synthetic staging failed")
    prepared = json.loads(process.stdout)
    bindings = []
    for worker in prepared["plan"]["workers"]:
        job, temporary = Path(worker["job"]), Path(worker["temporary"])
        binding = sampler.bind_native_api2_storage(job, temporary, account, require_pristine_job=True)
        bindings.append(binding)
        script = job / "render-api2"
        with script.open("x") as stream:
            stream.write(worker_script(case, worker["index"]))
            stream.flush()
            os.fsync(stream.fileno())
        os.chown(script, account.uid, account.gid)
        script.chmod(0o600)
    request = root / "batch-input.json"
    with request.open("xb") as stream:
        stream.write(campaign.harness.canonical_json_bytes(prepared["plan"]) + b"\n")
        stream.flush()
        os.fsync(stream.fileno())
    request.chmod(0o400)
    limit = 1000 if case == "one-hang" else 5000
    with live_output_capture_paths() as (stdout, stderr):
        command = [
            "/usr/bin/python3",
            "-I",
            str(campaign.ROOT / "benchmarks/tools/process_tree_sampler.py"),
            "--cwd",
            prepared["cwd"],
            "--temporary-directory",
            prepared["temporary"],
            "--stdout",
            str(stdout),
            "--stderr",
            str(stderr),
            "--stdin",
            str(request),
            "--isolate-network",
            "--root-wall-timeout-ms",
            str(limit),
            "--descendant-grace-ms",
            "50",
            "--",
            str(campaign.HERE / "native_batch_launcher.py"),
            "render",
            "paired-minimal-static",
            "--artifacts",
            prepared["artifacts"],
        ]
        save(root / "sampler.command.json", command)
        lock = sampler.exclusive_parent_lock(parent) if case == "busy" else contextlib.nullcontext()
        with lock:
            try:
                process = subprocess.run(command, capture_output=True, timeout=20)
            except subprocess.TimeoutExpired as failure:
                (root / "sampler.stdout").write_bytes(failure.stdout or b"")
                (root / "sampler.stderr").write_bytes(failure.stderr or b"")
                save(root / "sampler.process.json", {"outcome": "outer-timeout", "cleanup_verified": False})
                raise campaign.common.FatalInfrastructureError(
                    "Lifecycle outer timeout; stop before another process"
                ) from failure
        (root / "sampler.stdout").write_bytes(process.stdout)
        (root / "sampler.stderr").write_bytes(process.stderr)
        save(
            root / "sampler.process.json",
            {"exit_code": process.returncode, "outer_seconds": 20, "root_limit_ms": limit},
        )
        # Only known, scoped sampler cleanup permits reading finished job trees.
        after = sorted(path.name for path in parent.iterdir() if path.is_dir())
        require(after == before and b"CGROUP_CLEANUP_FAILED" not in process.stderr, "Lifecycle cgroup cleanup changed")
        expected_error = "ROOT_WALL_TIMEOUT" if case == "one-hang" else "CGROUP_BUSY" if case == "busy" else None
        if expected_error:
            require(
                process.returncode == 2
                and process.stdout == b""
                and f"measurement-incomplete[{expected_error}]".encode() in process.stderr,
                "Wrong lifecycle failure",
            )
            proof = None
        else:
            require(process.returncode == 0 and process.stderr == b"", "Sampler control failed")
            proof = json.loads(process.stdout)
            require(proof["cgroup_drained"] is True, "Undrained control cgroup")
            assert_exact_counters(proof)
            assert_validator_accepts(proof, case == "recovery")
        (root / "launcher.stdout").write_bytes(stdout.read_bytes())
        (root / "launcher.stderr").write_bytes(stderr.read_bytes())
    identities = []
    for worker, binding in zip(prepared["plan"]["workers"], bindings):
        job, temporary = Path(worker["job"]), Path(worker["temporary"])
        sampler.bind_native_api2_storage(job, temporary, account, expected=binding)
        for pid_file in job.glob("*.pid.json"):
            identity = read(pid_file)
            require(process_gone(identity), "Known synthetic process survived scoped cleanup")
            identities.append(identity)
        retained = root / f"worker-{worker['index']}"
        retained.mkdir()
        campaign.copy_job(job, retained / "job")
    if case in {"one-failure", "overflow", "recovery"}:
        launcher = read(root / "launcher.stdout")
        require(
            len(launcher["workers"]) == 2 and proof["cleanup"]["kill_used"] is False, "Missing clean two-worker outcome"
        )
        campaign.retain_worker_streams(root, {"stdout": (root / "launcher.stdout").read_text()})
        if case == "one-failure":
            require(
                proof["exit_code"] == 1 and [w["exit_code"] for w in launcher["workers"]] == [1, 0],
                "Failure control changed",
            )
        elif case == "overflow":
            require(proof["exit_code"] == 1 and "exceeded its bound" in launcher["error"], "Overflow not rejected")
            require(any(w["streams_complete"] is False for w in launcher["workers"]), "Overflow disguised as complete")
            require(
                all(
                    len(base64.b64decode(w[s + "_base64"])) <= campaign.batch.MAX_STREAM
                    for w in launcher["workers"]
                    for s in ("stdout", "stderr")
                ),
                "Overflow capture exceeded cap",
            )
        else:
            require(
                proof["exit_code"] == 0
                and launcher["error"] is None
                and all(w["exit_code"] == 0 and w["streams_complete"] is True for w in launcher["workers"]),
                "Recovery failed",
            )
    elif case == "early-root-death":
        require(
            proof["signal"] == 9 and proof["cleanup"]["kill_used"] is True and identities,
            "Early root death did not exercise descendant cleanup",
        )
    elif case == "busy":
        require(not identities, "Competing sampler launched a synthetic worker")
    else:
        require(identities, "Hung control never started its child")
    facts = {
        "schema": SCHEMA,
        "case": case,
        "passed": True,
        "synthetic_only": True,
        "cgroup_children_before": before,
        "cgroup_children_after": after,
        "gone_processes": identities,
        "launcher_sha256": campaign.digest(campaign.HERE / "native_batch_launcher.py"),
        "bound_executable": native,
        "root_limit_ms": limit,
        "native_benchmark_root_limit_ms_unchanged": 65000,
    }
    save(root / "result.json", facts)
    save(root / "manifest.json", campaign.common.inventory(root))
    for worker, binding in zip(prepared["plan"]["workers"], bindings):
        job, temporary = Path(worker["job"]), Path(worker["temporary"])
        sampler.bind_native_api2_storage(job, temporary, account, expected=binding)
        require(job.parent.parent == Path(os.environ["PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT"]), "Unsafe synthetic cleanup")
        shutil.rmtree(job.parent)
    outer = Path(prepared["cwd"])
    require(outer.parent == Path(os.environ["PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT"]), "Unsafe launcher cleanup")
    sampler.validate_engine_temporary_directory(outer, account)
    shutil.rmtree(outer)
    return facts


class SourceTests(unittest.TestCase):
    def test_fixed_synthetic_programs_compile_and_unknown_cases_fail(self) -> None:
        for case in CASES:
            for index in (0, 1):
                compile(worker_script(case, index), "synthetic-render-api2", "exec")
        with self.assertRaises(ValueError):
            worker_script("other", 0)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--php", type=Path)
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SourceTests)
        require(unittest.TextTestRunner().run(suite).wasSuccessful(), "Lifecycle source tests failed")
        return
    require(__debug__, "Existing sampler control assertions require normal Python, not -O")
    require(sys.platform == "linux" and os.geteuid() == 0, "Lifecycle execution requires isolated Linux root broker")
    require(
        args.out is not None and args.php is not None and not args.out.exists() and args.out.parent.is_dir(),
        "Fresh output/php required",
    )
    args.out.mkdir(mode=0o700)
    binary = Path("/usr/bin/python3").resolve(strict=True)
    campaign.sampler.command_identity([str(binary), "render-api2"], campaign.sampler.resolve_engine_account())
    results = []
    for case in CASES:
        root = args.out / case
        root.mkdir(mode=0o700)
        results.append(run_case(case, root, args.php, binary))
    save(args.out / "report.json", {"schema": SCHEMA, "passed": True, "synthetic_only": True, "results": results})
    save(args.out / "manifest.json", campaign.common.inventory(args.out))
    print(json.dumps({"passed": True, "controls": len(results), "synthetic_only": True}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, RuntimeError, AssertionError, KeyError, TypeError) as failure:
        print(f"native-batch-lifecycle: {failure}", file=sys.stderr)
        raise SystemExit(1)
