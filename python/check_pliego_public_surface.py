#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fail when Pliego's buyer-facing surface drifts from retained release evidence."""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[1]
BENCHMARK_TOOLS = ROOT / "benchmarks" / "tools"
SHOWCASE = ROOT / "docs" / "pliego" / "showcase"
MANIFEST = SHOWCASE / "manifest.json"
PUBLIC_MARKDOWN = (
    ROOT / "README.md",
    ROOT / "ROADMAP.md",
    ROOT / "docs" / "project-overview.md",
    ROOT / "docs" / "releases" / "v0.3.md",
    ROOT / "docs" / "releases" / "v0.3.3.md",
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


def verify_benchmark_publication() -> bool:
    process = subprocess.run(
        [sys.executable, str(BENCHMARK_TOOLS / "public_hosted_benchmark.py"), "verify"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    require(process.returncode == 0, f"hosted benchmark publication failed verification: {process.stderr.strip()}")
    status = process.stdout.strip()
    require(
        status in {"public hosted benchmark publication verified", "public hosted benchmark remains unpublished"},
        f"hosted benchmark verifier returned an unknown status: {status!r}",
    )
    return status == "public hosted benchmark publication verified"


def verify_current_copy(published_benchmark: bool) -> None:
    readme = read(ROOT / "README.md")
    overview = read(ROOT / "docs" / "project-overview.md")
    launch = read(ROOT / "docs" / "releases" / "v0.3.md")
    patch_release = read(ROOT / "docs" / "releases" / "v0.3.3.md")
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
        "**Current stable line:** Pliego 0.3 / API 2. **Recommended build:** v0.3.3.",
        "->store(",
        "Storage::disk($stored->disk)->download(",
        "pliego --contract-probe",
        "Link annotations are also outside the advertised v0.3.3 API 2 profile.",
        "passes it to Laravel Storage",
        "version-locked adapter dependency graphs",
        "docs/pliego/showcase/manifest.json",
    )
    for needle in required_readme:
        require(needle in readme, f"README omitted required evidence boundary: {needle!r}")
    if not published_benchmark:
        for needle in (
            "There is no committed performance snapshot yet.",
            "directional `github-hosted-exploratory` timing/resource evidence",
            "Authoritative\ntables and production rankings remain N/A",
        ):
            require(needle in readme, f"README omitted unpublished benchmark boundary: {needle!r}")
    else:
        for needle in (
            "The published `minimal-static` snapshot measures the released Pliego v0.3.3 bundle",
            "sealed three-repeat series",
            "timed samples passed the shared PDF oracle",
            "no best or canonical repeat is selected",
            "[Full comparative aggregate report and spread]",
            "[Immutable evidence release]",
            "Authoritative\ntables and production rankings remain N/A",
        ):
            require(needle in readme, f"README omitted published benchmark boundary: {needle!r}")

    forbidden = (
        "Network access remains opt-in",
        "chartjs-report.pdf",
        "chartjs-report.png",
        "Pliego is faster",
        "pinned one-shot paths",
        "`store()` never buffers",
    )
    combined = "\n".join((readme, overview, launch, patch_release))
    for needle in forbidden:
        require(needle not in combined, f"unsupported public claim or stale artifact reference: {needle!r}")
    require(
        re.search(r"\b\d+(?:\.\d+)?[xX]\s+faster\b", combined) is None,
        "buyer-facing copy contains an unretained multiplicative performance claim",
    )
    require("PDF link annotations | Not advertised in v0.3.3 API 2" in support, "link limitation is not public")
    require("collapsed-table-borders" in support, "collapsed-table API 2 limitation is not public")
    require("Chart.js is not advertised" in laravel, "Laravel guide overstates current Chart.js support")
    require("collapsed-table-borders" in laravel, "Laravel guide omits the collapsed-table API 2 limit")
    require("current stable package is 0.3.3" in php, "PHP guide omits the current stable package")
    require("pass their paths through `InputAsset`" in php, "PHP guide misstates the API 2 asset input")
    require("makes no comparative performance claim" in php, "PHP guide omits the benchmark boundary")
    require("supply their bytes through `assets`" not in php, "PHP guide retains stale API 2 asset guidance")
    require(
        "v0.3.3 API 2 does not advertise link annotations" in threat_model,
        "threat model overstates API 2 link support",
    )
    require(
        "v0.3.3 is the current recommended build" in overview,
        "project overview omits the current recommended release",
    )
    require("The current recommended patch is **v0.3.3**." in launch, "launch overview recommends a stale patch")
    for needle in (
        "41c6cf0e9cf1c73f4f70eba9d413fa97063a3154",
        "496d2809d3b47e6aef6596b229a8b7f2135d35ae",
        "788bc6980b117375625b56ec93d40a60da5a3a2d",
        "package run 33198517854",
        "promotion run 33203326313",
        "(`immutable: false`)",
    ):
        require(needle in patch_release, f"v0.3.3 notes omit exact release evidence: {needle!r}")
    for needle in (
        "fresh public-only Windows Laravel 13.29.0 consumer",
        "passed 53 focused assertions",
        "9115be57f785d08d1f4b13b7d70ff30bd4d9d052c1c96cc97065d78fcf291ec3",
        "RESOURCE_DENIED",
        "not representative adoption",
    ):
        require(needle in launch, f"launch overview omits scoped v0.3.3 consumer evidence: {needle!r}")


def verified_manifest_path(relative: str) -> Path:
    require(isinstance(relative, str) and len(relative) > 0, "showcase file path must be a non-empty string")
    relative_path = Path(relative)
    require(not relative_path.is_absolute(), f"showcase path must be relative: {relative!r}")
    lexical_candidate = Path(os.path.abspath(SHOWCASE / relative_path))
    require(
        lexical_candidate == ROOT or ROOT in lexical_candidate.parents,
        f"showcase path escapes repository: {relative!r}",
    )
    current = ROOT
    for part in lexical_candidate.relative_to(ROOT).parts:
        current /= part
        require(not current.is_symlink(), f"showcase path is not a regular file: {relative!r}")
    candidate = lexical_candidate.resolve(strict=True)
    require(candidate == ROOT or ROOT in candidate.parents, f"showcase path escapes repository: {relative!r}")
    require(candidate.is_file(), f"showcase path is not a regular file: {relative!r}")
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
    require(isinstance(files, list) and len(files) > 0, "showcase manifest has no files")
    indexed: dict[str, dict[str, object]] = {}
    for descriptor in files:
        require(isinstance(descriptor, dict), "showcase file descriptor is not an object")
        require(set(descriptor) == {"path", "bytes", "sha256"}, "showcase file descriptor shape changed")
        relative = descriptor["path"]
        require(isinstance(relative, str) and relative not in indexed, "showcase file path is invalid or duplicated")
        require(isinstance(descriptor["bytes"], int) and descriptor["bytes"] >= 0, "showcase byte count is invalid")
        require(
            isinstance(descriptor["sha256"], str) and SHA256.fullmatch(descriptor["sha256"]) is not None,
            "showcase SHA-256 is invalid",
        )
        path = verified_manifest_path(relative)
        payload = path.read_bytes()
        require(len(payload) == descriptor["bytes"], f"showcase byte count drifted: {relative}")
        require(hashlib.sha256(payload).hexdigest() == descriptor["sha256"], f"showcase hash drifted: {relative}")
        indexed[relative] = descriptor

    documents = data.get("documents")
    require(
        isinstance(documents, list)
        and {item.get("id") for item in documents if isinstance(item, dict)} == {"invoice", "operating-report"},
        "showcase document set changed",
    )
    for document in documents:
        require(isinstance(document, dict), "showcase document is not an object")
        for field in ("source", "pdf"):
            require(document.get(field) in indexed, f"showcase document references an unbound {field}")
        require(isinstance(document.get("pages"), int) and document["pages"] > 0, "showcase page count is invalid")
        require(document.get("repeat_render_sha256_matched") is True, "showcase repeat-render proof is absent")
        require(document.get("all_pages_visually_reviewed") is True, "showcase visual review is absent")
        for field in ("input_manifest_sha256", "request_sha256"):
            require(
                isinstance(document.get(field), str) and SHA256.fullmatch(document[field]) is not None,
                f"showcase {field} is invalid",
            )


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
            require(
                candidate == ROOT or ROOT in candidate.parents,
                f"local Markdown link escapes repository: {source.relative_to(ROOT)} -> {target}",
            )
            require(candidate.exists(), f"broken local Markdown link: {source.relative_to(ROOT)} -> {target}")


def main() -> int:
    try:
        published_benchmark = verify_benchmark_publication()
        verify_current_copy(published_benchmark)
        verify_showcase()
        verify_local_links()
    except (
        SurfaceError,
        OSError,
        UnicodeError,
        json.JSONDecodeError,
    ) as error:
        print(f"check_pliego_public_surface: {error}", file=sys.stderr)
        return 1
    suffix = " and the checksum-bound hosted benchmark" if published_benchmark else ""
    print(f"Pliego public surface matches v0.3.3 release evidence, the retained v0.3.2 showcase{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
