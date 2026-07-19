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
    command = [str(binary)]
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


def verify_environment_artifact(
    summary: dict[str, object], requested_timezone: str
) -> None:
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
            "locale": {"requested": "en-US", "resolved": "en-US"},
            "timezone": {
                "requested": requested_timezone,
                "resolved": requested_timezone,
            },
        },
        f"unexpected resolved environment artifact: {artifact!r}",
    )


def verify_invalid_request(binary: Path, fixture: Path) -> None:
    result = subprocess.run(
        [str(binary), "--locale", "de-DE", fixture.name],
        cwd=fixture.parent,
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    require(result.returncode == 2, f"invalid locale exited with {result.returncode}")
    error = final_json(result).get("error")
    require(
        isinstance(error, dict) and error.get("code") == "INVALID_REQUEST",
        f"invalid locale did not produce typed INVALID_REQUEST: {error!r}",
    )


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
        changed = run(
            binary,
            fixture,
            temp_root,
            host_locale="de_DE.UTF-8",
            host_timezone="CST6CDT",
            requested_timezone="PST8PDT",
        )

        first_payload = payload(first)
        second_payload = payload(second)
        changed_payload = payload(changed)
        require(
            first_payload == second_payload,
            f"defaults changed with host locale/timezone: {first_payload!r} != {second_payload!r}",
        )
        require(first_payload.get("navigatorLanguage") == "en-US", repr(first_payload))
        require(first_payload.get("intlLocale") == "en-US", repr(first_payload))
        require(first_payload.get("intlTimezone") == "UTC", repr(first_payload))
        require(first_payload.get("localHour") == 12, repr(first_payload))
        require(changed_payload.get("localHour") == 4, repr(changed_payload))
        require(
            changed_payload.get("intlTimezone") != first_payload.get("intlTimezone"),
            "changed requested timezone did not change Intl's resolved timezone",
        )
        require(
            changed_payload.get("formatted") != first_payload.get("formatted"),
            "changed requested timezone did not change Intl date formatting",
        )
        verify_environment_artifact(first, "UTC")
        verify_environment_artifact(second, "UTC")
        verify_environment_artifact(changed, "PST8PDT")
        verify_invalid_request(binary, fixture)

    print("deterministic environment check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
