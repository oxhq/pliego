#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Dependency-free JSON Schema validation for benchmark results.

Implements the subset of draft-07 used by `schema/benchmark-result.v1.json`
(no `jsonschema` required): type (including null unions), const, enum, oneOf,
required, properties, additionalProperties, minimum, minItems, $ref
resolution into `definitions`, and pattern.

Usage:
    python3 benchmarks/tools/validate_result.py <result.json> [<schema.json>]
"""

from __future__ import annotations

import json
import math
import re
import statistics
import sys
from collections import Counter
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SCHEMA = ROOT / "benchmarks" / "schema" / "benchmark-result.v1.json"


class Violation:
    def __init__(self, path: str, message: str) -> None:
        self.path = path
        self.message = message

    def __str__(self) -> str:
        return f"{self.path}: {self.message}"


def percentile(values: list[int | float], p: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, round(p / 100.0 * (len(ordered) - 1))))
    return float(ordered[index])


def percentiles(values: list[int | float | None]) -> dict[str, float]:
    measured = [value for value in values if value is not None]
    if not measured:
        return {key: 0.0 for key in ("min", "p50", "p95", "p99", "max", "mean")}
    return {
        "min": min(measured),
        "p50": percentile(measured, 50),
        "p95": percentile(measured, 95),
        "p99": percentile(measured, 99),
        "max": max(measured),
        "mean": statistics.fmean(measured),
    }


def resolve(schema: dict[str, Any], ref: str) -> dict[str, Any]:
    if not ref.startswith("#/definitions/"):
        raise ValueError(f"unsupported $ref: {ref}")
    definitions = schema.get("definitions", {})
    name = ref[len("#/definitions/") :]
    if name not in definitions:
        raise ValueError(f"unknown definition in $ref: {ref}")
    return definitions[name]


def type_matches(value: Any, expected: str) -> bool:
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if expected == "string":
        return isinstance(value, str)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "null":
        return value is None
    return True


def validate(
    data: Any,
    schema: dict[str, Any],
    path: str,
    violations: list[Violation],
    root: dict[str, Any] | None = None,
) -> None:
    if root is None:
        root = schema
    if "$ref" in schema:
        validate(data, resolve(root, schema["$ref"]), path, violations, root)
        return

    if "oneOf" in schema:
        branch_violations: list[list[Violation]] = []
        for branch in schema["oneOf"]:
            failures: list[Violation] = []
            validate(data, branch, path, failures, root)
            branch_violations.append(failures)
        matches = sum(not failures for failures in branch_violations)
        if matches != 1:
            closest = min(branch_violations, key=len)
            detail = f"; closest: {closest[0]}" if closest else ""
            violations.append(Violation(path, f"expected exactly one oneOf match, got {matches}{detail}"))
        return

    if "type" in schema:
        types = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(type_matches(data, t) for t in types):
            violations.append(Violation(path, f"expected type {types!r}, got {type(data).__name__}"))
            return

    if "const" in schema and data != schema["const"]:
        violations.append(Violation(path, f"expected const {schema['const']!r}, got {data!r}"))

    if "enum" in schema and data not in schema["enum"]:
        violations.append(Violation(path, f"expected one of {schema['enum']!r}, got {data!r}"))

    if "pattern" in schema and isinstance(data, str):
        if re.fullmatch(schema["pattern"], data) is None:
            violations.append(Violation(path, f"does not match pattern {schema['pattern']!r}"))

    if isinstance(data, (int, float)) and not isinstance(data, bool):
        if "minimum" in schema and data < schema["minimum"]:
            violations.append(Violation(path, f"{data} < minimum {schema['minimum']}"))

    if isinstance(data, list):
        if "minItems" in schema and len(data) < schema["minItems"]:
            violations.append(Violation(path, f"expected >= {schema['minItems']} items, got {len(data)}"))
        if "items" in schema and isinstance(schema["items"], dict):
            for index, item in enumerate(data):
                validate(item, schema["items"], f"{path}[{index}]", violations, root)

    if isinstance(data, dict):
        properties = schema.get("properties", {})
        for required in schema.get("required", []):
            if required not in data:
                violations.append(Violation(path, f"missing required property {required!r}"))
        for key, value in data.items():
            child = f"{path}.{key}"
            if key in properties:
                validate(value, properties[key], child, violations, root)
            elif schema.get("additionalProperties") is False:
                violations.append(Violation(path, f"unexpected property {key!r}"))
            elif isinstance(schema.get("additionalProperties"), dict):
                validate(value, schema["additionalProperties"], child, violations, root)


def validate_semantics(data: dict[str, Any], path: str, violations: list[Violation]) -> None:
    if data["schema"] == "pliego.benchmark-bundle":
        for index, result in enumerate(data["results"]):
            validate_semantics(result, f"{path}.results[{index}]", violations)
        return
    if data.get("status") == "not-applicable":
        return

    samples = data["samples"]
    if data["protocol"]["sample_count"] != len(samples):
        violations.append(
            Violation(
                f"{path}.protocol.sample_count",
                f"must equal the {len(samples)} retained samples",
            )
        )

    indices = [sample["index"] for sample in samples]
    if indices != list(range(len(samples))):
        violations.append(Violation(f"{path}.samples", "sample indices must be contiguous and ordered from zero"))

    protocol_rss_method = data["protocol"]["rss_method"]
    for index, sample in enumerate(samples):
        sample_path = f"{path}.samples[{index}]"
        if sample["rss_method"] != protocol_rss_method:
            violations.append(
                Violation(
                    f"{sample_path}.rss_method",
                    f"must equal protocol rss_method {protocol_rss_method!r}",
                )
            )
        correctness = sample["correctness"]
        output = sample["output"]
        failure = sample["failure"]
        expected_pass = bool(correctness["checks"]) and all(
            check["status"] == "pass" for check in correctness["checks"]
        )
        if correctness["pass"] != expected_pass:
            violations.append(
                Violation(
                    f"{sample_path}.correctness.pass",
                    "must be true exactly when no correctness check failed",
                )
            )
        if sample["ok"] != correctness["pass"]:
            violations.append(Violation(f"{sample_path}.ok", "must equal correctness.pass"))
        bridge_timings = sample.get("bridge_timings")
        if bridge_timings is not None:
            phase_total = math.fsum(value for value in bridge_timings["phases_ms"].values() if value is not None)
            if abs(phase_total - bridge_timings["total_ms"]) >= 0.02:
                violations.append(
                    Violation(
                        f"{sample_path}.bridge_timings.total_ms",
                        f"must equal the additive phase total {phase_total!r}",
                    )
                )
            native_engine = bridge_timings["native_engine_ms"]
            bridge_overhead = bridge_timings["bridge_overhead_ms"]
            if (native_engine is None) != (bridge_overhead is None):
                violations.append(
                    Violation(
                        f"{sample_path}.bridge_timings.bridge_overhead_ms",
                        "must be null exactly when native_engine_ms is null",
                    )
                )
            elif (
                native_engine is not None and abs(native_engine + bridge_overhead - bridge_timings["total_ms"]) >= 0.002
            ):
                violations.append(
                    Violation(
                        f"{sample_path}.bridge_timings.bridge_overhead_ms",
                        "must reconcile native_engine_ms to total_ms",
                    )
                )
        if output["published_pdf"] != failure["published_pdf"]:
            violations.append(
                Violation(
                    f"{sample_path}.failure.published_pdf",
                    "must equal output.published_pdf",
                )
            )
        if output["published_pdf"]:
            if output["pdf_bytes"] is None or output["pdf_bytes"] <= 0:
                violations.append(Violation(f"{sample_path}.output.pdf_bytes", "must be positive for a published PDF"))
            for field in ("pdf_sha256", "page_count"):
                if output[field] is None:
                    violations.append(Violation(f"{sample_path}.output.{field}", "must be present for a published PDF"))
        else:
            for field in ("pdf_bytes", "pdf_sha256", "page_count"):
                if output[field] is not None:
                    violations.append(
                        Violation(f"{sample_path}.output.{field}", "must be null when no PDF was published")
                    )

        if not sample["ok"] and "retained" not in sample:
            violations.append(Violation(sample_path, "failed samples must retain output and artifacts"))
        if sample["ok"]:
            expected_failure_code = data["fixture"].get("expected_failure_code")
            if expected_failure_code is None:
                if sample["exit_code"] != 0:
                    violations.append(Violation(f"{sample_path}.exit_code", "passing normal fixtures must exit zero"))
                if not output["published_pdf"]:
                    violations.append(
                        Violation(
                            f"{sample_path}.output.published_pdf",
                            "passing normal fixtures must publish a PDF",
                        )
                    )
                if failure["code"] is not None:
                    violations.append(
                        Violation(f"{sample_path}.failure.code", "passing normal fixtures cannot report a failure")
                    )
                expected_page_count = data["fixture"].get("expected_page_count")
                if expected_page_count is not None and output["page_count"] != expected_page_count:
                    violations.append(
                        Violation(
                            f"{sample_path}.output.page_count",
                            f"must equal expected page count {expected_page_count}",
                        )
                    )
            else:
                if sample["exit_code"] == 0:
                    violations.append(
                        Violation(f"{sample_path}.exit_code", "passing expected failures must exit nonzero")
                    )
                if output["published_pdf"]:
                    violations.append(
                        Violation(
                            f"{sample_path}.output.published_pdf",
                            "passing expected failures must not publish a PDF",
                        )
                    )
                if failure["code"] != expected_failure_code:
                    violations.append(
                        Violation(
                            f"{sample_path}.failure.code",
                            f"must equal expected failure code {expected_failure_code!r}",
                        )
                    )

    aggregates = data["aggregates"]
    passing_samples = [sample for sample in samples if sample["ok"] and sample["correctness"]["pass"]]
    walls = [sample["wall_ms"] for sample in passing_samples]
    aggregate_series = {
        f"{path}.aggregates.latency": (aggregates["latency"], percentiles(walls)),
        f"{path}.aggregates.cpu.user_ms": (
            aggregates["cpu"]["user_ms"],
            percentiles([sample["user_ms"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.cpu.sys_ms": (
            aggregates["cpu"]["sys_ms"],
            percentiles([sample["sys_ms"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.cpu.wall_ms": (aggregates["cpu"]["wall_ms"], percentiles(walls)),
        f"{path}.aggregates.memory.peak_rss_kib": (
            aggregates["memory"]["peak_rss_kib"],
            percentiles([sample["peak_rss_kib"] for sample in passing_samples]),
        ),
        f"{path}.aggregates.output.pdf_bytes": (
            aggregates["output"]["pdf_bytes"],
            percentiles([sample["output"]["pdf_bytes"] for sample in passing_samples]),
        ),
    }
    for aggregate_path, (actual, expected) in aggregate_series.items():
        if actual != expected:
            violations.append(Violation(aggregate_path, f"must equal aggregate of passing samples {expected!r}"))

    if "expected_page_count" in data["fixture"]:
        expected_page_count = data["fixture"]["expected_page_count"]
        if aggregates["output"]["page_count"] != expected_page_count:
            violations.append(
                Violation(
                    f"{path}.aggregates.output.page_count",
                    f"must equal fixture expected_page_count {expected_page_count!r}",
                )
            )

    mean_wall = statistics.fmean(walls) if walls else 0.0
    if "scaling" in aggregates:
        page_count = aggregates["output"]["page_count"]
        rss = percentiles([sample["peak_rss_kib"] for sample in passing_samples])
        expected_scaling = {
            "per_page_wall_ms": round(mean_wall / page_count, 3) if page_count else 0.0,
            "per_page_peak_rss_kib": round(rss["mean"] / page_count, 1) if page_count else 0.0,
        }
        if aggregates["scaling"] != expected_scaling:
            violations.append(
                Violation(f"{path}.aggregates.scaling", f"must equal passing-sample scaling {expected_scaling!r}")
            )
    if "throughput" in aggregates:
        expected_throughput = {
            "renders_per_minute": round(60_000 / mean_wall, 2) if mean_wall else 0.0,
            "concurrency": 1,
        }
        if aggregates["throughput"] != expected_throughput:
            violations.append(
                Violation(
                    f"{path}.aggregates.throughput",
                    f"must equal serial passing-sample throughput {expected_throughput!r}",
                )
            )

    pass_count = sum(sample["correctness"]["pass"] for sample in samples)
    all_passed = pass_count == len(samples)
    expected_correctness = {
        "pass_count": pass_count,
        "total": len(samples),
        "passed": all_passed,
    }
    aggregate_correctness = aggregates["correctness"]
    for field, expected in expected_correctness.items():
        if aggregate_correctness[field] != expected:
            violations.append(
                Violation(
                    f"{path}.aggregates.correctness.{field}",
                    f"must equal {expected!r} from retained samples",
                )
            )

    expected_status = "supported" if all_passed else "failed"
    if "status" in data and data["status"] != expected_status:
        violations.append(Violation(f"{path}.status", f"must be {expected_status!r}"))

    failed_samples = [sample for sample in samples if not sample["ok"]]
    expected_codes = Counter((sample["failure"]["code"] or "unknown") for sample in failed_samples)
    aggregate_failures = aggregates["failures"]
    if aggregate_failures["count"] != len(failed_samples):
        violations.append(
            Violation(
                f"{path}.aggregates.failures.count",
                f"must equal the {len(failed_samples)} failed samples",
            )
        )
    if "codes" in aggregate_failures and aggregate_failures["codes"] != dict(expected_codes):
        violations.append(
            Violation(
                f"{path}.aggregates.failures.codes",
                f"must equal the failed-sample code counts {dict(expected_codes)!r}",
            )
        )

    hashes = [
        sample["output"]["pdf_sha256"]
        for sample in samples
        if sample["ok"] and sample["correctness"]["pass"] and sample["output"]["pdf_sha256"]
    ]
    hash_counts = Counter(hashes)
    expected_determinism = {
        "identical_pdf_sha256": max(hash_counts.values(), default=0),
        "total": len(hashes),
        "pdf_sha256_variants": len(hash_counts),
    }
    determinism = aggregates["determinism"]
    for field, expected in expected_determinism.items():
        if determinism[field] != expected:
            violations.append(
                Violation(
                    f"{path}.aggregates.determinism.{field}",
                    f"must equal {expected} from passing sample hashes",
                )
            )


def validate_document(data: Any, schema: dict[str, Any]) -> list[Violation]:
    violations: list[Violation] = []
    validate(data, schema, "$", violations)
    if not violations:
        validate_semantics(data, "$", violations)
    return violations


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print("usage: validate_result.py <result.json> [<schema.json>]", file=sys.stderr)
        return 2
    data = load_json(Path(sys.argv[1]))
    schema = load_json(Path(sys.argv[2]) if len(sys.argv) == 3 else DEFAULT_SCHEMA)
    violations = validate_document(data, schema)
    if violations:
        for violation in violations:
            print(f"violation {violation}", file=sys.stderr)
        print(f"result failed validation with {len(violations)} violation(s)", file=sys.stderr)
        return 1
    print("result validated against schema")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
