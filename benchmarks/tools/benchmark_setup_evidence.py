#!/usr/bin/env python3

"""Write the always-retained diagnostic for benchmark-host setup phases."""

from __future__ import annotations

import argparse
import os
import re
import stat
import sys
from datetime import datetime, timezone
from pathlib import Path

import benchmark_publication

DOCUMENT_NAME = "benchmark-setup-diagnostic.v1.json"
MANIFEST_NAME = "SHA256SUMS"


def require_output_root(path: Path) -> Path:
    if not path.is_absolute() or path.parent.resolve(strict=True) != path.parent:
        raise benchmark_publication.PublicationError("setup diagnostic root must be absolute below a canonical parent")
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    resolved = path.resolve(strict=True)
    if path != resolved or path.is_symlink() or not path.is_dir():
        raise benchmark_publication.PublicationError("setup diagnostic root must be one canonical directory")
    if os.name == "posix":
        metadata = path.stat(follow_symlinks=False)
        if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
            raise benchmark_publication.PublicationError("setup diagnostic root must be runner-owned with mode 0700")
    return resolved


def write_diagnostic(args: argparse.Namespace) -> Path:
    root = require_output_root(args.output_dir)
    if re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", args.phase) is None:
        raise benchmark_publication.PublicationError("setup phase must be one bounded lowercase identifier")
    expected_exit = None if args.status == "started" else (0 if args.status == "passed" else args.exit_code)
    if expected_exit is not None and (type(expected_exit) is not int or expected_exit < 0):
        raise benchmark_publication.PublicationError("setup exit code is invalid for its status")
    if args.status == "failed" and args.exit_code is None:
        raise benchmark_publication.PublicationError("failed setup evidence requires an exit code")
    document = {
        "schema": "pliego.benchmark-setup-diagnostic",
        "version": 1,
        "status": args.status,
        "phase": args.phase,
        "exit_code": expected_exit,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "repository": args.repository,
        "revision": args.revision,
        "run_id": args.run_id,
        "run_attempt": args.run_attempt,
        "job": args.job,
        "workflow_ref": args.workflow_ref,
        "runner_os": args.runner_os,
        "runner_arch": args.runner_arch,
    }
    document_path = root / DOCUMENT_NAME
    benchmark_publication.atomic_write_bytes(document_path, benchmark_publication.json_bytes(document, indent=2))
    benchmark_publication.atomic_write_bytes(
        root / MANIFEST_NAME,
        f"{benchmark_publication.sha256_file(document_path)}  {DOCUMENT_NAME}\n".encode("ascii"),
    )
    return document_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--status", required=True, choices=("started", "failed", "passed"))
    parser.add_argument("--phase", required=True)
    parser.add_argument("--exit-code", type=int)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--job", required=True)
    parser.add_argument("--workflow-ref", required=True)
    parser.add_argument("--runner-os", required=True)
    parser.add_argument("--runner-arch", required=True)
    args = parser.parse_args()
    try:
        path = write_diagnostic(args)
    except (OSError, benchmark_publication.PublicationError) as error:
        print(f"benchmark_setup_evidence: {error}", file=sys.stderr)
        return 1
    print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
