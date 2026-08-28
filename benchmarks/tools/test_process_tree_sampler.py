#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fixture, live cgroup-v2, observer, and PHP bridge checks."""

from __future__ import annotations

import argparse
import base64
import contextlib
import errno
import hashlib
import io
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterator
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import process_tree_sampler
from benchmark_runtime import BROWSERSHOT_TARGET, GENERIC_TARGET, runtime_target
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


def delegated_namespace_root_fixture(directory: Path) -> tuple[Path, Path]:
    root = directory / "root"
    harness = root / "harness"
    proc = directory / "proc"
    harness.mkdir(parents=True)
    (proc / "self").mkdir(parents=True)
    (root / "cgroup.controllers").write_text("cpu io memory pids\n", encoding="ascii")
    (root / "cgroup.subtree_control").write_text("cpu io memory pids\n", encoding="ascii")
    (root / "cgroup.procs").write_text("", encoding="ascii")
    (root / "cgroup.threads").write_text("", encoding="ascii")
    (harness / "cgroup.procs").write_text(f"{os.getpid()}\n", encoding="ascii")
    (harness / "cgroup.threads").write_text(f"{os.getpid()}\n", encoding="ascii")
    (harness / "cgroup.type").write_text("domain\n", encoding="ascii")
    (proc / "self" / "cgroup").write_text("0::/harness\n", encoding="ascii")
    return root, proc


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
    if sys.platform == "linux":
        assert process_tree_sampler.DURABLE_WRITE_FLAG != 0
        assert process_tree_sampler.ENGINE_OUTPUT_OPEN_FLAGS & process_tree_sampler.DURABLE_WRITE_FLAG
    empty_io = process_tree_sampler.parse_io_stat("8:0 \n", "hosted-zero-io")
    assert empty_io == {"8:0": {"rbytes": 0, "wbytes": 0, "rios": 0, "wios": 0}}
    must_be_incomplete(
        "CGROUP_COUNTER_MISSING",
        lambda: process_tree_sampler.parse_io_stat("8:0 rbytes=1\n", "partial-io"),
    )
    must_be_incomplete(
        "CGROUP_COUNTER_INVALID",
        lambda: process_tree_sampler.parse_io_stat("not-a-device \n", "invalid-device"),
    )
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
    must_be_incomplete(
        "ENGINE_ACCOUNT_UNSAFE",
        lambda: process_tree_sampler.resolve_engine_account(
            lambda name: SimpleNamespace(pw_name=name, pw_uid=1234, pw_gid=1234, pw_dir="/nonexistent")
        ),
    )
    fixture_account = process_tree_sampler.resolve_engine_account(
        lambda name: SimpleNamespace(
            pw_name=name,
            pw_uid=1234,
            pw_gid=1234,
            pw_dir=process_tree_sampler.ENGINE_ACCOUNT_HOME,
        )
    )
    assert fixture_account.name == process_tree_sampler.ENGINE_ACCOUNT
    writable_home = SimpleNamespace(st_mode=0o040700, st_dev=1, st_ino=2)
    with (
        mock.patch.object(process_tree_sampler.os, "stat", return_value=writable_home),
        mock.patch.object(process_tree_sampler, "engine_account_can_write", return_value=True),
    ):
        must_be_incomplete(
            "ENGINE_ACCOUNT_UNSAFE",
            lambda: process_tree_sampler.validate_engine_account_home(fixture_account),
        )
    with mock.patch.dict(
        os.environ,
        {
            "BROWSERSHOT_CHROME_PATH": "/opt/chrome",
            "BROWSERSHOT_NODE_BINARY": "/usr/bin/node",
            "GITHUB_TOKEN": "must-not-leak",
            "LANG": "C",
        },
        clear=True,
    ):
        child_environment = process_tree_sampler.engine_environment(fixture_account, "/engine-owned-artifacts")
    assert "GITHUB_TOKEN" not in child_environment and child_environment["LANG"] == "C"
    assert child_environment["BROWSERSHOT_CHROME_PATH"] == "/opt/chrome"
    assert child_environment["BROWSERSHOT_NODE_BINARY"] == "/usr/bin/node"
    assert child_environment["TMPDIR"] == "/engine-owned-artifacts"
    assert child_environment["HOME"] == process_tree_sampler.ENGINE_ACCOUNT_HOME
    assert not (set(process_tree_sampler.RUNTIME_DIRECTORY_NAMES) - {"HOME"}).intersection(child_environment)
    private_runtime = {
        variable: f"/engine-owned-artifacts/{name}"
        for variable, name in process_tree_sampler.RUNTIME_DIRECTORY_NAMES.items()
    }
    private_environment = process_tree_sampler.engine_environment(
        fixture_account, "/engine-owned-artifacts", private_runtime
    )
    assert {variable: private_environment[variable] for variable in private_runtime} == private_runtime
    assert runtime_target(("/repo/benchmarks/adapters/browsershot/adapter.php", "render")) == BROWSERSHOT_TARGET
    assert runtime_target(("/repo/benchmarks/adapters/dompdf/adapter.php", "render")) == GENERIC_TARGET
    assert runtime_target(("/usr/lib/pliego", "render-api2")) == GENERIC_TARGET
    with tempfile.TemporaryDirectory() as raw:
        runtime_root = Path(raw)
        older = runtime_root / "home" / "older.bin"
        newer = runtime_root / "xdg-cache" / "newer.bin"
        older.parent.mkdir()
        newer.parent.mkdir()
        older.write_bytes(b"old")
        newer.write_bytes(b"newest")
        os.utime(older, ns=(1_000_000_000, 1_000_000_000))
        os.utime(newer, ns=(2_000_000_000, 2_000_000_000))
        diagnostics = process_tree_sampler.runtime_directory_diagnostics(runtime_root, limit=1)
    assert "xdg-cache/newer.bin:file:size=6" in diagnostics
    assert "home/older.bin" not in diagnostics
    assert "browser-runtime-file-count=2" in diagnostics
    with tempfile.TemporaryDirectory() as raw:
        exact_root = Path(raw).resolve()
        for index in range(3):
            (exact_root / f"exact-{index}").write_text("fixture", encoding="ascii")
        exact_inventory = process_tree_sampler.runtime_directory_diagnostics(exact_root, scan_limit=3)
        (exact_root / "overflow").write_text("fixture", encoding="ascii")
        truncated_inventory = process_tree_sampler.runtime_directory_diagnostics(exact_root, scan_limit=3)
    assert "browser-runtime-entries-examined=3" in exact_inventory
    assert "browser-runtime-scan-truncated=false" in exact_inventory
    assert "browser-runtime-scan-truncated=true" in truncated_inventory

    with mock.patch.object(
        process_tree_sampler, "runtime_directory_diagnostics", side_effect=RuntimeError("must not escape")
    ):
        unavailable = process_tree_sampler.browser_failure_diagnostics(Path("/fixture"))
    assert unavailable == "browser-runtime-inventory-unavailable=RuntimeError:unknown"
    assert "must not escape" not in unavailable
    with tempfile.TemporaryDirectory() as raw:
        output = Path(raw) / "engine.stderr"
        output.write_bytes(b"prefix\n" + b"x" * 32)
        assert process_tree_sampler.bounded_output_tail(output, limit=8) == json.dumps("x" * 8)
        missing = process_tree_sampler.bounded_output_tail(Path(raw) / "missing")
        assert missing.startswith("unavailable:FileNotFoundError:")
    if sys.platform == "linux":
        with tempfile.TemporaryDirectory(prefix="pliego mount ") as raw:
            root = Path(raw).resolve()
            nested = root / "nested mount"
            leaf = nested / "leaf"
            leaf.mkdir(parents=True)
            escaped_root = str(root).replace(" ", "\\040")
            escaped_nested = str(nested).replace(" ", "\\040")
            mountinfo = (
                f"36 25 8:1 / {escaped_root} rw,relatime - ext4 /dev/root rw\n"
                f"37 36 8:2 / {escaped_nested} rw,relatime - xfs /dev/fixture rw\n"
            )
            assert process_tree_sampler.decode_mountinfo_path(escaped_nested) == str(nested)
            assert process_tree_sampler.mountinfo_filesystem_type(root, mountinfo) == "ext4"
            assert process_tree_sampler.mountinfo_filesystem_type(leaf, mountinfo) == "xfs"

        with (
            tempfile.TemporaryDirectory() as raw,
            mock.patch.object(process_tree_sampler, "mountinfo_filesystem_type", return_value="ext4"),
            mock.patch.object(
                process_tree_sampler,
                "directory_inode_flags",
                return_value=(
                    process_tree_sampler.FS_DIRSYNC_FL
                    | process_tree_sampler.FS_SYNC_FL
                    | process_tree_sampler.FS_NOATIME_FL
                ),
            ),
            mock.patch.object(
                process_tree_sampler,
                "controlled_storage_root_identity",
                side_effect=process_tree_sampler.engine_temporary_directory_identity,
            ),
        ):
            root = Path(raw).resolve()
            artifacts = root / "artifacts"
            temporary = root / "temporary"
            artifacts.mkdir(mode=0o700)
            temporary.mkdir(mode=0o700)
            current_account = process_tree_sampler.EngineAccount(
                "fixture-engine",
                os.getuid(),
                os.getgid(),
                process_tree_sampler.ENGINE_ACCOUNT_HOME,
            )
            native_sandbox = root / "native-sandbox"
            native_job = native_sandbox / "job"
            native_temporary = native_sandbox / "temporary"
            native_sandbox.mkdir(mode=0o700)
            native_job.mkdir(mode=0o700)
            native_temporary.mkdir(mode=0o700)
            (native_job / "input").mkdir(mode=0o700)
            (native_job / "input-manifest.json").write_bytes(b"{}\n")
            (native_job / "input-manifest.json").chmod(0o600)
            native_bindings = process_tree_sampler.bind_native_api2_storage(
                native_job,
                native_temporary,
                current_account,
                require_pristine_job=True,
            )
            assert native_bindings == {
                "controlled_root": process_tree_sampler.engine_temporary_directory_identity(root),
                "sandbox": process_tree_sampler.engine_temporary_directory_identity(native_sandbox),
                "job": process_tree_sampler.engine_temporary_directory_identity(native_job),
                "temporary": process_tree_sampler.engine_temporary_directory_identity(native_temporary),
            }

            wrong_job = native_sandbox / "work"
            wrong_job.mkdir(mode=0o700)
            must_be_incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                lambda: process_tree_sampler.bind_native_api2_storage(
                    wrong_job, native_temporary, current_account
                ),
            )
            wrong_temporary = native_sandbox / "scratch"
            wrong_temporary.mkdir(mode=0o700)
            must_be_incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                lambda: process_tree_sampler.bind_native_api2_storage(
                    native_job, wrong_temporary, current_account
                ),
            )
            other_sandbox = root / "other-native-sandbox"
            other_temporary = other_sandbox / "temporary"
            other_sandbox.mkdir(mode=0o700)
            other_temporary.mkdir(mode=0o700)
            must_be_incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                lambda: process_tree_sampler.bind_native_api2_storage(
                    native_job, other_temporary, current_account
                ),
            )

            unexpected_job_entry = native_job / "unexpected"
            unexpected_job_entry.write_bytes(b"unexpected")
            must_be_incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                lambda: process_tree_sampler.bind_native_api2_storage(
                    native_job,
                    native_temporary,
                    current_account,
                    require_pristine_job=True,
                ),
            )
            unexpected_job_entry.unlink()
            unexpected_scratch_entry = native_temporary / "unexpected"
            unexpected_scratch_entry.write_bytes(b"unexpected")
            must_be_incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                lambda: process_tree_sampler.bind_native_api2_storage(
                    native_job,
                    native_temporary,
                    current_account,
                    require_pristine_job=True,
                ),
            )
            unexpected_scratch_entry.unlink()

            original_input = native_job / "real-input"
            (native_job / "input").rename(original_input)
            (native_job / "input").symlink_to(original_input, target_is_directory=True)
            must_be_incomplete(
                "NATIVE_API2_STORAGE_UNSAFE",
                lambda: process_tree_sampler.bind_native_api2_storage(
                    native_job,
                    native_temporary,
                    current_account,
                    require_pristine_job=True,
                ),
            )
            (native_job / "input").unlink()
            original_input.rename(native_job / "input")

            with mock.patch.object(process_tree_sampler, "mountinfo_filesystem_type", return_value="tmpfs"):
                must_be_incomplete(
                    "ENGINE_TEMPORARY_BACKING_UNSAFE",
                    lambda: process_tree_sampler.bind_native_api2_storage(
                        native_job, native_temporary, current_account
                    ),
                )
            required_flags = (
                process_tree_sampler.FS_DIRSYNC_FL
                | process_tree_sampler.FS_SYNC_FL
                | process_tree_sampler.FS_NOATIME_FL
            )
            for missing_flag in (
                process_tree_sampler.FS_DIRSYNC_FL,
                process_tree_sampler.FS_SYNC_FL,
                process_tree_sampler.FS_NOATIME_FL,
            ):
                with mock.patch.object(
                    process_tree_sampler,
                    "directory_inode_flags",
                    return_value=required_flags & ~missing_flag,
                ):
                    must_be_incomplete(
                        "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
                        lambda: process_tree_sampler.bind_native_api2_storage(
                            native_job, native_temporary, current_account
                        ),
                    )

            replaced_job = native_sandbox / "replaced-job"
            native_job.rename(replaced_job)
            native_job.mkdir(mode=0o700)
            (native_job / "input").mkdir(mode=0o700)
            (native_job / "input-manifest.json").write_bytes(b"{}\n")
            (native_job / "input-manifest.json").chmod(0o600)
            must_be_incomplete(
                "NATIVE_API2_STORAGE_REPLACED",
                lambda: process_tree_sampler.bind_native_api2_storage(
                    native_job,
                    native_temporary,
                    current_account,
                    expected=native_bindings,
                ),
            )

            runtime_environment = process_tree_sampler.prepare_runtime_directories(temporary, current_account)
            assert runtime_environment == {
                variable: str(temporary / name)
                for variable, name in process_tree_sampler.RUNTIME_DIRECTORY_NAMES.items()
            }
            root_identity = process_tree_sampler.engine_temporary_directory_identity(temporary)
            runtime_binding = process_tree_sampler.runtime_directory_bindings(
                temporary, root_identity, runtime_environment, current_account
            )
            expected_identities = {
                variable: (binding["identity"]["device"], binding["identity"]["inode"])
                for variable, binding in runtime_binding["bindings"].items()
            }
            assert (
                process_tree_sampler.runtime_directory_bindings(
                    temporary, root_identity, runtime_environment, current_account, expected_identities
                )
                == runtime_binding
            )
            escaped = dict(runtime_environment)
            escaped["HOME"] = str(root)
            must_be_incomplete(
                "ENGINE_RUNTIME_DIRECTORY_UNSAFE",
                lambda: process_tree_sampler.runtime_directory_bindings(
                    temporary, root_identity, escaped, current_account
                ),
            )
            original_home = temporary / process_tree_sampler.RUNTIME_DIRECTORY_NAMES["HOME"]
            replaced_home = temporary / "replaced-home"
            original_home.rename(replaced_home)
            original_home.mkdir(mode=0o700)
            original_home.chmod(0o700)
            must_be_incomplete(
                "ENGINE_RUNTIME_DIRECTORY_REPLACED",
                lambda: process_tree_sampler.runtime_directory_bindings(
                    temporary, root_identity, runtime_environment, current_account, expected_identities
                ),
            )
            adapter_command = ("/adapter", "render", "input.html", "--artifacts", str(artifacts))
            assert process_tree_sampler.engine_temporary_directory(
                adapter_command, current_account, str(temporary)
            ) == str(temporary)
            must_be_incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_REQUIRED",
                lambda: process_tree_sampler.engine_temporary_directory(adapter_command, current_account, None),
            )
            assert process_tree_sampler.engine_temporary_directory(
                ("/pliego", "render-api2"), current_account, str(artifacts)
            ) == str(artifacts)
            identity = process_tree_sampler.engine_temporary_directory_identity(artifacts)
            process_tree_sampler.revalidate_engine_temporary_directory(artifacts, current_account, identity)
            must_be_incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_REPLACED",
                lambda: process_tree_sampler.revalidate_engine_temporary_directory(
                    artifacts, current_account, (identity[0], identity[1] + 1)
                ),
            )
            must_be_incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_REQUIRED",
                lambda: process_tree_sampler.engine_temporary_directory(
                    ("/pliego", "render-api2"), current_account, None
                ),
            )
            artifacts.chmod(0o755)
            must_be_incomplete(
                "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
                lambda: process_tree_sampler.engine_temporary_directory(
                    adapter_command, current_account, str(temporary)
                ),
            )
            artifacts.chmod(0o700)
            with mock.patch.object(process_tree_sampler, "mountinfo_filesystem_type", return_value="tmpfs"):
                must_be_incomplete(
                    "ENGINE_TEMPORARY_BACKING_UNSAFE",
                    lambda: process_tree_sampler.validate_engine_temporary_directory(artifacts, current_account),
                )
            with mock.patch.object(
                process_tree_sampler,
                "directory_inode_flags",
                return_value=process_tree_sampler.FS_SYNC_FL | process_tree_sampler.FS_NOATIME_FL,
            ):
                must_be_incomplete(
                    "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
                    lambda: process_tree_sampler.validate_engine_temporary_directory(artifacts, current_account),
                )
            with mock.patch.object(
                process_tree_sampler,
                "directory_inode_flags",
                return_value=process_tree_sampler.FS_DIRSYNC_FL | process_tree_sampler.FS_NOATIME_FL,
            ):
                must_be_incomplete(
                    "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
                    lambda: process_tree_sampler.validate_engine_temporary_directory(artifacts, current_account),
                )
            with mock.patch.object(
                process_tree_sampler,
                "directory_inode_flags",
                return_value=process_tree_sampler.FS_DIRSYNC_FL | process_tree_sampler.FS_SYNC_FL,
            ):
                must_be_incomplete(
                    "ENGINE_TEMPORARY_DIRECTORY_UNSAFE",
                    lambda: process_tree_sampler.validate_engine_temporary_directory(artifacts, current_account),
                )

    with tempfile.TemporaryDirectory() as raw:
        cgroup_root = Path(raw).resolve()
        staging_path = cgroup_root / "staging"
        staging_path.mkdir()
        staging = process_tree_sampler.BoundDirectory(staging_path, -1, 1, 2)
        pid = 123
        stopped = (process_tree_sampler.signal.SIGSTOP << 8) | 0x7F
        network = {
            "mode": "linux-private-network-namespace-v1",
            "host_namespace": "net:[1]",
            "engine_namespace": "net:[2]",
            "interfaces": ["lo"],
        }
        handshake = {
            "ok": True,
            "cwd": "/fixture/job",
            "tmpdir": "/fixture/temporary",
            "executable_accessible": True,
            "executable_writable": False,
            "cwd_accessible": True,
            "migration_write_probes": {
                name: {"cgroup.procs": "EACCES", "cgroup.threads": "EPERM"}
                for name in ("parent", "harness", "staging", "measurement")
            },
            "network_isolation": network,
        }
        security = {
            "uid": [fixture_account.uid] * 4,
            "gid": [fixture_account.gid] * 4,
            "supplementary_groups": [],
            "cap_inheritable_hex": "0",
            "cap_permitted_hex": "0",
            "cap_effective_hex": "0",
            "cap_bounding_hex": "0",
            "cap_ambient_hex": "0",
            "no_new_privs": 1,
        }

        def namespace(path: Path) -> str:
            return (
                network["host_namespace"] if path.parts[-3:] == ("self", "ns", "net") else network["engine_namespace"]
            )

        with (
            mock.patch.object(process_tree_sampler.os, "kill"),
            mock.patch.object(process_tree_sampler.os, "waitpid", return_value=(pid, stopped)),
            mock.patch.object(process_tree_sampler.os, "read", return_value=json.dumps(handshake).encode()),
            mock.patch.object(process_tree_sampler.os, "close"),
            mock.patch.object(process_tree_sampler.os, "readlink", side_effect=namespace),
            mock.patch.object(process_tree_sampler, "read_stat", return_value={"start_ticks": 77}),
            mock.patch.object(process_tree_sampler, "cgroup_relative", return_value="/staging"),
            mock.patch.object(process_tree_sampler, "cgroup_pids", return_value=[pid]),
            mock.patch.object(process_tree_sampler, "read_process_security", return_value=security),
        ):
            retained = process_tree_sampler.finish_authority_handshake(
                pid,
                42,
                fixture_account,
                staging,
                cgroup_root,
                (pid, 77),
                cgroup_root,
            )
        assert retained["network_isolation"] == network
        assert retained["launch_context"] == {
            "cwd": "/fixture/job",
            "tmpdir": "/fixture/temporary",
        }

    with tempfile.TemporaryDirectory() as raw:
        executable = Path(raw) / "true"
        shutil.copy2(shutil.which("true") or "/bin/true", executable)
        executable.chmod(0o555)
        must_be_incomplete(
            "ENGINE_COMMAND_INVALID",
            lambda: process_tree_sampler.command_identity([str(executable.resolve())], fixture_account),
        )
        argv, _ = process_tree_sampler.command_identity(
            [str(executable.resolve()), "render-api2"],
            fixture_account,
        )
        assert argv == (str(executable.resolve()), "render-api2")
        adapter_argv, _ = process_tree_sampler.command_identity(
            [str(executable.resolve()), "render", "input.html"],
            fixture_account,
        )
        assert adapter_argv == (str(executable.resolve()), "render", "input.html")
        must_be_incomplete(
            "ENGINE_COMMAND_INVALID",
            lambda: process_tree_sampler.command_identity(
                [str(executable.resolve()), "render-api2", "--unexpected"],
                fixture_account,
            ),
        )
        with (
            mock.patch.object(process_tree_sampler, "require_broker_root"),
            mock.patch.object(process_tree_sampler, "resolve_engine_account", return_value=fixture_account),
            mock.patch.object(process_tree_sampler, "require_delegated_parent") as delegated,
        ):
            must_be_incomplete(
                "ENGINE_COMMAND_INVALID",
                lambda: process_tree_sampler.sample_command(
                    [str(executable.resolve())],
                    raw,
                    os.devnull,
                    os.devnull,
                    1,
                    1,
                    0,
                    1,
                    Path(raw),
                ),
            )
            delegated.assert_not_called()

    if sys.platform == "linux" and os.geteuid() == 0:
        with tempfile.TemporaryDirectory() as raw:
            stdin_path = (Path(raw) / "request.json").resolve()
            stdin_path.write_bytes(b"{}\n")
            stdin_path.chmod(0o400)
            descriptor = process_tree_sampler.open_stdin_descriptor(str(stdin_path))
            try:
                assert os.read(descriptor, 3) == b"{}\n"
            finally:
                os.close(descriptor)
            stdin_path.chmod(0o600)
            must_be_incomplete(
                "ENGINE_STDIN_MUTABLE",
                lambda: process_tree_sampler.open_stdin_descriptor(str(stdin_path)),
            )

    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        directory.chmod(0o755)
        root, parent, proc = delegated_fixture(directory)
        delegation = require_fixture_parent(parent, root, proc)
        assert delegation.path == parent
        delegation.close()

        (parent / "cgroup.type").unlink()
        must_be_incomplete("CGROUP_INTERFACES_MISSING", lambda: require_fixture_parent(parent, root, proc))
        (parent / "cgroup.type").write_text("domain\n", encoding="ascii")

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
        directory.chmod(0o755)
        root, proc = delegated_namespace_root_fixture(directory)
        delegation = process_tree_sampler.require_delegated_parent(
            root,
            root,
            proc,
            os.getuid(),
            os.getgid(),
        )
        assert delegation.path == root
        delegation.close()

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
        bound = process_tree_sampler.open_bound_directory(cgroup, directory)
        snapshot, settle = process_tree_sampler.wait_for_accounting_quiescence(bound, 1.0, 20.0)
        assert snapshot["memory_file_dirty_bytes"] == 0
        assert settle["reads"] == 2 and settle["stable_reads"] == 2
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
    assert "measurement-incomplete[CGROUP_PARENT_REQUIRED]" in missing.stderr


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


