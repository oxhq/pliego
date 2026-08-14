#!/usr/bin/env python3
"""Exercise the production parent/worker boundary through the real Pliego binary."""

from __future__ import annotations

import argparse
from contextlib import redirect_stderr
import hashlib
from io import StringIO
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any, Callable, NoReturn


INTERNAL_ENV_PREFIX = "PLIEGO_INTERNAL_RENDER_"
COMMIT_RE = re.compile(r"[0-9a-f]{40}")
TARGET_RE = re.compile(r"[A-Za-z0-9_]+(?:-[A-Za-z0-9_.]+){2,}")
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}")
RESOURCE_RE = re.compile(r"[0-9a-f]{64}")
PAGE_PREVIEW_RE = re.compile(r"page-[0-9]{4}\.png")

CAPTURED_ROOT_FILES = {
    "console.jsonl",
    "document.pdf",
    "environment.json",
    "fonts.json",
    "layout-debug.json",
    "pages.json",
    "pdf-structure.json",
    "readiness.json",
    "render.png",
    "resources.jsonl",
    "scene-preview.png",
    "scene-report.json",
    "scene.json",
    "session-state.jsonl",
}
CAPTURED_REQUIRED_ROOT_FILES = CAPTURED_ROOT_FILES - {"scene-preview.png"}
FAILED_ROOT_FILES = {
    "console.jsonl",
    "environment.json",
    "failure.json",
    "readiness.json",
    "render.png",
    "resources.jsonl",
    "session-state.jsonl",
}
FAILED_REQUIRED_ROOT_FILES = {
    "console.jsonl",
    "failure.json",
    "resources.jsonl",
    "session-state.jsonl",
}
COMMITTED_PUBLICATION_FILES = {"committed.json", "lease", "outcome.json", "plan.json", "prepared.json"}
PLANNED_PUBLICATION_FILES = {"lease", "plan.json"}
STDERR_NEWLINE = b"\n"


def fail(message: str) -> NoReturn:
    print(f"render supervisor check: {message}", file=sys.stderr)
    raise SystemExit(1)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def expect_rejection(action: Callable[[], object], label: str) -> None:
    with redirect_stderr(StringIO()):
        try:
            action()
        except SystemExit:
            return
    fail(f"negative self-test was accepted: {label}")


