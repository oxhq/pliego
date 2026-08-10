#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Dependency-free self-test for the proposed Pliego API 2 contract."""

from __future__ import annotations

import copy
import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent
SCHEMA_DIR = ROOT / "schema"
GOLDEN_DIR = ROOT / "goldens"
FIXTURE_DIR = ROOT / "fixtures"
INPUT_ROOT = FIXTURE_DIR / "input"
DELIVERY_ROOT = FIXTURE_DIR / "delivery"
DIAGNOSTIC_ROOT = FIXTURE_DIR / "diagnostics"
INPUT_MANIFEST_PATH = FIXTURE_DIR / "input-manifest.json"
BUNDLE_MANIFEST_PATH = DELIVERY_ROOT / "bundle.json"
SCENE_PATH = DELIVERY_ROOT / "scene.json"
SCHEMAS: dict[str, dict[str, Any]] = {}

I32_MIN = -(2**31)
I32_MAX = 2**31 - 1
U32_MAX = 2**32 - 1
A4_APP_UNITS = (47622, 67351)
WINDOWS_RESERVED_NAMES = {
    "aux",
    "con",
    "nul",
    "prn",
    *(f"com{index}" for index in range(1, 10)),
    *(f"lpt{index}" for index in range(1, 10)),
}

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


def parse_integer(token: str) -> int:
    if token == "-0":
        raise ValueError("negative zero is not canonical")
    return int(token)


def reject_float(token: str) -> None:
    raise ValueError(f"floating-point JSON number {token!r} is not permitted")


def reject_nonfinite_json(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value!r}")


def load_json(path: Path) -> Any:
    return json.loads(
        path.read_text(encoding="utf-8"),
        object_pairs_hook=reject_duplicate_keys,
        parse_int=parse_integer,
        parse_float=reject_float,
        parse_constant=reject_nonfinite_json,
    )


def canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"


