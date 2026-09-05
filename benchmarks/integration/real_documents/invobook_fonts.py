# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Observed Chromium invoice font profile, deliberately separate from Krilla."""

from __future__ import annotations

import math
from pathlib import Path
import re

from pypdf import PdfReader
from pypdf.generic import ByteStringObject, ContentStream, TextStringObject

from ledger_fonts import digest, font_from_bytes, glyph_fingerprints, source_font
from manufacturing_corpus import require


def unicode_map(raw: bytes) -> dict[int, str]:
    text = raw.decode("ascii")
    require("usecmap" not in text, "Inherited Chrome Unicode maps are unqualified")
    spaces = re.findall(r"(\d+)\s+begincodespacerange(.*?)endcodespacerange", text, re.S)
    require(
        len(spaces) == 1 and spaces[0][0] == "1" and re.fullmatch(r"\s*<0000>\s*<FFFF>\s*", spaces[0][1]),
        "Expected 16-bit Chrome codespace",
    )
    mappings = {}
    for kind, width in (("bfchar", 2), ("bfrange", 3)):
        blocks = re.findall(r"(\d+)\s+begin" + kind + r"(.*?)end" + kind, text, re.S)
        require(text.count("begin" + kind) == len(blocks), "Malformed Chrome Unicode map block")
        pattern = r"\s*".join([r"<([0-9A-Fa-f]{4})>"] * width)
        for count, block in blocks:
            values = re.findall(pattern, block)
            require(
                len(values) == int(count) and not re.sub(pattern, "", block).strip(),
                "Only counted scalar BMP Chrome mappings are qualified",
            )
            for entry in values:
                first = int(entry[0], 16)
                last = first if kind == "bfchar" else int(entry[1], 16)
                value = int(entry[-1], 16)
                require(0 < first <= last <= 65535, "Invalid Chrome CID range")
                for cid in range(first, last + 1):
                    codepoint = value + cid - first
                    require(
                        0 < codepoint <= 65535 and not 0xD800 <= codepoint <= 0xDFFF and cid not in mappings,
                        "Invalid/duplicate Chrome Unicode mapping",
                    )
                    mappings[cid] = chr(codepoint)
    require(bool(mappings), "Missing Chrome Unicode mappings")
    return mappings


def cid_widths(descendant: dict) -> dict[int, float]:
    values = descendant["/W"]
    result = {}
    index = 0
    while index < len(values):
        first = values[index]
        require(isinstance(first, int) and 0 <= first <= 65535, "Invalid Chrome width CID")
        following = values[index + 1]
        if isinstance(following, list):
            widths = following
            index += 2
        else:
            require(isinstance(following, int) and first <= following <= 65535, "Invalid Chrome width range")
            widths = [values[index + 2]] * (following - first + 1)
            index += 3
        for cid, width in enumerate(widths, start=first):
            require(
                cid <= 65535 and cid not in result and math.isfinite(float(width)) and float(width) >= 0,
                "Invalid/duplicate Chrome PDF width",
            )
            result[cid] = float(width)
    return result


def used_cids(reader: PdfReader) -> dict[str, set[int]]:
    require(len(reader.pages) == 1, "Observed Chrome invoice font profile is one-page only")
    # This exact producer paints scalar Tj strings, not forms or arbitrary text
    # operators. New producer surfaces need a separately reviewed mapping gate.
    allowed = {
        b"cm",
        b"q",
        b"Q",
        b"RG",
        b"rg",
        b"gs",
        b"re",
        b"f",
        b"W*",
        b"n",
        b"BDC",
        b"EMC",
        b"BT",
        b"ET",
        b"Tf",
        b"Tm",
        b"Tj",
        b"Td",
    }
    current = None
    stack = []
    in_text = False
    result = {}
    for arguments, operator in ContentStream(reader.pages[0].get_contents(), reader).operations:
        require(operator in allowed, "Unqualified Chrome content operator: " + repr(operator))
        if operator == b"q":
            stack.append(current)
        elif operator == b"Q":
            require(bool(stack), "Unbalanced Chrome graphics state")
            current = stack.pop()
        elif operator == b"BT":
            require(not in_text, "Nested Chrome text object")
            in_text = True
        elif operator == b"ET":
            require(in_text, "Unexpected Chrome text-object end")
            in_text = False
        elif operator == b"Tf":
            require(
                in_text and len(arguments) == 2 and math.isfinite(float(arguments[1])) and float(arguments[1]) > 0,
                "Invalid active Chrome font",
            )
            current = str(arguments[0])
        elif operator == b"gs":
            require(len(arguments) == 1, "Invalid Chrome graphics-state selection")
            state = reader.pages[0]["/Resources"]["/ExtGState"][arguments[0]].get_object()
            require("/Font" not in state, "Graphics-state font selection is unqualified")
        elif operator == b"Tj":
            require(in_text and current is not None and len(arguments) == 1, "Chrome text has no active font")
            value = arguments[0]
            require(isinstance(value, (ByteStringObject, TextStringObject)), "Invalid Chrome text bytes")
            raw = value.original_bytes if isinstance(value, TextStringObject) else bytes(value)
            require(len(raw) > 0 and len(raw) % 2 == 0, "Invalid Identity-H Chrome text length")
            result.setdefault(current, set()).update(
                int.from_bytes(raw[i : i + 2], "big") for i in range(0, len(raw), 2)
            )
    require(not stack and not in_text and bool(result), "Incomplete Chrome content stream")
    return result


