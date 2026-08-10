#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Dependency-free self-test for the proposed Pliego API 2 schemas and goldens."""

from __future__ import annotations

import copy
import hashlib
import json
import math
import re
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parent
SCHEMA_DIR = ROOT / "schema"
GOLDEN_DIR = ROOT / "goldens"
SCHEMAS: dict[str, dict[str, Any]] = {}

KNOWN_SCHEMA_KEYWORDS = {
    "$id",
    "$ref",
    "$schema",
    "additionalProperties",
    "anyOf",
    "const",
    "default",
    "definitions",
    "description",
    "enum",
    "exclusiveMinimum",
    "items",
    "maximum",
    "maxItems",
    "maxLength",
    "minimum",
    "minItems",
    "minLength",
    "oneOf",
    "pattern",
    "properties",
    "required",
    "title",
    "type",
    "uniqueItems",
}


@dataclass(frozen=True)
class Violation:
    path: str
    message: str

    def __str__(self) -> str:
        return f"{self.path}: {self.message}"


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON object member {key!r}")
        value[key] = item
    return value


def reject_nonfinite_json(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value!r}")


def load_json(path: Path) -> Any:
    return json.loads(
        path.read_text(encoding="utf-8"),
        object_pairs_hook=reject_duplicate_keys,
        parse_constant=reject_nonfinite_json,
    )


def load_schemas() -> None:
    for path in sorted(SCHEMA_DIR.glob("*.json")):
        schema = load_json(path)
        if not isinstance(schema, dict):
            raise AssertionError(f"{path}: schema must be an object")
        SCHEMAS[path.name] = schema

    expected = {
        "document-scene.v1.json",
        "render-request.v1.json",
        "render-result.v1.json",
    }
    if set(SCHEMAS) != expected:
        raise AssertionError(f"unexpected schema set: {sorted(SCHEMAS)}")
    ids = [schema.get("$id") for schema in SCHEMAS.values()]
    if len(ids) != len(set(ids)):
        raise AssertionError("schema $id values must be unique")


def follow_pointer(document: dict[str, Any], fragment: str) -> dict[str, Any]:
    value: Any = document
    if fragment:
        if not fragment.startswith("/"):
            raise AssertionError(f"unsupported JSON pointer fragment #{fragment}")
        for part in fragment[1:].split("/"):
            key = part.replace("~1", "/").replace("~0", "~")
            if not isinstance(value, dict) or key not in value:
                raise AssertionError(f"unresolved schema pointer #{fragment}")
            value = value[key]
    if not isinstance(value, dict):
        raise AssertionError(f"schema pointer #{fragment} did not resolve to an object")
    return value


def resolve_ref(ref: str, root_name: str) -> tuple[dict[str, Any], str]:
    if ref.startswith("#"):
        document_name = root_name
        fragment = ref[1:]
    else:
        document_ref, separator, fragment = ref.partition("#")
        document_name = Path(urlsplit(document_ref).path).name
        if not separator:
            fragment = ""
    if document_name not in SCHEMAS:
        raise AssertionError(f"unresolved schema document in $ref {ref!r}")
    return follow_pointer(SCHEMAS[document_name], fragment), document_name


def audit_schema(node: dict[str, Any], path: str, root_name: str) -> None:
    unknown = set(node) - KNOWN_SCHEMA_KEYWORDS
    if unknown:
        raise AssertionError(f"{root_name}{path}: unsupported schema keywords {sorted(unknown)}")
    if node.get("type") == "object" and node.get("additionalProperties") is not False:
        raise AssertionError(f"{root_name}{path}: object schema is not closed")
    if "$ref" in node:
        resolve_ref(node["$ref"], root_name)

    for container in ("definitions", "properties"):
        for name, child in node.get(container, {}).items():
            if not isinstance(child, dict):
                raise AssertionError(f"{root_name}{path}/{container}/{name}: schema must be an object")
            audit_schema(child, f"{path}/{container}/{name}", root_name)
    for container in ("oneOf", "anyOf"):
        for index, child in enumerate(node.get(container, [])):
            if not isinstance(child, dict):
                raise AssertionError(f"{root_name}{path}/{container}/{index}: schema must be an object")
            audit_schema(child, f"{path}/{container}/{index}", root_name)
    if isinstance(node.get("items"), dict):
        audit_schema(node["items"], f"{path}/items", root_name)
    if isinstance(node.get("additionalProperties"), dict):
        audit_schema(node["additionalProperties"], f"{path}/additionalProperties", root_name)


