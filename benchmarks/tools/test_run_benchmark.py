#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import importlib.util
import subprocess
from pathlib import Path

SCRIPT = Path(__file__).with_name("run_benchmark.py")
SPEC = importlib.util.spec_from_file_location("run_benchmark", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def main() -> None:
    original = benchmark.subprocess.run
    benchmark.subprocess.run = lambda *args, **kwargs: subprocess.CompletedProcess(
        args=[], returncode=0, stdout='{"wall_ms":1}\n', stderr=""
    )
    try:
        try:
            benchmark.collect_samples(
                Path("php"),
                "minimal-static",
                {"input": "benchmarks/fixtures/minimal-static/input.html"},
                Path("pliego"),
                2,
                0,
            )
        except SystemExit as error:
            assert error.code == 1
        else:
            raise AssertionError("partial runner output was accepted")
    finally:
        benchmark.subprocess.run = original

    print("Pliego benchmark runner-count self-test passed")


if __name__ == "__main__":
    main()
