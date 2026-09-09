# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Apply only the reviewed shared overlay to two clean, already pinned checkouts.

No fetch, dependency resolution, installation, application boot or link repair.
CI must audit the resulting exact locks before installing either dependency tree.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

HERE = Path(__file__).resolve().parent
MANIFEST = HERE / "invobook_modernized_baseline.json"
OVERLAY = HERE / "runtime-inputs/invobook-laravel12"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def member(root: Path, relative: str) -> Path:
    parts = relative.split("/")
    require(
        all(part not in {"", ".", ".."} and "\\" not in part and ":" not in part for part in parts),
        "Invalid overlay path",
    )
    path = root
    for part in parts:
        path = path / part
        require(not path.is_symlink() and not path.is_junction(), "Overlay path contains a link: " + relative)
    require(path.resolve().is_relative_to(root.resolve()), "Overlay path escaped its root")
    return path


def git(root: Path, *args: str) -> str:
    result = subprocess.run(["git", "-C", str(root), *args], capture_output=True, check=True, timeout=30)
    return result.stdout.decode("utf-8").strip()


def reviewed_overlay() -> tuple[dict, dict[str, str]]:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    require(
        manifest["schema"] == "pliego.invobook-modernized-baseline.v1" and manifest["name"] == "modernized-laravel12",
        "Wrong reviewed baseline",
    )
    files = dict(manifest["changedFiles"])
    fork = manifest["fork"]
    files.update(
        {fork["path"] + "/" + name: value for name, value in (fork["changedFiles"] | fork["untrackedFiles"]).items()}
    )
    require(
        {path.relative_to(OVERLAY).as_posix() for path in OVERLAY.rglob("*") if path.is_file()}
        == set(files) | {".gitattributes"},
        "Unexpected overlay inventory",
    )
    for relative, expected in files.items():
        path = member(OVERLAY, relative)
        require(
            path.is_file() and path.stat().st_nlink == 1 and digest(path) == expected,
            "Reviewed overlay hash mismatch: " + relative,
        )
    return manifest, files


def prepare(app: Path) -> dict:
    manifest, files = reviewed_overlay()
    app = app.absolute()
    require(app.is_dir() and app.resolve() == app, "Expected a real application checkout without linked ancestors")
    fork = member(app, manifest["fork"]["path"])
    for root, commit, untracked in [
        (app, manifest["applicationCommit"], manifest["untracked"]),
        (fork, manifest["fork"]["sourceCommit"], []),
    ]:
        require(git(root, "rev-parse", "HEAD") == commit, "Checkout source commit mismatch")
        require(git(root, "diff", "--no-ext-diff", "HEAD", "--name-only") == "", "Checkout already has changes")
        require(
            git(root, "ls-files", "--others", "--exclude-standard").splitlines() == untracked,
            "Checkout has unexpected untracked files",
        )
        require(
            git(root, "ls-files", "--others", "--ignored", "--exclude-standard") == "",
            "Checkout contains ignored files; use a fresh checkout",
        )
    for relative in ["vendor", "node_modules", "public/build", ".env"]:
        path = member(app, relative)
        require(not path.exists(), "Use a fresh checkout before installing or booting the app: " + relative)
    # Validate the complete write set before changing any file. Existing public
    # chart links are outside this set and are never dereferenced or replaced.
    for relative in files:
        target = member(app, relative)
        require(target.parent.is_dir(), "Overlay target parent missing")
        require(target.is_file() or relative.endswith("/PLIEGO-COMPATIBILITY.md"), "Overlay target missing")
        require(not target.exists() or target.stat().st_nlink == 1, "Overlay target is hard-linked")
    for relative in files:
        shutil.copyfile(member(OVERLAY, relative), member(app, relative))
    require(all(digest(member(app, name)) == value for name, value in files.items()), "Applied overlay differs")
    return {
        "schema": "pliego.invobook-modernized-preparation.v1",
        "baseline": manifest["name"],
        "application_commit": manifest["applicationCommit"],
        "fork_commit": manifest["fork"]["sourceCommit"],
        "manifest_sha256": digest(MANIFEST),
        "preparer_sha256": digest(Path(__file__)),
        "files": files,
        "boundary": "Shared reviewed overlay only. No install, audit, app execution, PDF or timing proof.",
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--app", type=Path)
    group.add_argument("--verify-overlay", action="store_true")
    args = parser.parse_args()
    if args.verify_overlay:
        manifest, files = reviewed_overlay()
        report = {"baseline": manifest["name"], "verified_overlay_files": len(files)}
    else:
        report = prepare(args.app)
    print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
