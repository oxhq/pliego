#!/usr/bin/python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fixed unprivileged two-native-process batch; not a general command runner."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import select
import stat
import subprocess
import sys
import time
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "benchmarks/tools"))
if sys.platform == "linux":
    import process_tree_sampler as sampler
else:
    sampler = None  # Pure validation tests are portable; execution is Linux-only.

CONTRACT = "pliego.native-two-job-batch.v1"
MAX_STREAM = 2 * 1024 * 1024


class OverlapUnavailable(ValueError):
    pass


def require(value: Any, message: str) -> None:
    if not value:
        raise ValueError(message)


def native_identity(binary: dict) -> tuple[int, int]:
    require(set(binary) == {"path", "sha256", "bytes", "device", "inode"}, "Wrong binary identity fields")
    path = Path(binary["path"])
    require(path.is_absolute() and path.resolve(strict=True) == path, "Noncanonical native binary")
    info = path.stat()
    require(stat.S_ISREG(info.st_mode) and not os.access(path, os.W_OK), "Native executable is mutable")
    require(
        (info.st_dev, info.st_ino, info.st_size) == (binary["device"], binary["inode"], binary["bytes"]),
        "Native executable identity changed",
    )
    # The root broker hashes immutable bytes outside measurement. Do not add a
    # full executable rehash to every timed native launch.
    require(
        isinstance(binary["sha256"], str) and re.fullmatch(r"[0-9a-f]{64}", binary["sha256"]),
        "Missing native byte identity",
    )
    return info.st_dev, info.st_ino


def validate_plan(plan: Any) -> None:
    require(isinstance(plan, dict) and set(plan) == {"schema", "binary", "workers"}, "Wrong batch fields")
    require(
        plan["schema"] == CONTRACT and isinstance(plan["workers"], list) and len(plan["workers"]) == 2,
        "Batch must contain exactly two workers",
    )
    require(all(isinstance(worker, dict) for worker in plan["workers"]), "Invalid workers")
    require(
        [worker.get("index") for worker in plan["workers"]] == [0, 1]
        and all(type(worker["index"]) is int for worker in plan["workers"]),
        "Wrong worker order",
    )
    paths = []
    for worker in plan["workers"]:
        require(set(worker) == {"index", "job", "temporary", "request", "request_sha256"}, "Wrong worker fields")
        require(isinstance(worker["request"], str), "Request is not text")
        request = worker["request"].encode("utf-8")
        require(0 < len(request) <= 4096 and request.endswith(b"\n"), "Unbounded/noncanonical request")
        require(hashlib.sha256(request).hexdigest() == worker["request_sha256"], "Changed request")
        for key in ("job", "temporary"):
            path = Path(worker[key])
            require(path.is_absolute() and path.resolve(strict=True) == path and path.is_dir(), "Unsafe worker path")
            paths.append(path)
        require(
            paths[-2].name == "job" and paths[-1].name == "temporary" and paths[-2].parent == paths[-1].parent,
            "Workers require job/temporary siblings",
        )
    require(len(set(paths)) == 4 and paths[0].parent != paths[2].parent, "Worker storage is not independent")
    require(plan["workers"][0]["request"] == plan["workers"][1]["request"], "Batch inputs are not paired")


def overlap_witness(processes: list[subprocess.Popen], pidfds: list[int], expected: tuple[int, int]) -> dict:
    parent_group = sampler.cgroup_relative()
    poller = select.poll()
    for pidfd in pidfds:
        poller.register(pidfd, select.POLLIN)
    observed = []
    for process in processes:
        try:
            info = (Path("/proc") / str(process.pid) / "exe").stat()
        except FileNotFoundError as failure:
            raise OverlapUnavailable("Worker exited before overlap witness") from failure
        require((info.st_dev, info.st_ino) == expected, "Worker did not exec the bound native binary")
        process_stat = sampler.read_stat(process.pid)
        if process_stat is None:
            raise OverlapUnavailable("Worker disappeared before overlap witness")
        group = sampler.cgroup_relative(str(process.pid))
        require(group == parent_group, "Native worker escaped the batch cgroup")
        observed.append(
            {
                "pid": process.pid,
                "start_ticks": process_stat["start_ticks"],
                "cgroup": group,
                "executable_device": info.st_dev,
                "executable_inode": info.st_ino,
            }
        )
    require(len({item["pid"] for item in observed}) == 2, "Duplicate native PID")
    at = time.monotonic_ns()
    if poller.poll(0):
        raise OverlapUnavailable("No live native overlap witness")
    return {"observed": True, "at_monotonic_ns": at, "workers": observed}


