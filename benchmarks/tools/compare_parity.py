#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Differential parity runner for the Pliego engine seam.

The CLI adapter and the `DocumentEngine` boundary must behave identically. This
script runs two binaries against the same fixtures with identical arguments and
compares the *stable* outcome contract — status, typed failure, render id, scene
identity and capture status, page and link evidence, and the published PDF
(hash + bytes at the exact requested output path).

Two modes:

* Differential (default): `--baseline A --candidate B` — every fixture must
  produce the same stable outcome from both binaries, or the run fails.
* Self-parity: omit `--candidate` — the same binary is run `--repeat N` times
  and must be deterministic. Differential mode rejects byte-identical inputs.

Timing fields, artifact directory names, and absolute paths are deliberately
excluded: they are environment, not outcome. A parity break exits non-zero so
CI can treat it as a hard gate.

Usage:
    python3 benchmarks/tools/compare_parity.py \\
        --baseline /path/to/pliego-0.1.1 \\
        --candidate /path/to/pliego-new \\
        --fixture minimal-static --fixture invoice-showcase
    python3 benchmarks/tools/compare_parity.py \\
        --baseline /path/to/pliego --repeat 3 --out parity.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "benchmarks" / "manifest.toml"
ARTIFACT_FILES = {
    "scene_artifact": "scene.json",
    "fonts_artifact": "fonts.json",
    "pdf_structure": "pdf-structure.json",
}


