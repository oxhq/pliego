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
import struct
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
API2_EPOCH_LIMIT_MS = 8_640_000_000_000_000
API2_VIRTUAL_SPAN_MAX_MS = 2**53 - 1
API2_REQUEST_MAX_BYTES = 1_048_576
API2_INPUT_MANIFEST_MAX_BYTES = 16 * 1024 * 1024
API2_INPUT_MANIFEST_MAX_ENTRIES = 16 * 1024
API2_INPUT_CONTENT_MAX_BYTES = 64 * 1024 * 1024
API2_INPUT_TREE_MAX_DEPTH = 32
API2_INPUT_TREE_MAX_NODES = 16 * 1024
API2_ENTRYPOINT_MEDIA_TYPE = "text/html;charset=utf-8"
NANOSECONDS_PER_MILLISECOND = 1_000_000
A4_APP_UNITS = (47622, 67351)
PROTOCOL_FIELDS = (
    "input_manifest",
    "request",
    "result",
    "document_scene",
    "bundle_manifest",
)
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
        "document-scene.v2.json",
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


def member_order_semantics(
    instance: Any,
    schema: dict[str, Any],
    root_name: str,
    path: str = "$",
) -> list[Violation]:
    if "$ref" in schema:
        target, target_name = resolve_ref(schema["$ref"], root_name)
        return member_order_semantics(instance, target, target_name, path)

    if "oneOf" in schema:
        matches = [branch for branch in schema["oneOf"] if not validate(instance, branch, root_name)]
        if len(matches) != 1:
            return [Violation(path, "cannot select one schema branch for canonical member ordering")]
        return member_order_semantics(instance, matches[0], root_name, path)

    violations: list[Violation] = []
    if isinstance(instance, dict):
        properties = schema.get("properties", {})
        if properties:
            actual = list(instance)
            expected = [name for name in properties if name in instance]
            if actual != expected:
                violations.append(Violation(path, "object members must follow schema property order"))
            for name, value in instance.items():
                if name in properties:
                    violations.extend(member_order_semantics(value, properties[name], root_name, f"{path}.{name}"))
    elif isinstance(instance, list) and isinstance(schema.get("items"), dict):
        for index, item in enumerate(instance):
            violations.extend(member_order_semantics(item, schema["items"], root_name, f"{path}[{index}]"))
    return violations


def safe_relative_path(value: str) -> bool:
    parts = value.split("/")
    if (
        not value
        or len(value.encode("ascii", errors="ignore")) != len(value)
        or len(value) > 240
        or value.startswith("/")
        or "\\" in value
        or any(part in ("", ".", "..") for part in parts)
    ):
        return False
    for part in parts:
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
    folded_directories: dict[str, str] = {}
    for value in paths:
        parts = value.split("/")
        for count in range(1, len(parts)):
            directory = "/".join(parts[:count])
            folded_directory = directory.lower()
            previous = folded_directories.setdefault(folded_directory, directory)
            if previous != directory:
                violations.append(Violation(path, "directory paths have an ASCII case collision"))
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
    directories = {
        "/".join(parts[:count]) for value in paths for parts in [value.split("/")] for count in range(1, len(parts))
    }
    for index, value in enumerate(paths):
        if len(value.split("/")) > API2_INPUT_TREE_MAX_DEPTH:
            violations.append(
                Violation(
                    f"{path}.entries[{index}].path",
                    f"input tree depth exceeds {API2_INPUT_TREE_MAX_DEPTH}",
                )
            )
    if len(paths) + len(directories) > API2_INPUT_TREE_MAX_NODES:
        violations.append(
            Violation(
                f"{path}.entries",
                f"input tree exceeds {API2_INPUT_TREE_MAX_NODES} total files and directories",
            )
        )
    if sum(entry["bytes"] for entry in entries) > API2_INPUT_CONTENT_MAX_BYTES:
        violations.append(
            Violation(
                f"{path}.entries",
                f"declared content exceeds the {API2_INPUT_CONTENT_MAX_BYTES}-byte aggregate limit",
            )
        )
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
        manifest_entries = {entry["path"]: entry for entry in manifest["entries"]}
        entrypoint_entry = manifest_entries.get(entrypoint)
        if entrypoint_entry is None:
            violations.append(Violation(f"{path}.input.entrypoint", "entrypoint is absent from input manifest"))
        elif entrypoint_entry["media_type"] != API2_ENTRYPOINT_MEDIA_TYPE:
            violations.append(
                Violation(
                    f"{path}.input.entrypoint",
                    f"entrypoint media type must be {API2_ENTRYPOINT_MEDIA_TYPE}",
                )
            )
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

    resource_media_types: dict[str, str] = {}

    def bind_resource(resource: str, media_type: str, resource_path: str) -> None:
        existing = resource_media_types.setdefault(resource, media_type)
        if existing != media_type:
            violations.append(Violation(resource_path, f"resource media type conflicts with {existing!r}"))

    if semantic_layer is not None:
        bind_resource(semantic_layer["resource"], semantic_layer["media_type"], f"{path}.semantic_layer.resource")

    for page_index, page in enumerate(scene["pages"]):
        page_path = f"{path}.pages[{page_index}]"
        if page["number"] != page_index + 1:
            violations.append(Violation(f"{page_path}.number", "page numbers must be contiguous from one"))
        size = page["size_app_units"]
        if (size["width"], size["height"]) != (request_width, request_height):
            violations.append(Violation(f"{page_path}.size_app_units", "does not resolve request authority"))
        if page["margins_app_units"] != request_margins:
            violations.append(Violation(f"{page_path}.margins_app_units", "does not resolve request authority"))
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
                font = operation["font"]
                bind_resource(font["resource"], "application/octet-stream", f"{operation_path}.font.resource")
                tags = [variation["tag"] for variation in font["variations"]]
                if any(left >= right for left, right in zip(tags, tags[1:])):
                    violations.append(
                        Violation(
                            f"{operation_path}.font.variations",
                            "variation tags must be strictly ascending",
                        )
                    )
                if len(operation["text"].encode("utf-8")) > U32_MAX:
                    violations.append(
                        Violation(f"{operation_path}.text", "UTF-8 text length exceeds unsigned 32-bit range")
                    )
                boundaries = utf8_boundaries(operation["text"])
                for glyph_index, glyph in enumerate(operation["glyphs"]):
                    glyph_path = f"{operation_path}.glyphs[{glyph_index}].text_range"
                    start = glyph["text_range"]["start"]
                    end = glyph["text_range"]["end"]
                    if start >= end or start not in boundaries or end not in boundaries:
                        violations.append(Violation(glyph_path, "range must be nonempty and on UTF-8 boundaries"))
            elif operation["type"] == "image":
                bind_resource(operation["resource"], operation["media_type"], f"{operation_path}.resource")
    return violations


