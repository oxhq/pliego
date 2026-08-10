#!/usr/bin/env python3

"""Adversarial self-checks for the retained observer A/B publication proof."""

from __future__ import annotations

import json
import random
import tempfile
from copy import deepcopy
from pathlib import Path
from unittest import mock

import observer_ab
import test_validate_result

SCHEMA = json.loads(observer_ab.DEFAULT_SCHEMA.read_text(encoding="utf-8"))


def raw_proof(index: int, observation_enabled: bool) -> dict:
    proof = test_validate_result.resource_usage()
    mode = 1 if observation_enabled else 0
    proof["tree_wall_ms"] = 1000.0 + 5.0 * mode
    proof["sampler_cpu_user_ms"] = float(mode)
    proof["sampler_cpu_sys_ms"] = 0.0
    proof["cgroup_path"] = f"/sys/fs/cgroup/pliego/pliego-render-{1000 + index * 2 + mode}-{index:015x}{mode}"
    proof["root_pid"] = 1000 + index * 2 + mode
    proof["root_start_ticks"] = 10000 + index * 2 + mode
    proof["sampled_diagnostics"]["observation_enabled"] = observation_enabled
    identity = proof["launch_security"]["command_identity"]
    identity["output_directory"] = {
        "path": f"/tmp/pair-{index}-{mode}-output",
        "device": 1,
        "inode": 100 + index * 4 + mode * 2,
    }
    identity["artifact_directory"] = {
        "path": f"/tmp/pair-{index}-{mode}-artifacts",
        "device": 1,
        "inode": 101 + index * 4 + mode * 2,
    }
    return proof


def measurements() -> dict:
    rng = random.Random(observer_ab.SEED)
    pairs = []
    for index in range(observer_ab.PAIR_COUNT):
        order = ["observation-off", "observation-on"]
        rng.shuffle(order)
        pairs.append(
            observer_ab.measurement_pair(
                index,
                order,
                raw_proof(index, False),
                raw_proof(index, True),
            )
        )
    return observer_ab.build_measurements(pairs, duration_seconds=1.5)


def set_path(value: dict, path: tuple[str, ...], replacement: object) -> None:
    cursor = value
    for part in path[:-1]:
        cursor = cursor[part]
    cursor[path[-1]] = replacement


