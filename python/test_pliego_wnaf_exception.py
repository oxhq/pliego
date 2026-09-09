#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Focused controls for the exact-source wnaf exception, including Python -O."""

from __future__ import annotations

from copy import deepcopy
from pathlib import Path
import subprocess
import tempfile
import unittest

import check_pliego_wnaf_exception as guard

ROOT = Path(__file__).resolve().parents[1]


class WnafExceptionTests(unittest.TestCase):
    sources: dict[str, bytes]

    @classmethod
    def setUpClass(cls) -> None:
        # Unit fixtures use committed blobs so sparse developer checkouts can run
        # controls. The production guard itself reads actual files only.
        cls.sources = {
            path: subprocess.check_output(["git", "show", f"HEAD:{path}"], cwd=ROOT, timeout=30)
            for path in guard.SOURCE_SHA256
        }

    def config(self) -> dict[str, object]:
        return {"advisories": {"ignore": [{"crate": guard.PACKAGE, "reason": guard.REASON}]}}

    def test_exact_source_and_exception(self) -> None:
        guard.check_exception(self.config())
        guard.check_sources(self.sources.__getitem__)

    def test_every_source_change_rejected(self) -> None:
        for path in self.sources:
            with self.subTest(path=path):
                changed = dict(self.sources)
                changed[path] += b"\n"
                with self.assertRaisesRegex(ValueError, "applicability source changed"):
                    guard.check_sources(changed.__getitem__)

    def test_missing_source_rejected(self) -> None:
        missing = dict(self.sources)
        del missing["Cargo.lock"]
        with self.assertRaises(KeyError):
            guard.check_sources(missing.__getitem__)

    def test_unreadable_source_rejected(self) -> None:
        def denied(path: str) -> bytes:
            raise PermissionError(path)

        with self.assertRaises(PermissionError):
            guard.check_sources(denied)

    def test_exception_variants_rejected(self) -> None:
        correct = {"crate": guard.PACKAGE, "reason": guard.REASON}
        for entries in (
            [],
            [correct, correct],
            ["wnaf"],
            ["wnaf@0.14.0"],
            [{"crate": "wnaf@0.14", "reason": guard.REASON}],
            [{"crate": guard.PACKAGE, "reason": "unreviewed"}],
            [correct, {"crate": "wnaf", "reason": "broad"}],
            [correct, "https://crates.io#wnaf@0.14.1"],
            [7],
            [{"crate": 7}],
        ):
            with self.subTest(entries=entries), self.assertRaises(ValueError):
                guard.check_exception({"advisories": {"ignore": deepcopy(entries)}})

    def test_global_yank_bypass_rejected(self) -> None:
        for key, value in (("yanked", "allow"), ("yanked", []), ("disable-yank-checking", True)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                guard.check_exception(
                    {"advisories": {"ignore": [{"crate": guard.PACKAGE, "reason": guard.REASON}], key: value}}
                )

    def test_other_rustsec_entries_preserved(self) -> None:
        guard.check_exception(
            {"advisories": {"ignore": ["RUSTSEC-2024-0436", {"crate": guard.PACKAGE, "reason": guard.REASON}]}}
        )

    def test_malformed_config_rejected(self) -> None:
        for config in ({}, {"advisories": []}, {"advisories": {}}, {"advisories": {"ignore": "wnaf"}}):
            with self.subTest(config=config), self.assertRaises(ValueError):
                guard.check_exception(config)

    def test_actual_file_verification(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "deny.toml").write_text(
                '[advisories]\nignore = [{ crate = "' + guard.PACKAGE + '", reason = "' + guard.REASON + '" }]\n',
                encoding="utf-8",
            )
            for path, data in self.sources.items():
                target = root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            guard.verify(root)
            (root / "Cargo.lock").unlink()
            with self.assertRaises(OSError):
                guard.verify(root)


if __name__ == "__main__":
    unittest.main()
