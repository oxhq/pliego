/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let mut in_workspace_package = false;
    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_workspace_package = line == "[workspace.package]";
            continue;
        }
        if !in_workspace_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "version" {
            continue;
        }
        let value = value.trim();
        let Some(version) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            return Err("workspace.package.version must be one quoted string".into());
        };
        if version.is_empty() || version.chars().any(char::is_whitespace) {
            return Err("workspace.package.version must be one non-empty token".into());
        }
        return Ok(version.to_owned());
    }
    Err("workspace.package.version is missing".into())
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
    Ok(())
}

fn normalize_git_path(workspace_root: &Path, git_path: &str) -> PathBuf {
    let path = PathBuf::from(git_path);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{normalize_git_path, servo_workspace_version};

    #[test]
    fn reads_only_the_workspace_package_version() {
        let root =
            std::env::temp_dir().join(format!("pliego-build-version-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join("Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nversion = \"9.9.9\"\n[workspace.package]\nversion = \"0.4.0\"\n",
        )
        .unwrap();

        assert_eq!(servo_workspace_version(&manifest).unwrap(), "0.4.0");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_missing_or_unquoted_workspace_versions() {
        let root = std::env::temp_dir().join(format!(
            "pliego-build-invalid-version-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join("Cargo.toml");

        fs::write(&manifest, "[package]\nversion = \"9.9.9\"\n").unwrap();
        assert_eq!(
            servo_workspace_version(&manifest).unwrap_err(),
            "workspace.package.version is missing"
        );

        fs::write(&manifest, "[workspace.package]\nversion = 0.4.0\n").unwrap();
        assert_eq!(
            servo_workspace_version(&manifest).unwrap_err(),
            "workspace.package.version must be one quoted string"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_primary_checkout_git_paths_from_the_workspace_root() {
        let workspace = Path::new("workspace");
        assert_eq!(
            normalize_git_path(workspace, ".git/HEAD"),
            workspace.join(".git/HEAD")
        );

        let absolute = std::env::temp_dir().join("pliego-git-head");
        assert_eq!(
            normalize_git_path(workspace, absolute.to_str().unwrap()),
            absolute
        );
    }
}
