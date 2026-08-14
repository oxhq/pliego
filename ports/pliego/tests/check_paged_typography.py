#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

PAGE_SIZE = "240x80"
PAGE_MARGINS = "10,10,10,10"
REFERENCE_SCHEMA = "pliego.paged-typography-reference"
SUPPORT_SCHEMA = "pliego.typography-support"
FONT_PLACEHOLDER = "__PLIEGO_FONT_BASE64__"
UNIX_SOCKET_PATH_BYTES = 108
WAYLAND_SOCKET_NAME = "wayland-0"


@dataclass(frozen=True)
class Case:
    fixture: str
    text: str
    family: str
    font: str
    font_sha256: str
    script: str
    license: str
    license_reference: str


CASES = {
    "latin": Case(
        "latin.html",
        "LATIN CONTROL LATIN CONTROL LATIN CONTROL LATIN CONTROL LATIN CONTROL LATIN CONTROL LATIN CONTROL LATIN CONTROL",
        "Ahem",
        "tests/wpt/tests/fonts/Ahem.ttf",
        "b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448",
        "Latin",
        "Public domain / CC0",
        "https://www.w3.org/Style/CSS/Test/Fonts/Ahem/",
    ),
    "rtl-adlam": Case(
        "rtl-adlam.html",
        "𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄 𞤀𞤁𞤂𞤁𞤄",
        "Noto Sans Adlam",
        "tests/wpt/tests/fonts/noto/NotoSansAdlam-hinted/NotoSansAdlam-Regular.ttf",
        "3253b5b6a0cb541f819832c2bb815d528622b38f143e1e718003a58db9d557ea",
        "Adlam (RTL)",
        "SIL Open Font License 1.1",
        "tests/wpt/tests/fonts/noto/NotoSansAdlam-hinted/LICENSE_OFL.txt",
    ),
    "complex-devanagari": Case(
        "complex-devanagari.html",
        "स्थलों स्वभाव क्लब क्रम क्षण ष्क्रस ङ्ङम ङ्क्रस स्थलों स्वभाव क्लब क्रम क्षण ष्क्रस ङ्ङम ङ्क्रस स्थलों स्वभाव क्लब क्रम क्षण ष्क्रस ङ्ङम ङ्क्रस",
        "Noto Sans Devanagari",
        "tests/wpt/tests/fonts/noto/NotoSansDevanagari-Regular.ttf",
        "b1dffa1fccb30dc45287111834a9db15c652b05d4d67201abe73e67717017590",
        "Devanagari",
        "SIL Open Font License 1.1",
        "https://github.com/notofonts/noto-fonts/blob/main/LICENSE",
    ),
}


def fail(message: str, code: int = 1) -> None:
    print(f"paged typography check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_object(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"artifact does not exist: {path}")
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"artifact is not an object: {path}")
    return value


def digest(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    value = json.loads(lines[-1])
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def canonical_text(value: str) -> str:
    return " ".join(value.replace("\f", " ").split())


def unix_socket_path_fits(path: Path) -> bool:
    return len(os.fsencode(path)) + 1 <= UNIX_SOCKET_PATH_BYTES


def materialize_fixture(repo: Path, case: Case, template: Path, output: Path) -> Path:
    asset = repo / case.font
    require(asset.is_file(), f"missing pinned font: {case.font}")
    font_bytes = asset.read_bytes()
    require(digest(font_bytes) == f"sha256:{case.font_sha256}", f"{case.font} digest drifted")
    source = template.read_text(encoding="utf-8")
    require(source.count(FONT_PLACEHOLDER) == 1, f"{case.fixture} font placeholder differs")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        source.replace(FONT_PLACEHOLDER, base64.b64encode(font_bytes).decode("ascii")),
        encoding="utf-8",
    )
    return output


