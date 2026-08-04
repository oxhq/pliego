#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import copy
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
from typing import Any

from PIL import Image

CHART_JS_VERSION = "4.5.1"
CHART_JS_UMD_SHA256 = "ecc3cd1eeb8c34d2178e3f59fd63ec5a3d84358c11730af0b9958dc886d7652a"
CANVAS_SIZE = (678, 250)
PAGE_SIZE = (760, 840)
REPORT_FONT = (
    Path(__file__).resolve().parents[3] / "components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
)
REPORT_FONT_SHA256 = "7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954"
REGULAR_BEFORE = "PLIEGO-R1"
SYNTHETIC_BOLD = "PLIEGO-B"
REGULAR_AFTER = "PLIEGO-R2"
TRANSLUCENT_BOLD = "PLIEGO-B50"
FIDELITY_PROBE_SEQUENCE = (
    REGULAR_BEFORE,
    SYNTHETIC_BOLD,
    REGULAR_AFTER,
    TRANSLUCENT_BOLD,
)
TRANSLUCENT_COLOR = (15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.5)


def fail(message: str, code: int = 1) -> None:
    print(f"Chart.js report check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def canonical_text(operation: dict[str, Any]) -> str:
    return " ".join(str(operation.get("text", "")).split())


def fidelity_probe_slices(texts: list[dict[str, Any]]) -> dict[str, list[dict[str, Any]]]:
    probes = {}
    cursor = 0
    for marker in FIDELITY_PROBE_SEQUENCE:
        matches = []
        for start in range(cursor, len(texts)):
            combined = ""
            for end in range(start, len(texts)):
                fragment = canonical_text(texts[end])
                if not fragment:
                    break
                combined += fragment
                if combined == marker:
                    matches.append((start, end + 1))
                    break
                if not marker.startswith(combined):
                    break
        require(len(matches) == 1, f"font fidelity probe {marker!r} is absent or duplicated: {matches!r}")
        start, cursor = matches[0]
        probe = texts[start:cursor]
        for field in ("font", "font_size", "color"):
            require(
                all(operation.get(field) == probe[0].get(field) for operation in probe[1:]),
                f"font fidelity probe {marker!r} changes {field} inside its text slice",
            )
        probes[marker] = probe
    return probes


def verify_font_fidelity(texts: list[dict[str, Any]], instances: object) -> None:
    require(isinstance(instances, list) and len(instances) == 2, repr(instances))
    require(all(isinstance(instance, dict) for instance in instances), repr(instances))
    flags_by_id: dict[str, bool] = {}
    identities = set()
    for instance in instances:
        instance_id = instance.get("id")
        synthetic_bold = instance.get("synthetic_bold")
        require(isinstance(instance_id, str) and instance_id.startswith("sha256:"), repr(instance))
        require(isinstance(synthetic_bold, bool), repr(instance))
        require(instance_id not in flags_by_id, f"duplicate font instance identity: {instance_id}")
        flags_by_id[instance_id] = synthetic_bold
        identities.add(
            (
                instance.get("resource"),
                instance.get("face_index"),
                repr(instance.get("variations")),
            )
        )
    require(set(flags_by_id.values()) == {False, True}, repr(instances))
    require(len(identities) == 1, repr(instances))
    require(
        {operation.get("font") for operation in texts} == set(flags_by_id),
        "regular and synthetic-bold text were not both retained",
    )

    probes = fidelity_probe_slices(texts)
    regular_id = probes[REGULAR_BEFORE][0].get("font")
    require(flags_by_id.get(regular_id) is False, "regular-before text is not bound to the regular instance")
    require(
        probes[SYNTHETIC_BOLD][0].get("font") != regular_id
        and flags_by_id.get(probes[SYNTHETIC_BOLD][0].get("font")) is True,
        "bold probe text is not bound to the synthetic-bold instance",
    )
    require(
        probes[REGULAR_AFTER][0].get("font") == regular_id,
        "regular-after text did not return to the regular font instance",
    )
    require(
        probes[TRANSLUCENT_BOLD][0].get("font") == probes[SYNTHETIC_BOLD][0].get("font"),
        "translucent bold text is not bound to the synthetic-bold instance",
    )

    color = probes[TRANSLUCENT_BOLD][0].get("color")
    require(isinstance(color, dict), "translucent text has no resolved color")
    channels = []
    for channel in ("r", "g", "b", "a"):
        value = color.get(channel)
        require(
            isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(float(value)),
            f"translucent text {channel} is not finite",
        )
        channels.append(float(value))
    require(
        all(math.isclose(actual, expected, abs_tol=1e-6) for actual, expected in zip(channels, TRANSLUCENT_COLOR)),
        f"translucent text color differs: {tuple(channels)!r}",
    )


def read_json(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"missing artifact: {path}")
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"artifact is not an object: {path}")
    return value


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    value = json.loads(lines[-1])
    require(isinstance(value, dict), "terminal output is not an object")
    return value


