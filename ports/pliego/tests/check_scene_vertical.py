#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

CSS_PX_TO_PDF_PT = 72.0 / 96.0
INLINE_PNG_BASE64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
INLINE_PNG = base64.b64decode(INLINE_PNG_BASE64, validate=True)
SAFE_LINK = "https://example.com/pliego"
JS_MUTATION_TEXT = "JS MUTATION OK"
SOURCE_AHEM_SHA256 = "b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448"
SANITIZED_AHEM_SHA256 = "649a7613cfa59d415188415e1488eb40fc9953742338a793538380234a539869"
RunResult = tuple[bytes, str, bytes, bytes, dict[str, Any]]


def fail(message: str, code: int = 1) -> None:
    print(f"canonical scene check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def require_object(value: object, description: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{description} is not an object")
    return value


def require_list(value: object, description: str) -> list[Any]:
    require(isinstance(value, list), f"{description} is not an array")
    return value


def read_json(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"artifact does not exist: {path}")
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read JSON artifact {path}: {error}")
    return require_object(value, str(path))


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
    return require_object(value, "final stdout JSON")


def retain_process_output(result: subprocess.CompletedProcess[str], destination: Path) -> None:
    require(not destination.exists(), f"retained run already exists: {destination}")
    try:
        destination.mkdir(parents=True)
        (destination / "process.stdout.log").write_text(result.stdout, encoding="utf-8")
        (destination / "process.stderr.log").write_text(result.stderr, encoding="utf-8")
    except OSError as error:
        fail(f"cannot retain process output in {destination}: {error}")


def retain_outputs(temp_root: Path, destination: Path) -> None:
    artifacts = temp_root / "artifacts"
    document_pdf = temp_root / "document.pdf"
    try:
        if artifacts.is_dir():
            shutil.copytree(artifacts, destination / "artifacts")
        if document_pdf.is_file():
            shutil.copy2(document_pdf, destination / "document.pdf")
    except OSError as error:
        fail(f"cannot retain run outputs in {destination}: {error}")


