/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Semantic validation for an internal render worker's staged artifact tree.
//!
//! The structural promotion validator owns alias, link, filesystem, aggregate-byte, and private
//! path checks. This module owns the smaller semantic allowlist that binds a normally exited
//! worker's receipt to the exact artifacts the parent is willing to expose.

#![cfg(not(any(target_os = "android", target_env = "ohos")))]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::marker::PhantomData;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::de::{self, DeserializeOwned, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use super::DeferredCapturedPublication;
use super::readiness::{Readiness, parse_snapshot};
use super::resource_policy::{MAX_RESPONSE_HEADER_BYTES, MAX_RESPONSE_HEADER_COUNT};
use super::session::MAX_PROMOTION_TREE_ENTRIES;

const MAX_CONTROL_JSON_BYTES: u64 = 1024 * 1024;
const MAX_CONSOLE_EVENTS: usize = 4_096;
const MAX_CONSOLE_EVIDENCE_BYTES: u64 = 1024 * 1024;
const CONSOLE_EVIDENCE_ENTRY_OVERHEAD_BYTES: u64 = 64;
const MAX_RESOURCE_LEDGER_ROWS: u64 = MAX_PROMOTION_TREE_ENTRIES * 2 + 1;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

const REQUIRED_BASE_FILES: &[&str] = &["console.jsonl", "resources.jsonl", "session-state.jsonl"];

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ArtifactContractViolation {
    reason: String,
}

impl ArtifactContractViolation {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ArtifactContractViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl Error for ArtifactContractViolation {}

pub(crate) type ArtifactContractResult<T = ()> = Result<T, ArtifactContractViolation>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FailedArtifactExpectation<'a> {
    pub(crate) render_id: &'a str,
    pub(crate) code: &'a str,
    pub(crate) message: &'a str,
    pub(crate) public_output: &'a Path,
    pub(crate) locale: &'a str,
    pub(crate) timezone: &'a str,
    pub(crate) page: &'a serde_json::Value,
    pub(crate) resource_policy: &'a serde_json::Value,
    pub(crate) input: CapturedInputExpectation<'a>,
    pub(crate) allow_host_fonts: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CapturedArtifactExpectation<'a> {
    pub(crate) public_artifacts: &'a Path,
    pub(crate) public_output: &'a Path,
    pub(crate) locale: &'a str,
    pub(crate) timezone: &'a str,
    pub(crate) page: &'a serde_json::Value,
    pub(crate) resource_policy: &'a serde_json::Value,
    pub(crate) input: CapturedInputExpectation<'a>,
    pub(crate) allow_host_fonts: bool,
    pub(crate) allow_partial_scene: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CapturedInputExpectation<'a> {
    pub(crate) url: &'a str,
    pub(crate) sha256: &'a str,
    pub(crate) resource: &'a str,
    pub(crate) bytes: u64,
}

