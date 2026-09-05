# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Shared DejaVu program/closure gates; ledger defaults are not other fixtures' expectations."""

from __future__ import annotations

import hashlib
import io
import json
import math
import re
from functools import lru_cache
from pathlib import Path

from fontTools.ttLib import TTFont
from pypdf import PdfReader
from pypdf.generic import ByteStringObject, ContentStream, DictionaryObject, TextStringObject

from ledger_checks import closed_path, plain_path, require

USED_FACES = {"DejaVuSans.ttf", "DejaVuSans-Bold.ttf", "DejaVuSans-Oblique.ttf"}


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def font_from_bytes(raw: bytes) -> TTFont:
    font = TTFont(io.BytesIO(raw), lazy=False, recalcBBoxes=False, recalcTimestamp=False)
    require("glyf" in font and "hmtx" in font, "Ledger requires the original TrueType outline surface")
    return font


def glyph_fingerprints(font: TTFont) -> dict[str, str]:
    result = {}
    for name in font.getGlyphOrder():
        # OTS repairs stale glyf bounding boxes in the original DejaVu. Compare
        # actual decomposed contour coordinates, not a pen's xMin/LSB shift.
        coordinates, end_points, flags = font["glyf"][name].getCoordinates(font["glyf"])
        points = [[int(v) if float(v).is_integer() else v for v in point] for point in coordinates]
        value = [font["hmtx"].metrics[name], points, list(end_points), list(flags)]
        result[name] = digest(json.dumps(value, separators=(",", ":")).encode())
    return result


def semantic_font_identity(font: TTFont, glyphs: dict[str, str]) -> str:
    # Sanitization may rewrite sfnt metadata/padding. It may not change any
    # source glyph outline, horizontal metric, Unicode mapping or face style.
    value = {
        "units_per_em": font["head"].unitsPerEm,
        "glyph_order": font.getGlyphOrder(),
        "glyphs": glyphs,
        "cmap": font.getBestCmap(),
        "weight": font["OS/2"].usWeightClass,
        "selection": font["OS/2"].fsSelection,
    }
    return digest(json.dumps(value, sort_keys=True, separators=(",", ":")).encode())


def source_font(path: str, expected_sha256: str) -> tuple[dict, TTFont]:
    raw = plain_path(Path(path)).read_bytes()
    require(digest(raw) == expected_sha256, "Source font byte identity changed")
    return _parsed_source_font(raw, expected_sha256)


@lru_cache(maxsize=12)
def _parsed_source_font(raw: bytes, expected_sha256: str) -> tuple[dict, TTFont]:
    font = font_from_bytes(raw)
    glyphs = glyph_fingerprints(font)
    return {"sha256": expected_sha256, "glyphs": glyphs, "semantic": semantic_font_identity(font, glyphs)}, font


def match_subset_program(
    raw: bytes, sources: dict[str, tuple[dict, TTFont]], required_faces: set[str]
) -> tuple[str, TTFont]:
    """Match named TrueType subset glyph programs/metrics, not PDF text encoding.

    Krilla preserves original glyph names. A caller with a different glyph-name
    or CID mapping surface must prove that mapping separately, not relabel it.
    """
    subset = font_from_bytes(raw)
    glyphs = glyph_fingerprints(subset)
    matches = [
        name
        for name, (facts, original) in sources.items()
        if subset["head"].unitsPerEm == original["head"].unitsPerEm
        and all(facts["glyphs"].get(glyph) == value for glyph, value in glyphs.items())
    ]
    require(
        len(matches) == 1 and matches[0] in required_faces, "PDF subset outline/metric provenance is ambiguous or wrong"
    )
    return matches[0], subset


