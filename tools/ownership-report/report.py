#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import argparse
import fnmatch
import json
import subprocess
import sys
import tomllib
from collections.abc import Sequence
from pathlib import Path


OWNERSHIPS = ("upstream", "shared", "pliego", "unknown")
MANIFEST = Path(__file__).resolve().parents[2] / "ownership.toml"
Rule = tuple[str, str]


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Report changed paths by ownership zone.")
    parser.add_argument("--base", required=True, help="base Git revision")
    parser.add_argument("--head", required=True, help="head Git revision")
    parser.add_argument("--strict", action="store_true", help="fail when a changed path is unclassified")
    parser.add_argument("--json", dest="json_path", type=Path, help="write deterministic JSON to this path")
    return parser.parse_args(argv)


def load_rules(path: Path) -> list[Rule]:
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    entries = data.get("path")
    if not isinstance(entries, list) or not entries:
        raise ValueError(f"{path}: expected at least one [[path]] rule")

    rules: list[Rule] = []
    for index, entry in enumerate(entries, start=1):
        if not isinstance(entry, dict):
            raise ValueError(f"{path}: rule {index} must be a table")
        pattern = entry.get("pattern")
        ownership = entry.get("ownership")
        if not isinstance(pattern, str) or not pattern:
            raise ValueError(f"{path}: rule {index} has no pattern")
        if ownership not in OWNERSHIPS[:-1]:
            raise ValueError(f"{path}: rule {index} has invalid ownership {ownership!r}")
        rules.append((pattern, ownership))
    return rules


def resolve_ref(ref: str) -> str:
    result = subprocess.run(
        ["git", "rev-parse", "--verify", "--end-of-options", f"{ref}^{{commit}}"],
        check=True,
        capture_output=True,
    )
    return result.stdout.decode("ascii").strip()


def changed_paths(base: str, head: str) -> list[str]:
    result = subprocess.run(
        [
            "git",
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--ignore-submodules=none",
            "-z",
            base,
            head,
            "--",
        ],
        check=True,
        capture_output=True,
    )
    return sorted(path.decode("utf-8") for path in result.stdout.split(b"\0") if path)


def classify_path(path: str, rules: list[Rule]) -> str:
    ownership = "unknown"
    for pattern, candidate in rules:
        if fnmatch.fnmatchcase(path, pattern):
            ownership = candidate
    return ownership


def build_report(base: str, head: str, rules: list[Rule]) -> dict[str, object]:
    paths = [{"ownership": classify_path(path, rules), "path": path} for path in changed_paths(base, head)]
    counts = {ownership: sum(path["ownership"] == ownership for path in paths) for ownership in OWNERSHIPS}
    return {"base": base, "counts": counts, "head": head, "paths": paths, "total": len(paths)}


def render_human(report: dict[str, object]) -> str:
    counts = report["counts"]
    paths = report["paths"]
    lines = [f"Ownership report {report['base']}..{report['head']}"]
    for ownership in OWNERSHIPS:
        lines.append(f"{ownership}: {counts[ownership]}")
        lines.extend(
            f"  {json.dumps(path['path'], ensure_ascii=True)}" for path in paths if path["ownership"] == ownership
        )
    lines.append(f"total: {report['total']}")
    return "\n".join(lines)


def write_json(path: Path, report: dict[str, object]) -> None:
    with path.open("w", encoding="utf-8", newline="\n") as output:
        output.write(json.dumps(report, indent=2, sort_keys=True))
        output.write("\n")


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        rules = load_rules(MANIFEST)
        base = resolve_ref(args.base)
        head = resolve_ref(args.head)
        report = build_report(base, head, rules)
        if args.json_path:
            write_json(args.json_path, report)
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode("utf-8", errors="replace").strip() or str(error)
        print(f"ownership-report: {detail}", file=sys.stderr)
        return 2
    except (OSError, UnicodeError, ValueError) as error:
        print(f"ownership-report: {error}", file=sys.stderr)
        return 2

    print(render_human(report))
    return 1 if args.strict and report["counts"]["unknown"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
