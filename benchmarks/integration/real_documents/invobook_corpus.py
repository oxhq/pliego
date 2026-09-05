"""Byte and business-fact closure for the shared repaired Invobook invoice."""

import json
from pathlib import Path

from manufacturing_corpus import digest, member, require

DEFAULT_CORPUS = Path(__file__).resolve().parent / "fixtures/invobook"
INPUT_SHA256 = "afd286bca202309923fd66bee1f71e732bdd340d1b05a2307094feb535fa7195"
FACTS_SHA256 = "ecfe7ef88c4f94d2d7579178757215aa4d43c788958b62591525e777ca652762"
FONT_SHA256 = {
    "DejaVuSans.ttf": "7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954",
    "DejaVuSans-Bold.ttf": "e6476c1b80502924294eed40894c5b18e06c181444ca953e5334262df9c27724",
}


def load_corpus(root: Path = DEFAULT_CORPUS, assets: Path | None = None) -> tuple[dict, dict]:
    root = root.resolve()
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    require(manifest["schema"] == "pliego.real-document.invobook-corpus.v1", "Unexpected invoice corpus")
    paths = [record["path"] for record in manifest["files"]]
    require(len(paths) == len(set(paths)), "Duplicate corpus member")
    require(
        {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()}
        == set(paths) | {"manifest.json"},
        "Unlisted or missing corpus file",
    )
    for record in manifest["files"]:
        path = member(root, record["path"])
        require(
            path.stat().st_size == record["bytes"] and digest(path) == record["sha256"],
            "Corpus bytes differ: " + record["path"],
        )
    require(digest(member(root, "input.html")) == INPUT_SHA256, "Frozen invoice HTML changed")
    require(digest(member(root, "expected-facts.json")) == FACTS_SHA256, "Frozen invoice facts changed")
    require(
        {font["path"] for font in manifest["fonts"]} == {"fonts/" + name for name in FONT_SHA256},
        "Frozen invoice font inventory changed",
    )
    for name, expected_hash in FONT_SHA256.items():
        require(digest(member(root, "fonts/" + name)) == expected_hash, "Original invoice font changed")
    require(
        manifest["request"] == {"pageSize": "47622x67351au", "pageMargins": "0,0,0,0au"},
        "Frozen invoice request geometry changed",
    )
    if assets is not None:
        for font in manifest["fonts"]:
            require(
                digest(member(assets.resolve(), Path(font["path"]).name)) == digest(member(root, font["path"])),
                "Staged invoice font changed",
            )
    facts = json.loads(member(root, "expected-facts.json").read_text(encoding="utf-8"))
    require(facts["schema"] == "pliego.invobook-invoice-facts.v1", "Unexpected invoice facts")
    require(len(facts["items"]) == 2, "Expected two actual work-session items")
    for item in facts["items"]:
        require(item["quantity"] * item["unitPriceCents"] == item["subtotalCents"], "Broken row equation")
    require(sum(item["subtotalCents"] for item in facts["items"]) == facts["subtotalCents"], "Broken subtotal equation")
    require(facts["subtotalCents"] + facts["taxCents"] == facts["totalCents"], "Broken tax/total equation")
    return manifest, facts