def checked_bundle(bundle_path: Path, scene_path: Path, pdf_path: Path) -> tuple[dict, dict[str, Path]]:
    root = plain_path(bundle_path.parent, directory=True)
    bundle_path = plain_path(bundle_path)
    scene_path = plain_path(scene_path)
    pdf_path = plain_path(pdf_path)
    bundle = json.loads(bundle_path.read_bytes())
    require(
        bundle["schema"] == "pliego.bundle-manifest" and bundle["version"] == 1,
        "Ledger qualification requirement failed: bundle['schema'] == 'pliego.bundle-manifest' and bundle['version'] == 1",
    )
    resources = {}
    names = set()
    for entry in bundle["entries"]:
        path = closed_path(root, entry["path"])
        name = entry["path"].casefold()
        require(name not in names, "Duplicate or case-aliased bundle path")
        names.add(name)
        raw = path.read_bytes()
        require(
            len(raw) == entry["bytes"] and "sha256:" + digest(raw) == entry["sha256"],
            "Ledger qualification requirement failed: len(raw) == entry['bytes'] and 'sha256:' + digest(raw) == entry['sha256']",
        )
        resources[entry["path"]] = path
    require(resources["scene.json"] == scene_path, "Scene is not the bundle's exact scene")
    require(
        digest(resources["document.pdf"].read_bytes()) == digest(pdf_path.read_bytes()), "PDF is not bound by bundle"
    )
    scene = json.loads(scene_path.read_bytes())
    require(
        scene["schema"] == "pliego.document-scene" and scene["version"] == 2,
        "Ledger qualification requirement failed: scene['schema'] == 'pliego.document-scene' and scene['version'] == 2",
    )
    return scene, resources


