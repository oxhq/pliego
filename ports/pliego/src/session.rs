/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use embedder_traits::WebResourceLoadRole;
use same_file::Handle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BUNDLE_FILE_NAME: &str = "bundle.json";
const PUBLICATION_DIRECTORY_NAME: &str = "publication";
const PUBLICATION_LEASE_FILE_NAME: &str = "lease";
const PUBLICATION_PLAN_FILE_NAME: &str = "plan.json";
const PUBLICATION_OUTCOME_FILE_NAME: &str = "outcome.json";
const PUBLICATION_PREPARED_FILE_NAME: &str = "prepared.json";
const PUBLICATION_COMMITTED_FILE_NAME: &str = "committed.json";
const MAX_PUBLICATION_BUNDLE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PUBLICATION_OUTCOME_BYTES: u64 = 1024 * 1024;
const MAX_CONTROL_JSON_BYTES: u64 = 1024 * 1024;
/// Supervisor acceptance limits are part of the controlled-runtime boundary, not PDF limits.
/// Callers can distinguish a bounded-closure rejection by downcasting the `io::Error` source.
pub(crate) const MAX_PROMOTION_TREE_DEPTH: usize = 32;
pub(crate) const MAX_PROMOTION_TREE_ENTRIES: u64 = 16 * 1024;
pub(crate) const MAX_PROMOTION_TREE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StagedArtifactLimit {
    Depth,
    Entries,
    AggregateBytes,
}

#[derive(Debug)]
pub(crate) struct StagedArtifactLimitExceeded {
    pub(crate) limit: StagedArtifactLimit,
    pub(crate) maximum: u64,
}

impl fmt::Display for StagedArtifactLimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self.limit {
            StagedArtifactLimit::Depth => "depth",
            StagedArtifactLimit::Entries => "entry count",
            StagedArtifactLimit::AggregateBytes => "aggregate byte count",
        };
        write!(
            formatter,
            "staged artifact tree exceeds the {label} limit of {}",
            self.maximum
        )
    }
}

impl Error for StagedArtifactLimitExceeded {}

fn staged_artifact_limit_error(limit: StagedArtifactLimit, maximum: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        StagedArtifactLimitExceeded { limit, maximum },
    )
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationArtifact {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationPlanReceipt {
    schema: String,
    version: u32,
    transaction_id: String,
    render_id: String,
    request_fingerprint: String,
    artifact_root: String,
    artifact_root_identity: String,
    requested_output: String,
    output: String,
    output_parent_identity: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationPreparedReceipt {
    schema: String,
    version: u32,
    transaction_id: String,
    plan_sha256: String,
    output: PublicationArtifact,
    staging: PublicationArtifact,
    bundle: PublicationArtifact,
    outcome: PublicationArtifact,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationCommittedReceipt {
    schema: String,
    version: u32,
    transaction_id: String,
    prepared_sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PublicationRecoveryState {
    Planned,
    Committed {
        summary: serde_json::Value,
        cli_bytes: Vec<u8>,
        recovered: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedPublicationReceipt {
    sha256: String,
}

/// Owns the process lease and immutable receipt chain for one output publication.
///
/// Receipt links provide atomic visibility. This slice does not claim sudden-power-loss
/// durability; native kill and filesystem durability barriers remain separate proof gates.
#[derive(Debug)]
pub(crate) struct PublicationJournal {
    artifact_root: BoundDirectory,
    output_parent: BoundDirectory,
    directory: BoundDirectory,
    lease: Handle,
    plan: PublicationPlanReceipt,
    plan_sha256: String,
}

pub(crate) fn validate_publication_outcome_bytes(outcome_bytes: &[u8]) -> io::Result<()> {
    if outcome_bytes.len() as u64 > MAX_PUBLICATION_OUTCOME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("publication outcome exceeds the {MAX_PUBLICATION_OUTCOME_BYTES}-byte limit"),
        ));
    }
    if !outcome_bytes.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "publication outcome must end with a newline",
        ));
    }
    serde_json::from_slice::<serde_json::Value>(outcome_bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("publication outcome is invalid JSON: {error}"),
        )
    })?;
    Ok(())
}

impl Drop for PublicationJournal {
    fn drop(&mut self) {
        // Closing only this descriptor can leave a `flock` held by a concurrently forked or
        // duplicated descriptor. Explicitly release the logical owner's lease before close.
        let _ = self.lease.as_file().unlock();
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleEntry {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct BundleManifest<'a> {
    schema: &'static str,
    version: u32,
    render_id: &'a str,
    entries: Vec<BundleEntry>,
    output: BundleEntry,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedBundleManifest {
    schema: String,
    version: u32,
    render_id: String,
    entries: Vec<BundleEntry>,
    output: BundleEntry,
}

#[derive(Debug)]
pub(crate) struct PreparedDocumentPdf {
    destination: PathBuf,
    storage: Option<PreparedDocumentPdfStorage>,
    sha256: String,
    bytes: u64,
}

#[derive(Debug)]
enum PreparedDocumentPdfStorage {
    External {
        publication_destination: PathBuf,
        destination_parent: BoundDirectory,
        staged: OwnedFile,
    },
    Artifact {
        artifact_root: BoundDirectory,
        file: OwnedFile,
    },
}

#[derive(Debug)]
pub(crate) struct PreparedBundle {
    file: Option<OwnedFile>,
    artifact_root: BoundDirectory,
    render_id: String,
    output: BundleEntry,
    sha256: String,
    bytes: u64,
}

#[derive(Debug)]
pub(crate) enum PreparedPublicationError {
    Bundle(io::Error),
    Output(io::Error),
}

#[derive(Debug)]
struct BoundDirectory {
    requested_path: PathBuf,
    path: PathBuf,
    handle: Handle,
    movable: bool,
}

#[derive(Debug)]
struct OwnedFile {
    path: PathBuf,
    handle: Handle,
    remove_on_drop: bool,
}

impl BoundDirectory {
    fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_move_access(path, false)
    }

    fn open_movable(path: PathBuf) -> io::Result<Self> {
        Self::open_with_move_access(path, true)
    }

    fn open_with_move_access(path: PathBuf, movable: bool) -> io::Result<Self> {
        let requested_path = std::path::absolute(path)?;
        require_path_without_aliases(&requested_path)?;
        let path = requested_path.canonicalize()?;
        require_directory_without_symlink(&path)?;
        let handle = Handle::from_file(open_bound_directory_handle(&path, movable)?)?;
        let directory = Self {
            requested_path,
            path,
            handle,
            movable,
        };
        directory.require_current()?;
        Ok(directory)
    }

    fn require_current(&self) -> io::Result<()> {
        for path in [&self.requested_path, &self.path] {
            let metadata = std::fs::symlink_metadata(path)?;
            if path_metadata_is_alias(&metadata) || !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "publication parent is no longer the bound directory: {}",
                        path.display()
                    ),
                ));
            }
            let current = Handle::from_file(open_bound_directory_handle(path, self.movable)?)?;
            if !handles_match(&current, &self.handle)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "publication parent changed after output preparation: {}",
                        path.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn try_clone(&self) -> io::Result<Self> {
        self.require_current()?;
        Ok(Self {
            requested_path: self.requested_path.clone(),
            path: self.path.clone(),
            handle: Handle::from_file(self.handle.as_file().try_clone()?)?,
            movable: self.movable,
        })
    }

    fn identity(&self) -> io::Result<String> {
        self.require_current()?;
        open_file_identity(self.handle.as_file(), &self.path)
    }
}

/// Validate the parent of a requested publication target while retaining API1's absolute requested
/// spelling. Existing final components are deliberately left to create/recovery classification.
pub(crate) fn validated_publication_target(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let parent = absolute.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "session artifact path has no parent directory",
        )
    })?;
    require_path_without_aliases(parent)?;
    Ok(absolute)
}

/// Validate the path-only publication preconditions that do not require a staged artifact root.
///
/// The supervisor runs this before starting Servo so deterministic request/path failures keep the
/// same API1 classification instead of being rewritten as a worker termination after rendering.
/// The real journal still rebinds and revalidates every identity before promotion.
pub(crate) fn preflight_publication_request(
    logical_artifact_root: &Path,
    output: &Path,
) -> io::Result<()> {
    let requested_output = output.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("output path is not valid UTF-8: {}", output.display()),
        )
    })?;
    let output = std::path::absolute(output)?;
    let output_parent_path = output
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output has no parent"))?
        .to_owned();
    output.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("output path is not valid UTF-8: {}", output.display()),
        )
    })?;
    let logical_artifact_root = std::path::absolute(logical_artifact_root)?;
    if output_parent_path != logical_artifact_root {
        let output_parent = BoundDirectory::open(output_parent_path)?;
        output_parent.identity()?;
    }
    receipt_path(&logical_artifact_root)?;
    receipt_path(&output)?;
    // Preserve the same requested-spelling UTF-8 precondition used in the transaction ID.
    let _ = requested_output;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct PromotionTreeClosure {
    entries: u64,
    bytes: u64,
    artifacts: Vec<PromotionTreeEntry>,
}

#[derive(Debug, Eq, PartialEq)]
struct PromotionTreeEntry {
    path: String,
    kind: PromotionTreeEntryKind,
}

#[derive(Debug, Eq, PartialEq)]
enum PromotionTreeEntryKind {
    Directory,
    File { sha256: String, bytes: u64 },
}

/// Validates a bounded private artifact closure without exposing it.
///
/// The stage and its private container spellings are always forbidden inside artifact bytes;
/// callers may add other private path spellings that must not cross the publication boundary.
/// This is deliberately a structural closure check, not an artifact allowlist or schema check.
/// The supervisor must validate the exact success or typed-failure artifact contract first.
pub(crate) fn validate_staged_artifacts(
    staging: &Path,
    forbidden_utf8_prefixes: &[&Path],
) -> io::Result<()> {
    let staging = std::path::absolute(staging)?;
    let staging_name = immediate_child_name(&staging, "staging artifact root")?.to_owned();
    let container_path = staging.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging artifact root has no parent",
        )
    })?;
    let container = BoundDirectory::open(container_path.to_owned())?;
    let staged = BoundDirectory::open_movable(staging.clone())?;
    require_private_promotion_container(&container)?;
    require_immediate_bound_child(&staging, &container, &staging_name)?;
    if promotion_filesystem_id(container.handle.as_file())?
        != promotion_filesystem_id(staged.handle.as_file())?
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private artifact tree and its container must be on the same filesystem",
        ));
    }
    let mut private_paths = vec![
        staging.as_path(),
        staged.path.as_path(),
        container.requested_path.as_path(),
        container.path.as_path(),
    ];
    private_paths.extend_from_slice(forbidden_utf8_prefixes);
    let forbidden_prefixes = promotion_private_prefixes(&private_paths)?;
    validate_promotion_tree(&staged, &forbidden_prefixes).map(|_| ())
}

/// Removes a private source container only when the held identity is still its path target and
/// the directory is empty. A non-empty container is retained for private failure evidence.
pub(crate) fn remove_empty_private_container(container: &Path) -> io::Result<bool> {
    let container = std::path::absolute(container)?;
    let name = immediate_child_name(&container, "private staging container")?.to_owned();
    let parent_path = container.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private staging container has no parent",
        )
    })?;
    let parent = BoundDirectory::open(parent_path.to_owned())?;
    let container = BoundDirectory::open_movable(container)?;
    require_immediate_bound_child(&container.requested_path, &parent, &name)?;
    require_private_promotion_container(&container)?;
    let identity = container.identity()?;
    match remove_empty_bound_directory(&container, &parent, &name) {
        Ok(()) => {},
        Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => return Ok(false),
        Err(error) => return Err(error),
    }
    if open_file_identity(container.handle.as_file(), &container.requested_path)? != identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private staging container identity changed during empty removal",
        ));
    }
    drop(container);
    require_child_absent(&parent, &name, "private staging container")?;
    let _ = parent.handle.as_file().sync_all();
    Ok(true)
}

/// Atomically exposes a closed private artifact tree without replacing an existing root.
///
/// `staging` must be an immediate child of a caller-created private container. On Unix that
/// container must be owned by the effective user and have mode 0700. The private container and
/// `public` parent must be on the same filesystem. The residual race boundary is therefore the
/// current OS principal: another process running as that same principal can mutate its 0700 tree.
/// Every in-tree lease must be dropped before this function is called.
pub(crate) fn promote_staged_artifacts(
    source_container: &Path,
    staging: &Path,
    public: &Path,
) -> io::Result<()> {
    let source_container = std::path::absolute(source_container)?;
    let staging = std::path::absolute(staging)?;
    let public = std::path::absolute(public)?;
    let staging_name = immediate_child_name(&staging, "staging artifact root")?.to_owned();
    let public_name = immediate_child_name(&public, "public artifact root")?.to_owned();
    let staging_parent = staging.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging artifact root has no parent",
        )
    })?;
    if staging_parent != source_container {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging artifact root must be an immediate child of the supplied private container",
        ));
    }
    let destination_parent_path = public.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "public artifact root has no parent",
        )
    })?;

    let source_container = BoundDirectory::open(source_container)?;
    let destination_parent = BoundDirectory::open(destination_parent_path.to_owned())?;
    let staged = BoundDirectory::open_movable(staging.clone())?;
    require_private_promotion_container(&source_container)?;
    require_immediate_bound_child(&staging, &source_container, &staging_name)?;
    require_immediate_bound_child(&public, &destination_parent, &public_name)?;
    if handles_match(&source_container.handle, &destination_parent.handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging artifact root must be inside a separate private container",
        ));
    }
    require_same_promotion_filesystem(&source_container, &staged, &destination_parent)?;

    let source_container_identity = source_container.identity()?;
    let destination_parent_identity = destination_parent.identity()?;
    let staged_identity = staged.identity()?;
    require_child_absent(&destination_parent, &public_name, "public artifact root")?;
    let forbidden_prefixes = promotion_private_prefixes(&[
        &staging,
        &staged.path,
        &source_container.requested_path,
        &source_container.path,
    ])?;
    let before = validate_promotion_tree(&staged, &forbidden_prefixes)?;

    require_bound_identity(&source_container, &source_container_identity)?;
    require_bound_identity(&destination_parent, &destination_parent_identity)?;
    staged.require_current()?;
    rename_bound_directory_no_replace(
        &staged,
        &source_container,
        &staging_name,
        &destination_parent,
        &public_name,
    )?;

    let after = (|| {
        require_bound_identity(&source_container, &source_container_identity)?;
        require_bound_identity(&destination_parent, &destination_parent_identity)?;
        require_child_absent(&source_container, &staging_name, "staging artifact root")?;
        let held_identity = open_file_identity(staged.handle.as_file(), &public)?;
        if held_identity != staged_identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "held staged artifact root identity changed during promotion",
            ));
        }
        let promoted = BoundDirectory::open_movable(public.clone())?;
        if promoted.identity()? != staged_identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "promoted artifact root does not match the held private root",
            ));
        }
        let closure = validate_promotion_tree(&promoted, &forbidden_prefixes)?;
        if closure != before {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "promoted artifact tree changed across atomic exposure",
            ));
        }
        Ok(())
    })();

    if let Err(validation_error) = after {
        let rollback = rename_bound_directory_no_replace(
            &staged,
            &destination_parent,
            &public_name,
            &source_container,
            &staging_name,
        )
        .and_then(|()| {
            require_child_absent(&destination_parent, &public_name, "public artifact root")?;
            staged.require_current()?;
            if staged.identity()? != staged_identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "rolled-back artifact root does not match the held private root",
                ));
            }
            Ok(())
        });
        return match rollback {
            Ok(()) => Err(validation_error),
            Err(rollback_error) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "promoted artifact validation failed ({validation_error}); atomic rollback failed ({rollback_error})"
                ),
            )),
        };
    }

    let _ = source_container.handle.as_file().sync_all();
    let _ = destination_parent.handle.as_file().sync_all();
    Ok(())
}

fn immediate_child_name<'a>(path: &'a Path, label: &str) -> io::Result<&'a OsStr> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} has no final component"),
        )
    })?;
    if !matches!(
        Path::new(name).components().next(),
        Some(Component::Normal(_))
    ) || Path::new(name).components().count() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} is not one normal path component"),
        ));
    }
    Ok(name)
}

fn require_immediate_bound_child(
    path: &Path,
    parent: &BoundDirectory,
    name: &OsStr,
) -> io::Result<()> {
    if path.parent() != Some(parent.requested_path.as_path())
        || path != parent.requested_path.join(name)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "promotion path is not an immediate child of its bound parent: {}",
                path.display()
            ),
        ));
    }
    parent.require_current()
}

fn require_bound_identity(directory: &BoundDirectory, expected: &str) -> io::Result<()> {
    if directory.identity()? == expected {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "bound promotion directory identity changed: {}",
            directory.requested_path.display()
        ),
    ))
}

#[cfg(unix)]
fn require_private_promotion_container(container: &BoundDirectory) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    container.require_current()?;
    let metadata = container.handle.as_file().metadata()?;
    if metadata.mode() & 0o7777 != 0o700 || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "staging container must be owned by the effective user with mode 0700",
        ));
    }
    #[cfg(target_os = "macos")]
    require_no_macos_extended_acl(container.handle.as_file())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_no_macos_extended_acl(file: &File) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::fd::AsRawFd;

    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;

    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut c_void;
        fn acl_free(object: *mut c_void) -> libc::c_int;
    }

    struct OwnedAcl(*mut c_void);
    impl Drop for OwnedAcl {
        fn drop(&mut self) {
            // SAFETY: acl_get_fd_np returned this releasable ACL allocation.
            let _ = unsafe { acl_free(self.0) };
        }
    }

    // Darwin reports a missing FILESEC_ACL property as a null ACL with ENOENT.
    unsafe { *libc::__error() = 0 };
    // SAFETY: the file descriptor remains live and ACL_TYPE_EXTENDED is the Darwin ACL type.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        // SAFETY: __error returns this thread's errno location.
        let error = unsafe { *libc::__error() };
        return if matches!(error, 0 | libc::ENOENT) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error))
        };
    }
    let _acl = OwnedAcl(acl);
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private staging directory may not inherit an extended ACL",
    ))
}

#[cfg(windows)]
fn require_private_promotion_container(container: &BoundDirectory) -> io::Result<()> {
    container.require_current()?;
    require_windows_private_directory(container.handle.as_file())
}

#[cfg(windows)]
struct WindowsUserSid {
    storage: Vec<usize>,
}

#[cfg(windows)]
impl WindowsUserSid {
    fn as_ptr(&self) -> windows_sys::Win32::Security::PSID {
        self.storage.as_ptr().cast_mut().cast()
    }
}

#[cfg(windows)]
fn current_process_user_sid() -> io::Result<WindowsUserSid> {
    use std::mem::size_of;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by OpenProcessToken and is closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    let mut token = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a process pseudo-handle and token points to writable storage.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    let mut required = 0_u32;
    // SAFETY: the zero-length query intentionally supplies a null output to obtain the size.
    let first =
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut required) };
    if first != 0 || required < size_of::<TOKEN_USER>() as u32 {
        return Err(if first == 0 {
            io::Error::last_os_error()
        } else {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "process token returned an invalid user-information size",
            )
        });
    }
    let word_count = (required as usize).div_ceil(size_of::<usize>());
    let mut token_storage = vec![0_usize; word_count];
    // SAFETY: the aligned buffer has at least `required` bytes and the token remains live.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            token_storage.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful TokenUser query initializes a TOKEN_USER at the start of the buffer.
    let source = unsafe { (*(token_storage.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    // SAFETY: TokenUser supplies a valid SID for the lifetime of token_storage.
    let sid_bytes = unsafe { GetLengthSid(source) };
    if sid_bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid_words = (sid_bytes as usize).div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; sid_words];
    // SAFETY: both buffers are valid for sid_bytes and do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(
            source.cast::<u8>(),
            storage.as_mut_ptr().cast::<u8>(),
            sid_bytes as usize,
        );
    }
    Ok(WindowsUserSid { storage })
}

#[cfg(windows)]
fn require_windows_private_directory(file: &File) -> io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, CONTAINER_INHERIT_ACE,
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetSecurityDescriptorControl, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    struct OwnedSecurityDescriptor(PSECURITY_DESCRIPTOR);
    impl Drop for OwnedSecurityDescriptor {
        fn drop(&mut self) {
            // SAFETY: GetSecurityInfo allocated the descriptor with LocalAlloc.
            let _ = unsafe { LocalFree(self.0) };
        }
    }

    let expected_user = current_process_user_sid()?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the directory handle includes READ_CONTROL and all output pointers are writable.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as _,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = OwnedSecurityDescriptor(descriptor);
    if owner.is_null() || dacl.is_null() || descriptor.0.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "staging container has no protected owner-only DACL",
        ));
    }
    // SAFETY: both SIDs are valid while their owning buffers remain live.
    if unsafe { EqualSid(owner, expected_user.as_ptr()) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "staging container is not owned by the current user",
        ));
    }
    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: descriptor is live and both scalar outputs are writable.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "staging container DACL is not protected",
        ));
    }
    let mut acl_information = ACL_SIZE_INFORMATION::default();
    // SAFETY: dacl is part of the live descriptor and output has the advertised size.
    if unsafe {
        GetAclInformation(
            dacl,
            (&raw mut acl_information).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if acl_information.AceCount != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "staging container DACL must grant exactly one principal",
        ));
    }
    let mut ace = std::ptr::null_mut();
    // SAFETY: the ACL reports one ACE and ace points to writable pointer storage.
    if unsafe { GetAce(dacl, 0, &mut ace) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
    let expected_flags = (CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE) as u8;
    // SAFETY: GetAce returned a live ACCESS_ALLOWED_ACE-sized record from the ACL.
    let valid = unsafe {
        (*ace).Header.AceType == 0
            && (*ace).Header.AceFlags == expected_flags
            && (*ace).Mask == FILE_ALL_ACCESS
            && EqualSid(
                std::ptr::addr_of_mut!((*ace).SidStart).cast(),
                expected_user.as_ptr(),
            ) != 0
    };
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "staging container DACL is not an owner-only full-access grant",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn require_private_promotion_container(_container: &BoundDirectory) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private staged artifact containers are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn promotion_filesystem_id(file: &File) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;

    Ok(file.metadata()?.dev())
}

