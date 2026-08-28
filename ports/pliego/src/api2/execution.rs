/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! One-shot API 2 execution and atomic job-root publication.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rand::random;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[cfg(test)]
use super::artifacts::EncodedResource;
use super::artifacts::{EncodedProfileNullScene, render_profile_null_pdf};
use super::input_job::load_input_job;
use super::render_job::ResolvedRenderJob;
use super::{
    EngineIdentity, InvocationError, current_engine_identity, decode_render_request,
    encode_profile_null_scene, hex_lower,
};
use crate::document_session::{DocumentCaptureOutcome, DocumentSession, SessionError};
use crate::session::{
    BoundDirectory, create_private_directory, promote_staged_artifacts_into,
    remove_empty_private_container,
};

const PDF_MEDIA_TYPE: &str = "application/pdf";
const BUNDLE_MEDIA_TYPE: &str = "application/vnd.pliego.bundle-manifest+json";
const JSON_MEDIA_TYPE: &str = "application/json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiagnosticRetention {
    None,
    OnFailure,
    Always,
}

pub(crate) enum Api2CommandOutcome {
    Result { stdout: Vec<u8>, success: bool },
    TransportFailure { diagnostic: String },
}

impl Api2CommandOutcome {
    fn transport(diagnostic: impl Into<String>) -> Self {
        Self::TransportFailure {
            diagnostic: diagnostic.into(),
        }
    }
}

pub(crate) fn execute_render(
    reader: &mut impl Read,
    servo_base: &str,
) -> Result<Api2CommandOutcome, InvocationError> {
    // Reject malformed request frames before performing the comparatively expensive executable
    // identity hash. Establish identity before pairing the decoded request with its input job: once
    // that job is accepted, every remaining failure is either a terminal RenderResult or a
    // transport failure and must never be mislabeled as an invocation error.
    let request = decode_render_request(reader)?;
    let engine = current_engine_identity(servo_base)?;
    let job_root_path = std::env::current_dir().map_err(|error| {
        InvocationError::new(format!("cannot resolve the cwd-v1 job root: {error}"))
    })?;
    let loaded = load_input_job(&job_root_path, &request)?;
    let (job_root, input) = loaded.into_parts();
    let job = ResolvedRenderJob::from_resolved_input(request.clone(), input)?;
    let retention = diagnostic_retention(&request);

    let capture =
        DocumentSession::start_api2_execution(job).and_then(|execution| execution.capture());
    match capture {
        Ok(capture) => Ok(finish_success(
            &job_root, request, engine, retention, capture,
        )),
        Err(error) => Ok(finish_failure(
            &job_root,
            request,
            engine,
            retention,
            None,
            classify_session_error(&error),
            error.code,
            error.message,
        )),
    }
}

fn finish_success(
    job_root: &BoundDirectory,
    request: Value,
    engine: EngineIdentity,
    retention: DiagnosticRetention,
    capture: DocumentCaptureOutcome,
) -> Api2CommandOutcome {
    let scene = match encode_profile_null_scene(&request, &capture.capture, |resource| {
        capture.resource_store.resolve_content(resource)
    }) {
        Ok(scene) => scene,
        Err(error) => {
            return finish_failure(
                job_root,
                request,
                engine,
                retention,
                None,
                StableErrorKind::Artifact,
                "SCENE_ENCODING_FAILED".into(),
                error.to_string(),
            );
        },
    };
    let pdf = match render_profile_null_pdf(&scene) {
        Ok(pdf) if !pdf.is_empty() && pdf.starts_with(b"%PDF-") => pdf,
        Ok(_) => {
            return finish_failure(
                job_root,
                request,
                engine,
                retention,
                None,
                StableErrorKind::Artifact,
                "DOCUMENT_PDF_INVALID".into(),
                "generated PDF is empty or has no PDF header".into(),
            );
        },
        Err(error) => {
            return finish_failure(
                job_root,
                request,
                engine,
                retention,
                None,
                StableErrorKind::Artifact,
                "DOCUMENT_PDF_RENDER_FAILED".into(),
                error.to_string(),
            );
        },
    };

    // Retained success diagnostics are committed before delivery. If they cannot be retained, no
    // deterministic delivery is exposed and the accepted request terminates as a transport failure.
    let diagnostics = if retention == DiagnosticRetention::Always {
        let bytes = match canonical_json(&SuccessDiagnostics {
            environment: &request["environment"],
            readiness: &capture.readiness,
        }) {
            Ok(bytes) => bytes,
            Err(error) => return Api2CommandOutcome::transport(error.to_string()),
        };
        match publish_diagnostics(job_root, "environment.json", &bytes) {
            Ok(diagnostics) => diagnostics,
            Err(error) => {
                return Api2CommandOutcome::transport(format!(
                    "cannot retain required API 2 diagnostics: {error}"
                ));
            },
        }
    } else {
        DiagnosticInventory::empty()
    };

    let delivery = match publish_delivery(job_root, pdf, scene) {
        Ok(delivery) => delivery,
        Err(error) => {
            return finish_failure(
                job_root,
                request,
                engine,
                retention,
                (retention == DiagnosticRetention::Always).then_some(diagnostics),
                StableErrorKind::Artifact,
                "DELIVERY_PUBLICATION_FAILED".into(),
                error.to_string(),
            );
        },
    };

    let result = SuccessResult {
        schema: "pliego.render-result",
        version: 1,
        api: super::API_VERSION,
        status: "success",
        request: &request,
        engine,
        delivery,
        conformance: Conformance::not_requested(),
        diagnostics,
        error: None,
    };
    match canonical_json(&result) {
        Ok(stdout) => Api2CommandOutcome::Result {
            stdout,
            success: true,
        },
        Err(error) => Api2CommandOutcome::transport(error.to_string()),
    }
}