def type_matches(value: Any, expected: str) -> bool:
    if expected == "null":
        return value is None
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)
    if expected == "string":
        return isinstance(value, str)
    if expected == "array":
        return isinstance(value, list)
    if expected == "object":
        return isinstance(value, dict)
    raise AssertionError(f"unsupported schema type {expected!r}")


def validate(instance: Any, schema: dict[str, Any], root_name: str, path: str = "$") -> list[Violation]:
    violations: list[Violation] = []
    validate_into(instance, schema, root_name, path, violations)
    return violations


def validate_into(
    instance: Any,
    schema: dict[str, Any],
    root_name: str,
    path: str,
    violations: list[Violation],
) -> None:
    if "$ref" in schema:
        target, target_name = resolve_ref(schema["$ref"], root_name)
        validate_into(instance, target, target_name, path, violations)
        return

    if "oneOf" in schema:
        branch_failures: list[list[Violation]] = []
        for branch in schema["oneOf"]:
            failures: list[Violation] = []
            validate_into(instance, branch, root_name, path, failures)
            branch_failures.append(failures)
        matches = sum(not failures for failures in branch_failures)
        if matches != 1:
            closest = min(branch_failures, key=len)
            detail = f"; closest: {closest[0]}" if closest else ""
            violations.append(Violation(path, f"expected exactly one oneOf match, got {matches}{detail}"))

    if "anyOf" in schema:
        matches = 0
        for branch in schema["anyOf"]:
            failures = []
            validate_into(instance, branch, root_name, path, failures)
            matches += not failures
        if matches == 0:
            violations.append(Violation(path, "expected at least one anyOf match"))

    if "type" in schema:
        expected_types = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(type_matches(instance, expected) for expected in expected_types):
            violations.append(Violation(path, f"expected type {expected_types!r}, got {type(instance).__name__}"))
            return

    if "const" in schema and instance != schema["const"]:
        violations.append(Violation(path, f"expected const {schema['const']!r}, got {instance!r}"))
    if "enum" in schema and instance not in schema["enum"]:
        violations.append(Violation(path, f"expected one of {schema['enum']!r}, got {instance!r}"))

    if isinstance(instance, str):
        if len(instance) < schema.get("minLength", 0):
            violations.append(Violation(path, f"string is shorter than {schema['minLength']} characters"))
        if "maxLength" in schema and len(instance) > schema["maxLength"]:
            violations.append(Violation(path, f"string is longer than {schema['maxLength']} characters"))
        if "pattern" in schema and re.fullmatch(schema["pattern"], instance) is None:
            violations.append(Violation(path, f"does not match pattern {schema['pattern']!r}"))

    if isinstance(instance, (int, float)) and not isinstance(instance, bool):
        if not math.isfinite(instance):
            violations.append(Violation(path, "number must be finite"))
        if "minimum" in schema and instance < schema["minimum"]:
            violations.append(Violation(path, f"{instance} < minimum {schema['minimum']}"))
        if "exclusiveMinimum" in schema and instance <= schema["exclusiveMinimum"]:
            violations.append(Violation(path, f"{instance} <= exclusive minimum {schema['exclusiveMinimum']}"))
        if "maximum" in schema and instance > schema["maximum"]:
            violations.append(Violation(path, f"{instance} > maximum {schema['maximum']}"))

    if isinstance(instance, list):
        if len(instance) < schema.get("minItems", 0):
            violations.append(Violation(path, f"expected at least {schema['minItems']} items"))
        if "maxItems" in schema and len(instance) > schema["maxItems"]:
            violations.append(Violation(path, f"expected at most {schema['maxItems']} items"))
        if schema.get("uniqueItems"):
            normalized = [json.dumps(item, sort_keys=True, separators=(",", ":")) for item in instance]
            if len(normalized) != len(set(normalized)):
                violations.append(Violation(path, "array items must be unique"))
        if isinstance(schema.get("items"), dict):
            for index, item in enumerate(instance):
                validate_into(item, schema["items"], root_name, f"{path}[{index}]", violations)

    if isinstance(instance, dict):
        properties = schema.get("properties", {})
        for required in schema.get("required", []):
            if required not in instance:
                violations.append(Violation(path, f"missing required property {required!r}"))
        for key, value in instance.items():
            if key in properties:
                validate_into(value, properties[key], root_name, f"{path}.{key}", violations)
            elif schema.get("additionalProperties") is False:
                violations.append(Violation(path, f"unexpected property {key!r}"))


