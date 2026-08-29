#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Run one fixed benchmark render command as an unprivileged account in a root-owned cgroup."""

from __future__ import annotations

import argparse
import contextlib
import ctypes
import errno
import hashlib
import json
import math
import os
import secrets
import select
import signal
import socket
import stat
import struct
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterator

sys.path.insert(0, str(Path(__file__).resolve().parent))
from benchmark_runtime import BROWSERSHOT_TARGET, runtime_contract, runtime_target

fcntl = None
pwd = None
resource = None
if sys.platform == "linux":
    import fcntl
    import pwd
    import resource

PROC = Path("/proc")
CGROUP_ROOT = Path("/sys/fs/cgroup")
REQUIRED_CONTROLLERS = frozenset({"cpu", "io", "memory", "pids"})
HARNESS_CHILD = "harness"
ENGINE_ACCOUNT = "pliego-benchmark-engine"
ENGINE_ACCOUNT_HOME = "/nonexistent/pliego-benchmark-engine"
KILL_GRACE_MS = 1000.0
PR_CAPBSET_DROP = 24
PR_SET_NO_NEW_PRIVS = 38
PR_CAP_AMBIENT = 47
PR_CAP_AMBIENT_CLEAR_ALL = 4
CAP_SETPCAP = 8
CLONE_NEWNET = 0x40000000
SIOCGIFFLAGS = 0x8913
SIOCSIFFLAGS = 0x8914
IFF_UP = 0x1
API2_REQUEST_MAX_BYTES = 1024 * 1024
# Production clients receive engine stdout/stderr through memory-backed pipes.
# The benchmark uses bound tmpfs files so the outer PHP runner can read output
# after the sampler exits without introducing artificial block-backed dirty pages.
# O_SYNC preserves synchronous regular-file write semantics inside the sample.
SYNC_WRITE_FLAG = getattr(os, "O_SYNC", 0)
ENGINE_OUTPUT_OPEN_FLAGS = os.O_WRONLY | os.O_TRUNC | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0) | SYNC_WRITE_FLAG
ENGINE_OUTPUT_CAPTURE_ROOT = Path("/dev/shm")
ENGINE_OUTPUT_CAPTURE_CONTRACT = "root-bound-tmpfs-engine-output-v1"
ENGINE_OUTPUT_CAPTURE_MAX_BYTES = 16 * 1024 * 1024
FS_IOC_GETFLAGS = 0x80086601
FS_SYNC_FL = 0x00000008
FS_NOATIME_FL = 0x00000080
FS_DIRSYNC_FL = 0x00010000
RUNTIME_DIRECTORY_NAMES = {
    "HOME": "home",
    "XDG_CACHE_HOME": "xdg-cache",
    "XDG_CONFIG_HOME": "xdg-config",
    "XDG_DATA_HOME": "xdg-data",
    "XDG_RUNTIME_DIR": "xdg-runtime",
    "XDG_STATE_HOME": "xdg-state",
}
RUNTIME_INVENTORY_SCAN_LIMIT = 512


class MeasurementIncomplete(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def incomplete(code: str, message: str) -> MeasurementIncomplete:
    return MeasurementIncomplete(code, message)


def enable_loopback() -> None:
    if fcntl is None:
        raise incomplete("NETWORK_ISOLATION_UNAVAILABLE", "loopback control requires Linux fcntl")
    request = struct.pack("16sH22x", b"lo", 0)
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as control:
            response = fcntl.ioctl(control.fileno(), SIOCGIFFLAGS, request)
            flags = struct.unpack_from("H", response, 16)[0]
            if flags & IFF_UP == 0:
                fcntl.ioctl(control.fileno(), SIOCSIFFLAGS, struct.pack("16sH22x", b"lo", flags | IFF_UP))
                response = fcntl.ioctl(control.fileno(), SIOCGIFFLAGS, request)
                flags = struct.unpack_from("H", response, 16)[0]
    except OSError as error:
        raise incomplete("NETWORK_ISOLATION_UNAVAILABLE", f"cannot enable private loopback: {error}") from error
    if flags & IFF_UP == 0:
        raise incomplete("NETWORK_ISOLATION_INVALID", "private loopback remained down")


def enter_empty_network_namespace(host_namespace: str) -> dict[str, Any]:
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.unshare(CLONE_NEWNET) != 0:
        error = ctypes.get_errno()
        raise incomplete("NETWORK_ISOLATION_UNAVAILABLE", f"unshare(CLONE_NEWNET): {os.strerror(error)}")
    enable_loopback()
    engine_namespace = os.readlink("/proc/self/ns/net")
    # A sysfs mount can remain bound to the parent network namespace after
    # unshare(CLONE_NEWNET). Query the current namespace through the socket API
    # instead of accepting that stale mount as isolation evidence.
    interfaces = sorted(name for _, name in socket.if_nameindex())
    if engine_namespace == host_namespace or interfaces != ["lo"]:
        raise incomplete(
            "NETWORK_ISOLATION_INVALID",
            f"private namespace retained external interfaces: host={host_namespace} engine={engine_namespace} interfaces={interfaces}",
        )
    return {
        "mode": "linux-private-network-namespace-v1",
        "host_namespace": host_namespace,
        "engine_namespace": engine_namespace,
        "interfaces": interfaces,
    }


def probe_network_isolation() -> dict[str, Any]:
    host_namespace = os.readlink("/proc/self/ns/net")
    read_fd, write_fd = os.pipe2(os.O_CLOEXEC)
    pid = os.fork()
    if pid == 0:
        try:
            os.close(read_fd)
            payload = {"ok": True, "proof": enter_empty_network_namespace(host_namespace)}
            os.write(write_fd, json.dumps(payload, separators=(",", ":")).encode("ascii"))
            os._exit(0)
        except BaseException as error:
            with contextlib.suppress(BaseException):
                code = error.code if isinstance(error, MeasurementIncomplete) else "NETWORK_ISOLATION_UNAVAILABLE"
                os.write(
                    write_fd,
                    json.dumps({"ok": False, "code": code, "error": str(error)}, separators=(",", ":")).encode(),
                )
            os._exit(2)
    os.close(write_fd)
    payload = os.read(read_fd, 65536)
    os.close(read_fd)
    _, status_value = os.waitpid(pid, 0)
    try:
        decoded = json.loads(payload)
    except json.JSONDecodeError as error:
        raise incomplete("NETWORK_ISOLATION_UNAVAILABLE", f"invalid isolation probe: {error}") from error
    if not os.WIFEXITED(status_value) or os.WEXITSTATUS(status_value) != 0 or decoded.get("ok") is not True:
        raise incomplete(decoded.get("code", "NETWORK_ISOLATION_UNAVAILABLE"), decoded.get("error", "probe failed"))
    return decoded["proof"]


def require_linux(parser: argparse.ArgumentParser, platform: str) -> None:
    if platform != "linux":
        parser.error("cgroup-v2 accounting requires Linux")


@dataclass(frozen=True)
class EngineAccount:
    name: str
    uid: int
    gid: int
    home: str


@dataclass(frozen=True)
class ExecutableIdentity:
    path: Path
    sha256: str
    device: int
    inode: int


def require_broker_root(uid: int | None = None, euid: int | None = None) -> None:
    real = os.getuid() if uid is None else uid
    effective = os.geteuid() if euid is None else euid
    if real != 0 or effective != 0:
        raise incomplete("BROKER_ROOT_REQUIRED", "cgroup measurement broker must run with real and effective UID 0")


def resolve_engine_account(
    lookup: Callable[[str], Any] | None = None,
) -> EngineAccount:
    if lookup is None:
        if pwd is None:
            raise incomplete("ENGINE_ACCOUNT_UNAVAILABLE", f"account lookup is unavailable for {ENGINE_ACCOUNT}")
        lookup = pwd.getpwnam
    try:
        entry = lookup(ENGINE_ACCOUNT)
    except (KeyError, OSError) as error:
        raise incomplete("ENGINE_ACCOUNT_UNAVAILABLE", f"required account {ENGINE_ACCOUNT!r} is absent") from error
    uid = int(entry.pw_uid)
    gid = int(entry.pw_gid)
    if uid <= 0 or gid <= 0:
        raise incomplete("ENGINE_ACCOUNT_UNSAFE", f"{ENGINE_ACCOUNT!r} must have non-root UID and GID")
    home = str(entry.pw_dir)
    if home != ENGINE_ACCOUNT_HOME:
        raise incomplete(
            "ENGINE_ACCOUNT_UNSAFE",
            f"{ENGINE_ACCOUNT!r} must have canonical home {ENGINE_ACCOUNT_HOME!r}, got {home!r}",
        )
    account = EngineAccount(ENGINE_ACCOUNT, uid, gid, home)
    validate_engine_account_home(account)
    return account


def engine_account_can_write(path: Path, account: EngineAccount) -> bool:
    """Ask with the engine account's effective authority, including POSIX ACLs."""
    read_fd, write_fd = os.pipe2(os.O_CLOEXEC)
    pid = os.fork()
    if pid == 0:
        try:
            os.close(read_fd)
            os.setgroups([])
            os.setgid(account.gid)
            os.setuid(account.uid)
            writable = os.access(path, os.W_OK, effective_ids=True)
            os.write(write_fd, b"1" if writable else b"0")
            os._exit(0)
        except BaseException:
            os._exit(2)
    os.close(write_fd)
    result = os.read(read_fd, 1)
    os.close(read_fd)
    _, status_value = os.waitpid(pid, 0)
    if not os.WIFEXITED(status_value) or os.WEXITSTATUS(status_value) != 0 or result not in {b"0", b"1"}:
        raise incomplete("ENGINE_ACCOUNT_UNSAFE", "cannot prove engine-account home write authority")
    return result == b"1"


def validate_engine_account_home(account: EngineAccount) -> dict[str, Any]:
    path = Path(account.home)
    if account.home != ENGINE_ACCOUNT_HOME or not path.is_absolute() or ".." in path.parts:
        raise incomplete("ENGINE_ACCOUNT_UNSAFE", "engine account home is not the required canonical path")
    try:
        metadata = os.stat(path, follow_symlinks=False)
    except FileNotFoundError:
        return {"path": ENGINE_ACCOUNT_HOME, "state": "absent"}
    except OSError as error:
        raise incomplete("ENGINE_ACCOUNT_UNSAFE", f"cannot inspect engine account home: {error}") from error
    if not stat.S_ISDIR(metadata.st_mode):
        raise incomplete("ENGINE_ACCOUNT_UNSAFE", "engine account home must be absent or a non-writable directory")
    if engine_account_can_write(path, account):
        raise incomplete("ENGINE_ACCOUNT_UNSAFE", "engine account home is writable by the engine account")
    return {
        "path": ENGINE_ACCOUNT_HOME,
        "state": "non-writable",
        "identity": {"device": metadata.st_dev, "inode": metadata.st_ino},
    }


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="ascii")
    except (OSError, UnicodeError) as error:
        raise incomplete("CGROUP_INTERFACE_UNREADABLE", f"cannot read {path}: {error}") from error


def require_root_owned(metadata: os.stat_result, label: str, owner_uid: int, owner_gid: int) -> None:
    if metadata.st_uid != owner_uid or metadata.st_gid != owner_gid:
        raise incomplete(
            "CGROUP_OWNERSHIP_UNSAFE",
            f"{label} must be owned by {owner_uid}:{owner_gid}, got {metadata.st_uid}:{metadata.st_gid}",
        )
    if stat.S_IMODE(metadata.st_mode) & 0o022:
        raise incomplete("CGROUP_PERMISSIONS_UNSAFE", f"{label} permits group or other writes")


def identity_can_write(metadata: os.stat_result, uid: int, gid: int) -> bool:
    mode = stat.S_IMODE(metadata.st_mode)
    if metadata.st_uid == uid:
        return bool(mode & stat.S_IWUSR)
    if metadata.st_gid == gid:
        return bool(mode & stat.S_IWGRP)
    return bool(mode & stat.S_IWOTH)


def child_replaceable(parent: os.stat_result, child: os.stat_result, uid: int, gid: int) -> bool:
    if not identity_can_write(parent, uid, gid) or not stat.S_ISDIR(parent.st_mode):
        return False
    sticky = bool(stat.S_IMODE(parent.st_mode) & stat.S_ISVTX)
    return not sticky or uid in {parent.st_uid, child.st_uid}