fn finish_failure(
    job_root: &BoundDirectory,
    request: Value,
    engine: EngineIdentity,
    retention: DiagnosticRetention,
    retained_diagnostics: Option<DiagnosticInventory>,
    kind: StableErrorKind,
    code: String,
    message: String,
) -> Api2CommandOutcome {
    let diagnostics = if let Some(diagnostics) = retained_diagnostics {
        diagnostics
    } else if retention != DiagnosticRetention::None {
        let bytes = match canonical_json(&FailureDiagnostics {
            code: &code,
            message: &message,
        }) {
            Ok(bytes) => bytes,
            Err(error) => return Api2CommandOutcome::transport(error.to_string()),
        };
        match publish_diagnostics(job_root, "failure.json", &bytes) {
            Ok(diagnostics) => diagnostics,
            Err(error) => {
                return Api2CommandOutcome::transport(format!(
                    "cannot retain required API 2 diagnostics: {error}"
                ));
            },
        }
    } else {
        DiagnosticInventory::empty()
    };
    let result = FailureResult {
        schema: "pliego.render-result",
        version: 1,
        api: super::API_VERSION,
        status: "failed",
        request: &request,
        engine,
        delivery: None,
        conformance: Conformance::not_requested(),
        diagnostics,
        error: ErrorDescriptor { kind },
    };
    match canonical_json(&result) {
        Ok(stdout) => Api2CommandOutcome::Result {
            stdout,
            success: false,
        },
        Err(error) => Api2CommandOutcome::transport(error.to_string()),
    }
}

fn publish_delivery(
    job_root: &BoundDirectory,
    pdf: Vec<u8>,
    scene: EncodedProfileNullScene,
) -> Result<Delivery, PublicationError> {
    let mut entries = Vec::with_capacity(scene.resources.len() + 2);
    let pdf_descriptor = descriptor("document.pdf", PDF_MEDIA_TYPE, &pdf)?;
    entries.push(pdf_descriptor.clone());
    for (address, resource) in &scene.resources {
        let digest = address.strip_prefix("sha256:").ok_or_else(|| {
            PublicationError::new("scene resource has no canonical content-address prefix")
        })?;
        let path = format!("resources/{digest}");
        let entry = descriptor(&path, resource.media_type, &resource.bytes)?;
        if entry.sha256 != *address {
            return Err(PublicationError::new(
                "scene resource path and byte digest do not match",
            ));
        }
        entries.push(entry);
    }
    let scene_descriptor = descriptor("scene.json", scene.media_type, &scene.bytes)?;
    if scene_descriptor.sha256 != scene.sha256 {
        return Err(PublicationError::new(
            "scene descriptor does not match encoded scene identity",
        ));
    }
    entries.push(scene_descriptor.clone());
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));

    let bundle_bytes = canonical_json(&BundleManifest {
        schema: "pliego.bundle-manifest",
        version: 1,
        entries: &entries,
    })?;
    let bundle_descriptor = descriptor("bundle.json", BUNDLE_MEDIA_TYPE, &bundle_bytes)?;

    let staging = PublicationStaging::new(job_root, "delivery")?;
    write_new(&staging.staged.join("document.pdf"), &pdf)?;
    write_new(&staging.staged.join("scene.json"), &scene.bytes)?;
    if !scene.resources.is_empty() {
        let resources = staging.staged.join("resources");
        create_private_directory(&resources)?;
        for (address, resource) in scene.resources {
            let digest = address
                .strip_prefix("sha256:")
                .expect("validated scene resource address");
            write_new(&resources.join(digest), &resource.bytes)?;
        }
        sync_directory(&resources)?;
    }
    write_new(&staging.staged.join("bundle.json"), &bundle_bytes)?;
    sync_directory(&staging.staged)?;
    staging.promote(job_root)?;

    Ok(Delivery {
        pdf: pdf_descriptor,
        scene: scene_descriptor,
        bundle: bundle_descriptor,
    })
}

