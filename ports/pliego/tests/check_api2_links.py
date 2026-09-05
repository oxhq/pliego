#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Packaged API 2 link proof: closed inputs, exact Scene v2 bounds, PDF URI annotations.

This is an annotation/geometry oracle, not a general visual or accessible-PDF
qualification. Negative cases retain their actual typed failure, or explicitly
report an unsafe/internal URL ignored as a nonlink; neither counts as link support.
PDF coordinates permit 0.02pt writer rounding. No PDF metadata determinism claim.
Plain solid backgrounds require authored scene geometry and color as well as
the link annotation proof. Rounded, fixed, clipped and image-layer backgrounds
remain excluded. This does not replace visual document qualification.
Ordinary one-sided/same-color solid borders additionally require exact authored
Au rectangle decomposition; this is not general CSS border support.
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
import time
from collections import Counter
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pypdf
from pypdf import PdfReader, PdfWriter
from pypdf.errors import PyPdfError
from pypdf.generic import ArrayObject, DictionaryObject, FloatObject, NameObject, TextStringObject

from check_api2_contract import (
    PROBE_TIMEOUT_SECONDS,
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
        "name": "solid-inline-background",
        "body": f'<a href="{HTTPS}">DEDUP</a>',
        "css": "a{background-color:#eee}",
        "links": {HTTPS: 1},
        "text": ["DEDUP"],
        "exact_rects": [[HTTPS, [2880, 3060, 3600, 720]]],
        "backgrounds": [[[[2880, 3060, 3600, 720], [238, 238, 238, 255]]]],
    },
    {
        "name": "solid-descendant-background",
        "body": f'<a class="box" href="{HTTPS}"><span>BOX</span></a>',
        "css": ".box{display:block;width:120px;height:40px;background-color:#eee}",
        "links": {HTTPS: 1},
        "text": ["BOX"],
        "exact_rects": [[HTTPS, [2880, 2880, 7200, 2400]]],
        "backgrounds": [[[[2880, 2880, 7200, 2400], [238, 238, 238, 255]]]],
    },
    *(
        {
            "name": f"solid-{owner}-background-{pages}-pages",
            "body": "".join(f'<p class="next">PAGE{page}</p>' for page in range(1, pages + 1)),
            "css": f"{owner}{{background-color:{color}}}p+p{{break-before:page}}",
            "links": {},
            "text": [f"PAGE{page}" for page in range(1, pages + 1)],
            "pages": pages,
            "exact_rects": [],
            # Request-owned page area, not the old scrolling-overflow union:
            # 48px margins on exact 47622 x 67351 Au A4 pages. Page five also
            # catches accumulated f32 page-origin drift in the capture path.
            "backgrounds": [[[[2880, 2880, 41862, 61591], rgba]] for _ in range(pages)],
        }
        for owner, color, rgba in (("body", "#fff", [255, 255, 255, 255]), ("html", "#369", [51, 102, 153, 255]))
        for pages in (1, 5)
    ),
    *(
        {
            "name": f"ordinary-border-bottom-{thickness}px",
            "body": '<div class="border"></div><p>AFTER</p>',
            "css": "body{padding:0.35px 0 0 0.35px}.border{width:100.35px;height:20.35px;"
            f"border-bottom:{thickness}px solid #ccc}}",
            "links": {},
            "text": ["AFTER"],
            "exact_rects": [],
            # 48px request margin + 0.35px padding => 2880 + 21 Au.
            # Content height 20.35px => 1221 Au; width 100.35px => 6021 Au.
            # These odd Au values are deliberately not binary-exact CSS pixels.
            "solid_paths": [[[[2901, 4122, 6021, thickness * 60], [204, 204, 204, 255]]]],
        }
        for thickness in (1, 2)
    ),
    {
        "name": "ordinary-border-four-sides",
        "body": '<div class="border"></div><p>AFTER</p>',
        "css": "body{padding:0.35px 0 0 0.35px}.border{width:100.35px;height:20.35px;"
        "border-style:solid;border-color:#ccc;border-width:1px 2px 3px 4px}",
        "links": {},
        "text": ["AFTER"],
        "exact_rects": [],
        # Outer box: [2901,2901,6381,1461] Au. Horizontal strips own corners;
        # vertical strips occupy only the 1221 Au content-height interior.
        "solid_paths": [
            [
                [[2901, 2901, 6381, 60], [204, 204, 204, 255]],
                [[2901, 4182, 6381, 180], [204, 204, 204, 255]],
                [[2901, 2961, 240, 1221], [204, 204, 204, 255]],
                [[9162, 2961, 120, 1221], [204, 204, 204, 255]],
            ]
        ],
    },
    *(
        {
            "name": f"ordinary-border-unsupported-{name}",
            "body": '<div class="border"></div><p>AFTER</p>',
            "css": ".border{width:100.35px;height:20.35px;border:1px solid #ccc;" + css + "}",
            "exclude": f"ordinary-border-{name}",
            "failure": ["artifact", "SCENE_ENCODING_FAILED"],
            "failure_message_contains": "unsupported paint kinds: box",
        }
        for name, css in (("mixed-color", "border-bottom-color:#333"), ("rounded", "border-radius:4px"))
    ),
    {
        "name": "ordinary-border-unsupported-transparent-corners",
        "body": '<div class="border"></div><p>AFTER</p>',
        # The 4x4px border box has zero content area. Its opaque right border
        # paints a triangular corner even though the other three 2px sides are
        # transparent. Nonoverlapping rectangular strips cannot represent it.
        "css": ".border{box-sizing:border-box;width:4px;height:4px;"
        "border:2px solid transparent;border-right-color:#d00}",
        "exclude": "ordinary-border-transparent-corners",
        "failure": ["artifact", "SCENE_ENCODING_FAILED"],
        "failure_message_contains": "unsupported paint kinds: box",
    },
    *(
        {
            "name": f"unsupported-background-{name}",
            "body": f'<a class="box" href="{HTTPS}">EXCLUDED</a>',
            "css": ".box{display:block;width:120px;height:40px;background-color:#eee;" + css + "}",
            "exclude": f"background-{name}",
            "failure": ["capture", "SCENE_CAPTURE_FAILED"]
            if name == "fixed"
            else ["artifact", "SCENE_ENCODING_FAILED"],
        }
        for name, css in (
            ("rounded", "border-radius:4px"),
            ("fixed", "background-attachment:fixed"),
            ("padding-clip", "background-clip:padding-box;padding:4px"),
            ("gradient", "background-image:linear-gradient(#eee,#ddd)"),
        )
    ),
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


