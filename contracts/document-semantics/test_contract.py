#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Dependency-free self-test for canonical Pliego document semantics v1."""

from __future__ import annotations

import argparse
import copy
import hashlib
import importlib.util
import json
import subprocess
import sys
import unicodedata
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from types import ModuleType
from typing import Any


ROOT = Path(__file__).resolve().parent
REPO_ROOT = ROOT.parents[1]
API2_ROOT = REPO_ROOT / "contracts" / "api2"
API2_TEST = API2_ROOT / "test_contract.py"
SCHEMA_PATH = ROOT / "schema" / "document-semantics.v1.json"
SCHEMA_NAME = SCHEMA_PATH.name
ACCEPTED_PATH = ROOT / "goldens" / "accepted" / "representative.json"
REJECTION_CORPUS_PATH = ROOT / "goldens" / "rejected" / "cases.json"
NEGATIVE_ZERO_PATH = ROOT / "goldens" / "rejected" / "negative-zero.json"
SCENE_PATH = API2_ROOT / "fixtures" / "delivery" / "scene.json"
RUNTIME_PATH = API2_ROOT / "goldens" / "accepted" / "runtime-contract.json"
REQUEST_PATH = API2_ROOT / "goldens" / "accepted" / "render-request.a4.json"
FRESH_PROCESS_COUNT = 100
PDFUA1_PROFILE = {"schema": "pliego.profile.pdfua-1", "version": 1}
MAX_STRUCTURE_DEPTH = 1024
MAX_TABLE_SLOTS = 1_000_000

PLAIN_ROLES = {
    "document",
    "section",
    "paragraph",
    "span",
    "list-label",
    "list-body",
    "table-head",
    "table-body",
    "table-foot",
    "table-row",
    "figure",
    "formula",
    "caption",
    "quote",
    "code",
    "note",
}
ROLE_SEMANTICS = {
    "heading": "heading",
    "list": "list",
    "list-item": "list-item",
    "table": "table",
    "table-header-cell": "table-cell",
    "table-cell": "table-cell",
    "link": "link",
}
TABLE_GROUP_ORDER = {"table-head": 0, "table-body": 1, "table-foot": 2}
PAINT_KIND = {"text": "text", "image": "image", "path": "path", "annotation": "link"}


@dataclass
class GraphState:
    node_parent: dict[int, int] = field(default_factory=dict)
    artifact_parent: dict[int, int] = field(default_factory=dict)
    fragment_owners: dict[int, list[tuple[str, int]]] = field(default_factory=lambda: defaultdict(list))


