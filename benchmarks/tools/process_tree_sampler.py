#!/usr/bin/python3 -I

"""Run the fixed render command as an unprivileged account in a root-owned cgroup."""

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
import stat
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterator

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


def reject_json_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON constant {value!r} is forbidden")


HARNESS_CHILD = "harness"
ENGINE_ACCOUNT = "pliego-benchmark-engine"
INSTALLED_BROKER = Path("/usr/local/libexec/pliego-cgroup-broker")
KILL_GRACE_MS = 1000.0
DEFAULT_DEADLINE_MS = 120_000.0
MAX_DEADLINE_MS = 600_000.0
MAX_ENGINE_EXECUTABLE_BYTES = 512 * 1024 * 1024
MAX_ENGINE_OUTPUT_BYTES = 64 * 1024 * 1024
MAX_ENGINE_PIDS = 128
PR_CAPBSET_DROP = 24
PR_SET_PDEATHSIG = 1
PR_GET_PDEATHSIG = 2
PR_SET_DUMPABLE = 4
PR_SET_NO_NEW_PRIVS = 38
PR_CAP_AMBIENT = 47
PR_CAP_AMBIENT_CLEAR_ALL = 4
CAP_SETPCAP = 8
CLONE_NEWNET = 0x40000000
AT_EMPTY_PATH = 0x1000
F_ADD_SEALS = 1033
F_SEAL_SEAL = 0x0001
F_SEAL_SHRINK = 0x0002
F_SEAL_GROW = 0x0004
F_SEAL_WRITE = 0x0008
METHOD = "linux-cgroup-v2-v2"


class MeasurementIncomplete(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def incomplete(code: str, message: str) -> MeasurementIncomplete:
    return MeasurementIncomplete(code, message)


_BROKER_SIGNAL: int | None = None


@contextlib.contextmanager
def broker_signal_guard(deadline_seconds: float | None = None) -> Iterator[None]:
    global _BROKER_SIGNAL
    _BROKER_SIGNAL = None
    previous: dict[int, Any] = {}

    def interrupted(signum: int, _frame: Any) -> None:
        global _BROKER_SIGNAL
        _BROKER_SIGNAL = signum
        if signum == signal.SIGALRM:
            raise incomplete("ENGINE_TIMEOUT", "broker deadline expired")
        raise incomplete("BROKER_INTERRUPTED", f"broker received signal {signum}")

    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP, signal.SIGALRM):
        previous[signum] = signal.signal(signum, interrupted)
    previous_timer = signal.getitimer(signal.ITIMER_REAL)
    if deadline_seconds is not None:
        signal.setitimer(signal.ITIMER_REAL, max(deadline_seconds, 0.001))
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0.0)
        for signum, handler in previous.items():
            signal.signal(signum, handler)
        if previous_timer != (0.0, 0.0):
            signal.setitimer(signal.ITIMER_REAL, *previous_timer)
        _BROKER_SIGNAL = None


def require_before_deadline(deadline: float, stage: str) -> None:
    if _BROKER_SIGNAL is not None:
        raise incomplete("BROKER_INTERRUPTED", f"broker received signal {_BROKER_SIGNAL} during {stage}")
    if time.monotonic() >= deadline:
        raise incomplete("ENGINE_TIMEOUT", f"broker deadline expired during {stage}")


def require_linux(parser: argparse.ArgumentParser, platform: str) -> None:
    if platform != "linux":
        parser.error("cgroup-v2 accounting requires Linux")


@dataclass(frozen=True)
class EngineAccount:
    name: str
    uid: int
    gid: int
    home: str


@dataclass
class ExecutableIdentity:
    path: Path
    sha256: str
    device: int
    inode: int
    size: int
    source_fd: int
    sealed_fd: int

    def close(self) -> None:
        for field in ("source_fd", "sealed_fd"):
            descriptor = getattr(self, field)
            if descriptor >= 0:
                os.close(descriptor)
                setattr(self, field, -1)


@dataclass
class BoundRunPaths:
    cwd_path: Path
    cwd_fd: int
    cwd_device: int
    cwd_inode: int
    input_relative: str
    input_fd: int
    input_device: int
    input_inode: int
    input_sha256: str
    workspace_path: Path
    workspace_fd: int
    workspace_device: int
    workspace_inode: int
    output_path: Path
    output_dir_fd: int
    output_dir_device: int
    output_dir_inode: int
    artifacts_path: Path
    artifacts_fd: int
    artifacts_device: int
    artifacts_inode: int

    def close(self) -> None:
        for field in ("cwd_fd", "input_fd", "workspace_fd", "output_dir_fd", "artifacts_fd"):
            descriptor = getattr(self, field)
            if descriptor >= 0:
                os.close(descriptor)
                setattr(self, field, -1)


def require_broker_root(uid: int | None = None, euid: int | None = None) -> None:
    real = os.getuid() if uid is None else uid
    effective = os.geteuid() if euid is None else euid
    if real != 0 or effective != 0:
        raise incomplete("BROKER_ROOT_REQUIRED", "cgroup measurement broker must run with real and effective UID 0")


def require_broker_installation(path: Path | None = None) -> None:
    requested = Path(__file__) if path is None else path
    try:
        resolved = requested.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        raise incomplete("BROKER_INSTALLATION_UNSAFE", f"cannot bind installed broker: {error}") from error
    if resolved != INSTALLED_BROKER or not stat.S_ISREG(metadata.st_mode):
        raise incomplete("BROKER_INSTALLATION_UNSAFE", f"broker must execute from {INSTALLED_BROKER}")
    if metadata.st_uid != 0 or metadata.st_gid != 0 or stat.S_IMODE(metadata.st_mode) & 0o022:
        raise incomplete("BROKER_INSTALLATION_UNSAFE", "installed broker must be root-owned and immutable")


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
    shell = str(getattr(entry, "pw_shell", "/usr/sbin/nologin"))
    if Path(home).exists() or Path(shell).name not in {"nologin", "false"}:
        raise incomplete("ENGINE_ACCOUNT_UNSAFE", f"{ENGINE_ACCOUNT!r} requires no home and a nologin shell")
    return EngineAccount(ENGINE_ACCOUNT, uid, gid, home)