@contextlib.contextmanager
def workload_engine(arguments: list[str] | None = None) -> Iterator[str]:
    arguments = arguments or ["--workload-parent", "32", "0.8"]
    embedded_child = """import sys
import time

mebibytes = int(sys.argv[1])
duration = float(sys.argv[2])
allocation = bytearray(mebibytes * 1024 * 1024)
for offset in range(0, len(allocation), 4096):
    allocation[offset] = (offset // 4096) % 251
time.sleep(duration)
raise SystemExit(allocation[0])
"""
    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        directory.chmod(0o755)
        engine = directory / "fixture-engine.py"
        engine.write_text(
            f"""#!/usr/bin/env python3
import errno
import os
from pathlib import Path
import subprocess
import sys
import time

ARGUMENTS = {arguments!r}


def parent_workload(mebibytes, duration):
    child_program = {embedded_child!r}
    children = [
        subprocess.Popen(
            [sys.executable, "-c", child_program, str(mebibytes), str(duration)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=index == 1,
        )
        for index in range(2)
    ]
    return max(child.wait() for child in children)


def leaking_parent(pid_path):
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


def migration_attack(parent):
    for interface in ("cgroup.procs", "cgroup.threads"):
        try:
            with open(Path(parent) / interface, "w", encoding="ascii") as output:
                output.write(f"{{os.getpid()}}\\n")
        except PermissionError as error:
            if error.errno not in {{errno.EACCES, errno.EPERM}}:
                return 91
        else:
            return 90
    return parent_workload(4, 0.3)


def main():
    if ARGUMENTS[0] == "--workload-parent":
        return parent_workload(int(ARGUMENTS[1]), float(ARGUMENTS[2]))
    if ARGUMENTS[0] == "--workload-leak":
        return leaking_parent(ARGUMENTS[1])
    if ARGUMENTS[0] == "--workload-migration-attack":
        return migration_attack(ARGUMENTS[1])
    raise ValueError(f"unsupported fixture workload: {{ARGUMENTS!r}}")


if __name__ == "__main__":
    raise SystemExit(main())
""",
            encoding="utf-8",
        )
        engine.chmod(0o555)
        yield str(engine.resolve())