def qualify_chrome_fonts(reader: PdfReader, source_fonts: dict[str, tuple[Path, str]]) -> dict:
    used = used_cids(reader)
    fonts = reader.pages[0]["/Resources"]["/Font"]
    require(set(used) == set(fonts), "Unpainted or undeclared Chrome font resource")
    records = []
    seen = set()
    for resource_name, reference in fonts.items():
        font = reference.get_object()
        require(font["/Subtype"] == "/Type0" and font["/Encoding"] == "/Identity-H", "Unqualified Chrome font encoding")
        descendants = font["/DescendantFonts"]
        require(len(descendants) == 1, "Unexpected Chrome descendant font count")
        descendant = descendants[0].get_object()
        require(descendant["/CIDToGIDMap"] == "/Identity", "Unqualified Chrome CID remapping")
        name = re.sub(r"^/[A-Z]{6}\+", "", str(font["/BaseFont"])) + ".ttf"
        require(name in source_fonts and name not in seen, "Wrong/duplicate original Chrome face")
        seen.add(name)
        path, expected_hash = source_fonts[name]
        source, original = source_font(str(path), expected_hash)
        descriptor = descendant["/FontDescriptor"].get_object()
        require("/FontFile2" in descriptor, "Chrome font is not embedded TrueType")
        raw = descriptor["/FontFile2"].get_object().get_data()
        subset = font_from_bytes(raw)
        require(
            subset["head"].unitsPerEm == original["head"].unitsPerEm
            and subset["OS/2"].usWeightClass == original["OS/2"].usWeightClass
            and subset["OS/2"].fsSelection == original["OS/2"].fsSelection,
            "Chrome source face style changed",
        )
        mappings = unicode_map(font["/ToUnicode"].get_object().get_data())
        require(used[str(resource_name)] == set(mappings), "Painted Chrome CIDs differ from Unicode map")
        glyphs = glyph_fingerprints(subset)
        order = subset.getGlyphOrder()
        cmap = original.getBestCmap()
        widths = cid_widths(descendant)
        for cid, text in mappings.items():
            require(cid < len(order) and ord(text) in cmap, "Chrome glyph is absent from subset/source")
            source_name = cmap[ord(text)]
            require(
                glyphs[order[cid]] == source["glyphs"][source_name],
                "Chrome subset outline/metric differs from original Unicode glyph",
            )
            width = widths.get(cid, float(descendant.get("/DW", 1000)))
            expected_width = original["hmtx"].metrics[source_name][0] * 1000 / original["head"].unitsPerEm
            require(abs(width - expected_width) <= 0.00001, "Chrome PDF advance width differs from original metric")
        records.append(
            {
                "pdfName": str(font["/BaseFont"]),
                "source": name,
                "sourceSha256": expected_hash,
                "embeddedSha256": digest(raw),
                "paintedGlyphMappings": len(mappings),
            }
        )
    require(seen == set(source_fonts), "Required original Chrome faces differ")
    return {
        "outcome": "passed",
        "policy": "chrome-identity-h-painted-cid-source-unicode-outline-metric-style-v1",
        "embedded": records,
        "paintedGlyphMappings": sum(record["paintedGlyphMappings"] for record in records),
    }