/// Validate a captured worker tree before the parent creates a publication plan.
pub(crate) fn validate_captured_artifact_contract(
    root: &Path,
    deferred: &DeferredCapturedPublication,
    expected: CapturedArtifactExpectation<'_>,
) -> ArtifactContractResult {
    require_deferred_counts(deferred)?;
    let tree = inspect_tree(root, TreeKind::Captured)?;

    for name in REQUIRED_BASE_FILES {
        tree.require_root_file(name)?;
    }
    for name in [
        "environment.json",
        "fonts.json",
        "layout-debug.json",
        "pages.json",
        "readiness.json",
        "render.png",
        "scene-report.json",
        "scene.json",
    ] {
        tree.require_root_file(name)?;
    }
    tree.require_root_directory("resources")?;

    if deferred.capture_status != "complete"
        && !(deferred.capture_status == "partial" && expected.allow_partial_scene)
    {
        return Err(invalid(
            "captured scene status is not permitted by the original request",
        ));
    }
    validate_readiness(root, deferred)?;
    let resource_ledger = validate_captured_resource_ledger(root, &tree, deferred)?;
    validate_environment(root, deferred, expected, &resource_ledger)?;
    // layout-debug.json is a data-plane snapshot with no independently trusted semantic receipt.
    // Its contract is one complete JSON value under the supervisor's aggregate tree-byte bound.
    let _: IgnoredAny = serde_json::from_reader(BufReader::new(
        File::open(root.join("layout-debug.json"))
            .map_err(|_| invalid("cannot open layout-debug.json"))?,
    ))
    .map_err(|_| invalid("layout-debug.json is not valid JSON"))?;
    let scene = validate_scene(root, deferred)?;
    let fonts = validate_fonts(root, &tree, expected.allow_host_fonts, &scene)?;
    if !scene.font_instances.is_subset(&fonts.instances) {
        return Err(invalid(
            "scene.json references a font instance absent from fonts.json",
        ));
    }
    validate_render_image(root, deferred)?;
    validate_previews(root, &tree, deferred)?;
    validate_pdf(root, &tree, deferred, &scene)?;
    validate_pages(root, deferred, &scene)?;
    validate_scene_report(root, deferred, expected, &scene, &fonts)?;
    let accounted_resources = scene
        .resources
        .iter()
        .chain(fonts.resources.iter())
        .chain(resource_ledger.resources.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    if accounted_resources != tree.resources.keys().cloned().collect() {
        return Err(invalid(
            "resources directory is not exactly reconciled to scene, font, and ledger evidence",
        ));
    }
    validate_captured_session_state(root)?;
    validate_console_ledger(root)?;
    Ok(())
}

/// Validate a typed worker failure before exposing its diagnostic artifact tree.
pub(crate) fn validate_failed_artifact_contract(
    root: &Path,
    expected: FailedArtifactExpectation<'_>,
) -> ArtifactContractResult {
    let tree = inspect_tree(root, TreeKind::Failed)?;
    for name in REQUIRED_BASE_FILES {
        tree.require_root_file(name)?;
    }
    tree.require_root_directory("resources")?;
    tree.require_root_file("failure.json")?;
    if tree.root_files.contains("render.png") {
        require_png(&root.join("render.png"), "render.png", None)?;
    }

    // These are parent-owned publication controls. Their presence means this is not a pristine
    // worker result, even if failure.json itself is well formed.
    if tree.root_files.contains("bundle.json") || tree.root_directories.contains("publication") {
        return Err(invalid(
            "failed worker tree contains parent-owned publication controls",
        ));
    }

    let failure: FailureArtifact = read_bounded_json(
        &root.join("failure.json"),
        MAX_CONTROL_JSON_BYTES,
        "failure.json",
    )?;
    if failure.status != "failed"
        || failure.render_id != expected.render_id
        || failure.error.code != expected.code
        || failure.error.message != expected.message
    {
        return Err(invalid(
            "failure.json does not match the accepted worker failure",
        ));
    }

    let session_phase = validate_failed_session_state(root, expected)?;
    let console_rows = validate_console_ledger(root)?;
    let resource_ledger = validate_failed_resource_ledger(root, &tree, expected)?;
    if resource_ledger.resources != tree.resources.keys().cloned().collect() {
        return Err(invalid(
            "failure resources directory is not exactly reconciled to loaded ledger rows",
        ));
    }
    if resource_ledger.input.as_ref().is_some_and(|input| {
        input.render_id != expected.render_id
            || input.url != expected.input.url
            || input.sha256 != expected.input.sha256
            || input.resource != expected.input.resource
            || input.bytes != expected.input.bytes
            || input.source != "document_root"
            || !input.main_frame
    }) {
        return Err(invalid(
            "failure resource ledger main-frame input does not match the parent identity",
        ));
    }

    let mut readiness_is_failed = false;
    if tree.root_files.contains("readiness.json") {
        let readiness: serde_json::Value = read_bounded_json(
            &root.join("readiness.json"),
            MAX_CONTROL_JSON_BYTES,
            "readiness.json",
        )?;
        if readiness
            .as_object()
            .and_then(|object| object.get("render_id"))
            .and_then(serde_json::Value::as_str)
            != Some(expected.render_id)
        {
            return Err(invalid(
                "failure readiness evidence does not match the render ID",
            ));
        }
        let snapshot = serde_json::to_string(&readiness)
            .map_err(|_| invalid("cannot normalize failure readiness evidence"))?;
        match parse_snapshot(&snapshot) {
            Ok(Readiness::Failed { error })
                if error.code == expected.code && error.message == expected.message =>
            {
                readiness_is_failed = true;
            },
            Ok(Readiness::Pending | Readiness::Ready { .. }) => {},
            _ => {
                return Err(invalid(
                    "failure readiness evidence is not a permitted readiness snapshot",
                ));
            },
        }
    }
    if session_phase == FailureSessionPhase::BeforeStart
        && tree.root_files.contains("readiness.json")
        && !readiness_is_failed
    {
        return Err(invalid(
            "pre-start failure cannot contain preserved Ready or Pending evidence",
        ));
    }
    if tree.root_files.contains("environment.json") {
        let environment: FailureEnvironmentArtifact = read_bounded_json(
            &root.join("environment.json"),
            MAX_CONTROL_JSON_BYTES,
            "environment.json",
        )?;
        if environment.locale.requested != expected.locale
            || environment.locale.resolved != expected.locale
            || environment.timezone.requested != expected.timezone
            || environment.timezone.resolved != expected.timezone
            || environment.page != *expected.page
            || !resource_policy_matches_expected(
                &environment.resource_policy,
                expected.resource_policy,
            )
            || environment.fonts.host_fonts != expected_host_font_policy(expected.allow_host_fonts)
        {
            return Err(invalid(
                "failure environment does not match the original deterministic request",
            ));
        }
        let pending = environment.document_pdf.status == "pending"
            && environment.document_pdf.error.0.is_none();
        let failed = environment.document_pdf.status == "failed"
            && environment
                .document_pdf
                .error
                .0
                .as_ref()
                .is_some_and(|error| {
                    error.code == expected.code && error.message == expected.message
                });
        if environment.document_pdf.artifact.as_str()
            != expected.public_output.to_string_lossy().as_ref()
            || !(pending || failed)
        {
            return Err(invalid(
                "failure environment does not bind a permitted logical public PDF state",
            ));
        }
        let environment_phase =
            validate_failure_environment_phases(&environment, &resource_ledger, expected)?;
        if session_phase == FailureSessionPhase::BeforeStart
            && (environment_phase != FailureEnvironmentPhase::Base
                || console_rows != 0
                || resource_ledger.has_document_rows)
            || session_phase == FailureSessionPhase::Started
                && (resource_ledger.has_asset_failure
                    || environment_phase == FailureEnvironmentPhase::Base
                        && expected.code != "SESSION_ARTIFACT_WRITE_FAILED")
        {
            return Err(invalid(
                "failure environment phase does not match session-state.jsonl",
            ));
        }
    } else if session_phase != FailureSessionPhase::BeforeStart
        || !resource_ledger.resources.is_empty()
        || resource_ledger.input.is_some()
        || resource_ledger.has_standalone_failure
        || resource_ledger.has_document_rows
        || resource_ledger.has_asset_failure
        || resource_ledger.accounting != EnvironmentResourceAccounting::default()
        || console_rows != 0
    {
        return Err(invalid(
            "failure evidence without environment.json is not a legal pre-start phase",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TreeKind {
    Captured,
    Failed,
}

struct ArtifactTree {
    root_files: BTreeSet<String>,
    root_directories: BTreeSet<String>,
    preview_pages: BTreeSet<String>,
    resources: BTreeMap<String, u64>,
}

impl ArtifactTree {
    fn require_root_file(&self, name: &str) -> ArtifactContractResult {
        if self.root_files.contains(name) {
            Ok(())
        } else {
            Err(invalid(format!("staged artifact tree is missing {name}")))
        }
    }

    fn require_root_directory(&self, name: &str) -> ArtifactContractResult {
        if self.root_directories.contains(name) {
            Ok(())
        } else {
            Err(invalid(format!(
                "staged artifact tree is missing the {name} directory"
            )))
        }
    }
}

fn inspect_tree(root: &Path, kind: TreeKind) -> ArtifactContractResult<ArtifactTree> {
    require_directory(root, "staged artifact root")?;
    let mut entries = 0_u64;
    let mut root_files = BTreeSet::new();
    let mut root_directories = BTreeSet::new();
    let mut preview_pages = BTreeSet::new();
    let mut resources = BTreeMap::new();

    for entry in read_directory(root, "staged artifact root")? {
        let entry = directory_entry(entry, "staged artifact root")?;
        increment_entries(&mut entries)?;
        let name = utf8_name(&entry.path())?;
        let metadata = symlink_metadata(&entry.path(), &name)?;
        if metadata.is_file() {
            if !allowed_root_file(&name, kind) {
                return Err(invalid(format!(
                    "staged artifact tree contains unknown root file {name}"
                )));
            }
            root_files.insert(name);
        } else if metadata.is_dir() {
            let allowed = match kind {
                TreeKind::Captured => matches!(name.as_str(), "pages" | "resources"),
                TreeKind::Failed => name == "resources",
            };
            if !allowed {
                return Err(invalid(format!(
                    "staged artifact tree contains unknown root directory {name}"
                )));
            }
            root_directories.insert(name.clone());
            match name.as_str() {
                "resources" => resources = inspect_resources(&entry.path(), &mut entries)?,
                "pages" => {
                    preview_pages = inspect_preview_pages(&entry.path(), &mut entries)?;
                },
                _ => unreachable!(),
            }
        } else {
            return Err(invalid(format!(
                "staged artifact root entry {name} is not a regular file or directory"
            )));
        }
    }

    Ok(ArtifactTree {
        root_files,
        root_directories,
        preview_pages,
        resources,
    })
}

fn allowed_root_file(name: &str, kind: TreeKind) -> bool {
    match kind {
        TreeKind::Captured => matches!(
            name,
            "console.jsonl"
                | "document.pdf"
                | "environment.json"
                | "fonts.json"
                | "layout-debug.json"
                | "pages.json"
                | "pdf-structure.json"
                | "readiness.json"
                | "render.png"
                | "resources.jsonl"
                | "scene-preview.png"
                | "scene-report.json"
                | "scene.json"
                | "session-state.jsonl"
        ),
        TreeKind::Failed => matches!(
            name,
            "console.jsonl"
                | "environment.json"
                | "failure.json"
                | "readiness.json"
                | "render.png"
                | "resources.jsonl"
                | "session-state.jsonl"
        ),
    }
}

fn inspect_resources(
    directory: &Path,
    entries: &mut u64,
) -> ArtifactContractResult<BTreeMap<String, u64>> {
    let mut resources = BTreeMap::new();
    for entry in read_directory(directory, "resources directory")? {
        let entry = directory_entry(entry, "resources directory")?;
        increment_entries(entries)?;
        let name = utf8_name(&entry.path())?;
        if !is_lower_sha256_digest(&name) {
            return Err(invalid(format!(
                "resources directory contains invalid content address {name}"
            )));
        }
        require_regular_file(&entry.path(), &format!("resource {name}"))?;
        let (sha256, bytes) = streaming_identity(&entry.path(), &format!("resource {name}"))?;
        if sha256.strip_prefix("sha256:") != Some(name.as_str()) {
            return Err(invalid(format!(
                "resource {name} does not match its content hash"
            )));
        }
        resources.insert(name, bytes);
    }
    Ok(resources)
}

fn inspect_preview_pages(
    directory: &Path,
    entries: &mut u64,
) -> ArtifactContractResult<BTreeSet<String>> {
    let mut pages = BTreeSet::new();
    for entry in read_directory(directory, "preview pages directory")? {
        let entry = directory_entry(entry, "preview pages directory")?;
        increment_entries(entries)?;
        let name = utf8_name(&entry.path())?;
        let Some(index) = preview_page_index(&name) else {
            return Err(invalid(format!(
                "preview pages directory contains invalid entry {name}"
            )));
        };
        if index == 0 || u64::try_from(index).unwrap_or(u64::MAX) > MAX_PROMOTION_TREE_ENTRIES {
            return Err(invalid(format!(
                "preview page index is out of range: {name}"
            )));
        }
        require_png(&entry.path(), &format!("preview page {name}"), None)?;
        pages.insert(name);
    }
    Ok(pages)
}

fn validate_readiness(
    root: &Path,
    deferred: &DeferredCapturedPublication,
) -> ArtifactContractResult {
    if deferred.readiness_bytes == 0 || deferred.readiness_bytes > MAX_CONTROL_JSON_BYTES {
        return Err(invalid("deferred readiness size is out of bounds"));
    }
    let path = root.join("readiness.json");
    let bytes = read_bounded_bytes(&path, MAX_CONTROL_JSON_BYTES, "readiness.json")?;
    if bytes.len() as u64 != deferred.readiness_bytes
        || content_address(&bytes) != deferred.readiness_sha256
    {
        return Err(invalid(
            "readiness.json does not match its deferred identity",
        ));
    }
    let readiness: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("readiness.json is not valid JSON"))?;
    let Some(object) = readiness.as_object() else {
        return Err(invalid("readiness.json must be a JSON object"));
    };
    if object.get("render_id").and_then(serde_json::Value::as_str)
        != Some(deferred.render_id.as_str())
    {
        return Err(invalid(
            "readiness.json does not match the deferred render ID",
        ));
    }
    let snapshot =
        std::str::from_utf8(&bytes).map_err(|_| invalid("readiness.json is not valid UTF-8"))?;
    if !matches!(parse_snapshot(snapshot), Ok(Readiness::Ready { .. })) {
        return Err(invalid("readiness.json is not a Ready snapshot"));
    }
    Ok(())
}

fn validate_environment(
    root: &Path,
    deferred: &DeferredCapturedPublication,
    expected: CapturedArtifactExpectation<'_>,
    resource_ledger: &CapturedResourceLedgerBindings,
) -> ArtifactContractResult {
    let environment: EnvironmentArtifact = read_bounded_json(
        &root.join("environment.json"),
        MAX_CONTROL_JSON_BYTES,
        "environment.json",
    )?;
    if environment.locale.requested != expected.locale
        || environment.locale.resolved != expected.locale
        || environment.timezone.requested != expected.timezone
        || environment.timezone.resolved != expected.timezone
        || environment.page != *expected.page
        || !resource_policy_matches_expected(&environment.resource_policy, expected.resource_policy)
    {
        return Err(invalid(
            "environment.json does not match the original deterministic request",
        ));
    }
    if environment.document_pdf.artifact.as_str()
        != expected.public_output.to_string_lossy().as_ref()
        || environment.document_pdf.status != "pending"
        || environment.document_pdf.error.0.is_some()
    {
        return Err(invalid(
            "environment.json does not bind the pending logical public PDF path",
        ));
    }
    if environment.fonts.host_fonts != expected_host_font_policy(expected.allow_host_fonts) {
        return Err(invalid(
            "environment.json host-font policy does not match the original request",
        ));
    }
    if environment.runtime.adapter != "document-session" {
        return Err(invalid(
            "environment.json does not identify the document-session runtime",
        ));
    }
    if environment.phase_timings_ms.controlled_runtime != deferred.controlled_runtime_ms
        || environment.phase_timings_ms.scene_capture != deferred.scene_capture_ms
    {
        return Err(invalid(
            "environment.json phase timings do not match the deferred receipt",
        ));
    }
    if environment.resolved_input_hash != deferred.resolved_input_hash {
        return Err(invalid(
            "environment.json does not match the deferred input identity",
        ));
    }
    if environment.resource_accounting != resource_ledger.accounting {
        return Err(invalid(
            "environment.json resource accounting does not match resources.jsonl",
        ));
    }
    if environment.input_resource != resource_ledger.input
        || environment.input_resource.render_id != deferred.render_id
        || environment.input_resource.url != expected.input.url
        || environment.input_resource.sha256 != expected.input.sha256
        || environment.input_resource.resource != expected.input.resource
        || environment.input_resource.bytes != expected.input.bytes
        || environment.input_resource.source != "document_root"
        || !environment.input_resource.main_frame
    {
        return Err(invalid(
            "environment.json input resource does not match parent and ledger evidence",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FailureEnvironmentPhase {
    Base,
    Runtime,
    Input,
    Resolved,
}

fn validate_failure_environment_phases(
    environment: &FailureEnvironmentArtifact,
    resource_ledger: &FailedResourceLedgerBindings,
    expected: FailedArtifactExpectation<'_>,
) -> ArtifactContractResult<FailureEnvironmentPhase> {
    let evidence_group = match (
        environment.runtime.as_ref(),
        environment.resource_accounting.as_ref(),
        environment.phase_timings_ms.as_ref(),
    ) {
        (None, None, None) => false,
        (Some(runtime), Some(accounting), Some(timings)) => {
            if !resource_ledger.complete
                || runtime.adapter != "document-session"
                || accounting != &resource_ledger.accounting
                || !valid_optional_timing(timings.controlled_runtime.0)
                || !valid_optional_timing(timings.scene_capture.0)
            {
                return Err(invalid(
                    "failure environment runtime evidence does not match resources.jsonl",
                ));
            }
            true
        },
        _ => {
            return Err(invalid(
                "failure environment contains a partial runtime evidence phase",
            ));
        },
    };

    if let Some(input) = environment.input_resource.as_ref() {
        if !evidence_group
            || resource_ledger.input.as_ref() != Some(input)
            || input.render_id != expected.render_id
            || input.url != expected.input.url
            || input.sha256 != expected.input.sha256
            || input.resource != expected.input.resource
            || input.bytes != expected.input.bytes
            || input.source != "document_root"
            || !input.main_frame
        {
            return Err(invalid(
                "failure environment input resource does not match parent and ledger evidence",
            ));
        }
    }
    if let Some(resolved_input_hash) = environment.resolved_input_hash.as_deref() {
        if environment.input_resource.is_none()
            || resolved_input_hash != resource_ledger.resolved_input_hash
            || !resolved_input_hash
                .strip_prefix("sha256:")
                .is_some_and(is_lower_sha256_digest)
        {
            return Err(invalid(
                "failure environment resolved input hash is not phase-bound to resources.jsonl",
            ));
        }
    }
    Ok(if environment.resolved_input_hash.is_some() {
        FailureEnvironmentPhase::Resolved
    } else if environment.input_resource.is_some() {
        FailureEnvironmentPhase::Input
    } else if evidence_group {
        FailureEnvironmentPhase::Runtime
    } else {
        FailureEnvironmentPhase::Base
    })
}

fn valid_optional_timing(value: Option<f64>) -> bool {
    value.is_none_or(|value| value.is_finite() && value >= 0.0 && !value.is_sign_negative())
}

fn resource_policy_matches_expected(
    actual: &serde_json::Value,
    expected: &serde_json::Value,
) -> bool {
    let mut actual = actual.clone();
    let mut expected = expected.clone();
    normalize_asset_cache_runtime(&mut actual).is_ok()
        && normalize_asset_cache_runtime(&mut expected).is_ok()
        && actual == expected
}

fn normalize_asset_cache_runtime(policy: &mut serde_json::Value) -> ArtifactContractResult {
    let Some(policy) = policy.as_object_mut() else {
        return Err(invalid("resource policy evidence is not an object"));
    };
    let Some(manifest) = policy.get_mut("asset_manifest") else {
        return Ok(());
    };
    let Some(manifest) = manifest.as_object_mut() else {
        return Err(invalid("asset manifest evidence is not an object"));
    };

    let Some(assets) = manifest
        .get_mut("assets")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Err(invalid("asset manifest evidence has no asset array"));
    };
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut invalidations = 0u64;
    for asset in assets {
        let Some(asset) = asset.as_object_mut() else {
            return Err(invalid("asset manifest entry is not an object"));
        };
        match asset
            .remove("cache_result")
            .and_then(|value| value.as_str().map(str::to_owned))
            .as_deref()
        {
            Some("hit") => hits = hits.saturating_add(1),
            Some("miss") => misses = misses.saturating_add(1),
            Some("invalidated") => invalidations = invalidations.saturating_add(1),
            _ => return Err(invalid("asset manifest entry has an invalid cache result")),
        }
    }

    let Some(cache) = manifest
        .get_mut("cache")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Err(invalid("asset manifest evidence has no cache object"));
    };
    let recorded_hits = cache.remove("hits").and_then(|value| value.as_u64());
    let recorded_misses = cache.remove("misses").and_then(|value| value.as_u64());
    let recorded_invalidations = cache
        .remove("invalidations")
        .and_then(|value| value.as_u64());
    let evictions = cache.remove("evictions").and_then(|value| value.as_u64());
    if recorded_hits != Some(hits)
        || recorded_misses != Some(misses)
        || recorded_invalidations != Some(invalidations)
        || evictions.is_none_or(|evictions| evictions > misses.saturating_add(invalidations))
    {
        return Err(invalid(
            "asset manifest cache accounting does not match its asset entries",
        ));
    }
    Ok(())
}

struct SceneBindings {
    resources: BTreeSet<String>,
    font_instances: BTreeSet<String>,
    pages: Vec<ScenePageBinding>,
    text_operations: Vec<(usize, usize, String)>,
}

struct ScenePageBinding {
    size: ArtifactSize,
    operation_counts: OperationCounts,
    expected_extracted_unicode: String,
    embedded_font_ids: Vec<String>,
}

fn validate_scene(
    root: &Path,
    deferred: &DeferredCapturedPublication,
) -> ArtifactContractResult<SceneBindings> {
    let path = root.join("scene.json");
    let (sha256, bytes) = streaming_identity(&path, "scene.json")?;
    if bytes == 0 || sha256 != deferred.scene_hash {
        return Err(invalid("scene.json does not match its deferred hash"));
    }
    let scene: pliego::DocumentScene = serde_json::from_reader(BufReader::new(
        File::open(path).map_err(|_| invalid("cannot open scene.json"))?,
    ))
    .map_err(|_| invalid("scene.json does not match the document scene schema"))?;
    scene
        .validate()
        .map_err(|_| invalid("scene.json is not a valid DocumentScene"))?;
    // The producer's original f64 can serialize to a shortest decimal that parses
    // back to an adjacent f64 and therefore reserializes with a different final
    // digit. The deferred receipt already binds the exact scene bytes; validate the
    // parsed schema and every public semantic field below without imposing an
    // invalid deserialize/reserialize byte-idempotence requirement.
    if scene.schema != deferred.scene_schema
        || scene.version != deferred.scene_version
        || scene.pages.len() != deferred.page_count
    {
        return Err(invalid(
            "scene.json schema, version, or page count does not match the deferred receipt",
        ));
    }
    let mut resources = BTreeSet::new();
    let mut font_instances = BTreeSet::new();
    let mut pages = Vec::with_capacity(scene.pages.len());
    let mut text_operations = Vec::new();
    for (page_index, page) in scene.pages.iter().enumerate() {
        let mut operation_counts = OperationCounts::default();
        let mut expected_extracted_unicode = String::new();
        let mut embedded_font_ids = Vec::new();
        for (operation_index, operation) in page.operations.iter().enumerate() {
            match operation {
                pliego::Operation::Image { resource, .. } => {
                    operation_counts.image += 1;
                    let digest = resource
                        .strip_prefix("sha256:")
                        .filter(|digest| is_lower_sha256_digest(digest))
                        .ok_or_else(|| {
                            invalid("scene.json contains an invalid image content address")
                        })?;
                    resources.insert(digest.to_owned());
                },
                pliego::Operation::Text {
                    text, font, glyphs, ..
                } => {
                    operation_counts.text += 1;
                    font_instances.insert(font.clone());
                    text_operations.push((page_index, operation_index, font.clone()));
                    if !glyphs.is_empty() {
                        expected_extracted_unicode.push_str(text);
                        if !embedded_font_ids.contains(font) {
                            embedded_font_ids.push(font.clone());
                        }
                    }
                },
                pliego::Operation::Path { .. } => operation_counts.vector += 1,
                pliego::Operation::Link { .. } => operation_counts.link += 1,
            }
        }
        pages.push(ScenePageBinding {
            size: ArtifactSize {
                width: page.size.width,
                height: page.size.height,
            },
            operation_counts,
            expected_extracted_unicode,
            embedded_font_ids,
        });
    }
    Ok(SceneBindings {
        resources,
        font_instances,
        pages,
        text_operations,
    })
}

fn validate_render_image(
    root: &Path,
    deferred: &DeferredCapturedPublication,
) -> ArtifactContractResult {
    if deferred.rendered_bytes == 0 {
        return Err(invalid("deferred render image size is zero"));
    }
    require_png(
        &root.join("render.png"),
        "render.png",
        Some(deferred.rendered_bytes),
    )
}

fn validate_previews(
    root: &Path,
    tree: &ArtifactTree,
    deferred: &DeferredCapturedPublication,
) -> ArtifactContractResult {
    match deferred.preview_status.as_str() {
        "rendered" if deferred.preview_count == deferred.page_count => {},
        "unsupported" if deferred.preview_count == 0 => {},
        _ => {
            return Err(invalid(
                "deferred preview status and counts are inconsistent",
            ));
        },
    }

    match deferred.preview_count {
        0 => {
            if tree.root_files.contains("scene-preview.png")
                || tree.root_directories.contains("pages")
            {
                return Err(invalid(
                    "preview artifacts exist for a deferred zero-preview result",
                ));
            }
        },
        1 => {
            if tree.root_directories.contains("pages") {
                return Err(invalid(
                    "single-page preview must not use the pages directory",
                ));
            }
            tree.require_root_file("scene-preview.png")?;
            require_png(&root.join("scene-preview.png"), "scene-preview.png", None)?;
        },
        count => {
            if tree.root_files.contains("scene-preview.png") {
                return Err(invalid("multi-page preview must not use scene-preview.png"));
            }
            tree.require_root_directory("pages")?;
            let expected = (1..=count)
                .map(|index| format!("page-{index:04}.png"))
                .collect::<BTreeSet<_>>();
            if tree.preview_pages != expected {
                return Err(invalid(
                    "preview page files do not match the deferred preview count",
                ));
            }
        },
    }
    Ok(())
}

fn validate_pdf(
    root: &Path,
    tree: &ArtifactTree,
    deferred: &DeferredCapturedPublication,
    scene: &SceneBindings,
) -> ArtifactContractResult {
    if deferred.pdf_status != "rendered" || deferred.pdf_structure_status != "rendered" {
        return Err(invalid(
            "captured worker result must contain a rendered PDF and structure",
        ));
    }
    tree.require_root_file("document.pdf")?;
    tree.require_root_file("pdf-structure.json")?;
    let pdf_path = root.join("document.pdf");
    require_pdf(&pdf_path)?;
    let (pdf_sha256, pdf_bytes) = streaming_identity(&pdf_path, "document.pdf")?;
    let structure: PdfStructureArtifact = serde_json::from_reader(BufReader::new(
        File::open(root.join("pdf-structure.json"))
            .map_err(|_| invalid("cannot open pdf-structure.json"))?,
    ))
    .map_err(|_| invalid("pdf-structure.json is not a valid structure artifact"))?;
    if structure.schema != "pliego.pdf-structure"
        || structure.version != 1
        || structure.backend != "krilla"
        || structure.pdf.artifact != "document.pdf"
        || structure.pdf.sha256 != pdf_sha256
        || structure.pdf.bytes != pdf_bytes
        || structure.page_count != deferred.page_count
        || structure.pages.0.len() != deferred.page_count
    {
        return Err(invalid(
            "pdf-structure.json does not match document.pdf and the deferred receipt",
        ));
    }
    for (index, (page, expected_page)) in structure.pages.0.iter().zip(&scene.pages).enumerate() {
        let expected_media_box = [
            0.0,
            0.0,
            expected_page.size.width * pliego::pdf::CSS_PX_TO_PDF_PT,
            expected_page.size.height * pliego::pdf::CSS_PX_TO_PDF_PT,
        ];
        if page.index != index
            || page.scene_page_size_css_px != expected_page.size
            || page.media_box_pt != expected_media_box
            || page.expected_extracted_unicode != expected_page.expected_extracted_unicode
            || page.embedded_font_ids.0 != expected_page.embedded_font_ids
            || page.operation_counts != expected_page.operation_counts
        {
            return Err(invalid(
                "pdf-structure.json page evidence does not match scene.json",
            ));
        }
    }
    Ok(())
}

fn validate_pages(
    root: &Path,
    deferred: &DeferredCapturedPublication,
    scene: &SceneBindings,
) -> ArtifactContractResult {
    let pages: PagesArtifact = serde_json::from_reader(BufReader::new(
        File::open(root.join("pages.json")).map_err(|_| invalid("cannot open pages.json"))?,
    ))
    .map_err(|_| invalid("pages.json does not match the pages artifact schema"))?;
    if pages.schema != "pliego.pages"
        || pages.version != 1
        || pages.page_count != deferred.page_count
    {
        return Err(invalid(
            "pages.json schema or page count does not match the deferred receipt",
        ));
    }
    validate_page_bindings(&pages.pages.0, deferred, scene, "pages.json")
}

fn validate_scene_report(
    root: &Path,
    deferred: &DeferredCapturedPublication,
    expected: CapturedArtifactExpectation<'_>,
    scene: &SceneBindings,
    fonts: &FontBindings,
) -> ArtifactContractResult {
    let report: SceneReportArtifact = serde_json::from_reader(BufReader::new(
        File::open(root.join("scene-report.json"))
            .map_err(|_| invalid("cannot open scene-report.json"))?,
    ))
    .map_err(|_| invalid("scene-report.json does not match the report schema"))?;
    validate_page_bindings(
        &report.preview.pages.0,
        deferred,
        scene,
        "scene-report.json",
    )?;
    let expected_unsupported = expected_preview_unsupported(scene, fonts);
    let expected_preview_status = if expected_unsupported.is_empty() {
        "rendered"
    } else {
        "unsupported"
    };
    let expected_preview_count = if expected_unsupported.is_empty() {
        scene.pages.len()
    } else {
        0
    };
    let expected_capture = if !report.capture.text_mapping_gaps.0.is_empty() {
        ("partial", Some("SCENE_CAPTURE_LIMITATIONS"))
    } else if !report.capture.unsupported_events.0.is_empty() {
        ("partial", Some("SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS"))
    } else {
        ("complete", None)
    };

    let expected_preview = (deferred.preview_count == 1).then_some("scene-preview.png");
    let public_pdf = expected
        .public_artifacts
        .join("document.pdf")
        .to_string_lossy()
        .into_owned();
    let public_structure = expected
        .public_artifacts
        .join("pdf-structure.json")
        .to_string_lossy()
        .into_owned();
    if report.scene.schema != deferred.scene_schema
        || report.scene.version != deferred.scene_version
        || report.scene.hash != deferred.scene_hash
        || report.scene.validation != "valid"
        || report.capture.status != deferred.capture_status
        || report.capture.code.0 != deferred.capture_code
        || report.capture.status != expected_capture.0
        || report.capture.code.0.as_deref() != expected_capture.1
        || report.capture.unsupported_events.0.len() != deferred.unsupported_event_count
        || report.capture.text_mapping_gaps.0.len() != deferred.text_mapping_gap_count
        || report.preview.page_size != scene.pages[0].size
        || report.preview.operation_counts != scene.pages[0].operation_counts
        || report.preview.status != deferred.preview_status
        || report.preview.status != expected_preview_status
        || report.preview.artifact.0.as_deref() != expected_preview
        || report.preview.page_count != deferred.preview_count
        || report.preview.page_count != expected_preview_count
        || report.preview.unsupported.0 != expected_unsupported
        || report.document_pdf.status != deferred.pdf_status
        || report.document_pdf.artifact != public_pdf
        || report.document_pdf.error.0.is_some()
        || report.pdf_structure.status != deferred.pdf_structure_status
        || report.pdf_structure.artifact != public_structure
        || report.pdf_structure.error.0.is_some()
    {
        return Err(invalid(
            "scene-report.json does not match the deferred receipt and logical public paths",
        ));
    }
    if !report
        .capture
        .unsupported_events
        .0
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence)
        || !report.capture.text_mapping_gaps.0.windows(2).all(|pair| {
            (pair[0].sequence, pair[0].glyph_index) < (pair[1].sequence, pair[1].glyph_index)
        })
    {
        return Err(invalid(
            "scene-report.json capture diagnostics are not in canonical producer order",
        ));
    }
    validate_canvas_diagnostics(&report.capture.canvases.0, scene)?;
    Ok(())
}

fn validate_page_bindings(
    pages: &[PageBinding],
    deferred: &DeferredCapturedPublication,
    scene: &SceneBindings,
    label: &str,
) -> ArtifactContractResult {
    if pages.len() != deferred.page_count {
        return Err(invalid(format!(
            "{label} page rows do not match the deferred page count"
        )));
    }
    for (index, page) in pages.iter().enumerate() {
        let expected_artifact = if deferred.preview_count == 0 {
            None
        } else if deferred.preview_count == 1 {
            Some("scene-preview.png".to_owned())
        } else {
            Some(format!("pages/page-{:04}.png", index + 1))
        };
        if page.index != index
            || page.artifact.0 != expected_artifact
            || page.page_size != scene.pages[index].size
            || page.operation_counts != scene.pages[index].operation_counts
        {
            return Err(invalid(format!(
                "{label} preview bindings do not match the deferred page order"
            )));
        }
    }
    Ok(())
}

fn expected_preview_unsupported(
    scene: &SceneBindings,
    fonts: &FontBindings,
) -> Vec<PreviewUnsupportedArtifact> {
    let mut expected = Vec::new();
    // A varied instance is the only producer-supported reason a successful capture can omit its
    // preview: missing image bytes fail PDF production and cannot reach this contract.
    for (page_index, operation_index, font) in &scene.text_operations {
        if fonts.varied_instances.contains(font) {
            expected.push(PreviewUnsupportedArtifact {
                code: "SCENE_CAPTURE_PREVIEW_UNSUPPORTED_FONT_VARIATIONS".to_owned(),
                page_index: *page_index,
                operation_index: *operation_index,
                kind: "text".to_owned(),
                font: Some(font.clone()),
            });
        }
    }
    expected
}

fn validate_canvas_diagnostics(
    canvases: &[CanvasCaptureArtifact],
    scene: &SceneBindings,
) -> ArtifactContractResult {
    let scene_resources = scene
        .resources
        .iter()
        .map(|digest| format!("sha256:{digest}"))
        .collect::<BTreeSet<_>>();
    let total_vectors = scene
        .pages
        .iter()
        .try_fold(0usize, |total, page| {
            total.checked_add(page.operation_counts.vector)
        })
        .ok_or_else(|| invalid("scene vector operation count overflow"))?;
    let mut emitted_vectors = 0usize;
    let mut sequences = BTreeSet::new();
    for canvas in canvases {
        if canvas.sequences.0.is_empty()
            || canvas.diagnostics.schema != "pliego.hybrid-canvas-diagnostics"
            || canvas.diagnostics.version != 1
        {
            return Err(invalid(
                "scene-report.json contains invalid Canvas diagnostics identity",
            ));
        }
        for sequence in &canvas.sequences.0 {
            if !sequences.insert(*sequence) {
                return Err(invalid(
                    "scene-report.json contains duplicate Canvas placement sequences",
                ));
            }
        }
        let placements = canvas.sequences.0.len();
        emitted_vectors = emitted_vectors
            .checked_add(
                canvas
                    .diagnostics
                    .vector_operation_count
                    .checked_mul(placements)
                    .ok_or_else(|| invalid("Canvas vector operation count overflow"))?,
            )
            .ok_or_else(|| invalid("Canvas vector operation count overflow"))?;
        if !canvas
            .diagnostics
            .fallbacks
            .0
            .windows(2)
            .all(|pair| pair[0].command_index < pair[1].command_index)
        {
            return Err(invalid(
                "scene-report.json Canvas fallbacks are not in producer order",
            ));
        }
        let mut rasterized_area_px = 0u64;
        for fallback in &canvas.diagnostics.fallbacks.0 {
            let width = exact_nonnegative_integer(fallback.bounds.width)
                .ok_or_else(|| invalid("Canvas fallback has an invalid width"))?;
            let height = exact_nonnegative_integer(fallback.bounds.height)
                .ok_or_else(|| invalid("Canvas fallback has an invalid height"))?;
            let area = width
                .checked_mul(height)
                .ok_or_else(|| invalid("Canvas fallback area overflow"))?;
            if !finite_nonnegative_rect(&fallback.bounds)
                || area == 0
                || fallback.area_px != area
                || !scene_resources.contains(&fallback.resource)
            {
                return Err(invalid(
                    "scene-report.json contains invalid Canvas fallback evidence",
                ));
            }
            rasterized_area_px = rasterized_area_px
                .checked_add(area)
                .ok_or_else(|| invalid("Canvas rasterized area overflow"))?;
        }
        if rasterized_area_px != canvas.diagnostics.rasterized_area_px {
            return Err(invalid(
                "scene-report.json Canvas rasterized area is inconsistent",
            ));
        }
    }
    if emitted_vectors > total_vectors {
        return Err(invalid(
            "scene-report.json Canvas vectors exceed the canonical scene",
        ));
    }
    Ok(())
}

fn exact_nonnegative_integer(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= u64::MAX as f64)
        .then(|| value as u64)
}

fn finite_nonnegative_rect(rect: &ArtifactRect) -> bool {
    [rect.x, rect.y, rect.width, rect.height]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0 && !value.is_sign_negative())
}

struct FontBindings {
    resources: BTreeSet<String>,
    instances: BTreeSet<String>,
    varied_instances: BTreeSet<String>,
}

fn validate_fonts(
    root: &Path,
    tree: &ArtifactTree,
    allow_host_fonts: bool,
    scene: &SceneBindings,
) -> ArtifactContractResult<FontBindings> {
    let fonts: FontsArtifact = serde_json::from_reader(BufReader::new(
        File::open(root.join("fonts.json")).map_err(|_| invalid("cannot open fonts.json"))?,
    ))
    .map_err(|_| invalid("fonts.json does not match the font report schema"))?;
    if fonts.schema != "pliego.font-report"
        || fonts.version != 1
        || fonts.manifest.resolution != "css-order"
    {
        return Err(invalid("fonts.json has an unknown schema or resolution"));
    }
    if fonts.policy.host_fonts != expected_host_font_policy(allow_host_fonts) {
        return Err(invalid(
            "fonts.json host-font policy does not match the original request",
        ));
    }

    let mut persisted = BTreeSet::new();
    let mut resource_digests = BTreeSet::new();
    let mut previous_resource = None;
    for resource in fonts.font_resources.0 {
        let digest = resource
            .resource
            .strip_prefix("sha256:")
            .filter(|digest| is_lower_sha256_digest(digest))
            .ok_or_else(|| invalid("fonts.json contains an invalid font content address"))?
            .to_owned();
        let decoded = BASE64_STANDARD
            .decode(&resource.bytes_base64)
            .map_err(|_| invalid("fonts.json contains invalid base64 font bytes"))?;
        if content_address(&decoded) != resource.resource
            || BASE64_STANDARD.encode(&decoded) != resource.bytes_base64
            || tree.resources.get(&digest).copied() != Some(decoded.len() as u64)
            || previous_resource
                .as_ref()
                .is_some_and(|previous| previous >= &resource.resource)
            || !persisted.insert(resource.resource)
        {
            return Err(invalid(
                "fonts.json contains a missing, duplicate, or misbound font resource",
            ));
        }
        previous_resource = Some(format!("sha256:{digest}"));
        resource_digests.insert(digest);
    }
    let mut instances = BTreeMap::new();
    let mut varied_instances = BTreeSet::new();
    let mut previous_instance = None;
    for instance in fonts.font_instances.0 {
        let varied = !instance.variations.0.is_empty();
        let variations_are_canonical = instance.variations.0.iter().all(|variation| {
            variation.value.is_finite()
                && !(variation.value == 0.0 && variation.value.is_sign_negative())
        }) && instance.variations.0.windows(2).all(|pair| {
            (pair[0].tag, pair[0].value.to_bits()) <= (pair[1].tag, pair[1].value.to_bits())
        });
        if !persisted.contains(&instance.resource)
            || !variations_are_canonical
            || font_instance_id(
                &instance.resource,
                instance.face_index,
                &instance.variations.0,
                instance.synthetic_bold,
            )? != instance.id
            || previous_instance
                .as_ref()
                .is_some_and(|previous| previous >= &instance.id)
            || instances.contains_key(&instance.id)
        {
            return Err(invalid(
                "fonts.json contains a missing-resource or duplicate font instance",
            ));
        }
        if varied {
            varied_instances.insert(instance.id.clone());
        }
        previous_instance = Some(instance.id.clone());
        instances.insert(instance.id, (instance.resource, instance.face_index));
    }
    let selections = fonts.selections.0;
    if !selections.windows(2).all(|pair| {
        (
            &pair[0].instance,
            pair[0].source,
            &pair[0].requested_families.0,
            &pair[0].selected_family,
        ) < (
            &pair[1].instance,
            pair[1].source,
            &pair[1].requested_families.0,
            &pair[1].selected_family,
        )
    }) {
        return Err(invalid(
            "fonts.json selections are not in canonical producer order",
        ));
    }
    for selection in &selections {
        if selection.source == FontSelectionSource::Host && !allow_host_fonts {
            return Err(invalid(
                "fonts.json contains a host selection forbidden by the original request",
            ));
        }
        if !persisted.contains(&selection.resource)
            || !scene.font_instances.contains(&selection.instance)
            || instances.get(&selection.instance)
                != Some(&(selection.resource.clone(), selection.face_index))
        {
            return Err(invalid(
                "fonts.json selection does not match a persisted font instance",
            ));
        }
    }
    let expected_manifest = selections
        .iter()
        .filter(|selection| {
            matches!(
                selection.source,
                FontSelectionSource::Bundled
                    | FontSelectionSource::Data
                    | FontSelectionSource::Memory
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if fonts.manifest.entries.0 != expected_manifest {
        return Err(invalid(
            "fonts.json manifest does not match the producer selection subset",
        ));
    }
    let mut expected_warnings = BTreeMap::new();
    for selection in &selections {
        if let (Some(requested), Some(selected)) = (
            selection.requested_families.0.first(),
            selection.selected_family.as_ref(),
        ) && !requested.eq_ignore_ascii_case(selected)
        {
            let key = (
                selection.instance.clone(),
                requested.clone(),
                selected.clone(),
                selection.requested_families.0.clone(),
            );
            expected_warnings
                .entry(key)
                .or_insert_with(|| FontWarningArtifact {
                    code: "FONT_FALLBACK_USED".to_owned(),
                    instance: selection.instance.clone(),
                    requested_family: requested.clone(),
                    selected_family: selected.clone(),
                    fallback_chain: selection.requested_families.clone(),
                });
        }
    }
    if fonts.warnings.0 != expected_warnings.into_values().collect::<Vec<_>>() {
        return Err(invalid(
            "fonts.json warnings do not match the producer selection fallbacks",
        ));
    }
    Ok(FontBindings {
        resources: resource_digests,
        instances: instances.into_keys().collect(),
        varied_instances,
    })
}

fn expected_host_font_policy(allow_host_fonts: bool) -> &'static str {
    if allow_host_fonts {
        "allowed"
    } else {
        "denied"
    }
}

fn font_instance_id(
    resource: &str,
    face_index: u32,
    variations: &[FontVariationArtifact],
    synthetic_bold: bool,
) -> ArtifactContractResult<String> {
    let digest = resource
        .strip_prefix("sha256:")
        .and_then(decode_lower_sha256)
        .ok_or_else(|| invalid("font instance resource is not a canonical content address"))?;
    let mut hasher = Sha256::new();
    hasher.update(if synthetic_bold {
        b"pliego-font-instance-v2\0".as_slice()
    } else {
        b"pliego-font-instance-v1\0".as_slice()
    });
    hasher.update(digest);
    hasher.update(face_index.to_be_bytes());
    hasher.update((variations.len() as u64).to_be_bytes());
    for variation in variations {
        hasher.update(variation.tag.to_be_bytes());
        hasher.update(variation.value.to_bits().to_be_bytes());
    }
    if synthetic_bold {
        hasher.update([1]);
    }
    Ok(format!("sha256:{}", lower_hex(&hasher.finalize())))
}

fn decode_lower_sha256(value: &str) -> Option<[u8; 32]> {
    if !is_lower_sha256_digest(value) {
        return None;
    }
    let mut decoded = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        decoded[index] = (lower_hex_nibble(pair[0])? << 4) | lower_hex_nibble(pair[1])?;
    }
    Some(decoded)
}

fn lower_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn validate_captured_resource_ledger(
    root: &Path,
    tree: &ArtifactTree,
    deferred: &DeferredCapturedPublication,
) -> ArtifactContractResult<CapturedResourceLedgerBindings> {
    let rows = read_jsonl_with_limit::<serde_json::Value>(
        &root.join("resources.jsonl"),
        "resources.jsonl",
        true,
        MAX_RESOURCE_LEDGER_ROWS,
    )?;
    if rows.is_empty() || rows.len() % 2 != 0 {
        return Err(invalid(
            "captured resources.jsonl must contain request/terminal row pairs",
        ));
    }

    let mut resources = BTreeSet::new();
    let mut accounting = EnvironmentResourceAccounting::default();
    let mut input = None;
    let mut url_to_resource = BTreeMap::new();
    for (index, pair) in rows.chunks_exact(2).enumerate() {
        let requested: CapturedResourceRequestRow = serde_json::from_value(pair[0].clone())
            .map_err(|_| invalid("resources.jsonl has an invalid requested row"))?;
        let terminal: CapturedResourceTerminalRow = serde_json::from_value(pair[1].clone())
            .map_err(|_| invalid("resources.jsonl has an invalid terminal row"))?;
        validate_document_session_resource_pair(
            tree,
            &deferred.render_id,
            index,
            &requested,
            &terminal,
            &mut resources,
            &mut accounting,
            &mut input,
            &mut url_to_resource,
        )?;
    }

    if compute_resolved_input_hash(&deferred.render_id, &url_to_resource)
        != deferred.resolved_input_hash
    {
        return Err(invalid(
            "deferred resolved input hash does not match resources.jsonl",
        ));
    }
    Ok(CapturedResourceLedgerBindings {
        resources,
        accounting,
        input: input.ok_or_else(|| invalid("resources.jsonl has no main-frame input binding"))?,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_document_session_resource_pair(
    tree: &ArtifactTree,
    render_id: &str,
    index: usize,
    requested: &CapturedResourceRequestRow,
    terminal: &CapturedResourceTerminalRow,
    resources: &mut BTreeSet<String>,
    accounting: &mut EnvironmentResourceAccounting,
    input: &mut Option<EnvironmentInputResource>,
    url_to_resource: &mut BTreeMap<String, String>,
) -> ArtifactContractResult {
    let expected_request_id = format!("document-session:{index:06}");
    if requested.render_id != render_id
        || terminal.render_id != render_id
        || requested.policy != "pliego.resource-policy.v1"
        || terminal.policy != "pliego.resource-policy.v1"
        || requested.request_id != expected_request_id
        || terminal.request_id != expected_request_id
        || requested.url != terminal.url
        || !is_canonical_url(&terminal.url)
        || terminal
            .referrer_url
            .0
            .as_deref()
            .is_some_and(|url| !is_canonical_url(url))
        || http::Method::from_bytes(terminal.method.as_bytes()).is_err()
        || requested.status != "requested"
        || requested.bytes.0.is_some()
        || terminal.urls.0.as_slice() != [terminal.url.as_str()]
    {
        return Err(invalid(
            "resources.jsonl request and terminal rows are not producer-bound",
        ));
    }

    accounting.requests = accounting
        .requests
        .checked_add(1)
        .ok_or_else(|| invalid("resource request count overflow"))?;
    if terminal.bytes.0.is_none() {
        accounting.unavailable_bodies = accounting
            .unavailable_bodies
            .checked_add(1)
            .ok_or_else(|| invalid("unavailable resource count overflow"))?;
    }
    if let Some(bytes) = terminal.bytes.0 {
        accounting.body_bytes = accounting
            .body_bytes
            .checked_add(bytes)
            .ok_or_else(|| invalid("resource byte accounting overflow"))?;
    }

    match terminal.status {
        CapturedResourceStatus::Loaded => {
            accounting.loaded = accounting
                .loaded
                .checked_add(1)
                .ok_or_else(|| invalid("loaded resource count overflow"))?;
            validate_captured_loaded_resource(tree, terminal, resources)?;
            if terminal.method != "HEAD" {
                let resource = terminal
                    .resource
                    .0
                    .as_ref()
                    .expect("loaded resource shape was validated");
                if let Some(previous) =
                    url_to_resource.insert(terminal.url.clone(), resource.clone())
                    && previous != *resource
                {
                    return Err(invalid(
                        "resources.jsonl maps one URL to conflicting content addresses",
                    ));
                }
            }
        },
        CapturedResourceStatus::Delegated => {
            return Err(invalid(
                "document-session resources.jsonl cannot contain delegated evidence",
            ));
        },
        CapturedResourceStatus::Cancelled => {
            accounting.failed = accounting
                .failed
                .checked_add(1)
                .ok_or_else(|| invalid("failed resource count overflow"))?;
            let Some(failure) = terminal.failure.0.as_ref() else {
                return Err(invalid(
                    "resources.jsonl cancelled row has no failure evidence",
                ));
            };
            if terminal.source.0.is_some()
                || terminal.fatal
                || !terminal.cancelled
                || terminal.load_role != CapturedResourceLoadRole::DocumentMetadata
                || terminal.is_for_main_frame
                || terminal.code.0.as_deref() != Some(failure.code.as_str())
                || failure.code.is_empty()
                || failure.fatal
                || failure.reason.is_empty()
                || terminal.response_status.0.is_some()
                || terminal.content_type.0.is_some()
                || terminal.bytes.0.is_some()
                || terminal.sha256.0.is_some()
                || terminal.resource.0.is_some()
                || terminal.content_hash.0.is_some()
                || terminal.response_headers.0.is_some()
                || terminal.cache_result.0.is_some()
                || terminal.artifact.0.is_some()
            {
                return Err(invalid(
                    "resources.jsonl cancelled row has an invalid producer shape",
                ));
            }
        },
    }

    if terminal.is_for_main_frame {
        if input.is_some()
            || terminal.status != CapturedResourceStatus::Loaded
            || terminal.method != "GET"
            || terminal.destination != CapturedResourceDestination::Document
            || terminal.load_role != CapturedResourceLoadRole::DocumentContent
            || terminal.referrer_url.0.is_some()
            || terminal.is_redirect
            || terminal.source.0 != Some(CapturedResourceSource::DocumentRoot)
            || terminal.response_status.0 != Some(200)
        {
            return Err(invalid(
                "resources.jsonl main-frame input row is not the producer input binding",
            ));
        }
        *input = Some(EnvironmentInputResource {
            render_id: terminal.render_id.clone(),
            url: terminal.url.clone(),
            sha256: terminal
                .sha256
                .0
                .clone()
                .ok_or_else(|| invalid("main-frame input row has no SHA-256"))?,
            resource: terminal
                .resource
                .0
                .clone()
                .ok_or_else(|| invalid("main-frame input row has no content address"))?,
            bytes: terminal
                .bytes
                .0
                .ok_or_else(|| invalid("main-frame input row has no byte count"))?,
            source: "document_root".to_owned(),
            main_frame: true,
        });
    }
    Ok(())
}

fn compute_resolved_input_hash(render_id: &str, resources: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    update_length_prefixed_hash(&mut hasher, b"pliego.resolved-input.v1");
    update_length_prefixed_hash(&mut hasher, render_id.as_bytes());
    for (url, resource) in resources {
        if url.starts_with("http://") || url.starts_with("https://") {
            update_length_prefixed_hash(&mut hasher, url.as_bytes());
            update_length_prefixed_hash(&mut hasher, resource.as_bytes());
        }
    }
    format!("sha256:{}", lower_hex(&hasher.finalize()))
}

fn update_length_prefixed_hash(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn is_canonical_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| url.as_str() == value)
}

fn validate_captured_loaded_resource(
    tree: &ArtifactTree,
    row: &CapturedResourceTerminalRow,
    resources: &mut BTreeSet<String>,
) -> ArtifactContractResult {
    let digest = row
        .sha256
        .0
        .as_deref()
        .filter(|digest| is_lower_sha256_digest(digest))
        .ok_or_else(|| invalid("resources.jsonl loaded row has an invalid SHA-256"))?;
    let content_address = format!("sha256:{digest}");
    let artifact = format!("resources/{digest}");
    let headers = row
        .response_headers
        .0
        .as_ref()
        .ok_or_else(|| invalid("resources.jsonl loaded row has no response headers"))?;
    let header_name_bytes = headers.names.0.iter().try_fold(0u64, |bytes, name| {
        bytes
            .checked_add(name.len() as u64)
            .ok_or_else(|| invalid("response header name byte count overflow"))
    })?;
    let empty_headers_sha256 = lower_hex(&Sha256::digest(b"pliego.response-headers.v1\0"));
    if row.source.0.is_none()
        || !matches!(row.method.as_str(), "GET" | "HEAD")
        || row.is_redirect
        || row.fatal
        || row.cancelled
        || row.code.0.is_some()
        || row.failure.0.is_some()
        || row.response_status.0.is_none()
        || row
            .response_status
            .0
            .is_some_and(|status| http::StatusCode::from_u16(status).is_err())
        || row.bytes.0.is_none()
        || row.resource.0.as_deref() != Some(content_address.as_str())
        || row.content_hash.0.as_deref() != Some(content_address.as_str())
        || row.artifact.0.as_deref() != Some(artifact.as_str())
        || tree.resources.get(digest).copied() != row.bytes.0
        || headers.count < u64::try_from(headers.names.0.len()).unwrap_or(u64::MAX)
        || headers.count > MAX_RESPONSE_HEADER_COUNT as u64
        || headers.bytes > MAX_RESPONSE_HEADER_BYTES
        || headers.bytes < header_name_bytes
        || !is_lower_sha256_digest(&headers.sha256)
        || (headers.count == 0
            && (headers.bytes != 0
                || !headers.names.0.is_empty()
                || headers.sha256 != empty_headers_sha256))
        || !headers.names.0.windows(2).all(|pair| pair[0] < pair[1])
        || headers.names.0.iter().any(|name| {
            name != &name.to_ascii_lowercase()
                || http::header::HeaderName::from_bytes(name.as_bytes()).is_err()
        })
        || match row.source.0 {
            Some(CapturedResourceSource::AssetCache) => !matches!(
                row.cache_result.0.as_deref(),
                Some("hit" | "miss" | "invalidated")
            ),
            Some(_) => row.cache_result.0.is_some(),
            None => true,
        }
        || match row.source.0 {
            Some(CapturedResourceSource::DataUrl) => !row.url.starts_with("data:"),
            Some(CapturedResourceSource::DocumentRoot) => !row.url.starts_with("file:"),
            Some(CapturedResourceSource::Http) => {
                !row.url.starts_with("http:") && !row.url.starts_with("https:")
            },
            Some(CapturedResourceSource::AssetCache | CapturedResourceSource::VirtualResource) => {
                false
            },
            None => true,
        }
        || (row.method == "HEAD" && row.bytes.0 != Some(0))
    {
        return Err(invalid(
            "resources.jsonl loaded row does not match its content-addressed producer evidence",
        ));
    }
    resources.insert(digest.to_owned());
    Ok(())
}

fn validate_failed_resource_ledger(
    root: &Path,
    tree: &ArtifactTree,
    expected: FailedArtifactExpectation<'_>,
) -> ArtifactContractResult<FailedResourceLedgerBindings> {
    let rows = read_jsonl_with_limit::<serde_json::Value>(
        &root.join("resources.jsonl"),
        "resources.jsonl",
        true,
        MAX_RESOURCE_LEDGER_ROWS,
    )?;
    if rows.len() == 1
        && rows[0].get("policy").and_then(serde_json::Value::as_str)
            == Some("pliego.asset-cache.v1")
    {
        let row: AssetFailureResourceRow = serde_json::from_value(rows[0].clone())
            .map_err(|_| invalid("resources.jsonl has an invalid asset failure row"))?;
        validate_asset_failure_resource_row(&row, expected)?;
        return Ok(FailedResourceLedgerBindings {
            resolved_input_hash: compute_resolved_input_hash(expected.render_id, &BTreeMap::new()),
            has_asset_failure: true,
            ..FailedResourceLedgerBindings::default()
        });
    }

    let mut bindings = FailedResourceLedgerBindings::default();
    let mut url_to_resource = BTreeMap::new();
    let mut cursor = 0usize;
    let mut pair_index = 0usize;
    while cursor < rows.len() {
        bindings.has_document_rows = true;
        if rows[cursor]
            .get("request_id")
            .is_some_and(serde_json::Value::is_null)
        {
            if cursor + 1 != rows.len() || bindings.has_standalone_failure {
                return Err(invalid(
                    "resources.jsonl has a non-terminal standalone failure row",
                ));
            }
            let row: StandaloneResourceFailureRow = serde_json::from_value(rows[cursor].clone())
                .map_err(|_| {
                    invalid("resources.jsonl has an invalid standalone resource failure row")
                })?;
            validate_standalone_resource_failure(&row, expected)?;
            bindings.accounting.requests = bindings
                .accounting
                .requests
                .checked_add(1)
                .ok_or_else(|| invalid("resource request count overflow"))?;
            bindings.accounting.failed = bindings
                .accounting
                .failed
                .checked_add(1)
                .ok_or_else(|| invalid("failed resource count overflow"))?;
            bindings.accounting.unavailable_bodies = bindings
                .accounting
                .unavailable_bodies
                .checked_add(1)
                .ok_or_else(|| invalid("unavailable resource count overflow"))?;
            bindings.has_standalone_failure = true;
            cursor += 1;
            continue;
        }

        let requested: CapturedResourceRequestRow = serde_json::from_value(rows[cursor].clone())
            .map_err(|_| invalid("resources.jsonl has an invalid requested row"))?;
        if cursor + 1 == rows.len() {
            let expected_request_id = format!("document-session:{pair_index:06}");
            if expected.code != "SESSION_ARTIFACT_WRITE_FAILED"
                || requested.render_id != expected.render_id
                || requested.policy != "pliego.resource-policy.v1"
                || requested.request_id != expected_request_id
                || !is_canonical_url(&requested.url)
                || requested.status != "requested"
                || requested.bytes.0.is_some()
            {
                return Err(invalid(
                    "resources.jsonl has an unpermitted incomplete request row",
                ));
            }
            bindings.complete = false;
            cursor += 1;
            continue;
        }
        let terminal: CapturedResourceTerminalRow =
            serde_json::from_value(rows[cursor + 1].clone())
                .map_err(|_| invalid("resources.jsonl has an invalid terminal row"))?;
        validate_document_session_resource_pair(
            tree,
            expected.render_id,
            pair_index,
            &requested,
            &terminal,
            &mut bindings.resources,
            &mut bindings.accounting,
            &mut bindings.input,
            &mut url_to_resource,
        )?;
        cursor += 2;
        pair_index += 1;
    }
    bindings.resolved_input_hash =
        compute_resolved_input_hash(expected.render_id, &url_to_resource);
    Ok(bindings)
}

fn validate_asset_failure_resource_row(
    row: &AssetFailureResourceRow,
    expected: FailedArtifactExpectation<'_>,
) -> ArtifactContractResult {
    let asset = expected
        .resource_policy
        .get("asset_manifest")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid("asset failure row has no parent-bound asset manifest"))?;
    let error = asset
        .get("error")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid("asset failure row has no parent-bound asset error"))?;
    if row.render_id != expected.render_id
        || row.policy != "pliego.asset-cache.v1"
        || row.request_id.0.is_some()
        || row.status != "failed"
        || row.code != expected.code
        || row.reason != expected.message
        || row.cache_result.0.is_some()
        || row.bytes.0.is_some()
        || row
            .url
            .0
            .as_deref()
            .is_some_and(|url| !is_canonical_url(url))
        || asset.get("schema").and_then(serde_json::Value::as_str) != Some("pliego.asset-manifest")
        || asset.get("version").and_then(serde_json::Value::as_u64) != Some(1)
        || asset.get("status").and_then(serde_json::Value::as_str) != Some("failed")
        || asset.get("manifest").and_then(serde_json::Value::as_str) != Some(row.manifest.as_str())
        || error.get("code").and_then(serde_json::Value::as_str) != Some(row.code.as_str())
        || error.get("message").and_then(serde_json::Value::as_str) != Some(row.reason.as_str())
        || error.get("url") != Some(&option_string_value(&row.url.0))
        || error.get("expected") != Some(&option_string_value(&row.expected.0))
        || error.get("actual") != Some(&option_string_value(&row.actual.0))
    {
        return Err(invalid(
            "resources.jsonl asset failure does not match parent-bound resource policy evidence",
        ));
    }
    Ok(())
}

fn option_string_value(value: &Option<String>) -> serde_json::Value {
    value
        .as_ref()
        .map_or(serde_json::Value::Null, |value| value.clone().into())
}

fn validate_standalone_resource_failure(
    row: &StandaloneResourceFailureRow,
    expected: FailedArtifactExpectation<'_>,
) -> ArtifactContractResult {
    let expected_message = format!("{}: {}", row.reason, row.url);
    let cancelled =
        row.cancelled && !row.fatal && row.load_role == CapturedResourceLoadRole::DocumentMetadata;
    let fatal = !row.cancelled && row.fatal;
    if row.render_id != expected.render_id
        || row.policy != "pliego.resource-policy.v1"
        || row.request_id.0.is_some()
        || row.code != expected.code
        || expected_message != expected.message
        || !is_canonical_url(&row.url)
        || row
            .referrer_url
            .0
            .as_deref()
            .is_some_and(|url| !is_canonical_url(url))
        || http::Method::from_bytes(row.method.as_bytes()).is_err()
        || row.reason.is_empty()
        || !(cancelled || fatal)
        || row.bytes.0.is_some()
    {
        return Err(invalid(
            "resources.jsonl standalone failure does not match the accepted worker failure",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FailureSessionPhase {
    BeforeStart,
    Started,
}

fn validate_failed_session_state(
    root: &Path,
    expected: FailedArtifactExpectation<'_>,
) -> ArtifactContractResult<FailureSessionPhase> {
    let rows = read_jsonl::<SessionStateRow>(
        &root.join("session-state.jsonl"),
        "session-state.jsonl",
        true,
    )?;
    let Some(terminal) = rows.last() else {
        return Err(invalid("session-state.jsonl has no terminal failure row"));
    };
    let phase = match rows.as_slice() {
        [_failed] => Some(FailureSessionPhase::BeforeStart),
        [started, _failed] if started.state == "started" && started.message.0.is_none() => {
            Some(FailureSessionPhase::Started)
        },
        _ => None,
    };
    if phase.is_none()
        || terminal.state != "failed"
        || terminal.message.0.as_deref() != Some(expected.message)
    {
        return Err(invalid(
            "session-state.jsonl is not an exact producer failure sequence",
        ));
    }
    Ok(phase.expect("failure session phase was validated"))
}

fn validate_captured_session_state(root: &Path) -> ArtifactContractResult {
    let rows = read_jsonl::<SessionStateRow>(
        &root.join("session-state.jsonl"),
        "session-state.jsonl",
        true,
    )?;
    if rows.len() != 1 || rows[0].state != "started" || rows[0].message.0.is_some() {
        return Err(invalid(
            "captured session-state.jsonl must contain exactly the pre-publication started state",
        ));
    }
    Ok(())
}

fn validate_console_ledger(root: &Path) -> ArtifactContractResult<usize> {
    let rows = read_jsonl::<ConsoleRow>(&root.join("console.jsonl"), "console.jsonl", true)?;
    let bytes = rows.iter().try_fold(0u64, |bytes, row| {
        bytes
            .checked_add(row.level.serialized_len())
            .and_then(|bytes| bytes.checked_add(row.message.len() as u64))
            .and_then(|bytes| bytes.checked_add(CONSOLE_EVIDENCE_ENTRY_OVERHEAD_BYTES))
            .ok_or_else(|| invalid("console evidence byte count overflow"))
    })?;
    if rows.len() > MAX_CONSOLE_EVENTS || bytes > MAX_CONSOLE_EVIDENCE_BYTES {
        return Err(invalid("console.jsonl exceeds the producer evidence bound"));
    }
    Ok(rows.len())
}

fn require_deferred_counts(deferred: &DeferredCapturedPublication) -> ArtifactContractResult {
    if deferred.page_count == 0
        || u64::try_from(deferred.page_count).unwrap_or(u64::MAX) > MAX_PROMOTION_TREE_ENTRIES
        || u64::try_from(deferred.preview_count).unwrap_or(u64::MAX) > MAX_PROMOTION_TREE_ENTRIES
        || deferred.preview_count > deferred.page_count
    {
        return Err(invalid("deferred artifact counts are out of bounds"));
    }
    Ok(())
}

fn read_jsonl<T>(
    path: &Path,
    label: &str,
    require_canonical_rows: bool,
) -> ArtifactContractResult<Vec<T>>
where
    T: DeserializeOwned,
{
    read_jsonl_with_limit(
        path,
        label,
        require_canonical_rows,
        MAX_PROMOTION_TREE_ENTRIES,
    )
}

fn read_jsonl_with_limit<T>(
    path: &Path,
    label: &str,
    require_canonical_rows: bool,
    max_rows: u64,
) -> ArtifactContractResult<Vec<T>>
where
    T: DeserializeOwned,
{
    require_regular_file(path, label)?;
    let file = File::open(path).map_err(|_| invalid(format!("cannot open {label}")))?;
    let mut reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|_| invalid(format!("cannot read {label}")))?;
        if read == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            return Err(invalid(format!("{label} does not end with a line feed")));
        }
        line.pop();
        if line.is_empty() || line.last() == Some(&b'\r') {
            return Err(invalid(format!("{label} contains invalid line framing")));
        }
        if u64::try_from(rows.len()).unwrap_or(u64::MAX) >= max_rows {
            return Err(invalid(format!("{label} exceeds the artifact entry limit")));
        }
        if require_canonical_rows {
            let value: serde_json::Value = serde_json::from_slice(&line)
                .map_err(|_| invalid(format!("{label} contains invalid JSON framing")))?;
            if serde_json::to_vec(&value)
                .map_err(|_| invalid(format!("cannot normalize {label}")))?
                != line
            {
                return Err(invalid(format!("{label} contains a non-canonical row")));
            }
        }
        let row: T = serde_json::from_slice(&line)
            .map_err(|_| invalid(format!("{label} contains invalid JSON framing")))?;
        rows.push(row);
    }
    Ok(rows)
}

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn read_directory(path: &Path, label: &str) -> ArtifactContractResult<fs::ReadDir> {
    fs::read_dir(path).map_err(|_| invalid(format!("cannot read {label}")))
}

fn directory_entry(
    entry: std::io::Result<fs::DirEntry>,
    label: &str,
) -> ArtifactContractResult<fs::DirEntry> {
    entry.map_err(|_| invalid(format!("cannot enumerate {label}")))
}

fn increment_entries(entries: &mut u64) -> ArtifactContractResult {
    *entries = entries
        .checked_add(1)
        .ok_or_else(|| invalid("staged artifact entry count overflow"))?;
    if *entries > MAX_PROMOTION_TREE_ENTRIES {
        return Err(invalid(format!(
            "staged artifact tree exceeds the {MAX_PROMOTION_TREE_ENTRIES}-entry limit"
        )));
    }
    Ok(())
}

fn utf8_name(path: &Path) -> ArtifactContractResult<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid("staged artifact name is not valid UTF-8"))
}

fn symlink_metadata(path: &Path, label: &str) -> ArtifactContractResult<fs::Metadata> {
    fs::symlink_metadata(path).map_err(|_| invalid(format!("cannot inspect {label}")))
}

fn require_directory(path: &Path, label: &str) -> ArtifactContractResult {
    let metadata = symlink_metadata(path, label)?;
    if metadata.is_dir() {
        Ok(())
    } else {
        Err(invalid(format!("{label} is not a directory")))
    }
}

fn require_regular_file(path: &Path, label: &str) -> ArtifactContractResult<fs::Metadata> {
    let metadata = symlink_metadata(path, label)?;
    if metadata.is_file() {
        Ok(metadata)
    } else {
        Err(invalid(format!("{label} is not a regular file")))
    }
}

fn require_png(path: &Path, label: &str, expected_bytes: Option<u64>) -> ArtifactContractResult {
    let metadata = require_regular_file(path, label)?;
    if expected_bytes.is_some_and(|bytes| bytes != metadata.len()) {
        return Err(invalid(format!("{label} size does not match its receipt")));
    }
    let mut signature = [0_u8; 8];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut signature))
        .map_err(|_| invalid(format!("cannot read {label} PNG signature")))?;
    if &signature != PNG_SIGNATURE {
        return Err(invalid(format!("{label} has an invalid PNG signature")));
    }
    Ok(())
}

fn require_pdf(path: &Path) -> ArtifactContractResult {
    require_regular_file(path, "document.pdf")?;
    let mut signature = [0_u8; 5];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut signature))
        .map_err(|_| invalid("cannot read document.pdf signature"))?;
    if &signature != b"%PDF-" {
        return Err(invalid("document.pdf has an invalid PDF signature"));
    }
    Ok(())
}