def require_account_idle(account: EngineAccount, proc_root: Path = PROC) -> None:
    active: list[int] = []
    try:
        entries = list(proc_root.iterdir())
    except OSError as error:
        raise incomplete("ENGINE_ACCOUNT_UNAVAILABLE", f"cannot scan {proc_root}: {error}") from error
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            for line in (entry / "status").read_text(encoding="ascii").splitlines():
                if line.startswith("Uid:") and int(line.split()[1]) == account.uid:
                    active.append(int(entry.name))
                    break
        except (OSError, UnicodeError, ValueError, IndexError):
            continue
    if active:
        raise incomplete("ENGINE_ACCOUNT_BUSY", f"locked engine account already owns processes: {sorted(active)}")


def runner_identity(environment: dict[str, str] | None = None) -> tuple[int, int]:
    values = os.environ if environment is None else environment
    try:
        uid = int(values["SUDO_UID"])
        gid = int(values["SUDO_GID"])
    except (KeyError, ValueError) as error:
        raise incomplete("BROKER_INVOCATION_UNSAFE", "broker must be entered by non-interactive sudo") from error
    if uid <= 0 or gid <= 0:
        raise incomplete("BROKER_INVOCATION_UNSAFE", "broker invoker must be non-root")
    return uid, gid


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


def hash_fd(descriptor: int) -> str:
    digest = hashlib.sha256()
    offset = 0
    while True:
        chunk = os.pread(descriptor, 1024 * 1024, offset)
        if not chunk:
            break
        digest.update(chunk)
        offset += len(chunk)
    return digest.hexdigest()


def sealed_executable_copy(source_fd: int, size: int) -> int:
    if not hasattr(os, "memfd_create") or fcntl is None:
        raise incomplete("SEALED_EXECUTABLE_UNAVAILABLE", "memfd sealing is required for bound executable bytes")
    flags = getattr(os, "MFD_CLOEXEC", 0x0001) | getattr(os, "MFD_ALLOW_SEALING", 0x0002)
    descriptor = os.memfd_create("pliego-benchmark-engine", flags)
    try:
        offset = 0
        while offset < size:
            chunk = os.pread(source_fd, min(1024 * 1024, size - offset), offset)
            if not chunk:
                raise incomplete("ENGINE_EXECUTABLE_CHANGED", "executable shortened while binding bytes")
            written = os.write(descriptor, chunk)
            if written != len(chunk):
                raise incomplete("ENGINE_EXECUTABLE_CHANGED", "short write while sealing executable bytes")
            offset += written
        os.fchmod(descriptor, 0o555)
        fcntl.fcntl(
            descriptor,
            F_ADD_SEALS,
            F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL,
        )
        os.lseek(descriptor, 0, os.SEEK_SET)
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def command_identity(command: list[str], account: EngineAccount) -> tuple[tuple[str, ...], ExecutableIdentity]:
    if not command or any(not isinstance(arg, str) or not arg or "\0" in arg for arg in command):
        raise incomplete("ENGINE_COMMAND_INVALID", "engine argv must contain nonempty NUL-free strings")
    if len(command) < 3 or command[1] != "render":
        raise incomplete("ENGINE_COMMAND_INVALID", "sampler accepts only the fixed engine 'render <input>' argv")
    requested = Path(command[0])
    if not requested.is_absolute():
        raise incomplete("ENGINE_COMMAND_INVALID", "engine executable path must be absolute")
    try:
        executable = requested.resolve(strict=True)
    except OSError as error:
        raise incomplete("ENGINE_EXECUTABLE_UNAVAILABLE", f"cannot resolve engine executable: {error}") from error
    if requested != executable:
        raise incomplete("ENGINE_EXECUTABLE_NOT_CANONICAL", f"engine executable must be canonical, got {requested}")
    flags = os.O_RDONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0) | os.O_NONBLOCK
    try:
        descriptor = os.open(executable, flags)
        metadata = os.fstat(descriptor)
    except OSError as error:
        raise incomplete("ENGINE_EXECUTABLE_UNAVAILABLE", f"cannot stat {executable}: {error}") from error
    if not stat.S_ISREG(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) & 0o111 == 0:
        os.close(descriptor)
        raise incomplete("ENGINE_EXECUTABLE_UNSAFE", f"engine executable is not a regular executable: {executable}")
    if metadata.st_size <= 0 or metadata.st_size > MAX_ENGINE_EXECUTABLE_BYTES:
        os.close(descriptor)
        raise incomplete(
            "ENGINE_EXECUTABLE_SIZE_UNSAFE",
            f"engine executable must be in (0, {MAX_ENGINE_EXECUTABLE_BYTES}] bytes",
        )
    if identity_can_write(metadata, account.uid, account.gid):
        os.close(descriptor)
        raise incomplete("ENGINE_EXECUTABLE_MUTABLE", f"engine account can write {executable}")

    child_metadata = metadata
    for parent_path in executable.parents:
        parent_metadata = os.stat(parent_path, follow_symlinks=False)
        if child_replaceable(parent_metadata, child_metadata, account.uid, account.gid):
            os.close(descriptor)
            raise incomplete(
                "ENGINE_EXECUTABLE_MUTABLE",
                f"engine account can replace path component below {parent_path}",
            )
        child_metadata = parent_metadata

    digest = hash_fd(descriptor)
    sealed = sealed_executable_copy(descriptor, metadata.st_size)
    if hash_fd(sealed) != digest:
        os.close(descriptor)
        os.close(sealed)
        raise incomplete("ENGINE_EXECUTABLE_CHANGED", "sealed executable bytes do not match the source digest")
    return tuple(command), ExecutableIdentity(
        executable,
        digest,
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        descriptor,
        sealed,
    )