def canonical_json_line(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")


def stderr_line(value: str) -> bytes:
    return value.encode("utf-8") + STDERR_NEWLINE


def canonical_missing_path(path: Path) -> str:
    resolved = path.parent.resolve(strict=True) / path.name
    value = str(resolved)
    if os.name != "nt" or value.startswith("\\\\?\\"):
        return value
    if value.startswith("\\\\"):
        return "\\\\?\\UNC\\" + value[2:]
    return "\\\\?\\" + value


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number {value}")


def decode_single_json_frame(stdout: bytes, label: str) -> dict[str, Any]:
    require(stdout.endswith(b"\n"), f"{label} stdout has no final LF: {stdout!r}")
    require(b"\r" not in stdout, f"{label} stdout contains CR bytes")
    require(stdout.count(b"\n") == 1, f"{label} stdout is not exactly one line")
    body = stdout[:-1]
    require(body == body.strip(), f"{label} stdout has surrounding whitespace")
    try:
        value = json.loads(body, parse_constant=reject_json_constant)
    except (UnicodeDecodeError, ValueError) as error:
        fail(f"{label} stdout is not one UTF-8 JSON object: {error}")
    require(isinstance(value, dict), f"{label} stdout is not a JSON object")
    require(
        stdout == canonical_json_line(value),
        f"{label} stdout is not canonical compact JSON plus one LF",
    )
    return value


def clean_environment() -> dict[str, str]:
    return {key: value for key, value in os.environ.items() if not key.startswith(INTERNAL_ENV_PREFIX)}


def run(
    command: list[str],
    *,
    cwd: Path,
    env_overrides: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    environment = clean_environment()
    if env_overrides:
        environment.update(env_overrides)
    try:
        return subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=120,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        fail(f"process exceeded 120 seconds: {error.cmd!r}")


def require_no_private_container(parent: Path, label: str) -> None:
    private = sorted(path.name for path in parent.glob(".pliego-runtime-*"))
    require(not private, f"{label} retained private runtime containers: {private}")


def require_top_level(parent: Path, expected: set[str], label: str) -> None:
    actual = {path.name for path in parent.iterdir()}
    require(actual == expected, f"{label} top-level closure drifted: {sorted(actual)}")


def exact_keys(value: Any, expected: set[str], label: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{label} is not a JSON object")
    require(set(value) == expected, f"{label} keys drifted: {sorted(value)}")
    return value


def read_json_object(path: Path, label: str) -> dict[str, Any]:
    require(path.is_file(), f"{label} is missing: {path}")
    try:
        value = json.loads(path.read_bytes(), parse_constant=reject_json_constant)
    except (OSError, UnicodeDecodeError, ValueError) as error:
        fail(f"{label} is not valid UTF-8 JSON: {error}")
    require(isinstance(value, dict), f"{label} is not a JSON object")
    return value


def require_regular_tree(root: Path, label: str) -> None:
    require(root.is_dir() and not root.is_symlink(), f"{label} is not a regular directory")
    for path in root.rglob("*"):
        require(not path.is_symlink(), f"{label} contains a symlink: {path.relative_to(root)}")
        require(path.is_file() or path.is_dir(), f"{label} contains a special entry: {path.relative_to(root)}")


def require_resource_directory(directory: Path, label: str) -> set[str]:
    require(directory.is_dir() and not directory.is_symlink(), f"{label} resources directory is missing")
    resources = set()
    for path in directory.iterdir():
        require(path.is_file() and not path.is_symlink(), f"{label} resource is not a regular file: {path.name}")
        require(RESOURCE_RE.fullmatch(path.name) is not None, f"{label} resource name is not a digest: {path.name}")
        require(sha256_file(path) == path.name, f"{label} resource bytes do not match {path.name}")
        resources.add(path.name)
    return resources


def failure_ledger_resources(path: Path) -> set[str]:
    require(path.is_file() and not path.is_symlink(), "failure resources.jsonl is missing")
    raw = path.read_bytes()
    require(b"\r" not in raw, "failure resources.jsonl contains CR bytes")
    require(not raw or raw.endswith(b"\n"), "failure resources.jsonl has no final LF")
    resources = set()
    for line_number, line in enumerate(raw.splitlines(), 1):
        try:
            row = json.loads(line, parse_constant=reject_json_constant)
        except (UnicodeDecodeError, ValueError) as error:
            fail(f"failure resources.jsonl line {line_number} is invalid: {error}")
        require(isinstance(row, dict), f"failure resources.jsonl line {line_number} is not an object")
        if row.get("status") != "loaded":
            continue
        artifact = row.get("artifact")
        require(isinstance(artifact, str), f"loaded failure resource row {line_number} has no artifact")
        prefix = "resources/"
        require(artifact.startswith(prefix), f"loaded failure resource row {line_number} has an invalid artifact")
        digest = artifact[len(prefix) :]
        require(
            RESOURCE_RE.fullmatch(digest) is not None,
            f"loaded failure resource row {line_number} has an invalid digest",
        )
        require(row.get("sha256") == digest, f"loaded failure resource row {line_number} SHA-256 drifted")
        require(row.get("resource") == f"sha256:{digest}", f"loaded failure resource row {line_number} address drifted")
        require(
            row.get("content_hash") == f"sha256:{digest}",
            f"loaded failure resource row {line_number} content hash drifted",
        )
        resources.add(digest)
    return resources


def require_publication_directory(directory: Path, expected: set[str], label: str) -> None:
    require(directory.is_dir() and not directory.is_symlink(), f"{label} publication directory is missing")
    actual = {path.name for path in directory.iterdir()}
    require(actual == expected, f"{label} publication closure drifted: {sorted(actual)}")
    for path in directory.iterdir():
        require(path.is_file() and not path.is_symlink(), f"{label} publication entry is not a regular file")


def safe_bundle_path(value: Any) -> str:
    require(isinstance(value, str) and value, "bundle entry path is not a nonempty string")
    require("\\" not in value, f"bundle entry path uses a backslash: {value!r}")
    parsed = PurePosixPath(value)
    require(not parsed.is_absolute(), f"bundle entry path is absolute: {value!r}")
    require(all(part not in {"", ".", ".."} for part in parsed.parts), f"bundle entry path is unsafe: {value!r}")
    require(parsed.as_posix() == value, f"bundle entry path is not canonical: {value!r}")
    return value


def require_bundle_closure(
    artifacts: Path,
    output: Path,
    render_id: str,
    *,
    logical_output: str | None = None,
) -> None:
    bundle = exact_keys(
        read_json_object(artifacts / "bundle.json", "bundle.json"),
        {"schema", "version", "render_id", "entries", "output"},
        "bundle.json",
    )
    require(bundle["schema"] == "pliego.bundle" and bundle["version"] == 1, "bundle identity drifted")
    require(bundle["render_id"] == render_id, "bundle render ID drifted")
    entries = bundle["entries"]
    require(isinstance(entries, list) and entries, "bundle entries are empty or invalid")
    declared: set[str] = set()
    previous = ""
    for index, raw in enumerate(entries):
        entry = exact_keys(raw, {"path", "sha256", "bytes"}, f"bundle entry {index}")
        relative = safe_bundle_path(entry["path"])
        require(relative > previous, "bundle entries are not strictly sorted")
        previous = relative
        require(relative not in declared, f"bundle path is duplicated: {relative}")
        declared.add(relative)
        path = artifacts / Path(*PurePosixPath(relative).parts)
        require(path.is_file() and not path.is_symlink(), f"bundle path is not a regular file: {relative}")
        require(entry["sha256"] == f"sha256:{sha256_file(path)}", f"bundle hash drifted: {relative}")
        require(
            type(entry["bytes"]) is int and entry["bytes"] == path.stat().st_size, f"bundle size drifted: {relative}"
        )

    actual = {
        path.relative_to(artifacts).as_posix()
        for path in artifacts.rglob("*")
        if path.is_file() and path.name != "bundle.json" and "publication" not in path.relative_to(artifacts).parts
    }
    require(actual == declared, f"bundle file closure drifted: declared={sorted(declared)}, actual={sorted(actual)}")

    output_entry = exact_keys(bundle["output"], {"path", "sha256", "bytes"}, "bundle output")
    require(output_entry["path"] == (logical_output or str(output)), "bundle output path drifted")
    require(output.is_file() and not output.is_symlink(), "published output is not a regular file")
    require(output_entry["sha256"] == f"sha256:{sha256_file(output)}", "bundle output hash drifted")
    require(
        type(output_entry["bytes"]) is int and output_entry["bytes"] == output.stat().st_size,
        "bundle output size drifted",
    )


def require_success_artifact_closure(
    artifacts: Path,
    output: Path,
    render_id: str,
    *,
    logical_output: str | None = None,
) -> None:
    require_regular_tree(artifacts, "success artifacts")
    files = {path.name for path in artifacts.iterdir() if path.is_file()}
    directories = {path.name for path in artifacts.iterdir() if path.is_dir()}
    require(files <= CAPTURED_ROOT_FILES | {"bundle.json"}, f"success root files drifted: {sorted(files)}")
    require(
        CAPTURED_REQUIRED_ROOT_FILES | {"bundle.json"} <= files, f"success root files are incomplete: {sorted(files)}"
    )
    require(
        directories <= {"pages", "publication", "resources"}, f"success root directories drifted: {sorted(directories)}"
    )
    require(
        {"publication", "resources"} <= directories, f"success root directories are incomplete: {sorted(directories)}"
    )
    has_single_preview = "scene-preview.png" in files
    has_page_previews = "pages" in directories
    require(has_single_preview != has_page_previews, "success preview shape is ambiguous or absent")
    if has_page_previews:
        pages = list((artifacts / "pages").iterdir())
        require(bool(pages), "success pages directory is empty")
        for page in pages:
            require(
                page.is_file() and not page.is_symlink() and PAGE_PREVIEW_RE.fullmatch(page.name) is not None,
                f"success page preview is invalid: {page.name}",
            )
    require_resource_directory(artifacts / "resources", "success")
    require_publication_directory(artifacts / "publication", COMMITTED_PUBLICATION_FILES, "success")
    require_bundle_closure(artifacts, output, render_id, logical_output=logical_output)


def require_failure_artifact_closure(artifacts: Path) -> None:
    require_regular_tree(artifacts, "failure artifacts")
    files = {path.name for path in artifacts.iterdir() if path.is_file()}
    directories = {path.name for path in artifacts.iterdir() if path.is_dir()}
    require(files <= FAILED_ROOT_FILES, f"failure root files drifted: {sorted(files)}")
    require(FAILED_REQUIRED_ROOT_FILES <= files, f"failure root files are incomplete: {sorted(files)}")
    if "render.png" in files:
        require(
            (artifacts / "render.png").read_bytes().startswith(b"\x89PNG\r\n\x1a\n"), "failure render.png is invalid"
        )
    require(directories == {"publication", "resources"}, f"failure root directories drifted: {sorted(directories)}")
    resources = require_resource_directory(artifacts / "resources", "failure")
    ledger_resources = failure_ledger_resources(artifacts / "resources.jsonl")
    require(
        resources == ledger_resources,
        f"failure resource closure drifted: files={sorted(resources)}, ledger={sorted(ledger_resources)}",
    )
    require_publication_directory(artifacts / "publication", PLANNED_PUBLICATION_FILES, "failure")


def is_render_id(value: Any) -> bool:
    return (
        isinstance(value, str)
        and value.startswith("sha256:")
        and len(value) == 71
        and all(character in "0123456789abcdef" for character in value[7:])
    )


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def record_process(case: Path, result: subprocess.CompletedProcess[bytes]) -> None:
    (case / "process.stdout").write_bytes(result.stdout)
    (case / "process.stderr").write_bytes(result.stderr)
    (case / "process.json").write_bytes(canonical_json_line({"returncode": result.returncode}))


def require_typed_path_failure(
    result: subprocess.CompletedProcess[bytes],
    *,
    label: str,
    artifacts: Path,
    output: Path,
    code: str,
    expected_message: str | None = None,
    message_prefix: str | None = None,
) -> dict[str, Any]:
    require(
        (expected_message is None) != (message_prefix is None),
        f"{label} must define exactly one message expectation",
    )
    require(result.returncode == 1, f"{label} exited {result.returncode}: {result.stderr!r}")
    frame = exact_keys(
        decode_single_json_frame(result.stdout, label),
        {"artifacts", "document_pdf", "engine", "error", "render_id", "status"},
        f"{label} terminal",
    )
    require(frame["status"] == "failed", f"{label} status drifted")
    require(frame["engine"] == "pliego", f"{label} engine identity drifted")
    require(frame["artifacts"] == str(artifacts), f"{label} artifacts path drifted")
    require(frame["document_pdf"] == str(output), f"{label} output path drifted")
    require(is_render_id(frame["render_id"]), f"{label} render ID is invalid")
    error = exact_keys(frame["error"], {"code", "message"}, f"{label} error")
    require(error["code"] == code, f"{label} code drifted: {error['code']!r}")
    message = error["message"]
    require(isinstance(message, str), f"{label} message is not a string")
    if expected_message is not None:
        require(message == expected_message, f"{label} message drifted: {message!r}")
    else:
        assert message_prefix is not None
        require(message.startswith(message_prefix), f"{label} message prefix drifted: {message!r}")
        require(message != message_prefix, f"{label} message omitted its platform diagnostic")
    require(result.stderr.endswith(STDERR_NEWLINE), f"{label} stderr has no platform line ending")
    stderr_body = result.stderr[: -len(STDERR_NEWLINE)]
    require(
        b"\r" not in stderr_body and b"\n" not in stderr_body,
        f"{label} stderr is not exactly one line: {result.stderr!r}",
    )
    expected_stderr = stderr_line(f"pliego: {code}: {message}")
    require(result.stderr == expected_stderr, f"{label} stderr drifted: {result.stderr!r}")
    return frame


def validate_binary_identity(probe: Any, binary_sha256: str) -> tuple[str, str]:
    root = exact_keys(probe, {"schema", "version", "engine", "contracts", "invocation"}, "contract probe")
    require(root["schema"] == "pliego.runtime-contract" and root["version"] == 1, "contract probe identity drifted")
    engine = exact_keys(
        root["engine"],
        {"name", "version", "api", "source_commit", "runtime"},
        "contract probe engine",
    )
    require(engine["name"] == "pliego", "contract probe engine name drifted")
    source_commit = engine["source_commit"]
    require(
        isinstance(source_commit, str) and COMMIT_RE.fullmatch(source_commit) is not None,
        "contract probe source commit is not canonical",
    )
    runtime = exact_keys(
        engine["runtime"],
        {"mode", "target", "binary_sha256", "servo_base"},
        "contract probe runtime",
    )
    target = runtime["target"]
    require(
        isinstance(target, str) and TARGET_RE.fullmatch(target) is not None,
        "contract probe target is not canonical",
    )
    require(runtime["mode"] == "one-shot", "contract probe runtime mode drifted")
    require(runtime["binary_sha256"] == binary_sha256, "contract probe binary hash drifted")
    require(SHA256_RE.fullmatch(runtime["binary_sha256"]) is not None, "contract probe binary hash is invalid")
    require(
        isinstance(runtime["servo_base"], str) and COMMIT_RE.fullmatch(runtime["servo_base"]) is not None,
        "contract probe Servo base is not canonical",
    )
    return source_commit, target


def contract_probe_case(pliego: Path, root: Path) -> tuple[str, str]:
    case = root / "contract-probe"
    case.mkdir()
    result = run([str(pliego), "--contract-probe"], cwd=case)
    require(result.returncode == 0, f"contract probe exited {result.returncode}: {result.stderr!r}")
    require(result.stderr == b"", f"contract probe wrote stderr: {result.stderr!r}")
    probe = decode_single_json_frame(result.stdout, "contract probe")
    identity = validate_binary_identity(probe, f"sha256:{sha256_file(pliego)}")
    record_process(case, result)
    return identity


def require_expected_binary_identity(
    observed_source_commit: str,
    observed_target: str,
    expected_source_commit: str,
    expected_target: str,
) -> None:
    require(
        expected_source_commit == observed_source_commit,
        f"requested source commit {expected_source_commit} differs from executable {observed_source_commit}",
    )
    require(
        expected_target == observed_target,
        f"requested target {expected_target} differs from executable {observed_target}",
    )


def proof_file_closure(root: Path) -> list[dict[str, Any]]:
    closure = []
    for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
        relative = path.relative_to(root).as_posix()
        closure.append(
            {
                "path": relative,
                "bytes": path.stat().st_size,
                "sha256": f"sha256:{sha256_file(path)}",
            }
        )
    return closure


def write_proof(root: Path, pliego: Path, fixture: Path, source_commit: str, target: str) -> None:
    fixture_files = ["explicit-fail.html", "local-success.html", "local-success.js"]
    proof = {
        "schema": "pliego.render-supervisor-process-proof",
        "version": 1,
        "binary_sha256": f"sha256:{sha256_file(pliego)}",
        "source_commit": source_commit,
        "target": target,
        "fixtures": {name: f"sha256:{sha256_file(fixture / name)}" for name in fixture_files},
        "cases": [
            "contract-probe",
            "parser-failure",
            "preflight-output-exists",
            "preflight-overlap",
            "preflight-publication-parent-missing",
            "rejected-worker-capability",
            "request-failure",
            "relative-explicit-success",
            "shorthand-success",
            "shorthand-typed-failure",
            "success",
            "typed-failure",
        ],
        "files": proof_file_closure(root),
    }
    (root / "proof.json").write_bytes(canonical_json_line(proof))


def success_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "success"
    case.mkdir()
    artifacts = case / "artifacts"
    output = case / "document.pdf"
    result = run(
        [
            str(pliego),
            "render",
            "local-success.html",
            "--output",
            str(output),
            "--artifacts",
            str(artifacts),
        ],
        cwd=fixture,
    )
    application_top_level = {path.name for path in case.iterdir()}
    record_process(case, result)
    require(result.returncode == 0, f"success exited {result.returncode}: {result.stderr!r}")
    require(result.stderr == b"", f"success wrote stderr: {result.stderr!r}")
    summary = decode_single_json_frame(result.stdout, "success")
    require(summary.get("status") == "rendered", "success status is not rendered")
    require(Path(summary.get("artifacts", "")) == artifacts, "success artifacts path drifted")
    require(Path(summary.get("document_pdf", "")) == output, "success output path drifted")
    require(is_render_id(summary.get("render_id")), "success render ID is invalid")
    require(output.is_file(), "success did not publish the requested PDF")
    require((artifacts / "bundle.json").is_file(), "success did not seal bundle.json")
    require_success_artifact_closure(artifacts, output, summary["render_id"])
    require_no_private_container(case, "success")
    require(
        application_top_level == {"artifacts", "document.pdf"},
        f"success top-level closure drifted: {sorted(application_top_level)}",
    )


def shorthand_success_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "shorthand-success"
    case.mkdir()
    temporary = case / "tmp"
    temporary.mkdir()
    result = run(
        [str(pliego), "local-success.html"],
        cwd=fixture,
        env_overrides={"TMPDIR": str(temporary), "TEMP": str(temporary), "TMP": str(temporary)},
    )
    require(result.returncode == 0, f"shorthand success exited {result.returncode}: {result.stderr!r}")
    require(result.stderr == b"", f"shorthand success wrote stderr: {result.stderr!r}")
    summary = decode_single_json_frame(result.stdout, "shorthand success")
    require(summary.get("status") == "rendered", "shorthand success status is not rendered")
    artifacts = Path(summary.get("artifacts", ""))
    output = Path(summary.get("document_pdf", ""))
    require(artifacts.is_absolute(), "shorthand artifacts path is not absolute")
    require(output.is_absolute(), "shorthand PDF path is not absolute")
    require(output == artifacts / "document.pdf", "shorthand PDF is not artifact-owned")
    require(artifacts.parent.samefile(temporary), "shorthand artifacts escaped the configured temporary root")
    require(is_render_id(summary.get("render_id")), "shorthand success render ID is invalid")
    require_success_artifact_closure(artifacts, output, summary["render_id"])
    require_no_private_container(temporary, "shorthand success")
    temporary_entries = list(temporary.iterdir())
    require(
        len(temporary_entries) == 1 and temporary_entries[0].samefile(artifacts),
        "shorthand success temporary closure drifted",
    )
    record_process(case, result)


def shorthand_typed_failure_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "shorthand-typed-failure"
    case.mkdir()
    temporary = case / "tmp"
    temporary.mkdir()
    result = run(
        [str(pliego), "explicit-fail.html"],
        cwd=fixture,
        env_overrides={"TMPDIR": str(temporary), "TEMP": str(temporary), "TMP": str(temporary)},
    )
    require(result.returncode == 1, f"shorthand failure exited {result.returncode}: {result.stderr!r}")
    frame = decode_single_json_frame(result.stdout, "shorthand typed failure")
    require(frame.get("status") == "failed", "shorthand failure status drifted")
    require(
        frame.get("error") == {"code": "FIXTURE_READINESS_FAILED", "message": "expected explicit failure"},
        f"shorthand failure identity drifted: {frame.get('error')!r}",
    )
    require(
        result.stderr == stderr_line("pliego: FIXTURE_READINESS_FAILED: expected explicit failure"),
        f"shorthand failure stderr drifted: {result.stderr!r}",
    )
    artifacts = Path(frame.get("artifacts", ""))
    output = Path(frame.get("document_pdf", ""))
    require(artifacts.is_absolute(), "shorthand failure artifacts path is not absolute")
    require(output == artifacts / "document.pdf", "shorthand failure PDF is not artifact-owned")
    require(artifacts.parent.samefile(temporary), "shorthand failure artifacts escaped the temporary root")
    require_failure_artifact_closure(artifacts)
    require(not output.exists(), "shorthand failure published a PDF")
    require(not (artifacts / "document.pdf").exists(), "shorthand failure exposed a diagnostic PDF")
    require_no_private_container(temporary, "shorthand typed failure")
    temporary_entries = list(temporary.iterdir())
    require(
        len(temporary_entries) == 1 and temporary_entries[0].samefile(artifacts),
        "shorthand failure temporary closure drifted",
    )
    record_process(case, result)


def relative_explicit_success_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "relative-explicit-success"
    case.mkdir()
    shutil.copy2(fixture / "local-success.html", case / "local-success.html")
    shutil.copy2(fixture / "local-success.js", case / "local-success.js")
    result = run(
        [
            str(pliego),
            "render",
            "local-success.html",
            "--output",
            "document.pdf",
            "--artifacts",
            "artifacts",
        ],
        cwd=case,
    )
    require(result.returncode == 0, f"relative success exited {result.returncode}: {result.stderr!r}")
    require(result.stderr == b"", f"relative success wrote stderr: {result.stderr!r}")
    summary = decode_single_json_frame(result.stdout, "relative explicit success")
    require(summary.get("status") == "rendered", "relative success status is not rendered")
    require(summary.get("document_pdf") == "document.pdf", "relative output spelling was not preserved")
    artifacts = case / "artifacts"
    output = case / "document.pdf"
    reported_artifacts = Path(summary.get("artifacts", ""))
    require(reported_artifacts.is_absolute(), "relative artifacts path is not physically bound")
    require(reported_artifacts.samefile(artifacts), "relative artifacts physical identity drifted")
    require_success_artifact_closure(
        artifacts,
        output,
        summary["render_id"],
        logical_output="document.pdf",
    )
    require_no_private_container(case, "relative explicit success")
    require_top_level(
        case,
        {"artifacts", "document.pdf", "local-success.html", "local-success.js"},
        "relative explicit success",
    )
    record_process(case, result)


def parser_failure_case(pliego: Path, root: Path) -> None:
    case = root / "parser-failure"
    case.mkdir()
    result = run([str(pliego), "--definitely-invalid"], cwd=case)
    expected = {
        "status": "failed",
        "error": {
            "code": "INVALID_REQUEST",
            "message": "unknown option: --definitely-invalid",
        },
    }
    require(result.returncode == 2, f"parser failure exited {result.returncode}")
    require(result.stdout == canonical_json_line(expected), "parser failure stdout drifted")
    require(
        result.stderr == stderr_line("pliego: INVALID_REQUEST: unknown option: --definitely-invalid"),
        f"parser failure stderr drifted: {result.stderr!r}",
    )
    require(list(case.iterdir()) == [], "parser failure created filesystem output")
    record_process(case, result)


def request_failure_case(pliego: Path, root: Path) -> None:
    case = root / "request-failure"
    case.mkdir()
    missing = case / "missing.html"
    artifacts = case / "artifacts"
    output = case / "document.pdf"
    result = run(
        [
            str(pliego),
            "render",
            missing.name,
            "--output",
            str(output),
            "--artifacts",
            str(artifacts),
        ],
        cwd=case,
    )
    expected_stderr = stderr_line(f"pliego: document is unavailable: {canonical_missing_path(missing)}")
    require(result.returncode == 2, f"request failure exited {result.returncode}")
    require(result.stdout == b"", f"request failure wrote stdout: {result.stdout!r}")
    require(
        result.stderr == expected_stderr,
        f"request failure stderr drifted: {result.stderr!r}",
    )
    require(not artifacts.exists(), "request failure created public artifacts")
    require(not output.exists(), "request failure created a PDF")
    require_no_private_container(case, "request failure")
    record_process(case, result)


def preflight_overlap_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "preflight-overlap"
    case.mkdir()
    artifacts = case / "artifacts"
    output = artifacts / "nested" / "document.pdf"
    result = run(
        [
            str(pliego),
            "render",
            "local-success.html",
            "--output",
            str(output),
            "--artifacts",
            str(artifacts),
        ],
        cwd=fixture,
    )
    require_typed_path_failure(
        result,
        label="preflight overlap",
        artifacts=artifacts,
        output=output,
        code="OUTPUT_ARTIFACTS_OVERLAP",
        expected_message="requested output must be outside the artifact directory",
    )
    require(not artifacts.exists(), "preflight overlap created public artifacts")
    require(not output.exists(), "preflight overlap published the requested PDF")
    require_no_private_container(case, "preflight overlap")
    require_top_level(case, set(), "preflight overlap")
    record_process(case, result)


def preflight_output_exists_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "preflight-output-exists"
    case.mkdir()
    artifacts = case / "artifacts"
    output = case / "document.pdf"
    caller_bytes = b"caller-owned output must remain unchanged\n"
    output.write_bytes(caller_bytes)
    result = run(
        [
            str(pliego),
            "render",
            "local-success.html",
            "--output",
            str(output),
            "--artifacts",
            str(artifacts),
        ],
        cwd=fixture,
    )
    require_typed_path_failure(
        result,
        label="preflight output exists",
        artifacts=artifacts,
        output=output,
        code="OUTPUT_ALREADY_EXISTS",
        expected_message=f"requested output already exists: {output}",
    )
    require(output.read_bytes() == caller_bytes, "preflight output-exists changed caller bytes")
    require(not artifacts.exists(), "preflight output-exists created public artifacts")
    require_no_private_container(case, "preflight output exists")
    require_top_level(case, {"document.pdf"}, "preflight output exists")
    record_process(case, result)


def preflight_publication_parent_missing_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "preflight-publication-parent-missing"
    case.mkdir()
    artifacts = case / "artifacts"
    output_parent = case / "missing-output-parent"
    output = output_parent / "document.pdf"
    result = run(
        [
            str(pliego),
            "render",
            "local-success.html",
            "--output",
            str(output),
            "--artifacts",
            str(artifacts),
        ],
        cwd=fixture,
    )
    require_typed_path_failure(
        result,
        label="preflight publication parent missing",
        artifacts=artifacts,
        output=output,
        code="PUBLICATION_TRANSACTION_FAILED",
        message_prefix="cannot begin publication transaction: ",
    )
    require(not output_parent.exists(), "preflight publication-parent created the missing parent")
    require(not artifacts.exists(), "preflight publication-parent created public artifacts")
    require_no_private_container(case, "preflight publication parent missing")
    require_top_level(case, set(), "preflight publication parent missing")
    record_process(case, result)


def rejected_worker_capability_case(pliego: Path, root: Path) -> None:
    case = root / "rejected-worker-capability"
    case.mkdir()
    staged = case / ".pliego-runtime-forged" / "artifacts"
    public = case / "artifacts"
    output = case / "document.pdf"
    result = run(
        [str(pliego), "--definitely-invalid"],
        cwd=case,
        env_overrides={
            "PLIEGO_INTERNAL_RENDER_WORKER_V1": "0" * 64,
            "PLIEGO_INTERNAL_RENDER_PARENT_PID": str(os.getpid()),
            "PLIEGO_INTERNAL_RENDER_STAGE_CONTAINER": str(staged.parent),
            "PLIEGO_INTERNAL_RENDER_STAGE_ARTIFACTS": str(staged),
            "PLIEGO_INTERNAL_RENDER_PUBLIC_ARTIFACTS": str(public),
            "PLIEGO_INTERNAL_RENDER_PUBLIC_OUTPUT": str(output),
        },
    )
    require(result.returncode == 2, f"rejected worker exited {result.returncode}")
    require(result.stdout == b"", "rejected worker fell through to public stdout")
    require(result.stderr == b"", "rejected worker fell through to public stderr")
    require(list(case.iterdir()) == [], "rejected worker capability mutated the filesystem")
    record_process(case, result)


def typed_failure_case(pliego: Path, fixture: Path, root: Path) -> None:
    case = root / "typed-failure"
    case.mkdir()
    artifacts = case / "artifacts"
    output = case / "document.pdf"
    result = run(
        [
            str(pliego),
            "render",
            "explicit-fail.html",
            "--output",
            str(output),
            "--artifacts",
            str(artifacts),
        ],
        cwd=fixture,
    )
    require(result.returncode == 1, f"typed failure exited {result.returncode}: {result.stderr!r}")
    frame = decode_single_json_frame(result.stdout, "typed failure")
    require(frame.get("status") == "failed", "typed failure status drifted")
    require(
        set(frame) == {"artifacts", "document_pdf", "engine", "error", "render_id", "status"},
        f"typed failure terminal keys drifted: {sorted(frame)}",
    )
    require(frame.get("artifacts") == str(artifacts), "typed failure artifacts path drifted")
    require(frame.get("document_pdf") == str(output), "typed failure output path drifted")
    require(frame.get("engine") == "pliego", "typed failure engine identity drifted")
    require(is_render_id(frame.get("render_id")), "typed failure render ID is invalid")
    require(
        frame.get("error")
        == {
            "code": "FIXTURE_READINESS_FAILED",
            "message": "expected explicit failure",
        },
        f"typed failure identity drifted: {frame.get('error')!r}",
    )
    require(
        result.stderr == stderr_line("pliego: FIXTURE_READINESS_FAILED: expected explicit failure"),
        f"typed failure stderr drifted: {result.stderr!r}",
    )
    failure_path = artifacts / "failure.json"
    require(failure_path.is_file(), "typed failure evidence was not promoted")
    failure = json.loads(failure_path.read_bytes(), parse_constant=reject_json_constant)
    require(
        failure == {key: frame[key] for key in ("status", "render_id", "error")},
        f"failure.json differs from the terminal result: {failure!r}",
    )
    require(not output.exists(), "typed failure published the requested PDF")
    require(not (artifacts / "document.pdf").exists(), "typed failure exposed a staged PDF")
    require(not (artifacts / "bundle.json").exists(), "typed failure exposed a success bundle")
    require_failure_artifact_closure(artifacts)
    require_no_private_container(case, "typed failure")
    require_top_level(case, {"artifacts"}, "typed failure")
    record_process(case, result)


def self_test() -> None:
    value = {"error": {"code": "FIXTURE"}, "status": "failed"}
    encoded = canonical_json_line(value)
    require(decode_single_json_frame(encoded, "self-test") == value, "frame decode failed")
    for invalid in [b"", b"{}", b"{}\r\n", b" {}\n", b"{}\n{}\n", b'{"value":NaN}\n']:
        expect_rejection(
            lambda invalid=invalid: decode_single_json_frame(invalid, "negative self-test"),
            f"invalid frame {invalid!r}",
        )
    require(is_render_id("sha256:" + "a" * 64), "valid render ID was rejected")
    require(not is_render_id("sha256:" + "A" * 64), "uppercase render ID was accepted")

    artifacts = Path("artifact-path")
    output = Path("document.pdf")
    path_failure = {
        "artifacts": str(artifacts),
        "document_pdf": str(output),
        "engine": "pliego",
        "error": {
            "code": "OUTPUT_ARTIFACTS_OVERLAP",
            "message": "requested output must be outside the artifact directory",
        },
        "render_id": "sha256:" + "a" * 64,
        "status": "failed",
    }
    path_failure_result = subprocess.CompletedProcess(
        [],
        1,
        canonical_json_line(path_failure),
        stderr_line("pliego: OUTPUT_ARTIFACTS_OVERLAP: requested output must be outside the artifact directory"),
    )
    require_typed_path_failure(
        path_failure_result,
        label="path failure self-test",
        artifacts=artifacts,
        output=output,
        code="OUTPUT_ARTIFACTS_OVERLAP",
        expected_message="requested output must be outside the artifact directory",
    )
    publication_message = "cannot begin publication transaction: fixture diagnostic"
    publication_failure = {
        **path_failure,
        "error": {"code": "PUBLICATION_TRANSACTION_FAILED", "message": publication_message},
    }
    publication_failure_result = subprocess.CompletedProcess(
        [],
        1,
        canonical_json_line(publication_failure),
        stderr_line(f"pliego: PUBLICATION_TRANSACTION_FAILED: {publication_message}"),
    )
    require_typed_path_failure(
        publication_failure_result,
        label="path failure prefix self-test",
        artifacts=artifacts,
        output=output,
        code="PUBLICATION_TRANSACTION_FAILED",
        message_prefix="cannot begin publication transaction: ",
    )
    expect_rejection(
        lambda: require_typed_path_failure(
            subprocess.CompletedProcess(
                [],
                2,
                path_failure_result.stdout,
                path_failure_result.stderr,
            ),
            label="negative path failure exit",
            artifacts=artifacts,
            output=output,
            code="OUTPUT_ARTIFACTS_OVERLAP",
            expected_message="requested output must be outside the artifact directory",
        ),
        "path failure exit code",
    )
    expect_rejection(
        lambda: require_typed_path_failure(
            subprocess.CompletedProcess([], 1, path_failure_result.stdout, b"wrong stderr\n"),
            label="negative path failure stderr",
            artifacts=artifacts,
            output=output,
            code="OUTPUT_ARTIFACTS_OVERLAP",
            expected_message="requested output must be outside the artifact directory",
        ),
        "path failure stderr",
    )
    expect_rejection(
        lambda: require_typed_path_failure(
            path_failure_result,
            label="negative path failure path",
            artifacts=Path("wrong-artifact-path"),
            output=output,
            code="OUTPUT_ARTIFACTS_OVERLAP",
            expected_message="requested output must be outside the artifact directory",
        ),
        "path failure artifact path",
    )
    expect_rejection(
        lambda: require_typed_path_failure(
            subprocess.CompletedProcess(
                [],
                1,
                canonical_json_line(
                    {
                        **publication_failure,
                        "error": {
                            "code": "PUBLICATION_TRANSACTION_FAILED",
                            "message": "cannot begin publication transaction: ",
                        },
                    }
                ),
                stderr_line("pliego: PUBLICATION_TRANSACTION_FAILED: cannot begin publication transaction: "),
            ),
            label="negative path failure diagnostic",
            artifacts=artifacts,
            output=output,
            code="PUBLICATION_TRANSACTION_FAILED",
            message_prefix="cannot begin publication transaction: ",
        ),
        "path failure missing platform diagnostic",
    )
    multiline_message = "cannot begin publication transaction: first\nsecond"
    expect_rejection(
        lambda: require_typed_path_failure(
            subprocess.CompletedProcess(
                [],
                1,
                canonical_json_line(
                    {
                        **publication_failure,
                        "error": {
                            "code": "PUBLICATION_TRANSACTION_FAILED",
                            "message": multiline_message,
                        },
                    }
                ),
                stderr_line(f"pliego: PUBLICATION_TRANSACTION_FAILED: {multiline_message}"),
            ),
            label="negative path failure framing",
            artifacts=artifacts,
            output=output,
            code="PUBLICATION_TRANSACTION_FAILED",
            message_prefix="cannot begin publication transaction: ",
        ),
        "path failure multiline stderr",
    )

    binary_sha256 = "sha256:" + "a" * 64
    probe = {
        "schema": "pliego.runtime-contract",
        "version": 1,
        "engine": {
            "name": "pliego",
            "version": "0.2.0",
            "api": 2,
            "source_commit": "1" * 40,
            "runtime": {
                "mode": "one-shot",
                "target": "x86_64-pc-windows-msvc",
                "binary_sha256": binary_sha256,
                "servo_base": "2" * 40,
            },
        },
        "contracts": [],
        "invocation": {},
    }
    require(
        validate_binary_identity(probe, binary_sha256) == ("1" * 40, "x86_64-pc-windows-msvc"),
        "contract probe identity self-test failed",
    )
    expect_rejection(
        lambda: require_expected_binary_identity(
            "1" * 40,
            "x86_64-pc-windows-msvc",
            "0" * 40,
            "not-a-target",
        ),
        "unbound caller proof identity",
    )

    with tempfile.TemporaryDirectory(prefix="pliego-supervisor-closure-self-test-") as temporary:
        artifacts = Path(temporary) / "artifacts"
        artifacts.mkdir()
        for name in FAILED_REQUIRED_ROOT_FILES:
            (artifacts / name).write_bytes(b"")
        resources = artifacts / "resources"
        resources.mkdir()
        publication = artifacts / "publication"
        publication.mkdir()
        for name in PLANNED_PUBLICATION_FILES:
            (publication / name).write_bytes(b"")
        (artifacts / "unexpected.bin").write_bytes(b"unexpected")
        expect_rejection(
            lambda: require_failure_artifact_closure(artifacts),
            "unexpected failure root artifact",
        )
        (artifacts / "unexpected.bin").unlink()
        (resources / "unexpected.bin").write_bytes(b"unexpected")
        expect_rejection(
            lambda: require_failure_artifact_closure(artifacts),
            "unexpected nested resource artifact",
        )
        (resources / "unexpected.bin").unlink()
        orphan = b"digest-valid but absent from resources.jsonl"
        orphan_digest = hashlib.sha256(orphan).hexdigest()
        (resources / orphan_digest).write_bytes(orphan)
        expect_rejection(
            lambda: require_failure_artifact_closure(artifacts),
            "digest-valid unreferenced failure resource",
        )
    print("render supervisor checker self-test: PASS")


def run_contract(
    pliego: Path,
    fixture: Path,
    root: Path,
    *,
    source_commit: str | None = None,
    target: str | None = None,
) -> None:
    observed_source_commit, observed_target = contract_probe_case(pliego, root)
    if source_commit is not None and target is not None:
        require_expected_binary_identity(observed_source_commit, observed_target, source_commit, target)
    success_case(pliego, fixture, root)
    shorthand_success_case(pliego, fixture, root)
    shorthand_typed_failure_case(pliego, fixture, root)
    relative_explicit_success_case(pliego, fixture, root)
    parser_failure_case(pliego, root)
    request_failure_case(pliego, root)
    preflight_overlap_case(pliego, fixture, root)
    preflight_output_exists_case(pliego, fixture, root)
    preflight_publication_parent_missing_case(pliego, fixture, root)
    rejected_worker_capability_case(pliego, root)
    typed_failure_case(pliego, fixture, root)
    if source_commit is not None and target is not None:
        write_proof(root, pliego, fixture, observed_source_commit, observed_target)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pliego", type=Path)
    parser.add_argument("--proof-directory", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--target")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    require(args.pliego is not None, "--pliego is required")
    pliego = args.pliego.resolve()
    require(pliego.is_file(), f"Pliego binary does not exist: {pliego}")
    fixture = Path(__file__).resolve().parent / "fixtures" / "document-session"
    if args.proof_directory is not None:
        require(
            isinstance(args.source_commit, str)
            and len(args.source_commit) == 40
            and all(character in "0123456789abcdef" for character in args.source_commit),
            "--source-commit must be one full lowercase Git SHA when retaining proof",
        )
        require(
            isinstance(args.target, str) and TARGET_RE.fullmatch(args.target) is not None,
            "--target must be one canonical Rust target triple when retaining proof",
        )
        root = args.proof_directory.resolve()
        require(not root.exists(), f"proof directory already exists: {root}")
        root.mkdir(parents=True)
        run_contract(
            pliego,
            fixture,
            root,
            source_commit=args.source_commit,
            target=args.target,
        )
    else:
        require(args.source_commit is None, "--source-commit requires --proof-directory")
        require(args.target is None, "--target requires --proof-directory")
        with tempfile.TemporaryDirectory(prefix="pliego-render-supervisor-") as temporary:
            run_contract(pliego, fixture, Path(temporary))
    print(f"render supervisor process contract: PASS (binary_sha256={sha256_file(pliego)})")


if __name__ == "__main__":
    main()
