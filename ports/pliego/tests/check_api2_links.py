#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Packaged API 2 link proof: closed inputs, exact Scene v2 bounds, PDF URI annotations.

This is an annotation/geometry oracle, not a general visual or accessible-PDF
qualification. Negative cases retain their actual typed failure, or explicitly
report an unsafe/internal URL ignored as a nonlink; neither counts as link support.
PDF coordinates permit 0.02pt writer rounding. No PDF metadata determinism claim.
Inline background paint still lacks exact authority and is an explicit excluded
case, not functional link qualification. Positive duplicate suppression uses two
text descendants inside one unpainted owner; Rust units cover direct-owner dedup.
"""

from __future__ import annotations

import argparse
import copy
import io
import json
import math
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path
from typing import Any

import pypdf
from pypdf import PdfReader, PdfWriter
from pypdf.errors import PyPdfError
from pypdf.generic import ArrayObject, DictionaryObject, FloatObject, NameObject, TextStringObject

from check_api2_contract import (
    SHA256_RE,
    build_execution_fixture,
    canonical_json,
    create_private_job_root,
    run_probe,
    sha256_bytes,
    sha256_file,
    validate_probe,
    verify_artifact,
)


TOLERANCE_PT = 0.02
HTTPS = "https://example.test/document"
CASES = (
    {
        "name": "uri-schemes",
        "body": '<p><a href="http://example.test/plain">HTTP</a></p>'
        f'<p><a href="{HTTPS}">HTTPS</a></p><p><a href="mailto:reader@example.test">MAIL</a></p>',
        "links": {"http://example.test/plain": 1, HTTPS: 1, "mailto:reader@example.test": 1},
        "text": ["HTTP", "HTTPS", "MAIL"],
        "exact_rects": [
            ["http://example.test/plain", [2880, 3060, 2880, 720]],
            [HTTPS, [2880, 4140, 3600, 720]],
            ["mailto:reader@example.test", [2880, 5220, 2880, 720]],
        ],
    },
    {
        "name": "descendant-box",
        "body": f'<a class="box" href="{HTTPS}"><div><span>BOX</span></div></a>',
        "links": {HTTPS: 1},
        "text": ["BOX"],
        "css": ".box{display:block;width:120px;height:40px}",
        "bounds_size": [7200, 2400],
        "exact_rects": [[HTTPS, [2880, 2880, 7200, 2400]]],
    },
    {
        "name": "wrapped-inline",
        "body": f'<div class="wrap"><a href="{HTTPS}">FIRST SECOND THIRD</a></div>',
        "links": {HTTPS: 3},
        "text": ["FIRST", "SECOND", "THIRD"],
        "css": ".wrap{width:72px}",
        "exact_rects": [
            [HTTPS, [2880, 3060, 3600, 720]],
            [HTTPS, [2880, 4140, 4320, 720]],
            [HTTPS, [2880, 5220, 3600, 720]],
        ],
    },
    {
        "name": "second-page",
        "body": f'<p>FIRSTPAGE</p><p class="next"><a href="{HTTPS}">SECONDPAGE</a></p>',
        "links": {HTTPS: 1},
        "text": ["FIRSTPAGE", "SECONDPAGE"],
        "pages": 2,
        "link_page": 2,
        "css": ".next{break-before:page}",
        "exact_rects": [[HTTPS, [2880, 3060, 7200, 720]]],
    },
    {
        "name": "duplicate-suppression",
        "body": f'<a href="{HTTPS}"><span>FIRST</span><span>SECOND</span></a>',
        "links": {HTTPS: 1},
        "text": ["FIRST", "SECOND"],
        "exact_rects": [[HTTPS, [2880, 3060, 7920, 720]]],
    },
    {
        "name": "unsupported-inline-background",
        "body": f'<a href="{HTTPS}">DEDUP</a>',
        "exclude": "inline-background-authority",
        "failure": ["artifact", "SCENE_ENCODING_FAILED"],
        "css": "a{background-color:#eee}",
    },
    {
        "name": "unsafe-javascript",
        "body": '<a href="javascript:alert(1)">UNSAFE</a>',
        "exclude": "unsafe-uri",
        "failure": ["artifact", "SCENE_ENCODING_FAILED"],
        "may_ignore": True,
        "text": ["UNSAFE"],
    },
    {
        "name": "internal-fragment",
        "body": '<a href="#destination">INTERNAL</a><p id="destination">TARGET</p>',
        "exclude": "internal-uri",
        "failure": ["artifact", "SCENE_ENCODING_FAILED"],
        "may_ignore": True,
        "text": ["INTERNAL", "TARGET"],
    },
    {
        "name": "unsupported-decoration",
        "body": f'<a href="{HTTPS}">UNDERLINE</a>',
        "exclude": "decoration",
        "failure": ["artifact", "SCENE_ENCODING_FAILED"],
        "css": "a{text-decoration:underline}",
    },
    {
        "name": "unsupported-cross-page",
        "body": f'<a class="cross" href="{HTTPS}"><p>TOP</p><p>BOTTOM</p></a>',
        "exclude": "cross-page-box",
        "failure": ["capture", "SCENE_CAPTURE_FAILED"],
        "css": ".cross{display:block;height:1500px}",
    },
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def check_excluded_failure(case: dict[str, Any], kind: str, codes: list[str]) -> None:
    require(bool(case.get("exclude")), f"supported link failed: kind={kind}, codes={codes}")
    # These pairs are observed in the retained v0.3.3 baseline. In particular,
    # unsafe/internal failures can precede URL validation at missing authority;
    # the code pair alone is not proof that the URL-policy branch executed.
    expected_kind, expected_code = case["failure"]
    require(
        kind == expected_kind and codes == [expected_code], f"excluded link failure changed: kind={kind}, codes={codes}"
    )


def build_fixture(repository: Path, root: Path, case: dict[str, Any]) -> tuple[bytes, Path]:
    template = (repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes()
    payload, source = build_execution_fixture(repository, root, template)
    html = (
        '<!doctype html><html lang="en-US"><head><meta charset="utf-8">'
        '<link rel="stylesheet" href="styles.css"><title>API 2 links</title></head>'
        f"<body>{case['body']}</body></html>\n"
    )
    css = (
        '@font-face{font-family:Ahem;src:url("font.ttf") format("truetype");}'
        "html,body,p{margin:0}body{font-family:Ahem;font-size:12px;line-height:18px;color:#000}"
        "a{color:#000;text-decoration:none}" + case.get("css", "") + "\n"
    )
    (source / "input/document.html").write_text(html, encoding="utf-8", newline="\n")
    (source / "input/styles.css").write_text(css, encoding="utf-8", newline="\n")
    manifest = json.loads((source / "input-manifest.json").read_bytes())
    for entry in manifest["entries"]:
        path = source / "input" / entry["path"]
        entry.update(sha256=sha256_file(path), bytes=path.stat().st_size)
    manifest_bytes = canonical_json(manifest)
    (source / "input-manifest.json").write_bytes(manifest_bytes)
    request = json.loads(payload)
    request["input"]["manifest"].update(sha256=sha256_bytes(manifest_bytes), bytes=len(manifest_bytes))
    request["settlement"]["limits"]["host_wall_ms"] = 10000
    return canonical_json(request), source


def scene_links(scene: dict[str, Any], case: dict[str, Any]) -> list[list[dict[str, Any]]]:
    require(scene.get("schema") == "pliego.document-scene" and scene.get("version") == 2, "not Scene v2")
    require(scene.get("app_units_per_css_px") == 60, "scene app-unit scale changed")
    pages = scene["pages"]
    require(len(pages) == case.get("pages", 1), "unexpected scene page count")
    links_by_page, text = [], []
    for index, page in enumerate(pages):
        require(page["number"] == index + 1, "scene page identity changed")
        width, height = (page["size_app_units"][axis] for axis in ("width", "height"))
        require(type(width) is int and type(height) is int and min(width, height) > 0, "invalid exact page size")
        links, exact_rects = [], []
        for operation in page["operations"]:
            if operation["type"] == "text":
                text.append(operation["text"])
            if operation["type"] != "link":
                continue
            bounds = operation["bounds"]
            require(set(bounds) == {"x", "y", "width", "height"}, "link bounds fields changed")
            require(all(type(value) is int for value in bounds.values()), "link geometry is not exact integer Au")
            x, y, w, h = (bounds[key] for key in ("x", "y", "width", "height"))
            require(
                x >= 0 and y >= 0 and w > 0 and h > 0 and x + w <= width and y + h <= height,
                "link rectangle is empty, clipped or outside its page",
            )
            if "bounds_size" in case:
                require([w, h] == case["bounds_size"], "descendant link lost the authored box dimensions")
            links.append(
                {"uri": operation["target"], "rect": [x / 80, (height - y - h) / 80, (x + w) / 80, (height - y) / 80]}
            )
            exact_rects.append([operation["target"], [x, y, w, h]])
        expected = case.get("links", {}) if index + 1 == case.get("link_page", 1) else {}
        require(Counter(link["uri"] for link in links) == Counter(expected), "scene URI count/page differs")
        if "exact_rects" in case:
            # These authored-Ahem expectations also prevent a jointly shifted
            # scene and PDF from passing a merely self-consistent comparison.
            expected_rects = case["exact_rects"] if index + 1 == case.get("link_page", 1) else []
            require(exact_rects == expected_rects, "link placement differs from the authored fixture geometry")
        keys = [(link["uri"], tuple(link["rect"])) for link in links]
        require(len(keys) == len(set(keys)), "scene emitted a duplicate link rectangle")
        links_by_page.append(links)
    require(all(token in " ".join(text) for token in case.get("text", [])), "scene lost fixture text")
    return links_by_page


def check_pdf(reader: PdfReader, scene: dict[str, Any], expected: list[list[dict[str, Any]]]) -> list[Any]:
    require(len(reader.pages) == len(expected), "PDF page count differs from scene")
    observed = []
    for index, (page, links) in enumerate(zip(reader.pages, expected)):
        size = scene["pages"][index]["size_app_units"]
        want_box = [0.0, 0.0, size["width"] / 80, size["height"] / 80]
        require(page.rotation == 0 and page.get("/UserUnit", 1) == 1, "unexpected PDF page transform")
        for box in (page.mediabox, page.cropbox):
            require(
                all(abs(float(actual) - want) <= TOLERANCE_PT for actual, want in zip(box, want_box)),
                "PDF page dimensions/crop differ from scene",
            )
        annotations = []
        for reference in page["/Annots"] if "/Annots" in page else []:
            annotation = reference.get_object()
            require(annotation.get("/Subtype") == "/Link", "unexpected nonlink annotation")
            action = annotation["/A"] if "/A" in annotation else None
            require(
                isinstance(action, DictionaryObject) and action.get("/S") == "/URI" and "/Dest" not in annotation,
                "link is not a URI action",
            )
            uri = action["/URI"] if "/URI" in action else None
            require(isinstance(uri, str), "PDF URI is not a decoded text string")
            rect = [float(value) for value in annotation["/Rect"]]
            require(len(rect) == 4 and all(math.isfinite(value) for value in rect), "invalid PDF annotation rectangle")
            annotations.append({"uri": uri, "rect": rect})
        require(len(annotations) == len(links), "PDF annotation count differs from scene")

        def key(item: dict[str, Any]) -> tuple[Any, Any]:
            return item["uri"], item["rect"]

        for actual, want in zip(sorted(annotations, key=key), sorted(links, key=key)):
            require(actual["uri"] == want["uri"], "PDF URI differs from scene")
            require(
                all(abs(a - b) <= TOLERANCE_PT for a, b in zip(actual["rect"], want["rect"])),
                "PDF annotation rectangle differs from exact scene",
            )
        observed.append(annotations)
    return observed


def verify_delivery(job: Path, result: dict[str, Any]) -> tuple[dict[str, Any], Path]:
    delivery = job / "delivery"
    for kind, name in (("pdf", "document.pdf"), ("scene", "scene.json"), ("bundle", "bundle.json")):
        verify_artifact(delivery, result["delivery"][kind], name)
    bundle_bytes = (delivery / "bundle.json").read_bytes()
    bundle = json.loads(bundle_bytes)
    require(
        canonical_json(bundle) == bundle_bytes
        and bundle.get("schema") == "pliego.bundle-manifest"
        and bundle.get("version") == 1,
        "invalid canonical bundle",
    )
    paths = [entry["path"] for entry in bundle["entries"]]
    require(
        paths == sorted(set(paths)) and {"scene.json", "document.pdf"} <= set(paths) and "bundle.json" not in paths,
        "invalid bundle closure",
    )
    for entry in bundle["entries"]:
        verify_artifact(delivery, entry)
    scene = json.loads((delivery / "scene.json").read_bytes())
    require(scene.get("request_page") == result["request"]["page"], "scene lost request page authority")
    entries = {entry["path"]: entry for entry in bundle["entries"]}
    for page in scene["pages"]:
        require(page["size_app_units"] == {"width": 47622, "height": 67351}, "fixture is not exact A4")
        require(page["margins_app_units"] == result["request"]["page"]["margins_app_units"], "page margins changed")
        for operation in page["operations"]:
            resource = operation["font"]["resource"] if operation["type"] == "text" else operation.get("resource")
            if resource is not None:
                require(bool(SHA256_RE.fullmatch(resource)), "noncanonical scene resource address")
                require(
                    entries.get("resources/" + resource[7:], {}).get("sha256") == resource,
                    "scene resource is absent from hash-bound bundle closure",
                )
    return scene, delivery / "document.pdf"


def run_case(binary: Path, repository: Path, root: Path, case: dict[str, Any], probe: dict[str, Any]) -> dict[str, Any]:
    payload, source = build_fixture(repository, root, case)
    (root / "request.json").write_bytes(payload)
    job = root / "job"
    create_private_job_root(job)
    shutil.copy2(source / "input-manifest.json", job / "input-manifest.json")
    shutil.copytree(source / "input", job / "input")
    try:
        completed = subprocess.run(
            [str(binary), "render-api2"],
            cwd=job,
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=30,
        )
    except subprocess.TimeoutExpired as error:
        (root / "stdout.json").write_bytes(error.stdout or b"")
        (root / "stderr.txt").write_bytes(error.stderr or b"")
        (root / "process.json").write_bytes(canonical_json({"outcome": "host-timeout", "limit_seconds": 30}))
        raise
    (root / "stdout.json").write_bytes(completed.stdout)
    (root / "stderr.txt").write_bytes(completed.stderr)
    (root / "process.json").write_bytes(canonical_json({"exit_code": completed.returncode}))
    result = json.loads(completed.stdout)
    require(not completed.stderr and canonical_json(result) == completed.stdout, "noncanonical result/stderr")
    require(
        result.get("schema") == "pliego.render-result" and result.get("version") == 1 and result.get("api") == 2,
        "not RenderResult v1",
    )
    require(result.get("request") == json.loads(payload) and result.get("engine") == probe["engine"], "identity lost")
    require(result["diagnostics"]["retained"] and result["diagnostics"]["artifacts"], "missing diagnostics")
    diagnostics = [verify_artifact(job, entry) for entry in result["diagnostics"]["artifacts"]]
    if result.get("status") == "failed":
        require(
            completed.returncode == 1 and result.get("delivery") is None and not (job / "delivery").exists(),
            "failed render published partial delivery or wrong process status",
        )
        kind = result.get("error", {}).get("kind")
        codes = [json.loads(path.read_bytes()).get("code") for path in diagnostics if path.suffix == ".json"]
        codes = [code for code in codes if isinstance(code, str) and code]
        require(isinstance(kind, str) and bool(kind) and bool(codes), "failure has no typed retained cause")
        check_excluded_failure(case, kind, codes)
        return {"outcome": "rejected-as-excluded", "error_kind": kind, "codes": codes}
    require(
        completed.returncode == 0 and result.get("status") == "success" and result.get("error") is None,
        "render did not succeed",
    )
    require(not case.get("exclude") or case.get("may_ignore"), "excluded geometry was accepted")
    scene, pdf = verify_delivery(job, result)
    expected = scene_links(scene, case)
    reader = PdfReader(pdf, strict=True)
    annotations = check_pdf(reader, scene, expected)
    text = "\n".join(page.extract_text() or "" for page in reader.pages)
    (root / "pdf-text.txt").write_text(text, encoding="utf-8", newline="\n")
    require(all(token in text for token in case.get("text", [])), "PDF lost fixture text")
    (root / "annotations.json").write_bytes(canonical_json(annotations))
    return {
        "outcome": "ignored-as-nonlink" if case.get("exclude") else "accepted",
        "pdf_sha256": sha256_file(pdf),
        "annotations_per_page": [len(page) for page in annotations],
    }


def synthetic_pdf(pages: list[list[dict[str, Any]]]) -> PdfReader:
    writer = PdfWriter()
    for index, page_annotations in enumerate(pages):
        writer.add_blank_page(width=100, height=200)
        for annotation in page_annotations:
            writer.add_annotation(
                index,
                DictionaryObject(
                    {
                        NameObject("/Subtype"): NameObject("/Link"),
                        NameObject("/Rect"): ArrayObject([FloatObject(value) for value in annotation["rect"]]),
                        NameObject("/A"): DictionaryObject(
                            {
                                NameObject("/S"): NameObject("/URI"),
                                NameObject("/URI"): TextStringObject(annotation["uri"]),
                            }
                        ),
                    }
                ),
            )
    output = io.BytesIO()
    writer.write(output)
    output.seek(0)
    return PdfReader(output, strict=True)


def self_test() -> None:
    for excluded in (case for case in CASES if case.get("exclude")):
        kind, code = excluded["failure"]
        check_excluded_failure(excluded, kind, [code])
        for wrong_kind, wrong_codes in (("input", [code]), (kind, ["WRONG_CODE"]), (kind, []), (kind, [code, code])):
            try:
                check_excluded_failure(excluded, wrong_kind, wrong_codes)
            except AssertionError:
                pass
            else:
                raise AssertionError("exclusion oracle accepted a changed failure pair")
    case = {"links": {HTTPS: 1}, "pages": 2, "exact_rects": [[HTTPS, [800, 1600, 2400, 800]]]}
    scene = {
        "schema": "pliego.document-scene",
        "version": 2,
        "app_units_per_css_px": 60,
        "pages": [
            {"number": i + 1, "size_app_units": {"width": 8000, "height": 16000}, "operations": []} for i in range(2)
        ],
    }
    scene["pages"][0]["operations"] = [
        {"type": "link", "target": HTTPS, "bounds": {"x": 800, "y": 1600, "width": 2400, "height": 800}}
    ]
    expected = scene_links(scene, case)
    require(expected == [[{"uri": HTTPS, "rect": [10, 170, 40, 180]}], []], "Au/PDF axis transform changed")
    check_pdf(synthetic_pdf(expected), scene, expected)
    for corruption in ("uri", "missing", "duplicate", "rect", "page", "page-count"):
        wrong = copy.deepcopy(expected)
        if corruption == "uri":
            wrong[0][0]["uri"] = "https://wrong.test/"
        elif corruption == "missing":
            wrong[0].clear()
        elif corruption == "duplicate":
            wrong[0].append(copy.deepcopy(wrong[0][0]))
        elif corruption == "rect":
            wrong[0][0]["rect"][1] += 1
        elif corruption == "page":
            wrong.reverse()
        else:
            wrong.pop()
        try:
            check_pdf(synthetic_pdf(wrong), scene, expected)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"oracle accepted {corruption} corruption")
    for corruption in ("float", "duplicate", "outside", "shifted"):
        wrong = copy.deepcopy(scene)
        link = wrong["pages"][0]["operations"][0]
        if corruption == "float":
            link["bounds"]["x"] = 800.0
        elif corruption == "duplicate":
            wrong["pages"][0]["operations"].append(copy.deepcopy(link))
        elif corruption == "outside":
            link["bounds"]["y"] = 16000
        else:
            link["bounds"]["x"] += 80
        try:
            scene_links(wrong, case)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"scene oracle accepted {corruption} corruption")
    repository = Path(__file__).resolve().parents[3]
    with tempfile.TemporaryDirectory(prefix="pliego-links-self-test-") as temporary:
        for fixture in CASES:
            root = Path(temporary) / fixture["name"]
            first, source = build_fixture(repository, root / "a", fixture)
            second, _ = build_fixture(repository, root / "b", fixture)
            require(first == second, "fixture input closure is not repeatable")
            request = json.loads(first)
            require(request["resources"] == {"network": "deny", "host_fonts": "deny"}, "open fixture resources")
            verify_artifact(source, request["input"]["manifest"])
            for entry in json.loads((source / "input-manifest.json").read_bytes())["entries"]:
                verify_artifact(source / "input", entry)
    print(f"API 2 links self-test: ok (pypdf {pypdf.__version__})")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--proof-directory", type=Path)
    parser.add_argument("--source-commit")
    parser.add_argument("--pypdf-version", help="Require the pinned CI parser version")
    args = parser.parse_args()
    require(
        not args.pypdf_version or args.pypdf_version == pypdf.__version__, "pypdf version differs from required pin"
    )
    if args.self_test:
        self_test()
        return
    if args.binary is None or args.proof_directory is None:
        parser.error("--binary and --proof-directory are required")
    binary, output = args.binary.resolve(strict=True), args.proof_directory.resolve()
    output.mkdir(parents=True, exist_ok=False)
    probe, raw = run_probe(binary)
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
        "schema": "pliego.api2-links-proof",
        "version": 1,
        "engine": engine,
        "pypdf_version": pypdf.__version__,
        "pdf_tolerance_pt": TOLERANCE_PT,
        "cases": [],
    }
    for case in CASES:
        row = {"case": case, "passed": False}
        try:
            row.update(
                run_case(binary, Path(__file__).resolve().parents[3], output / case["name"], case, probe), passed=True
            )
        except (
            AssertionError,
            OSError,
            subprocess.SubprocessError,
            ValueError,
            KeyError,
            TypeError,
            PyPdfError,
        ) as error:
            row["failure"] = str(error)
        report["cases"].append(row)
        (output / "report.json").write_bytes(canonical_json(report))
        print(f"{case['name']}: {row.get('outcome', row.get('failure'))}")
    require(all(row["passed"] for row in report["cases"]), f"link gate failed; retained evidence: {output}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError, PyPdfError) as error:
        print(f"API 2 link check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