def command_identity(command: list[str], account: EngineAccount) -> tuple[tuple[str, ...], ExecutableIdentity]:
    if not command or any(not isinstance(arg, str) or not arg or "\0" in arg for arg in command):
        raise incomplete("ENGINE_COMMAND_INVALID", "engine argv must contain nonempty NUL-free strings")
    api2 = len(command) == 2 and command[1] == "render-api2"
    adapter = len(command) >= 3 and command[1] == "render"
    if not api2 and not adapter:
        raise incomplete(
            "ENGINE_COMMAND_INVALID",
            "sampler accepts only fixed 'render-api2' or benchmark-adapter 'render <input>' argv",
        )
    requested = Path(command[0])
    if not requested.is_absolute():
        raise incomplete("ENGINE_COMMAND_INVALID", "engine executable path must be absolute")
    try:
        executable = requested.resolve(strict=True)
    except OSError as error:
        raise incomplete("ENGINE_EXECUTABLE_UNAVAILABLE", f"cannot resolve engine executable: {error}") from error
    if requested != executable:
        raise incomplete("ENGINE_EXECUTABLE_NOT_CANONICAL", f"engine executable must be canonical, got {requested}")
    try:
        metadata = os.stat(executable, follow_symlinks=False)
    except OSError as error:
        raise incomplete("ENGINE_EXECUTABLE_UNAVAILABLE", f"cannot stat {executable}: {error}") from error
    if not stat.S_ISREG(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) & 0o111 == 0:
        raise incomplete("ENGINE_EXECUTABLE_UNSAFE", f"engine executable is not a regular executable: {executable}")
    if identity_can_write(metadata, account.uid, account.gid):
        raise incomplete("ENGINE_EXECUTABLE_MUTABLE", f"engine account can write {executable}")

    child_metadata = metadata
    for parent_path in executable.parents:
        parent_metadata = os.stat(parent_path, follow_symlinks=False)
        if child_replaceable(parent_metadata, child_metadata, account.uid, account.gid):
            raise incomplete(
                "ENGINE_EXECUTABLE_MUTABLE",
                f"engine account can replace path component below {parent_path}",
            )
        child_metadata = parent_metadata

    digest = hashlib.sha256()
    with executable.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return tuple(command), ExecutableIdentity(executable, digest.hexdigest(), metadata.st_dev, metadata.st_ino)


def open_stdin_descriptor(stdin_path: str | None) -> int:
    if stdin_path is None:
        return os.open(os.devnull, os.O_RDONLY | os.O_CLOEXEC)

    requested = Path(stdin_path)
    if not requested.is_absolute():
        raise incomplete("ENGINE_STDIN_INVALID", "engine stdin path must be absolute")
    try:
        resolved = requested.resolve(strict=True)
    except OSError as error:
        raise incomplete("ENGINE_STDIN_UNAVAILABLE", f"cannot resolve engine stdin: {error}") from error
    if requested != resolved:
        raise incomplete("ENGINE_STDIN_INVALID", f"engine stdin path must be canonical, got {requested}")

    flags = os.O_RDONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(resolved, flags)
    except OSError as error:
        raise incomplete("ENGINE_STDIN_UNAVAILABLE", f"cannot open engine stdin: {error}") from error
    try:
        metadata = os.fstat(descriptor)
        path_metadata = os.stat(resolved, follow_symlinks=False)
        if not stat.S_ISREG(metadata.st_mode) or (metadata.st_dev, metadata.st_ino) != (
            path_metadata.st_dev,
            path_metadata.st_ino,
        ):
            raise incomplete("ENGINE_STDIN_UNSAFE", "engine stdin is not one identity-bound regular file")
        if metadata.st_nlink != 1:
            raise incomplete("ENGINE_STDIN_UNSAFE", "engine stdin must have exactly one hard link")
        if metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) & 0o222:
            raise incomplete(
                "ENGINE_STDIN_MUTABLE",
                "engine stdin must be root-owned and have no writable permission bits",
            )
        if metadata.st_size < 1 or metadata.st_size > API2_REQUEST_MAX_BYTES:
            raise incomplete(
                "ENGINE_STDIN_INVALID",
                f"engine stdin must contain 1..{API2_REQUEST_MAX_BYTES} bytes",
            )
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def verify_executable(identity: ExecutableIdentity) -> None:
    try:
        metadata = os.stat(identity.path, follow_symlinks=False)
    except OSError as error:
        raise incomplete("ENGINE_EXECUTABLE_REPLACED", f"cannot recheck {identity.path}: {error}") from error
    if (metadata.st_dev, metadata.st_ino) != (identity.device, identity.inode):
        raise incomplete("ENGINE_EXECUTABLE_REPLACED", f"engine executable changed: {identity.path}")


@dataclass
class BoundDirectory:
    path: Path
    fd: int
    device: int
    inode: int
    parent_fd: int | None = None
    name: str | None = None

    def close(self) -> None:
        if self.fd >= 0:
            os.close(self.fd)
            self.fd = -1

    def verify_path_identity(self) -> None:
        try:
            metadata = os.stat(self.path, follow_symlinks=False)
        except OSError as error:
            raise incomplete("CGROUP_PATH_REPLACED", f"cannot recheck {self.path}: {error}") from error
        if not stat.S_ISDIR(metadata.st_mode) or (metadata.st_dev, metadata.st_ino) != (self.device, self.inode):
            raise incomplete("CGROUP_PATH_REPLACED", f"bound cgroup path changed: {self.path}")


def open_bound_directory(path: Path, cgroup_root: Path) -> BoundDirectory:
    if not path.is_absolute():
        raise incomplete("CGROUP_PARENT_NOT_ABSOLUTE", "delegated parent must be an absolute path")
    try:
        canonical_root = cgroup_root.resolve(strict=True)
        canonical_path = path.resolve(strict=True)
    except OSError as error:
        raise incomplete("CGROUP_PARENT_UNAVAILABLE", f"cannot resolve delegated parent: {error}") from error
    if path != canonical_path:
        raise incomplete("CGROUP_PARENT_NOT_CANONICAL", f"delegated parent must be canonical, got {path}")
    try:
        relative = canonical_path.relative_to(canonical_root)
    except ValueError as error:
        raise incomplete("CGROUP_PARENT_OUTSIDE_V2", f"{canonical_path} is outside {canonical_root}") from error

    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    descriptors: list[int] = []
    bound_fd = -1
    try:
        descriptor = os.open(canonical_root, flags)
        descriptors.append(descriptor)
        for component in relative.parts:
            descriptor = os.open(component, flags, dir_fd=descriptor)
            descriptors.append(descriptor)
        bound_fd = os.dup(descriptors[-1])
        metadata = os.fstat(bound_fd)
    except OSError as error:
        if bound_fd >= 0:
            os.close(bound_fd)
        raise incomplete("CGROUP_PARENT_UNSAFE", f"cannot bind canonical delegated parent {path}: {error}") from error
    finally:
        for descriptor in descriptors:
            os.close(descriptor)
    bound = BoundDirectory(canonical_path, bound_fd, metadata.st_dev, metadata.st_ino)
    bound.verify_path_identity()
    return bound


def read_at(directory_fd: int, name: str, label: str) -> str:
    flags = os.O_RDONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(name, flags, dir_fd=directory_fd)
        with os.fdopen(descriptor, "r", encoding="ascii") as stream:
            return stream.read()
    except (OSError, UnicodeError) as error:
        raise incomplete("CGROUP_INTERFACE_UNREADABLE", f"cannot read {label}: {error}") from error


def read_bound(directory: BoundDirectory, name: str) -> str:
    return read_at(directory.fd, name, str(directory.path / name))


def write_bound(directory: BoundDirectory, name: str, value: str) -> None:
    flags = os.O_WRONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(name, flags, dir_fd=directory.fd)
        try:
            encoded = value.encode("ascii")
            if os.write(descriptor, encoded) != len(encoded):
                raise OSError("short write")
        finally:
            os.close(descriptor)
    except (OSError, UnicodeError) as error:
        raise incomplete("CGROUP_INTERFACE_UNWRITABLE", f"cannot write {directory.path / name}: {error}") from error


def parse_flat(raw: str, label: str, required: set[str] | frozenset[str] = frozenset()) -> dict[str, int]:
    values: dict[str, int] = {}
    for line in raw.splitlines():
        fields = line.split()
        if len(fields) != 2 or fields[0] in values:
            raise incomplete("CGROUP_COUNTER_INVALID", f"malformed counter line in {label}: {line!r}")
        try:
            value = int(fields[1])
        except ValueError as error:
            raise incomplete("CGROUP_COUNTER_INVALID", f"non-integer counter in {label}: {line!r}") from error
        if value < 0:
            raise incomplete("CGROUP_COUNTER_INVALID", f"negative counter in {label}: {line!r}")
        values[fields[0]] = value
    missing = required - values.keys()
    if missing:
        raise incomplete("CGROUP_COUNTER_MISSING", f"{label} omitted {sorted(missing)}")
    return values


def parse_io_stat(raw: str, label: str) -> dict[str, dict[str, int]]:
    devices: dict[str, dict[str, int]] = {}
    for line in raw.splitlines():
        fields = line.split()
        if not fields:
            raise incomplete("CGROUP_COUNTER_INVALID", f"malformed io.stat line in {label}: {line!r}")
        device = fields[0]
        major, separator, minor = device.partition(":")
        if (
            not separator
            or not major.isascii()
            or not major.isdigit()
            or not minor.isascii()
            or not minor.isdigit()
            or device in devices
        ):
            raise incomplete("CGROUP_COUNTER_INVALID", f"malformed io.stat line in {label}: {line!r}")
        counters: dict[str, int] = {}
        for field in fields[1:]:
            key, separator, raw = field.partition("=")
            if not separator or not key or key in counters:
                raise incomplete("CGROUP_COUNTER_INVALID", f"malformed io.stat field: {field!r}")
            try:
                value = int(raw)
            except ValueError as error:
                raise incomplete("CGROUP_COUNTER_INVALID", f"non-integer io.stat field: {field!r}") from error
            if value < 0:
                raise incomplete("CGROUP_COUNTER_INVALID", f"negative io.stat field: {field!r}")
            counters[key] = value
        required = frozenset({"rbytes", "wbytes", "rios", "wios"})
        present = required & counters.keys()
        if present and present != required:
            missing = sorted(required - present)
            raise incomplete("CGROUP_COUNTER_MISSING", f"io.stat device {device} omitted {missing}")
        # blkcg_print_one_stat emits only "MAJ:MIN " when all four primary
        # counters are zero, while policy-specific counters may still follow.
        for key in required:
            counters.setdefault(key, 0)
        devices[device] = counters
    return devices


def parse_integer(raw: str, label: str) -> int:
    raw = raw.strip()
    try:
        value = int(raw)
    except ValueError as error:
        raise incomplete("CGROUP_COUNTER_INVALID", f"expected integer in {label}, got {raw!r}") from error
    if value < 0:
        raise incomplete("CGROUP_COUNTER_INVALID", f"negative counter in {label}: {value}")
    return value


def cgroup_flat(
    cgroup: BoundDirectory,
    name: str,
    required: set[str] | frozenset[str] = frozenset(),
) -> dict[str, int]:
    return parse_flat(read_bound(cgroup, name), str(cgroup.path / name), required)


def cgroup_integer(cgroup: BoundDirectory, name: str) -> int:
    return parse_integer(read_bound(cgroup, name), str(cgroup.path / name))


def counter_snapshot(cgroup: BoundDirectory) -> dict[str, Any]:
    memory_stat = cgroup_flat(cgroup, "memory.stat", {"file_dirty", "file_writeback"})
    return {
        "cpu_stat": cgroup_flat(cgroup, "cpu.stat", {"usage_usec", "user_usec", "system_usec"}),
        "io_stat": parse_io_stat(read_bound(cgroup, "io.stat"), str(cgroup.path / "io.stat")),
        "memory_current_bytes": cgroup_integer(cgroup, "memory.current"),
        "memory_peak_bytes": cgroup_integer(cgroup, "memory.peak"),
        "memory_file_dirty_bytes": memory_stat["file_dirty"],
        "memory_file_writeback_bytes": memory_stat["file_writeback"],
        "pids_peak": cgroup_integer(cgroup, "pids.peak"),
        "cgroup_events": cgroup_flat(cgroup, "cgroup.events", {"populated", "frozen"}),
    }


def io_total(io_stat: dict[str, dict[str, int]], field: str) -> int:
    return sum(counters.get(field, 0) for counters in io_stat.values())


def require_zero_snapshot(snapshot: dict[str, Any], stage: str, populated: int, pids_peak: int) -> None:
    cpu = snapshot["cpu_stat"]
    if any(cpu[field] != 0 for field in ("usage_usec", "user_usec", "system_usec")):
        raise incomplete("CGROUP_NOT_FRESH", f"{stage} cpu.stat is not zero")
    if any(value != 0 for counters in snapshot["io_stat"].values() for value in counters.values()):
        raise incomplete("CGROUP_NOT_FRESH", f"{stage} io.stat is not zero")
    for field in (
        "memory_current_bytes",
        "memory_peak_bytes",
        "memory_file_dirty_bytes",
        "memory_file_writeback_bytes",
    ):
        if snapshot[field] != 0:
            raise incomplete("CGROUP_NOT_FRESH", f"{stage} {field} is not zero")
    if snapshot["pids_peak"] != pids_peak:
        raise incomplete("CGROUP_NOT_FRESH", f"{stage} pids.peak must be {pids_peak}")
    if snapshot["cgroup_events"]["populated"] != populated:
        raise incomplete("CGROUP_NOT_FRESH", f"{stage} populated must be {populated}")
    if snapshot["cgroup_events"]["frozen"] != 0:
        raise incomplete("CGROUP_FROZEN", f"{stage} cgroup is frozen")


