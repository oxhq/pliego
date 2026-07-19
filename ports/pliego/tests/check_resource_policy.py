#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import hashlib
import html
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Iterator

POLICY = "pliego.resource-policy.v1"
RESOURCE_TIMEOUT_MS = 1_000
PROCESS_TIMEOUT_SECONDS = 30
LOCAL_BODY = 'window.localLoaded = "LOCAL_RESOURCE_BODY_SECRET";\n'
VIRTUAL_BODY = 'window.virtualLoaded = "VIRTUAL_RESOURCE_BODY_SECRET";\n'
HTTP_BODY = b'window.httpLoaded = "HTTP_RESOURCE_BODY_SECRET";\n'
LEAK_MARKERS = (
    "LOCAL_RESOURCE_BODY_SECRET",
    "VIRTUAL_RESOURCE_BODY_SECRET",
    "HTTP_RESOURCE_BODY_SECRET",
)


def fail(message: str, code: int = 1) -> None:
    print(f"resource policy check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def read_object(path: Path) -> dict[str, Any]:
    require(path.is_file(), f"artifact does not exist: {path}")
    try:
        value = json.loads(path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read JSON artifact {path}: {error}")
    require(isinstance(value, dict), f"JSON artifact is not an object: {path}")
    return value


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def resource_rows(artifacts: Path, render_id: str) -> list[dict[str, Any]]:
    path = artifacts / "resources.jsonl"
    require(path.is_file(), f"resource log does not exist: {path}")
    raw = path.read_text(encoding="utf-8")
    for marker in LEAK_MARKERS:
        require(marker not in raw, f"resources.jsonl leaked resource body marker {marker}")
    rows = []
    for line_number, line in enumerate(raw.splitlines(), 1):
        try:
            row = json.loads(line)
        except json.JSONDecodeError as error:
            fail(f"invalid resources.jsonl line {line_number}: {error}")
        require(isinstance(row, dict), f"resource log line {line_number} is not an object")
        require(row.get("policy") == POLICY, f"resource log omitted policy: {row!r}")
        require(row.get("render_id") == render_id, f"resource log omitted render_id: {row!r}")
        require("body" not in row and "source" not in row, f"resource log leaked source fields: {row!r}")
        rows.append(row)
    require(bool(rows), "resources.jsonl is empty")
    return rows


def document(fixture: str, scripts: list[str], checks: dict[str, str] | None = None) -> str:
    tags = "\n".join(f'<script src="{html.escape(url, quote=True)}"></script>' for url in scripts)
    payload = {"fixture": json.dumps(fixture)}
    payload.update({name: f"Boolean({expression})" for name, expression in (checks or {}).items()})
    ready = ",\n      ".join(f"{json.dumps(name)}: {value}" for name, value in payload.items())
    return f"""<!doctype html>
<meta charset="utf-8">
<title>{html.escape(fixture)}</title>
<style>html, body {{ margin: 0 }} body {{ font: 16px sans-serif }}</style>
<p>{html.escape(fixture)}</p>
{tags}
<script>
  addEventListener('load', () => window.pliego?.ready({{
      {ready}
  }}));
</script>
"""


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self) -> None:
        super().__init__(("127.0.0.1", 0), FixtureHandler)
        self.requests: list[str] = []
        self.requests_lock = threading.Lock()
        self.stall_started = threading.Event()
        self.stall_started_at: float | None = None
        self.release_stall = threading.Event()

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.server_port}/"

    def record(self, path: str) -> None:
        with self.requests_lock:
            self.requests.append(path)

    def count(self, path: str) -> int:
        with self.requests_lock:
            return self.requests.count(path)


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        server = self.server
        assert isinstance(server, FixtureServer)
        server.record(self.path)
        if self.path == "/ok.js":
            self.respond(200, HTTP_BODY, "text/javascript")
        elif self.path == "/redirect.js":
            self.send_response(302)
            self.send_header("Location", "/ok.js")
            self.send_header("Content-Length", "0")
            self.end_headers()
        elif self.path == "/missing.js":
            self.respond(404, b"not found\n", "text/plain")
        elif self.path == "/timeout.js":
            self.respond(408, b"request timeout\n", "text/plain")
        elif self.path == "/stall.js":
            server.stall_started_at = time.monotonic()
            server.stall_started.set()
            server.release_stall.wait(PROCESS_TIMEOUT_SECONDS + 5)
            self.close_connection = True
        else:
            self.respond(404, b"unknown fixture\n", "text/plain")

    def respond(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, _format: str, *_args: object) -> None:
        pass


