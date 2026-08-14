#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import binascii
import hashlib
import json
import math
import os
import struct
import subprocess
import sys
import tempfile
from pathlib import Path


def fail(message: str, code: int = 1) -> None:
    print(f"layout capture smoke test: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def number(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def integer(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def positive(value: object) -> bool:
    return number(value) and value > 0


def positive_rects(fragments: list[object], kind: str) -> list[object]:
    rects = [
        fragment.get("rect") for fragment in fragments if isinstance(fragment, dict) and fragment.get("kind") == kind
    ]
    return [
        rect
        for rect in rects
        if isinstance(rect, dict) and positive(rect.get("width")) and positive(rect.get("height"))
    ]


def output_path(summary: dict[str, object], key: str, root: Path) -> Path:
    value = summary.get(key)
    require(isinstance(value, str) and bool(value), f"final output has no {key!r} path")
    path = Path(value)
    return path if path.is_absolute() else root / path


def main() -> int:
    modes = {
        None: "session",
        "defer": "readiness-double",
        "text": "text-capture",
        "link": "link-capture",
        "effects": "effects-capture",
    }
    mode = sys.argv[2] if len(sys.argv) == 3 else None
    if len(sys.argv) not in (2, 3) or mode not in modes:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> [defer|text|link|effects]", 2)

    root = Path(__file__).resolve().parents[2]
    binary = Path(sys.argv[1]).expanduser().resolve()
    fixture_name = modes[mode]
    fixture = Path(f"tests/pliego/fixtures/{fixture_name}/index.html")
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require((root / fixture).is_file(), f"{fixture_name} fixture does not exist: {root / fixture}")

    with tempfile.TemporaryDirectory(prefix="pliego-layout-capture-") as temp_dir:
        environment = os.environ.copy()
        environment.update({"TMPDIR": temp_dir, "TMP": temp_dir, "TEMP": temp_dir})
        try:
            if mode == "effects":
                overlap_artifacts = Path(temp_dir) / "overlap-artifacts"
                overlap_output = overlap_artifacts / "document.pdf"
                overlap = subprocess.run(
                    [
                        str(binary),
                        "render",
                        str(fixture),
                        "--output",
                        str(overlap_output),
                        "--artifacts",
                        str(overlap_artifacts),
                    ],
                    cwd=root,
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=60,
                    check=False,
                )
                require(overlap.returncode == 1, f"overlapping output exited {overlap.returncode}")
                require(
                    overlap.stdout.endswith("\n") and "\r" not in overlap.stdout and overlap.stdout.count("\n") == 1,
                    f"overlapping output stdout framing drifted: {overlap.stdout!r}",
                )
                overlap_summary = json.loads(
                    overlap.stdout,
                    parse_constant=lambda value: fail(f"overlapping output contains {value}"),
                )
                require(isinstance(overlap_summary, dict), "overlapping output terminal is not an object")
                require(
                    overlap.stdout == json.dumps(overlap_summary, ensure_ascii=False, separators=(",", ":")) + "\n",
                    "overlapping output terminal is not canonical JSON",
                )
                require(
                    set(overlap_summary) == {"artifacts", "document_pdf", "engine", "error", "render_id", "status"},
                    repr(overlap_summary),
                )
                require(
                    overlap_summary.get("status") == "failed"
                    and overlap_summary.get("engine") == "pliego"
                    and overlap_summary.get("artifacts") == str(overlap_artifacts)
                    and overlap_summary.get("document_pdf") == str(overlap_output)
                    and overlap_summary.get("error")
                    == {
                        "code": "OUTPUT_ARTIFACTS_OVERLAP",
                        "message": "requested output must be outside the artifact directory",
                    },
                    repr(overlap_summary),
                )
                overlap_render_id = overlap_summary.get("render_id")
                require(
                    isinstance(overlap_render_id, str)
                    and overlap_render_id.startswith("sha256:")
                    and len(overlap_render_id) == 71
                    and all(character in "0123456789abcdef" for character in overlap_render_id[7:]),
                    f"overlapping output render ID drifted: {overlap_render_id!r}",
                )
                require(
                    overlap.stderr
                    == "pliego: OUTPUT_ARTIFACTS_OVERLAP: requested output must be outside the artifact directory\n",
                    f"overlapping output stderr drifted: {overlap.stderr!r}",
                )
                require(not overlap_artifacts.exists(), "overlapping output created public artifacts")
                require(not overlap_output.exists(), "overlapping output path was populated")
                require(
                    not list(Path(temp_dir).glob(".pliego-runtime-*")),
                    "overlapping output retained a private runtime container",
                )
                require(not list(Path(temp_dir).iterdir()), "overlapping output mutated its publication parent")

                rejected_artifacts = Path(temp_dir) / "rejected-artifacts"
                rejected_output = Path(temp_dir) / "rejected.pdf"
                rejected = subprocess.run(
                    [
                        str(binary),
                        "render",
                        str(fixture),
                        "--output",
                        str(rejected_output),
                        "--artifacts",
                        str(rejected_artifacts),
                    ],
                    cwd=root,
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=60,
                    check=False,
                )
                rejected_lines = [line.strip() for line in rejected.stdout.splitlines() if line.strip()]
                require(rejected.returncode != 0 and bool(rejected_lines), "partial scene was published")
                rejected_summary = json.loads(rejected_lines[-1])
                require(
                    rejected_summary.get("status") == "failed"
                    and rejected_summary.get("error", {}).get("code") == "SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS",
                    repr(rejected_summary),
                )
                require(not rejected_output.exists(), "partial scene PDF was published")
                rejected_environment = json.loads((rejected_artifacts / "environment.json").read_text(encoding="utf-8"))
                require(
                    rejected_environment.get("document_pdf", {}).get("status") == "failed"
                    and rejected_environment.get("document_pdf", {}).get("error", {}).get("code")
                    == "SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS",
                    repr(rejected_environment.get("document_pdf")),
                )
                rejected_report = json.loads((rejected_artifacts / "scene-report.json").read_text(encoding="utf-8"))
                require(
                    rejected_report.get("document_pdf", {}).get("status") == "failed"
                    and rejected_report.get("document_pdf", {}).get("error", {}).get("code")
                    == "SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS",
                    repr(rejected_report.get("document_pdf")),
                )
            command = [str(binary)]
            if mode == "effects":
                command.append("--allow-partial-scene")
            command.append(str(fixture))
            result = subprocess.run(
                command,
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
        except OSError as error:
            fail(f"could not execute {binary}: {error}")
        except subprocess.TimeoutExpired:
            fail(f"Pliego did not finish the {fixture_name} fixture within 60 seconds")

        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "no process output"
            fail(f"Pliego exited with {result.returncode}: {detail[-2000:]}")

        lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
        require(bool(lines), "Pliego produced no stdout JSON")
        try:
            summary = json.loads(lines[-1])
        except json.JSONDecodeError as error:
            fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
        require(isinstance(summary, dict), "final stdout JSON is not an object")

        artifacts = output_path(summary, "artifacts", root)
        readiness_path = artifacts / "readiness.json"
        require(readiness_path.is_file(), f"readiness artifact does not exist: {readiness_path}")
        try:
            readiness = json.loads(readiness_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            fail(f"could not load readiness artifact {readiness_path}: {error}")
        require(isinstance(readiness, dict), "readiness artifact is not a JSON object")
        if mode is None:
            require(summary.get("readiness") is None, repr(summary.get("readiness")))
            require(
                readiness.get("status") == "ready"
                and readiness.get("payload") is None
                and readiness.get("font_status") == "loaded",
                repr(readiness),
            )
        elif mode == "defer":
            expected_readiness = {"fixture": "readiness-double", "winner": "ready"}
            require(summary.get("readiness") == expected_readiness, repr(summary.get("readiness")))
            require(
                readiness.get("status") == "ready"
                and readiness.get("payload") == expected_readiness
                and readiness.get("font_status") == "loaded",
                repr(readiness),
            )

        layout_path = output_path(summary, "layout_debug", root)
        require(layout_path.is_file(), f"layout debug artifact does not exist: {layout_path}")
        try:
            snapshot = json.loads(layout_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            fail(f"could not load layout debug artifact {layout_path}: {error}")
        require(isinstance(snapshot, dict), "layout debug artifact is not a JSON object")

        boxes = snapshot.get("boxes")
        require(isinstance(boxes, list), "layout debug artifact has no boxes array")

        fragments = snapshot.get("fragments")
        require(isinstance(fragments, list) and bool(fragments), "fragment array is empty or missing")
        paint_events = snapshot.get("paint_events")
        require(
            isinstance(paint_events, list) and bool(paint_events),
            "paint event array is empty or missing",
        )
        require(
            all(isinstance(event, dict) for event in paint_events),
            "paint event array contains a non-object entry",
        )
        sequences = [event.get("sequence") for event in paint_events]
        require(
            sequences == list(range(len(paint_events))),
            f"paint event sequences are not dense and ordered: {sequences!r}",
        )
        if mode == "effects":
            kinds = {event.get("kind") for event in paint_events}
            require("box" in kinds, f"ancestor opacity was not reported: {kinds!r}")
            require("text-effects" in kinds, f"text effects were not reported: {kinds!r}")
            require("content-geometry" in kinds, f"overflow clipping was not reported: {kinds!r}")
            image_ids = {
                fragment.get("paint_fragment_id")
                for fragment in fragments
                if isinstance(fragment, dict) and fragment.get("kind") == "image"
            }
            clipped_ids = {
                event.get("fragment_id") for event in paint_events if event.get("kind") == "content-geometry"
            }
            require(
                bool(image_ids & clipped_ids),
                f"clipped image was not rejected: images={image_ids!r}, clips={clipped_ids!r}",
            )
        elif mode == "text":
            font_resources = snapshot.get("font_resources")
            require(
                isinstance(font_resources, list) and bool(font_resources),
                "text fixture produced no captured font resources",
            )
            require(
                all(isinstance(resource, dict) for resource in font_resources),
                "font resource array contains a non-object entry",
            )
            resource_ids = [resource.get("resource") for resource in font_resources]
            require(
                all(isinstance(resource_id, str) for resource_id in resource_ids),
                "a font resource has no resource ID",
            )
            require(
                resource_ids == sorted(resource_ids) and len(resource_ids) == len(set(resource_ids)),
                f"font resources are not sorted and unique: {resource_ids!r}",
            )
            for resource_index, resource in enumerate(font_resources):
                encoded = resource.get("bytes_base64")
                require(
                    isinstance(encoded, str),
                    f"font resource {resource_index} has no base64 bytes",
                )
                try:
                    font_bytes = base64.b64decode(encoded, validate=True)
                except (binascii.Error, ValueError) as error:
                    fail(f"font resource {resource_index} has invalid base64 bytes: {error}")
                expected_resource = f"sha256:{hashlib.sha256(font_bytes).hexdigest()}"
                require(
                    resource.get("resource") == expected_resource,
                    f"font resource {resource_index} digest does not match its bytes",
                )

            font_instances = snapshot.get("font_instances")
            require(
                isinstance(font_instances, list) and bool(font_instances),
                "text fixture produced no captured font instances",
            )
            require(
                all(isinstance(instance, dict) for instance in font_instances),
                "font instance array contains a non-object entry",
            )
            instance_ids = [instance.get("id") for instance in font_instances]
            require(
                all(isinstance(instance_id, str) for instance_id in instance_ids),
                "a font instance has no instance ID",
            )
            require(
                instance_ids == sorted(instance_ids) and len(instance_ids) == len(set(instance_ids)),
                f"font instances are not sorted and unique: {instance_ids!r}",
            )
            for instance_index, instance in enumerate(font_instances):
                resource_id = instance.get("resource")
                require(
                    resource_ids.count(resource_id) == 1,
                    f"font instance {instance_index} does not join exactly one resource",
                )
                variations = instance.get("variations")
                require(
                    isinstance(variations, list),
                    f"font instance {instance_index} has no variations array",
                )
                variation_keys = []
                for variation_index, variation in enumerate(variations):
                    require(
                        isinstance(variation, dict),
                        f"font instance {instance_index} variation {variation_index} is not an object",
                    )
                    tag = variation.get("tag")
                    value = variation.get("value")
                    require(
                        integer(tag) and 0 <= tag <= 0xFFFFFFFF,
                        f"font instance {instance_index} variation {variation_index} has invalid tag",
                    )
                    require(
                        number(value) and math.isfinite(value),
                        f"font instance {instance_index} variation {variation_index} is not finite",
                    )
                    require(
                        value != 0 or math.copysign(1.0, value) > 0,
                        f"font instance {instance_index} variation {variation_index} has negative zero",
                    )
                    try:
                        value_bits = struct.unpack(">I", struct.pack(">f", value))[0]
                    except (OverflowError, struct.error) as error:
                        fail(
                            f"font instance {instance_index} variation {variation_index} "
                            f"is not representable as f32: {error}"
                        )
                    variation_keys.append((tag, value_bits))
                require(
                    variation_keys == sorted(variation_keys),
                    f"font instance {instance_index} variations are not canonically sorted",
                )

            text_runs = [
                fragment.get("text_run")
                for fragment in fragments
                if isinstance(fragment, dict) and isinstance(fragment.get("text_run"), dict)
            ]
            require(bool(text_runs), "text fixture produced no captured text runs")
            texts = [run.get("text") for run in text_runs]
            require(all(isinstance(text, str) for text in texts), "a text run has no text string")
            captured_text = "".join(texts)
            require(captured_text == "CAFÉ OFFICE", f"captured text was {captured_text!r}")
            for run_index, run in enumerate(text_runs):
                font_instance_id = run.get("font_instance_id")
                require(
                    instance_ids.count(font_instance_id) == 1,
                    f"text run {run_index} does not join exactly one font instance",
                )
                require(
                    bool(run.get("font_identifier")),
                    f"text run {run_index} has an empty font_identifier",
                )
                require(run.get("font_size") == 20, f"text run {run_index} font_size is not 20")
                glyphs = run.get("glyphs")
                require(
                    isinstance(glyphs, list) and bool(glyphs),
                    f"text run {run_index} has no glyphs",
                )
                for glyph_index, glyph in enumerate(glyphs):
                    require(
                        isinstance(glyph, dict),
                        f"text run {run_index} glyph {glyph_index} is not an object",
                    )
                    for key in ("id", "x", "y", "advance"):
                        require(
                            number(glyph.get(key)),
                            f"text run {run_index} glyph {glyph_index} has nonnumeric {key}",
                        )
        elif mode == "link":
            links = snapshot.get("links")
            require(isinstance(links, list), "link fixture produced no links array")
            require(len(links) == 1, f"expected one rendered hyperlink; got {links!r}")
            link = links[0]
            require(isinstance(link, dict), "captured link is not an object")
            expected_target = (
                root / fixture.parent / "final" / "report.html"
            ).resolve().as_uri() + "?edition=1#summary"
            require(
                link.get("url") == expected_target,
                f"expected resolved link URL {expected_target!r}; got {link.get('url')!r}",
            )
            tag_id = link.get("tag_id")
            require(
                isinstance(tag_id, int) and not isinstance(tag_id, bool),
                f"captured link has invalid tag_id {tag_id!r}",
            )
            link_rects = [
                fragment.get("rect")
                for fragment in fragments
                if isinstance(fragment, dict)
                and fragment.get("tag_id") == tag_id
                and isinstance(fragment.get("rect"), dict)
            ]
            require(
                len(link_rects) >= 2,
                f"expected one wrapping link to join at least two fragments; got {link_rects!r}",
            )
        elif mode is None:
            box_kinds = [box.get("kind") for box in boxes if isinstance(box, dict)]
            require(
                "independent" in box_kinds,
                f"expected an independent box; got kinds {box_kinds}",
            )
            for kind in ("text", "image"):
                require(
                    bool(positive_rects(fragments, kind)),
                    f"expected a {kind} fragment with positive width and height",
                )
                fragment_ids = {
                    fragment.get("paint_fragment_id")
                    for fragment in fragments
                    if isinstance(fragment, dict)
                    and fragment.get("kind") == kind
                    and integer(fragment.get("paint_fragment_id"))
                }
                event_fragment_ids = {
                    event.get("fragment_id")
                    for event in paint_events
                    if event.get("kind") == kind and integer(event.get("fragment_id"))
                }
                require(
                    bool(fragment_ids & event_fragment_ids),
                    f"expected a {kind} paint event joined to its captured fragment; "
                    f"fragment ids={fragment_ids!r}, event ids={event_fragment_ids!r}",
                )
            expected_image_url = (root / fixture.parent / "mark.svg").resolve().as_uri()
            image_urls = [
                fragment.get("image_url")
                for fragment in fragments
                if isinstance(fragment, dict) and fragment.get("kind") == "image"
            ]
            require(
                expected_image_url in image_urls,
                f"expected laid-out image URL {expected_image_url!r}; got {image_urls!r}",
            )
            resource_log = artifacts / "resources.jsonl"
            require(resource_log.is_file(), f"resource log does not exist: {resource_log}")
            try:
                resources = [
                    json.loads(line) for line in resource_log.read_text(encoding="utf-8").splitlines() if line.strip()
                ]
            except (OSError, json.JSONDecodeError) as error:
                fail(f"could not load resource log {resource_log}: {error}")
            image_resources = [
                resource
                for resource in resources
                if isinstance(resource, dict)
                and resource.get("status") == "loaded"
                and expected_image_url in resource.get("urls", [])
            ]
            image_bytes = (root / fixture.parent / "mark.svg").read_bytes()
            expected_digest = hashlib.sha256(image_bytes).hexdigest()
            require(
                bool(image_resources),
                f"expected a loaded image resource; got {image_resources!r}",
            )
            require(
                {resource.get("sha256") for resource in image_resources} == {expected_digest},
                f"captured image requests did not converge on one digest: {image_resources!r}",
            )
            image_resource = image_resources[0]
            require(
                image_resource.get("resource") == f"sha256:{expected_digest}",
                f"captured image has no content address: {image_resource!r}",
            )
            content_type = image_resource.get("content_type")
            require(
                isinstance(content_type, str) and content_type.partition(";")[0].strip().lower() == "image/svg+xml",
                f"captured image content type is not image/svg+xml: {content_type!r}",
            )
            artifact = image_resource.get("artifact")
            require(isinstance(artifact, str), "captured image has no artifact path")
            resource_path = artifacts / artifact
            require(resource_path.is_file(), f"resource artifact does not exist: {resource_path}")
            require(
                resource_path.read_bytes() == image_bytes,
                "content-addressed image bytes differ from the source fixture",
            )

        for key in ("paint_content_width", "paint_content_height"):
            require(positive(snapshot.get(key)), f"{key} must be positive; got {snapshot.get(key)!r}")
        scroll_nodes = snapshot.get("paint_scroll_node_count")
        require(
            isinstance(scroll_nodes, int) and not isinstance(scroll_nodes, bool) and scroll_nodes >= 2,
            f"paint_scroll_node_count must be at least 2; got {scroll_nodes!r}",
        )
        for key in ("paintable", "contentful"):
            require(snapshot.get(key) is True, f"{key} must be true; got {snapshot.get(key)!r}")
        epoch = snapshot.get("paint_epoch")
        require(
            isinstance(epoch, int) and not isinstance(epoch, bool) and epoch >= 0,
            f"paint_epoch must be a nonnegative integer; got {epoch!r}",
        )
        require(
            isinstance(snapshot.get("first_reflow"), bool),
            f"first_reflow must be boolean; got {snapshot.get('first_reflow')!r}",
        )

        rendered_path = output_path(summary, "rendered_image", root)
        require(rendered_path.is_file(), f"rendered image does not exist: {rendered_path}")
        require(rendered_path.stat().st_size > 0, f"rendered image is empty: {rendered_path}")

    print("layout capture smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