#[cfg(windows)]
fn promotion_filesystem_id(file: &File) -> io::Result<u64> {
    Ok(windows_file_identity(file)?.0)
}

#[cfg(not(any(unix, windows)))]
fn promotion_filesystem_id(_file: &File) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "staged artifact filesystem identity is unsupported on this platform",
    ))
}

fn require_same_promotion_filesystem(
    source_container: &BoundDirectory,
    staged: &BoundDirectory,
    destination_parent: &BoundDirectory,
) -> io::Result<()> {
    let expected = promotion_filesystem_id(destination_parent.handle.as_file())?;
    for directory in [source_container, staged] {
        if promotion_filesystem_id(directory.handle.as_file())? != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private artifact tree and public parent must be on the same filesystem",
            ));
        }
    }
    Ok(())
}

fn promotion_private_prefixes(paths: &[&Path]) -> io::Result<Vec<Vec<u8>>> {
    let mut prefixes = Vec::new();
    for path in paths {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let raw = path.as_os_str().as_bytes();
            prefixes.push(raw.to_vec());
            prefixes.push(
                raw.iter()
                    .map(|byte| if *byte == b'\\' { b'/' } else { *byte })
                    .collect(),
            );
            prefixes.push(
                raw.iter()
                    .map(|byte| if *byte == b'/' { b'\\' } else { *byte })
                    .collect(),
            );
        }

        // Public JSON and diagnostics use a UTF-8 spelling. Keep scanning that representation on
        // every platform, including the replacement-character spelling of an otherwise non-UTF-8
        // OS path, while Unix additionally scans the lossless raw bytes above.
        let value = path.to_string_lossy();
        let mut candidates = Vec::new();
        append_private_utf8_spellings(&mut candidates, &value);
        if let Some(leaf) = path.file_name() {
            let leaf = leaf.to_string_lossy();
            if private_leaf_token(&leaf) {
                candidates.push(leaf.into_owned());
            }
        }
        #[cfg(windows)]
        for alias in windows_short_path_aliases(path)? {
            append_private_utf8_spellings(&mut candidates, &alias.to_string_lossy());
        }
        for candidate in candidates {
            if !candidate.is_empty() {
                prefixes.push(candidate.as_bytes().to_vec());
                let json = serde_json::to_string(&candidate).map_err(io::Error::other)?;
                prefixes.push(json[1..json.len() - 1].as_bytes().to_vec());
            }
        }
    }
    prefixes.sort();
    prefixes.dedup();
    Ok(prefixes)
}

fn append_private_utf8_spellings(candidates: &mut Vec<String>, value: &str) {
    candidates.push(value.to_owned());
    candidates.push(value.replace('\\', "/"));
    candidates.push(value.replace('/', "\\"));
}

fn private_leaf_token(value: &str) -> bool {
    let Some(nonce) = value.strip_prefix(".pliego-runtime-") else {
        return false;
    };
    matches!(nonce.len(), 32 | 64)
        && nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn validate_promotion_tree(
    root: &BoundDirectory,
    forbidden_prefixes: &[Vec<u8>],
) -> io::Result<PromotionTreeClosure> {
    root.require_current()?;
    let filesystem = promotion_filesystem_id(root.handle.as_file())?;
    let mut closure = PromotionTreeClosure {
        entries: 0,
        bytes: 0,
        artifacts: Vec::new(),
    };
    validate_promotion_directory(root, "", 0, filesystem, forbidden_prefixes, &mut closure)?;
    root.require_current()?;
    Ok(closure)
}

fn validate_promotion_directory(
    directory: &BoundDirectory,
    relative_parent: &str,
    depth: usize,
    filesystem: u64,
    forbidden_prefixes: &[Vec<u8>],
    closure: &mut PromotionTreeClosure,
) -> io::Result<()> {
    if depth > MAX_PROMOTION_TREE_DEPTH {
        return Err(staged_artifact_limit_error(
            StagedArtifactLimit::Depth,
            MAX_PROMOTION_TREE_DEPTH as u64,
        ));
    }
    directory.require_current()?;
    if promotion_filesystem_id(directory.handle.as_file())? != filesystem {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "staged artifact tree crosses a filesystem boundary: {}",
                directory.requested_path.display()
            ),
        ));
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&directory.requested_path)? {
        entries.push(entry?);
        if entries.len() as u64 > MAX_PROMOTION_TREE_ENTRIES {
            return Err(staged_artifact_limit_error(
                StagedArtifactLimit::Entries,
                MAX_PROMOTION_TREE_ENTRIES,
            ));
        }
    }
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        closure.entries = closure.entries.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "staged artifact entry count overflow",
            )
        })?;
        if closure.entries > MAX_PROMOTION_TREE_ENTRIES {
            return Err(staged_artifact_limit_error(
                StagedArtifactLimit::Entries,
                MAX_PROMOTION_TREE_ENTRIES,
            ));
        }
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "staged artifact names must be valid UTF-8",
            )
        })?;
        let relative = if relative_parent.is_empty() {
            name.to_owned()
        } else {
            format!("{relative_parent}/{name}")
        };
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if path_metadata_is_alias(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "staged artifact tree may not contain symlinks or reparse points: {}",
                    path.display()
                ),
            ));
        }
        if metadata.is_dir() {
            let child = BoundDirectory::open(path)?;
            if promotion_filesystem_id(child.handle.as_file())? != filesystem {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "staged artifact tree crosses a filesystem boundary: {}",
                        child.requested_path.display()
                    ),
                ));
            }
            closure.artifacts.push(PromotionTreeEntry {
                path: relative.clone(),
                kind: PromotionTreeEntryKind::Directory,
            });
            validate_promotion_directory(
                &child,
                &relative,
                depth + 1,
                filesystem,
                forbidden_prefixes,
                closure,
            )?;
            child.require_current()?;
            continue;
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "staged artifact tree may only contain regular files and directories: {}",
                    path.display()
                ),
            ));
        }

        let file = File::open(&path)?;
        let handle = Handle::from_file(file)?;
        if !path_matches_handle(&path, &handle)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "staged artifact path changed while opening: {}",
                    path.display()
                ),
            ));
        }
        let before = handle.as_file().metadata()?;
        require_single_link_regular_file(handle.as_file(), &before, &path)?;
        if promotion_filesystem_id(handle.as_file())? != filesystem {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "staged artifact file is on another filesystem: {}",
                    path.display()
                ),
            ));
        }
        let next_bytes = closure.bytes.checked_add(before.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "staged artifact byte count overflow",
            )
        })?;
        if next_bytes > MAX_PROMOTION_TREE_BYTES {
            return Err(staged_artifact_limit_error(
                StagedArtifactLimit::AggregateBytes,
                MAX_PROMOTION_TREE_BYTES,
            ));
        }
        let (sha256, bytes) =
            hash_promotion_file(handle.as_file(), &path, before.len(), forbidden_prefixes)?;
        let after = handle.as_file().metadata()?;
        require_single_link_regular_file(handle.as_file(), &after, &path)?;
        if bytes != before.len()
            || after.len() != before.len()
            || !path_matches_handle(&path, &handle)?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "staged artifact changed during validation: {}",
                    path.display()
                ),
            ));
        }
        closure.bytes = next_bytes;
        closure.artifacts.push(PromotionTreeEntry {
            path: relative,
            kind: PromotionTreeEntryKind::File { sha256, bytes },
        });
    }
    directory.require_current()
}

#[cfg(unix)]
fn require_single_link_regular_file(
    _file: &File,
    metadata: &std::fs::Metadata,
    path: &Path,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.is_file() && metadata.nlink() == 1 {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "staged artifact must be a single-link regular file: {}",
            path.display()
        ),
    ))
}

#[cfg(windows)]
fn require_single_link_regular_file(
    file: &File,
    metadata: &std::fs::Metadata,
    path: &Path,
) -> io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
    };

    let mut information = FILE_STANDARD_INFO::default();
    // SAFETY: the file handle remains live and the output buffer has the exact advertised size.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileStandardInfo,
            (&raw mut information).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    if metadata.is_file() && information.NumberOfLinks == 1 {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "staged artifact must be a single-link regular file: {}",
            path.display()
        ),
    ))
}

#[cfg(not(any(unix, windows)))]
fn require_single_link_regular_file(
    _file: &File,
    _metadata: &std::fs::Metadata,
    _path: &Path,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "regular-file link-count validation is unsupported on this platform",
    ))
}

fn hash_promotion_file(
    file: &File,
    path: &Path,
    expected_bytes: u64,
    forbidden_prefixes: &[Vec<u8>],
) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut tail = Vec::new();
    let longest_prefix = forbidden_prefixes.iter().map(Vec::len).max().unwrap_or(0);
    let scans_decoded_json = matches!(
        path.extension().and_then(OsStr::to_str),
        Some("json" | "jsonl")
    );
    let mut json_scanner =
        scans_decoded_json.then(|| JsonPrivatePathScanner::new(forbidden_prefixes));
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let remaining = expected_bytes.saturating_sub(bytes);
        let bounded_read = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = read_open_file_at(file, &mut buffer[..bounded_read], bytes)?;
        if read == 0 {
            break;
        }
        if bytes.saturating_add(read as u64) > expected_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("staged artifact grew during validation: {}", path.display()),
            ));
        }
        let chunk = &buffer[..read];
        hasher.update(chunk);
        let mut searchable = Vec::with_capacity(tail.len() + chunk.len());
        searchable.extend_from_slice(&tail);
        searchable.extend_from_slice(chunk);
        if forbidden_prefixes
            .iter()
            .any(|prefix| contains_private_fragment_bytes(&searchable, prefix))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "staged artifact contains a private staging path: {}",
                    path.display()
                ),
            ));
        }
        if let Some(scanner) = &mut json_scanner {
            scanner.feed(chunk, path)?;
        }
        let retain = longest_prefix.saturating_sub(1).min(searchable.len());
        tail.clear();
        tail.extend_from_slice(&searchable[searchable.len() - retain..]);
        bytes = bytes.checked_add(read as u64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "staged artifact byte count overflow",
            )
        })?;
    }
    if let Some(scanner) = json_scanner {
        scanner.finish(path)?;
    }
    Ok((
        format!("sha256:{}", lowercase_hex(&hasher.finalize())),
        bytes,
    ))
}

struct JsonPrivatePathScanner<'a> {
    forbidden_prefixes: &'a [Vec<u8>],
    longest_prefix: usize,
    tail: std::collections::VecDeque<u8>,
    in_string: bool,
    escaped: bool,
    unicode_digits: u8,
    unicode_value: u16,
    pending_high_surrogate: Option<u16>,
}

impl<'a> JsonPrivatePathScanner<'a> {
    fn new(forbidden_prefixes: &'a [Vec<u8>]) -> Self {
        Self {
            forbidden_prefixes,
            longest_prefix: forbidden_prefixes.iter().map(Vec::len).max().unwrap_or(0),
            tail: std::collections::VecDeque::new(),
            in_string: false,
            escaped: false,
            unicode_digits: 0,
            unicode_value: 0,
            pending_high_surrogate: None,
        }
    }

    fn feed(&mut self, bytes: &[u8], path: &Path) -> io::Result<()> {
        for byte in bytes {
            if self.unicode_digits != 0 {
                let digit = match byte {
                    b'0'..=b'9' => u16::from(*byte - b'0'),
                    b'a'..=b'f' => u16::from(*byte - b'a' + 10),
                    b'A'..=b'F' => u16::from(*byte - b'A' + 10),
                    _ => return Err(invalid_json_escape(path)),
                };
                self.unicode_value = (self.unicode_value << 4) | digit;
                self.unicode_digits -= 1;
                if self.unicode_digits == 0 {
                    self.append_unicode_escape(path)?;
                    self.escaped = false;
                }
                continue;
            }
            if !self.in_string {
                if *byte == b'"' {
                    self.in_string = true;
                    self.tail.clear();
                    self.pending_high_surrogate = None;
                }
                continue;
            }
            if self.escaped {
                match byte {
                    b'"' | b'\\' | b'/' => self.append_decoded(&[*byte], path)?,
                    b'b' => self.append_decoded(&[0x08], path)?,
                    b'f' => self.append_decoded(&[0x0c], path)?,
                    b'n' => self.append_decoded(b"\n", path)?,
                    b'r' => self.append_decoded(b"\r", path)?,
                    b't' => self.append_decoded(b"\t", path)?,
                    b'u' => {
                        self.unicode_digits = 4;
                        self.unicode_value = 0;
                        continue;
                    },
                    _ => return Err(invalid_json_escape(path)),
                }
                self.escaped = false;
                continue;
            }
            match byte {
                b'\\' => self.escaped = true,
                b'"' => {
                    if self.pending_high_surrogate.is_some() {
                        return Err(invalid_json_escape(path));
                    }
                    self.in_string = false;
                    self.tail.clear();
                },
                0x00..=0x1f => return Err(invalid_json_escape(path)),
                _ => self.append_decoded(&[*byte], path)?,
            }
        }
        Ok(())
    }

    fn append_unicode_escape(&mut self, path: &Path) -> io::Result<()> {
        let code = self.unicode_value;
        self.unicode_value = 0;
        match code {
            0xd800..=0xdbff => {
                if self.pending_high_surrogate.replace(code).is_some() {
                    return Err(invalid_json_escape(path));
                }
                Ok(())
            },
            0xdc00..=0xdfff => {
                let high = self
                    .pending_high_surrogate
                    .take()
                    .ok_or_else(|| invalid_json_escape(path))?;
                let scalar =
                    0x10000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(code) - 0xdc00);
                self.append_scalar(scalar, path)
            },
            _ => {
                if self.pending_high_surrogate.is_some() {
                    return Err(invalid_json_escape(path));
                }
                self.append_scalar(u32::from(code), path)
            },
        }
    }

    fn append_scalar(&mut self, scalar: u32, path: &Path) -> io::Result<()> {
        let value = char::from_u32(scalar).ok_or_else(|| invalid_json_escape(path))?;
        let mut encoded = [0_u8; 4];
        self.append_decoded(value.encode_utf8(&mut encoded).as_bytes(), path)
    }

    fn append_decoded(&mut self, bytes: &[u8], path: &Path) -> io::Result<()> {
        for byte in bytes {
            self.tail.push_back(*byte);
            if self.tail.len() > self.longest_prefix {
                self.tail.pop_front();
            }
            if self
                .forbidden_prefixes
                .iter()
                .any(|prefix| deque_ends_with_private_fragment(&self.tail, prefix))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "staged JSON artifact contains a decoded private staging path: {}",
                        path.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn finish(self, path: &Path) -> io::Result<()> {
        if self.in_string
            || self.escaped
            || self.unicode_digits != 0
            || self.pending_high_surrogate.is_some()
        {
            return Err(invalid_json_escape(path));
        }
        Ok(())
    }
}

fn contains_private_fragment_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let first = needle[0];
    let mut offset = 0;
    while offset + needle.len() <= haystack.len() {
        let last_start = haystack.len() - needle.len();
        let Some(relative) = haystack[offset..=last_start]
            .iter()
            .position(|byte| *byte == first || cfg!(windows) && byte.eq_ignore_ascii_case(&first))
        else {
            return false;
        };
        offset += relative;
        let candidate = &haystack[offset..offset + needle.len()];
        if candidate == needle || cfg!(windows) && candidate.eq_ignore_ascii_case(needle) {
            return true;
        }
        offset += 1;
    }
    false
}

fn deque_ends_with_private_fragment(
    haystack: &std::collections::VecDeque<u8>,
    needle: &[u8],
) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .iter()
            .rev()
            .zip(needle.iter().rev())
            .all(|(candidate, expected)| {
                candidate == expected || cfg!(windows) && candidate.eq_ignore_ascii_case(expected)
            })
}

fn invalid_json_escape(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "staged JSON artifact has invalid string escaping: {}",
            path.display()
        ),
    )
}

#[cfg(unix)]
fn require_child_absent(parent: &BoundDirectory, name: &OsStr, label: &str) -> io::Result<()> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    parent.require_current()?;
    let name = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} contains a NUL byte"),
        )
    })?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the parent descriptor and C string remain live; metadata points to writable storage.
    let result = unsafe {
        libc::fstatat(
            parent.handle.as_file().as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{label} already exists"),
        ));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(windows)]
fn require_child_absent(parent: &BoundDirectory, name: &OsStr, label: &str) -> io::Result<()> {
    parent.require_current()?;
    match std::fs::symlink_metadata(parent.requested_path.join(name)) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{label} already exists"),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(any(unix, windows)))]
fn require_child_absent(_parent: &BoundDirectory, _name: &OsStr, _label: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "relative child validation is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn remove_empty_bound_directory(
    directory: &BoundDirectory,
    parent: &BoundDirectory,
    name: &OsStr,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    directory.require_current()?;
    parent.require_current()?;
    let name = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private container name contains a NUL byte",
        )
    })?;
    // SAFETY: the held parent descriptor and normal basename remain live for the unlinkat call.
    let result = unsafe {
        libc::unlinkat(
            parent.handle.as_file().as_raw_fd(),
            name.as_ptr(),
            libc::AT_REMOVEDIR,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn remove_empty_bound_directory(
    directory: &BoundDirectory,
    parent: &BoundDirectory,
    _name: &OsStr,
) -> io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    if !directory.movable {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private container was not opened with Windows delete access",
        ));
    }
    directory.require_current()?;
    parent.require_current()?;
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the identity-bound directory handle remains live and the disposition buffer has
    // the exact advertised size. Windows rejects this operation while the directory is non-empty.
    let succeeded = unsafe {
        SetFileInformationByHandle(
            directory.handle.as_file().as_raw_handle() as _,
            FileDispositionInfo,
            (&raw const disposition).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn remove_empty_bound_directory(
    _directory: &BoundDirectory,
    _parent: &BoundDirectory,
    _name: &OsStr,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "identity-bound private container removal is unsupported on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn rename_bound_directory_no_replace(
    _source: &BoundDirectory,
    source_parent: &BoundDirectory,
    source_name: &OsStr,
    destination_parent: &BoundDirectory,
    destination_name: &OsStr,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let source_name = CString::new(source_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging name contains a NUL byte",
        )
    })?;
    let destination_name = CString::new(destination_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "public name contains a NUL byte",
        )
    })?;
    // SAFETY: both names and held parent descriptors remain live for the syscall. RENAME_NOREPLACE
    // makes destination creation exclusive; ENOSYS or unsupported-filesystem errors fail closed.
    let result = unsafe {
        libc::renameat2(
            source_parent.handle.as_file().as_raw_fd(),
            source_name.as_ptr(),
            destination_parent.handle.as_file().as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_bound_directory_no_replace(
    _source: &BoundDirectory,
    source_parent: &BoundDirectory,
    source_name: &OsStr,
    destination_parent: &BoundDirectory,
    destination_name: &OsStr,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let source_name = CString::new(source_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging name contains a NUL byte",
        )
    })?;
    let destination_name = CString::new(destination_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "public name contains a NUL byte",
        )
    })?;
    // SAFETY: both names and held parent descriptors remain live for the call. RENAME_EXCL
    // forbids replacement and there is intentionally no path-based fallback.
    let result = unsafe {
        libc::renameatx_np(
            source_parent.handle.as_file().as_raw_fd(),
            source_name.as_ptr(),
            destination_parent.handle.as_file().as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_bound_directory_no_replace(
    source: &BoundDirectory,
    _source_parent: &BoundDirectory,
    _source_name: &OsStr,
    destination_parent: &BoundDirectory,
    destination_name: &OsStr,
) -> io::Result<()> {
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    if !source.movable {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staged directory was not opened with Windows move access",
        ));
    }
    let destination_name: Vec<u16> = destination_name.encode_wide().collect();
    if destination_name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "public name contains an embedded NUL",
        ));
    }
    let name_bytes = destination_name
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "public name is too long"))?;
    let information_bytes = offset_of!(FILE_RENAME_INFORMATION, FileName)
        .checked_add(name_bytes as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "public name is too long"))?
        .max(size_of::<FILE_RENAME_INFORMATION>());
    let information_length = u32::try_from(information_bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "public name is too long"))?;
    let word_count = information_bytes.div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; word_count];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();

    // SAFETY: storage is aligned and sized for the fixed header plus UTF-16 tail. The movable
    // source handle and destination-parent root handle remain owned for the whole call.
    let status = unsafe {
        (*information).Anonymous.ReplaceIfExists = false;
        (*information).RootDirectory = destination_parent.handle.as_file().as_raw_handle() as _;
        (*information).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            destination_name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            destination_name.len(),
        );
        let mut io_status = IO_STATUS_BLOCK::default();
        NtSetInformationFile(
            source.handle.as_file().as_raw_handle() as _,
            &mut io_status,
            information.cast(),
            information_length,
            FileRenameInformation,
        )
    };
    if status < 0 {
        // SAFETY: this is a pure status-code conversion.
        let windows_error = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(windows_error as i32));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rename_bound_directory_no_replace(
    _source: &BoundDirectory,
    _source_parent: &BoundDirectory,
    _source_name: &OsStr,
    _destination_parent: &BoundDirectory,
    _destination_name: &OsStr,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace artifact promotion is unsupported on this platform",
    ))
}

impl OwnedFile {
    fn bind(path: PathBuf, file: File) -> io::Result<Self> {
        let handle = Handle::from_file(file)?;
        let owned = Self {
            path,
            handle,
            remove_on_drop: true,
        };
        owned.require_current()?;
        Ok(owned)
    }

    fn bind_retained(path: PathBuf, file: File) -> io::Result<Self> {
        let handle = Handle::from_file(file)?;
        let owned = Self {
            path,
            handle,
            remove_on_drop: false,
        };
        owned.require_current()?;
        Ok(owned)
    }

    fn require_current(&self) -> io::Result<()> {
        if path_matches_handle(&self.path, &self.handle)? {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "owned file path changed after creation: {}",
                self.path.display()
            ),
        ))
    }

    fn verify_content(&self, expected_sha256: &str, expected_bytes: u64) -> io::Result<()> {
        let (sha256, bytes) = hash_open_file(self.handle.as_file(), &self.path)?;
        if sha256 != expected_sha256 || bytes != expected_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "owned file bytes changed after creation: {}",
                    self.path.display()
                ),
            ));
        }
        Ok(())
    }

    fn remove(mut self) -> io::Result<()> {
        self.remove_on_drop = false;
        remove_owned_file(&self.path, &self.handle)
    }

    fn preserve(mut self) {
        self.remove_on_drop = false;
    }
}