def retain_summary(summary: dict[str, Any], destination: Path) -> None:
    source_value = summary.get("artifacts")
    require(isinstance(source_value, str) and bool(source_value), "summary has no artifacts directory")
    source = Path(source_value)
    require(source.is_dir(), f"artifacts directory does not exist: {source}")
    require((destination / "artifacts").is_dir(), f"artifacts were not retained in {destination}")
    require((destination / "document.pdf").is_file(), f"document.pdf was not retained in {destination}")
    try:
        retained_summary = dict(summary)
        retained_summary["retained_artifacts"] = "artifacts"
        retained_summary["retained_document_pdf"] = "document.pdf"
        (destination / "summary.json").write_text(
            json.dumps(retained_summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    except OSError as error:
        fail(f"cannot retain artifacts in {destination}: {error}")


def artifact_path(summary: dict[str, Any], key: str) -> Path:
    value = summary.get(key)
    require(isinstance(value, str) and bool(value), f"summary has no {key} path")
    path = Path(value)
    require(path.is_file(), f"{key} does not exist: {path}")
    return path


def verify_scene(summary: dict[str, Any]) -> tuple[bytes, str, set[str]]:
    scene_summary = require_object(summary.get("scene"), "scene summary")
    require(scene_summary.get("schema") == "pliego.document-scene", repr(scene_summary))
    require(scene_summary.get("version") == 1, repr(scene_summary))
    require(scene_summary.get("validation") == "valid", repr(scene_summary))
    require(scene_summary.get("preview_status") == "rendered", repr(scene_summary))
    require(
        scene_summary.get("capture_status") == "partial",
        repr(scene_summary),
    )
    require(
        scene_summary.get("capture_code") == "SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS",
        repr(scene_summary),
    )
    require(
        isinstance(scene_summary.get("unsupported_event_count"), int) and scene_summary["unsupported_event_count"] > 0,
        "text fixture did not expose its unsupported paint events",
    )
    require(
        scene_summary.get("text_mapping_gap_count") == 0,
        f"text fixture has unmapped glyphs: {scene_summary!r}",
    )

    scene_path = artifact_path(summary, "scene_artifact")
    scene_bytes = scene_path.read_bytes()
    require(not scene_bytes.endswith(b"\n"), "scene.json is not the exact compact encoding")
    expected_hash = f"sha256:{hashlib.sha256(scene_bytes).hexdigest()}"
    require(scene_summary.get("hash") == expected_hash, "scene hash does not match exact bytes")
    scene = require_object(json.loads(scene_bytes), "scene.json")
    compact_scene = json.dumps(scene, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    require(scene_bytes == compact_scene, "scene.json is not the canonical compact JSON encoding")
    require(scene.get("schema") == "pliego.document-scene", repr(scene))
    require(scene.get("version") == 1, repr(scene))
    pages = require_list(scene.get("pages"), "scene pages")
    require(len(pages) == 1, f"expected one scene page, found {len(pages)}")
    page = require_object(pages[0], "scene page")
    require(
        page.get("size") == {"width": 612.0, "height": 792.0},
        f"scene size did not come from paged-root geometry: {page.get('size')!r}",
    )
    operations = require_list(page.get("operations"), "scene operations")
    kinds = {require_object(operation, "scene operation").get("type") for operation in operations}
    expected_kinds = {"text", "path", "image", "link"}
    require(expected_kinds <= kinds, f"fixture omitted canonical operations: {kinds!r}")
    require(kinds <= expected_kinds, f"fixture emitted unexpected operations: {kinds!r}")
    fonts: set[str] = set()
    mapped_non_ascii = False
    image_count = 0
    safe_link_count = 0
    captured_text: list[str] = []
    for operation_index, value in enumerate(operations):
        operation = require_object(value, f"scene operation {operation_index}")
        if operation.get("type") == "image":
            image_count += 1
            resource = operation.get("resource")
            expected_resource = f"sha256:{hashlib.sha256(INLINE_PNG).hexdigest()}"
            require(resource == expected_resource, f"unexpected image resource: {operation!r}")
            require(
                artifact_path_from_address(summary, resource).read_bytes() == INLINE_PNG,
                "canonical image sidecar differs from the inline PNG",
            )
            continue
        if operation.get("type") == "link":
            safe_link_count += operation.get("target") == SAFE_LINK
            continue
        if operation.get("type") != "text":
            continue
        text = operation.get("text")
        font = operation.get("font")
        require(isinstance(text, str) and bool(text), f"invalid text operation: {operation!r}")
        require(isinstance(font, str) and bool(font), f"invalid text font: {operation!r}")
        captured_text.append(text)
        fonts.add(font)
        encoded = text.encode("utf-8")
        boundaries = {0}
        offset = 0
        for character in text:
            offset += len(character.encode("utf-8"))
            boundaries.add(offset)
        glyphs = require_list(operation.get("glyphs"), f"text operation {operation_index} glyphs")
        require(bool(glyphs), f"text operation {operation_index} has no glyphs")
        for glyph_index, glyph_value in enumerate(glyphs):
            glyph = require_object(glyph_value, f"text operation {operation_index} glyph {glyph_index}")
            source = require_object(
                glyph.get("text_range"),
                f"text operation {operation_index} glyph {glyph_index} source range",
            )
            start = source.get("start")
            end = source.get("end")
            require(
                isinstance(start, int)
                and not isinstance(start, bool)
                and isinstance(end, int)
                and not isinstance(end, bool),
                f"glyph source range is not integral: {source!r}",
            )
            require(
                0 <= start < end <= len(encoded),
                f"glyph source range is empty or out of bounds: {source!r}",
            )
            require(
                start in boundaries and end in boundaries,
                f"glyph source range splits a UTF-8 code point: {source!r}",
            )
            y = glyph.get("y")
            require(isinstance(y, (int, float)), f"glyph has no numeric y position: {glyph!r}")
            mapped_non_ascii |= any(byte >= 0x80 for byte in encoded[start:end])
    require(fonts, "text fixture has no referenced fonts")
    require(image_count == 1, f"expected one canonical image, found {image_count}")
    require(safe_link_count >= 1, f"safe link was not retained: {operations!r}")
    require(mapped_non_ascii, "UTF-8 fixture text was not mapped to any retained glyph")
    require(JS_MUTATION_TEXT in "".join(captured_text), "JS DOM mutation is absent from scene text")
    return scene_bytes, expected_hash, fonts


def verify_paged_root(summary: dict[str, Any]) -> None:
    environment = require_object(summary.get("environment"), "render environment")
    page_option = require_object(environment.get("page"), "page option")
    require(
        page_option.get("size_css_px") == {"width": 612.0, "height": 792.0},
        repr(page_option),
    )
    require(
        page_option.get("margins_css_px") == {"top": 72.0, "right": 54.0, "bottom": 36.0, "left": 18.0},
        repr(page_option),
    )
    layout = read_json(artifact_path(summary, "layout_debug"))
    sequence = require_object(layout.get("page_sequence"), "layout page sequence")
    pages = require_list(sequence.get("pages"), "layout pages")
    require(len(pages) == 1, f"expected one layout page, found {len(pages)}")
    page = require_object(pages[0], "layout page")
    require(page.get("index") == 0, repr(page))
    require(page.get("width") == 612.0 and page.get("height") == 792.0, repr(page))
    require(
        page.get("available_inline_size") == 540.0 and page.get("available_block_size") == 684.0,
        repr(page),
    )


def verify_fonts(
    summary: dict[str, Any], referenced_fonts: set[str], fixture_font: Path
) -> tuple[bytes, set[str]]:
    fonts_path = artifact_path(summary, "fonts_artifact")
    fonts_bytes = fonts_path.read_bytes()
    fonts = read_json(fonts_path)
    resources = require_list(fonts.get("font_resources"), "font resources")
    instances = require_list(fonts.get("font_instances"), "font instances")
    resources_by_id: dict[str, bytes] = {}
    for value in resources:
        resource = require_object(value, "font resource")
        address = resource.get("resource")
        encoded = resource.get("bytes_base64")
        require(isinstance(address, str), f"invalid font resource address: {address!r}")
        require(isinstance(encoded, str), f"invalid base64 for font resource {address}")
        try:
            decoded = base64.b64decode(encoded, validate=True)
        except ValueError as error:
            fail(f"font resource {address} is not strict base64: {error}")
        require(
            base64.b64encode(decoded).decode("ascii") == encoded,
            f"font resource {address} is not canonical padded base64",
        )
        actual = f"sha256:{hashlib.sha256(decoded).hexdigest()}"
        require(address == actual, f"font resource address mismatch: {address} != {actual}")
        resources_by_id[address] = decoded
        resource_path = artifact_path_from_address(summary, address)
        require(resource_path.read_bytes() == decoded, f"font bytes differ: {resource_path}")

    instances_by_id: dict[str, dict[str, Any]] = {}
    for value in instances:
        instance = require_object(value, "font instance")
        identifier = instance.get("id")
        resource = instance.get("resource")
        require(isinstance(identifier, str), f"invalid font instance ID: {identifier!r}")
        require(resource in resources_by_id, f"font instance {identifier} has no resource")
        require(isinstance(instance.get("face_index"), int), repr(instance))
        require_list(instance.get("variations"), f"font instance {identifier} variations")
        instances_by_id[identifier] = instance

    require(referenced_fonts <= instances_by_id.keys(), "scene references missing font instances")
    fixture_font_bytes = fixture_font.read_bytes()
    require(
        hashlib.sha256(fixture_font_bytes).hexdigest() == SOURCE_AHEM_SHA256,
        "bundled Ahem source changed without updating the canonical font oracle",
    )
    fixture_resource = f"sha256:{SANITIZED_AHEM_SHA256}"
    require(
        fixture_resource in resources_by_id,
        "sanitized bundled fixture font is absent from fonts.json",
    )
    fixture_font_ids = {
        identifier
        for identifier, instance in instances_by_id.items()
        if instance.get("resource") == fixture_resource and identifier in referenced_fonts
    }
    require(fixture_font_ids, "scene text does not reference the bundled fixture font")
    return fonts_bytes, fixture_font_ids


def artifact_path_from_address(summary: dict[str, Any], address: str) -> Path:
    require(address.startswith("sha256:"), f"invalid content address: {address}")
    artifacts = summary.get("artifacts")
    require(isinstance(artifacts, str) and bool(artifacts), "summary has no artifacts directory")
    path = Path(artifacts) / "resources" / address.removeprefix("sha256:")
    require(path.is_file(), f"content-addressed resource does not exist: {path}")
    return path


def operation_counts(operations: list[dict[str, Any]]) -> dict[str, int]:
    return {
        "text": sum(operation.get("type") == "text" for operation in operations),
        "vector": sum(operation.get("type") == "path" for operation in operations),
        "image": sum(operation.get("type") == "image" for operation in operations),
        "link": sum(operation.get("type") == "link" for operation in operations),
    }


def verify_report_and_preview(summary: dict[str, Any], scene_hash: str, scene_bytes: bytes) -> bytes:
    report = read_json(artifact_path(summary, "scene_report"))
    scene_summary = require_object(summary.get("scene"), "scene summary")
    report_scene = require_object(report.get("scene"), "report scene")
    require(report_scene.get("hash") == scene_summary.get("hash"), "report scene hash differs")
    require(report_scene.get("hash") == scene_hash, "report hash does not match compact bytes")
    capture = require_object(report.get("capture"), "capture report")
    require(capture.get("status") == scene_summary.get("capture_status"), repr(capture))
    require(
        capture.get("code") == "SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS",
        repr(capture),
    )
    unsupported_events = require_list(capture.get("unsupported_events"), "unsupported paint events")
    require(bool(unsupported_events), "partial capture has no unsupported paint events")
    unsupported_kinds = {require_object(event, "unsupported paint event").get("kind") for event in unsupported_events}
    require("box" in unsupported_kinds, f"fixture did not report box paint: {unsupported_kinds!r}")
    require(capture.get("text_mapping_gaps") == [], repr(capture))
    preview = require_object(report.get("preview"), "preview report")
    require(preview.get("status") == "rendered", repr(preview))
    require(preview.get("artifact") == "scene-preview.png", repr(preview))
    require(
        preview.get("unsupported") == [],
        f"preview silently left canonical operations unsupported: {preview!r}",
    )
    scene = require_object(json.loads(scene_bytes), "scene.json")
    scene_page = require_object(require_list(scene.get("pages"), "scene pages")[0], "scene page")
    scene_operations = [
        require_object(value, "scene operation")
        for value in require_list(scene_page.get("operations"), "scene operations")
    ]
    require(preview.get("page_size") == scene_page.get("size"), repr(preview))
    require(preview.get("operation_counts") == operation_counts(scene_operations), repr(preview))

    preview_path = artifact_path(summary, "scene_preview")
    preview_bytes = preview_path.read_bytes()
    require(
        preview_bytes.startswith(b"\x89PNG\r\n\x1a\n"),
        f"scene preview is not a PNG: {preview_path}",
    )
    require(len(preview_bytes) >= 24 and preview_bytes[12:16] == b"IHDR", "PNG has no IHDR")
    preview_size = (
        int.from_bytes(preview_bytes[16:20], "big"),
        int.from_bytes(preview_bytes[20:24], "big"),
    )
    require(preview_size == (612, 792), f"preview PNG dimensions are {preview_size!r}")
    require(
        preview_size == (scene_page["size"]["width"], scene_page["size"]["height"]),
        "preview PNG dimensions differ from canonical scene page size",
    )
    return preview_bytes


def verify_pdf_bundle(
    summary: dict[str, Any], scene_bytes: bytes, fixture_font_ids: set[str]
) -> dict[str, Any]:
    pdf_path = artifact_path(summary, "document_pdf")
    require(summary.get("document_pdf_status") == "rendered", repr(summary))
    pdf_bytes = pdf_path.read_bytes()
    require(pdf_bytes.startswith(b"%PDF-"), f"document PDF has no PDF header: {pdf_path}")
    require(b"/ToUnicode" in pdf_bytes, "document PDF has no ToUnicode map for selectable text")
    require(
        b"/FontFile2" in pdf_bytes or b"/FontFile3" in pdf_bytes,
        "document PDF has no embedded font program",
    )

    structure_path = artifact_path(summary, "pdf_structure")
    require(summary.get("pdf_structure_status") == "rendered", repr(summary))
    structure = read_json(structure_path)
    require(structure.get("schema") == "pliego.pdf-structure", repr(structure))
    require(structure.get("version") == 1, repr(structure))
    require(structure.get("backend") == "krilla", repr(structure))
    pdf_record = require_object(structure.get("pdf"), "PDF structure PDF record")
    require(pdf_record.get("artifact") == "document.pdf", repr(pdf_record))
    require(pdf_record.get("bytes") == len(pdf_bytes), repr(pdf_record))
    require(
        pdf_record.get("sha256") == f"sha256:{hashlib.sha256(pdf_bytes).hexdigest()}",
        "PDF structure hash does not match document.pdf",
    )

    scene = require_object(json.loads(scene_bytes), "scene.json")
    scene_pages = require_list(scene.get("pages"), "scene pages")
    structure_pages = require_list(structure.get("pages"), "PDF structure pages")
    require(structure.get("page_count") == len(scene_pages), repr(structure))
    require(len(structure_pages) == len(scene_pages), repr(structure_pages))
    for index, (scene_value, structure_value) in enumerate(zip(scene_pages, structure_pages)):
        scene_page = require_object(scene_value, f"scene page {index}")
        structure_page = require_object(structure_value, f"PDF structure page {index}")
        size = require_object(scene_page.get("size"), f"scene page {index} size")
        require(structure_page.get("index") == index, repr(structure_page))
        require(
            structure_page.get("scene_page_size_css_px") == size,
            repr(structure_page),
        )
        require(
            structure_page.get("media_box_pt")
            == [
                0.0,
                0.0,
                size.get("width") * CSS_PX_TO_PDF_PT,
                size.get("height") * CSS_PX_TO_PDF_PT,
            ],
            repr(structure_page),
        )
        operations = [
            require_object(value, f"scene page {index} operation")
            for value in require_list(scene_page.get("operations"), "scene operations")
        ]
        expected_text = "".join(operation["text"] for operation in operations if operation.get("type") == "text")
        expected_fonts = list(
            dict.fromkeys(operation["font"] for operation in operations if operation.get("type") == "text")
        )
        expected_counts = operation_counts(operations)
        require(expected_counts["image"] == 1, repr(expected_counts))
        require(expected_counts["link"] >= 1, repr(expected_counts))
        require(
            structure_page.get("expected_extracted_unicode") == expected_text,
            repr(structure_page),
        )
        require(structure_page.get("embedded_font_ids") == expected_fonts, repr(structure_page))
        require(
            fixture_font_ids <= set(structure_page.get("embedded_font_ids", [])),
            "bundled fixture font is not embedded in the PDF",
        )
        require(structure_page.get("operation_counts") == expected_counts, repr(structure_page))

    report = read_json(artifact_path(summary, "scene_report"))
    report_pdf = require_object(report.get("document_pdf"), "report document PDF")
    artifact_pdf = Path(summary["artifacts"]) / "document.pdf"
    require(artifact_pdf.read_bytes() == pdf_bytes, "artifact and published PDFs differ")
    require(report_pdf.get("artifact") == str(artifact_pdf), repr(report_pdf))
    require(report_pdf.get("status") == "rendered", repr(report_pdf))
    require(report_pdf.get("error") is None, repr(report_pdf))
    report_structure = require_object(report.get("pdf_structure"), "report PDF structure")
    require(report_structure.get("artifact") == str(structure_path), repr(report_structure))
    require(report_structure.get("status") == "rendered", repr(report_structure))
    require(report_structure.get("error") is None, repr(report_structure))

    environment = require_object(summary.get("environment"), "render environment")
    environment_pdf = require_object(environment.get("document_pdf"), "environment document PDF")
    require(environment_pdf.get("artifact") == str(pdf_path), repr(environment_pdf))
    require(environment_pdf.get("status") == "rendered", repr(environment_pdf))
    require(environment_pdf.get("error") is None, repr(environment_pdf))
    persisted_environment = read_json(artifact_path(summary, "environment_artifact"))
    require(
        persisted_environment.get("document_pdf") == environment_pdf,
        "terminal and persisted PDF environment states differ",
    )
    return {
        "schema": structure["schema"],
        "version": structure["version"],
        "backend": structure["backend"],
        "page_count": structure["page_count"],
        "pages": structure_pages,
    }


def verify_artifact_bundle(summary: dict[str, Any]) -> None:
    bundle_path = artifact_path(summary, "bundle")
    bundle = read_json(bundle_path)
    require(bundle.get("schema") == "pliego.bundle", repr(bundle))
    require(bundle.get("version") == 1, repr(bundle))
    require(bundle.get("render_id") == summary.get("render_id"), repr(bundle))

    artifact_root = Path(summary["artifacts"])
    entries = [require_object(value, "bundle entry") for value in require_list(bundle.get("entries"), "bundle entries")]
    paths = [entry.get("path") for entry in entries]
    require(all(isinstance(path, str) and bool(path) for path in paths), repr(paths))
    require(paths == sorted(paths), f"bundle entries are not sorted: {paths!r}")
    require(len(paths) == len(set(paths)), f"bundle entries are duplicated: {paths!r}")

    actual_paths = []
    for artifact in artifact_root.rglob("*"):
        if not artifact.is_file() or artifact == bundle_path:
            continue
        require(
            not (
                artifact.name.startswith(".")
                and ".pliego-" in artifact.name
                and artifact.name.endswith(".tmp")
            ),
            f"temporary publication file survived: {artifact}",
        )
        actual_paths.append(artifact.relative_to(artifact_root).as_posix())
    require(paths == sorted(actual_paths), "bundle does not cover the exact artifact file set")

    for entry, relative in zip(entries, paths):
        require("\\" not in relative and ".." not in relative.split("/"), repr(entry))
        contents = (artifact_root / relative).read_bytes()
        require(entry.get("bytes") == len(contents), repr(entry))
        require(
            entry.get("sha256") == f"sha256:{hashlib.sha256(contents).hexdigest()}",
            f"bundle hash does not match {relative}",
        )

    output = require_object(bundle.get("output"), "bundle output")
    output_path = Path(output.get("path", ""))
    require(output_path == artifact_path(summary, "document_pdf"), repr(output))
    output_bytes = output_path.read_bytes()
    require(output.get("bytes") == len(output_bytes), repr(output))
    require(
        output.get("sha256") == f"sha256:{hashlib.sha256(output_bytes).hexdigest()}",
        "bundle hash does not match the published PDF",
    )


def materialize_fixture(source: Path, font: Path, destination: Path) -> Path:
    marker = 'url("Ahem.ttf")'
    document = source.read_text(encoding="utf-8")
    require(document.count(marker) == 1, "canonical fixture has no unique font URL marker")
    encoded_font = base64.b64encode(font.read_bytes()).decode("ascii")
    materialized = destination / source.name
    materialized.write_text(
        document.replace(marker, f'url("data:font/ttf;base64,{encoded_font}")'),
        encoding="utf-8",
    )
    return materialized


def run_and_verify(
    binary: Path,
    fixture: Path,
    fixture_font: Path,
    temp_root: Path,
    retained_run: Path,
) -> RunResult:
    materialized_fixture = materialize_fixture(fixture, fixture_font, temp_root)
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(temp_root), "TMP": str(temp_root), "TEMP": str(temp_root)})
    result = subprocess.run(
        [
            str(binary),
            "render",
            materialized_fixture.name,
            "--allow-host-fonts",
            "--output",
            str(temp_root / "document.pdf"),
            "--artifacts",
            str(temp_root / "artifacts"),
            "--page-size",
            "612x792",
            "--page-margins",
            "72,54,36,18",
        ],
        cwd=temp_root,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    retain_process_output(result, retained_run)
    retain_outputs(temp_root, retained_run)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no process output"
        fail(f"Pliego exited with {result.returncode}: {detail[-4000:]}")
    summary = final_json(result)
    retain_summary(summary, retained_run)
    require(summary.get("status") == "rendered", repr(summary))
    readiness = require_object(summary.get("readiness"), "readiness payload")
    require(readiness.get("fixture") == "text-scene", repr(readiness))
    verify_paged_root(summary)
    scene_bytes, scene_hash, referenced_fonts = verify_scene(summary)
    fonts_bytes, fixture_font_ids = verify_fonts(summary, referenced_fonts, fixture_font)
    preview_bytes = verify_report_and_preview(summary, scene_hash, scene_bytes)
    pdf_contract = verify_pdf_bundle(summary, scene_bytes, fixture_font_ids)
    verify_artifact_bundle(summary)
    return scene_bytes, scene_hash, fonts_bytes, preview_bytes, pdf_contract


def comparison_report(first: RunResult, second: RunResult) -> dict[str, Any]:
    return {
        "schema": "pliego.canonical-scene-comparison",
        "version": 1,
        "runs": {"first": "first/artifacts", "second": "second/artifacts"},
        "scene_hashes": {"first": first[1], "second": second[1]},
        "comparisons": {
            "scene_bytes": first[0] == second[0],
            "scene_hash": first[1] == second[1],
            "font_bundle": first[2] == second[2],
            "preview_png": first[3] == second[3],
            "pdf_semantics": first[4] == second[4],
        },
        "pdf_byte_equality_checked": False,
    }


def self_test() -> None:
    first: RunResult = (b"scene", "sha256:same", b"fonts", b"preview", {"page_count": 1})
    report = comparison_report(first, first)
    require(all(report["comparisons"].values()), repr(report))
    require(report["pdf_byte_equality_checked"] is False, repr(report))
    changed: RunResult = (b"scene", "sha256:changed", b"fonts", b"preview", {"page_count": 1})
    mismatch = comparison_report(first, changed)
    require(mismatch["comparisons"]["scene_hash"] is False, repr(mismatch))


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        print("canonical scene checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)

    binary = Path(sys.argv[1]).expanduser().resolve()
    fixture = Path(__file__).resolve().parent / "fixtures/text-scene/index.html"
    fixture_font = fixture.with_name("Ahem.ttf")
    output = Path(sys.argv[2]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(fixture.is_file(), f"fixture does not exist: {fixture}")
    require(fixture_font.is_file(), f"fixture font does not exist: {fixture_font}")
    require(not (output / "first").exists(), f"retained run already exists: {output / 'first'}")
    require(not (output / "second").exists(), f"retained run already exists: {output / 'second'}")
    require(not (output / "comparison.json").exists(), f"comparison report already exists: {output}")
    try:
        output.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        fail(f"cannot create output directory {output}: {error}")

    with tempfile.TemporaryDirectory(prefix="pliego-canonical-scene-") as temp:
        root = Path(temp)
        first_root = root / "first"
        second_root = root / "second"
        first_root.mkdir()
        second_root.mkdir()
        first = run_and_verify(binary, fixture, fixture_font, first_root, output / "first")
        second = run_and_verify(binary, fixture, fixture_font, second_root, output / "second")
        report = comparison_report(first, second)
        comparison_path = output / "comparison.json"
        try:
            comparison_path.write_text(
                json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
        except OSError as error:
            fail(f"cannot write comparison report {comparison_path}: {error}")
        failed = [name for name, matches in report["comparisons"].items() if not matches]
        require(
            not failed,
            f"identical runs differ in {', '.join(failed)}; see {comparison_path}",
        )

    print(f"canonical scene check: ok ({first[1]}; artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
