#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPORTER = Path(__file__).with_name("report.py")


class OwnershipReportTests(unittest.TestCase):
    def setUp(self):
        temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(temporary_directory.cleanup)
        self.repo = Path(temporary_directory.name)
        self.run_git("init", "--quiet")
        self.run_git("config", "user.email", "pliego-test@example.invalid")
        self.run_git("config", "user.name", "Pliego Test")
        self.run_git("commit", "--allow-empty", "--quiet", "-m", "base")
        self.base = self.run_git("rev-parse", "HEAD")

    def run_git(self, *args):
        result = subprocess.run(
            ["git", *args],
            cwd=self.repo,
            check=True,
            capture_output=True,
            text=True,
        )
        return result.stdout.strip()

    def write(self, relative_path):
        path = self.repo / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("fixture\n", encoding="utf-8")

    def run_report(self, head, json_path, *, strict=False):
        command = [
            sys.executable,
            str(REPORTER),
            "--base",
            self.base,
            "--head",
            head,
            "--json",
            str(json_path),
        ]
        if strict:
            command.append("--strict")
        return subprocess.run(command, cwd=self.repo, capture_output=True, text=True)

    def test_real_range_is_deterministic_and_strict_rejects_unknown(self):
        self.write("components/script/upstream.rs")
        self.write("components/layout/shared.rs")
        self.write("components/layout/fragmentation/pliego.rs")
        self.write("unclassified-λ.txt")
        self.run_git("add", ".")
        self.run_git("commit", "--quiet", "-m", "head")
        head = self.run_git("rev-parse", "HEAD")

        first_json = self.repo / "first.json"
        second_json = self.repo / "second.json"
        first = self.run_report(head, first_json)
        second = self.run_report(head, second_json)

        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(first.stdout, second.stdout)
        self.assertEqual(first_json.read_bytes(), second_json.read_bytes())

        report = json.loads(first_json.read_text(encoding="utf-8"))
        self.assertEqual(report["base"], self.base)
        self.assertEqual(report["head"], head)
        self.assertEqual(report["counts"], {"pliego": 1, "shared": 1, "unknown": 1, "upstream": 1})
        self.assertEqual(
            report["paths"],
            [
                {"ownership": "pliego", "path": "components/layout/fragmentation/pliego.rs"},
                {"ownership": "shared", "path": "components/layout/shared.rs"},
                {"ownership": "upstream", "path": "components/script/upstream.rs"},
                {"ownership": "unknown", "path": "unclassified-λ.txt"},
            ],
        )
        self.assertEqual(report["total"], 4)

        strict_json = self.repo / "strict.json"
        strict = self.run_report(head, strict_json, strict=True)
        self.assertEqual(strict.returncode, 1, strict.stderr)
        self.assertEqual(strict.stdout, first.stdout)
        self.assertEqual(strict_json.read_bytes(), first_json.read_bytes())


if __name__ == "__main__":
    unittest.main()
