#!/usr/bin/env python3

# SPDX-License-Identifier: MIT
# xfail-license: The Pliego PHP SDK is MIT-licensed; see ../LICENSE.

"""Linux-only real API 2 forced-cancellation proof. No PID/name-based signal fallback."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import select
import signal
import subprocess
import sys
import time

PROC = Path("/proc")
OUTER_SECONDS = 180
SAMPLE_SECONDS = 0.02  # Census sampling only; never used as evidence of termination.


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


@dataclass(frozen=True)
class Stat:
    pid: int
    comm: str
    state: str
    ppid: int
    user_ticks: int
    system_ticks: int
    start_ticks: int

    @property
    def identity(self) -> tuple[int, int]:
        return self.pid, self.start_ticks


def parse_stat(raw: str) -> Stat:
    """Linux fields 1, 2, 3, 4, 14, 15 and 22; comm can itself contain ')'."""
    require(len(raw) <= 4096, "/proc stat exceeds bound")
    prefix, separator, suffix = raw.rpartition(") ")
    pid, opening, comm = prefix.partition(" (")
    fields = suffix.split()
    require(bool(separator and opening) and len(fields) >= 20, "malformed /proc stat")
    row = Stat(int(pid), comm, fields[0], int(fields[1]), int(fields[11]), int(fields[12]), int(fields[19]))
    require(row.pid > 0 and row.ppid >= 0 and row.start_ticks >= 0, "invalid process identity")
    require(len(row.state) == 1 and min(row.user_ticks, row.system_ticks) >= 0, "invalid process counters")
    return row


def read_stat(path: Path) -> Stat:
    with path.open(encoding="utf-8", errors="replace") as source:
        return parse_stat(source.read(4097))


def lineage(rows: dict[int, Stat], pid: int, root: tuple[int, int]) -> list[tuple[int, int]]:
    """Return child-to-root identities only for a complete, noncyclic, current ancestry."""
    chain: list[tuple[int, int]] = []
    seen: set[int] = set()
    while pid in rows and pid not in seen:
        row = rows[pid]
        chain.append(row.identity)
        if pid == root[0]:
            return chain if row.identity == root and len(chain) > 1 else []
        seen.add(pid)
        parent = rows.get(row.ppid)
        if parent is not None and parent.start_ticks > row.start_ticks:
            return []
        pid = row.ppid
    return []


def is_cancel_render(meta: dict, binary_identity: tuple[int, int], cancel_root: str) -> bool:
    if tuple(meta["exe_identity"]) != binary_identity or meta["argv"][1:] != ["render-api2"]:
        return False
    try:
        relative = PurePosixPath(meta["cwd"]).relative_to(PurePosixPath(cancel_root))
    except ValueError:
        return False
    return (
        len(relative.parts) == 2
        and re.fullmatch("[0-9a-f]{32}", relative.parts[0]) is not None
        and relative.parts[1] == "runtime"
    )


def script_cpu_growth(first: Stat, last: Stat, minimum_ticks: int) -> bool:
    return (
        first.identity == last.identity
        and first.comm == last.comm
        and first.comm.startswith("Script#")
        and last.user_ticks + last.system_ticks - first.user_ticks - first.system_ticks >= minimum_ticks
    )


def terminated(fd: int) -> bool:
    return bool(select.select([fd], [], [], 0)[0])


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def metadata(pid: int) -> dict:
    directory = PROC / str(pid)
    exe = (directory / "exe").stat()
    with (directory / "cmdline").open("rb") as source:
        raw = source.read(8193)
    require(len(raw) <= 8192 and raw.endswith(b"\0"), "owned process command line is not bounded/NUL terminated")
    return {
        "exe": os.readlink(directory / "exe"),
        "exe_identity": [exe.st_dev, exe.st_ino],
        "argv": [os.fsdecode(part) for part in raw[:-1].split(b"\0")],
        "cwd": os.readlink(directory / "cwd"),
    }


@dataclass
class Pinned:
    stat: Stat
    fd: int
    ancestry: list[tuple[int, int]]
    meta: dict


def pin(row: Stat, ancestry: list[tuple[int, int]]) -> Pinned:
    fd = os.pidfd_open(row.pid)
    try:
        current = read_stat(PROC / str(row.pid) / "stat")
        meta = metadata(row.pid)
        if current.identity != row.identity or terminated(fd):
            raise ProcessLookupError("process changed/exited while acquiring pidfd")
        return Pinned(current, fd, ancestry, meta)
    except BaseException:
        os.close(fd)
        raise


class Proof:
    def __init__(self, php: Path, binary: Path, root: Path):
        self.root, self.binary = root, binary
        self.started = time.monotonic()
        self.owned: dict[tuple[int, int], Pinned] = {}
        self.phases: list[str] = []
        self.buffer = b""
        self.journal = (root / "process-census.jsonl").open("x", encoding="utf-8")
        self.stderr = (root / "php.stderr").open("xb")
        self.stdout = (root / "php.stdout").open("xb")
        driver = Path(__file__).with_name("production_cancellation_test.php")
        self.child = subprocess.Popen(
            [str(php), str(driver), str(binary), str(root / "php")],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            start_new_session=True,
        )
        try:
            self.php = pin(read_stat(PROC / str(self.child.pid) / "stat"), [])
            supplied_php = php.stat()
            require(
                tuple(self.php.meta["exe_identity"]) == (supplied_php.st_dev, supplied_php.st_ino),
                "owned PHP process does not match supplied executable identity",
            )
            self.owned[self.php.stat.identity] = self.php
            self.event("owned-php", self.php)
        except BaseException:
            # PHP cannot spawn anything until START. An unreaped Popen child cannot
            # have its PID reused; still acquire a pidfd before cleanup signaling.
            fd = os.pidfd_open(self.child.pid)
            try:
                if not terminated(fd):
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                self.child.wait(timeout=5)
            finally:
                os.close(fd)
                for stream in [self.child.stdin, self.child.stdout, self.journal, self.stderr, self.stdout]:
                    stream.close()
            raise

    def event(self, name: str, process: Pinned | None = None, **values) -> None:
        record = {"event": name, "elapsed_seconds": time.monotonic() - self.started, **values}
        if process is not None:
            record.update({"stat": asdict(process.stat), "ancestry": process.ancestry, "process": process.meta})
        self.journal.write(json.dumps(record, sort_keys=True) + "\n")
        self.journal.flush()

    def census(self) -> dict[int, Stat]:
        rows = {}
        for path in PROC.iterdir():
            if path.name.isdecimal():
                try:
                    row = read_stat(path / "stat")
                    rows[row.pid] = row
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    continue
        require(len(rows) <= 16384, "runner process census exceeds declared bound")
        for row in rows.values():
            chain = lineage(rows, row.pid, self.php.stat.identity)
            if not chain or row.state == "Z":
                continue
            if row.identity not in self.owned:
                require(len(self.owned) < 64, "owned process census exceeds 64 identities")
                try:
                    process = pin(row, chain)
                except (FileNotFoundError, ProcessLookupError):
                    self.event("transient-before-pidfd", identity=row.identity)
                    continue
                self.owned[row.identity] = process
                self.event("observed-child", process)
            process = self.owned[row.identity]
            if not terminated(process.fd):
                try:
                    meta = metadata(row.pid)
                    if not terminated(process.fd) and meta != process.meta:
                        process.meta = meta
                        self.event("owned-exec-or-cwd-change", process)
                except (FileNotFoundError, ProcessLookupError):
                    pass
        return rows

    def pump(self, deadline: float) -> dict[int, Stat]:
        require(time.monotonic() < deadline, "phase deadline exceeded")
        rows = self.census()
        readable = select.select(
            [self.child.stdout, self.php.fd], [], [], max(0, min(SAMPLE_SECONDS, deadline - time.monotonic()))
        )[0]
        if self.child.stdout in readable:
            data = os.read(self.child.stdout.fileno(), 4096)
            self.stdout.write(data)
            self.stdout.flush()
            self.buffer += data
            require(len(self.buffer) <= 8192, "PHP event frame exceeds bound")
            while b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                event = json.loads(line)
                require(
                    set(event) == {"phase", "php_pid"} and event["php_pid"] == self.child.pid,
                    "PHP event identity mismatch",
                )
                self.event("php-phase", phase=event["phase"])
                require(event["phase"] != "failed", "PHP driver failed; see php/report.json and php.stderr")
                expected = ["boot", "ready", "rendering", "cancelled", "passed"]
                require(
                    len(self.phases) < len(expected) and event["phase"] == expected[len(self.phases)],
                    "PHP phase sequence drift",
                )
                self.phases.append(event["phase"])
        return rows

    def phase(self, name: str, seconds: float) -> None:
        deadline = time.monotonic() + seconds
        while name not in self.phases:
            self.pump(deadline)
            require(not terminated(self.php.fd) or name in self.phases, f"PHP exited before {name}")

    def send(self, text: str) -> None:
        require(not terminated(self.php.fd), "PHP caller must remain alive")
        self.child.stdin.write((text + "\n").encode())
        self.child.stdin.flush()

    def signal(self, process: Pinned, value: int, reason: str) -> None:
        if not terminated(process.fd):
            signal.pidfd_send_signal(process.fd, value)
            self.event("signal", process, signal=value, reason=reason)

    def native_members(self, native: Pinned, rows: dict[int, Stat]) -> dict[tuple[int, int], Pinned]:
        members = {native.stat.identity: native}
        for process in self.owned.values():
            current = rows.get(process.stat.pid)
            if native.stat.identity in process.ancestry or (
                current is not None
                and current.identity == process.stat.identity
                and lineage(rows, process.stat.pid, native.stat.identity)
            ):
                members[process.stat.identity] = process
        return members

    def run(self) -> dict:
        self.phase("boot", 5)
        self.send("START")
        self.phase("ready", 80)
        self.send("RUN")
        self.phase("rendering", 2)
        binary_stat = self.binary.stat()
        binary_identity = (binary_stat.st_dev, binary_stat.st_ino)
        native = None
        first_threads: dict[tuple[int, int], Stat] = {}
        cpu = None
        deadline = time.monotonic() + 20
        minimum_ticks = max(1, math.ceil(os.sysconf("SC_CLK_TCK") * 0.15))
        while cpu is None and time.monotonic() < deadline:
            self.pump(deadline)
            require(not terminated(self.php.fd), "PHP exited before active native observation")
            candidates = [
                p
                for p in self.owned.values()
                if p is not self.php
                and not terminated(p.fd)
                and is_cancel_render(p.meta, binary_identity, str(self.root / "php" / "cancel-jobs"))
            ]
            require(len(candidates) <= 1, "ambiguous render-api2 identity; refusing cancellation")
            if not candidates:
                continue
            require(native is None or native is candidates[0], "native process identity changed")
            native = candidates[0]
            tasks = list((PROC / str(native.stat.pid) / "task").iterdir())
            require(len(tasks) <= 1024, "native thread census exceeds bound")
            for path in tasks:
                try:
                    row = read_stat(path / "stat")
                except (FileNotFoundError, ProcessLookupError):
                    continue
                if row.comm.startswith("Script#"):
                    first = first_threads.setdefault(row.identity, row)
                    if script_cpu_growth(first, row, minimum_ticks):
                        cpu = {
                            "first": asdict(first),
                            "last": asdict(row),
                            "minimum_ticks": minimum_ticks,
                            "clock_ticks_per_second": os.sysconf("SC_CLK_TCK"),
                            "scope": "Named Servo ScriptThread consumed CPU during the sole infinite-input render. This is not a JS instruction trace or a console journal.",
                        }
        require(
            cpu is not None,
            "required Script# CPU growth was not observed within 20 seconds; infinite-turn cancellation is unqualified",
        )
        self.event("script-cpu-growth", native, observation=cpu)
        require(native is not None and not terminated(native.fd), "active native process disappeared")
        require(
            is_cancel_render(metadata(native.stat.pid), binary_identity, str(self.root / "php" / "cancel-jobs")),
            "native executable/command/cwd identity changed before cancellation",
        )
        branch = {native.stat.identity: native}
        stop_sent = set()
        deadline = time.monotonic() + 3
        while True:
            for process in branch.values():
                if process.stat.identity not in stop_sent:
                    self.signal(process, signal.SIGSTOP, "freeze census before forced cancellation")
                    stop_sent.add(process.stat.identity)
            rows = self.pump(deadline)
            branch.update(self.native_members(native, rows))
            stopped = True
            for process in branch.values():
                if terminated(process.fd):
                    require(process is not native, "native exited before forced cancellation")
                    continue
                current = read_stat(PROC / str(process.stat.pid) / "stat")
                require(current.identity == process.stat.identity, "stopped process identity drift")
                tasks = list((PROC / str(process.stat.pid) / "task").iterdir())
                require(len(tasks) <= 1024, "stopped thread census exceeds bound")
                stopped &= (
                    process.stat.identity in stop_sent
                    and bool(tasks)
                    and all(read_stat(path / "stat").state in {"T", "t"} for path in tasks)
                )
            if stopped:
                # A fresh census after every known thread has stopped catches
                # children created while the preceding census was in flight.
                before = len(branch)
                branch.update(self.native_members(native, self.census()))
                if len(branch) == before:
                    break
        require(not terminated(self.php.fd), "PHP exited before forced cancellation")
        self.event("frozen-native-census", identities=list(branch))
        cancelled_at = time.monotonic()
        for process in reversed(list(branch.values())):
            self.signal(process, signal.SIGKILL, "forced caller cancellation")
        deadline = time.monotonic() + 5
        while not all(terminated(p.fd) for p in branch.values()):
            self.pump(deadline)
        require(not terminated(self.php.fd), "PHP caller did not survive native cancellation")
        self.event("native-pidfds-exited", identities=list(branch))
        self.phase("cancelled", 5)
        self.send("RECOVER")
        self.phase("passed", 80)
        self.child.wait(timeout=5)
        require(self.child.returncode == 0, "PHP driver returned failure")
        require(all(terminated(p.fd) for p in self.owned.values()), "an observed PHP descendant survived completion")
        report = json.loads((self.root / "php" / "report.json").read_text())
        require(report["status"] == "passed", "PHP retained report is not passed")
        require(
            report["binary_sha256"] == "sha256:" + file_hash(self.binary), "PHP/native executable byte identity drift"
        )
        return {
            "native": asdict(native.stat),
            "script_cpu": cpu,
            "native_identities": list(branch),
            "native_descendant_count": len(branch) - 1,
            "php_survived_cancellation": True,
            "all_observed_native_pidfds_exited": True,
            "all_observed_php_pidfds_exited": True,
            "cancel_to_recovery_complete_seconds": time.monotonic() - cancelled_at,
        }

    def close(self) -> list[str]:
        errors = []
        try:
            self.census()
        except BaseException as error:
            errors.append(f"final census: {error}")
        # Stop the owned PHP parent first, so recovery cannot spawn during cleanup.
        for process in self.owned.values():
            try:
                self.signal(process, signal.SIGSTOP, "watchdog/final cleanup")
            except OSError as error:
                errors.append(f"cleanup stop {process.stat.identity}: {error}")
        try:
            self.census()
        except BaseException as error:
            errors.append(f"cleanup census: {error}")
        for process in reversed(list(self.owned.values())):
            try:
                self.signal(process, signal.SIGKILL, "watchdog/final cleanup")
            except OSError as error:
                errors.append(f"cleanup kill {process.stat.identity}: {error}")
        pending = [p.fd for p in self.owned.values() if not terminated(p.fd)]
        deadline = time.monotonic() + 5
        while pending and time.monotonic() < deadline:
            exited = select.select(pending, [], [], max(0, deadline - time.monotonic()))[0]
            pending = [fd for fd in pending if fd not in exited]
        if pending:
            errors.append("owned pidfd exit was not observed within cleanup bound")
        try:
            self.child.wait(timeout=1)
        except subprocess.TimeoutExpired:
            errors.append("owned PHP child could not be reaped")
        for process in self.owned.values():
            os.close(process.fd)
        for stream in [self.child.stdin, self.child.stdout, self.journal, self.stderr, self.stdout]:
            stream.close()
        return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--php", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    require(
        sys.platform == "linux" and hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"),
        "requires Linux with Python pidfd APIs; no PID-based fallback is permitted",
    )
    # Probe actual kernel support before launching the PHP child.
    self_fd = os.pidfd_open(os.getpid())
    try:
        signal.pidfd_send_signal(self_fd, 0)
    finally:
        os.close(self_fd)
    php, binary = args.php.resolve(strict=True), args.binary.resolve(strict=True)
    with php.open("rb") as source:
        require(source.read(4) == b"\x7fELF", "supplied PHP must be a native ELF executable, not a wrapper")
    with binary.open("rb") as source:
        require(source.read(4) == b"\x7fELF", "supplied engine is not native ELF")
    require(args.output.is_absolute() and args.output.parent.is_dir(), "output needs an existing absolute parent")
    args.output.mkdir(mode=0o700)
    root = args.output.resolve(strict=True)
    import resource

    report = {
        "schema": "pliego.linux-api2-forced-cancellation-proof",
        "version": 1,
        "status": "failed",
        "python": sys.version,
        "kernel": os.uname().release,
        "binary": str(binary),
        "binary_sha256": file_hash(binary),
        "php_executable": {
            "path": str(php),
            "sha256": file_hash(php),
            "device": php.stat().st_dev,
            "inode": php.stat().st_ino,
        },
        "orchestrator_sha256": file_hash(Path(__file__)),
        "php_driver_sha256": file_hash(Path(__file__).with_name("production_cancellation_test.php")),
        "host": {
            "cpu_affinity": sorted(os.sched_getaffinity(0)),
            "inherited_rlimits": {
                name: resource.getrlimit(getattr(resource, name))
                for name in ["RLIMIT_AS", "RLIMIT_CPU", "RLIMIT_CORE", "RLIMIT_NOFILE", "RLIMIT_NPROC"]
            },
            "cgroup_membership": (PROC / "self" / "cgroup").read_text(),
            "note": "Inherited runner settings, not new memory/CPU admission or cgroup containment.",
        },
        "limits": {
            "outer_seconds": OUTER_SECONDS,
            "engine_ms": 60000,
            "sdk_seconds": 65,
            "preflight_seconds": 80,
            "script_cpu_observation_seconds": 20,
            "freeze_seconds": 3,
            "native_exit_seconds": 5,
            "recovery_seconds": 80,
            "cleanup_seconds": 6,
            "max_owned_identities": 64,
            "max_threads_per_process": 1024,
        },
        "scope": "Forced SIGKILL cancellation of the owned real render-api2 process and observed descendants. No historical/escaped descendant, graceful shutdown, daemon, hostile-input containment, or performance claim.",
    }
    proof = None

    def interrupt(signum, _frame):
        raise TimeoutError(f"outer watchdog/signal {signum}")

    for signum in [signal.SIGALRM, signal.SIGTERM, signal.SIGINT]:
        signal.signal(signum, interrupt)
    signal.setitimer(signal.ITIMER_REAL, OUTER_SECONDS)
    try:
        proof = Proof(php, binary, root)
        report["proof"] = proof.run()
        require(file_hash(binary) == report["binary_sha256"], "native binary bytes changed during proof")
        require(file_hash(php) == report["php_executable"]["sha256"], "supplied PHP bytes changed during proof")
        report["status"] = "passed"
    except BaseException as error:
        report["failure"] = {"class": type(error).__name__, "message": str(error)}
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        if proof is not None:
            signal.setitimer(signal.ITIMER_REAL, 10)
            try:
                report["cleanup_errors"] = proof.close()
            except BaseException as error:
                report["cleanup_errors"] = [f"cleanup interrupted: {error}"]
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)
            if report["cleanup_errors"]:
                report["status"] = "failed"
        (root / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"Linux forced cancellation proof {report['status']}: {root}")
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
