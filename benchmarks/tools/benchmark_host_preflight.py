#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fail-closed wrapper for commands run on Pliego's dedicated benchmark host."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-host-proof.v1.json"
EXPECTED_REPOSITORY = "OxHQ/pliego"
EXPECTED_GROUP = "Pliego dedicated benchmarks"
EXPECTED_LABELS = {"self-hosted", "Linux", "X64", "pliego-benchmark-pinned-v1"}
MODES = ("production", "negative-github-hosted", "negative-missing-thermal")

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_host_proof  # noqa: E402


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def canonical_sha256(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def read_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8", errors="strict").strip()
    except (OSError, UnicodeError):
        return None


def read_int(path: Path) -> int | None:
    raw = read_text(path)
    try:
        return int(raw) if raw is not None else None
    except ValueError:
        return None


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def read_config(path: Path) -> tuple[dict[str, Any], str | None, str | None]:
    descriptor: int | None = None
    try:
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NONBLOCK", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 64 * 1024:
            raise OSError("config must be a regular file no larger than 64 KiB")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = None
            raw = handle.read(64 * 1024 + 1)
        if len(raw) > 64 * 1024:
            raise OSError("config grew beyond 64 KiB while being read")
        value = json.loads(raw.decode("utf-8"))
        if not isinstance(value, dict):
            raise ValueError("config root must be an object")
        return value, hashlib.sha256(raw).hexdigest(), None
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        return {}, None, f"{type(error).__name__}: {error}"
    finally:
        if descriptor is not None:
            os.close(descriptor)


def parse_cpu_set(raw: str | None) -> set[int]:
    if not raw:
        return set()
    cpus: set[int] = set()
    for part in raw.split(","):
        part = part.strip()
        if not part:
            raise ValueError("empty CPU-set component")
        if "-" in part:
            first_text, last_text = part.split("-", 1)
            first, last = int(first_text), int(last_text)
            if first < 0 or last < first:
                raise ValueError(f"invalid CPU range {part!r}")
            cpus.update(range(first, last + 1))
        else:
            cpu = int(part)
            if cpu < 0:
                raise ValueError(f"invalid CPU {cpu}")
            cpus.add(cpu)
    return cpus


def format_cpu_set(cpus: set[int]) -> str:
    if not cpus:
        return ""
    runs: list[str] = []
    start = previous = min(cpus)
    for cpu in sorted(cpus - {start}):
        if cpu == previous + 1:
            previous = cpu
            continue
        runs.append(str(start) if start == previous else f"{start}-{previous}")
        start = previous = cpu
    runs.append(str(start) if start == previous else f"{start}-{previous}")
    return ",".join(runs)


class Recorder:
    def __init__(self) -> None:
        self.events: list[dict[str, Any]] = []

    def add(self, event: str, **detail: Any) -> None:
        self.events.append(
            {
                "sequence": len(self.events),
                "timestamp": now(),
                "event": event,
                "detail": detail,
            }
        )


class Gate:
    def __init__(self) -> None:
        self.checks: list[dict[str, Any]] = []

    def check(self, identifier: str, observed: Any, expected: Any, passed: bool | None = None) -> None:
        self.checks.append(
            {
                "id": identifier,
                "passed": observed == expected if passed is None else bool(passed),
                "observed": observed,
                "expected": expected,
            }
        )

    def passed(self) -> bool:
        return all(check["passed"] for check in self.checks)


class HostLock:
    def __init__(self, path: Path, fixture: bool) -> None:
        self.path = path
        self.fixture = fixture
        self.handle: Any = None
        self.owned = False

    def acquire(self) -> tuple[bool, str]:
        try:
            if self.fixture:
                self.path.parent.mkdir(parents=True, exist_ok=True)
            elif not self.path.parent.is_dir():
                raise OSError("lock parent directory must be provisioned by the host administrator")
            flags = os.O_CREAT | os.O_RDWR
            if self.fixture:
                flags |= os.O_EXCL
            else:
                flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
            descriptor = os.open(self.path, flags, 0o600)
            metadata = os.fstat(descriptor)
            valid_owner = self.fixture or (hasattr(os, "geteuid") and metadata.st_uid == os.geteuid())
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or not valid_owner:
                os.close(descriptor)
                raise OSError("lock must be a single-link regular file owned by the runner")
            self.handle = os.fdopen(descriptor, "r+", encoding="ascii")
            if not self.fixture:
                import fcntl

                fcntl.flock(self.handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.owned = True
            self.handle.seek(0)
            self.handle.truncate()
            self.handle.write(f"pid={os.getpid()}\n")
            self.handle.flush()
            return True, str(self.path)
        except (ImportError, OSError) as error:
            self.release()
            return False, f"{type(error).__name__}: {error}"

    def release(self) -> None:
        if self.handle is not None:
            try:
                if not self.fixture:
                    import fcntl

                    fcntl.flock(self.handle.fileno(), fcntl.LOCK_UN)
                self.handle.close()
            except (ImportError, OSError):
                pass
            self.handle = None
        if self.fixture and self.owned:
            try:
                self.path.unlink()
            except OSError:
                pass
        self.owned = False


def status_fields(path: Path) -> dict[str, str]:
    raw = read_text(path)
    if raw is None:
        return {}
    return {
        key.strip(): value.strip()
        for line in raw.splitlines()
        if ":" in line
        for key, value in [line.split(":", 1)]
    }


def topology(sys_root: Path, cpus: set[int]) -> dict[str, dict[str, Any]]:
    observed: dict[str, dict[str, Any]] = {}
    for cpu in sorted(cpus):
        root = sys_root / "devices" / "system" / "cpu" / f"cpu{cpu}" / "topology"
        siblings = read_text(root / "thread_siblings_list")
        try:
            siblings = format_cpu_set(parse_cpu_set(siblings)) if siblings is not None else None
        except ValueError:
            siblings = None
        observed[str(cpu)] = {
            "package": read_int(root / "physical_package_id"),
            "core": read_int(root / "core_id"),
            "siblings": siblings,
        }
    return observed


def proc_parent(proc_root: Path, pid: int) -> int | None:
    raw = read_text(proc_root / str(pid) / "stat")
    if raw is None:
        return None
    end = raw.rfind(")")
    fields = raw[end + 2 :].split() if end >= 0 else []
    try:
        return int(fields[1]) if len(fields) > 1 else None
    except ValueError:
        return None


def ancestors(proc_root: Path, self_pid: int, injected: list[int] | None) -> set[int]:
    if injected is not None:
        return {int(pid) for pid in injected}
    found = {self_pid}
    current = self_pid
    for _ in range(64):
        parent = proc_parent(proc_root, current)
        if parent is None or parent <= 0 or parent in found:
            break
        found.add(parent)
        current = parent
    return found


def competing_processes(
    proc_root: Path, cpus: set[int], exempt_pids: set[int]
) -> tuple[list[dict[str, Any]], bool]:
    competing: list[dict[str, Any]] = []
    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return competing, False
    complete = True
    for entry in entries:
        if not entry.name.isdigit() or int(entry.name) in exempt_pids:
            continue
        fields = status_fields(entry / "status")
        affinity_raw = fields.get("Cpus_allowed_list")
        if affinity_raw is None:
            complete = False
            continue
        try:
            affinity = parse_cpu_set(affinity_raw)
        except ValueError:
            complete = False
            continue
        if not cpus.intersection(affinity):
            continue
        try:
            command = (entry / "cmdline").read_bytes().replace(b"\0", b" ").strip()
        except OSError:
            complete = False
            continue
        if not command:
            continue
        competing.append(
            {
                "pid": int(entry.name),
                "name": fields.get("Name", "unknown"),
                "affinity": format_cpu_set(affinity),
            }
        )
    return sorted(competing, key=lambda item: item["pid"]), complete


def normalize_expected_topology(raw: Any) -> dict[str, dict[str, Any]]:
    if not isinstance(raw, dict):
        return {}
    normalized: dict[str, dict[str, Any]] = {}
    for cpu, entry in raw.items():
        if not isinstance(entry, dict):
            continue
        siblings = entry.get("siblings")
        try:
            siblings = format_cpu_set(parse_cpu_set(siblings))
        except (TypeError, ValueError):
            siblings = None
        normalized[str(cpu)] = {
            "package": entry.get("package"),
            "core": entry.get("core"),
            "siblings": siblings,
        }
    return normalized


def observe_cpu(
    proc_root: Path,
    sys_root: Path,
    config: dict[str, Any],
    runtime: dict[str, Any],
) -> dict[str, Any]:
    cpu_config = config.get("cpu") if isinstance(config.get("cpu"), dict) else {}
    configured_raw = cpu_config.get("set")
    try:
        configured = parse_cpu_set(configured_raw)
    except (TypeError, ValueError):
        configured = set()
    allowed_raw = status_fields(proc_root / "self" / "status").get("Cpus_allowed_list")
    online_raw = read_text(sys_root / "devices" / "system" / "cpu" / "online")
    isolated_raw = read_text(sys_root / "devices" / "system" / "cpu" / "isolated")
    self_pid = int(runtime.get("self_pid", os.getpid()))
    injected_ancestors = runtime.get("ancestor_pids")
    exempt = ancestors(proc_root, self_pid, injected_ancestors if isinstance(injected_ancestors, list) else None)
    competing, scan_complete = competing_processes(proc_root, configured, exempt)
    return {
        "configured": format_cpu_set(configured) or None,
        "allowed": canonical_cpu_set(allowed_raw),
        "online": canonical_cpu_set(online_raw),
        "isolated": canonical_cpu_set(isolated_raw),
        "topology": topology(sys_root, configured),
        "competing_processes": competing,
        "process_scan_complete": scan_complete,
    }


def canonical_cpu_set(raw: str | None) -> str | None:
    try:
        formatted = format_cpu_set(parse_cpu_set(raw))
    except ValueError:
        return None
    return formatted or None


def observe_controls(proc_root: Path, sys_root: Path, cpus: set[int]) -> dict[str, Any]:
    governors = {
        str(cpu): read_text(
            sys_root
            / "devices"
            / "system"
            / "cpu"
            / f"cpu{cpu}"
            / "cpufreq"
            / "scaling_governor"
        )
        for cpu in sorted(cpus)
    }
    boost_raw = read_text(sys_root / "devices" / "system" / "cpu" / "cpufreq" / "boost")
    no_turbo = read_text(sys_root / "devices" / "system" / "cpu" / "intel_pstate" / "no_turbo")
    if boost_raw in {"0", "1"}:
        boost = "disabled" if boost_raw == "0" else "enabled"
    elif no_turbo in {"0", "1"}:
        boost = "disabled" if no_turbo == "1" else "enabled"
    else:
        boost = None
    smt_raw = read_text(sys_root / "devices" / "system" / "cpu" / "smt" / "control")
    smt = {"on": "enabled", "forceon": "enabled", "off": "disabled", "notsupported": "not-supported"}.get(
        smt_raw or ""
    )
    return {
        "governors": governors,
        "boost": boost,
        "smt": smt,
        "aslr": read_int(proc_root / "sys" / "kernel" / "randomize_va_space"),
    }


def pressure(path: Path) -> dict[str, int | float | None]:
    raw = read_text(path)
    values: dict[str, int | float | None] = {"avg10": None, "avg60": None, "avg300": None, "total": None}
    if raw is None:
        return values
    some = next((line for line in raw.splitlines() if line.startswith("some ")), None)
    if some is None:
        return values
    for token in some.split()[1:]:
        if "=" not in token:
            continue
        key, value = token.split("=", 1)
        if key not in values:
            continue
        try:
            parsed = int(value) if key == "total" else float(value)
            values[key] = parsed if parsed >= 0 else None
        except ValueError:
            pass
    return values


def interrupt_total(path: Path) -> int | None:
    raw = read_text(path)
    if raw is None:
        return None
    total = 0
    found = False
    for line in raw.splitlines():
        if ":" not in line:
            continue
        for token in line.split(":", 1)[1].split():
            if not token.isdigit():
                break
            total += int(token)
            found = True
    return total if found else None


def configured_readings(root: Path, paths: Any) -> dict[str, int]:
    readings: dict[str, int] = {}
    if not isinstance(paths, list):
        return readings
    for relative in paths:
        if not isinstance(relative, str) or Path(relative).is_absolute() or ".." in Path(relative).parts:
            continue
        value = read_int(root / relative)
        if value is not None:
            readings[relative] = value
    return readings


def snapshot(
    proc_root: Path,
    sys_root: Path,
    config: dict[str, Any],
    control_fingerprint: str | None,
    cpu: dict[str, Any],
    controls: dict[str, Any],
    *,
    omit_thermal: bool = False,
) -> dict[str, Any]:
    load_raw = read_text(proc_root / "loadavg")
    load: list[float | None] = [None, None, None]
    if load_raw:
        try:
            values = load_raw.split()[:3]
            load = [float(value) for value in values] if len(values) == 3 else [None, None, None]
        except ValueError:
            load = [None, None, None]
    sensors = config.get("sensors") if isinstance(config.get("sensors"), dict) else {}
    temperatures = {} if omit_thermal else configured_readings(sys_root, sensors.get("thermal_paths"))
    return {
        "captured_at": now(),
        "load": {"one": load[0], "five": load[1], "fifteen": load[2]},
        "psi": {
            "cpu": pressure(proc_root / "pressure" / "cpu"),
            "memory": pressure(proc_root / "pressure" / "memory"),
            "io": pressure(proc_root / "pressure" / "io"),
        },
        "interrupts_total": interrupt_total(proc_root / "interrupts"),
        "temperatures_millic": temperatures,
        "throttle_counters": configured_readings(sys_root, sensors.get("throttle_paths")),
        "control_fingerprint": control_fingerprint,
        "cpu": cpu,
        "controls": controls,
    }


def config_errors(config: dict[str, Any], fixture: bool) -> list[str]:
    errors: list[str] = []
    if config.get("schema") != "pliego.benchmark-host-config" or config.get("version") != 1:
        errors.append("schema/version")
    runner = config.get("runner") if isinstance(config.get("runner"), dict) else {}
    if not isinstance(runner.get("name"), str) or not runner.get("name"):
        errors.append("runner.name")
    if not isinstance(runner.get("group_id"), int) or runner.get("group_id", 0) < 1:
        errors.append("runner.group_id")
    cpu = config.get("cpu") if isinstance(config.get("cpu"), dict) else {}
    try:
        cpus = parse_cpu_set(cpu.get("set"))
    except (TypeError, ValueError):
        cpus = set()
    if not cpus:
        errors.append("cpu.set")
    expected_topology = normalize_expected_topology(cpu.get("topology"))
    if set(expected_topology) != {str(cpu_id) for cpu_id in cpus}:
        errors.append("cpu.topology")
    controls = config.get("controls") if isinstance(config.get("controls"), dict) else {}
    if controls.get("boost") not in {"enabled", "disabled"}:
        errors.append("controls.boost")
    if controls.get("smt") not in {"enabled", "disabled", "not-supported"}:
        errors.append("controls.smt")
    if controls.get("aslr") not in {0, 1, 2}:
        errors.append("controls.aslr")
    sensors = config.get("sensors") if isinstance(config.get("sensors"), dict) else {}
    for key in ("thermal_paths", "throttle_paths"):
        paths = sensors.get(key)
        if not isinstance(paths, list) or not paths or not all(isinstance(path, str) for path in paths):
            errors.append(f"sensors.{key}")
    limits = config.get("limits") if isinstance(config.get("limits"), dict) else {}
    for key in (
        "max_load_one_per_cpu",
        "max_cpu_psi_avg10",
        "max_memory_psi_avg10",
        "max_io_psi_avg10",
        "max_temperature_millic",
        "max_temperature_drift_millic",
        "max_interrupt_delta",
    ):
        if not isinstance(limits.get(key), (int, float)) or isinstance(limits.get(key), bool) or limits[key] < 0:
            errors.append(f"limits.{key}")
    lock_path = config.get("lock_path")
    if not isinstance(lock_path, str) or not lock_path or (not fixture and not Path(lock_path).is_absolute()):
        errors.append("lock_path")
    return errors


def api_get(url: str, token: str) -> dict[str, Any]:
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2026-03-10",
        "User-Agent": "pliego-benchmark-host-proof-v1",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(request, timeout=20) as response:
        value = json.loads(response.read().decode("utf-8"))
    return value if isinstance(value, dict) else {}


def collect_api(config: dict[str, Any], token: str, fixture_root: Path | None, errors: list[str]) -> dict[str, Any]:
    if fixture_root is not None:
        value = read_json(fixture_root / "api.json")
        if not value:
            errors.append("api fixture is unreadable")
        return value
    runner = config.get("runner") if isinstance(config.get("runner"), dict) else {}
    group_id = runner.get("group_id", 0)
    urls = {
        "branch": "https://api.github.com/repos/OxHQ/pliego/branches/main",
        "runner_group": f"https://api.github.com/orgs/OxHQ/actions/runner-groups/{group_id}",
        "group_runners": f"https://api.github.com/orgs/OxHQ/actions/runner-groups/{group_id}/runners?per_page=100",
    }
    responses: dict[str, Any] = {}
    for name, url in urls.items():
        try:
            responses[name] = api_get(url, token if name != "branch" else "")
        except (OSError, UnicodeError, json.JSONDecodeError, urllib.error.HTTPError) as error:
            errors.append(f"GitHub API {name}: {type(error).__name__}: {error}")
            responses[name] = {}
    return responses


def find_runner(response: Any, name: str | None) -> dict[str, Any]:
    if not isinstance(response, dict) or not isinstance(response.get("runners"), list):
        return {}
    for runner in response["runners"]:
        if isinstance(runner, dict) and runner.get("name") == name:
            return runner
    return {}


def runner_proof(env: dict[str, str], api: dict[str, Any]) -> dict[str, Any]:
    name = env.get("RUNNER_NAME")
    runner = find_runner(api.get("group_runners"), name)
    labels = [label.get("name") for label in runner.get("labels", []) if isinstance(label, dict)]
    return {
        "name": name,
        "os": env.get("RUNNER_OS"),
        "arch": env.get("RUNNER_ARCH"),
        "id": runner.get("id") if isinstance(runner.get("id"), int) else None,
        "status": runner.get("status") if isinstance(runner.get("status"), str) else None,
        "busy": runner.get("busy") if isinstance(runner.get("busy"), bool) else None,
        "group": api.get("runner_group", {}).get("name") if isinstance(api.get("runner_group"), dict) else None,
        "labels": sorted(label for label in labels if isinstance(label, str)),
    }


def git_checkout_sha(fixture_root: Path | None, runtime: dict[str, Any]) -> str | None:
    if fixture_root is not None:
        value = runtime.get("checkout_sha")
        return value if isinstance(value, str) else None
    try:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    value = result.stdout.strip()
    return value if result.returncode == 0 else None


def git_worktree_clean(fixture_root: Path | None, runtime: dict[str, Any]) -> bool:
    if fixture_root is not None:
        return runtime.get("worktree_clean") is True
    try:
        result = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0 and not result.stdout


def normalized_env(fixture_root: Path | None) -> dict[str, str]:
    if fixture_root is None:
        return dict(os.environ)
    return {key: str(value) for key, value in read_json(fixture_root / "env.json").items()}


def threshold_checks(gate: Gate, prefix: str, snap: dict[str, Any], config: dict[str, Any], cpu_count: int) -> None:
    limits = config.get("limits") if isinstance(config.get("limits"), dict) else {}
    load = snap["load"]["one"]
    load_limit = limits.get("max_load_one_per_cpu")
    max_load = load_limit * cpu_count if isinstance(load_limit, (int, float)) else None
    gate.check(
        f"{prefix}.load",
        load,
        f"0 <= value <= {max_load}",
        load is not None and max_load is not None and 0 <= load <= max_load,
    )
    for kind, limit_key in (
        ("cpu", "max_cpu_psi_avg10"),
        ("memory", "max_memory_psi_avg10"),
        ("io", "max_io_psi_avg10"),
    ):
        observed = snap["psi"][kind]["avg10"]
        limit = limits.get(limit_key)
        gate.check(
            f"{prefix}.psi.{kind}",
            observed,
            f"<= {limit}",
            observed is not None and isinstance(limit, (int, float)) and 0 <= observed <= limit,
        )
    temperatures = snap["temperatures_millic"]
    sensors = config.get("sensors") if isinstance(config.get("sensors"), dict) else {}
    expected_thermal = sensors.get("thermal_paths") if isinstance(sensors.get("thermal_paths"), list) else []
    max_temperature = limits.get("max_temperature_millic")
    gate.check(
        f"{prefix}.thermal",
        temperatures,
        f"nonempty and each <= {max_temperature}",
        bool(temperatures)
        and set(temperatures) == set(expected_thermal)
        and isinstance(max_temperature, (int, float))
        and all(value <= max_temperature for value in temperatures.values()),
    )
    counters = snap["throttle_counters"]
    expected_counters = sensors.get("throttle_paths") if isinstance(sensors.get("throttle_paths"), list) else []
    gate.check(
        f"{prefix}.throttle_counters",
        counters,
        "readable and nonempty",
        bool(counters) and set(counters) == set(expected_counters) and all(value >= 0 for value in counters.values()),
    )
    gate.check(
        f"{prefix}.interrupts",
        snap["interrupts_total"],
        "readable nonnegative integer",
        isinstance(snap["interrupts_total"], int) and snap["interrupts_total"] >= 0,
    )


def write_artifacts(
    output: Path,
    proof: dict[str, Any],
    recorder: Recorder,
    gate: Gate,
    collection_errors: list[str],
) -> list[str]:
    chronology = output / validate_host_proof.CHRONOLOGY_NAME
    diagnostics = output / validate_host_proof.DIAGNOSTICS_NAME
    proof_path = output / validate_host_proof.PROOF_NAME
    chronology.write_text(
        "".join(json.dumps(event, sort_keys=True) + "\n" for event in recorder.events),
        encoding="utf-8",
    )
    diagnostics_payload = {
        "schema": "pliego.benchmark-host-diagnostics",
        "version": 1,
        "status": proof["status"],
        "mode": proof["mode"],
        "failed_checks": [check["id"] for check in gate.checks if not check["passed"]],
        "collection_errors": collection_errors,
    }
    diagnostics.write_text(json.dumps(diagnostics_payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    stdout_path = output / validate_host_proof.STDOUT_NAME
    stderr_path = output / validate_host_proof.STDERR_NAME
    proof_path.write_text(json.dumps(proof, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    manifest_files = sorted((proof_path, chronology, diagnostics, stdout_path, stderr_path), key=lambda path: path.name)
    (output / validate_host_proof.MANIFEST_NAME).write_text(
        "".join(f"{validate_host_proof.sha256_file(path)}  {path.name}\n" for path in manifest_files),
        encoding="ascii",
    )
    schema = read_json(SCHEMA)
    return validate_host_proof.validate_document(
        proof,
        schema,
        output,
        allow_fixture_evidence=proof["evidence_source"] == "fixture",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=MODES, default="production")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--config", type=Path, default=Path("/etc/pliego-benchmark-host.v1.json"))
    parser.add_argument("--token-env", default="PLIEGO_BENCHMARK_PROOF_TOKEN")
    parser.add_argument("--fixture-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("a benchmark command is required after --")

    output = args.output_dir.resolve()
    if not output.parent.is_dir():
        parser.error("--output-dir parent must already exist")
    try:
        output.mkdir(mode=0o700)
    except OSError as error:
        parser.error(f"--output-dir must be a new private directory: {error}")
    stdout_path = output / validate_host_proof.STDOUT_NAME
    stderr_path = output / validate_host_proof.STDERR_NAME
    stdout_path.write_bytes(b"")
    stderr_path.write_bytes(b"")

    fixture_root = args.fixture_root.resolve() if args.fixture_root else None
    proc_root = fixture_root / "proc" if fixture_root else Path("/proc")
    sys_root = fixture_root / "sys" if fixture_root else Path("/sys")
    runtime = read_json(fixture_root / "runtime.json") if fixture_root else {}
    config_path = fixture_root / "config.json" if fixture_root else args.config
    config, config_hash, config_read_error = read_config(config_path)
    env = normalized_env(fixture_root)
    proof_token = env.get(args.token_env) or os.environ.get(args.token_env, "")
    command_contains_token = bool(proof_token) and any(proof_token in argument for argument in command)
    retained_command = [argument.replace(proof_token, "<redacted>") for argument in command] if proof_token else command
    collection_errors: list[str] = []
    if config_read_error:
        collection_errors.append(f"host config: {config_read_error}")
    recorder = Recorder()
    gate = Gate()
    recorder.add("preflight.started", mode=args.mode, evidence_source="fixture" if fixture_root else "live")

    errors = config_errors(config, fixture_root is not None)
    gate.check("config.valid", errors, [], not errors)
    runner_config = config.get("runner") if isinstance(config.get("runner"), dict) else {}
    api = collect_api(config, proof_token, fixture_root, collection_errors)
    runner = runner_proof(env, api)
    branch = api.get("branch") if isinstance(api.get("branch"), dict) else {}
    branch_commit = branch.get("commit") if isinstance(branch.get("commit"), dict) else {}
    identity = {
        "repository": env.get("GITHUB_REPOSITORY"),
        "event": env.get("GITHUB_EVENT_NAME"),
        "ref": env.get("GITHUB_REF"),
        "ref_protected": env.get("GITHUB_REF_PROTECTED", "").lower() == "true",
        "sha": env.get("GITHUB_SHA"),
        "checkout_sha": git_checkout_sha(fixture_root, runtime),
        "default_branch_sha": branch_commit.get("sha") if isinstance(branch_commit.get("sha"), str) else None,
        "worktree_clean": git_worktree_clean(fixture_root, runtime),
    }
    gate.check("identity.repository", identity["repository"], EXPECTED_REPOSITORY)
    gate.check("identity.event", identity["event"], "workflow_dispatch")
    gate.check("identity.ref", identity["ref"], "refs/heads/main")
    gate.check("identity.protected", identity["ref_protected"], True)
    gate.check("identity.api_protected", branch.get("protected"), True)
    gate.check("identity.worktree_clean", identity["worktree_clean"], True)
    gate.check("command.secret_free", command_contains_token, False)
    immutable = [identity["sha"], identity["checkout_sha"], identity["default_branch_sha"]]
    gate.check(
        "identity.immutable_sha",
        immutable,
        "three identical 40-character lowercase SHAs",
        len(set(immutable)) == 1 and isinstance(immutable[0], str) and re.fullmatch(r"[0-9a-f]{40}", immutable[0]) is not None,
    )

    group_runner = find_runner(api.get("group_runners"), runner["name"])
    gate.check("runner.name", runner["name"], runner_config.get("name"))
    gate.check("runner.os", runner["os"], "Linux")
    gate.check("runner.arch", runner["arch"], "X64")
    gate.check("runner.group", runner["group"], EXPECTED_GROUP)
    gate.check(
        "runner.id",
        runner["id"],
        "positive integer",
        isinstance(runner["id"], int) and runner["id"] > 0,
    )
    gate.check(
        "runner.labels",
        runner["labels"],
        f"contains {sorted(EXPECTED_LABELS)}",
        EXPECTED_LABELS.issubset(set(runner["labels"])),
    )
    gate.check(
        "runner.group_assignment",
        group_runner.get("id"),
        runner["id"],
        isinstance(runner["id"], int) and runner["id"] > 0 and group_runner.get("id") == runner["id"],
    )
    gate.check("runner.api_os", group_runner.get("os"), "linux")
    gate.check("runner.group_id", api.get("runner_group", {}).get("id"), runner_config.get("group_id"))
    gate.check("runner.online", runner["status"], "online")
    gate.check("runner.busy", runner["busy"], True)
    if args.mode == "negative-github-hosted":
        gate.check("negative.github_hosted", runner["labels"], "dedicated self-hosted labels", False)

    lock_path_raw = config.get("lock_path")
    if fixture_root:
        lock_path = fixture_root / str(lock_path_raw or "benchmark.lock")
    else:
        lock_path = Path(str(lock_path_raw or "/var/lock/pliego-benchmark-host.lock"))
    host_lock = HostLock(lock_path, fixture_root is not None)
    acquired, lock_observation = host_lock.acquire()
    gate.check("host.exclusive_lock", lock_observation, "exclusive lock acquired", acquired)

    try:
        cpu = observe_cpu(proc_root, sys_root, config, runtime)
        try:
            configured_cpus = parse_cpu_set(cpu["configured"])
            online_cpus = parse_cpu_set(cpu["online"])
        except (TypeError, ValueError):
            configured_cpus, online_cpus = set(), set()
        expected_topology = normalize_expected_topology(
            config.get("cpu", {}).get("topology") if isinstance(config.get("cpu"), dict) else {}
        )
        gate.check("cpu.affinity", cpu["allowed"], cpu["configured"])
        gate.check(
            "cpu.online",
            cpu["online"],
            f"contains {cpu['configured']}",
            bool(configured_cpus) and configured_cpus.issubset(online_cpus),
        )
        gate.check("cpu.isolation", cpu["isolated"], cpu["configured"])
        gate.check("cpu.topology", cpu["topology"], expected_topology)
        whole_cores = bool(configured_cpus) and all(
            parse_cpu_set(entry.get("siblings")) <= configured_cpus
            for entry in cpu["topology"].values()
            if entry.get("siblings") is not None
        ) and all(entry.get("siblings") is not None for entry in cpu["topology"].values())
        gate.check("cpu.sibling_policy", whole_cores, True)
        gate.check("host.process_scan", cpu["process_scan_complete"], True)
        gate.check("host.no_competing_workload", cpu["competing_processes"], [])

        controls = observe_controls(proc_root, sys_root, configured_cpus)
        expected_controls = config.get("controls") if isinstance(config.get("controls"), dict) else {}
        gate.check(
            "controls.governor",
            controls["governors"],
            "performance on every configured CPU",
            bool(controls["governors"])
            and all(value == "performance" for value in controls["governors"].values()),
        )
        gate.check("controls.boost", controls["boost"], expected_controls.get("boost"))
        gate.check("controls.smt", controls["smt"], expected_controls.get("smt"))
        gate.check("controls.aslr", controls["aslr"], expected_controls.get("aslr"))
        control_cpu = {
            key: cpu[key]
            for key in ("configured", "allowed", "online", "isolated", "topology")
        }
        fingerprint = canonical_sha256({"cpu": control_cpu, "controls": controls}) if configured_cpus else None
        pre = snapshot(
            proc_root,
            sys_root,
            config,
            fingerprint,
            cpu,
            controls,
            omit_thermal=args.mode == "negative-missing-thermal",
        )
        threshold_checks(gate, "pre", pre, config, len(configured_cpus))

        command_proof: dict[str, Any] = {
            "argv": retained_command,
            "started": False,
            "exit_code": None,
        }
        post: dict[str, Any] | None = None
        drift: dict[str, Any] | None = None
        if not gate.passed():
            recorder.add("preflight.rejected", failed_checks=[check["id"] for check in gate.checks if not check["passed"]])
            status = "rejected"
            failure = {"code": "HOST_PREFLIGHT_REJECTED", "message": "dedicated host preflight rejected"}
        else:
            recorder.add("preflight.passed", check_count=len(gate.checks))
            recorder.add("samples.started", argv=retained_command)
            command_proof["started"] = True
            command_env = dict(os.environ)
            command_env.pop(args.token_env, None)
            with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
                try:
                    result = subprocess.run(
                        command,
                        cwd=ROOT,
                        env=command_env,
                        stdout=stdout,
                        stderr=stderr,
                        timeout=3600,
                    )
                    command_exit = result.returncode
                except subprocess.TimeoutExpired:
                    command_exit = 124
                    stderr.write(b"benchmark command exceeded the 3600-second host-gate timeout\n")
                except OSError as error:
                    command_exit = 127
                    stderr.write(f"benchmark command could not start: {error}\n".encode("utf-8", errors="replace"))
            command_proof["exit_code"] = command_exit
            recorder.add("samples.finished", exit_code=command_exit)

            post_cpu = observe_cpu(proc_root, sys_root, config, runtime)
            post_controls = observe_controls(proc_root, sys_root, configured_cpus)
            post_control_cpu = {
                key: post_cpu[key]
                for key in ("configured", "allowed", "online", "isolated", "topology")
            }
            post_fingerprint = canonical_sha256({"cpu": post_control_cpu, "controls": post_controls})
            post = snapshot(proc_root, sys_root, config, post_fingerprint, post_cpu, post_controls)
            throttle_keys_equal = set(pre["throttle_counters"]) == set(post["throttle_counters"])
            throttle_delta = {
                key: post["throttle_counters"][key] - pre["throttle_counters"][key]
                for key in sorted(set(pre["throttle_counters"]) & set(post["throttle_counters"]))
            }
            temperature_keys_equal = set(pre["temperatures_millic"]) == set(post["temperatures_millic"])
            temperature_delta = {
                key: post["temperatures_millic"][key] - pre["temperatures_millic"][key]
                for key in sorted(set(pre["temperatures_millic"]) & set(post["temperatures_millic"]))
            }
            interrupt_delta = (
                post["interrupts_total"] - pre["interrupts_total"]
                if isinstance(post["interrupts_total"], int) and isinstance(pre["interrupts_total"], int)
                else None
            )
            drift = {
                "controls_equal": post_fingerprint == fingerprint,
                "throttle_delta": throttle_delta,
                "temperature_delta_millic": temperature_delta,
                "interrupt_delta": interrupt_delta,
            }
            threshold_checks(gate, "post", post, config, len(configured_cpus))
            _, post_config_hash, post_config_error = read_config(config_path)
            if post_config_error:
                collection_errors.append(f"postflight host config: {post_config_error}")
            gate.check("post.config", post_config_hash, config_hash)
            gate.check("post.host_controls", post_fingerprint, fingerprint)
            gate.check("post.process_scan", post_cpu["process_scan_complete"], True)
            gate.check("post.no_competing_workload", post_cpu["competing_processes"], [])
            gate.check(
                "post.throttle_drift",
                throttle_delta,
                "all counters unchanged",
                throttle_keys_equal and bool(throttle_delta) and all(delta == 0 for delta in throttle_delta.values()),
            )
            limits = config.get("limits") if isinstance(config.get("limits"), dict) else {}
            max_temperature_drift = limits.get("max_temperature_drift_millic", -1)
            gate.check(
                "post.temperature_drift",
                temperature_delta,
                f"absolute delta <= {max_temperature_drift}",
                temperature_keys_equal
                and bool(temperature_delta)
                and isinstance(max_temperature_drift, (int, float))
                and all(abs(delta) <= max_temperature_drift for delta in temperature_delta.values()),
            )
            max_interrupt_delta = limits.get("max_interrupt_delta", -1)
            gate.check(
                "post.interrupt_drift",
                interrupt_delta,
                f"0 <= delta <= {max_interrupt_delta}",
                isinstance(interrupt_delta, int)
                and isinstance(max_interrupt_delta, (int, float))
                and 0 <= interrupt_delta <= max_interrupt_delta,
            )
            gate.check("command.exit", command_exit, 0)
            recorder.add("postflight.finished", passed=gate.passed())
            if command_exit != 0:
                status = "failed"
                failure = {"code": "BENCHMARK_COMMAND_FAILED", "message": "benchmark command exited nonzero"}
            elif not gate.passed():
                status = "failed"
                failure = {"code": "HOST_POSTFLIGHT_REJECTED", "message": "dedicated host postflight rejected"}
            else:
                status = "accepted"
                failure = None

        proof: dict[str, Any] = {
            "schema": "pliego.benchmark-host-proof",
            "version": 1,
            "status": status,
            "mode": args.mode,
            "generated_at": recorder.events[0]["timestamp"],
            "evidence_source": "fixture" if fixture_root else "live",
            "identity": identity,
            "runner": runner,
            "host_config": {
                "path": str(config_path),
                "sha256": config_hash,
                "schema": config.get("schema") if isinstance(config.get("schema"), str) else None,
                "version": config.get("version") if isinstance(config.get("version"), int) else None,
            },
            "pre": pre,
            "post": post,
            "drift": drift,
            "command": command_proof,
            "checks": gate.checks,
            "failure": failure,
            "authority": {
                "software_gate": "implemented",
                "hardware_provisioning": "external",
                "organization_runner_configuration": "external",
                "roadmap_state": "incomplete-external-authority-required",
            },
        }
        violations = write_artifacts(output, proof, recorder, gate, collection_errors)
        if violations:
            for violation in violations:
                print(f"benchmark_host_preflight: internal proof validation failed: {violation}", file=sys.stderr)
            return 2
        print(f"benchmark host proof: {status} ({output / validate_host_proof.PROOF_NAME})")
        return 0 if status == "accepted" else 1
    finally:
        host_lock.release()


if __name__ == "__main__":
    raise SystemExit(main())