def scene_resources(scene: dict[str, Any]) -> set[str]:
    resources: set[str] = set()
    if scene["semantic_layer"] is not None:
        resources.add(scene["semantic_layer"]["resource"])
    for page in scene["pages"]:
        for operation in page["operations"]:
            if operation["type"] == "text":
                resources.add(operation["font"]["resource"])
            elif operation["type"] == "image":
                resources.add(operation["resource"])
    return resources


def scene_resource_media_types(scene: dict[str, Any]) -> dict[str, str]:
    resources: dict[str, str] = {}
    if scene["semantic_layer"] is not None:
        resources[scene["semantic_layer"]["resource"]] = scene["semantic_layer"]["media_type"]
    for page in scene["pages"]:
        for operation in page["operations"]:
            if operation["type"] == "text":
                resources[operation["font"]["resource"]] = "application/octet-stream"
            elif operation["type"] == "image":
                resources[operation["resource"]] = operation["media_type"]
    return resources


def bytes_match_resource_media_type(media_type: str, data: bytes) -> bool:
    if media_type == "image/png":
        return data.startswith(b"\x89PNG\r\n\x1a\n")
    if media_type == "image/jpeg":
        return data.startswith(b"\xff\xd8\xff")
    if media_type == "image/gif":
        return data.startswith((b"GIF87a", b"GIF89a"))
    if media_type == "image/webp":
        return len(data) >= 12 and data.startswith(b"RIFF") and data[8:12] == b"WEBP"
    return True


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
        expected_media_types = scene_resource_media_types(scene)
        for resource, media_type in expected_media_types.items():
            resource_entry = next((entry for entry in entries if entry["sha256"] == resource), None)
            if resource_entry is not None and resource_entry["media_type"] != media_type:
                violations.append(
                    Violation(
                        f"{path}.entries",
                        f"scene resource {resource} requires media type {media_type!r}",
                    )
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
            elif (
                file_path.is_file()
                and entry["path"].startswith("resources/")
                and not bytes_match_resource_media_type(entry["media_type"], file_path.read_bytes())
            ):
                violations.append(Violation(f"{path}.entries[{index}]", "resource bytes do not match media type"))
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


def protocol_key(contract: dict[str, Any]) -> tuple[Any, ...]:
    return (
        contract["api"],
        *((contract[name]["schema"], contract[name]["version"]) for name in PROTOCOL_FIELDS),
    )


def runtime_semantics(runtime: dict[str, Any], path: str = "$") -> list[Violation]:
    contracts = runtime["contracts"]
    normalized = [json.dumps(item, separators=(",", ":")) for item in contracts]
    violations: list[Violation] = []
    if normalized != sorted(normalized):
        violations.append(Violation(f"{path}.contracts", "contract tuples must be canonically ordered"))
    if len(normalized) != len(set(normalized)):
        violations.append(Violation(f"{path}.contracts", "contract tuples must be unique"))
    protocol_keys: set[tuple[Any, ...]] = set()
    for index, contract in enumerate(contracts):
        key = protocol_key(contract)
        if key in protocol_keys:
            violations.append(
                Violation(
                    f"{path}.contracts[{index}]",
                    "the same protocol tuple is advertised more than once",
                )
            )
        protocol_keys.add(key)
        profiles = contract["profiles"]
        profile_keys = [(profile["schema"], profile["version"]) for profile in profiles]
        if profile_keys != sorted(profile_keys):
            violations.append(Violation(f"{path}.contracts[{index}].profiles", "profiles must be canonically ordered"))
    return violations


def request_runtime_semantics(
    request: dict[str, Any],
    runtime: dict[str, Any],
    path: str = "$",
) -> list[Violation]:
    expected_key = (
        request["api"],
        ("pliego.input-manifest", 1),
        (request["schema"], request["version"]),
        ("pliego.render-result", 1),
        ("pliego.document-scene", 2),
        ("pliego.bundle-manifest", 1),
    )
    matches = [contract for contract in runtime["contracts"] if protocol_key(contract) == expected_key]
    if len(matches) != 1:
        return [Violation(path, "runtime does not advertise exactly one matching API 2 protocol tuple")]
    profile = request["profile"]
    if profile is not None and profile not in matches[0]["profiles"]:
        return [Violation(f"{path}.profile", "profile is not advertised by the selected protocol tuple")]
    return []


def schema_errors(kind: str, value: Any) -> list[Violation]:
    schema_name = {
        "bundle_manifest": "bundle-manifest.v1.json",
        "input_manifest": "input-manifest.v1.json",
        "request": "render-request.v1.json",
        "result": "render-result.v1.json",
        "runtime": "runtime-contract.v1.json",
        "scene": "document-scene.v2.json",
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


def assert_canonical_fixture(path: Path, value: Any, schema_name: str) -> None:
    order_errors = member_order_semantics(value, SCHEMAS[schema_name], schema_name)
    if order_errors:
        raise AssertionError(f"{path}: noncanonical typed member order:\n" + "\n".join(map(str, order_errors)))
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


def assert_controlled_time_boundaries(request: dict[str, Any], input_manifest: dict[str, Any]) -> None:
    for name, epoch_ms in (
        ("minimum controlled epoch", -API2_EPOCH_LIMIT_MS),
        ("maximum controlled epoch", API2_EPOCH_LIMIT_MS),
    ):
        boundary = copy.deepcopy(request)
        boundary["time"]["epoch_unix_ms"] = epoch_ms
        assert_valid(name, "request", boundary, request_semantics(boundary, input_manifest))

    for name, epoch_ms, expected in (
        ("epoch below minimum", -API2_EPOCH_LIMIT_MS - 1, f"minimum {-API2_EPOCH_LIMIT_MS}"),
        ("epoch above maximum", API2_EPOCH_LIMIT_MS + 1, f"maximum {API2_EPOCH_LIMIT_MS}"),
    ):
        outside = copy.deepcopy(request)
        outside["time"]["epoch_unix_ms"] = epoch_ms
        assert_rejected(name, "request", outside, expected)

    for name, span_ms in (
        ("minimum virtual span", 1),
        ("maximum virtual span", API2_VIRTUAL_SPAN_MAX_MS),
    ):
        boundary = copy.deepcopy(request)
        boundary["settlement"]["limits"]["virtual_span_ms"] = span_ms
        assert_valid(name, "request", boundary, request_semantics(boundary, input_manifest))

    for name, span_ms, expected in (
        ("virtual span below minimum", 0, "minimum 1"),
        (
            "virtual span above maximum",
            API2_VIRTUAL_SPAN_MAX_MS + 1,
            f"maximum {API2_VIRTUAL_SPAN_MAX_MS}",
        ),
    ):
        outside = copy.deepcopy(request)
        outside["settlement"]["limits"]["virtual_span_ms"] = span_ms
        assert_rejected(name, "request", outside, expected)

    combined = copy.deepcopy(request)
    combined["time"]["epoch_unix_ms"] = API2_EPOCH_LIMIT_MS
    combined["settlement"]["limits"]["virtual_span_ms"] = API2_VIRTUAL_SPAN_MAX_MS
    assert_valid(
        "maximum epoch and span combination",
        "request",
        combined,
        request_semantics(combined, input_manifest),
    )
    epoch_ns = API2_EPOCH_LIMIT_MS * NANOSECONDS_PER_MILLISECOND
    span_ns = API2_VIRTUAL_SPAN_MAX_MS * NANOSECONDS_PER_MILLISECOND
    if epoch_ns != 8_640_000_000_000_000_000_000:
        raise AssertionError("maximum epoch must scale exactly to signed integer nanoseconds")
    if span_ns != 9_007_199_254_740_991_000_000:
        raise AssertionError("maximum virtual span must scale exactly to integer nanoseconds")
    if epoch_ns + span_ns != 17_647_199_254_740_991_000_000:
        raise AssertionError("maximum accepted wall-time arithmetic must not narrow through u64")


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
    unavailable_runtime = copy.deepcopy(runtime)
    unavailable_runtime["contracts"] = []

    assert_canonical_fixture(INPUT_MANIFEST_PATH, input_manifest, "input-manifest.v1.json")
    assert_canonical_fixture(BUNDLE_MANIFEST_PATH, bundle_manifest, "bundle-manifest.v1.json")
    assert_canonical_fixture(SCENE_PATH, delivery_scene, "document-scene.v2.json")
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
    assert_valid(
        "unavailable executable foundation",
        "runtime",
        unavailable_runtime,
        runtime_semantics(unavailable_runtime),
    )
    assert_rejected(
        "request without an advertised tuple",
        "request",
        request_a4,
        "runtime does not advertise exactly one matching API 2 protocol tuple",
        request_semantics(request_a4, input_manifest) + request_runtime_semantics(request_a4, unavailable_runtime),
    )
    assert_valid(
        "request/runtime negotiation",
        "request",
        request_a4,
        request_semantics(request_a4, input_manifest) + request_runtime_semantics(request_a4, runtime),
    )
    assert_controlled_time_boundaries(request_a4, input_manifest)
    if success["request"] != request_a4 or failure["request"] != request_a4:
        raise AssertionError("both result branches must retain the exact normalized request")
    if runtime["engine"] != success["engine"] or runtime["engine"] != failure["engine"]:
        raise AssertionError("probe and both result branches must retain the exact engine identity")
    if runtime["invocation"]["request_max_bytes"] != API2_REQUEST_MAX_BYTES:
        raise AssertionError("API 2 request framing limit drifted")
    if runtime["invocation"]["job_root_transport"] != "cwd-v1":
        raise AssertionError("API 2 job-root transport drifted")
    if runtime["invocation"]["input_manifest_max_bytes"] != API2_INPUT_MANIFEST_MAX_BYTES:
        raise AssertionError("API 2 input-manifest byte limit drifted")
    if runtime["invocation"]["input_content_max_bytes"] != API2_INPUT_CONTENT_MAX_BYTES:
        raise AssertionError("API 2 input-content byte limit drifted")

    exact_manifest_limit = copy.deepcopy(request_a4)
    exact_manifest_limit["input"]["manifest"]["bytes"] = API2_INPUT_MANIFEST_MAX_BYTES
    assert_valid("inclusive input-manifest byte limit", "request", exact_manifest_limit)
    over_manifest_limit = copy.deepcopy(exact_manifest_limit)
    over_manifest_limit["input"]["manifest"]["bytes"] += 1
    assert_rejected(
        "input-manifest byte limit overflow",
        "request",
        over_manifest_limit,
        f"maximum {API2_INPUT_MANIFEST_MAX_BYTES}",
    )

    maximum_path = f"{'a' * 100}/{'b' * 100}/{'c' * 38}"
    maximum_entry = {
        "path": maximum_path,
        "media_type": f"a/{'b' * 253}",
        "sha256": f"sha256:{'0' * 64}",
        "bytes": API2_INPUT_CONTENT_MAX_BYTES,
    }
    empty_manifest = {
        "schema": "pliego.input-manifest",
        "version": 1,
        "url_root": "pliego-input:///",
        "entries": [],
    }
    maximum_entry_bytes = len(canonical_json_bytes(maximum_entry)) - 1
    maximum_manifest_bytes = (
        len(canonical_json_bytes(empty_manifest))
        + API2_INPUT_MANIFEST_MAX_ENTRIES * maximum_entry_bytes
        + API2_INPUT_MANIFEST_MAX_ENTRIES
        - 1
    )
    if maximum_manifest_bytes != 10_207_321 or maximum_manifest_bytes > API2_INPUT_MANIFEST_MAX_BYTES:
        raise AssertionError("input-manifest byte and entry limits no longer cover the full schema envelope")

    over_entry_limit = copy.deepcopy(input_manifest)
    over_entry_limit["entries"] = [input_manifest["entries"][0]] * (API2_INPUT_MANIFEST_MAX_ENTRIES + 1)
    assert_rejected(
        "input-manifest entry limit overflow",
        "input_manifest",
        over_entry_limit,
        f"at most {API2_INPUT_MANIFEST_MAX_ENTRIES} items",
    )

    path_at_depth = "/".join(f"d{index}" for index in range(API2_INPUT_TREE_MAX_DEPTH))
    exact_depth = copy.deepcopy(input_manifest)
    exact_depth["entries"] = [
        {
            "path": path_at_depth,
            "media_type": "application/octet-stream",
            "sha256": f"sha256:{'0' * 64}",
            "bytes": 0,
        }
    ]
    assert_valid(
        "inclusive input-tree depth limit",
        "input_manifest",
        exact_depth,
        input_manifest_semantics(exact_depth),
    )
    over_depth = copy.deepcopy(exact_depth)
    over_depth["entries"][0]["path"] += "/overflow"
    assert_rejected(
        "input-tree depth overflow",
        "input_manifest",
        over_depth,
        f"input tree depth exceeds {API2_INPUT_TREE_MAX_DEPTH}",
        input_manifest_semantics(over_depth),
    )

    exact_node_limit = copy.deepcopy(input_manifest)
    exact_node_limit["entries"] = [
        {
            "path": f"d{index:05}/file.bin",
            "media_type": "application/octet-stream",
            "sha256": f"sha256:{'0' * 64}",
            "bytes": 0,
        }
        for index in range(API2_INPUT_TREE_MAX_NODES // 2)
    ]
    assert_valid(
        "inclusive input-tree node limit",
        "input_manifest",
        exact_node_limit,
        input_manifest_semantics(exact_node_limit),
    )
    over_node_limit = copy.deepcopy(exact_node_limit)
    over_node_limit["entries"].append(
        {
            "path": "d08192/file.bin",
            "media_type": "application/octet-stream",
            "sha256": f"sha256:{'0' * 64}",
            "bytes": 0,
        }
    )
    assert_rejected(
        "input-tree node overflow",
        "input_manifest",
        over_node_limit,
        f"input tree exceeds {API2_INPUT_TREE_MAX_NODES} total files and directories",
        input_manifest_semantics(over_node_limit),
    )

    directory_case_collision = copy.deepcopy(input_manifest)
    directory_case_collision["entries"] = [
        {
            "path": "A/x.bin",
            "media_type": "application/octet-stream",
            "sha256": f"sha256:{'0' * 64}",
            "bytes": 0,
        },
        {
            "path": "a/y.bin",
            "media_type": "application/octet-stream",
            "sha256": f"sha256:{'1' * 64}",
            "bytes": 0,
        },
    ]
    assert_rejected(
        "input-tree implied-directory case collision",
        "input_manifest",
        directory_case_collision,
        "directory paths have an ASCII case collision",
        input_manifest_semantics(directory_case_collision),
    )

    exact_content_limit = copy.deepcopy(input_manifest)
    exact_content_limit["entries"] = [
        {
            "path": "assets/a.bin",
            "media_type": "application/octet-stream",
            "sha256": f"sha256:{'0' * 64}",
            "bytes": API2_INPUT_CONTENT_MAX_BYTES // 4,
        },
        {
            "path": "assets/b.bin",
            "media_type": "application/octet-stream",
            "sha256": f"sha256:{'0' * 64}",
            "bytes": API2_INPUT_CONTENT_MAX_BYTES // 4,
        },
        {
            "path": "document.html",
            "media_type": "text/html;charset=utf-8",
            "sha256": f"sha256:{'1' * 64}",
            "bytes": API2_INPUT_CONTENT_MAX_BYTES // 2,
        },
    ]
    assert_valid(
        "inclusive aggregate input-content limit",
        "input_manifest",
        exact_content_limit,
        input_manifest_semantics(exact_content_limit),
    )
    over_content_limit = copy.deepcopy(exact_content_limit)
    over_content_limit["entries"][2]["bytes"] += 1
    assert_rejected(
        "aggregate input-content limit overflow",
        "input_manifest",
        over_content_limit,
        f"declared content exceeds the {API2_INPUT_CONTENT_MAX_BYTES}-byte aggregate limit",
        input_manifest_semantics(over_content_limit),
    )
    one_oversized_entry = copy.deepcopy(input_manifest)
    one_oversized_entry["entries"][0]["bytes"] = API2_INPUT_CONTENT_MAX_BYTES + 1
    assert_rejected(
        "individual input entry exceeds aggregate limit",
        "input_manifest",
        one_oversized_entry,
        f"maximum {API2_INPUT_CONTENT_MAX_BYTES}",
    )

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
    assert_rejected(
        "legacy CSS page precedence",
        "request",
        golden("rejected/render-request.css-page-precedence.json"),
        "unexpected property 'css_page_precedence'",
    )
    non_html_entrypoint = golden("rejected/render-request.non-html-entrypoint.json")
    assert_rejected(
        "non-HTML entrypoint",
        "request",
        non_html_entrypoint,
        "entrypoint media type must be text/html;charset=utf-8",
        request_semantics(non_html_entrypoint, input_manifest),
    )
    noncanonical_html_manifest = copy.deepcopy(input_manifest)
    next(entry for entry in noncanonical_html_manifest["entries"] if entry["path"] == "document.html")["media_type"] = (
        "text/html"
    )
    assert_rejected(
        "noncanonical HTML entrypoint media type",
        "request",
        golden("accepted/render-request.a4.json"),
        "entrypoint media type must be text/html;charset=utf-8",
        request_semantics(golden("accepted/render-request.a4.json"), noncanonical_html_manifest),
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

    legacy_scene_identity = copy.deepcopy(scene)
    legacy_scene_identity["version"] = 1
    assert_rejected(
        "shipped internal scene identity reused by public scene",
        "scene",
        legacy_scene_identity,
        "expected const 2",
    )

    glyph_overflow = golden("rejected/document-scene.glyph-u32-overflow.json")
    assert_rejected("glyph u32 overflow", "scene", glyph_overflow, "maximum 4294967295")
    assert_rejected(
        "CSS page provenance",
        "scene",
        golden("rejected/document-scene.css-page-source.json"),
        "expected const 'request-defaults'",
    )
    range_overflow = copy.deepcopy(scene)
    range_overflow["pages"][0]["operations"][0]["glyphs"][0]["text_range"]["end"] = U32_MAX + 1
    assert_rejected("glyph range u32 overflow", "scene", range_overflow, "maximum 4294967295")
    descending_rtl_ranges = copy.deepcopy(scene)
    first_glyph = descending_rtl_ranges["pages"][0]["operations"][0]["glyphs"][0]
    first_glyph["text_range"] = {"start": 1, "end": 2}
    second_glyph = copy.deepcopy(first_glyph)
    second_glyph["id"] = 43
    second_glyph["text_range"] = {"start": 0, "end": 1}
    descending_rtl_ranges["pages"][0]["operations"][0]["glyphs"].append(second_glyph)
    assert_valid(
        "descending visual-order RTL glyph ranges",
        "scene",
        descending_rtl_ranges,
        scene_semantics(descending_rtl_ranges, request_a4),
    )

    for name, bits in (
        ("positive zero font variation", 0),
        ("minimum positive subnormal font variation", 1),
        ("maximum positive finite font variation", 2_139_095_039),
        ("minimum negative subnormal font variation", 2_147_483_649),
        ("maximum negative finite font variation", 4_286_578_687),
    ):
        boundary = copy.deepcopy(scene)
        boundary["pages"][0]["operations"][0]["font"]["variations"][0]["value_f32_bits"] = bits
        assert_valid(name, "scene", boundary, scene_semantics(boundary, request_a4))
        decoded = struct.unpack(">f", bits.to_bytes(4, "big"))[0]
        if int.from_bytes(struct.pack(">f", decoded), "big") != bits:
            raise AssertionError(f"{name} did not round-trip through exact IEEE-754 binary32 bits")

    for name, bits in (
        ("negative font variation bits", -1),
        ("negative zero font variation", 2_147_483_648),
        ("positive infinity font variation", 2_139_095_040),
        ("positive NaN font variation", 2_143_289_344),
        ("negative infinity font variation", 4_286_578_688),
        ("negative NaN font variation", 4_290_772_992),
        ("font variation bits above u32", U32_MAX + 1),
    ):
        invalid_variation = copy.deepcopy(scene)
        invalid_variation["pages"][0]["operations"][0]["font"]["variations"][0]["value_f32_bits"] = bits
        assert_rejected(name, "scene", invalid_variation, "oneOf")

    reordered_variations = copy.deepcopy(scene)
    reordered_variations["pages"][0]["operations"][0]["font"]["variations"] = [
        {"tag": 2, "value_f32_bits": 0},
        {"tag": 1, "value_f32_bits": 0},
    ]
    assert_rejected(
        "reordered font variation tags",
        "scene",
        reordered_variations,
        "variation tags must be strictly ascending",
        scene_semantics(reordered_variations, request_a4),
    )
    duplicate_variation_tags = copy.deepcopy(scene)
    duplicate_variation_tags["pages"][0]["operations"][0]["font"]["variations"] = [
        {"tag": 1, "value_f32_bits": 0},
        {"tag": 1, "value_f32_bits": 1},
    ]
    assert_rejected(
        "duplicate font variation tags",
        "scene",
        duplicate_variation_tags,
        "variation tags must be strictly ascending",
        scene_semantics(duplicate_variation_tags, request_a4),
    )
    legacy_scalar_font = copy.deepcopy(scene)
    legacy_scalar_font["pages"][0]["operations"][0]["font"] = scene["pages"][0]["operations"][0]["font"]["resource"]
    assert_rejected("legacy scalar font identity", "scene", legacy_scalar_font, "expected type ['object']")
    invalid_path = golden("rejected/document-scene.invalid-path-data.json")
    assert_rejected("noncanonical path grammar", "scene", invalid_path, "does not match pattern")
    unsupported_image_media = copy.deepcopy(scene)
    unsupported_image_media["pages"][0]["operations"][2]["media_type"] = "image/svg+xml"
    assert_rejected(
        "unsupported public scene image media type",
        "scene",
        unsupported_image_media,
        "expected one of",
    )
    invalid_link = golden("rejected/document-scene.noncanonical-link.json")
    assert_rejected(
        "noncanonical link",
        "scene",
        invalid_link,
        "canonical absolute URL",
        scene_semantics(invalid_link),
    )
    for name, target in (
        ("link userinfo", "https://user@example.test/a"),
        ("link default port", "https://example.test:443/a"),
        ("link dot segment", "https://example.test/a/../b"),
        ("link lowercase percent escape", "https://example.test/%7euser"),
        ("link escaped unreserved byte", "https://example.test/%7Euser"),
        ("link empty HTTP path", "https://example.test"),
        ("mailto uppercase domain", "mailto:invoice@Example.test"),
    ):
        noncanonical_url = copy.deepcopy(scene)
        noncanonical_url["pages"][0]["operations"][3]["target"] = target
        assert_rejected(
            name,
            "scene",
            noncanonical_url,
            "canonical absolute URL",
            scene_semantics(noncanonical_url, request_a4),
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
    for name, unsafe_path in (
        ("input path dot segment", "assets/z/./mark.svg"),
        ("input path Windows device basename", "assets/z/CON.txt"),
    ):
        value = copy.deepcopy(input_manifest)
        value["entries"][1]["path"] = unsafe_path
        assert_rejected(name, "input_manifest", value, "path is not portable", input_manifest_semantics(value))
    assert_rejected(
        "noncanonical input URL root",
        "input_manifest",
        golden("rejected/input-manifest.noncanonical-root.json"),
        "pliego-input:///",
    )
    reordered_manifest_members = {
        "entries": copy.deepcopy(input_manifest["entries"]),
        "schema": input_manifest["schema"],
        "version": input_manifest["version"],
        "url_root": input_manifest["url_root"],
    }
    assert_rejected(
        "input manifest member reordering",
        "input_manifest",
        reordered_manifest_members,
        "object members must follow schema property order",
        member_order_semantics(
            reordered_manifest_members,
            SCHEMAS["input-manifest.v1.json"],
            "input-manifest.v1.json",
        ),
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
    mismatched_image_media = copy.deepcopy(bundle_manifest)
    next(
        entry
        for entry in mismatched_image_media["entries"]
        if entry["sha256"] == scene["pages"][0]["operations"][2]["resource"]
    )["media_type"] = "image/jpeg"
    assert_rejected(
        "bundle image media type differs from scene",
        "bundle_manifest",
        mismatched_image_media,
        "requires media type 'image/png'",
        bundle_manifest_semantics(mismatched_image_media, scene),
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
    duplicate_protocol_tuple = copy.deepcopy(runtime)
    second_contract = copy.deepcopy(duplicate_protocol_tuple["contracts"][0])
    second_contract["profiles"] = [{"schema": "pliego.profile.future", "version": 1}]
    duplicate_protocol_tuple["contracts"].append(second_contract)
    assert_rejected(
        "ambiguous duplicate protocol tuple",
        "runtime",
        duplicate_protocol_tuple,
        "same protocol tuple is advertised more than once",
        runtime_semantics(duplicate_protocol_tuple),
    )

    bundle_inline = copy.deepcopy(success)
    bundle_inline["delivery"]["bundle"]["entries"] = copy.deepcopy(bundle_manifest["entries"])
    assert_rejected("bundle descriptor contains manifest", "result", bundle_inline, "unexpected property")
    public_internal_code = copy.deepcopy(failure)
    public_internal_code["error"]["code"] = "RESOURCE_MANIFEST_MISSING"
    assert_rejected("internal code in public error", "result", public_internal_code, "unexpected property")
    unstable_error_kind = copy.deepcopy(failure)
    unstable_error_kind["error"]["kind"] = "layout-thread-timeout"
    assert_rejected("unstable public error kind", "result", unstable_error_kind, "expected one of")
    host_fonts = copy.deepcopy(request_a4)
    host_fonts["resources"]["host_fonts"] = "allow"
    assert_rejected("live host font lookup", "request", host_fonts, "expected const 'deny'")
    premature_operation_semantics = copy.deepcopy(scene)
    premature_operation_semantics["pages"][0]["operations"][0]["meta"] = {
        "semantics": {"role": "heading", "label": "Invoice"}
    }
    assert_rejected(
        "operation semantics before R3.5",
        "scene",
        premature_operation_semantics,
        "unexpected property 'meta'",
    )

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
        "does not resolve request authority",
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
    assert_rejected(
        "unadvertised future profile",
        "request",
        profiled_request,
        "profile is not advertised by the selected protocol tuple",
        request_runtime_semantics(profiled_request, runtime),
    )
    advertised_profile = copy.deepcopy(runtime)
    advertised_profile["contracts"][0]["profiles"] = [copy.deepcopy(future_profile)]
    assert_valid(
        "generic future profile probe slot",
        "runtime",
        advertised_profile,
        runtime_semantics(advertised_profile),
    )
    assert_valid(
        "advertised future profile negotiation",
        "request",
        profiled_request,
        request_semantics(profiled_request, input_manifest)
        + request_runtime_semantics(profiled_request, advertised_profile),
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

    base_scene_identity = content_address(canonical_json_bytes(scene))
    for name, mutate in (
        ("font face index", lambda font: font.__setitem__("face_index", font["face_index"] + 1)),
        (
            "font variation bits",
            lambda font: font["variations"][0].__setitem__(
                "value_f32_bits", font["variations"][0]["value_f32_bits"] + 1
            ),
        ),
        ("synthetic bold", lambda font: font.__setitem__("synthetic_bold", not font["synthetic_bold"])),
    ):
        changed_font_scene = copy.deepcopy(scene)
        mutate(changed_font_scene["pages"][0]["operations"][0]["font"])
        assert_valid(name, "scene", changed_font_scene, scene_semantics(changed_font_scene, request_a4))
        if content_address(canonical_json_bytes(changed_font_scene)) == base_scene_identity:
            raise AssertionError(f"{name} must change scene identity")
        if scene_resources(changed_font_scene) != scene_resources(scene):
            raise AssertionError(f"{name} must not create a second raw font resource")

    shared_resource_instances = copy.deepcopy(scene)
    second_text = copy.deepcopy(shared_resource_instances["pages"][0]["operations"][0])
    second_text["font"]["face_index"] += 1
    shared_resource_instances["pages"][0]["operations"].insert(1, second_text)
    assert_valid(
        "two font instances sharing one raw resource",
        "scene",
        shared_resource_instances,
        scene_semantics(shared_resource_instances, request_a4),
    )
    if scene_resources(shared_resource_instances) != scene_resources(scene):
        raise AssertionError("two font instances sharing bytes must require only one bundled font resource")
    if content_address(canonical_json_bytes(shared_resource_instances)) == base_scene_identity:
        raise AssertionError("adding a distinct font instance must change scene identity")

    font_with_internal_id = copy.deepcopy(scene)
    font_with_internal_id["pages"][0]["operations"][0]["font"]["id"] = "sha256:" + "0" * 64
    assert_rejected(
        "internal font instance id",
        "scene",
        font_with_internal_id,
        "unexpected property 'id'",
    )

    rejected_count = len(list((GOLDEN_DIR / "rejected").glob("*.json")))
    print(
        "Pliego API 2 contract self-test passed: "
        f"8 accepted artifacts, {rejected_count} rejected goldens, actual byte closure verified"
    )


if __name__ == "__main__":
    main()