def cgroup_relative(pid: str = "self", proc_root: Path = PROC) -> str:
    for line in read_text(proc_root / pid / "cgroup").splitlines():
        hierarchy, controllers, path = line.split(":", 2)
        if hierarchy == "0" and controllers == "" and path.startswith("/"):
            return path
    raise incomplete("CGROUP_V2_REQUIRED", f"{proc_root / pid / 'cgroup'} has no unified hierarchy")


def require_bound_security(
    bound: BoundDirectory,
    interfaces: set[str] | frozenset[str],
    owner_uid: int,
    owner_gid: int,
) -> None:
    require_root_owned(os.fstat(bound.fd), str(bound.path), owner_uid, owner_gid)
    for interface in interfaces:
        try:
            metadata = os.stat(interface, dir_fd=bound.fd, follow_symlinks=False)
        except OSError as error:
            raise incomplete("CGROUP_INTERFACES_MISSING", f"cannot stat {bound.path / interface}: {error}") from error
        if not stat.S_ISREG(metadata.st_mode):
            raise incomplete("CGROUP_INTERFACES_MISSING", f"not a regular cgroup interface: {bound.path / interface}")
        require_root_owned(metadata, str(bound.path / interface), owner_uid, owner_gid)


def require_delegated_parent(
    parent: Path,
    cgroup_root: Path = CGROUP_ROOT,
    proc_root: Path = PROC,
    owner_uid: int = 0,
    owner_gid: int = 0,
) -> BoundDirectory:
    root = cgroup_root.resolve(strict=True)
    if not (root / "cgroup.controllers").is_file():
        raise incomplete("CGROUP_V2_REQUIRED", f"{root} is not a cgroup-v2 mount")
    bound = open_bound_directory(parent, root)
    try:
        namespace_root = bound.path == root
        parent_interfaces = {"cgroup.controllers", "cgroup.procs", "cgroup.subtree_control", "cgroup.threads"}
        if not namespace_root:
            parent_interfaces.add("cgroup.type")
        require_bound_security(bound, frozenset(parent_interfaces), owner_uid, owner_gid)
        if os.fstat(bound.fd).st_dev != os.stat(root, follow_symlinks=False).st_dev:
            raise incomplete("CGROUP_PARENT_OUTSIDE_V2", "delegated parent is not on the cgroup-v2 mount")
        relative = cgroup_relative("self", proc_root).lstrip("/")
        try:
            self_path = (root / relative).resolve(strict=True)
        except OSError as error:
            raise incomplete("CGROUP_DELEGATION_INVALID", f"cannot resolve harness cgroup {relative!r}") from error
        expected_harness = bound.path / HARNESS_CHILD
        if self_path != expected_harness:
            raise incomplete(
                "CGROUP_DELEGATION_INVALID",
                f"harness must run in manager child {expected_harness}, got {self_path}",
            )
        harness = open_bound_directory(expected_harness, root)
        try:
            require_bound_security(
                harness,
                frozenset({"cgroup.procs", "cgroup.threads", "cgroup.type"}),
                owner_uid,
                owner_gid,
            )
        finally:
            harness.close()
        if not namespace_root and read_bound(bound, "cgroup.type").strip() != "domain":
            raise incomplete("CGROUP_DOMAIN_REQUIRED", f"{bound.path} is not a domain cgroup")

        controllers = set(read_bound(bound, "cgroup.controllers").split())
        enabled = set(read_bound(bound, "cgroup.subtree_control").split())
        for label, values in (("available", controllers), ("enabled", enabled)):
            missing = REQUIRED_CONTROLLERS - values
            if missing:
                raise incomplete("CGROUP_CONTROLLERS_MISSING", f"{label} controllers omit {sorted(missing)}")
        if read_bound(bound, "cgroup.procs").split():
            raise incomplete("CGROUP_PARENT_POPULATED", "delegated parent must be empty")
        if not os.access(".", os.W_OK, dir_fd=bound.fd) or not os.access(
            "cgroup.procs", os.W_OK, dir_fd=bound.fd, follow_symlinks=False
        ):
            raise incomplete("CGROUP_DELEGATION_INVALID", "delegated parent is not writable")

        with os.scandir(bound.fd) as entries:
            children = sorted(entry.name for entry in entries if entry.is_dir(follow_symlinks=False))
        if children != [HARNESS_CHILD]:
            raise incomplete(
                "CGROUP_NOT_EXCLUSIVE",
                f"delegated parent must contain only manager child {HARNESS_CHILD!r}, found {children!r}",
            )
        bound.verify_path_identity()
        return bound
    except BaseException:
        bound.close()
        raise


@contextlib.contextmanager
def exclusive_parent_lock(parent: Path, lock_root: Path | None = None) -> Iterator[None]:
    if fcntl is None:
        raise incomplete("CGROUP_V2_REQUIRED", "fcntl is unavailable")
    root = lock_root or Path(os.environ.get("XDG_RUNTIME_DIR", "/tmp"))
    digest = hashlib.sha256(str(parent.resolve()).encode()).hexdigest()[:16]
    lock_path = root / f"pliego-benchmark-cgroup-{os.getuid()}-{digest}.lock"
    flags = os.O_CREAT | os.O_RDWR | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(lock_path, flags, 0o600)
    except OSError as error:
        raise incomplete("CGROUP_LOCK_UNAVAILABLE", f"cannot open {lock_path}: {error}") from error
    try:
        metadata = os.fstat(descriptor)
        if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
            raise incomplete("CGROUP_LOCK_UNSAFE", f"unsafe ownership or mode on {lock_path}")
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise incomplete("CGROUP_BUSY", f"another sampler holds {lock_path}") from error
        yield
    finally:
        os.close(descriptor)


def create_leaf(
    parent: BoundDirectory,
    prefix: str = "pliego-render",
    owner_uid: int = 0,
    owner_gid: int = 0,
) -> BoundDirectory:
    parent.verify_path_identity()
    name = f"{prefix}-{os.getpid()}-{secrets.token_hex(8)}"
    child_path = parent.path / name
    try:
        os.mkdir(name, mode=0o700, dir_fd=parent.fd)
    except OSError as error:
        raise incomplete("CGROUP_CREATE_FAILED", f"cannot create unique direct child {child_path}: {error}") from error

    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(name, flags, dir_fd=parent.fd)
        metadata = os.fstat(descriptor)
    except OSError as error:
        with contextlib.suppress(OSError):
            os.rmdir(name, dir_fd=parent.fd)
        raise incomplete("CGROUP_CREATE_FAILED", f"cannot bind fresh child {child_path}: {error}") from error
    child = BoundDirectory(child_path, descriptor, metadata.st_dev, metadata.st_ino, parent.fd, name)
    try:
        required = {
            "cgroup.events",
            "cgroup.kill",
            "cgroup.max.depth",
            "cgroup.max.descendants",
            "cgroup.procs",
            "cgroup.subtree_control",
            "cgroup.threads",
            "cgroup.type",
            "cpu.stat",
            "io.stat",
            "memory.current",
            "memory.peak",
            "memory.stat",
            "pids.peak",
        }
        missing: list[str] = []
        for interface in required:
            try:
                interface_stat = os.stat(interface, dir_fd=child.fd, follow_symlinks=False)
            except OSError:
                missing.append(interface)
                continue
            if not stat.S_ISREG(interface_stat.st_mode):
                missing.append(interface)
        if missing:
            raise incomplete("CGROUP_INTERFACES_MISSING", f"fresh child omitted {sorted(missing)}")
        require_bound_security(child, required, owner_uid, owner_gid)
        if read_bound(child, "cgroup.type").strip() != "domain":
            raise incomplete("CGROUP_DOMAIN_REQUIRED", f"fresh child {child.path} is not a domain cgroup")
        if read_bound(child, "cgroup.subtree_control").split():
            raise incomplete("CGROUP_LEAF_DELEGATED", f"fresh child {child.path} delegates controllers")
        for interface in ("cgroup.max.depth", "cgroup.max.descendants"):
            write_bound(child, interface, "0\n")
            if read_bound(child, interface).strip() != "0":
                raise incomplete("CGROUP_LEAF_DELEGATED", f"cannot prohibit descendants in {child.path}")
        for interface in ("cgroup.procs", "cgroup.kill"):
            writable = os.open(
                interface,
                os.O_WRONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0),
                dir_fd=child.fd,
            )
            os.close(writable)
        parent.verify_path_identity()
        child.verify_path_identity()
        return child
    except BaseException:
        with contextlib.suppress(MeasurementIncomplete, OSError):
            remove_bound_leaf(child, parent)
        child.close()
        raise


def read_stat(pid: int, proc_root: Path = PROC) -> dict[str, int] | None:
    try:
        raw = (proc_root / str(pid) / "stat").read_text(encoding="ascii")
    except (OSError, UnicodeError):
        return None
    end = raw.rfind(")")
    fields = raw[end + 2 :].split() if end >= 0 else []
    if len(fields) < 22:
        return None
    try:
        return {
            "pid": pid,
            "ppid": int(fields[1]),
            "process_group": int(fields[2]),
            "session": int(fields[3]),
            "start_ticks": int(fields[19]),
            "rss_pages": max(0, int(fields[21])),
        }
    except ValueError:
        return None


def read_pss_kib(pid: int, proc_root: Path = PROC) -> int | None:
    try:
        for line in (proc_root / str(pid) / "smaps_rollup").read_text(encoding="ascii").splitlines():
            if line.startswith("Pss:"):
                return int(line.split()[1])
    except (OSError, UnicodeError, ValueError):
        pass
    return None


def cgroup_pids(cgroup: BoundDirectory) -> list[int]:
    found: set[int] = set()
    cgroup.verify_path_identity()
    with os.scandir(cgroup.fd) as entries:
        descendants = [entry.name for entry in entries if entry.is_dir(follow_symlinks=False) or entry.is_symlink()]
    if descendants:
        raise incomplete("CGROUP_PATH_UNSAFE", f"non-delegated render cgroup has descendants: {descendants}")
    for raw in read_bound(cgroup, "cgroup.procs").split():
        try:
            pid = int(raw)
        except ValueError as error:
            raise incomplete("CGROUP_MEMBERSHIP_INVALID", f"non-integer cgroup PID: {raw!r}") from error
        if pid > 0:
            found.add(pid)
    return sorted(found)


def scan_cgroup(
    cgroup: BoundDirectory,
    include_pss: bool,
    root_identity: tuple[int, int],
    proc_root: Path = PROC,
) -> list[dict[str, int | None]]:
    found: list[dict[str, int | None]] = []
    for pid in cgroup_pids(cgroup):
        process = read_stat(pid, proc_root)
        if process is None:
            continue
        if pid == root_identity[0] and process["start_ticks"] != root_identity[1]:
            raise incomplete("ROOT_IDENTITY_CHANGED", f"root PID {pid} start time changed")
        process["pss_kib"] = read_pss_kib(pid, proc_root) if include_pss else None
        found.append(process)
    return found


def prctl(option: int, arg2: int = 0) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(option, arg2, 0, 0, 0) != 0:
        value = ctypes.get_errno()
        raise OSError(value, os.strerror(value))


def drop_engine_authority(account: EngineAccount) -> None:
    try:
        last_capability = int(read_text(PROC / "sys/kernel/cap_last_cap").strip())
    except ValueError as error:
        raise OSError(errno.EINVAL, "invalid cap_last_cap") from error
    for capability in [value for value in range(last_capability + 1) if value != CAP_SETPCAP] + [CAP_SETPCAP]:
        prctl(PR_CAPBSET_DROP, capability)
    prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL)
    os.setgroups([])
    os.setresgid(account.gid, account.gid, account.gid)
    os.setresuid(account.uid, account.uid, account.uid)
    prctl(PR_SET_NO_NEW_PRIVS, 1)


def migration_write_probes(paths: dict[str, Path]) -> dict[str, dict[str, str]]:
    results: dict[str, dict[str, str]] = {}
    flags = os.O_WRONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    for label, directory in paths.items():
        results[label] = {}
        for interface in ("cgroup.procs", "cgroup.threads"):
            try:
                descriptor = os.open(directory / interface, flags)
            except OSError as error:
                results[label][interface] = errno.errorcode.get(error.errno, f"ERRNO_{error.errno}")
            else:
                os.close(descriptor)
                results[label][interface] = "WRITABLE"
    return results