def verify_executable(identity: ExecutableIdentity) -> None:
    try:
        metadata = os.stat(identity.path, follow_symlinks=False)
    except OSError as error:
        raise incomplete("ENGINE_EXECUTABLE_REPLACED", f"cannot recheck {identity.path}: {error}") from error
    if (metadata.st_dev, metadata.st_ino) != (identity.device, identity.inode):
        raise incomplete("ENGINE_EXECUTABLE_REPLACED", f"engine executable changed: {identity.path}")
    bound = os.fstat(identity.source_fd)
    if (bound.st_dev, bound.st_ino, bound.st_size) != (identity.device, identity.inode, identity.size):
        raise incomplete("ENGINE_EXECUTABLE_CHANGED", "bound executable identity changed after hashing")
    if hash_fd(identity.source_fd) != identity.sha256:
        raise incomplete("ENGINE_EXECUTABLE_CHANGED", "bound executable bytes changed after hashing")


def _open_directory(path: Path, label: str) -> tuple[int, os.stat_result]:
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
        metadata = os.fstat(descriptor)
    except OSError as error:
        raise incomplete("COMMAND_PATH_UNSAFE", f"cannot bind {label} {path}: {error}") from error
    return descriptor, metadata


def _require_frozen_input(metadata: os.stat_result, label: str) -> None:
    if metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) & 0o022:
        raise incomplete(
            "COMMAND_INPUT_MUTABLE",
            f"{label} must be root-owned and not group/other writable on a publishable host",
        )


def _require_output_directory(metadata: os.stat_result, label: str, runner_uid: int, engine_gid: int) -> None:
    mode = stat.S_IMODE(metadata.st_mode)
    if metadata.st_uid != runner_uid or metadata.st_gid != engine_gid:
        raise incomplete(
            "COMMAND_OUTPUT_UNSAFE",
            f"{label} must be owned by runner UID {runner_uid} and engine GID {engine_gid}",
        )
    if mode & 0o007 or mode & 0o030 != 0o030:
        raise incomplete(
            "COMMAND_OUTPUT_UNSAFE",
            f"{label} must grant engine-group write/execute and no other access",
        )


def _command_option(argv: tuple[str, ...], name: str) -> tuple[int, str]:
    if argv.count(name) != 1:
        raise incomplete("ENGINE_COMMAND_INVALID", f"engine argv must contain exactly one {name}")
    index = argv.index(name)
    if index + 1 >= len(argv):
        raise incomplete("ENGINE_COMMAND_INVALID", f"engine argv omitted the {name} value")
    return index + 1, argv[index + 1]


def bind_run_paths(
    argv: tuple[str, ...],
    cwd: str,
    account: EngineAccount,
    runner_uid: int,
) -> tuple[tuple[str, ...], BoundRunPaths]:
    requested_cwd = Path(cwd)
    if not requested_cwd.is_absolute():
        raise incomplete("ENGINE_COMMAND_INVALID", "engine cwd must be absolute")
    try:
        canonical_cwd = requested_cwd.resolve(strict=True)
    except OSError as error:
        raise incomplete("COMMAND_PATH_UNSAFE", f"cannot resolve engine cwd: {error}") from error
    if canonical_cwd != requested_cwd:
        raise incomplete("COMMAND_PATH_UNSAFE", f"engine cwd must be canonical, got {requested_cwd}")
    cwd_fd, cwd_metadata = _open_directory(canonical_cwd, "cwd")
    descriptors = [cwd_fd]
    try:
        _require_frozen_input(cwd_metadata, "cwd")
        input_relative = argv[2]
        if Path(input_relative).name != input_relative or input_relative in {".", ".."}:
            raise incomplete("ENGINE_COMMAND_INVALID", "input argv must be one relative file name")
        input_flags = os.O_RDONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0) | os.O_NONBLOCK
        input_fd = os.open(input_relative, input_flags, dir_fd=cwd_fd)
        descriptors.append(input_fd)
        input_metadata = os.fstat(input_fd)
        if not stat.S_ISREG(input_metadata.st_mode):
            raise incomplete("COMMAND_PATH_UNSAFE", "bound input is not a regular file")
        _require_frozen_input(input_metadata, "input")

        output_index, output_raw = _command_option(argv, "--output")
        artifacts_index, artifacts_raw = _command_option(argv, "--artifacts")
        output_path = Path(output_raw)
        artifacts_path = Path(artifacts_raw)
        if not output_path.is_absolute() or not artifacts_path.is_absolute():
            raise incomplete("ENGINE_COMMAND_INVALID", "output and artifact paths must be absolute")
        output_parent = output_path.parent.resolve(strict=True)
        artifacts_canonical = artifacts_path.resolve(strict=True)
        if output_parent != output_path.parent or artifacts_canonical != artifacts_path:
            raise incomplete("COMMAND_PATH_UNSAFE", "output and artifact directories must be canonical")
        workspace = output_parent.parent
        if artifacts_canonical.parent != workspace or workspace.resolve(strict=True) != workspace:
            raise incomplete("COMMAND_PATH_UNSAFE", "output and artifact directories must be siblings in one workspace")
        workspace_fd, workspace_metadata = _open_directory(workspace, "workspace directory")
        descriptors.append(workspace_fd)
        output_fd, output_metadata = _open_directory(output_parent, "output directory")
        descriptors.append(output_fd)
        artifacts_fd, artifacts_metadata = _open_directory(artifacts_canonical, "artifact directory")
        descriptors.append(artifacts_fd)
        bound_identities = {
            (metadata.st_dev, metadata.st_ino)
            for metadata in (
                cwd_metadata,
                input_metadata,
                workspace_metadata,
                output_metadata,
                artifacts_metadata,
            )
        }
        if len(bound_identities) != 5:
            raise incomplete(
                "COMMAND_PATH_UNSAFE", "cwd, input, workspace, output, and artifact identities must differ"
            )
        _require_output_directory(output_metadata, "output directory", runner_uid, account.gid)
        _require_output_directory(artifacts_metadata, "artifact directory", runner_uid, account.gid)
        if output_path.exists():
            raise incomplete("COMMAND_OUTPUT_UNSAFE", "requested output must not exist before launch")

        adjusted = list(argv)
        adjusted[output_index] = f"/proc/self/fd/{output_fd}/{output_path.name}"
        adjusted[artifacts_index] = f"/proc/self/fd/{artifacts_fd}"
        paths = BoundRunPaths(
            canonical_cwd,
            cwd_fd,
            cwd_metadata.st_dev,
            cwd_metadata.st_ino,
            input_relative,
            input_fd,
            input_metadata.st_dev,
            input_metadata.st_ino,
            hash_fd(input_fd),
            workspace,
            workspace_fd,
            workspace_metadata.st_dev,
            workspace_metadata.st_ino,
            output_path,
            output_fd,
            output_metadata.st_dev,
            output_metadata.st_ino,
            artifacts_canonical,
            artifacts_fd,
            artifacts_metadata.st_dev,
            artifacts_metadata.st_ino,
        )
        descriptors.clear()
        return tuple(adjusted), paths
    except BaseException:
        for descriptor in descriptors:
            with contextlib.suppress(OSError):
                os.close(descriptor)
        raise