def safe_relative_path(value: str) -> bool:
    path = PurePosixPath(value)
    return (
        bool(value)
        and not path.is_absolute()
        and "\\" not in value
        and "//" not in value
        and all(part not in ("", ".", "..") for part in path.parts)
    )


def request_semantics(request: dict[str, Any], path: str = "$") -> list[Violation]:
    violations: list[Violation] = []
    entrypoint = request["input"]["entrypoint"]
    if not safe_relative_path(entrypoint):
        violations.append(Violation(f"{path}.input.entrypoint", "must be a safe normalized relative path"))

    roots = request["resources"]["network"]["allowed_http_roots"]
    if roots != sorted(set(roots)):
        violations.append(Violation(f"{path}.resources.network.allowed_http_roots", "must be sorted and unique"))
    for index, root in enumerate(roots):
        parsed = urlsplit(root)
        if (
            parsed.scheme not in ("http", "https")
            or not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.query
            or parsed.fragment
            or not parsed.path.endswith("/")
        ):
            violations.append(Violation(f"{path}.resources.network.allowed_http_roots[{index}]", "invalid HTTP root"))

    margins = request["page"]["margins_css_px"]
    size = request["page"]["size"]
    if "name" in size:
        width, height = 793.7008, 1122.5197
    else:
        width, height = size["width_css_px"], size["height_css_px"]
    if margins["left"] + margins["right"] >= width:
        violations.append(Violation(f"{path}.page.margins_css_px", "horizontal margins consume the page"))
    if margins["top"] + margins["bottom"] >= height:
        violations.append(Violation(f"{path}.page.margins_css_px", "vertical margins consume the page"))
    return violations


def utf8_boundaries(value: str) -> set[int]:
    boundaries = {0}
    offset = 0
    for character in value:
        offset += len(character.encode("utf-8"))
        boundaries.add(offset)
    return boundaries


def scene_semantics(scene: dict[str, Any], path: str = "$") -> list[Violation]:
    violations: list[Violation] = []
    for page_index, page in enumerate(scene["pages"]):
        page_width = page["size"]["width"]
        page_height = page["size"]["height"]
        for operation_index, operation in enumerate(page["operations"]):
            operation_path = f"{path}.pages[{page_index}].operations[{operation_index}]"
            if operation["type"] in ("path", "image", "link"):
                bounds = operation["bounds"]
                if bounds["x"] + bounds["width"] > page_width or bounds["y"] + bounds["height"] > page_height:
                    violations.append(Violation(f"{operation_path}.bounds", "must remain within the page"))
            if operation["type"] == "link":
                parsed = urlsplit(operation["target"])
                if parsed.scheme not in ("http", "https", "mailto"):
                    violations.append(Violation(f"{operation_path}.target", "uses an unsafe link scheme"))
            if operation["type"] != "text":
                continue
            boundaries = utf8_boundaries(operation["text"])
            for glyph_index, glyph in enumerate(operation["glyphs"]):
                if glyph["x"] + glyph["advance"] > page_width or glyph["y"] > page_height:
                    violations.append(
                        Violation(f"{operation_path}.glyphs[{glyph_index}]", "must remain within the page")
                    )
                text_range = glyph["text_range"]
                start, end = text_range["start"], text_range["end"]
                if start >= end or start not in boundaries or end not in boundaries:
                    violations.append(
                        Violation(
                            f"{operation_path}.glyphs[{glyph_index}].text_range",
                            "must be a nonempty range on UTF-8 boundaries",
                        )
                    )
    return violations


def scene_resources(scene: dict[str, Any]) -> set[str]:
    resources: set[str] = set()
    for page in scene["pages"]:
        for operation in page["operations"]:
            if operation["type"] == "text":
                resources.add(operation["font"])
            elif operation["type"] == "image":
                resources.add(operation["resource"])
    return resources