def validate_engine_temporary_directory(requested: Path, account: EngineAccount) -> str:
    if not requested.is_absolute():
        raise incomplete("ENGINE_TEMPORARY_DIRECTORY_UNSAFE", "engine temporary path must be absolute")
    try:
        resolved = requested.resolve(strict=True)
        metadata = os.stat(resolved, follow_symlinks=False)
    except OSError as error:
        raise incomplete(
            "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
            f"cannot bind engine temporary path {requested}: {error}",
        ) from error
    if requested != resolved or not stat.S_ISDIR(metadata.st_mode):
        raise incomplete("ENGINE_TEMPORARY_DIRECTORY_UNSAFE", "engine temporary path must be a canonical directory")
    if metadata.st_uid != account.uid or metadata.st_gid != account.gid or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise incomplete(
            "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
            f"engine temporary path must be owned by {account.uid}:{account.gid} with mode 0700",
        )
    if mountinfo_filesystem_type(resolved) != "ext4":
        raise incomplete(
            "ENGINE_TEMPORARY_BACKING_UNSAFE",
            "engine temporary path must be backed by ext4",
        )
    flags = directory_inode_flags(resolved)
    if flags & FS_DIRSYNC_FL == 0:
        raise incomplete(
            "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
            "engine temporary path must carry the inherited FS_DIRSYNC_FL flag",
        )
    if flags & FS_SYNC_FL == 0:
        raise incomplete(
            "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
            "engine temporary path must carry the inherited FS_SYNC_FL flag",
        )
    if flags & FS_NOATIME_FL == 0:
        raise incomplete(
            "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
            "engine temporary path must carry the inherited FS_NOATIME_FL flag",
        )
    return str(resolved)


def decode_mountinfo_path(value: str) -> str:
    return value.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")


def mountinfo_filesystem_type(path: Path, mountinfo: str | None = None) -> str | None:
    resolved = path.resolve(strict=True)
    text = (PROC / "self" / "mountinfo").read_text(encoding="utf-8") if mountinfo is None else mountinfo
    selected: tuple[int, str] | None = None
    for line in text.splitlines():
        fields = line.split()
        try:
            separator = fields.index("-")
            mount = Path(decode_mountinfo_path(fields[4]))
            filesystem = fields[separator + 1]
        except (IndexError, ValueError):
            continue
        if resolved == mount or mount in resolved.parents:
            candidate = (len(mount.parts), filesystem)
            if selected is None or candidate[0] > selected[0]:
                selected = candidate
    return selected[1] if selected is not None else None


def output_capture_root_binding(path: Path, metadata: os.stat_result) -> dict[str, Any]:
    return {
        "path": str(path),
        "identity": {"device": metadata.st_dev, "inode": metadata.st_ino},
        "owner_uid": metadata.st_uid,
        "owner_gid": metadata.st_gid,
        "mode": stat.S_IMODE(metadata.st_mode),
    }


def output_capture_stream_binding(path: Path, metadata: os.stat_result) -> dict[str, Any]:
    return {
        **output_capture_root_binding(path, metadata),
        "link_count": metadata.st_nlink,
        "size_bytes": metadata.st_size,
    }


def engine_output_capture_snapshot(
    stdout_path: Path,
    stderr_path: Path,
    account: EngineAccount,
    expected: dict[str, Any] | None = None,
    capture_root: Path = ENGINE_OUTPUT_CAPTURE_ROOT,
    broker_uid: int = 0,
    broker_gid: int = 0,
    max_bytes_per_stream: int = ENGINE_OUTPUT_CAPTURE_MAX_BYTES,
) -> dict[str, Any]:
    """Bind root-created memory-backed output paths the engine cannot replace."""

    try:
        resolved_root = capture_root.resolve(strict=True)
        root_metadata = os.stat(capture_root, follow_symlinks=False)
    except OSError as error:
        raise incomplete(
            "ENGINE_OUTPUT_CAPTURE_UNSAFE",
            f"cannot bind engine output capture root {capture_root}: {error}",
        ) from error
    if capture_root != resolved_root or not stat.S_ISDIR(root_metadata.st_mode):
        raise incomplete(
            "ENGINE_OUTPUT_CAPTURE_UNSAFE",
            "engine output capture root must be a canonical directory",
        )
    if (
        root_metadata.st_uid != broker_uid
        or root_metadata.st_gid != broker_gid
        or stat.S_IMODE(root_metadata.st_mode) != 0o1777
    ):
        raise incomplete(
            "ENGINE_OUTPUT_CAPTURE_UNSAFE",
            f"engine output capture root must be owned by {broker_uid}:{broker_gid} with mode 01777",
        )
    try:
        filesystem = mountinfo_filesystem_type(capture_root)
    except (OSError, UnicodeError) as error:
        raise incomplete(
            "ENGINE_OUTPUT_CAPTURE_BACKING_UNSAFE",
            f"cannot determine engine output capture backing: {error}",
        ) from error
    if filesystem != "tmpfs":
        raise incomplete(
            "ENGINE_OUTPUT_CAPTURE_BACKING_UNSAFE",
            "engine output capture root must be backed by tmpfs",
        )

    streams: dict[str, dict[str, Any]] = {}
    for label, requested in (("stdout", stdout_path), ("stderr", stderr_path)):
        try:
            resolved = requested.resolve(strict=True)
            metadata = os.stat(requested, follow_symlinks=False)
        except OSError as error:
            raise incomplete(
                "ENGINE_OUTPUT_CAPTURE_UNSAFE",
                f"cannot bind engine {label} capture path {requested}: {error}",
            ) from error
        if (
            not requested.is_absolute()
            or requested != resolved
            or requested.parent != capture_root
            or not stat.S_ISREG(metadata.st_mode)
        ):
            raise incomplete(
                "ENGINE_OUTPUT_CAPTURE_UNSAFE",
                f"engine {label} capture must be a canonical regular direct child of {capture_root}",
            )
        if (
            metadata.st_uid != broker_uid
            or metadata.st_gid != broker_gid
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_nlink != 1
            or metadata.st_dev != root_metadata.st_dev
            or identity_can_write(metadata, account.uid, account.gid)
            or child_replaceable(root_metadata, metadata, account.uid, account.gid)
        ):
            raise incomplete(
                "ENGINE_OUTPUT_CAPTURE_UNSAFE",
                f"engine {label} capture must be a single-link same-tmpfs broker-owned mode-0600 file "
                "that is non-replaceable by the engine account",
            )
        if expected is None and metadata.st_size != 0:
            raise incomplete(
                "ENGINE_OUTPUT_CAPTURE_UNSAFE",
                f"engine {label} capture must be empty before launch",
            )
        if metadata.st_size > max_bytes_per_stream:
            raise incomplete(
                "ENGINE_OUTPUT_CAPTURE_LIMIT_EXCEEDED",
                f"engine {label} capture exceeded {max_bytes_per_stream} bytes",
            )
        streams[label] = output_capture_stream_binding(requested, metadata)

    stdout_identity = (
        streams["stdout"]["identity"]["device"],
        streams["stdout"]["identity"]["inode"],
    )
    stderr_identity = (
        streams["stderr"]["identity"]["device"],
        streams["stderr"]["identity"]["inode"],
    )
    if stdout_path == stderr_path or stdout_identity == stderr_identity:
        raise incomplete(
            "ENGINE_OUTPUT_CAPTURE_UNSAFE",
            "engine stdout and stderr capture paths must bind distinct files",
        )
    snapshot = {
        "root": output_capture_root_binding(capture_root, root_metadata),
        "streams": streams,
    }
    if expected is not None:
        if snapshot["root"] != expected["root"]:
            raise incomplete(
                "ENGINE_OUTPUT_CAPTURE_REPLACED",
                "engine output capture root identity changed during execution",
            )
        for label in ("stdout", "stderr"):
            before = {key: value for key, value in expected["streams"][label].items() if key != "size_bytes"}
            after = {key: value for key, value in snapshot["streams"][label].items() if key != "size_bytes"}
            if after != before:
                raise incomplete(
                    "ENGINE_OUTPUT_CAPTURE_REPLACED",
                    f"engine {label} capture path identity changed during execution",
                )
    return snapshot


def open_bound_engine_output(path: str, snapshot: dict[str, Any], label: str) -> int:
    requested = Path(path)
    root_binding = snapshot["root"]
    binding = snapshot["streams"][label]
    root_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    try:
        root_descriptor = os.open(requested.parent, root_flags)
    except OSError as error:
        raise incomplete(
            "ENGINE_OUTPUT_CAPTURE_UNSAFE",
            f"cannot open bound engine output capture root: {error}",
        ) from error
    try:
        root_metadata = os.fstat(root_descriptor)
        if output_capture_root_binding(requested.parent, root_metadata) != root_binding:
            raise incomplete(
                "ENGINE_OUTPUT_CAPTURE_REPLACED",
                "engine output capture root identity changed before launch",
            )
        descriptor = os.open(requested.name, ENGINE_OUTPUT_OPEN_FLAGS, dir_fd=root_descriptor)
        try:
            metadata = os.fstat(descriptor)
            observed = output_capture_stream_binding(requested, metadata)
            if not stat.S_ISREG(metadata.st_mode) or observed != binding:
                raise incomplete(
                    "ENGINE_OUTPUT_CAPTURE_REPLACED",
                    f"engine {label} capture identity changed before launch",
                )
            return descriptor
        except BaseException:
            os.close(descriptor)
            raise
    finally:
        os.close(root_descriptor)


def directory_inode_flags(path: Path) -> int:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0))
    try:
        flags = ctypes.c_ulong()
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.ioctl(descriptor, FS_IOC_GETFLAGS, ctypes.byref(flags)) != 0:
            error = ctypes.get_errno()
            raise incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
                f"cannot read engine temporary inode flags: {os.strerror(error)}",
            )
        return int(flags.value)
    finally:
        os.close(descriptor)


def engine_temporary_directory_identity(path: Path) -> tuple[int, int]:
    try:
        metadata = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise incomplete(
            "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
            f"cannot bind engine temporary directory identity: {error}",
        ) from error
    if not stat.S_ISDIR(metadata.st_mode):
        raise incomplete("ENGINE_TEMPORARY_DIRECTORY_UNSAFE", "engine temporary path is no longer a directory")
    return metadata.st_dev, metadata.st_ino


def controlled_storage_root_identity(path: Path) -> tuple[int, int]:
    """Bind the canonical root that carries the inherited storage flags."""

    try:
        resolved = path.resolve(strict=True)
        metadata = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise incomplete(
            "ENGINE_TEMPORARY_ROOT_UNSAFE",
            f"cannot bind controlled storage root: {error}",
        ) from error
    if path != resolved or not stat.S_ISDIR(metadata.st_mode):
        raise incomplete(
            "ENGINE_TEMPORARY_ROOT_UNSAFE",
            "controlled storage root must be a canonical directory",
        )
    if metadata.st_uid != 0 or metadata.st_gid != 0 or stat.S_IMODE(metadata.st_mode) != 0o711:
        raise incomplete(
            "ENGINE_TEMPORARY_ROOT_UNSAFE",
            "controlled storage root must be root-owned with mode 0711",
        )
    if mountinfo_filesystem_type(path) != "ext4":
        raise incomplete("ENGINE_TEMPORARY_BACKING_UNSAFE", "controlled storage root must use ext4")
    flags = directory_inode_flags(path)
    required = FS_DIRSYNC_FL | FS_SYNC_FL | FS_NOATIME_FL
    if flags & required != required:
        raise incomplete(
            "ENGINE_TEMPORARY_ROOT_UNSAFE",
            "controlled storage root must carry FS_NOATIME_FL, FS_SYNC_FL, and FS_DIRSYNC_FL",
        )
    return metadata.st_dev, metadata.st_ino


def revalidate_engine_temporary_directory(
    path: Path, account: EngineAccount, expected_identity: tuple[int, int]
) -> None:
    validate_engine_temporary_directory(path, account)
    if engine_temporary_directory_identity(path) != expected_identity:
        raise incomplete(
            "ENGINE_TEMPORARY_DIRECTORY_REPLACED",
            "engine temporary directory identity changed during execution",
        )