def check_excluded_failure(
    case: dict[str, Any], kind: str, codes: list[str], messages: list[str] | None = None
) -> None:
    require(bool(case.get("exclude")), f"supported link failed: kind={kind}, codes={codes}")
    # These pairs are observed in the retained v0.3.3 baseline. In particular,
    # unsafe/internal failures can precede URL validation at missing authority;
    # the code pair alone is not proof that the URL-policy branch executed.
    expected_kind, expected_code = case["failure"]
    require(
        kind == expected_kind and codes == [expected_code], f"excluded link failure changed: kind={kind}, codes={codes}"
    )
    if "failure_message_contains" in case:
        require(
            any(case["failure_message_contains"] in message for message in messages or []),
            "excluded ordinary border did not retain its unsupported-paint cause",
        )


def build_fixture(
    repository: Path, root: Path, case: dict[str, Any], *, host_wall_ms: int | None = None
) -> tuple[bytes, Path]:
    require(
        host_wall_ms is None or (type(host_wall_ms) is int and host_wall_ms in (10000, 60000)),
        "unknown fixture diagnostic budget",
    )
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
    # Qualification preserves the API golden default; historical diagnostics
    # explicitly select their original 10s budget instead.
    if host_wall_ms is not None:
        request["settlement"]["limits"]["host_wall_ms"] = host_wall_ms
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
        paint_key = "solid_paths" if "solid_paths" in case else "backgrounds"
        if paint_key in case:
            paints = []
            for operation in page["operations"]:
                if operation["type"] != "path":
                    continue
                bounds, fill = operation["bounds"], operation["fill"]
                rect = [bounds[key] for key in ("x", "y", "width", "height")]
                rgba = [fill[key] for key in ("r", "g", "b", "a")]
                require(all(type(value) is int for value in rect + rgba), "solid paint authority is not integer")
                if paint_key == "solid_paths":
                    x, y, w, h = rect
                    require(
                        x >= 0 and y >= 0 and w > 0 and h > 0 and x + w <= width and y + h <= height,
                        "ordinary border is empty, clipped or outside its page",
                    )
                    require(
                        operation.get("data") == f"M {x} {y} L {x + w} {y} L {x + w} {y + h} L {x} {y + h} Z"
                        and operation.get("fill_rule") == "non-zero",
                        "ordinary border path is not its exact closed rectangle",
                    )
                paints.append([rect, rgba])
            require(paints == case[paint_key][index], "solid paint geometry/color/order changed")
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