def artifact(summary: dict[str, Any], key: str) -> Path:
    value = summary.get(key)
    require(isinstance(value, str), f"summary has no {key}")
    path = Path(value)
    require(path.is_file(), f"missing {key}: {path}")
    return path


def require_chromatic_pixels(source: Image.Image, description: str) -> tuple[int, int]:
    pixels = list(source.convert("RGBA").getdata())
    visible = 0
    chromatic_pixels = 0
    chromatic: set[tuple[int, int, int]] = set()
    for red, green, blue, alpha in pixels:
        if alpha == 0:
            continue
        visible += 1
        if alpha >= 64 and max(red, green, blue) - min(red, green, blue) >= 48:
            chromatic_pixels += 1
            chromatic.add((red >> 5, green >> 5, blue >> 5))
    require(visible > 1_000, f"{description} is effectively empty: {visible} visible pixels")
    require(chromatic_pixels > 1_000, f"{description} retained too few colored pixels: {chromatic_pixels}")
    require(len(chromatic) >= 2, f"{description} is monochrome: {sorted(chromatic)!r}")
    return visible, len(chromatic)


def require_non_monochrome_chart(path: Path) -> tuple[int, int]:
    with Image.open(path) as source:
        require(source.format == "PNG", "Canvas resource is not a PNG")
        require(source.size == CANVAS_SIZE, f"Canvas PNG dimensions differ: {source.size!r}")
        return require_chromatic_pixels(source, "Canvas chart")


