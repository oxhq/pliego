/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::ffi::OsString;
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
}

#[derive(Debug)]
struct OwnedFile {
    path: PathBuf,
    handle: Handle,
    remove_on_drop: bool,
}

impl BoundDirectory {
    fn open(path: PathBuf) -> io::Result<Self> {
        let requested_path = std::path::absolute(path)?;
        require_path_without_aliases(&requested_path)?;
        let path = requested_path.canonicalize()?;
        require_directory_without_symlink(&path)?;
        let handle = Handle::from_file(open_directory_handle(&path)?)?;
        let directory = Self {
            requested_path,
            path,
            handle,
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
            let current = Handle::from_file(open_directory_handle(path)?)?;
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
        })
    }

    fn identity(&self) -> io::Result<String> {
        self.require_current()?;
        open_file_identity(self.handle.as_file(), &self.path)
    }
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
        self.output.path == requested_output &&
            self.output.sha256 == output.sha256 &&
            self.output.bytes == output.bytes
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
        render_id: &str,
        request_fingerprint: &str,
        output: &Path,
    ) -> io::Result<Self> {
        let (plan, output_parent) =
            Self::expected_plan(artifact_root, render_id, request_fingerprint, output)?;
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
        render_id: &str,
        request_fingerprint: &str,
        output: &Path,
    ) -> io::Result<Self> {
        let (plan, output_parent) =
            Self::expected_plan(artifact_root, render_id, request_fingerprint, output)?;
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
        let output_parent = BoundDirectory::open(output_parent_path)?;
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
                artifact_root: receipt_path(&artifact_root.requested_path)?,
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
        if self.artifact_root.identity()? != self.plan.artifact_root_identity ||
            self.output_parent.identity()? != self.plan.output_parent_identity
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
        if output_artifact.path != self.plan.output ||
            output.output_parent_identity()? != self.plan.output_parent_identity ||
            !bundle.matches_output(&output_artifact, &self.plan.requested_output)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "prepared output does not match the publication plan",
            ));
        }
        if outcome_bytes.len() as u64 > MAX_PUBLICATION_OUTCOME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "publication outcome exceeds the {MAX_PUBLICATION_OUTCOME_BYTES}-byte limit"
                ),
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
        let staging_is_owned_temporary = staging.parent() == output.parent() &&
            staging
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with('.') && name.contains(".pliego-") && name.ends_with(".tmp")
                });
        if receipt.schema != "pliego.publication-prepared" ||
            receipt.version != 1 ||
            receipt.transaction_id != self.plan.transaction_id ||
            receipt.plan_sha256 != self.plan_sha256 ||
            receipt.output.path != self.plan.output ||
            receipt.bundle.path != expected_bundle ||
            receipt.outcome.path != expected_outcome ||
            (!staging_is_output && !staging_is_owned_temporary) ||
            receipt.staging.sha256 != receipt.output.sha256 ||
            receipt.staging.bytes != receipt.output.bytes
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
        if receipt.schema != "pliego.publication-committed" ||
            receipt.version != 1 ||
            receipt.transaction_id != self.plan.transaction_id ||
            receipt.prepared_sha256 != prepared_sha256
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
        create_private_directory(&directory.join("resources"))?;
        for name in ["console.jsonl", "resources.jsonl", "session-state.jsonl"] {
            private_file_options()
                .write(true)
                .create_new(true)
                .open(directory.join(name))?;
        }
        Ok(Self {
            directory,
            directory_binding,
            render_id,
        })
    }

    pub(crate) fn open_for_publication_recovery(
        directory: impl AsRef<Path>,
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
        Ok(Self {
            directory,
            directory_binding,
            render_id,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
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
        if digest.len() != 64 ||
            !digest
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
fn path_metadata_is_alias(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn path_metadata_is_alias(metadata: &std::fs::Metadata) -> bool {
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
            if path.parent() == Some(root) &&
                path.file_name().and_then(|name| name.to_str()) ==
                    Some(PUBLICATION_DIRECTORY_NAME)
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
    if !path_matches_handle(&path, &handle)? ||
        bytes.len() as u64 != handle.as_file().metadata()?.len()
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
    if bytes.len() as u64 > MAX_PUBLICATION_OUTCOME_BYTES ||
        bytes.len() as u64 != handle.as_file().metadata()?.len() ||
        !path_matches_handle(path, &handle)? ||
        receipt_sha256(&bytes) != artifact.sha256 ||
        bytes.len() as u64 != artifact.bytes
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
    if offset != artifact.bytes ||
        handle.as_file().metadata()?.len() != artifact.bytes ||
        receipt_sha256(&bytes) != artifact.sha256 ||
        !path_matches_handle(&bundle_path, handle)?
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
    if manifest.schema != "pliego.bundle" ||
        manifest.version != 1 ||
        manifest.render_id != render_id ||
        manifest.output != *output ||
        manifest
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
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

#[cfg(unix)]
fn open_directory_handle(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn open_directory_handle(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE,
    };

    OpenOptions::new()
        .access_mode(FILE_ADD_FILE | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_directory_handle(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn private_file_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}

#[cfg(not(unix))]
fn private_file_options() -> OpenOptions {
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
    if source != absolute_destination ||
        source.parent() != Some(artifact_root.requested_path.as_path())
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
    if publication_destination.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", destination.display()),
        ));
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
        BUNDLE_FILE_NAME, LocalDocument, MAX_PUBLICATION_OUTCOME_BYTES, OwnedFile,
        PUBLICATION_COMMITTED_FILE_NAME, PUBLICATION_DIRECTORY_NAME, PUBLICATION_LEASE_FILE_NAME,
        PUBLICATION_OUTCOME_FILE_NAME, PUBLICATION_PLAN_FILE_NAME, PUBLICATION_PREPARED_FILE_NAME,
        PublicationRecoveryState, SessionArtifacts, SessionFailure, WebResourceLoadRole,
        contextualize_clonefileat_error, path_metadata_is_alias, serialize_publication_outcome,
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
}