impl Drop for OwnedFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = remove_owned_file(&self.path, &self.handle);
        }
    }
}

impl PreparedDocumentPdf {
    fn bundle_entry(&self) -> io::Result<BundleEntry> {
        let path = self.destination.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "published output path is not valid UTF-8: {}",
                    self.destination.display()
                ),
            )
        })?;
        Ok(BundleEntry {
            path: path.to_owned(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        })
    }

    fn verify_staged(&self) -> io::Result<()> {
        match self.storage.as_ref().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External {
                destination_parent,
                staged,
                ..
            } => {
                destination_parent.require_current()?;
                staged.verify_content(&self.sha256, self.bytes)
            },
            PreparedDocumentPdfStorage::Artifact {
                artifact_root,
                file,
            } => {
                artifact_root.require_current()?;
                file.require_current()?;
                file.verify_content(&self.sha256, self.bytes)?;
                file.require_current()?;
                artifact_root.require_current()
            },
        }
    }

    /// Make the prepared bytes visible at the caller-owned path without replacing it.
    ///
    /// A successful handle-source link makes the output visible. The caller must then
    /// record the committed receipt or report that transaction recovery is required.
    pub(crate) fn commit(
        &mut self,
        bundle: &PreparedBundle,
    ) -> Result<(), PreparedPublicationError> {
        self.verify_staged()
            .map_err(PreparedPublicationError::Output)?;
        bundle.verify().map_err(PreparedPublicationError::Bundle)?;
        self.commit_validated()
            .map_err(PreparedPublicationError::Output)?;
        self.storage.take();
        Ok(())
    }

    #[cfg(test)]
    fn commit_for_test(mut self) -> io::Result<()> {
        self.verify_staged()?;
        self.commit_validated()
    }

    fn commit_validated(&mut self) -> io::Result<()> {
        match self.storage.as_ref().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External {
                publication_destination,
                destination_parent,
                staged,
            } => {
                if publication_destination.try_exists()? {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "output already exists: {}",
                            publication_destination.display()
                        ),
                    ));
                }
                destination_parent.require_current()?;

                // This must remain the final fallible output-visibility action. The
                // platform implementation names the held file through the held destination
                // directory and never resolves the mutable staging pathname.
                publish_owned_file(staged, destination_parent, publication_destination)?;
                let _ = destination_parent.handle.as_file().sync_all();
                Ok(())
            },
            PreparedDocumentPdfStorage::Artifact {
                artifact_root,
                file,
            } => {
                artifact_root.require_current()?;
                file.require_current()?;
                file.verify_content(&self.sha256, self.bytes)?;
                file.require_current()?;
                artifact_root.require_current()
            },
        }
    }

    fn publication_artifact(&self) -> io::Result<PublicationArtifact> {
        let path = match self.storage.as_ref().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External {
                publication_destination,
                ..
            } => publication_destination,
            PreparedDocumentPdfStorage::Artifact { file, .. } => &file.path,
        };
        Ok(PublicationArtifact {
            path: receipt_path(path)?,
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        })
    }

    fn staging_artifact(&self) -> io::Result<PublicationArtifact> {
        let file = match self.storage.as_ref().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External { staged, .. } => staged,
            PreparedDocumentPdfStorage::Artifact { file, .. } => file,
        };
        Ok(PublicationArtifact {
            path: receipt_path(&file.path)?,
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        })
    }

    fn output_parent_identity(&self) -> io::Result<String> {
        match self.storage.as_ref().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External {
                destination_parent, ..
            } => destination_parent.identity(),
            PreparedDocumentPdfStorage::Artifact { artifact_root, .. } => artifact_root.identity(),
        }
    }

    pub(crate) fn preserve_for_recovery(mut self) {
        match self.storage.take().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External { staged, .. } => staged.preserve(),
            PreparedDocumentPdfStorage::Artifact { file, .. } => file.preserve(),
        }
    }

    #[cfg(test)]
    fn prepared_file_mut(&mut self) -> &mut OwnedFile {
        match self.storage.as_mut().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External { staged, .. } => staged,
            PreparedDocumentPdfStorage::Artifact { file, .. } => file,
        }
    }

    #[cfg(test)]
    fn prepared_file_path(&self) -> &Path {
        match self.storage.as_ref().expect("prepared output is present") {
            PreparedDocumentPdfStorage::External { staged, .. } => &staged.path,
            PreparedDocumentPdfStorage::Artifact { file, .. } => &file.path,
        }
    }
}

impl PreparedBundle {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.file.as_ref().expect("prepared bundle is present").path
    }

    pub(crate) fn discard(mut self) -> io::Result<()> {
        if let Err(error) = self.artifact_root.require_current() {
            self.file
                .take()
                .expect("prepared bundle is present")
                .preserve();
            return Err(error);
        }
        self.file
            .take()
            .expect("prepared bundle is present")
            .remove()
    }

    pub(crate) fn verify(&self) -> io::Result<()> {
        self.artifact_root.require_current()?;
        let file = self.file.as_ref().expect("prepared bundle is present");
        file.require_current()?;
        verify_bundle_closure(
            &self.artifact_root,
            &self.publication_artifact()?,
            Some(file),
            &self.render_id,
            &self.output,
        )?;
        file.require_current()?;
        self.artifact_root.require_current()
    }

    fn matches_output(&self, output: &PublicationArtifact, requested_output: &str) -> bool {
        self.output.path == requested_output
            && self.output.sha256 == output.sha256
            && self.output.bytes == output.bytes
    }

    fn publication_artifact(&self) -> io::Result<PublicationArtifact> {
        let file = self.file.as_ref().expect("prepared bundle is present");
        Ok(PublicationArtifact {
            path: receipt_path(&file.path)?,
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        })
    }

    pub(crate) fn preserve(mut self) {
        self.file
            .take()
            .expect("prepared bundle is present")
            .preserve();
    }
}

impl Drop for PreparedBundle {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        if self.artifact_root.require_current().is_ok() {
            let _ = file.remove();
        } else {
            file.preserve();
        }
    }
}

impl PublicationJournal {
    fn begin(
        artifact_root: &BoundDirectory,
        logical_artifact_root: &Path,
        render_id: &str,
        request_fingerprint: &str,
        output: &Path,
    ) -> io::Result<Self> {
        let (plan, output_parent) = Self::expected_plan(
            artifact_root,
            logical_artifact_root,
            render_id,
            request_fingerprint,
            output,
        )?;
        let publication_directory = artifact_root
            .requested_path
            .join(PUBLICATION_DIRECTORY_NAME);
        create_private_directory(&publication_directory)?;
        let directory = BoundDirectory::open(publication_directory)?;
        artifact_root.require_current()?;
        Self::open(artifact_root, output_parent, directory, plan, true)
    }

    fn resume(
        artifact_root: &BoundDirectory,
        logical_artifact_root: &Path,
        render_id: &str,
        request_fingerprint: &str,
        output: &Path,
    ) -> io::Result<Self> {
        let (plan, output_parent) = Self::expected_plan(
            artifact_root,
            logical_artifact_root,
            render_id,
            request_fingerprint,
            output,
        )?;
        let directory = BoundDirectory::open(
            artifact_root
                .requested_path
                .join(PUBLICATION_DIRECTORY_NAME),
        )?;
        artifact_root.require_current()?;
        Self::open(artifact_root, output_parent, directory, plan, false)
    }