def content_address(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def load_schemas() -> None:
    for path in sorted(SCHEMA_DIR.glob("*.json")):
        schema = load_json(path)
        if not isinstance(schema, dict):
            raise AssertionError(f"{path}: schema must be an object")
        SCHEMAS[path.name] = schema

    expected = {
        "bundle-manifest.v1.json",
        "document-scene.v1.json",
        "input-manifest.v1.json",
        "render-request.v1.json",
        "render-result.v1.json",
        "runtime-contract.v1.json",
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
    if node.get("type") == "number":
        raise AssertionError(f"{root_name}{path}: floating-point public numbers are forbidden")
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


def type_matches(value: Any, expected: str) -> bool:
    if expected == "null":
        return value is None
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
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
            failures: list[Violation] = []
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

    if isinstance(instance, int) and not isinstance(instance, bool):
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
    if (
        not value
        or len(value.encode("ascii", errors="ignore")) != len(value)
        or len(value) > 240
        or path.is_absolute()
        or "\\" in value
        or "//" in value
        or any(part in ("", ".", "..") for part in path.parts)
    ):
        return False
    for part in path.parts:
        if len(part) > 100 or part[-1] in (".", " "):
            return False
        if part.split(".", 1)[0].lower() in WINDOWS_RESERVED_NAMES:
            return False
    return True


def path_set_semantics(paths: list[str], path: str) -> list[Violation]:
    violations: list[Violation] = []
    if paths != sorted(paths, key=lambda value: value.encode("ascii")):
        violations.append(Violation(path, "paths must be in ascending ASCII byte order"))
    if len(paths) != len(set(paths)):
        violations.append(Violation(path, "paths must be unique"))
    folded = [value.lower() for value in paths]
    if len(folded) != len(set(folded)):
        violations.append(Violation(path, "paths have an ASCII case collision"))
    for index, value in enumerate(paths):
        if not safe_relative_path(value):
            violations.append(Violation(f"{path}[{index}]", "path is not portable"))
    folded_set = set(folded)
    for value in folded:
        parts = value.split("/")
        for count in range(1, len(parts)):
            if "/".join(parts[:count]) in folded_set:
                violations.append(Violation(path, "file and directory paths collide"))
    return violations


def descriptor_matches_bytes(descriptor: dict[str, Any], data: bytes) -> bool:
    return descriptor["bytes"] == len(data) and descriptor["sha256"] == content_address(data)


def listed_files(root: Path) -> list[str]:
    return sorted(
        (path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()),
        key=lambda value: value.encode("ascii"),
    )


def input_manifest_semantics(manifest: dict[str, Any], root: Path | None = None, path: str = "$") -> list[Violation]:
    entries = manifest["entries"]
    paths = [entry["path"] for entry in entries]
    violations = path_set_semantics(paths, f"{path}.entries")
    if root is None:
        return violations

    actual_paths = listed_files(root)
    if paths != actual_paths:
        violations.append(Violation(f"{path}.entries", f"manifest/files differ: {paths!r} != {actual_paths!r}"))
    for index, entry in enumerate(entries):
        file_path = root / PurePosixPath(entry["path"])
        if file_path.is_file() and not descriptor_matches_bytes(entry, file_path.read_bytes()):
            violations.append(Violation(f"{path}.entries[{index}]", "hash or byte length does not match fixture"))
    return violations


def page_dimensions(page: dict[str, Any]) -> tuple[int, int]:
    size = page["size"]
    if "name" in size:
        return A4_APP_UNITS
    return size["width_app_units"], size["height_app_units"]


def request_semantics(
    request: dict[str, Any], manifest: dict[str, Any] | None = None, path: str = "$"
) -> list[Violation]:
    violations: list[Violation] = []
    entrypoint = request["input"]["entrypoint"]
    if not safe_relative_path(entrypoint):
        violations.append(Violation(f"{path}.input.entrypoint", "path is not portable"))
    if manifest is not None:
        manifest_paths = {entry["path"] for entry in manifest["entries"]}
        if entrypoint not in manifest_paths:
            violations.append(Violation(f"{path}.input.entrypoint", "entrypoint is absent from input manifest"))
        manifest_bytes = INPUT_MANIFEST_PATH.read_bytes()
        if not descriptor_matches_bytes(request["input"]["manifest"], manifest_bytes):
            violations.append(Violation(f"{path}.input.manifest", "descriptor does not match input manifest bytes"))

    width, height = page_dimensions(request["page"])
    margins = request["page"]["margins_app_units"]
    if margins["left"] + margins["right"] >= width:
        violations.append(Violation(f"{path}.page.margins_app_units", "horizontal margins consume the page"))
    if margins["top"] + margins["bottom"] >= height:
        violations.append(Violation(f"{path}.page.margins_app_units", "vertical margins consume the page"))
    return violations


def utf8_boundaries(value: str) -> set[int]:
    boundaries = {0}
    offset = 0
    for character in value:
        offset += len(character.encode("utf-8"))
        boundaries.add(offset)
    return boundaries


def path_data_semantics(value: str, path: str) -> list[Violation]:
    violations: list[Violation] = []
    tokens = value.split(" ")
    if len(tokens) < 3 or tokens[0] != "M":
        return [Violation(path, "path must begin with absolute M")]
    arity = {"M": 2, "L": 2, "Q": 4, "C": 6, "Z": 0}
    index = 0
    while index < len(tokens):
        command = tokens[index]
        if command not in arity:
            violations.append(Violation(path, f"unknown path command {command!r}"))
            break
        index += 1
        count = arity[command]
        if index + count > len(tokens):
            violations.append(Violation(path, f"path command {command} is truncated"))
            break
        for token in tokens[index : index + count]:
            try:
                coordinate = int(token)
            except ValueError:
                violations.append(Violation(path, f"path coordinate {token!r} is not an integer"))
                continue
            if not I32_MIN <= coordinate <= I32_MAX:
                violations.append(Violation(path, "path coordinate exceeds signed 32-bit app units"))
        index += count
    return violations


def canonical_percent_encoding(value: str) -> bool:
    if re.search(r"%(?![0-9A-F]{2})", value):
        return False
    unreserved = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
    return all(chr(int(match.group()[1:], 16)) not in unreserved for match in re.finditer(r"%[0-9A-F]{2}", value))


def canonical_link_target(value: str) -> bool:
    if not canonical_percent_encoding(value) or value.endswith(("?", "#")):
        return False
    try:
        value.encode("ascii")
    except UnicodeEncodeError:
        return False
    parsed = urlsplit(value)
    if parsed.scheme in ("http", "https"):
        try:
            port = parsed.port
        except ValueError:
            return False
        if (
            not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.netloc != parsed.netloc.lower()
            or not parsed.path.startswith("/")
            or port == (80 if parsed.scheme == "http" else 443)
        ):
            return False
        try:
            parsed.netloc.encode("ascii")
        except UnicodeEncodeError:
            return False
        return all(segment not in (".", "..") for segment in unquote(parsed.path).split("/"))
    if parsed.scheme == "mailto":
        address = parsed.path
        if parsed.netloc or parsed.fragment or "@" not in address:
            return False
        local, domain = address.rsplit("@", 1)
        return bool(local and domain and domain == domain.lower())
    return False


def scene_semantics(scene: dict[str, Any], request: dict[str, Any] | None = None, path: str = "$") -> list[Violation]:
    violations: list[Violation] = []
    if request is not None and scene["request_page"] != request["page"]:
        violations.append(Violation(f"{path}.request_page", "does not equal the accepted request page policy"))
    semantic_layer = scene["semantic_layer"]
    if request is not None and semantic_layer is not None and semantic_layer["profile"] != request["profile"]:
        violations.append(
            Violation(f"{path}.semantic_layer.profile", "does not equal the requested conformance profile")
        )
    request_width, request_height = page_dimensions(scene["request_page"])
    request_margins = scene["request_page"]["margins_app_units"]

    for page_index, page in enumerate(scene["pages"]):
        page_path = f"{path}.pages[{page_index}]"
        if page["number"] != page_index + 1:
            violations.append(Violation(f"{page_path}.number", "page numbers must be contiguous from one"))
        if page["style_source"] == "request-defaults":
            size = page["size_app_units"]
            if (size["width"], size["height"]) != (request_width, request_height):
                violations.append(Violation(f"{page_path}.size_app_units", "does not resolve request defaults"))
            if page["margins_app_units"] != request_margins:
                violations.append(Violation(f"{page_path}.margins_app_units", "does not resolve request defaults"))
        size = page["size_app_units"]
        margins = page["margins_app_units"]
        if margins["left"] + margins["right"] >= size["width"]:
            violations.append(Violation(f"{page_path}.margins_app_units", "horizontal margins consume the page"))
        if margins["top"] + margins["bottom"] >= size["height"]:
            violations.append(Violation(f"{page_path}.margins_app_units", "vertical margins consume the page"))

        for operation_index, operation in enumerate(page["operations"]):
            operation_path = f"{page_path}.operations[{operation_index}]"
            if operation["type"] == "path":
                violations.extend(path_data_semantics(operation["data"], f"{operation_path}.data"))
            elif operation["type"] == "link" and not canonical_link_target(operation["target"]):
                violations.append(Violation(f"{operation_path}.target", "target is not a canonical absolute URL"))
            elif operation["type"] == "text":
                if len(operation["text"].encode("utf-8")) > U32_MAX:
                    violations.append(
                        Violation(f"{operation_path}.text", "UTF-8 text length exceeds unsigned 32-bit range")
                    )
                boundaries = utf8_boundaries(operation["text"])
                last_start = 0
                for glyph_index, glyph in enumerate(operation["glyphs"]):
                    glyph_path = f"{operation_path}.glyphs[{glyph_index}].text_range"
                    start = glyph["text_range"]["start"]
                    end = glyph["text_range"]["end"]
                    if start >= end or start not in boundaries or end not in boundaries:
                        violations.append(Violation(glyph_path, "range must be nonempty and on UTF-8 boundaries"))
                    if start < last_start:
                        violations.append(Violation(glyph_path, "ranges must be nondecreasing"))
                    last_start = start
    return violations


def scene_resources(scene: dict[str, Any]) -> set[str]:
    resources: set[str] = set()
    if scene["semantic_layer"] is not None:
        resources.add(scene["semantic_layer"]["resource"])
    for page in scene["pages"]:
        for operation in page["operations"]:
            if operation["type"] == "text":
                resources.add(operation["font"])
            elif operation["type"] == "image":
                resources.add(operation["resource"])
    return resources


def bundle_manifest_semantics(
    manifest: dict[str, Any],
    scene: dict[str, Any] | None = None,
    additional_resources: set[str] | None = None,
    root: Path | None = None,
    path: str = "$",
) -> list[Violation]:
    entries = manifest["entries"]
    paths = [entry["path"] for entry in entries]
    violations = path_set_semantics(paths, f"{path}.entries")
    if paths.count("document.pdf") != 1 or paths.count("scene.json") != 1:
        violations.append(Violation(f"{path}.entries", "must contain exactly one PDF and one scene"))
    for index, entry in enumerate(entries):
        if entry["path"].startswith("resources/"):
            digest = entry["path"].removeprefix("resources/")
            if entry["sha256"] != f"sha256:{digest}":
                violations.append(Violation(f"{path}.entries[{index}]", "resource path/hash mismatch"))

    if scene is not None:
        expected = scene_resources(scene).union(additional_resources or set())
        actual = {entry["sha256"] for entry in entries if entry["path"].startswith("resources/")}
        if expected != actual:
            violations.append(
                Violation(f"{path}.entries", f"scene resource closure differs: {expected!r} != {actual!r}")
            )
        semantic_layer = scene["semantic_layer"]
        if semantic_layer is not None:
            semantic_entry = next(
                (entry for entry in entries if entry["sha256"] == semantic_layer["resource"]),
                None,
            )
            if semantic_entry is None or semantic_entry["media_type"] != semantic_layer["media_type"]:
                violations.append(
                    Violation(
                        f"{path}.entries",
                        "semantic-layer resource and media type are not bound by bundle manifest",
                    )
                )

    if root is not None:
        actual_paths = listed_files(root)
        expected_paths = sorted(["bundle.json", *paths], key=lambda value: value.encode("ascii"))
        if actual_paths != expected_paths:
            violations.append(Violation(f"{path}.entries", "delivery has an unlisted or missing file"))
        for index, entry in enumerate(entries):
            file_path = root / PurePosixPath(entry["path"])
            if file_path.is_file() and not descriptor_matches_bytes(entry, file_path.read_bytes()):
                violations.append(Violation(f"{path}.entries[{index}]", "hash or byte length does not match fixture"))
    return violations


def diagnostic_semantics(result: dict[str, Any], path: str) -> list[Violation]:
    diagnostics = result["diagnostics"]
    violations: list[Violation] = []
    retention = result["request"]["diagnostics"]["retention"]
    expected_retained = retention == "always" or (retention == "on-failure" and result["status"] == "failed")
    if diagnostics["retained"] != expected_retained:
        violations.append(Violation(f"{path}.diagnostics.retained", "does not match retention policy"))
    if not diagnostics["retained"] and diagnostics["artifacts"]:
        violations.append(Violation(f"{path}.diagnostics.artifacts", "must be empty when not retained"))
    artifact_paths = [artifact["path"] for artifact in diagnostics["artifacts"]]
    violations.extend(path_set_semantics(artifact_paths, f"{path}.diagnostics.artifacts"))
    for index, artifact in enumerate(diagnostics["artifacts"]):
        file_path = DIAGNOSTIC_ROOT / PurePosixPath(artifact["path"])
        if not file_path.is_file() or not descriptor_matches_bytes(artifact, file_path.read_bytes()):
            violations.append(Violation(f"{path}.diagnostics.artifacts[{index}]", "descriptor does not match fixture"))
    return violations


def result_semantics(
    result: dict[str, Any],
    input_manifest: dict[str, Any] | None = None,
    scene: dict[str, Any] | None = None,
    bundle_manifest: dict[str, Any] | None = None,
    path: str = "$",
) -> list[Violation]:
    violations = request_semantics(result["request"], input_manifest, f"{path}.request")
    violations.extend(diagnostic_semantics(result, path))
    conformance = result["conformance"]
    requested_profile = result["request"]["profile"]
    if conformance["requested"] != requested_profile:
        violations.append(Violation(f"{path}.conformance.requested", "does not equal the request profile"))
    evidence = conformance["evidence"]
    if evidence is not None and evidence["profile"] != requested_profile:
        violations.append(Violation(f"{path}.conformance.evidence.profile", "does not equal the request profile"))
    if requested_profile is None:
        if conformance["status"] != "not-requested" or evidence is not None:
            violations.append(
                Violation(
                    f"{path}.conformance",
                    "a null request profile cannot make a conformance claim or carry evidence",
                )
            )
    else:
        if conformance["status"] == "not-requested":
            violations.append(Violation(f"{path}.conformance.status", "cannot ignore a requested profile"))
        if conformance["status"] == "satisfied" and evidence is None:
            violations.append(
                Violation(
                    f"{path}.conformance.evidence",
                    "satisfied conformance requires non-null deterministic evidence",
                )
            )
        if result["status"] == "success" and conformance["status"] != "satisfied":
            violations.append(
                Violation(
                    f"{path}.conformance.status",
                    "successful delivery with a requested profile must be satisfied",
                )
            )
        if result["status"] == "failed" and conformance["status"] == "satisfied":
            violations.append(Violation(f"{path}.conformance.status", "a failed result cannot claim satisfaction"))
    if result["status"] == "failed":
        return violations
    if scene is None or bundle_manifest is None:
        violations.append(Violation(f"{path}.delivery", "success validation requires scene and bundle fixtures"))
        return violations

    violations.extend(scene_semantics(scene, result["request"], f"{path}.delivery.scene"))
    evidence_resources = {evidence["resource"]} if evidence is not None else set()
    violations.extend(
        bundle_manifest_semantics(
            bundle_manifest,
            scene,
            evidence_resources,
            DELIVERY_ROOT,
            f"{path}.delivery.bundle",
        )
    )
    delivery = result["delivery"]
    fixture_paths = {
        "pdf": DELIVERY_ROOT / "document.pdf",
        "scene": SCENE_PATH,
        "bundle": BUNDLE_MANIFEST_PATH,
    }
    for name, file_path in fixture_paths.items():
        if not descriptor_matches_bytes(delivery[name], file_path.read_bytes()):
            violations.append(Violation(f"{path}.delivery.{name}", "descriptor does not match fixture bytes"))

    entries = {entry["path"]: entry for entry in bundle_manifest["entries"]}
    for name in ("pdf", "scene"):
        if entries.get(delivery[name]["path"]) != delivery[name]:
            violations.append(Violation(f"{path}.delivery.{name}", "descriptor is not bound by bundle manifest"))
    if evidence is not None:
        evidence_entry = next(
            (entry for entry in bundle_manifest["entries"] if entry["sha256"] == evidence["resource"]),
            None,
        )
        if evidence_entry is None or evidence_entry["media_type"] != evidence["media_type"]:
            violations.append(
                Violation(
                    f"{path}.conformance.evidence",
                    "evidence resource and media type are not bound by bundle manifest",
                )
            )
    diagnostic_paths = {artifact["path"] for artifact in result["diagnostics"]["artifacts"]}
    if diagnostic_paths.intersection(entries):
        violations.append(Violation(f"{path}.diagnostics.artifacts", "diagnostic entered deterministic bundle"))
    return violations


def runtime_semantics(runtime: dict[str, Any], path: str = "$") -> list[Violation]:
    contracts = runtime["contracts"]
    normalized = [json.dumps(item, separators=(",", ":")) for item in contracts]
    violations: list[Violation] = []
    if normalized != sorted(normalized):
        violations.append(Violation(f"{path}.contracts", "contract tuples must be canonically ordered"))
    if len(normalized) != len(set(normalized)):
        violations.append(Violation(f"{path}.contracts", "contract tuples must be unique"))
    for index, contract in enumerate(contracts):
        profiles = contract["profiles"]
        profile_keys = [(profile["schema"], profile["version"]) for profile in profiles]
        if profile_keys != sorted(profile_keys):
            violations.append(Violation(f"{path}.contracts[{index}].profiles", "profiles must be canonically ordered"))
    return violations


def schema_errors(kind: str, value: Any) -> list[Violation]:
    schema_name = {
        "bundle_manifest": "bundle-manifest.v1.json",
        "input_manifest": "input-manifest.v1.json",
        "request": "render-request.v1.json",
        "result": "render-result.v1.json",
        "runtime": "runtime-contract.v1.json",
        "scene": "document-scene.v1.json",
    }[kind]
    return validate(value, SCHEMAS[schema_name], schema_name)


def assert_valid(name: str, kind: str, value: Any, semantic_errors: list[Violation] | None = None) -> None:
    errors = schema_errors(kind, value)
    if not errors and semantic_errors:
        errors.extend(semantic_errors)
    if errors:
        raise AssertionError(f"{name} should be accepted:\n" + "\n".join(map(str, errors)))


def assert_rejected(
    name: str,
    kind: str,
    value: Any,
    expected: str,
    semantic_errors: list[Violation] | None = None,
) -> None:
    errors = schema_errors(kind, value)
    if not errors and semantic_errors:
        errors.extend(semantic_errors)
    rendered = "\n".join(map(str, errors))
    if not errors or expected not in rendered:
        raise AssertionError(f"{name} should be rejected with {expected!r}, got:\n{rendered}")


def golden(path: str) -> Any:
    return load_json(GOLDEN_DIR / path)


def assert_canonical_fixture(path: Path, value: Any) -> None:
    if path.read_bytes() != canonical_json_bytes(value):
        raise AssertionError(f"{path}: JSON bytes are not canonical compact UTF-8 plus LF")


def assert_minimal_pdf_fixture(path: Path) -> None:
    data = path.read_bytes()
    if not data.startswith(b"%PDF-") or not data.rstrip().endswith(b"%%EOF"):
        raise AssertionError(f"{path}: missing PDF header or trailer")
    start_match = re.search(rb"startxref\n([0-9]+)\n%%EOF\n?$", data)
    if start_match is None or int(start_match.group(1)) != data.index(b"xref\n"):
        raise AssertionError(f"{path}: startxref does not address the xref table")
    xref_lines = data[data.index(b"xref\n") :].splitlines()
    if xref_lines[:2] != [b"xref", b"0 5"]:
        raise AssertionError(f"{path}: unexpected xref shape")
    for object_number, line in enumerate(xref_lines[3:7], start=1):
        offset = int(line.split()[0])
        if not data[offset:].startswith(f"{object_number} 0 obj".encode("ascii")):
            raise AssertionError(f"{path}: xref entry {object_number} has the wrong offset")
    stream_match = re.search(rb"/Length ([0-9]+) >>\nstream\n(.*?)\nendstream", data, re.DOTALL)
    if stream_match is None or int(stream_match.group(1)) != len(stream_match.group(2)):
        raise AssertionError(f"{path}: stream length is not self-consistent")


def main() -> None:
    load_schemas()
    for name, schema in SCHEMAS.items():
        if schema.get("$schema") != "http://json-schema.org/draft-07/schema#":
            raise AssertionError(f"{name}: unexpected JSON Schema dialect")
        audit_schema(schema, "", name)

    input_manifest = load_json(INPUT_MANIFEST_PATH)
    bundle_manifest = load_json(BUNDLE_MANIFEST_PATH)
    delivery_scene = load_json(SCENE_PATH)
    request_a4 = golden("accepted/render-request.a4.json")
    request_explicit = golden("accepted/render-request.explicit-page.json")
    scene = golden("accepted/document-scene.json")
    success = golden("accepted/render-result.success.json")
    failure = golden("accepted/render-result.failure.json")
    runtime = golden("accepted/runtime-contract.json")

    assert_canonical_fixture(INPUT_MANIFEST_PATH, input_manifest)
    assert_canonical_fixture(BUNDLE_MANIFEST_PATH, bundle_manifest)
    assert_canonical_fixture(SCENE_PATH, delivery_scene)
    if scene != delivery_scene:
        raise AssertionError("accepted scene golden and canonical scene bytes differ")
    assert_minimal_pdf_fixture(DELIVERY_ROOT / "document.pdf")

    assert_valid(
        "input manifest",
        "input_manifest",
        input_manifest,
        input_manifest_semantics(input_manifest, INPUT_ROOT),
    )
    assert_valid("native A4 request", "request", request_a4, request_semantics(request_a4, input_manifest))
    assert_valid(
        "explicit page request",
        "request",
        request_explicit,
        request_semantics(request_explicit, input_manifest),
    )
    assert_valid("public ordered scene", "scene", scene, scene_semantics(scene, request_a4))
    assert_valid(
        "bundle manifest",
        "bundle_manifest",
        bundle_manifest,
        bundle_manifest_semantics(bundle_manifest, scene, root=DELIVERY_ROOT),
    )
    assert_valid(
        "success result",
        "result",
        success,
        result_semantics(success, input_manifest, scene, bundle_manifest),
    )
    assert_valid(
        "failure result",
        "result",
        failure,
        result_semantics(failure, input_manifest),
    )
    assert_valid("runtime contract", "runtime", runtime, runtime_semantics(runtime))
    if success["request"] != request_a4 or failure["request"] != request_a4:
        raise AssertionError("both result branches must retain the exact normalized request")
    if runtime["engine"] != success["engine"] or runtime["engine"] != failure["engine"]:
        raise AssertionError("probe and both result branches must retain the exact engine identity")

    assert_rejected(
        "request API mismatch",
        "request",
        golden("rejected/render-request.api-mismatch.json"),
        "expected const 2",
    )
    assert_rejected(
        "request live network",
        "request",
        golden("rejected/render-request.live-network.json"),
        "expected const 'deny'",
    )
    for name, kind, relative in (
        ("request unknown member", "request", "rejected/render-request.unknown-member.json"),
        ("scene unknown member", "scene", "rejected/document-scene.unknown-member.json"),
        ("result unknown member", "result", "rejected/render-result.unknown-member.json"),
        ("input manifest unknown member", "input_manifest", "rejected/input-manifest.unknown-member.json"),
        ("bundle manifest unknown member", "bundle_manifest", "rejected/bundle-manifest.unknown-member.json"),
        ("runtime unknown member", "runtime", "rejected/runtime-contract.unknown-member.json"),
    ):
        assert_rejected(name, kind, golden(relative), "unexpected property")

    glyph_overflow = golden("rejected/document-scene.glyph-u32-overflow.json")
    assert_rejected("glyph u32 overflow", "scene", glyph_overflow, "maximum 4294967295")
    invalid_path = golden("rejected/document-scene.invalid-path-data.json")
    assert_rejected("noncanonical path grammar", "scene", invalid_path, "does not match pattern")
    invalid_link = golden("rejected/document-scene.noncanonical-link.json")
    assert_rejected(
        "noncanonical link",
        "scene",
        invalid_link,
        "canonical absolute URL",
        scene_semantics(invalid_link),
    )
    try:
        golden("rejected/document-scene.negative-zero.json")
    except ValueError as error:
        if "negative zero" not in str(error):
            raise
    else:
        raise AssertionError("negative-zero golden should fail lexical decoding")

    for name, relative, expected in (
        ("input path case collision", "rejected/input-manifest.case-collision.json", "case collision"),
        ("input manifest reordering", "rejected/input-manifest.unsorted.json", "ascending ASCII byte order"),
        ("input path prefix collision", "rejected/input-manifest.path-prefix-collision.json", "file and directory"),
    ):
        value = golden(relative)
        assert_rejected(name, "input_manifest", value, expected, input_manifest_semantics(value))
    assert_rejected(
        "noncanonical input URL root",
        "input_manifest",
        golden("rejected/input-manifest.noncanonical-root.json"),
        "pliego-input:///",
    )

    extra = golden("rejected/bundle-manifest.extra-entry.json")
    assert_rejected("extra bundle entry", "bundle_manifest", extra, "oneOf")
    unsorted = golden("rejected/bundle-manifest.unsorted.json")
    assert_rejected(
        "bundle manifest reordering",
        "bundle_manifest",
        unsorted,
        "ascending ASCII byte order",
        bundle_manifest_semantics(unsorted),
    )
    missing = golden("rejected/bundle-manifest.missing-resource.json")
    assert_rejected(
        "bundle missing scene resource",
        "bundle_manifest",
        missing,
        "scene resource closure differs",
        bundle_manifest_semantics(missing, scene),
    )

    assert_rejected(
        "failed result with partial delivery",
        "result",
        golden("rejected/render-result.partial-delivery.json"),
        "delivery",
    )
    assert_rejected(
        "cross-paired runtime tuple",
        "runtime",
        golden("rejected/runtime-contract.cross-paired.json"),
        "expected const 1",
    )
    assert_rejected(
        "noncanonical target triple",
        "runtime",
        golden("rejected/runtime-contract.noncanonical-target.json"),
        "does not match pattern",
    )

    bundle_inline = copy.deepcopy(success)
    bundle_inline["delivery"]["bundle"]["entries"] = copy.deepcopy(bundle_manifest["entries"])
    assert_rejected("bundle descriptor contains manifest", "result", bundle_inline, "unexpected property")
    public_internal_code = copy.deepcopy(failure)
    public_internal_code["error"]["code"] = "RESOURCE_MANIFEST_MISSING"
    assert_rejected("internal code in public error", "result", public_internal_code, "unexpected property")
    host_fonts = copy.deepcopy(request_a4)
    host_fonts["resources"]["host_fonts"] = "allow"
    assert_rejected("live host font lookup", "request", host_fonts, "expected const 'deny'")

    mismatched_scene = copy.deepcopy(scene)
    mismatched_scene["request_page"] = copy.deepcopy(request_explicit["page"])
    assert_rejected(
        "result request/scene page mismatch",
        "result",
        success,
        "does not equal the accepted request page policy",
        result_semantics(success, input_manifest, mismatched_scene, bundle_manifest),
    )
    wrong_default = copy.deepcopy(scene)
    wrong_default["pages"][0]["size_app_units"]["width"] += 1
    assert_rejected(
        "request-default page geometry drift",
        "scene",
        wrong_default,
        "does not resolve request defaults",
        scene_semantics(wrong_default, request_a4),
    )
    oversized_coordinate = copy.deepcopy(scene)
    oversized_coordinate["pages"][0]["operations"][1]["data"] = "M 2147483648 0 L 0 0"
    assert_rejected(
        "path coordinate outside i32",
        "scene",
        oversized_coordinate,
        "signed 32-bit app units",
        scene_semantics(oversized_coordinate, request_a4),
    )

    future_profile = {"schema": "pliego.profile.future", "version": 1}
    profiled_request = copy.deepcopy(request_a4)
    profiled_request["profile"] = copy.deepcopy(future_profile)
    assert_valid(
        "generic future profile request slot",
        "request",
        profiled_request,
        request_semantics(profiled_request, input_manifest),
    )
    advertised_profile = copy.deepcopy(runtime)
    advertised_profile["contracts"][0]["profiles"] = [copy.deepcopy(future_profile)]
    assert_valid(
        "generic future profile probe slot",
        "runtime",
        advertised_profile,
        runtime_semantics(advertised_profile),
    )
    semantic_scene = copy.deepcopy(scene)
    semantic_scene["semantic_layer"] = {
        "schema": "pliego.document-semantics.future",
        "version": 1,
        "profile": copy.deepcopy(future_profile),
        "resource": "sha256:83ee8b728ac4d73ac6454bd63f58ef28f97d40d38ad6ddfd499e3f890b4a8ca4",
        "media_type": "application/vnd.pliego.document-semantics+json",
    }
    assert_rejected(
        "semantic layer without requested profile",
        "scene",
        semantic_scene,
        "does not equal the requested conformance profile",
        scene_semantics(semantic_scene, request_a4),
    )
    mismatched_profile = copy.deepcopy(success)
    mismatched_profile["request"] = copy.deepcopy(profiled_request)
    assert_rejected(
        "profile result echo mismatch",
        "result",
        mismatched_profile,
        "does not equal the request profile",
        result_semantics(mismatched_profile, input_manifest, scene, bundle_manifest),
    )
    evidence_free_claim = copy.deepcopy(success)
    evidence_free_claim["request"] = copy.deepcopy(profiled_request)
    evidence_free_claim["conformance"] = {
        "requested": copy.deepcopy(future_profile),
        "status": "satisfied",
        "evidence": None,
    }
    assert_rejected(
        "profile satisfaction without evidence",
        "result",
        evidence_free_claim,
        "requires non-null deterministic evidence",
        result_semantics(evidence_free_claim, input_manifest, scene, bundle_manifest),
    )
    mismatched_evidence = copy.deepcopy(success)
    mismatched_evidence["request"] = copy.deepcopy(profiled_request)
    mismatched_evidence["conformance"] = {
        "requested": copy.deepcopy(future_profile),
        "status": "satisfied",
        "evidence": {
            "schema": "pliego.conformance-evidence.future",
            "version": 1,
            "profile": {"schema": "pliego.profile.other", "version": 1},
            "resource": "sha256:83ee8b728ac4d73ac6454bd63f58ef28f97d40d38ad6ddfd499e3f890b4a8ca4",
            "media_type": "application/vnd.pliego.conformance-evidence+json",
        },
    }
    assert_rejected(
        "evidence/profile mismatch",
        "result",
        mismatched_evidence,
        "does not equal the request profile",
        result_semantics(mismatched_evidence, input_manifest, scene, bundle_manifest),
    )
    null_profile_claim = copy.deepcopy(success)
    null_profile_claim["conformance"]["status"] = "satisfied"
    assert_rejected(
        "conformance claim with null profile",
        "result",
        null_profile_claim,
        "null request profile cannot make a conformance claim",
        result_semantics(null_profile_claim, input_manifest, scene, bundle_manifest),
    )
    conformance_failure = copy.deepcopy(failure)
    conformance_failure["error"]["kind"] = "conformance"
    assert_valid(
        "reserved conformance error kind",
        "result",
        conformance_failure,
        result_semantics(conformance_failure, input_manifest),
    )

    reordered_scene = copy.deepcopy(scene)
    reordered_scene["pages"][0]["operations"].reverse()
    if content_address(canonical_json_bytes(reordered_scene)) == content_address(SCENE_PATH.read_bytes()):
        raise AssertionError("operation reordering must change scene identity")

    rejected_count = len(list((GOLDEN_DIR / "rejected").glob("*.json")))
    print(
        "Pliego API 2 contract self-test passed: "
        f"8 accepted artifacts, {rejected_count} rejected goldens, actual byte closure verified"
    )


if __name__ == "__main__":
    main()