def verify_run_paths(paths: BoundRunPaths) -> None:
    identities = (
        (paths.cwd_fd, paths.cwd_device, paths.cwd_inode, "cwd"),
        (paths.input_fd, paths.input_device, paths.input_inode, "input"),
        (paths.workspace_fd, paths.workspace_device, paths.workspace_inode, "workspace directory"),
        (paths.output_dir_fd, paths.output_dir_device, paths.output_dir_inode, "output directory"),
        (paths.artifacts_fd, paths.artifacts_device, paths.artifacts_inode, "artifact directory"),
    )
    for descriptor, device, inode, label in identities:
        metadata = os.fstat(descriptor)
        if (metadata.st_dev, metadata.st_ino) != (device, inode):
            raise incomplete("COMMAND_IDENTITY_CHANGED", f"bound {label} identity changed")
    if hash_fd(paths.input_fd) != paths.input_sha256:
        raise incomplete("COMMAND_IDENTITY_CHANGED", "bound input bytes changed before exec")
    try:
        current_input = os.stat(paths.input_relative, dir_fd=paths.cwd_fd, follow_symlinks=False)
    except OSError as error:
        raise incomplete("COMMAND_IDENTITY_CHANGED", f"input path changed before exec: {error}") from error
    if (current_input.st_dev, current_input.st_ino) != (paths.input_device, paths.input_inode):
        raise incomplete("COMMAND_IDENTITY_CHANGED", "input path was replaced after binding")


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
        if not fields or fields[0] in devices:
            raise incomplete("CGROUP_COUNTER_INVALID", f"malformed io.stat line in {label}: {line!r}")
        major, separator, minor = fields[0].partition(":")
        if not separator or not major.isdigit() or not minor.isdigit():
            raise incomplete("CGROUP_COUNTER_INVALID", f"malformed io.stat device in {label}: {fields[0]!r}")
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
        required_counters = ("rbytes", "wbytes", "rios", "wios")
        if len(fields) == 1:
            counters = {required: 0 for required in required_counters}
        else:
            for required in required_counters:
                if required not in counters:
                    raise incomplete("CGROUP_COUNTER_MISSING", f"io.stat device {fields[0]} omitted {required}")
        devices[fields[0]] = counters
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
        require_bound_security(
            bound,
            frozenset(
                {"cgroup.controllers", "cgroup.procs", "cgroup.subtree_control", "cgroup.threads", "cgroup.type"}
            ),
            owner_uid,
            owner_gid,
        )
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
        if read_bound(bound, "cgroup.type").strip() != "domain":
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
            "pids.max",
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
        write_bound(child, "pids.max", f"{MAX_ENGINE_PIDS}\n")
        if read_bound(child, "pids.max").strip() != str(MAX_ENGINE_PIDS):
            raise incomplete("CGROUP_LIMIT_FAILED", "cannot bind the engine process-count limit")
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
    members_before = cgroup_pids(cgroup)
    for pid in members_before:
        before = read_stat(pid, proc_root)
        if before is None:
            continue
        if pid == root_identity[0] and before["start_ticks"] != root_identity[1]:
            raise incomplete("ROOT_IDENTITY_CHANGED", f"root PID {pid} start time changed")
        try:
            pidfd = os.pidfd_open(pid)
        except (AttributeError, OSError):
            continue
        try:
            pss = read_pss_kib(pid, proc_root) if include_pss else None
            after = read_stat(pid, proc_root)
            members_after = cgroup_pids(cgroup)
        finally:
            os.close(pidfd)
        if after is None or pid not in members_after or before["start_ticks"] != after["start_ticks"]:
            continue
        after["pss_kib"] = pss
        after["membership_verified"] = 1
        found.append(after)
    return found


def prctl(option: int, arg2: int = 0) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(option, arg2, 0, 0, 0) != 0:
        value = ctypes.get_errno()
        raise OSError(value, os.strerror(value))


def parent_death_signal() -> int:
    libc = ctypes.CDLL(None, use_errno=True)
    retained = ctypes.c_int()
    if libc.prctl(PR_GET_PDEATHSIG, ctypes.byref(retained), 0, 0, 0) != 0:
        value = ctypes.get_errno()
        raise OSError(value, os.strerror(value))
    return retained.value