def descriptor_matches_entry(descriptor: dict[str, Any], entries: dict[str, dict[str, Any]]) -> bool:
    return entries.get(descriptor["path"]) == descriptor


def result_semantics(
    result: dict[str, Any],
    scene: dict[str, Any] | None,
    path: str = "$",
) -> list[Violation]:
    violations = request_semantics(result["request"], f"{path}.request")
    diagnostics = result["diagnostics"]
    retention = result["request"]["diagnostics"]["retention"]
    should_retain = retention == "always" or (retention == "on-failure" and result["status"] == "failed")
    if diagnostics["retained"] != should_retain:
        violations.append(Violation(f"{path}.diagnostics.retained", "does not match the request retention policy"))
    if not diagnostics["retained"] and diagnostics["artifacts"]:
        violations.append(
            Violation(f"{path}.diagnostics.artifacts", "must be empty when diagnostics were not retained")
        )
    for index, artifact in enumerate(diagnostics["artifacts"]):
        if not safe_relative_path(artifact["path"]):
            violations.append(Violation(f"{path}.diagnostics.artifacts[{index}].path", "unsafe relative path"))

    if result["status"] == "failed":
        return violations
    if scene is None:
        violations.append(Violation(f"{path}.delivery.scene", "success validation requires the scene artifact"))
        return violations

    delivery = result["delivery"]
    entries_list = delivery["bundle"]["entries"]
    entry_paths = [entry["path"] for entry in entries_list]
    if entry_paths != sorted(entry_paths):
        violations.append(Violation(f"{path}.delivery.bundle.entries", "paths must be lexicographically ordered"))
    if len(entry_paths) != len(set(entry_paths)):
        violations.append(Violation(f"{path}.delivery.bundle.entries", "paths must be unique"))
    for index, entry in enumerate(entries_list):
        entry_path = entry["path"]
        if not safe_relative_path(entry_path):
            violations.append(Violation(f"{path}.delivery.bundle.entries[{index}].path", "unsafe relative path"))
        if entry_path.startswith("diagnostics/"):
            violations.append(Violation(f"{path}.delivery.bundle.entries[{index}].path", "diagnostic entered bundle"))
        if entry_path.startswith("resources/"):
            digest = entry_path.removeprefix("resources/")
            if entry["sha256"] != f"sha256:{digest}":
                violations.append(Violation(f"{path}.delivery.bundle.entries[{index}]", "resource path/hash mismatch"))

    entries = {entry["path"]: entry for entry in entries_list}
    for name in ("pdf", "scene"):
        if not descriptor_matches_entry(delivery[name], entries):
            violations.append(Violation(f"{path}.delivery.{name}", "descriptor is not bound by the bundle"))
    if delivery["bundle"]["path"] in entries:
        violations.append(Violation(f"{path}.delivery.bundle.entries", "bundle must not list itself"))

    normalized_scene = json.dumps(scene, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    expected_scene_hash = f"sha256:{hashlib.sha256(normalized_scene).hexdigest()}"
    if delivery["scene"]["sha256"] != expected_scene_hash:
        violations.append(Violation(f"{path}.delivery.scene.sha256", "does not hash the normalized scene"))
    if delivery["scene"]["bytes"] != len(normalized_scene):
        violations.append(Violation(f"{path}.delivery.scene.bytes", "does not equal normalized scene bytes"))

    expected_resources = scene_resources(scene)
    bundled_resources = {entry["sha256"] for entry in entries_list if entry["path"].startswith("resources/")}
    missing = sorted(expected_resources - bundled_resources)
    extra = sorted(bundled_resources - expected_resources)
    if missing:
        violations.append(Violation(f"{path}.delivery.bundle.entries", f"missing scene resources {missing}"))
    if extra:
        violations.append(Violation(f"{path}.delivery.bundle.entries", f"unreferenced resources {extra}"))

    diagnostic_paths = {artifact["path"] for artifact in diagnostics["artifacts"]}
    overlap = sorted(diagnostic_paths.intersection(entry_paths))
    if overlap:
        violations.append(Violation(f"{path}.diagnostics.artifacts", f"diagnostics entered bundle {overlap}"))
    return violations


def schema_errors(kind: str, value: Any) -> list[Violation]:
    schema_name = {
        "request": "render-request.v1.json",
        "result": "render-result.v1.json",
        "scene": "document-scene.v1.json",
    }[kind]
    return validate(value, SCHEMAS[schema_name], schema_name)


def assert_valid(name: str, kind: str, value: Any, scene: dict[str, Any] | None = None) -> None:
    errors = schema_errors(kind, value)
    if kind == "request" and not errors:
        errors.extend(request_semantics(value))
    elif kind == "scene" and not errors:
        errors.extend(scene_semantics(value))
    elif kind == "result" and not errors:
        errors.extend(result_semantics(value, scene))
    if errors:
        raise AssertionError(f"{name} should be accepted:\n" + "\n".join(map(str, errors)))


def assert_rejected(
    name: str,
    kind: str,
    value: Any,
    expected: str,
    scene: dict[str, Any] | None = None,
) -> None:
    errors = schema_errors(kind, value)
    if kind == "request" and not errors:
        errors.extend(request_semantics(value))
    elif kind == "scene" and not errors:
        errors.extend(scene_semantics(value))
    elif kind == "result" and not errors:
        errors.extend(result_semantics(value, scene))
    rendered = "\n".join(map(str, errors))
    if not errors or expected not in rendered:
        raise AssertionError(f"{name} should be rejected with {expected!r}, got:\n{rendered}")


def golden(path: str) -> Any:
    return load_json(GOLDEN_DIR / path)


def main() -> None:
    load_schemas()
    for name, schema in SCHEMAS.items():
        if schema.get("$schema") != "http://json-schema.org/draft-07/schema#":
            raise AssertionError(f"{name}: unexpected JSON Schema dialect")
        audit_schema(schema, "", name)

    request_a4 = golden("accepted/render-request.a4.json")
    request_explicit = golden("accepted/render-request.explicit-page.json")
    scene = golden("accepted/document-scene.json")
    success = golden("accepted/render-result.success.json")
    failure = golden("accepted/render-result.failure.json")

    assert_valid("native A4 request", "request", request_a4)
    assert_valid("explicit page request", "request", request_explicit)
    assert_valid("public ordered scene", "scene", scene)
    assert_valid("success result", "result", success, scene)
    assert_valid("failure result", "result", failure)
    if success["request"] != request_a4 or failure["request"] != request_a4:
        raise AssertionError("both result branches must retain the exact normalized request")

    assert_rejected(
        "request API mismatch",
        "request",
        golden("rejected/render-request.api-mismatch.json"),
        "expected const 2",
    )
    for name, kind, path in (
        ("request unknown member", "request", "rejected/render-request.unknown-member.json"),
        ("scene unknown member", "scene", "rejected/document-scene.unknown-member.json"),
        ("result unknown member", "result", "rejected/render-result.unknown-member.json"),
    ):
        assert_rejected(name, kind, golden(path), "unexpected property")
    assert_rejected(
        "failed result with partial delivery",
        "result",
        golden("rejected/render-result.partial-delivery.json"),
        "delivery",
    )
    assert_rejected(
        "bundle missing one scene resource",
        "result",
        golden("rejected/render-result.missing-resource.json"),
        "missing scene resources",
        scene,
    )

    unsorted = copy.deepcopy(success)
    unsorted["delivery"]["bundle"]["entries"].reverse()
    assert_rejected("unsorted bundle", "result", unsorted, "lexicographically ordered", scene)
    diagnostic_in_bundle = copy.deepcopy(success)
    diagnostic_in_bundle["delivery"]["bundle"]["entries"].append(copy.deepcopy(success["diagnostics"]["artifacts"][0]))
    assert_rejected("diagnostic in bundle", "result", diagnostic_in_bundle, "diagnostic entered bundle", scene)
    digest_mismatch = copy.deepcopy(success)
    digest_mismatch["delivery"]["bundle"]["entries"][1]["sha256"] = "sha256:" + "9" * 64
    assert_rejected("resource path/hash mismatch", "result", digest_mismatch, "resource path/hash mismatch", scene)

    print("Pliego API 2 contract self-test passed: 5 accepted goldens, 6 rejected goldens")


if __name__ == "__main__":
    main()
