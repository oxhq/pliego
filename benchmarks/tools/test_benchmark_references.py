# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

from __future__ import annotations

from copy import deepcopy
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import benchmark_references as references
import run_comparison
import summarize_comparisons
from test_run_comparison import target_identities


class ReferenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve() / "reference"
        shutil.copytree(references.FROZEN_ROOT, self.root)
        self.series = json.loads(
            (
                references.ROOT / "docs/benchmarks/results/v0.3.3-minimal-static-github-hosted/hosted-series.v1.json"
            ).read_text(encoding="utf-8")
        )

    def test_exact_original_bytes_and_current_default(self) -> None:
        old = references._load_frozen(self.root)
        self.assertEqual(len(old.frozen), 7)
        self.assertEqual(sum(map(len, old.frozen.values())), 133677)
        for name, (size, digest) in references.FROZEN_FILES.items():
            self.assertEqual(old.fingerprint(name), (digest, size))
        current = references.select()
        self.assertIsNone(current.frozen)
        for name in references.FROZEN_FILES:
            payload = (references.ROOT / name).read_bytes()
            self.assertEqual(current.fingerprint(name), (hashlib.sha256(payload).hexdigest(), len(payload)))
        for name in (
            "benchmarks/adapters/dompdf/adapter.php",
            "benchmarks/adapters/browsershot/adapter.php",
            "benchmarks/tools/pdf_oracle.py",
        ):
            self.assertNotEqual(current.fingerprint(name), old.fingerprint(name))

    def test_all_seven_corruptions_fail_even_after_prior_success(self) -> None:
        references._load_frozen(self.root)
        for name in references.FROZEN_FILES:
            with self.subTest(name=name):
                path = self.root / name
                original = path.read_bytes()
                path.write_bytes(bytes([original[0] ^ 1]) + original[1:])
                with self.assertRaisesRegex(ValueError, "bytes changed"):
                    references._load_frozen(self.root)
                path.write_bytes(original)
        references._load_frozen(self.root)

    def test_inventory_and_lengths_fail_closed(self) -> None:
        extra = self.root / "extra.txt"
        extra.write_text("unexpected", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "inventory"):
            references._load_frozen(self.root)
        extra.unlink()
        path = self.root / next(iter(references.FROZEN_FILES))
        original = path.read_bytes()
        path.write_bytes(original + b"\n")
        with self.assertRaisesRegex(ValueError, "byte size"):
            references._load_frozen(self.root)
        path.unlink()
        with self.assertRaisesRegex(ValueError, "incomplete"):
            references._load_frozen(self.root)

    def test_paths_cannot_escape_or_select_other_payloads(self) -> None:
        old = references._load_frozen(self.root)
        for name in ("../manifest.toml", "/etc/passwd", "C:/file", "benchmarks\\manifest.toml", "unknown"):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "fixed inventory"):
                old.read(name)

    def test_file_and_ancestor_symlinks_rejected(self) -> None:
        target = self.root / next(iter(references.FROZEN_FILES))
        original = target.read_bytes()
        outside = self.root.parent / "outside"
        outside.write_bytes(original)
        target.unlink()
        try:
            target.symlink_to(outside)
        except OSError as error:
            self.skipTest(f"host does not allow symlink creation: {error}")
        with self.assertRaisesRegex(ValueError, "linked/reparse"):
            references._load_frozen(self.root)
        target.unlink()
        target.write_bytes(original)
        linked_root = self.root.parent / "linked"
        linked_root.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "linked/reparse"):
            references._load_frozen(linked_root)

    def test_exact_source_and_repeat_selection(self) -> None:
        for repeat in (None, 1, 2, 3):
            source = deepcopy(references.SOURCE)
            if repeat is not None:
                source["repeat"] = repeat
            self.assertIsNotNone(references.select(source).frozen)
        for field, original in references.SOURCE.items():
            if field == "revision":
                continue
            source = deepcopy(references.SOURCE)
            source[field] = original + 1 if type(original) is int else original + "changed"
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "source provenance"):
                references.select(source)
        for repeat in (False, True, 0, 4, 1.0, "1", None):
            with self.subTest(repeat=repeat), self.assertRaisesRegex(ValueError, "repeat"):
                references.select({**references.SOURCE, "repeat": repeat})
        for extra in ({"reference": "../../"}, {"run_attempt": True}, {"run_id": 33243607869.0}):
            with self.subTest(extra=extra), self.assertRaisesRegex(ValueError, "source provenance"):
                references.select({**references.SOURCE, **extra})
        self.assertIsNone(references.select({**references.SOURCE, "revision": "a" * 40}).frozen)

    def test_old_and_current_identities_are_not_interchangeable(self) -> None:
        historical = references.select(references.SOURCE)
        old_violations = []
        run_comparison.validate_target_identities(self.series["targets"], old_violations, historical)
        run_comparison.validate_oracle_identity(self.series["oracle"], old_violations, historical)
        self.assertEqual(old_violations, [])
        mismatched = []
        run_comparison.validate_target_identities(self.series["targets"], mismatched)
        run_comparison.validate_oracle_identity(self.series["oracle"], mismatched)
        self.assertTrue(mismatched)
        current = target_identities()
        live_violations = []
        run_comparison.validate_target_identities(current, live_violations)
        self.assertEqual(live_violations, [])
        wrong_reference = []
        run_comparison.validate_target_identities(current, wrong_reference, historical)
        self.assertTrue(wrong_reference)

    def test_historical_series_uses_frozen_reference_without_resealing(self) -> None:
        original = deepcopy(self.series)
        self.assertEqual(summarize_comparisons.validate_series(self.series), [])
        self.assertEqual(self.series, original)
        for change in ({"revision": "a" * 40}, {"run_id": 33243607870}):
            bad = deepcopy(self.series)
            bad["source"].update(change)
            bad["series_sha256"] = summarize_comparisons.series_sha256(bad)
            self.assertTrue(summarize_comparisons.validate_series(bad))
        broken = self.root / "benchmarks/tools/pdf_oracle.py"
        broken.write_bytes(broken.read_bytes() + b"\n")
        with patch.object(references, "FROZEN_ROOT", self.root):
            self.assertTrue(summarize_comparisons.validate_series(self.series))

    def test_optimized_python_cannot_accept_corruption(self) -> None:
        target = self.root / "benchmarks/adapters/dompdf/adapter.php"
        payload = target.read_bytes()
        target.write_bytes(bytes([payload[0] ^ 1]) + payload[1:])
        result = subprocess.run(
            [
                sys.executable,
                "-O",
                "-c",
                "import sys;from pathlib import Path;sys.path.insert(0,sys.argv[1]);"
                "import benchmark_references as r;r.FROZEN_ROOT=Path(sys.argv[2]);r.select(r.SOURCE)",
                str(Path(__file__).parent),
                str(self.root),
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("bytes changed", result.stderr)


if __name__ == "__main__":
    unittest.main()