@contextmanager
def fixture_server() -> Iterator[FixtureServer]:
    server = FixtureServer()
    thread = threading.Thread(target=server.serve_forever, name="pliego-resource-server", daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.release_stall.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def retain(root: Path, result: subprocess.CompletedProcess[str], destination: Path) -> None:
    destination.mkdir(parents=True)
    (destination / "process.stdout.log").write_text(result.stdout, encoding="utf-8")
    (destination / "process.stderr.log").write_text(result.stderr, encoding="utf-8")
    inputs = destination / "inputs"
    inputs.mkdir()
    for source in sorted((*root.glob("*.html"), *root.glob("*.js"))):
        shutil.copy2(source, inputs / source.name)
    if (root / "artifacts").is_dir():
        shutil.copytree(root / "artifacts", destination / "artifacts")
    if (root / "document.pdf").is_file():
        shutil.copy2(root / "document.pdf", destination / "document.pdf")


def run(
    binary: Path,
    root: Path,
    destination: Path,
    scripts: list[str],
    *,
    fixture: str,
    checks: dict[str, str] | None = None,
    options: tuple[str, ...] = (),
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    (root / "document.html").write_text(document(fixture, scripts, checks), encoding="utf-8")
    command = [
        str(binary),
        "render",
        "document.html",
        "--output",
        str(root / "document.pdf"),
        "--artifacts",
        str(root / "artifacts"),
        "--allow-host-fonts",
        "--page-size",
        "240x160",
        "--page-margins",
        "8,8,8,8",
        "--resource-timeout-ms",
        str(RESOURCE_TIMEOUT_MS),
        *options,
    ]
    environment = os.environ.copy()
    environment.update({"NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost"})
    try:
        result = subprocess.run(
            command,
            cwd=root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=PROCESS_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else (error.stdout or "")
        stderr = error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else (error.stderr or "")
        result = subprocess.CompletedProcess(command, 124, stdout, stderr)
        retain(root, result, destination)
        (destination / "process-timeout.json").write_text(
            json.dumps({"case": fixture, "timeout_seconds": PROCESS_TIMEOUT_SECONDS}, indent=2) + "\n",
            encoding="utf-8",
        )
        fail(
            f"{fixture} exceeded {PROCESS_TIMEOUT_SECONDS}s; retained evidence at {destination} "
            "(possible Servo post-Allow cancellation gap)"
        )
    retain(root, result, destination)
    summary = final_json(result)
    (destination / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return result, summary


def verify_policy(summary: dict[str, Any], artifacts: Path, *, network: str) -> dict[str, Any]:
    render_id = summary.get("render_id")
    require(isinstance(render_id, str) and render_id.startswith("sha256:"), repr(summary))
    environment = read_object(artifacts / "environment.json")
    policy = environment.get("resource_policy")
    require(isinstance(policy, dict), f"environment has no resource policy: {environment!r}")
    require(policy.get("schema") == POLICY and policy.get("version") == 1, repr(policy))
    require(policy.get("render_id") == render_id, repr(policy))
    require(policy.get("network") == network, repr(policy))
    require(policy.get("filesystem") == "document-root", repr(policy))
    require(policy.get("data_urls") == "allow", repr(policy))
    require(policy.get("redirects") == "deny", repr(policy))
    require(policy.get("timeout_ms") == RESOURCE_TIMEOUT_MS, repr(policy))
    return policy


def verify_failure(
    result: subprocess.CompletedProcess[str],
    summary: dict[str, Any],
    root: Path,
    expected_code: str,
    *,
    network: str,
) -> list[dict[str, Any]]:
    require(result.returncode == 1, f"failure exited with {result.returncode}: {result.stderr[-2000:]}")
    require(summary.get("status") == "failed", repr(summary))
    error = summary.get("error")
    require(isinstance(error, dict) and error.get("code") == expected_code, repr(error))
    failure = read_object(root / "artifacts/failure.json")
    terminal_failure = {key: summary.get(key) for key in ("status", "render_id", "error")}
    require(
        failure == terminal_failure, f"failure.json differs from terminal JSON: {failure!r} != {terminal_failure!r}"
    )
    verify_policy(summary, root / "artifacts", network=network)
    rows = resource_rows(root / "artifacts", str(summary["render_id"]))
    expected_status = {
        "RESOURCE_DENIED": "denied",
        "RESOURCE_NOT_FOUND": "not_found",
        "RESOURCE_TIMEOUT": "timeout",
    }[expected_code]
    require(
        any(row.get("code") == expected_code and row.get("status") == expected_status for row in rows),
        f"resource log omitted {expected_code}/{expected_status}: {rows!r}",
    )
    require(not (root / "document.pdf").exists(), "failed render published the requested PDF")
    require(not (root / "artifacts/document.pdf").exists(), "failed render retained a diagnostic PDF")
    return rows


def verify_healthy(
    result: subprocess.CompletedProcess[str],
    summary: dict[str, Any],
    root: Path,
    expected_readiness: dict[str, Any],
    *,
    network: str,
) -> dict[str, Any]:
    require(result.returncode == 0, f"healthy render exited with {result.returncode}: {result.stderr[-2000:]}")
    require(summary.get("status") == "rendered", repr(summary))
    require(summary.get("readiness") == expected_readiness, repr(summary.get("readiness")))
    require((root / "document.pdf").read_bytes().startswith(b"%PDF-"), "healthy PDF is invalid")
    require(not (root / "artifacts/failure.json").exists(), "healthy render wrote failure.json")
    policy = verify_policy(summary, root / "artifacts", network=network)
    resource_rows(root / "artifacts", str(summary["render_id"]))
    return policy


def self_test() -> None:
    fixture = document("escape", ['https://example.test/a.js?x="&y=<'], {"loaded": "window.loaded === true"})
    require('src="https://example.test/a.js?x=&quot;&amp;y=&lt;"' in fixture, "fixture URL is not escaped")
    require('"fixture": "escape"' in fixture, "fixture payload is malformed")
    with fixture_server() as server:
        for path, status, body in (
            ("/ok.js", 200, HTTP_BODY),
            ("/redirect.js", 302, b""),
            ("/missing.js", 404, b"not found\n"),
            ("/timeout.js", 408, b"request timeout\n"),
        ):
            connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=2)
            connection.request("GET", path)
            response = connection.getresponse()
            require(response.status == status, f"{path} returned {response.status}")
            require(response.read() == body, f"{path} returned an unexpected body")
            connection.close()
        connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=0.1)
        connection.request("GET", "/stall.js")
        require(server.stall_started.wait(1), "stall endpoint was not accepted")
        try:
            connection.getresponse()
        except (TimeoutError, socket.timeout):
            pass
        else:
            fail("stall endpoint returned a response")
        finally:
            connection.close()


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        print("resource policy checker self-test: ok")
        return 0
    if len(sys.argv) != 3:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary> <output-directory>", 2)

    binary = Path(sys.argv[1]).expanduser().resolve()
    output = Path(sys.argv[2]).expanduser().resolve()
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(not output.exists(), f"output directory already exists: {output}")
    output.mkdir(parents=True)

    with tempfile.TemporaryDirectory(prefix="pliego-resource-policy-") as temp, fixture_server() as server:
        temp_root = Path(temp)
        allow_http = ("--allow-http-root", server.base_url)

        healthy_root = temp_root / "allowed"
        healthy_root.mkdir()
        (healthy_root / "local.js").write_text(LOCAL_BODY, encoding="utf-8")
        (healthy_root / "virtual.js").write_text(VIRTUAL_BODY, encoding="utf-8")
        virtual_url = "pliego://host/virtual.js"
        result, summary = run(
            binary,
            healthy_root,
            output / "allowed",
            ["local.js", virtual_url, f"{server.base_url}ok.js"],
            fixture="allowed",
            checks={
                "local": 'window.localLoaded === "LOCAL_RESOURCE_BODY_SECRET"',
                "virtual": 'window.virtualLoaded === "VIRTUAL_RESOURCE_BODY_SECRET"',
                "http": 'window.httpLoaded === "HTTP_RESOURCE_BODY_SECRET"',
            },
            options=(*allow_http, "--virtual-resource", f"{virtual_url}=virtual.js"),
        )
        policy = verify_healthy(
            result,
            summary,
            healthy_root,
            {"fixture": "allowed", "local": True, "virtual": True, "http": True},
            network="configured-roots",
        )
        require(policy.get("http_roots") == [server.base_url], repr(policy))
        virtual = policy.get("virtual_resources")
        require(isinstance(virtual, list) and len(virtual) == 1, repr(policy))
        require(virtual[0].get("url") == virtual_url and virtual[0].get("available") is True, repr(virtual))
        require(virtual[0].get("bytes") == len(VIRTUAL_BODY.encode()), repr(virtual))
        require(virtual[0].get("sha256") == hashlib.sha256(VIRTUAL_BODY.encode()).hexdigest(), repr(virtual))
        require(server.count("/ok.js") == 1, "allowed HTTP resource did not reach the server exactly once")

        outside = temp_root / "outside.js"
        outside.write_text('window.outsideLoaded = "must not execute";\n', encoding="utf-8")
        failure_cases = (
            ("traversal", ["../outside.js"], (), "RESOURCE_DENIED", "deny"),
            ("denied-network", [f"{server.base_url}ok.js"], (), "RESOURCE_DENIED", "deny"),
            ("redirect", [f"{server.base_url}redirect.js"], allow_http, "RESOURCE_DENIED", "configured-roots"),
            ("missing-local", ["missing.js"], (), "RESOURCE_NOT_FOUND", "deny"),
            (
                "missing-virtual",
                ["pliego://host/missing.js"],
                ("--virtual-resource", "pliego://host/missing.js=missing.js"),
                "RESOURCE_NOT_FOUND",
                "deny",
            ),
            ("missing-http", [f"{server.base_url}missing.js"], allow_http, "RESOURCE_NOT_FOUND", "configured-roots"),
            ("http-408", [f"{server.base_url}timeout.js"], allow_http, "RESOURCE_TIMEOUT", "configured-roots"),
            ("http-stall", [f"{server.base_url}stall.js"], allow_http, "RESOURCE_TIMEOUT", "configured-roots"),
        )
        for name, scripts, options, expected_code, network in failure_cases:
            root = temp_root / name
            root.mkdir()
            before_ok = server.count("/ok.js")
            requested_path = {
                "redirect": "/redirect.js",
                "missing-http": "/missing.js",
                "http-408": "/timeout.js",
                "http-stall": "/stall.js",
            }.get(name)
            before_requested = server.count(requested_path) if requested_path else 0
            result, summary = run(
                binary,
                root,
                output / name,
                scripts,
                fixture=name,
                options=options,
            )
            rows = verify_failure(result, summary, root, expected_code, network=network)
            if name == "denied-network":
                require(server.count("/ok.js") == before_ok, "denied network request reached the server")
            if requested_path:
                require(
                    server.count(requested_path) == before_requested + 1,
                    f"{name} did not reach {requested_path} exactly once",
                )
            if name == "redirect":
                require(server.count("/ok.js") == before_ok, "denied redirect reached its target")
                require(
                    any(row.get("code") == "RESOURCE_DENIED" and row.get("is_redirect") is True for row in rows),
                    f"redirect denial was not identified as a redirect: {rows!r}",
                )
            if name == "http-stall":
                require(server.stall_started_at is not None, "stalled response was not accepted")
                observed_ms = round((time.monotonic() - server.stall_started_at) * 1_000)
                require(
                    observed_ms <= RESOURCE_TIMEOUT_MS + 5_000,
                    f"stalled response exceeded its bounded deadline: {observed_ms}ms",
                )
                (output / name / "timeout-evidence.json").write_text(
                    json.dumps(
                        {"configured_timeout_ms": RESOURCE_TIMEOUT_MS, "observed_completion_ms": observed_ms},
                        indent=2,
                    )
                    + "\n",
                    encoding="utf-8",
                )

        recovery_root = temp_root / "recovery"
        recovery_root.mkdir()
        (recovery_root / "local.js").write_text(LOCAL_BODY, encoding="utf-8")
        result, summary = run(
            binary,
            recovery_root,
            output / "recovery",
            ["local.js"],
            fixture="recovery",
            checks={"local": 'window.localLoaded === "LOCAL_RESOURCE_BODY_SECRET"'},
        )
        verify_healthy(
            result,
            summary,
            recovery_root,
            {"fixture": "recovery", "local": True},
            network="deny",
        )
        (output / "server-requests.json").write_text(json.dumps(server.requests, indent=2) + "\n", encoding="utf-8")

    print(f"resource policy check: ok (artifacts: {output})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