def unshare_network() -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.unshare(CLONE_NEWNET) != 0:
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
    prctl(PR_SET_DUMPABLE, 0)
    prctl(PR_SET_PDEATHSIG, signal.SIGKILL)


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


def migration_fd_probes(directories: dict[str, int]) -> dict[str, dict[str, str]]:
    results: dict[str, dict[str, str]] = {}
    flags = os.O_WRONLY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
    for label, directory_fd in directories.items():
        results[label] = {}
        for interface in ("cgroup.procs", "cgroup.threads"):
            try:
                descriptor = os.open(interface, flags, dir_fd=directory_fd)
            except OSError as error:
                results[label][interface] = errno.errorcode.get(error.errno, f"ERRNO_{error.errno}")
            else:
                os.close(descriptor)
                results[label][interface] = "WRITABLE"
    return results


def engine_environment(account: EngineAccount) -> dict[str, str]:
    return {
        "HOME": account.home,
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "LOGNAME": account.name,
        "PATH": "/usr/bin:/bin",
        "TMPDIR": "/tmp",
        "TZ": "UTC",
        "USER": account.name,
    }


def execveat(descriptor: int, argv: tuple[str, ...], environment: dict[str, str]) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    encoded_argv = [value.encode() for value in argv]
    encoded_environment = [f"{name}={value}".encode() for name, value in sorted(environment.items())]
    argv_array = (ctypes.c_char_p * (len(encoded_argv) + 1))(*encoded_argv, None)
    environment_array = (ctypes.c_char_p * (len(encoded_environment) + 1))(*encoded_environment, None)
    if libc.execveat(descriptor, ctypes.c_char_p(b""), argv_array, environment_array, AT_EMPTY_PATH) != 0:
        value = ctypes.get_errno()
        raise OSError(value, os.strerror(value))


def close_descriptors_except(allowed: set[int]) -> None:
    entries = list((PROC / "self" / "fd").iterdir())
    for entry in entries:
        try:
            descriptor = int(entry.name)
        except ValueError:
            continue
        if descriptor > 2 and descriptor not in allowed:
            with contextlib.suppress(OSError):
                os.close(descriptor)


def fork_stopped(
    command: tuple[str, ...],
    executable: ExecutableIdentity,
    paths: BoundRunPaths,
    account: EngineAccount,
    probe_paths: dict[str, Path],
    probe_directories: dict[str, int],
    broker_net_inode: int,
) -> tuple[int, int, int, int]:
    if not hasattr(os, "memfd_create"):
        raise incomplete("OUTPUT_CAPTURE_UNAVAILABLE", "anonymous bounded output capture requires memfd_create")
    stdout_capture = os.memfd_create("pliego-engine-stdout", getattr(os, "MFD_CLOEXEC", 0x0001))
    stderr_capture = os.memfd_create("pliego-engine-stderr", getattr(os, "MFD_CLOEXEC", 0x0001))
    descriptors = [
        os.open(os.devnull, os.O_RDONLY | os.O_CLOEXEC),
        stdout_capture,
        stderr_capture,
    ]
    handshake_read, handshake_write = os.pipe2(os.O_CLOEXEC)
    try:
        pid = os.fork()
        if pid == 0:
            try:
                os.close(handshake_read)
                broker_pid = os.getppid()
                os.setsid()
                prctl(PR_SET_PDEATHSIG, signal.SIGKILL)
                if os.getppid() != broker_pid:
                    raise OSError(errno.EOWNERDEAD, "broker died before launcher setup")
                unshare_network()
                os.fchdir(paths.cwd_fd)
                for target, descriptor in enumerate(descriptors):
                    os.dup2(descriptor, target)
                resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_ENGINE_OUTPUT_BYTES, MAX_ENGINE_OUTPUT_BYTES))
                os.kill(os.getpid(), signal.SIGSTOP)
                drop_engine_authority(account)
                if os.getppid() != broker_pid or parent_death_signal() != signal.SIGKILL:
                    raise OSError(errno.EOWNERDEAD, "broker parent/death signal changed during authority drop")
                payload = {
                    "ok": True,
                    "execution_binding": "sealed-memfd-execveat",
                    "parent_death_signal": parent_death_signal(),
                    "cwd_accessible": os.access(".", os.R_OK | os.X_OK),
                    "network_namespace": os.stat(PROC / "self/ns/net").st_ino != broker_net_inode,
                    "migration_write_probes": migration_write_probes(probe_paths),
                    "migration_fd_probes": migration_fd_probes(probe_directories),
                }
                os.write(
                    handshake_write,
                    json.dumps(payload, separators=(",", ":"), allow_nan=False).encode("ascii"),
                )
                os.kill(os.getpid(), signal.SIGSTOP)
                allowed = {executable.sealed_fd, paths.output_dir_fd, paths.artifacts_fd}
                for descriptor in allowed:
                    os.set_inheritable(descriptor, True)
                close_descriptors_except(allowed)
                execveat(executable.sealed_fd, command, engine_environment(account))
            except BaseException as error:
                with contextlib.suppress(BaseException):
                    payload = {"ok": False, "error": f"{type(error).__name__}: {error}"}
                    os.write(
                        handshake_write,
                        json.dumps(payload, separators=(",", ":"), allow_nan=False).encode("utf-8", "replace"),
                    )
                    os.write(2, f"cgroup launcher: {error}\n".encode("utf-8", "replace"))
                os._exit(127)
    finally:
        os.close(handshake_write)
        for descriptor in descriptors[:1]:
            with contextlib.suppress(OSError):
                os.close(descriptor)

    waited, status_value = os.waitpid(pid, os.WUNTRACED)
    if waited != pid or not os.WIFSTOPPED(status_value) or os.WSTOPSIG(status_value) != signal.SIGSTOP:
        os.close(handshake_read)
        raise incomplete("LAUNCH_HANDSHAKE_FAILED", f"child {pid} did not stop before exec")
    return pid, handshake_read, stdout_capture, stderr_capture


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
        handshake = json.loads(raw, parse_constant=reject_json_constant)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as error:
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
    if handshake.get("cwd_accessible") is not True:
        raise incomplete("ENGINE_PATH_INACCESSIBLE", "engine account cannot access its bound cwd")
    if handshake.get("execution_binding") != "sealed-memfd-execveat":
        raise incomplete("ENGINE_EXECUTABLE_UNBOUND", "launcher did not bind execution to sealed bytes")
    if handshake.get("parent_death_signal") != signal.SIGKILL:
        raise incomplete("ENGINE_LIFECYCLE_UNSAFE", "launcher did not retain SIGKILL on broker death")
    if handshake.get("network_namespace") is not True:
        raise incomplete("ENGINE_NETWORK_UNSAFE", "launcher did not enter a private network namespace")
    for field in ("migration_write_probes", "migration_fd_probes"):
        probes = handshake.get(field)
        if not isinstance(probes, dict) or set(probes) != {"parent", "harness", "staging", "measurement"}:
            raise incomplete("ENGINE_MIGRATION_PROBE_INVALID", f"launcher omitted {field}")
        for results in probes.values():
            if (
                not isinstance(results, dict)
                or set(results) != {"cgroup.procs", "cgroup.threads"}
                or any(result not in {"EACCES", "EPERM"} for result in results.values())
            ):
                raise incomplete("ENGINE_CGROUP_WRITABLE", f"engine account can access migration via {field}: {probes}")
    return {
        "account": account.name,
        "uid": account.uid,
        "gid": account.gid,
        "status": security,
        "execution_binding": handshake["execution_binding"],
        "parent_death_signal": handshake["parent_death_signal"],
        "network_namespace_isolated": handshake.get("network_namespace") is True,
        "migration_write_probes": handshake["migration_write_probes"],
        "migration_fd_probes": handshake["migration_fd_probes"],
    }


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
        if _BROKER_SIGNAL is not None:
            raise incomplete("BROKER_INTERRUPTED", "broker interrupted during accounting settle")
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