    fn expected_plan(
        artifact_root: &BoundDirectory,
        logical_artifact_root: &Path,
        render_id: &str,
        request_fingerprint: &str,
        output: &Path,
    ) -> io::Result<(PublicationPlanReceipt, BoundDirectory)> {
        artifact_root.require_current()?;
        let requested_output = output.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("output path is not valid UTF-8: {}", output.display()),
            )
        })?;
        let output = std::path::absolute(output)?;
        let output_parent_path = output
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output has no parent"))?
            .to_owned();
        let output_name = output.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "output path has no final component",
            )
        })?;
        let output_name = output_name.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("output path is not valid UTF-8: {}", output.display()),
            )
        })?;
        let logical_artifact_root = std::path::absolute(logical_artifact_root)?;
        let output_parent = if output_parent_path == logical_artifact_root {
            artifact_root.try_clone()?
        } else {
            BoundDirectory::open(output_parent_path)?
        };
        let artifact_root_identity = artifact_root.identity()?;
        let output_parent_identity = output_parent.identity()?;
        let transaction_id = publication_transaction_id(
            render_id,
            request_fingerprint,
            &artifact_root_identity,
            &output_parent_identity,
            requested_output,
            output_name,
        );
        Ok((
            PublicationPlanReceipt {
                schema: "pliego.publication-plan".into(),
                version: 1,
                transaction_id,
                render_id: render_id.to_owned(),
                request_fingerprint: request_fingerprint.to_owned(),
                artifact_root: receipt_path(&logical_artifact_root)?,
                artifact_root_identity,
                requested_output: requested_output.to_owned(),
                output: receipt_path(&output)?,
                output_parent_identity,
            },
            output_parent,
        ))
    }

    fn open(
        artifact_root: &BoundDirectory,
        output_parent: BoundDirectory,
        directory: BoundDirectory,
        plan: PublicationPlanReceipt,
        create: bool,
    ) -> io::Result<Self> {
        directory.require_current()?;

        let lease_path = directory.requested_path.join(PUBLICATION_LEASE_FILE_NAME);
        if !create {
            let metadata = std::fs::symlink_metadata(&lease_path)?;
            if path_metadata_is_alias(&metadata) || !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "publication lease is not a regular file: {}",
                        lease_path.display()
                    ),
                ));
            }
        }
        let mut lease_options = private_file_options();
        lease_options.read(true).write(true);
        if create {
            lease_options.create_new(true);
        }
        let lease_file = lease_options.open(&lease_path)?;
        let lease = Handle::from_file(lease_file)?;
        if !path_matches_handle(&lease_path, &lease)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "publication lease path changed while opening it: {}",
                    lease_path.display()
                ),
            ));
        }
        lease.as_file().try_lock().map_err(|error| {
            let error = io::Error::from(error);
            io::Error::new(
                error.kind(),
                format!(
                    "publication transaction is already leased at {}: {error}",
                    lease_path.display()
                ),
            )
        })?;

        let plan_sha256 = if create {
            write_immutable_receipt(&directory, PUBLICATION_PLAN_FILE_NAME, &plan)?
        } else {
            let (existing, sha256) = read_required_receipt::<PublicationPlanReceipt>(
                &directory,
                PUBLICATION_PLAN_FILE_NAME,
            )?;
            if existing != plan {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "existing publication plan does not match the requested render and output",
                ));
            }
            sha256
        };
        let journal = Self {
            artifact_root: artifact_root.try_clone()?,
            output_parent,
            directory,
            lease,
            plan,
            plan_sha256,
        };
        journal.require_current()?;
        Ok(journal)
    }

    fn require_current(&self) -> io::Result<()> {
        self.artifact_root.require_current()?;
        self.output_parent.require_current()?;
        self.directory.require_current()?;
        if self.artifact_root.identity()? != self.plan.artifact_root_identity
            || self.output_parent.identity()? != self.plan.output_parent_identity
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "publication transaction directory identity changed",
            ));
        }
        let lease_path = self
            .directory
            .requested_path
            .join(PUBLICATION_LEASE_FILE_NAME);
        if !path_matches_handle(&lease_path, &self.lease)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "publication lease path no longer names the held lease: {}",
                    lease_path.display()
                ),
            ));
        }
        Ok(())
    }

    pub(crate) fn recover(&self) -> io::Result<PublicationRecoveryState> {
        self.require_current()?;
        let (plan, plan_sha256) = read_required_receipt::<PublicationPlanReceipt>(
            &self.directory,
            PUBLICATION_PLAN_FILE_NAME,
        )?;
        if plan != self.plan || plan_sha256 != self.plan_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "publication plan changed after the lease was acquired",
            ));
        }

        let prepared = read_optional_receipt::<PublicationPreparedReceipt>(
            &self.directory,
            PUBLICATION_PREPARED_FILE_NAME,
        )?;
        let committed = read_optional_receipt::<PublicationCommittedReceipt>(
            &self.directory,
            PUBLICATION_COMMITTED_FILE_NAME,
        )?;
        let Some((prepared, prepared_sha256)) = prepared else {
            if committed.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "committed publication receipt has no prepared receipt",
                ));
            }
            return Ok(PublicationRecoveryState::Planned);
        };
        self.validate_prepared(&prepared)?;

        let (summary, cli_bytes) = read_publication_summary(&prepared.outcome)?;
        if let Some((committed, _)) = committed {
            self.validate_committed(&committed, &prepared_sha256)?;
            verify_publication_artifact(&prepared.output)?;
            verify_bundle_closure(
                &self.artifact_root,
                &prepared.bundle,
                None,
                &self.plan.render_id,
                &BundleEntry {
                    path: self.plan.requested_output.clone(),
                    sha256: prepared.output.sha256.clone(),
                    bytes: prepared.output.bytes,
                },
            )?;
            return Ok(PublicationRecoveryState::Committed {
                summary,
                cli_bytes,
                recovered: false,
            });
        }

        verify_bundle_closure(
            &self.artifact_root,
            &prepared.bundle,
            None,
            &self.plan.render_id,
            &BundleEntry {
                path: self.plan.requested_output.clone(),
                sha256: prepared.output.sha256.clone(),
                bytes: prepared.output.bytes,
            },
        )?;
        if publication_artifact_exists(&prepared.output)? {
            verify_publication_artifact(&prepared.output)?;
            self.cleanup_recovered_staging(&prepared);
        } else {
            self.publish_recovered_staging(&prepared)?;
        }
        let token = PreparedPublicationReceipt {
            sha256: prepared_sha256,
        };
        self.record_committed(&token, None)?;
        Ok(PublicationRecoveryState::Committed {
            summary,
            cli_bytes,
            recovered: true,
        })
    }

    pub(crate) fn record_prepared(
        &self,
        output: &PreparedDocumentPdf,
        bundle: &PreparedBundle,
        outcome_bytes: &[u8],
    ) -> io::Result<PreparedPublicationReceipt> {
        self.require_current()?;
        output.verify_staged()?;
        bundle.verify()?;
        let output_artifact = output.publication_artifact()?;
        if output_artifact.path != self.plan.output
            || output.output_parent_identity()? != self.plan.output_parent_identity
            || !bundle.matches_output(&output_artifact, &self.plan.requested_output)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "prepared output does not match the publication plan",
            ));
        }
        validate_publication_outcome_bytes(outcome_bytes)?;
        let outcome_sha256 = write_immutable_bytes(
            &self.directory,
            PUBLICATION_OUTCOME_FILE_NAME,
            outcome_bytes,
        )?;
        let receipt = PublicationPreparedReceipt {
            schema: "pliego.publication-prepared".into(),
            version: 1,
            transaction_id: self.plan.transaction_id.clone(),
            plan_sha256: self.plan_sha256.clone(),
            output: output_artifact,
            staging: output.staging_artifact()?,
            bundle: bundle.publication_artifact()?,
            outcome: PublicationArtifact {
                path: receipt_path(
                    &self
                        .directory
                        .requested_path
                        .join(PUBLICATION_OUTCOME_FILE_NAME),
                )?,
                sha256: outcome_sha256,
                bytes: outcome_bytes.len() as u64,
            },
        };
        let sha256 =
            write_immutable_receipt(&self.directory, PUBLICATION_PREPARED_FILE_NAME, &receipt)?;
        Ok(PreparedPublicationReceipt { sha256 })
    }

    pub(crate) fn record_committed(
        &self,
        prepared_token: &PreparedPublicationReceipt,
        live_bundle: Option<&PreparedBundle>,
    ) -> io::Result<()> {
        self.require_current()?;
        let (prepared, prepared_sha256) = read_required_receipt::<PublicationPreparedReceipt>(
            &self.directory,
            PUBLICATION_PREPARED_FILE_NAME,
        )?;
        self.validate_prepared(&prepared)?;
        if prepared_sha256 != prepared_token.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "prepared publication receipt changed before commit",
            ));
        }
        verify_publication_artifact(&prepared.output)?;
        if let Some(bundle) = live_bundle {
            bundle.verify()?;
            if bundle.publication_artifact()? != prepared.bundle {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "held bundle does not match the prepared publication receipt",
                ));
            }
        } else {
            verify_bundle_closure(
                &self.artifact_root,
                &prepared.bundle,
                None,
                &self.plan.render_id,
                &BundleEntry {
                    path: self.plan.requested_output.clone(),
                    sha256: prepared.output.sha256.clone(),
                    bytes: prepared.output.bytes,
                },
            )?;
        }
        read_publication_summary(&prepared.outcome)?;
        let receipt = PublicationCommittedReceipt {
            schema: "pliego.publication-committed".into(),
            version: 1,
            transaction_id: self.plan.transaction_id.clone(),
            prepared_sha256,
        };
        write_immutable_receipt(&self.directory, PUBLICATION_COMMITTED_FILE_NAME, &receipt)?;
        Ok(())
    }

    fn validate_prepared(&self, receipt: &PublicationPreparedReceipt) -> io::Result<()> {
        let artifact_root = Path::new(&self.plan.artifact_root);
        let expected_bundle = receipt_path(&artifact_root.join(BUNDLE_FILE_NAME))?;
        let expected_outcome = receipt_path(
            &self
                .directory
                .requested_path
                .join(PUBLICATION_OUTCOME_FILE_NAME),
        )?;
        let output = Path::new(&receipt.output.path);
        let staging = Path::new(&receipt.staging.path);
        let staging_is_output = staging == output;
        let staging_is_owned_temporary = staging.parent() == output.parent()
            && staging
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with('.') && name.contains(".pliego-") && name.ends_with(".tmp")
                });
        if receipt.schema != "pliego.publication-prepared"
            || receipt.version != 1
            || receipt.transaction_id != self.plan.transaction_id
            || receipt.plan_sha256 != self.plan_sha256
            || receipt.output.path != self.plan.output
            || receipt.bundle.path != expected_bundle
            || receipt.outcome.path != expected_outcome
            || (!staging_is_output && !staging_is_owned_temporary)
            || receipt.staging.sha256 != receipt.output.sha256
            || receipt.staging.bytes != receipt.output.bytes
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "prepared publication receipt does not match its plan",
            ));
        }
        Ok(())
    }

    fn publish_recovered_staging(&self, receipt: &PublicationPreparedReceipt) -> io::Result<()> {
        let staging_path = PathBuf::from(&receipt.staging.path);
        let output_path = PathBuf::from(&receipt.output.path);
        if staging_path == output_path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-owned prepared output is missing during recovery",
            ));
        }
        let metadata = std::fs::symlink_metadata(&staging_path)?;
        if path_metadata_is_alias(&metadata) || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "prepared staging path is not a regular file: {}",
                    staging_path.display()
                ),
            ));
        }
        let staging_file = owned_file_options()
            .read(true)
            .write(true)
            .open(&staging_path)?;
        let staging = OwnedFile::bind_retained(staging_path, staging_file)?;
        staging.verify_content(&receipt.staging.sha256, receipt.staging.bytes)?;
        self.require_current()?;
        match publish_owned_file(&staging, &self.output_parent, &output_path) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                verify_publication_artifact(&receipt.output)?;
            },
            Err(error) => return Err(error),
        }
        let _ = self.output_parent.handle.as_file().sync_all();
        let _ = staging.remove();
        Ok(())
    }

    fn cleanup_recovered_staging(&self, receipt: &PublicationPreparedReceipt) {
        if receipt.staging.path == receipt.output.path {
            return;
        }
        let staging_path = PathBuf::from(&receipt.staging.path);
        let staging_file = match owned_file_options()
            .read(true)
            .write(true)
            .open(&staging_path)
        {
            Ok(file) => file,
            Err(_) => return,
        };
        let Ok(staging) = OwnedFile::bind_retained(staging_path, staging_file) else {
            return;
        };
        if staging
            .verify_content(&receipt.staging.sha256, receipt.staging.bytes)
            .is_ok()
        {
            let _ = staging.remove();
        }
    }

    fn validate_committed(
        &self,
        receipt: &PublicationCommittedReceipt,
        prepared_sha256: &str,
    ) -> io::Result<()> {
        if receipt.schema != "pliego.publication-committed"
            || receipt.version != 1
            || receipt.transaction_id != self.plan.transaction_id
            || receipt.prepared_sha256 != prepared_sha256
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "committed publication receipt does not match its prepared receipt",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct LocalDocument {
    root: PathBuf,
    path: PathBuf,
}

impl LocalDocument {
    pub fn resolve(
        root: impl AsRef<Path>,
        requested: impl AsRef<Path>,
    ) -> Result<Self, SessionFailure> {
        let supplied_root = root.as_ref();
        let root =
            supplied_root
                .canonicalize()
                .map_err(|source| SessionFailure::RootUnavailable {
                    path: supplied_root.to_owned(),
                    source,
                })?;

        if !root.is_dir() {
            return Err(SessionFailure::RootNotDirectory(root));
        }

        let requested = requested.as_ref();
        if requested.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(SessionFailure::UnsafeRequestPath(requested.to_owned()));
        }

        let unresolved = root.join(requested);
        let path =
            unresolved
                .canonicalize()
                .map_err(|source| SessionFailure::DocumentUnavailable {
                    path: unresolved,
                    source,
                })?;

        Self::from_canonical_paths(root, path)
    }

    fn from_canonical_paths(root: PathBuf, path: PathBuf) -> Result<Self, SessionFailure> {
        if !path.starts_with(&root) {
            return Err(SessionFailure::OutsideRoot { root, path });
        }

        if !path.is_file() {
            return Err(SessionFailure::DocumentNotFile(path));
        }

        Ok(Self { root, path })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub struct SessionArtifacts {
    directory: PathBuf,
    public_directory: PathBuf,
    directory_binding: BoundDirectory,
    render_id: String,
}

impl SessionArtifacts {
    #[cfg(test)]
    pub fn create(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_owned();
        let render_id = directory
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session artifact path has no final component",
                )
            })?
            .to_string_lossy()
            .into_owned();
        Self::create_with_render_id(directory, render_id)
    }

    pub fn create_with_render_id(
        directory: impl AsRef<Path>,
        render_id: impl Into<String>,
    ) -> io::Result<Self> {
        let directory = directory.as_ref();
        Self::create_staged_with_render_id(directory, directory, render_id)
    }

    pub(crate) fn create_staged_with_render_id(
        directory: impl AsRef<Path>,
        public_directory: impl AsRef<Path>,
        render_id: impl Into<String>,
    ) -> io::Result<Self> {
        let requested_directory = std::path::absolute(directory.as_ref())?;
        let requested_parent = requested_directory.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "session artifact path has no parent directory",
            )
        })?;
        require_path_without_aliases(requested_parent)?;
        let render_id = render_id.into();
        if render_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "render ID may not be empty",
            ));
        }
        create_private_directory(&requested_directory)?;
        let directory_binding = BoundDirectory::open(requested_directory)?;
        let directory = directory_binding.requested_path.clone();
        let public_directory = std::path::absolute(public_directory.as_ref())?;
        create_private_directory(&directory.join("resources"))?;
        for name in ["console.jsonl", "resources.jsonl", "session-state.jsonl"] {
            private_file_options()
                .write(true)
                .create_new(true)
                .open(directory.join(name))?;
        }
        Ok(Self {
            directory,
            public_directory,
            directory_binding,
            render_id,
        })
    }

    pub(crate) fn open_for_publication_recovery(
        directory: impl AsRef<Path>,
        render_id: impl Into<String>,
    ) -> io::Result<Self> {
        let directory = directory.as_ref();
        Self::open_staged_for_publication(directory, directory, render_id)
    }

    pub(crate) fn open_staged_for_publication(
        directory: impl AsRef<Path>,
        public_directory: impl AsRef<Path>,
        render_id: impl Into<String>,
    ) -> io::Result<Self> {
        let requested_directory = std::path::absolute(directory.as_ref())?;
        let requested_parent = requested_directory.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "session artifact path has no parent directory",
            )
        })?;
        require_path_without_aliases(requested_parent)?;
        let render_id = render_id.into();
        if render_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "render ID may not be empty",
            ));
        }
        let directory_binding = BoundDirectory::open(requested_directory)?;
        let directory = directory_binding.requested_path.clone();
        let public_directory = std::path::absolute(public_directory.as_ref())?;
        Ok(Self {
            public_directory,
            directory,
            directory_binding,
            render_id,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn public_directory(&self) -> &Path {
        &self.public_directory
    }

    pub(crate) fn artifact_identity(&self, name: &str) -> io::Result<(String, u64)> {
        self.require_current()?;
        let path = self.artifact_path(name)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        if path_metadata_is_alias(&metadata) || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("session artifact is not a regular file: {}", path.display()),
            ));
        }
        let file = File::open(&path)?;
        let handle = Handle::from_file(file)?;
        if !path_matches_handle(&path, &handle)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "session artifact path changed while opening: {}",
                    path.display()
                ),
            ));
        }
        let identity = hash_open_file(handle.as_file(), &path)?;
        if !path_matches_handle(&path, &handle)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "session artifact path changed while hashing: {}",
                    path.display()
                ),
            ));
        }
        self.require_current()?;
        Ok(identity)
    }

    pub(crate) fn read_json_artifact(
        &self,
        name: &str,
        expected_sha256: &str,
        expected_bytes: u64,
    ) -> io::Result<serde_json::Value> {
        let path = self.artifact_path(name)?;
        if expected_bytes > MAX_CONTROL_JSON_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "session control JSON exceeds the {MAX_CONTROL_JSON_BYTES}-byte limit: {}",
                    path.display()
                ),
            ));
        }
        let (sha256, bytes) = self.artifact_identity(name)?;
        if sha256 != expected_sha256 || bytes != expected_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("session artifact identity changed: {}", path.display()),
            ));
        }
        let bytes = std::fs::read(&path)?;
        if bytes.len() as u64 != expected_bytes || receipt_sha256(&bytes) != expected_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("session artifact changed while reading: {}", path.display()),
            ));
        }
        self.require_current()?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    pub(crate) fn require_session_state_append_access(&self) -> io::Result<()> {
        self.require_current()?;
        let path = self.artifact_path("session-state.jsonl")?;
        let metadata = std::fs::symlink_metadata(&path)?;
        if path_metadata_is_alias(&metadata) || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("session state is not a regular file: {}", path.display()),
            ));
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        let handle = Handle::from_file(file)?;
        if !path_matches_handle(&path, &handle)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("session state changed while opening: {}", path.display()),
            ));
        }
        self.require_current()
    }

    fn artifact_path(&self, name: &str) -> io::Result<PathBuf> {
        let path = Path::new(name);
        if path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session artifact name must be one normal path component",
            ));
        }
        Ok(self.directory.join(path))
    }

    pub fn render_id(&self) -> String {
        self.render_id.clone()
    }

    fn require_current(&self) -> io::Result<()> {
        self.directory_binding.require_current()
    }

    pub(crate) fn begin_publication(
        &self,
        output: impl AsRef<Path>,
        request_fingerprint: &str,
    ) -> io::Result<PublicationJournal> {
        self.require_current()?;
        PublicationJournal::begin(
            &self.directory_binding,
            &self.public_directory,
            &self.render_id,
            request_fingerprint,
            output.as_ref(),
        )
    }

    pub(crate) fn resume_publication(
        &self,
        output: impl AsRef<Path>,
        request_fingerprint: &str,
    ) -> io::Result<PublicationJournal> {
        self.require_current()?;
        PublicationJournal::resume(
            &self.directory_binding,
            &self.public_directory,
            &self.render_id,
            request_fingerprint,
            output.as_ref(),
        )
    }

    pub fn record_state(&self, state: &str, message: Option<&str>) -> io::Result<()> {
        self.append(
            "session-state.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "state": state,
                "message": message,
            }),
        )
    }

    pub fn record_console(&self, level: &str, message: &str) -> io::Result<()> {
        self.append(
            "console.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "level": level,
                "message": message,
            }),
        )
    }

    pub fn record_resource_request(&self, request_id: &str, url: &str) -> io::Result<()> {
        self.append(
            "resources.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "render_id": self.render_id,
                "policy": "pliego.resource-policy.v1",
                "request_id": request_id,
                "url": url,
                "status": "requested",
                "bytes": null,
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_resource_failure(
        &self,
        code: &str,
        status: &str,
        url: &str,
        method: &str,
        destination: &str,
        load_role: WebResourceLoadRole,
        fatal: bool,
        referrer_url: Option<&str>,
        is_for_main_frame: bool,
        is_redirect: bool,
        reason: &str,
    ) -> io::Result<()> {
        let cancelled = !fatal && load_role == WebResourceLoadRole::DocumentMetadata;
        self.append(
            "resources.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "render_id": self.render_id,
                "policy": "pliego.resource-policy.v1",
                "request_id": null,
                "url": url,
                "status": status,
                "code": code,
                "method": method,
                "destination": destination,
                "load_role": load_role,
                "fatal": !cancelled,
                "cancelled": cancelled,
                "referrer_url": referrer_url,
                "is_for_main_frame": is_for_main_frame,
                "is_redirect": is_redirect,
                "reason": reason,
                "bytes": null,
            }),
        )
    }

    #[cfg(any(feature = "shell-oracle", test))]
    pub fn record_loaded_resource(
        &self,
        request_id: &str,
        urls: &[String],
        response_status: Option<u16>,
        content_type: Option<&str>,
        sha256: &str,
        body: &[u8],
        cache_result: Option<&str>,
    ) -> io::Result<()> {
        let artifact = self.write_resource_digest(sha256, body)?;

        self.append(
            "resources.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "render_id": self.render_id,
                "policy": "pliego.resource-policy.v1",
                "request_id": request_id,
                "url": urls.last(),
                "urls": urls,
                "status": "loaded",
                "response_status": response_status,
                "content_type": content_type,
                "bytes": body.len() as u64,
                "sha256": sha256,
                "resource": format!("sha256:{sha256}"),
                "content_hash": format!("sha256:{sha256}"),
                "cache_result": cache_result,
                "artifact": artifact,
            }),
        )
    }

    /// Record one terminal resource event supplied by a Pliego-owned runtime.
    ///
    /// The caller supplies the runtime-specific evidence fields; this artifact
    /// boundary owns the timestamp, render identity, and policy identity so a
    /// runtime cannot accidentally publish evidence for a different session.
    #[cfg(feature = "document-session")]
    pub fn record_resource_evidence(&self, evidence: serde_json::Value) -> io::Result<()> {
        let mut evidence = evidence.as_object().cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource evidence must be a JSON object",
            )
        })?;
        evidence.insert("timestamp_ms".into(), serde_json::json!(timestamp_ms()));
        evidence.insert("render_id".into(), self.render_id.clone().into());
        evidence.insert(
            "policy".into(),
            serde_json::Value::String("pliego.resource-policy.v1".into()),
        );
        self.append("resources.jsonl", serde_json::Value::Object(evidence))
    }

    pub fn record_asset_failure(
        &self,
        code: &str,
        manifest: &Path,
        url: Option<&str>,
        reason: &str,
        expected: Option<&str>,
        actual: Option<&str>,
    ) -> io::Result<()> {
        self.append(
            "resources.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "render_id": self.render_id,
                "policy": "pliego.asset-cache.v1",
                "request_id": null,
                "url": url,
                "status": "failed",
                "code": code,
                "manifest": manifest,
                "reason": reason,
                "expected": expected,
                "actual": actual,
                "cache_result": null,
                "bytes": null,
            }),
        )
    }

    pub fn write_content_addressed_resource(
        &self,
        resource: &str,
        body: &[u8],
    ) -> io::Result<String> {
        let digest = resource.strip_prefix("sha256:").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("resource is not a SHA-256 content address: {resource}"),
            )
        })?;
        self.write_resource_digest(digest, body)
    }

    pub fn write_scene(&self, normalized_scene: &[u8]) -> io::Result<()> {
        self.write_bytes("scene.json", normalized_scene)
    }

    pub fn write_fonts(&self, fonts: &serde_json::Value) -> io::Result<()> {
        self.write_json("fonts.json", fonts)
    }

    pub fn write_scene_report(&self, report: &serde_json::Value) -> io::Result<()> {
        self.write_json("scene-report.json", report)
    }

    pub fn write_scene_preview(&self, png: &[u8]) -> io::Result<()> {
        self.write_bytes("scene-preview.png", png)
    }

    #[cfg(feature = "document-session")]
    pub fn write_render_image(&self, png: &[u8]) -> io::Result<()> {
        self.write_bytes("render.png", png)
    }

    pub fn write_scene_previews(&self, pages: &[Vec<u8>]) -> io::Result<Vec<PathBuf>> {
        self.require_current()?;
        if pages.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scene preview requires at least one page",
            ));
        }
        if pages.len() == 1 {
            self.write_scene_preview(&pages[0])?;
            return Ok(vec![self.directory.join("scene-preview.png")]);
        }

        let directory = self.directory.join("pages");
        create_private_directory(&directory)?;
        pages
            .iter()
            .enumerate()
            .map(|(index, png)| {
                let path = directory.join(format!("page-{:04}.png", index + 1));
                let mut file = open_private_file(&path)?;
                file.write_all(png)?;
                Ok(path)
            })
            .collect()
    }

    pub fn write_pages(&self, pages: &serde_json::Value) -> io::Result<()> {
        self.write_json("pages.json", pages)
    }

    pub fn write_document_pdf(&self, pdf: &[u8]) -> io::Result<()> {
        self.write_bytes("document.pdf", pdf)
    }

    /// Stage the diagnostic PDF beside its caller-owned destination.
    ///
    /// The returned value owns the staged file until it is committed or dropped.
    pub(crate) fn prepare_document_pdf(
        &self,
        destination: impl AsRef<Path>,
    ) -> io::Result<PreparedDocumentPdf> {
        self.require_current()?;
        let source = self.directory.join("document.pdf");
        let destination = destination.as_ref().to_owned();
        let absolute_destination = std::path::absolute(&destination)?;
        if source == absolute_destination {
            return prepare_artifact_file(
                source,
                destination,
                absolute_destination,
                self.directory_binding.try_clone()?,
            );
        }
        prepare_new_file(&source, &destination)
    }

    /// Bind the completed diagnostic artifacts and prepared PDF to this render ID.
    pub(crate) fn write_prepared_bundle(
        &self,
        output: &PreparedDocumentPdf,
    ) -> io::Result<PreparedBundle> {
        self.require_current()?;
        output.verify_staged()?;
        require_rendered_terminal_state(&self.directory.join("session-state.jsonl"))?;
        require_directory_without_symlink(&self.directory)?;

        let mut entries = Vec::new();
        collect_bundle_entries(&self.directory, &self.directory, &mut entries)?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        self.require_current()?;

        let document_pdf = entries
            .iter()
            .find(|entry| entry.path == "document.pdf")
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "bundle artifacts have no staged document.pdf",
                )
            })?;
        if document_pdf.sha256 != output.sha256 || document_pdf.bytes != output.bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "staged document.pdf changed after output preparation",
            ));
        }
        let output_entry = output.bundle_entry()?;
        let manifest = BundleManifest {
            schema: "pliego.bundle",
            version: 1,
            render_id: &self.render_id,
            entries,
            output: output_entry.clone(),
        };

        let bundle_path = self.directory.join(BUNDLE_FILE_NAME);
        let bundle_file = owned_file_options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&bundle_path)?;
        let mut bundle = OwnedFile::bind(bundle_path, bundle_file)?;
        let write_result = (|| {
            serde_json::to_writer_pretty(bundle.handle.as_file_mut(), &manifest)
                .map_err(io::Error::other)?;
            bundle.handle.as_file_mut().write_all(b"\n")?;
            bundle.handle.as_file().sync_all()?;
            bundle.require_current()
        })();
        if let Err(error) = write_result {
            let cleanup = bundle.remove();
            if let Err(cleanup_error) = cleanup {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{error}; cannot remove incomplete owned bundle: {cleanup_error}"),
                ));
            }
            return Err(error);
        }
        self.require_current()?;
        let (sha256, bytes) = hash_open_file(bundle.handle.as_file(), &bundle.path)?;
        if bytes > MAX_PUBLICATION_BUNDLE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bundle manifest exceeds the {MAX_PUBLICATION_BUNDLE_BYTES}-byte limit"),
            ));
        }
        Ok(PreparedBundle {
            file: Some(bundle),
            artifact_root: self.directory_binding.try_clone()?,
            render_id: self.render_id.clone(),
            output: output_entry,
            sha256,
            bytes,
        })
    }

    pub fn write_pdf_structure(&self, structure: &serde_json::Value) -> io::Result<()> {
        self.write_json("pdf-structure.json", structure)
    }

    pub fn write_readiness(&self, readiness: &serde_json::Value) -> io::Result<()> {
        let mut readiness = readiness.clone();
        let object = readiness.as_object_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "readiness must be a JSON object",
            )
        })?;
        object.insert(
            "render_id".into(),
            serde_json::Value::String(self.render_id()),
        );
        self.write_json("readiness.json", &readiness)
    }

    pub fn write_layout_debug(&self, snapshot: &serde_json::Value) -> io::Result<()> {
        self.write_json("layout-debug.json", snapshot)
    }

    pub fn write_environment(&self, environment: &serde_json::Value) -> io::Result<()> {
        self.write_json("environment.json", environment)
    }

    pub fn write_failure(&self, code: &str, message: &str) -> io::Result<()> {
        self.write_json(
            "failure.json",
            &serde_json::json!({
                "status": "failed",
                "render_id": self.render_id,
                "error": {
                    "code": code,
                    "message": message,
                },
            }),
        )
    }

    fn write_resource_digest(&self, digest: &str, body: &[u8]) -> io::Result<String> {
        self.require_current()?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid lowercase SHA-256 digest: {digest}"),
            ));
        }

        let artifact = format!("resources/{digest}");
        let path = self.directory.join(&artifact);
        match private_file_options()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file.write_all(body)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if std::fs::read(&path)? != body {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("resource digest collision for {digest}"),
                    ));
                }
            },
            Err(error) => return Err(error),
        }
        Ok(artifact)
    }

    fn write_bytes(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.require_current()?;
        let mut file = open_private_file(&self.directory.join(name))?;
        file.write_all(bytes)
    }

    fn write_json(&self, name: &str, value: &serde_json::Value) -> io::Result<()> {
        self.require_current()?;
        let mut file = open_private_file(&self.directory.join(name))?;
        serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }

    fn append(&self, name: &str, event: serde_json::Value) -> io::Result<()> {
        self.require_current()?;
        let mut file = OpenOptions::new()
            .append(true)
            .open(self.directory.join(name))?;
        serde_json::to_writer(&mut file, &event).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }
}

fn require_rendered_terminal_state(path: &Path) -> io::Result<()> {
    let contents = std::fs::read_to_string(path)?;
    let event = contents
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session state has no terminal event",
            )
        })?;
    let event: serde_json::Value = serde_json::from_str(event).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("session terminal state is invalid JSON: {error}"),
        )
    })?;
    if event.get("state").and_then(serde_json::Value::as_str) != Some("rendered") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bundle may only be written after the rendered terminal state",
        ));
    }
    Ok(())
}

fn require_directory_without_symlink(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if path_metadata_is_alias(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bundle artifact root must be a directory, not a symlink or special file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn require_path_without_aliases(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if path_metadata_is_alias(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "publication paths may not contain symlink or reparse-point components: {}",
                    ancestor.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn path_metadata_is_alias(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
pub(crate) fn path_metadata_is_alias(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn collect_bundle_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<BundleEntry>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if path_metadata_is_alias(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bundle artifacts may not contain symlinks: {}",
                    path.display()
                ),
            ));
        }
        if metadata.is_dir() {
            if path.parent() == Some(root)
                && path.file_name().and_then(|name| name.to_str())
                    == Some(PUBLICATION_DIRECTORY_NAME)
            {
                continue;
            }
            collect_bundle_entries(root, &path, entries)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bundle artifacts may only contain regular files and directories: {}",
                    path.display()
                ),
            ));
        }

        let relative = normalized_relative_path(root, &path)?;
        if is_bundle_excluded(&relative) {
            continue;
        }
        let (sha256, bytes) = hash_regular_file(&path)?;
        entries.push(BundleEntry {
            path: relative,
            sha256,
            bytes,
        });
    }
    Ok(())
}

fn normalized_relative_path(root: &Path, path: &Path) -> io::Result<String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bundle artifact escapes its root {}: {}",
                root.display(),
                path.display()
            ),
        )
    })?;
    let mut normalized = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bundle artifact path is unsafe: {}", path.display()),
            ));
        };
        let component = component.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bundle artifact path is not valid UTF-8: {}",
                    path.display()
                ),
            )
        })?;
        if component.is_empty() || component.contains('\\') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bundle artifact path is unsafe: {}", path.display()),
            ));
        }
        normalized.push(component);
    }
    if normalized.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bundle artifact path may not be the artifact root",
        ));
    }
    Ok(normalized.join("/"))
}

fn is_bundle_excluded(relative: &str) -> bool {
    relative == BUNDLE_FILE_NAME
}

fn receipt_path(path: &Path) -> io::Result<String> {
    let path = std::path::absolute(path)?;
    path.to_str().map(str::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication receipt path is not valid UTF-8: {}",
                path.display()
            ),
        )
    })
}

fn publication_transaction_id(
    render_id: &str,
    request_fingerprint: &str,
    artifact_root_identity: &str,
    output_parent_identity: &str,
    requested_output: &str,
    output_name: &str,
) -> String {
    let mut hasher = Sha256::new();
    for field in [
        "pliego.publication-transaction.v1",
        render_id,
        request_fingerprint,
        artifact_root_identity,
        output_parent_identity,
        requested_output,
        output_name,
    ] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    format!("sha256:{}", lowercase_hex(&hasher.finalize()))
}

fn receipt_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", lowercase_hex(&hasher.finalize()))
}

fn serialize_receipt(receipt: &impl Serialize) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(receipt).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
fn serialize_publication_outcome(summary: &serde_json::Value) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(summary).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_immutable_receipt(
    directory: &BoundDirectory,
    name: &str,
    receipt: &impl Serialize,
) -> io::Result<String> {
    let expected = serialize_receipt(receipt)?;
    write_immutable_bytes(directory, name, &expected)
}

fn write_immutable_bytes(
    directory: &BoundDirectory,
    name: &str,
    expected: &[u8],
) -> io::Result<String> {
    directory.require_current()?;
    let expected_sha256 = receipt_sha256(&expected);
    let final_path = directory.requested_path.join(name);
    if final_path.try_exists()? {
        let existing = read_receipt_bytes(directory, name)?;
        if existing != expected {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "immutable publication receipt already exists with different bytes: {}",
                    final_path.display()
                ),
            ));
        }
        return Ok(expected_sha256);
    }

    for attempt in 0..32 {
        let temporary_path = directory.requested_path.join(format!(
            ".{name}.pliego-receipt-{}-{attempt}.tmp",
            std::process::id()
        ));
        let temporary_file = match owned_file_options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let mut temporary = OwnedFile::bind(temporary_path, temporary_file)?;
        temporary.handle.as_file_mut().write_all(&expected)?;
        temporary.handle.as_file().sync_all()?;
        temporary.verify_content(&expected_sha256, expected.len() as u64)?;
        directory.require_current()?;
        match publish_owned_file(&temporary, directory, &final_path) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = read_receipt_bytes(directory, name)?;
                if existing != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "immutable publication receipt raced with different bytes: {}",
                            final_path.display()
                        ),
                    ));
                }
            },
            Err(error) => return Err(error),
        }
        let _ = directory.handle.as_file().sync_all();
        // The handle-source link above is the final reported fallible action. The
        // held bytes were already synced and verified, so later recovery validates
        // the visible receipt instead of turning a successful link into an error.
        return Ok(expected_sha256);
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "all temporary receipt names already exist beside {}",
            final_path.display()
        ),
    ))
}

