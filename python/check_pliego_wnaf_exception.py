#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fail closed if the reviewed wnaf yank exception or its exact source drifts."""

from __future__ import annotations

import argparse
from collections.abc import Callable, Mapping
import hashlib
import json
from pathlib import Path
import re
import sys
import tomllib

PACKAGE = "wnaf@0.14.0"
REASON = "Pinned big-endian Pliego graph; docs/security/wnaf-0.14.0-review.md. Remove on crypto update/before 0.5."

# Exact aaf native source reviewed in docs/security/wnaf-0.14.0-review.md.
# The full lock pins all six published crate checksums and both reverse-parent
# sets. Do not refresh these hashes mechanically to retain a stale exception.
SOURCE_SHA256 = {
    "Cargo.lock": "cd3e85600546db2db1017f30eecd1c9e933f0898745450ecf3b986211be3a5d7",
    "Cargo.toml": "d2be78e92d962e311df394a63fadf00d37c08f7d38fd035c4e66e45bfdbbb896",
    "ports/pliego/Cargo.toml": "4f1af46a004434b0ff171ff303694aaace58cb576dbf913073c42f3c9a20d430",
    "components/script/Cargo.toml": "da6f3438319d7782b7e0caa082788a0953c7058b10a0d4c5d954e72727c71b93",
    "components/script/dom/webcrypto/subtlecrypto/ec_common.rs": (
        "720169bfcad10ac12fd0f29b7293cbf63c1e2f025f288002d0d83f4faf6018f1"
    ),
    "components/script/dom/webcrypto/subtlecrypto/ecdsa_operation.rs": (
        "f0d9d7d3d008b274531f572a35f0a41526ea7d853911f12c039addae7afae176"
    ),
    "components/script/dom/webcrypto/subtlecrypto/ecdh_operation.rs": (
        "e33b5a976929bfb8e934bf6fe60d665e9afe1a3f623566327c8163fdccfed5f1"
    ),
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def check_exception(config: Mapping[str, object]) -> None:
    advisories = config.get("advisories")
    if not isinstance(advisories, dict):
        raise ValueError("Missing advisories configuration")
    require(advisories.get("yanked", "warn") in ("warn", "deny"), "Global yank checking must remain enabled")
    require(advisories.get("disable-yank-checking", False) is False, "Yank checking must not be disabled")
    ignored = advisories.get("ignore")
    if not isinstance(ignored, list):
        raise ValueError("Missing advisory ignore list")
    matching = []
    for entry in ignored:
        require(isinstance(entry, (str, dict)), "Malformed advisory ignore entry")
        spec = entry.get("crate", "") if isinstance(entry, dict) else entry
        require(isinstance(spec, str), "Malformed ignored crate specification")
        if isinstance(spec, str) and re.search(r"(?:^|[#/])wnaf(?:@|$)", spec):
            matching.append(entry)
    require(matching == [{"crate": PACKAGE, "reason": REASON}], "Missing, duplicate or broadened wnaf exception")


def check_sources(read_source: Callable[[str], bytes]) -> None:
    for path, expected in SOURCE_SHA256.items():
        actual = hashlib.sha256(read_source(path)).hexdigest()
        require(actual == expected, f"wnaf applicability source changed: {path}; remove exception or re-review")


def verify(root: Path) -> None:
    check_exception(tomllib.loads((root / "deny.toml").read_text(encoding="utf-8")))
    check_sources(lambda path: (root / path).read_bytes())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        verify(args.root)
    except (OSError, ValueError) as error:
        print(f"wnaf exception guard: {error}", file=sys.stderr)
        return 1
    print(
        json.dumps(
            {
                "verified": True,
                "package": PACKAGE,
                "source_files": len(SOURCE_SHA256),
                "boundary": "Exact static applicability only; live RustSec/Tidy checks remain required",
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
