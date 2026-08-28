#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
import json
from pathlib import Path
import tempfile
import tomllib
from typing import Any

import comparison_metrics
import run_benchmark
import run_comparison
from test_comparison_metrics import passing_sample
from test_validate_result import resource_usage


TARGETS = sorted(run_comparison.TARGET_IDS)


def target_identities() -> list[dict[str, Any]]:
    manifest = tomllib.loads(run_comparison.MANIFEST.read_text(encoding="utf-8"))
    identities: list[dict[str, Any]] = []
    for target_id in TARGETS:
        target = manifest["targets"][target_id]
        if target["kind"] == "pliego":
            engine = {
                "name": "pliego",
                "version": target["version"],
                "binary_path": "/opt/pliego-benchmark/pliego",
                "binary_sha256": target["binary_sha256"],
                "binary_bytes": target["binary_bytes"],
                "commit": target["commit"],
                "release_tag": target["release_tag"],
                "servo_build": target["servo_build"],
                "servo_base": target["servo_base"],
                "bundle": target["archive"],
                "bundle_sha256": target["archive_sha256"],
                "bundle_bytes": target["archive_bytes"],
                "profile": target["profile"],
            }
            runtime = run_comparison.pliego_runtime_identity(engine)
        else:
            adapter = (run_comparison.ROOT / target["adapter"]).resolve()
            adapter_path = f"/workspace/{target['adapter']}"
            engine = {
                "name": target["package"].split("/")[-1],
                "version": target["version"],
                "package": target["package"],
                "binary_path": adapter_path,
                "binary_sha256": run_benchmark.file_sha256(adapter),
                "binary_bytes": adapter.stat().st_size,
                "profile": target["profile"],
            }
            runtime = {
                "contract": "pliego.benchmark-adapter.v1",
                "target": target_id,
                "package": target["package"],
                "package_version": target["version"],
                "adapter_path": adapter_path,
                "adapter_sha256": engine["binary_sha256"],
                "composer_lock_sha256": run_benchmark.file_sha256(
                    (run_comparison.ROOT / target["lockfiles"][0]).resolve()
                ),
                "composer_vendor_path": f"/workspace/{Path(target['adapter']).parent.as_posix()}/vendor",
                "composer_vendor_sha256": "5" * 64,
                "php_path": "/usr/bin/php8.3",
                "php_sha256": "6" * 64,
                "php_version": "8.3.0",
            }
            if target.get("requires_network_isolation"):
                runtime.update(
                    {
                        "puppeteer_version": "25.8.0",
                        "package_lock_sha256": run_benchmark.file_sha256(
                            (run_comparison.ROOT / target["lockfiles"][1]).resolve()
                        ),
                        "node_modules_path": "/workspace/benchmarks/adapters/browsershot/node_modules",
                        "node_modules_sha256": "7" * 64,
                        "node_path": "/opt/hostedtoolcache/node/bin/node",
                        "node_sha256": "8" * 64,
                        "node_version": "v24.0.0",
                        "chrome_path": "/usr/bin/google-chrome",
                        "chrome_sha256": "9" * 64,
                        "chrome_version": "Google Chrome 140",
                    }
                )
        identities.append(
            run_comparison.seal_target_identity(
                {"id": target_id, "label": target["label"], "engine": engine, "runtime": runtime}
            )
        )
    return identities


IDENTITIES = target_identities()
IDENTITY_BY_ID = {identity["id"]: identity for identity in IDENTITIES}


def measured_sample(target_id: str, index: int) -> dict[str, Any]:
    sample = passing_sample(target_id, index)
    usage = resource_usage()
    usage["launch_security"]["network_isolation"] = {
        "mode": "linux-private-network-namespace-v1",
        "host_namespace": "net:[100]",
        "engine_namespace": f"net:[{index + 200}]",
        "interfaces": ["lo"],
    }
    identity = IDENTITY_BY_ID[target_id]
    usage["launch_security"]["argv"] = [identity["engine"]["binary_path"], "render"]
    usage["launch_security"]["executable"]["path"] = identity["engine"]["binary_path"]
    usage["launch_security"]["executable"]["sha256"] = identity["engine"]["binary_sha256"]
    sample.update(
        {
            "measurement_method": "linux-cgroup-v2-v1",
            "resource_usage": usage,
            "wall_ms": usage["wall_ms"],
            "user_ms": usage["cpu_user_ms"],
            "sys_ms": usage["cpu_sys_ms"],
            "memory_current_bytes": usage["memory_current_bytes"],
            "memory_peak_bytes": usage["memory_peak_bytes"],
            "sampled_peak_rss_kib_lower_bound": usage["sampled_diagnostics"]["sampled_peak_summed_rss_kib_lower_bound"],
            "sampled_peak_pss_kib_lower_bound": usage["sampled_diagnostics"]["sampled_peak_summed_pss_kib_lower_bound"],
            "read_bytes": usage["read_bytes"],
            "write_bytes": usage["write_bytes"],
            "read_operations": usage["read_operations"],
            "write_operations": usage["write_operations"],
        }
    )
    return sample