def require_pdf_chart(pdf: Path, bounds: object, output: Path) -> None:
    pdftoppm = shutil.which("pdftoppm")
    require(pdftoppm is not None, "pdftoppm is required to verify the Chart.js PDF")
    require(isinstance(bounds, dict), f"Canvas image has no bounds: {bounds!r}")
    try:
        x = float(bounds["x"])
        y = float(bounds["y"])
        width = float(bounds["width"])
        height = float(bounds["height"])
    except (KeyError, TypeError, ValueError) as error:
        fail(f"Canvas image bounds are invalid: {bounds!r}: {error}")
    require(
        all(math.isfinite(value) for value in (x, y, width, height)) and width > 0 and height > 0,
        f"Canvas image bounds are invalid: {bounds!r}",
    )

    prefix = output / "document-page"
    result = subprocess.run(
        [
            pdftoppm,
            "-png",
            "-r",
            "96",
            "-f",
            "1",
            "-l",
            "1",
            "-singlefile",
            str(pdf),
            str(prefix),
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    require(result.returncode == 0, f"pdftoppm failed: {result.stderr[-2000:]}")
    raster = prefix.with_suffix(".png")
    require(raster.is_file(), f"pdftoppm did not produce {raster}")
    with Image.open(raster) as page:
        require(page.size == PAGE_SIZE, f"PDF raster dimensions differ: {page.size!r}")
        crop_box = (
            math.floor(x),
            math.floor(y),
            math.ceil(x + width),
            math.ceil(y + height),
        )
        require(
            crop_box[0] >= 0
            and crop_box[1] >= 0
            and crop_box[2] <= page.width
            and crop_box[3] <= page.height,
            f"Canvas image escapes the PDF page: {crop_box!r}",
        )
        chart = page.crop(crop_box)
        chart.save(output / "document-chart.png")
        require_chromatic_pixels(chart, "PDF chart")


def install_fixture(fixture: Path, destination: Path) -> Path:
    npm = shutil.which("npm")
    require(npm is not None, "npm is required to install the locked Chart.js fixture")
    shutil.copytree(fixture, destination)
    require(REPORT_FONT.is_file(), f"missing report font: {REPORT_FONT}")
    require(hashlib.sha256(REPORT_FONT.read_bytes()).hexdigest() == REPORT_FONT_SHA256, "report font digest differs")
    shutil.copy2(REPORT_FONT, destination / "ReportSans.ttf")
    environment = os.environ.copy()
    environment.update(
        {
            "CI": "true",
            "NPM_CONFIG_AUDIT": "false",
            "NPM_CONFIG_FUND": "false",
            "NPM_CONFIG_IGNORE_SCRIPTS": "true",
            "NPM_CONFIG_UPDATE_NOTIFIER": "false",
        }
    )
    result = subprocess.run(
        [npm, "ci", "--ignore-scripts", "--no-audit", "--no-fund"],
        cwd=destination,
        env=environment,
        capture_output=True,
        text=True,
        timeout=180,
        check=False,
    )
    if result.returncode != 0:
        fail(result.stderr[-4000:] or result.stdout[-4000:])
    asset = destination / "node_modules/chart.js/dist/chart.umd.js"
    require(asset.is_file(), f"locked install did not produce {asset}")
    digest = hashlib.sha256(asset.read_bytes()).hexdigest()
    require(digest == CHART_JS_UMD_SHA256, f"Chart.js UMD digest differs: {digest}")
    installed = read_json(destination / "node_modules/chart.js/package.json")
    require(installed.get("version") == CHART_JS_VERSION, repr(installed.get("version")))
    return destination / "index.html"


def run(binary: Path, fixture: Path, output: Path) -> tuple[int, int]:
    artifacts = output / "artifacts"
    document = output / "document.pdf"
    environment = os.environ.copy()
    environment.update({"TMPDIR": str(output), "TMP": str(output), "TEMP": str(output)})
    result = subprocess.run(
        [
            str(binary),
            "render",
            fixture.name,
            "--output",
            str(document),
            "--artifacts",
            str(artifacts),
            "--page-size",
            "760x840",
            "--page-margins",
            "28,28,28,28",
        ],
        cwd=fixture.parent,
        env=environment,
        capture_output=True,
        text=True,
        timeout=90,
        check=False,
    )
    if result.returncode != 0:
        fail(result.stderr[-4000:] or result.stdout[-4000:])
    summary = final_json(result)
    require(summary.get("status") == "rendered", repr(summary))

    readiness = summary.get("readiness")
    require(isinstance(readiness, dict), f"missing readiness payload: {readiness!r}")
    require(readiness.get("fixture") == "chartjs-report", repr(readiness))
    require(readiness.get("chartVersion") == CHART_JS_VERSION, repr(readiness))
    require(
        (readiness.get("canvasWidth"), readiness.get("canvasHeight")) == CANVAS_SIZE,
        repr(readiness),
    )
    require(readiness.get("datasetCount") == 2 and readiness.get("dataPointCount") == 6, repr(readiness))
    require(readiness.get("readbackBytes") == CANVAS_SIZE[0] * CANVAS_SIZE[1] * 4, repr(readiness))
    require(isinstance(readiness.get("paintedPixels"), int) and readiness["paintedPixels"] > 1_000, repr(readiness))
    require(isinstance(readiness.get("chromaticBuckets"), int) and readiness["chromaticBuckets"] >= 2, repr(readiness))

    scene_summary = summary.get("scene")
    require(isinstance(scene_summary, dict), "summary has no scene")
    require(scene_summary.get("capture_status") == "complete", repr(scene_summary))
    require(scene_summary.get("unsupported_event_count") == 0, repr(scene_summary))
    require(scene_summary.get("text_mapping_gap_count") == 0, repr(scene_summary))

    report = read_json(artifact(summary, "scene_report"))
    capture = report.get("capture")
    require(isinstance(capture, dict) and capture.get("status") == "complete", repr(capture))
    require(capture.get("unsupported_events") == [], repr(capture))
    require(capture.get("text_mapping_gaps") == [], repr(capture))
    canvases = capture.get("canvases")
    require(isinstance(canvases, list) and len(canvases) == 1, repr(canvases))
    diagnostics = canvases[0].get("diagnostics", {})
    require(diagnostics.get("rasterized_area_px") == CANVAS_SIZE[0] * CANVAS_SIZE[1], repr(diagnostics))
    fallbacks = diagnostics.get("fallbacks")
    require(isinstance(fallbacks, list) and len(fallbacks) == 1, repr(fallbacks))
    require(fallbacks[0].get("reason") == "pixel_readback", repr(fallbacks[0]))
    preview = report.get("preview")
    require(isinstance(preview, dict) and preview.get("status") == "rendered", repr(preview))
    require(preview.get("unsupported") == [], repr(preview))

    scene = read_json(artifact(summary, "scene_artifact"))
    pages = scene.get("pages")
    require(isinstance(pages, list) and len(pages) == 1, repr(pages))
    operations = pages[0].get("operations")
    require(isinstance(operations, list), "scene page has no operations")
    texts = [operation for operation in operations if operation.get("type") == "text"]
    paths = [operation for operation in operations if operation.get("type") == "path"]
    borders = [
        operation
        for operation in paths
        if operation.get("meta", {}).get("semantics", {}).get("label") == "border"
    ]
    images = [operation for operation in operations if operation.get("type") == "image"]
    require(len(texts) >= 12, f"styled report retained too little text: {len(texts)}")
    retained_text = " ".join(
        " ".join(str(operation.get("text", "")) for operation in texts).split()
    )
    for marker in ["Northstar Operations", "Recognized revenue", "Account contribution"]:
        require(marker in retained_text, f"retained report text is missing {marker!r}")

    fonts = read_json(artifact(summary, "fonts_artifact"))
    verify_font_fidelity(texts, fonts.get("font_instances"))
    require(len(paths) >= 8, f"styled report retained too few vector paths: {len(paths)}")
    require(len(borders) >= 12, f"styled report silently dropped ordinary borders: {len(borders)}")
    accents = [
        operation
        for operation in borders
        if math.isclose(float(operation.get("bounds", {}).get("height", 0)), 3.0)
        and float(operation.get("bounds", {}).get("width", 0)) > 200
    ]
    require(len(accents) == 3, f"KPI accent borders differ: {accents!r}")
    require(len(images) == 1, f"expected one Canvas image operation: {images!r}")
    resource = images[0].get("resource")
    require(isinstance(resource, str) and resource.startswith("sha256:"), repr(images[0]))
    resource_path = artifacts / "resources" / resource.removeprefix("sha256:")
    require(resource_path.is_file() and resource_path.stat().st_size > 0, f"missing Canvas image: {resource_path}")
    png = resource_path.read_bytes()
    require(hashlib.sha256(png).hexdigest() == resource.removeprefix("sha256:"), "Canvas resource hash differs")
    visible, chromatic = require_non_monochrome_chart(resource_path)

    pdf_path = artifact(summary, "document_pdf")
    pdf = pdf_path.read_bytes()
    require(len(pdf) > 500 and pdf.startswith(b"%PDF-"), "document output is not a nonempty PDF")
    require(summary.get("document_pdf_status") == "rendered", repr(summary))
    require(report.get("document_pdf", {}).get("status") == "rendered", repr(report.get("document_pdf")))
    require_pdf_chart(pdf_path, images[0].get("bounds"), output)
    return visible, chromatic


def self_test(fixture: Path) -> None:
    package = read_json(fixture / "package.json")
    lock = read_json(fixture / "package-lock.json")
    require(package.get("dependencies") == {"chart.js": CHART_JS_VERSION}, repr(package))
    require(lock.get("lockfileVersion") == 3, repr(lock.get("lockfileVersion")))
    packages = lock.get("packages")
    require(isinstance(packages, dict), "package lock has no packages")
    require(packages.get("node_modules/chart.js", {}).get("version") == CHART_JS_VERSION, repr(packages))
    require(packages.get("node_modules/@kurkle/color", {}).get("version") == "0.3.4", repr(packages))
    require(
        packages.get("node_modules/chart.js", {}).get("integrity", "").startswith("sha512-"),
        "Chart.js package lock has no integrity",
    )
    require(not (fixture / "node_modules").exists(), "fixture must not commit node_modules")
    require(REPORT_FONT.is_file(), f"missing report font: {REPORT_FONT}")
    require(hashlib.sha256(REPORT_FONT.read_bytes()).hexdigest() == REPORT_FONT_SHA256, "report font digest differs")

    html = (fixture / "index.html").read_text(encoding="utf-8")
    defer = html.index("window.pliego?.defer()")
    asset = html.index("node_modules/chart.js/dist/chart.umd.js")
    require(defer < asset, "Pliego defer must run before Chart.js loads")
    for marker in [
        "Northstar Operations",
        "Recognized revenue",
        "Account contribution",
        "ReportSans.ttf",
        "document.fonts.ready",
        "responsive: false",
        "animation: false",
        "events: []",
        "getImageData(0, 0, canvas.width, canvas.height)",
        "window.pliego?.ready(",
        *FIDELITY_PROBE_SEQUENCE,
        "rgba(15, 23, 42, 0.5)",
    ]:
        require(marker in html, f"fixture is missing {marker!r}")
    for unsupported_style in ["border-radius", "box-shadow", "transform:", "filter:", "opacity:"]:
        require(unsupported_style not in html, f"fixture uses {unsupported_style!r}")

    regular_id = "sha256:regular"
    bold_id = "sha256:bold"
    opaque_color = {"r": 0.4, "g": 0.45, "b": 0.55, "a": 1.0}
    instances = [
        {
            "id": regular_id,
            "resource": "sha256:font",
            "face_index": 0,
            "variations": [],
            "synthetic_bold": False,
        },
        {
            "id": bold_id,
            "resource": "sha256:font",
            "face_index": 0,
            "variations": [],
            "synthetic_bold": True,
        },
    ]
    probe_texts = [
        {
            "type": "text",
            "text": REGULAR_BEFORE,
            "font": regular_id,
            "font_size": 9.0,
            "color": opaque_color,
        },
        {
            "type": "text",
            "text": SYNTHETIC_BOLD,
            "font": bold_id,
            "font_size": 9.0,
            "color": opaque_color,
        },
        {
            "type": "text",
            "text": REGULAR_AFTER,
            "font": regular_id,
            "font_size": 9.0,
            "color": opaque_color,
        },
        {
            "type": "text",
            "text": TRANSLUCENT_BOLD,
            "font": bold_id,
            "font_size": 9.0,
            "color": dict(zip(("r", "g", "b", "a"), TRANSLUCENT_COLOR)),
        },
    ]
    verify_font_fidelity(probe_texts, instances)

    split_probe_texts = []
    for operation in probe_texts:
        prefix = copy.deepcopy(operation)
        prefix["text"] = "PLIEGO-"
        suffix = copy.deepcopy(operation)
        suffix["text"] = canonical_text(operation).removeprefix("PLIEGO-")
        split_probe_texts.extend([prefix, suffix])
    verify_font_fidelity(split_probe_texts, instances)

    def require_fidelity_rejection(broken: list[dict[str, Any]], description: str) -> None:
        with redirect_stderr(StringIO()):
            try:
                verify_font_fidelity(broken, instances)
            except SystemExit:
                return
        fail(f"self-test accepted {description}")

    wrong_binding = copy.deepcopy(probe_texts)
    wrong_binding[1]["font"] = regular_id
    require_fidelity_rejection(wrong_binding, "bold text bound to the regular instance")
    wrong_order = copy.deepcopy(probe_texts)
    wrong_order[1], wrong_order[2] = wrong_order[2], wrong_order[1]
    require_fidelity_rejection(wrong_order, "regular-bold-regular state order drift")
    opaque = copy.deepcopy(probe_texts)
    opaque[3]["color"]["a"] = 1.0
    require_fidelity_rejection(opaque, "opaque text in the translucent probe")
    mixed_font_split = copy.deepcopy(split_probe_texts)
    mixed_font_split[1]["font"] = bold_id
    require_fidelity_rejection(mixed_font_split, "split probe with mixed font instances")
    mixed_style_split = copy.deepcopy(split_probe_texts)
    mixed_style_split[-1]["font_size"] = 10.0
    require_fidelity_rejection(mixed_style_split, "split probe with mixed text styles")

    licenses = (fixture / "THIRD_PARTY_LICENSES.md").read_text(encoding="utf-8")
    require(
        all(name in licenses for name in ["Chart.js 4.5.1", "@kurkle/color 0.3.4", "DejaVu Sans 2.37"]),
        "license metadata differs",
    )


def main() -> int:
    fixture = Path(__file__).resolve().parent / "fixtures/chartjs-report"
    if sys.argv[1:] == ["--self-test"]:
        self_test(fixture)
        print("Chart.js report checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory> | --self-test", 2)
    binary = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2]).resolve()
    require(binary.is_file(), f"missing Pliego binary: {binary}")
    require(not output.exists(), f"output already exists: {output}")
    output.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="pliego-chartjs-report-") as temporary:
        installed = install_fixture(fixture, Path(temporary) / "fixture")
        visible, chromatic = run(binary, installed, output)
    print(f"Chart.js report check: ok ({visible} visible pixels; {chromatic} chromatic buckets; artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