def main() -> None:
    valid = measurements()
    assert observer_ab.validate_measurements(valid) == []
    normalized_mutations = [
        (("cgroup_boundary", "method"), "linux-cgroup-v2-v1"),
        (("cgroup_boundary", "scope"), "attacker-scope"),
        (("cgroup_boundary", "delegated_parent"), "/sys/fs/cgroup/attacker"),
        (("launch_security", "account"), "root"),
        (("launch_security", "uid"), 992),
        (("launch_security", "gid"), 992),
        (("launch_security", "execution_binding"), "path-exec"),
        (("launch_security", "parent_death_signal"), 0),
        (("launch_security", "network_namespace_isolated"), False),
        (("launch_security", "executable", "path"), "/tmp/replaced"),
        (("launch_security", "executable", "sha256"), "1" * 64),
        (("launch_security", "executable", "device"), 2),
        (("launch_security", "executable", "inode"), 3),
        (("launch_security", "executable", "bytes"), 2),
        (("frozen_command", "cwd", "path"), "/attacker"),
        (("frozen_command", "cwd", "device"), 2),
        (("frozen_command", "cwd", "inode"), 4),
        (("frozen_command", "input", "manifest_path"), "/attacker/input.html"),
        (("frozen_command", "input", "argv_relative_path"), "other.html"),
        (("frozen_command", "input", "sha256"), "1" * 64),
        (("frozen_command", "input", "device"), 2),
        (("frozen_command", "input", "inode"), 5),
        (("static_controls", "cleanup_kill_used"), True),
        (("static_controls", "cleanup_kill_grace_ms"), 0),
        (("static_controls", "accounting_poll_interval_ms"), 0),
        (("static_controls", "empty_cgroup_events"), {"populated": 1, "frozen": 0}),
        (("static_controls", "after_join_cgroup_events"), {"populated": 0, "frozen": 0}),
        (("static_controls", "final_cgroup_events"), {"populated": 1, "frozen": 0}),
    ]
    for name in (
        "uid",
        "gid",
        "supplementary_groups",
        "cap_inheritable_hex",
        "cap_permitted_hex",
        "cap_effective_hex",
        "cap_bounding_hex",
        "cap_ambient_hex",
        "no_new_privs",
    ):
        replacement: object = [0, 0, 0, 0] if name in {"uid", "gid"} else ([1] if name == "supplementary_groups" else 0)
        if name.startswith("cap_"):
            replacement = "0000000000000001"
        normalized_mutations.append((("launch_security", "status", name), replacement))
    for probe in ("migration_write_probes", "migration_fd_probes"):
        for boundary in ("parent", "harness", "staging", "measurement"):
            for interface in ("cgroup.procs", "cgroup.threads"):
                normalized_mutations.append((("launch_security", probe, boundary, interface), "WRITABLE"))
    for path, replacement in normalized_mutations:
        broken = deepcopy(valid)
        set_path(broken["pairs"][0]["normalized_on"], path, replacement)
        assert observer_ab.validate_measurements(broken), path

    malicious = deepcopy(valid)
    for side in ("normalized_off", "normalized_on"):
        malicious["pairs"][0][side]["launch_security"]["status"]["cap_effective_hex"] = "0000000000000001"
    assert any("cap_effective_hex" in violation for violation in observer_ab.validate_measurements(malicious))
    threshold = deepcopy(valid)
    for pair in threshold["pairs"]:
        pair["observation_on"]["tree_wall_ms"] = 1020.0
        pair["overhead_percent"] = 2.0
    threshold["p95_overhead_percent"] = 2.0
    threshold["passed"] = False
    assert any("below 2 percent" in violation for violation in observer_ab.validate_measurements(threshold))

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        source = root / "source-broker"
        installed = root / "installed-broker"
        source.write_bytes(b"broker")
        installed.write_bytes(b"broker")
        measurement_path = (root / "measurements.json").resolve()
        measurement_path.write_text(json.dumps(valid, indent=2) + "\n", encoding="utf-8")
        loaded, raw_binding = observer_ab.load_bound_object(measurement_path, "observer measurements")
        file_binding = observer_ab.finalize_measurements_binding(loaded, raw_binding)
        host = {
            "identity": {"sha": "a" * 40},
            "host_config": {"sha256": "b" * 64},
            "runner": {"id": 7, "name": "pliego-pinned-01"},
            "command": {"observer_measurements": file_binding},
        }
        digest = "c" * 64
        binding = observer_ab.expected_binding(host, digest, source, installed)
        document = {
            "schema": "pliego.benchmark-observer-proof",
            "version": 1,
            "status": "accepted",
            "generated_at": "2026-08-09T00:00:00+00:00",
            "binding": binding,
            "measurements_binding": file_binding,
            "measurements": valid,
        }
        with mock.patch.object(observer_ab, "load_host_proof", return_value=(host, digest)):
            built = observer_ab.build_proof(measurement_path, root / "proof.json", source, installed)
            assert built["measurements_binding"] == file_binding
            replaced = deepcopy(valid)
            replaced["generated_at"] = "2026-08-09T01:00:00+00:00"
            measurement_path.write_text(json.dumps(replaced, indent=2) + "\n", encoding="utf-8")
            try:
                observer_ab.build_proof(measurement_path, root / "proof.json", source, installed)
            except observer_ab.benchmark_publication.PublicationError as error:
                assert "host-bound digest" in str(error)
            else:
                raise AssertionError("observer measurement replacement after host proof was accepted")
            assert observer_ab.validate_proof(document, SCHEMA, root / "proof.json", source, installed) == []
            for field in binding:
                broken = deepcopy(document)
                broken["binding"][field] = (
                    8 if field == "runner_id" else "d" * (40 if field == "harness_revision" else 64)
                )
                if field == "runner_name":
                    broken["binding"][field] = "invented-runner"
                assert any(
                    "binding differs" in violation
                    for violation in observer_ab.validate_proof(broken, SCHEMA, root / "proof.json", source, installed)
                ), field
        installed.write_bytes(b"replaced broker")
        try:
            observer_ab.expected_binding(host, digest, source, installed)
        except observer_ab.benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("source/installed broker replacement was accepted")

    print("observer A/B adversarial checks passed")


if __name__ == "__main__":
    main()
