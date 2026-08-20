#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Validate a retained cross-target execution artifact and its raw samples."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import run_benchmark


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: validate_interleaved_run.py <artifact.json>", file=sys.stderr)
        return 2
    artifact = load_json(Path(sys.argv[1]))
    violations = run_benchmark.validate_interleaved_artifact(artifact)
    if violations:
        for violation in violations:
            print(f"violation {violation}", file=sys.stderr)
        print(f"interleaved artifact failed validation with {len(violations)} violation(s)", file=sys.stderr)
        return 1
    print("interleaved artifact validated against schedule, sample, and identity contracts")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