def render(binary: Path, repo: Path, case: Case, fixture: Path, destination: Path) -> dict[str, Any]:
    destination.mkdir(parents=True)
    home = destination / "home"
    home.mkdir()
    output = destination / "document.pdf"
    artifacts = destination / "artifacts"
    short_runtime_root = "/tmp" if os.name == "posix" else None
    with tempfile.TemporaryDirectory(prefix="pliego-runtime-", dir=short_runtime_root) as runtime_name:
        runtime = Path(runtime_name)
        runtime.chmod(0o700)
        if os.name == "posix":
            require(
                unix_socket_path_fits(runtime / WAYLAND_SOCKET_NAME),
                f"XDG runtime socket path exceeds {UNIX_SOCKET_PATH_BYTES} bytes",
            )
        environment = os.environ.copy()
        environment.update(
            {
                "HOME": str(home),
                "XDG_CACHE_HOME": str(home / "cache"),
                "XDG_CONFIG_HOME": str(home / "config"),
                "XDG_DATA_HOME": str(home / "data"),
                "XDG_RUNTIME_DIR": str(runtime),
            }
        )
        result = subprocess.run(
            [
                str(binary),
                "render",
                fixture.name,
                "--output",
                str(output),
                "--artifacts",
                str(artifacts),
                "--page-size",
                PAGE_SIZE,
                "--page-margins",
                PAGE_MARGINS,
            ],
            cwd=fixture.parent,
            env=environment,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
    (destination / "process.stdout.log").write_text(result.stdout, encoding="utf-8")
    (destination / "process.stderr.log").write_text(result.stderr, encoding="utf-8")
    require(result.returncode == 0, f"{case.fixture} failed: {result.stderr[-3000:]}")
    summary = final_json(result)
    require(summary.get("status") == "rendered", repr(summary))
    readiness = summary.get("readiness")
    expected_fixture = f"paged-typography-{Path(case.fixture).stem}"
    require(
        isinstance(readiness, dict) and readiness.get("fixture") == expected_fixture,
        repr(summary),
    )
    require(summary.get("document_pdf_status") == "rendered", repr(summary))
    require(summary.get("scene", {}).get("text_mapping_gap_count") == 0, repr(summary.get("scene")))
    return summary


def text_operations(page: dict[str, Any]) -> list[dict[str, Any]]:
    operations = page.get("operations")
    require(isinstance(operations, list), "scene page has no operations")
    return [operation for operation in operations if operation.get("type") == "text"]


def validate_clusters(case_name: str, page_index: int, operation: dict[str, Any]) -> list[list[int]]:
    text = operation.get("text")
    glyphs = operation.get("glyphs")
    require(isinstance(text, str) and text, f"{case_name} page {page_index} has empty text")
    require(isinstance(glyphs, list) and glyphs, f"{case_name} page {page_index} has no glyphs")
    boundaries = {len(text[:index].encode()) for index in range(len(text) + 1)}
    ranges = []
    for glyph in glyphs:
        text_range = glyph.get("text_range")
        require(isinstance(text_range, dict), f"{case_name} has an unmapped glyph")
        pair = [text_range.get("start"), text_range.get("end")]
        require(all(isinstance(value, int) for value in pair), f"{case_name} has a non-integer range")
        require(
            pair[0] in boundaries and pair[1] in boundaries and pair[0] < pair[1],
            f"{case_name} has an invalid UTF-8 range {pair}",
        )
        ranges.append(pair)
    clusters = sorted({tuple(pair) for pair in ranges})
    require(clusters[0][0] == 0 and clusters[-1][1] == len(text.encode()), f"{case_name} lost edge text")
    require(
        all(left[1] == right[0] for left, right in zip(clusters, clusters[1:])),
        f"{case_name} has a lost or overlapping text range: {clusters!r}",
    )
    return [list(pair) for pair in clusters]


def geometry_reference(operations: list[dict[str, Any]]) -> dict[str, Any]:
    values = []
    xs = []
    ys = []
    advance = 0.0
    for operation in operations:
        for glyph in operation["glyphs"]:
            text_range = glyph["text_range"]
            row = [
                glyph["id"],
                glyph["x"],
                glyph["y"],
                glyph["advance"],
                text_range["start"],
                text_range["end"],
            ]
            values.append(row)
            xs.append(glyph["x"])
            ys.append(glyph["y"])
            advance += glyph["advance"]
    require(bool(values), "page has no glyph geometry")
    encoded = json.dumps(values, ensure_ascii=False, separators=(",", ":")).encode()
    return {
        "glyph_count": len(values),
        "min_x": min(xs),
        "max_x": max(xs),
        "min_y": min(ys),
        "max_y": max(ys),
        "advance_sum": advance,
        "sha256": digest(encoded),
    }


def verify_font(repo: Path, case_name: str, case: Case, summary: dict[str, Any]) -> str:
    asset = repo / case.font
    require(asset.is_file(), f"missing pinned font: {case.font}")
    if not case.license_reference.startswith("https://"):
        require((repo / case.license_reference).is_file(), f"missing font license: {case.license_reference}")
    require(digest(asset.read_bytes()) == f"sha256:{case.font_sha256}", f"{case.font} digest drifted")
    fonts = read_object(Path(summary["fonts_artifact"]))
    require(fonts.get("policy") == {"host_fonts": "denied"}, repr(fonts.get("policy")))
    selections = fonts.get("selections")
    require(isinstance(selections, list), f"{case_name} font selections are absent")
    selected = [
        selection
        for selection in selections
        if str(selection.get("selected_family", "")).casefold() == case.family.casefold()
    ]
    require(bool(selected), f"{case_name} did not select {case.family}: {selections!r}")
    require(
        all(
            selection.get("source") == "data" and selection.get("requested_families") == [case.family]
            for selection in selected
        ),
        f"{case_name} font identity differs: {selected!r}",
    )
    selected_resources = {selection.get("resource") for selection in selected}
    require(
        len(selected_resources) == 1
        and all(isinstance(resource, str) and resource.startswith("sha256:") for resource in selected_resources),
        f"{case_name} selected font resource differs: {selected_resources!r}",
    )
    expected_resource = selected_resources.pop()
    resources = fonts.get("font_resources")
    require(
        isinstance(resources, list)
        and any(
            resource.get("resource") == expected_resource
            and digest(base64.b64decode(resource.get("bytes_base64", ""), validate=True)) == expected_resource
            for resource in resources
        ),
        f"{case_name} font bytes are not retained",
    )
    return expected_resource


def extract_pdf_text(pdf: Path) -> str:
    tool = shutil.which("pdftotext")
    require(tool is not None, "pdftotext is required for the typography extraction gate")
    result = subprocess.run(
        [tool, "-enc", "UTF-8", "-raw", "-nopgbrk", str(pdf), "-"],
        capture_output=True,
        timeout=20,
        check=False,
    )
    require(result.returncode == 0, f"pdftotext failed: {result.stderr.decode(errors='replace')}")
    return result.stdout.decode("utf-8-sig")


def reference(repo: Path, case_name: str, case: Case, summary: dict[str, Any]) -> dict[str, Any]:
    scene = read_object(Path(summary["scene_artifact"]))
    pages = scene.get("pages")
    require(isinstance(pages, list) and len(pages) >= 2, f"{case_name} did not cross a page boundary")
    page_manifest = read_object(Path(summary["pages_artifact"]))
    preview_rows = page_manifest.get("pages")
    require(isinstance(preview_rows, list) and len(preview_rows) == len(pages), f"{case_name} previews differ")
    page_references = []
    logical_text = []
    for page_index, (page, preview) in enumerate(zip(pages, preview_rows)):
        operations = text_operations(page)
        require(bool(operations), f"{case_name} page {page_index} has no text")
        operation_text = [operation["text"] for operation in operations]
        logical_text.extend(operation_text)
        page_references.append(
            {
                "text": operation_text,
                "clusters": [validate_clusters(case_name, page_index, operation) for operation in operations],
                "geometry": geometry_reference(operations),
                "preview_sha256": digest((Path(summary["artifacts"]) / preview["artifact"]).read_bytes()),
            }
        )
    require("".join(logical_text) == case.text, f"{case_name} scene text was lost, duplicated, or reordered")

    structure = read_object(Path(summary["pdf_structure"]))
    pdf_pages = structure.get("pages")
    require(isinstance(pdf_pages, list) and len(pdf_pages) == len(pages), f"{case_name} PDF pages differ")
    expected_pdf_text = "".join(str(page.get("expected_extracted_unicode", "")) for page in pdf_pages)
    require(expected_pdf_text == case.text, f"{case_name} PDF source text differs")
    pdf = Path(summary["document_pdf"])
    pdf_bytes = pdf.read_bytes()
    require(b"/ToUnicode" in pdf_bytes, f"{case_name} PDF has no ToUnicode map")
    require(b"/FontFile2" in pdf_bytes or b"/FontFile3" in pdf_bytes, f"{case_name} PDF has no embedded font")
    extracted = extract_pdf_text(pdf)
    require(canonical_text(extracted) == canonical_text(case.text), f"{case_name} pdftotext differs: {extracted!r}")

    layout = read_object(Path(summary["layout_debug"]))
    continuations = layout.get("page_sequence", {}).get("continuations")
    require(
        isinstance(continuations, list) and len(continuations) == len(pages) - 1,
        f"{case_name} continuation count differs: {continuations!r}",
    )
    require(
        all(
            continuation.get("page_index") == index
            and continuation.get("token", {}).get("kind") == "inline"
            and continuation.get("token", {}).get("resume_page_index") == index + 1
            for index, continuation in enumerate(continuations)
        ),
        f"{case_name} has an invalid inline continuation: {continuations!r}",
    )
    font_resource = verify_font(repo, case_name, case, summary)
    return {
        "page_count": len(pages),
        "font_resource": font_resource,
        "pages": page_references,
        "continuations": continuations,
        "scene_sha256": digest(Path(summary["scene_artifact"]).read_bytes()),
        "pdf_sha256": digest(pdf_bytes),
        "extracted_text_sha256": digest(canonical_text(extracted).encode()),
    }


def verify_repeat(case_name: str, first: dict[str, Any], second: dict[str, Any]) -> None:
    for field in ("render_id",):
        require(first.get(field) == second.get(field), f"{case_name} {field} is nondeterministic")
    for field in ("scene_artifact", "fonts_artifact"):
        require(
            Path(first[field]).read_bytes() == Path(second[field]).read_bytes(),
            f"{case_name} {Path(field).name} is nondeterministic",
        )
    require(
        Path(first["document_pdf"]).read_bytes() == Path(second["document_pdf"]).read_bytes(),
        f"{case_name} PDF is nondeterministic",
    )


def support_state(references: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema": SUPPORT_SCHEMA,
        "version": 1,
        "cases": {
            name: {
                "state": "supported",
                "script": CASES[name].script,
                "font": CASES[name].font,
                "font_resource": value["font_resource"],
                "font_license": CASES[name].license,
                "font_license_reference": CASES[name].license_reference,
                "page_count": value["page_count"],
            }
            for name, value in references.items()
        },
        "upstream_limits": [
            {
                "feature": "mixed-direction bidi continuation",
                "state": "blocked upstream",
                "scope": "This gate proves a single-direction RTL paragraph only.",
                "evidence": "Servo's retained line-source handoff requires visually collected UTF-8 ranges to remain monotonically increasing; mixed bidi reordering can violate that precondition.",
            },
            {
                "feature": "FontFaceSet readiness",
                "state": "blocked upstream",
                "scope": "Fixtures load repository-pinned bytes through a generated CSS data URL, require the captured SHA-256 font identity, then wait two animation frames.",
                "evidence": "The pinned Servo build does not implement document.fonts.load; relative @font-face URLs under file origins fail before selection, while data URLs settle through normal style and layout.",
            },
        ],
    }


def run(binary: Path, output: Path, *, record: bool) -> None:
    repo = Path(__file__).resolve().parents[3]
    fixture_dir = Path(__file__).resolve().parent / "fixtures/paged-typography"
    expected_path = fixture_dir / "expected.json"
    expected = read_object(expected_path)
    require(expected.get("schema") == REFERENCE_SCHEMA and expected.get("version") == 1, repr(expected))
    require(not output.exists(), f"output directory already exists: {output}")
    output.mkdir(parents=True)
    references = {}
    for name, case in CASES.items():
        fixture = materialize_fixture(repo, case, fixture_dir / case.fixture, output / name / "input.html")
        first = render(binary, repo, case, fixture, output / name / "first")
        second = render(binary, repo, case, fixture, output / name / "second")
        verify_repeat(name, first, second)
        first_reference = reference(repo, name, case, first)
        require(first_reference == reference(repo, name, case, second), f"{name} reference is nondeterministic")
        references[name] = first_reference
    (output / "support-state.json").write_text(
        json.dumps(support_state(references), ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    if record:
        expected_path.write_text(
            json.dumps(
                {"schema": REFERENCE_SCHEMA, "version": 1, "cases": references},
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
    else:
        require(expected.get("cases") == references, "fixed page, visual, glyph, or cluster reference differs")


def self_test() -> None:
    require(set(CASES) == {"latin", "rtl-adlam", "complex-devanagari"}, "fixture matrix differs")
    repo = Path(__file__).resolve().parents[3]
    fixtures = Path(__file__).resolve().parent / "fixtures/paged-typography"
    for case in CASES.values():
        require(
            digest((repo / case.font).read_bytes()) == f"sha256:{case.font_sha256}",
            f"{case.font} digest drifted",
        )
        require(
            (fixtures / case.fixture).read_text(encoding="utf-8").count(FONT_PLACEHOLDER) == 1,
            f"{case.fixture} font placeholder differs",
        )
    sample = "क्ष"
    require(len(sample.encode()) == 9 and canonical_text("a\n b\f") == "a b", "Unicode helpers differ")
    require(
        unix_socket_path_fits(Path("/tmp/pliego-runtime-12345678") / WAYLAND_SOCKET_NAME)
        and not unix_socket_path_fits(Path("/") / ("x" * (UNIX_SOCKET_PATH_BYTES - 1))),
        "Unix socket path boundary differs",
    )
    require(
        support_state({name: {"font_resource": "sha256:x", "page_count": 2} for name in CASES})["upstream_limits"][0][
            "state"
        ]
        == "blocked upstream",
        "upstream limit state differs",
    )


def main() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        self_test()
        print("paged typography self-test: ok")
        return 0
    record = len(arguments) == 3 and arguments[0] == "--record"
    if (record and len(arguments) != 3) or (not record and len(arguments) != 2):
        fail(
            f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory> | --record <pliego-binary> <output-directory> | --self-test",
            2,
        )
    binary = Path(arguments[-2]).expanduser().resolve()
    output = Path(arguments[-1]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    self_test()
    run(binary, output, record=record)
    print(f"paged typography check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