def flush_bound_output_filesystems(paths: BoundRunPaths) -> None:
    """Force candidate writes to accounting completion without reopening paths."""

    libc = ctypes.CDLL(None, use_errno=True)
    syncfs = getattr(libc, "syncfs", None)
    if syncfs is None:
        raise incomplete("CGROUP_ACCOUNTING_UNAVAILABLE", "syncfs is required for complete writeback accounting")
    syncfs.argtypes = [ctypes.c_int]
    syncfs.restype = ctypes.c_int
    devices: set[int] = set()
    for descriptor in (paths.output_dir_fd, paths.artifacts_fd):
        device = os.fstat(descriptor).st_dev
        if device in devices:
            continue
        devices.add(device)
        if syncfs(descriptor) != 0:
            error = ctypes.get_errno()
            raise incomplete("CGROUP_ACCOUNTING_IO_ERROR", f"syncfs failed: {os.strerror(error)}")


def read_capture(descriptor: int, label: str) -> str:
    payload = os.pread(descriptor, MAX_ENGINE_OUTPUT_BYTES + 1, 0)
    if len(payload) > MAX_ENGINE_OUTPUT_BYTES:
        raise incomplete("ENGINE_OUTPUT_LIMIT", f"{label} exceeded the broker capture limit")
    return payload.decode("utf-8", "replace")


def sample_command(
    command: list[str],
    cwd: str,
    interval_ms: float,
    pss_interval_ms: float,
    descendant_grace_ms: float,
    settle_timeout_ms: float,
    deadline_ms: float,
    observe_processes: bool,
    cgroup_parent: Path,
    cgroup_root: Path = CGROUP_ROOT,
    proc_root: Path = PROC,
    lock_root: Path | None = None,
) -> dict[str, Any]:
    if deadline_ms <= 0 or deadline_ms > MAX_DEADLINE_MS:
        raise incomplete("BROKER_DEADLINE_INVALID", f"deadline must be in (0, {MAX_DEADLINE_MS}] milliseconds")
    broker_deadline = time.monotonic() + deadline_ms / 1000.0
    with broker_signal_guard(deadline_ms / 1000.0):
        return _sample_command_guarded(
            command,
            cwd,
            interval_ms,
            pss_interval_ms,
            descendant_grace_ms,
            settle_timeout_ms,
            deadline_ms,
            observe_processes,
            cgroup_parent,
            cgroup_root,
            proc_root,
            lock_root,
            broker_deadline,
        )