def fail(message: str) -> None:
    print(f"compare_parity: {message}", file=sys.stderr)
    raise SystemExit(1)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_sha256(value: Any) -> str:
    encoded = json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def binary_identity(path: Path) -> dict[str, Any]:
    resolved = path.resolve()
    result = subprocess.run([str(resolved), "--version"], capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        fail(f"cannot identify binary {resolved}: exit {result.returncode}")
    version = "\n".join(part.strip() for part in (result.stdout, result.stderr) if part.strip())
    if not version:
        fail(f"cannot identify binary {resolved}: empty --version output")
    return {
        "path": str(resolved),
        "sha256": sha256(resolved),
        "bytes": resolved.stat().st_size,
        "version": version,
    }


def check_prep(fixture_id: str, fixture: dict[str, Any]) -> None:
    if fixture_id == "chartjs-showcase":
        base = ROOT / "ports" / "pliego" / "tests" / "fixtures" / "chartjs-report"
        missing = [
            p
            for p in (
                base / "node_modules" / "chart.js" / "dist" / "chart.umd.js",
                base / "ReportSans.ttf",
            )
            if not p.is_file()
        ]
        if missing:
            fail(
                f"fixture {fixture_id!r} is not prepared; missing {missing[0]}. "
                f"Run npm ci in {base} and copy DejaVuSans.ttf to ReportSans.ttf there."
            )
    for generated in ("ledger-20-pages", "statement-100-pages", "font-image-heavy"):
        if fixture_id == generated and not (ROOT / fixture["input"]).is_file():
            fail(f"fixture {fixture_id!r} is not prepared; run python3 benchmarks/tools/generate_fixtures.py first")


def fixture_identity(input_path: Path, fixture: dict[str, Any]) -> dict[str, Any]:
    assets: dict[str, str] = {}
    for asset in fixture.get("assets", []):
        asset_path = input_path.parent / asset
        if not asset_path.is_file():
            fail(f"fixture asset not found: {asset_path}")
        assets[asset] = sha256(asset_path)
    return {
        "input": str(input_path.relative_to(ROOT)),
        "input_sha256": sha256(input_path),
        "assets": assets,
    }


def reported_path(value: Any, cwd: Path) -> Path | None:
    if not isinstance(value, str) or not value:
        return None
    path = Path(value)
    try:
        return (path if path.is_absolute() else cwd / path).resolve()
    except (OSError, RuntimeError):
        return None


def json_object(path: Path) -> tuple[dict[str, Any], bool]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return {}, False
    return (value, True) if isinstance(value, dict) else ({}, False)


def has_pdf_envelope(path: Path) -> bool:
    try:
        size = path.stat().st_size
        with path.open("rb") as handle:
            header = handle.read(5)
            handle.seek(max(0, size - 1024))
            trailer = handle.read()
    except OSError:
        return False
    return header == b"%PDF-" and b"%%EOF" in trailer


def is_v1(value: Any) -> bool:
    return type(value) is int and value == 1


def stable_outcome(
    binary: Path,
    input_path: Path,
    run_dir: Path,
    correctness: dict[str, Any],
) -> dict[str, Any]:
    """Run one render and reduce it to the parity-relevant outcome contract.

    The engine resolves the input relative to the process cwd and rejects
    absolute paths (mirroring the PHP SDK, which runs with cwd set to the
    bundle directory). We therefore run with cwd = the fixture directory and
    pass the bare file name.
    """
    run_dir.mkdir(parents=True)
    output = run_dir / "document.pdf"
    artifacts = run_dir / "artifacts"
    command = [
        str(binary.resolve()),
        "render",
        input_path.name,
        "--output",
        str(output),
        "--artifacts",
        str(artifacts),
    ]
    result = subprocess.run(command, capture_output=True, text=True, timeout=600, cwd=input_path.parent)
    (run_dir / "stdout.log").write_text(result.stdout, encoding="utf-8")
    (run_dir / "stderr.log").write_text(result.stderr, encoding="utf-8")
    summary: dict[str, Any] = {}
    for line in result.stdout.splitlines():
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            summary = parsed
            break

    # Error messages may embed the ephemeral artifact directory; normalize it
    # away so the comparison stays on the outcome, not the retained path.
    error = summary.get("error") if isinstance(summary.get("error"), dict) else {}
    if isinstance(error.get("message"), str):
        error["message"] = error["message"].replace(str(run_dir), "<artifacts>")

    output_exists = output.exists() or output.is_symlink()
    output_is_regular = output.is_file() and not output.is_symlink()
    pdf_hash = sha256(output) if output_is_regular else None
    pdf_bytes = output.stat().st_size if output_is_regular else None
    reported_pdf = reported_path(summary.get("document_pdf"), input_path.parent)
    reported_artifacts = reported_path(summary.get("artifacts"), input_path.parent)

    scene = summary.get("scene") if isinstance(summary.get("scene"), dict) else {}
    # Read only the exact requested artifact files. Summary paths are evidence
    # to cross-check, never authority for which files the harness consumes.
    expected_paths = {key: artifacts / filename for key, filename in ARTIFACT_FILES.items()}
    expected_artifact_root = run_dir.resolve() / "artifacts"
    reported_paths = {key: reported_path(summary.get(key), input_path.parent) for key in ARTIFACT_FILES}
    artifact_path_matches = {
        key: reported == expected_artifact_root / ARTIFACT_FILES[key] if reported else None
        for key, reported in reported_paths.items()
    }
    artifact_hashes: dict[str, str] = {}
    for key, path in expected_paths.items():
        if path.is_file() and not path.is_symlink():
            artifact_hashes[key] = sha256(path)
    retained_pdf = artifacts / "document.pdf"
    retained_pdf_is_regular = retained_pdf.is_file() and not retained_pdf.is_symlink()
    if retained_pdf_is_regular:
        artifact_hashes["document_pdf_artifact"] = sha256(retained_pdf)

    captured_scene, scene_json_valid = json_object(expected_paths["scene_artifact"])
    fonts, fonts_json_valid = json_object(expected_paths["fonts_artifact"])
    structure, structure_json_valid = json_object(expected_paths["pdf_structure"])
    scene_pages = captured_scene.get("pages")
    scene_pages = scene_pages if isinstance(scene_pages, list) else None
    structure_pages = structure.get("pages")
    structure_pages = structure_pages if isinstance(structure_pages, list) else None
    summary_previews = summary.get("scene_previews")
    summary_previews = summary_previews if isinstance(summary_previews, list) else None

    link_targets = sorted(
        operation["target"]
        for page in scene_pages or []
        if isinstance(page, dict) and isinstance(page.get("operations"), list)
        for operation in page["operations"]
        if isinstance(operation, dict) and operation.get("type") == "link" and isinstance(operation.get("target"), str)
    )
    text = "".join(
        page.get("expected_extracted_unicode", "")
        for page in structure_pages or []
        if isinstance(page, dict) and isinstance(page.get("expected_extracted_unicode", ""), str)
    )
    structure_pdf = structure.get("pdf") if isinstance(structure.get("pdf"), dict) else {}
    structure_page_count = structure.get("page_count")
    structure_page_count = structure_page_count if type(structure_page_count) is int else None
    return {
        "exit_code": result.returncode,
        "status": summary.get("status"),
        "error_code": error.get("code"),
        "error_message": error.get("message"),
        "render_id": summary.get("render_id"),
        "artifacts_reported": reported_artifacts is not None,
        "artifacts_match_requested": reported_artifacts == expected_artifact_root if reported_artifacts else None,
        "artifact_path_matches": artifact_path_matches,
        "artifact_json_valid": {
            "scene_artifact": scene_json_valid,
            "fonts_artifact": fonts_json_valid,
            "pdf_structure": structure_json_valid,
        },
        "scene_schema": scene.get("schema"),
        "scene_version": scene.get("version"),
        "scene_hash": scene.get("hash"),
        "scene_capture_status": scene.get("capture_status"),
        "scene_capture_code": scene.get("capture_code"),
        "scene_preview_status": scene.get("preview_status"),
        "scene_unsupported_event_count": scene.get("unsupported_event_count"),
        "scene_text_mapping_gap_count": scene.get("text_mapping_gap_count"),
        "scene_artifact_schema": captured_scene.get("schema"),
        "scene_artifact_version": captured_scene.get("version"),
        "scene_artifact_page_count": len(scene_pages) if scene_pages is not None else None,
        "fonts_artifact_schema": fonts.get("schema"),
        "fonts_artifact_version": fonts.get("version"),
        "document_pdf_status": summary.get("document_pdf_status"),
        "pdf_structure_status": summary.get("pdf_structure_status"),
        "requested_output_exists": output_exists,
        "requested_output_is_regular": output_is_regular,
        "document_pdf_reported": reported_pdf is not None,
        "document_pdf_matches_output": reported_pdf == run_dir.resolve() / "document.pdf" if reported_pdf else None,
        "pdf_envelope_valid": has_pdf_envelope(output),
        "retained_pdf_bytes": retained_pdf.stat().st_size if retained_pdf_is_regular else None,
        "pdf_structure_schema": structure.get("schema"),
        "pdf_structure_version": structure.get("version"),
        "pdf_structure_pdf_artifact": structure_pdf.get("artifact"),
        "pdf_structure_pdf_sha256": structure_pdf.get("sha256"),
        "pdf_structure_pdf_bytes": structure_pdf.get("bytes"),
        "pdf_structure_pages_count": len(structure_pages) if structure_pages is not None else None,
        "summary_preview_page_count": len(summary_previews) if summary_previews is not None else None,
        "page_count": structure_page_count,
        "text_contains": {expected: expected in text for expected in correctness.get("text_contains", [])},
        "link_count": len(link_targets),
        "link_targets_sha256": canonical_sha256(link_targets),
        "pdf_sha256": pdf_hash,
        "pdf_bytes": pdf_bytes,
        "artifact_hashes": artifact_hashes,
    }


def correctness_problems(label: str, fixture: dict[str, Any], outcome: dict[str, Any]) -> list[str]:
    correctness = fixture.get("correctness", {})
    problems: list[str] = []
    if not isinstance(outcome["render_id"], str) or not outcome["render_id"].strip():
        problems.append(f"{label}: expected a non-empty render_id")
    if not outcome["artifacts_reported"] or not outcome["artifacts_match_requested"]:
        problems.append(f"{label}: expected artifacts to identify the requested artifact root")
    if outcome["document_pdf_reported"] and not outcome["document_pdf_matches_output"]:
        problems.append(f"{label}: reported document_pdf does not match the requested output path")
    if fixture.get("expect_failure"):
        expected_code = correctness.get("failure_code")
        if outcome["exit_code"] == 0:
            problems.append(f"{label}: expected a non-zero exit")
        if outcome["status"] != "failed":
            problems.append(f"{label}: expected status='failed', got {outcome['status']!r}")
        if outcome["error_code"] != expected_code:
            problems.append(f"{label}: expected error_code={expected_code!r}, got {outcome['error_code']!r}")
        if correctness.get("pdf_published") is False and outcome["requested_output_exists"]:
            problems.append(f"{label}: expected the requested output path to remain absent")
        return problems

    if outcome["exit_code"] != 0:
        problems.append(f"{label}: expected exit_code=0, got {outcome['exit_code']!r}")
    if outcome["status"] != "rendered":
        problems.append(f"{label}: expected status='rendered', got {outcome['status']!r}")
    if outcome["error_code"] is not None:
        problems.append(f"{label}: unexpected error_code={outcome['error_code']!r}")
    if (
        outcome["document_pdf_status"] != "rendered"
        or not outcome["requested_output_exists"]
        or not outcome["requested_output_is_regular"]
        or outcome["pdf_sha256"] is None
        or not outcome["pdf_envelope_valid"]
    ):
        problems.append(f"{label}: expected a rendered PDF")
    if not outcome["document_pdf_reported"] or not outcome["document_pdf_matches_output"]:
        problems.append(f"{label}: expected document_pdf to identify the requested output path")
    required_artifacts = {"scene_artifact", "fonts_artifact", "pdf_structure"}
    missing_artifacts = sorted(required_artifacts - outcome["artifact_hashes"].keys())
    if missing_artifacts:
        problems.append(f"{label}: missing required retained artifact(s): {', '.join(missing_artifacts)}")
    mismatched_artifact_paths = sorted(
        key for key in required_artifacts if outcome["artifact_path_matches"].get(key) is not True
    )
    if mismatched_artifact_paths:
        problems.append(f"{label}: artifact path mismatch(es): {', '.join(mismatched_artifact_paths)}")
    invalid_json = sorted(key for key in required_artifacts if outcome["artifact_json_valid"].get(key) is not True)
    if invalid_json:
        problems.append(f"{label}: invalid artifact JSON: {', '.join(invalid_json)}")

    expected_scene_hash = outcome["artifact_hashes"].get("scene_artifact")
    expected_scene_hash = f"sha256:{expected_scene_hash}" if expected_scene_hash else None
    if (
        outcome["scene_schema"] != "pliego.document-scene"
        or not is_v1(outcome["scene_version"])
        or outcome["scene_artifact_schema"] != "pliego.document-scene"
        or not is_v1(outcome["scene_artifact_version"])
        or outcome["scene_hash"] != expected_scene_hash
    ):
        problems.append(f"{label}: scene summary does not bind the exact scene artifact")
    if (
        outcome["scene_capture_status"] != "complete"
        or outcome["scene_capture_code"] is not None
        or outcome["scene_preview_status"] != "rendered"
        or type(outcome["scene_unsupported_event_count"]) is not int
        or outcome["scene_unsupported_event_count"] != 0
        or type(outcome["scene_text_mapping_gap_count"]) is not int
        or outcome["scene_text_mapping_gap_count"] != 0
    ):
        problems.append(f"{label}: scene capture is not complete and lossless")
    if outcome["fonts_artifact_schema"] != "pliego.font-report" or not is_v1(outcome["fonts_artifact_version"]):
        problems.append(f"{label}: expected pliego.font-report v1")

    retained_pdf_hash = outcome["artifact_hashes"].get("document_pdf_artifact")
    expected_pdf_hash = outcome["pdf_sha256"]
    if (
        outcome["pdf_structure_status"] != "rendered"
        or outcome["pdf_structure_schema"] != "pliego.pdf-structure"
        or not is_v1(outcome["pdf_structure_version"])
        or outcome["pdf_structure_pdf_artifact"] != "document.pdf"
        or outcome["pdf_structure_pdf_sha256"] != (f"sha256:{expected_pdf_hash}" if expected_pdf_hash else None)
        or type(outcome["pdf_structure_pdf_bytes"]) is not int
        or outcome["pdf_structure_pdf_bytes"] != outcome["pdf_bytes"]
        or retained_pdf_hash != expected_pdf_hash
        or outcome["retained_pdf_bytes"] != outcome["pdf_bytes"]
    ):
        problems.append(f"{label}: PDF structure does not bind the exact requested and retained PDF bytes")
    if (
        outcome["page_count"] is None
        or outcome["page_count"] != outcome["pdf_structure_pages_count"]
        or outcome["page_count"] != outcome["scene_artifact_page_count"]
        or outcome["page_count"] != outcome["summary_preview_page_count"]
    ):
        problems.append(f"{label}: page counts are internally inconsistent")
    expected_pages = correctness.get("page_count")
    if expected_pages is not None and outcome["page_count"] != expected_pages:
        problems.append(f"{label}: expected page_count={expected_pages}, got {outcome['page_count']!r}")
    for expected, found in outcome["text_contains"].items():
        if not found:
            problems.append(f"{label}: expected document text {expected!r}")
    expected_links = correctness.get("link_targets")
    if expected_links is not None:
        expected_links = sorted(expected_links)
        if outcome["link_count"] != len(expected_links):
            problems.append(f"{label}: expected link_count={len(expected_links)}, got {outcome['link_count']!r}")
        expected_digest = canonical_sha256(expected_links)
        if outcome["link_targets_sha256"] != expected_digest:
            problems.append(
                f"{label}: expected link_targets_sha256={expected_digest!r}, got {outcome['link_targets_sha256']!r}"
            )
    return problems


def diff(label: str, baseline: dict[str, Any], candidate: dict[str, Any]) -> list[str]:
    problems: list[str] = []
    for key in baseline:
        if baseline[key] != candidate[key]:
            problems.append(f"{label}: {key}: baseline={baseline[key]!r} candidate={candidate[key]!r}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description="Pliego differential parity runner")
    parser.add_argument("--baseline", required=True, help="baseline pliego binary")
    parser.add_argument("--candidate", help="candidate pliego binary (omit for self-parity)")
    parser.add_argument("--fixture", action="append", help="fixture ids (default: all)")
    parser.add_argument("--repeat", type=int, default=1, help="runs per binary per fixture")
    parser.add_argument("--out", help="write a JSON report instead of printing")
    args = parser.parse_args()
    if args.repeat < 1:
        fail("--repeat must be at least 1")

    baseline = Path(args.baseline)
    if not baseline.is_file():
        fail(f"baseline binary not found: {baseline}")
    self_parity = args.candidate is None
    candidate = Path(args.candidate) if args.candidate else baseline
    if not candidate.is_file():
        fail(f"candidate binary not found: {candidate}")

    baseline_identity = binary_identity(baseline)
    candidate_identity = baseline_identity if self_parity else binary_identity(candidate)
    if not self_parity and baseline_identity["sha256"] == candidate_identity["sha256"]:
        fail("differential mode requires distinct binary bytes; omit --candidate for an explicit self-parity run")

    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    fixture_ids = args.fixture or sorted(manifest["fixtures"].keys())

    report: dict[str, Any] = {
        "tool": "compare_parity",
        "baseline": str(baseline.resolve()),
        "candidate": str(candidate.resolve()),
        "self_parity": self_parity,
        "binaries": {
            "baseline": baseline_identity,
            "candidate": candidate_identity,
        },
        "evidence_inputs": {
            "harness_sha256": sha256(Path(__file__)),
            "manifest_sha256": sha256(MANIFEST),
        },
        "repeat": args.repeat,
        "results": {},
        "passed": True,
    }
    failures: list[str] = []
    ran_count = 0
    for fixture_id in fixture_ids:
        fixture = manifest["fixtures"].get(fixture_id)
        if fixture is None:
            fail(f"unknown fixture {fixture_id!r}")
        check_prep(fixture_id, fixture)
        input_path = ROOT / fixture["input"]
        if not input_path.is_file():
            fail(f"fixture {fixture_id!r} input not found: {input_path}")
        report["evidence_inputs"].setdefault("fixtures", {})[fixture_id] = fixture_identity(input_path, fixture)

        ran_count += 1
        run_root = Path(tempfile.mkdtemp(prefix=f"pliego-parity-{fixture_id}-"))
        correctness = fixture.get("correctness", {})
        baseline_outcomes = [
            stable_outcome(
                baseline,
                input_path,
                run_root / f"baseline-{index}",
                correctness,
            )
            for index in range(args.repeat)
        ]
        candidate_outcomes = [
            stable_outcome(
                candidate,
                input_path,
                run_root / f"candidate-{index}",
                correctness,
            )
            for index in range(args.repeat)
        ]

        problems: list[str] = []
        for label, outcomes in (
            ("baseline", baseline_outcomes),
            ("candidate", candidate_outcomes),
        ):
            for run_index, outcome in enumerate(outcomes):
                problems.extend(correctness_problems(f"{fixture_id}[{label} {run_index}]", fixture, outcome))
        # Cross-binary parity: every baseline run must match every candidate run.
        for index, baseline_outcome in enumerate(baseline_outcomes):
            for run_index, candidate_outcome in enumerate(candidate_outcomes):
                problems.extend(
                    diff(
                        f"{fixture_id}[baseline {index} vs candidate {run_index}]",
                        baseline_outcome,
                        candidate_outcome,
                    )
                )
        # Within-binary determinism: repeated runs of the same binary must match.
        for label, outcomes in (
            ("baseline", baseline_outcomes),
            ("candidate", candidate_outcomes),
        ):
            if len(outcomes) > 1:
                for run_index in range(1, len(outcomes)):
                    problems.extend(
                        diff(
                            f"{fixture_id}[{label} run 0 vs {run_index}]",
                            outcomes[0],
                            outcomes[run_index],
                        )
                    )

        report["results"][fixture_id] = {
            "baseline": baseline_outcomes,
            "candidate": candidate_outcomes,
            "problems": problems,
        }
        if problems:
            report["results"][fixture_id]["retained_artifacts"] = str(run_root)
            report["passed"] = False
            failures.extend(problems)
            for problem in problems:
                print(f"PARITY BREAK: {problem}")
            print(f"PARITY ARTIFACTS: {fixture_id}: {run_root}")
        else:
            shutil.rmtree(run_root)
            outcome = baseline_outcomes[0]
            state = outcome["status"]
            if state == "failed":
                print(f"[{fixture_id}] parity ok (both failed, {outcome['error_code']}) exit={outcome['exit_code']}")
            else:
                print(
                    f"[{fixture_id}] parity ok: {outcome['page_count']} pages, "
                    f"pdf {outcome['pdf_bytes']} B sha256={outcome['pdf_sha256'][:12]}..."
                )

    if args.out:
        Path(args.out).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if ran_count == 0:
        print("compare_parity: no fixtures actually ran (all skipped?)", file=sys.stderr)
        return 1
    if not report["passed"]:
        print(f"compare_parity: {len(failures)} parity break(s)", file=sys.stderr)
        return 1
    print(f"compare_parity: all {len(fixture_ids)} fixture(s) passed parity")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