def workload_command(engine: str) -> list[str]:
    return [engine, "render-api2"]


def sampled(command: list[str], descendant_grace_ms: float = 1000.0) -> dict:
    account = process_tree_sampler.resolve_engine_account()
    temporary_root = os.environ.get("PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT")
    assert temporary_root, "live sampler proof requires PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT"
    with tempfile.TemporaryDirectory(dir=temporary_root) as raw:
        sandbox = Path(raw).resolve()
        os.chown(sandbox, account.uid, account.gid)
        sandbox.chmod(0o700)
        job = sandbox / "job"
        temporary = sandbox / "temporary"
        job.mkdir(mode=0o700)
        temporary.mkdir(mode=0o700)
        (job / "input").mkdir(mode=0o700)
        (job / "input-manifest.json").write_bytes(b"{}\n")
        for path in (job, temporary, job / "input", job / "input-manifest.json"):
            os.chown(path, account.uid, account.gid)
            path.chmod(0o700 if path.is_dir() else 0o600)
        result = subprocess.run(
            [
                sys.executable,
                str(SAMPLER),
                "--interval-ms",
                "75",
                "--pss-interval-ms",
                "250",
                "--descendant-grace-ms",
                str(descendant_grace_ms),
                "--cwd",
                str(job),
                "--temporary-directory",
                str(temporary),
                "--",
                *command,
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def direct_wall_ms(command: list[str]) -> float:
    started = time.monotonic()
    result = subprocess.run(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
    assert result.returncode == 0
    return (time.monotonic() - started) * 1000.0


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
    minimum_one_shot_wall_ms = round(
        proof["wall_ms"] + proof["drain_ms"] + proof["accounting_settle"]["duration_ms"],
        3,
    )
    sample = {
        "ok": ok,
        "exit_code": proof["exit_code"],
        "signal": proof["signal"],
        "wall_ms": proof["wall_ms"],
        "one_shot_wall_ms": minimum_one_shot_wall_ms,
        "user_ms": proof["cpu_user_ms"],
        "sys_ms": proof["cpu_sys_ms"],
        "memory_current_bytes": proof["memory_current_bytes"],
        "memory_peak_bytes": proof["memory_peak_bytes"],
        "sampled_peak_rss_kib_lower_bound": diagnostics["sampled_peak_summed_rss_kib_lower_bound"],
        "sampled_peak_pss_kib_lower_bound": diagnostics["sampled_peak_summed_pss_kib_lower_bound"],
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
    account = process_tree_sampler.resolve_engine_account()
    with workload_engine() as engine:
        proof = sampled(workload_command(engine))
    parent = os.environ["PLIEGO_BENCHMARK_CGROUP_PARENT"]
    with workload_engine(["--workload-migration-attack", parent]) as engine:
        attacked = sampled(workload_command(engine))
        assert attacked["exit_code"] == 0, attacked
        assert attacked["counters"]["final"]["pids_peak"] >= 3
        assert set(attacked["launch_security"]["migration_write_probes"]["parent"].values()) <= {
            "EACCES",
            "EPERM",
        }
    assert proof["method"] == "linux-cgroup-v2-v1"
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
    assert proof["cgroup_drained"] is True
    assert proof["cleanup"]["kill_used"] is False
    assert proof["counters"]["final"]["pids_peak"] >= 3
    assert proof["memory_peak_bytes"] >= 48 * 1024 * 1024
    observed = proof["sampled_diagnostics"]["observed_processes"]
    assert len(observed) >= 3, observed
    assert any(row["pid"] == proof["root_pid"] and row["start_ticks"] == proof["root_start_ticks"] for row in observed)
    assert proof["sampled_diagnostics"]["sampled_peak_summed_rss_kib_lower_bound"] > 48 * 1024
    assert "lower bounds" in proof["sampled_diagnostics"]["coverage"]
    assert_exact_counters(proof)
    assert_validator_accepts(proof, True)

    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        os.chown(directory, account.uid, account.gid)
        directory.chmod(0o700)
        pid_path = directory / "leaked.pid"
        with workload_engine(["--workload-leak", str(pid_path)]) as engine:
            cleaned = sampled(
                workload_command(engine),
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
        input_path = directory / "input.html"
        output_path = directory / "output.pdf"
        artifacts_path = directory / "ignored-artifacts"
        engine = directory / "fake-engine.py"
        input_bytes = b"<p>fixture</p>"
        input_path.write_bytes(input_bytes)
        input_sha256 = hashlib.sha256(input_bytes).hexdigest()
        bundle = hashlib.sha256()
        bundle.update(b"input.html\0" + bytes.fromhex(input_sha256))
        delivery_fixture = Path(__file__).resolve().parents[2] / "contracts" / "api2" / "fixtures" / "delivery"
        pdf_base64 = base64.b64encode((delivery_fixture / "document.pdf").read_bytes()).decode("ascii")
        scene_base64 = base64.b64encode((delivery_fixture / "scene.json").read_bytes()).decode("ascii")
        bundle_base64 = base64.b64encode((delivery_fixture / "bundle.json").read_bytes()).decode("ascii")
        engine.write_text(
            f"""#!/usr/bin/python3 -B
import base64, hashlib, json, os, pathlib, sys


def write_synced(path, payload):
    with path.open('wb') as output:
        output.write(payload)
        output.flush()
        os.fsync(output.fileno())


def sync_directory(path):
    flags = os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0) | getattr(os, 'O_CLOEXEC', 0)
    descriptor = os.open(path, flags)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


assert sys.argv[1:] == ['render-api2']
request_bytes = sys.stdin.buffer.read()
request = json.loads(request_bytes)
assert request_bytes == (json.dumps(request, separators=(',', ':')) + '\\n').encode()
assert request['api'] == 2
root = pathlib.Path.cwd()
temporary = pathlib.Path(os.environ['TMPDIR'])
assert temporary.is_dir()
assert root.name == 'job'
assert temporary.name == 'temporary'
assert root.parent == temporary.parent
assert sorted(path.name for path in root.iterdir()) == ['input', 'input-manifest.json']
manifest_bytes = (root / 'input-manifest.json').read_bytes()
manifest_descriptor = request['input']['manifest']
assert manifest_descriptor['bytes'] == len(manifest_bytes)
assert manifest_descriptor['sha256'] == 'sha256:' + hashlib.sha256(manifest_bytes).hexdigest()
manifest = json.loads(manifest_bytes)
assert manifest['entries'][0]['path'] == 'input.html'
assert manifest['entries'][0]['sha256'] == 'sha256:' + hashlib.sha256((root / 'input/input.html').read_bytes()).hexdigest()
delivery = root / 'delivery'
delivery.mkdir()
diagnostics = pathlib.Path.cwd() / 'diagnostics'
diagnostics.mkdir()
pdf = base64.b64decode({pdf_base64!r})
scene = base64.b64decode({scene_base64!r})
bundle = base64.b64decode({bundle_base64!r})
environment = b'{{"fixture":true}}\\n'
write_synced(delivery / 'document.pdf', pdf)
write_synced(delivery / 'scene.json', scene)
write_synced(delivery / 'bundle.json', bundle)
write_synced(diagnostics / 'environment.json', environment)
sync_directory(delivery)
sync_directory(diagnostics)
sync_directory(root)
result = {{
    'schema': 'pliego.render-result',
    'version': 1,
    'api': 2,
    'status': 'success',
    'request': request,
    'engine': {{
        'name': 'pliego',
        'version': '0.0.0-test',
        'api': 2,
        'source_commit': '0' * 40,
        'runtime': {{
            'mode': 'one-shot',
            'target': 'x86_64-unknown-linux-gnu',
            'binary_sha256': 'sha256:' + hashlib.sha256(pathlib.Path(sys.argv[0]).read_bytes()).hexdigest(),
            'servo_base': '1' * 40,
        }},
    }},
    'delivery': {{
        'pdf': {{
            'path': 'document.pdf',
            'media_type': 'application/pdf',
            'sha256': 'sha256:' + hashlib.sha256(pdf).hexdigest(),
            'bytes': len(pdf),
        }},
        'scene': {{
            'path': 'scene.json',
            'media_type': 'application/vnd.pliego.document-scene+json',
            'sha256': 'sha256:' + hashlib.sha256(scene).hexdigest(),
            'bytes': len(scene),
        }},
        'bundle': {{
            'path': 'bundle.json',
            'media_type': 'application/vnd.pliego.bundle-manifest+json',
            'sha256': 'sha256:' + hashlib.sha256(bundle).hexdigest(),
            'bytes': len(bundle),
        }},
    }},
    'conformance': {{'requested': None, 'status': 'not-requested', 'evidence': None}},
    'diagnostics': {{'retained': True, 'artifacts': [{{
        'path': 'diagnostics/environment.json',
        'media_type': 'application/json',
        'sha256': 'sha256:' + hashlib.sha256(environment).hexdigest(),
        'bytes': len(environment),
    }}]}},
    'error': None,
}}
print(json.dumps(result, separators=(',', ':')))
sys.stdout.flush()
os.fsync(sys.stdout.fileno())
""",
            encoding="utf-8",
        )
        engine.chmod(0o755)
        run = subprocess.run(
            [
                php,
                str(PHP_RUNNER),
                "--binary",
                str(engine),
                "--native-api2",
                "--input",
                input_path.name,
                "--output",
                str(output_path),
                "--artifacts",
                str(artifacts_path),
                "--samples",
                "1",
                "--warmup",
                "0",
                "--cwd",
                str(directory),
                "--fixture-input-sha256",
                input_sha256,
                "--fixture-bundle-sha256",
                bundle.hexdigest(),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert run.returncode == 0, run.stderr
        sample = json.loads(run.stdout)
        assert sample["ok"] is True
        assert sample["correctness"]["pass"] is True
        checks = {check["name"]: check["status"] for check in sample["correctness"]["checks"]}
        assert checks["api2_request_echo"] == "pass"
        assert checks["api2_pdf_descriptor"] == "pass"
        assert checks["api2_scene_descriptor"] == "pass"
        assert checks["api2_bundle_descriptor"] == "pass"
        assert checks["api2_diagnostics_bound"] == "pass"
        assert sample["measurement_method"] == "linux-cgroup-v2-v1"
        assert sample["resource_usage"]["root_start_ticks"] > 0
        assert sample["resource_usage"]["cgroup_drained"] is True
        assert sample["resource_usage"]["launch_security"]["argv"] == [str(engine.resolve()), "render-api2"]
        native_storage = sample["resource_usage"]["launch_security"]["temporary_storage"][
            "native_api2_path_bindings"
        ]
        assert native_storage["pre"] == native_storage["post"]
        bindings = native_storage["pre"]["bindings"]
        sandbox = Path(bindings["sandbox"]["path"])
        assert Path(bindings["job"]["path"]) == sandbox / "job"
        assert Path(bindings["temporary"]["path"]) == sandbox / "temporary"
        assert len(
            {
                (binding["identity"]["device"], binding["identity"]["inode"])
                for binding in bindings.values()
            }
        ) == 4
        assert sample["memory_peak_bytes"] == sample["resource_usage"]["memory_peak_bytes"]
        violations: list[validate_result.Violation] = []
        validate_result.validate_resource_usage(sample, "$.sample", violations)
        assert not violations, "\n".join(map(str, violations))


def acceptance_overhead_proof() -> tuple[dict, dict]:
    pair_count = 20
    seed = 20260808
    rng = random.Random(seed)
    pairs: list[dict] = []
    with workload_engine(["--workload-parent", "32", "1.5"]) as engine:
        command = workload_command(engine)
        direct_wall_ms(command)
        sampled(command)
        for index in range(pair_count):
            order = ["direct", "sampled"]
            rng.shuffle(order)
            direct_ms = 0.0
            measurement: dict = {}
            for mode in order:
                if mode == "direct":
                    direct_ms = direct_wall_ms(command)
                else:
                    measurement = sampled(command)
            sampled_ms = float(measurement["wall_ms"])
            pairs.append(
                {
                    "index": index,
                    "order": order,
                    "direct_wall_ms": direct_ms,
                    "sampled_wall_ms": sampled_ms,
                    "overhead_percent": (sampled_ms - direct_ms) * 100.0 / direct_ms,
                    "sampler_cpu_percent_of_wall": float(measurement["sampler_cpu_percent_of_wall"]),
                }
            )
    overhead = [float(pair["overhead_percent"]) for pair in pairs]
    sampler_cpu = [float(pair["sampler_cpu_percent_of_wall"]) for pair in pairs]
    overhead_p95 = validate_result.percentile(overhead, 95)
    observer_effect = {
        "method": "randomized-paired-wall-v1",
        "seed": seed,
        "pair_count": pair_count,
        "workload_duration_seconds": 1.5,
        "percentile_method": validate_result.PERCENTILE_METHOD,
        "p95_overhead_percent": round(overhead_p95, 3),
        "threshold_percent_exclusive": 2.0,
        "passed": overhead_p95 < 2.0,
        "pairs": pairs,
    }
    sampler_cpu_diagnostic = {
        "method": "sampler-process-cpu-divided-by-engine-wall",
        "percentile_method": validate_result.PERCENTILE_METHOD,
        "p50_percent_of_wall": round(validate_result.percentile(sampler_cpu, 50), 3),
        "p95_percent_of_wall": round(validate_result.percentile(sampler_cpu, 95), 3),
        "raw_percent_of_wall": sampler_cpu,
    }
    return observer_effect, sampler_cpu_diagnostic


def main(live: bool, php_integration: bool, acceptance_overhead: bool) -> None:
    fixture_proofs()
    proof = live_cgroup_proofs() if live or acceptance_overhead else None
    if php_integration:
        php_integration_proof()
    output: dict = {"fixture_proofs": "passed", "live": proof}
    if acceptance_overhead:
        observer_effect, sampler_cpu = acceptance_overhead_proof()
        output["observer_effect"] = observer_effect
        output["sampler_cpu_diagnostic"] = sampler_cpu
        assert observer_effect["passed"], observer_effect
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--php-integration", action="store_true")
    parser.add_argument("--acceptance-overhead", action="store_true")
    parser.add_argument("--workload-child", nargs=2, metavar=("MEBIBYTES", "DURATION"))
    parser.add_argument("--workload-parent", nargs=2, metavar=("MEBIBYTES", "DURATION"))
    parser.add_argument("--workload-leak", metavar="PID_PATH")
    parser.add_argument("--workload-migration-attack", metavar="CGROUP_PARENT")
    args = parser.parse_args()
    if args.workload_child:
        raise SystemExit(child_workload(int(args.workload_child[0]), float(args.workload_child[1])))
    if args.workload_parent:
        raise SystemExit(parent_workload(int(args.workload_parent[0]), float(args.workload_parent[1])))
    if args.workload_leak:
        raise SystemExit(leaking_parent(args.workload_leak))
    if args.workload_migration_attack:
        raise SystemExit(migration_attack(args.workload_migration_attack))
    main(args.live, args.php_integration, args.acceptance_overhead)