def _sample_command_guarded(
    command: list[str],
    cwd: str,
    interval_ms: float,
    pss_interval_ms: float,
    descendant_grace_ms: float,
    settle_timeout_ms: float,
    deadline_ms: float,
    observe_processes: bool,
    cgroup_parent: Path,
    cgroup_root: Path,
    proc_root: Path,
    lock_root: Path | None,
    broker_deadline: float,
) -> dict[str, Any]:
    if resource is None:
        raise incomplete("CGROUP_V2_REQUIRED", "Linux resource accounting is unavailable")
    if not hasattr(os, "pidfd_open"):
        raise incomplete("PIDFD_REQUIRED", "os.pidfd_open is unavailable")

    require_broker_root()
    runner_uid, _runner_gid = runner_identity()
    account = resolve_engine_account()
    if runner_uid == account.uid:
        raise incomplete("BROKER_INVOCATION_UNSAFE", "runner and engine identities must be distinct")
    require_account_idle(account, proc_root)

    executable: ExecutableIdentity | None = None
    paths: BoundRunPaths | None = None
    parent: BoundDirectory | None = None
    harness: BoundDirectory | None = None
    child: BoundDirectory | None = None
    staging: BoundDirectory | None = None
    root_pid: int | None = None
    handshake_read: int | None = None
    stdout_capture: int | None = None
    stderr_capture: int | None = None
    root_in_measurement = False
    root_reaped = False
    usage_start = resource.getrusage(resource.RUSAGE_SELF)

    try:
        argv, executable = command_identity(command, account)
        exec_argv, paths = bind_run_paths(argv, cwd, account, runner_uid)
        parent = require_delegated_parent(cgroup_parent, cgroup_root, proc_root)
        harness = open_bound_directory(parent.path / HARNESS_CHILD, cgroup_root)
        with exclusive_parent_lock(parent.path, lock_root):
            parent.verify_path_identity()
            child = create_leaf(parent)
            staging = create_leaf(parent, "pliego-launch")
            empty = counter_snapshot(child)
            require_zero_snapshot(empty, "empty", populated=0, pids_peak=0)
            require_zero_snapshot(counter_snapshot(staging), "staging_empty", populated=0, pids_peak=0)

            probe_paths = {
                "parent": parent.path,
                "harness": harness.path,
                "staging": staging.path,
                "measurement": child.path,
            }
            probe_directories = {
                "parent": parent.fd,
                "harness": harness.fd,
                "staging": staging.fd,
                "measurement": child.fd,
            }
            broker_net_inode = os.stat(proc_root / "self/ns/net").st_ino
            root_pid, handshake_read, stdout_capture, stderr_capture = fork_stopped(
                exec_argv,
                executable,
                paths,
                account,
                probe_paths,
                probe_directories,
                broker_net_inode,
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
            launch_security.update(
                {
                    "argv": list(argv),
                    "executable": {
                        "path": str(executable.path),
                        "sha256": executable.sha256,
                        "device": executable.device,
                        "inode": executable.inode,
                        "bytes": executable.size,
                    },
                    "command_identity": {
                        "cwd": {
                            "path": str(paths.cwd_path),
                            "device": paths.cwd_device,
                            "inode": paths.cwd_inode,
                        },
                        "input": {
                            "manifest_path": str(paths.cwd_path / paths.input_relative),
                            "argv_relative_path": paths.input_relative,
                            "sha256": paths.input_sha256,
                            "device": paths.input_device,
                            "inode": paths.input_inode,
                        },
                        "workspace_directory": {
                            "path": str(paths.workspace_path),
                            "device": paths.workspace_device,
                            "inode": paths.workspace_inode,
                        },
                        "output_directory": {
                            "path": str(paths.output_path.parent),
                            "device": paths.output_dir_device,
                            "inode": paths.output_dir_inode,
                        },
                        "artifact_directory": {
                            "path": str(paths.artifacts_path),
                            "device": paths.artifacts_device,
                            "inode": paths.artifacts_inode,
                        },
                    },
                }
            )
            verify_executable(executable)
            verify_run_paths(paths)
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
            diagnostic_samples: list[dict[str, Any]] = []
            processes: dict[tuple[int, int], dict[str, int | float]] = {}
            next_pss = 0.0

            def take_sample(started: float, now: float) -> None:
                nonlocal next_pss
                if not observe_processes:
                    return
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
                diagnostic_samples.append(
                    {
                        "elapsed_ms": elapsed_ms,
                        "cgroup_memory_current_bytes": cgroup_integer(child, "memory.current"),
                        "sequential_sampled_summed_rss_kib_diagnostic": sum(
                            int(row["rss_pages"]) * page_size // 1024 for row in raw_processes
                        ),
                        "sequential_sampled_summed_pss_kib_diagnostic": (
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
            try:
                while status_value is None:
                    require_before_deadline(broker_deadline, "root process")
                    timeout = max(0.0, min(next_sample, broker_deadline) - time.monotonic())
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
                        while next_sample <= now:
                            next_sample += interval_ms / 1000.0
            finally:
                os.close(pidfd)

            kill_used = False
            lingering_before_kill: list[dict[str, int]] = []
            drain_deadline = min(broker_deadline, root_ended + descendant_grace_ms / 1000.0)
            while cgroup_flat(child, "cgroup.events", {"populated"})["populated"] != 0:
                require_before_deadline(broker_deadline, "descendant drain")
                now = time.monotonic()
                if now >= drain_deadline:
                    kill_used = True
                    lingering_before_kill = [
                        {"pid": int(row["pid"]), "start_ticks": int(row["start_ticks"])}
                        for row in scan_cgroup(child, False, root_identity, proc_root)
                    ]
                    write_bound(child, "cgroup.kill", "1\n")
                    kill_deadline = min(broker_deadline, now + KILL_GRACE_MS / 1000.0)
                    while cgroup_flat(child, "cgroup.events", {"populated"})["populated"] != 0:
                        require_before_deadline(kill_deadline, "cgroup.kill drain")
                        time.sleep(min(interval_ms, 25.0) / 1000.0)
                    break
                take_sample(started, now)
                time.sleep(min(interval_ms / 1000.0, max(0.0, drain_deadline - now)))

            drained_at = time.monotonic()
            take_sample(started, drained_at)
            require_before_deadline(broker_deadline, "output writeback")
            flush_bound_output_filesystems(paths)
            require_before_deadline(broker_deadline, "accounting settle")
            remaining_ms = max(0.0, (broker_deadline - time.monotonic()) * 1000.0)
            final, settle = wait_for_accounting_quiescence(
                child,
                interval_ms,
                min(settle_timeout_ms, remaining_ms),
            )
            measurement_completed = time.monotonic()
            require_before_deadline(broker_deadline, "measurement completion")
            if final["cgroup_events"]["populated"] != 0:
                raise incomplete("CGROUP_CLEANUP_FAILED", "render cgroup repopulated during accounting settle")

            exit_code, child_signal = exit_fields(status_value)
            usage_end = resource.getrusage(resource.RUSAGE_SELF)
            elapsed = [float(sample["elapsed_ms"]) for sample in diagnostic_samples]
            gaps = [later - earlier for earlier, later in zip(elapsed, elapsed[1:])]
            rss_peaks = [int(sample["sequential_sampled_summed_rss_kib_diagnostic"]) for sample in diagnostic_samples]
            pss_peaks = [
                int(sample["sequential_sampled_summed_pss_kib_diagnostic"])
                for sample in diagnostic_samples
                if sample["sequential_sampled_summed_pss_kib_diagnostic"] is not None
            ]
            root_wall_ms = round((root_ended - started) * 1000.0, 3)
            tree_wall_ms = round((drained_at - started) * 1000.0, 3)
            result = {
                "method": METHOD,
                "scope": "fresh-root-owned-cgroup-subtree",
                "delegated_parent": str(parent.path),
                "cgroup_path": str(child.path),
                "root_pid": root_identity[0],
                "root_start_ticks": root_identity[1],
                "launch_security": launch_security,
                "root_wall_ms": root_wall_ms,
                "tree_wall_ms": tree_wall_ms,
                "measurement_complete_ms": round((measurement_completed - started) * 1000.0, 3),
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
                "drain_ms": round(tree_wall_ms - root_wall_ms, 3),
                "accounting_settle": settle,
                "cleanup": {
                    "kill_used": kill_used,
                    "kill_grace_ms": KILL_GRACE_MS,
                    "lingering_before_kill": lingering_before_kill,
                },
                "counters": {"empty": empty, "after_join": after_join, "final": final},
                "sampled_diagnostics": {
                    "coverage": (
                        "sequential pidfd-identity and membership-verified procfs diagnostics; values are "
                        "time-smeared observations, not simultaneous bounds or authoritative accounting"
                    ),
                    "observation_enabled": observe_processes,
                    "sample_interval_ms": interval_ms,
                    "pss_interval_ms": pss_interval_ms,
                    "page_size_bytes": page_size,
                    "max_sample_gap_ms": round(max(gaps, default=0.0), 3),
                    "sequential_sampled_peak_summed_rss_kib_diagnostic": max(rss_peaks, default=0),
                    "sequential_sampled_peak_summed_pss_kib_diagnostic": max(pss_peaks) if pss_peaks else None,
                    "observed_processes": sorted(
                        processes.values(), key=lambda row: (int(row["pid"]), int(row["start_ticks"]))
                    ),
                    "samples": diagnostic_samples,
                },
                "engine_stdout": read_capture(stdout_capture, "engine stdout"),
                "engine_stderr": read_capture(stderr_capture, "engine stderr"),
                "sampler_cpu_user_ms": round((usage_end.ru_utime - usage_start.ru_utime) * 1000.0, 3),
                "sampler_cpu_sys_ms": round((usage_end.ru_stime - usage_start.ru_stime) * 1000.0, 3),
            }
            parent.verify_path_identity()
            remove_bound_leaf(child, parent)
            child.close()
            child = None
            return result
    finally:
        if handshake_read is not None:
            with contextlib.suppress(OSError):
                os.close(handshake_read)
        if child is not None and parent is not None:
            force_cleanup(child, parent, None if root_reaped or not root_in_measurement else root_pid)
            child.close()
        if staging is not None and parent is not None:
            force_cleanup(staging, parent, None if root_reaped or root_in_measurement else root_pid)
            staging.close()
        for descriptor in (stdout_capture, stderr_capture):
            if descriptor is not None:
                with contextlib.suppress(OSError):
                    os.close(descriptor)
        if harness is not None:
            harness.close()
        if parent is not None:
            parent.close()
        if paths is not None:
            paths.close()
        if executable is not None:
            executable.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cwd", default=os.getcwd())
    parser.add_argument("--cgroup-parent", type=Path, required=True)
    parser.add_argument("--interval-ms", type=float, default=75.0)
    parser.add_argument("--pss-interval-ms", type=float, default=250.0)
    parser.add_argument("--descendant-grace-ms", type=float, default=1000.0)
    parser.add_argument("--settle-timeout-ms", type=float, default=10000.0)
    parser.add_argument("--deadline-ms", type=float, default=DEFAULT_DEADLINE_MS)
    parser.add_argument("--observation", choices=("on", "off"), default="on")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    require_linux(parser, sys.platform)
    if not command:
        parser.error("a command is required after --")
    if (
        not all(
            math.isfinite(value)
            for value in (
                args.interval_ms,
                args.pss_interval_ms,
                args.descendant_grace_ms,
                args.settle_timeout_ms,
                args.deadline_ms,
            )
        )
        or args.interval_ms <= 0
        or args.pss_interval_ms < args.interval_ms
        or args.descendant_grace_ms < 0
        or args.settle_timeout_ms < args.interval_ms
        or args.deadline_ms <= 0
        or args.deadline_ms > MAX_DEADLINE_MS
    ):
        parser.error("intervals/deadline must be bounded and PSS/settle intervals must cover the sample interval")
    try:
        require_broker_installation()
        result = sample_command(
            command,
            args.cwd,
            args.interval_ms,
            args.pss_interval_ms,
            args.descendant_grace_ms,
            args.settle_timeout_ms,
            args.deadline_ms,
            args.observation == "on",
            args.cgroup_parent,
        )
    except (MeasurementIncomplete, OSError) as error:
        code = error.code if isinstance(error, MeasurementIncomplete) else "CGROUP_IO_ERROR"
        print(f"process_tree_sampler: measurement-incomplete[{code}]: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, separators=(",", ":"), allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