fn read_receipt_bytes(directory: &BoundDirectory, name: &str) -> io::Result<Vec<u8>> {
    const MAX_RECEIPT_BYTES: u64 = 1024 * 1024;

    directory.require_current()?;
    let path = directory.requested_path.join(name);
    let metadata = std::fs::symlink_metadata(&path)?;
    if path_metadata_is_alias(&metadata) || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication receipt is not a regular file: {}",
                path.display()
            ),
        ));
    }
    if metadata.len() > MAX_RECEIPT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("publication receipt is too large: {}", path.display()),
        ));
    }
    let handle = Handle::from_file(File::open(&path)?)?;
    if !path_matches_handle(&path, &handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication receipt path changed while opening it: {}",
                path.display()
            ),
        ));
    }
    let held_bytes = handle.as_file().metadata()?.len();
    if held_bytes > MAX_RECEIPT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("publication receipt is too large: {}", path.display()),
        ));
    }
    let mut bytes = Vec::with_capacity(held_bytes as usize);
    handle
        .as_file()
        .try_clone()?
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("publication receipt grew while reading: {}", path.display()),
        ));
    }
    if !path_matches_handle(&path, &handle)?
        || bytes.len() as u64 != handle.as_file().metadata()?.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication receipt changed while reading: {}",
                path.display()
            ),
        ));
    }
    directory.require_current()?;
    Ok(bytes)
}

fn read_publication_summary(
    artifact: &PublicationArtifact,
) -> io::Result<(serde_json::Value, Vec<u8>)> {
    let path = Path::new(&artifact.path);
    let metadata = std::fs::symlink_metadata(path)?;
    if path_metadata_is_alias(&metadata) || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication outcome is not a regular file: {}",
                path.display()
            ),
        ));
    }
    let handle = Handle::from_file(File::open(path)?)?;
    if !path_matches_handle(path, &handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication outcome path changed while opening it: {}",
                path.display()
            ),
        ));
    }
    let held_bytes = handle.as_file().metadata()?.len();
    if held_bytes > MAX_PUBLICATION_OUTCOME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("publication outcome is too large: {}", path.display()),
        ));
    }
    let mut bytes = Vec::with_capacity(held_bytes as usize);
    handle
        .as_file()
        .try_clone()?
        .take(MAX_PUBLICATION_OUTCOME_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PUBLICATION_OUTCOME_BYTES
        || bytes.len() as u64 != handle.as_file().metadata()?.len()
        || !path_matches_handle(path, &handle)?
        || receipt_sha256(&bytes) != artifact.sha256
        || bytes.len() as u64 != artifact.bytes
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication outcome changed while reading: {}",
                path.display()
            ),
        ));
    }
    let summary = serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid publication outcome {}: {error}", path.display()),
        )
    })?;
    Ok((summary, bytes))
}

fn read_optional_receipt<T>(
    directory: &BoundDirectory,
    name: &str,
) -> io::Result<Option<(T, String)>>
where
    T: for<'de> Deserialize<'de>,
{
    let path = directory.requested_path.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {},
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let bytes = read_receipt_bytes(directory, name)?;
    let receipt = serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid publication receipt {}: {error}", path.display()),
        )
    })?;
    let sha256 = receipt_sha256(&bytes);
    Ok(Some((receipt, sha256)))
}

fn read_required_receipt<T>(directory: &BoundDirectory, name: &str) -> io::Result<(T, String)>
where
    T: for<'de> Deserialize<'de>,
{
    read_optional_receipt(directory, name)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication receipt is missing: {}",
                directory.requested_path.join(name).display()
            ),
        )
    })
}

fn publication_artifact_exists(artifact: &PublicationArtifact) -> io::Result<bool> {
    let path = Path::new(&artifact.path);
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !path_metadata_is_alias(&metadata) => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication destination is a symlink or special file: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn verify_publication_artifact(artifact: &PublicationArtifact) -> io::Result<()> {
    let path = Path::new(&artifact.path);
    let metadata = std::fs::symlink_metadata(path)?;
    if path_metadata_is_alias(&metadata) || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication artifact is not a regular file: {}",
                path.display()
            ),
        ));
    }
    let handle = Handle::from_file(File::open(path)?)?;
    if !path_matches_handle(path, &handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication artifact path changed while opening it: {}",
                path.display()
            ),
        ));
    }
    let (sha256, bytes) = hash_open_file(handle.as_file(), path)?;
    if !path_matches_handle(path, &handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication artifact path changed while verifying it: {}",
                path.display()
            ),
        ));
    }
    if sha256 != artifact.sha256 || bytes != artifact.bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "publication artifact does not match its prepared receipt: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn verify_bundle_closure(
    artifact_root: &BoundDirectory,
    artifact: &PublicationArtifact,
    live_file: Option<&OwnedFile>,
    render_id: &str,
    output: &BundleEntry,
) -> io::Result<()> {
    artifact_root.require_current()?;
    let bundle_path = artifact_root.requested_path.join(BUNDLE_FILE_NAME);
    if artifact.path != receipt_path(&bundle_path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "prepared bundle path does not match the bound artifact root",
        ));
    }

    let opened;
    let handle = if let Some(file) = live_file {
        file.require_current()?;
        if receipt_path(&file.path)? != artifact.path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "held bundle path does not match the prepared receipt",
            ));
        }
        &file.handle
    } else {
        let metadata = std::fs::symlink_metadata(&bundle_path)?;
        if path_metadata_is_alias(&metadata) || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "prepared bundle is not a regular file: {}",
                    bundle_path.display()
                ),
            ));
        }
        opened = Handle::from_file(File::open(&bundle_path)?)?;
        &opened
    };
    if !path_matches_handle(&bundle_path, handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "prepared bundle path changed while opening it: {}",
                bundle_path.display()
            ),
        ));
    }
    let held_len = handle.as_file().metadata()?.len();
    if artifact.bytes > MAX_PUBLICATION_BUNDLE_BYTES || held_len != artifact.bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "prepared bundle length does not match its receipt",
        ));
    }
    let capacity = usize::try_from(artifact.bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "prepared bundle is too large to address in this process",
        )
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(io::Error::other)?;
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = read_open_file_at(handle.as_file(), &mut buffer, offset)?;
        if read == 0 {
            break;
        }
        offset = offset.checked_add(read as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "prepared bundle is too large")
        })?;
        if offset > artifact.bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "prepared bundle grew while reading",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    if offset != artifact.bytes
        || handle.as_file().metadata()?.len() != artifact.bytes
        || receipt_sha256(&bytes) != artifact.sha256
        || !path_matches_handle(&bundle_path, handle)?
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "prepared bundle changed while reading",
        ));
    }

    let manifest: OwnedBundleManifest = serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("prepared bundle is invalid JSON: {error}"),
        )
    })?;
    if manifest.schema != "pliego.bundle"
        || manifest.version != 1
        || manifest.render_id != render_id
        || manifest.output != *output
        || manifest
            .entries
            .windows(2)
            .any(|entries| entries[0].path >= entries[1].path)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "prepared bundle manifest does not match the publication transaction",
        ));
    }

    let mut current_entries = Vec::new();
    collect_bundle_entries(
        &artifact_root.requested_path,
        &artifact_root.requested_path,
        &mut current_entries,
    )?;
    current_entries.sort_by(|left, right| left.path.cmp(&right.path));
    if current_entries != manifest.entries {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact bundle entries changed after preparation",
        ));
    }
    artifact_root.require_current()?;
    if !path_matches_handle(&bundle_path, handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "prepared bundle path changed during closure verification",
        ));
    }
    Ok(())
}

fn hash_regular_file(path: &Path) -> io::Result<(String, u64)> {
    let metadata = std::fs::symlink_metadata(path)?;
    if path_metadata_is_alias(&metadata) || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bundle entry must be a regular file, not a symlink or special file: {}",
                path.display()
            ),
        ));
    }

    let handle = Handle::from_file(File::open(path)?)?;
    if !path_matches_handle(path, &handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bundle entry path changed while opening: {}",
                path.display()
            ),
        ));
    }
    let digest = hash_open_file(handle.as_file(), path)?;
    if digest.1 != metadata.len() || !path_matches_handle(path, &handle)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bundle entry changed while hashing: {}", path.display()),
        ));
    }
    Ok(digest)
}

fn hash_open_file(file: &File, path: &Path) -> io::Result<(String, u64)> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("owned path is not a regular file: {}", path.display()),
        ));
    }

    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = read_open_file_at(file, &mut buffer, bytes)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes.checked_add(read as u64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("owned file is too large to count: {}", path.display()),
            )
        })?;
    }
    if bytes != file.metadata()?.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("owned file changed while hashing: {}", path.display()),
        ));
    }
    Ok((
        format!("sha256:{}", lowercase_hex(&hasher.finalize())),
        bytes,
    ))
}

fn path_matches_handle(path: &Path, expected: &Handle) -> io::Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if path_metadata_is_alias(&metadata) || !metadata.is_file() {
        return Ok(false);
    }
    handles_match(&Handle::from_path(path)?, expected)
}

#[cfg(not(windows))]
fn handles_match(left: &Handle, right: &Handle) -> io::Result<bool> {
    Ok(left == right)
}

#[cfg(unix)]
fn open_file_identity(file: &File, _path: &Path) -> io::Result<String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(format!(
        "unix:{:016x}:{:016x}",
        metadata.dev(),
        metadata.ino()
    ))
}

#[cfg(windows)]
fn open_file_identity(file: &File, _path: &Path) -> io::Result<String> {
    let (volume, identifier) = windows_file_identity(file)?;
    Ok(format!(
        "windows:{volume:016x}:{}",
        lowercase_hex(&identifier)
    ))
}

#[cfg(not(any(unix, windows)))]
fn open_file_identity(_file: &File, path: &Path) -> io::Result<String> {
    Ok(format!("path:{}", receipt_path(path)?))
}

#[cfg(windows)]
fn handles_match(left: &Handle, right: &Handle) -> io::Result<bool> {
    Ok(windows_file_identity(left.as_file())? == windows_file_identity(right.as_file())?)
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut identity = FILE_ID_INFO::default();
    // SAFETY: the file handle remains live and the output buffer has the exact
    // FILE_ID_INFO size required by FileIdInfo.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileIdInfo,
            (&raw mut identity).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((identity.VolumeSerialNumber, identity.FileId.Identifier))
}

#[cfg(unix)]
fn read_open_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;

    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_open_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;

    // Unlike Unix read_at, seek_read sets the handle's shared cursor to the end
    // of each read. Do not write through this handle after verification unless
    // the cursor is restored or synchronized; concurrent use also needs synchronization.
    file.seek_read(buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_open_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::io::{Seek, SeekFrom};

    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.read(buffer)
}

#[cfg(not(windows))]
fn remove_owned_file(path: &Path, expected: &Handle) -> io::Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    if !path_matches_handle(path, expected)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing to remove a path that no longer names the owned file: {}",
                path.display()
            ),
        ));
    }
    std::fs::remove_file(path)
}

#[cfg(windows)]
fn remove_owned_file(_path: &Path, expected: &Handle) -> io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the handle remains owned for the duration of this call and the
    // buffer points to a correctly sized FILE_DISPOSITION_INFO value.
    let succeeded = unsafe {
        SetFileInformationByHandle(
            expected.as_file().as_raw_handle() as _,
            FileDispositionInfo,
            (&raw const disposition).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(unix)]
pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)?;
    #[cfg(target_os = "macos")]
    {
        let validation = File::open(path).and_then(|file| require_no_macos_extended_acl(&file));
        if let Err(error) = validation {
            let _ = std::fs::remove_dir(path);
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn windows_long_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    const LEGACY_CREATE_DIRECTORY_LIMIT: usize = 248;
    const SEP: u16 = b'\\' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const U: u16 = b'U' as u16;
    const N: u16 = b'N' as u16;
    const C: u16 = b'C' as u16;
    const VERBATIM_PREFIX: &[u16] = &[SEP, SEP, QUESTION, SEP];
    const NT_PREFIX: &[u16] = &[SEP, QUESTION, QUESTION, SEP];
    const DEVICE_PREFIX: &[u16] = &[SEP, SEP, DOT, SEP];
    const UNC_PREFIX: &[u16] = &[SEP, SEP, QUESTION, SEP, U, N, C, SEP];

    let absolute = std::path::absolute(path)?;
    let mut path: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private directory path contains an embedded NUL",
        ));
    }
    if path.starts_with(VERBATIM_PREFIX) || path.starts_with(NT_PREFIX) {
        path.push(0);
        return Ok(path);
    }
    if path.len() + 1 < LEGACY_CREATE_DIRECTORY_LIMIT {
        path.push(0);
        return Ok(path);
    }

    let mut verbatim = Vec::with_capacity(path.len() + UNC_PREFIX.len() + 1);
    if path.starts_with(DEVICE_PREFIX) {
        verbatim.extend_from_slice(VERBATIM_PREFIX);
        verbatim.extend_from_slice(&path[DEVICE_PREFIX.len()..]);
    } else if path.starts_with(&[SEP, SEP]) {
        verbatim.extend_from_slice(UNC_PREFIX);
        verbatim.extend_from_slice(&path[2..]);
    } else if path.get(1) == Some(&(b':' as u16)) && path.get(2) == Some(&SEP) {
        verbatim.extend_from_slice(VERBATIM_PREFIX);
        verbatim.extend_from_slice(&path);
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private directory path could not be normalized for Win32 creation",
        ));
    }
    verbatim.push(0);
    Ok(verbatim)
}

#[cfg(windows)]
pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::mem::size_of;

    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, InitializeAcl,
        InitializeSecurityDescriptor, OBJECT_INHERIT_ACE, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
        SetSecurityDescriptorOwner,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ALL_ACCESS};

    let user = current_process_user_sid()?;
    // SAFETY: current_process_user_sid always returns a validated copied SID.
    let sid_bytes = unsafe { windows_sys::Win32::Security::GetLengthSid(user.as_ptr()) };
    if sid_bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let acl_bytes = size_of::<ACL>()
        .checked_add(size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>())
        .and_then(|bytes| bytes.checked_sub(size_of::<u32>()))
        .and_then(|bytes| bytes.checked_add(sid_bytes as usize))
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "private DACL is too large"))?;
    let acl_words = (acl_bytes as usize).div_ceil(size_of::<usize>());
    let mut acl_storage = vec![0_usize; acl_words];
    let acl = acl_storage.as_mut_ptr().cast::<ACL>();
    // SAFETY: ACL storage is aligned, writable, and has acl_bytes capacity.
    if unsafe { InitializeAcl(acl, acl_bytes, ACL_REVISION) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let inheritance = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
    // SAFETY: the initialized ACL and copied current-user SID remain live through directory create.
    if unsafe {
        AddAccessAllowedAceEx(
            acl,
            ACL_REVISION,
            inheritance,
            FILE_ALL_ACCESS,
            user.as_ptr(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut descriptor = SECURITY_DESCRIPTOR::default();
    // SAFETY: descriptor points to writable SECURITY_DESCRIPTOR storage.
    if unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: descriptor, SID, and ACL all remain live until CreateDirectoryW returns.
    if unsafe { SetSecurityDescriptorOwner((&raw mut descriptor).cast(), user.as_ptr(), 0) } == 0
        || unsafe { SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, acl, 0) } == 0
        || unsafe {
            SetSecurityDescriptorControl(
                (&raw mut descriptor).cast(),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: (&raw mut descriptor).cast(),
        bInheritHandle: 0,
    };
    let path_wide = windows_long_path(path)?;
    // SAFETY: the UTF-16 path is terminated and all security descriptor storage remains live.
    if unsafe { CreateDirectoryW(path_wide.as_ptr(), &raw mut attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let validation =
        open_directory_handle(path).and_then(|file| require_windows_private_directory(&file));
    if let Err(error) = validation {
        let _ = std::fs::remove_dir(path);
        return Err(error);
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

#[cfg(unix)]
fn open_directory_handle(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn open_bound_directory_handle(path: &Path, _movable: bool) -> io::Result<File> {
    open_directory_handle(path)
}

#[cfg(windows)]
fn open_directory_handle(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, READ_CONTROL, SYNCHRONIZE,
    };

    OpenOptions::new()
        .access_mode(
            FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_READ_ATTRIBUTES
                | FILE_TRAVERSE
                | READ_CONTROL
                | SYNCHRONIZE,
        )
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// Return every distinct 8.3 spelling that can address an existing Windows path.
///
/// The full short path covers aliases in any parent component; the leaf token covers decoded
/// relative diagnostics. An unavailable alias is represented by an empty vector, while query,
/// sizing, encoding, or identity ambiguity fails closed.
#[cfg(windows)]
pub(crate) fn windows_short_path_aliases(path: &Path) -> io::Result<Vec<OsString>> {
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let wide = windows_long_path(path)?;

    // SAFETY: wide is a live NUL-terminated input and the documented zero-sized query accepts a
    // null output pointer.
    let required = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut short = vec![0_u16; required as usize];
    // SAFETY: both buffers remain live, the input is NUL-terminated, and the output has exactly
    // the capacity returned by the first query.
    let copied = unsafe { GetShortPathNameW(wide.as_ptr(), short.as_mut_ptr(), required) };
    if copied == 0 {
        return Err(io::Error::last_os_error());
    }
    if copied >= required || short[copied as usize] != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short-path query returned an ambiguous buffer length",
        ));
    }
    short.truncate(copied as usize);
    if short.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short-path query returned an embedded NUL",
        ));
    }

    let short_path = PathBuf::from(OsString::from_wide(&short));
    let original = Handle::from_path(path)?;
    let shortened = Handle::from_path(&short_path)?;
    if !handles_match(&original, &shortened)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short-path query returned a different filesystem object",
        ));
    }

    let mut aliases = Vec::new();
    if short_path != path {
        aliases.push(short_path.as_os_str().to_owned());
    }
    if let (Some(short_leaf), Some(long_leaf)) = (short_path.file_name(), path.file_name()) {
        if short_leaf != long_leaf {
            aliases.push(short_leaf.to_owned());
        }
    }
    aliases.sort();
    aliases.dedup();
    Ok(aliases)
}

#[cfg(windows)]
fn open_bound_directory_handle(path: &Path, movable: bool) -> io::Result<File> {
    if !movable {
        return open_directory_handle(path);
    }

    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, READ_CONTROL,
        SYNCHRONIZE,
    };

    OpenOptions::new()
        .access_mode(DELETE | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | READ_CONTROL | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_directory_handle(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_bound_directory_handle(path: &Path, _movable: bool) -> io::Result<File> {
    open_directory_handle(path)
}

#[cfg(unix)]
pub(crate) fn private_file_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}

#[cfg(not(unix))]
pub(crate) fn private_file_options() -> OpenOptions {
    OpenOptions::new()
}

#[cfg(unix)]
fn owned_file_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}

#[cfg(windows)]
fn owned_file_options() -> OpenOptions {
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_SHARE_READ};

    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ);
    options
}

#[cfg(not(any(unix, windows)))]
fn owned_file_options() -> OpenOptions {
    OpenOptions::new()
}

fn open_private_file(path: &Path) -> io::Result<File> {
    private_file_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(target_os = "linux")]
fn publish_owned_file(
    source: &OwnedFile,
    destination_parent: &BoundDirectory,
    destination: &Path,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let destination_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path has no final component",
        )
    })?;
    if destination.parent() != Some(destination_parent.requested_path.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output is not an immediate child of its bound parent",
        ));
    }
    let source_path = CString::new(format!(
        "/proc/self/fd/{}",
        source.handle.as_file().as_raw_fd()
    ))
    .expect("descriptor path contains no NUL");
    let destination_name = CString::new(destination_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output name contains a NUL byte",
        )
    })?;

    // SAFETY: both C strings live for the call, both descriptors are held by
    // their owners, and linkat does not retain any supplied pointer.
    let result = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source_path.as_ptr(),
            destination_parent.handle.as_file().as_raw_fd(),
            destination_name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct DestinationVolumeCloneUnsupported {
    destination: PathBuf,
    source: io::Error,
}

#[cfg(any(target_os = "macos", test))]
impl fmt::Display for DestinationVolumeCloneUnsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "destination volume does not support fclonefileat cloning required for fail-closed publication at {}: {}",
            self.destination.display(),
            self.source
        )
    }
}

#[cfg(any(target_os = "macos", test))]
impl Error for DestinationVolumeCloneUnsupported {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(any(target_os = "macos", test))]
fn contextualize_clonefileat_error(
    error: io::Error,
    destination: &Path,
    enotsup: i32,
) -> io::Error {
    if error.raw_os_error() != Some(enotsup) {
        return error;
    }

    let kind = error.kind();
    io::Error::new(
        kind,
        DestinationVolumeCloneUnsupported {
            destination: destination.to_owned(),
            source: error,
        },
    )
}

#[cfg(target_os = "macos")]
fn publish_owned_file(
    source: &OwnedFile,
    destination_parent: &BoundDirectory,
    destination: &Path,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let destination_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path has no final component",
        )
    })?;
    if destination.parent() != Some(destination_parent.requested_path.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output is not an immediate child of its bound parent",
        ));
    }
    let destination_name = CString::new(destination_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output name contains a NUL byte",
        )
    })?;

    // SAFETY: both descriptors and the destination C string remain live for
    // the call. fclonefileat creates the destination without replacing it.
    let result = unsafe {
        libc::fclonefileat(
            source.handle.as_file().as_raw_fd(),
            destination_parent.handle.as_file().as_raw_fd(),
            destination_name.as_ptr(),
            0,
        )
    };
    if result != 0 {
        return Err(contextualize_clonefileat_error(
            io::Error::last_os_error(),
            destination,
            libc::ENOTSUP,
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn publish_owned_file(
    source: &OwnedFile,
    destination_parent: &BoundDirectory,
    destination: &Path,
) -> io::Result<()> {
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_LINK_INFORMATION, FileLinkInformation, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let destination_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path has no final component",
        )
    })?;
    if destination.parent() != Some(destination_parent.requested_path.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output is not an immediate child of its bound parent",
        ));
    }
    let destination_name: Vec<u16> = destination_name.encode_wide().collect();
    let name_bytes = destination_name
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output name is too long"))?;
    let information_bytes = offset_of!(FILE_LINK_INFORMATION, FileName)
        .checked_add(name_bytes as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output name is too long"))?
        .max(size_of::<FILE_LINK_INFORMATION>());
    let information_length = u32::try_from(information_bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "output name is too long"))?;
    let word_count = information_bytes.div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; word_count];
    let information = storage.as_mut_ptr().cast::<FILE_LINK_INFORMATION>();

    // SAFETY: storage is pointer-aligned, large enough for the fixed header and
    // UTF-16 tail, and all raw handles remain owned for the duration of the call.
    let status = unsafe {
        (*information).Anonymous.ReplaceIfExists = false;
        (*information).RootDirectory = destination_parent.handle.as_file().as_raw_handle() as _;
        (*information).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            destination_name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            destination_name.len(),
        );
        let mut io_status = IO_STATUS_BLOCK::default();
        NtSetInformationFile(
            source.handle.as_file().as_raw_handle() as _,
            &mut io_status,
            information.cast(),
            information_length,
            FileLinkInformation,
        )
    };
    if status < 0 {
        // SAFETY: RtlNtStatusToDosError is a pure status-code conversion.
        let windows_error = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(windows_error as i32));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn publish_owned_file(
    _source: &OwnedFile,
    _destination_parent: &BoundDirectory,
    _destination: &Path,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic handle-source publication is unsupported on this platform",
    ))
}

