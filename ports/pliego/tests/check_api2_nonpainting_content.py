#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Native API 2 proof for exact nonpainting zero-area/font-size-zero content.

Barcode-style zero/positive rectangles with NBSP or empty content retain their
visible bars and a positive-font control. No zero-size Text/Path may be emitted.
Positive-font descendants remain visible. Decorations, shadows and background
images retain their existing explicit exclusions. These are minimized source
regressions, not an adapted business corpus or general invisible-CSS support.
"""

from __future__ import annotations

import argparse
import copy
import json
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from check_api2_contract import (
    PROBE_TIMEOUT_SECONDS,
    build_execution_fixture,
    canonical_json,
    create_private_job_root,
    run_probe,
    sha256_bytes,
    sha256_file,
    validate_probe,
    verify_artifact,
)
from check_api2_fixed_content import process_timeout_seconds, resolve_font_resource


SIZE = {"width": 47622, "height": 67351}
MARGINS = dict.fromkeys(("top", "right", "bottom", "left"), 2721)
FONT = Path(__file__).parent / "fixtures/text-scene/Ahem.ttf"
BAR = {"x": 11577, "y": 2721, "width": 72, "height": 1800}
BLACK = {"r": 0, "g": 0, "b": 0, "a": 255}
CASES = (
    {"name": "zero-width-empty", "width": "0", "empty": True},
    {"name": "zero-width-nbsp", "width": "0"},
    {"name": "positive-width-empty", "width": "1.2", "empty": True},
    {"name": "positive-width-nbsp", "width": "1.2"},
    {"name": "zero-height-empty", "width": "1.2", "height": "0", "empty": True},
    {"name": "zero-height-nbsp", "width": "1.2", "height": "0"},
    {"name": "positive-font-descendant", "width": "1.2", "descendant": True},
    {"name": "reject-zero-text-decoration", "width": "0", "reject": "decoration"},
    {"name": "reject-zero-text-shadow", "width": "0", "reject": "text-shadow"},
    {"name": "reject-zero-box-shadow", "width": "0", "empty": True, "reject": "box-shadow"},
    {"name": "reject-positive-text-decoration", "width": "1.2", "descendant": True, "reject": "decoration"},
    {"name": "reject-zero-background-image", "width": "0", "empty": True, "reject": "image"},
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def has_bar(case: dict[str, Any]) -> bool:
    return case["width"] != "0" and case.get("height", "30") != "0"


def check_scene(scene: dict[str, Any], case: dict[str, Any], font_resource: str) -> dict[str, Any]:
    require(
        scene.get("schema") == "pliego.document-scene"
        and scene.get("version") == 2
        and scene.get("app_units_per_css_px") == 60,
        "not exact Scene v2",
    )
    pages = scene.get("pages", [])
    require(len(pages) == 1 and pages[0].get("number") == 1, "minimized barcode case changed page count")
    page = pages[0]
    require(
        page.get("style_source") == "request-defaults"
        and page.get("size_app_units") == SIZE
        and page.get("margins_app_units") == MARGINS,
        "source request geometry changed",
    )
    operations = page["operations"]
    require(all(op.get("type") in ("text", "path") for op in operations), "unexpected paint type")
    paths = [op for op in operations if op["type"] == "path"]
    texts = [op for op in operations if op["type"] == "text"]
    # Positioned descendants paint after normal-flow siblings, even when above
    # them geometrically. Compare reading-position order for these two labels;
    # still require the visible descendant to paint after its own opaque bar.
    texts.sort(key=lambda op: min((glyph["y"] for glyph in op.get("glyphs", [])), default=-1))
    require(len(paths) == int(has_bar(case)), "visible bar missing/duplicated or zero-area Path emitted")
    if paths:
        require(
            paths[0].get("bounds") == BAR and paths[0].get("fill") == BLACK and bool(paths[0].get("data")),
            "visible bar lost exact authored Au geometry/color",
        )
    if case.get("descendant"):
        descendant = [op for op in texts if op.get("text") == "A"]
        require(
            len(descendant) == 1 and operations.index(paths[0]) < operations.index(descendant[0]),
            "opaque bar covers its positive text descendant",
        )
    require(
        [op.get("text") for op in texts] == (["A", "KEEP"] if case.get("descendant") else ["KEEP"]),
        "positive text/control lost, duplicated, reordered or zero-font text retained",
    )
    expected_font = {"resource": font_resource, "face_index": 0, "variations": [], "synthetic_bold": False}
    for op in texts:
        require(
            op.get("font_size_app_units") == 720 and op.get("font") == expected_font,
            "positive text font identity/size changed or zero-size Text emitted",
        )
        glyphs = op.get("glyphs", [])
        require(len(glyphs) == len(op["text"]), "positive glyph count changed")
        x, y = (11577, 3537) if op["text"] == "A" else (2721, 5337)
        for index, glyph in enumerate(glyphs):
            require(all(type(glyph.get(key)) is int for key in ("x", "y", "advance")), "text lost integer authority")
            require(
                (glyph["x"], glyph["y"], glyph["advance"]) == (x + index * 720, y, 720),
                "positive glyph geometry changed",
            )
    return {
        "pages": 1,
        "visible_bars": len(paths),
        "positive_text": [op["text"] for op in texts],
        "zero_size_operations": 0,
    }


def build_fixture(repository: Path, root: Path, case: dict[str, Any]) -> tuple[bytes, Path]:
    payload, source = build_execution_fixture(
        repository, root, (repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes()
    )
    content = "" if case.get("empty") else "&nbsp;"
    if case.get("descendant"):
        content = '<span style="font-size:12px">A</span>'
    html = (
        '<!doctype html><html lang="en-US"><head><meta charset="utf-8"><link rel="stylesheet" href="styles.css"></head><body><div class="holder"><div class="bar">'
        + content
        + '</div></div><div class="control">KEEP</div></body></html>\n'
    )
    css = '@font-face{font-family:Ahem;src:url("font.ttf") format("truetype");}html,body{margin:0}body{font:12px/20px Ahem;color:#000}.holder{position:relative;width:147.6px;height:30px}.control{height:20px}'
    css += f".bar{{font:0px/20px Ahem;background-color:black;width:{case['width']}px;height:{case.get('height', '30')}px;position:absolute;left:147.6px;top:0}}"
    extra = {
        "decoration": "text-decoration:underline",
        "text-shadow": "text-shadow:1px 1px 1px red",
        "box-shadow": "box-shadow:1px 0 1px black",
        "image": "background-image:linear-gradient(black,black)",
    }
    if case.get("reject"):
        css += ".bar{" + extra[case["reject"]] + "}"
    (source / "input/document.html").write_text(html, encoding="utf-8", newline="\n")
    (source / "input/styles.css").write_text(css + "\n", encoding="utf-8", newline="\n")
    manifest = json.loads((source / "input-manifest.json").read_bytes())
    for entry in manifest["entries"]:
        path = source / "input" / entry["path"]
        entry.update(sha256=sha256_file(path), bytes=path.stat().st_size)
    manifest_bytes = canonical_json(manifest)
    (source / "input-manifest.json").write_bytes(manifest_bytes)
    request = json.loads(payload)
    request["input"]["manifest"].update(sha256=sha256_bytes(manifest_bytes), bytes=len(manifest_bytes))
    request["page"]["size"] = {"width_app_units": SIZE["width"], "height_app_units": SIZE["height"]}
    request["page"]["margins_app_units"] = MARGINS
    return canonical_json(request), source


def run_case(
    binary: Path, repository: Path, root: Path, case: dict[str, Any], probe: dict[str, Any], process_timeout: int
) -> dict[str, Any]:
    payload, source = build_fixture(repository, root, case)
    (root / "request.json").write_bytes(payload)
    job = root / "job"
    create_private_job_root(job)
    shutil.copy2(source / "input-manifest.json", job / "input-manifest.json")
    shutil.copytree(source / "input", job / "input")
    started = time.monotonic()
    try:
        process = subprocess.run(
            [str(binary), "render-api2"],
            cwd=job,
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=process_timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        (root / "stdout.json").write_bytes(error.stdout or b"")
        (root / "stderr.txt").write_bytes(error.stderr or b"")
        (root / "process.json").write_bytes(
            canonical_json(
                {
                    "outcome": "host-timeout",
                    "limit_seconds": process_timeout,
                    "wall_seconds": time.monotonic() - started,
                }
            )
        )
        raise
    (root / "stdout.json").write_bytes(process.stdout)
    (root / "stderr.txt").write_bytes(process.stderr)
    (root / "process.json").write_bytes(
        canonical_json(
            {
                "exit_code": process.returncode,
                "limit_seconds": process_timeout,
                "wall_seconds": time.monotonic() - started,
            }
        )
    )
    require(not process.stderr, "API 2 emitted stderr")
    result = json.loads(process.stdout)
    require(
        canonical_json(result) == process.stdout
        and result.get("schema") == "pliego.render-result"
        and result.get("version") == 1
        and result.get("api") == 2,
        "result framing/schema changed",
    )
    require(
        result.get("engine") == probe["engine"] and result.get("request") == json.loads(payload),
        "runtime/request identity lost",
    )
    require(result["diagnostics"]["retained"] and result["diagnostics"]["artifacts"], "missing retained diagnostics")
    diagnostics = [verify_artifact(job, item) for item in result["diagnostics"]["artifacts"]]
    if case.get("reject"):
        require(
            process.returncode == 1
            and result.get("status") == "failed"
            and result.get("error", {}).get("kind") == "artifact",
            "paint exclusion did not retain artifact failure",
        )
        require(result.get("delivery") is None and not (job / "delivery").exists(), "excluded paint published delivery")
        marker = "text-effects" if case["reject"] in ("decoration", "text-shadow") else "box"
        causes = [json.loads(path.read_bytes()) for path in diagnostics if path.suffix == ".json"]
        require(
            any(item.get("code") == "SCENE_ENCODING_FAILED" and marker in item.get("message", "") for item in causes),
            "paint exclusion lost its specific retained cause",
        )
        return {
            "outcome": "rejected-as-excluded",
            "kind": "artifact",
            "code": "SCENE_ENCODING_FAILED",
            "marker": marker,
        }
    require(
        process.returncode == 0 and result.get("status") == "success" and result.get("error") is None,
        f"supported nonpainting case failed: {result.get('error')}",
    )
    delivery = job / "delivery"
    for kind, filename in (("pdf", "document.pdf"), ("scene", "scene.json"), ("bundle", "bundle.json")):
        verify_artifact(delivery, result["delivery"][kind], filename)
    bundle = json.loads((delivery / "bundle.json").read_bytes())
    for item in bundle["entries"]:
        verify_artifact(delivery, item)
    pdf = delivery / "document.pdf"
    require(pdf.read_bytes().startswith(b"%PDF-"), "missing PDF header")
    font = resolve_font_resource(delivery, bundle)
    evidence = check_scene(json.loads((delivery / "scene.json").read_bytes()), case, font["resolved_sha256"])
    evidence["font_binding"] = font
    tool = shutil.which("pdftotext")
    if tool:
        extracted = subprocess.run(
            [tool, "-enc", "UTF-8", "-layout", str(pdf), "-"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=15,
            check=False,
        )
        (root / "pdf-text.txt").write_bytes(extracted.stdout)
        (root / "pdftotext.stderr.txt").write_bytes(extracted.stderr)
        require(extracted.returncode == 0, "PDF extraction failed")
        require(
            extracted.stdout.decode("utf-8").split() == (["A", "KEEP"] if case.get("descendant") else ["KEEP"]),
            "PDF visible text changed",
        )
        evidence["pdf_text"] = "passed"
    else:
        evidence["pdf_text"] = "not-run: pdftotext unavailable"
    return {"outcome": "accepted", "pdf_sha256": sha256_file(pdf), **evidence}


def fake_scene(case: dict[str, Any], font_resource: str) -> dict[str, Any]:
    operations = []
    if has_bar(case):
        operations.append(
            {
                "type": "path",
                "bounds": dict(BAR),
                "fill": dict(BLACK),
                "data": "M 11577 2721 L 11649 2721 L 11649 4521 L 11577 4521 Z",
            }
        )
    for text, x, y in ([("A", 11577, 3537)] if case.get("descendant") else []) + [("KEEP", 2721, 5337)]:
        operations.append(
            {
                "type": "text",
                "text": text,
                "font_size_app_units": 720,
                "font": {"resource": font_resource, "face_index": 0, "variations": [], "synthetic_bold": False},
                "glyphs": [{"x": x + index * 720, "y": y, "advance": 720} for index in range(len(text))],
            }
        )
    return {
        "schema": "pliego.document-scene",
        "version": 2,
        "app_units_per_css_px": 60,
        "pages": [
            {
                "number": 1,
                "style_source": "request-defaults",
                "size_app_units": dict(SIZE),
                "margins_app_units": dict(MARGINS),
                "operations": operations,
            }
        ],
    }


def self_test() -> None:
    require(
        len(CASES) == 12 and sum(bool(case.get("reject")) for case in CASES) == 5, "nonpainting case inventory changed"
    )
    font = sha256_file(FONT)
    for case in CASES:
        if not case.get("reject"):
            check_scene(fake_scene(case, font), case, font)
    case = CASES[6]
    scene = fake_scene(case, font)
    for change in (
        "missing-bar",
        "missing-control",
        "missing-descendant",
        "zero-text",
        "zero-path",
        "bar-position",
        "bar-color",
        "wrong-font",
        "wrong-glyph",
    ):
        invalid = copy.deepcopy(scene)
        operations = invalid["pages"][0]["operations"]
        if change == "missing-bar":
            operations.pop(0)
        elif change == "missing-control":
            operations.pop()
        elif change == "missing-descendant":
            operations.pop(1)
        elif change == "zero-text":
            operations[-1]["font_size_app_units"] = 0
        elif change == "zero-path":
            operations[0]["bounds"]["width"] = 0
        elif change == "bar-position":
            operations[0]["bounds"]["x"] += 1
        elif change == "bar-color":
            operations[0]["fill"]["a"] = 0
        elif change == "wrong-font":
            operations[-1]["font"]["resource"] = "sha256:" + "0" * 64
        else:
            operations[-1]["glyphs"][0]["advance"] = 0
        try:
            check_scene(invalid, case, font)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"nonpainting oracle accepted {change}")
    repository = Path(__file__).resolve().parents[3]
    with tempfile.TemporaryDirectory(prefix="pliego-nonpainting-self-test-") as temporary:
        first, _ = build_fixture(repository, Path(temporary) / "first", CASES[0])
        second, _ = build_fixture(repository, Path(temporary) / "second", CASES[0])
        golden = json.loads((repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes())
        require(
            json.loads(first)["settlement"]["limits"] == golden["settlement"]["limits"]
            and golden["settlement"]["limits"]["host_wall_ms"] == 60000,
            "qualification changed API default limits",
        )
        require(
            first == second and json.loads(first)["resources"] == {"network": "deny", "host_fonts": "deny"},
            "fixture closure changed/is not offline",
        )
    print("API 2 nonpainting checker self-test: ok (pure oracle/fixture checks; no native render)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--out", "--proof-directory", dest="out", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--process-timeout-seconds", type=process_timeout_seconds, default=65)
    parser.add_argument("--require-pdf-text", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.binary is None or args.out is None:
        parser.error("--binary and --out are required outside --self-test")
    if args.require_pdf_text and shutil.which("pdftotext") is None:
        parser.error("--require-pdf-text requires pdftotext on PATH")
    binary = args.binary.resolve(strict=True)
    output = args.out.resolve()
    output.mkdir(parents=True, exist_ok=False)
    probe_process = {"binary_bytes": binary.stat().st_size, "limit_seconds": PROBE_TIMEOUT_SECONDS, "outcome": "failed"}
    started = time.monotonic()
    try:
        probe, raw = run_probe(binary)
        probe_process["outcome"] = "success"
    finally:
        probe_process["wall_seconds"] = time.monotonic() - started
        (output / "probe-process.json").write_bytes(canonical_json(probe_process))
    (output / "probe.json").write_bytes(raw)
    engine = probe["engine"]
    validate_probe(
        probe,
        binary_sha256=sha256_file(binary),
        expected_version=engine["version"],
        expected_source_commit=args.source_commit or engine["source_commit"],
        expected_target=engine["runtime"]["target"],
        expected_servo_base=engine["runtime"]["servo_base"],
    )
    report = {
        "schema": "pliego.api2-nonpainting-proof",
        "version": 1,
        "engine": engine,
        "probe_process": probe_process,
        "process_timeout_seconds": args.process_timeout_seconds,
        "cases": [],
    }
    repository = Path(__file__).resolve().parents[3]
    for case in CASES:
        row = {"case": case, "passed": False}
        try:
            row.update(
                run_case(binary, repository, output / case["name"], case, probe, args.process_timeout_seconds),
                passed=True,
            )
        except (AssertionError, OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
            row["failure"] = str(error)
        process_file = output / case["name"] / "process.json"
        if process_file.is_file():
            row["process"] = json.loads(process_file.read_bytes())
        report["cases"].append(row)
        (output / "report.json").write_bytes(canonical_json(report))
        print(f"{case['name']}: {'passed' if row['passed'] else row['failure']}", flush=True)
    require(all(row["passed"] for row in report["cases"]), f"nonpainting regression failed; evidence: {output}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"API 2 nonpainting check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