def artifact() -> dict[str, Any]:
    return run_benchmark.execute_interleaved_run(
        TARGETS,
        "minimal-static",
        10,
        100,
        1,
        lambda _target: None,
        lambda _target, _iteration: None,
        measured_sample,
    )


def oracle() -> dict[str, str]:
    identity = {
        "contract": "pliego.pdf-oracle.v1",
        "oracle_path": "/workspace/benchmarks/tools/pdf_oracle.py",
        "oracle_sha256": run_benchmark.file_sha256(run_benchmark.PDF_ORACLE),
    }
    for tool in ("pdfinfo", "pdftotext", "pdffonts", "pdftoppm"):
        identity[f"{tool}_path"] = f"/usr/bin/{tool}"
        identity[f"{tool}_sha256"] = "1" * 64
        identity[f"{tool}_version"] = f"{tool} 1.0"
    return identity


def comparison(source: dict[str, Any]) -> dict[str, Any]:
    targets = source["schedule"]["targets"]
    data: dict[str, Any] = {
        "schema": "pliego.benchmark-hosted-comparison",
        "version": 1,
        "evidence_class": "github-hosted-exploratory",
        "publication_status": "hosted-snapshot",
        "claim_boundary": run_comparison.CLAIM_BOUNDARY,
        "comparison_sha256": "",
        "generated_at": "2026-08-28T00:00:00+00:00",
        "source": {
            "repository": "oxhq/pliego",
            "revision": "2" * 40,
            "ref": "refs/heads/main",
            "event_name": "workflow_dispatch",
            "workflow": "Pliego hosted performance snapshot",
            "workflow_ref": "oxhq/pliego/.github/workflows/pliego-performance.yml@refs/heads/main",
            "job": "hosted-snapshot",
            "run_id": 123,
            "run_attempt": 1,
            "repeat": 1,
            "run_url": "https://github.com/oxhq/pliego/actions/runs/123/attempts/1",
        },
        "runner": {
            "name": "GitHub Actions 1",
            "os": "Linux",
            "arch": "X64",
            "environment": "github-hosted",
            "image_os": "ubuntu24",
            "image_version": "20260828.1",
        },
        "host": {
            "os": "Linux",
            "arch": "x86_64",
            "kernel": "6.11",
            "cpu_model": "synthetic",
            "cores": 4,
            "ram_bytes": 16 * 1024 * 1024 * 1024,
            "dedicated": False,
            "uname": "Linux synthetic",
            "os_release": "Ubuntu",
            "lscpu_json": "{}",
            "virtualization": "kvm",
            "cgroup": {},
        },
        "ambient": {"before": {}, "after": {}},
        "protocol": {
            "fixture_id": "minimal-static",
            "targets": targets,
            "correctness_preflight_iterations": 1,
            "warmup_iterations": 10,
            "sample_count": 100,
            "seed": 1,
            "schedule_contract": "pliego.cross-target-schedule.v1",
            "sample_order": "globally-interleaved-seeded",
            "network": "private-loopback-only",
            "measurement_method": "linux-cgroup-v2-v1",
            "percentile_method": "nearest-rank-v1",
        },
        "fixture": {
            "id": "minimal-static",
            "input": "benchmarks/fixtures/minimal-static/comparator.html",
            "input_sha256": "3" * 64,
            "bundle_sha256": "4" * 64,
        },
        "oracle": oracle(),
        "targets": [deepcopy(IDENTITY_BY_ID[target_id]) for target_id in targets],
        "interleaved_artifact": {
            "file": "interleaved-run.v1.json",
            "artifact_sha256": source["artifact_sha256"],
            "schedule_sha256": run_benchmark.canonical_json_sha256(source["schedule"]),
        },
        "aggregates": comparison_metrics.aggregate_interleaved_artifact(source),
        "evidence_binding": {
            "contract": "pliego.hosted-comparison-evidence-binding.v1",
            "sha256": "",
        },
    }
    data["evidence_binding"]["sha256"] = run_comparison.evidence_binding_sha256(data)
    data["comparison_sha256"] = run_comparison.comparison_sha256(data)
    return data


