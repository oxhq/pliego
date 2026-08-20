/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Inactive, descriptor-bound API 2 input loading.
//!
//! The executable still advertises no API 2 tuple and never calls this module. Keeping the loader
//! below the decoder lets tests prove the filesystem authority boundary before render activation.

use std::collections::BTreeMap;
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::collections::BTreeSet;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::{OsStr, OsString};
use std::path::Path;

use serde_json::Value;
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use sha2::{Digest, Sha256};

use super::InvocationError;
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use super::{
    INPUT_FIELDS, closed_object, decode_input_manifest, hex_lower, required, required_string,
    validate_request,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::{INPUT_MANIFEST_MAX_BYTES, INPUT_TREE_MAX_NODES, required_u64};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::session::BoundDirectory;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ResolvedInputJob {
    canonical_manifest: Vec<u8>,
    entrypoint: String,
    resources: BTreeMap<String, LoadedInputResource>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct LoadedInputResource {
    media_type: String,
    content_address: String,
    declared_bytes: u64,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
struct ExpectedInputResource {
    media_type: String,
    content_address: String,
    bytes: u64,
}

/// Freeze one fixed-layout cwd-v1 job into an immutable, host-path-free input store.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn load_input_job(
    _job_root: &Path,
    _request: &Value,
) -> Result<ResolvedInputJob, InvocationError> {
    Err(InvocationError::new(
        "cwd-v1 input loading requires descriptor-relative filesystem authority",
    ))
}

/// Freeze one fixed-layout cwd-v1 job into an immutable, host-path-free input store.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn load_input_job(
    job_root: &Path,
    request: &Value,
) -> Result<ResolvedInputJob, InvocationError> {
    validate_request(request).map_err(InvocationError::new)?;
    let input = request_input(request)?;
    let entrypoint = required_string(input, "$.input", "entrypoint")
        .map_err(InvocationError::new)?
        .to_owned();
    let manifest_descriptor = closed_object(
        required(input, "$.input", "manifest").map_err(InvocationError::new)?,
        "$.input.manifest",
        super::MANIFEST_FIELDS,
    )
    .map_err(InvocationError::new)?;
    let manifest_bytes = required_u64(
        manifest_descriptor,
        "$.input.manifest",
        "bytes",
        1,
        INPUT_MANIFEST_MAX_BYTES as u64,
    )
    .map_err(InvocationError::new)?;

    let root = BoundDirectory::open_private(job_root.to_owned()).map_err(|_| {
        InvocationError::new("job root does not satisfy the private directory boundary")
    })?;
    let expected_root = [
        OsString::from("input"),
        OsString::from("input-manifest.json"),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual_root = root
        .child_names(2)
        .map_err(|_| InvocationError::new("cannot inspect the exact job-root closure"))?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if actual_root != expected_root {
        return Err(InvocationError::new(
            "job root must contain exactly input-manifest.json and input/",
        ));
    }

    let canonical_manifest = root
        .read_single_link_child(OsStr::new("input-manifest.json"), manifest_bytes)
        .map_err(|_| {
            InvocationError::new(
                "input-manifest.json does not satisfy the single-link file boundary",
            )
        })?;
    let manifest = decode_input_manifest(request, &canonical_manifest)?;
    let (expected_files, expected_directories) = expected_input_tree(&manifest)?;
    let input_directory = root.open_child(OsStr::new("input")).map_err(|_| {
        InvocationError::new("input must be an identity-bound directory, not an alias")
    })?;

    let mut loaded = BTreeMap::new();
    let mut actual_nodes = 0usize;
    load_directory(
        &input_directory,
        "",
        &expected_files,
        &expected_directories,
        &mut loaded,
        &mut actual_nodes,
    )?;
    if loaded.len() != expected_files.len() {
        return Err(InvocationError::new(
            "input tree is missing a manifest-authorized file or directory",
        ));
    }
    let final_root = root
        .child_names(2)
        .map_err(|_| InvocationError::new("cannot recheck the exact job-root closure"))?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if final_root != expected_root {
        return Err(InvocationError::new(
            "job root changed while loading the input closure",
        ));
    }

    finish_resolved_input_job(canonical_manifest, entrypoint, loaded)
}

impl ResolvedInputJob {
    pub(crate) fn into_session_parts(self) -> (String, BTreeMap<String, LoadedInputResource>) {
        let Self {
            canonical_manifest: _,
            entrypoint,
            resources,
        } = self;
        (entrypoint, resources)
    }
}

impl LoadedInputResource {
    pub(crate) fn into_session_parts(self) -> (String, String, u64, Vec<u8>) {
        (
            self.media_type,
            self.content_address,
            self.declared_bytes,
            self.bytes,
        )
    }
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn finish_resolved_input_job(
    canonical_manifest: Vec<u8>,
    entrypoint: String,
    resources: BTreeMap<String, LoadedInputResource>,
) -> Result<ResolvedInputJob, InvocationError> {
    let entrypoint_resource = resources.get(&entrypoint).ok_or_else(|| {
        InvocationError::new("validated input manifest lost its declared entrypoint")
    })?;
    std::str::from_utf8(&entrypoint_resource.bytes).map_err(|_| {
        InvocationError::new(format!(
            "input entrypoint {entrypoint:?} is not valid UTF-8"
        ))
    })?;
    Ok(ResolvedInputJob {
        canonical_manifest,
        entrypoint,
        resources,
    })
}

#[cfg(test)]
pub(crate) fn resolve_input_job_for_test(
    request: &Value,
    canonical_manifest: &[u8],
    mut bodies: BTreeMap<String, Vec<u8>>,
) -> Result<ResolvedInputJob, InvocationError> {
    validate_request(request).map_err(InvocationError::new)?;
    let input = request_input(request)?;
    let entrypoint = required_string(input, "$.input", "entrypoint")
        .map_err(InvocationError::new)?
        .to_owned();
    let manifest = decode_input_manifest(request, canonical_manifest)?;
    let (expected_files, _) = expected_input_tree(&manifest)?;
    let actual_paths = bodies.keys().cloned().collect::<BTreeSet<_>>();
    let expected_paths = expected_files.keys().cloned().collect::<BTreeSet<_>>();
    if actual_paths != expected_paths {
        return Err(InvocationError::new(
            "input bodies must exactly match the manifest-authorized files",
        ));
    }

    let mut resources = BTreeMap::new();
    for (path, expected) in expected_files {
        let bytes = bodies
            .remove(&path)
            .ok_or_else(|| InvocationError::new("input bodies lost a manifest-authorized file"))?;
        if u64::try_from(bytes.len()).ok() != Some(expected.bytes) {
            return Err(InvocationError::new(format!(
                "input file {path:?} does not match its declared byte count"
            )));
        }
        let content_address = format!("sha256:{}", hex_lower(&Sha256::digest(&bytes)));
        if content_address != expected.content_address {
            return Err(InvocationError::new(format!(
                "input file {path:?} does not match its declared SHA-256"
            )));
        }
        resources.insert(
            path,
            LoadedInputResource {
                media_type: expected.media_type,
                content_address,
                declared_bytes: expected.bytes,
                bytes,
            },
        );
    }
    finish_resolved_input_job(canonical_manifest.to_vec(), entrypoint, resources)
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn request_input(request: &Value) -> Result<&serde_json::Map<String, Value>, InvocationError> {
    let request =
        closed_object(request, "$", super::TOP_LEVEL_FIELDS).map_err(InvocationError::new)?;
    closed_object(
        required(request, "$", "input").map_err(InvocationError::new)?,
        "$.input",
        INPUT_FIELDS,
    )
    .map_err(InvocationError::new)
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn expected_input_tree(
    manifest: &Value,
) -> Result<(BTreeMap<String, ExpectedInputResource>, BTreeSet<String>), InvocationError> {
    let entries = manifest["entries"]
        .as_array()
        .ok_or_else(|| InvocationError::new("validated input manifest lost its entry array"))?;
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    for entry in entries {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| InvocationError::new("validated input path is not a string"))?;
        let segments = path.split('/').collect::<Vec<_>>();
        for end in 1..segments.len() {
            directories.insert(segments[..end].join("/"));
        }
        files.insert(
            path.to_owned(),
            ExpectedInputResource {
                media_type: entry["media_type"]
                    .as_str()
                    .ok_or_else(|| {
                        InvocationError::new("validated input media type is not a string")
                    })?
                    .to_owned(),
                content_address: entry["sha256"]
                    .as_str()
                    .ok_or_else(|| {
                        InvocationError::new("validated input content address is not a string")
                    })?
                    .to_owned(),
                bytes: entry["bytes"].as_u64().ok_or_else(|| {
                    InvocationError::new("validated input byte count is not an integer")
                })?,
            },
        );
    }
    Ok((files, directories))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn load_directory(
    directory: &BoundDirectory,
    relative_parent: &str,
    expected_files: &BTreeMap<String, ExpectedInputResource>,
    expected_directories: &BTreeSet<String>,
    loaded: &mut BTreeMap<String, LoadedInputResource>,
    actual_nodes: &mut usize,
) -> Result<(), InvocationError> {
    let remaining_nodes = INPUT_TREE_MAX_NODES.saturating_sub(*actual_nodes);
    let before = directory
        .child_names(remaining_nodes)
        .map_err(|_| InvocationError::new("cannot inspect an input directory closure"))?;
    for name in &before {
        *actual_nodes = actual_nodes
            .checked_add(1)
            .filter(|count| *count <= INPUT_TREE_MAX_NODES)
            .ok_or_else(|| {
                InvocationError::new(format!(
                    "input tree exceeds {INPUT_TREE_MAX_NODES} total files and directories"
                ))
            })?;
        let name = name
            .to_str()
            .ok_or_else(|| InvocationError::new("input tree names must be valid UTF-8"))?;
        let relative = if relative_parent.is_empty() {
            name.to_owned()
        } else {
            format!("{relative_parent}/{name}")
        };
        if expected_directories.contains(&relative) {
            let child = directory.open_child(OsStr::new(name)).map_err(|_| {
                InvocationError::new(format!(
                    "input directory {relative:?} is missing, replaced, or an alias"
                ))
            })?;
            load_directory(
                &child,
                &relative,
                expected_files,
                expected_directories,
                loaded,
                actual_nodes,
            )?;
            continue;
        }

        let expected = expected_files.get(&relative).ok_or_else(|| {
            InvocationError::new(format!(
                "input tree contains an unlisted node at {relative:?}"
            ))
        })?;
        let bytes = directory
            .read_single_link_child(OsStr::new(name), expected.bytes)
            .map_err(|_| {
                InvocationError::new(format!(
                    "input file {relative:?} does not satisfy its file descriptor"
                ))
            })?;
        let content_address = format!("sha256:{}", hex_lower(&Sha256::digest(&bytes)));
        if content_address != expected.content_address {
            return Err(InvocationError::new(format!(
                "input file {relative:?} does not match its declared SHA-256"
            )));
        }
        loaded.insert(
            relative,
            LoadedInputResource {
                media_type: expected.media_type.clone(),
                content_address,
                declared_bytes: expected.bytes,
                bytes,
            },
        );
    }
    let after = directory
        .child_names(before.len())
        .map_err(|_| InvocationError::new("cannot recheck an input directory closure"))?;
    if before != after {
        return Err(InvocationError::new(
            "input directory changed while loading its exact closure",
        ));
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::Value;

    use super::*;
    use crate::api2::decode_render_request;
    use crate::session::create_private_directory;

    const REQUEST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/goldens/accepted/render-request.a4.json"
    ));

    struct Sandbox(PathBuf);

    impl Sandbox {
        fn fixture() -> (Self, PathBuf) {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sandbox = std::env::temp_dir()
                .join(format!("pliego-api2-input-{}-{unique}", std::process::id()));
            fs::create_dir(&sandbox).unwrap();
            let job = sandbox.join("job");
            create_private_directory(&job).unwrap();
            let input = job.join("input");
            fs::create_dir(&input).unwrap();
            fs::create_dir(input.join("assets")).unwrap();

            let fixtures =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/api2/fixtures");
            fs::copy(
                fixtures.join("input-manifest.json"),
                job.join("input-manifest.json"),
            )
            .unwrap();
            for relative in [
                "document.html",
                "styles.css",
                "assets/fixture-font.bin",
                "assets/mark.svg",
            ] {
                fs::copy(fixtures.join("input").join(relative), input.join(relative)).unwrap();
            }
            (Self(sandbox), job)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn request() -> Value {
        decode_render_request(&mut &REQUEST[..]).unwrap()
    }

    #[test]
    fn freezes_the_exact_fixture_into_a_host_path_free_store() {
        let (_sandbox, job) = Sandbox::fixture();
        let loaded = load_input_job(&job, &request()).unwrap();
        assert_eq!(loaded.entrypoint, "document.html");
        assert_eq!(loaded.resources.len(), 4);
        assert_eq!(
            loaded.resources["document.html"].bytes,
            fs::read(job.join("input/document.html")).unwrap()
        );
        assert_eq!(
            loaded.canonical_manifest,
            fs::read(job.join("input-manifest.json")).unwrap()
        );

        fs::write(job.join("input/document.html"), b"changed after freeze").unwrap();
        assert_ne!(
            loaded.resources["document.html"].bytes,
            fs::read(job.join("input/document.html")).unwrap()
        );
    }

    #[test]
    fn rejects_extra_root_and_input_tree_nodes() {
        let (_sandbox, job) = Sandbox::fixture();
        fs::write(job.join(".pliego-status"), b"retained").unwrap();
        assert!(
            load_input_job(&job, &request())
                .unwrap_err()
                .to_string()
                .contains("exact job-root closure")
        );
        fs::remove_file(job.join(".pliego-status")).unwrap();
        fs::create_dir(job.join("input/empty")).unwrap();
        assert!(
            load_input_job(&job, &request())
                .unwrap_err()
                .to_string()
                .contains("unlisted node")
        );
    }

    #[test]
    fn rejects_missing_changed_and_hard_linked_input_files() {
        let (_sandbox, job) = Sandbox::fixture();
        fs::remove_file(job.join("input/styles.css")).unwrap();
        assert!(load_input_job(&job, &request()).is_err());

        let (_sandbox, job) = Sandbox::fixture();
        let path = job.join("input/styles.css");
        let mut changed = fs::read(&path).unwrap();
        changed[0] ^= 1;
        fs::write(&path, changed).unwrap();
        assert!(
            load_input_job(&job, &request())
                .unwrap_err()
                .to_string()
                .contains("does not match its declared SHA-256")
        );

        let (sandbox, job) = Sandbox::fixture();
        let outside = sandbox.0.join("outside-document.html");
        fs::rename(job.join("input/document.html"), &outside).unwrap();
        fs::hard_link(&outside, job.join("input/document.html")).unwrap();
        assert!(load_input_job(&job, &request()).is_err());
    }

    #[test]
    fn rejects_a_hard_linked_manifest() {
        let (sandbox, job) = Sandbox::fixture();
        let outside = sandbox.0.join("outside-input-manifest.json");
        fs::rename(job.join("input-manifest.json"), &outside).unwrap();
        fs::hard_link(&outside, job.join("input-manifest.json")).unwrap();
        assert!(load_input_job(&job, &request()).is_err());
    }

    #[test]
    fn rejects_expected_file_and_input_directory_aliases() {
        use std::os::unix::fs::symlink;

        let (sandbox, job) = Sandbox::fixture();
        let path = job.join("input/styles.css");
        let outside = sandbox.0.join("outside-styles.css");
        fs::rename(&path, &outside).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(load_input_job(&job, &request()).is_err());

        let (sandbox, job) = Sandbox::fixture();
        let input = job.join("input");
        let outside = sandbox.0.join("outside-input");
        fs::rename(&input, &outside).unwrap();
        symlink(&outside, &input).unwrap();
        assert!(load_input_job(&job, &request()).is_err());
    }

    #[test]
    fn rejects_nonprivate_roots_and_blocking_special_files() {
        use std::os::unix::fs::PermissionsExt;

        let (_sandbox, job) = Sandbox::fixture();
        fs::set_permissions(&job, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load_input_job(&job, &request()).is_err());

        let (_sandbox, job) = Sandbox::fixture();
        let path = job.join("input/styles.css");
        fs::remove_file(&path).unwrap();
        let raw = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: raw is a live NUL-terminated pathname and mode is a valid FIFO permission mask.
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(load_input_job(&job, &request()).is_err());
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn cwd_v1_loader_fails_closed_without_descriptor_relative_authority() {
        assert!(
            load_input_job(Path::new("."), &Value::Null)
                .unwrap_err()
                .to_string()
                .contains("requires descriptor-relative filesystem authority")
        );
    }
}
