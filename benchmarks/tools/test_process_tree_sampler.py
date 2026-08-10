#!/usr/bin/env python3

"""Fixture, live cgroup-v2, observer, and PHP bridge checks."""

from __future__ import annotations

import argparse
import contextlib
import ctypes
import errno
import hashlib
import io
import json
import os
import random
import signal
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterator
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import benchmark_publication
import observer_ab
import process_tree_sampler
import validate_result

SAMPLER = Path(__file__).with_name("process_tree_sampler.py")
PHP_RUNNER = Path(__file__).resolve().parents[1] / "runners" / "pliego.php"


def write_proc_stat(proc: Path, pid: int, start_ticks: int) -> None:
    directory = proc / str(pid)
    directory.mkdir(parents=True)
    fields = [
        "S",
        "1",
        str(pid),
        str(pid),
        "0",
        "0",
        "0",
        "0",
        "0",
        "0",
        "0",
        "1",
        "1",
        "0",
        "0",
        "20",
        "0",
        "1",
        "0",
        str(start_ticks),
        "4096",
        "1",
    ]
    (directory / "stat").write_text(f"{pid} (fixture) {' '.join(fields)}\n", encoding="ascii")


def delegated_fixture(directory: Path) -> tuple[Path, Path, Path]:
    root = directory / "cgroup"
    parent = root / "delegated"
    harness = parent / "harness"
    proc = directory / "proc"
    harness.mkdir(parents=True)
    (proc / "self").mkdir(parents=True)
    (root / "cgroup.controllers").write_text("cpu io memory pids\n", encoding="ascii")
    (parent / "cgroup.type").write_text("domain\n", encoding="ascii")
    (parent / "cgroup.controllers").write_text("cpu io memory pids\n", encoding="ascii")
    (parent / "cgroup.subtree_control").write_text("cpu io memory pids\n", encoding="ascii")
    (parent / "cgroup.procs").write_text("", encoding="ascii")
    (parent / "cgroup.threads").write_text("", encoding="ascii")
    (harness / "cgroup.procs").write_text(f"{os.getpid()}\n", encoding="ascii")
    (harness / "cgroup.threads").write_text(f"{os.getpid()}\n", encoding="ascii")
    (harness / "cgroup.type").write_text("domain\n", encoding="ascii")
    (proc / "self" / "cgroup").write_text("0::/delegated/harness\n", encoding="ascii")
    return root, parent, proc


def require_fixture_parent(parent: Path, root: Path, proc: Path) -> process_tree_sampler.BoundDirectory:
    return process_tree_sampler.require_delegated_parent(parent, root, proc, os.getuid(), os.getgid())


def counter_fixture(cgroup: Path) -> None:
    cgroup.mkdir(parents=True, exist_ok=True)
    (cgroup / "cpu.stat").write_text("usage_usec 0\nuser_usec 0\nsystem_usec 0\n", encoding="ascii")
    (cgroup / "io.stat").write_text("8:0 rbytes=0 wbytes=0 rios=0 wios=0\n", encoding="ascii")
    (cgroup / "memory.current").write_text("0\n", encoding="ascii")
    (cgroup / "memory.peak").write_text("0\n", encoding="ascii")
    (cgroup / "memory.stat").write_text("file_dirty 0\nfile_writeback 0\n", encoding="ascii")
    (cgroup / "pids.peak").write_text("0\n", encoding="ascii")
    (cgroup / "cgroup.events").write_text("populated 0\nfrozen 0\n", encoding="ascii")


def must_be_incomplete(code: str, operation: object) -> None:
    try:
        operation()  # type: ignore[operator]
    except process_tree_sampler.MeasurementIncomplete as error:
        assert error.code == code, error
    else:
        raise AssertionError(f"expected measurement-incomplete[{code}]")