fn publish_diagnostics(
    job_root: &BoundDirectory,
    name: &str,
    bytes: &[u8],
) -> Result<DiagnosticInventory, PublicationError> {
    let staging = PublicationStaging::new(job_root, "diagnostics")?;
    write_new(&staging.staged.join(name), bytes)?;
    sync_directory(&staging.staged)?;
    staging.promote(job_root)?;
    Ok(DiagnosticInventory {
        retained: true,
        artifacts: vec![ArtifactDescriptor {
            path: format!("diagnostics/{name}"),
            media_type: JSON_MEDIA_TYPE,
            sha256: content_address(bytes),
            bytes: byte_length(bytes)?,
        }],
    })
}

struct PublicationStaging {
    container: PathBuf,
    staged: PathBuf,
    public_name: &'static str,
    promoted: bool,
}

impl PublicationStaging {
    fn new(job_root: &BoundDirectory, public_name: &'static str) -> Result<Self, PublicationError> {
        if !matches!(public_name, "delivery" | "diagnostics") {
            return Err(PublicationError::new("unsupported publication root"));
        }
        let job_root_path = job_root.current_path()?;
        let parent = job_root_path
            .parent()
            .ok_or_else(|| PublicationError::new("cwd-v1 job root has no parent"))?;
        let mut container = None;
        for _ in 0..16 {
            let nonce: [u8; 16] = random();
            let candidate = parent.join(format!(".pliego-api2-stage-{}", hex_lower(&nonce)));
            match create_private_directory(&candidate) {
                Ok(()) => {
                    container = Some(candidate);
                    break;
                },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
                Err(error) => return Err(error.into()),
            }
        }
        let container = container.ok_or_else(|| {
            PublicationError::new("cannot reserve a unique private publication container")
        })?;
        let staged = container.join(public_name);
        if let Err(error) = create_private_directory(&staged) {
            let _ = remove_empty_private_container(&container);
            return Err(error.into());
        }
        Ok(Self {
            container,
            staged,
            public_name,
            promoted: false,
        })
    }

    fn promote(mut self, job_root: &BoundDirectory) -> Result<(), PublicationError> {
        promote_staged_artifacts_into(
            &self.container,
            &self.staged,
            job_root,
            OsStr::new(self.public_name),
        )?;
        self.promoted = true;
        let _ = remove_empty_private_container(&self.container);
        Ok(())
    }
}

impl Drop for PublicationStaging {
    fn drop(&mut self) {
        if !self.promoted {
            // Never recursively delete through a pathname that a same-principal caller can
            // replace while publication is in flight. Non-empty containers are retained as
            // private failure evidence; an empty post-promotion container is removed explicitly.
            let _ = remove_empty_private_container(&self.container);
        }
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), PublicationError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), PublicationError> {
    BoundDirectory::open_private(path.to_owned())?.sync_all()?;
    Ok(())
}

fn descriptor(
    path: &str,
    media_type: &'static str,
    bytes: &[u8],
) -> Result<ArtifactDescriptor, PublicationError> {
    if bytes.is_empty() {
        return Err(PublicationError::new(format!("{path} must not be empty")));
    }
    Ok(ArtifactDescriptor {
        path: path.to_owned(),
        media_type,
        sha256: content_address(bytes),
        bytes: byte_length(bytes)?,
    })
}

fn byte_length(bytes: &[u8]) -> Result<u64, PublicationError> {
    u64::try_from(bytes.len())
        .map_err(|_| PublicationError::new("artifact byte length is not representable"))
}