fn read_bounded_bytes(path: &Path, maximum: u64, label: &str) -> ArtifactContractResult<Vec<u8>> {
    let metadata = require_regular_file(path, label)?;
    if metadata.len() > maximum {
        return Err(invalid(format!("{label} exceeds the {maximum}-byte limit")));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .map_err(|_| invalid(format!("cannot open {label}")))?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid(format!("cannot read {label}")))?;
    if bytes.len() as u64 > maximum || bytes.len() as u64 != metadata.len() {
        return Err(invalid(format!("{label} changed while it was read")));
    }
    Ok(bytes)
}

fn read_bounded_json<T>(path: &Path, maximum: u64, label: &str) -> ArtifactContractResult<T>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = read_bounded_bytes(path, maximum, label)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid(format!("{label} is not valid JSON")))
}

fn streaming_identity(path: &Path, label: &str) -> ArtifactContractResult<(String, u64)> {
    require_regular_file(path, label)?;
    let mut file = File::open(path).map_err(|_| invalid(format!("cannot open {label}")))?;
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| invalid(format!("cannot hash {label}")))?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| invalid(format!("{label} byte count overflow")))?;
        digest.update(&buffer[..read]);
    }
    Ok((format!("sha256:{}", lower_hex(&digest.finalize())), bytes))
}