def process_timeout_seconds(value: str) -> int:
    try:
        seconds = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("process timeout must be an integer") from error
    if not 1 <= seconds <= PROBE_TIMEOUT_SECONDS:
        raise argparse.ArgumentTypeError(f"process timeout must be between 1 and {PROBE_TIMEOUT_SECONDS} seconds")
    return seconds


def run_case(
    binary: Path,
    repository: Path,
    root: Path,
    case: dict[str, Any],
    probe: dict[str, Any],
    process_timeout: int = 65,
) -> dict[str, Any]:
    payload, source = build_fixture(repository, root, case)
    (root / "request.json").write_bytes(payload)
    job = root / "job"
    create_private_job_root(job)
    shutil.copy2(source / "input-manifest.json", job / "input-manifest.json")
    shutil.copytree(source / "input", job / "input")
    started = time.monotonic()
    try:
        completed = subprocess.run(
            [str(binary), "render-api2"],
            cwd=job,
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=process_timeout,
        )
    except subprocess.TimeoutExpired as error:
        elapsed = time.monotonic() - started
        (root / "stdout.json").write_bytes(error.stdout or b"")
        (root / "stderr.txt").write_bytes(error.stderr or b"")
        (root / "process.json").write_bytes(
            canonical_json(
                {
                    "outcome": "host-timeout",
                    "limit_seconds": process_timeout,
                    "wall_seconds": elapsed,
                }
            )
        )
        raise
    elapsed = time.monotonic() - started
    (root / "stdout.json").write_bytes(completed.stdout)
    (root / "stderr.txt").write_bytes(completed.stderr)
    (root / "process.json").write_bytes(
        canonical_json(
            {
                "exit_code": completed.returncode,
                "limit_seconds": process_timeout,
                "wall_seconds": elapsed,
            }
        )
    )
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
        diagnostic_records = [json.loads(path.read_bytes()) for path in diagnostics if path.suffix == ".json"]
        codes = [record.get("code") for record in diagnostic_records]
        codes = [code for code in codes if isinstance(code, str) and code]
        messages = [record["message"] for record in diagnostic_records if isinstance(record.get("message"), str)]
        require(isinstance(kind, str) and bool(kind) and bool(codes), "failure has no typed retained cause")
        check_excluded_failure(case, kind, codes, messages)
        return {"outcome": "rejected-as-excluded", "error_kind": kind, "codes": codes, "messages": messages}
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
    for valid in ("1", "30", "65", str(PROBE_TIMEOUT_SECONDS)):
        require(process_timeout_seconds(valid) == int(valid), "valid process timeout was changed")
    for invalid in ("0", "-1", str(PROBE_TIMEOUT_SECONDS + 1), "1.5", "nan", "invalid"):
        try:
            process_timeout_seconds(invalid)
        except argparse.ArgumentTypeError:
            pass
        else:
            raise AssertionError(f"process timeout accepted {invalid!r}")
    with tempfile.TemporaryDirectory(prefix="pliego-process-timeout-self-test-") as temporary:
        binary = Path(sys.executable)
        output = Path(temporary) / "case"
        real_run = subprocess.run

        def timeout_run(command: list[str], **kwargs: Any) -> subprocess.CompletedProcess:
            if command == [str(binary), "render-api2"]:
                raise subprocess.TimeoutExpired(
                    command,
                    kwargs["timeout"],
                    output=b"partial-output",
                    stderr=b"partial-error",
                )
            return real_run(command, **kwargs)

        with patch.object(subprocess, "run", side_effect=timeout_run):
            try:
                run_case(binary, Path(__file__).resolve().parents[3], output, CASES[0], {}, 180)
            except subprocess.TimeoutExpired:
                pass
            else:
                raise AssertionError("process timeout did not propagate")
        require((output / "stdout.json").read_bytes() == b"partial-output", "timeout stdout was lost")
        require((output / "stderr.txt").read_bytes() == b"partial-error", "timeout stderr was lost")
        process = json.loads((output / "process.json").read_bytes())
        require(process["outcome"] == "host-timeout" and process["limit_seconds"] == 180, "timeout identity changed")
        require(process["wall_seconds"] >= 0, "missing process wall time")
    for excluded in (case for case in CASES if case.get("exclude")):
        kind, code = excluded["failure"]
        messages = [excluded.get("failure_message_contains", "")]
        check_excluded_failure(excluded, kind, [code], messages)
        for wrong_kind, wrong_codes in (("input", [code]), (kind, ["WRONG_CODE"]), (kind, []), (kind, [code, code])):
            try:
                check_excluded_failure(excluded, wrong_kind, wrong_codes)
            except AssertionError:
                pass
            else:
                raise AssertionError("exclusion oracle accepted a changed failure pair")
        if "failure_message_contains" in excluded:
            for wrong_messages in (None, [], ["page 1 operation 0 has no exact fixed-point authority"]):
                try:
                    check_excluded_failure(excluded, kind, [code], wrong_messages)
                except AssertionError:
                    pass
                else:
                    raise AssertionError("ordinary-border exclusion accepted an unrelated encoding failure")
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
    painted_scene = copy.deepcopy(scene)
    painted_scene["pages"][0]["operations"].append(
        {
            "type": "path",
            "bounds": {"x": 800, "y": 1600, "width": 2400, "height": 800},
            "fill": {"r": 238, "g": 238, "b": 238, "a": 255},
        }
    )
    painted_case = {**case, "backgrounds": [[[[800, 1600, 2400, 800], [238, 238, 238, 255]]], []]}
    scene_links(painted_scene, painted_case)
    for corruption in ("position", "color", "missing", "duplicate", "float"):
        wrong = copy.deepcopy(painted_scene)
        operations = wrong["pages"][0]["operations"]
        if corruption == "position":
            operations[-1]["bounds"]["y"] += 60
        elif corruption == "color":
            operations[-1]["fill"]["r"] = 0
        elif corruption == "missing":
            operations.pop()
        elif corruption == "duplicate":
            operations.append(copy.deepcopy(operations[-1]))
        else:
            operations[-1]["bounds"]["x"] = 800.0
        try:
            scene_links(wrong, painted_case)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"background oracle accepted {corruption} corruption")
    border_case = next(case for case in CASES if case["name"] == "ordinary-border-four-sides")

    def border_operation(rect: list[int], rgba: list[int]) -> dict[str, Any]:
        x, y, w, h = rect
        return {
            "type": "path",
            "bounds": dict(zip(("x", "y", "width", "height"), rect)),
            "fill": dict(zip(("r", "g", "b", "a"), rgba)),
            "data": f"M {x} {y} L {x + w} {y} L {x + w} {y + h} L {x} {y + h} Z",
            "fill_rule": "non-zero",
        }

    border_scene = {
        "schema": "pliego.document-scene",
        "version": 2,
        "app_units_per_css_px": 60,
        "pages": [
            {
                "number": 1,
                "size_app_units": {"width": 47622, "height": 67351},
                "operations": [border_operation(rect, rgba) for rect, rgba in border_case["solid_paths"][0]]
                + [{"type": "text", "text": "AFTER"}],
            }
        ],
    }
    scene_links(border_scene, border_case)
    for corruption in (
        "shifted",
        "width",
        "overlap",
        "missing",
        "duplicate",
        "reordered",
        "color",
        "path",
        "fill-rule",
        "float",
    ):
        wrong = copy.deepcopy(border_scene)
        operations = wrong["pages"][0]["operations"]
        if corruption in ("shifted", "width", "overlap"):
            if corruption == "shifted":
                for operation in operations[:-1]:
                    operation["bounds"]["x"] += 60
            elif corruption == "width":
                operations[0]["bounds"]["width"] -= 1
            else:
                operations[2]["bounds"]["y"] -= 60
                operations[2]["bounds"]["height"] += 60
            # Keep each corrupt path self-consistent with its bounds; only the
            # independently authored geometry should reject these mutations.
            for index, operation in enumerate(operations[:-1]):
                rect = [operation["bounds"][key] for key in ("x", "y", "width", "height")]
                rgba = [operation["fill"][key] for key in ("r", "g", "b", "a")]
                operations[index] = border_operation(rect, rgba)
        elif corruption == "missing":
            operations.pop(0)
        elif corruption == "duplicate":
            operations.insert(0, copy.deepcopy(operations[0]))
        elif corruption == "reordered":
            operations[0], operations[1] = operations[1], operations[0]
        elif corruption == "color":
            operations[0]["fill"]["r"] = 0
        elif corruption == "path":
            operations[0]["data"] = operations[0]["data"].removesuffix(" Z")
        elif corruption == "fill-rule":
            operations[0]["fill_rule"] = "non_zero"
        else:
            operations[0]["bounds"]["x"] = float(operations[0]["bounds"]["x"])
        try:
            scene_links(wrong, border_case)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"ordinary-border oracle accepted {corruption} corruption")
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
    golden_limits = json.loads((repository / "contracts/api2/goldens/accepted/render-request.a4.json").read_bytes())[
        "settlement"
    ]["limits"]
    require(golden_limits["host_wall_ms"] == 60000, "API default budget changed")
    with tempfile.TemporaryDirectory(prefix="pliego-links-self-test-") as temporary:
        for invalid in (True, 10000.0, 0, -1, 30000, 65000):
            try:
                build_fixture(repository, Path(temporary) / "invalid", CASES[0], host_wall_ms=invalid)
            except AssertionError:
                pass
            else:
                raise AssertionError("invalid diagnostic budget accepted")
        for fixture in CASES:
            root = Path(temporary) / fixture["name"]
            first, source = build_fixture(repository, root / "a", fixture)
            second, _ = build_fixture(repository, root / "b", fixture)
            require(first == second, "fixture input closure is not repeatable")
            request = json.loads(first)
            require(request["settlement"]["limits"] == golden_limits, "qualification changed API default limits")
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
    parser.add_argument(
        "--process-timeout-seconds",
        type=process_timeout_seconds,
        default=65,
        help="Caller process bound including startup/self-hashing; default 65, direct debug CI uses 180.",
    )
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
    probe_process = {"binary_bytes": binary.stat().st_size, "limit_seconds": PROBE_TIMEOUT_SECONDS, "outcome": "failed"}
    probe_started = time.monotonic()
    try:
        probe, raw = run_probe(binary)
        probe_process["outcome"] = "success"
    finally:
        probe_process["wall_seconds"] = time.monotonic() - probe_started
        (output / "probe-process.json").write_bytes(canonical_json(probe_process))
        print(f"Contract probe process: {json.dumps(probe_process, sort_keys=True)}", flush=True)
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
        "probe_process": probe_process,
        "process_timeout_seconds": args.process_timeout_seconds,
        "pypdf_version": pypdf.__version__,
        "pdf_tolerance_pt": TOLERANCE_PT,
        "cases": [],
    }
    for case in CASES:
        row = {"case": case, "passed": False}
        try:
            row.update(
                run_case(
                    binary,
                    Path(__file__).resolve().parents[3],
                    output / case["name"],
                    case,
                    probe,
                    args.process_timeout_seconds,
                ),
                passed=True,
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
        process_record = output / case["name"] / "process.json"
        if process_record.is_file():
            row["process"] = json.loads(process_record.read_bytes())
        report["cases"].append(row)
        (output / "report.json").write_bytes(canonical_json(report))
        print(f"{case['name']}: {row.get('outcome', row.get('failure'))}", flush=True)
    require(all(row["passed"] for row in report["cases"]), f"link gate failed; retained evidence: {output}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, subprocess.SubprocessError, PyPdfError) as error:
        print(f"API 2 link check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