def bind_native_api2_storage(
    cwd: Path,
    temporary: Path,
    account: EngineAccount,
    expected: dict[str, tuple[int, int]] | None = None,
    require_pristine_job: bool = False,
) -> dict[str, tuple[int, int]]:
    """Bind the native job and scratch siblings inside one controlled sandbox."""

    job = Path(validate_engine_temporary_directory(cwd, account))
    scratch = Path(validate_engine_temporary_directory(temporary, account))
    if job.name != "job" or scratch.name != "temporary" or job.parent != scratch.parent:
        raise incomplete(
            "NATIVE_API2_STORAGE_UNSAFE",
            "native API 2 cwd and TMPDIR must be job/temporary siblings",
        )
    sandbox = Path(validate_engine_temporary_directory(job.parent, account))
    controlled_root = sandbox.parent
    bindings = {
        "controlled_root": controlled_storage_root_identity(controlled_root),
        "sandbox": engine_temporary_directory_identity(sandbox),
        "job": engine_temporary_directory_identity(job),
        "temporary": engine_temporary_directory_identity(scratch),
    }
    if len(set(bindings.values())) != len(bindings):
        raise incomplete(
            "NATIVE_API2_STORAGE_UNSAFE",
            "native API 2 storage directories must have distinct identities",
        )
    if require_pristine_job:
        try:
            with os.scandir(job) as entries:
                job_entries = {entry.name: entry for entry in entries}
            with os.scandir(scratch) as entries:
                scratch_entries = list(entries)
        except OSError as error:
            raise incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                f"cannot inspect native API 2 storage: {error}",
            ) from error
        if sorted(job_entries) != ["input", "input-manifest.json"] or scratch_entries:
            raise incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                "native API 2 storage was not pristine before measurement",
            )
        input_entry = job_entries["input"]
        manifest_entry = job_entries["input-manifest.json"]
        try:
            input_metadata = input_entry.stat(follow_symlinks=False)
            manifest_metadata = manifest_entry.stat(follow_symlinks=False)
        except OSError as error:
            raise incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                f"cannot inspect native API 2 input types: {error}",
            ) from error
        if (
            not input_entry.is_dir(follow_symlinks=False)
            or not manifest_entry.is_file(follow_symlinks=False)
            or input_metadata.st_uid != account.uid
            or input_metadata.st_gid != account.gid
            or stat.S_IMODE(input_metadata.st_mode) != 0o700
            or manifest_metadata.st_uid != account.uid
            or manifest_metadata.st_gid != account.gid
            or stat.S_IMODE(manifest_metadata.st_mode) != 0o600
        ):
            raise incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                "native API 2 input must be an owned mode-0700 directory and manifest an owned mode-0600 file",
            )
    if len({device for device, _ in bindings.values()}) != 1:
        raise incomplete(
            "NATIVE_API2_STORAGE_UNSAFE",
            "native API 2 storage bindings must share one filesystem device",
        )
    if expected is not None and bindings != expected:
        raise incomplete(
            "NATIVE_API2_STORAGE_REPLACED",
            "native API 2 storage identity changed during execution",
        )
    return bindings


def native_api2_storage_snapshot(
    cwd: Path,
    temporary: Path,
    bindings: dict[str, tuple[int, int]],
) -> dict[str, Any]:
    """Serialize the bound native API 2 storage topology for retained evidence."""

    paths = {
        "controlled_root": cwd.parent.parent,
        "sandbox": cwd.parent,
        "job": cwd,
        "temporary": temporary,
    }
    return {
        "contract": "native-api2-storage-bindings-v1",
        "bindings": {
            name: {
                "path": str(paths[name]),
                "identity": {"device": identity[0], "inode": identity[1]},
            }
            for name, identity in bindings.items()
        },
    }


def prepare_runtime_directories(root: Path, account: EngineAccount) -> dict[str, str]:
    environment: dict[str, str] = {}
    for variable, name in RUNTIME_DIRECTORY_NAMES.items():
        path = root / name
        try:
            os.mkdir(path, 0o700)
            os.chown(path, account.uid, account.gid)
            os.chmod(path, 0o700)
        except OSError as error:
            raise incomplete(
                "ENGINE_RUNTIME_DIRECTORY_UNAVAILABLE",
                f"cannot provision private {variable} directory: {error}",
            ) from error
        validate_engine_temporary_directory(path, account)
        try:
            with os.scandir(path) as entries:
                list(entries)
            descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        except OSError as error:
            raise incomplete(
                "ENGINE_RUNTIME_DIRECTORY_UNAVAILABLE",
                f"cannot seal private {variable} directory: {error}",
            ) from error
        environment[variable] = str(path)
    return environment


def directory_diagnostics(
    root: Path,
    label: str,
    expected_identity: tuple[int, int],
    limit: int = 6,
    scan_limit: int = RUNTIME_INVENTORY_SCAN_LIMIT,
) -> str:
    """Describe bounded failure state without following a replaced root."""

    files: list[tuple[int, str]] = []
    entries_examined = 0
    truncated = False
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
    root_descriptor: int | None = None
    pending: list[tuple[int, str]] = []
    try:
        root_descriptor = os.open(root, flags)
        root_metadata = os.fstat(root_descriptor)
        if (root_metadata.st_dev, root_metadata.st_ino) != expected_identity:
            return f"{label}-inventory-unavailable=identity-mismatch"
        pending.append((root_descriptor, ""))
        root_descriptor = None
        while pending and not truncated:
            current_descriptor, prefix = pending.pop()
            try:
                with os.scandir(current_descriptor) as entries:
                    for entry in entries:
                        name = entry.name
                        if entries_examined >= scan_limit:
                            truncated = True
                            break
                        entries_examined += 1
                        metadata = entry.stat(follow_symlinks=False)
                        relative = f"{prefix}/{name}" if prefix else name
                        if stat.S_ISDIR(metadata.st_mode):
                            child_descriptor: int | None = os.open(name, flags, dir_fd=current_descriptor)
                            try:
                                child_metadata = os.fstat(child_descriptor)
                                if (child_metadata.st_dev, child_metadata.st_ino) != (
                                    metadata.st_dev,
                                    metadata.st_ino,
                                ):
                                    return f"{label}-inventory-unavailable=descendant-identity-mismatch"
                                pending.append((child_descriptor, relative))
                                child_descriptor = None
                            finally:
                                if child_descriptor is not None:
                                    os.close(child_descriptor)
                            continue
                        kind = "file" if stat.S_ISREG(metadata.st_mode) else "other"
                        if len(relative) > 120:
                            relative = "..." + relative[-117:]
                        files.append(
                            (
                                metadata.st_mtime_ns,
                                f"{relative}:{kind}:size={metadata.st_size}:blocks={metadata.st_blocks}",
                            )
                        )
            finally:
                os.close(current_descriptor)
    except OSError as error:
        return f"{label}-inventory-unavailable={type(error).__name__}:{error.errno}"
    finally:
        if root_descriptor is not None:
            with contextlib.suppress(OSError):
                os.close(root_descriptor)
        for descriptor, _prefix in pending:
            with contextlib.suppress(OSError):
                os.close(descriptor)
    files.sort(key=lambda item: (-item[0], item[1]))
    retained = [description for _, description in files[:limit]]
    return (
        f"{label}-files-newest={retained!r}; {label}-file-count={len(files)};"
        f" {label}-entries-examined={entries_examined}; {label}-scan-truncated={str(truncated).lower()}"
    )


def runtime_directory_diagnostics(
    root: Path,
    expected_identity: tuple[int, int],
    limit: int = 6,
    scan_limit: int = RUNTIME_INVENTORY_SCAN_LIMIT,
) -> str:
    """Describe a bounded sample of the newest browser-runtime files after failure."""

    return directory_diagnostics(root, "browser-runtime", expected_identity, limit, scan_limit)


def browser_failure_diagnostics(root: Path, expected_identity: tuple[int, int]) -> str:
    """Keep browser failure context bounded without replacing the original failure."""

    try:
        return runtime_directory_diagnostics(root, expected_identity)
    except Exception as error:
        error_number = getattr(error, "errno", None)
        suffix = str(error_number) if isinstance(error_number, int) else "unknown"
        return f"browser-runtime-inventory-unavailable={type(error).__name__}:{suffix}"


def bounded_output_tail(path: Path, limit: int = 2048) -> str:
    """Retain a bounded escaped engine diagnostic without changing the failure."""

    try:
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - limit))
            payload = stream.read(limit)
    except OSError as error:
        return f"unavailable:{type(error).__name__}:{error.errno}"
    text = payload.decode("utf-8", "replace")
    return json.dumps(text, ensure_ascii=True)


def output_path_metadata(path: Path) -> dict[str, Any]:
    """Retain bounded output-file identity without reading protocol payloads."""

    try:
        metadata = path.stat(follow_symlinks=False)
    except OSError as error:
        return {"status": "unavailable", "error": f"{type(error).__name__}:{error.errno}"}
    kind = "file" if stat.S_ISREG(metadata.st_mode) else "other"
    return {
        "status": "present",
        "kind": kind,
        "size": metadata.st_size,
        "blocks": metadata.st_blocks,
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
    }


def engine_failure_diagnostics(
    cwd: Path,
    engine_tmpdir: Path,
    stdout_path: Path,
    stderr_path: Path,
    engine_tmpdir_identity: tuple[int, int],
    native_api2_storage: dict[str, tuple[int, int]] | None,
) -> str:
    """Keep target-neutral storage context bounded after a quiescence failure."""

    roots = [("engine-temporary", engine_tmpdir, engine_tmpdir_identity)]
    if native_api2_storage is not None:
        roots.append(("engine-sandbox", cwd.parent, native_api2_storage["sandbox"]))
    inventories = [directory_diagnostics(path, label, identity) for label, path, identity in roots]
    outputs = {
        "stdout": output_path_metadata(stdout_path),
        "stderr": output_path_metadata(stderr_path),
    }
    inventories.append(f"engine-output-metadata={json.dumps(outputs, sort_keys=True, separators=(',', ':'))}")
    inventories.append(f"engine-stderr-tail={bounded_output_tail(stderr_path)}")
    return "; ".join(inventories)


def require_bound_directory_entries(
    path: Path,
    expected_identity: tuple[int, int],
    expected_entries: set[str],
) -> None:
    """Fail closed if a bound runtime directory gained or lost an entry."""

    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
    descriptor: int | None = None
    try:
        descriptor = os.open(path, flags)
        metadata = os.fstat(descriptor)
        if (metadata.st_dev, metadata.st_ino) != expected_identity:
            raise incomplete(
                "ENGINE_RUNTIME_DIRECTORY_REPLACED",
                f"private runtime directory identity changed: {path}",
            )
        seen: set[str] = set()
        with os.scandir(descriptor) as entries:
            for entry in entries:
                if entry.name not in expected_entries or entry.name in seen:
                    raise incomplete(
                        "ENGINE_RUNTIME_DIRECTORY_NOT_EMPTY",
                        f"private runtime directory retained an unexpected entry: {path / entry.name}",
                    )
                seen.add(entry.name)
        if seen != expected_entries:
            raise incomplete(
                "ENGINE_RUNTIME_DIRECTORY_REPLACED",
                f"private runtime directory omitted a bound entry: {path}",
            )
    except MeasurementIncomplete:
        raise
    except OSError as error:
        raise incomplete(
            "ENGINE_RUNTIME_DIRECTORY_UNAVAILABLE",
            f"cannot inspect private runtime directory {path}: {error}",
        ) from error
    finally:
        if descriptor is not None:
            with contextlib.suppress(OSError):
                os.close(descriptor)


def runtime_directory_bindings(
    root: Path,
    root_identity: tuple[int, int],
    environment: dict[str, str],
    account: EngineAccount,
    expected: dict[str, tuple[int, int]] | None = None,
) -> dict[str, Any]:
    if set(environment) != set(RUNTIME_DIRECTORY_NAMES):
        raise incomplete("ENGINE_RUNTIME_DIRECTORY_UNSAFE", "private runtime environment has unexpected variables")
    bindings: dict[str, Any] = {}
    for variable, name in RUNTIME_DIRECTORY_NAMES.items():
        path = Path(environment[variable])
        if path != root / name or not path.is_absolute():
            raise incomplete("ENGINE_RUNTIME_DIRECTORY_UNSAFE", f"{variable} escaped the private temporary root")
        validate_engine_temporary_directory(path, account)
        identity = engine_temporary_directory_identity(path)
        if expected is not None and identity != expected[variable]:
            raise incomplete("ENGINE_RUNTIME_DIRECTORY_REPLACED", f"private {variable} directory identity changed")
        bindings[variable] = {
            "relative_path": name,
            "identity": {"device": identity[0], "inode": identity[1]},
        }
    identities = {(item["identity"]["device"], item["identity"]["inode"]) for item in bindings.values()}
    if len(identities) != len(bindings) or root_identity in identities:
        raise incomplete("ENGINE_RUNTIME_DIRECTORY_UNSAFE", "private runtime directories do not have unique identities")
    require_bound_directory_entries(root, root_identity, set(RUNTIME_DIRECTORY_NAMES.values()))
    for variable, binding in bindings.items():
        identity = (binding["identity"]["device"], binding["identity"]["inode"])
        require_bound_directory_entries(Path(environment[variable]), identity, set())
    return {
        "contract": "runtime-path-bindings-v1",
        "temporary_root": {
            "path": str(root),
            "identity": {"device": root_identity[0], "inode": root_identity[1]},
        },
        "bindings": bindings,
    }