fn content_address(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_lower(&Sha256::digest(bytes)))
}

fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>, InvocationError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| InvocationError::new(format!("cannot serialize API 2 result: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn diagnostic_retention(request: &Value) -> DiagnosticRetention {
    match request["diagnostics"]["retention"].as_str() {
        Some("none") => DiagnosticRetention::None,
        Some("on-failure") => DiagnosticRetention::OnFailure,
        Some("always") => DiagnosticRetention::Always,
        _ => unreachable!("validated request has an unknown diagnostic retention policy"),
    }
}

fn classify_session_error(error: &SessionError) -> StableErrorKind {
    let code = error.code.as_str();
    if code.starts_with("RESOURCE_") {
        StableErrorKind::Resource
    } else if code.starts_with("READINESS_") {
        StableErrorKind::Readiness
    } else if code.starts_with("SETTLEMENT_") || code.starts_with("CONTROLLED_") {
        StableErrorKind::Settlement
    } else if code.starts_with("SCENE_") || code.starts_with("CAPTURE_") {
        StableErrorKind::Capture
    } else if code.contains("PDF") || code.starts_with("FONT_") {
        StableErrorKind::Artifact
    } else {
        StableErrorKind::Internal
    }
}

#[derive(Debug)]
struct PublicationError(String);

impl PublicationError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PublicationError {}

impl From<std::io::Error> for PublicationError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<InvocationError> for PublicationError {
    fn from(error: InvocationError) -> Self {
        Self(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ArtifactDescriptor {
    path: String,
    media_type: &'static str,
    sha256: String,
    bytes: u64,
}

#[derive(Serialize)]
struct BundleManifest<'a> {
    schema: &'static str,
    version: u32,
    entries: &'a [ArtifactDescriptor],
}

#[derive(Serialize)]
struct Delivery {
    pdf: ArtifactDescriptor,
    scene: ArtifactDescriptor,
    bundle: ArtifactDescriptor,
}

#[derive(Serialize)]
struct Conformance {
    requested: Option<()>,
    status: &'static str,
    evidence: Option<()>,
}

impl Conformance {
    const fn not_requested() -> Self {
        Self {
            requested: None,
            status: "not-requested",
            evidence: None,
        }
    }
}

#[derive(Serialize)]
struct DiagnosticInventory {
    retained: bool,
    artifacts: Vec<ArtifactDescriptor>,
}

impl DiagnosticInventory {
    const fn empty() -> Self {
        Self {
            retained: false,
            artifacts: Vec::new(),
        }
    }
}

#[derive(Serialize)]
struct SuccessDiagnostics<'a> {
    environment: &'a Value,
    readiness: &'a Value,
}

#[derive(Serialize)]
struct FailureDiagnostics<'a> {
    code: &'a str,
    message: &'a str,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum StableErrorKind {
    Resource,
    Readiness,
    Settlement,
    Capture,
    Artifact,
    Internal,
}

#[derive(Serialize)]
struct ErrorDescriptor {
    kind: StableErrorKind,
}

#[derive(Serialize)]
struct SuccessResult<'a> {
    schema: &'static str,
    version: u32,
    api: u32,
    status: &'static str,
    request: &'a Value,
    engine: EngineIdentity,
    delivery: Delivery,
    conformance: Conformance,
    diagnostics: DiagnosticInventory,
    error: Option<()>,
}

