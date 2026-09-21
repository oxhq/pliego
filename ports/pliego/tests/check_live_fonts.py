#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import threading
from collections.abc import Callable
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

FONT_SHA256 = "62a9934d08c576ce59cdfda6e1b31746e5785f58562b3fd545df791075e5796c"
PROCESS_TIMEOUT_SECONDS = 60
UNIX_SOCKET_PATH_BYTES = 108
WAYLAND_SOCKET_NAME = "wayland-0"


def fail(message: str, code: int = 1) -> None:
    print(f"live font check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_object(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"missing JSON artifact: {path}")
    value = json.loads(path.read_bytes())
    require(isinstance(value, dict), f"JSON artifact is not an object: {path}")
    return value


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, path: str, content_type: str, body: Callable[[], bytes]) -> None:
        super().__init__(("127.0.0.1", 0), FixtureHandler)
        self.fixture_path = path
        self.content_type = content_type
        self.body = body
        self.request_count = 0

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.server_port}/"


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        server = self.server
        assert isinstance(server, FixtureServer)
        if self.path != server.fixture_path:
            self.send_error(404)
            return
        server.request_count += 1
        body = server.body()
        self.send_response(200)
        self.send_header("Content-Type", server.content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_args: object) -> None:
        pass


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    value = json.loads(lines[-1])
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def unix_socket_path_fits(path: Path) -> bool:
    return len(os.fsencode(path)) + 1 <= UNIX_SOCKET_PATH_BYTES


