#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
import json
from pathlib import Path

import validate_result


SCHEMA = json.loads(
    (Path(__file__).resolve().parents[1] / "schema" / "benchmark-result.v1.json").read_text(encoding="utf-8")
)
HASH = "0" * 64
PERCENTILES = {key: 1 for key in ("min", "p50", "p95", "p99", "max", "mean")}


def result() -> dict:
    return {
        "schema": "pliego.benchmark-result",
        "version": 1,
        "generated_at": "2026-08-08T00:00:00+00:00",
        "host": {
            "os": "Linux",
            "arch": "x86_64",
            "kernel": "test",
            "cpu_model": "test",
            "cores": 1,
            "ram_bytes": 1,
            "dedicated": True,
        },
        "toolchain": {
            "engine": {
                "name": "pliego",
                "version": "0.1.1",
                "binary_path": "/tmp/pliego",
            },
            "python_version": "3.11",
            "php_version": "8.3",
            "harness_revision": "4f0beb41a4d",
        },
        "protocol": {
            "warmup_iterations": 1,
            "sample_count": 1,
            "sample_order": "sequential",
            "network": "disabled",
            "binary_profile": "checked-release",
            "rss_method": "time-v",
        },
        "target": {"id": "pliego-0.1.1", "label": "Pliego 0.1.1"},
        "fixture": {
            "id": "minimal-static",
            "purpose": "startup",
            "category": "static",
            "input": "input.html",
            "input_sha256": HASH,
            "bundle_sha256": HASH,
        },
        "samples": [
            {
                "index": 0,
                "ok": True,
                "exit_code": 0,
                "wall_ms": 1,
                "user_ms": 1,
                "sys_ms": 0,
                "peak_rss_kib": 1,
                "rss_method": "time-v",
                "phase_timings_ms": {"capture": 1},
                "output": {
                    "pdf_bytes": 1,
                    "pdf_sha256": HASH,
                    "page_count": 1,
                    "artifact_bytes": 0,
                    "published_pdf": True,
                },
                "correctness": {"pass": True, "checks": []},
                "failure": {"code": None, "published_pdf": True},
            }
        ],
        "aggregates": {
            "latency": PERCENTILES,
            "cpu": {
                "user_ms": PERCENTILES,
                "sys_ms": PERCENTILES,
                "wall_ms": PERCENTILES,
            },
            "memory": {"peak_rss_kib": PERCENTILES},
            "output": {"pdf_bytes": PERCENTILES, "page_count": 1},
            "correctness": {"pass_count": 1, "total": 1, "passed": True},
            "determinism": {
                "identical_pdf_sha256": 1,
                "total": 1,
                "pdf_sha256_variants": 1,
            },
            "failures": {"count": 0, "codes": {}},
        },
    }


def errors(value: object) -> list[validate_result.Violation]:
    found: list[validate_result.Violation] = []
    validate_result.validate(value, SCHEMA, "$", found)
    return found


def must_fail(value: object, expected: str) -> None:
    found = errors(value)
    assert found and expected in "\n".join(map(str, found)), found


def main() -> None:
    legacy = result()
    assert not errors(legacy)

    supported = deepcopy(legacy)
    supported["status"] = "supported"
    supported["toolchain"]["engine"] = {
        "name": "dompdf",
        "version": "3.1.0",
        "package": "dompdf/dompdf",
    }
    failed = deepcopy(legacy)
    failed["status"] = "failed"
    failed["samples"][0]["retained"] = {
        "output_dir": "/tmp/pliego-output",
        "artifacts_dir": "/tmp/pliego-artifacts",
    }
    not_applicable = {
        key: deepcopy(legacy[key])
        for key in ("schema", "version", "generated_at", "host", "toolchain", "target", "fixture")
    }
    not_applicable.update(
        status="not-applicable",
        reason="fixture requires JavaScript",
        protocol={
            "warmup_iterations": 0,
            "sample_count": 0,
            "sample_order": "sequential",
            "network": "disabled",
            "binary_profile": "package",
            "rss_method": "unavailable",
        },
    )
    bundle = {
        "schema": "pliego.benchmark-bundle",
        "version": 1,
        "generated_at": legacy["generated_at"],
        "results": [legacy, supported, failed, not_applicable],
    }
    assert not errors(bundle)

    must_fail([legacy], "oneOf")
    for field in ("rss_method", "output", "correctness", "failure"):
        broken = deepcopy(legacy)
        del broken["samples"][0][field]
        must_fail(broken, field)
    broken = deepcopy(legacy)
    broken["samples"][0]["phase_timings_ms"]["capture"] = "fast"
    must_fail(broken, "phase_timings_ms.capture")
    broken = deepcopy(legacy)
    broken["toolchain"]["competitors"] = {"dompdf": 3}
    must_fail(broken, "competitors.dompdf")
    broken = deepcopy(legacy)
    broken["aggregates"]["failures"]["codes"]["TIMEOUT"] = 0
    must_fail(broken, "minimum")
    broken = deepcopy(legacy)
    broken["fixture"]["input_sha256"] = "unknown"
    must_fail(broken, "pattern")
    broken = deepcopy(not_applicable)
    broken["samples"] = []
    must_fail(broken, "samples")
    print("benchmark result validator self-check passed")


if __name__ == "__main__":
    main()
