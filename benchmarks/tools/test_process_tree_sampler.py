#!/usr/bin/env python3

"""Fixture, live cgroup-v2, observer, and PHP bridge checks."""

from __future__ import annotations

import argparse
import contextlib
import errno
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
    assert "GITHUB_TOKEN" not in child_environment and child_environment["LANG"] == "C"

    with tempfile.TemporaryDirectory() as raw:
        executable = Path(raw) / "true"
        shutil.copy2(shutil.which("true") or "/bin/true", executable)
        executable.chmod(0o555)
        must_be_incomplete(
            "ENGINE_COMMAND_INVALID",
            lambda: process_tree_sampler.command_identity([str(executable.resolve())], fixture_account),
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
def workload_engine() -> Iterator[str]:
    with tempfile.TemporaryDirectory() as raw:
        directory = Path(raw)
        directory.chmod(0o755)
        engine = directory / "fixture-engine.py"
        engine.write_text(
            "#!/usr/bin/env python3\n"
            "import os, sys\n"
            f"os.execv({str(Path(sys.executable).resolve())!r}, "
            f"[{str(Path(sys.executable).resolve())!r}, {str(Path(__file__).resolve())!r}, *sys.argv[3:]])\n",
            encoding="utf-8",
        )
        engine.chmod(0o555)
        yield str(engine.resolve())


def workload_command(engine: str, mebibytes: int = 32, duration: float = 0.8) -> list[str]:
    return [engine, "render", "fixture.html", "--workload-parent", str(mebibytes), str(duration)]


def sampled(command: list[str], descendant_grace_ms: float = 1000.0) -> dict:
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
    sample = {
        "ok": ok,
        "exit_code": proof["exit_code"],
        "signal": proof["signal"],
        "wall_ms": proof["wall_ms"],
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
        attacked = sampled([engine, "render", "fixture.html", "--workload-migration-attack", parent])
        assert attacked["exit_code"] == 0
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
    assert all(int(launch["status"][field], 16) == 0 for field in (
        "cap_inheritable_hex", "cap_permitted_hex", "cap_effective_hex", "cap_bounding_hex", "cap_ambient_hex"
    ))
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
        input_path = directory / "input.html"
        output_path = directory / "output.pdf"
        artifacts_path = directory / "ignored-artifacts"
        engine = directory / "fake-engine.py"
        input_path.write_text("<p>fixture</p>", encoding="utf-8")
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
        run = subprocess.run(
            [
                php,
                str(PHP_RUNNER),
                "--binary",
                str(engine),
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
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert run.returncode == 0, run.stderr
        sample = json.loads(run.stdout)
        assert sample["measurement_method"] == "linux-cgroup-v2-v1"
        assert sample["resource_usage"]["root_start_ticks"] > 0
        assert sample["resource_usage"]["cgroup_drained"] is True
        assert sample["memory_peak_bytes"] == sample["resource_usage"]["memory_peak_bytes"]
        violations: list[validate_result.Violation] = []
        validate_result.validate_resource_usage(sample, "$.sample", violations)
        assert not violations, "\n".join(map(str, violations))


def acceptance_overhead_proof() -> tuple[dict, dict]:
    pair_count = 20
    seed = 20260808
    rng = random.Random(seed)
    pairs: list[dict] = []
    with workload_engine() as engine:
        command = workload_command(engine, duration=1.5)
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
