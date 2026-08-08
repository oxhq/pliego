#!/usr/bin/env python3

"""Run one command in a new Linux session and sample its complete process tree."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

PROC = Path("/proc")
resource = None
if sys.platform == "linux":
    import resource


def require_linux(parser: argparse.ArgumentParser, platform: str) -> None:
    if platform != "linux":
        parser.error("this sampler requires Linux procfs")


def read_stat(pid: int) -> dict[str, int] | None:
    try:
        raw = (PROC / str(pid) / "stat").read_text(encoding="ascii")
    except (OSError, UnicodeError):
        return None
    end = raw.rfind(")")
    fields = raw[end + 2 :].split() if end >= 0 else []
    if len(fields) < 22:
        return None
    return {
        "pid": pid,
        "ppid": int(fields[1]),
        "process_group": int(fields[2]),
        "session": int(fields[3]),
        "user_ticks": int(fields[11]),
        "sys_ticks": int(fields[12]),
        "start_ticks": int(fields[19]),
        "rss_pages": max(0, int(fields[21])),
    }


def read_io(pid: int) -> tuple[int, int] | None:
    try:
        values = {
            key: int(value.strip())
            for key, value in (
                line.split(":", 1)
                for line in (PROC / str(pid) / "io").read_text(encoding="ascii").splitlines()
                if ":" in line
            )
        }
        return values["read_bytes"], values["write_bytes"]
    except (KeyError, OSError, UnicodeError, ValueError):
        return None


def read_pss_kib(pid: int) -> int | None:
    try:
        for line in (PROC / str(pid) / "smaps_rollup").read_text(encoding="ascii").splitlines():
            if line.startswith("Pss:"):
                return int(line.split()[1])
    except (OSError, UnicodeError, ValueError):
        pass
    return None


def scan_session(root_pid: int, include_pss: bool) -> list[dict[str, int | None]]:
    found: list[dict[str, int | None]] = []
    with os.scandir(PROC) as entries:
        for entry in entries:
            if not entry.name.isdigit():
                continue
            stat = read_stat(int(entry.name))
            if stat is None or stat["session"] != root_pid:
                continue
            io = read_io(stat["pid"])
            stat["read_bytes"] = io[0] if io is not None else None
            stat["write_bytes"] = io[1] if io is not None else None
            stat["pss_kib"] = read_pss_kib(stat["pid"]) if include_pss else None
            found.append(stat)
    return sorted(found, key=lambda process: int(process["pid"]))


def sample_command(
    command: list[str],
    cwd: str,
    stdout_path: str,
    stderr_path: str,
    interval_ms: float,
    pss_interval_ms: float,
    descendant_grace_ms: float,
) -> dict[str, Any]:
    if resource is None:
        raise OSError("this sampler requires Linux procfs")
    page_size = os.sysconf("SC_PAGE_SIZE")
    clock_ticks = os.sysconf("SC_CLK_TCK")
    samples: list[dict[str, Any]] = []
    processes: dict[tuple[int, int], dict[str, int | float | None]] = {}
    root_done = threading.Event()
    root_ended = [0.0]
    tree_complete = [False]
    lingering_pids: list[int] = []
    sampler_error: list[BaseException] = []
    usage_start = resource.getrusage(resource.RUSAGE_SELF)

    with open(stdout_path, "wb") as engine_stdout, open(stderr_path, "wb") as engine_stderr:
        started = time.monotonic()
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=engine_stdout,
            stderr=engine_stderr,
            start_new_session=True,
        )

        def sample_loop() -> None:
            try:
                next_pss = started + pss_interval_ms / 1000.0
                empty_after_exit = 0
                while True:
                    before_scan = time.monotonic()
                    include_pss = before_scan >= next_pss
                    members = scan_session(process.pid, include_pss)
                    summed_rss_kib = sum(int(member["rss_pages"]) * page_size // 1024 for member in members)
                    elapsed_ms = round((time.monotonic() - started) * 1000.0, 3)
                    if include_pss:
                        next_pss = before_scan + pss_interval_ms / 1000.0

                    raw_processes = []
                    for member in members:
                        identity = (int(member["pid"]), int(member["start_ticks"]))
                        current = processes.setdefault(
                            identity,
                            {
                                "pid": member["pid"],
                                "start_ticks": member["start_ticks"],
                                "first_seen_ms": elapsed_ms,
                                "last_seen_ms": elapsed_ms,
                                "ppid": member["ppid"],
                                "process_group": member["process_group"],
                                "user_ticks": 0,
                                "sys_ticks": 0,
                                "read_bytes": None,
                                "write_bytes": None,
                            },
                        )
                        current["last_seen_ms"] = elapsed_ms
                        current["ppid"] = member["ppid"]
                        current["user_ticks"] = max(int(current["user_ticks"]), int(member["user_ticks"]))
                        current["sys_ticks"] = max(int(current["sys_ticks"]), int(member["sys_ticks"]))
                        for field in ("read_bytes", "write_bytes"):
                            value = member[field]
                            if value is not None:
                                previous = current[field]
                                current[field] = int(value) if previous is None else max(int(previous), int(value))
                        raw_processes.append(
                            {
                                "pid": member["pid"],
                                "start_ticks": member["start_ticks"],
                                "rss_pages": member["rss_pages"],
                                "pss_kib": member["pss_kib"],
                            }
                        )

                    pss_values = [member["pss_kib"] for member in members]
                    samples.append(
                        {
                            "elapsed_ms": elapsed_ms,
                            "summed_rss_kib": summed_rss_kib,
                            "summed_pss_kib": (
                                sum(int(value) for value in pss_values)
                                if include_pss and members and all(value is not None for value in pss_values)
                                else None
                            ),
                            "processes": raw_processes,
                        }
                    )

                    if root_done.is_set():
                        if members:
                            empty_after_exit = 0
                            lingering_pids[:] = [int(member["pid"]) for member in members]
                        else:
                            empty_after_exit += 1
                            lingering_pids.clear()
                        if empty_after_exit >= 2:
                            tree_complete[0] = True
                            return
                        if (time.monotonic() - root_ended[0]) * 1000.0 >= descendant_grace_ms:
                            return

                    delay = interval_ms / 1000.0
                    if root_done.is_set():
                        time.sleep(delay)
                    else:
                        root_done.wait(delay)
            except BaseException as error:  # surfaced after the child is reaped
                sampler_error.append(error)

        sampler = threading.Thread(target=sample_loop, name="procfs-session-sampler")
        sampler.start()
        return_code = process.wait()
        root_ended[0] = time.monotonic()
        root_done.set()
        sampler.join()

    if sampler_error:
        raise RuntimeError(f"sampler thread failed: {sampler_error[0]}") from sampler_error[0]

    usage_end = resource.getrusage(resource.RUSAGE_SELF)
    wall_ms = (root_ended[0] - started) * 1000.0
    sampler_user_ms = (usage_end.ru_utime - usage_start.ru_utime) * 1000.0
    sampler_sys_ms = (usage_end.ru_stime - usage_start.ru_stime) * 1000.0
    process_rows = sorted(processes.values(), key=lambda row: (int(row["pid"]), int(row["start_ticks"])))
    io_complete = bool(process_rows) and all(
        row["read_bytes"] is not None and row["write_bytes"] is not None for row in process_rows
    )
    elapsed = [float(sample["elapsed_ms"]) for sample in samples]
    gaps = [later - earlier for earlier, later in zip(elapsed, elapsed[1:])]
    pss_peaks = [int(sample["summed_pss_kib"]) for sample in samples if sample["summed_pss_kib"] is not None]
    signal = -return_code if return_code < 0 else None

    return {
        "method": "linux-procfs-session-poll-v1",
        "scope": "new-session-and-inherited-descendants",
        "root_pid": process.pid,
        "session_id": process.pid,
        "sample_interval_ms": interval_ms,
        "pss_interval_ms": pss_interval_ms,
        "max_sample_gap_ms": round(max(gaps, default=0.0), 3),
        "clock_ticks_per_second": clock_ticks,
        "page_size_bytes": page_size,
        "wall_ms": round(wall_ms, 3),
        "exit_code": return_code if return_code >= 0 else 128 + int(signal),
        "signal": signal,
        "peak_summed_rss_kib": max((int(sample["summed_rss_kib"]) for sample in samples), default=0),
        "peak_summed_pss_kib": max(pss_peaks) if pss_peaks else None,
        "pss_method": "procfs-smaps_rollup-summed" if pss_peaks else "unavailable",
        "cpu_user_ticks": sum(int(row["user_ticks"]) for row in process_rows),
        "cpu_sys_ticks": sum(int(row["sys_ticks"]) for row in process_rows),
        "cpu_user_ms": round(sum(int(row["user_ticks"]) for row in process_rows) * 1000 / clock_ticks, 3),
        "cpu_sys_ms": round(sum(int(row["sys_ticks"]) for row in process_rows) * 1000 / clock_ticks, 3),
        "read_bytes": sum(int(row["read_bytes"]) for row in process_rows) if io_complete else None,
        "write_bytes": sum(int(row["write_bytes"]) for row in process_rows) if io_complete else None,
        "observed_pids": sorted({int(row["pid"]) for row in process_rows}),
        "tree_complete": tree_complete[0],
        "lingering_pids": lingering_pids,
        "discovery": (
            "all /proc PIDs whose session id matches the root; membership survives reparenting, "
            "but a process created and reaped entirely between samples cannot be observed, and "
            "counter increments after a process's final observation cannot be recovered; PSS starts "
            "after one full PSS interval and is unavailable for shorter commands"
        ),
        "sampler_cpu_user_ms": round(sampler_user_ms, 3),
        "sampler_cpu_sys_ms": round(sampler_sys_ms, 3),
        "sampler_cpu_percent_of_wall": round(
            (sampler_user_ms + sampler_sys_ms) * 100.0 / wall_ms if wall_ms > 0 else 0.0, 3
        ),
        "processes": process_rows,
        "samples": samples,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cwd", default=os.getcwd())
    parser.add_argument("--stdout", default=os.devnull, help="command stdout file")
    parser.add_argument("--stderr", default=os.devnull, help="command stderr file")
    parser.add_argument("--interval-ms", type=float, default=75.0)
    parser.add_argument("--pss-interval-ms", type=float, default=250.0)
    parser.add_argument("--descendant-grace-ms", type=float, default=1000.0)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    require_linux(parser, sys.platform)
    if not command:
        parser.error("a command is required after --")
    if args.interval_ms <= 0 or args.pss_interval_ms < args.interval_ms or args.descendant_grace_ms < 0:
        parser.error("intervals must be positive and PSS interval must be >= sample interval")
    try:
        result = sample_command(
            command,
            args.cwd,
            args.stdout,
            args.stderr,
            args.interval_ms,
            args.pss_interval_ms,
            args.descendant_grace_ms,
        )
    except (OSError, RuntimeError) as error:
        print(f"process_tree_sampler: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
