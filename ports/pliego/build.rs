/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod build_support;

use build_support::{normalize_git_path, servo_workspace_version_from_manifest};

fn git_output(workspace_root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| error.to_string())
}

fn servo_workspace_version(workspace_manifest: &Path) -> Result<String, String> {
    let manifest = fs::read_to_string(workspace_manifest).map_err(|error| error.to_string())?;
    servo_workspace_version_from_manifest(&manifest)
}

fn emit_servo_build_identity(workspace_root: &Path) -> Result<(), Box<dyn Error>> {
    let workspace_manifest = workspace_root.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", workspace_manifest.display());

    let servo_version = servo_workspace_version(&workspace_manifest)?;
    println!("cargo:rustc-env=PLIEGO_SERVO_VERSION={servo_version}");

    match git_output(workspace_root, &["rev-parse", "--short", "HEAD"]) {
        Ok(revision) if !revision.is_empty() => {
            println!("cargo:rustc-env=PLIEGO_GIT_SHA={revision}");
        },
        Ok(_) | Err(_) => println!("cargo:rustc-env=PLIEGO_GIT_SHA=nogit"),
    }

    if let Ok(head_path) = git_output(workspace_root, &["rev-parse", "--git-path", "HEAD"]) {
        let head_path = normalize_git_path(workspace_root, &head_path);
        println!("cargo:rerun-if-changed={}", head_path.display());
    }
    if let Ok(reference) = git_output(workspace_root, &["symbolic-ref", "-q", "HEAD"]) &&
        let Ok(reference_path) =
            git_output(workspace_root, &["rev-parse", "--git-path", &reference])
    {
        let reference_path = normalize_git_path(workspace_root, &reference_path);
        println!("cargo:rerun-if-changed={}", reference_path.display());
    }
    if let Ok(packed_refs_path) =
        git_output(workspace_root, &["rev-parse", "--git-path", "packed-refs"])
    {
        let packed_refs_path = normalize_git_path(workspace_root, &packed_refs_path);
        println!("cargo:rerun-if-changed={}", packed_refs_path.display());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("Pliego must live below the Servo workspace root")?;
    emit_servo_build_identity(workspace_root)?;

    if std::env::var("CARGO_CFG_TARGET_OS")? != "windows" {
        return Ok(());
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .ok_or("unexpected Cargo OUT_DIR")?;
    let Some(angle_out) = fs::read_dir(profile_dir.join("build"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("mozangle-"))
        })
        .map(|path| path.join("out"))
        .find(|path| path.join("libEGL.dll").is_file())
    else {
        return Ok(());
    };

    for name in ["libEGL.dll", "libGLESv2.dll"] {
        fs::copy(angle_out.join(name), profile_dir.join(name))?;
    }

    println!("cargo:rerun-if-changed={}", angle_out.display());
    Ok(())
}
