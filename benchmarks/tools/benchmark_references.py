# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Identity bytes for current runs and one immutable historical publication.

Historical PHP/Python files are inert data: never import or execute them. The
same current validators still check schemas, raw samples, aggregates and seals.
"""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import stat
from types import MappingProxyType
from collections.abc import Mapping
from typing import Any
import tomllib

ROOT = Path(__file__).resolve().parents[2]
FROZEN_ROOT = ROOT / "benchmarks/references/v0.3.3-minimal-static"
SOURCE = {
    "event_name": "workflow_dispatch",
    "job": "hosted-snapshot",
    "ref": "refs/heads/main",
    "repository": "oxhq/pliego",
    "revision": "48e8192992d1e9b66eed8ded164ba67cc25a3e7c",
    "run_attempt": 1,
    "run_id": 33243607869,
    "run_url": "https://github.com/oxhq/pliego/actions/runs/33243607869/attempts/1",
    "workflow": "Pliego hosted performance snapshot",
    "workflow_ref": "oxhq/pliego/.github/workflows/pliego-performance.yml@refs/heads/main",
}
# Captured at SOURCE.revision; independently byte-compared with the publication
# commit b7815fe3399ba5daee7f0ae4aa46084a8a9667db. Do not update these for new runs.
FROZEN_FILES = {
    "benchmarks/manifest.toml": (7439, "0f03f71a6f959d6f0d3c309765fb1dd9e72d2ca2ec497e288c025c3954b642b2"),
    "benchmarks/adapters/dompdf/adapter.php": (
        14659,
        "cf01b40980d21cf9bbc93029afe5f909c601f34f6499da1e52046e462b9284a8",
    ),
    "benchmarks/adapters/dompdf/composer.lock": (
        17880,
        "ee8abef30a8fc0b769d337c00899c5d7c5b7ed9f240e7879b4e3bf334ce90ae0",
    ),
    "benchmarks/adapters/browsershot/adapter.php": (
        57078,
        "f5062e8a7b419fe1864ff06a883ef7391bb7d871f76587810419507ba9b67b42",
    ),
    "benchmarks/adapters/browsershot/composer.lock": (
        7503,
        "706f8056ab11c7aa30a88931ad141221c014f969fffa04de4dbd37d091d01e45",
    ),
    "benchmarks/adapters/browsershot/package-lock.json": (
        12762,
        "db2dd6754e6c4bf8726ea71d62f103e60a10fd55c9d3166d5c678314e5c63144",
    ),
    "benchmarks/tools/pdf_oracle.py": (16356, "2faba9d9a20d426f36082dd8f817b7a3057fc85907a1624b2e70b434355489ee"),
}


@dataclass(frozen=True)
class Reference:
    root: Path
    frozen: Mapping[str, bytes] | None = None

    def read(self, name: str) -> bytes:
        if self.frozen is not None:
            if name not in self.frozen:
                raise ValueError(f"historical reference path is not in the fixed inventory: {name}")
            return self.frozen[name]
        return (self.root / name).read_bytes()

    def fingerprint(self, name: str) -> tuple[str, int]:
        payload = self.read(name)
        return hashlib.sha256(payload).hexdigest(), len(payload)

    def manifest(self) -> dict[str, Any]:
        return tomllib.loads(self.read("benchmarks/manifest.toml").decode("utf-8"))


def _unlinked(path: Path) -> os.stat_result:
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
        raise ValueError(f"historical reference contains a linked/reparse path: {path}")
    return metadata


def _identity(value: os.stat_result) -> tuple[int, ...]:
    return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_mode, value.st_nlink)


def _load_frozen(root: Path) -> Reference:
    # Check lexical ancestors before traversal/resolution can erase link evidence.
    for path in reversed((root, *root.parents)):
        if not stat.S_ISDIR(_unlinked(path).st_mode):
            raise ValueError(f"historical reference ancestor is not a directory: {path}")
    expected_directories = {
        parent.as_posix() for name in FROZEN_FILES for parent in Path(name).parents if parent != Path(".")
    }
    seen: set[str] = set()
    payloads: dict[str, bytes] = {}
    for path in root.rglob("*"):
        before = _unlinked(path)
        name = path.relative_to(root).as_posix()
        if stat.S_ISDIR(before.st_mode) and name in expected_directories:
            continue
        if name not in FROZEN_FILES or not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise ValueError(f"historical reference inventory contains an unexpected/nonregular file: {name}")
        expected_size, expected_hash = FROZEN_FILES[name]
        if before.st_size != expected_size:
            raise ValueError(f"historical reference byte size changed: {name}")
        payload = path.read_bytes()
        after = _unlinked(path)
        if _identity(before) != _identity(after) or hashlib.sha256(payload).hexdigest() != expected_hash:
            raise ValueError(f"historical reference bytes changed: {name}")
        seen.add(name)
        payloads[name] = payload
    if seen != set(FROZEN_FILES):
        raise ValueError("historical reference fixed file inventory is incomplete")
    return Reference(root, MappingProxyType(payloads))


def select(source: dict[str, Any] | None = None) -> Reference:
    """Default to current execution bytes; only one exact past source selects data.

    Unknown revisions cannot select arbitrary directories. A claimed historical
    revision with altered provenance fails, rather than falling back to current.
    """
    if source is not None and not isinstance(source, dict):
        raise ValueError("identity reference source must be an object")
    if source is None or source.get("revision") != SOURCE["revision"]:
        return Reference(ROOT)
    observed = dict(source)
    if "repeat" in observed:
        repeat = observed.pop("repeat")
        if type(repeat) is not int or repeat not in (1, 2, 3):
            raise ValueError("historical reference repeat must be 1, 2 or 3")
    # Canonical JSON also distinguishes bool/float from the pinned integer IDs.
    if json.dumps(observed, sort_keys=True, allow_nan=False) != json.dumps(SOURCE, sort_keys=True):
        raise ValueError("historical reference source provenance is not the exact published run")
    return _load_frozen(FROZEN_ROOT)
