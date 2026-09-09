# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Verify the small, frozen Aureus work-order corpus without booting the app."""

from __future__ import annotations

from decimal import Decimal
import hashlib
import json
from pathlib import Path, PurePosixPath

DEFAULT_CORPUS = Path(__file__).resolve().parent / "fixtures/manufacturing"
INPUT_SHA256 = "a08f2e7250be77df689e62768b5929bed7a41ed9d4ae1783a0e297fc709f2c3c"
FACTS_SHA256 = "6eb005d4762d7aa13ba01b0c1cd67501d7efe2f07cb51b6cd5821f5316e79b8d"
FONT_SHA256 = {
    "DejaVuSans.ttf": "7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954",
    "DejaVuSans-Bold.ttf": "e6476c1b80502924294eed40894c5b18e06c181444ca953e5334262df9c27724",
}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def member(root: Path, relative: str) -> Path:
    """Manifest paths are portable relative paths, never aliases or escapes."""
    path = PurePosixPath(relative)
    require(
        bool(path.parts)
        and not path.is_absolute()
        and ".." not in path.parts
        and "\\" not in relative
        and ":" not in relative,
        "Invalid corpus member path",
    )
    result = root.joinpath(*path.parts)
    require(result.resolve().is_relative_to(root.resolve()), "Corpus member escaped root")
    require(not result.is_symlink() and result.is_file(), "Missing/aliased corpus member")
    return result


def load_corpus(root: Path = DEFAULT_CORPUS, assets: Path | None = None) -> tuple[dict, dict]:
    root = root.resolve()
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    require(
        manifest["schema"] == "pliego.real-document.manufacturing-corpus.v1"
        and manifest["track"] == "aureus-manufacturing-008-font-closed",
        "Unexpected manufacturing corpus",
    )
    paths = [record["path"] for record in manifest["files"]]
    require(len(paths) == len(set(paths)), "Duplicate corpus member")
    actual_paths = {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()}
    require(actual_paths == set(paths) | {"manifest.json"}, "Unlisted or missing corpus file")
    for record in manifest["files"]:
        path = member(root, record["path"])
        require(path.stat().st_size == record["bytes"], "Corpus size differs: " + record["path"])
        require(digest(path) == record["sha256"], "Corpus hash differs: " + record["path"])
    require(digest(member(root, manifest["input"])) == INPUT_SHA256, "Frozen HTML changed")
    require(digest(member(root, manifest["expectedFacts"])) == FACTS_SHA256, "Frozen facts changed")
    require(
        manifest["request"]["pageSize"] == "47622x67351au"
        and manifest["request"]["pageMargins"] == "2721,2721,2721,2721au"
        and manifest["request"]["authority"] == "request-only-v1",
        "Frozen request geometry changed",
    )
    html = member(root, manifest["input"]).read_bytes()
    css = member(root, "delivery-fonts.css").read_bytes()
    original = member(root, manifest["originalInput"]).read_bytes()
    require(
        html.count(css) == 1 and html.replace(css, b"", 1) == original,
        "More than the declared shared font closure changed",
    )
    require(html.count(b"@font-face") == 2, "Expected exactly two supplied font faces")
    require(
        {font["path"] for font in manifest["fonts"]} == {"resources/" + name for name in FONT_SHA256},
        "Frozen font inventory changed",
    )
    for name, expected_hash in FONT_SHA256.items():
        require(digest(member(root, "resources/" + name)) == expected_hash, "Original manufacturing font changed")
    if assets is not None:
        for font in manifest["fonts"]:
            original_font = member(root, font["path"])
            supplied = member(assets.resolve(), PurePosixPath(font["path"]).name)
            require(digest(supplied) == digest(original_font), "Supplied font bytes changed")
    expected = json.loads(member(root, manifest["expectedFacts"]).read_text(encoding="utf-8"))
    require(expected["schema"] == "aureus.manufacturing-work-order.synthetic.v1", "Unexpected fact schema")
    require(
        [item["name"] for item in expected["components"]] == manifest["expectedComponentOrder"],
        "Frozen capture component order changed",
    )
    quantity = Decimal(str(expected["quantity"]))
    for item in expected["components"]:
        require(
            Decimal(str(item["perUnit"])) * quantity == Decimal(str(item["required"])),
            "Component multiplication is inconsistent",
        )
    for operation in expected["operations"]:
        require(
            Decimal(str(operation["seconds"])) / 60 == Decimal(str(operation["minutes"])),
            "Operation duration is inconsistent",
        )
    workflow = json.loads(member(root, "workflow-facts.json").read_text(encoding="utf-8"))
    require(
        workflow["state"] == "done" and workflow["quantityProducing"] == expected["quantity"],
        "Original production workflow was not complete",
    )
    return manifest, expected
