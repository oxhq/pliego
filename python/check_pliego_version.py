#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Verify Pliego's exact, ordered four-line version contract."""

from __future__ import annotations

import argparse
from pathlib import Path
import sys


PLIEGO_API_VERSION = "2"


def expected_version_payload(
    *,
    pliego_version: str,
    servo_version: str,
    git_sha: str,
    servo_base_sha: str,
) -> bytes:
    values = {
        "Pliego version": pliego_version,
        "Servo version": servo_version,
        "Git revision": git_sha,
        "Servo base revision": servo_base_sha,
    }
    for label, value in values.items():
        if not value or any(character.isspace() for character in value):
            raise ValueError(f"{label} must be one non-empty token")

    return (
        f"pliego {pliego_version}\n"
        f"pliego-api {PLIEGO_API_VERSION}\n"
        f"Servo {servo_version}-{git_sha}\n"
        f"Servo base {servo_base_sha}\n"
    ).encode("utf-8")


def check_version_payload(
    payload: bytes,
    *,
    pliego_version: str,
    servo_version: str,
    git_sha: str,
    servo_base_sha: str,
    source: str = "version output",
) -> list[str]:
    try:
        expected = expected_version_payload(
            pliego_version=pliego_version,
            servo_version=servo_version,
            git_sha=git_sha,
            servo_base_sha=servo_base_sha,
        )
    except ValueError as error:
        return [f"{source} expected identity is invalid: {error}"]
    without_crlf = payload.replace(b"\r\n", b"")
    if b"\r" in without_crlf or (b"\r\n" in payload and b"\n" in without_crlf):
        return [f"{source} must use consistent LF or CRLF line endings"]
    normalized = payload.replace(b"\r\n", b"\n")
    if normalized != expected:
        return [f"{source} must match the exact ordered four-line Pliego version contract"]
    return []


def self_test() -> None:
    arguments = {
        "pliego_version": "0.1.1",
        "servo_version": "0.4.0",
        "git_sha": "0123456789",
        "servo_base_sha": "a" * 40,
    }
    expected = expected_version_payload(**arguments)
    assert not check_version_payload(expected, **arguments)
    assert not check_version_payload(expected.replace(b"\n", b"\r\n"), **arguments)

    lines = expected.splitlines(keepends=True)
    invalid_payloads = (
        b"".join(lines[:-1]),
        expected + b"unexpected\n",
        b"".join((lines[1], lines[0], *lines[2:])),
        b"pliego 9.9.9\n" + b"".join(lines[1:]),
        lines[0] + b"pliego-api 1\n" + b"".join(lines[2:]),
        b"".join(lines[:2]) + b"Servo 0.4.0-deadbeef\n" + lines[3],
        b"".join(lines[:3]) + b"Servo base deadbeef\n",
        expected.rstrip(b"\n"),
        b"\xef\xbb\xbf" + expected,
    )
    for payload in invalid_payloads:
        assert check_version_payload(payload, **arguments) == [
            "version output must match the exact ordered four-line Pliego version contract"
        ]

    invalid_line_endings = (
        expected.replace(b"\n", b"\r", 1),
        expected.replace(b"\n", b"\r\n", 1),
    )
    for payload in invalid_line_endings:
        assert check_version_payload(payload, **arguments) == [
            "version output must use consistent LF or CRLF line endings"
        ]

    for key in arguments:
        for value in ("", "two tokens"):
            invalid = dict(arguments)
            invalid[key] = value
            try:
                expected_version_payload(**invalid)
            except ValueError as error:
                assert str(error).endswith("must be one non-empty token")
            else:
                raise AssertionError(f"accepted invalid {key}")
            assert check_version_payload(expected, **invalid)[0].startswith(
                "version output expected identity is invalid:"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version_output", nargs="?", type=Path)
    parser.add_argument("--pliego-version")
    parser.add_argument("--servo-version")
    parser.add_argument("--git-sha")
    parser.add_argument("--servo-base-sha")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    identity = {
        "pliego_version": args.pliego_version,
        "servo_version": args.servo_version,
        "git_sha": args.git_sha,
        "servo_base_sha": args.servo_base_sha,
    }
    supplied_identity = any(value is not None for value in identity.values())
    if args.self_test:
        if args.version_output is not None or supplied_identity:
            parser.error("--self-test cannot be combined with validation inputs")
        self_test()
        print("Pliego version checker self-test: ok")
        return 0

    if args.version_output is None:
        parser.error("version_output is required")
    missing = [name.replace("_", "-") for name, value in identity.items() if value is None]
    if missing:
        parser.error("missing required arguments: " + ", ".join(f"--{name}" for name in missing))

    assert args.pliego_version is not None
    assert args.servo_version is not None
    assert args.git_sha is not None
    assert args.servo_base_sha is not None
    try:
        payload = args.version_output.read_bytes()
        errors = check_version_payload(
            payload,
            pliego_version=args.pliego_version,
            servo_version=args.servo_version,
            git_sha=args.git_sha,
            servo_base_sha=args.servo_base_sha,
        )
    except (OSError, ValueError) as error:
        errors = [str(error)]
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"Pliego version verified: {args.version_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
