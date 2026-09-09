#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Retained, local exploratory Pliego/API 2 versus app-locked Browsershot runs.

Prepared HTML, cold PHP/native/browser processes, serial alternating order.
This does not measure the original application's URL/auth/queue workflow.
"""

from __future__ import annotations

import argparse
import collections
import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import time


HERE = Path(__file__).resolve().parent
ORACLE = HERE.parent / "tools" / "pdf_oracle.py"
PROVIDERS = ("browsershot", "pliego")
STATUSES = {"render_success", "unsupported", "render_failure", "transport_failure", "storage_failure", "setup_failure"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def save_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def load_fixture(path: Path) -> dict:
    value = json.loads((path / "fixture.json").read_text(encoding="utf-8"))
    if value.get("schema") != "pliego.application-fixture.v1" or not isinstance(value.get("id"), str):
        raise ValueError(f"Invalid fixture metadata: {path}")
    expected = value.get("expected", {}).get("textContains")
    if (
        not isinstance(expected, list)
        or not expected
        or not all(isinstance(text, str) and text.strip() for text in expected)
    ):
        raise ValueError(f"Fixture needs nonempty expected.textContains: {path}")
    if not isinstance(value.get("assets"), list):
        raise ValueError(f"Fixture needs an assets list: {path}")
    return value


def input_identity(path: Path, fixture: dict) -> dict:
    files = ["fixture.json", "input.html"] + [asset["path"] for asset in fixture["assets"]]
    hashes = {}
    for relative in files:
        source = (path / relative).resolve(strict=True)
        if not source.is_relative_to(path) or not source.is_file():
            raise ValueError(f"Fixture file escapes input root: {relative}")
        hashes[relative] = sha256(source)
    return hashes


def run_process(command: list[str], directory: Path, seconds: float, *, env: dict | None = None) -> dict:
    """Bound the direct invocation; retain stdout/stderr even on timeout/crash.

    Timeout cleanup is best effort (taskkill /T or POSIX process group), not
    evidence of production process-tree containment or a hostile-input sandbox.
    """
    save_json(directory / "command.json", command)
    started = time.perf_counter()
    record = {"timedOut": False}
    with (directory / "stdout.txt").open("wb") as stdout, (directory / "stderr.txt").open("wb") as stderr:
        try:
            process = subprocess.Popen(
                command, stdout=stdout, stderr=stderr, env=env, start_new_session=os.name != "nt"
            )
        except OSError as error:
            return {"launchError": str(error), "processWallMs": (time.perf_counter() - started) * 1000}
        try:
            record["exitCode"] = process.wait(timeout=seconds)
        except subprocess.TimeoutExpired:
            record["timedOut"] = True
            try:
                if os.name == "nt":
                    killed = subprocess.run(
                        ["taskkill.exe", "/PID", str(process.pid), "/T", "/F"], capture_output=True, timeout=15
                    )
                    record["cleanup"] = {
                        "method": "taskkill-tree",
                        "exitCode": killed.returncode,
                        "stdout": killed.stdout.decode(errors="replace"),
                        "stderr": killed.stderr.decode(errors="replace"),
                    }
                else:
                    os.killpg(process.pid, signal.SIGKILL)
                    record["cleanup"] = {"method": "process-group-kill", "signalSent": True}
            except (OSError, subprocess.TimeoutExpired) as error:
                record["cleanup"] = {"error": str(error)}
            if process.poll() is None:
                process.kill()
            record["exitCode"] = process.wait(timeout=15)
    record["processWallMs"] = (time.perf_counter() - started) * 1000
    return record


def node_preflight(args: argparse.Namespace, output: Path) -> dict:
    """Check the real configured module once, without creating timed samples."""
    directory = output / "node-preflight"
    directory.mkdir()
    try:
        node = Path(args.node).resolve(strict=True)
        modules = Path(args.node_modules).resolve(strict=True)
        if not node.is_file() or not (modules / "puppeteer" / "package.json").is_file():
            raise ValueError("Node executable or Puppeteer package is unavailable")
        command = [
            str(node),
            "-e",
            'const p=require("puppeteer");process.stdout.write(JSON.stringify({node:process.version,puppeteer:require("puppeteer/package.json").version,module:require.resolve("puppeteer"),launch:typeof p.launch}));',
        ]
        execution = run_process(
            command, directory, min(10, args.timeout_seconds), env={**os.environ, "NODE_PATH": str(modules)}
        )
        result = {"execution": execution, "status": "setup_failure", "untimed": True}
        if execution.get("exitCode") == 0 and not execution.get("timedOut"):
            identity = json.loads((directory / "stdout.txt").read_text(encoding="utf-8"))
            if identity.get("launch") != "function":
                raise ValueError("Configured Puppeteer does not expose launch()")
            result.update(status="pass", identity=identity)
        return result
    except (OSError, TypeError, ValueError) as error:
        return {"status": "setup_failure", "untimed": True, "error": str(error)}


def oracle_command(pdf: Path, fixture: dict) -> list[str]:
    expected = fixture["expected"]
    command = [
        sys.executable,
        str(ORACLE),
        "--pdf",
        str(pdf),
        "--page-width-points",
        str(210 / 25.4 * 72),
        "--page-height-points",
        str(297 / 25.4 * 72),
        "--dimension-tolerance-points",
        "1",
    ]
    for text in expected["textContains"]:
        command += ["--text-contains", text]
    for field, flag in (("fontFamilies", "--font-family"), ("linkTargets", "--link-target")):
        for value in expected.get(field, []):
            command += [flag, value]
    if "pageCount" in expected:
        command += ["--page-count", str(expected["pageCount"])]
    return command


def attempt(
    args: argparse.Namespace, fixture_path: Path, fixture: dict, provider: str, repeat: int, directory: Path
) -> dict:
    directory.mkdir()
    command = [
        args.php,
        str(HERE / "compare.php"),
        "--provider",
        provider,
        "--fixture",
        str(fixture_path),
        "--output",
        str(directory),
    ]
    for flag in ("app-autoload", "sdk-autoload", "binary", "chrome", "node", "node-modules"):
        value = getattr(args, flag.replace("-", "_"))
        if value:
            command += ["--" + flag, value]
    execution = run_process(command, directory, args.timeout_seconds)
    sample = {
        "fixture": fixture["id"],
        "provider": provider,
        "repeat": repeat,
        "directory": str(directory),
        "execution": execution,
    }
    if "launchError" in execution:
        sample["outcome"] = "setup_failure"
    elif execution["timedOut"]:
        sample["outcome"] = "timeout"
    else:
        try:
            result = json.loads((directory / "stdout.txt").read_text(encoding="utf-8"))
            if result.get("schema") != "pliego.application-render.v1" or result.get("status") not in STATUSES:
                raise ValueError("Adapter result schema/status is invalid")
            if result.get("provider") != provider or result.get("fixture") != fixture["id"]:
                # A setup failure can happen before fixture metadata is read.
                if result["status"] != "setup_failure":
                    raise ValueError("Adapter result identity mismatch")
            sample["adapter"] = result
            sample["outcome"] = result["status"]
            if result["status"] == "render_success":
                if execution["exitCode"] != 0:
                    raise ValueError("Successful adapter result with unsuccessful process exit")
                pdf = directory / "storage.pdf"
                stored = result.get("storage", {})
                if not stored.get("readbackVerified") or sha256(pdf) != stored.get("sha256"):
                    sample["outcome"] = "storage_failure"
                else:
                    oracle_dir = directory / "oracle"
                    oracle_dir.mkdir()
                    oracle_run = run_process(oracle_command(pdf, fixture), oracle_dir, args.timeout_seconds)
                    sample["oracleExecution"] = oracle_run
                    if (
                        oracle_run.get("timedOut")
                        or "launchError" in oracle_run
                        or oracle_run.get("exitCode") not in (0, 1)
                    ):
                        sample["outcome"] = "oracle_failure"
                    else:
                        oracle = json.loads((oracle_dir / "stdout.txt").read_text(encoding="utf-8"))
                        sample["oracle"] = oracle
                        sample["outcome"] = (
                            "correct"
                            if oracle.get("pass") is True and oracle_run["exitCode"] == 0
                            else "incorrect_output"
                        )
        except (OSError, ValueError, TypeError) as error:
            sample["outcome"] = "transport_failure"
            sample["harnessError"] = str(error)
    save_json(directory / "sample.json", sample)
    return sample


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def summarize(samples: list[dict]) -> list[dict]:
    """Count PDF-fact outcomes, not full correctness; never pool fixtures."""
    reports = []
    for fixture in dict.fromkeys(sample["fixture"] for sample in samples):
        report = {
            "fixture": fixture,
            "providers": {},
            "legacyOverPliegoMedianRatio": None,
            "acceptance": "PDF facts only; joint visual/document acceptance not established",
        }
        for provider in PROVIDERS:
            attempts = [sample for sample in samples if sample["fixture"] == fixture and sample["provider"] == provider]
            correct = [sample for sample in attempts if sample["outcome"] == "correct"]
            latency = [sample["execution"]["processWallMs"] for sample in correct]
            report["providers"][provider] = {
                "attempts": len(attempts),
                "correct": len(correct),
                "notCorrect": len(attempts) - len(correct),
                "outcomes": dict(collections.Counter(sample["outcome"] for sample in attempts)),
                "notCorrectFraction": (len(attempts) - len(correct)) / len(attempts) if attempts else None,
                "correctOnlyProcessWallMs": {
                    "p50": statistics.median(latency),
                    "p95NearestRank": percentile(latency, 0.95),
                }
                if latency
                else None,
            }
        # Text/page facts can pass on visibly broken output. This blocker census
        # intentionally cannot promote its samples to performance comparisons.
        report["ratioNote"] = (
            "Deferred: joint visual/document acceptance and performance qualification are not implemented"
        )
        reports.append(report)
    return reports


def markdown(report: dict) -> str:
    lines = [
        "# Application renderer comparison",
        "",
        "Local exploratory evidence; not release, independent-adoption, or full-application proof.",
        "",
        "Track: frozen prepared HTML; cold processes; serial, alternating provider order. PDF oracle is untimed.",
        "Process wall time includes PHP startup, dependency loading, rendering, local stream storage, and hash readback; input preparation is excluded.",
        "Node/Puppeteer dependency preflight runs once before all samples and is excluded from sample timings.",
        "No CPU, peak-memory, concurrency throughput, crash-durable storage, or production process-tree containment claim.",
        "PDF fact checks are not a substitute for visual review, complete pagination/row conservation, or accessibility validation.",
        "",
        "Performance comparisons are deferred until joint visual/document acceptance; use the existing hosted benchmark harness after qualification.",
        "",
        "| Fixture | Provider | PDF facts passed / attempts | Other outcomes | Fact-passing p50 / p95 ms (diagnostic) |",
        "| --- | --- | --- | --- | --- |",
    ]
    for item in report["summary"]:
        for provider, result in item["providers"].items():
            latency = result["correctOnlyProcessWallMs"]
            timing = f"{latency['p50']:.2f} / {latency['p95NearestRank']:.2f}" if latency else "n/a"
            outcomes = (
                ", ".join(f"{key}: {count}" for key, count in result["outcomes"].items() if key != "correct") or "none"
            )
            lines.append(
                f"| {item['fixture']} | {provider} | {result['correct']} / {result['attempts']} | {outcomes} | {timing} |"
            )
    for item in report["summary"]:
        lines += ["", f"{item['fixture']}: no speed ratio; {item['ratioNote']}."]
    if report.get("setupFailure"):
        lines += [
            "",
            "No samples were attempted: Node/Puppeteer dependency preflight failed. See node-preflight raw output and report.json.",
        ]
    lines += [
        "",
        "All attempt records, original PDFs, readback PDFs, commands, raw stdout/stderr, and oracle checks are retained next to this report.",
        "",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", action="append", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--php", default="php")
    for flag in ("app-autoload", "sdk-autoload", "binary", "chrome", "node", "node-modules"):
        parser.add_argument("--" + flag)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--timeout-seconds", type=float, default=90)
    args = parser.parse_args()
    if args.repeats < 1 or args.timeout_seconds <= 0:
        parser.error("Repeats and timeout must be positive")
    fixtures = [(Path(raw).resolve(strict=True), None) for raw in args.fixture]
    fixtures = [(path, load_fixture(path)) for path, _ in fixtures]
    if len({fixture["id"] for _, fixture in fixtures}) != len(fixtures):
        parser.error("Fixture IDs must be unique")
    output = args.output.resolve()
    if any(output == path or output.is_relative_to(path) for path, _ in fixtures):
        parser.error("Output must not be inside a fixture")
    output.mkdir(parents=True, exist_ok=False)
    frozen = {fixture["id"]: input_identity(path, fixture) for path, fixture in fixtures}
    report = {
        "schema": "pliego.application-comparison.v1",
        "evidenceLevel": "local-exploratory",
        "outcomeScope": "The internal correct outcome means PDF fact checks passed, not visual/document acceptance",
        "createdAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host": {"platform": platform.platform(), "python": sys.version},
        "track": "prepared-html-cold-process-local",
        "repeats": args.repeats,
        "fixtureMetadata": [fixture for _, fixture in fixtures],
        "inputHashes": frozen,
        "identities": {},
        "samples": [],
    }
    for name in ("binary", "chrome", "node", "app_autoload", "sdk_autoload"):
        value = getattr(args, name)
        if value and Path(value).is_file():
            report["identities"][name] = {"path": str(Path(value).resolve()), "sha256": sha256(Path(value))}
    for name in ("pdfinfo", "pdftotext", "pdffonts", "pdftoppm"):
        tool = shutil.which(name)
        report["identities"][name] = {"path": tool, "sha256": sha256(Path(tool))} if tool else {"unavailable": True}
    report["identities"]["oracle"] = {"sha256": sha256(ORACLE)}
    report["identities"]["adapter"] = {"sha256": sha256(HERE / "compare.php")}
    report["nodePreflight"] = node_preflight(args, output)
    if report["nodePreflight"]["status"] != "pass":
        report.update(setupFailure="node_dependency_preflight", summary=[])
        save_json(output / "run.json", report)
        save_json(output / "report.json", report)
        (output / "report.md").write_text(markdown(report), encoding="utf-8")
        print("Setup failure: Node/Puppeteer preflight; no samples attempted", flush=True)
        return 2
    save_json(output / "run.json", report)
    for index, (path, fixture) in enumerate(fixtures):
        for repeat in range(1, args.repeats + 1):
            order = PROVIDERS if repeat % 2 else tuple(reversed(PROVIDERS))
            for provider in order:
                directory = output / f"fixture-{index + 1:02d}-repeat-{repeat:03d}-{provider}"
                sample = attempt(args, path, fixture, provider, repeat, directory)
                if input_identity(path, fixture) != frozen[fixture["id"]]:
                    sample["outcome"] = "input_changed"
                    save_json(directory / "sample.json", sample)
                    report["samples"].append(sample)
                    save_json(output / "run.json", report)
                    raise RuntimeError("Frozen input changed during rendering; stop and retain evidence")
                report["samples"].append(sample)
                save_json(output / "run.json", report)
                print(f"{fixture['id']} repeat={repeat} {provider}: {sample['outcome']}", flush=True)
    report["summary"] = summarize(report["samples"])
    save_json(output / "report.json", report)
    (output / "report.md").write_text(markdown(report), encoding="utf-8")
    return 0 if all(sample["outcome"] == "correct" for sample in report["samples"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