def engine_temporary_directory(command: tuple[str, ...], account: EngineAccount, explicit: str | None) -> str:
    if len(command) >= 2 and command[1] == "render":
        positions = [index for index, value in enumerate(command) if value == "--artifacts"]
        if len(positions) != 1 or positions[0] + 1 >= len(command):
            raise incomplete("ENGINE_COMMAND_INVALID", "benchmark adapter argv must contain one --artifacts path")
        artifacts = validate_engine_temporary_directory(Path(command[positions[0] + 1]), account)
        if explicit is None:
            raise incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_REQUIRED",
                "benchmark adapter requires an explicit private temporary directory",
            )
        temporary = validate_engine_temporary_directory(Path(explicit), account)
        if temporary == artifacts:
            raise incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
                "adapter temporary path must be distinct from its artifact directory",
            )
        return temporary
    if len(command) >= 2 and command[1] == "render-api2":
        if explicit is None:
            raise incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_REQUIRED",
                "native API 2 benchmark requires an explicit private temporary directory",
            )
        return validate_engine_temporary_directory(Path(explicit), account)
    if explicit is not None:
        return validate_engine_temporary_directory(Path(explicit), account)
    return "/tmp"


def engine_environment(
    account: EngineAccount,
    temporary_directory: str = "/tmp",
    runtime_directories: dict[str, str] | None = None,
) -> dict[str, str]:
    allowed = {
        "BROWSERSHOT_CHROME_PATH",
        "BROWSERSHOT_NODE_BINARY",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LD_LIBRARY_PATH",
        "PATH",
        "TZ",
    }
    environment = {
        name: value for name, value in os.environ.items() if name in allowed or name.startswith("FONTCONFIG_")
    }
    environment.update(
        {
            "HOME": account.home,
            "LOGNAME": account.name,
            "PATH": environment.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
            "TMPDIR": temporary_directory,
            "USER": account.name,
        }
    )
    if runtime_directories is not None:
        environment.update(runtime_directories)
    return environment


def fork_stopped(
    command: tuple[str, ...],
    cwd: str,
    stdin_path: str | None,
    stdout_path: str,
    stderr_path: str,
    account: EngineAccount,
    probe_paths: dict[str, Path],
    isolate_network: bool,
    host_network_namespace: str,
    temporary_directory: str,
    runtime_directories: dict[str, str] | None,
    output_capture_before: dict[str, Any],
) -> tuple[int, int]:
    if SYNC_WRITE_FLAG == 0:
        raise incomplete("SYNCHRONOUS_ENGINE_OUTPUT_UNAVAILABLE", "the host exposes no O_SYNC")
    descriptors = [open_stdin_descriptor(stdin_path)]
    try:
        descriptors.append(open_bound_engine_output(stdout_path, output_capture_before, "stdout"))
        descriptors.append(open_bound_engine_output(stderr_path, output_capture_before, "stderr"))
    except BaseException:
        for descriptor in descriptors:
            with contextlib.suppress(OSError):
                os.close(descriptor)
        raise
    launch_environment = engine_environment(account, temporary_directory, runtime_directories)
    try:
        handshake_read, handshake_write = os.pipe2(os.O_CLOEXEC)
    except BaseException:
        for descriptor in descriptors:
            with contextlib.suppress(OSError):
                os.close(descriptor)
        raise
    try:
        pid = os.fork()
        if pid == 0:
            try:
                os.close(handshake_read)
                os.setsid()
                os.chdir(cwd)
                for target, descriptor in enumerate(descriptors):
                    os.dup2(descriptor, target)
                for descriptor in descriptors:
                    if descriptor > 2:
                        os.close(descriptor)
                os.kill(os.getpid(), signal.SIGSTOP)
                network_isolation = enter_empty_network_namespace(host_network_namespace) if isolate_network else None
                drop_engine_authority(account)
                payload = {
                    "ok": True,
                    "executable": command[0],
                    "cwd": os.getcwd(),
                    "tmpdir": launch_environment["TMPDIR"],
                    "identity": {
                        "uid": os.getuid(),
                        "euid": os.geteuid(),
                        "gid": os.getgid(),
                        "egid": os.getegid(),
                        "groups": os.getgroups(),
                    },
                    "executable_accessible": os.access(command[0], os.X_OK),
                    "executable_writable": os.access(command[0], os.W_OK),
                    "cwd_accessible": os.access(cwd, os.R_OK | os.X_OK),
                    "migration_write_probes": migration_write_probes(probe_paths),
                }
                if network_isolation is not None:
                    payload["network_isolation"] = network_isolation
                os.write(handshake_write, json.dumps(payload, separators=(",", ":")).encode("ascii"))
                os.kill(os.getpid(), signal.SIGSTOP)
                os.execve(
                    command[0],
                    command,
                    launch_environment,
                )
            except BaseException as error:
                with contextlib.suppress(BaseException):
                    payload = {"ok": False, "error": f"{type(error).__name__}: {error}"}
                    os.write(handshake_write, json.dumps(payload, separators=(",", ":")).encode("utf-8", "replace"))
                    os.write(2, f"cgroup launcher: {error}\n".encode("utf-8", "replace"))
                os._exit(127)
    except BaseException:
        os.close(handshake_read)
        raise
    finally:
        os.close(handshake_write)
        for descriptor in descriptors:
            with contextlib.suppress(OSError):
                os.close(descriptor)

    waited, status_value = os.waitpid(pid, os.WUNTRACED)
    if waited != pid or not os.WIFSTOPPED(status_value) or os.WSTOPSIG(status_value) != signal.SIGSTOP:
        os.close(handshake_read)
        raise incomplete("LAUNCH_HANDSHAKE_FAILED", f"child {pid} did not stop before exec")
    return pid, handshake_read


def read_process_security(pid: int, proc_root: Path = PROC) -> dict[str, Any]:
    try:
        fields = {}
        for line in (proc_root / str(pid) / "status").read_text(encoding="ascii").splitlines():
            key, separator, value = line.partition(":")
            if separator:
                fields[key] = value.strip()
        return {
            "uid": [int(value) for value in fields["Uid"].split()],
            "gid": [int(value) for value in fields["Gid"].split()],
            "supplementary_groups": [int(value) for value in fields["Groups"].split()],
            "cap_inheritable_hex": fields["CapInh"].lower(),
            "cap_permitted_hex": fields["CapPrm"].lower(),
            "cap_effective_hex": fields["CapEff"].lower(),
            "cap_bounding_hex": fields["CapBnd"].lower(),
            "cap_ambient_hex": fields["CapAmb"].lower(),
            "no_new_privs": int(fields["NoNewPrivs"]),
        }
    except (KeyError, OSError, UnicodeError, ValueError) as error:
        raise incomplete(
            "ENGINE_IDENTITY_UNAVAILABLE", f"cannot read security status for child {pid}: {error}"
        ) from error


def finish_authority_handshake(
    pid: int,
    handshake_read: int,
    account: EngineAccount,
    staging: BoundDirectory,
    cgroup_root: Path,
    root_identity: tuple[int, int],
    proc_root: Path,
) -> dict[str, Any]:
    os.kill(pid, signal.SIGCONT)
    waited, status_value = os.waitpid(pid, os.WUNTRACED)
    try:
        raw = os.read(handshake_read, 65536)
    finally:
        os.close(handshake_read)
    if waited != pid or not os.WIFSTOPPED(status_value) or os.WSTOPSIG(status_value) != signal.SIGSTOP:
        raise incomplete("AUTHORITY_DROP_FAILED", f"child {pid} exited before the verified second stop")
    try:
        handshake = json.loads(raw)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise incomplete("AUTHORITY_DROP_FAILED", f"invalid launcher authority handshake: {error}") from error
    if not isinstance(handshake, dict) or handshake.get("ok") is not True:
        raise incomplete("AUTHORITY_DROP_FAILED", f"launcher authority drop failed: {handshake!r}")

    process = read_stat(pid, proc_root)
    if process is None or (pid, process["start_ticks"]) != root_identity:
        raise incomplete("ROOT_IDENTITY_CHANGED", f"launcher identity changed during authority drop: {pid}")
    expected = "/" + staging.path.relative_to(cgroup_root.resolve(strict=True)).as_posix()
    if cgroup_relative(str(pid), proc_root) != expected or cgroup_pids(staging) != [pid]:
        raise incomplete("CGROUP_JOIN_FAILED", "authority-dropped launcher left its staging cgroup")

    security = read_process_security(pid, proc_root)
    expected_uid = [account.uid] * 4
    expected_gid = [account.gid] * 4
    if security["uid"] != expected_uid or security["gid"] != expected_gid:
        raise incomplete("ENGINE_IDENTITY_UNSAFE", f"launcher retained unexpected credentials: {security}")
    if security["supplementary_groups"]:
        raise incomplete("ENGINE_IDENTITY_UNSAFE", "launcher retained supplementary groups")
    for field in (
        "cap_inheritable_hex",
        "cap_permitted_hex",
        "cap_effective_hex",
        "cap_bounding_hex",
        "cap_ambient_hex",
    ):
        try:
            capability_value = int(security[field], 16)
        except ValueError as error:
            raise incomplete("ENGINE_CAPABILITIES_RETAINED", f"invalid {field}={security[field]}") from error
        if capability_value != 0:
            raise incomplete("ENGINE_CAPABILITIES_RETAINED", f"launcher retained {field}={security[field]}")
    if security["no_new_privs"] != 1:
        raise incomplete("ENGINE_PRIVILEGES_UNSAFE", "launcher did not set no_new_privs")
    if handshake.get("executable_accessible") is not True or handshake.get("cwd_accessible") is not True:
        raise incomplete(
            "ENGINE_PATH_INACCESSIBLE",
            "engine account path access failed: "
            f"executable={handshake.get('executable')!r} "
            f"cwd={handshake.get('cwd')!r} "
            f"identity={handshake.get('identity')!r} "
            f"executable_accessible={handshake.get('executable_accessible')!r} "
            f"cwd_accessible={handshake.get('cwd_accessible')!r}",
        )
    if handshake.get("executable_writable") is not False:
        raise incomplete("ENGINE_EXECUTABLE_MUTABLE", "engine account can write its executable")
    launch_context: dict[str, str] = {}
    for name in ("cwd", "tmpdir"):
        value = handshake.get(name)
        candidate = Path(value) if isinstance(value, str) else None
        if candidate is None or not candidate.is_absolute() or ".." in candidate.parts or str(candidate) != value:
            raise incomplete(
                "ENGINE_LAUNCH_CONTEXT_INVALID",
                f"launcher reported a non-canonical {name}: {value!r}",
            )
        launch_context[name] = value
    probes = handshake.get("migration_write_probes")
    if not isinstance(probes, dict) or set(probes) != {"parent", "harness", "staging", "measurement"}:
        raise incomplete("ENGINE_MIGRATION_PROBE_INVALID", "launcher omitted cgroup migration write probes")
    for results in probes.values():
        if (
            not isinstance(results, dict)
            or set(results) != {"cgroup.procs", "cgroup.threads"}
            or any(result not in {"EACCES", "EPERM"} for result in results.values())
        ):
            raise incomplete("ENGINE_CGROUP_WRITABLE", f"engine account can access a migration interface: {probes}")
    result = {
        "account": account.name,
        "uid": account.uid,
        "gid": account.gid,
        "launch_context": launch_context,
        "status": security,
        "migration_write_probes": probes,
    }
    network = handshake.get("network_isolation")
    if network is not None:
        if (
            not isinstance(network, dict)
            or network.get("mode") != "linux-private-network-namespace-v1"
            or network.get("interfaces") != ["lo"]
            or network.get("host_namespace") == network.get("engine_namespace")
        ):
            raise incomplete("NETWORK_ISOLATION_INVALID", f"invalid launcher network proof: {network!r}")
        actual_host = os.readlink(proc_root / "self" / "ns" / "net")
        actual_engine = os.readlink(proc_root / str(pid) / "ns" / "net")
        if network.get("host_namespace") != actual_host or network.get("engine_namespace") != actual_engine:
            raise incomplete("NETWORK_ISOLATION_INVALID", f"launcher network proof changed: {network!r}")
        result["network_isolation"] = network
    return result