def run(
    binary: Path,
    root: Path,
    document: str,
    roots: tuple[str, ...],
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    root.mkdir()
    home = root / "home"
    home.mkdir()
    (root / "document.html").write_text(document, encoding="utf-8")
    command = [
        str(binary),
        "render",
        "document.html",
        "--output",
        str(root / "document.pdf"),
        "--artifacts",
        str(root / "artifacts"),
        "--page-size",
        "320x240",
        "--page-margins",
        "8,8,8,8",
    ]
    for allowed_root in roots:
        command.extend(("--allow-http-root", allowed_root))
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
                "NO_PROXY": "127.0.0.1,localhost",
                "XDG_CACHE_HOME": str(home / "cache"),
                "XDG_CONFIG_HOME": str(home / "config"),
                "XDG_DATA_HOME": str(home / "data"),
                "XDG_RUNTIME_DIR": str(runtime),
                "no_proxy": "127.0.0.1,localhost",
            }
        )
        result = subprocess.run(
            command,
            cwd=root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=PROCESS_TIMEOUT_SECONDS,
            check=False,
        )
    (root / "process.stdout.log").write_text(result.stdout, encoding="utf-8")
    (root / "process.stderr.log").write_text(result.stderr, encoding="utf-8")
    summary = final_json(result)
    (root / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return result, summary


def render(binary: Path, root: Path, document: str, roots: tuple[str, str]) -> dict[str, Any]:
    result, summary = run(binary, root, document, roots)
    require(result.returncode == 0, f"render failed: {result.stderr[-2000:]}")
    require(summary.get("status") == "rendered", repr(summary))
    return summary


def loaded_resource(artifacts: Path, url: str, expected: bytes, content_type: str) -> None:
    rows = [json.loads(line) for line in (artifacts / "resources.jsonl").read_text().splitlines()]
    matches = [row for row in rows if row.get("status") == "loaded" and row.get("url") == url]
    require(bool(matches), f"resource log omitted {url}: {rows!r}")
    digest = hashlib.sha256(expected).hexdigest()
    row = matches[-1]
    require(row.get("sha256") == digest, f"wrong retained hash for {url}: {row!r}")
    require(row.get("bytes") == len(expected), f"wrong retained size for {url}: {row!r}")
    require(str(row.get("content_type", "")).startswith(content_type), f"wrong MIME for {url}: {row!r}")
    artifact = row.get("artifact")
    require(isinstance(artifact, str), f"resource has no retained artifact: {row!r}")
    require((artifacts / artifact).read_bytes() == expected, f"retained bytes differ for {url}")


def verify(summary: dict[str, Any], css_url: str, css: bytes, font_url: str, font: bytes) -> None:
    artifacts = Path(str(summary["artifacts"]))
    resolved = summary.get("resolved_input_hash")
    require(isinstance(resolved, str) and resolved.startswith("sha256:"), repr(resolved))
    environment = read_object(artifacts / "environment.json")
    require(environment.get("resolved_input_hash") == resolved, repr(environment))
    readiness = read_object(artifacts / "readiness.json")
    require(readiness.get("status") == "ready", repr(readiness))
    require(readiness.get("font_status") == "loaded", repr(readiness))
    payload = readiness.get("payload")
    require(isinstance(payload, dict) and payload.get("fixture") == "live-remote-font", repr(readiness))
    loaded_resource(artifacts, css_url, css, "text/css")
    loaded_resource(artifacts, font_url, font, "font/woff2")

    fonts = read_object(artifacts / "fonts.json")
    selections = fonts.get("selections")
    require(
        isinstance(selections, list) and any(selection.get("source") == "remote" for selection in selections),
        f"remote font was not selected: {selections!r}",
    )
    pdf = Path(str(summary["document_pdf"])).read_bytes()
    require(pdf.startswith(b"%PDF-") and (b"/FontFile2" in pdf or b"/FontFile3" in pdf), "PDF omitted font")


def verify_denied(
    result: subprocess.CompletedProcess[str],
    summary: dict[str, Any],
    root: Path,
    font_url: str,
) -> None:
    require(result.returncode == 1 and summary.get("status") == "failed", repr(summary))
    error = summary.get("error")
    require(isinstance(error, dict) and error.get("code") == "RESOURCE_DENIED", repr(error))
    rows = [json.loads(line) for line in (root / "artifacts/resources.jsonl").read_text().splitlines()]
    require(
        any(row.get("url") == font_url and row.get("code") == "RESOURCE_DENIED" for row in rows),
        f"font-origin denial was not retained: {rows!r}",
    )
    require(not (root / "document.pdf").exists(), "denied render published a PDF")
    require(not (root / "artifacts/document.pdf").exists(), "denied render retained a diagnostic PDF")


def main() -> int:
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)
    binary = Path(sys.argv[1]).expanduser().resolve()
    output = Path(sys.argv[2]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(not output.exists(), f"output directory already exists: {output}")
    output.mkdir(parents=True)

    repository = Path(__file__).resolve().parents[3]
    font = (repository / "tests/wpt/tests/css/WOFF2/support/valid-005.woff2").read_bytes()
    require(hashlib.sha256(font).hexdigest() == FONT_SHA256, "pinned WOFF2 fixture changed")
    css_suffix = [b""]
    font_server = FixtureServer("/fixture.woff2", "font/woff2", lambda: font)
    css_server = FixtureServer(
        "/family.css",
        "text/css; charset=utf-8",
        lambda: (
            (
                '@font-face { font-family: "Pliego Remote"; '
                f'src: url("{font_server.base_url}fixture.woff2") format("woff2"); }}\n'
                '.sample { font: 200px "Pliego Remote"; }\n'
            ).encode()
            + css_suffix[0]
        ),
    )
    servers = (css_server, font_server)
    threads = [threading.Thread(target=server.serve_forever, daemon=True) for server in servers]
    for thread in threads:
        thread.start()
    try:
        css_url = f"{css_server.base_url}family.css"
        font_url = f"{font_server.base_url}fixture.woff2"
        document = f"""<!doctype html>
<meta charset="utf-8">
<link rel="stylesheet" href="{css_url}">
<div class="sample">P</div>
<script>
window.pliego.defer();
document.fonts.ready.then(() => window.pliego.ready({{fixture: "live-remote-font"}}));
</script>
"""
        first_css = css_server.body()
        first = render(binary, output / "first", document, (css_server.base_url, font_server.base_url))
        verify(first, css_url, first_css, font_url, font)
        second = render(binary, output / "second", document, (css_server.base_url, font_server.base_url))
        verify(second, css_url, first_css, font_url, font)
        require(first["render_id"] == second["render_id"], "unchanged prefetch render IDs differ")
        require(first["resolved_input_hash"] == second["resolved_input_hash"], "unchanged live inputs differ")

        css_suffix[0] = b"/* changed remote bytes */\n"
        changed_css = css_server.body()
        changed = render(binary, output / "changed", document, (css_server.base_url, font_server.base_url))
        verify(changed, css_url, changed_css, font_url, font)
        require(first["render_id"] == changed["render_id"], "remote bytes changed the prefetch render ID")
        require(first["resolved_input_hash"] != changed["resolved_input_hash"], "remote bytes did not change identity")
        require(css_server.request_count >= 3 and font_server.request_count >= 3, "both live origins were not fetched")

        css_requests = css_server.request_count
        font_requests = font_server.request_count
        denied_result, denied = run(binary, output / "denied-font-origin", document, (css_server.base_url,))
        verify_denied(denied_result, denied, output / "denied-font-origin", font_url)
        require(css_server.request_count > css_requests, "allowed stylesheet origin was not fetched")
        require(font_server.request_count == font_requests, "denied font request reached its origin")
    finally:
        for server in servers:
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=5)

    print("live font check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