#[derive(Serialize)]
struct FailureResult<'a> {
    schema: &'static str,
    version: u32,
    api: u32,
    status: &'static str,
    request: &'a Value,
    engine: EngineIdentity,
    delivery: Option<()>,
    conformance: Conformance,
    diagnostics: DiagnosticInventory,
    error: ErrorDescriptor,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SERVO_BASE_SHA: &str = "313b6d5ecc113b08010ce434140db3ca5abcc71c";

    fn test_root(label: &str) -> PathBuf {
        let nonce: [u8; 16] = random();
        std::env::temp_dir().join(format!(
            "pliego-api2-execution-{label}-{}-{}",
            std::process::id(),
            hex_lower(&nonce)
        ))
    }

    #[test]
    fn required_diagnostics_publication_failure_is_a_transport_failure() {
        let engine = current_engine_identity(TEST_SERVO_BASE_SHA).unwrap();
        for retention in [DiagnosticRetention::OnFailure, DiagnosticRetention::Always] {
            let sandbox = test_root(match retention {
                DiagnosticRetention::OnFailure => "on-failure",
                DiagnosticRetention::Always => "always",
                DiagnosticRetention::None => unreachable!(),
            });
            create_private_directory(&sandbox).unwrap();
            let root = sandbox.join("job");
            create_private_directory(&root).unwrap();
            let bound = BoundDirectory::open_private(root.clone()).unwrap();
            create_private_directory(&root.join("diagnostics")).unwrap();

            let outcome = finish_failure(
                &bound,
                serde_json::json!({"diagnostics":{"retention":"always"}}),
                engine.clone(),
                retention,
                None,
                StableErrorKind::Internal,
                "TEST_FAILURE".into(),
                "forced diagnostic collision".into(),
            );
            match outcome {
                Api2CommandOutcome::TransportFailure { diagnostic } => {
                    assert!(
                        diagnostic.contains("cannot retain required API 2 diagnostics"),
                        "{diagnostic}"
                    );
                },
                Api2CommandOutcome::Result { .. } => {
                    panic!("required diagnostic publication failure emitted a RenderResult")
                },
            }
            assert!(!root.join("delivery").exists());
            assert_eq!(fs::read_dir(root.join("diagnostics")).unwrap().count(), 0);
            drop(bound);
            fs::remove_dir_all(sandbox).unwrap();
        }
    }

    #[test]
    fn failed_publication_never_recursively_deletes_a_replacement_container() {
        let sandbox = test_root("replacement-cleanup");
        create_private_directory(&sandbox).unwrap();
        let root = sandbox.join("job");
        create_private_directory(&root).unwrap();
        let bound = BoundDirectory::open_private(root.clone()).unwrap();
        let staging = PublicationStaging::new(&bound, "delivery").unwrap();
        let original_container = sandbox.join("retained-original");
        fs::rename(&staging.container, &original_container).unwrap();
        create_private_directory(&staging.container).unwrap();
        fs::write(staging.container.join("caller-owned"), b"preserve me").unwrap();

        let replacement_container = staging.container.clone();
        drop(staging);

        assert_eq!(
            fs::read(replacement_container.join("caller-owned")).unwrap(),
            b"preserve me"
        );
        assert!(original_container.join("delivery").is_dir());
        drop(bound);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_publication_staging_uses_protected_owner_only_directories() {
        let sandbox = test_root("private-staging");
        create_private_directory(&sandbox).unwrap();
        let root = sandbox.join("job");
        create_private_directory(&root).unwrap();
        let bound = BoundDirectory::open_private(root.clone()).unwrap();
        let staging = PublicationStaging::new(&bound, "delivery").unwrap();
        let container_path = staging.container.clone();

        let container = BoundDirectory::open_private(staging.container.clone()).unwrap();
        let staged = BoundDirectory::open_private(staging.staged.clone()).unwrap();
        drop(staged);
        drop(container);
        drop(staging);
        drop(bound);
        assert!(container_path.exists());
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn bundle_entries_are_ascii_path_ordered_and_self_excluding() {
        let scene = EncodedProfileNullScene {
            bytes: b"{\"schema\":\"pliego.document-scene\",\"version\":2}\n".to_vec(),
            sha256: content_address(b"{\"schema\":\"pliego.document-scene\",\"version\":2}\n"),
            media_type: "application/vnd.pliego.document-scene+json",
            resources: [(
                content_address(b"font"),
                EncodedResource {
                    media_type: "application/octet-stream",
                    bytes: b"font".to_vec(),
                },
            )]
            .into_iter()
            .collect(),
        };
        let mut entries = vec![
            descriptor("scene.json", scene.media_type, &scene.bytes).unwrap(),
            descriptor("document.pdf", PDF_MEDIA_TYPE, b"%PDF-1.7\n").unwrap(),
        ];
        for (address, resource) in scene.resources {
            entries.push(
                descriptor(
                    &format!("resources/{}", address.trim_start_matches("sha256:")),
                    resource.media_type,
                    &resource.bytes,
                )
                .unwrap(),
            );
        }
        entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        assert_eq!(entries[0].path, "document.pdf");
        assert_eq!(
            entries[1].path,
            format!(
                "resources/{}",
                content_address(b"font").strip_prefix("sha256:").unwrap()
            )
        );
        assert_eq!(entries[2].path, "scene.json");
        assert!(entries.iter().all(|entry| entry.path != "bundle.json"));
    }
}
