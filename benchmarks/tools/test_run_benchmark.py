#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

import importlib.util
import subprocess
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("run_benchmark.py")
SPEC = importlib.util.spec_from_file_location("run_benchmark", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def main() -> None:
    with patch.object(
        benchmark.subprocess,
        "run",
        return_value=subprocess.CompletedProcess(args=[], returncode=0, stdout='{"wall_ms":1}\n', stderr=""),
    ):
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

    with patch.object(
        benchmark,
        "run",
        return_value=subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr="PHP 8.4.0\n"),
    ):
        assert benchmark.tool_version(["php", "--version"]) == "PHP 8.4.0"

    command = benchmark.build_command(
        Path("php"),
        "comma-text",
        {
            "input": "benchmarks/fixtures/minimal-static/input.html",
            "correctness": {"text_contains": ["Revenue, net", "Total"]},
        },
        Path("pliego"),
        1,
        0,
    )
    fragments = [command[index + 1] for index, value in enumerate(command) if value == "--text-contains"]
    assert fragments == ["Revenue, net", "Total"]

    print("Pliego benchmark runner-count self-test passed")


if __name__ == "__main__":
    main()
