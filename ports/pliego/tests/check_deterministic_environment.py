#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import copy
import hashlib
import json
import math
import os
import subprocess
import sys
import tempfile
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
from typing import NoReturn


def fail(message: str, code: int = 1) -> NoReturn:
    print(f"deterministic environment check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def json_values_equal(left: object, right: object) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        if not isinstance(right, dict) or left.keys() != right.keys():
            return False
        return all(json_values_equal(left[key], right[key]) for key in left)
    if isinstance(left, list):
        if not isinstance(right, list) or len(left) != len(right):
            return False
        return all(
            json_values_equal(left_value, right_value) for left_value, right_value in zip(left, right, strict=True)
        )
    return left == right


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, object]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def run(
    binary: Path,
    fixture: Path,
    temp_root: Path,
    *,
    host_locale: str,
    host_timezone: str,
    requested_locale: str | None = None,
    requested_timezone: str | None = None,
) -> dict[str, object]:
    environment = os.environ.copy()
    environment.update(
        {
            "LANG": host_locale,
            "LC_ALL": host_locale,
            "TZ": host_timezone,
            "TMPDIR": str(temp_root),
            "TMP": str(temp_root),
            "TEMP": str(temp_root),
        }
    )
    command = [str(binary), "--allow-host-fonts"]
    if requested_locale is not None:
        command.extend(["--locale", requested_locale])
    if requested_timezone is not None:
        command.extend(["--timezone", requested_timezone])
    command.append(fixture.name)
    result = subprocess.run(
        command,
        cwd=fixture.parent,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no process output"
        fail(f"Pliego exited with {result.returncode}: {detail[-2000:]}")
    return final_json(result)


def payload(summary: dict[str, object]) -> dict[str, object]:
    value = summary.get("readiness")
    require(isinstance(value, dict), "render summary has no readiness payload")
    assert isinstance(value, dict)
    return value


def scene_hash(summary: dict[str, object]) -> str:
    scene = summary.get("scene")
    require(isinstance(scene, dict), "render summary has no scene result")
    assert isinstance(scene, dict)
    value = scene.get("hash")
    require(
        isinstance(value, str) and value.startswith("sha256:"),
        f"render summary has no content-addressed scene hash: {scene!r}",
    )
    assert isinstance(value, str)
    return value


def document_pdf(summary: dict[str, object]) -> bytes:
    require(
        summary.get("document_pdf_status") == "rendered",
        f"render summary PDF is not rendered: {summary!r}",
    )
    value = summary.get("document_pdf")
    require(isinstance(value, str) and bool(value), "render summary has no PDF artifact")
    assert isinstance(value, str)
    path = Path(value)
    require(path.is_file(), f"PDF artifact does not exist: {path}")
    pdf = path.read_bytes()
    require(pdf.startswith(b"%PDF-"), f"PDF artifact is invalid: {path}")
    return pdf


def is_sha256_address(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == len("sha256:") + 64
        and value.startswith("sha256:")
        and all(character in "0123456789abcdef" for character in value.removeprefix("sha256:"))
    )


def resolved_input_hash(render_id: str) -> str:
    hasher = hashlib.sha256()
    for value in (b"pliego.resolved-input.v1", render_id.encode("utf-8")):
        hasher.update(len(value).to_bytes(8, "big"))
        hasher.update(value)
    return f"sha256:{hasher.hexdigest()}"


def summary_phase_timings(summary: dict[str, object]) -> dict[str, object]:
    timings = summary.get("phase_timings_ms")
    require(isinstance(timings, dict), f"summary has no phase timings: {timings!r}")
    assert isinstance(timings, dict)
    pair: dict[str, object] = {}
    for phase in ("controlled_runtime", "scene_capture"):
        value = timings.get(phase)
        if type(value) not in (int, float):
            fail(f"summary phase timing {phase!r} is not numeric: {value!r}")
        assert isinstance(value, (int, float))
        require(math.isfinite(value) and value >= 0, f"summary phase timing {phase!r} is invalid: {value!r}")
        pair[phase] = value
    return pair


def resource_contract(
    summary: dict[str, object],
    fixture: Path,
    render_id: str,
) -> tuple[dict[str, object], dict[str, object]]:
    artifacts_value = summary.get("artifacts")
    require(isinstance(artifacts_value, str), f"summary has no artifacts directory: {artifacts_value!r}")
    assert isinstance(artifacts_value, str)
    artifacts = Path(artifacts_value)
    require(artifacts.is_dir() and not artifacts.is_symlink(), f"invalid artifacts directory: {artifacts}")
    resource_log = artifacts / "resources.jsonl"
    require(
        resource_log.is_file() and not resource_log.is_symlink(),
        f"invalid resource evidence log: {resource_log}",
    )
    lines = resource_log.read_text(encoding="utf-8").splitlines()
    require(len(lines) == 2 and all(lines), f"resource evidence must be one requested/terminal pair: {lines!r}")
    rows = []
    for line_number, line in enumerate(lines, 1):
        try:
            row = json.loads(line)
        except json.JSONDecodeError as error:
            fail(f"invalid resources.jsonl line {line_number}: {error}")
        require(isinstance(row, dict), f"resource log line {line_number} is not an object")
        assert isinstance(row, dict)
        rows.append(row)
    requested_rows = [row for row in rows if row.get("status") == "requested"]
    loaded_rows = [row for row in rows if row.get("status") == "loaded"]
    require(
        len(requested_rows) == 1 and len(loaded_rows) == 1,
        f"resource evidence must contain one request and one loaded result: {rows!r}",
    )
    requested = requested_rows[0]
    terminal = loaded_rows[0]
    fixture_bytes = fixture.read_bytes()
    digest = hashlib.sha256(fixture_bytes).hexdigest()
    address = f"sha256:{digest}"
    url = fixture.resolve().as_uri()
    request_id = requested.get("request_id")
    require(isinstance(request_id, str) and bool(request_id), f"requested resource has no request ID: {requested!r}")
    require("bytes" in requested and requested["bytes"] is None, f"requested resource has body bytes: {requested!r}")
    require(terminal.get("request_id") == request_id, f"terminal resource request ID differs: {terminal!r}")
    for row in (requested, terminal):
        require(row.get("render_id") == render_id, f"resource render ID differs: {row!r}")
        require(row.get("policy") == "pliego.resource-policy.v1", f"resource policy differs: {row!r}")
        require(row.get("url") == url, f"resource URL differs: {row!r}")
    require(terminal.get("is_for_main_frame") is True, f"loaded resource is not the main frame: {terminal!r}")
    require(terminal.get("source") == "document_root", f"loaded resource source differs: {terminal!r}")
    require(type(terminal.get("bytes")) is int, f"terminal byte count is not an integer: {terminal!r}")
    require(
        terminal.get("bytes") == len(fixture_bytes)
        and terminal.get("sha256") == digest
        and terminal.get("resource") == address
        and terminal.get("content_hash") == address
        and terminal.get("artifact") == f"resources/{digest}"
        and "failure" in terminal
        and terminal["failure"] is None,
        f"terminal main-frame resource identity differs: {terminal!r}",
    )

    retained = artifacts / str(terminal["artifact"])
    require(
        not (artifacts / "resources").is_symlink() and retained.is_file() and not retained.is_symlink(),
        f"retained input resource is not a regular owned file: {retained}",
    )
    require(
        retained.resolve().is_relative_to(artifacts.resolve()),
        f"retained input resource escapes the artifact root: {retained}",
    )
    require(retained.read_bytes() == fixture_bytes, f"retained input resource bytes differ: {retained}")

    terminal_rows = [terminal]
    accounting: dict[str, object] = {
        "requests": len(terminal_rows),
        "loaded": sum(row.get("status") == "loaded" for row in terminal_rows),
        "delegated": sum(row.get("status") == "delegated" for row in terminal_rows),
        "failed": sum(row.get("failure") is not None for row in terminal_rows),
        "body_bytes": sum(row["bytes"] for row in terminal_rows if type(row.get("bytes")) is int),
        "unavailable_bodies": sum(row.get("bytes") is None for row in terminal_rows),
    }
    require(
        accounting
        == {
            "requests": 1,
            "loaded": 1,
            "delegated": 0,
            "failed": 0,
            "body_bytes": len(fixture_bytes),
            "unavailable_bodies": 0,
        },
        f"derived resource accounting differs: {accounting!r}",
    )
    return accounting, {
        "render_id": render_id,
        "url": url,
        "sha256": digest,
        "resource": address,
        "bytes": len(fixture_bytes),
        "source": "document_root",
        "main_frame": True,
    }


def expected_environment_payload(
    summary: dict[str, object],
    requested_locale: str,
    requested_timezone: str,
    fixture: Path,
) -> dict[str, object]:
    render_id = summary.get("render_id")
    assert isinstance(render_id, str)
    accounting, input_resource = resource_contract(summary, fixture, render_id)
    return {
        "locale": {"requested": requested_locale, "resolved": requested_locale},
        "timezone": {"requested": requested_timezone, "resolved": requested_timezone},
        "page": {
            "size_css_px": {
                "width": 793.7000122070312,
                "height": 1122.5167236328125,
            },
            "margins_css_px": {
                "top": 45.349998474121094,
                "right": 60.46666717529297,
                "bottom": 45.349998474121094,
                "left": 60.46666717529297,
            },
        },
        "fonts": {"host_fonts": "allowed"},
        "resource_policy": {
            "schema": "pliego.resource-policy.v1",
            "version": 1,
            "render_id": render_id,
            "network": "deny",
            "http_roots": [],
            "filesystem": "document-root",
            "data_urls": "allow",
            "redirects": "deny",
            "timeout_ms": 10000,
            "virtual_resources": [],
        },
        "document_pdf": {
            "artifact": summary.get("document_pdf"),
            "status": "rendered",
            "error": None,
        },
        "runtime": {"adapter": "document-session"},
        "resource_accounting": accounting,
        "phase_timings_ms": summary_phase_timings(summary),
        "input_resource": input_resource,
        "resolved_input_hash": summary.get("resolved_input_hash"),
    }


def verify_environment_payload(
    summary: dict[str, object],
    artifact: object,
    requested_locale: str,
    requested_timezone: str,
    fixture: Path,
) -> None:
    require(isinstance(artifact, dict), "environment artifact is not an object")
    assert isinstance(artifact, dict)
    summary_environment = summary.get("environment")
    require(isinstance(summary_environment, dict), "summary has no embedded environment")
    require(
        json_values_equal(artifact, summary_environment),
        "environment artifact differs from the embedded summary environment",
    )
    phase_timings = artifact.get("phase_timings_ms")
    require(
        isinstance(phase_timings, dict) and set(phase_timings) == {"controlled_runtime", "scene_capture"},
        f"environment phase timing schema differs: {phase_timings!r}",
    )
    assert isinstance(phase_timings, dict)
    for phase, value in phase_timings.items():
        if type(value) not in (int, float):
            fail(f"environment phase timing {phase!r} is not numeric: {value!r}")
        assert isinstance(value, (int, float))
        require(math.isfinite(value) and value >= 0, f"environment phase timing {phase!r} is invalid: {value!r}")
    resource_accounting = artifact.get("resource_accounting")
    accounting_fields = {"requests", "loaded", "delegated", "failed", "body_bytes", "unavailable_bodies"}
    require(
        isinstance(resource_accounting, dict) and set(resource_accounting) == accounting_fields,
        f"environment resource accounting schema differs: {resource_accounting!r}",
    )
    assert isinstance(resource_accounting, dict)
    for field, value in resource_accounting.items():
        if type(value) is not int:
            fail(f"environment resource accounting field {field!r} is not an integer: {value!r}")
        assert isinstance(value, int)
        require(value >= 0, f"environment resource accounting field {field!r} is negative: {value!r}")
    input_resource = artifact.get("input_resource")
    require(
        isinstance(input_resource, dict)
        and set(input_resource) == {"render_id", "url", "sha256", "resource", "bytes", "source", "main_frame"},
        f"environment input resource schema differs: {input_resource!r}",
    )
    assert isinstance(input_resource, dict)
    input_bytes = input_resource.get("bytes")
    if type(input_bytes) is not int:
        fail(f"environment input byte count is not an integer: {input_resource!r}")
    assert isinstance(input_bytes, int)
    require(input_bytes >= 0, f"environment input byte count is negative: {input_resource!r}")
    require(input_resource.get("main_frame") is True, repr(input_resource))
    render_id = summary.get("render_id")
    require(is_sha256_address(render_id), f"summary has no content-addressed render ID: {render_id!r}")
    assert isinstance(render_id, str)
    resolved_input = summary.get("resolved_input")
    require(isinstance(resolved_input, str), f"summary has no resolved input path: {resolved_input!r}")
    assert isinstance(resolved_input, str)
    try:
        input_matches_fixture = Path(resolved_input).samefile(fixture)
    except OSError as error:
        fail(f"cannot compare the summary input with the fixture: {error}")
    require(
        input_matches_fixture,
        f"summary resolved input does not match the fixture: {resolved_input!r}",
    )

    resolved_hash = summary.get("resolved_input_hash")
    require(
        is_sha256_address(resolved_hash),
        f"summary has no content-addressed resolved input hash: {resolved_hash!r}",
    )
    require(
        resolved_hash == resolved_input_hash(render_id),
        f"summary resolved input hash is not bound to its render ID: {resolved_hash!r}",
    )

    require(
        json_values_equal(
            artifact,
            expected_environment_payload(summary, requested_locale, requested_timezone, fixture),
        ),
        f"unexpected resolved environment artifact: {artifact!r}",
    )


def verify_environment_artifact(
    summary: dict[str, object],
    requested_locale: str,
    requested_timezone: str,
    fixture: Path,
) -> None:
    value = summary.get("environment_artifact")
    require(isinstance(value, str) and bool(value), "summary has no environment artifact")
    assert isinstance(value, str)
    path = Path(value)
    artifacts_value = summary.get("artifacts")
    require(isinstance(artifacts_value, str), "summary has no artifacts directory")
    assert isinstance(artifacts_value, str)
    artifacts = Path(artifacts_value)
    try:
        artifact_root_matches = path.parent.samefile(artifacts)
    except OSError as error:
        fail(f"cannot compare the environment artifact root with the summary: {error}")
    require(
        path.name == "environment.json" and artifact_root_matches,
        f"environment artifact is outside its artifact root: {path}",
    )
    require(path.is_file() and not path.is_symlink(), f"environment artifact is not a regular file: {path}")
    try:
        artifact = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read environment artifact {path}: {error}")
    verify_environment_payload(summary, artifact, requested_locale, requested_timezone, fixture)


def verify_invalid_request(binary: Path, fixture: Path) -> None:
    for option, value in (
        ("--locale", "not-a-locale"),
        ("--timezone", "America/Tijuana"),
    ):
        result = subprocess.run(
            [str(binary), option, value, fixture.name],
            cwd=fixture.parent,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        require(
            result.returncode == 2,
            f"invalid {option} exited with {result.returncode}",
        )
        error = final_json(result).get("error")
        require(
            isinstance(error, dict) and error.get("code") == "INVALID_REQUEST",
            f"invalid {option} did not produce typed INVALID_REQUEST: {error!r}",
        )


def require_only_changes(baseline: dict[str, object], changed: dict[str, object], allowed: set[str]) -> None:
    keys = baseline.keys() | changed.keys()
    unexpected = {
        key: (baseline.get(key), changed.get(key)) for key in keys - allowed if baseline.get(key) != changed.get(key)
    }
    require(not unexpected, f"unexpected environment-dependent changes: {unexpected!r}")


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="pliego-deterministic-environment-self-test-") as temp:
        root = Path(temp)
        fixture = root / "index.html"
        fixture_bytes = b"deterministic input"
        fixture.write_bytes(fixture_bytes)
        artifacts = root / "artifacts"
        resources = artifacts / "resources"
        resources.mkdir(parents=True)
        digest = hashlib.sha256(fixture_bytes).hexdigest()
        address = f"sha256:{digest}"
        url = fixture.resolve().as_uri()
        render_id = f"sha256:{'1' * 64}"
        resolved_hash = "sha256:d7369e65bf4c8e749211504d73393deb5ee8ee94c80fc30e5b7d0c50dc3a7df4"
        require(resolved_input_hash(render_id) == resolved_hash, "resolved input identity vector differs")
        requested: dict[str, object] = {
            "render_id": render_id,
            "policy": "pliego.resource-policy.v1",
            "request_id": "request-1",
            "url": url,
            "status": "requested",
            "bytes": None,
        }
        terminal: dict[str, object] = {
            "render_id": render_id,
            "policy": "pliego.resource-policy.v1",
            "request_id": "request-1",
            "url": url,
            "status": "loaded",
            "is_for_main_frame": True,
            "source": "document_root",
            "bytes": len(fixture_bytes),
            "sha256": digest,
            "resource": address,
            "content_hash": address,
            "artifact": f"resources/{digest}",
            "failure": None,
        }

        def write_resource_contract(rows: list[dict[str, object]], body: bytes | None = fixture_bytes) -> None:
            (artifacts / "resources.jsonl").write_text(
                "".join(f"{json.dumps(row, sort_keys=True)}\n" for row in rows),
                encoding="utf-8",
            )
            retained = resources / digest
            if body is None:
                if retained.exists():
                    retained.unlink()
            else:
                retained.write_bytes(body)

        baseline_rows: list[dict[str, object]] = [requested, terminal]
        write_resource_contract(baseline_rows)
        summary: dict[str, object] = {
            "artifacts": str(artifacts),
            "render_id": render_id,
            "resolved_input": str(fixture.resolve()),
            "resolved_input_hash": resolved_hash,
            "document_pdf": str(root / "document.pdf"),
            "environment_artifact": str(artifacts / "environment.json"),
            "phase_timings_ms": {
                "controlled_runtime": 12.5,
                "scene_capture": 1.25,
                "scene_setup": 2.0,
                "preview_raster": 3.0,
                "pdf_serialize": 4.0,
            },
        }
        artifact = expected_environment_payload(summary, "en-US", "UTC", fixture)
        summary["environment"] = copy.deepcopy(artifact)
        (artifacts / "environment.json").write_text(json.dumps(artifact), encoding="utf-8")
        verify_environment_artifact(summary, "en-US", "UTC", fixture)
        if os.name == "nt":
            verbatim_summary = copy.deepcopy(summary)
            verbatim_summary["resolved_input"] = f"\\\\?\\{fixture.resolve()}"
            verify_environment_payload(verbatim_summary, artifact, "en-US", "UTC", fixture)

        def require_rejection(
            candidate: dict[str, object],
            description: str,
            candidate_summary: dict[str, object] | None = None,
        ) -> None:
            if candidate_summary is None:
                candidate_summary = copy.deepcopy(summary)
                candidate_summary["environment"] = copy.deepcopy(candidate)
            with redirect_stderr(StringIO()):
                try:
                    verify_environment_payload(candidate_summary, candidate, "en-US", "UTC", fixture)
                except SystemExit as error:
                    if error.code == 1:
                        return
            fail(f"self-test accepted {description}")

        delete = object()

        def require_corruption(path: tuple[str, ...], value: object, description: str) -> None:
            candidate = copy.deepcopy(artifact)
            target = candidate
            for field in path[:-1]:
                nested = target.get(field)
                assert isinstance(nested, dict)
                target = nested
            if value is delete:
                del target[path[-1]]
            else:
                target[path[-1]] = value
            require_rejection(candidate, description)

        for path, value, description in [
            (("unexpected",), True, "an extra top-level environment field"),
            (("input_resource",), delete, "a missing input resource"),
            (("runtime", "adapter"), "shell", "the shell runtime adapter"),
            (("resource_policy", "version"), True, "a Boolean resource policy version"),
            (("resource_policy", "timeout_ms"), 10000.0, "a floating resource policy timeout"),
            (("resource_accounting", "requests"), True, "a Boolean resource count"),
            (
                ("resource_accounting", "body_bytes"),
                len(fixture.read_bytes()) + 1,
                "resource bytes not bound to the input",
            ),
            (("phase_timings_ms", "scene_capture"), -1.0, "a negative phase timing"),
            (("phase_timings_ms", "scene_capture"), False, "a Boolean phase timing"),
            (("phase_timings_ms", "unexpected"), 0.0, "an extra phase timing"),
            (
                ("input_resource", "render_id"),
                f"sha256:{'2' * 64}",
                "an input resource bound to another render",
            ),
            (("input_resource", "url"), "file:///wrong.html", "an input resource bound to another URL"),
            (("input_resource", "sha256"), "0" * 64, "an input resource with another byte hash"),
            (
                ("input_resource", "resource"),
                f"sha256:{'0' * 64}",
                "an input resource with another content address",
            ),
            (
                ("input_resource", "bytes"),
                len(fixture.read_bytes()) + 1,
                "an input resource with another byte count",
            ),
            (("input_resource", "main_frame"), 1, "an integer main-frame marker"),
            (
                ("resolved_input_hash",),
                f"sha256:{'0' * 64}",
                "an environment hash not bound to the summary",
            ),
        ]:
            require_corruption(path, value, description)

        changed_summary = copy.deepcopy(summary)
        changed_summary["environment"] = {"forged": True}
        require_rejection(artifact, "an environment artifact differing from the summary", changed_summary)

        changed_summary = copy.deepcopy(summary)
        changed_summary["environment"]["resource_policy"]["version"] = True  # type: ignore[index]
        require_rejection(artifact, "an artifact/summary resource policy type mismatch", changed_summary)

        changed_summary = copy.deepcopy(summary)
        changed_summary["phase_timings_ms"]["scene_capture"] = 9.0  # type: ignore[index]
        require_rejection(artifact, "environment timings not bound to the summary", changed_summary)

        changed_summary = copy.deepcopy(summary)
        changed_artifact = copy.deepcopy(artifact)
        changed_artifact["phase_timings_ms"]["scene_capture"] = float("inf")  # type: ignore[index]
        changed_summary["environment"] = copy.deepcopy(changed_artifact)
        changed_summary["phase_timings_ms"]["scene_capture"] = float("inf")  # type: ignore[index]
        require_rejection(changed_artifact, "a jointly forged non-finite phase timing", changed_summary)

        changed_summary = copy.deepcopy(summary)
        changed_summary["resolved_input_hash"] = f"sha256:{'0' * 64}"
        changed_artifact = copy.deepcopy(artifact)
        changed_artifact["resolved_input_hash"] = changed_summary["resolved_input_hash"]
        changed_summary["environment"] = copy.deepcopy(changed_artifact)
        require_rejection(
            changed_artifact,
            "a jointly forged summary and environment resolved-input hash",
            changed_summary,
        )

        def require_resource_rejection(
            rows: list[dict[str, object]],
            description: str,
            body: bytes | None = fixture_bytes,
        ) -> None:
            write_resource_contract(rows, body)
            require_rejection(artifact, description)
            write_resource_contract(baseline_rows)

        require_resource_rejection([terminal], "a missing requested resource row")
        require_resource_rejection([requested], "a missing terminal resource row")
        require_resource_rejection([requested, requested, terminal], "a duplicate requested resource row")
        require_resource_rejection([requested, terminal, terminal], "a duplicate terminal resource row")
        changed_terminal = copy.deepcopy(terminal)
        changed_terminal["unexpected"] = True
        write_resource_contract([requested, changed_terminal])
        verify_environment_payload(summary, artifact, "en-US", "UTC", fixture)
        write_resource_contract(baseline_rows)
        changed_terminal = copy.deepcopy(terminal)
        del changed_terminal["failure"]
        require_resource_rejection([requested, changed_terminal], "a missing terminal failure field")
        for field, value in [
            ("request_id", "document-session:999999"),
            ("render_id", f"sha256:{'2' * 64}"),
            ("url", "file:///wrong.html"),
            ("policy", "wrong-policy"),
            ("status", "delegated"),
            ("is_for_main_frame", False),
            ("source", "http"),
            ("sha256", "0" * 64),
            ("resource", f"sha256:{'0' * 64}"),
            ("content_hash", f"sha256:{'0' * 64}"),
            ("bytes", len(fixture_bytes) + 1),
            ("artifact", "resources/wrong"),
            ("failure", {"code": "forged"}),
            ("is_for_main_frame", 1),
            ("bytes", float(len(fixture_bytes))),
        ]:
            changed_terminal = copy.deepcopy(terminal)
            changed_terminal[field] = value
            require_resource_rejection([requested, changed_terminal], f"terminal resource {field} drift")
        for field, value in [
            ("request_id", ""),
            ("render_id", f"sha256:{'2' * 64}"),
            ("url", "file:///wrong.html"),
            ("policy", "wrong-policy"),
            ("bytes", 0),
        ]:
            changed_requested = copy.deepcopy(requested)
            changed_requested[field] = value
            require_resource_rejection([changed_requested, terminal], f"requested resource {field} drift")
        require_resource_rejection(baseline_rows, "a missing retained resource blob", None)
        require_resource_rejection(baseline_rows, "a tampered retained resource blob", b"tampered")

        changed_rows = [requested, terminal, terminal]
        changed_artifact = copy.deepcopy(artifact)
        changed_artifact["resource_accounting"]["requests"] = 2  # type: ignore[index]
        changed_artifact["resource_accounting"]["loaded"] = 2  # type: ignore[index]
        changed_summary = copy.deepcopy(summary)
        changed_summary["environment"] = copy.deepcopy(changed_artifact)
        write_resource_contract(changed_rows)
        require_rejection(
            changed_artifact,
            "jointly forged accounting with an extra terminal row",
            changed_summary,
        )
        write_resource_contract(baseline_rows)


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("deterministic environment checker self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)

    binary = Path(arguments[0]).expanduser().resolve()
    fixture = Path(__file__).resolve().parent / "fixtures/deterministic-environment/index.html"
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(fixture.is_file(), f"fixture does not exist: {fixture}")
    self_test()

    with tempfile.TemporaryDirectory(prefix="pliego-deterministic-environment-") as temp:
        temp_root = Path(temp)
        first = run(
            binary,
            fixture,
            temp_root,
            host_locale="de_DE.UTF-8",
            host_timezone="CST6CDT",
        )
        second = run(
            binary,
            fixture,
            temp_root,
            host_locale="fr_FR.UTF-8",
            host_timezone="EST5EDT",
        )
        changed_timezone = run(
            binary,
            fixture,
            temp_root,
            host_locale="de_DE.UTF-8",
            host_timezone="CST6CDT",
            requested_timezone="PST8PDT",
        )
        changed_locale = run(
            binary,
            fixture,
            temp_root,
            host_locale="fr_FR.UTF-8",
            host_timezone="EST5EDT",
            requested_locale="es-MX",
        )

        first_payload = payload(first)
        second_payload = payload(second)
        timezone_payload = payload(changed_timezone)
        locale_payload = payload(changed_locale)
        first_scene_hash = scene_hash(first)
        second_scene_hash = scene_hash(second)
        timezone_scene_hash = scene_hash(changed_timezone)
        locale_scene_hash = scene_hash(changed_locale)
        first_pdf = document_pdf(first)
        second_pdf = document_pdf(second)
        timezone_pdf = document_pdf(changed_timezone)
        locale_pdf = document_pdf(changed_locale)
        require(
            first_payload == second_payload,
            f"defaults changed with host locale/timezone: {first_payload!r} != {second_payload!r}",
        )
        require(
            first_scene_hash == second_scene_hash,
            f"scene hash changed with host locale/timezone: {first_scene_hash} != {second_scene_hash}",
        )
        require(
            first_pdf == second_pdf,
            "PDF bytes changed with host locale/timezone",
        )
        require(first_payload.get("navigatorLanguage") == "en-US", repr(first_payload))
        require(first_payload.get("intlLocale") == "en-US", repr(first_payload))
        require(first_payload.get("intlTimezone") == "UTC", repr(first_payload))
        require(first_payload.get("localHour") == 12, repr(first_payload))
        require(timezone_payload.get("localHour") == 4, repr(timezone_payload))
        require(
            timezone_payload.get("intlTimezone") != first_payload.get("intlTimezone"),
            "changed requested timezone did not change Intl's resolved timezone",
        )
        require(
            timezone_payload.get("formatted") != first_payload.get("formatted"),
            "changed requested timezone did not change Intl date formatting",
        )
        require(
            timezone_scene_hash != first_scene_hash,
            "changed requested timezone did not change the formatted scene",
        )
        require(
            timezone_pdf != first_pdf,
            "changed requested timezone did not change the formatted PDF",
        )
        require_only_changes(
            first_payload,
            timezone_payload,
            {"intlTimezone", "localHour", "formatted"},
        )
        require(locale_payload.get("navigatorLanguage") == "es-MX", repr(locale_payload))
        require(locale_payload.get("intlLocale") == "es-MX", repr(locale_payload))
        require(locale_payload.get("intlTimezone") == "UTC", repr(locale_payload))
        require(locale_payload.get("localHour") == 12, repr(locale_payload))
        require(
            locale_payload.get("formatted") != first_payload.get("formatted"),
            repr(locale_payload),
        )
        require(locale_scene_hash != first_scene_hash, "locale did not change formatted scene")
        require(locale_pdf != first_pdf, "locale did not change formatted PDF")
        require_only_changes(
            first_payload,
            locale_payload,
            {"navigatorLanguage", "intlLocale", "formatted"},
        )
        verify_environment_artifact(first, "en-US", "UTC", fixture)
        verify_environment_artifact(second, "en-US", "UTC", fixture)
        verify_environment_artifact(changed_timezone, "en-US", "PST8PDT", fixture)
        verify_environment_artifact(changed_locale, "es-MX", "UTC", fixture)
        verify_invalid_request(binary, fixture)

    print("deterministic environment check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
