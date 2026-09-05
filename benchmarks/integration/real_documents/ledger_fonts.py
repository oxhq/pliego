"""Shared DejaVu program/closure gates; ledger defaults are not other fixtures' expectations."""

from __future__ import annotations

import hashlib
import io
import json
import re
from functools import lru_cache
from pathlib import Path

from fontTools.ttLib import TTFont
from pypdf import PdfReader

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
    if provider == "pliego":
        require(scene_path is not None and bundle_path is not None, "Candidate font proof requires scene and bundle")
        scene, entries = checked_bundle(bundle_path, scene_path, pdf)
        for page in scene["pages"]:
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
                    scene_glyphs.add((selected["source"], order[glyph["id"]], unicode))
        require({r["source"] for r in resources.values()} == required_faces, "Required scene faces differ")

    seen = set()
    embedded = []
    pdf_glyphs = set()
    seen_objects = set()
    for page in reader.pages:
        for reference in page["/Resources"]["/Font"].values():
            font = reference.get_object()
            identity = (reference.idnum, reference.generation) if hasattr(reference, "idnum") else id(font)
            if identity in seen_objects:
                continue
            seen_objects.add(identity)
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
            else:
                source, subset = match_subset_program(raw, sources, required_faces)
                require(descendant["/CIDToGIDMap"] == "/Identity", "Unqualified CID-to-glyph remapping")
                order = subset.getGlyphOrder()
                for cid, unicode in unicode_map(font).items():
                    require(0 < cid < len(order), "Invalid PDF subset glyph id")
                    pdf_glyphs.add((source, order[cid], unicode))
            seen.add(source)
            embedded.append({"pdfName": str(descendant["/BaseFont"]), "source": source, "embeddedSha256": embedded_sha})
    require(seen == required_faces, "Required embedded faces differ")
    if provider == "pliego":
        require(pdf_glyphs == scene_glyphs, "PDF Unicode/glyph identities differ from the hash-bound captured scene")
    return {
        "outcome": "passed",
        "policy": "original-whole-font-bytes"
        if provider == "dompdf"
        else "source-outline-metric-cmap-style-and-scene-subset-glyph-closure-v1",
        "embedded": embedded,
        "sceneResources": [{k: r[k] for k in ("source", "sha256", "semantic")} for r in resources.values()],
        "scenePdfGlyphMappings": len(pdf_glyphs),
    }
