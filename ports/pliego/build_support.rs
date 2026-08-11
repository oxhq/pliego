/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::path::{Path, PathBuf};

pub(crate) fn servo_workspace_version_from_manifest(manifest: &str) -> Result<String, String> {
    let document = manifest
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("workspace Cargo.toml is invalid TOML: {error}"))?;
    let version = document
        .as_table()
        .get("workspace")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|workspace| workspace.get("package"))
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|package| package.get("version"))
        .ok_or("workspace.package.version is missing")?
        .as_str()
        .ok_or("workspace.package.version must be a string")?;
    if version.is_empty() || version.chars().any(char::is_whitespace) {
        return Err("workspace.package.version must be one non-empty token".into());
    }
    Ok(version.to_owned())
}

pub(crate) fn normalize_git_path(workspace_root: &Path, git_path: &str) -> PathBuf {
    let path = PathBuf::from(git_path);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{normalize_git_path, servo_workspace_version_from_manifest};

    #[test]
    fn reads_only_the_workspace_package_version() {
        assert_eq!(
            servo_workspace_version_from_manifest(
                "[package]\nversion = \"9.9.9\"\n[workspace.package]\nversion = \"0.4.0\"\n"
            )
            .unwrap(),
            "0.4.0"
        );
    }

    #[test]
    fn accepts_literal_strings_and_inline_comments() {
        assert_eq!(
            servo_workspace_version_from_manifest(
                "[workspace.package] # package metadata\nversion = '0.4.0' # Servo version\n"
            )
            .unwrap(),
            "0.4.0"
        );
    }

    #[test]
    fn rejects_missing_non_string_or_whitespace_workspace_versions() {
        assert_eq!(
            servo_workspace_version_from_manifest("[package]\nversion = \"9.9.9\"\n").unwrap_err(),
            "workspace.package.version is missing"
        );
        assert_eq!(
            servo_workspace_version_from_manifest("[workspace.package]\nversion = 4\n")
                .unwrap_err(),
            "workspace.package.version must be a string"
        );
        assert_eq!(
            servo_workspace_version_from_manifest("[workspace.package]\nversion = \"0.4.0 dev\"\n")
                .unwrap_err(),
            "workspace.package.version must be one non-empty token"
        );
    }

    #[test]
    fn rejects_malformed_toml_instead_of_partially_parsing_it() {
        let error = servo_workspace_version_from_manifest(
            "[workspace.package]\nversion = \"0.4.0\" trailing\n",
        )
        .unwrap_err();
        assert!(error.starts_with("workspace Cargo.toml is invalid TOML:"));
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