fn content_address(bytes: &[u8]) -> String {
    format!("sha256:{}", lower_hex(&Sha256::digest(bytes)))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn is_lower_sha256_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn preview_page_index(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("page-")?.strip_suffix(".png")?;
    if digits.len() < 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index = digits.parse::<usize>().ok()?;
    (format!("page-{index:04}.png") == name).then_some(index)
}

fn invalid(reason: impl Into<String>) -> ArtifactContractViolation {
    ArtifactContractViolation::new(reason)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentArtifact {
    locale: EnvironmentRequestedResolved,
    timezone: EnvironmentRequestedResolved,
    page: serde_json::Value,
    resource_policy: serde_json::Value,
    fonts: HostFontPolicy,
    document_pdf: EnvironmentDocumentPdf,
    input_resource: EnvironmentInputResource,
    runtime: EnvironmentRuntime,
    resource_accounting: EnvironmentResourceAccounting,
    phase_timings_ms: EnvironmentPhaseTimings,
    resolved_input_hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureEnvironmentArtifact {
    locale: EnvironmentRequestedResolved,
    timezone: EnvironmentRequestedResolved,
    page: serde_json::Value,
    resource_policy: serde_json::Value,
    fonts: HostFontPolicy,
    document_pdf: EnvironmentDocumentPdf,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    runtime: Option<EnvironmentRuntime>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    resource_accounting: Option<EnvironmentResourceAccounting>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    phase_timings_ms: Option<FailureEnvironmentPhaseTimings>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    input_resource: Option<EnvironmentInputResource>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    resolved_input_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentRequestedResolved {
    requested: String,
    resolved: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentDocumentPdf {
    artifact: String,
    status: String,
    error: RequiredNullable<FailureDetail>,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct EnvironmentInputResource {
    render_id: String,
    url: String,
    sha256: String,
    resource: String,
    bytes: u64,
    source: String,
    main_frame: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentRuntime {
    adapter: String,
}

#[derive(Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct EnvironmentResourceAccounting {
    requests: u64,
    loaded: u64,
    delegated: u64,
    failed: u64,
    body_bytes: u64,
    unavailable_bodies: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentPhaseTimings {
    controlled_runtime: f64,
    scene_capture: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureEnvironmentPhaseTimings {
    controlled_runtime: RequiredNullable<f64>,
    scene_capture: RequiredNullable<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ArtifactSize {
    width: f64,
    height: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct OperationCounts {
    text: usize,
    vector: usize,
    image: usize,
    link: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PagesArtifact {
    schema: String,
    version: u32,
    page_count: usize,
    pages: BoundedPageBindings,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PageBinding {
    index: usize,
    artifact: RequiredNullable<String>,
    page_size: ArtifactSize,
    operation_counts: OperationCounts,
}

struct BoundedPageBindings(Vec<PageBinding>);

impl<'de> Deserialize<'de> for BoundedPageBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedPageBindingsVisitor)
    }
}

struct BoundedPageBindingsVisitor;

impl<'de> Visitor<'de> for BoundedPageBindingsVisitor {
    type Value = BoundedPageBindings;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded array of page artifact bindings")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut pages = Vec::new();
        while let Some(page) = sequence.next_element::<PageBinding>()? {
            if u64::try_from(pages.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom(
                    "page artifact array exceeds the artifact entry limit",
                ));
            }
            pages.push(page);
        }
        Ok(BoundedPageBindings(pages))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneReportArtifact {
    scene: SceneReportScene,
    capture: SceneReportCapture,
    preview: SceneReportPreview,
    document_pdf: SceneReportPublication,
    pdf_structure: SceneReportPublication,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneReportScene {
    schema: String,
    version: u32,
    hash: String,
    validation: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneReportCapture {
    status: String,
    code: RequiredNullable<String>,
    unsupported_events: BoundedUnsupportedPaintEvents,
    text_mapping_gaps: BoundedTextMappingGaps,
    canvases: BoundedCanvasCaptures,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneReportPreview {
    status: String,
    artifact: RequiredNullable<String>,
    page_count: usize,
    pages: BoundedPageBindings,
    page_size: ArtifactSize,
    operation_counts: OperationCounts,
    unsupported: BoundedPreviewUnsupported,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneReportPublication {
    status: String,
    artifact: String,
    error: RequiredNullable<FailureDetail>,
}

type BoundedUnsupportedPaintEvents = BoundedVec<UnsupportedPaintEventArtifact>;
type BoundedTextMappingGaps = BoundedVec<TextMappingGapArtifact>;
type BoundedCanvasCaptures = BoundedVec<CanvasCaptureArtifact>;
type BoundedCanvasFallbacks = BoundedVec<CanvasFallbackArtifact>;
type BoundedPreviewUnsupported = BoundedVec<PreviewUnsupportedArtifact>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsupportedPaintEventArtifact {
    sequence: usize,
    #[serde(rename = "kind")]
    _kind: UnsupportedPaintKindArtifact,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum UnsupportedPaintKindArtifact {
    Box,
    RootBackground,
    Outline,
    CollapsedTableBorders,
    Iframe,
    TextEffects,
    ContentGeometry,
    SvgAnimation,
    SvgCompositing,
    SvgStroke,
    SvgPaint,
    SvgImage,
    SvgText,
    SvgInvalidPath,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextMappingGapArtifact {
    sequence: usize,
    glyph_index: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanvasCaptureArtifact {
    sequences: BoundedVec<usize>,
    diagnostics: CanvasDiagnosticsArtifact,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanvasDiagnosticsArtifact {
    schema: String,
    version: u32,
    vector_operation_count: usize,
    rasterized_area_px: u64,
    fallbacks: BoundedCanvasFallbacks,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanvasFallbackArtifact {
    command_index: usize,
    #[serde(rename = "reason")]
    _reason: CanvasFallbackReasonArtifact,
    bounds: ArtifactRect,
    area_px: u64,
    resource: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CanvasFallbackReasonArtifact {
    PixelReadback,
    Filter,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PreviewUnsupportedArtifact {
    code: String,
    page_index: usize,
    operation_index: usize,
    kind: String,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    font: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontsArtifact {
    schema: String,
    version: u32,
    policy: HostFontPolicy,
    manifest: FontManifest,
    font_resources: BoundedFontResources,
    font_instances: BoundedFontInstances,
    selections: BoundedFontSelections,
    warnings: BoundedFontWarnings,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostFontPolicy {
    host_fonts: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontManifest {
    resolution: String,
    entries: BoundedFontSelections,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontResourceArtifact {
    resource: String,
    bytes_base64: String,
}

struct BoundedFontResources(Vec<FontResourceArtifact>);

impl<'de> Deserialize<'de> for BoundedFontResources {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedFontResourcesVisitor)
    }
}

struct BoundedFontResourcesVisitor;

impl<'de> Visitor<'de> for BoundedFontResourcesVisitor {
    type Value = BoundedFontResources;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded font resource array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut resources = Vec::new();
        while let Some(resource) = sequence.next_element::<FontResourceArtifact>()? {
            if u64::try_from(resources.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom(
                    "font resource array exceeds the artifact entry limit",
                ));
            }
            resources.push(resource);
        }
        Ok(BoundedFontResources(resources))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontInstanceReference {
    id: String,
    resource: String,
    face_index: u32,
    variations: BoundedFontVariations,
    synthetic_bold: bool,
}

struct BoundedFontInstances(Vec<FontInstanceReference>);

impl<'de> Deserialize<'de> for BoundedFontInstances {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedFontInstancesVisitor)
    }
}

struct BoundedFontInstancesVisitor;

impl<'de> Visitor<'de> for BoundedFontInstancesVisitor {
    type Value = BoundedFontInstances;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded array of font instance references")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut instances = Vec::new();
        while let Some(instance) = sequence.next_element::<FontInstanceReference>()? {
            if u64::try_from(instances.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom(
                    "font instance array exceeds the artifact entry limit",
                ));
            }
            instances.push(instance);
        }
        Ok(BoundedFontInstances(instances))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct FontSelectionReference {
    instance: String,
    resource: String,
    face_index: u32,
    source: FontSelectionSource,
    requested_families: BoundedStrings,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    selected_family: Option<String>,
}

struct BoundedFontSelections(Vec<FontSelectionReference>);

impl<'de> Deserialize<'de> for BoundedFontSelections {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedFontSelectionsVisitor)
    }
}

struct BoundedFontSelectionsVisitor;

impl<'de> Visitor<'de> for BoundedFontSelectionsVisitor {
    type Value = BoundedFontSelections;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded array of font selections")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut selections = Vec::new();
        while let Some(selection) = sequence.next_element::<FontSelectionReference>()? {
            if u64::try_from(selections.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom(
                    "font selection array exceeds the artifact entry limit",
                ));
            }
            selections.push(selection);
        }
        Ok(BoundedFontSelections(selections))
    }
}

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
enum FontSelectionSource {
    Bundled,
    Data,
    Host,
    Memory,
    Remote,
    Unknown,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontVariationArtifact {
    tag: u32,
    value: f32,
}

struct BoundedFontVariations(Vec<FontVariationArtifact>);

impl<'de> Deserialize<'de> for BoundedFontVariations {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedFontVariationsVisitor)
    }
}

struct BoundedFontVariationsVisitor;

impl<'de> Visitor<'de> for BoundedFontVariationsVisitor {
    type Value = BoundedFontVariations;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded font variation array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut variations = Vec::new();
        while let Some(variation) = sequence.next_element::<FontVariationArtifact>()? {
            if u64::try_from(variations.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom(
                    "font variation array exceeds the artifact entry limit",
                ));
            }
            variations.push(variation);
        }
        Ok(BoundedFontVariations(variations))
    }
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct FontWarningArtifact {
    code: String,
    instance: String,
    requested_family: String,
    selected_family: String,
    fallback_chain: BoundedStrings,
}

struct BoundedFontWarnings(Vec<FontWarningArtifact>);

impl<'de> Deserialize<'de> for BoundedFontWarnings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedFontWarningsVisitor)
    }
}

struct BoundedFontWarningsVisitor;

impl<'de> Visitor<'de> for BoundedFontWarningsVisitor {
    type Value = BoundedFontWarnings;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded font warning array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut warnings = Vec::new();
        while let Some(warning) = sequence.next_element::<FontWarningArtifact>()? {
            if u64::try_from(warnings.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom(
                    "font warning array exceeds the artifact entry limit",
                ));
            }
            warnings.push(warning);
        }
        Ok(BoundedFontWarnings(warnings))
    }
}

struct CapturedResourceLedgerBindings {
    resources: BTreeSet<String>,
    accounting: EnvironmentResourceAccounting,
    input: EnvironmentInputResource,
}

struct FailedResourceLedgerBindings {
    resources: BTreeSet<String>,
    accounting: EnvironmentResourceAccounting,
    input: Option<EnvironmentInputResource>,
    resolved_input_hash: String,
    has_standalone_failure: bool,
    has_document_rows: bool,
    has_asset_failure: bool,
    complete: bool,
}

impl Default for FailedResourceLedgerBindings {
    fn default() -> Self {
        Self {
            resources: BTreeSet::new(),
            accounting: EnvironmentResourceAccounting::default(),
            input: None,
            resolved_input_hash: String::new(),
            has_standalone_failure: false,
            has_document_rows: false,
            has_asset_failure: false,
            complete: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedResourceRequestRow {
    #[serde(rename = "timestamp_ms")]
    _timestamp_ms: u128,
    render_id: String,
    policy: String,
    request_id: String,
    url: String,
    status: String,
    bytes: RequiredNullable<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedResourceTerminalRow {
    #[serde(rename = "timestamp_ms")]
    _timestamp_ms: u128,
    render_id: String,
    policy: String,
    request_id: String,
    url: String,
    urls: BoundedStrings,
    status: CapturedResourceStatus,
    code: RequiredNullable<String>,
    method: String,
    destination: CapturedResourceDestination,
    load_role: CapturedResourceLoadRole,
    fatal: bool,
    cancelled: bool,
    referrer_url: RequiredNullable<String>,
    is_for_main_frame: bool,
    is_redirect: bool,
    source: RequiredNullable<CapturedResourceSource>,
    response_status: RequiredNullable<u16>,
    content_type: RequiredNullable<String>,
    bytes: RequiredNullable<u64>,
    sha256: RequiredNullable<String>,
    resource: RequiredNullable<String>,
    content_hash: RequiredNullable<String>,
    response_headers: RequiredNullable<CapturedResponseHeaders>,
    cache_result: RequiredNullable<String>,
    artifact: RequiredNullable<String>,
    failure: RequiredNullable<CapturedResourceFailure>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum CapturedResourceStatus {
    Loaded,
    Delegated,
    Cancelled,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
enum CapturedResourceLoadRole {
    DocumentContent,
    DocumentMetadata,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
enum CapturedResourceDestination {
    None,
    Audio,
    AudioWorklet,
    Document,
    Embed,
    Font,
    Frame,
    IFrame,
    Image,
    Json,
    Manifest,
    Object,
    PaintWorklet,
    Report,
    Script,
    ServiceWorker,
    SharedWorker,
    Style,
    Track,
    Video,
    WebIdentity,
    Worker,
    Xslt,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CapturedResourceSource {
    AssetCache,
    DataUrl,
    DocumentRoot,
    Http,
    VirtualResource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedResponseHeaders {
    count: u64,
    bytes: u64,
    names: BoundedStrings,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedResourceFailure {
    code: String,
    #[serde(rename = "status")]
    _status: ResourceFailureStatus,
    fatal: bool,
    reason: String,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ResourceFailureStatus {
    Changed,
    Denied,
    NotFound,
    Timeout,
    Unsupported,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StandaloneResourceFailureRow {
    #[serde(rename = "timestamp_ms")]
    _timestamp_ms: u128,
    render_id: String,
    policy: String,
    request_id: RequiredNullable<String>,
    url: String,
    #[serde(rename = "status")]
    _status: ResourceFailureStatus,
    code: String,
    method: String,
    #[serde(rename = "destination")]
    _destination: CapturedResourceDestination,
    load_role: CapturedResourceLoadRole,
    fatal: bool,
    cancelled: bool,
    referrer_url: RequiredNullable<String>,
    #[serde(rename = "is_for_main_frame")]
    _is_for_main_frame: bool,
    #[serde(rename = "is_redirect")]
    _is_redirect: bool,
    reason: String,
    bytes: RequiredNullable<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetFailureResourceRow {
    #[serde(rename = "timestamp_ms")]
    _timestamp_ms: u128,
    render_id: String,
    policy: String,
    request_id: RequiredNullable<String>,
    url: RequiredNullable<String>,
    status: String,
    code: String,
    manifest: String,
    reason: String,
    expected: RequiredNullable<String>,
    actual: RequiredNullable<String>,
    cache_result: RequiredNullable<String>,
    bytes: RequiredNullable<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(transparent)]
struct RequiredNullable<T>(Option<T>);

#[derive(Clone, Eq, PartialEq)]
struct BoundedStrings(Vec<String>);

impl<'de> Deserialize<'de> for BoundedStrings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedStringsVisitor)
    }
}

struct BoundedStringsVisitor;

impl<'de> Visitor<'de> for BoundedStringsVisitor {
    type Value = BoundedStrings;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded string array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<String>()? {
            if u64::try_from(values.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom(
                    "string array exceeds the artifact entry limit",
                ));
            }
            values.push(value);
        }
        Ok(BoundedStrings(values))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionStateRow {
    #[serde(rename = "timestamp_ms")]
    _timestamp_ms: u128,
    state: String,
    message: RequiredNullable<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsoleRow {
    #[serde(rename = "timestamp_ms")]
    _timestamp_ms: u128,
    level: ConsoleLevel,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ConsoleLevel {
    Log,
    Debug,
    Info,
    Warn,
    Error,
    Trace,
    Dir,
}

impl ConsoleLevel {
    fn serialized_len(&self) -> u64 {
        match self {
            Self::Log | Self::Dir => 3,
            Self::Info | Self::Warn => 4,
            Self::Debug | Self::Error | Self::Trace => 5,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureArtifact {
    status: String,
    render_id: String,
    error: FailureDetail,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureDetail {
    code: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PdfStructureArtifact {
    schema: String,
    version: u32,
    backend: String,
    pdf: PdfIdentity,
    page_count: usize,
    pages: BoundedVec<PdfStructurePage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PdfIdentity {
    artifact: String,
    sha256: String,
    bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PdfStructurePage {
    index: usize,
    scene_page_size_css_px: ArtifactSize,
    media_box_pt: [f64; 4],
    expected_extracted_unicode: String,
    embedded_font_ids: BoundedStrings,
    operation_counts: OperationCounts,
}

struct BoundedVec<T>(Vec<T>);

impl<'de, T> Deserialize<'de> for BoundedVec<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedVecVisitor(PhantomData))
    }
}

struct BoundedVecVisitor<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for BoundedVecVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = BoundedVec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an artifact-entry-bounded array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<T>()? {
            if u64::try_from(values.len()).unwrap_or(u64::MAX) >= MAX_PROMOTION_TREE_ENTRIES {
                return Err(de::Error::custom("array exceeds the artifact entry limit"));
            }
            values.push(value);
        }
        Ok(BoundedVec(values))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn producer_float_encoding_need_not_be_reserialization_idempotent() {
        let bytes = br#"{"schema":"pliego.document-scene","version":1,"pages":[{"size":{"width":1.0,"height":1.0},"operations":[{"type":"path","bounds":{"x":0.0,"y":0.0,"width":96.00000762939453,"height":1.0},"data":"M0 0h1v1z","fill":{"r":0.0,"g":0.0,"b":0.0,"a":1.0},"fill_rule":"non_zero","meta":{}}]}]}"#;
        let scene: pliego::DocumentScene = serde_json::from_slice(bytes).unwrap();
        scene.validate().unwrap();
        assert_ne!(scene.normalized_json().unwrap(), bytes);
    }

    #[test]
    fn asset_cache_runtime_accounting_may_advance_between_parent_and_worker() {
        let expected = serde_json::json!({
            "schema": "pliego.resource-policy.v1",
            "asset_manifest": {
                "cache": {
                    "scope": "pliego.asset-cache.v1",
                    "hits": 0,
                    "misses": 2,
                    "invalidations": 0,
                    "evictions": 1
                },
                "assets": [
                    {"url": "https://assets.invalid/a", "sha256": "a", "cache_result": "miss"},
                    {"url": "https://assets.invalid/b", "sha256": "b", "cache_result": "miss"}
                ]
            }
        });
        let actual = serde_json::json!({
            "schema": "pliego.resource-policy.v1",
            "asset_manifest": {
                "cache": {
                    "scope": "pliego.asset-cache.v1",
                    "hits": 1,
                    "misses": 1,
                    "invalidations": 0,
                    "evictions": 1
                },
                "assets": [
                    {"url": "https://assets.invalid/a", "sha256": "a", "cache_result": "hit"},
                    {"url": "https://assets.invalid/b", "sha256": "b", "cache_result": "miss"}
                ]
            }
        });
        assert!(resource_policy_matches_expected(&actual, &expected));

        let mut invalid = actual.clone();
        invalid["asset_manifest"]["cache"]["hits"] = serde_json::json!(2);
        assert!(!resource_policy_matches_expected(&invalid, &expected));
        invalid = actual.clone();
        invalid["asset_manifest"]["assets"][0]["url"] =
            serde_json::json!("https://assets.invalid/substituted");
        assert!(!resource_policy_matches_expected(&invalid, &expected));
    }

    struct TemporaryTree(PathBuf);

    impl TemporaryTree {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pliego-artifact-contract-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TemporaryTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FixtureExpectations {
        page: serde_json::Value,
        resource_policy: serde_json::Value,
        input_url: String,
        input_sha256: String,
        input_resource: String,
        input_bytes: u64,
        font_instance_id: String,
    }

    impl FixtureExpectations {
        fn contract<'a>(
            &'a self,
            public_artifacts: &'a Path,
            public_output: &'a Path,
        ) -> CapturedArtifactExpectation<'a> {
            CapturedArtifactExpectation {
                public_artifacts,
                public_output,
                locale: "en-US",
                timezone: "UTC",
                page: &self.page,
                resource_policy: &self.resource_policy,
                input: CapturedInputExpectation {
                    url: &self.input_url,
                    sha256: &self.input_sha256,
                    resource: &self.input_resource,
                    bytes: self.input_bytes,
                },
                allow_host_fonts: false,
                allow_partial_scene: false,
            }
        }

        fn failed_contract<'a>(
            &'a self,
            render_id: &'a str,
            code: &'a str,
            message: &'a str,
            public_output: &'a Path,
        ) -> FailedArtifactExpectation<'a> {
            FailedArtifactExpectation {
                render_id,
                code,
                message,
                public_output,
                locale: "en-US",
                timezone: "UTC",
                page: &self.page,
                resource_policy: &self.resource_policy,
                input: CapturedInputExpectation {
                    url: &self.input_url,
                    sha256: &self.input_sha256,
                    resource: &self.input_resource,
                    bytes: self.input_bytes,
                },
                allow_host_fonts: false,
            }
        }
    }

    #[test]
    fn accepts_exact_bound_multi_page_capture() {
        let tree = TemporaryTree::new("capture");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = tree.0.with_extension("requested.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 2, &public_artifacts, &public_output);

        validate_captured_artifact_contract(
            &tree.0,
            &deferred,
            fixture.contract(&public_artifacts, &public_output),
        )
        .unwrap();
    }

    #[test]
    fn rejects_host_font_policy_mismatches_and_forbidden_host_selection() {
        let tree = TemporaryTree::new("host-font-policy");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);

        let fonts_path = tree.0.join("fonts.json");
        let mut fonts: serde_json::Value =
            serde_json::from_slice(&fs::read(&fonts_path).unwrap()).unwrap();
        fonts["policy"]["host_fonts"] = "allowed".into();
        fs::write(&fonts_path, serde_json::to_vec(&fonts).unwrap()).unwrap();
        assert!(
            validate_captured_artifact_contract(&tree.0, &deferred, expected)
                .unwrap_err()
                .to_string()
                .contains("host-font policy")
        );

        fonts["policy"]["host_fonts"] = "denied".into();
        fonts["selections"][0]["source"] = "host".into();
        fs::write(&fonts_path, serde_json::to_vec(&fonts).unwrap()).unwrap();
        assert!(
            validate_captured_artifact_contract(&tree.0, &deferred, expected)
                .unwrap_err()
                .to_string()
                .contains("host selection forbidden")
        );

        fonts["selections"][0]["source"] = "memory".into();
        fs::write(&fonts_path, serde_json::to_vec(&fonts).unwrap()).unwrap();
        let environment_path = tree.0.join("environment.json");
        let mut environment: serde_json::Value =
            serde_json::from_slice(&fs::read(&environment_path).unwrap()).unwrap();
        environment["fonts"]["host_fonts"] = "allowed".into();
        fs::write(environment_path, serde_json::to_vec(&environment).unwrap()).unwrap();
        assert!(
            validate_captured_artifact_contract(&tree.0, &deferred, expected)
                .unwrap_err()
                .to_string()
                .contains("host-font policy")
        );
    }

    #[test]
    fn rejects_missing_unknown_and_tampered_environment_fields() {
        let tree = TemporaryTree::new("environment-shape");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);
        let path = tree.0.join("environment.json");
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove("locale");
        fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut unknown = original.clone();
        unknown["attacker"] = true.into();
        fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut page = original.clone();
        page["page"]["size_css_px"]["width"] = 2.into();
        fs::write(&path, serde_json::to_vec(&page).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut input = original.clone();
        input["input_resource"]["url"] = "file:///other.html".into();
        fs::write(&path, serde_json::to_vec(&input).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut accounting = original;
        accounting["resource_accounting"]["body_bytes"] = 0.into();
        fs::write(&path, serde_json::to_vec(&accounting).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
    }

    #[test]
    fn rejects_non_producer_font_shapes_sources_and_warnings() {
        let tree = TemporaryTree::new("font-shape");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);
        let path = tree.0.join("fonts.json");
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

        for invalid_source in ["HOST", "bogus"] {
            let mut fonts = original.clone();
            fonts["selections"][0]["source"] = invalid_source.into();
            fs::write(&path, serde_json::to_vec(&fonts).unwrap()).unwrap();
            assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
        }

        let mut incomplete = original.clone();
        incomplete["font_instances"][0]
            .as_object_mut()
            .unwrap()
            .remove("face_index");
        fs::write(&path, serde_json::to_vec(&incomplete).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut unknown = original.clone();
        unknown["selections"][0]["attacker"] = true.into();
        fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut warning = original;
        warning["warnings"][0]["code"] = "ARBITRARY_WARNING".into();
        fs::write(&path, serde_json::to_vec(&warning).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
    }

    #[test]
    fn rejects_scene_derived_pages_report_and_pdf_tampering() {
        let tree = TemporaryTree::new("scene-derived-artifacts");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);

        let pages_path = tree.0.join("pages.json");
        let pages = fs::read(&pages_path).unwrap();
        let mut tampered: serde_json::Value = serde_json::from_slice(&pages).unwrap();
        tampered["pages"][0]["page_size"] = serde_json::Value::Null;
        fs::write(&pages_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
        fs::write(&pages_path, pages).unwrap();

        let report_path = tree.0.join("scene-report.json");
        let report = fs::read(&report_path).unwrap();
        let mut tampered: serde_json::Value = serde_json::from_slice(&report).unwrap();
        tampered["preview"]["operation_counts"]["image"] = 1.into();
        fs::write(&report_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
        fs::write(&report_path, report).unwrap();

        let structure_path = tree.0.join("pdf-structure.json");
        let mut structure: serde_json::Value =
            serde_json::from_slice(&fs::read(&structure_path).unwrap()).unwrap();
        structure["pages"][0] = serde_json::Value::Null;
        fs::write(structure_path, serde_json::to_vec(&structure).unwrap()).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
    }

    #[test]
    fn rejects_unknown_entries_and_resource_hash_mismatches() {
        let tree = TemporaryTree::new("unknown");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);
        fs::write(tree.0.join("worker-private.txt"), b"not public").unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        fs::remove_file(tree.0.join("worker-private.txt")).unwrap();
        let resource = fs::read_dir(tree.0.join("resources"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(resource, b"changed").unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
    }

    #[test]
    fn rejects_mismatched_scene_and_render_receipts() {
        let tree = TemporaryTree::new("identities");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (mut deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);
        let scene_hash = deferred.scene_hash.clone();
        deferred.scene_hash = content_address(b"another scene");
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        deferred.scene_hash = scene_hash;
        deferred.rendered_bytes += 1;
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
    }

    #[test]
    fn rejects_non_ready_snapshot_even_when_receipt_identity_matches() {
        let tree = TemporaryTree::new("not-ready");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (mut deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let readiness = serde_json::to_vec(&serde_json::json!({
            "status": "pending",
            "render_id": deferred.render_id,
        }))
        .unwrap();
        fs::write(tree.0.join("readiness.json"), &readiness).unwrap();
        deferred.readiness_sha256 = content_address(&readiness);
        deferred.readiness_bytes = readiness.len() as u64;
        assert!(
            validate_captured_artifact_contract(
                &tree.0,
                &deferred,
                fixture.contract(&public_artifacts, &public_output),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_success_state_and_orphan_resources() {
        let tree = TemporaryTree::new("state-and-orphan");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);
        let state = fs::read(tree.0.join("session-state.jsonl")).unwrap();
        fs::write(tree.0.join("session-state.jsonl"), b"").unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        fs::write(
            tree.0.join("session-state.jsonl"),
            &state[..state.len() - 1],
        )
        .unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut concatenated = state[..state.len() - 1].to_vec();
        concatenated.extend_from_slice(&state);
        fs::write(tree.0.join("session-state.jsonl"), concatenated).unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        fs::write(tree.0.join("session-state.jsonl"), state).unwrap();
        let orphan = b"unreferenced resource";
        let orphan_hash = content_address(orphan);
        fs::write(
            tree.0
                .join("resources")
                .join(orphan_hash.strip_prefix("sha256:").unwrap()),
            orphan,
        )
        .unwrap();
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
    }

    #[test]
    fn accepts_large_streamed_producer_artifacts_within_the_tree_limit() {
        let tree = TemporaryTree::new("large-data-plane");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (mut deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let large = "x".repeat(MAX_CONTROL_JSON_BYTES as usize + 1024);
        let scene: pliego::DocumentScene = serde_json::from_value(serde_json::json!({
            "schema": "pliego.document-scene",
            "version": 1,
            "pages": [{
                "size": { "width": 1.0, "height": 1.0 },
                "operations": [{
                    "type": "text",
                    "text": large.clone(),
                    "font": fixture.font_instance_id.clone(),
                    "font_size": 1.0,
                    "color": { "r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0 },
                    "glyphs": [{
                        "id": 1,
                        "x": 0.0,
                        "y": 0.0,
                        "advance": 1.0,
                        "text_range": { "start": 0, "end": large.len() },
                    }],
                    "meta": {},
                }],
            }],
        }))
        .unwrap();
        let normalized_scene = scene.normalized_json().unwrap();
        fs::write(tree.0.join("scene.json"), &normalized_scene).unwrap();
        deferred.scene_hash = content_address(&normalized_scene);
        let mut pages: serde_json::Value =
            serde_json::from_slice(&fs::read(tree.0.join("pages.json")).unwrap()).unwrap();
        pages["pages"][0]["operation_counts"]["text"] = 1.into();
        fs::write(
            tree.0.join("pages.json"),
            serde_json::to_vec(&pages).unwrap(),
        )
        .unwrap();
        let mut structure: serde_json::Value =
            serde_json::from_slice(&fs::read(tree.0.join("pdf-structure.json")).unwrap()).unwrap();
        structure["pages"][0]["expected_extracted_unicode"] = large.clone().into();
        structure["pages"][0]["embedded_font_ids"] =
            serde_json::json!([fixture.font_instance_id.clone()]);
        structure["pages"][0]["operation_counts"]["text"] = 1.into();
        fs::write(
            tree.0.join("pdf-structure.json"),
            serde_json::to_vec(&structure).unwrap(),
        )
        .unwrap();
        fs::write(
            tree.0.join("layout-debug.json"),
            serde_json::to_vec(&serde_json::json!({ "payload": large })).unwrap(),
        )
        .unwrap();
        let report_path = tree.0.join("scene-report.json");
        let mut report: serde_json::Value =
            serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
        report["scene"]["hash"] = deferred.scene_hash.clone().into();
        report["preview"]["pages"][0]["operation_counts"]["text"] = 1.into();
        report["preview"]["operation_counts"]["text"] = 1.into();
        fs::write(report_path, serde_json::to_vec(&report).unwrap()).unwrap();

        validate_captured_artifact_contract(
            &tree.0,
            &deferred,
            fixture.contract(&public_artifacts, &public_output),
        )
        .unwrap();
    }

    #[test]
    fn accepts_repeated_loaded_occurrences_for_one_content_address() {
        let tree = TemporaryTree::new("repeated-resource");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let path = tree.0.join("resources.jsonl");
        let mut rows = read_jsonl::<serde_json::Value>(&path, "resources.jsonl", true).unwrap();
        let mut requested = rows[0].clone();
        requested["request_id"] = "document-session:000001".into();
        requested["url"] = "file:///fixture-style.css".into();
        let mut terminal = rows[1].clone();
        terminal["request_id"] = "document-session:000001".into();
        terminal["url"] = "file:///fixture-style.css".into();
        terminal["urls"] = serde_json::json!(["file:///fixture-style.css"]);
        terminal["destination"] = "Style".into();
        terminal["is_for_main_frame"] = false.into();
        rows.extend([requested, terminal]);
        write_jsonl_values(&path, &rows);
        let environment_path = tree.0.join("environment.json");
        let mut environment: serde_json::Value =
            serde_json::from_slice(&fs::read(&environment_path).unwrap()).unwrap();
        environment["resource_accounting"]["requests"] = 2.into();
        environment["resource_accounting"]["loaded"] = 2.into();
        environment["resource_accounting"]["body_bytes"] = (fixture.input_bytes * 2).into();
        fs::write(environment_path, serde_json::to_vec(&environment).unwrap()).unwrap();

        validate_captured_artifact_contract(
            &tree.0,
            &deferred,
            fixture.contract(&public_artifacts, &public_output),
        )
        .unwrap();
    }

    #[test]
    fn rejects_producer_impossible_resource_rows_and_resolved_hashes() {
        let tree = TemporaryTree::new("resource-row-shape");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = public_artifacts.join("document.pdf");
        let (mut deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let expected = fixture.contract(&public_artifacts, &public_output);
        let path = tree.0.join("resources.jsonl");
        let original = read_jsonl::<serde_json::Value>(&path, "resources.jsonl", true).unwrap();

        let mut rows = original.clone();
        rows[1]["method"] = "POST".into();
        write_jsonl_values(&path, &rows);
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut rows = original.clone();
        rows[1]["response_status"] = 999.into();
        write_jsonl_values(&path, &rows);
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut rows = original.clone();
        rows[1]["response_headers"]["count"] = (MAX_RESPONSE_HEADER_COUNT as u64 + 1).into();
        write_jsonl_values(&path, &rows);
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        let mut rows = original.clone();
        rows[1]["attacker"] = true.into();
        write_jsonl_values(&path, &rows);
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());

        write_jsonl_values(&path, &original);
        deferred.resolved_input_hash = content_address(b"invented resource map");
        assert!(validate_captured_artifact_contract(&tree.0, &deferred, expected).is_err());
    }

    #[test]
    fn failure_contract_binds_phased_environment_and_ledger_input() {
        let tree = TemporaryTree::new("failure-phases");
        let public_artifacts = tree.0.with_extension("public");
        let public_output = tree.0.with_extension("requested.pdf");
        let (deferred, fixture) =
            write_captured_fixture(&tree.0, 1, &public_artifacts, &public_output);
        let code = "READINESS_TIMEOUT";
        let message = "deadline elapsed";
        for name in [
            "document.pdf",
            "fonts.json",
            "layout-debug.json",
            "pages.json",
            "pdf-structure.json",
            "render.png",
            "scene-preview.png",
            "scene-report.json",
            "scene.json",
        ] {
            fs::remove_file(tree.0.join(name)).unwrap();
        }
        let font_digest = content_address(b"font bytes");
        fs::remove_file(
            tree.0
                .join("resources")
                .join(font_digest.strip_prefix("sha256:").unwrap()),
        )
        .unwrap();
        write_failure_state(&tree.0, message, true);
        write_failure_artifact(&tree.0, &deferred.render_id, code, message);
        let expected = fixture.failed_contract(&deferred.render_id, code, message, &public_output);

        validate_failed_artifact_contract(&tree.0, expected).unwrap();

        let environment_path = tree.0.join("environment.json");
        let environment = fs::read(&environment_path).unwrap();
        fs::remove_file(&environment_path).unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());
        fs::write(&environment_path, &environment).unwrap();

        let mut runtime_only: serde_json::Value = serde_json::from_slice(&environment).unwrap();
        runtime_only
            .as_object_mut()
            .unwrap()
            .remove("input_resource");
        runtime_only
            .as_object_mut()
            .unwrap()
            .remove("resolved_input_hash");
        fs::write(
            &environment_path,
            serde_json::to_vec(&runtime_only).unwrap(),
        )
        .unwrap();
        validate_failed_artifact_contract(&tree.0, expected).unwrap();

        let ledger_path = tree.0.join("resources.jsonl");
        let original_rows =
            read_jsonl::<serde_json::Value>(&ledger_path, "resources.jsonl", true).unwrap();
        let mut rows = original_rows.clone();
        rows[0]["url"] = "file:///different.html".into();
        rows[1]["url"] = "file:///different.html".into();
        rows[1]["urls"] = serde_json::json!(["file:///different.html"]);
        write_jsonl_values(&ledger_path, &rows);
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());
        write_jsonl_values(&ledger_path, &original_rows);

        let mut unknown: serde_json::Value = serde_json::from_slice(&environment).unwrap();
        unknown["attacker"] = true.into();
        fs::write(&environment_path, serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());

        let mut missing: serde_json::Value = serde_json::from_slice(&environment).unwrap();
        missing.as_object_mut().unwrap().remove("locale");
        fs::write(&environment_path, serde_json::to_vec(&missing).unwrap()).unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());

        let mut partial_phase: serde_json::Value = serde_json::from_slice(&environment).unwrap();
        partial_phase
            .as_object_mut()
            .unwrap()
            .remove("resource_accounting");
        fs::write(
            environment_path,
            serde_json::to_vec(&partial_phase).unwrap(),
        )
        .unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());

        let mut base_phase: serde_json::Value = serde_json::from_slice(&environment).unwrap();
        for field in [
            "runtime",
            "resource_accounting",
            "phase_timings_ms",
            "input_resource",
            "resolved_input_hash",
        ] {
            base_phase.as_object_mut().unwrap().remove(field);
        }
        fs::write(
            tree.0.join("environment.json"),
            serde_json::to_vec(&base_phase).unwrap(),
        )
        .unwrap();
        write_failure_state(&tree.0, message, false);
        fs::write(
            tree.0.join("readiness.json"),
            serde_json::to_vec(&serde_json::json!({
                "status": "failed",
                "render_id": deferred.render_id.clone(),
                "error": { "code": code, "message": message },
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());
    }

    #[test]
    fn failure_contract_binds_exact_error_and_rejects_publication_controls() {
        let tree = TemporaryTree::new("failure");
        write_base_tree(&tree.0);
        let render_id = content_address(b"render");
        let public_output = tree.0.with_extension("public.pdf");
        fs::write(
            tree.0.join("session-state.jsonl"),
            serde_json::to_vec(&serde_json::json!({
                "timestamp_ms": 1,
                "state": "failed",
                "message": "deadline elapsed",
            }))
            .unwrap()
            .into_iter()
            .chain([b'\n'])
            .collect::<Vec<_>>(),
        )
        .unwrap();
        fs::write(
            tree.0.join("failure.json"),
            serde_json::to_vec(&serde_json::json!({
                "status": "failed",
                "render_id": render_id,
                "error": {
                    "code": "READINESS_TIMEOUT",
                    "message": "deadline elapsed",
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let page = serde_json::json!({ "fixture": "page" });
        let resource_policy = serde_json::json!({ "fixture": "policy" });
        let input_sha256 = "0".repeat(64);
        let input_resource = format!("sha256:{input_sha256}");
        let expected = FailedArtifactExpectation {
            render_id: &render_id,
            code: "READINESS_TIMEOUT",
            message: "deadline elapsed",
            public_output: &public_output,
            locale: "en-US",
            timezone: "UTC",
            page: &page,
            resource_policy: &resource_policy,
            input: CapturedInputExpectation {
                url: "file:///fixture.html",
                sha256: &input_sha256,
                resource: &input_resource,
                bytes: 0,
            },
            allow_host_fonts: false,
        };
        validate_failed_artifact_contract(&tree.0, expected).unwrap();

        fs::write(tree.0.join("render.png"), PNG_SIGNATURE).unwrap();
        validate_failed_artifact_contract(&tree.0, expected).unwrap();
        fs::write(tree.0.join("document.pdf"), b"%PDF-1.7\n").unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());
        fs::remove_file(tree.0.join("document.pdf")).unwrap();
        fs::remove_file(tree.0.join("render.png")).unwrap();

        let orphan = b"unreferenced failure resource";
        let orphan_hash = content_address(orphan);
        let orphan_path = tree
            .0
            .join("resources")
            .join(orphan_hash.strip_prefix("sha256:").unwrap());
        fs::write(&orphan_path, orphan).unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());
        fs::remove_file(orphan_path).unwrap();

        fs::create_dir(tree.0.join("publication")).unwrap();
        assert!(validate_failed_artifact_contract(&tree.0, expected).is_err());
    }

    fn write_captured_fixture(
        root: &Path,
        pages: usize,
        public_artifacts: &Path,
        public_output: &Path,
    ) -> (DeferredCapturedPublication, FixtureExpectations) {
        write_base_tree(root);
        fs::write(
            root.join("session-state.jsonl"),
            serde_json::to_vec(&serde_json::json!({
                "timestamp_ms": 1,
                "state": "started",
                "message": null,
            }))
            .unwrap()
            .into_iter()
            .chain([b'\n'])
            .collect::<Vec<_>>(),
        )
        .unwrap();
        let render_id = content_address(b"render");
        let resolved_input_hash = compute_resolved_input_hash(&render_id, &BTreeMap::new());
        let page = serde_json::json!({
            "size_css_px": { "width": 1.0, "height": 1.0 },
            "margins_css_px": { "top": 0.0, "right": 0.0, "bottom": 0.0, "left": 0.0 },
        });
        let resource_policy = serde_json::json!({
            "schema": "pliego.resource-policy.v1",
            "version": 1,
            "render_id": render_id,
            "network": "deny",
            "http_roots": [],
            "filesystem": "document-root",
            "data_urls": "allow",
            "redirects": "deny",
            "timeout_ms": 10_000,
            "virtual_resources": [],
        });
        let input_url = "file:///fixture.html".to_owned();
        let input_body = b"<!doctype html><title>fixture</title>";
        let input_content_type = "text/html; charset=utf-8";
        let input_response_headers = fixture_response_headers(input_body.len(), input_content_type);
        let input_sha256 = lower_hex(&Sha256::digest(input_body));
        let input_resource = format!("sha256:{input_sha256}");
        fs::write(root.join("resources").join(&input_sha256), input_body).unwrap();
        let font_body = b"font bytes";
        let font_resource = content_address(font_body);
        let font_digest = font_resource.strip_prefix("sha256:").unwrap();
        fs::write(root.join("resources").join(font_digest), font_body).unwrap();
        let font_instance_id = font_instance_id(&font_resource, 0, &[], false).unwrap();
        let readiness = serde_json::to_vec(&serde_json::json!({
            "status": "ready",
            "payload": { "fixture": true },
            "font_status": "loaded",
            "render_id": render_id,
        }))
        .unwrap();
        fs::write(root.join("readiness.json"), &readiness).unwrap();
        fs::write(
            root.join("environment.json"),
            serde_json::to_vec(&serde_json::json!({
                "locale": { "requested": "en-US", "resolved": "en-US" },
                "timezone": { "requested": "UTC", "resolved": "UTC" },
                "page": page,
                "resource_policy": resource_policy,
                "fonts": { "host_fonts": "denied" },
                "resolved_input_hash": resolved_input_hash,
                "document_pdf": {
                    "artifact": public_output.to_string_lossy(),
                    "status": "pending",
                    "error": null,
                },
                "input_resource": {
                    "render_id": render_id,
                    "url": input_url,
                    "sha256": input_sha256,
                    "resource": input_resource,
                    "bytes": input_body.len(),
                    "source": "document_root",
                    "main_frame": true,
                },
                "runtime": { "adapter": "document-session" },
                "resource_accounting": {
                    "requests": 1,
                    "loaded": 1,
                    "delegated": 0,
                    "failed": 0,
                    "body_bytes": input_body.len(),
                    "unavailable_bodies": 0,
                },
                "phase_timings_ms": {
                    "controlled_runtime": 1.0,
                    "scene_capture": 1.0,
                },
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(root.join("layout-debug.json"), b"{}").unwrap();
        let png = b"\x89PNG\r\n\x1a\nfixture";
        fs::write(root.join("render.png"), png).unwrap();
        if pages == 1 {
            fs::write(root.join("scene-preview.png"), png).unwrap();
        } else {
            fs::create_dir(root.join("pages")).unwrap();
            for page in 1..=pages {
                fs::write(root.join("pages").join(format!("page-{page:04}.png")), png).unwrap();
            }
        }
        let resource_rows = [
            serde_json::json!({
                "timestamp_ms": 1,
                "render_id": render_id,
                "policy": "pliego.resource-policy.v1",
                "request_id": "document-session:000000",
                "url": input_url,
                "status": "requested",
                "bytes": null,
            }),
            serde_json::json!({
                "timestamp_ms": 1,
                "render_id": render_id,
                "policy": "pliego.resource-policy.v1",
                "request_id": "document-session:000000",
                "url": input_url,
                "urls": [input_url],
                "status": "loaded",
                "code": null,
                "method": "GET",
                "destination": "Document",
                "load_role": "DocumentContent",
                "fatal": false,
                "cancelled": false,
                "referrer_url": null,
                "is_for_main_frame": true,
                "is_redirect": false,
                "source": "document_root",
                "response_status": 200,
                "content_type": input_content_type,
                "bytes": input_body.len(),
                "sha256": input_sha256,
                "resource": input_resource,
                "content_hash": input_resource,
                "response_headers": input_response_headers,
                "cache_result": null,
                "artifact": format!("resources/{input_sha256}"),
                "failure": null,
            }),
        ];
        let mut resource_ledger = Vec::new();
        for row in resource_rows {
            resource_ledger.extend_from_slice(&serde_json::to_vec(&row).unwrap());
            resource_ledger.push(b'\n');
        }
        fs::write(root.join("resources.jsonl"), resource_ledger).unwrap();
        let page_rows = (0..pages)
            .map(|index| {
                serde_json::json!({
                    "index": index,
                    "artifact": if pages == 1 {
                        "scene-preview.png".to_owned()
                    } else {
                        format!("pages/page-{:04}.png", index + 1)
                    },
                    "page_size": { "width": 1.0, "height": 1.0 },
                    "operation_counts": {
                        "text": usize::from(index == 0),
                        "vector": 0,
                        "image": 0,
                        "link": 0,
                    },
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            root.join("pages.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": "pliego.pages",
                "version": 1,
                "page_count": pages,
                "pages": page_rows,
            }))
            .unwrap(),
        )
        .unwrap();
        let scene: pliego::DocumentScene = serde_json::from_value(serde_json::json!({
            "schema": "pliego.document-scene",
            "version": 1,
            "pages": (0..pages).map(|index| serde_json::json!({
                "size": { "width": 1.0, "height": 1.0 },
                "operations": if index == 0 {
                    vec![serde_json::json!({
                        "type": "text",
                        "text": "fixture",
                        "font": font_instance_id,
                        "font_size": 1.0,
                        "color": { "r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0 },
                        "glyphs": [{
                            "id": 1,
                            "x": 0.0,
                            "y": 0.0,
                            "advance": 1.0,
                            "text_range": { "start": 0, "end": 7 },
                        }],
                        "meta": {},
                    })]
                } else {
                    Vec::new()
                },
            })).collect::<Vec<_>>(),
        }))
        .unwrap();
        let scene = scene.normalized_json().unwrap();
        fs::write(root.join("scene.json"), &scene).unwrap();
        let pdf = b"%PDF-1.7\nfixture";
        fs::write(root.join("document.pdf"), pdf).unwrap();
        fs::write(
            root.join("pdf-structure.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": "pliego.pdf-structure",
                "version": 1,
                "backend": "krilla",
                "pdf": {
                    "artifact": "document.pdf",
                    "sha256": content_address(pdf),
                    "bytes": pdf.len(),
                },
                "page_count": pages,
                "pages": (0..pages).map(|index| serde_json::json!({
                    "index": index,
                    "scene_page_size_css_px": { "width": 1.0, "height": 1.0 },
                    "media_box_pt": [0.0, 0.0, 0.75, 0.75],
                    "expected_extracted_unicode": if index == 0 { "fixture" } else { "" },
                    "embedded_font_ids": if index == 0 {
                        vec![font_instance_id.clone()]
                    } else {
                        Vec::new()
                    },
                    "operation_counts": {
                        "text": usize::from(index == 0),
                        "vector": 0,
                        "image": 0,
                        "link": 0,
                    },
                })).collect::<Vec<_>>(),
            }))
            .unwrap(),
        )
        .unwrap();

        let deferred = DeferredCapturedPublication {
            schema: "pliego.deferred-captured-publication".into(),
            version: 1,
            render_id,
            readiness_sha256: content_address(&readiness),
            readiness_bytes: readiness.len() as u64,
            resolved_input_hash,
            controlled_runtime_ms: 1.0,
            scene_capture_ms: 1.0,
            scene_schema: "pliego.document-scene".into(),
            scene_version: 1,
            scene_hash: content_address(&scene),
            page_count: pages,
            preview_count: pages,
            capture_status: "complete".into(),
            capture_code: None,
            preview_status: "rendered".into(),
            unsupported_event_count: 0,
            text_mapping_gap_count: 0,
            pdf_status: "rendered".into(),
            pdf_structure_status: "rendered".into(),
            scene_setup_ms: 1.0,
            preview_ms: 1.0,
            pdf_ms: 1.0,
            rendered_bytes: png.len() as u64,
        };
        fs::write(
            root.join("fonts.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": "pliego.font-report",
                "version": 1,
                "policy": { "host_fonts": "denied" },
                "manifest": {
                    "resolution": "css-order",
                    "entries": [{
                        "instance": font_instance_id,
                        "resource": font_resource,
                        "face_index": 0,
                        "source": "memory",
                        "requested_families": ["Missing Preferred", "Fixture Sans"],
                        "selected_family": "Fixture Sans",
                    }],
                },
                "font_resources": [{
                    "resource": font_resource,
                    "bytes_base64": "Zm9udCBieXRlcw==",
                }],
                "font_instances": [{
                    "id": font_instance_id,
                    "resource": font_resource,
                    "face_index": 0,
                    "variations": [],
                    "synthetic_bold": false,
                }],
                "selections": [{
                    "instance": font_instance_id,
                    "resource": font_resource,
                    "face_index": 0,
                    "source": "memory",
                    "requested_families": ["Missing Preferred", "Fixture Sans"],
                    "selected_family": "Fixture Sans",
                }],
                "warnings": [{
                    "code": "FONT_FALLBACK_USED",
                    "instance": font_instance_id,
                    "requested_family": "Missing Preferred",
                    "selected_family": "Fixture Sans",
                    "fallback_chain": ["Missing Preferred", "Fixture Sans"],
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("scene-report.json"),
            serde_json::to_vec(&serde_json::json!({
                "scene": {
                    "schema": deferred.scene_schema,
                    "version": deferred.scene_version,
                    "hash": deferred.scene_hash,
                    "validation": "valid",
                },
                "capture": {
                    "status": deferred.capture_status,
                    "code": deferred.capture_code,
                    "unsupported_events": [],
                    "text_mapping_gaps": [],
                    "canvases": [],
                },
                "preview": {
                    "status": deferred.preview_status,
                    "artifact": (pages == 1).then_some("scene-preview.png"),
                    "page_count": pages,
                    "pages": page_rows,
                    "page_size": { "width": 1.0, "height": 1.0 },
                    "operation_counts": { "text": 1, "vector": 0, "image": 0, "link": 0 },
                    "unsupported": [],
                },
                "document_pdf": {
                    "status": deferred.pdf_status,
                    "artifact": public_artifacts.join("document.pdf").to_string_lossy(),
                    "error": null,
                },
                "pdf_structure": {
                    "status": deferred.pdf_structure_status,
                    "artifact": public_artifacts.join("pdf-structure.json").to_string_lossy(),
                    "error": null,
                },
            }))
            .unwrap(),
        )
        .unwrap();
        (
            deferred,
            FixtureExpectations {
                page,
                resource_policy,
                input_url,
                input_sha256,
                input_resource,
                input_bytes: input_body.len() as u64,
                font_instance_id,
            },
        )
    }

    fn write_base_tree(root: &Path) {
        fs::create_dir(root.join("resources")).unwrap();
        for name in REQUIRED_BASE_FILES {
            fs::write(root.join(name), b"").unwrap();
        }
    }

    fn write_jsonl_values(path: &Path, rows: &[serde_json::Value]) {
        let mut bytes = Vec::new();
        for row in rows {
            bytes.extend_from_slice(&serde_json::to_vec(row).unwrap());
            bytes.push(b'\n');
        }
        fs::write(path, bytes).unwrap();
    }

    fn write_failure_state(root: &Path, message: &str, started: bool) {
        let mut rows = Vec::new();
        if started {
            rows.push(serde_json::json!({
                "timestamp_ms": 1,
                "state": "started",
                "message": null,
            }));
        }
        rows.push(serde_json::json!({
            "timestamp_ms": 2,
            "state": "failed",
            "message": message,
        }));
        write_jsonl_values(&root.join("session-state.jsonl"), &rows);
    }

    fn write_failure_artifact(root: &Path, render_id: &str, code: &str, message: &str) {
        fs::write(
            root.join("failure.json"),
            serde_json::to_vec(&serde_json::json!({
                "status": "failed",
                "render_id": render_id,
                "error": { "code": code, "message": message },
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn fixture_response_headers(bytes: usize, content_type: &str) -> serde_json::Value {
        let content_length = bytes.to_string();
        let entries = [
            ("content-length", content_length.as_str()),
            ("content-type", content_type),
        ];
        let mut canonical = b"pliego.response-headers.v1\0".to_vec();
        let mut retained_bytes = 0u64;
        for (name, value) in entries {
            retained_bytes += (name.len() + value.len()) as u64;
            canonical.extend_from_slice(&(name.len() as u32).to_be_bytes());
            canonical.extend_from_slice(name.as_bytes());
            canonical.extend_from_slice(&(value.len() as u32).to_be_bytes());
            canonical.extend_from_slice(value.as_bytes());
        }
        serde_json::json!({
            "count": 2,
            "bytes": retained_bytes,
            "names": ["content-length", "content-type"],
            "sha256": lower_hex(&Sha256::digest(&canonical)),
        })
    }
}