def reseal(source: dict[str, Any]) -> None:
    source["raw_samples"] = [
        run_benchmark.raw_sample_record(source["schedule"]["fixture_id"], entry, record["sample"])
        for entry, record in zip(source["schedule"]["timed"], source["raw_samples"], strict=True)
    ]
    source["artifact_sha256"] = run_benchmark.interleaved_artifact_sha256(source)


def verified_release(data: dict[str, Any]) -> dict[str, Any]:
    engine = next(target for target in data["targets"] if target["id"] == "pliego-0.3.2")["engine"]
    return {
        "schema": "pliego.verified-release",
        "version": 1,
        "target": "pliego-0.3.2",
        "release_tag": engine["release_tag"],
        "commit": engine["commit"],
        "servo_build": engine["servo_build"],
        "servo_base": engine["servo_base"],
        "platform": "linux-x86_64",
        "profile": engine["profile"],
        "runtime_manifest": "benchmarks/releases/v0.3.2/runtimes.json",
        "runtime_manifest_bytes": 3290,
        "runtime_manifest_sha256": "6d48a02bf8b60c3e947a8fd3784593f8a841e79694ba82eb36835532588ab2d9",
        "archive": "/cache/pliego-0.3.2-linux-x86_64.tar.gz",
        "binary": "/cache/pliego-0.3.2-linux-x86_64/pliego",
        "archive_sha256": engine["bundle_sha256"],
        "archive_bytes": engine["bundle_bytes"],
        "binary_sha256": engine["binary_sha256"],
        "binary_bytes": engine["binary_bytes"],
    }


def write_bundle(directory: Path, source: dict[str, Any], data: dict[str, Any]) -> None:
    files = {
        "interleaved-run.v1.json": json.dumps(source, indent=2, sort_keys=True) + "\n",
        "hosted-comparison.v1.json": json.dumps(data, indent=2, sort_keys=True) + "\n",
        "all-metrics.md": run_comparison.render_markdown(data),
        "verified-release.json": json.dumps(verified_release(data), indent=2) + "\n",
    }
    for name, contents in files.items():
        (directory / name).write_text(contents, encoding="utf-8")
    run_comparison.write_checksums(directory, [directory / name for name in files])


