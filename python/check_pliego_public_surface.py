#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fail when Pliego's buyer-facing surface drifts from retained release evidence."""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[1]
SHOWCASE = ROOT / "docs" / "pliego" / "showcase"
MANIFEST = SHOWCASE / "manifest.json"
PUBLIC_MARKDOWN = (
    ROOT / "README.md",
    ROOT / "ROADMAP.md",
    ROOT / "docs" / "project-overview.md",
    ROOT / "docs" / "releases" / "v0.3.md",
    ROOT / "docs" / "pliego" / "support-profile.md",
    ROOT / "docs" / "security" / "threat-model.md",
    ROOT / "docs" / "funding" / "2026.md",
    ROOT / "docs" / "benchmarks" / "README.md",
    ROOT / "sdk" / "laravel" / "README.md",
    ROOT / "sdk" / "php" / "README.md",
)
LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


class SurfaceError(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SurfaceError(message)


def read(path: Path) -> str:
    require(path.is_file(), f"required public file is absent: {path.relative_to(ROOT)}")
    return path.read_text(encoding="utf-8")


def verify_current_copy() -> None:
    readme = read(ROOT / "README.md")
    overview = read(ROOT / "docs" / "project-overview.md")
    launch = read(ROOT / "docs" / "releases" / "v0.3.md")
    support = read(ROOT / "docs" / "pliego" / "support-profile.md")
    threat_model = read(ROOT / "docs" / "security" / "threat-model.md")
    laravel = read(ROOT / "sdk" / "laravel" / "README.md")
    php = read(ROOT / "sdk" / "php" / "README.md")

    for path, content in (
        (ROOT / "README.md", readme),
        (ROOT / "docs" / "project-overview.md", overview),
        (ROOT / "docs" / "releases" / "v0.3.md", launch),
    ):
        require("v0.1.1" not in content, f"stale v0.1.1 buyer-facing copy: {path.relative_to(ROOT)}")

    required_readme = (
        "**Current stable line:** Pliego 0.3 / API 2. **Recommended build:** v0.3.2.",
        "->store(",
        "Storage::disk($stored->disk)->download(",
        "pliego --contract-probe",
        "There are no committed publishable performance numbers.",
        "Link annotations are also outside the advertised v0.3.2 API 2 profile.",
        "docs/pliego/showcase/manifest.json",
    )
    for needle in required_readme:
        require(needle in readme, f"README omitted required evidence boundary: {needle!r}")

    forbidden = (
        "Network access remains opt-in",
        "chartjs-report.pdf",
        "chartjs-report.png",
        "Pliego is faster",
    )
    combined = "\n".join((readme, overview, launch))
    for needle in forbidden:
        require(needle not in combined, f"unsupported public claim or stale artifact reference: {needle!r}")
    require(
        re.search(r"\b\d+(?:\.\d+)?[xX]\s+faster\b", combined) is None,
        "buyer-facing copy contains an unretained multiplicative performance claim",
    )
    require("PDF link annotations | Not advertised in v0.3.2 API 2" in support, "link limitation is not public")
    require("collapsed-table-borders" in support, "collapsed-table API 2 limitation is not public")
    require("Chart.js is not advertised" in laravel, "Laravel guide overstates current Chart.js support")
    require("collapsed-table-borders" in laravel, "Laravel guide omits the collapsed-table API 2 limit")
    require("current stable package is 0.3.2" in php, "PHP guide omits the current stable package")
    require("pass their paths through `InputAsset`" in php, "PHP guide misstates the API 2 asset input")
    require("makes no comparative performance claim" in php, "PHP guide omits the benchmark boundary")
    require("supply their bytes through `assets`" not in php, "PHP guide retains stale API 2 asset guidance")
    require(
        "v0.3.2 API 2 does not advertise link annotations" in threat_model,
        "threat model overstates API 2 link support",
    )


def verified_manifest_path(relative: str) -> Path:
    require(isinstance(relative, str) and relative, "showcase file path must be a non-empty string")
    candidate = (SHOWCASE / relative).resolve(strict=True)
    require(candidate == ROOT or ROOT in candidate.parents, f"showcase path escapes repository: {relative!r}")
    require(candidate.is_file() and not candidate.is_symlink(), f"showcase path is not a regular file: {relative!r}")
    return candidate


def verify_showcase() -> None:
    data = json.loads(read(MANIFEST))
    require(data.get("schema") == "pliego.showcase-manifest", "showcase manifest schema changed")
    require(data.get("version") == 1, "showcase manifest version changed")
    engine = data.get("engine")
    require(isinstance(engine, dict), "showcase engine identity is absent")
    require(engine.get("release_tag") == "v0.3.2", "showcase is not bound to v0.3.2")
    require(
        engine.get("source_commit") == "1c237ed7e1f8247254facfe2284d88acf270185a",
        "showcase source commit differs from the published v0.3.2 engine",
    )

    files = data.get("files")
    require(isinstance(files, list) and files, "showcase manifest has no files")
    indexed: dict[str, dict[str, object]] = {}
    for descriptor in files:
        require(isinstance(descriptor, dict), "showcase file descriptor is not an object")
        require(set(descriptor) == {"path", "bytes", "sha256"}, "showcase file descriptor shape changed")
        relative = descriptor["path"]
        require(isinstance(relative, str) and relative not in indexed, "showcase file path is invalid or duplicated")
        require(isinstance(descriptor["bytes"], int) and descriptor["bytes"] >= 0, "showcase byte count is invalid")
        require(isinstance(descriptor["sha256"], str) and SHA256.fullmatch(descriptor["sha256"]), "showcase SHA-256 is invalid")
        path = verified_manifest_path(relative)
        payload = path.read_bytes()
        require(len(payload) == descriptor["bytes"], f"showcase byte count drifted: {relative}")
        require(hashlib.sha256(payload).hexdigest() == descriptor["sha256"], f"showcase hash drifted: {relative}")
        indexed[relative] = descriptor

    documents = data.get("documents")
    require(isinstance(documents, list) and {item.get("id") for item in documents if isinstance(item, dict)} == {"invoice", "operating-report"}, "showcase document set changed")
    for document in documents:
        require(isinstance(document, dict), "showcase document is not an object")
        for field in ("source", "pdf"):
            require(document.get(field) in indexed, f"showcase document references an unbound {field}")
        require(isinstance(document.get("pages"), int) and document["pages"] > 0, "showcase page count is invalid")
        require(document.get("repeat_render_sha256_matched") is True, "showcase repeat-render proof is absent")
        require(document.get("all_pages_visually_reviewed") is True, "showcase visual review is absent")
        for field in ("input_manifest_sha256", "request_sha256"):
            require(isinstance(document.get(field), str) and SHA256.fullmatch(document[field]), f"showcase {field} is invalid")


def markdown_target(raw: str) -> str | None:
    raw = raw.strip()
    if raw.startswith("<") and ">" in raw:
        raw = raw[1 : raw.index(">")]
    else:
        raw = raw.split(maxsplit=1)[0]
    if not raw or raw.startswith(("#", "http://", "https://", "mailto:")):
        return None
    return unquote(raw.split("#", 1)[0])


def verify_local_links() -> None:
    for source in PUBLIC_MARKDOWN:
        for match in LINK.finditer(read(source)):
            target = markdown_target(match.group(1))
            if target is None:
                continue
            candidate = (source.parent / target).resolve()
            require(candidate == ROOT or ROOT in candidate.parents, f"local Markdown link escapes repository: {source.relative_to(ROOT)} -> {target}")
            require(candidate.exists(), f"broken local Markdown link: {source.relative_to(ROOT)} -> {target}")


def main() -> int:
    try:
        verify_current_copy()
        verify_showcase()
        verify_local_links()
    except (SurfaceError, OSError, UnicodeError, json.JSONDecodeError) as error:
        print(f"check_pliego_public_surface: {error}", file=sys.stderr)
        return 1
    print("Pliego public surface matches retained v0.3.2 evidence")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
