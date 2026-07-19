#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

EXPECTED_TEXT = "PLIEGO FONT FALLBACK"
FALLBACK_CHAIN = ["Missing Preferred", "Pliego Fixture"]


def fail(message: str, code: int = 1) -> None:
    print(f"font resolution check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_json(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"artifact does not exist: {path}")
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"artifact is not an object: {path}")
    return value


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    value = json.loads(lines[-1])
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def render(
    binary: Path, fixture: Path, root: Path, host: str, *, allow_host_fonts: bool = False
) -> dict[str, Any]:
    home = root / f"home-{host}"
    home.mkdir()
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(home),
            "XDG_CACHE_HOME": str(home / "cache"),
            "XDG_CONFIG_HOME": str(home / "config"),
            "XDG_DATA_HOME": str(home / "data"),
        }
    )
    output = root / f"{host}.pdf"
    artifacts = root / f"artifacts-{host}"
    command = [str(binary), "render", fixture.name]
    if allow_host_fonts:
        command.append("--allow-host-fonts")
    command.extend(["--output", str(output), "--artifacts", str(artifacts)])
    result = subprocess.run(
        command,
        cwd=fixture.parent,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    require(result.returncode == 0, f"{host} render failed: {result.stderr[-2000:]}")
    summary = final_json(result)
    require(summary.get("status") == "rendered", repr(summary))
    require(summary.get("document_pdf_status") == "rendered", repr(summary))
    return summary


def verify_fonts(summary: dict[str, Any]) -> bytes:
    fonts = read_json(Path(summary["fonts_artifact"]))
    require(fonts.get("schema") == "pliego.font-report", repr(fonts))
    require(fonts.get("version") == 1, repr(fonts))
    require(fonts.get("policy") == {"host_fonts": "denied"}, repr(fonts.get("policy")))
    selections = fonts.get("selections")
    require(isinstance(selections, list) and bool(selections), "no selected fonts")
    require(
        all(selection.get("source") == "bundled" for selection in selections),
        f"non-bundled selection escaped the default policy: {selections!r}",
    )
    require(
        any(
            selection.get("requested_families") == FALLBACK_CHAIN
            and str(selection.get("selected_family", "")).casefold() == "pliego fixture"
            for selection in selections
        ),
        f"fallback resolution order is absent: {selections!r}",
    )
    warnings = fonts.get("warnings")
    require(
        isinstance(warnings, list)
        and any(
            warning.get("code") == "FONT_FALLBACK_USED"
            and warning.get("requested_family") == FALLBACK_CHAIN[0]
            and str(warning.get("selected_family", "")).casefold() == "pliego fixture"
            and warning.get("fallback_chain") == FALLBACK_CHAIN
            for warning in warnings
        ),
        f"missing-family fallback did not warn: {warnings!r}",
    )
    manifest = fonts.get("manifest")
    require(isinstance(manifest, dict), "font report has no manifest")
    require(manifest.get("resolution") == "css-order", repr(manifest))
    entries = manifest.get("entries")
    require(isinstance(entries, list) and bool(entries), "bundled manifest is empty")
    require(
        all(entry.get("source") in {"bundled", "data", "memory"} for entry in entries),
        f"manifest contains a non-portable font: {entries!r}",
    )

    resources = fonts.get("font_resources")
    require(isinstance(resources, list) and bool(resources), "font resources are empty")
    for resource in resources:
        encoded = resource.get("bytes_base64")
        address = resource.get("resource")
        require(isinstance(encoded, str) and isinstance(address, str), repr(resource))
        decoded = base64.b64decode(encoded, validate=True)
        require(address == f"sha256:{hashlib.sha256(decoded).hexdigest()}", repr(resource))
    return Path(summary["fonts_artifact"]).read_bytes()


def verify_host_opt_in(summary: dict[str, Any]) -> None:
    fonts = read_json(Path(summary["fonts_artifact"]))
    require(fonts.get("policy") == {"host_fonts": "allowed"}, repr(fonts.get("policy")))
    selections = fonts.get("selections")
    require(isinstance(selections, list), "host font report has no selections")
    host = [selection for selection in selections if selection.get("source") == "host"]
    require(bool(host), f"opted-in host font was not reported: {selections!r}")
    resources = fonts.get("font_resources")
    require(isinstance(resources, list), "host font report has no resources")
    resource_ids = {resource.get("resource") for resource in resources}
    require(
        all(
            isinstance(selection.get("instance"), str)
            and isinstance(selection.get("resource"), str)
            and selection["resource"].startswith("sha256:")
            and selection["resource"] in resource_ids
            and isinstance(selection.get("requested_families"), list)
            and bool(selection["requested_families"])
            for selection in host
        ),
        f"host selections lack stable identities: {host!r}",
    )


def verify_pdf(summary: dict[str, Any]) -> bytes:
    pdf = Path(summary["document_pdf"]).read_bytes()
    require(pdf.startswith(b"%PDF-"), "document is not a PDF")
    require(b"/ToUnicode" in pdf, "PDF has no ToUnicode mapping")
    require(b"/FontFile2" in pdf or b"/FontFile3" in pdf, "PDF has no embedded font")
    structure = read_json(Path(summary["pdf_structure"]))
    pages = structure.get("pages")
    require(isinstance(pages, list), "PDF structure has no pages")
    extracted = "".join(str(page.get("expected_extracted_unicode", "")) for page in pages)
    require(EXPECTED_TEXT in extracted, f"PDF source Unicode differs: {extracted!r}")
    return pdf


def check(binary: Path) -> None:
    fixture = Path(__file__).resolve().parent / "fixtures/text-scene/font-resolution.html"
    host_fixture = fixture.with_name("index.html")
    with tempfile.TemporaryDirectory(prefix="pliego-font-resolution-") as temp:
        root = Path(temp)
        first = render(binary, fixture, root, "first")
        second = render(binary, fixture, root, "second")
        require(first["render_id"] == second["render_id"], "render IDs differ across clean homes")
        require(first["scene"]["hash"] == second["scene"]["hash"], "scene hashes differ across clean homes")
        require(verify_fonts(first) == verify_fonts(second), "font reports differ across clean homes")
        require(verify_pdf(first) == verify_pdf(second), "PDF bytes differ across clean homes")
        opted_in = render(binary, host_fixture, root, "host-opt-in", allow_host_fonts=True)
        verify_host_opt_in(opted_in)


def self_test() -> None:
    require(FALLBACK_CHAIN[0] != FALLBACK_CHAIN[1], "fallback fixture needs two families")
    sample = b"font"
    require(len(hashlib.sha256(sample).hexdigest()) == 64, "SHA-256 unavailable")


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("font resolution self-test: ok")
        return 0
    if len(arguments) != 1:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> | --self-test", 2)
    binary = Path(arguments[0]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    self_test()
    check(binary)
    print("font resolution check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
