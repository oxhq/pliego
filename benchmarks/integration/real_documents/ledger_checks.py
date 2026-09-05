"""Fail-closed ledger requirements and portable, non-link artifact paths."""

from __future__ import annotations

import os
import re
import stat
from pathlib import Path


def require(condition: object, message: object = "Ledger qualification requirement failed") -> None:
    # Unlike an assert statement this remains authoritative under python -O.
    if not condition:
        raise AssertionError(message)


def plain_path(path: Path, *, directory: bool = False) -> Path:
    """Inspect every lexical ancestor before resolving, including Windows reparse points."""
    require(".." not in path.parts, "Parent traversal in artifact path")
    absolute = Path(os.path.abspath(path))
    current = Path(absolute.anchor)
    components = absolute.parts[1:]
    for component in (None, *components):
        if component is not None:
            current /= component
        info = current.lstat()
        require(not stat.S_ISLNK(info.st_mode), f"Symbolic link in artifact path: {current}")
        require(
            not getattr(info, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0),
            f"Reparse point in artifact path: {current}",
        )
        if current != absolute or directory:
            require(stat.S_ISDIR(info.st_mode), f"Artifact ancestor is not a directory: {current}")
        else:
            require(stat.S_ISREG(info.st_mode), f"Artifact is not a regular file: {current}")
    return absolute.resolve(strict=True)


def closed_path(root: Path, relative: str) -> Path:
    """Accept portable manifest names only, never host-dependent aliases or traversal."""
    require(isinstance(relative, str) and bool(re.fullmatch(r"[A-Za-z0-9._/-]+", relative)), "Invalid artifact name")
    parts = relative.split("/")
    reserved = {"CON", "PRN", "AUX", "NUL"} | {f"{prefix}{n}" for prefix in ("COM", "LPT") for n in range(1, 10)}
    require(all(part not in {"", ".", ".."} and not part.endswith(".") for part in parts), "Noncanonical artifact name")
    require(all(part.split(".")[0].upper() not in reserved for part in parts), "Reserved artifact name")
    root = plain_path(root, directory=True)
    path = plain_path(root.joinpath(*parts))
    require(path.is_relative_to(root), "Artifact escaped its root")
    return path
