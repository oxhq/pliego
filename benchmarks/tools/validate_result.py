#!/usr/bin/env python3

"""Dependency-free JSON Schema validation for benchmark results.

Implements the subset of draft-07 used by `schema/benchmark-result.v1.json`
(no `jsonschema` required): type (including null unions), const, enum,
required, properties, additionalProperties, minimum, minItems, $ref
resolution into `definitions`, and pattern. Anything outside that subset is
ignored so the schema stays the only authority.

Usage:
    python3 benchmarks/tools/validate_result.py <result.json> [<schema.json>]
"""

from __future__ import annotations

import json
import re
import sys
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

    if "type" in schema:
        types = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(type_matches(data, t) for t in types):
            violations.append(
                Violation(path, f"expected type {types!r}, got {type(data).__name__}")
            )
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
            violations.append(
                Violation(path, f"expected >= {schema['minItems']} items, got {len(data)}")
            )
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
    violations: list[Violation] = []
    validate(data, schema, "$", violations)
    if violations:
        for violation in violations:
            print(f"violation {violation}", file=sys.stderr)
        print(f"result failed validation with {len(violations)} violation(s)", file=sys.stderr)
        return 1
    print("result validated against schema")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
