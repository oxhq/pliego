# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Pure reconstruction guards; no application dependency installation or boot."""

from pathlib import Path
import os
from collections.abc import Callable
import tempfile
import unittest
from unittest.mock import patch

import prepare_invobook_modernized as preparation


class ModernizedPreparationTests(unittest.TestCase):
    def clean_git(self, app: Path, ignored: str = "") -> Callable[..., str]:
        manifest, _ = preparation.reviewed_overlay()

        def response(root: Path, *args: str) -> str:
            if args == ("rev-parse", "HEAD"):
                return manifest["applicationCommit"] if root == app else manifest["fork"]["sourceCommit"]
            if "--ignored" in args:
                return ignored if root == app else ""
            if args[0] == "ls-files":
                return "packages/glow-chart/" if root == app else ""
            return ""

        return response

    def test_exact_reviewed_overlay(self) -> None:
        manifest, files = preparation.reviewed_overlay()
        self.assertEqual(len(files), 7)
        self.assertEqual(manifest["installedPackages"]["spatie/browsershot"], "5.4.0")
        self.assertEqual(files["composer.lock"], "2d88395bf904aaa2bdddf63d38168a240190c6cc89cbf5333fc1124c569ec21d")

    def test_unsafe_member_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative in ["../composer.json", "/composer.json", "a//b", "a\\b", "C:/outside"]:
                with self.subTest(relative=relative), self.assertRaises(ValueError):
                    preparation.member(root, relative)

    def test_modified_checkout_rejected_before_writes(self) -> None:
        manifest, _ = preparation.reviewed_overlay()
        with tempfile.TemporaryDirectory() as temporary:
            app = Path(temporary)
            (app / "packages/glow-chart").mkdir(parents=True)
            with (
                patch.object(preparation, "git", side_effect=[manifest["applicationCommit"], "composer.json"]),
                patch.object(preparation.shutil, "copyfile") as copy,
            ):
                with self.assertRaisesRegex(ValueError, "already has changes"):
                    preparation.prepare(app)
                copy.assert_not_called()

    def test_wrong_pin_rejected_before_writes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            app = Path(temporary)
            with (
                patch.object(preparation, "git", return_value="0" * 40),
                patch.object(preparation.shutil, "copyfile") as copy,
            ):
                with self.assertRaisesRegex(ValueError, "commit mismatch"):
                    preparation.prepare(app)
                copy.assert_not_called()

    def test_ignored_runtime_files_rejected_before_writes(self) -> None:
        for relative in ["bootstrap/cache/config.php", "public/hot"]:
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as temporary:
                app = Path(temporary)
                target = app / relative
                target.parent.mkdir(parents=True)
                target.write_bytes(b"preserve this ignored input")
                with (
                    patch.object(preparation, "git", side_effect=self.clean_git(app, relative)),
                    patch.object(preparation.shutil, "copyfile") as copy,
                ):
                    with self.assertRaisesRegex(ValueError, "ignored files"):
                        preparation.prepare(app)
                    copy.assert_not_called()
                self.assertEqual(target.read_bytes(), b"preserve this ignored input")

    def test_hardlinked_target_rejected_before_writes(self) -> None:
        _, files = preparation.reviewed_overlay()
        with tempfile.TemporaryDirectory() as temporary:
            app = Path(temporary) / "app"
            for relative in files:
                target = app / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b"original checkout input")
            outside = Path(temporary) / "external-alias.json"
            os.link(app / "composer.json", outside)
            with (
                patch.object(preparation, "git", side_effect=self.clean_git(app)),
                patch.object(preparation.shutil, "copyfile") as copy,
            ):
                with self.assertRaisesRegex(ValueError, "hard-linked"):
                    preparation.prepare(app)
                copy.assert_not_called()
            self.assertEqual(outside.read_bytes(), b"original checkout input")


if __name__ == "__main__":
    unittest.main()
