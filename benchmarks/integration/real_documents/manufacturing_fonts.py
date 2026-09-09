# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Thin provider bridge; each caller supplies its own exact source-font pins."""

from __future__ import annotations

import hashlib
import importlib.metadata
import json
from pathlib import Path
import re

from manufacturing_corpus import digest, require


def qualify_pdf_fonts(
    corpus: Path,
    source_fonts: dict[str, tuple[Path, str]],
    pdf: Path,
    provider: str,
    scene: Path | None,
    bundle: Path | None,
) -> dict:
    require(provider in {"dompdf", "browsershot", "pliego"}, "Unknown font provider")
    require(bool(source_fonts), "Missing caller-owned source-font pins")
    for source, expected_hash in source_fonts.values():
        require(digest(source) == expected_hash, "Caller-bound original font bytes changed")
    require(
        (scene is not None and bundle is not None) if provider == "pliego" else (scene is None and bundle is None),
        "Pliego requires both --scene and --bundle; legacy providers must not supply them",
    )
    from ledger_fonts import qualify_fonts
    from pypdf import PdfReader

    reader = PdfReader(pdf, strict=True)
    if provider == "browsershot":
        from invobook_fonts import qualify_chrome_fonts

        proof = qualify_chrome_fonts(reader, source_fonts)
    else:
        proof = qualify_fonts(
            reader,
            corpus,
            provider,
            scene,
            bundle,
            pdf,
            required_faces=set(source_fonts),
            source_fonts=source_fonts,
        )
    proof["sourceFontSha256"] = {name: sha for name, (_, sha) in source_fonts.items()}
    proof["dependencies"] = {name: importlib.metadata.version(name) for name in ("pypdf", "fonttools")}
    return proof


def layout_fingerprint(observed: dict) -> str:
    """Exact extracted facts only; no object IDs, subset tags, paths or metadata."""
    layout = {
        "pages": [
            {
                "width": page["width"],
                "height": page["height"],
                "words": [{key: word[key] for key in ("text", "x0", "y0", "x1", "y1")} for word in page["words"]],
            }
            for page in observed["pages"]
        ],
        "fonts": sorted(
            [
                {
                    "name": re.sub(r"^[A-Z]{6}\+", "", font["name"]),
                    **{key: font[key] for key in ("embedded", "unicode") if key in font},
                }
                for font in observed["fonts"]
            ],
            key=lambda font: font["name"],
        ),
    }
    if "barcodes" in observed:
        layout.update(
            imageWidth=observed["imageWidth"],
            imageHeight=observed["imageHeight"],
            barcodes=sorted(
                [{key: code[key] for key in ("text", "valid", "format", "points")} for code in observed["barcodes"]],
                key=lambda code: (code["text"], code["points"]),
            ),
        )
    return hashlib.sha256(
        json.dumps(layout, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    ).hexdigest()
