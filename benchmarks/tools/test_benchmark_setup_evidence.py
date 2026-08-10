#!/usr/bin/env python3

"""Self-check the setup diagnostic survives and accurately records failures."""

from __future__ import annotations

import argparse
import os
import stat
import tempfile
from pathlib import Path

import benchmark_publication
import benchmark_setup_evidence
import validate_result


def arguments(root: Path, status: str, phase: str, exit_code: int | None = None) -> argparse.Namespace:
    return argparse.Namespace(
        output_dir=root,
        status=status,
        phase=phase,
        exit_code=exit_code,
        repository=benchmark_publication.CANONICAL_REPOSITORY,
        revision="a" * 40,
        run_id=123,
        run_attempt=1,
        job="dedicated",
        workflow_ref=benchmark_publication.TRUSTED_WORKFLOW_REF,
        runner_os="Linux",
        runner_arch="X64",
    )


def main() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = (Path(raw) / "setup").resolve()
        document_path = benchmark_setup_evidence.write_diagnostic(arguments(root, "started", "checkout"))
        started, _ = benchmark_publication.load_bound_json_object(document_path, "setup diagnostic")
        assert started["status"] == "started" and started["exit_code"] is None
        failed_path = benchmark_setup_evidence.write_diagnostic(arguments(root, "failed", "host-preflight", 17))
        failed, _ = benchmark_publication.load_bound_json_object(failed_path, "setup diagnostic")
        assert failed["status"] == "failed" and failed["exit_code"] == 17
        schema = benchmark_publication.strict_json_loads(
            (Path(__file__).resolve().parents[1] / "schema" / "benchmark-setup-diagnostic.v1.json").read_bytes(),
            "setup diagnostic schema",
        )
        violations: list[validate_result.Violation] = []
        validate_result.validate(failed, schema, "$", violations)
        assert not violations, "\n".join(map(str, violations))
        assert (root / "SHA256SUMS").read_text(encoding="ascii").split()[0] == benchmark_publication.sha256_file(
            failed_path
        )
        if os.name == "posix":
            assert stat.S_IMODE(root.stat().st_mode) == 0o700
        try:
            benchmark_setup_evidence.write_diagnostic(arguments(root, "failed", "bad phase", 1))
        except benchmark_publication.PublicationError:
            pass
        else:
            raise AssertionError("malformed setup phase was accepted")
    print("benchmark setup diagnostic checks passed")


if __name__ == "__main__":
    main()
