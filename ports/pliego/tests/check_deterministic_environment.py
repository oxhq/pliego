#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def fail(message: str, code: int = 1) -> None:
    print(f"deterministic environment check: {message}", file=sys.stderr)
    raise SystemExit(code)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def final_json(result: subprocess.CompletedProcess[str]) -> dict[str, object]:
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    require(bool(lines), "Pliego produced no stdout JSON")
    try:
        value = json.loads(lines[-1])
    except json.JSONDecodeError as error:
        fail(f"final stdout line is not JSON: {error}: {lines[-1]!r}")
    require(isinstance(value, dict), "final stdout JSON is not an object")
    return value


def run(
    binary: Path,
    fixture: Path,
    temp_root: Path,
    *,
    host_locale: str,
    host_timezone: str,
    requested_locale: str | None = None,
    requested_timezone: str | None = None,
) -> dict[str, object]:
    environment = os.environ.copy()
    environment.update(
        {
            "LANG": host_locale,
            "LC_ALL": host_locale,
            "TZ": host_timezone,
            "TMPDIR": str(temp_root),
            "TMP": str(temp_root),
            "TEMP": str(temp_root),
        }
    )
    command = [str(binary), "--allow-host-fonts"]
    if requested_locale is not None:
        command.extend(["--locale", requested_locale])
    if requested_timezone is not None:
        command.extend(["--timezone", requested_timezone])
    command.append(fixture.name)
    result = subprocess.run(
        command,
        cwd=fixture.parent,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no process output"
        fail(f"Pliego exited with {result.returncode}: {detail[-2000:]}")
    return final_json(result)


def payload(summary: dict[str, object]) -> dict[str, object]:
    value = summary.get("readiness")
    require(isinstance(value, dict), "render summary has no readiness payload")
    return value


def scene_hash(summary: dict[str, object]) -> str:
    scene = summary.get("scene")
    require(isinstance(scene, dict), "render summary has no scene result")
    value = scene.get("hash")
    require(
        isinstance(value, str) and value.startswith("sha256:"),
        f"render summary has no content-addressed scene hash: {scene!r}",
    )
    return value


def document_pdf(summary: dict[str, object]) -> bytes:
    require(
        summary.get("document_pdf_status") == "rendered",
        f"render summary PDF is not rendered: {summary!r}",
    )
    value = summary.get("document_pdf")
    require(isinstance(value, str) and bool(value), "render summary has no PDF artifact")
    path = Path(value)
    require(path.is_file(), f"PDF artifact does not exist: {path}")
    pdf = path.read_bytes()
    require(pdf.startswith(b"%PDF-"), f"PDF artifact is invalid: {path}")
    return pdf


def verify_environment_artifact(summary: dict[str, object], requested_locale: str, requested_timezone: str) -> None:
    value = summary.get("environment_artifact")
    require(isinstance(value, str) and bool(value), "summary has no environment artifact")
    path = Path(value)
    require(path.is_file(), f"environment artifact does not exist: {path}")
    try:
        artifact = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read environment artifact {path}: {error}")
    require(
        artifact
        == {
            "locale": {
                "requested": requested_locale,
                "resolved": requested_locale,
            },
            "timezone": {
                "requested": requested_timezone,
                "resolved": requested_timezone,
            },
            "page": {
                "size_css_px": {
                    "width": 793.7000122070312,
                    "height": 1122.5167236328125,
                },
                "margins_css_px": {
                    "top": 45.349998474121094,
                    "right": 60.46666717529297,
                    "bottom": 45.349998474121094,
                    "left": 60.46666717529297,
                },
            },
            "fonts": {
                "host_fonts": "allowed",
            },
            "resource_policy": {
                "schema": "pliego.resource-policy.v1",
                "version": 1,
                "render_id": summary.get("render_id"),
                "network": "deny",
                "http_roots": [],
                "filesystem": "document-root",
                "data_urls": "allow",
                "redirects": "deny",
                "timeout_ms": 10000,
                "virtual_resources": [],
            },
            "document_pdf": {
                "artifact": summary.get("document_pdf"),
                "status": "rendered",
                "error": None,
            },
        },
        f"unexpected resolved environment artifact: {artifact!r}",
    )


def verify_invalid_request(binary: Path, fixture: Path) -> None:
    for option, value in (
        ("--locale", "not-a-locale"),
        ("--timezone", "America/Tijuana"),
    ):
        result = subprocess.run(
            [str(binary), option, value, fixture.name],
            cwd=fixture.parent,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        require(
            result.returncode == 2,
            f"invalid {option} exited with {result.returncode}",
        )
        error = final_json(result).get("error")
        require(
            isinstance(error, dict) and error.get("code") == "INVALID_REQUEST",
            f"invalid {option} did not produce typed INVALID_REQUEST: {error!r}",
        )


def require_only_changes(baseline: dict[str, object], changed: dict[str, object], allowed: set[str]) -> None:
    keys = baseline.keys() | changed.keys()
    unexpected = {
        key: (baseline.get(key), changed.get(key)) for key in keys - allowed if baseline.get(key) != changed.get(key)
    }
    require(not unexpected, f"unexpected environment-dependent changes: {unexpected!r}")


def main() -> int:
    if len(sys.argv) != 2:
        fail(f"usage: {Path(sys.argv[0]).name} <pliego-binary>", 2)

    binary = Path(sys.argv[1]).expanduser().resolve()
    fixture = Path(__file__).resolve().parent / "fixtures/deterministic-environment/index.html"
    require(binary.is_file(), f"Pliego binary does not exist: {binary}")
    require(fixture.is_file(), f"fixture does not exist: {fixture}")

    with tempfile.TemporaryDirectory(prefix="pliego-deterministic-environment-") as temp:
        temp_root = Path(temp)
        first = run(
            binary,
            fixture,
            temp_root,
            host_locale="de_DE.UTF-8",
            host_timezone="CST6CDT",
        )
        second = run(
            binary,
            fixture,
            temp_root,
            host_locale="fr_FR.UTF-8",
            host_timezone="EST5EDT",
        )
        changed_timezone = run(
            binary,
            fixture,
            temp_root,
            host_locale="de_DE.UTF-8",
            host_timezone="CST6CDT",
            requested_timezone="PST8PDT",
        )
        changed_locale = run(
            binary,
            fixture,
            temp_root,
            host_locale="fr_FR.UTF-8",
            host_timezone="EST5EDT",
            requested_locale="es-MX",
        )

        first_payload = payload(first)
        second_payload = payload(second)
        timezone_payload = payload(changed_timezone)
        locale_payload = payload(changed_locale)
        first_scene_hash = scene_hash(first)
        second_scene_hash = scene_hash(second)
        timezone_scene_hash = scene_hash(changed_timezone)
        locale_scene_hash = scene_hash(changed_locale)
        first_pdf = document_pdf(first)
        second_pdf = document_pdf(second)
        timezone_pdf = document_pdf(changed_timezone)
        locale_pdf = document_pdf(changed_locale)
        require(
            first_payload == second_payload,
            f"defaults changed with host locale/timezone: {first_payload!r} != {second_payload!r}",
        )
        require(
            first_scene_hash == second_scene_hash,
            f"scene hash changed with host locale/timezone: {first_scene_hash} != {second_scene_hash}",
        )
        require(
            first_pdf == second_pdf,
            "PDF bytes changed with host locale/timezone",
        )
        require(first_payload.get("navigatorLanguage") == "en-US", repr(first_payload))
        require(first_payload.get("intlLocale") == "en-US", repr(first_payload))
        require(first_payload.get("intlTimezone") == "UTC", repr(first_payload))
        require(first_payload.get("localHour") == 12, repr(first_payload))
        require(timezone_payload.get("localHour") == 4, repr(timezone_payload))
        require(
            timezone_payload.get("intlTimezone") != first_payload.get("intlTimezone"),
            "changed requested timezone did not change Intl's resolved timezone",
        )
        require(
            timezone_payload.get("formatted") != first_payload.get("formatted"),
            "changed requested timezone did not change Intl date formatting",
        )
        require(
            timezone_scene_hash != first_scene_hash,
            "changed requested timezone did not change the formatted scene",
        )
        require(
            timezone_pdf != first_pdf,
            "changed requested timezone did not change the formatted PDF",
        )
        require_only_changes(
            first_payload,
            timezone_payload,
            {"intlTimezone", "localHour", "formatted"},
        )
        require(locale_payload.get("navigatorLanguage") == "es-MX", repr(locale_payload))
        require(locale_payload.get("intlLocale") == "es-MX", repr(locale_payload))
        require(locale_payload.get("intlTimezone") == "UTC", repr(locale_payload))
        require(locale_payload.get("localHour") == 12, repr(locale_payload))
        require(
            locale_payload.get("formatted") != first_payload.get("formatted"),
            repr(locale_payload),
        )
        require(locale_scene_hash != first_scene_hash, "locale did not change formatted scene")
        require(locale_pdf != first_pdf, "locale did not change formatted PDF")
        require_only_changes(
            first_payload,
            locale_payload,
            {"navigatorLanguage", "intlLocale", "formatted"},
        )
        verify_environment_artifact(first, "en-US", "UTC")
        verify_environment_artifact(second, "en-US", "UTC")
        verify_environment_artifact(changed_timezone, "en-US", "PST8PDT")
        verify_environment_artifact(changed_locale, "es-MX", "UTC")
        verify_invalid_request(binary, fixture)

    print("deterministic environment check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