fn prepare_artifact_file(
    source: PathBuf,
    destination: PathBuf,
    absolute_destination: PathBuf,
    artifact_root: BoundDirectory,
) -> io::Result<PreparedDocumentPdf> {
    if source != absolute_destination
        || source.parent() != Some(artifact_root.requested_path.as_path())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact-owned output must be the bound diagnostic document.pdf",
        ));
    }
    artifact_root.require_current()?;
    let metadata = std::fs::symlink_metadata(&source)?;
    if path_metadata_is_alias(&metadata) || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("document PDF is not a regular file: {}", source.display()),
        ));
    }
    let file = owned_file_options().read(true).write(true).open(&source)?;
    let file = OwnedFile::bind_retained(source, file)?;
    let (sha256, bytes) = hash_open_file(file.handle.as_file(), &file.path)?;
    file.require_current()?;
    artifact_root.require_current()?;
    Ok(PreparedDocumentPdf {
        destination,
        storage: Some(PreparedDocumentPdfStorage::Artifact {
            artifact_root,
            file,
        }),
        sha256,
        bytes,
    })
}

fn prepare_new_file(source: &Path, destination: &Path) -> io::Result<PreparedDocumentPdf> {
    let file_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path has no final component",
        )
    })?;
    let file_name = file_name.to_owned();
    let absolute_destination = std::path::absolute(destination)?;
    let source = std::path::absolute(source)?;
    let requested_parent = absolute_destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output has no parent"))?
        .to_owned();
    let destination_parent = BoundDirectory::open(requested_parent)?;
    let parent = destination_parent.requested_path.clone();
    let publication_destination = parent.join(&file_name);
    match std::fs::symlink_metadata(&publication_destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("output already exists: {}", destination.display()),
            ));
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(error) => return Err(error),
    }
    let source_path_metadata = std::fs::symlink_metadata(&source)?;
    if path_metadata_is_alias(&source_path_metadata) || !source_path_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("document PDF is not a regular file: {}", source.display()),
        ));
    }
    let mut source_file = File::open(&source)?;
    let source_metadata = source_file.metadata()?;

    for attempt in 0..32 {
        let mut temporary_name = OsString::from(".");
        temporary_name.push(&file_name);
        temporary_name.push(format!(".pliego-{}-{attempt}.tmp", std::process::id()));
        let temporary_path = parent.join(temporary_name);
        let temporary_file = match owned_file_options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let mut temporary = OwnedFile::bind(temporary_path, temporary_file)?;

        let write_result = (|| {
            let mut hasher = Sha256::new();
            let mut bytes = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = source_file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                temporary.handle.as_file_mut().write_all(&buffer[..read])?;
                hasher.update(&buffer[..read]);
                bytes = bytes.checked_add(read as u64).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "document PDF is too large")
                })?;
            }
            if bytes != source_metadata.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "document PDF changed while preparing output",
                ));
            }
            temporary.handle.as_file().sync_all()?;
            Ok((
                format!("sha256:{}", lowercase_hex(&hasher.finalize())),
                bytes,
            ))
        })();
        let (sha256, bytes) = match write_result {
            Ok(result) => result,
            Err(error) => return Err(error),
        };
        temporary.verify_content(&sha256, bytes)?;
        return Ok(PreparedDocumentPdf {
            destination: destination.to_owned(),
            storage: Some(PreparedDocumentPdfStorage::External {
                publication_destination,
                destination_parent,
                staged: temporary,
            }),
            sha256,
            bytes,
        });
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "all temporary output names already exist beside {}",
            destination.display()
        ),
    ))
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Debug)]
pub enum SessionFailure {
    RootUnavailable { path: PathBuf, source: io::Error },
    RootNotDirectory(PathBuf),
    UnsafeRequestPath(PathBuf),
    DocumentUnavailable { path: PathBuf, source: io::Error },
    DocumentNotFile(PathBuf),
    OutsideRoot { root: PathBuf, path: PathBuf },
}

impl fmt::Display for SessionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootUnavailable { path, .. } => {
                write!(
                    formatter,
                    "document root is unavailable: {}",
                    path.display()
                )
            },
            Self::RootNotDirectory(path) => {
                write!(
                    formatter,
                    "document root is not a directory: {}",
                    path.display()
                )
            },
            Self::UnsafeRequestPath(path) => {
                write!(
                    formatter,
                    "document path may not be absolute or traverse parents: {}",
                    path.display()
                )
            },
            Self::DocumentUnavailable { path, .. } => {
                write!(formatter, "document is unavailable: {}", path.display())
            },
            Self::DocumentNotFile(path) => {
                write!(formatter, "document is not a file: {}", path.display())
            },
            Self::OutsideRoot { root, path } => write!(
                formatter,
                "document is outside root {}: {}",
                root.display(),
                path.display()
            ),
        }
    }
}