def move_stopped_child(
    pid: int,
    cgroup: BoundDirectory,
    cgroup_root: Path,
    proc_root: Path,
) -> tuple[int, int]:
    try:
        write_bound(cgroup, "cgroup.procs", f"{pid}\n")
    except MeasurementIncomplete as error:
        raise incomplete("CGROUP_JOIN_FAILED", f"cannot move child {pid} into {cgroup.path}: {error}") from error
    process = read_stat(pid, proc_root)
    if process is None:
        raise incomplete("ROOT_IDENTITY_UNAVAILABLE", f"cannot read /proc identity for stopped child {pid}")
    identity = (pid, process["start_ticks"])
    if pid not in cgroup_pids(cgroup):
        raise incomplete("CGROUP_JOIN_FAILED", f"child {pid} is absent from {cgroup.path}/cgroup.procs")
    expected = "/" + cgroup.path.relative_to(cgroup_root.resolve(strict=True)).as_posix()
    if cgroup_relative(str(pid), proc_root) != expected:
        raise incomplete("CGROUP_JOIN_FAILED", f"child {pid} did not enter {expected}")
    cgroup.verify_path_identity()
    return identity


def exit_fields(status_value: int) -> tuple[int, int | None]:
    if os.WIFEXITED(status_value):
        return os.WEXITSTATUS(status_value), None
    if os.WIFSIGNALED(status_value):
        child_signal = os.WTERMSIG(status_value)
        return 128 + child_signal, child_signal
    raise incomplete("ROOT_WAIT_INVALID", f"unexpected wait status {status_value}")


def remove_bound_leaf(cgroup: BoundDirectory, parent: BoundDirectory) -> None:
    parent.verify_path_identity()
    cgroup.verify_path_identity()
    if cgroup.parent_fd != parent.fd or not cgroup.name:
        raise incomplete("CGROUP_PATH_UNSAFE", "render cgroup is not the bound direct child")
    metadata = os.stat(cgroup.name, dir_fd=parent.fd, follow_symlinks=False)
    if not stat.S_ISDIR(metadata.st_mode) or (metadata.st_dev, metadata.st_ino) != (cgroup.device, cgroup.inode):
        raise incomplete("CGROUP_PATH_REPLACED", f"render cgroup path changed: {cgroup.path}")
    events = cgroup_flat(cgroup, "cgroup.events", {"populated"})
    if events["populated"] != 0 or read_bound(cgroup, "cgroup.procs").split():
        raise incomplete("CGROUP_CLEANUP_FAILED", "refusing to remove a populated render cgroup")
    with os.scandir(cgroup.fd) as entries:
        descendants = [entry.name for entry in entries if entry.is_dir(follow_symlinks=False) or entry.is_symlink()]
    if descendants:
        raise incomplete("CGROUP_CLEANUP_FAILED", f"refusing recursive cleanup; child cgroups remain: {descendants}")
    os.rmdir(cgroup.name, dir_fd=parent.fd)
    try:
        os.stat(cgroup.name, dir_fd=parent.fd, follow_symlinks=False)
    except FileNotFoundError:
        pass
    else:
        raise incomplete("CGROUP_CLEANUP_FAILED", f"render cgroup still exists: {cgroup.path}")
    parent.verify_path_identity()


def force_cleanup(cgroup: BoundDirectory, parent: BoundDirectory, root_pid: int | None) -> None:
    with contextlib.suppress(MeasurementIncomplete):
        write_bound(cgroup, "cgroup.kill", "1\n")
        deadline = time.monotonic() + KILL_GRACE_MS / 1000.0
        while time.monotonic() < deadline:
            with contextlib.suppress(MeasurementIncomplete):
                if cgroup_flat(cgroup, "cgroup.events", {"populated"})["populated"] == 0:
                    break
            time.sleep(0.01)
    if root_pid is not None:
        with contextlib.suppress(ChildProcessError):
            waited, _ = os.waitpid(root_pid, os.WNOHANG)
            if waited == 0:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(root_pid, signal.SIGKILL)
                os.waitpid(root_pid, 0)
    with contextlib.suppress(MeasurementIncomplete, OSError):
        remove_bound_leaf(cgroup, parent)


def wait_for_accounting_quiescence(
    cgroup: BoundDirectory,
    poll_interval_ms: float,
    timeout_ms: float,
) -> tuple[dict[str, Any], dict[str, int | float]]:
    started = time.monotonic()
    previous: dict[str, Any] | None = None
    snapshot: dict[str, Any] | None = None
    reads = 0
    while True:
        snapshot = counter_snapshot(cgroup)
        reads += 1
        clean = snapshot["memory_file_dirty_bytes"] == 0 and snapshot["memory_file_writeback_bytes"] == 0
        stable = (
            previous is not None
            and snapshot["io_stat"] == previous["io_stat"]
            and snapshot["cpu_stat"] == previous["cpu_stat"]
        )
        if (
            clean
            and stable
            and previous["memory_file_dirty_bytes"] == 0
            and previous["memory_file_writeback_bytes"] == 0
        ):
            return snapshot, {
                "poll_interval_ms": poll_interval_ms,
                "timeout_ms": timeout_ms,
                "duration_ms": round((time.monotonic() - started) * 1000.0, 3),
                "reads": reads,
                "stable_reads": 2,
                "stable_observations": [
                    {
                        "cpu_stat": observation["cpu_stat"],
                        "io_stat": observation["io_stat"],
                        "memory_file_dirty_bytes": observation["memory_file_dirty_bytes"],
                        "memory_file_writeback_bytes": observation["memory_file_writeback_bytes"],
                    }
                    for observation in (previous, snapshot)
                ],
            }
        if (time.monotonic() - started) * 1000.0 >= timeout_ms:
            raise incomplete(
                "CGROUP_ACCOUNTING_NOT_QUIESCENT",
                "memory writeback did not clear or cpu.stat/io.stat did not stabilize; "
                f"dirty={snapshot['memory_file_dirty_bytes']} writeback={snapshot['memory_file_writeback_bytes']}",
            )
        previous = snapshot
        time.sleep(poll_interval_ms / 1000.0)