def main() -> None:
    source = artifact()
    data = comparison(source)
    assert not run_comparison.validate_comparison(data, source)
    markdown = run_comparison.render_markdown(data)
    assert "github-hosted-exploratory" in markdown
    assert "Wall p99" in markdown
    assert "Full aggregate table" in markdown
    assert "`sampled_peak_pss_kib_lower_bound`" in markdown
    assert "not dedicated-host production claims" in markdown

    changed_claim = deepcopy(data)
    changed_claim["claim_boundary"] = "Authoritative production ranking."
    changed_claim["comparison_sha256"] = run_comparison.comparison_sha256(changed_claim)
    errors = "\n".join(map(str, run_comparison.validate_comparison(changed_claim, source)))
    assert "claim_boundary" in errors and "expected const" in errors

    changed_oracle = deepcopy(data)
    changed_oracle["oracle"]["oracle_sha256"] = "a" * 64
    changed_oracle["evidence_binding"]["sha256"] = run_comparison.evidence_binding_sha256(changed_oracle)
    changed_oracle["comparison_sha256"] = run_comparison.comparison_sha256(changed_oracle)
    errors = "\n".join(map(str, run_comparison.validate_comparison(changed_oracle, source)))
    assert "oracle.oracle_sha256" in errors

    authoritative = deepcopy(data)
    authoritative["host"]["dedicated"] = True
    authoritative["comparison_sha256"] = run_comparison.comparison_sha256(authoritative)
    errors = "\n".join(map(str, run_comparison.validate_comparison(authoritative, source)))
    assert "host.dedicated" in errors and "expected const False" in errors

    changed_aggregate = deepcopy(data)
    changed_aggregate["aggregates"]["targets"][0]["metrics"]["wall_ms"]["statistics"]["p50"] = 999
    changed_aggregate["comparison_sha256"] = run_comparison.comparison_sha256(changed_aggregate)
    errors = "\n".join(map(str, run_comparison.validate_comparison(changed_aggregate, source)))
    assert "aggregates" in errors

    changed_identity = deepcopy(data)
    changed_identity["targets"][0]["engine"]["version"] = "999.0.0"
    changed_identity["targets"][0]["identity_sha256"] = run_comparison.target_identity_sha256(
        changed_identity["targets"][0]
    )
    changed_identity["evidence_binding"]["sha256"] = run_comparison.evidence_binding_sha256(changed_identity)
    changed_identity["comparison_sha256"] = run_comparison.comparison_sha256(changed_identity)
    errors = "\n".join(map(str, run_comparison.validate_comparison(changed_identity, source)))
    assert "engine.version" in errors

    network_escape = deepcopy(source)
    network_escape["raw_samples"][0]["sample"]["resource_usage"]["launch_security"].pop("network_isolation")
    reseal(network_escape)
    escaped_data = comparison(network_escape)
    errors = "\n".join(map(str, run_comparison.validate_comparison(escaped_data, network_escape)))
    assert "lacks private-network proof" in errors

    executable_swap = deepcopy(source)
    executable_swap["raw_samples"][0]["sample"]["resource_usage"]["launch_security"]["executable"]["sha256"] = "a" * 64
    reseal(executable_swap)
    errors = "\n".join(map(str, run_comparison.validate_comparison(data, executable_swap)))
    assert "launch_security.executable.sha256" in errors

    wall_mismatch = deepcopy(source)
    wall_mismatch["raw_samples"][0]["sample"]["wall_ms"] += 1
    reseal(wall_mismatch)
    errors = "\n".join(map(str, run_comparison.validate_comparison(data, wall_mismatch)))
    assert ".wall_ms" in errors and "resource-usage validation failed" in errors

    cpu_counter_mismatch = deepcopy(source)
    cpu_counter_mismatch["raw_samples"][0]["sample"]["resource_usage"]["counters"]["final"]["cpu_stat"][
        "user_usec"
    ] += 1000
    reseal(cpu_counter_mismatch)
    errors = "\n".join(map(str, run_comparison.validate_comparison(data, cpu_counter_mismatch)))
    assert "cpu" in errors and "resource-usage validation failed" in errors

    io_counter_mismatch = deepcopy(source)
    io_counter_mismatch["raw_samples"][0]["sample"]["resource_usage"]["counters"]["final"]["io_stat"]["8:0"][
        "wbytes"
    ] += 1
    reseal(io_counter_mismatch)
    errors = "\n".join(map(str, run_comparison.validate_comparison(data, io_counter_mismatch)))
    assert "write_bytes" in errors or "io_stat" in errors

    wrong_artifact = deepcopy(data)
    wrong_artifact["interleaved_artifact"]["artifact_sha256"] = "f" * 64
    wrong_artifact["comparison_sha256"] = run_comparison.comparison_sha256(wrong_artifact)
    errors = "\n".join(map(str, run_comparison.validate_comparison(wrong_artifact, source)))
    assert "interleaved_artifact.artifact_sha256" in errors

    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        write_bundle(directory, source, data)
        assert not run_comparison.validate_bundle(directory)

        (directory / "all-metrics.md").write_text("tampered\n", encoding="utf-8")
        run_comparison.write_checksums(
            directory,
            [directory / name for name in run_comparison.REQUIRED_BUNDLE_FILES if name != "SHA256SUMS"],
        )
        errors = "\n".join(map(str, run_comparison.validate_bundle(directory)))
        assert "$bundle.all-metrics.md" in errors

    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        write_bundle(directory, source, data)
        (directory / "verified-release.json").unlink()
        errors = "\n".join(map(str, run_comparison.validate_bundle(directory)))
        assert "$bundle.files" in errors


if __name__ == "__main__":
    main()