impl Error for SessionFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RootUnavailable { source, .. } | Self::DocumentUnavailable { source, .. } => {
                Some(source)
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Seek, Write};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        BUNDLE_FILE_NAME, LocalDocument, MAX_CONTROL_JSON_BYTES, MAX_PUBLICATION_OUTCOME_BYTES,
        OwnedFile, PUBLICATION_COMMITTED_FILE_NAME, PUBLICATION_DIRECTORY_NAME,
        PUBLICATION_LEASE_FILE_NAME, PUBLICATION_OUTCOME_FILE_NAME, PUBLICATION_PLAN_FILE_NAME,
        PUBLICATION_PREPARED_FILE_NAME, PublicationRecoveryState, SessionArtifacts, SessionFailure,
        WebResourceLoadRole, contextualize_clonefileat_error, create_private_directory,
        path_metadata_is_alias, promote_staged_artifacts, remove_empty_private_container,
        serialize_publication_outcome, validate_staged_artifacts,
    };

    #[test]
    fn clonefileat_enotsup_reports_destination_volume_without_reclassifying_other_errors() {
        const SYNTHETIC_ENOTSUP: i32 = 45;
        let destination = PathBuf::from("volume/report.pdf");
        let unsupported = std::io::Error::from_raw_os_error(SYNTHETIC_ENOTSUP);
        let expected_kind = unsupported.kind();

        let contextualized =
            contextualize_clonefileat_error(unsupported, &destination, SYNTHETIC_ENOTSUP);
        assert_eq!(contextualized.kind(), expected_kind);
        assert!(contextualized.to_string().contains("destination volume"));
        assert!(contextualized.to_string().contains("fclonefileat"));
        assert!(
            contextualized
                .to_string()
                .contains(&destination.display().to_string())
        );
        let source = contextualized
            .get_ref()
            .unwrap()
            .downcast_ref::<super::DestinationVolumeCloneUnsupported>()
            .unwrap();
        assert_eq!(source.source.raw_os_error(), Some(SYNTHETIC_ENOTSUP));

        let passthrough = contextualize_clonefileat_error(
            std::io::Error::from_raw_os_error(5),
            &destination,
            SYNTHETIC_ENOTSUP,
        );
        assert_eq!(passthrough.raw_os_error(), Some(5));
        assert!(!passthrough.to_string().contains("destination volume"));
    }

    fn replace_open_file(file: &mut std::fs::File, bytes: &[u8]) {
        file.set_len(0).unwrap();
        file.rewind().unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    fn test_temp_dir() -> PathBuf {
        std::env::temp_dir().canonicalize().unwrap()
    }

    fn assert_planned_stage_promotes_and_resumes(shorthand_output: bool) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-staged-plan-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(format!(".pliego-runtime-{}", "a".repeat(32)));
        create_private_directory(&container).unwrap();
        let stage = container.join(format!("stage-{unique}"));
        let public = sandbox.join("artifacts");
        let output = if shorthand_output {
            public.join("document.pdf")
        } else {
            sandbox.join("invoice.pdf")
        };
        let render_id = format!("sha256:staged-plan-{unique}");
        let request_fingerprint = format!("sha256:staged-request-{unique}");
        let artifacts =
            SessionArtifacts::create_staged_with_render_id(&stage, &public, &render_id).unwrap();
        artifacts.write_document_pdf(b"%PDF-staged-plan").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let journal = artifacts
            .begin_publication(&output, &request_fingerprint)
            .unwrap();
        assert!(matches!(
            journal.recover().unwrap(),
            PublicationRecoveryState::Planned
        ));
        let staged_plan = fs::read(
            stage
                .join(PUBLICATION_DIRECTORY_NAME)
                .join(PUBLICATION_PLAN_FILE_NAME),
        )
        .unwrap();
        let staged_plan_json: serde_json::Value = serde_json::from_slice(&staged_plan).unwrap();
        assert_eq!(
            staged_plan_json["artifact_root"],
            std::path::absolute(&public)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert!(
            !staged_plan
                .windows(stage.to_string_lossy().len())
                .any(|window| window == stage.to_string_lossy().as_bytes())
        );
        drop(journal);
        drop(artifacts);
        validate_staged_artifacts(&stage, &[&container]).unwrap();

        promote_staged_artifacts(&container, &stage, &public).unwrap();
        assert!(!stage.exists());
        assert_eq!(
            fs::read(
                public
                    .join(PUBLICATION_DIRECTORY_NAME)
                    .join(PUBLICATION_PLAN_FILE_NAME)
            )
            .unwrap(),
            staged_plan
        );
        for (_, bytes) in snapshot_tree(&public) {
            for private in [&stage, &container] {
                let private = private.to_string_lossy();
                assert!(
                    !bytes
                        .windows(private.len())
                        .any(|window| window == private.as_bytes()),
                    "promoted artifact leaked private path {private}"
                );
            }
        }

        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&public, &render_id).unwrap();
        let journal = artifacts
            .resume_publication(&output, &request_fingerprint)
            .unwrap();
        assert!(matches!(
            journal.recover().unwrap(),
            PublicationRecoveryState::Planned
        ));
        drop(journal);
        drop(artifacts);
        // The shorthand output is the artifact-owned PDF itself, so atomic tree promotion makes
        // it visible in the recoverable Planned state. An external output remains withheld until
        // the parent commits the publication.
        if shorthand_output {
            assert_eq!(fs::read(&output).unwrap(), b"%PDF-staged-plan");
        } else {
            assert!(!output.exists());
        }

        fs::remove_dir_all(&public).unwrap();
        assert!(remove_empty_private_container(&container).unwrap());
        assert!(!container.exists());
        fs::remove_dir(&sandbox).unwrap();
    }

    #[test]
    fn staged_external_plan_survives_atomic_promotion_and_resumes_planned() {
        assert_planned_stage_promotes_and_resumes(false);
    }

    #[test]
    fn staged_shorthand_plan_survives_atomic_promotion_and_resumes_planned() {
        assert_planned_stage_promotes_and_resumes(true);
    }

    #[test]
    fn staged_promotion_is_exclusive_and_preserves_both_roots_on_collision() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-staged-collision-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(format!(".pliego-private-{unique}"));
        create_private_directory(&container).unwrap();
        let stage = container.join(format!("stage-{unique}"));
        let public = sandbox.join("artifacts");
        let artifacts =
            SessionArtifacts::create_staged_with_render_id(&stage, &public, "sha256:collision")
                .unwrap();
        artifacts.write_document_pdf(b"%PDF-private").unwrap();
        drop(artifacts);
        fs::create_dir(&public).unwrap();
        fs::write(public.join("sentinel"), b"caller-owned").unwrap();

        let error = promote_staged_artifacts(&container, &stage, &public).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(public.join("sentinel")).unwrap(), b"caller-owned");
        assert_eq!(
            fs::read(stage.join("document.pdf")).unwrap(),
            b"%PDF-private"
        );
        assert!(!remove_empty_private_container(&container).unwrap());
        assert!(stage.exists());

        fs::remove_dir_all(&public).unwrap();
        fs::remove_dir_all(&stage).unwrap();
        fs::remove_dir(&container).unwrap();
        fs::remove_dir(&sandbox).unwrap();
    }

    #[test]
    fn staged_validation_rejects_hard_linked_regular_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-staged-hardlink-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(format!(".pliego-private-{unique}"));
        create_private_directory(&container).unwrap();
        let stage = container.join(format!("stage-{unique}"));
        let public = sandbox.join("artifacts");
        let artifacts =
            SessionArtifacts::create_staged_with_render_id(&stage, &public, "sha256:hardlink")
                .unwrap();
        artifacts.write_document_pdf(b"%PDF-linked").unwrap();
        fs::hard_link(stage.join("document.pdf"), stage.join("linked.pdf")).unwrap();
        drop(artifacts);

        let error = validate_staged_artifacts(&stage, &[]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("single-link regular file"));
        assert!(!public.exists());

        fs::remove_dir_all(&stage).unwrap();
        fs::remove_dir(&container).unwrap();
        fs::remove_dir(&sandbox).unwrap();
    }

    #[test]
    fn staged_validation_rejects_private_random_leaf_tokens_in_artifact_bytes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-staged-token-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(format!(".pliego-runtime-{}", "a".repeat(32)));
        create_private_directory(&container).unwrap();
        let stage = container.join(format!("stage-{unique}"));
        let public = sandbox.join("artifacts");
        let artifacts =
            SessionArtifacts::create_staged_with_render_id(&stage, &public, "sha256:token")
                .unwrap();
        let private_leaf = container.file_name().unwrap().to_string_lossy();
        #[cfg(windows)]
        let private_leaf = private_leaf.to_ascii_uppercase();
        fs::write(stage.join("leak.txt"), private_leaf.as_bytes()).unwrap();
        drop(artifacts);

        let error = validate_staged_artifacts(&stage, &[]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("private staging path"));
        assert!(!public.exists());

        fs::remove_dir_all(&stage).unwrap();
        fs::remove_dir(&container).unwrap();
        fs::remove_dir(&sandbox).unwrap();
    }

    #[test]
    fn staged_validation_decodes_json_string_escapes_before_private_path_scan() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-staged-json-token-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(format!(".pliego-runtime-{}", "a".repeat(32)));
        create_private_directory(&container).unwrap();
        let stage = container.join("artifacts");
        let public = sandbox.join("public-artifacts");
        let artifacts = SessionArtifacts::create_staged_with_render_id(
            &stage,
            &public,
            "sha256:json-escaped-private-token",
        )
        .unwrap();
        let private_leaf = container.file_name().unwrap().to_string_lossy();
        let escaped = format!(
            r#"{{"path":"\u002e{}"}}"#,
            private_leaf
                .strip_prefix('.')
                .expect("private fixture leaf starts with a dot")
        );
        fs::write(stage.join("layout-debug.json"), escaped).unwrap();
        drop(artifacts);

        let error = validate_staged_artifacts(&stage, &[]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("decoded private staging path"));
        assert!(!public.exists());

        fs::remove_dir_all(&stage).unwrap();
        fs::remove_dir(&container).unwrap();
        fs::remove_dir(&sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn staged_validation_supports_non_utf8_private_paths_and_scans_their_raw_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-staged-non-utf8-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(std::ffi::OsString::from_vec(vec![
            b'.', b'p', b'l', b'i', b'e', b'g', b'o', b'-', 0x80, b'-', b'1', b'2', b'3', b'4',
        ]));
        create_private_directory(&container).unwrap();
        let stage = container.join("artifacts");
        let public = sandbox.join("public-artifacts");
        let artifacts = SessionArtifacts::create_staged_with_render_id(
            &stage,
            &public,
            "sha256:non-utf8-private-path",
        )
        .unwrap();
        artifacts
            .write_document_pdf(b"%PDF-non-utf8-private-path")
            .unwrap();
        drop(artifacts);

        validate_staged_artifacts(&stage, &[&container, &stage]).unwrap();
        fs::write(
            stage.join("raw-private-path.bin"),
            container.as_os_str().as_bytes(),
        )
        .unwrap();
        let error = validate_staged_artifacts(&stage, &[&container, &stage]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("private staging path"));
        assert!(!public.exists());

        fs::remove_dir_all(&stage).unwrap();
        fs::remove_dir(&container).unwrap();
        fs::remove_dir(&sandbox).unwrap();
    }

    #[test]
    fn control_json_rejects_oversized_expected_length_before_reading() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-control-json-limit-{}-{unique}",
            std::process::id()
        ));
        let artifacts = SessionArtifacts::create(&sandbox).unwrap();
        fs::write(sandbox.join("environment.json"), b"{}").unwrap();

        let error = artifacts
            .read_json_artifact(
                "environment.json",
                "sha256:not-consulted",
                MAX_CONTROL_JSON_BYTES + 1,
            )
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("control JSON"));
        assert!(error.to_string().contains("byte limit"));

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    fn snapshot_tree(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        fn visit(
            root: &std::path::Path,
            directory: &std::path::Path,
            snapshot: &mut Vec<(String, Vec<u8>)>,
        ) {
            let mut entries: Vec<_> = fs::read_dir(directory)
                .unwrap()
                .map(Result::unwrap)
                .collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let metadata = fs::symlink_metadata(&path).unwrap();
                if path_metadata_is_alias(&metadata) {
                    snapshot.push((
                        relative,
                        format!("symlink:{}", fs::read_link(&path).unwrap().display()).into_bytes(),
                    ));
                } else if metadata.is_dir() {
                    snapshot.push((format!("{relative}/"), Vec::new()));
                    visit(root, &path, snapshot);
                } else {
                    let bytes = fs::read(&path).unwrap_or_else(|error| {
                        format!("unreadable:{:?}:{}", error.kind(), metadata.len()).into_bytes()
                    });
                    snapshot.push((relative, bytes));
                }
            }
        }

        let mut snapshot = Vec::new();
        visit(root, root, &mut snapshot);
        snapshot
    }

    #[cfg(windows)]
    #[test]
    fn windows_junction_metadata_is_rejected_as_an_alias() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-junction-metadata-{}-{unique}",
            std::process::id()
        ));
        let target = sandbox.join("target");
        let junction = sandbox.join("junction");
        fs::create_dir_all(&target).unwrap();
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success(), "failed to create junction fixture");

        let metadata = fs::symlink_metadata(&junction).unwrap();
        assert!(path_metadata_is_alias(&metadata));

        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(&sandbox).unwrap();
    }

    struct PreservedPublicationFixture {
        sandbox: PathBuf,
        artifacts: PathBuf,
        output: PathBuf,
        staging: PathBuf,
        render_id: String,
        request_fingerprint: String,
    }

    fn preserved_publication_fixture(prefix: &str) -> PreservedPublicationFixture {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let output = sandbox.join("invoice.pdf");
        let render_id = format!("sha256:{prefix}-render");
        let request_fingerprint = format!("sha256:{prefix}-request");
        let artifacts =
            SessionArtifacts::create_with_render_id(&artifact_path, &render_id).unwrap();
        let journal = artifacts
            .begin_publication(&output, &request_fingerprint)
            .unwrap();
        artifacts.write_scene(br#"{"schema":"fixture"}"#).unwrap();
        artifacts.write_document_pdf(b"%PDF-preserved").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let staging = prepared.prepared_file_path().to_owned();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        journal
            .record_prepared(
                &prepared,
                &bundle,
                &serialize_publication_outcome(&serde_json::json!({
                    "document_pdf": output.to_string_lossy(),
                    "render_id": render_id,
                    "status": "rendered",
                }))
                .unwrap(),
            )
            .unwrap();
        prepared.preserve_for_recovery();
        bundle.preserve();
        drop(journal);
        drop(artifacts);
        PreservedPublicationFixture {
            sandbox,
            artifacts: artifact_path,
            output,
            staging,
            render_id,
            request_fingerprint,
        }
    }

    fn assert_recovery_rejects_bundle_change(label: &str, change: impl FnOnce(&std::path::Path)) {
        let fixture = preserved_publication_fixture(&format!("pliego-bundle-{label}"));
        change(&fixture.artifacts);
        let before = snapshot_tree(&fixture.artifacts);
        let staging_before = fs::read(&fixture.staging).unwrap();
        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&fixture.artifacts, &fixture.render_id)
                .unwrap();
        let recovery = artifacts
            .resume_publication(&fixture.output, &fixture.request_fingerprint)
            .unwrap();
        let error = recovery.recover().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        drop(recovery);
        drop(artifacts);
        assert_eq!(snapshot_tree(&fixture.artifacts), before);
        assert_eq!(fs::read(&fixture.staging).unwrap(), staging_before);
        assert!(!fixture.output.exists());
        assert!(
            !fixture
                .artifacts
                .join(PUBLICATION_DIRECTORY_NAME)
                .join(PUBLICATION_COMMITTED_FILE_NAME)
                .exists()
        );
        fs::remove_dir_all(fixture.sandbox).unwrap();
    }

    #[test]
    fn publication_lease_is_exclusive_and_plan_is_idempotent() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-publication-lease-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let render_id = "sha256:lease-fixture";
        let request_fingerprint = "sha256:lease-request";
        let artifacts = SessionArtifacts::create_with_render_id(&artifact_path, render_id).unwrap();
        let output = sandbox.join("invoice.pdf");

        let journal = artifacts
            .begin_publication(&output, request_fingerprint)
            .unwrap();
        assert_eq!(
            journal.recover().unwrap(),
            PublicationRecoveryState::Planned
        );
        let leased = artifacts
            .resume_publication(&output, request_fingerprint)
            .unwrap_err();
        assert_eq!(leased.kind(), std::io::ErrorKind::WouldBlock);
        let first_plan = fs::read(
            artifacts
                .directory()
                .join(PUBLICATION_DIRECTORY_NAME)
                .join(PUBLICATION_PLAN_FILE_NAME),
        )
        .unwrap();

        drop(journal);
        drop(artifacts);
        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&artifact_path, render_id).unwrap();
        let reopened = artifacts
            .resume_publication(&output, request_fingerprint)
            .unwrap();
        assert_eq!(
            reopened.recover().unwrap(),
            PublicationRecoveryState::Planned
        );
        assert_eq!(
            fs::read(
                artifacts
                    .directory()
                    .join(PUBLICATION_DIRECTORY_NAME)
                    .join(PUBLICATION_PLAN_FILE_NAME)
            )
            .unwrap(),
            first_plan
        );

        drop(reopened);
        let mismatch = artifacts
            .resume_publication(&output, "sha256:different-request")
            .unwrap_err();
        assert_eq!(mismatch.kind(), std::io::ErrorKind::InvalidData);
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dropping_publication_journal_unlocks_a_duplicated_descriptor() {
        use std::os::fd::{AsRawFd, FromRawFd};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-publication-duplicated-lease-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let render_id = "sha256:duplicated-lease-fixture";
        let request_fingerprint = "sha256:duplicated-lease-request";
        let artifacts = SessionArtifacts::create_with_render_id(&artifact_path, render_id).unwrap();
        let output = sandbox.join("invoice.pdf");
        let journal = artifacts
            .begin_publication(&output, request_fingerprint)
            .unwrap();

        // SAFETY: `journal` owns this live descriptor for the duration of `dup`.
        let duplicated_fd = unsafe { libc::dup(journal.lease.as_file().as_raw_fd()) };
        assert!(
            duplicated_fd >= 0,
            "cannot duplicate publication lease: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `duplicated_fd` is a fresh owned descriptor returned by `dup` above.
        let inherited_lease = unsafe { std::fs::File::from_raw_fd(duplicated_fd) };

        drop(journal);
        let reopened = artifacts
            .resume_publication(&output, request_fingerprint)
            .unwrap();
        assert_eq!(
            reopened.recover().unwrap(),
            PublicationRecoveryState::Planned
        );
        assert!(inherited_lease.metadata().unwrap().is_file());

        drop(reopened);
        drop(inherited_lease);
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn prepared_and_committed_receipts_are_hash_linked_and_excluded_from_bundle() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-publication-receipts-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create_with_render_id(
            sandbox.join("artifacts"),
            "sha256:receipt-fixture",
        )
        .unwrap();
        let output = sandbox.join("invoice.pdf");
        let journal = artifacts
            .begin_publication(&output, "sha256:receipt-request")
            .unwrap();
        artifacts.write_document_pdf(b"%PDF-receipt").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let mut prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let summary = serde_json::json!({
            "document_pdf": output.to_string_lossy(),
            "render_id": "sha256:receipt-fixture",
            "status": "rendered",
        });
        let mut expected_summary_bytes = serde_json::to_vec(&summary).unwrap();
        expected_summary_bytes.push(b'\n');
        let prepared_receipt = journal
            .record_prepared(&prepared, &bundle, &expected_summary_bytes)
            .unwrap();

        let bundle_json: serde_json::Value =
            serde_json::from_slice(&fs::read(bundle.path()).unwrap()).unwrap();
        assert!(
            bundle_json["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| !entry["path"].as_str().unwrap().starts_with("publication/"))
        );

        prepared.commit(&bundle).unwrap();
        journal
            .record_committed(&prepared_receipt, Some(&bundle))
            .unwrap();
        bundle.preserve();
        let PublicationRecoveryState::Committed {
            summary: recovered_summary,
            cli_bytes,
            recovered,
        } = journal.recover().unwrap()
        else {
            panic!("committed transaction should be terminal")
        };
        assert!(!recovered);
        assert_eq!(recovered_summary, summary);
        assert_eq!(cli_bytes, expected_summary_bytes);
        let publication = artifacts.directory().join(PUBLICATION_DIRECTORY_NAME);
        let prepared_json: serde_json::Value = serde_json::from_slice(
            &fs::read(publication.join(PUBLICATION_PREPARED_FILE_NAME)).unwrap(),
        )
        .unwrap();
        let committed_json: serde_json::Value = serde_json::from_slice(
            &fs::read(publication.join(PUBLICATION_COMMITTED_FILE_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(prepared_json["schema"], "pliego.publication-prepared");
        assert_eq!(committed_json["prepared_sha256"], prepared_receipt.sha256);
        assert_eq!(
            fs::read(publication.join(PUBLICATION_OUTCOME_FILE_NAME)).unwrap(),
            expected_summary_bytes
        );

        drop(prepared);
        drop(journal);
        drop(artifacts);
        fs::remove_file(output).unwrap();
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn recovery_finalizes_an_exact_visible_output_without_republishing_it() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-publication-recover-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let render_id = "sha256:recover-fixture";
        let request_fingerprint = "sha256:recover-request";
        let artifacts = SessionArtifacts::create_with_render_id(&artifact_path, render_id).unwrap();
        let output = sandbox.join("invoice.pdf");
        let journal = artifacts
            .begin_publication(&output, request_fingerprint)
            .unwrap();
        artifacts.write_document_pdf(b"%PDF-recover").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let mut prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let summary = serde_json::json!({
            "document_pdf": output.to_string_lossy(),
            "render_id": render_id,
            "status": "rendered",
        });
        let mut expected_summary_bytes = serde_json::to_vec(&summary).unwrap();
        expected_summary_bytes.push(b'\n');
        journal
            .record_prepared(&prepared, &bundle, &expected_summary_bytes)
            .unwrap();
        prepared.commit(&bundle).unwrap();
        bundle.preserve();
        drop(prepared);
        drop(journal);
        drop(artifacts);

        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&artifact_path, render_id).unwrap();
        let recovery = artifacts
            .resume_publication(&output, request_fingerprint)
            .unwrap();
        let PublicationRecoveryState::Committed {
            summary: recovered_summary,
            cli_bytes,
            recovered,
        } = recovery.recover().unwrap()
        else {
            panic!("prepared transaction should recover")
        };
        assert!(recovered);
        assert_eq!(recovered_summary, summary);
        assert_eq!(cli_bytes, expected_summary_bytes);
        let publication = artifact_path.join(PUBLICATION_DIRECTORY_NAME);
        let committed_before = fs::read(publication.join(PUBLICATION_COMMITTED_FILE_NAME)).unwrap();
        let second = recovery.recover().unwrap();
        let PublicationRecoveryState::Committed {
            summary: second_summary,
            cli_bytes: second_summary_bytes,
            recovered: second_recovered,
        } = second
        else {
            panic!("second recovery should remain terminal")
        };
        assert!(!second_recovered);
        assert_eq!(second_summary, summary);
        assert_eq!(second_summary_bytes, expected_summary_bytes);
        assert_eq!(
            fs::read(publication.join(PUBLICATION_COMMITTED_FILE_NAME)).unwrap(),
            committed_before
        );
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-recover");

        drop(recovery);
        drop(artifacts);
        fs::remove_file(output).unwrap();
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn process_restart_republishes_prepared_staging_and_returns_exact_summary_bytes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-publication-relink-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let render_id = "sha256:relink-fixture";
        let request_fingerprint = "sha256:relink-request";
        let output = sandbox.join("invoice.pdf");
        let artifacts = SessionArtifacts::create_with_render_id(&artifact_path, render_id).unwrap();
        let journal = artifacts
            .begin_publication(&output, request_fingerprint)
            .unwrap();
        artifacts.write_document_pdf(b"%PDF-relink").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let staging_path = prepared.prepared_file_path().to_owned();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let summary = serde_json::json!({
            "document_pdf": output.to_string_lossy(),
            "render_id": render_id,
            "status": "rendered",
        });
        let mut expected_summary_bytes = serde_json::to_vec(&summary).unwrap();
        expected_summary_bytes.push(b'\n');
        journal
            .record_prepared(&prepared, &bundle, &expected_summary_bytes)
            .unwrap();
        prepared.preserve_for_recovery();
        bundle.preserve();
        drop(journal);
        drop(artifacts);
        assert!(!output.exists());
        assert!(staging_path.exists());

        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&artifact_path, render_id).unwrap();
        let recovery = artifacts
            .resume_publication(&output, request_fingerprint)
            .unwrap();
        let PublicationRecoveryState::Committed {
            summary: recovered_summary,
            cli_bytes,
            recovered,
        } = recovery.recover().unwrap()
        else {
            panic!("prepared transaction should recover after restart")
        };
        assert!(recovered);
        assert_eq!(recovered_summary, summary);
        assert_eq!(cli_bytes, expected_summary_bytes);
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-relink");
        assert!(!staging_path.exists());
        assert!(
            artifact_path
                .join(PUBLICATION_DIRECTORY_NAME)
                .join(PUBLICATION_COMMITTED_FILE_NAME)
                .is_file()
        );

        drop(recovery);
        drop(artifacts);
        fs::remove_file(output).unwrap();
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn bundle_closure_rejects_live_mutation_before_any_receipt_or_output() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-live-bundle-mutation-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create_with_render_id(
            sandbox.join("artifacts"),
            "sha256:live-bundle-mutation",
        )
        .unwrap();
        let output = sandbox.join("invoice.pdf");
        let journal = artifacts
            .begin_publication(&output, "sha256:live-bundle-request")
            .unwrap();
        artifacts.write_scene(b"sealed scene").unwrap();
        artifacts.write_document_pdf(b"%PDF-live-mutation").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        fs::write(artifacts.directory().join("scene.json"), b"mutated scene").unwrap();
        let before = snapshot_tree(artifacts.directory());

        let error = journal
            .record_prepared(
                &prepared,
                &bundle,
                &serialize_publication_outcome(&serde_json::json!({ "status": "rendered" }))
                    .unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(snapshot_tree(artifacts.directory()), before);
        let publication = artifacts.directory().join(PUBLICATION_DIRECTORY_NAME);
        assert!(!publication.join(PUBLICATION_OUTCOME_FILE_NAME).exists());
        assert!(!publication.join(PUBLICATION_PREPARED_FILE_NAME).exists());
        assert!(!output.exists());

        drop(bundle);
        drop(prepared);
        drop(journal);
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn recovery_rejects_mutated_added_deleted_and_temp_shaped_bundle_entries_without_mutation() {
        assert_recovery_rejects_bundle_change("mutated", |artifacts| {
            fs::write(artifacts.join("scene.json"), b"changed after preparation").unwrap();
        });
        assert_recovery_rejects_bundle_change("added", |artifacts| {
            fs::write(artifacts.join("unexpected.txt"), b"added after preparation").unwrap();
        });
        assert_recovery_rejects_bundle_change("temp-shaped-added", |artifacts| {
            let resources = artifacts.join("resources");
            fs::create_dir_all(&resources).unwrap();
            fs::write(
                resources.join(".x.pliego-hidden.tmp"),
                b"unsealed temp-shaped artifact",
            )
            .unwrap();
        });
        assert_recovery_rejects_bundle_change("deleted", |artifacts| {
            fs::remove_file(artifacts.join("scene.json")).unwrap();
        });
    }

    #[test]
    fn oversized_outcome_is_rejected_before_sealing_or_output_visibility() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-oversized-outcome-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create_with_render_id(
            sandbox.join("artifacts"),
            "sha256:oversized-outcome",
        )
        .unwrap();
        let output = sandbox.join("invoice.pdf");
        let journal = artifacts
            .begin_publication(&output, "sha256:oversized-outcome-request")
            .unwrap();
        artifacts.write_document_pdf(b"%PDF-oversized").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let summary = serde_json::json!({
            "readiness": "x".repeat(MAX_PUBLICATION_OUTCOME_BYTES as usize),
        });
        let before = snapshot_tree(artifacts.directory());

        let oversized_outcome = serialize_publication_outcome(&summary).unwrap();
        let error = journal
            .record_prepared(&prepared, &bundle, &oversized_outcome)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(snapshot_tree(artifacts.directory()), before);
        let publication = artifacts.directory().join(PUBLICATION_DIRECTORY_NAME);
        assert!(!publication.join(PUBLICATION_OUTCOME_FILE_NAME).exists());
        assert!(!publication.join(PUBLICATION_PREPARED_FILE_NAME).exists());
        assert!(!output.exists());

        drop(bundle);
        drop(prepared);
        drop(journal);
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn nonregular_committed_receipt_and_lease_are_rejected_without_mutation() {
        let fixture = preserved_publication_fixture("pliego-directory-committed");
        let committed = fixture
            .artifacts
            .join(PUBLICATION_DIRECTORY_NAME)
            .join(PUBLICATION_COMMITTED_FILE_NAME);
        fs::create_dir(&committed).unwrap();
        let before = snapshot_tree(&fixture.artifacts);
        let staging_before = fs::read(&fixture.staging).unwrap();
        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&fixture.artifacts, &fixture.render_id)
                .unwrap();
        let recovery = artifacts
            .resume_publication(&fixture.output, &fixture.request_fingerprint)
            .unwrap();
        assert_eq!(
            recovery.recover().unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        drop(recovery);
        drop(artifacts);
        assert_eq!(snapshot_tree(&fixture.artifacts), before);
        assert_eq!(fs::read(&fixture.staging).unwrap(), staging_before);
        assert!(!fixture.output.exists());
        fs::remove_dir_all(fixture.sandbox).unwrap();

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-directory-lease-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let output = sandbox.join("invoice.pdf");
        let artifacts =
            SessionArtifacts::create_with_render_id(&artifact_path, "sha256:directory-lease")
                .unwrap();
        let journal = artifacts
            .begin_publication(&output, "sha256:directory-lease-request")
            .unwrap();
        drop(journal);
        drop(artifacts);
        let lease = artifact_path
            .join(PUBLICATION_DIRECTORY_NAME)
            .join(PUBLICATION_LEASE_FILE_NAME);
        fs::remove_file(&lease).unwrap();
        fs::create_dir(&lease).unwrap();
        let before = snapshot_tree(&artifact_path);
        let artifacts = SessionArtifacts::open_for_publication_recovery(
            &artifact_path,
            "sha256:directory-lease",
        )
        .unwrap();
        let error = artifacts
            .resume_publication(&output, "sha256:directory-lease-request")
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        drop(artifacts);
        assert_eq!(snapshot_tree(&artifact_path), before);
        assert!(!output.exists());
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn visible_exact_output_does_not_hide_a_mutated_bundle_entry() {
        let fixture = preserved_publication_fixture("pliego-visible-mutated-bundle");
        fs::write(&fixture.output, b"%PDF-preserved").unwrap();
        fs::write(
            fixture.artifacts.join("scene.json"),
            b"mutated after output visibility",
        )
        .unwrap();
        let before = snapshot_tree(&fixture.artifacts);
        let staging_before = fs::read(&fixture.staging).unwrap();
        let output_before = fs::read(&fixture.output).unwrap();
        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&fixture.artifacts, &fixture.render_id)
                .unwrap();
        let recovery = artifacts
            .resume_publication(&fixture.output, &fixture.request_fingerprint)
            .unwrap();
        assert_eq!(
            recovery.recover().unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        drop(recovery);
        drop(artifacts);
        assert_eq!(snapshot_tree(&fixture.artifacts), before);
        assert_eq!(fs::read(&fixture.staging).unwrap(), staging_before);
        assert_eq!(fs::read(&fixture.output).unwrap(), output_before);
        assert!(
            !fixture
                .artifacts
                .join(PUBLICATION_DIRECTORY_NAME)
                .join(PUBLICATION_COMMITTED_FILE_NAME)
                .exists()
        );
        fs::remove_dir_all(fixture.sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_optional_receipts_and_outcome_fail_before_recovery_mutation() {
        use std::os::unix::fs::symlink;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-dangling-prepared-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let output = sandbox.join("invoice.pdf");
        let artifacts =
            SessionArtifacts::create_with_render_id(&artifact_path, "sha256:dangling-prepared")
                .unwrap();
        let journal = artifacts
            .begin_publication(&output, "sha256:dangling-prepared-request")
            .unwrap();
        drop(journal);
        drop(artifacts);
        let publication = artifact_path.join(PUBLICATION_DIRECTORY_NAME);
        symlink(
            "missing-prepared-target",
            publication.join(PUBLICATION_PREPARED_FILE_NAME),
        )
        .unwrap();
        let before = snapshot_tree(&artifact_path);
        let artifacts = SessionArtifacts::open_for_publication_recovery(
            &artifact_path,
            "sha256:dangling-prepared",
        )
        .unwrap();
        let recovery = artifacts
            .resume_publication(&output, "sha256:dangling-prepared-request")
            .unwrap();
        assert_eq!(
            recovery.recover().unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        drop(recovery);
        drop(artifacts);
        assert_eq!(snapshot_tree(&artifact_path), before);
        assert!(!output.exists());
        fs::remove_dir_all(&sandbox).unwrap();

        for name in [
            PUBLICATION_COMMITTED_FILE_NAME,
            PUBLICATION_OUTCOME_FILE_NAME,
        ] {
            let fixture = preserved_publication_fixture(&format!("pliego-dangling-{name}"));
            let path = fixture
                .artifacts
                .join(PUBLICATION_DIRECTORY_NAME)
                .join(name);
            if name == PUBLICATION_OUTCOME_FILE_NAME {
                fs::remove_file(&path).unwrap();
            }
            symlink(format!("missing-{name}-target"), &path).unwrap();
            let before = snapshot_tree(&fixture.artifacts);
            let staging_before = fs::read(&fixture.staging).unwrap();
            let artifacts = SessionArtifacts::open_for_publication_recovery(
                &fixture.artifacts,
                &fixture.render_id,
            )
            .unwrap();
            let recovery = artifacts
                .resume_publication(&fixture.output, &fixture.request_fingerprint)
                .unwrap();
            assert_eq!(
                recovery.recover().unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
            drop(recovery);
            drop(artifacts);
            assert_eq!(snapshot_tree(&fixture.artifacts), before);
            assert_eq!(fs::read(&fixture.staging).unwrap(), staging_before);
            assert!(!fixture.output.exists());
            fs::remove_dir_all(fixture.sandbox).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_lease_is_rejected_without_mutating_the_transaction() {
        use std::os::unix::fs::symlink;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-symlink-lease-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_path = sandbox.join("artifacts");
        let output = sandbox.join("invoice.pdf");
        let artifacts =
            SessionArtifacts::create_with_render_id(&artifact_path, "sha256:symlink-lease")
                .unwrap();
        let journal = artifacts
            .begin_publication(&output, "sha256:symlink-lease-request")
            .unwrap();
        drop(journal);
        drop(artifacts);
        let lease = artifact_path
            .join(PUBLICATION_DIRECTORY_NAME)
            .join(PUBLICATION_LEASE_FILE_NAME);
        fs::remove_file(&lease).unwrap();
        symlink(PUBLICATION_PLAN_FILE_NAME, &lease).unwrap();
        let before = snapshot_tree(&artifact_path);

        let artifacts =
            SessionArtifacts::open_for_publication_recovery(&artifact_path, "sha256:symlink-lease")
                .unwrap();
        let error = artifacts
            .resume_publication(&output, "sha256:symlink-lease-request")
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        drop(artifacts);
        assert_eq!(snapshot_tree(&artifact_path), before);
        assert!(!output.exists());
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn retained_binding_failure_never_deletes_the_held_or_caller_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-retained-bind-failure-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let held = sandbox.join("held.pdf");
        let caller = sandbox.join("document.pdf");
        fs::write(&held, b"held diagnostic").unwrap();
        fs::write(&caller, b"caller sentinel").unwrap();

        let error = OwnedFile::bind_retained(caller.clone(), std::fs::File::open(&held).unwrap())
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&held).unwrap(), b"held diagnostic");
        assert_eq!(fs::read(&caller).unwrap(), b"caller sentinel");

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn resolves_a_local_file_and_rejects_escape_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox =
            test_temp_dir().join(format!("pliego-session-{}-{unique}", std::process::id()));
        let root = sandbox.join("root");
        let inside = root.join("index.html");
        let outside = sandbox.join("outside.html");
        fs::create_dir_all(&root).unwrap();
        fs::write(&inside, "<title>inside</title>").unwrap();
        fs::write(&outside, "<title>outside</title>").unwrap();

        let document = LocalDocument::resolve(&root, "index.html").unwrap();
        assert_eq!(document.root(), root.canonicalize().unwrap());
        assert_eq!(document.path(), inside.canonicalize().unwrap());
        assert!(matches!(
            LocalDocument::resolve(&root, "../outside.html"),
            Err(SessionFailure::UnsafeRequestPath(_))
        ));
        assert!(matches!(
            LocalDocument::from_canonical_paths(
                root.canonicalize().unwrap(),
                outside.canonicalize().unwrap()
            ),
            Err(SessionFailure::OutsideRoot { .. })
        ));

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn writes_the_three_session_traces_as_json_lines() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            test_temp_dir().join(format!("pliego-artifacts-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();

        artifacts.record_state("started", None).unwrap();
        artifacts.record_console("info", "fixture-ready").unwrap();
        artifacts
            .record_resource_request("request-1", "file:///index.html")
            .unwrap();
        let resource_body = b"hello";
        let resource_hash = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        artifacts
            .record_loaded_resource(
                "request-1",
                &["file:///index.html".to_owned()],
                Some(200),
                Some("text/html; charset=utf-8"),
                resource_hash,
                resource_body,
                None,
            )
            .unwrap();
        artifacts
            .record_resource_failure(
                "RESOURCE_DENIED",
                "denied",
                "https://example.test/font.woff2",
                "GET",
                "Font",
                WebResourceLoadRole::DocumentMetadata,
                false,
                Some("file:///index.html"),
                false,
                false,
                "network access is disabled",
            )
            .unwrap();
        artifacts
            .write_readiness(&serde_json::json!({
                "status": "ready",
                "payload": { "fixture": true }
            }))
            .unwrap();
        artifacts
            .write_layout_debug(&serde_json::json!({
                "boxes": [{ "depth": 0, "kind": "block" }],
                "fragments": [{ "depth": 0, "kind": "box" }]
            }))
            .unwrap();

        assert_eq!(artifacts.directory(), directory);
        let state: serde_json::Value = serde_json::from_str(
            fs::read_to_string(directory.join("session-state.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        let console: serde_json::Value = serde_json::from_str(
            fs::read_to_string(directory.join("console.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        let resources: Vec<serde_json::Value> =
            fs::read_to_string(directory.join("resources.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        let readiness: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(directory.join("readiness.json")).unwrap())
                .unwrap();
        let layout_debug: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(directory.join("layout-debug.json")).unwrap())
                .unwrap();
        assert_eq!(state["state"], "started");
        assert_eq!(console["message"], "fixture-ready");
        assert_eq!(resources.len(), 3);
        assert_eq!(resources[0]["status"], "requested");
        assert_eq!(resources[0]["render_id"], artifacts.render_id());
        assert_eq!(resources[0]["policy"], "pliego.resource-policy.v1");
        assert_eq!(resources[0]["request_id"], "request-1");
        assert_eq!(resources[1]["status"], "loaded");
        assert_eq!(resources[1]["request_id"], "request-1");
        assert_eq!(resources[1]["url"], "file:///index.html");
        assert_eq!(resources[1]["urls"][0], "file:///index.html");
        assert_eq!(resources[1]["response_status"], 200);
        assert_eq!(resources[1]["content_type"], "text/html; charset=utf-8");
        assert_eq!(resources[1]["bytes"], resource_body.len());
        assert_eq!(resources[1]["sha256"], resource_hash);
        assert_eq!(resources[1]["resource"], format!("sha256:{resource_hash}"));
        assert_eq!(
            resources[1]["content_hash"],
            format!("sha256:{resource_hash}")
        );
        assert_eq!(resources[1]["cache_result"], serde_json::Value::Null);
        assert_eq!(resources[2]["status"], "denied");
        assert_eq!(resources[2]["code"], "RESOURCE_DENIED");
        assert_eq!(resources[2]["request_id"], serde_json::Value::Null);
        assert_eq!(resources[2]["url"], "https://example.test/font.woff2");
        assert_eq!(resources[2]["destination"], "Font");
        assert_eq!(resources[2]["load_role"], "DocumentMetadata");
        assert_eq!(resources[2]["fatal"], false);
        assert_eq!(resources[2]["cancelled"], true);
        assert_eq!(resources[2]["reason"], "network access is disabled");
        assert_eq!(
            fs::read(directory.join("resources").join(resource_hash)).unwrap(),
            resource_body
        );
        assert_eq!(readiness["payload"]["fixture"], true);
        assert_eq!(readiness["render_id"], artifacts.render_id());
        assert_eq!(layout_debug["boxes"][0]["kind"], "block");

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn writes_exact_scene_artifacts_and_verifies_resource_collisions() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            test_temp_dir().join(format!("pliego-scene-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let scene = br#"{"schema":"pliego.document-scene","version":1,"pages":[]}"#;
        let fonts = serde_json::json!({
            "resources": [{ "resource": "sha256:font" }],
            "instances": []
        });
        let report = serde_json::json!({
            "capture": { "status": "partial", "unsupported_events": [] },
            "preview": { "status": "rendered", "unsupported": [] }
        });
        let pdf_structure = serde_json::json!({
            "schema": "pliego.pdf-structure",
            "version": 1,
            "pages": [],
        });
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let resource = format!("sha256:{digest}");

        artifacts.write_scene(scene).unwrap();
        artifacts.write_fonts(&fonts).unwrap();
        artifacts.write_scene_report(&report).unwrap();
        artifacts.write_scene_preview(b"\x89PNG\r\n\x1a\n").unwrap();
        artifacts.write_document_pdf(b"%PDF-fixture").unwrap();
        artifacts.write_pdf_structure(&pdf_structure).unwrap();
        assert_eq!(
            artifacts
                .write_content_addressed_resource(&resource, b"hello")
                .unwrap(),
            format!("resources/{digest}")
        );
        artifacts
            .write_content_addressed_resource(&resource, b"hello")
            .unwrap();

        assert_eq!(fs::read(directory.join("scene.json")).unwrap(), scene);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join("fonts.json")).unwrap()
            )
            .unwrap(),
            fonts
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join("scene-report.json")).unwrap()
            )
            .unwrap(),
            report
        );
        assert_eq!(
            fs::read(directory.join("scene-preview.png")).unwrap(),
            b"\x89PNG\r\n\x1a\n"
        );
        assert_eq!(
            fs::read(directory.join("document.pdf")).unwrap(),
            b"%PDF-fixture"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join("pdf-structure.json")).unwrap()
            )
            .unwrap(),
            pdf_structure
        );
        assert_eq!(
            fs::read(directory.join("resources").join(digest)).unwrap(),
            b"hello"
        );
        let collision = artifacts
            .write_content_addressed_resource(&resource, b"different")
            .unwrap_err();
        assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn refuses_to_reuse_an_existing_session_directory() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            test_temp_dir().join(format!("pliego-exclusive-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        artifacts.record_console("info", "preserve-me").unwrap();
        let original = fs::read(directory.join("console.jsonl")).unwrap();

        let collision = SessionArtifacts::create(&directory).unwrap_err();
        assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(directory.join("console.jsonl")).unwrap(), original);

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_an_aliased_artifact_parent_without_leaving_a_directory() {
        use std::os::unix::fs::symlink;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-artifact-parent-alias-{}-{unique}",
            std::process::id()
        ));
        let real_parent = sandbox.join("real-parent");
        let alias = sandbox.join("alias");
        fs::create_dir_all(&real_parent).unwrap();
        symlink(&real_parent, &alias).unwrap();
        let requested = alias.join("artifacts");

        let error = SessionArtifacts::create(&requested).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!real_parent.join("artifacts").exists());

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn keeps_an_explicit_render_id_independent_of_the_artifact_path() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = test_temp_dir().join(format!(
            "pliego-explicit-id-{}-{unique}",
            std::process::id()
        ));
        let artifacts =
            SessionArtifacts::create_with_render_id(&directory, "sha256:stable-fixture").unwrap();

        artifacts
            .write_readiness(&serde_json::json!({ "status": "ready" }))
            .unwrap();
        let readiness: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("readiness.json")).unwrap()).unwrap();
        assert_eq!(artifacts.render_id(), "sha256:stable-fixture");
        assert_eq!(readiness["render_id"], "sha256:stable-fixture");

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn writes_a_typed_failure_bound_to_the_render_id() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = test_temp_dir().join(format!(
            "pliego-failure-artifact-{}-{unique}",
            std::process::id()
        ));
        let artifacts =
            SessionArtifacts::create_with_render_id(&directory, "sha256:failed-fixture").unwrap();

        artifacts
            .write_failure("FIXTURE_FAILED", "fixture failure")
            .unwrap();
        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("failure.json")).unwrap()).unwrap();
        assert_eq!(failure["status"], "failed");
        assert_eq!(failure["render_id"], "sha256:failed-fixture");
        assert_eq!(failure["error"]["code"], "FIXTURE_FAILED");
        assert_eq!(failure["error"]["message"], "fixture failure");

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomically_publishes_a_pdf_without_replacing_an_existing_output() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox =
            test_temp_dir().join(format!("pliego-publish-{}-{unique}", std::process::id()));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-first").unwrap();

        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        assert!(!output.exists());
        prepared.commit_for_test().unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-first");
        assert_eq!(
            fs::read(artifacts.directory().join("document.pdf")).unwrap(),
            b"%PDF-first"
        );
        let collision = artifacts.prepare_document_pdf(&output).unwrap_err();
        assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-first");
        assert!(fs::read_dir(&sandbox).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn publishes_to_a_single_code_unit_file_name() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-short-output-name-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("x");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();

        artifacts
            .prepare_document_pdf(&output)
            .unwrap()
            .commit_for_test()
            .unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-owned");

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn rejects_mutated_prepared_bytes_before_publication() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-handle-drift-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        let mut prepared = artifacts.prepare_document_pdf(&output).unwrap();

        replace_open_file(
            prepared.prepared_file_mut().handle.as_file_mut(),
            b"attacker bytes",
        );
        let error = prepared.commit_for_test().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!output.exists());

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn binds_the_artifact_owned_pdf_without_external_publication() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-self-publication-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();

        artifacts.record_state("rendered", None).unwrap();
        let mut prepared = artifacts
            .prepare_document_pdf(artifacts.directory().join("document.pdf"))
            .unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        prepared.commit(&bundle).unwrap();
        bundle.preserve();
        assert_eq!(
            fs::read(artifacts.directory().join("document.pdf")).unwrap(),
            b"%PDF-owned"
        );

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_output_paths_with_symlinked_parent_components() {
        use std::os::unix::fs::symlink;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-output-parent-alias-{}-{unique}",
            std::process::id()
        ));
        let real_output = sandbox.join("real-output");
        let alias = sandbox.join("alias");
        fs::create_dir_all(&real_output).unwrap();
        symlink(&real_output, &alias).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();

        let error = artifacts
            .prepare_document_pdf(alias.join("invoice.pdf"))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!real_output.join("invoice.pdf").exists());

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_replaced_output_parent_before_commit() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-output-parent-replacement-{}-{unique}",
            std::process::id()
        ));
        let output_parent = sandbox.join("output");
        let moved_parent = sandbox.join("held-output");
        fs::create_dir_all(&output_parent).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        let prepared = artifacts
            .prepare_document_pdf(output_parent.join("invoice.pdf"))
            .unwrap();

        fs::rename(&output_parent, &moved_parent).unwrap();
        fs::create_dir(&output_parent).unwrap();
        let error = prepared.commit_for_test().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!output_parent.join("invoice.pdf").exists());
        assert!(!moved_parent.join("invoice.pdf").exists());

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_replaced_artifact_root_before_bundle_creation() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-artifact-root-replacement-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let artifact_root = artifacts.directory().to_owned();
        let moved_root = sandbox.join("held-artifacts");
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();

        fs::rename(&artifact_root, &moved_root).unwrap();
        fs::create_dir(&artifact_root).unwrap();
        let error = artifacts.write_prepared_bundle(&prepared).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!artifact_root.join(BUNDLE_FILE_NAME).exists());
        assert!(!output.exists());

        drop(prepared);
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn publishes_the_held_file_after_the_staging_path_is_replaced() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-path-replacement-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        let moved = sandbox.join("held.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let staged_path = prepared.prepared_file_path().to_owned();

        fs::rename(&staged_path, &moved).unwrap();
        fs::write(&staged_path, b"attacker sentinel").unwrap();
        prepared.commit_for_test().unwrap();

        assert_eq!(fs::read(&output).unwrap(), b"%PDF-owned");
        assert_eq!(fs::read(&staged_path).unwrap(), b"attacker sentinel");
        assert_eq!(fs::read(&moved).unwrap(), b"%PDF-owned");
        fs::remove_file(staged_path).unwrap();
        fs::remove_file(moved).unwrap();
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn keeps_the_windows_staging_name_exclusive_until_commit() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-path-exclusive-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let staged_path = prepared.prepared_file_path().to_owned();

        assert!(fs::remove_file(&staged_path).is_err());
        assert!(fs::write(&staged_path, b"attacker sentinel").is_err());
        prepared.commit_for_test().unwrap();

        assert_eq!(fs::read(&output).unwrap(), b"%PDF-owned");
        assert!(!staged_path.exists());
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn keeps_bound_windows_directories_exclusive_until_publication_finishes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-bound-directory-exclusive-{}-{unique}",
            std::process::id()
        ));
        let output_parent = sandbox.join("output");
        fs::create_dir_all(&output_parent).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let artifact_root = artifacts.directory().to_owned();
        let output = output_parent.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();

        assert!(fs::rename(&output_parent, sandbox.join("moved-output")).is_err());
        assert!(fs::rename(&artifact_root, sandbox.join("moved-artifacts")).is_err());
        prepared.commit_for_test().unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-owned");

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn binds_sorted_artifacts_and_the_published_pdf_to_the_render_id() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox =
            test_temp_dir().join(format!("pliego-bundle-{}-{unique}", std::process::id()));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create_with_render_id(
            sandbox.join("artifacts"),
            "sha256:bundle-fixture",
        )
        .unwrap();
        let output = sandbox.join("invoice.pdf");

        artifacts.write_scene(b"{}\n").unwrap();
        artifacts.write_document_pdf(b"%PDF-bundle").unwrap();
        let mut prepared = artifacts.prepare_document_pdf(&output).unwrap();
        assert!(!output.exists());
        artifacts.record_state("started", None).unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared_bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let bundle: serde_json::Value =
            serde_json::from_slice(&fs::read(prepared_bundle.path()).unwrap()).unwrap();

        assert_eq!(bundle["schema"], "pliego.bundle");
        assert_eq!(bundle["version"], 1);
        assert_eq!(bundle["render_id"], "sha256:bundle-fixture");
        assert_eq!(bundle["output"]["path"], output.to_string_lossy().as_ref());
        assert_eq!(bundle["output"]["bytes"], 11);
        assert_eq!(
            bundle["output"]["sha256"],
            "sha256:1e3325b692c5c5d3a7e354870e4ee26947d6d4614f48e5e4d2125bb944eeae16"
        );
        let paths: Vec<_> = bundle["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "console.jsonl",
                "document.pdf",
                "resources.jsonl",
                "scene.json",
                "session-state.jsonl",
            ]
        );
        prepared.commit(&prepared_bundle).unwrap();
        prepared_bundle.preserve();
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-bundle");

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn rejects_mutated_bundle_bytes_before_publication() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-bundle-handle-drift-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let mut bundle = artifacts.write_prepared_bundle(&prepared).unwrap();

        replace_open_file(
            bundle
                .file
                .as_mut()
                .expect("prepared bundle is present")
                .handle
                .as_file_mut(),
            b"attacker bundle",
        );
        let error = bundle.verify().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!output.exists());
        bundle.discard().unwrap();
        drop(prepared);

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_discard_a_replacement_bundle_path() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-bundle-path-replacement-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let mut prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let bundle_path = bundle.path().to_owned();
        let moved = artifacts.directory().join("held-bundle.json");

        fs::rename(&bundle_path, &moved).unwrap();
        fs::write(&bundle_path, b"caller bundle").unwrap();
        let verify_error = bundle.verify().unwrap_err();
        assert_eq!(verify_error.kind(), std::io::ErrorKind::InvalidData);
        let discard_error = bundle.discard().unwrap_err();
        assert_eq!(discard_error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&bundle_path).unwrap(), b"caller bundle");
        assert!(!output.exists());

        fs::remove_file(bundle_path).unwrap();
        fs::remove_file(moved).unwrap();
        drop(prepared);
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_replaced_artifact_root_even_when_bundle_inode_is_linked_back() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-bundle-root-replacement-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifact_root = sandbox.join("artifacts");
        let moved_root = sandbox.join("moved-artifacts");
        let artifacts = SessionArtifacts::create(&artifact_root).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let mut prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();

        fs::rename(&artifact_root, &moved_root).unwrap();
        fs::create_dir(&artifact_root).unwrap();
        fs::hard_link(
            moved_root.join(BUNDLE_FILE_NAME),
            artifact_root.join(BUNDLE_FILE_NAME),
        )
        .unwrap();

        let error = prepared.commit(&bundle).unwrap_err();
        assert!(matches!(error, super::PreparedPublicationError::Bundle(_)));
        assert!(!output.exists());
        let discard_error = bundle.discard().unwrap_err();
        assert_eq!(discard_error.kind(), std::io::ErrorKind::InvalidData);
        assert!(artifact_root.join(BUNDLE_FILE_NAME).exists());

        fs::remove_dir_all(&artifact_root).unwrap();
        drop(artifacts);
        fs::remove_dir_all(&moved_root).unwrap();
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn keeps_the_windows_bundle_name_exclusive_until_preserved() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-bundle-path-exclusive-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let mut prepared = artifacts.prepare_document_pdf(&output).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let bundle_path = bundle.path().to_owned();

        assert!(fs::remove_file(&bundle_path).is_err());
        assert!(fs::write(&bundle_path, b"attacker bundle").is_err());
        bundle.verify().unwrap();
        prepared.commit(&bundle).unwrap();
        bundle.preserve();

        assert_eq!(fs::read(&output).unwrap(), b"%PDF-owned");
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn failed_finalization_drops_the_prepared_output_without_publishing() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-drop-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-prepared").unwrap();

        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        assert!(!output.exists());
        assert!(fs::read_dir(&sandbox).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));
        drop(prepared);

        assert!(!output.exists());
        assert!(fs::read_dir(&sandbox).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));
        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn bundle_rejects_document_pdf_drift_before_publication() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-drift-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-original").unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        artifacts.write_document_pdf(b"%PDF-mutated").unwrap();
        artifacts.record_state("rendered", None).unwrap();

        let error = artifacts.write_prepared_bundle(&prepared).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("changed after output preparation")
        );
        assert!(!output.exists());
        drop(prepared);

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn publication_collision_preserves_caller_bytes_and_cleans_staging() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-collision-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        fs::write(&output, b"caller sentinel").unwrap();

        let error = prepared.commit_for_test().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&output).unwrap(), b"caller sentinel");
        assert!(fs::read_dir(&sandbox).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn bundle_collision_does_not_publish_the_prepared_output() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-bundle-collision-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        let prepared = artifacts.prepare_document_pdf(&output).unwrap();
        artifacts.record_state("rendered", None).unwrap();
        fs::write(
            artifacts.directory().join(BUNDLE_FILE_NAME),
            b"caller bundle",
        )
        .unwrap();

        let error = artifacts.write_prepared_bundle(&prepared).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!output.exists());
        assert_eq!(
            fs::read(artifacts.directory().join(BUNDLE_FILE_NAME)).unwrap(),
            b"caller bundle"
        );
        drop(prepared);

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn preparation_fails_after_all_bounded_temporary_names_are_taken() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-prepared-exhaustion-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-owned").unwrap();
        for attempt in 0..32 {
            fs::write(
                sandbox.join(format!(
                    ".invoice.pdf.pliego-{}-{attempt}.tmp",
                    std::process::id()
                )),
                b"occupied",
            )
            .unwrap();
        }

        let error = artifacts.prepare_document_pdf(&output).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!output.exists());

        drop(artifacts);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn finalization_preflight_rejects_read_only_session_state() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = test_temp_dir().join(format!(
            "pliego-read-only-state-{}-{unique}",
            std::process::id()
        ));
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let state = directory.join("session-state.jsonl");
        let mut permissions = fs::metadata(&state).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&state, permissions).unwrap();

        assert!(artifacts.require_session_state_append_access().is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(&state).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&state, permissions).unwrap();
        }
        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn creates_private_session_directories_and_files() {
        use std::os::unix::fs::PermissionsExt;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            test_temp_dir().join(format!("pliego-private-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

        artifacts.write_scene(b"{}").unwrap();
        artifacts
            .write_environment(&serde_json::json!({ "phase": "initial" }))
            .unwrap();
        artifacts
            .write_environment(&serde_json::json!({ "phase": "final" }))
            .unwrap();
        artifacts
            .write_content_addressed_resource(&format!("sha256:{digest}"), b"hello")
            .unwrap();

        for path in [&directory, &directory.join("resources")] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for path in [
            directory.join("console.jsonl"),
            directory.join("resources.jsonl"),
            directory.join("session-state.jsonl"),
            directory.join("scene.json"),
            directory.join("environment.json"),
            directory.join("resources").join(digest),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_private_directory_accepts_no_extended_acl() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-no-extended-acl-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let private = sandbox.join(format!(".pliego-runtime-{}", "a".repeat(32)));
        create_private_directory(&private).unwrap();
        fs::remove_dir(private).unwrap();
        fs::remove_dir(sandbox).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_private_directory_rejects_an_inherited_extended_acl() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-inherited-acl-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let status = std::process::Command::new("chmod")
            .arg("+a")
            .arg("everyone allow list,search,file_inherit,directory_inherit")
            .arg(&sandbox)
            .status()
            .unwrap();
        assert!(status.success());

        let private = sandbox.join(format!(".pliego-runtime-{}", "a".repeat(32)));
        let error = create_private_directory(&private).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!error.to_string().contains(".pliego-runtime-"));
        assert!(
            !error
                .to_string()
                .contains(&private.to_string_lossy().into_owned())
        );
        assert!(!private.exists());

        let status = std::process::Command::new("chmod")
            .arg("-N")
            .arg(&sandbox)
            .status()
            .unwrap();
        assert!(status.success());
        fs::remove_dir(sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_directory_creation_supports_extended_length_paths() {
        use std::os::windows::ffi::OsStrExt;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-long-private-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let parent = sandbox.join("p".repeat(180));
        fs::create_dir(&parent).unwrap();
        let private = parent.join(format!(".pliego-runtime-{}", "a".repeat(32)));
        assert!(private.as_os_str().encode_wide().count() >= 260);

        create_private_directory(&private).unwrap();
        assert!(private.is_dir());
        super::windows_short_path_aliases(&private).unwrap();

        fs::remove_dir(&private).unwrap();
        fs::remove_dir(&parent).unwrap();
        fs::remove_dir(&sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_short_path_alias_is_scanned_when_available() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = test_temp_dir().join(format!(
            "pliego-short-alias-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(format!(
            ".pliego-runtime-{unique:032x}{:032x}",
            std::process::id()
        ));
        create_private_directory(&container).unwrap();
        let stage = container.join("artifacts");
        fs::create_dir(&stage).unwrap();

        let aliases = super::windows_short_path_aliases(&container).unwrap();
        let Some(full_alias) = aliases
            .iter()
            .find(|alias| PathBuf::from(alias).is_absolute())
        else {
            eprintln!(
                "SKIP: volume assigned no distinct 8.3 alias; GetShortPathNameW query succeeded"
            );
            fs::remove_dir_all(sandbox).unwrap();
            return;
        };
        let prefixes = super::promotion_private_prefixes(&[&container]).unwrap();
        for alias in &aliases {
            let alias = alias.to_string_lossy();
            assert!(
                prefixes.iter().any(|prefix| prefix == alias.as_bytes()),
                "short alias was not routed through private prefix generation: {alias}"
            );
        }
        fs::write(
            stage.join("short-alias-leak.bin"),
            full_alias.to_string_lossy().as_bytes(),
        )
        .unwrap();

        let error = validate_staged_artifacts(&stage, &[]).unwrap_err();
        assert!(error.to_string().contains("private staging path"));

        fs::remove_dir_all(sandbox).unwrap();
    }
}