def unicode_map(font: dict) -> dict[int, str]:
    require(font["/Encoding"] == "/Identity-H", "Unexpected candidate font encoding")
    raw = font["/ToUnicode"].get_data().decode("ascii")
    require("beginbfrange" not in raw, "Unqualified candidate ToUnicode range encoding")
    result = {}
    for block in re.findall(r"beginbfchar(.*?)endbfchar", raw, re.S):
        for cid, text in re.findall(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>", block):
            key = int(cid, 16)
            require(key not in result, "Ledger qualification requirement failed: key not in result")
            result[key] = bytes.fromhex(text).decode("utf-16-be")
    require(result, "PDF font lacks usable Unicode mappings")
    return result


def painted_text_runs(operations: list, fonts: dict[str, dict]) -> tuple[list[tuple[str, str, str]], int]:
    """Observed Krilla text profile; never infer painted usage from resources.

    TJ positioning may split a scene run into many strings. Preserve the glyph
    sequence across those splits. The observed alias override is narrower:
    one inline /Span ActualText scopes exactly one show containing one CID.
    Multi-glyph clusters, nested marks and Form XObjects require a separate
    qualification, not an implicit page-font fallback.
    """
    result, stack = [], []
    font_name = None
    in_text = False
    actual_text = None
    actual_shows = 0
    overrides = 0
    graphics = {b"cm", b"rg", b"RG", b"m", b"l", b"h", b"f", b"f*", b"re", b"n", b"W", b"W*"}
    for operands, operator in operations:
        if operator == b"q":
            require(not in_text and actual_text is None, "Unqualified graphics save inside text")
            stack.append(font_name)
        elif operator == b"Q":
            require(not in_text and actual_text is None and stack, "Unbalanced text graphics restore")
            font_name = stack.pop()
        elif operator == b"BT":
            require(not in_text and actual_text is None, "Nested/unscoped text object")
            in_text = True
        elif operator == b"ET":
            require(in_text and actual_text is None, "Unbalanced text object/ActualText")
            in_text = False
        elif operator == b"Tf":
            require(in_text and len(operands) == 2, "Unscoped/malformed font selection")
            font_name = str(operands[0])
            require(
                font_name in fonts and math.isfinite(float(operands[1])) and float(operands[1]) > 0,
                "Unbound font or invalid text size",
            )
        elif operator == b"Tr":
            require(in_text and operands == [0], "Only actual filled glyphs qualify")
        elif operator == b"Tm":
            require(
                in_text and len(operands) == 6 and all(math.isfinite(float(value)) for value in operands),
                "Invalid text matrix",
            )
        elif operator == b"BDC":
            require(
                in_text and actual_text is None and len(operands) == 2 and str(operands[0]) == "/Span",
                "Nested/unscoped ActualText",
            )
            properties = operands[1]
            require(
                isinstance(properties, DictionaryObject) and set(properties) == {"/ActualText"},
                "Only inline, explicitly scoped ActualText qualifies",
            )
            actual_text = properties["/ActualText"]
            require(isinstance(actual_text, TextStringObject) and bool(actual_text), "Invalid ActualText string")
            actual_text.encode("utf-8", errors="strict")
            actual_shows = 0
        elif operator == b"EMC":
            require(
                in_text and actual_text is not None and actual_shows == 1,
                "ActualText must bind exactly one painted glyph show",
            )
            actual_text = None
        elif operator in (b"Tj", b"TJ"):
            require(in_text and font_name in fonts and len(operands) == 1, "Unscoped/unbound text show")
            values = operands[0] if operator == b"TJ" else operands
            require(isinstance(values, list) and bool(values), "Malformed text show")
            cids, chunks = [], 0
            for value in values:
                if isinstance(value, (ByteStringObject, TextStringObject)):
                    raw = value.original_bytes
                    require(raw and len(raw) % 2 == 0, "Expected nonempty Identity-H CID bytes")
                    cids.extend(int.from_bytes(raw[offset : offset + 2], "big") for offset in range(0, len(raw), 2))
                    chunks += 1
                else:
                    require(
                        operator == b"TJ"
                        and not isinstance(value, bool)
                        and isinstance(value, (int, float))
                        and math.isfinite(float(value)),
                        "Invalid TJ position adjustment",
                    )
            require(cids, "Text show contains no painted CIDs")
            if actual_text is not None:
                actual_shows += 1
                require(
                    actual_shows == 1 and len(cids) == 1 and chunks == 1,
                    "ActualText run segmentation is outside the qualified single-CID profile",
                )
                overrides += 1
            model = fonts[font_name]
            for cid in cids:
                require(
                    0 < cid < len(model["order"]) and cid in model["mapping"],
                    "Used CID has no verified glyph/Unicode mapping",
                )
                result.append(
                    (
                        model["source"],
                        model["order"][cid],
                        str(actual_text) if actual_text is not None else model["mapping"][cid],
                    )
                )
        else:
            # In particular Do/BMC, external property dictionaries, text in a
            # Form, invisible Tr and unknown text operators cannot be laundered
            # through the page's otherwise valid font resource dictionary.
            require(
                operator in graphics and not in_text and actual_text is None,
                "Unqualified PDF content operator/text-bearing Form",
            )
    require(not stack and not in_text and actual_text is None, "Unclosed graphics/text/ActualText scope")
    return result, overrides


def qualify_fonts(
    reader: PdfReader,
    fixture: Path,
    provider: str,
    scene_path: Path | None,
    bundle_path: Path | None,
    pdf: Path,
    *,
    required_faces: set[str] | None = None,
    source_fonts: dict[str, tuple[Path, str]] | None = None,
) -> dict:
    """Verify exact whole dompdf fonts or Pliego source/scene/subset closure.

    ``fixture`` supplies ``font-closure.json`` assets with file/sha256 fields
    and ``resources/<file>``. Other documents must pass their exact face set;
    ``source_fonts`` may supply their own frozen paths/hashes without copying.
    """
    require(provider in {"dompdf", "pliego"}, "Unqualified font provider")
    required_faces = set(USED_FACES if required_faces is None else required_faces)
    require(required_faces, "Expected font faces must not be empty")
    if source_fonts is None:
        closure = json.loads(closed_path(fixture, "font-closure.json").read_bytes())
        source_fonts = {}
        for asset in closure["assets"]:
            require(asset["file"] not in source_fonts, "Duplicate source font")
            source_fonts[asset["file"]] = (closed_path(fixture, "resources/" + asset["file"]), asset["sha256"])
    sources = {}
    for name, (path, expected_sha256) in source_fonts.items():
        sources[name] = source_font(str(path), expected_sha256)
    require(required_faces <= sources.keys(), "Required source font is absent")
    resources = {}
    scene_glyphs = set()
    scene_pages = []
    if provider == "pliego":
        require(scene_path is not None and bundle_path is not None, "Candidate font proof requires scene and bundle")
        scene, entries = checked_bundle(bundle_path, scene_path, pdf)
        for page in scene["pages"]:
            painted = []
            for operation in page["operations"]:
                if operation["type"] != "text":
                    continue
                reference = operation["font"]
                require(
                    reference["face_index"] == 0 and (not reference["variations"]),
                    "Ledger qualification requirement failed: reference['face_index'] == 0 and (not reference['variations'])",
                )
                require(not reference["synthetic_bold"], "Select the supplied bold face, not a synthetic substitute")
                resource = reference["resource"]
                if resource not in resources:
                    require(
                        re.fullmatch("sha256:[0-9a-f]{64}", resource),
                        "Ledger qualification requirement failed: re.fullmatch('sha256:[0-9a-f]{64}', resource)",
                    )
                    path = entries["resources/" + resource.removeprefix("sha256:")]
                    raw = path.read_bytes()
                    require(
                        "sha256:" + digest(raw) == resource,
                        "Ledger qualification requirement failed: 'sha256:' + digest(raw) == resource",
                    )
                    font = font_from_bytes(raw)
                    glyphs = glyph_fingerprints(font)
                    semantic = semantic_font_identity(font, glyphs)
                    matches = [name for name, (facts, _) in sources.items() if facts["semantic"] == semantic]
                    require(len(matches) == 1, "Sanitized font does not preserve exactly one original face")
                    resources[resource] = {
                        "source": matches[0],
                        "sha256": digest(raw),
                        "semantic": semantic,
                        "font": font,
                        "glyphs": glyphs,
                    }
                selected = resources[resource]
                order = selected["font"].getGlyphOrder()
                text = operation["text"].encode("utf-8")
                for glyph in operation["glyphs"]:
                    span = glyph["text_range"]
                    require(
                        0 <= span["start"] < span["end"] <= len(text),
                        "Ledger qualification requirement failed: 0 <= span['start'] < span['end'] <= len(text)",
                    )
                    unicode = text[span["start"] : span["end"]].decode("utf-8")
                    require(type(glyph["id"]) is int and 0 < glyph["id"] < len(order), "Invalid scene glyph id")
                    identity = (selected["source"], order[glyph["id"]], unicode)
                    scene_glyphs.add(identity)
                    painted.append(identity)
            scene_pages.append(painted)
        require({r["source"] for r in resources.values()} == required_faces, "Required scene faces differ")

    seen = set()
    embedded = []
    pdf_glyphs = set()
    font_models = {}
    pdf_pages = []
    actual_text_overrides = 0
    for page in reader.pages:
        page_fonts = {}
        for font_name, reference in page["/Resources"]["/Font"].items():
            font = reference.get_object()
            identity = (reference.idnum, reference.generation) if hasattr(reference, "idnum") else id(font)
            if identity in font_models:
                page_fonts[str(font_name)] = font_models[identity]
                continue
            descendant = font.get("/DescendantFonts", [font])[0].get_object()
            descriptor = descendant["/FontDescriptor"].get_object()
            require("/FontFile2" in descriptor, "Unembedded or non-TrueType ledger font")
            raw = descriptor["/FontFile2"].get_object().get_data()
            embedded_sha = digest(raw)
            if provider == "dompdf":
                matches = [name for name, (facts, _) in sources.items() if facts["sha256"] == embedded_sha]
                require(
                    len(matches) == 1 and matches[0] in required_faces,
                    "Legacy embedded font is not original unsubsetted bytes",
                )
                source = matches[0]
                font_models[identity] = {}
            else:
                require(
                    font.get("/Subtype") == "/Type0"
                    and font.get("/Encoding") == "/Identity-H"
                    and descendant.get("/Subtype") == "/CIDFontType2",
                    "Expected Type0/Identity-H/CIDFontType2 candidate font",
                )
                source, subset = match_subset_program(raw, sources, required_faces)
                require(descendant["/CIDToGIDMap"] == "/Identity", "Unqualified CID-to-glyph remapping")
                order = subset.getGlyphOrder()
                mapping = unicode_map(font)
                for cid in mapping:
                    require(0 < cid < len(order), "Invalid PDF subset glyph id")
                font_models[identity] = {"source": source, "order": order, "mapping": mapping}
                page_fonts[str(font_name)] = font_models[identity]
            seen.add(source)
            embedded.append({"pdfName": str(descendant["/BaseFont"]), "source": source, "embeddedSha256": embedded_sha})
        if provider == "pliego":
            require("/Contents" in page, "PDF page has no actual painted text content")
            painted, overrides = painted_text_runs(ContentStream(page["/Contents"], reader).operations, page_fonts)
            pdf_pages.append(painted)
            pdf_glyphs.update(painted)
            actual_text_overrides += overrides
    require(seen == required_faces, "Required embedded faces differ")
    if provider == "pliego":
        require(
            pdf_pages == scene_pages and pdf_glyphs == scene_glyphs,
            "Actual painted PDF glyph/Unicode order/count differs from the hash-bound scene",
        )
    return {
        "outcome": "passed",
        "policy": "original-whole-font-bytes"
        if provider == "dompdf"
        else "source-outline-metric-cmap-style-and-used-scene-subset-glyph-closure-v2",
        "embedded": embedded,
        "sceneResources": [{k: r[k] for k in ("source", "sha256", "semantic")} for r in resources.values()],
        "scenePdfGlyphMappings": len(pdf_glyphs),
        "paintedGlyphCount": sum(len(page) for page in pdf_pages),
        "scopedActualTextOverrides": actual_text_overrides,
    }