def sample_command(
    command: list[str],
    cwd: str,
    stdout_path: str,
    stderr_path: str,
    interval_ms: float,
    pss_interval_ms: float,
    descendant_grace_ms: float,
    settle_timeout_ms: float,
    cgroup_parent: Path,
    cgroup_root: Path = CGROUP_ROOT,
    proc_root: Path = PROC,
    lock_root: Path | None = None,
    isolate_network: bool = False,
    stdin_path: str | None = None,
    temporary_directory: str | None = None,
) -> dict[str, Any]:
    if resource is None:
        raise incomplete("CGROUP_V2_REQUIRED", "Linux resource accounting is unavailable")
    if not hasattr(os, "pidfd_open"):
        raise incomplete("PIDFD_REQUIRED", "os.pidfd_open is unavailable")
    require_broker_root()
    account = resolve_engine_account()
    account_home_before = validate_engine_account_home(account)
    argv, executable = command_identity(command, account)
    output_capture_before = engine_output_capture_snapshot(Path(stdout_path), Path(stderr_path), account)
    engine_tmpdir = engine_temporary_directory(tuple(command), account, temporary_directory)
    engine_tmpdir_identity = engine_temporary_directory_identity(Path(engine_tmpdir))
    target_classification = runtime_target(argv)
    native_api2_storage = (
        bind_native_api2_storage(
            Path(cwd),
            Path(engine_tmpdir),
            account,
            require_pristine_job=True,
        )
        if len(argv) >= 2 and argv[1] == "render-api2"
        else None
    )
    private_browser_runtime = target_classification == BROWSERSHOT_TARGET
    runtime_directories = prepare_runtime_directories(Path(engine_tmpdir), account) if private_browser_runtime else None
    runtime_bindings_before = (
        runtime_directory_bindings(Path(engine_tmpdir), engine_tmpdir_identity, runtime_directories, account)
        if runtime_directories is not None
        else None
    )
    host_network_namespace = os.readlink(proc_root / "self" / "ns" / "net")
    parent = require_delegated_parent(cgroup_parent, cgroup_root, proc_root)
    child: BoundDirectory | None = None
    staging: BoundDirectory | None = None
    root_pid: int | None = None
    handshake_read: int | None = None
    root_in_measurement = False
    root_reaped = False
    usage_start = resource.getrusage(resource.RUSAGE_SELF)

    try:
        with exclusive_parent_lock(parent.path, lock_root):
            parent.verify_path_identity()
            child = create_leaf(parent)
            staging = create_leaf(parent, "pliego-launch")
            empty = counter_snapshot(child)
            require_zero_snapshot(empty, "empty", populated=0, pids_peak=0)
            require_zero_snapshot(counter_snapshot(staging), "staging_empty", populated=0, pids_peak=0)

            probe_paths = {
                "parent": parent.path,
                "harness": parent.path / HARNESS_CHILD,
                "staging": staging.path,
                "measurement": child.path,
            }
            root_pid, handshake_read = fork_stopped(
                argv,
                cwd,
                stdin_path,
                stdout_path,
                stderr_path,
                account,
                probe_paths,
                isolate_network,
                host_network_namespace,
                engine_tmpdir,
                runtime_directories,
                output_capture_before,
            )
            root_identity = move_stopped_child(root_pid, staging, cgroup_root, proc_root)
            launch_security = finish_authority_handshake(
                root_pid,
                handshake_read,
                account,
                staging,
                cgroup_root,
                root_identity,
                proc_root,
            )
            handshake_read = None
            expected_launch_context = {"cwd": str(Path(cwd)), "tmpdir": engine_tmpdir}
            if launch_security["launch_context"] != expected_launch_context:
                raise incomplete(
                    "ENGINE_LAUNCH_CONTEXT_INVALID",
                    "launcher cwd or TMPDIR differs from the bound sampler request",
                )
            launch_security.update(
                {
                    "argv": list(argv),
                    "executable": {
                        "path": str(executable.path),
                        "sha256": executable.sha256,
                        "device": executable.device,
                        "inode": executable.inode,
                    },
                }
            )
            verify_executable(executable)
            measurement_identity = move_stopped_child(root_pid, child, cgroup_root, proc_root)
            root_in_measurement = True
            if measurement_identity != root_identity or cgroup_pids(staging):
                raise incomplete("ROOT_IDENTITY_CHANGED", "launcher identity changed while entering measurement cgroup")
            after_join = counter_snapshot(child)
            require_zero_snapshot(after_join, "after_join", populated=1, pids_peak=1)
            initial_members = scan_cgroup(child, include_pss=False, root_identity=root_identity, proc_root=proc_root)
            if [(row["pid"], row["start_ticks"]) for row in initial_members] != [root_identity]:
                raise incomplete("ROOT_IDENTITY_UNBOUND", "stopped root was not the sole initial cgroup member")
            remove_bound_leaf(staging, parent)
            staging.close()
            staging = None

            page_size = os.sysconf("SC_PAGE_SIZE")
            samples: list[dict[str, Any]] = []
            processes: dict[tuple[int, int], dict[str, int | float]] = {}
            next_pss = 0.0

            def take_sample(started: float, now: float) -> None:
                nonlocal next_pss
                elapsed_ms = round(max(0.0, (now - started) * 1000.0), 3)
                include_pss = now >= next_pss
                if include_pss:
                    next_pss = now + pss_interval_ms / 1000.0
                members = scan_cgroup(child, include_pss, root_identity, proc_root)
                raw_processes: list[dict[str, int | None]] = []
                for member in members:
                    identity = (int(member["pid"]), int(member["start_ticks"]))
                    row = processes.setdefault(
                        identity,
                        {
                            "pid": identity[0],
                            "start_ticks": identity[1],
                            "first_seen_ms": elapsed_ms,
                            "last_seen_ms": elapsed_ms,
                        },
                    )
                    row["last_seen_ms"] = elapsed_ms
                    raw_processes.append(
                        {
                            "pid": identity[0],
                            "start_ticks": identity[1],
                            "rss_pages": int(member["rss_pages"]),
                            "pss_kib": member["pss_kib"],
                        }
                    )
                pss = [row["pss_kib"] for row in raw_processes]
                samples.append(
                    {
                        "elapsed_ms": elapsed_ms,
                        "cgroup_memory_current_bytes": cgroup_integer(child, "memory.current"),
                        "sampled_summed_rss_kib_lower_bound": sum(
                            int(row["rss_pages"]) * page_size // 1024 for row in raw_processes
                        ),
                        "sampled_summed_pss_kib_lower_bound": (
                            sum(int(value) for value in pss)
                            if include_pss and raw_processes and all(value is not None for value in pss)
                            else None
                        ),
                        "processes": raw_processes,
                    }
                )

            pidfd = os.pidfd_open(root_pid)
            poller = select.poll()
            poller.register(pidfd, select.POLLIN)
            started = time.monotonic()
            next_pss = started + pss_interval_ms / 1000.0
            os.kill(root_pid, signal.SIGCONT)
            next_sample = started + interval_ms / 1000.0
            status_value: int | None = None
            root_ended = started
            root_exit_observation: dict[str, Any] | None = None
            try:
                while status_value is None:
                    timeout = max(0.0, next_sample - time.monotonic())
                    ready = poller.poll(math.ceil(timeout * 1000.0))
                    now = time.monotonic()
                    if ready:
                        waited, status_value = os.waitpid(root_pid, 0)
                        if waited != root_pid:
                            raise incomplete("ROOT_WAIT_INVALID", f"waited for unexpected PID {waited}")
                        root_reaped = True
                        root_ended = now
                    if now >= next_sample or ready:
                        take_sample(started, now)
                        if ready:
                            root_exit_processes = samples[-1]["processes"]
                            root_exit_observation = {
                                "member_count": len(root_exit_processes),
                                "members": [
                                    {
                                        "pid": int(member["pid"]),
                                        "start_ticks": int(member["start_ticks"]),
                                    }
                                    for member in root_exit_processes[:16]
                                ],
                            }
                        while next_sample <= now:
                            next_sample += interval_ms / 1000.0
            finally:
                os.close(pidfd)

            kill_used = False
            lingering_before_kill: list[dict[str, int]] = []
            drain_deadline = root_ended + descendant_grace_ms / 1000.0
            while cgroup_flat(child, "cgroup.events", {"populated"})["populated"] != 0:
                now = time.monotonic()
                if now >= drain_deadline:
                    kill_used = True
                    lingering_before_kill = [
                        {"pid": int(row["pid"]), "start_ticks": int(row["start_ticks"])}
                        for row in scan_cgroup(child, False, root_identity, proc_root)
                    ]
                    write_bound(child, "cgroup.kill", "1\n")
                    kill_deadline = now + KILL_GRACE_MS / 1000.0
                    while cgroup_flat(child, "cgroup.events", {"populated"})["populated"] != 0:
                        if time.monotonic() >= kill_deadline:
                            raise incomplete("CGROUP_CLEANUP_FAILED", "cgroup.kill did not drain the render cgroup")
                        time.sleep(min(interval_ms, 25.0) / 1000.0)
                    break
                take_sample(started, now)
                time.sleep(min(interval_ms / 1000.0, max(0.0, drain_deadline - now)))

            drained_at = time.monotonic()
            take_sample(started, drained_at)
            try:
                final, settle = wait_for_accounting_quiescence(child, interval_ms, settle_timeout_ms)
            except MeasurementIncomplete as error:
                if error.code == "CGROUP_ACCOUNTING_NOT_QUIESCENT":
                    exit_code, child_signal = exit_fields(status_value)
                    failure_counters = counter_snapshot(child)
                    failure_members = scan_cgroup(child, False, root_identity, proc_root)
                    lifecycle = {
                        "accounting_failure": {
                            "dirty": failure_counters["memory_file_dirty_bytes"],
                            "writeback": failure_counters["memory_file_writeback_bytes"],
                            "populated": failure_counters["cgroup_events"]["populated"],
                            "member_count": len(failure_members),
                        },
                        "root_exit_observation": root_exit_observation,
                        "root_exit_code": exit_code,
                        "root_signal": child_signal,
                        "engine_wall_ms": round((root_ended - started) * 1000.0, 3),
                        "descendant_drain_ms": round((drained_at - root_ended) * 1000.0, 3),
                        "kill_used": kill_used,
                        "lingering_before_kill": lingering_before_kill[:32],
                    }
                    diagnostics = engine_failure_diagnostics(
                        Path(cwd),
                        Path(engine_tmpdir),
                        Path(stdout_path),
                        Path(stderr_path),
                        engine_tmpdir_identity,
                        native_api2_storage,
                    )
                    raise incomplete(
                        error.code,
                        f"{error}; engine-lifecycle={json.dumps(lifecycle, sort_keys=True, separators=(',', ':'))}; "
                        f"{diagnostics}",
                    ) from error
                raise
            if final["cgroup_events"]["populated"] != 0:
                raise incomplete("CGROUP_CLEANUP_FAILED", "render cgroup repopulated during accounting settle")
            revalidate_engine_temporary_directory(Path(engine_tmpdir), account, engine_tmpdir_identity)
            native_api2_path_bindings = None
            if native_api2_storage is not None:
                native_api2_storage_after = bind_native_api2_storage(
                    Path(cwd),
                    Path(engine_tmpdir),
                    account,
                    expected=native_api2_storage,
                )
                native_api2_path_bindings = {
                    "pre": native_api2_storage_snapshot(Path(cwd), Path(engine_tmpdir), native_api2_storage),
                    "post": native_api2_storage_snapshot(Path(cwd), Path(engine_tmpdir), native_api2_storage_after),
                }
            account_home_after = validate_engine_account_home(account)
            if account_home_after != account_home_before:
                raise incomplete("ENGINE_ACCOUNT_HOME_REPLACED", "engine account home state changed during execution")
            output_capture_after = engine_output_capture_snapshot(
                Path(stdout_path),
                Path(stderr_path),
                account,
                expected=output_capture_before,
            )
            runtime_path_bindings = None
            if runtime_directories is not None and runtime_bindings_before is not None:
                expected_runtime_identities = {
                    variable: (binding["identity"]["device"], binding["identity"]["inode"])
                    for variable, binding in runtime_bindings_before["bindings"].items()
                }
                runtime_path_bindings = {
                    "pre": runtime_bindings_before,
                    "post": runtime_directory_bindings(
                        Path(engine_tmpdir),
                        engine_tmpdir_identity,
                        runtime_directories,
                        account,
                        expected_runtime_identities,
                    ),
                }
            launch_security["temporary_storage"] = {
                "access_time": "FS_NOATIME_FL",
                "directory_sync": "FS_DIRSYNC_FL",
                "file_sync": "FS_SYNC_FL",
                "filesystem": "ext4",
                "runtime_environment": runtime_contract(target_classification),
                "native_api2_path_bindings": native_api2_path_bindings,
                "runtime_target": target_classification,
                "account_home": account_home_after,
                "runtime_path_bindings": runtime_path_bindings,
                "scope": "per-invocation-private",
            }
            launch_security["output_capture"] = {
                "contract": ENGINE_OUTPUT_CAPTURE_CONTRACT,
                "filesystem": "tmpfs",
                "max_bytes_per_stream": ENGINE_OUTPUT_CAPTURE_MAX_BYTES,
                "write_sync": "O_SYNC",
                "pre": output_capture_before,
                "post": output_capture_after,
            }

            exit_code, child_signal = exit_fields(status_value)
            usage_end = resource.getrusage(resource.RUSAGE_SELF)
            wall_ms = (root_ended - started) * 1000.0
            elapsed = [float(sample["elapsed_ms"]) for sample in samples]
            gaps = [later - earlier for earlier, later in zip(elapsed, elapsed[1:])]
            rss_peaks = [int(sample["sampled_summed_rss_kib_lower_bound"]) for sample in samples]
            pss_peaks = [
                int(sample["sampled_summed_pss_kib_lower_bound"])
                for sample in samples
                if sample["sampled_summed_pss_kib_lower_bound"] is not None
            ]
            result = {
                "method": "linux-cgroup-v2-v1",
                "scope": "fresh-root-owned-cgroup-subtree",
                "delegated_parent": str(parent.path),
                "cgroup_path": str(child.path),
                "root_pid": root_identity[0],
                "root_start_ticks": root_identity[1],
                "launch_security": launch_security,
                "wall_ms": round(wall_ms, 3),
                "exit_code": exit_code,
                "signal": child_signal,
                "cpu_user_ms": round(final["cpu_stat"]["user_usec"] / 1000.0, 3),
                "cpu_sys_ms": round(final["cpu_stat"]["system_usec"] / 1000.0, 3),
                "memory_current_bytes": final["memory_current_bytes"],
                "memory_peak_bytes": final["memory_peak_bytes"],
                "read_bytes": io_total(final["io_stat"], "rbytes"),
                "write_bytes": io_total(final["io_stat"], "wbytes"),
                "read_operations": io_total(final["io_stat"], "rios"),
                "write_operations": io_total(final["io_stat"], "wios"),
                "cgroup_drained": True,
                "drain_ms": round((drained_at - root_ended) * 1000.0, 3),
                "accounting_settle": settle,
                "cleanup": {
                    "kill_used": kill_used,
                    "kill_grace_ms": KILL_GRACE_MS,
                    "lingering_before_kill": lingering_before_kill,
                },
                "counters": {"empty": empty, "after_join": after_join, "final": final},
                "sampled_diagnostics": {
                    "coverage": (
                        "periodic post-exec cgroup membership snapshots; RSS and PSS peaks are lower bounds "
                        "and may miss processes shorter than the interval"
                    ),
                    "sample_interval_ms": interval_ms,
                    "pss_interval_ms": pss_interval_ms,
                    "page_size_bytes": page_size,
                    "max_sample_gap_ms": round(max(gaps, default=0.0), 3),
                    "sampled_peak_summed_rss_kib_lower_bound": max(rss_peaks, default=0),
                    "sampled_peak_summed_pss_kib_lower_bound": max(pss_peaks) if pss_peaks else None,
                    "observed_processes": sorted(
                        processes.values(), key=lambda row: (int(row["pid"]), int(row["start_ticks"]))
                    ),
                    "samples": samples,
                },
                "sampler_cpu_user_ms": round((usage_end.ru_utime - usage_start.ru_utime) * 1000.0, 3),
                "sampler_cpu_sys_ms": round((usage_end.ru_stime - usage_start.ru_stime) * 1000.0, 3),
            }
            sampler_cpu_ms = result["sampler_cpu_user_ms"] + result["sampler_cpu_sys_ms"]
            result["sampler_cpu_percent_of_wall"] = round(
                sampler_cpu_ms * 100.0 / result["wall_ms"] if result["wall_ms"] > 0 else 0.0,
                3,
            )
            parent.verify_path_identity()
            remove_bound_leaf(child, parent)
            child.close()
            child = None
            return result
    finally:
        if handshake_read is not None:
            with contextlib.suppress(OSError):
                os.close(handshake_read)
        if child is not None:
            force_cleanup(child, parent, None if root_reaped or not root_in_measurement else root_pid)
            child.close()
        if staging is not None:
            force_cleanup(staging, parent, None if root_reaped or root_in_measurement else root_pid)
            staging.close()
        parent.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cwd", default=os.getcwd())
    parser.add_argument("--temporary-directory", help="private engine-owned temporary directory")
    parser.add_argument("--stdin", help="canonical root-owned, read-only API 2 request file")
    parser.add_argument("--stdout", help="bound command stdout capture file")
    parser.add_argument("--stderr", help="bound command stderr capture file")
    parser.add_argument("--interval-ms", type=float, default=75.0)
    parser.add_argument("--pss-interval-ms", type=float, default=250.0)
    parser.add_argument("--descendant-grace-ms", type=float, default=1000.0)
    parser.add_argument("--settle-timeout-ms", type=float, default=10000.0)
    parser.add_argument("--isolate-network", action="store_true")
    parser.add_argument("--probe-network-isolation", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    require_linux(parser, sys.platform)
    if args.probe_network_isolation:
        try:
            print(json.dumps(probe_network_isolation(), separators=(",", ":")))
            return 0
        except (MeasurementIncomplete, OSError) as error:
            code = error.code if isinstance(error, MeasurementIncomplete) else "NETWORK_ISOLATION_UNAVAILABLE"
            print(f"process_tree_sampler: measurement-incomplete[{code}]: {error}", file=sys.stderr)
            return 2
    if not command:
        parser.error("a command is required after --")
    if (
        args.interval_ms <= 0
        or args.pss_interval_ms < args.interval_ms
        or args.descendant_grace_ms < 0
        or args.settle_timeout_ms < args.interval_ms
    ):
        parser.error("intervals must be positive and PSS/settle intervals must cover the sample interval")
    configured_parent = os.environ.get("PLIEGO_BENCHMARK_CGROUP_PARENT")
    if not configured_parent:
        error = incomplete("CGROUP_PARENT_REQUIRED", "PLIEGO_BENCHMARK_CGROUP_PARENT is required")
        print(f"process_tree_sampler: measurement-incomplete[{error.code}]: {error}", file=sys.stderr)
        return 2
    if args.stdout is None or args.stderr is None:
        error = incomplete(
            "ENGINE_OUTPUT_CAPTURE_REQUIRED",
            "both --stdout and --stderr bound capture paths are required",
        )
        print(f"process_tree_sampler: measurement-incomplete[{error.code}]: {error}", file=sys.stderr)
        return 2
    try:
        result = sample_command(
            command,
            args.cwd,
            args.stdout,
            args.stderr,
            args.interval_ms,
            args.pss_interval_ms,
            args.descendant_grace_ms,
            args.settle_timeout_ms,
            Path(configured_parent),
            isolate_network=args.isolate_network,
            stdin_path=args.stdin,
            temporary_directory=args.temporary_directory,
        )
    except (MeasurementIncomplete, OSError) as error:
        code = error.code if isinstance(error, MeasurementIncomplete) else "CGROUP_IO_ERROR"
        print(f"process_tree_sampler: measurement-incomplete[{code}]: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