def launch(plan: dict) -> dict:
    require(sys.platform == "linux" and os.geteuid() != 0, "Batch launcher requires the unprivileged Linux account")
    validate_plan(plan)
    expected = native_identity(plan["binary"])
    processes: list[subprocess.Popen] = []
    pidfds: list[int] = []
    records = []
    selector = selectors.DefaultSelector()
    witness = None
    error = None
    try:
        for worker in plan["workers"]:
            environment = {**os.environ, "TMPDIR": worker["temporary"]}
            started = time.monotonic_ns()
            process = subprocess.Popen(
                [plan["binary"]["path"], "render-api2"],
                cwd=worker["job"],
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
            )
            processes.append(process)
            pidfd = os.pidfd_open(process.pid)
            pidfds.append(pidfd)
            record = {
                "index": worker["index"],
                "pid": process.pid,
                "started_monotonic_ns": started,
                "exited_monotonic_ns": None,
                "exit_code": None,
                "streams_complete": False,
                "stdout": bytearray(),
                "stderr": bytearray(),
            }
            records.append(record)
            # The frozen minimal request is below PIPE_BUF; no large input is
            # written while the other worker's output remains undrained.
            process.stdin.write(worker["request"].encode("utf-8"))
            process.stdin.close()
            selector.register(pidfd, selectors.EVENT_READ, (worker["index"], "exit"))
            for name, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, (worker["index"], name))
        try:
            witness = overlap_witness(processes, pidfds, expected)
        except OverlapUnavailable as unavailable:
            # A fast/failed native child is still a real result. Retain both
            # outcomes, but the coordinator cannot qualify this batch.
            witness = {"observed": False, "reason": str(unavailable)}
        while selector.get_map():
            for key, _ in selector.select():
                index, kind = key.data
                record = records[index]
                if kind == "exit":
                    record["exit_code"] = processes[index].wait()
                    record["exited_monotonic_ns"] = time.monotonic_ns()
                    selector.unregister(key.fileobj)
                    continue
                payload = os.read(key.fd, 65536)
                if not payload:
                    selector.unregister(key.fileobj)
                    key.fileobj.close()
                else:
                    remaining = MAX_STREAM - len(record[kind])
                    record[kind].extend(payload[:remaining])
                    require(len(payload) <= remaining, "Worker output exceeded its bound")
        for record in records:
            record["streams_complete"] = True
        native_identity(plan["binary"])
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as failure:
        error = str(failure)
        for process in processes:
            if process.poll() is None:
                process.kill()
        for process in processes:
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                # The outer sampler owns cgroup-wide termination/drain. Never
                # turn an incomplete local cleanup into a successful batch.
                error += "; direct child did not reap"
    finally:
        selector.close()
        for descriptor in pidfds:
            os.close(descriptor)
        for process in processes:
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream is not None:
                    stream.close()
    for index, record in enumerate(records):
        if record["exit_code"] is None:
            record["exit_code"] = processes[index].returncode
        for name in ("stdout", "stderr"):
            raw = bytes(record.pop(name))
            record[name + "_base64"] = base64.b64encode(raw).decode("ascii")
            record[name + "_sha256"] = hashlib.sha256(raw).hexdigest()
    return {"schema": CONTRACT, "overlap": witness, "workers": records, "error": error}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["render"])
    parser.add_argument("input", choices=["paired-minimal-static"])
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.parse_args()
    raw = sys.stdin.buffer.read(32769)
    require(len(raw) <= 32768, "Batch input exceeded its bound")
    plan = json.loads(raw)
    result = launch(plan)
    result["plan_sha256"] = hashlib.sha256(raw).hexdigest()
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0 if result["error"] is None and all(item["exit_code"] == 0 for item in result["workers"]) else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, TypeError) as failure:
        print(f"native_batch_launcher: {failure}", file=sys.stderr)
        raise SystemExit(2)