def load_api2_contract() -> ModuleType:
    spec = importlib.util.spec_from_file_location("pliego_api2_contract_for_semantics", API2_TEST)
    if spec is None or spec.loader is None:
        raise AssertionError(f"cannot import API 2 self-test from {API2_TEST}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def prepare_contract() -> tuple[ModuleType, dict[str, Any]]:
    api2 = load_api2_contract()
    api2.SCHEMAS = {}
    api2.load_schemas()
    schema = api2.load_json(SCHEMA_PATH)
    if not isinstance(schema, dict):
        raise AssertionError("semantic schema must be a JSON object")
    api2.SCHEMAS[SCHEMA_NAME] = schema
    api2.audit_schema(schema, "#", SCHEMA_NAME)
    return api2, schema


def canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"


def semantic_digest(value: Any) -> str:
    return f"sha256:{hashlib.sha256(canonical_json_bytes(value)).hexdigest()}"


def error(path: str, message: str) -> str:
    return f"{path}: {message}"


def canonical_text_errors(value: str, path: str) -> list[str]:
    errors: list[str] = []
    first_control = next((character for character in value if unicodedata.category(character) == "Cc"), None)
    first_surrogate = next((character for character in value if unicodedata.category(character) == "Cs"), None)
    if first_control is not None:
        errors.append(error(path, f"text contains a control character U+{ord(first_control):04X}"))
    if first_surrogate is not None:
        errors.append(error(path, f"text contains a surrogate code point U+{ord(first_surrogate):04X}"))
    if unicodedata.normalize("NFC", value) != value:
        errors.append(error(path, "text must use Unicode NFC"))
    if value != value.strip():
        errors.append(error(path, "text must not have leading or trailing whitespace"))
    if first_surrogate is None and len(value.encode("utf-8")) > 16384:
        errors.append(error(path, "text exceeds the 16384-byte UTF-8 bound"))
    return errors


def canonical_language_errors(value: str, path: str) -> list[str]:
    subtags = value.split("-")
    variant_start = 1
    if variant_start < len(subtags):
        script = subtags[variant_start]
        if len(script) == 4 and "A" <= script[0] <= "Z" and all("a" <= character <= "z" for character in script[1:]):
            variant_start += 1
    if variant_start < len(subtags) and (
        len(subtags[variant_start]) == 2 or (len(subtags[variant_start]) == 3 and subtags[variant_start].isdigit())
    ):
        variant_start += 1
    variants = [subtag.lower() for subtag in subtags[variant_start:]]
    if len(variants) != len(set(variants)):
        return [error(path, "language tag contains a repeated variant subtag")]
    return []


def fragment_key(fragment: dict[str, Any]) -> tuple[int, int, int, int]:
    if fragment["kind"] == "text":
        return (
            fragment["paint"]["page"],
            fragment["paint"]["operation"],
            fragment["glyphs"]["start"],
            fragment["text_utf8"]["start"],
        )
    return (fragment["paint"]["page"], fragment["paint"]["operation"], 0, 0)


def graph_errors(document: dict[str, Any]) -> tuple[list[str], GraphState]:
    errors: list[str] = []
    state = GraphState()
    nodes = document["nodes"]
    artifacts = document["artifacts"]
    fragments = document["fragments"]

    if [node["id"] for node in nodes] != list(range(len(nodes))):
        errors.append(error("$.nodes", "node IDs must equal contiguous preorder array positions"))
    if [artifact["id"] for artifact in artifacts] != list(range(len(artifacts))):
        errors.append(error("$.artifacts", "artifact IDs must equal contiguous preorder array positions"))
    if [fragment["id"] for fragment in fragments] != list(range(len(fragments))):
        errors.append(error("$.fragments", "fragment IDs must equal contiguous locator-table positions"))

    fragment_keys = [fragment_key(fragment) for fragment in fragments]
    if fragment_keys != sorted(fragment_keys):
        errors.append(error("$.fragments", "fragments must follow ascending page/paint/range locator order"))
    for index, fragment in enumerate(fragments):
        if fragment["kind"] != "text":
            continue
        for range_name in ("glyphs", "text_utf8"):
            item_range = fragment[range_name]
            if item_range["start"] >= item_range["end"]:
                errors.append(error(f"$.fragments[{index}].{range_name}", "half-open range must be nonempty"))

    node_by_id = {node["id"]: node for node in nodes}
    node_order: list[int] = []
    node_seen: set[int] = set()
    node_visiting: set[int] = set()

    if document["root"] in node_by_id:
        node_stack: list[tuple[int, int | None, int, bool]] = [(document["root"], None, 1, False)]
        while node_stack:
            node_id, parent_id, depth, leaving = node_stack.pop()
            if leaving:
                node_visiting.remove(node_id)
                continue
            if node_id not in node_by_id:
                errors.append(error("$.nodes", f"dangling logical node {node_id}"))
                continue
            if node_id in node_visiting:
                errors.append(error("$.nodes", f"logical node cycle at {node_id}"))
                continue
            if node_id in node_seen:
                errors.append(error("$.nodes", f"logical node {node_id} is referenced more than once"))
                continue
            if depth > MAX_STRUCTURE_DEPTH:
                errors.append(
                    error(
                        f"$.nodes[{node_id}]",
                        f"logical structure depth exceeds v1 maximum of {MAX_STRUCTURE_DEPTH}",
                    )
                )
                continue
            node_visiting.add(node_id)
            node_seen.add(node_id)
            node_order.append(node_id)
            if parent_id is not None:
                state.node_parent[node_id] = parent_id
            children = node_by_id[node_id]["children"]
            for child in children:
                if child["kind"] == "fragment":
                    state.fragment_owners[child["id"]].append(("node", node_id))
            node_stack.append((node_id, parent_id, depth, True))
            node_stack.extend(
                (child["id"], node_id, depth + 1, False) for child in reversed(children) if child["kind"] == "node"
            )
    else:
        errors.append(error("$.root", f"dangling logical root {document['root']}"))
    if node_order != list(range(len(nodes))):
        errors.append(error("$.nodes", "depth-first logical traversal must equal deterministic preorder IDs"))

    roots = document["artifact_roots"]
    if roots != sorted(roots):
        errors.append(error("$.artifact_roots", "artifact roots must be in ascending preorder ID order"))
    artifact_by_id = {artifact["id"]: artifact for artifact in artifacts}
    artifact_order: list[int] = []
    artifact_seen: set[int] = set()
    artifact_visiting: set[int] = set()

    for root in roots:
        artifact_stack: list[tuple[int, int | None, int, bool]] = [(root, None, 1, False)]
        while artifact_stack:
            artifact_id, parent_id, depth, leaving = artifact_stack.pop()
            if leaving:
                artifact_visiting.remove(artifact_id)
                continue
            if artifact_id not in artifact_by_id:
                errors.append(error("$.artifacts", f"dangling artifact {artifact_id}"))
                continue
            if artifact_id in artifact_visiting:
                errors.append(error("$.artifacts", f"artifact cycle at {artifact_id}"))
                continue
            if artifact_id in artifact_seen:
                errors.append(error("$.artifacts", f"artifact {artifact_id} is referenced more than once"))
                continue
            if depth > MAX_STRUCTURE_DEPTH:
                errors.append(
                    error(
                        f"$.artifacts[{artifact_id}]",
                        f"artifact structure depth exceeds v1 maximum of {MAX_STRUCTURE_DEPTH}",
                    )
                )
                continue
            artifact_visiting.add(artifact_id)
            artifact_seen.add(artifact_id)
            artifact_order.append(artifact_id)
            if parent_id is not None:
                state.artifact_parent[artifact_id] = parent_id
            children = artifact_by_id[artifact_id]["children"]
            for child in children:
                if child["kind"] == "fragment":
                    state.fragment_owners[child["id"]].append(("artifact", artifact_id))
            artifact_stack.append((artifact_id, parent_id, depth, True))
            artifact_stack.extend(
                (child["id"], artifact_id, depth + 1, False)
                for child in reversed(children)
                if child["kind"] == "artifact"
            )
    if artifact_order != list(range(len(artifacts))):
        errors.append(error("$.artifacts", "depth-first artifact traversal must equal deterministic preorder IDs"))

    for fragment_id in state.fragment_owners:
        if fragment_id >= len(fragments):
            errors.append(error("$.fragments", f"dangling fragment {fragment_id}"))
    for fragment_id in range(len(fragments)):
        owners = state.fragment_owners.get(fragment_id, [])
        if len(owners) != 1:
            errors.append(
                error(
                    f"$.fragments[{fragment_id}]",
                    f"fragment {fragment_id} must have exactly one logical or artifact owner, got {len(owners)}",
                )
            )
    return errors, state


def direct_node_children(node: dict[str, Any]) -> list[int]:
    return [child["id"] for child in node["children"] if child["kind"] == "node"]


def first_node_index(document: dict[str, Any], role: str) -> int:
    for index, node in enumerate(document["nodes"]):
        if node["role"] == role:
            return index
    raise AssertionError(f"representative fixture has no {role!r} node")


def role_errors(document: dict[str, Any], api2: ModuleType, state: GraphState) -> list[str]:
    errors: list[str] = []
    nodes = document["nodes"]

    for index, node in enumerate(nodes):
        path = f"$.nodes[{index}]"
        role = node["role"]
        semantics_kind = node["semantics"]["kind"]
        expected_kind = "none" if role in PLAIN_ROLES else ROLE_SEMANTICS[role]
        if semantics_kind != expected_kind:
            errors.append(error(f"{path}.semantics", f"role {role!r} requires semantics kind {expected_kind!r}"))
        if role == "document" and index != document["root"]:
            errors.append(error(f"{path}.role", "document is reserved for the single logical root"))
        if index == document["root"] and role != "document":
            errors.append(error(f"{path}.role", "logical root must have document role"))

        for field_name in ("name", "alternate_text", "replacement_text"):
            value = node[field_name]
            if value is not None:
                errors.extend(canonical_text_errors(value, f"{path}.{field_name}"))
        if node["language"] is not None:
            errors.extend(canonical_language_errors(node["language"], f"{path}.language"))
        if role in {"figure", "formula"} and node["alternate_text"] is None:
            errors.append(error(f"{path}.alternate_text", f"meaningful {role}s require alternate text"))
        if role not in {"figure", "formula", "link"} and node["alternate_text"] is not None:
            errors.append(error(f"{path}.alternate_text", "alternate text is reserved for figure, formula, or link"))
        if node["replacement_text"] is not None and role not in {"span", "code", "figure", "formula"}:
            errors.append(
                error(f"{path}.replacement_text", "replacement text is allowed only on span, code, figure, or formula")
            )
        if role == "heading" and node["name"] is None:
            errors.append(error(f"{path}.name", "headings require a canonical title"))
        if role == "link":
            if node["name"] is None:
                errors.append(error(f"{path}.name", "tagged annotations require a non-null title"))
            if node["alternate_text"] is None:
                errors.append(error(f"{path}.alternate_text", "tagged annotations require alternate text"))
            if semantics_kind == "link" and not api2.canonical_link_target(node["semantics"]["target"]):
                errors.append(error(f"{path}.semantics.target", "target is not a canonical absolute URL"))

    for index, node in enumerate(nodes):
        path = f"$.nodes[{index}]"
        role = node["role"]
        parent = state.node_parent.get(index)
        parent_role = nodes[parent]["role"] if parent is not None else None
        if role == "list-item" and parent_role != "list":
            errors.append(error(path, "list-item must be a direct child of list"))
        if role in {"list-label", "list-body"} and parent_role != "list-item":
            errors.append(error(path, f"{role} must be a direct child of list-item"))
        if role in {"table-head", "table-body", "table-foot"} and parent_role != "table":
            errors.append(error(path, f"{role} must be a direct child of table"))
        if role == "table-row" and parent_role not in {"table-head", "table-body", "table-foot"}:
            errors.append(error(path, "table-row must be a direct child of a table row group"))
        if role in {"table-header-cell", "table-cell"} and parent_role != "table-row":
            errors.append(error(path, f"{role} must be a direct child of table-row"))

    heading_ids = [
        node["id"]
        for node in nodes
        if node["role"] == "heading" and node["semantics"].get("kind") == "heading" and "level" in node["semantics"]
    ]
    if heading_ids:
        first_level = nodes[heading_ids[0]]["semantics"]["level"]
        if first_level != 1:
            errors.append(error(f"$.nodes[{heading_ids[0]}].semantics.level", "first numbered heading must be level 1"))
        previous_level = first_level
        for heading_id in heading_ids[1:]:
            level = nodes[heading_id]["semantics"]["level"]
            if level > previous_level + 1:
                errors.append(
                    error(f"$.nodes[{heading_id}].semantics.level", "heading levels must not skip on descent")
                )
            previous_level = level

    errors.extend(list_errors(document))
    errors.extend(table_errors(document))
    return errors


def list_errors(document: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    nodes = document["nodes"]
    for node in nodes:
        if node["role"] != "list":
            continue
        path = f"$.nodes[{node['id']}]"
        if any(child["kind"] != "node" for child in node["children"]):
            errors.append(error(f"{path}.children", "list reading order may contain only list-item nodes"))
        item_ids = direct_node_children(node)
        if not item_ids or any(nodes[item_id]["role"] != "list-item" for item_id in item_ids):
            errors.append(error(f"{path}.children", "list requires one or more direct list-item children"))
            continue
        semantics = node["semantics"]
        if semantics["kind"] != "list":
            continue
        if semantics["ordered"] and semantics["start"] is None:
            errors.append(error(f"{path}.semantics.start", "ordered lists require an integer start"))
            continue
        if semantics["ordered"] and semantics["numbering"] == "none":
            errors.append(error(f"{path}.semantics.numbering", "ordered lists require an explicit numbering system"))
        if not semantics["ordered"] and semantics["start"] is not None:
            errors.append(error(f"{path}.semantics.start", "unordered lists require null start"))
        if not semantics["ordered"] and semantics["numbering"] != "none":
            errors.append(error(f"{path}.semantics.numbering", "unordered lists require numbering none"))
        first = semantics["start"] if semantics["ordered"] else 1
        for offset, item_id in enumerate(item_ids):
            item = nodes[item_id]
            item_semantics = item["semantics"]
            if item_semantics["kind"] == "list-item" and item_semantics["ordinal"] != first + offset:
                errors.append(
                    error(
                        f"$.nodes[{item_id}].semantics.ordinal",
                        "list-item ordinals must follow canonical list order",
                    )
                )
            if any(child["kind"] != "node" for child in item["children"]):
                errors.append(
                    error(f"$.nodes[{item_id}].children", "list-item may directly contain only label/body nodes")
                )
                continue
            child_roles = [nodes[child_id]["role"] for child_id in direct_node_children(item)]
            if child_roles != ["list-label", "list-body"]:
                errors.append(
                    error(
                        f"$.nodes[{item_id}].children",
                        "list-item requires exactly list-label then list-body",
                    )
                )
    return errors


def table_errors(document: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    nodes = document["nodes"]
    declared_table_slots = 0
    for node in nodes:
        if node["role"] != "table" or node["semantics"]["kind"] != "table":
            continue
        table_slots = node["semantics"]["rows"] * node["semantics"]["columns"]
        declared_table_slots += table_slots
        if table_slots > MAX_TABLE_SLOTS:
            errors.append(
                error(
                    f"$.nodes[{node['id']}].semantics",
                    f"table grid exceeds v1 maximum of {MAX_TABLE_SLOTS} slots",
                )
            )
    if declared_table_slots > MAX_TABLE_SLOTS:
        errors.append(
            error(
                "$.nodes",
                f"document table grids exceed v1 maximum of {MAX_TABLE_SLOTS} declared slots",
            )
        )
        return errors

    for table in nodes:
        if table["role"] != "table" or table["semantics"]["kind"] != "table":
            continue
        table_slots = table["semantics"]["rows"] * table["semantics"]["columns"]
        table_path = f"$.nodes[{table['id']}]"
        if any(child["kind"] != "node" for child in table["children"]):
            errors.append(error(f"{table_path}.children", "table may directly contain only row-group nodes"))
            continue
        group_ids = direct_node_children(table)
        group_roles = [nodes[group_id]["role"] for group_id in group_ids]
        if (
            not group_ids
            or any(role not in TABLE_GROUP_ORDER for role in group_roles)
            or group_roles != sorted(group_roles, key=TABLE_GROUP_ORDER.__getitem__)
            or len(group_roles) != len(set(group_roles))
            or "table-body" not in group_roles
        ):
            errors.append(error(f"{table_path}.children", "table groups must be unique and ordered head, body, foot"))
            continue

        row_ids: list[int] = []
        for group_id in group_ids:
            group = nodes[group_id]
            if any(child["kind"] != "node" for child in group["children"]):
                errors.append(error(f"$.nodes[{group_id}].children", "table row groups may contain only rows"))
                continue
            group_rows = direct_node_children(group)
            if any(nodes[row_id]["role"] != "table-row" for row_id in group_rows):
                errors.append(error(f"$.nodes[{group_id}].children", "table row groups may contain only rows"))
            row_ids.extend(group_rows)

        rows = table["semantics"]["rows"]
        columns = table["semantics"]["columns"]
        if len(row_ids) != rows:
            errors.append(error(f"{table_path}.semantics.rows", "declared table row count does not match structure"))
            continue

        occupancy: set[tuple[int, int]] = set()
        expanded_slots = 0
        header_ids: set[int] = set()
        cell_ids: list[int] = []
        for expected_row, row_id in enumerate(row_ids):
            row_node = nodes[row_id]
            if any(child["kind"] != "node" for child in row_node["children"]):
                errors.append(error(f"$.nodes[{row_id}].children", "table rows may directly contain only cells"))
                continue
            row_cells = direct_node_children(row_node)
            if not row_cells or any(
                nodes[cell_id]["role"] not in {"table-header-cell", "table-cell"} for cell_id in row_cells
            ):
                errors.append(error(f"$.nodes[{row_id}].children", "table row requires one or more cell children"))
                continue
            row_cell_semantics = [nodes[cell_id]["semantics"] for cell_id in row_cells]
            if all(candidate["kind"] == "table-cell" for candidate in row_cell_semantics):
                columns_in_order = [candidate["column"] for candidate in row_cell_semantics]
                if columns_in_order != sorted(columns_in_order):
                    errors.append(
                        error(f"$.nodes[{row_id}].children", "table cells must be ordered by starting column")
                    )
            for cell_id in row_cells:
                cell = nodes[cell_id]
                cell_semantics = cell["semantics"]
                if cell_semantics["kind"] != "table-cell":
                    continue
                cell_ids.append(cell_id)
                if cell_semantics["row"] != expected_row:
                    errors.append(error(f"$.nodes[{cell_id}].semantics.row", "cell row must equal canonical row order"))
                if (
                    cell_semantics["row"] + cell_semantics["row_span"] > rows
                    or cell_semantics["column"] + cell_semantics["column_span"] > columns
                ):
                    errors.append(
                        error(f"$.nodes[{cell_id}].semantics", "table cell span exceeds declared table bounds")
                    )
                    continue
                span_slots = cell_semantics["row_span"] * cell_semantics["column_span"]
                if expanded_slots + span_slots > table_slots:
                    errors.append(error(table_path, "total table cell spans exceed the declared grid"))
                    continue
                expanded_slots += span_slots
                if cell["role"] == "table-header-cell":
                    header_ids.add(cell_id)
                    if cell_semantics["scope"] == "none":
                        errors.append(
                            error(f"$.nodes[{cell_id}].semantics.scope", "header cells require explicit scope")
                        )
                    if cell_semantics["headers"]:
                        errors.append(
                            error(f"$.nodes[{cell_id}].semantics.headers", "header cells cannot name other headers")
                        )
                elif cell_semantics["scope"] != "none":
                    errors.append(error(f"$.nodes[{cell_id}].semantics.scope", "data cells require scope none"))
                for occupied_row in range(cell_semantics["row"], cell_semantics["row"] + cell_semantics["row_span"]):
                    for occupied_column in range(
                        cell_semantics["column"], cell_semantics["column"] + cell_semantics["column_span"]
                    ):
                        position = (occupied_row, occupied_column)
                        if position in occupancy:
                            errors.append(error(f"$.nodes[{cell_id}].semantics", f"table cell overlap at {position}"))
                        else:
                            occupancy.add(position)

        if len(occupancy) != table_slots:
            errors.append(error(table_path, "table cells must cover the declared grid exactly"))
        for cell_id in cell_ids:
            cell = nodes[cell_id]
            headers = cell["semantics"]["headers"]
            if headers != sorted(headers):
                errors.append(
                    error(f"$.nodes[{cell_id}].semantics.headers", "header references must be ascending node IDs")
                )
            for header_id in headers:
                if header_id not in header_ids:
                    errors.append(
                        error(
                            f"$.nodes[{cell_id}].semantics.headers",
                            f"header reference {header_id} is not a header cell in this table",
                        )
                    )
    return errors


def node_has_ancestor_role(document: dict[str, Any], state: GraphState, node_id: int, role: str) -> int | None:
    current: int | None = node_id
    while current is not None:
        if document["nodes"][current]["role"] == role:
            return current
        current = state.node_parent.get(current)
    return None


def subtree_fragment_ids(document: dict[str, Any], node_id: int) -> tuple[set[int], bool]:
    fragments: set[int] = set()
    visited: set[int] = set()
    depth_exceeded = False
    stack = [(node_id, 1)]
    while stack:
        current_id, depth = stack.pop()
        if depth > MAX_STRUCTURE_DEPTH:
            depth_exceeded = True
            continue
        if current_id in visited or not 0 <= current_id < len(document["nodes"]):
            continue
        visited.add(current_id)
        for child in document["nodes"][current_id]["children"]:
            if child["kind"] == "node":
                stack.append((child["id"], depth + 1))
            else:
                fragments.add(child["id"])
    return fragments, depth_exceeded


def policy_errors(document: dict[str, Any], state: GraphState) -> list[str]:
    errors: list[str] = []
    policy = document["policy"]
    logical_owned = sum(1 for owners in state.fragment_owners.values() if len(owners) == 1 and owners[0][0] == "node")
    if policy["logical_content"] == "required" and (len(document["nodes"]) <= 1 or logical_owned == 0):
        errors.append(error("$.policy.logical_content", "required logical content rejects an empty semantic tree"))
    if policy["logical_content"] == "allow-empty" and len(document["nodes"]) > 1:
        errors.append(error("$.policy.logical_content", "allow-empty is canonical only for a root-only semantic tree"))
    if policy["navigation"] == "required" and document["navigation"]["kind"] != "outline":
        errors.append(error("$.policy.navigation", "required navigation needs a nonempty outline"))
    if policy["navigation"] == "explicit-none" and document["navigation"]["kind"] != "none":
        errors.append(error("$.policy.navigation", "explicit-none navigation must use the typed none variant"))
    if document["navigation"]["kind"] == "none" and any(node["role"] == "heading" for node in document["nodes"]):
        errors.append(error("$.navigation", "none is invalid when the semantic tree contains headings"))
    return errors


def artifact_errors(document: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    for index, artifact in enumerate(document["artifacts"]):
        path = f"$.artifacts[{index}].subtype"
        if artifact["kind"] == "pagination" and artifact["subtype"] is None:
            errors.append(error(path, "pagination artifacts require header, footer, or page-number subtype"))
        if artifact["kind"] != "pagination" and artifact["subtype"] is not None:
            errors.append(error(path, "non-pagination artifacts require null subtype"))
    return errors


def navigation_errors(document: dict[str, Any], scene: dict[str, Any]) -> list[str]:
    navigation = document["navigation"]
    if navigation["kind"] == "none":
        return []

    errors: list[str] = []
    items = navigation["items"]
    if [item["id"] for item in items] != list(range(len(items))):
        errors.append(error("$.navigation.items", "outline IDs must equal contiguous preorder array positions"))
    item_by_id = {item["id"]: item for item in items}
    order: list[int] = []
    seen: set[int] = set()
    visiting: set[int] = set()

    outline_stack = [(root_id, 1, False) for root_id in reversed(navigation["roots"])]
    while outline_stack:
        item_id, depth, leaving = outline_stack.pop()
        if leaving:
            visiting.remove(item_id)
            continue
        if item_id not in item_by_id:
            errors.append(error("$.navigation", f"dangling outline item {item_id}"))
            continue
        if item_id in visiting:
            errors.append(error("$.navigation", f"outline cycle at {item_id}"))
            continue
        if item_id in seen:
            errors.append(error("$.navigation", f"outline item {item_id} is referenced more than once"))
            continue
        if depth > MAX_STRUCTURE_DEPTH:
            errors.append(
                error(
                    f"$.navigation.items[{item_id}]",
                    f"outline structure depth exceeds v1 maximum of {MAX_STRUCTURE_DEPTH}",
                )
            )
            continue
        visiting.add(item_id)
        seen.add(item_id)
        order.append(item_id)
        outline_stack.append((item_id, depth, True))
        outline_stack.extend((child_id, depth + 1, False) for child_id in reversed(item_by_id[item_id]["children"]))
    if order != list(range(len(items))):
        errors.append(error("$.navigation", "outline traversal must equal deterministic preorder IDs"))

    nodes = document["nodes"]
    fragments = document["fragments"]
    pages = {page["number"]: page for page in scene["pages"]}
    targets: list[int] = []
    for index, item in enumerate(items):
        path = f"$.navigation.items[{index}]"
        errors.extend(canonical_text_errors(item["title"], f"{path}.title"))
        if item["language"] is not None:
            errors.extend(canonical_language_errors(item["language"], f"{path}.language"))
        target = item["target_node"]
        targets.append(target)
        if target >= len(nodes):
            errors.append(error(f"{path}.target_node", f"outline target node {target} does not exist"))
            continue
        if nodes[target]["role"] == "heading" and item["title"] != nodes[target]["name"]:
            errors.append(error(f"{path}.title", "heading outline title must equal the canonical heading title"))
        destination = item["destination"]
        page_number = destination["page"]
        if page_number not in pages:
            errors.append(error(f"{path}.destination.page", f"outline destination page {page_number} does not exist"))
            continue
        page_size = pages[page_number]["size_app_units"]
        if destination["x_app_units"] >= page_size["width"] or destination["y_app_units"] >= page_size["height"]:
            errors.append(error(f"{path}.destination", "outline destination is outside the page box"))
        subtree_fragments, subtree_depth_exceeded = subtree_fragment_ids(document, target)
        if subtree_depth_exceeded:
            errors.append(
                error(
                    f"{path}.target_node",
                    f"outline target subtree depth exceeds v1 maximum of {MAX_STRUCTURE_DEPTH}",
                )
            )
        fragment_pages = {
            fragments[fragment_id]["paint"]["page"] for fragment_id in subtree_fragments if fragment_id < len(fragments)
        }
        if page_number not in fragment_pages:
            errors.append(
                error(f"{path}.destination.page", "destination page is not represented in the target node subtree")
            )
    if len(targets) != len(set(targets)):
        errors.append(error("$.navigation.items", "outline target nodes must be unique"))
    return errors


def scene_binding_errors(document: dict[str, Any], scene: dict[str, Any], state: GraphState) -> list[str]:
    errors: list[str] = []
    pages = {page["number"]: page for page in scene["pages"]}
    by_paint: dict[tuple[int, int], list[dict[str, Any]]] = defaultdict(list)

    for fragment in document["fragments"]:
        page_number = fragment["paint"]["page"]
        operation_index = fragment["paint"]["operation"]
        path = f"$.fragments[{fragment['id']}]"
        if page_number not in pages:
            errors.append(error(f"{path}.paint.page", f"page {page_number} does not exist"))
            continue
        operations = pages[page_number]["operations"]
        if operation_index >= len(operations):
            errors.append(error(f"{path}.paint.operation", f"paint operation {operation_index} does not exist"))
            continue
        operation = operations[operation_index]
        expected_operation_kind = PAINT_KIND[fragment["kind"]]
        if operation["type"] != expected_operation_kind:
            errors.append(
                error(
                    f"{path}.kind",
                    f"fragment kind {fragment['kind']!r} does not match paint operation {operation['type']!r}",
                )
            )
            continue
        by_paint[(page_number, operation_index)].append(fragment)

        owners = state.fragment_owners.get(fragment["id"], [])
        if len(owners) != 1:
            continue
        owner_kind, owner_id = owners[0]
        if fragment["kind"] == "annotation":
            if owner_kind != "node":
                errors.append(error(path, "annotations cannot belong to the artifact subtree"))
                continue
            link_id = node_has_ancestor_role(document, state, owner_id, "link")
            if link_id is None:
                errors.append(error(path, "annotation requires a logical link ancestor"))
            elif (
                document["nodes"][link_id]["semantics"]["kind"] == "link"
                and document["nodes"][link_id]["semantics"]["target"] != operation["target"]
            ):
                errors.append(error(path, "link semantic target does not equal annotation paint target"))
        if fragment["kind"] in {"image", "path"} and owner_kind == "node":
            figure_id = node_has_ancestor_role(document, state, owner_id, "figure")
            formula_id = node_has_ancestor_role(document, state, owner_id, "formula")
            semantic_image_id = figure_id if figure_id is not None else formula_id
            if semantic_image_id is None or document["nodes"][semantic_image_id]["alternate_text"] is None:
                errors.append(error(path, "logical graphic requires a figure or formula ancestor with alternate text"))

    for page in scene["pages"]:
        for operation_index, operation in enumerate(page["operations"]):
            paint_path = f"page {page['number']} operation {operation_index}"
            mapped = by_paint.get((page["number"], operation_index), [])
            if operation["type"] != "text":
                if len(mapped) != 1:
                    errors.append(
                        error("$.fragments", f"{paint_path} must have exactly one fragment, got {len(mapped)}")
                    )
                continue
            if not mapped:
                errors.append(error("$.fragments", f"{paint_path} text has no semantic or artifact fragments"))
                continue
            text_size = len(operation["text"].encode("utf-8"))
            boundaries = set(api2_utf8_boundaries(operation["text"]))
            glyph_cursor = 0
            text_cursor = 0
            for fragment in sorted(mapped, key=fragment_key):
                glyph_range = fragment["glyphs"]
                text_range = fragment["text_utf8"]
                if glyph_range["end"] > len(operation["glyphs"]):
                    errors.append(
                        error(f"$.fragments[{fragment['id']}].glyphs", "glyph range is outside the paint operation")
                    )
                elif glyph_range["start"] < glyph_range["end"]:
                    selected_glyphs = operation["glyphs"][glyph_range["start"] : glyph_range["end"]]
                    expected_text = {
                        "start": selected_glyphs[0]["text_range"]["start"],
                        "end": selected_glyphs[-1]["text_range"]["end"],
                    }
                    if text_range != expected_text:
                        errors.append(
                            error(
                                f"$.fragments[{fragment['id']}].text_utf8",
                                "text UTF-8 range does not match the selected glyphs",
                            )
                        )
                if (
                    text_range["end"] > text_size
                    or text_range["start"] not in boundaries
                    or text_range["end"] not in boundaries
                ):
                    errors.append(
                        error(
                            f"$.fragments[{fragment['id']}].text_utf8",
                            "text UTF-8 range is outside the paint operation or not on a code-point boundary",
                        )
                    )
                if glyph_range["start"] != glyph_cursor or text_range["start"] != text_cursor:
                    errors.append(
                        error(f"$.fragments[{fragment['id']}]", "text fragments overlap or leave a coverage gap")
                    )
                glyph_cursor = glyph_range["end"]
                text_cursor = text_range["end"]
            if glyph_cursor != len(operation["glyphs"]) or text_cursor != text_size:
                errors.append(error("$.fragments", f"{paint_path} text ranges do not cover the operation exactly"))
    return errors


def api2_utf8_boundaries(value: str) -> list[int]:
    boundaries = [0]
    length = 0
    for character in value:
        length += len(character.encode("utf-8"))
        boundaries.append(length)
    return boundaries


def semantic_errors(document: dict[str, Any], scene: dict[str, Any], api2: ModuleType) -> list[str]:
    errors: list[str] = []
    if not api2.safe_relative_path(document["source"]["entrypoint"]):
        errors.append(error("$.source.entrypoint", "entrypoint is not a portable relative path"))
    errors.extend(canonical_text_errors(document["metadata"]["title"], "$.metadata.title"))
    errors.extend(canonical_language_errors(document["language"], "$.language"))
    if document["metadata"]["language"] is not None:
        errors.extend(canonical_language_errors(document["metadata"]["language"], "$.metadata.language"))
    graph_failures, state = graph_errors(document)
    errors.extend(graph_failures)
    if graph_failures:
        return errors
    errors.extend(policy_errors(document, state))
    errors.extend(artifact_errors(document))
    errors.extend(role_errors(document, api2, state))
    errors.extend(navigation_errors(document, scene))
    errors.extend(scene_binding_errors(document, scene, state))
    return errors


def contract_errors(
    document: dict[str, Any],
    scene: dict[str, Any],
    api2: ModuleType,
    schema: dict[str, Any],
) -> list[str]:
    schema_failures = [str(item) for item in api2.validate(document, schema, SCHEMA_NAME)]
    if schema_failures:
        return schema_failures
    order_failures = [str(item) for item in api2.member_order_semantics(document, schema, SCHEMA_NAME)]
    if order_failures:
        return order_failures
    return semantic_errors(document, scene, api2)


def pointer_parent(value: Any, pointer: str) -> tuple[Any, str]:
    if not pointer.startswith("/"):
        raise AssertionError(f"mutation pointer must be absolute: {pointer!r}")
    parts = [part.replace("~1", "/").replace("~0", "~") for part in pointer[1:].split("/")]
    current = value
    for part in parts[:-1]:
        current = current[int(part)] if isinstance(current, list) else current[part]
    return current, parts[-1]


def pointer_get(value: Any, pointer: str) -> Any:
    parent, key = pointer_parent(value, pointer)
    return parent[int(key)] if isinstance(parent, list) else parent[key]


def pointer_set(value: Any, pointer: str, replacement: Any, *, add: bool) -> None:
    parent, key = pointer_parent(value, pointer)
    if isinstance(parent, list):
        index = int(key)
        if add:
            raise AssertionError("rejection corpus does not use list insertion")
        parent[index] = replacement
        return
    if add and key in parent:
        raise AssertionError(f"add mutation would replace existing key at {pointer}")
    if not add and key not in parent:
        raise AssertionError(f"replace mutation targets missing key at {pointer}")
    parent[key] = replacement


def pointer_remove(value: Any, pointer: str) -> None:
    parent, key = pointer_parent(value, pointer)
    if isinstance(parent, list):
        del parent[int(key)]
        return
    if key not in parent:
        raise AssertionError(f"remove mutation targets missing key at {pointer}")
    del parent[key]


def apply_mutations(base: dict[str, Any], mutations: list[dict[str, Any]]) -> dict[str, Any]:
    candidate = copy.deepcopy(base)
    for mutation in mutations:
        operation = mutation["operation"]
        if operation == "swap":
            expected_keys = {"operation", "path", "other"}
        elif operation == "remove":
            expected_keys = {"operation", "path"}
        else:
            expected_keys = {"operation", "path", "value"}
        if set(mutation) != expected_keys:
            raise AssertionError(f"mutation has unexpected fields: {sorted(mutation)}")
        if operation == "replace":
            pointer_set(candidate, mutation["path"], copy.deepcopy(mutation["value"]), add=False)
        elif operation == "add":
            pointer_set(candidate, mutation["path"], copy.deepcopy(mutation["value"]), add=True)
        elif operation == "remove":
            pointer_remove(candidate, mutation["path"])
        elif operation == "swap":
            left = copy.deepcopy(pointer_get(candidate, mutation["path"]))
            right = copy.deepcopy(pointer_get(candidate, mutation["other"]))
            pointer_set(candidate, mutation["path"], right, add=False)
            pointer_set(candidate, mutation["other"], left, add=False)
        else:
            raise AssertionError(f"unsupported mutation operation {operation!r}")
    return candidate


def verify_rejection_corpus(
    base: dict[str, Any],
    scene: dict[str, Any],
    api2: ModuleType,
    schema: dict[str, Any],
) -> int:
    corpus = api2.load_json(REJECTION_CORPUS_PATH)
    if set(corpus) != {"schema", "version", "base", "cases"}:
        raise AssertionError("rejection corpus envelope is not closed")
    if corpus["schema"] != "pliego.document-semantics-rejection-corpus" or corpus["version"] != 1:
        raise AssertionError("unsupported rejection corpus schema/version")
    if corpus["base"] != "../accepted/representative.json":
        raise AssertionError("rejection corpus base drifted")
    names: list[str] = []
    for case in corpus["cases"]:
        if set(case) != {"name", "mutations", "expected"}:
            raise AssertionError("rejection case envelope is not closed")
        names.append(case["name"])
        candidate = apply_mutations(base, case["mutations"])
        failures = contract_errors(candidate, scene, api2, schema)
        rendered = "\n".join(failures)
        if not failures:
            raise AssertionError(f"rejection case {case['name']!r} was accepted")
        if case["expected"] not in rendered:
            raise AssertionError(f"rejection case {case['name']!r} did not report {case['expected']!r}:\n{rendered}")
    if len(names) != len(set(names)):
        raise AssertionError("rejection case names must be unique")
    return len(names)


def verify_generated_adversaries(
    base: dict[str, Any],
    scene: dict[str, Any],
    api2: ModuleType,
    schema: dict[str, Any],
) -> int:
    rejection_count = 0

    def require_rejection(candidate: dict[str, Any], expected: str, name: str) -> None:
        nonlocal rejection_count
        failures = contract_errors(candidate, scene, api2, schema)
        rendered = "\n".join(failures)
        if expected not in rendered:
            raise AssertionError(f"generated adversary {name!r} did not report {expected!r}:\n{rendered}")
        rejection_count += 1

    for language in ("de-DE", "de-1901"):
        canonical_language = copy.deepcopy(base)
        canonical_language["language"] = language
        language_failures = contract_errors(canonical_language, scene, api2, schema)
        if language_failures:
            raise AssertionError(f"canonical language {language} was rejected:\n" + "\n".join(language_failures))

    repeated_variant = copy.deepcopy(base)
    repeated_variant["language"] = "sl-rozaj-rozaj"
    require_rejection(repeated_variant, "repeated variant subtag", "repeated BCP 47 variant")

    repeated_numeric_variant = copy.deepcopy(base)
    repeated_numeric_variant["language"] = "de-1901-1901"
    require_rejection(repeated_numeric_variant, "repeated variant subtag", "repeated numeric BCP 47 variant")

    c1_control = copy.deepcopy(base)
    c1_control["metadata"]["title"] = "Invoice\u0085copy"
    require_rejection(c1_control, "control character U+0085", "Unicode Cc")

    lone_surrogate = copy.deepcopy(base)
    lone_surrogate["metadata"]["title"] = "\ud800"
    require_rejection(lone_surrogate, "surrogate code point U+D800", "lone surrogate")

    for entrypoint in ("../secret", "./secret", "/etc/passwd", "a//b"):
        unsafe_entrypoint = copy.deepcopy(base)
        unsafe_entrypoint["source"]["entrypoint"] = entrypoint
        require_rejection(unsafe_entrypoint, "does not match pattern", f"unsafe entrypoint {entrypoint!r}")

    dangling_logical_child = copy.deepcopy(base)
    dangling_logical_child["nodes"][2]["children"][0] = {"kind": "node", "id": 999}
    require_rejection(dangling_logical_child, "dangling logical node 999", "dangling logical child")

    table_explosion = copy.deepcopy(base)
    table_index = first_node_index(table_explosion, "table")
    table_explosion["nodes"][table_index]["semantics"]["rows"] = 65535
    table_explosion["nodes"][table_index]["semantics"]["columns"] = 65535
    require_rejection(
        table_explosion,
        f"table grid exceeds v1 maximum of {MAX_TABLE_SLOTS} slots",
        "table slot explosion",
    )

    aggregate_table_explosion = copy.deepcopy(base)
    table_index = first_node_index(aggregate_table_explosion, "table")
    aggregate_table_explosion["nodes"][table_index]["semantics"]["rows"] = 10
    aggregate_table_explosion["nodes"][table_index]["semantics"]["columns"] = 60000
    second_table = copy.deepcopy(aggregate_table_explosion["nodes"][table_index])
    second_table["id"] = len(aggregate_table_explosion["nodes"])
    second_table["source"] = {"kind": "dom", "preorder": second_table["id"]}
    second_table["children"] = []
    aggregate_table_explosion["nodes"].append(second_table)
    aggregate_table_explosion["nodes"][0]["children"].append({"kind": "node", "id": second_table["id"]})
    require_rejection(
        aggregate_table_explosion,
        f"document table grids exceed v1 maximum of {MAX_TABLE_SLOTS} declared slots",
        "document-wide table slot explosion",
    )

    for role, expected_kind in (
        ("heading", "heading"),
        ("list", "list"),
        ("list-item", "list-item"),
        ("table", "table"),
        ("table-cell", "table-cell"),
        ("link", "link"),
    ):
        mismatched_semantics = copy.deepcopy(base)
        node_id = first_node_index(mismatched_semantics, role)
        mismatched_semantics["nodes"][node_id]["semantics"] = {"kind": "none"}
        require_rejection(
            mismatched_semantics,
            f"role {role!r} requires semantics kind {expected_kind!r}",
            f"{role} semantics mismatch without exception",
        )

    chain_length = 1050
    logical_chain = copy.deepcopy(base)
    logical_chain["policy"]["navigation"] = "explicit-none"
    logical_chain["navigation"] = {"kind": "none"}
    logical_chain["nodes"] = [
        {
            "id": node_id,
            "role": "document" if node_id == 0 else "section",
            "source": {"kind": "dom", "preorder": node_id},
            "language": None,
            "name": None,
            "alternate_text": None,
            "replacement_text": None,
            "semantics": {"kind": "none"},
            "children": (
                [{"kind": "node", "id": node_id + 1}]
                if node_id + 1 < chain_length
                else [
                    {"kind": "fragment", "id": 0},
                    {"kind": "fragment", "id": 2},
                    {"kind": "fragment", "id": 3},
                ]
            ),
        }
        for node_id in range(chain_length)
    ]
    require_rejection(
        logical_chain,
        f"logical structure depth exceeds v1 maximum of {MAX_STRUCTURE_DEPTH}",
        "1050-node logical chain",
    )

    artifact_chain = copy.deepcopy(base)
    artifact_chain["artifact_roots"] = [0]
    artifact_chain["artifacts"] = [
        {
            "id": artifact_id,
            "kind": "decoration",
            "subtype": None,
            "source": {"kind": "generated", "owner_preorder": 0, "slot": "after"},
            "children": (
                [{"kind": "artifact", "id": artifact_id + 1}]
                if artifact_id + 1 < chain_length
                else [{"kind": "fragment", "id": 1}]
            ),
        }
        for artifact_id in range(chain_length)
    ]
    require_rejection(
        artifact_chain,
        f"artifact structure depth exceeds v1 maximum of {MAX_STRUCTURE_DEPTH}",
        "1050-node artifact chain",
    )

    outline_chain = copy.deepcopy(base)
    outline_chain["navigation"] = {
        "kind": "outline",
        "roots": [0],
        "items": [
            {
                "id": item_id,
                "title": "Invoice",
                "language": None,
                "target_node": 1,
                "destination": {"page": 1, "x_app_units": 2880, "y_app_units": 4320},
                "children": [item_id + 1] if item_id + 1 < chain_length else [],
            }
            for item_id in range(chain_length)
        ],
    }
    require_rejection(
        outline_chain,
        f"outline structure depth exceeds v1 maximum of {MAX_STRUCTURE_DEPTH}",
        "1050-item outline chain",
    )

    return rejection_count


def verify_api2_boundary(document: dict[str, Any], digest: str, api2: ModuleType) -> None:
    if document["profile"] != PDFUA1_PROFILE:
        raise AssertionError("accepted semantic fixture must bind the selected exact PDF/UA-1 profile reference")
    expected_policy = {
        "logical_content": "required",
        "navigation": "required",
        "paint_coverage": "complete",
    }
    if document["policy"] != expected_policy:
        raise AssertionError(
            "the selected PDF/UA-1 fixture must fail closed on empty structure, outline, or paint coverage"
        )
    runtime = api2.load_json(RUNTIME_PATH)
    request = api2.load_json(REQUEST_PATH)
    scene = api2.load_json(SCENE_PATH)
    if any(contract["profiles"] for contract in runtime["contracts"]):
        raise AssertionError("API 2 runtime fixture must continue advertising no profiles")
    if scene["semantic_layer"] is not None:
        raise AssertionError("pre-R3.5 API 2 fixture must continue making no semantic-layer claim")

    semantic_ref = {
        "schema": document["schema"],
        "version": document["version"],
        "profile": copy.deepcopy(document["profile"]),
        "resource": digest,
        "media_type": "application/vnd.pliego.document-semantics+json",
    }
    scene_ref_schema = api2.SCHEMAS["document-scene.v1.json"]["definitions"]["semantic_layer_ref"]
    ref_failures = api2.validate(semantic_ref, scene_ref_schema, "document-scene.v1.json")
    ref_failures += api2.member_order_semantics(semantic_ref, scene_ref_schema, "document-scene.v1.json")
    if ref_failures:
        raise AssertionError("canonical semantics do not fit API 2's reserved semantic-layer slot")

    request["profile"] = copy.deepcopy(document["profile"])
    if api2.validate(request, api2.SCHEMAS["render-request.v1.json"], "render-request.v1.json"):
        raise AssertionError("API 2 generic request slot rejected the exact semantic profile reference")
    negotiation_failures = api2.request_runtime_semantics(request, runtime)
    if not any("not advertised" in str(item) for item in negotiation_failures):
        raise AssertionError("the current runtime fixture must reject the unadvertised PDF/UA-1 profile")


def emit_digest(path: Path) -> int:
    api2, schema = prepare_contract()
    document = api2.load_json(path)
    scene = api2.load_json(SCENE_PATH)
    failures = contract_errors(document, scene, api2, schema)
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(semantic_digest(document))
    return 0


def verify_fresh_processes(expected: str) -> None:
    command = [sys.executable, "-I", str(Path(__file__).resolve()), "--emit-digest", str(ACCEPTED_PATH)]
    for index in range(FRESH_PROCESS_COUNT):
        completed = subprocess.run(command, check=False, capture_output=True, text=True, encoding="utf-8")
        actual = completed.stdout.strip()
        if completed.returncode != 0 or actual != expected:
            raise AssertionError(
                f"fresh process {index + 1} did not reproduce {expected}: "
                f"exit={completed.returncode}, stdout={actual!r}, stderr={completed.stderr!r}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--emit-digest", type=Path)
    args = parser.parse_args()
    if args.emit_digest is not None:
        return emit_digest(args.emit_digest)

    api2, schema = prepare_contract()
    document = api2.load_json(ACCEPTED_PATH)
    scene = api2.load_json(SCENE_PATH)
    failures = contract_errors(document, scene, api2, schema)
    if failures:
        raise AssertionError("accepted representative semantic document failed:\n" + "\n".join(failures))
    if ACCEPTED_PATH.read_bytes() != canonical_json_bytes(document):
        raise AssertionError("accepted semantic golden is not exact canonical compact UTF-8 plus LF")

    digest = semantic_digest(document)
    verify_api2_boundary(document, digest, api2)
    rejected_count = verify_rejection_corpus(document, scene, api2, schema)
    generated_count = verify_generated_adversaries(document, scene, api2, schema)
    try:
        api2.load_json(NEGATIVE_ZERO_PATH)
    except ValueError as exception:
        if "negative zero" not in str(exception):
            raise
    else:
        raise AssertionError("lexical negative zero golden was accepted")

    oversized = copy.deepcopy(document)
    oversized["metadata"]["title"] = "x" * 4097
    oversized_failures = contract_errors(oversized, scene, api2, schema)
    if not any("longer than 4096" in str(item) for item in oversized_failures):
        raise AssertionError("bounded accessible text was not enforced")

    reordered = copy.deepcopy(document)
    reordered["nodes"][18]["children"].reverse()
    reordered_failures = contract_errors(reordered, scene, api2, schema)
    if reordered_failures:
        raise AssertionError("interleaved node/fragment reading order should remain representable")
    if semantic_digest(reordered) == digest:
        raise AssertionError("interleaved child reordering must change semantic identity")

    reparsed = json.loads(json.dumps(document, ensure_ascii=False, indent=2))
    if canonical_json_bytes(reparsed) != ACCEPTED_PATH.read_bytes():
        raise AssertionError("equivalent JSON presentation did not normalize to identical semantic bytes")
    verify_fresh_processes(digest)
    print(
        "Pliego canonical document-semantics v1 self-test passed: "
        f"1 accepted semantic artifact, {rejected_count + 1} rejected cases, "
        f"{generated_count} generated adversarial checks, "
        f"{FRESH_PROCESS_COUNT} fresh-process digests={digest}; API 2 runtime profiles remain empty"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
