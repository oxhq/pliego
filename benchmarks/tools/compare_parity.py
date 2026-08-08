#!/usr/bin/env python3

"""Differential parity runner for the Pliego engine seam.

The CLI adapter and the `DocumentEngine` boundary must behave identically. This
script runs two binaries against the same fixtures with identical arguments and
compares the *stable* outcome contract — status, typed failure, render id, scene
identity and capture status, page count, and the published PDF (hash + bytes).

Two modes:

* Differential (default): `--baseline A --candidate B` — every fixture must
  produce the same stable outcome from both binaries, or the run fails.
* Self-parity: omit `--candidate` (or pass the same path twice) — the same
  binary is run `--repeat N` times and must be deterministic.

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


def fail(message: str) -> None:
    print(f"compare_parity: {message}", file=sys.stderr)
    raise SystemExit(1)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


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
            fail(
                f"fixture {fixture_id!r} is not prepared; run "
                "python3 benchmarks/tools/generate_fixtures.py first"
            )


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
    result = subprocess.run(
        command, capture_output=True, text=True, timeout=600, cwd=input_path.parent
    )
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
    if (summary.get("error") or {}).get("message"):
        summary["error"]["message"] = summary["error"]["message"].replace(
            str(run_dir), "<artifacts>"
        )

    pdf_hash = None
    pdf_bytes = None
    pdf_path = summary.get("document_pdf")
    if pdf_path and Path(pdf_path).is_file():
        pdf_hash = sha256(Path(pdf_path))
        pdf_bytes = Path(pdf_path).stat().st_size

    scene = summary.get("scene") or {}
    # Compare the evidence itself, not only the summary that points to it.
    artifact_hashes: dict[str, str] = {}
    page_count = len(summary.get("scene_previews") or [])
    text = ""
    for key in ("scene_artifact", "fonts_artifact", "pdf_structure"):
        path = summary.get(key)
        if path and Path(path).is_file():
            artifact_hashes[key] = sha256(Path(path))
            if key == "pdf_structure":
                structure = json.loads(Path(path).read_text(encoding="utf-8"))
                page_count = structure.get("page_count", page_count)
                text = "".join(
                    page.get("expected_extracted_unicode", "")
                    for page in structure.get("pages", [])
                )
    return {
        "exit_code": result.returncode,
        "status": summary.get("status"),
        "error_code": (summary.get("error") or {}).get("code"),
        "error_message": (summary.get("error") or {}).get("message"),
        "render_id": summary.get("render_id"),
        "scene_schema": scene.get("schema"),
        "scene_version": scene.get("version"),
        "scene_hash": scene.get("hash"),
        "scene_capture_status": scene.get("capture_status"),
        "scene_capture_code": scene.get("capture_code"),
        "scene_preview_status": scene.get("preview_status"),
        "scene_unsupported_event_count": scene.get("unsupported_event_count"),
        "scene_text_mapping_gap_count": scene.get("text_mapping_gap_count"),
        "document_pdf_status": summary.get("document_pdf_status"),
        "page_count": page_count,
        "text_contains": {
            expected: expected in text for expected in correctness.get("text_contains", [])
        },
        "pdf_sha256": pdf_hash,
        "pdf_bytes": pdf_bytes,
        "artifact_hashes": artifact_hashes,
    }


def correctness_problems(
    label: str, fixture: dict[str, Any], outcome: dict[str, Any]
) -> list[str]:
    correctness = fixture.get("correctness", {})
    problems: list[str] = []
    if fixture.get("expect_failure"):
        expected_code = correctness.get("failure_code")
        if outcome["exit_code"] == 0:
            problems.append(f"{label}: expected a non-zero exit")
        if outcome["status"] != "failed":
            problems.append(f"{label}: expected status='failed', got {outcome['status']!r}")
        if outcome["error_code"] != expected_code:
            problems.append(
                f"{label}: expected error_code={expected_code!r}, got {outcome['error_code']!r}"
            )
        if correctness.get("pdf_published") is False and outcome["pdf_sha256"] is not None:
            problems.append(f"{label}: expected no published PDF")
        return problems

    if outcome["exit_code"] != 0:
        problems.append(f"{label}: expected exit_code=0, got {outcome['exit_code']!r}")
    if outcome["status"] != "rendered":
        problems.append(f"{label}: expected status='rendered', got {outcome['status']!r}")
    if outcome["error_code"] is not None:
        problems.append(f"{label}: unexpected error_code={outcome['error_code']!r}")
    if outcome["document_pdf_status"] != "rendered" or outcome["pdf_sha256"] is None:
        problems.append(f"{label}: expected a rendered PDF")
    expected_pages = correctness.get("page_count")
    if expected_pages is not None and outcome["page_count"] != expected_pages:
        problems.append(
            f"{label}: expected page_count={expected_pages}, got {outcome['page_count']!r}"
        )
    for expected, found in outcome["text_contains"].items():
        if not found:
            problems.append(f"{label}: expected document text {expected!r}")
    return problems


def diff(label: str, baseline: dict[str, Any], candidate: dict[str, Any]) -> list[str]:
    problems: list[str] = []
    for key in baseline:
        if baseline[key] != candidate[key]:
            problems.append(
                f"{label}: {key}: baseline={baseline[key]!r} candidate={candidate[key]!r}"
            )
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
    candidate = Path(args.candidate) if args.candidate else baseline
    if not candidate.is_file():
        fail(f"candidate binary not found: {candidate}")

    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    fixture_ids = args.fixture or sorted(manifest["fixtures"].keys())

    report: dict[str, Any] = {
        "tool": "compare_parity",
        "baseline": str(baseline.resolve()),
        "candidate": str(candidate.resolve()),
        "self_parity": candidate == baseline,
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
                problems.extend(
                    correctness_problems(
                        f"{fixture_id}[{label} {run_index}]", fixture, outcome
                    )
                )
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
                print(
                    f"[{fixture_id}] parity ok (both failed, {outcome['error_code']}) "
                    f"exit={outcome['exit_code']}"
                )
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
