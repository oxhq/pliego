#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Dependency-free self-test for the Pliego PDF/UA-1 profile contract."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType
from typing import Any


ROOT = Path(__file__).resolve().parent
SCHEMA_DIR = ROOT / "schema"
GOLDEN_DIR = ROOT / "goldens"
API2_ROOT = ROOT.parent / "api2"
API2_FIXTURE_ROOT = API2_ROOT / "fixtures"
SCHEMAS: dict[str, dict[str, Any]] = {}

SCHEMA_BY_KIND = {
    "profile": "profile-descriptor.v1.json",
    "assurance": "author-assurance.v1.json",
    "evidence": "conformance-evidence.v1.json",
    "lock": "validation-lock.v1.json",
}
AUTHORITY_ORDER = [
    "normative-pdfua-standard",
    "normative-base-pdf-standard",
    "test-protocol",
    "implementation-guidance",
    "reference-suite",
    "machine-validator",
]
EVIDENCE_GATES = (
    "per_render_machine",
    "release_corpus",
    "author_assurance",
    "assistive_technology",
)
PROFILE_GATES = [name.replace("_", "-") for name in EVIDENCE_GATES]


def load_api2_contract() -> ModuleType:
    path = API2_ROOT / "test_contract.py"
    spec = importlib.util.spec_from_file_location("_pliego_api2_contract", path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"cannot load API 2 contract helper from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


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


def content_address(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def descriptor_matches_bytes(descriptor: dict[str, Any], data: bytes) -> bool:
    return descriptor["bytes"] == len(data) and descriptor.get("sha256", descriptor.get("resource")) == content_address(
        data
    )


def load_schemas(api2_contract: ModuleType) -> None:
    for path in sorted(SCHEMA_DIR.glob("*.json")):
        schema = load_json(path)
        if not isinstance(schema, dict):
            raise AssertionError(f"{path}: schema must be an object")
        SCHEMAS[path.name] = schema

    expected = {
        "author-assurance.v1.json",
        "common.v1.json",
        "conformance-evidence.v1.json",
        "profile-descriptor.v1.json",
        "validation-lock.v1.json",
    }
    if set(SCHEMAS) != expected:
        raise AssertionError(f"unexpected PDF/UA-1 schema set: {sorted(SCHEMAS)}")
    ids = [schema.get("$id") for schema in SCHEMAS.values()]
    if len(ids) != len(set(ids)):
        raise AssertionError("PDF/UA-1 schema $id values must be unique")

    api2_contract.SCHEMAS = SCHEMAS
    for name, schema in SCHEMAS.items():
        api2_contract.audit_schema(schema, "", name)


def schema_errors(api2_contract: ModuleType, kind: str, value: Any) -> list[str]:
    schema_name = SCHEMA_BY_KIND[kind]
    schema = SCHEMAS[schema_name]
    errors = api2_contract.validate(value, schema, schema_name)
    errors += api2_contract.member_order_semantics(value, schema, schema_name)
    return [str(error) for error in errors]


def profile_semantics(profile: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    if profile["authority"]["order"] != AUTHORITY_ORDER:
        errors.append("$.authority.order: authority order is not the accepted hierarchy")
    inventory = profile["inventory"]
    if (
        inventory["machine"] + inventory["human"] + len(inventory["no_specific_test"])
        != inventory["failure_conditions"]
    ):
        errors.append("$.inventory: failure-condition classes do not close over the inventory")
    if inventory["no_specific_test"] != ["23-001", "27-001"]:
        errors.append("$.inventory.no_specific_test: unsupported Matterhorn 1.1 inventory")
    if profile["result"]["gates"] != PROFILE_GATES:
        errors.append("$.result.gates: evidence gates are not in the accepted order")
    return errors


def assurance_semantics(assurance: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    subject = assurance["subject"]
    input_manifest = load_json(API2_FIXTURE_ROOT / "input-manifest.json")
    input_entries = {entry["path"]: entry for entry in input_manifest["entries"]}
    if subject["scope"] == "template" and subject["template"] is None:
        errors.append("$.subject.template: template scope requires a content-addressed template")
    if subject["scope"] == "document" and subject["template"] is not None:
        errors.append("$.subject.template: document scope cannot attach template coverage")

    for name in ("entrypoint", "template"):
        descriptor = subject[name]
        if descriptor is None:
            continue
        source = API2_FIXTURE_ROOT / "input" / descriptor["path"]
        if not source.is_file() or not descriptor_matches_bytes(descriptor, source.read_bytes()):
            errors.append(f"$.subject.{name}: source descriptor does not match the exact input bytes")
        manifest_entry = input_entries.get(descriptor["path"])
        if manifest_entry is None or any(
            manifest_entry[field] != descriptor[field] for field in ("path", "sha256", "bytes")
        ):
            errors.append(f"$.subject.{name}: source descriptor is not bound by the input manifest")

    assessment = assurance["assessment"]
    summary = assessment["summary"]
    if summary["passed"] + summary["failed"] + summary["not_evaluated"] != summary["total"]:
        errors.append("$.assessment.summary: counts do not sum to total")
    if summary["total"] != 47:
        errors.append("$.assessment.summary.total: author assurance must cover the 47 human checks")
    if assessment["status"] == "passed" and summary != {
        "total": 47,
        "passed": 47,
        "failed": 0,
        "not_evaluated": 0,
    }:
        errors.append("$.assessment: passed assurance requires all 47 human checks to pass")
    if assessment["status"] == "failed" and summary["failed"] == 0:
        errors.append("$.assessment: failed assurance requires at least one failed check")
    return errors


def evidence_semantics(evidence: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    pdf_bytes = (API2_FIXTURE_ROOT / "delivery" / "document.pdf").read_bytes()
    if not descriptor_matches_bytes(evidence["subject_pdf"], pdf_bytes):
        errors.append("$.subject_pdf: descriptor does not match the exact PDF bytes")

    blockers = evidence["blockers"]
    if blockers != sorted(blockers, key=str.encode):
        errors.append("$.blockers: blockers must be in ascending ASCII byte order")

    machine_summary = evidence["per_render_machine"]["summary"]
    if (
        machine_summary["passed"] + machine_summary["failed"] + machine_summary["not_evaluated"]
        != machine_summary["total"]
    ):
        errors.append("$.per_render_machine.summary: counts do not sum to total")
    if machine_summary["total"] != 87:
        errors.append("$.per_render_machine.summary.total: machine evidence must cover 87 checks")

    statuses = [evidence[name]["status"] for name in EVIDENCE_GATES]
    decision = evidence["decision"]
    if decision == "satisfied":
        errors.append("$.decision: satisfied requires resolved proof closure from OXH-345 and OXH-346")
        if evidence["validation_lock"] is None:
            errors.append("$.decision: satisfied requires a content-addressed ready validation lock")
        if statuses != ["passed"] * len(EVIDENCE_GATES):
            errors.append("$.decision: satisfied requires all four evidence gates to pass")
        if blockers:
            errors.append("$.decision: satisfied cannot retain blockers")
        if machine_summary != {"total": 87, "passed": 87, "failed": 0, "not_evaluated": 0}:
            errors.append("$.per_render_machine.summary: satisfied requires all 87 machine checks to pass")
    elif decision == "failed":
        if "failed" not in statuses:
            errors.append("$.decision: failed requires at least one failed evidence gate")
    elif "not-evaluated" not in statuses or not blockers:
        errors.append("$.decision: not-evaluated requires a missing gate and an explicit blocker")
    return errors


def expected_lock_blockers(lock: dict[str, Any]) -> list[str]:
    authorities = lock["authorities"]
    verifier = lock["verifier"]
    blockers: list[str] = []
    if authorities["base_pdf_standard"]["clause_access"] != "reviewed":
        blockers.append("base-pdf-standard-clause-access-unreviewed")
    if authorities["base_pdf_standard"]["document_sha256"] is None:
        blockers.append("base-pdf-standard-document-sha256-unpinned")
    if authorities["implementation_guidance"]["document_sha256"] is None:
        blockers.append("implementation-guidance-sha256-unpinned")
    if authorities["test_protocol"]["document_sha256"] is None:
        blockers.append("matterhorn-sha256-unpinned")
    if authorities["pdfua_standard"]["clause_access"] != "reviewed":
        blockers.append("pdfua-standard-clause-access-unreviewed")
    if authorities["pdfua_standard"]["document_sha256"] is None:
        blockers.append("pdfua-standard-document-sha256-unpinned")
    if authorities["reference_suite"]["archive_sha256"] is None:
        blockers.append("reference-suite-archive-sha256-unpinned")
    if authorities["reference_suite"]["archive_url"] is None:
        blockers.append("reference-suite-archive-url-unpinned")
    if verifier["container"]["image_digest"] is None:
        blockers.append("verifier-container-image-digest-unpinned")
    if verifier["container"]["reference"] is None:
        blockers.append("verifier-container-reference-unpinned")
    if verifier["distribution_sha256"] is None:
        blockers.append("verifier-distribution-sha256-unpinned")
    if verifier["distribution_url"] is None:
        blockers.append("verifier-distribution-url-unpinned")
    if verifier["signature"]["verified_key_fingerprint"] is None:
        blockers.append("verifier-signature-key-unverified")
    if verifier["signature"]["sha256"] is None:
        blockers.append("verifier-signature-sha256-unpinned")
    if verifier["signature"]["url"] is None:
        blockers.append("verifier-signature-url-unpinned")
    return sorted(blockers, key=str.encode)


def lock_semantics(lock: dict[str, Any]) -> list[str]:
    expected = expected_lock_blockers(lock)
    blockers = lock["blockers"]
    errors: list[str] = []
    if blockers != sorted(blockers, key=str.encode):
        errors.append("$.blockers: blockers must be in ascending ASCII byte order")
    if lock["state"] == "ready":
        if expected:
            errors.append("$.state: ready lock retains unresolved authority or verifier pins")
        if blockers:
            errors.append("$.blockers: ready lock must have no blockers")
    else:
        if not expected:
            errors.append("$.state: blocked lock has no unresolved pin")
        if blockers != expected:
            errors.append("$.blockers: blocked lock must enumerate every unresolved pin exactly")
    return errors


def semantic_errors(kind: str, value: dict[str, Any]) -> list[str]:
    return {
        "profile": profile_semantics,
        "assurance": assurance_semantics,
        "evidence": evidence_semantics,
        "lock": lock_semantics,
    }[kind](value)


def assert_valid(api2_contract: ModuleType, name: str, kind: str, value: dict[str, Any]) -> None:
    errors = schema_errors(api2_contract, kind, value) + semantic_errors(kind, value)
    if errors:
        raise AssertionError(f"{name} unexpectedly failed:\n" + "\n".join(errors))


def assert_rejected(
    api2_contract: ModuleType,
    name: str,
    kind: str,
    value: dict[str, Any],
    expected: str,
) -> None:
    errors = schema_errors(api2_contract, kind, value) + semantic_errors(kind, value)
    detail = "\n".join(errors)
    if not errors:
        raise AssertionError(f"{name} was unexpectedly accepted")
    if expected not in detail:
        raise AssertionError(f"{name} did not report {expected!r}:\n{detail}")


def verify_author_assurance_ref(api2_contract: ModuleType) -> None:
    assurance_path = GOLDEN_DIR / "accepted" / "author-assurance.not-evaluated.json"
    descriptor = load_json(GOLDEN_DIR / "accepted" / "author-assurance-ref.json")
    schema_name = "common.v1.json"
    schema = SCHEMAS[schema_name]["definitions"]["author_assurance_input_ref"]
    errors = api2_contract.validate(descriptor, schema, schema_name)
    errors += api2_contract.member_order_semantics(descriptor, schema, schema_name)
    if errors:
        raise AssertionError("author-assurance-ref unexpectedly failed:\n" + "\n".join(map(str, errors)))
    if not descriptor_matches_bytes(descriptor, assurance_path.read_bytes()):
        raise AssertionError("author-assurance-ref does not content-address the exact assurance bytes")

    drifted = copy.deepcopy(descriptor)
    drifted["bytes"] += 1
    if descriptor_matches_bytes(drifted, assurance_path.read_bytes()):
        raise AssertionError("author-assurance-ref byte drift was not rejected")


def verify_runtime_does_not_advertise_profile(api2_contract: ModuleType) -> None:
    api2_contract.SCHEMAS = {}
    api2_contract.load_schemas()
    runtime_path = API2_ROOT / "goldens" / "accepted" / "runtime-contract.json"
    runtime = load_json(runtime_path)
    runtime_errors = api2_contract.validate(
        runtime,
        api2_contract.SCHEMAS["runtime-contract.v1.json"],
        "runtime-contract.v1.json",
    )
    runtime_errors += api2_contract.runtime_semantics(runtime)
    if runtime_errors:
        raise AssertionError("API 2 runtime fixture unexpectedly failed:\n" + "\n".join(map(str, runtime_errors)))
    if any(contract["profiles"] for contract in runtime["contracts"]):
        raise AssertionError("the current API 2 runtime fixture must advertise no conformance profiles")

    assurance_ref = load_json(GOLDEN_DIR / "accepted" / "author-assurance-ref.json")
    candidate_manifest = load_json(API2_FIXTURE_ROOT / "input-manifest.json")
    candidate_manifest["entries"].append(copy.deepcopy(assurance_ref))
    candidate_manifest["entries"].sort(key=lambda entry: entry["path"].encode("ascii"))
    manifest_errors = api2_contract.validate(
        candidate_manifest,
        api2_contract.SCHEMAS["input-manifest.v1.json"],
        "input-manifest.v1.json",
    )
    manifest_errors += api2_contract.input_manifest_semantics(candidate_manifest)
    if manifest_errors:
        raise AssertionError(
            "content-addressed author assurance is not a valid API 2 input entry:\n"
            + "\n".join(map(str, manifest_errors))
        )

    request = load_json(API2_ROOT / "goldens" / "accepted" / "render-request.a4.json")
    request["profile"] = {"schema": "pliego.profile.pdfua-1", "version": 1}
    request_schema_errors = api2_contract.validate(
        request,
        api2_contract.SCHEMAS["render-request.v1.json"],
        "render-request.v1.json",
    )
    if request_schema_errors:
        raise AssertionError("API 2 generic profile slot rejected the exact PDF/UA-1 reference")
    negotiation_errors = api2_contract.request_runtime_semantics(request, runtime)
    if not any("not advertised" in str(error) for error in negotiation_errors):
        raise AssertionError("unadvertised PDF/UA-1 request did not fail before rendering")


def main() -> None:
    api2_contract = load_api2_contract()
    load_schemas(api2_contract)

    accepted = {
        "profile": load_json(GOLDEN_DIR / "accepted" / "profile-descriptor.json"),
        "assurance": load_json(GOLDEN_DIR / "accepted" / "author-assurance.not-evaluated.json"),
        "evidence": load_json(GOLDEN_DIR / "accepted" / "conformance-evidence.not-evaluated.json"),
        "lock": load_json(GOLDEN_DIR / "accepted" / "validation-lock.blocked.json"),
    }
    for kind, value in accepted.items():
        assert_valid(api2_contract, f"accepted {kind}", kind, value)
    verify_author_assurance_ref(api2_contract)

    assert_rejected(
        api2_contract,
        "profile descriptor unknown member",
        "profile",
        load_json(GOLDEN_DIR / "rejected" / "profile-descriptor.unknown-member.json"),
        "unexpected property 'claim'",
    )
    assert_rejected(
        api2_contract,
        "assurance without human review",
        "assurance",
        load_json(GOLDEN_DIR / "rejected" / "author-assurance.passed-without-review.json"),
        "oneOf",
    )
    assert_rejected(
        api2_contract,
        "satisfied evidence with missing gates",
        "evidence",
        load_json(GOLDEN_DIR / "rejected" / "conformance-evidence.satisfied-with-missing-gates.json"),
        "satisfied requires",
    )
    assert_rejected(
        api2_contract,
        "satisfied evidence with forged artifact references",
        "evidence",
        load_json(GOLDEN_DIR / "rejected" / "conformance-evidence.satisfied-with-forged-artifacts.json"),
        "satisfied requires resolved proof closure",
    )
    assert_rejected(
        api2_contract,
        "ready lock with unresolved pins",
        "lock",
        load_json(GOLDEN_DIR / "rejected" / "validation-lock.ready-with-unpinned-artifacts.json"),
        "ready lock retains unresolved",
    )

    reordered_profile = copy.deepcopy(accepted["profile"])
    reordered_profile["authority"]["order"].reverse()
    assert_rejected(
        api2_contract,
        "reordered authority hierarchy",
        "profile",
        reordered_profile,
        "authority order is not the accepted hierarchy",
    )
    missing_revision_binding = copy.deepcopy(accepted["profile"])
    del missing_revision_binding["bindings"]["pdf_metadata"]["exact_revision_binding"]
    assert_rejected(
        api2_contract,
        "standard-only PDF/UA metadata",
        "profile",
        missing_revision_binding,
        "missing required property 'exact_revision_binding'",
    )
    invented_pdfua_revision = copy.deepcopy(accepted["profile"])
    invented_pdfua_revision["bindings"]["pdf_metadata"]["standard_identification"]["absent_properties"] = [
        "pdfuaid:amd",
        "pdfuaid:corr",
    ]
    assert_rejected(
        api2_contract,
        "PDF/UA-1 edition encoded as pdfuaid:rev",
        "profile",
        invented_pdfua_revision,
        "expected const ['pdfuaid:amd', 'pdfuaid:corr', 'pdfuaid:rev']",
    )
    invented_pdfua_amendment = copy.deepcopy(accepted["profile"])
    invented_pdfua_amendment["bindings"]["pdf_metadata"]["standard_identification"]["absent_properties"] = [
        "pdfuaid:corr",
        "pdfuaid:rev",
    ]
    assert_rejected(
        api2_contract,
        "PDF/UA-1 amendment identifier added",
        "profile",
        invented_pdfua_amendment,
        "expected const ['pdfuaid:amd', 'pdfuaid:corr', 'pdfuaid:rev']",
    )
    incomplete_lock = copy.deepcopy(accepted["lock"])
    incomplete_lock["blockers"].pop()
    assert_rejected(
        api2_contract,
        "incomplete blocker inventory",
        "lock",
        incomplete_lock,
        "enumerate every unresolved pin exactly",
    )

    verify_runtime_does_not_advertise_profile(api2_contract)
    rejected_count = len(list((GOLDEN_DIR / "rejected").glob("*.json")))
    print(
        "Pliego PDF/UA-1 contract self-test passed: "
        f"5 accepted artifacts, {rejected_count} rejected goldens, runtime profiles remain empty"
    )


if __name__ == "__main__":
    main()