def fixture_proofs() -> None:
    parser = argparse.ArgumentParser(prog="process_tree_sampler")
    error = io.StringIO()
    with contextlib.redirect_stderr(error):
        try:
            process_tree_sampler.require_linux(parser, "win32")
        except SystemExit as exit_error:
            assert exit_error.code == 2
        else:
            raise AssertionError("unsupported platform was accepted")
    assert "cgroup-v2 accounting requires Linux" in error.getvalue()

    must_be_incomplete("BROKER_ROOT_REQUIRED", lambda: process_tree_sampler.require_broker_root(1000, 1000))
    must_be_incomplete(
        "ENGINE_ACCOUNT_UNAVAILABLE",
        lambda: process_tree_sampler.resolve_engine_account(lambda _name: (_ for _ in ()).throw(KeyError())),
    )
    must_be_incomplete(
        "ENGINE_ACCOUNT_UNSAFE",
        lambda: process_tree_sampler.resolve_engine_account(
            lambda name: SimpleNamespace(pw_name=name, pw_uid=0, pw_gid=0, pw_dir="/root")
        ),
    )
    fixture_account = process_tree_sampler.resolve_engine_account(
        lambda name: SimpleNamespace(pw_name=name, pw_uid=1234, pw_gid=1234, pw_dir="/nonexistent")
    )
    assert fixture_account.name == process_tree_sampler.ENGINE_ACCOUNT
    with mock.patch.dict(os.environ, {"GITHUB_TOKEN": "must-not-leak", "LANG": "C"}, clear=True):
        child_environment = process_tree_sampler.engine_environment(fixture_account)
    assert "GITHUB_TOKEN" not in child_environment and child_environment["LANG"] == "C.UTF-8"
    assert child_environment == {
        "HOME": "/nonexistent",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "LOGNAME": process_tree_sampler.ENGINE_ACCOUNT,
        "PATH": "/usr/bin:/bin",
        "TMPDIR": "/tmp",
        "TZ": "UTC",
        "USER": process_tree_sampler.ENGINE_ACCOUNT,
    }
    must_be_incomplete("BROKER_INVOCATION_UNSAFE", lambda: process_tree_sampler.runner_identity({}))
    assert process_tree_sampler.runner_identity({"SUDO_UID": "1000", "SUDO_GID": "1000"}) == (1000, 1000)

    with tempfile.TemporaryDirectory() as raw:
        executable = Path(raw) / "true"
        executable.write_bytes(b"fixture")
        executable.chmod(0o555)
        must_be_incomplete(
            "ENGINE_COMMAND_INVALID",
            lambda: process_tree_sampler.command_identity([str(executable.resolve())], fixture_account),
        )

    if sys.platform == "linux":
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            directory.chmod(0o755)
            executable = directory / "engine"
            original = b"#!/bin/sh\nexit 0\n"
            executable.write_bytes(original)
            executable.chmod(0o555)
            _argv, identity = process_tree_sampler.command_identity(
                [str(executable.resolve()), "render", "input.html"], fixture_account
            )
            try:
                executable.chmod(0o755)
                executable.write_bytes(b"#!/bin/sh\nexit 99\n")
                assert process_tree_sampler.hash_fd(identity.sealed_fd) == hashlib.sha256(original).hexdigest()
                assert process_tree_sampler.hash_fd(identity.source_fd) != identity.sha256
                must_be_incomplete(
                    "ENGINE_EXECUTABLE_CHANGED", lambda: process_tree_sampler.verify_executable(identity)
                )
            finally:
                identity.close()

        with tempfile.TemporaryDirectory() as raw:
            oversized = Path(raw) / "oversized-engine"
            with oversized.open("wb") as stream:
                stream.truncate(process_tree_sampler.MAX_ENGINE_EXECUTABLE_BYTES + 1)
            oversized.chmod(0o555)
            must_be_incomplete(
                "ENGINE_EXECUTABLE_SIZE_UNSAFE",
                lambda: process_tree_sampler.command_identity(
                    [str(oversized.resolve()), "render", "input.html"],
                    fixture_account,
                ),
            )

        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            input_path = directory / "input.html"
            output = directory / "output"
            artifacts = directory / "artifacts"
            output.mkdir()
            artifacts.mkdir()
            input_path.write_text("old", encoding="utf-8")
            cwd_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            input_fd = os.open(input_path, os.O_RDONLY | os.O_CLOEXEC)
            output_fd = os.open(output, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            artifacts_fd = os.open(artifacts, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            cwd_stat, input_stat = os.fstat(cwd_fd), os.fstat(input_fd)
            output_stat, artifacts_stat = os.fstat(output_fd), os.fstat(artifacts_fd)
            paths = process_tree_sampler.BoundRunPaths(
                directory,
                cwd_fd,
                cwd_stat.st_dev,
                cwd_stat.st_ino,
                "input.html",
                input_fd,
                input_stat.st_dev,
                input_stat.st_ino,
                process_tree_sampler.hash_fd(input_fd),
                output / "document.pdf",
                output_fd,
                output_stat.st_dev,
                output_stat.st_ino,
                artifacts,
                artifacts_fd,
                artifacts_stat.st_dev,
                artifacts_stat.st_ino,
            )
            try:
                moved = directory.with_name(directory.name + "-moved")
                directory.rename(moved)
                directory.mkdir()
                process_tree_sampler.verify_run_paths(paths)
                (moved / "input.html").rename(moved / "old-input.html")
                (moved / "input.html").write_text("replacement", encoding="utf-8")
                must_be_incomplete("COMMAND_IDENTITY_CHANGED", lambda: process_tree_sampler.verify_run_paths(paths))
            finally:
                paths.close()
                shutil.rmtree(directory, ignore_errors=True)
                if moved.exists():
                    moved.rename(directory)
    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        directory.chmod(0o755)
        root, parent, proc = delegated_fixture(directory)
        delegation = require_fixture_parent(parent, root, proc)
        assert delegation.path == parent
        delegation.close()

        (parent / "cgroup.procs").chmod(0o666)
        must_be_incomplete("CGROUP_PERMISSIONS_UNSAFE", lambda: require_fixture_parent(parent, root, proc))
        (parent / "cgroup.procs").chmod(0o644)

        (proc / "self" / "cgroup").write_text("0::/delegated/other\n", encoding="ascii")
        must_be_incomplete(
            "CGROUP_DELEGATION_INVALID",
            lambda: require_fixture_parent(parent, root, proc),
        )
        (proc / "self" / "cgroup").write_text("0::/delegated/harness\n", encoding="ascii")
        (parent / "cgroup.procs").write_text("123\n", encoding="ascii")
        must_be_incomplete(
            "CGROUP_PARENT_POPULATED",
            lambda: require_fixture_parent(parent, root, proc),
        )
        (parent / "cgroup.procs").write_text("", encoding="ascii")

        must_be_incomplete(
            "CGROUP_PARENT_NOT_ABSOLUTE",
            lambda: require_fixture_parent(Path("delegated"), root, proc),
        )
        link = directory / "delegated-link"
        link.symlink_to(parent, target_is_directory=True)
        must_be_incomplete(
            "CGROUP_PARENT_NOT_CANONICAL",
            lambda: require_fixture_parent(link, root, proc),
        )

        (parent / "cgroup.subtree_control").write_text("cpu memory pids\n", encoding="ascii")
        must_be_incomplete(
            "CGROUP_CONTROLLERS_MISSING",
            lambda: require_fixture_parent(parent, root, proc),
        )
        (parent / "cgroup.subtree_control").write_text("cpu io memory pids\n", encoding="ascii")
        (parent / "unexpected").mkdir()
        must_be_incomplete(
            "CGROUP_NOT_EXCLUSIVE",
            lambda: require_fixture_parent(parent, root, proc),
        )

    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        root = directory / "root"
        parent = root / "parent"
        parent.mkdir(parents=True)
        bound_parent = process_tree_sampler.open_bound_directory(parent, root)
        collision_target = parent / "collision-target"
        collision_target.mkdir()
        collision_name = f"pliego-render-{os.getpid()}-fixed"
        (parent / collision_name).symlink_to(collision_target, target_is_directory=True)
        with mock.patch.object(process_tree_sampler.secrets, "token_hex", return_value="fixed"):
            must_be_incomplete("CGROUP_CREATE_FAILED", lambda: process_tree_sampler.create_leaf(bound_parent))
        assert collision_target.is_dir()

        leaf_path = parent / "leaf"
        counter_fixture(leaf_path)
        (leaf_path / "cgroup.procs").write_text("", encoding="ascii")
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | getattr(os, "O_NOFOLLOW", 0)
        leaf_fd = os.open("leaf", flags, dir_fd=bound_parent.fd)
        leaf_stat = os.fstat(leaf_fd)
        bound_leaf = process_tree_sampler.BoundDirectory(
            leaf_path,
            leaf_fd,
            leaf_stat.st_dev,
            leaf_stat.st_ino,
            bound_parent.fd,
            "leaf",
        )
        moved = parent / "moved-leaf"
        leaf_path.rename(moved)
        leaf_path.symlink_to(collision_target, target_is_directory=True)
        must_be_incomplete(
            "CGROUP_PATH_REPLACED",
            lambda: process_tree_sampler.remove_bound_leaf(bound_leaf, bound_parent),
        )
        assert leaf_path.is_symlink() and moved.is_dir() and collision_target.is_dir()
        bound_leaf.close()
        bound_parent.close()

    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        cgroup = directory / "leaf"
        counter_fixture(cgroup)
        assert process_tree_sampler.parse_io_stat("8:80 \n", "fixture") == {
            "8:80": {"rbytes": 0, "wbytes": 0, "rios": 0, "wios": 0}
        }
        bound = process_tree_sampler.open_bound_directory(cgroup, directory)
        snapshot, settle = process_tree_sampler.wait_for_accounting_quiescence(bound, 1.0, 20.0)
        assert snapshot["memory_file_dirty_bytes"] == 0
        assert settle["reads"] == 2 and len(settle["stable_observations"]) == 2
        (cgroup / "memory.stat").write_text("file_dirty 1\nfile_writeback 0\n", encoding="ascii")
        must_be_incomplete(
            "CGROUP_ACCOUNTING_NOT_QUIESCENT",
            lambda: process_tree_sampler.wait_for_accounting_quiescence(bound, 1.0, 3.0),
        )
        bound.close()

    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        proc = directory / "proc"
        cgroup = directory / "cgroup"
        cgroup.mkdir()
        root_pid = 4242
        write_proc_stat(proc, root_pid, 101)
        (cgroup / "cgroup.procs").write_text(f"{root_pid}\n", encoding="ascii")
        bound = process_tree_sampler.open_bound_directory(cgroup, directory)
        must_be_incomplete(
            "ROOT_IDENTITY_CHANGED",
            lambda: process_tree_sampler.scan_cgroup(bound, False, (root_pid, 100), proc),
        )
        bound.close()

    with tempfile.TemporaryDirectory() as raw:
        lock_root = Path(raw)
        parent = lock_root / "parent"
        parent.mkdir()
        with process_tree_sampler.exclusive_parent_lock(parent, lock_root):
            must_be_incomplete(
                "CGROUP_BUSY",
                lambda: process_tree_sampler.exclusive_parent_lock(parent, lock_root).__enter__(),
            )

    assert validate_result.PERCENTILE_METHOD == "nearest-rank-v1"
    assert validate_result.percentile([1, 3], 50) == 1

    env = os.environ.copy()
    env.pop("PLIEGO_BENCHMARK_CGROUP_PARENT", None)
    missing = subprocess.run(
        [sys.executable, str(SAMPLER), "--", sys.executable, "-c", "raise SystemExit(99)"],
        capture_output=True,
        text=True,
        env=env,
        timeout=10,
    )
    assert missing.returncode == 2
    assert "--cgroup-parent" in missing.stderr and "required" in missing.stderr


def child_workload(mebibytes: int, duration: float) -> int:
    allocation = bytearray(mebibytes * 1024 * 1024)
    for offset in range(0, len(allocation), 4096):
        allocation[offset] = (offset // 4096) % 251
    time.sleep(duration)
    return allocation[0]


def parent_workload(mebibytes: int, duration: float) -> int:
    children = [
        subprocess.Popen(
            [sys.executable, __file__, "--workload-child", str(mebibytes), str(duration)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=index == 1,
        )
        for index in range(2)
    ]
    return max(child.wait() for child in children)


def leaking_parent(pid_path: str) -> int:
    child = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(30)"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    with open(pid_path, "w", encoding="ascii") as output:
        output.write(str(child.pid))
        output.flush()
        os.fsync(output.fileno())
    return 0


def migration_attack(parent: str) -> int:
    for interface in ("cgroup.procs", "cgroup.threads"):
        try:
            with open(Path(parent) / interface, "w", encoding="ascii") as output:
                output.write(f"{os.getpid()}\n")
        except PermissionError as error:
            if error.errno not in {errno.EACCES, errno.EPERM}:
                return 91
        else:
            return 90
    return parent_workload(4, 0.3)


def clone_into_cgroup_attack(parent: str) -> int:
    descriptor = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        fields = (ctypes.c_uint64 * 11)()
        fields[0] = 0x200000000  # CLONE_INTO_CGROUP
        fields[4] = signal.SIGCHLD
        fields[10] = descriptor
        libc = ctypes.CDLL(None, use_errno=True)
        result = libc.syscall(435, ctypes.byref(fields), ctypes.sizeof(fields))
        if result == 0:
            os._exit(90)
        if result > 0:
            os.waitpid(result, 0)
            return 90
        return 0 if ctypes.get_errno() in {errno.EACCES, errno.EPERM} else 91
    finally:
        os.close(descriptor)


def detached_work(duration: float, marker: str) -> int:
    subprocess.Popen(
        [
            sys.executable,
            "-c",
            "import pathlib,sys,time; time.sleep(float(sys.argv[1])); pathlib.Path(sys.argv[2]).write_text('done')",
            str(duration),
            marker,
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    return 0


def hanging_tree(pid_path: str) -> int:
    child = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    Path(pid_path).write_text(str(child.pid), encoding="ascii")
    time.sleep(60)
    return 0


@contextlib.contextmanager
def workload_engine(
    root_sentinel: str | None = None,
    root_marker: str | None = None,
    version_marker: str | None = None,
) -> Iterator[str]:
    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        directory.chmod(0o755)
        engine = directory / "fixture-engine.py"
        engine.write_text(
            "#!/usr/bin/env python3\n"
            "import os, pathlib, sys\n"
            f"sentinel={root_sentinel!r}; root_marker={root_marker!r}; version_marker={version_marker!r}\n"
            "outcome='not-configured'\n"
            "if sentinel:\n"
            "    try: pathlib.Path(sentinel).read_text(); outcome='readable'\n"
            "    except PermissionError: outcome='denied'\n"
            "    except OSError as error: outcome=f'error:{error.errno}'\n"
            "pathlib.Path(root_marker).write_text(outcome) if root_marker else None\n"
            "if sys.argv[1:] == ['--version']:\n"
            "    pathlib.Path(version_marker).write_text(outcome) if version_marker else None\n"
            "    raise SystemExit(0)\n"
            "args=sys.argv[3:]; kept=[]; index=0\n"
            "while index < len(args):\n"
            "    if args[index] in {'--output','--artifacts'}:\n"
            "        index += 2\n"
            "    else:\n"
            "        kept.append(args[index]); index += 1\n"
            f"os.execv({str(Path(sys.executable).resolve())!r}, "
            f"[{str(Path(sys.executable).resolve())!r}, {str(Path(__file__).resolve())!r}, *kept])\n",
            encoding="utf-8",
        )
        engine.chmod(0o555)
        yield str(engine.resolve())


def workload_command(engine: str, mebibytes: int = 32, duration: float = 0.8) -> list[str]:
    return [engine, "render", "fixture.html", "--workload-parent", str(mebibytes), str(duration)]


@contextlib.contextmanager
def broker_invocation(
    command: list[str],
    *,
    descendant_grace_ms: float = 1000.0,
    deadline_ms: float = 10_000.0,
    observation: bool = True,
) -> Iterator[list[str]]:
    import grp

    cwd = Path(os.environ["PLIEGO_BENCHMARK_FIXTURE_CWD"])
    parent = os.environ["PLIEGO_BENCHMARK_CGROUP_PARENT"]
    broker = os.environ.get("PLIEGO_BENCHMARK_BROKER", "/usr/local/libexec/pliego-cgroup-broker")
    engine_gid = grp.getgrnam(process_tree_sampler.ENGINE_ACCOUNT).gr_gid
    with tempfile.TemporaryDirectory(prefix="pliego-broker-output-") as raw:
        root = Path(raw)
        output_dir = root / "output"
        artifacts_dir = root / "artifacts"
        for directory in (output_dir, artifacts_dir):
            directory.mkdir(mode=0o770)
            os.chown(directory, os.geteuid(), engine_gid)
            directory.chmod(0o770)
        bound_command = [
            *command[:3],
            "--output",
            str(output_dir / "document.pdf"),
            "--artifacts",
            str(artifacts_dir),
            *command[3:],
        ]
        yield [
            "/usr/bin/sudo",
            "--non-interactive",
            broker,
            "--cgroup-parent",
            parent,
            "--cwd",
            str(cwd),
            "--interval-ms",
            "75",
            "--pss-interval-ms",
            "250",
            "--descendant-grace-ms",
            str(descendant_grace_ms),
            "--deadline-ms",
            str(deadline_ms),
            "--observation",
            "on" if observation else "off",
            "--",
            *bound_command,
        ]


def broker_run(
    command: list[str],
    *,
    descendant_grace_ms: float = 1000.0,
    deadline_ms: float = 10_000.0,
    observation: bool = True,
) -> subprocess.CompletedProcess[str]:
    with broker_invocation(
        command,
        descendant_grace_ms=descendant_grace_ms,
        deadline_ms=deadline_ms,
        observation=observation,
    ) as invocation:
        return subprocess.run(
            invocation,
            capture_output=True,
            text=True,
            timeout=max(30.0, deadline_ms / 1000.0 + 10.0),
        )


def sampled(
    command: list[str],
    descendant_grace_ms: float = 1000.0,
    *,
    observation: bool = True,
) -> dict:
    result = broker_run(command, descendant_grace_ms=descendant_grace_ms, observation=observation)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


@contextlib.contextmanager
def engine_shared_directory() -> Iterator[Path]:
    import grp

    with tempfile.TemporaryDirectory(prefix="pliego-engine-shared-") as raw:
        directory = Path(raw)
        engine_gid = grp.getgrnam(process_tree_sampler.ENGINE_ACCOUNT).gr_gid
        os.chown(directory, os.geteuid(), engine_gid)
        directory.chmod(0o770)
        yield directory


def assert_exact_counters(proof: dict) -> None:
    final = proof["counters"]["final"]
    io_stat = final["io_stat"]
    assert proof["read_bytes"] == sum(device["rbytes"] for device in io_stat.values())
    assert proof["write_bytes"] == sum(device["wbytes"] for device in io_stat.values())
    assert proof["read_operations"] == sum(device["rios"] for device in io_stat.values())
    assert proof["write_operations"] == sum(device["wios"] for device in io_stat.values())
    assert proof["cpu_user_ms"] == round(final["cpu_stat"]["user_usec"] / 1000, 3)
    assert proof["cpu_sys_ms"] == round(final["cpu_stat"]["system_usec"] / 1000, 3)
    assert final["cgroup_events"]["populated"] == 0
    assert final["memory_file_dirty_bytes"] == 0
    assert final["memory_file_writeback_bytes"] == 0


def assert_validator_accepts(proof: dict, ok: bool) -> None:
    diagnostics = proof["sampled_diagnostics"]
    sample = {
        "ok": ok,
        "exit_code": proof["exit_code"],
        "signal": proof["signal"],
        "wall_ms": proof["tree_wall_ms"],
        "user_ms": proof["cpu_user_ms"],
        "sys_ms": proof["cpu_sys_ms"],
        "memory_current_bytes": proof["memory_current_bytes"],
        "memory_peak_bytes": proof["memory_peak_bytes"],
        "sequential_sampled_peak_rss_kib_diagnostic": diagnostics["sequential_sampled_peak_summed_rss_kib_diagnostic"],
        "sequential_sampled_peak_pss_kib_diagnostic": diagnostics["sequential_sampled_peak_summed_pss_kib_diagnostic"],
        "read_bytes": proof["read_bytes"],
        "write_bytes": proof["write_bytes"],
        "read_operations": proof["read_operations"],
        "write_operations": proof["write_operations"],
        "measurement_method": proof["method"],
        "resource_usage": proof,
    }
    violations: list[validate_result.Violation] = []
    validate_result.validate_resource_usage(sample, "$.sample", violations)
    assert not violations, "\n".join(map(str, violations))


def live_cgroup_proofs() -> dict:
    assert os.environ.get("PLIEGO_BENCHMARK_CGROUP_PARENT"), "live proof requires delegated parent"
    assert os.geteuid() != 0, "live harness and correctness tools must be unprivileged"
    account = process_tree_sampler.resolve_engine_account()
    sentinel = os.environ["PLIEGO_BENCHMARK_ROOT_SENTINEL"]
    with engine_shared_directory() as shared:
        root_marker = shared / "root-sentinel-outcome"
        version_marker = shared / "version-attempted"
        with workload_engine(sentinel, str(root_marker), str(version_marker)) as engine:
            proof = sampled(workload_command(engine))
            assert root_marker.read_text(encoding="ascii") == "denied", "candidate render retained root authority"
            assert not version_marker.exists(), "candidate --version executed inside the root broker"
            parent = os.environ["PLIEGO_BENCHMARK_CGROUP_PARENT"]
            attacked = sampled([engine, "render", "fixture.html", "--workload-migration-attack", parent])
            assert attacked["exit_code"] == 0
            assert attacked["counters"]["final"]["pids_peak"] >= 3
            assert set(attacked["launch_security"]["migration_write_probes"]["parent"].values()) <= {
                "EACCES",
                "EPERM",
            }
            assert set(attacked["launch_security"]["migration_fd_probes"]["measurement"].values()) <= {
                "EACCES",
                "EPERM",
            }
            clone_attacked = sampled([engine, "render", "fixture.html", "--workload-clone-attack", parent])
            assert clone_attacked["exit_code"] == 0

            marker = shared / "descendant-finished"
            descendant = sampled(
                [engine, "render", "fixture.html", "--workload-detached", "0.45", str(marker)],
                descendant_grace_ms=1000,
            )
            assert descendant["root_wall_ms"] < descendant["tree_wall_ms"]
            assert descendant["tree_wall_ms"] - descendant["root_wall_ms"] >= 300
            assert descendant["measurement_complete_ms"] >= descendant["tree_wall_ms"]
            assert marker.read_text(encoding="utf-8") == "done"
            assert descendant["cleanup"]["kill_used"] is False

            hang_pid = shared / "hang.pid"
            timed_out = broker_run(
                [engine, "render", "fixture.html", "--workload-hang", str(hang_pid)],
                deadline_ms=350,
                descendant_grace_ms=1000,
            )
            assert timed_out.returncode == 2, timed_out.stderr
            assert "measurement-incomplete[ENGINE_TIMEOUT]" in timed_out.stderr
            child_pid = int(hang_pid.read_text(encoding="ascii"))
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline and (Path("/proc") / str(child_pid)).exists():
                time.sleep(0.02)
            if (Path("/proc") / str(child_pid)).exists():
                raw_stat = (Path("/proc") / str(child_pid) / "stat").read_text(encoding="ascii")
                state = raw_stat[raw_stat.rfind(")") + 2 :].split()[0]
                assert state == "Z", f"timed-out descendant survived in state {state}"
            assert sorted(path.name for path in Path(parent).iterdir() if path.is_dir()) == ["harness"]

            signal_pid = shared / "signal-hang.pid"
            with broker_invocation(
                [engine, "render", "fixture.html", "--workload-hang", str(signal_pid)],
                deadline_ms=10_000,
                descendant_grace_ms=1000,
            ) as invocation:
                interrupted = subprocess.Popen(
                    invocation,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    start_new_session=True,
                )
                signal_deadline = time.monotonic() + 2
                while time.monotonic() < signal_deadline and not signal_pid.is_file():
                    time.sleep(0.02)
                assert signal_pid.is_file(), "signal-interruption workload did not start"
                os.killpg(interrupted.pid, signal.SIGTERM)
                _stdout, interrupted_stderr = interrupted.communicate(timeout=10)
            assert interrupted.returncode == 2, interrupted_stderr
            assert "measurement-incomplete[BROKER_INTERRUPTED]" in interrupted_stderr
            interrupted_child_pid = int(signal_pid.read_text(encoding="ascii"))
            signal_deadline = time.monotonic() + 2
            while time.monotonic() < signal_deadline and (Path("/proc") / str(interrupted_child_pid)).exists():
                time.sleep(0.02)
            if (Path("/proc") / str(interrupted_child_pid)).exists():
                raw_stat = (Path("/proc") / str(interrupted_child_pid) / "stat").read_text(encoding="ascii")
                state = raw_stat[raw_stat.rfind(")") + 2 :].split()[0]
                assert state == "Z", f"interrupted descendant survived in state {state}"
            assert sorted(path.name for path in Path(parent).iterdir() if path.is_dir()) == ["harness"]

    assert proof["method"] == "linux-cgroup-v2-v2"
    assert proof["scope"] == "fresh-root-owned-cgroup-subtree"
    launch = proof["launch_security"]
    assert launch["account"] == process_tree_sampler.ENGINE_ACCOUNT
    assert launch["uid"] == account.uid and launch["gid"] == account.gid
    assert launch["status"]["uid"] == [account.uid] * 4
    assert launch["status"]["gid"] == [account.gid] * 4
    assert launch["status"]["supplementary_groups"] == []
    assert all(
        int(launch["status"][field], 16) == 0
        for field in (
            "cap_inheritable_hex",
            "cap_permitted_hex",
            "cap_effective_hex",
            "cap_bounding_hex",
            "cap_ambient_hex",
        )
    )
    assert launch["status"]["no_new_privs"] == 1
    assert launch["parent_death_signal"] == signal.SIGKILL
    assert proof["counters"]["final"]["cgroup_events"]["populated"] == 0
    assert proof["cleanup"]["kill_used"] is False
    assert proof["root_wall_ms"] <= proof["tree_wall_ms"] <= proof["measurement_complete_ms"]
    assert proof["counters"]["final"]["pids_peak"] >= 3
    assert proof["memory_peak_bytes"] >= 48 * 1024 * 1024
    observed = proof["sampled_diagnostics"]["observed_processes"]
    assert len(observed) >= 3, observed
    assert any(row["pid"] == proof["root_pid"] and row["start_ticks"] == proof["root_start_ticks"] for row in observed)
    assert proof["sampled_diagnostics"]["sequential_sampled_peak_summed_rss_kib_diagnostic"] > 48 * 1024
    assert "not simultaneous bounds" in proof["sampled_diagnostics"]["coverage"]
    assert_exact_counters(proof)
    assert_validator_accepts(proof, True)

    with engine_shared_directory() as directory:
        pid_path = directory / "leaked.pid"
        with workload_engine() as engine:
            cleaned = sampled(
                [engine, "render", "fixture.html", "--workload-leak", str(pid_path)],
                descendant_grace_ms=50,
            )
        leaked_pid = int(pid_path.read_text(encoding="ascii"))
        assert cleaned["cleanup"]["kill_used"] is True
        assert any(row["pid"] == leaked_pid for row in cleaned["cleanup"]["lingering_before_kill"])
        try:
            os.kill(leaked_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raw_stat = (Path("/proc") / str(leaked_pid) / "stat").read_text(encoding="ascii")
            state = raw_stat[raw_stat.rfind(")") + 2 :].split()[0]
            assert state == "Z", f"cgroup.kill left PID {leaked_pid} alive in state {state}"
        assert_exact_counters(cleaned)
        assert_validator_accepts(cleaned, False)
    return proof


def php_integration_proof() -> None:
    php = shutil.which("php")
    assert php is not None, "Linux PHP is required for the PHP-to-Python integration gate"
    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        directory.chmod(0o755)
        failure_root = directory / "failures"
        failure_root.mkdir(mode=0o700)
        retained_workspace = failure_root / ".in-progress-integration"
        retained_workspace.mkdir(mode=0o700)
        fixture_cwd = Path(os.environ["PLIEGO_BENCHMARK_FIXTURE_CWD"])
        input_path = fixture_cwd / "fixture.html"
        engine = directory / "fake-engine.py"
        engine.write_text(
            """#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
output = pathlib.Path(args[args.index('--output') + 1])
artifacts = pathlib.Path(args[args.index('--artifacts') + 1])
artifacts.mkdir(parents=True, exist_ok=True)
with output.open('wb') as stream:
    stream.write(b'%PDF-1.4\\n%%EOF\\n')
    stream.flush()
    os.fsync(stream.fileno())
with (artifacts / 'scene-report.json').open('w') as stream:
    json.dump({'capture': {'status': 'complete'}, 'preview': {'page_count': 1}}, stream)
    stream.flush()
    os.fsync(stream.fileno())
print(json.dumps({'phase_timings_ms': {'capture': 1.0}}))
sys.stdout.flush()
os.fsync(sys.stdout.fileno())
""",
            encoding="utf-8",
        )
        engine.chmod(0o755)
        runner_command = [
            php,
            str(PHP_RUNNER),
            "--binary",
            str(engine),
            "--input",
            input_path.name,
            "--output",
            "output.pdf",
            "--artifacts",
            "ignored-artifacts",
            "--samples",
            "1",
            "--warmup",
            "0",
            "--cwd",
            str(fixture_cwd),
            "--retained-root",
            str(retained_workspace),
        ]
        root_refusal = subprocess.run(
            ["/usr/bin/sudo", "--non-interactive", *runner_command],
            capture_output=True,
            text=True,
            timeout=10,
        )
        assert root_refusal.returncode != 0
        assert "PDF verification must never run as root" in root_refusal.stderr
        run = subprocess.run(
            runner_command,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert run.returncode == 0, run.stderr
        sample = json.loads(run.stdout)
        assert sample["measurement_method"] == "linux-cgroup-v2-v2"
        assert sample["resource_usage"]["root_start_ticks"] > 0
        assert sample["resource_usage"]["counters"]["final"]["cgroup_events"]["populated"] == 0
        assert sample["wall_ms"] == sample["resource_usage"]["tree_wall_ms"]
        assert sample["memory_peak_bytes"] == sample["resource_usage"]["memory_peak_bytes"]
        violations: list[validate_result.Violation] = []
        validate_result.validate_resource_usage(sample, "$.sample", violations)
        assert not violations, "\n".join(map(str, violations))

        failed_run = subprocess.run(
            [*runner_command, "--page-count", "2"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert failed_run.returncode == 0, failed_run.stderr
        failed = json.loads(failed_run.stdout)
        assert failed["ok"] is False
        assert failed["correctness"]["pass"] is False
        retained_root = retained_workspace.resolve()
        for key in ("artifacts_dir", "output_dir"):
            retained = Path(failed["retained"][key])
            assert retained.resolve().is_relative_to(retained_root)
            assert retained.is_dir(), f"failed sample did not retain {key}: {retained}"
        assert (Path(failed["retained"]["artifacts_dir"]) / "scene-report.json").is_file()
        assert (Path(failed["retained"]["output_dir"]) / "output.pdf").is_file()
        evidence = benchmark_publication.write_failure_evidence(
            failure_root,
            {"sample": failed},
            "PHP integration correctness failure",
            retained_workspace,
        )
        assert not retained_workspace.exists()
        assert (evidence / "retained-samples").is_dir()
        assert "retained-samples/" in (evidence / "SHA256SUMS").read_text(encoding="utf-8")
        evidence_directories = [evidence, *(path for path in evidence.rglob("*") if path.is_dir())]
        for path in evidence_directories:
            if path.stat().st_uid == os.geteuid():
                path.chmod(0o700)
        shutil.rmtree(evidence)


def acceptance_overhead_proof() -> tuple[dict, dict]:
    pair_count = observer_ab.PAIR_COUNT
    rng = random.Random(observer_ab.SEED)
    pairs: list[dict] = []
    with workload_engine() as engine:
        command = workload_command(engine, duration=1.5)
        sampled(command, observation=False)
        sampled(command, observation=True)
        for index in range(pair_count):
            order = ["observation-off", "observation-on"]
            rng.shuffle(order)
            measurements: dict[str, dict] = {}
            for mode in order:
                measurements[mode] = sampled(command, observation=mode == "observation-on")
            off = measurements["observation-off"]
            on = measurements["observation-on"]
            pairs.append(observer_ab.measurement_pair(index, order, off, on))
    sampler_cpu = [float(pair["sampler_cpu_percent_of_tree_wall"]) for pair in pairs]
    observer_effect = observer_ab.build_measurements(pairs, duration_seconds=1.5)
    sampler_cpu_diagnostic = {
        "method": "sampler-process-cpu-divided-by-tree-wall-derived",
        "percentile_method": validate_result.PERCENTILE_METHOD,
        "p50_percent_of_tree_wall": round(validate_result.percentile(sampler_cpu, 50), 3),
        "p95_percent_of_tree_wall": round(validate_result.percentile(sampler_cpu, 95), 3),
        "raw_percent_of_tree_wall": sampler_cpu,
    }
    return observer_effect, sampler_cpu_diagnostic


def main(
    live: bool,
    php_integration: bool,
    acceptance_overhead: bool,
    observer_measurements_out: Path | None,
) -> None:
    fixture_proofs()
    proof = live_cgroup_proofs() if live or acceptance_overhead else None
    if php_integration:
        php_integration_proof()
    output: dict = {"fixture_proofs": "passed", "live": proof}
    if acceptance_overhead:
        observer_effect, sampler_cpu = acceptance_overhead_proof()
        output["observer_effect"] = observer_effect
        output["sampler_cpu_diagnostic"] = sampler_cpu
        if observer_measurements_out is not None:
            requested = observer_measurements_out
            destination = requested.resolve(strict=False)
            if not requested.is_absolute() or requested != destination:
                raise AssertionError("observer measurements output must be absolute and canonical")
            benchmark_publication.atomic_write_bytes(
                destination,
                json.dumps(observer_effect, indent=2, ensure_ascii=False).encode("utf-8") + b"\n",
            )
        assert observer_effect["passed"], observer_effect
    elif observer_measurements_out is not None:
        raise AssertionError("--observer-measurements-out requires --acceptance-overhead")
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--php-integration", action="store_true")
    parser.add_argument("--acceptance-overhead", action="store_true")
    parser.add_argument("--observer-measurements-out", type=Path)
    parser.add_argument("--workload-child", nargs=2, metavar=("MEBIBYTES", "DURATION"))
    parser.add_argument("--workload-parent", nargs=2, metavar=("MEBIBYTES", "DURATION"))
    parser.add_argument("--workload-leak", metavar="PID_PATH")
    parser.add_argument("--workload-migration-attack", metavar="CGROUP_PARENT")
    parser.add_argument("--workload-clone-attack", metavar="CGROUP_PARENT")
    parser.add_argument("--workload-detached", nargs=2, metavar=("DURATION", "MARKER"))
    parser.add_argument("--workload-hang", metavar="PID_PATH")
    args = parser.parse_args()
    if args.workload_child:
        raise SystemExit(child_workload(int(args.workload_child[0]), float(args.workload_child[1])))
    if args.workload_parent:
        raise SystemExit(parent_workload(int(args.workload_parent[0]), float(args.workload_parent[1])))
    if args.workload_leak:
        raise SystemExit(leaking_parent(args.workload_leak))
    if args.workload_migration_attack:
        raise SystemExit(migration_attack(args.workload_migration_attack))
    if args.workload_clone_attack:
        raise SystemExit(clone_into_cgroup_attack(args.workload_clone_attack))
    if args.workload_detached:
        raise SystemExit(detached_work(float(args.workload_detached[0]), args.workload_detached[1]))
    if args.workload_hang:
        raise SystemExit(hanging_tree(args.workload_hang))
    main(args.live, args.php_integration, args.acceptance_overhead, args.observer_measurements_out)
