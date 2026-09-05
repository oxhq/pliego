/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pliego's minimal one-document Servo owner.
//!
//! The default CLI route and its explicit alias settle a bounded document, reserve an exact Paint
//! presentation, consume ScriptThread's retained generation once, and read pixels only after both
//! sides still match. They never fall back to realtime capture. The direct realtime adapter remains
//! available only to internal diagnostics and the nonproduction parity boundary.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use dpi::PhysicalSize;
use embedder_traits::{
    DocumentCaptureCommit, DocumentCaptureConsumeRequest, DocumentCapturePrecondition,
    DocumentCaptureSurfaceFingerprint, DocumentClockConfiguration, DocumentPaintPresentationTicket,
    DocumentTimeControlAction, DocumentTimeControlCommand, DocumentTimeControlError,
    DocumentTimeControlObservation, DocumentTimeControlOutcome, DocumentTimeControlReceiveOutcome,
    DocumentTimeControlTryReceiveOutcome,
};
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};
use layout::pages::{PageDefinition, reserve_for_process};
use pliego::capture::{
    SceneCapture, capture_controlled_document_scene_with_canvas, capture_document_scene_with_canvas,
};
use pliego::event_loop_waker::{EventLoopWakeWaitOutcome, PliegoEventLoopWaker};
use pliego::pdf::{PdfFontResource, PdfFontVariation, render_document_pdf};
use servo::{
    ConsoleLogLevel, ControlledDocumentCaptureError, ControlledDocumentCaptureReservation,
    ControlledDocumentCaptureRetry, JSValue, LoadStatus, Preferences, RenderingContext, Servo,
    ServoBuilder, SoftwareRenderingContext, UserContentManager, UserScript, WebResourceLoad,
    WebResourceLoadRole, WebResourceResponse, WebView, WebViewBuilder, WebViewDelegate,
};
use servo_geometry::{
    DeviceIndependentIntPoint, DeviceIndependentIntRect, DeviceIndependentIntSize,
};
use url::Url;

use super::asset_cache::MAX_CACHE_BYTES;
use super::controlled_settlement::{
    ControlledSettlementCoordinator, ControlledSettlementError, ControlledSettlementProgress,
};
use super::engine::RenderEnvironment;
use super::owned_resource_store::{OwnedResourceStore, decode_bounded_data_url};
use super::readiness::{self, Readiness, ReadinessPolicy};
use super::render_environment::{apply_timezone, unexpected_host_font};
use super::resource_policy::{
    ControlledResource, MAX_RESOURCE_EVENTS, MAX_RESOURCE_METADATA_BYTES, MAX_RESOURCE_TIMEOUT_MS,
    ResourceAccounting, ResourceEvidence, ResourcePolicy, ResourcePolicyConfig,
    ResourcePolicyDecision, ResourcePolicyFailure, ResourcePolicySetupFailure, ResourceRequest,
    ResourceSource, create_controlled_http_client, fetch_controlled_http,
    normalize_controlled_response_headers, sha256_hex,
};
use super::runtime_policy::DeterministicRuntimePolicy;
use super::session::LocalDocument;
use crate::api2::{ResolvedInputJob, ResolvedRenderJob};

const RESOURCE_EVIDENCE_ENTRY_OVERHEAD_BYTES: u64 = 256;
const CONSOLE_EVIDENCE_ENTRY_OVERHEAD_BYTES: u64 = 64;
const MAX_CONSOLE_EVENTS: usize = 4_096;
const MAX_CONSOLE_BYTES: u64 = 1024 * 1024;

const TIMEOUT: Duration = Duration::from_secs(30);
const FROZEN_INPUT_URL_ROOT: &str = "pliego-input:///";
const SESSION_DIAGNOSTICS_ENV: &str = "PLIEGO_SESSION_DIAGNOSTICS";
const MAX_SESSION_DIAGNOSTIC_BYTES: usize = 8_192;
const MAX_SESSION_DIAGNOSTIC_ITEMS: usize = 8;

/// Private failure-only instrumentation. Never serialize raw outcomes: transport errors, capture
/// commits, resource URLs and authored console/readiness payloads may contain arbitrary content.
/// The opt-in state keeps only counters and the last bounded metadata snapshot, not a journal.
#[derive(Default)]
struct SessionDiagnostics {
    startup_ms: [Option<f64>; 3],
    phase: &'static str,
    commands: u64,
    response_count: u64,
    rejections: u64,
    generation_reobservations: u64,
    last_command: Option<ControlledSettlementStep>,
    last_outcome: Option<&'static str>,
    last_rejection: Option<&'static str>,
    last_observation: Option<serde_json::Value>,
    last_observation_response: Option<u64>,
    emitted: bool,
}

impl SessionDiagnostics {
    fn settlement_timeout(state: Option<&RefCell<Self>>, site: &'static str) -> SessionError {
        if let Some(state) = state {
            state.borrow_mut().phase = site;
        }
        SessionError::new(
            "SETTLEMENT_TIMEOUT",
            "controlled settlement exceeded the normalized host-wall limit",
        )
    }

    fn enabled(value: Option<&std::ffi::OsStr>, controlled: bool) -> bool {
        controlled && value == Some(std::ffi::OsStr::new("1"))
    }

    fn command(&mut self, command: &DocumentTimeControlCommand) {
        self.commands = self.commands.saturating_add(1);
        self.last_command = Some(controlled_settlement_step(command));
    }

    fn outcome(&mut self, outcome: &DocumentTimeControlReceiveOutcome) {
        self.response_count = self.response_count.saturating_add(1);
        self.last_outcome = Some(match outcome {
            DocumentTimeControlReceiveOutcome::CommandOutcome(outcome) => match outcome {
                DocumentTimeControlOutcome::Completed(observation) => {
                    self.last_observation = Some(session_observation_diagnostic(observation));
                    self.last_observation_response = Some(self.response_count);
                    "completed"
                },
                DocumentTimeControlOutcome::Rejected(error) => {
                    self.rejections = self.rejections.saturating_add(1);
                    let rejection = match error {
                        DocumentTimeControlError::EventLoopUnavailable => "event-loop-unavailable",
                        DocumentTimeControlError::TargetChanged { .. } => "target-changed",
                        DocumentTimeControlError::AdvanceInputChanged { .. } |
                        DocumentTimeControlError::AdvanceClockChanged { .. } |
                        DocumentTimeControlError::AdvanceProducerChanged { .. } => {
                            "advance-state-changed"
                        },
                        DocumentTimeControlError::Timer(_) => "timer-rejected",
                        _ => "other-rejection",
                    };
                    self.last_rejection = Some(rejection);
                    rejection
                },
                _ => "indeterminate",
            },
            _ => "transport-failure-or-indeterminate",
        });
    }

    fn failure_line(
        &self,
        deadline: SessionHostDeadline,
        error: &SessionError,
        delegate: Option<&DocumentDelegate>,
    ) -> Vec<u8> {
        let resources = delegate.map(|delegate| delegate.resources.borrow());
        let resource_entries = resources
            .as_ref()
            .map_or(&[][..], |log| log.entries.as_slice());
        let record = serde_json::json!({
            "diagnostic": "pliego-session-diagnostics-v1",
            "timeout_site": matches!(error.code.as_str(), "SETTLEMENT_TIMEOUT" | "READINESS_TIMEOUT").then_some(self.phase),
            "failure_class": match error.code.as_str() {
                "SETTLEMENT_TIMEOUT" => "SETTLEMENT_TIMEOUT",
                "READINESS_TIMEOUT" => "READINESS_TIMEOUT",
                "CONTROLLED_CAPTURE_TIMEOUT" => "CONTROLLED_CAPTURE_TIMEOUT",
                _ => "OTHER_SESSION_FAILURE",
            },
            "host_elapsed_ms": deadline.elapsed_ms(),
            "host_limit_ms": deadline.deadline.duration_since(deadline.started).as_secs_f64() * 1000.0,
            "startup_ms": {"render_context_ready": self.startup_ms[0],
                "servo_ready": self.startup_ms[1], "webview_ready": self.startup_ms[2]},
            "commands": self.commands,
            "responses": self.response_count, "rejections": self.rejections,
            "generation_reobservations": self.generation_reobservations,
            "last_command": self.last_command.map(|command| format!("{command:?}")),
            "last_outcome": self.last_outcome, "last_observation": self.last_observation,
            "last_rejection": self.last_rejection,
            "last_observation_response": self.last_observation_response,
            "load_complete": delegate.map(|delegate| delegate.load_complete.get()),
            "resource_events": resources.as_ref().map(|log| log.observed_events),
            "resource_entries": resource_entries.len(),
            "resource_loaded": resource_entries.iter().filter(|resource| resource.status == "loaded").count(),
            "resource_destinations": resource_entries.iter().take(MAX_SESSION_DIAGNOSTIC_ITEMS)
                .map(|resource| match resource.request.destination.as_str() {
                    value @ ("Document" | "Style" | "Font" | "Image" | "Script" | "Empty") => value,
                    _ => "other",
                }).collect::<Vec<_>>(),
        });
        let mut bytes = serde_json::to_vec(&record).unwrap_or_default();
        if bytes.is_empty() || bytes.len() >= MAX_SESSION_DIAGNOSTIC_BYTES {
            bytes = br#"{"diagnostic":"pliego-session-diagnostics-v1","encoding_failed":true}"#
                .to_vec();
        }
        bytes.push(b'\n');
        bytes
    }

    fn emit_failure(
        &mut self,
        deadline: SessionHostDeadline,
        error: &SessionError,
        delegate: Option<&DocumentDelegate>,
    ) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        // Best-effort stderr only, before Servo shutdown. A broken diagnostic sink must not replace
        // the original typed product error. There are no per-turn writes or public artifact files.
        let _ = std::io::stderr()
            .lock()
            .write_all(&self.failure_line(deadline, error, delegate));
    }
}

fn session_observation_diagnostic(
    observation: &DocumentTimeControlObservation,
) -> serde_json::Value {
    let producer_pending_by_kind = [
        timers::DocumentProducerKind::Task,
        timers::DocumentProducerKind::Resource,
        timers::DocumentProducerKind::Font,
        timers::DocumentProducerKind::Image,
        timers::DocumentProducerKind::ExternalCallback,
    ]
    .map(|kind| {
        serde_json::json!({"kind": kind,
        "pending": observation.producers.snapshot.for_kind(kind).pending()})
    });
    serde_json::json!({
        "virtual_time_ns": observation.now.as_nanos().to_string(),
        "top_level_epoch": observation.target.webview_epoch.0,
        "fully_active_pipelines": observation.target.fully_active_pipelines.len(),
        "pending_events": observation.pending_events,
        "input_batch_saturated": observation.input_batch_saturated,
        "has_next_deadline": observation.next_deadline.is_some(),
        "producer_stability": observation.producers.stability,
        "producer_pending": observation.producers.snapshot.pending(),
        "producer_pending_by_kind": producer_pending_by_kind,
        "documents": observation.documents.len(),
        "first_document_readiness": observation.documents.first().map(|document|
            document.readiness_blockers.iter().take(MAX_SESSION_DIAGNOSTIC_ITEMS).collect::<Vec<_>>()),
        "first_document_readiness_total": observation.documents.first().map(|document| document.readiness_blockers.len()),
    })
}

/// An exclusive, host-path-free authority over one fully resolved API 2 input closure.
struct FrozenInputAuthority {
    resources: BTreeMap<String, FrozenInputResource>,
}

struct FrozenInputResource {
    media_type: String,
    content_type: HeaderValue,
    content_address: String,
    body: RefCell<Option<Vec<u8>>>,
}

impl FrozenInputAuthority {
    fn from_resolved_job(job: ResolvedInputJob) -> Result<(Url, Self), SessionError> {
        let url_root = Url::parse(FROZEN_INPUT_URL_ROOT).map_err(|error| {
            SessionError::new(
                "INVALID_REQUEST",
                format!("cannot construct the fixed input URL root: {error}"),
            )
        })?;
        let (entrypoint, input_resources) = job.into_session_parts();
        let mut resources = BTreeMap::new();
        let mut resident_bytes = 0u64;
        for (path, resource) in input_resources {
            let url = url_root.join(&path).map_err(|error| {
                SessionError::new(
                    "INVALID_REQUEST",
                    format!("cannot map input path {path:?} into its URL authority: {error}"),
                )
            })?;
            if url.scheme() != "pliego-input" ||
                url.host_str().is_some() ||
                url.query().is_some() ||
                url.fragment().is_some() ||
                url.path().strip_prefix('/') != Some(path.as_str())
            {
                return Err(SessionError::new(
                    "INVALID_REQUEST",
                    format!("input path {path:?} escaped the fixed URL authority"),
                ));
            }

            let (media_type, content_address, declared_bytes, body) = resource.into_session_parts();
            if u64::try_from(body.len()).ok() != Some(declared_bytes) {
                return Err(SessionError::new(
                    "INVALID_REQUEST",
                    format!("input resource {path:?} changed from its declared byte count"),
                ));
            }
            let actual_content_address = format!("sha256:{}", sha256_hex(&body));
            if actual_content_address != content_address {
                return Err(SessionError::new(
                    "INVALID_REQUEST",
                    format!("input resource {path:?} changed from its declared SHA-256"),
                ));
            }
            let content_type = HeaderValue::from_str(&media_type).map_err(|error| {
                SessionError::new(
                    "INVALID_REQUEST",
                    format!("input resource {path:?} has an invalid media type: {error}"),
                )
            })?;
            resident_bytes = resident_bytes
                .checked_add(declared_bytes)
                .filter(|bytes| *bytes <= MAX_CACHE_BYTES)
                .ok_or_else(|| {
                    SessionError::new(
                        "INVALID_REQUEST",
                        format!(
                            "frozen input resources exceed the {MAX_CACHE_BYTES}-byte aggregate bound"
                        ),
                    )
                })?;
            if resources
                .insert(
                    url.to_string(),
                    FrozenInputResource {
                        media_type,
                        content_type,
                        content_address,
                        body: RefCell::new(Some(body)),
                    },
                )
                .is_some()
            {
                return Err(SessionError::new(
                    "INVALID_REQUEST",
                    format!("input path {path:?} aliases another fixed resource URL"),
                ));
            }
        }

        let input_url = url_root.join(&entrypoint).map_err(|error| {
            SessionError::new(
                "INVALID_REQUEST",
                format!("cannot map input entrypoint into its URL authority: {error}"),
            )
        })?;
        if !resources.contains_key(input_url.as_str()) {
            return Err(SessionError::new(
                "INVALID_REQUEST",
                "the frozen input authority has no declared entrypoint",
            ));
        }
        Ok((input_url, Self { resources }))
    }

    fn resolve(
        &self,
        request: &ResourceRequest,
        retained_resources: &OwnedResourceStore,
    ) -> Result<(ControlledResource, HeaderMap), ResourcePolicyFailure> {
        if request.is_redirect {
            return Err(ResourcePolicyFailure::new(
                request,
                "RESOURCE_DENIED",
                "denied",
                "redirects are disabled for the frozen input authority",
            ));
        }
        if !matches!(request.method.as_str(), "GET" | "HEAD") {
            return Err(ResourcePolicyFailure::new(
                request,
                "RESOURCE_DENIED",
                "denied",
                "only GET and HEAD frozen input requests are allowed",
            ));
        }
        let resource = self.resources.get(request.url.as_str()).ok_or_else(|| {
            ResourcePolicyFailure::new(
                request,
                "RESOURCE_DENIED",
                "denied",
                "resource is absent from the frozen pliego-input authority",
            )
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, resource.content_type.clone());
        let body = if request.method == "HEAD" {
            Vec::new()
        } else if let Some(body) = resource.body.borrow_mut().take() {
            body
        } else {
            retained_resources
                .resolve_content(&resource.content_address)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| {
                    ResourcePolicyFailure::new(
                        request,
                        "RESOURCE_CHANGED_DURING_RENDER",
                        "changed",
                        "frozen resource was not retained after ownership transfer",
                    )
                })?
        };
        Ok((
            ControlledResource {
                status: 200,
                content_type: Some(resource.media_type.clone()),
                body,
            },
            headers,
        ))
    }
}

fn owned_resource_store_for_session(
    resource_policy: &ResourcePolicy,
    frozen_input_authority: Option<&FrozenInputAuthority>,
) -> Result<OwnedResourceStore, SessionError> {
    if frozen_input_authority.is_some() && resource_policy.resident_bytes != 0 {
        return Err(SessionError::new(
            "INVALID_REQUEST",
            "frozen input authority cannot share resident host resources",
        ));
    }
    Ok(OwnedResourceStore::new(resource_policy.resident_bytes))
}

#[cfg(test)]
type ResourceEvidenceObserver = Rc<dyn Fn(&ResourceEvidence)>;

type ReadinessEvaluation = Rc<RefCell<Option<Result<JSValue, String>>>>;

#[cfg(test)]
static CONTROLLED_READINESS_EVALUATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static CONTROLLED_READINESS_CALLBACKS_DECODED: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static CONTROLLED_READINESS_FRESHNESS_SETTLEMENTS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
fn controlled_readiness_handshake_counts() -> (usize, usize, usize) {
    (
        CONTROLLED_READINESS_EVALUATIONS.load(AtomicOrdering::Relaxed),
        CONTROLLED_READINESS_CALLBACKS_DECODED.load(AtomicOrdering::Relaxed),
        CONTROLLED_READINESS_FRESHNESS_SETTLEMENTS.load(AtomicOrdering::Relaxed),
    )
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct SessionCaptureEvidence {
    pub(crate) stable_image_png: Option<Vec<u8>>,
    pub(crate) readiness: Option<serde_json::Value>,
    pub(crate) layout_debug: Option<serde_json::Value>,
    pub(crate) controlled_runtime_ms: Option<f64>,
    pub(crate) scene_capture_ms: Option<f64>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct SessionError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) resource_failure: Option<ResourcePolicyFailure>,
    pub(crate) resources: Vec<ResourceEvidence>,
    pub(crate) resource_accounting: ResourceAccounting,
    pub(crate) resource_store: OwnedResourceStore,
    pub(crate) console: Vec<(String, String)>,
    pub(crate) capture_evidence: SessionCaptureEvidence,
}

impl SessionError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            resource_failure: None,
            resources: Vec::new(),
            resource_accounting: ResourceAccounting::default(),
            resource_store: OwnedResourceStore::default(),
            console: Vec::new(),
            capture_evidence: SessionCaptureEvidence::default(),
        }
    }

    fn from_resource_failure(failure: ResourcePolicyFailure) -> Self {
        Self {
            code: failure.code.into(),
            message: format!("{}: {}", failure.reason, failure.url),
            resource_failure: Some(failure),
            resources: Vec::new(),
            resource_accounting: ResourceAccounting::default(),
            resource_store: OwnedResourceStore::default(),
            console: Vec::new(),
            capture_evidence: SessionCaptureEvidence::default(),
        }
    }

    fn with_evidence(
        mut self,
        resources: Vec<ResourceEvidence>,
        resource_store: OwnedResourceStore,
        console: Vec<(String, String)>,
    ) -> Self {
        self.resource_accounting = ResourceAccounting::from_evidence(&resources);
        // Successful/delegated rows stay in `resources`; the separate first failure is one request.
        if self.resource_failure.is_some() {
            self.resource_accounting = self.resource_accounting.with_failure();
        }
        self.resources = resources;
        self.resource_store = resource_store;
        self.console = console;
        self
    }

    fn with_capture_evidence(mut self, capture_evidence: SessionCaptureEvidence) -> Self {
        if self.capture_evidence.stable_image_png.is_none() {
            self.capture_evidence.stable_image_png = capture_evidence.stable_image_png;
        }
        if self.capture_evidence.readiness.is_none() {
            self.capture_evidence.readiness = capture_evidence.readiness;
        }
        if self.capture_evidence.layout_debug.is_none() {
            self.capture_evidence.layout_debug = capture_evidence.layout_debug;
        }
        if self.capture_evidence.controlled_runtime_ms.is_none() {
            self.capture_evidence.controlled_runtime_ms = capture_evidence.controlled_runtime_ms;
        }
        if self.capture_evidence.scene_capture_ms.is_none() {
            self.capture_evidence.scene_capture_ms = capture_evidence.scene_capture_ms;
        }
        self
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SessionError {}

pub(crate) struct DocumentOutcome {
    pub(crate) capture: SceneCapture,
    pub(crate) pdf: Vec<u8>,
    pub(crate) environment: RenderEnvironment,
    pub(crate) allow_host_fonts: bool,
    pub(crate) readiness: serde_json::Value,
    pub(crate) console: Vec<(String, String)>,
    pub(crate) resources: Vec<ResourceEvidence>,
    pub(crate) resource_accounting: ResourceAccounting,
    pub(crate) resource_store: OwnedResourceStore,
}

pub(crate) struct DocumentCaptureOutcome {
    pub(crate) capture: SceneCapture,
    pub(crate) stable_image_png: Vec<u8>,
    pub(crate) layout_debug: serde_json::Value,
    pub(crate) environment: RenderEnvironment,
    pub(crate) allow_host_fonts: bool,
    pub(crate) readiness: serde_json::Value,
    pub(crate) console: Vec<(String, String)>,
    pub(crate) resources: Vec<ResourceEvidence>,
    pub(crate) resource_accounting: ResourceAccounting,
    pub(crate) resource_store: OwnedResourceStore,
    pub(crate) controlled_runtime_ms: f64,
    pub(crate) scene_capture_ms: f64,
}

impl DocumentCaptureOutcome {
    fn render(self) -> Result<DocumentOutcome, SessionError> {
        let pdf = match self.render_pdf() {
            Ok(pdf) => pdf,
            Err(error) => {
                return Err(error
                    .with_capture_evidence(SessionCaptureEvidence {
                        stable_image_png: Some(self.stable_image_png),
                        readiness: Some(self.readiness),
                        layout_debug: Some(self.layout_debug),
                        controlled_runtime_ms: Some(self.controlled_runtime_ms),
                        scene_capture_ms: Some(self.scene_capture_ms),
                    })
                    .with_evidence(self.resources, self.resource_store, self.console));
            },
        };

        Ok(DocumentOutcome {
            capture: self.capture,
            pdf,
            environment: self.environment,
            allow_host_fonts: self.allow_host_fonts,
            readiness: self.readiness,
            console: self.console,
            resource_accounting: self.resource_accounting,
            resources: self.resources,
            resource_store: self.resource_store,
        })
    }

    fn render_pdf(&self) -> Result<Vec<u8>, SessionError> {
        if !self.capture.unsupported_events.is_empty() || !self.capture.text_mapping_gaps.is_empty()
        {
            return Err(SessionError::new(
                "SCENE_CAPTURE_INCOMPLETE",
                "captured scene contains unsupported paint or text mapping gaps",
            ));
        }

        let decoded_resources = self
            .capture
            .font_resources
            .iter()
            .map(|resource| {
                BASE64_STANDARD
                    .decode(&resource.bytes_base64)
                    .map(|bytes| (resource.resource.as_str(), bytes))
                    .map_err(|error| {
                        SessionError::new(
                            "FONT_RESOURCE_INVALID",
                            format!("cannot decode {}: {error}", resource.resource),
                        )
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let instances = self
            .capture
            .font_instances
            .iter()
            .map(|instance| (instance.id.as_str(), instance))
            .collect::<BTreeMap<_, _>>();
        let variations = self
            .capture
            .font_instances
            .iter()
            .map(|instance| {
                (
                    instance.id.as_str(),
                    instance
                        .variations
                        .iter()
                        .map(|variation| PdfFontVariation {
                            tag: variation.tag,
                            value: variation.value,
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let image_resources = self
            .capture
            .canvas_resources
            .iter()
            .chain(self.capture.embedded_image_resources.iter())
            .map(|resource| (resource.resource.as_str(), resource.png.as_slice()))
            .collect::<BTreeMap<_, _>>();
        render_document_pdf(
            &self.capture.scene,
            |font| {
                let instance = instances.get(font)?;
                Some(PdfFontResource {
                    bytes: decoded_resources.get(instance.resource.as_str())?,
                    face_index: instance.face_index,
                    variations: variations.get(font)?,
                    synthetic_bold: instance.synthetic_bold,
                })
            },
            |image| {
                image_resources
                    .get(image)
                    .copied()
                    .or_else(|| self.resource_store.resolve_content(image))
            },
        )
        .map_err(|error| SessionError::new("DOCUMENT_PDF_GENERATION_FAILED", error.to_string()))
    }
}

#[derive(Clone, Debug)]
struct ResourceEvidenceLog {
    entries: Vec<ResourceEvidence>,
    observed_events: usize,
    metadata_bytes: u64,
    max_entries: usize,
    max_metadata_bytes: u64,
}

impl Default for ResourceEvidenceLog {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            observed_events: 0,
            metadata_bytes: 0,
            max_entries: MAX_RESOURCE_EVENTS,
            max_metadata_bytes: MAX_RESOURCE_METADATA_BYTES,
        }
    }
}

impl ResourceEvidenceLog {
    fn begin_event(&mut self, request: &ResourceRequest) -> Result<(), ResourcePolicyFailure> {
        if self.observed_events < self.max_entries {
            self.observed_events += 1;
            return Ok(());
        }

        Err(ResourcePolicyFailure::new(
            request,
            "RESOURCE_METADATA_LIMIT_EXCEEDED",
            "denied",
            format!(
                "document resource loads exceed the {}-event bound",
                self.max_entries
            ),
        ))
    }

    fn push(&mut self, evidence: ResourceEvidence) -> Result<(), ResourcePolicyFailure> {
        let next_metadata_bytes = self
            .metadata_bytes
            .checked_add(evidence.metadata_bytes())
            .and_then(|bytes| bytes.checked_add(RESOURCE_EVIDENCE_ENTRY_OVERHEAD_BYTES))
            .filter(|bytes| *bytes <= self.max_metadata_bytes)
            .ok_or_else(|| {
                ResourcePolicyFailure::new(
                    &evidence.request,
                    "RESOURCE_METADATA_LIMIT_EXCEEDED",
                    "denied",
                    format!(
                        "resource evidence exceeds the {}-byte metadata bound",
                        self.max_metadata_bytes
                    ),
                )
            })?;
        self.metadata_bytes = next_metadata_bytes;
        self.entries.push(evidence);
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct ConsoleEvidenceLog {
    entries: Vec<(String, String)>,
    bytes: u64,
    limit_exceeded: bool,
}

impl ConsoleEvidenceLog {
    fn push(&mut self, level: String, message: String) {
        if self.limit_exceeded {
            return;
        }
        let next_bytes = self
            .bytes
            .saturating_add(level.len() as u64)
            .saturating_add(message.len() as u64)
            .saturating_add(CONSOLE_EVIDENCE_ENTRY_OVERHEAD_BYTES);
        if self.entries.len() >= MAX_CONSOLE_EVENTS || next_bytes > MAX_CONSOLE_BYTES {
            self.limit_exceeded = true;
            return;
        }
        self.bytes = next_bytes;
        self.entries.push((level, message));
    }
}

#[derive(Clone, Copy, Debug)]
struct SessionHostDeadline {
    started: Instant,
    deadline: Instant,
}

impl SessionHostDeadline {
    fn start(timeout: Duration) -> Result<Self, SessionError> {
        Self::from_started(Instant::now(), timeout)
    }

    fn from_started(started: Instant, timeout: Duration) -> Result<Self, SessionError> {
        let deadline = started
            .checked_add(timeout)
            .ok_or_else(|| SessionError::new("INVALID_REQUEST", "timeout is too large"))?;
        Ok(Self { started, deadline })
    }

    fn remaining_at(self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    fn is_elapsed_at(self, now: Instant) -> bool {
        self.remaining_at(now).is_zero()
    }

    fn is_elapsed(self) -> bool {
        self.is_elapsed_at(Instant::now())
    }

    fn elapsed_ms(self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    fn instant(self) -> Instant {
        self.deadline
    }
}

enum DocumentSessionRuntime {
    Realtime(ReadinessPolicy),
    Controlled {
        clock: DocumentClockConfiguration,
        waker: PliegoEventLoopWaker,
        readiness: ReadinessPolicy,
    },
}

pub(crate) struct DocumentSession {
    webview: WebView,
    // Drop the delegate-owned HTTP client before the final Servo handle shuts down its runtime.
    delegate: Rc<DocumentDelegate>,
    servo: Servo,
    environment: RenderEnvironment,
    allow_host_fonts: bool,
    host_deadline: SessionHostDeadline,
    diagnostics: Option<RefCell<SessionDiagnostics>>,
    _canvas_retention: servo_canvas::retained_canvas::CanvasRetentionGuard,
    rendering_context: Rc<SoftwareRenderingContext>,
}

/// A controlled Servo owner which can prepare one generation-bound capture candidate.
pub(crate) struct ControlledDocumentSession {
    session: DocumentSession,
    waker: PliegoEventLoopWaker,
    surface: DocumentCaptureSurfaceFingerprint,
}

/// Owner for one accepted API 2 execution and its exact normalized policy.
pub(crate) struct Api2Execution {
    controlled: ControlledDocumentSession,
}

impl Api2Execution {
    pub(crate) fn capture(self) -> Result<DocumentCaptureOutcome, SessionError> {
        self.controlled.prepare_capture_candidate()?.capture()
    }
}

/// One live session paired with its non-authoritative, generation-bound capture candidate.
pub(crate) struct PreparedDocumentCaptureCandidate {
    session: ControlledDocumentSession,
    precondition: Box<DocumentCapturePrecondition>,
    readiness: serde_json::Value,
    trace: Vec<ControlledSettlementStep>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlledSettlementStep {
    Observe,
    DriveOneTurn,
    AdvanceTo,
    PrepareCapture,
    ConsumeCapture,
    WaitForWake,
}

enum PresentationReservationProgress {
    DocumentWorkQueued,
    Reserved(DocumentPaintPresentationTicket),
}

/// Unwind-safe owner for a retained Paint ticket. Finalization consumes the ticket first, so the
/// Drop cleanup is intentionally idempotent on both success and ordinary errors.
struct PaintTicketAbortGuard<F: FnOnce()> {
    abort: Option<F>,
}

impl<F: FnOnce()> PaintTicketAbortGuard<F> {
    fn new(abort: F) -> Self {
        Self { abort: Some(abort) }
    }
}

impl<F: FnOnce()> Drop for PaintTicketAbortGuard<F> {
    fn drop(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort();
        }
    }
}

impl PreparedDocumentCaptureCandidate {
    pub(crate) fn precondition(&self) -> &DocumentCapturePrecondition {
        &self.precondition
    }

    pub(crate) fn trace(&self) -> &[ControlledSettlementStep] {
        &self.trace
    }

    /// Consume this candidate through Paint reservation, Script revalidation, and exact readback.
    pub(crate) fn capture(self) -> Result<DocumentCaptureOutcome, SessionError> {
        self.capture_with_canvas_freezer_and_hooks(
            |keys, generation| {
                servo_canvas::retained_canvas::freeze_canvas_snapshots_at_generation(
                    keys, generation,
                )
            },
            |_| {},
            |_, _| {},
        )
    }

    #[cfg(test)]
    pub(crate) fn capture_with_paint_hook(
        self,
        after_reservation: impl FnOnce(&WebView, &DocumentPaintPresentationTicket),
    ) -> Result<DocumentCaptureOutcome, SessionError> {
        self.capture_with_canvas_freezer_and_hooks(
            |keys, generation| {
                servo_canvas::retained_canvas::freeze_canvas_snapshots_at_generation(
                    keys, generation,
                )
            },
            |_| {},
            after_reservation,
        )
    }

    #[cfg(test)]
    pub(crate) fn capture_with_document_work_queued_hook(
        self,
        on_document_work_queued: impl FnOnce(&WebView),
    ) -> Result<DocumentCaptureOutcome, SessionError> {
        self.capture_with_canvas_freezer_and_hooks(
            |keys, generation| {
                servo_canvas::retained_canvas::freeze_canvas_snapshots_at_generation(
                    keys, generation,
                )
            },
            on_document_work_queued,
            |_, _| {},
        )
    }

    fn capture_with_canvas_freezer_and_hooks(
        self,
        freeze_canvas: impl FnOnce(
            &[(u32, u32)],
            u64,
        ) -> Result<
            servo_canvas::retained_canvas::FrozenCanvasSnapshots,
            servo_canvas::retained_canvas::FreezeCanvasSnapshotsError,
        >,
        on_document_work_queued: impl FnOnce(&WebView),
        after_reservation: impl FnOnce(&WebView, &DocumentPaintPresentationTicket),
    ) -> Result<DocumentCaptureOutcome, SessionError> {
        let result = self.capture_inner(
            freeze_canvas,
            Some(on_document_work_queued),
            after_reservation,
        );
        result.map_err(|error| self.session.enrich_error_evidence(error))
    }

    fn capture_inner<F>(
        &self,
        freeze_canvas: impl FnOnce(
            &[(u32, u32)],
            u64,
        ) -> Result<
            servo_canvas::retained_canvas::FrozenCanvasSnapshots,
            servo_canvas::retained_canvas::FreezeCanvasSnapshotsError,
        >,
        mut on_document_work_queued: Option<F>,
        after_reservation: impl FnOnce(&WebView, &DocumentPaintPresentationTicket),
    ) -> Result<DocumentCaptureOutcome, SessionError>
    where
        F: FnOnce(&WebView),
    {
        let mut capture_evidence = SessionCaptureEvidence {
            readiness: Some(self.readiness.clone()),
            ..Default::default()
        };
        let mut precondition = self.precondition.clone();
        let ticket = loop {
            let reservation =
                self.session
                    .reserve_presentation(&precondition)
                    .map_err(|error| {
                        with_current_readiness_evidence(error, capture_evidence.readiness.as_ref())
                    })?;
            match reservation {
                PresentationReservationProgress::DocumentWorkQueued => {
                    if let Some(hook) = on_document_work_queued.take() {
                        hook(&self.session.session.webview);
                    }
                    // A replacement handshake may discover a new target. Preserve only evidence
                    // authored by that handshake on failure; the prior target's readiness is not
                    // valid fallback evidence for it.
                    let (next, readiness, _) = self.session.prepare_ready_capture_candidate()?;
                    precondition = next;
                    capture_evidence.readiness = Some(readiness);
                },
                PresentationReservationProgress::Reserved(ticket) => break ticket,
            }
        };
        let _ticket_abort_guard = PaintTicketAbortGuard::new(|| {
            self.session
                .session
                .webview
                .abort_controlled_document_capture(ticket.id());
        });
        after_reservation(&self.session.session.webview, &ticket);
        self.consume_and_capture(&precondition, &ticket, freeze_canvas, &mut capture_evidence)
            .map_err(|error| error.with_capture_evidence(capture_evidence))
    }

    fn consume_and_capture(
        &self,
        precondition: &DocumentCapturePrecondition,
        ticket: &DocumentPaintPresentationTicket,
        freeze_canvas: impl FnOnce(
            &[(u32, u32)],
            u64,
        ) -> Result<
            servo_canvas::retained_canvas::FrozenCanvasSnapshots,
            servo_canvas::retained_canvas::FreezeCanvasSnapshotsError,
        >,
        capture_evidence: &mut SessionCaptureEvidence,
    ) -> Result<DocumentCaptureOutcome, SessionError> {
        self.session
            .session
            .check_failure("controlled capture consume")?;
        if self.session.session.host_deadline.is_elapsed() {
            return Err(SessionError::new(
                "CONTROLLED_CAPTURE_TIMEOUT",
                "controlled capture exceeded the normalized host-wall limit",
            ));
        }

        let command = DocumentTimeControlCommand::ConsumeCapture(Box::new(
            DocumentCaptureConsumeRequest::new_internal(
                Box::new(precondition.clone()),
                ticket.clone(),
            ),
        ));
        self.session
            .session
            .diagnostic(|diagnostics| diagnostics.command(&command));
        let response_waker = self.session.waker.clone();
        let receiver = self
            .session
            .session
            .webview
            .request_controlled_document_time_notifying(command, move || {
                response_waker.notify_control_response();
            })
            .map_err(|error| {
                SessionError::new(
                    "CONTROLLED_CAPTURE_CONSUME_FAILED",
                    format!("cannot submit the single-use capture consume: {error:?}"),
                )
            })?;
        let (outcome, _) = self.session.await_control_outcome(receiver)?;
        let commit = exact_capture_commit(outcome, precondition, ticket)?;
        let canvas_binding = precondition
            .sources()
            .canvas_capture_binding()
            .map_err(|error| {
                SessionError::new(
                    "CONTROLLED_CANVAS_BINDING_INVALID",
                    format!("consumed capture candidate has an invalid Canvas binding: {error:?}"),
                )
            })?;

        let screenshot = self
            .session
            .session
            .webview
            .finalize_controlled_document_capture(ticket, &commit)
            .map_err(|error| {
                SessionError::new(
                    "CONTROLLED_PAINT_FINALIZE_FAILED",
                    format!("Paint rejected the single-use presentation ticket: {error:?}"),
                )
            })?;
        let mut stable_image_png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(screenshot)
            .write_to(&mut stable_image_png, image::ImageFormat::Png)
            .map_err(|error| {
                SessionError::new(
                    "STABLE_RENDER_ENCODE_FAILED",
                    format!("cannot encode the generation-bound Servo frame: {error}"),
                )
            })?;
        capture_evidence.stable_image_png = Some(stable_image_png.into_inner());

        let snapshot = commit.serialized_layout_snapshot();
        let layout_debug = serde_json::from_str(snapshot).map_err(|error| {
            SessionError::new("SCENE_CAPTURE_LAYOUT_JSON_INVALID", error.to_string())
                .with_capture_evidence(std::mem::take(capture_evidence))
        })?;
        capture_evidence.layout_debug = Some(layout_debug);
        capture_evidence.controlled_runtime_ms =
            Some(self.session.session.host_deadline.elapsed_ms());

        let scene_capture_started = Instant::now();
        let capture = {
            let resources = self.session.session.delegate.resource_store.borrow();
            capture_controlled_document_scene_with_canvas(
                snapshot.as_bytes(),
                |url| resources.resolve_url(url),
                canvas_binding.as_ref(),
                freeze_canvas,
            )
        }
        .map_err(|error| {
            SessionError::new("SCENE_CAPTURE_FAILED", error.to_string())
                .with_capture_evidence(std::mem::take(capture_evidence))
        })?;
        capture_evidence.scene_capture_ms =
            Some(scene_capture_started.elapsed().as_secs_f64() * 1000.0);
        if let Err(message) = capture.scene.validate() {
            return Err(SessionError::new("SCENE_CAPTURE_INVALID", message)
                .with_capture_evidence(std::mem::take(capture_evidence)));
        }
        if let Err(error) =
            validate_host_font_policy(&capture, self.session.session.allow_host_fonts)
        {
            return Err(error.with_capture_evidence(std::mem::take(capture_evidence)));
        }
        if let Err(error) = self
            .session
            .session
            .check_failure("controlled document capture")
        {
            return Err(error.with_capture_evidence(std::mem::take(capture_evidence)));
        }

        let SessionCaptureEvidence {
            stable_image_png: Some(stable_image_png),
            readiness: Some(readiness),
            layout_debug: Some(layout_debug),
            controlled_runtime_ms: Some(controlled_runtime_ms),
            scene_capture_ms: Some(scene_capture_ms),
        } = std::mem::take(capture_evidence)
        else {
            unreachable!("completed controlled capture has complete staged evidence")
        };
        let resources =
            std::mem::take(&mut self.session.session.delegate.resources.borrow_mut().entries);
        let resource_store = self.session.session.delegate.resource_store.take();
        let console =
            std::mem::take(&mut self.session.session.delegate.console.borrow_mut().entries);
        Ok(DocumentCaptureOutcome {
            capture,
            stable_image_png,
            layout_debug,
            environment: self.session.session.environment,
            allow_host_fonts: self.session.session.allow_host_fonts,
            readiness,
            console,
            resource_accounting: ResourceAccounting::from_evidence(&resources),
            resources,
            resource_store,
            controlled_runtime_ms,
            scene_capture_ms,
        })
    }
}

impl DocumentSession {
    fn diagnostic(&self, update: impl FnOnce(&mut SessionDiagnostics)) {
        if let Some(diagnostics) = &self.diagnostics {
            update(&mut diagnostics.borrow_mut());
        }
    }

    fn emit_diagnostic_failure(&self, error: &SessionError) {
        self.diagnostic(|diagnostics| {
            diagnostics.emit_failure(self.host_deadline, error, Some(&self.delegate));
        });
    }

    pub(crate) fn new(
        input: impl AsRef<Path>,
        environment: RenderEnvironment,
        page: PageDefinition,
        resources: ResourcePolicyConfig,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
    ) -> Result<Self, SessionError> {
        Self::new_with_canvas_retention(
            input,
            environment,
            page,
            resources,
            allow_host_fonts,
            readiness,
            servo_canvas::retained_canvas::start_retaining_canvas_commands,
        )
    }

    pub(crate) fn from_resolved(
        document: &LocalDocument,
        resource_policy: ResourcePolicy,
        environment: RenderEnvironment,
        page: PageDefinition,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
    ) -> Result<Self, SessionError> {
        validate_resolved_resource_policy(document, &resource_policy)?;
        let session_host_timeout =
            validate_session_timeouts(readiness, resource_policy.timeout_ms)?;
        Self::new_resolved_with_canvas_retention(
            document.path().to_owned(),
            document.root().to_owned(),
            resource_policy,
            environment,
            page,
            allow_host_fonts,
            DocumentSessionRuntime::Realtime(readiness),
            session_host_timeout,
            servo_canvas::retained_canvas::start_retaining_canvas_commands,
        )
    }

    /// Build an opt-in controlled session with the API 1 readiness bootstrap script.
    pub(crate) fn new_controlled(
        input: impl AsRef<Path>,
        environment: RenderEnvironment,
        page: PageDefinition,
        resources: ResourcePolicyConfig,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
        runtime_policy: DeterministicRuntimePolicy,
    ) -> Result<ControlledDocumentSession, SessionError> {
        let input = input.as_ref().canonicalize().map_err(|error| {
            SessionError::new(
                "INVALID_REQUEST",
                format!("document is unavailable: {error}"),
            )
        })?;
        if !input.is_file() {
            return Err(SessionError::new(
                "INVALID_REQUEST",
                format!("document is not a file: {}", input.display()),
            ));
        }
        let bundle_root = input
            .parent()
            .ok_or_else(|| SessionError::new("INVALID_REQUEST", "document has no bundle root"))?
            .to_path_buf();
        let resource_policy = ResourcePolicy::resolve(&resources, &bundle_root);
        Self::new_resolved_controlled_parts(
            input,
            bundle_root,
            resource_policy,
            environment,
            page,
            allow_host_fonts,
            readiness,
            runtime_policy,
        )
    }

    pub(crate) fn from_resolved_controlled(
        document: &LocalDocument,
        resource_policy: ResourcePolicy,
        environment: RenderEnvironment,
        page: PageDefinition,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
        runtime_policy: DeterministicRuntimePolicy,
    ) -> Result<ControlledDocumentSession, SessionError> {
        validate_resolved_resource_policy(document, &resource_policy)?;
        Self::new_resolved_controlled_parts(
            document.path().to_owned(),
            document.root().to_owned(),
            resource_policy,
            environment,
            page,
            allow_host_fonts,
            readiness,
            runtime_policy,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_resolved_controlled_parts(
        input: PathBuf,
        bundle_root: PathBuf,
        resource_policy: ResourcePolicy,
        environment: RenderEnvironment,
        page: PageDefinition,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
        runtime_policy: DeterministicRuntimePolicy,
    ) -> Result<ControlledDocumentSession, SessionError> {
        validate_resource_timeout(resource_policy.timeout_ms)?;
        let runtime_policy = runtime_policy
            .validate()
            .map_err(|error| SessionError::new("INVALID_REQUEST", error.to_string()))?;
        let host_timeout = runtime_policy.host_wall_duration();
        let clock = runtime_policy
            .document_clock_configuration()
            .map_err(|error| SessionError::new("INVALID_REQUEST", error.to_string()))?;
        let waker = PliegoEventLoopWaker::new();
        let session = Self::new_resolved_with_canvas_retention(
            input,
            bundle_root,
            resource_policy,
            environment,
            page,
            allow_host_fonts,
            DocumentSessionRuntime::Controlled {
                clock,
                waker: waker.clone(),
                readiness,
            },
            host_timeout,
            servo_canvas::retained_canvas::start_retaining_canvas_commands,
        )?;
        let surface = session.controlled_capture_surface()?;
        Ok(ControlledDocumentSession {
            session,
            waker,
            surface,
        })
    }

    #[cfg(test)]
    fn new_with_canvas_retention_limits(
        input: impl AsRef<Path>,
        environment: RenderEnvironment,
        page: PageDefinition,
        resources: ResourcePolicyConfig,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
        canvas_retention_limits: (u64, u64, u64),
    ) -> Result<Self, SessionError> {
        let (max_commands, max_raster_bytes, max_objects) = canvas_retention_limits;
        Self::new_with_canvas_retention(
            input,
            environment,
            page,
            resources,
            allow_host_fonts,
            readiness,
            || {
                servo_canvas::retained_canvas::start_retaining_canvas_commands_for_testing(
                    max_commands,
                    max_raster_bytes,
                    max_objects,
                )
            },
        )
    }

    #[cfg(test)]
    fn set_resource_evidence_observer(&self, observer: ResourceEvidenceObserver) {
        let previous = self
            .delegate
            .resource_evidence_observer
            .borrow_mut()
            .replace(observer);
        assert!(
            previous.is_none(),
            "resource evidence observer was already set"
        );
    }

    fn new_with_canvas_retention(
        input: impl AsRef<Path>,
        environment: RenderEnvironment,
        page: PageDefinition,
        resources: ResourcePolicyConfig,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
        start_canvas_retention: impl FnOnce() -> servo_canvas::retained_canvas::CanvasRetentionGuard,
    ) -> Result<Self, SessionError> {
        let session_host_timeout = validate_session_timeouts(readiness, resources.timeout_ms)?;
        let input = input.as_ref().canonicalize().map_err(|error| {
            SessionError::new(
                "INVALID_REQUEST",
                format!("document is unavailable: {error}"),
            )
        })?;
        if !input.is_file() {
            return Err(SessionError::new(
                "INVALID_REQUEST",
                format!("document is not a file: {}", input.display()),
            ));
        }
        let bundle_root = input
            .parent()
            .ok_or_else(|| SessionError::new("INVALID_REQUEST", "document has no bundle root"))?
            .to_path_buf();
        let resource_policy = ResourcePolicy::resolve(&resources, &bundle_root);
        Self::new_resolved_with_canvas_retention(
            input,
            bundle_root,
            resource_policy,
            environment,
            page,
            allow_host_fonts,
            DocumentSessionRuntime::Realtime(readiness),
            session_host_timeout,
            start_canvas_retention,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_resolved_with_canvas_retention(
        input: PathBuf,
        bundle_root: PathBuf,
        resource_policy: ResourcePolicy,
        environment: RenderEnvironment,
        page: PageDefinition,
        allow_host_fonts: bool,
        runtime: DocumentSessionRuntime,
        session_host_timeout: Duration,
        start_canvas_retention: impl FnOnce() -> servo_canvas::retained_canvas::CanvasRetentionGuard,
    ) -> Result<Self, SessionError> {
        validate_resource_policy(&resource_policy)?;
        let input_url = Url::from_file_path(&input).map_err(|_| {
            SessionError::new(
                "INVALID_REQUEST",
                format!(
                    "cannot convert document path to a file URL: {}",
                    input.display()
                ),
            )
        })?;
        Self::new_validated_url_with_canvas_retention(
            input_url,
            bundle_root,
            resource_policy,
            None,
            environment,
            page,
            allow_host_fonts,
            runtime,
            session_host_timeout,
            start_canvas_retention,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_validated_url_with_canvas_retention(
        input_url: Url,
        bundle_root: PathBuf,
        resource_policy: ResourcePolicy,
        frozen_input_authority: Option<FrozenInputAuthority>,
        environment: RenderEnvironment,
        page: PageDefinition,
        allow_host_fonts: bool,
        runtime: DocumentSessionRuntime,
        session_host_timeout: Duration,
        start_canvas_retention: impl FnOnce() -> servo_canvas::retained_canvas::CanvasRetentionGuard,
    ) -> Result<Self, SessionError> {
        let surface_size = page.surface_pixel_size().ok_or_else(|| {
            SessionError::new(
                "RENDER_CONTEXT_FAILED",
                "page dimensions cannot be represented by the software rendering surface",
            )
        })?;
        let page_reservation = reserve_for_process(page).map_err(|_| {
            SessionError::new(
                "LAYOUT_CONFIGURATION_FAILED",
                "paged layout was already configured for this process",
            )
        })?;
        apply_timezone(environment.timezone)
            .map_err(|error| SessionError::new("ENVIRONMENT_CONFIGURATION_FAILED", error))?;
        let host_deadline = SessionHostDeadline::start(session_host_timeout)?;
        let diagnostics = SessionDiagnostics::enabled(
            std::env::var_os(SESSION_DIAGNOSTICS_ENV).as_deref(),
            matches!(&runtime, DocumentSessionRuntime::Controlled { .. }),
        )
        .then(|| {
            RefCell::new(SessionDiagnostics {
                phase: "startup-render-context",
                ..Default::default()
            })
        });
        let startup_error = |error: SessionError| {
            if let Some(diagnostics) = &diagnostics {
                diagnostics
                    .borrow_mut()
                    .emit_failure(host_deadline, &error, None);
            }
            error
        };

        let rendering_context = Rc::new(
            SoftwareRenderingContext::new(PhysicalSize::new(
                surface_size.width,
                surface_size.height,
            ))
            .map_err(|error| {
                startup_error(SessionError::new(
                    "RENDER_CONTEXT_FAILED",
                    format!("cannot create software rendering context: {error:?}"),
                ))
            })?,
        );
        rendering_context.make_current().map_err(|error| {
            startup_error(SessionError::new(
                "RENDER_CONTEXT_FAILED",
                format!("cannot activate software rendering context: {error:?}"),
            ))
        })?;
        page_reservation.commit().map_err(|_| {
            startup_error(SessionError::new(
                "LAYOUT_CONFIGURATION_FAILED",
                "paged layout was already configured for this process",
            ))
        })?;
        if let Some(diagnostics) = &diagnostics {
            let mut diagnostics = diagnostics.borrow_mut();
            diagnostics.startup_ms[0] = Some(host_deadline.elapsed_ms());
            diagnostics.phase = "startup-servo";
        }

        let mut preferences = Preferences::default();
        preferences.fonts_host_enabled = allow_host_fonts;
        preferences.intl_locale_override = environment.locale.into();
        preferences.network_http_proxy_uri.clear();
        preferences.network_https_proxy_uri.clear();

        let (servo_builder, readiness, document_clock) = match runtime {
            DocumentSessionRuntime::Realtime(readiness) => {
                (ServoBuilder::default(), Some(readiness), None)
            },
            DocumentSessionRuntime::Controlled {
                clock,
                waker,
                readiness,
            } => (
                ServoBuilder::default().event_loop_waker(Box::new(waker)),
                Some(readiness),
                Some(clock),
            ),
        };
        let servo = servo_builder.preferences(preferences).build();
        if let Some(diagnostics) = &diagnostics {
            let mut diagnostics = diagnostics.borrow_mut();
            diagnostics.startup_ms[1] = Some(host_deadline.elapsed_ms());
            diagnostics.phase = "startup-webview";
        }
        let resource_store = RefCell::new(
            owned_resource_store_for_session(&resource_policy, frozen_input_authority.as_ref())
                .map_err(startup_error)?,
        );
        let delegate = Rc::new(DocumentDelegate {
            bundle_root,
            resource_policy,
            frozen_input_authority,
            resource_store,
            paint_frames_automatically: document_clock.is_none(),
            ..Default::default()
        });
        let canvas_retention = start_canvas_retention();
        let mut webview_builder = WebViewBuilder::new(&servo, rendering_context.clone())
            .delegate(delegate.clone())
            .url(input_url);
        if let Some(readiness) = readiness {
            let user_content_manager = Rc::new(UserContentManager::new(&servo));
            user_content_manager
                .add_script(Rc::new(UserScript::from(readiness.document_start_script())));
            webview_builder = webview_builder.user_content_manager(user_content_manager);
        }
        if let Some(document_clock) = document_clock {
            webview_builder = webview_builder.document_clock(document_clock);
        }
        let webview = webview_builder.build();
        if let Some(diagnostics) = &diagnostics {
            let mut diagnostics = diagnostics.borrow_mut();
            diagnostics.startup_ms[2] = Some(host_deadline.elapsed_ms());
            diagnostics.phase = "webview-ready";
        }

        Ok(Self {
            webview,
            servo,
            delegate,
            environment,
            allow_host_fonts,
            host_deadline,
            diagnostics,
            _canvas_retention: canvas_retention,
            rendering_context,
        })
    }

    pub(crate) fn start_api2_execution(
        job: ResolvedRenderJob,
    ) -> Result<Api2Execution, SessionError> {
        let crate::api2::ResolvedRenderJobParts {
            input,
            environment,
            page,
            resources,
            allow_host_fonts,
            runtime_policy,
        } = job.into_parts();
        let (input_url, authority) = FrozenInputAuthority::from_resolved_job(input)?;
        let runtime_policy = runtime_policy
            .validate()
            .map_err(|error| SessionError::new("INVALID_REQUEST", error.to_string()))?;
        let host_timeout = runtime_policy.host_wall_duration();
        let clock = runtime_policy
            .document_clock_configuration()
            .map_err(|error| SessionError::new("INVALID_REQUEST", error.to_string()))?;
        let waker = PliegoEventLoopWaker::new();
        let session = Self::new_validated_url_with_canvas_retention(
            input_url,
            PathBuf::new(),
            resources,
            Some(authority),
            environment,
            page,
            allow_host_fonts,
            DocumentSessionRuntime::Controlled {
                clock,
                waker: waker.clone(),
                // Legacy readiness is only a private bridge for the controlled fixture. It is not
                // accepted from an API 2 caller and must not become part of contract activation.
                readiness: ReadinessPolicy {
                    timeout_ms: 1_000,
                    wait_for_fonts: false,
                },
            },
            host_timeout,
            servo_canvas::retained_canvas::start_retaining_canvas_commands,
        )?;
        let surface = session.controlled_capture_surface().map_err(|error| {
            session.emit_diagnostic_failure(&error);
            error
        })?;
        Ok(Api2Execution {
            controlled: ControlledDocumentSession {
                session,
                waker,
                surface,
            },
        })
    }

    fn controlled_capture_surface(
        &self,
    ) -> Result<DocumentCaptureSurfaceFingerprint, SessionError> {
        let device_pixel_scale = self.webview.hidpi_scale_factor().get();
        if device_pixel_scale.to_bits() != 1.0_f32.to_bits() {
            return Err(SessionError::new(
                "CAPTURE_SURFACE_INVALID",
                "controlled candidate currently requires an exact 1.0 device-pixel scale",
            ));
        }
        let size = self.rendering_context.size2d();
        let width = i32::try_from(size.width).map_err(|_| {
            SessionError::new(
                "CAPTURE_SURFACE_INVALID",
                "capture surface width is not representable",
            )
        })?;
        let height = i32::try_from(size.height).map_err(|_| {
            SessionError::new(
                "CAPTURE_SURFACE_INVALID",
                "capture surface height is not representable",
            )
        })?;
        let viewport = DeviceIndependentIntSize::new(width, height);
        let capture_rect = DeviceIndependentIntRect::new(
            DeviceIndependentIntPoint::new(0, 0),
            DeviceIndependentIntPoint::new(width, height),
        );
        DocumentCaptureSurfaceFingerprint::new(viewport, capture_rect, device_pixel_scale).map_err(
            |error| {
                SessionError::new(
                    "CAPTURE_SURFACE_INVALID",
                    format!("capture surface is invalid: {error:?}"),
                )
            },
        )
    }

    pub(crate) fn render(self) -> Result<DocumentOutcome, SessionError> {
        self.render_with_canvas_freezer(|keys| {
            servo_canvas::retained_canvas::freeze_canvas_snapshots(keys)
        })
    }

    pub(crate) fn capture(self) -> Result<DocumentCaptureOutcome, SessionError> {
        self.capture_with_canvas_freezer(|keys| {
            servo_canvas::retained_canvas::freeze_canvas_snapshots(keys)
        })
    }

    fn render_with_canvas_freezer(
        self,
        freeze_canvas: impl FnOnce(
            &[(u32, u32)],
        ) -> Result<
            servo_canvas::retained_canvas::FrozenCanvasSnapshots,
            servo_canvas::retained_canvas::FreezeCanvasSnapshotsError,
        >,
    ) -> Result<DocumentOutcome, SessionError> {
        self.capture_with_canvas_freezer(freeze_canvas)?.render()
    }

    fn capture_with_canvas_freezer(
        self,
        freeze_canvas: impl FnOnce(
            &[(u32, u32)],
        ) -> Result<
            servo_canvas::retained_canvas::FrozenCanvasSnapshots,
            servo_canvas::retained_canvas::FreezeCanvasSnapshotsError,
        >,
    ) -> Result<DocumentCaptureOutcome, SessionError> {
        self.capture_inner(freeze_canvas).map_err(|mut error| {
            error
                .capture_evidence
                .controlled_runtime_ms
                .get_or_insert(self.host_deadline.elapsed_ms());
            error.with_evidence(
                std::mem::take(&mut self.delegate.resources.borrow_mut().entries),
                self.delegate.resource_store.take(),
                std::mem::take(&mut self.delegate.console.borrow_mut().entries),
            )
        })
    }

    fn capture_inner(
        &self,
        freeze_canvas: impl FnOnce(
            &[(u32, u32)],
        ) -> Result<
            servo_canvas::retained_canvas::FrozenCanvasSnapshots,
            servo_canvas::retained_canvas::FreezeCanvasSnapshotsError,
        >,
    ) -> Result<DocumentCaptureOutcome, SessionError> {
        let mut capture_evidence = SessionCaptureEvidence::default();
        self.webview.show();
        self.spin_until_host_deadline("document load", || self.delegate.load_complete.get())?;

        let screenshot = Rc::new(RefCell::new(None));
        let screenshot_result = screenshot.clone();
        self.webview.take_screenshot(None, move |result| {
            *screenshot_result.borrow_mut() = Some(result.map_err(|error| format!("{error:?}")));
        });
        self.spin_until_host_deadline("stable render", || screenshot.borrow().is_some())?;
        let screenshot = screenshot
            .borrow_mut()
            .take()
            .ok_or_else(|| {
                SessionError::new(
                    "STABLE_RENDER_FAILED",
                    "stable-render callback completed without a result",
                )
            })?
            .map_err(|message| SessionError::new("STABLE_RENDER_FAILED", message))?;
        let mut stable_image_png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(screenshot)
            .write_to(&mut stable_image_png, image::ImageFormat::Png)
            .map_err(|error| {
                SessionError::new(
                    "STABLE_RENDER_ENCODE_FAILED",
                    format!("cannot encode the stable Servo frame: {error}"),
                )
            })?;
        capture_evidence.stable_image_png = Some(stable_image_png.into_inner());
        if !self.delegate.frame_ready.get() {
            return Err(SessionError::new(
                "STABLE_RENDER_FAILED",
                "Servo completed the barrier without producing a frame",
            )
            .with_capture_evidence(capture_evidence));
        }
        let readiness = match self.evaluate_readiness() {
            Ok(readiness) => readiness,
            Err(error) => return Err(error.with_capture_evidence(capture_evidence)),
        };
        capture_evidence.readiness = Some(readiness);
        let snapshot = match self.webview.debug_layout_snapshot() {
            Some(snapshot) => snapshot,
            None => {
                return Err(SessionError::new(
                    "SCENE_CAPTURE_UNAVAILABLE",
                    "Servo did not expose a retained layout snapshot",
                )
                .with_capture_evidence(capture_evidence));
            },
        };
        let layout_debug = match serde_json::from_str(&snapshot) {
            Ok(layout_debug) => layout_debug,
            Err(error) => {
                return Err(SessionError::new(
                    "SCENE_CAPTURE_LAYOUT_JSON_INVALID",
                    error.to_string(),
                )
                .with_capture_evidence(capture_evidence));
            },
        };
        capture_evidence.layout_debug = Some(layout_debug);
        capture_evidence.controlled_runtime_ms = Some(self.host_deadline.elapsed_ms());
        let scene_capture_started = Instant::now();
        let capture = {
            let resources = self.delegate.resource_store.borrow();
            capture_document_scene_with_canvas(
                snapshot.as_bytes(),
                |url| resources.resolve_url(url),
                freeze_canvas,
            )
        };
        capture_evidence.scene_capture_ms =
            Some(scene_capture_started.elapsed().as_secs_f64() * 1000.0);
        let capture = match capture {
            Ok(capture) => capture,
            Err(error) => {
                return Err(SessionError::new("SCENE_CAPTURE_FAILED", error.to_string())
                    .with_capture_evidence(capture_evidence));
            },
        };
        if let Err(message) = capture.scene.validate() {
            return Err(SessionError::new("SCENE_CAPTURE_INVALID", message)
                .with_capture_evidence(capture_evidence));
        }
        if let Err(error) = validate_host_font_policy(&capture, self.allow_host_fonts) {
            return Err(error.with_capture_evidence(capture_evidence));
        }
        if let Err(error) = self.check_failure("document capture") {
            return Err(error.with_capture_evidence(capture_evidence));
        }

        let SessionCaptureEvidence {
            stable_image_png: Some(stable_image_png),
            readiness: Some(readiness),
            layout_debug: Some(layout_debug),
            controlled_runtime_ms: Some(controlled_runtime_ms),
            scene_capture_ms: Some(scene_capture_ms),
        } = capture_evidence
        else {
            unreachable!("completed capture has complete staged evidence")
        };

        let resources = std::mem::take(&mut self.delegate.resources.borrow_mut().entries);
        let resource_store = self.delegate.resource_store.take();
        let console = std::mem::take(&mut self.delegate.console.borrow_mut().entries);
        Ok(DocumentCaptureOutcome {
            capture,
            stable_image_png,
            layout_debug,
            environment: self.environment,
            allow_host_fonts: self.allow_host_fonts,
            readiness,
            console,
            resource_accounting: ResourceAccounting::from_evidence(&resources),
            resources,
            resource_store,
            controlled_runtime_ms,
            scene_capture_ms,
        })
    }

    fn evaluate_readiness(&self) -> Result<serde_json::Value, SessionError> {
        loop {
            let evaluation = self.begin_readiness_evaluation();
            let value = self.await_readiness_evaluation(evaluation)?;
            let (evidence, state) = decode_readiness_evaluation(value)?;
            match state {
                Readiness::Ready { .. } => return Ok(evidence),
                Readiness::Failed { error } => {
                    return Err(SessionError::new(error.code, error.message)
                        .with_capture_evidence(SessionCaptureEvidence {
                            readiness: Some(evidence),
                            ..Default::default()
                        }));
                },
                Readiness::Pending if self.host_deadline.is_elapsed() => {
                    return Err(SessionError::new(
                        "READINESS_TIMEOUT",
                        "document readiness did not settle before the host deadline",
                    )
                    .with_capture_evidence(SessionCaptureEvidence {
                        readiness: Some(evidence),
                        ..Default::default()
                    }));
                },
                Readiness::Pending => {
                    self.servo.spin_event_loop();
                    std::thread::sleep(Duration::from_millis(1));
                },
            }
        }
    }

    fn begin_readiness_evaluation(&self) -> ReadinessEvaluation {
        let result = Rc::new(RefCell::new(None));
        let callback_result = result.clone();
        self.webview
            .evaluate_javascript(readiness::HOST_EVALUATION_EXPRESSION, move |value| {
                *callback_result.borrow_mut() = Some(value.map_err(|error| format!("{error:?}")));
            });
        result
    }

    fn await_readiness_evaluation(
        &self,
        result: ReadinessEvaluation,
    ) -> Result<Result<JSValue, String>, SessionError> {
        self.spin_until_host_deadline("readiness evaluation", || result.borrow().is_some())?;
        result.borrow_mut().take().ok_or_else(|| {
            SessionError::new(
                "READINESS_EVALUATION_FAILED",
                "readiness callback completed without a result",
            )
        })
    }

    fn spin_until_host_deadline(
        &self,
        label: &str,
        done: impl Fn() -> bool,
    ) -> Result<(), SessionError> {
        loop {
            // Preserve specific terminal failures, but never accept a callback after the hard
            // session deadline: one slow event-loop turn must not extend the aggregate budget.
            self.check_failure(label)?;
            if self.host_deadline.is_elapsed() {
                return Err(SessionError::new(
                    "RENDER_TIMEOUT",
                    format!("timed out waiting for {label}"),
                ));
            }
            if done() {
                return Ok(());
            }
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn check_failure(&self, label: &str) -> Result<(), SessionError> {
        if let Some(reason) = self.delegate.crashed.borrow().as_deref() {
            return Err(SessionError::new(
                "SERVO_CRASHED",
                format!("Servo crashed while waiting for {label}: {reason}"),
            ));
        }
        if let Some(reason) = self.delegate.resource_failure.borrow().as_ref() {
            return Err(SessionError::from_resource_failure(reason.clone()));
        }
        if self.delegate.console.borrow().limit_exceeded {
            return Err(SessionError::new(
                "CONSOLE_OUTPUT_LIMIT_EXCEEDED",
                format!(
                    "document console exceeds the {MAX_CONSOLE_EVENTS}-event or {MAX_CONSOLE_BYTES}-byte evidence bound"
                ),
            ));
        }
        Ok(())
    }
}

impl ControlledDocumentSession {
    fn settlement_timeout(&self, site: &'static str) -> SessionError {
        SessionDiagnostics::settlement_timeout(self.session.diagnostics.as_ref(), site)
    }

    fn enrich_error_evidence(&self, mut error: SessionError) -> SessionError {
        error
            .capture_evidence
            .controlled_runtime_ms
            .get_or_insert(self.session.host_deadline.elapsed_ms());
        self.session.emit_diagnostic_failure(&error);
        error.with_evidence(
            std::mem::take(&mut self.session.delegate.resources.borrow_mut().entries),
            self.session.delegate.resource_store.take(),
            std::mem::take(&mut self.session.delegate.console.borrow_mut().entries),
        )
    }

    fn reserve_presentation(
        &self,
        precondition: &DocumentCapturePrecondition,
    ) -> Result<PresentationReservationProgress, SessionError> {
        self.session.check_failure("controlled Paint reservation")?;
        if self.session.host_deadline.is_elapsed() {
            return Err(SessionError::new(
                "CONTROLLED_CAPTURE_TIMEOUT",
                "controlled Paint reservation exceeded the normalized host-wall limit",
            ));
        }

        match self
            .session
            .webview
            .reserve_controlled_document_capture(precondition)
        {
            Ok(ControlledDocumentCaptureReservation::Reserved(ticket)) => {
                Ok(PresentationReservationProgress::Reserved(ticket))
            },
            Ok(ControlledDocumentCaptureReservation::DocumentWorkQueued) => {
                Ok(PresentationReservationProgress::DocumentWorkQueued)
            },
            Err(ControlledDocumentCaptureError::Terminal(failure)) => Err(SessionError::new(
                "CONTROLLED_PAINT_RESERVATION_FAILED",
                format!("Paint rejected the capture candidate: {failure:?}"),
            )),
            Err(ControlledDocumentCaptureError::Retryable(
                ControlledDocumentCaptureRetry::FramePending,
            )) => {
                // Any owner-thread progress can also deliver Script input. Make that progress once,
                // then require a complete new Script settlement before reusing Paint state.
                self.session.servo.spin_event_loop();
                self.session.check_failure("controlled Paint reservation")?;
                if self.session.host_deadline.is_elapsed() {
                    return Err(SessionError::new(
                        "CONTROLLED_CAPTURE_TIMEOUT",
                        "controlled Paint reservation exceeded the normalized host-wall limit",
                    ));
                }
                Ok(PresentationReservationProgress::DocumentWorkQueued)
            },
            Err(ControlledDocumentCaptureError::Retryable(
                ControlledDocumentCaptureRetry::ReservationOccupied,
            )) => Err(SessionError::new(
                "CONTROLLED_PAINT_RESERVATION_FAILED",
                "Paint already retains another single-use capture reservation",
            )),
        }
    }

    /// Drive the controlled ScriptThread until it issues opaque capture-candidate evidence.
    /// No screenshot, layout snapshot, scene, PDF, or publication artifact is produced here.
    pub(crate) fn prepare_capture_candidate(
        self,
    ) -> Result<PreparedDocumentCaptureCandidate, SessionError> {
        self.session.webview.show();
        match self.prepare_ready_capture_candidate() {
            Ok((precondition, readiness, trace)) => Ok(PreparedDocumentCaptureCandidate {
                session: self,
                precondition,
                readiness,
                trace,
            }),
            Err(error) => Err(self.enrich_error_evidence(error)),
        }
    }

    /// Pair an authored API 1 readiness snapshot with a fresh candidate for the same target.
    /// Evaluating the snapshot is ordinary Script input and therefore invalidates the probe which
    /// preceded it. A ready snapshot is followed by another settlement before capture; a target
    /// mismatch at either boundary discards the snapshot and repeats under the one host budget.
    fn prepare_ready_capture_candidate(
        &self,
    ) -> Result<
        (
            Box<DocumentCapturePrecondition>,
            serde_json::Value,
            Vec<ControlledSettlementStep>,
        ),
        SessionError,
    > {
        let (mut probe, mut trace) = self.settle_capture_candidate()?;
        loop {
            let target = probe.target().clone();
            #[cfg(test)]
            CONTROLLED_READINESS_EVALUATIONS.fetch_add(1, AtomicOrdering::Relaxed);
            let evaluation = self.session.begin_readiness_evaluation();
            let (evaluated, evaluated_trace) = self
                .settle_capture_candidate()
                .map_err(|error| with_completed_readiness_evidence(error, &evaluation))?;
            trace.extend(evaluated_trace);
            let value = self
                .session
                .await_readiness_evaluation(evaluation.clone())
                .map_err(|error| with_completed_readiness_evidence(error, &evaluation))?;
            if evaluated.target() != &target {
                probe = evaluated;
                continue;
            }
            let (evidence, readiness) = decode_readiness_evaluation(value)?;
            #[cfg(test)]
            CONTROLLED_READINESS_CALLBACKS_DECODED.fetch_add(1, AtomicOrdering::Relaxed);
            match readiness {
                Readiness::Ready { .. } => {
                    #[cfg(test)]
                    CONTROLLED_READINESS_FRESHNESS_SETTLEMENTS
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    let (fresh, fresh_trace) = self
                        .settle_capture_candidate()
                        .map_err(|error| with_current_readiness_evidence(error, Some(&evidence)))?;
                    trace.extend(fresh_trace);
                    if fresh.target() == &target {
                        return Ok((fresh, evidence, trace));
                    }
                    probe = fresh;
                },
                Readiness::Failed { error } => {
                    return Err(SessionError::new(error.code, error.message)
                        .with_capture_evidence(SessionCaptureEvidence {
                            readiness: Some(evidence),
                            ..Default::default()
                        }));
                },
                Readiness::Pending if self.session.host_deadline.is_elapsed() => {
                    self.session
                        .diagnostic(|diagnostics| diagnostics.phase = "readiness-pending-deadline");
                    return Err(SessionError::new(
                        "READINESS_TIMEOUT",
                        "document readiness did not settle before the host deadline",
                    )
                    .with_capture_evidence(SessionCaptureEvidence {
                        readiness: Some(evidence),
                        ..Default::default()
                    }));
                },
                Readiness::Pending => {
                    #[cfg(test)]
                    CONTROLLED_READINESS_FRESHNESS_SETTLEMENTS
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    let (fresh, fresh_trace) = self
                        .settle_capture_candidate()
                        .map_err(|error| with_current_readiness_evidence(error, Some(&evidence)))?;
                    trace.extend(fresh_trace);
                    probe = fresh;
                },
            }
        }
    }

    fn settle_capture_candidate(
        &self,
    ) -> Result<
        (
            Box<DocumentCapturePrecondition>,
            Vec<ControlledSettlementStep>,
        ),
        SessionError,
    > {
        let coordinator = ControlledSettlementCoordinator::new(self.surface);
        let mut progress = coordinator.start();
        let mut wait_generation = None;
        let mut trace = Vec::new();

        loop {
            progress = match progress {
                ControlledSettlementProgress::Command(command) => {
                    self.session.check_failure("controlled settlement")?;
                    if self.session.host_deadline.is_elapsed() {
                        return Err(self.settlement_timeout("settlement-before-command"));
                    }
                    trace.push(controlled_settlement_step(&command));
                    self.session
                        .diagnostic(|diagnostics| diagnostics.command(&command));
                    let command_generation = self.waker.generation();
                    let response_waker = self.waker.clone();
                    let receiver = self
                        .session
                        .webview
                        .request_controlled_document_time_notifying(command, move || {
                            response_waker.notify_control_response();
                        })
                        .map_err(|error| {
                            SessionError::new(
                                "SETTLEMENT_CONTROL_FAILED",
                                format!("cannot submit controlled command: {error:?}"),
                            )
                        })?;
                    let (outcome, completion_generation) = self.await_control_outcome(receiver)?;
                    let next = coordinator
                        .consume_receive_outcome(outcome)
                        .map_err(settlement_transition_error)?;
                    if completion_generation.event_loop_changed_since(command_generation) {
                        self.session.diagnostic(|diagnostics| {
                            diagnostics.generation_reobservations =
                                diagnostics.generation_reobservations.saturating_add(1);
                        });
                        coordinator.discard_progress();
                        wait_generation = None;
                        ControlledSettlementProgress::Command(DocumentTimeControlCommand::Observe)
                    } else {
                        wait_generation = Some(completion_generation);
                        next
                    }
                },
                ControlledSettlementProgress::WaitForWake => {
                    trace.push(ControlledSettlementStep::WaitForWake);
                    self.session.check_failure("controlled settlement")?;
                    if self.session.host_deadline.is_elapsed() {
                        return Err(self.settlement_timeout("producer-wait-before-check"));
                    }
                    let observed = wait_generation
                        .take()
                        .unwrap_or_else(|| self.waker.generation());
                    // A producer ticket may belong to a task which was already queued before the
                    // preceding control command. Give Servo one owner-thread turn before sleeping;
                    // the pre-spin generation still catches work which arrives after Servo's last
                    // empty check.
                    let wait = self.waker.owner_turn_then_wait(
                        observed,
                        self.session.host_deadline.instant(),
                        || {
                            self.session.servo.spin_event_loop();
                            self.session.check_failure("controlled settlement")?;
                            if self.session.host_deadline.is_elapsed() {
                                return Err(self.settlement_timeout("producer-owner-after-spin"));
                            }
                            Ok(())
                        },
                    )?;
                    match wait {
                        EventLoopWakeWaitOutcome::Woken(_) => {
                            coordinator.discard_progress();
                            ControlledSettlementProgress::Command(
                                DocumentTimeControlCommand::Observe,
                            )
                        },
                        EventLoopWakeWaitOutcome::DeadlineReached(_) => {
                            return Err(self.settlement_timeout("producer-wake-deadline"));
                        },
                        EventLoopWakeWaitOutcome::GenerationExhausted(_) => {
                            return Err(SessionError::new(
                                "SETTLEMENT_WAKE_FAILED",
                                "controlled settlement wake generation was exhausted",
                            ));
                        },
                    }
                },
                ControlledSettlementProgress::Candidate(precondition) => {
                    return Ok((precondition, trace));
                },
            };
        }
    }

    fn await_control_outcome(
        &self,
        mut receiver: embedder_traits::DocumentTimeControlReceiver,
    ) -> Result<
        (
            DocumentTimeControlReceiveOutcome,
            pliego::event_loop_waker::EventLoopWakeGeneration,
        ),
        SessionError,
    > {
        loop {
            self.session.check_failure("controlled settlement")?;
            if self.session.host_deadline.is_elapsed() {
                return Err(self.settlement_timeout("response-before-spin"));
            }

            let before_spin = self.waker.generation();
            self.session.servo.spin_event_loop();
            self.session.check_failure("controlled settlement")?;
            if self.session.host_deadline.is_elapsed() {
                return Err(self.settlement_timeout("response-after-spin"));
            }

            match receiver.try_recv() {
                DocumentTimeControlTryReceiveOutcome::Complete(outcome) => {
                    let generation = self.waker.generation();
                    self.session
                        .diagnostic(|diagnostics| diagnostics.outcome(&outcome));
                    return Ok((outcome, generation));
                },
                DocumentTimeControlTryReceiveOutcome::Pending(pending) => receiver = pending,
            }

            match self
                .waker
                .wait_for_generation(before_spin, self.session.host_deadline.instant())
            {
                EventLoopWakeWaitOutcome::Woken(_) => {},
                EventLoopWakeWaitOutcome::DeadlineReached(_) => {
                    return Err(self.settlement_timeout("response-wake-deadline"));
                },
                EventLoopWakeWaitOutcome::GenerationExhausted(_) => {
                    return Err(SessionError::new(
                        "SETTLEMENT_WAKE_FAILED",
                        "controlled settlement wake generation was exhausted",
                    ));
                },
            }
        }
    }
}

fn decode_readiness_evaluation(
    value: Result<JSValue, String>,
) -> Result<(serde_json::Value, Readiness), SessionError> {
    let value = value.map_err(|error| SessionError::new("READINESS_EVALUATION_FAILED", error))?;
    let snapshot = match value {
        JSValue::String(snapshot) => snapshot,
        value => {
            return Err(SessionError::new(
                "READINESS_INVALID_RESULT",
                format!("expected readiness JSON string, got {value:?}"),
            ));
        },
    };
    let evidence: serde_json::Value = serde_json::from_str(&snapshot)
        .map_err(|error| SessionError::new("READINESS_INVALID_RESULT", error.to_string()))?;
    let readiness = readiness::parse_snapshot(&snapshot).map_err(|error| {
        SessionError::new("READINESS_INVALID_RESULT", error).with_capture_evidence(
            SessionCaptureEvidence {
                readiness: Some(evidence.clone()),
                ..Default::default()
            },
        )
    })?;
    Ok((evidence, readiness))
}

fn with_completed_readiness_evidence(
    error: SessionError,
    evaluation: &ReadinessEvaluation,
) -> SessionError {
    let completed = evaluation.borrow_mut().take();
    let Some(completed) = completed else {
        return error;
    };
    with_readiness_evaluation_evidence(error, &completed)
}

fn with_readiness_evaluation_evidence(
    error: SessionError,
    evaluation: &Result<JSValue, String>,
) -> SessionError {
    let Ok(JSValue::String(snapshot)) = evaluation else {
        return error;
    };
    let Ok(state) = readiness::parse_snapshot(snapshot) else {
        return error;
    };
    if let Readiness::Failed {
        error: readiness_error,
    } = state
    {
        if readiness_error.code != error.code || readiness_error.message != error.message {
            return error;
        }
    }
    let Ok(readiness) = serde_json::from_str(snapshot) else {
        return error;
    };
    with_current_readiness_evidence(error, Some(&readiness))
}

fn with_current_readiness_evidence(
    error: SessionError,
    readiness: Option<&serde_json::Value>,
) -> SessionError {
    error.with_capture_evidence(SessionCaptureEvidence {
        readiness: readiness.cloned(),
        ..Default::default()
    })
}

fn controlled_settlement_step(command: &DocumentTimeControlCommand) -> ControlledSettlementStep {
    match command {
        DocumentTimeControlCommand::Observe => ControlledSettlementStep::Observe,
        DocumentTimeControlCommand::DriveOneTurn => ControlledSettlementStep::DriveOneTurn,
        DocumentTimeControlCommand::AdvanceTo(_) => ControlledSettlementStep::AdvanceTo,
        DocumentTimeControlCommand::PrepareCapture(_) => ControlledSettlementStep::PrepareCapture,
        DocumentTimeControlCommand::ConsumeCapture(_) => ControlledSettlementStep::ConsumeCapture,
    }
}

fn exact_capture_commit(
    outcome: DocumentTimeControlReceiveOutcome,
    precondition: &DocumentCapturePrecondition,
    ticket: &DocumentPaintPresentationTicket,
) -> Result<DocumentCaptureCommit, SessionError> {
    let DocumentTimeControlReceiveOutcome::CommandOutcome(outcome) = outcome else {
        return Err(SessionError::new(
            "CONTROLLED_CAPTURE_CONSUME_INDETERMINATE",
            "capture consume transport ended without an authoritative result",
        ));
    };
    let DocumentTimeControlOutcome::Completed(observation) = outcome else {
        return match outcome {
            DocumentTimeControlOutcome::Rejected(error) => Err(SessionError::new(
                "CONTROLLED_CAPTURE_CONSUME_FAILED",
                format!("ScriptThread rejected the capture consume: {error:?}"),
            )),
            DocumentTimeControlOutcome::CaptureConsumeOutcomeIndeterminate { .. } => {
                Err(SessionError::new(
                    "CONTROLLED_CAPTURE_CONSUME_INDETERMINATE",
                    "ScriptThread may have consumed the single-use capture candidate",
                ))
            },
            DocumentTimeControlOutcome::AdvanceOutcomeIndeterminate { .. } => {
                Err(SessionError::new(
                    "CONTROLLED_CAPTURE_PROTOCOL_MISMATCH",
                    "capture consume returned an advance outcome",
                ))
            },
            DocumentTimeControlOutcome::Completed(_) => unreachable!(),
        };
    };
    let mut observation = *observation;
    let documents_match = observation.documents.len() == precondition.documents().len() &&
        observation
            .documents
            .iter()
            .zip(precondition.documents())
            .all(|(observed, expected)| {
                observed.pipeline_id == expected.pipeline_id &&
                    observed.script_rendering_epoch == Some(expected.script_rendering_epoch) &&
                    observed.readiness_blockers == expected.readiness_blockers
            });
    let Some(commit) = observation.capture_commit.take() else {
        return Err(SessionError::new(
            "CONTROLLED_CAPTURE_PROTOCOL_MISMATCH",
            "successful capture consume returned no commit",
        ));
    };
    if observation.action != DocumentTimeControlAction::CaptureConsumed ||
        observation.target != *precondition.target() ||
        observation.now != precondition.now() ||
        observation.next_deadline != precondition.next_deadline() ||
        observation.advance_token.is_some() ||
        observation.pending_events != precondition.pending_events() ||
        observation.input_batch_saturated != precondition.input_batch_saturated() ||
        observation.producers != precondition.producers() ||
        observation.execution != Some(precondition.execution()) ||
        observation.capture_preparation.is_some() ||
        !documents_match ||
        commit.candidate_id() != precondition.id() ||
        commit.ticket_id() != ticket.id() ||
        commit.target() != precondition.target() ||
        commit.pipeline_id() != ticket.pipeline_id() ||
        commit.script_rendering_epoch() != ticket.script_rendering_epoch() ||
        commit.surface() != precondition.surface() ||
        commit.presentation_generation() != ticket.presentation_generation() ||
        commit.publish_generation() != ticket.publish_generation() ||
        commit.layout_paint_epoch() != ticket.script_rendering_epoch()
    {
        return Err(SessionError::new(
            "CONTROLLED_CAPTURE_PROTOCOL_MISMATCH",
            "capture consume did not preserve the exact candidate and Paint ticket",
        ));
    }
    Ok(*commit)
}

fn settlement_transition_error(error: ControlledSettlementError) -> SessionError {
    SessionError::new("SETTLEMENT_FAILED", error.to_string())
}

fn validate_host_font_policy(
    capture: &SceneCapture,
    allow_host_fonts: bool,
) -> Result<(), SessionError> {
    let Some(resource) = unexpected_host_font(capture, allow_host_fonts) else {
        return Ok(());
    };

    Err(SessionError::new(
        "HOST_FONT_POLICY_VIOLATION",
        format!("Servo selected host font {resource} while host fonts were disabled"),
    ))
}

fn validate_resource_policy(policy: &ResourcePolicy) -> Result<(), SessionError> {
    match policy.setup_failure() {
        Some(ResourcePolicySetupFailure::Asset { error, .. }) => {
            Err(SessionError::new(error.code, error.message.clone()))
        },
        Some(ResourcePolicySetupFailure::Aggregate { code, message }) => {
            Err(SessionError::new(code, message))
        },
        None => Ok(()),
    }
}

fn session_host_timeout(readiness: ReadinessPolicy) -> Result<Duration, SessionError> {
    TIMEOUT
        .checked_add(Duration::from_millis(readiness.timeout_ms))
        .ok_or_else(|| SessionError::new("INVALID_REQUEST", "readiness timeout is too large"))
}

fn validate_session_timeouts(
    readiness: ReadinessPolicy,
    resource_timeout_ms: u64,
) -> Result<Duration, SessionError> {
    let session_host_timeout = session_host_timeout(readiness)?;
    validate_resource_timeout(resource_timeout_ms)?;
    Ok(session_host_timeout)
}

fn validate_resource_timeout(resource_timeout_ms: u64) -> Result<(), SessionError> {
    if !(1..=MAX_RESOURCE_TIMEOUT_MS).contains(&resource_timeout_ms) {
        return Err(SessionError::new(
            "INVALID_REQUEST",
            format!(
                "resource timeout must be between 1 and {MAX_RESOURCE_TIMEOUT_MS} milliseconds"
            ),
        ));
    }
    Ok(())
}

fn validate_resolved_resource_policy(
    document: &LocalDocument,
    resource_policy: &ResourcePolicy,
) -> Result<(), SessionError> {
    validate_resolved_resource_policy_root(document.root(), resource_policy)
}

fn validate_resolved_resource_policy_root(
    document_root: &Path,
    resource_policy: &ResourcePolicy,
) -> Result<(), SessionError> {
    match resource_policy.resolved_document_root() {
        Some(root) if root == document_root => Ok(()),
        Some(_) => Err(SessionError::new(
            "INVALID_REQUEST",
            "resource policy document root does not match the resolved document root",
        )),
        None => Err(SessionError::new(
            "INVALID_REQUEST",
            "resource policy has no resolved document root",
        )),
    }
}

#[derive(Default)]
struct DocumentDelegate {
    bundle_root: PathBuf,
    resource_policy: ResourcePolicy,
    frozen_input_authority: Option<FrozenInputAuthority>,
    controlled_http_client: OnceCell<net::connector::ServoClient>,
    resource_store: RefCell<OwnedResourceStore>,
    console: RefCell<ConsoleEvidenceLog>,
    crashed: RefCell<Option<String>>,
    frame_ready: Cell<bool>,
    /// Realtime sessions follow Servo's ordinary embedder repaint contract. Controlled sessions
    /// reserve and render one exact Paint generation explicitly after Script settlement.
    paint_frames_automatically: bool,
    load_complete: Cell<bool>,
    resource_failure: RefCell<Option<ResourcePolicyFailure>>,
    delivered_body_bytes: Cell<u64>,
    resources: RefCell<ResourceEvidenceLog>,
    #[cfg(test)]
    resource_evidence_observer: RefCell<Option<ResourceEvidenceObserver>>,
}

impl WebViewDelegate for DocumentDelegate {
    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        self.console
            .borrow_mut()
            .push(console_log_level_name(level).into(), message);
    }

    fn notify_new_frame_ready(&self, webview: WebView) {
        self.frame_ready.set(true);
        if self.paint_frames_automatically {
            webview.paint();
        }
    }

    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        if status == LoadStatus::Complete {
            self.load_complete.set(true);
        }
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, _backtrace: Option<String>) {
        *self.crashed.borrow_mut() = Some(reason);
    }

    fn load_web_resource(&self, _webview: WebView, load: WebResourceLoad) {
        let request = ResourceRequest {
            method: load.request().method.to_string(),
            url: load.request().url.clone(),
            destination: format!("{:?}", load.request().destination),
            load_role: load.request().load_role,
            referrer_url: load.request().referrer_url.clone(),
            is_for_main_frame: load.request().is_for_main_frame,
            is_redirect: load.request().is_redirect,
        };
        if let Err(failure) = self.resources.borrow_mut().begin_event(&request) {
            self.cancel_resource(load, failure);
            return;
        }
        if let Some(authority) = self.frozen_input_authority.as_ref() {
            // A resolved API 2 job never falls through to host files, data URLs, or the network.
            let resolved = authority.resolve(&request, &self.resource_store.borrow());
            match resolved {
                Ok((resource, headers)) => self.serve_owned_resource(
                    load,
                    request,
                    ResourceSource::VirtualResource,
                    resource,
                    headers,
                ),
                Err(failure) => self.deny_resource(load, request, failure),
            }
            return;
        }
        match self.resource_policy.decide(&self.bundle_root, &request) {
            ResourcePolicyDecision::Allow { source } => {
                if source != ResourceSource::DataUrl {
                    let evidence = ResourceEvidence::delegated(request, source);
                    if let Err(failure) = self.record_resource_evidence(evidence) {
                        self.cancel_resource(load, failure);
                    }
                    return;
                }
                let resource = match decode_bounded_data_url(&request) {
                    Ok(resource) => resource,
                    Err(failure) => {
                        self.cancel_resource(load, failure);
                        return;
                    },
                };
                let mut headers = HeaderMap::new();
                let content_type = resource.content_type.as_deref().unwrap_or_default();
                let content_type = match HeaderValue::from_str(content_type) {
                    Ok(content_type) => content_type,
                    Err(error) => {
                        self.cancel_resource(
                            load,
                            ResourcePolicyFailure::new(
                                &request,
                                "RESOURCE_DATA_URL_INVALID",
                                "invalid",
                                format!("data URL has an invalid content type: {error}"),
                            ),
                        );
                        return;
                    },
                };
                headers.insert(CONTENT_TYPE, content_type);
                self.serve_owned_resource(load, request, source, resource, headers);
            },
            ResourcePolicyDecision::FetchHttp => {
                let client = self
                    .controlled_http_client
                    .get_or_init(create_controlled_http_client);
                match fetch_controlled_http(
                    client,
                    &request,
                    &load.request().headers,
                    self.resource_policy.timeout_ms,
                ) {
                    Ok(response) => {
                        let resource = ControlledResource {
                            status: response.status.as_u16(),
                            content_type: response
                                .headers
                                .get(CONTENT_TYPE)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned),
                            body: response.body,
                        };
                        self.serve_owned_resource(
                            load,
                            request,
                            ResourceSource::Http,
                            resource,
                            response.headers,
                        );
                    },
                    Err(failure) => self.cancel_resource(load, failure),
                }
            },
            ResourcePolicyDecision::Synthesize {
                body,
                content_type,
                source,
            } => {
                let resource = ControlledResource {
                    status: 200,
                    content_type: Some(content_type.to_owned()),
                    body,
                };
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
                self.serve_owned_resource(load, request, source, resource, headers);
            },
            ResourcePolicyDecision::Fail(failure) => self.deny_resource(load, request, failure),
        }
    }
}

fn console_log_level_name(level: ConsoleLogLevel) -> &'static str {
    match level {
        ConsoleLogLevel::Log => "log",
        ConsoleLogLevel::Debug => "debug",
        ConsoleLogLevel::Info => "info",
        ConsoleLogLevel::Warn => "warn",
        ConsoleLogLevel::Error => "error",
        ConsoleLogLevel::Trace => "trace",
        ConsoleLogLevel::Dir => "dir",
    }
}

impl DocumentDelegate {
    fn serve_owned_resource(
        &self,
        load: WebResourceLoad,
        request: ResourceRequest,
        source: ResourceSource,
        resource: ControlledResource,
        headers: HeaderMap,
    ) {
        let next_delivered_body_bytes = match checked_delivered_body_bytes(
            &request,
            self.delivered_body_bytes.get(),
            resource.body.len() as u64,
            MAX_CACHE_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(failure) => {
                self.cancel_resource(load, failure);
                return;
            },
        };
        let status = match http::StatusCode::from_u16(resource.status) {
            Ok(status) => status,
            Err(error) => {
                self.cancel_resource(
                    load,
                    ResourcePolicyFailure::new(
                        &request,
                        "RESOURCE_METADATA_INVALID",
                        "invalid",
                        format!("controlled resource status is invalid: {error}"),
                    ),
                );
                return;
            },
        };
        let headers =
            match normalize_controlled_response_headers(&request, headers, resource.body.len()) {
                Ok(headers) => headers,
                Err(failure) => {
                    self.cancel_resource(load, failure);
                    return;
                },
            };
        let resource = match self
            .resource_store
            .borrow_mut()
            .retain_with_source(&request, source, resource, &headers)
        {
            Ok(resource) => resource,
            Err(failure) => {
                self.cancel_resource(load, failure);
                return;
            },
        };
        let evidence = ResourceEvidence::loaded_response(
            request.clone(),
            source,
            resource.status(),
            resource.content_type(),
            resource.body(),
            resource.response_headers().clone(),
        );
        debug_assert_eq!(
            evidence.content_address.as_deref(),
            Some(resource.content_address())
        );
        if let Err(failure) = self.record_resource_evidence(evidence) {
            self.cancel_resource(load, failure);
            return;
        }
        self.delivered_body_bytes.set(next_delivered_body_bytes);
        let mut intercepted = load.intercept(
            WebResourceResponse::new(request.url)
                .headers(headers)
                .status_code(status)
                .status_message(
                    status
                        .canonical_reason()
                        .unwrap_or_default()
                        .as_bytes()
                        .to_vec(),
                ),
        );
        intercepted.send_body_data(resource.body().to_vec());
        intercepted.finish();
    }

    fn record_resource_evidence(
        &self,
        evidence: ResourceEvidence,
    ) -> Result<(), ResourcePolicyFailure> {
        #[cfg(test)]
        let observed_evidence = evidence.clone();
        let result = self.resources.borrow_mut().push(evidence);
        #[cfg(test)]
        if result.is_ok() &&
            let Some(observer) = self.resource_evidence_observer.borrow().as_ref()
        {
            observer(&observed_evidence);
        }
        result
    }

    fn cancel_resource(&self, load: WebResourceLoad, failure: ResourcePolicyFailure) {
        if self.resource_failure.borrow().is_none() {
            *self.resource_failure.borrow_mut() = Some(failure);
        }
        Self::cancel_load(load);
    }

    fn deny_resource(
        &self,
        load: WebResourceLoad,
        request: ResourceRequest,
        failure: ResourcePolicyFailure,
    ) {
        if request.load_role != WebResourceLoadRole::DocumentMetadata ||
            !failure.is_optional_metadata_failure()
        {
            self.cancel_resource(load, failure);
            return;
        }

        debug_assert_eq!(request.load_role, WebResourceLoadRole::DocumentMetadata);
        if let Err(evidence_failure) =
            self.record_resource_evidence(ResourceEvidence::cancelled(request, failure))
        {
            self.cancel_resource(load, evidence_failure);
            return;
        }

        Self::cancel_load(load);
    }

    fn cancel_load(load: WebResourceLoad) {
        let url = load.request().url.clone();
        load.intercept(WebResourceResponse::new(url)).cancel();
    }
}

fn checked_delivered_body_bytes(
    request: &ResourceRequest,
    delivered: u64,
    additional: u64,
    limit: u64,
) -> Result<u64, ResourcePolicyFailure> {
    delivered
        .checked_add(additional)
        .filter(|bytes| *bytes <= limit)
        .ok_or_else(|| {
            ResourcePolicyFailure::new(
                request,
                "RESOURCE_DELIVERY_LIMIT_EXCEEDED",
                "denied",
                format!("delivered resource bodies exceed the {limit}-byte aggregate bound"),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{self, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use layout::pages::{PageDefinition, PageMargins};
    use pliego::capture::{CapturedFontSelection, CapturedFontSource, SceneCapture};
    use pliego::{DocumentScene, Operation, Page, Size};
    use servo::WebResourceLoadRole;
    use sha2::{Digest, Sha256};

    use super::super::resource_policy::{
        MAX_RESOURCE_TIMEOUT_MS, ResourceAccounting, ResourceEvidence, ResourcePolicy,
        ResourcePolicyFailure, ResourceRequest, ResourceSource, ResponseHeaderEvidence,
        VirtualResourceSpec,
    };
    use super::super::runtime_policy::DeterministicRuntimePolicy;
    use super::super::session::LocalDocument;
    use super::{
        Api2Execution, ConsoleEvidenceLog, ConsoleLogLevel, ControlledSettlementStep,
        DocumentOutcome, DocumentSession, FROZEN_INPUT_URL_ROOT, JSValue, MAX_CONSOLE_BYTES,
        MAX_CONSOLE_EVENTS, PaintTicketAbortGuard, RESOURCE_EVIDENCE_ENTRY_OVERHEAD_BYTES,
        ReadinessPolicy, RenderEnvironment, ResourceEvidenceLog, ResourcePolicyConfig,
        SessionCaptureEvidence, SessionError, SessionHostDeadline, console_log_level_name,
        controlled_readiness_handshake_counts, session_host_timeout, validate_host_font_policy,
        validate_resolved_resource_policy, validate_resource_policy,
        with_current_readiness_evidence, with_readiness_evaluation_evidence,
    };

    const ISOLATED_CASE_ENV: &str = "PLIEGO_DOCUMENT_SESSION_FIXTURE";
    const PLIEGO_INPUT_CASE_ENV: &str = "PLIEGO_DOCUMENT_SESSION_PLIEGO_INPUT_FIXTURE";
    const CHARTJS_INPUT_ENV: &str = "PLIEGO_DOCUMENT_SESSION_CHARTJS_INPUT";
    const HTTP_BASE_ENV: &str = "PLIEGO_DOCUMENT_SESSION_HTTP_BASE";
    const INVOICE_ORACLE_EVIDENCE_ENV: &str = "PLIEGO_INVOICE_ORACLE_EVIDENCE_ROOT";
    const CHARTJS_ORACLE_EVIDENCE_ENV: &str = "PLIEGO_CHARTJS_ORACLE_EVIDENCE_ROOT";
    const ISOLATED_TEST: &str = "document_session::tests::isolated_resource_and_readiness_fixture";
    const PLIEGO_INPUT_ISOLATED_TEST: &str =
        "document_session::tests::isolated_pliego_input_url_fixture";
    const API2_REQUEST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/goldens/accepted/render-request.a4.json"
    ));
    const API2_AHEM_TTF: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/text-scene/Ahem.ttf"
    ));
    const ALLOWED_HTTP_BODY: &[u8] = b"window.pliego.ready({ http_loaded: true });\n";
    const FIXTURE_PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    fn readiness_handshake_delta(before: (usize, usize, usize)) -> (usize, usize, usize) {
        let after = controlled_readiness_handshake_counts();
        (after.0 - before.0, after.1 - before.1, after.2 - before.2)
    }

    fn assert_successful_readiness_handshake(before: (usize, usize, usize)) {
        let (evaluations, callbacks, freshness) = readiness_handshake_delta(before);
        assert!(evaluations > 0);
        assert_eq!(callbacks, evaluations);
        assert_eq!(freshness, evaluations);
    }

    fn diagnostic_observation() -> embedder_traits::DocumentTimeControlObservation {
        use embedder_traits::{
            DocumentProducerStability, DocumentTimeControlAction, DocumentTimeControlTarget,
            DocumentTimeDocumentObservation, DocumentTimeProducerObservation,
        };
        use servo_base::id::{TEST_PIPELINE_ID, TEST_SCRIPT_EVENT_LOOP_ID, TEST_WEBVIEW_ID};
        use timers::{DocumentProducerCheckpoint, DocumentProducerFence, DocumentTime};

        let fence = DocumentProducerFence::with_execution_ledger(None);
        embedder_traits::DocumentTimeControlObservation {
            target: DocumentTimeControlTarget {
                webview_id: TEST_WEBVIEW_ID,
                event_loop_id: TEST_SCRIPT_EVENT_LOOP_ID,
                webview_epoch: Default::default(),
                pipelines: vec![TEST_PIPELINE_ID],
                fully_active_pipelines: vec![TEST_PIPELINE_ID],
            },
            now: DocumentTime::from_nanos(u128::MAX),
            next_deadline: None,
            advance_token: None,
            pending_events: 3,
            input_batch_saturated: true,
            action: DocumentTimeControlAction::Observed,
            producers: DocumentTimeProducerObservation {
                fence_id: fence.id(),
                checkpoint: DocumentProducerCheckpoint::ZERO,
                snapshot: fence.snapshot(),
                stability: DocumentProducerStability::NotCheckpointed,
            },
            documents: vec![
                DocumentTimeDocumentObservation {
                    pipeline_id: TEST_PIPELINE_ID,
                    script_rendering_epoch: None,
                    readiness_blockers: vec![
                        embedder_traits::DocumentTimeReadinessBlocker::Loading;
                        20
                    ],
                };
                20
            ],
            execution: None,
            capture_preparation: None,
            capture_commit: None,
        }
    }

    #[test]
    fn session_diagnostics_requires_exact_opt_in_and_controlled_mode() {
        use std::ffi::OsStr;

        use super::SessionDiagnostics;

        assert!(SessionDiagnostics::enabled(Some(OsStr::new("1")), true));
        assert!(!SessionDiagnostics::enabled(Some(OsStr::new("1")), false));
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("yes"),
            Some(" 1"),
        ] {
            assert!(!SessionDiagnostics::enabled(value.map(OsStr::new), true));
        }
    }

    #[test]
    fn session_diagnostics_keeps_bounded_actual_observation_and_rejection() {
        use embedder_traits::{
            DocumentTimeControlCommand, DocumentTimeControlError, DocumentTimeControlOutcome,
            DocumentTimeControlReceiveOutcome,
        };

        use super::SessionDiagnostics;

        let mut diagnostic = SessionDiagnostics::default();
        diagnostic.command(&DocumentTimeControlCommand::Observe);
        diagnostic.outcome(&DocumentTimeControlReceiveOutcome::CommandOutcome(
            DocumentTimeControlOutcome::Completed(Box::new(diagnostic_observation())),
        ));
        diagnostic.outcome(&DocumentTimeControlReceiveOutcome::CommandOutcome(
            DocumentTimeControlOutcome::Rejected(DocumentTimeControlError::EventLoopUnavailable),
        ));
        assert_eq!(diagnostic.commands, 1);
        assert_eq!(diagnostic.response_count, 2);
        assert_eq!(diagnostic.rejections, 1);
        assert_eq!(diagnostic.last_outcome, Some("event-loop-unavailable"));
        assert_eq!(diagnostic.last_observation_response, Some(1));
        let observation = diagnostic.last_observation.as_ref().unwrap();
        assert_eq!(observation["virtual_time_ns"], u128::MAX.to_string());
        assert_eq!(observation["pending_events"], 3);
        assert_eq!(observation["documents"], 20);
        assert_eq!(
            observation["first_document_readiness"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(observation["first_document_readiness_total"], 20);
        assert!(observation.get("capture_commit").is_none());
        diagnostic.commands = u64::MAX;
        diagnostic.command(&DocumentTimeControlCommand::Observe);
        assert_eq!(diagnostic.commands, u64::MAX);
    }

    #[test]
    fn session_diagnostics_redacts_payloads_and_bounds_resource_details() {
        use embedder_traits::{
            DocumentTimeControlReceiveOutcome, DocumentTimeControlTransportFailure,
        };

        use super::{DocumentDelegate, MAX_SESSION_DIAGNOSTIC_BYTES, SessionDiagnostics};

        const SECRET: &str = "PRIVATE-DOCUMENT-PAYLOAD";
        let request = ResourceRequest {
            method: SECRET.into(),
            url: url::Url::parse(&format!("https://secret.test/{SECRET}")).unwrap(),
            destination: SECRET.into(),
            load_role: WebResourceLoadRole::DocumentMetadata,
            referrer_url: Some(
                url::Url::parse(&format!("https://referrer.test/{SECRET}")).unwrap(),
            ),
            is_for_main_frame: true,
            is_redirect: false,
        };
        let mut resource = ResourceEvidence::loaded(
            request,
            ResourceSource::VirtualResource,
            SECRET,
            SECRET.as_bytes(),
        );
        resource.sha256 = Some(SECRET.into());
        let delegate = DocumentDelegate::default();
        delegate.resources.borrow_mut().entries = vec![resource; 20];
        delegate.resources.borrow_mut().observed_events = 20;
        delegate
            .console
            .borrow_mut()
            .push("error".into(), SECRET.into());
        delegate.load_complete.set(true);
        let mut diagnostic = SessionDiagnostics::default();
        diagnostic.outcome(&DocumentTimeControlReceiveOutcome::ObserveTransportFailure(
            DocumentTimeControlTransportFailure::DeserializationFailed(SECRET.repeat(1000)),
        ));
        let mut error = SessionError::new(SECRET, SECRET);
        error.capture_evidence.readiness = Some(serde_json::json!({"payload": SECRET}));
        let line = diagnostic.failure_line(
            SessionHostDeadline::start(Duration::from_secs(10)).unwrap(),
            &error,
            Some(&delegate),
        );
        assert!(line.len() <= MAX_SESSION_DIAGNOSTIC_BYTES);
        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(!String::from_utf8_lossy(&line).contains(SECRET));
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["failure_class"], "OTHER_SESSION_FAILURE");
        assert_eq!(value["last_outcome"], "transport-failure-or-indeterminate");
        assert_eq!(value["load_complete"], true);
        assert_eq!(value["resource_destinations"].as_array().unwrap().len(), 8);
        assert_eq!(value["resource_destinations"][0], "other");
        assert_eq!(value["resource_entries"], 20);
        assert_eq!(value["resource_loaded"], 20);
        assert!(value["last_observation"].is_null());
        assert!(value["startup_ms"]["servo_ready"].is_null());
        diagnostic.last_observation =
            Some(serde_json::json!({"oversized_internal_detail": "x".repeat(20_000)}));
        let line = diagnostic.failure_line(
            SessionHostDeadline::start(Duration::from_secs(10)).unwrap(),
            &error,
            Some(&delegate),
        );
        assert!(line.len() <= MAX_SESSION_DIAGNOSTIC_BYTES);
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["encoding_failed"], true);
    }

    #[test]
    fn session_diagnostics_timeout_sites_and_startup_are_private_metadata() {
        for phase in [
            "settlement-before-command",
            "producer-wait-before-check",
            "producer-owner-after-spin",
            "producer-wake-deadline",
            "response-before-spin",
            "response-after-spin",
            "response-wake-deadline",
        ] {
            let diagnostic = std::cell::RefCell::new(super::SessionDiagnostics {
                startup_ms: [Some(10.0), Some(20.0), Some(30.0)],
                ..Default::default()
            });
            let error = super::SessionDiagnostics::settlement_timeout(Some(&diagnostic), phase);
            let disabled = super::SessionDiagnostics::settlement_timeout(None, phase);
            assert_eq!(error, disabled);
            let line = diagnostic.borrow().failure_line(
                SessionHostDeadline::start(Duration::from_secs(10)).unwrap(),
                &error,
                None,
            );
            let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
            assert_eq!(value["timeout_site"], phase);
            assert_eq!(value["failure_class"], "SETTLEMENT_TIMEOUT");
            assert_eq!(value["startup_ms"]["webview_ready"], 30.0);
            assert_eq!(value["host_limit_ms"], 10_000.0);
            assert_eq!(error.code, "SETTLEMENT_TIMEOUT");
            assert_eq!(
                error.message,
                "controlled settlement exceeded the normalized host-wall limit"
            );
            assert!(value["load_complete"].is_null());
        }
    }

    #[test]
    fn session_diagnostics_stderr_only() {
        use super::{SESSION_DIAGNOSTICS_ENV, SessionDiagnostics};
        const CHILD: &str = "PLIEGO_SESSION_DIAGNOSTICS_TEST_CHILD";
        const MARKER: &str = "pliego-session-diagnostics-v1";
        if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
            if SessionDiagnostics::enabled(
                std::env::var_os(SESSION_DIAGNOSTICS_ENV).as_deref(),
                true,
            ) {
                let mut diagnostic = SessionDiagnostics {
                    phase: "test-failure",
                    ..Default::default()
                };
                let deadline = SessionHostDeadline::start(Duration::from_secs(10)).unwrap();
                let error = SessionError::new("SETTLEMENT_TIMEOUT", "unchanged product failure");
                diagnostic.emit_failure(deadline, &error, None);
                diagnostic.emit_failure(deadline, &error, None);
            }
            println!("SESSION_DIAGNOSTIC_STDOUT_SENTINEL");
            return;
        }
        for activation in [None, Some("0"), Some("1")] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "document_session::tests::session_diagnostics_stderr_only",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove(SESSION_DIAGNOSTICS_ENV);
            if let Some(value) = activation {
                command.env(SESSION_DIAGNOSTICS_ENV, value);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(stdout.contains("SESSION_DIAGNOSTIC_STDOUT_SENTINEL"));
            assert!(!stdout.contains(MARKER));
            if activation == Some("1") {
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert_eq!(stderr.lines().count(), 1);
                let value: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
                assert_eq!(value["diagnostic"], MARKER);
                assert_eq!(value["timeout_site"], "test-failure");
            } else {
                assert!(output.stderr.is_empty());
            }
        }
    }

    #[test]
    fn paint_ticket_abort_guard_runs_during_unwind() {
        let aborts = Cell::new(0);
        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _guard = PaintTicketAbortGuard::new(|| aborts.set(aborts.get() + 1));
            panic!("exercise retained-ticket unwind cleanup");
        }));

        assert!(unwind.is_err());
        assert_eq!(aborts.get(), 1);
    }

    #[test]
    fn reservation_error_keeps_current_readiness_without_overwriting_richer_evidence() {
        let latest = serde_json::json!({
            "status": "ready",
            "payload": { "revision": 2 },
            "font_status": "loaded",
        });
        let enriched = with_current_readiness_evidence(
            SessionError::new("RESERVATION_FAILED", "reservation failed"),
            Some(&latest),
        );
        assert_eq!(enriched.capture_evidence.readiness.as_ref(), Some(&latest));

        let terminal = serde_json::json!({
            "status": "failed",
            "error": { "code": "PAGE_FAILED", "message": "authored failure" },
        });
        let enriched = with_current_readiness_evidence(
            SessionError::new("PAGE_FAILED", "authored failure").with_capture_evidence(
                SessionCaptureEvidence {
                    readiness: Some(terminal.clone()),
                    ..Default::default()
                },
            ),
            Some(&latest),
        );
        assert_eq!(
            enriched.capture_evidence.readiness.as_ref(),
            Some(&terminal)
        );
    }

    #[test]
    fn settlement_error_rejects_parseable_but_invalid_readiness_evidence() {
        let evaluation = Ok(JSValue::String(
            r#"{"status":"ready","font_status":"loaded"}"#.to_owned(),
        ));
        let error = with_readiness_evaluation_evidence(
            SessionError::new("SETTLEMENT_FAILED", "settlement failed"),
            &evaluation,
        );

        assert!(error.capture_evidence.readiness.is_none());
    }

    #[test]
    fn settlement_error_rejects_failed_readiness_evidence_from_a_different_failure() {
        for snapshot in [
            r#"{"status":"failed","error":{"code":"OTHER_FAILURE","message":"settlement failed"}}"#,
            r#"{"status":"failed","error":{"code":"SETTLEMENT_FAILED","message":"different failure"}}"#,
        ] {
            let evaluation = Ok(JSValue::String(snapshot.to_owned()));
            let error = with_readiness_evaluation_evidence(
                SessionError::new("SETTLEMENT_FAILED", "settlement failed"),
                &evaluation,
            );

            assert!(error.capture_evidence.readiness.is_none());
        }
    }

    #[test]
    fn settlement_error_keeps_exactly_matching_failed_readiness_evidence() {
        let snapshot = serde_json::json!({
            "status": "failed",
            "error": {
                "code": "SETTLEMENT_FAILED",
                "message": "settlement failed",
            },
        });
        let evaluation = Ok(JSValue::String(snapshot.to_string()));
        let error = with_readiness_evaluation_evidence(
            SessionError::new("SETTLEMENT_FAILED", "settlement failed"),
            &evaluation,
        );

        assert_eq!(error.capture_evidence.readiness.as_ref(), Some(&snapshot));
    }

    const AHEM_SOURCE_RESOURCE: &str =
        "sha256:b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448";
    const AHEM_CAPTURED_RESOURCE: &str =
        "sha256:649a7613cfa59d415188415e1488eb40fc9953742338a793538380234a539869";
    const FIXTURE_INPUT: &str =
        "sha256:5e55d09a14e3635cf446d563680b7aa9cdc4a29eb05779b999de8bf957f1bd98";
    // Pre-DocumentSession servoshell oracle for the exact linked fixture above:
    // source=34c55cf4c7723a4ce757556bab454dc6b6a6f212
    // binary=6b46bcbbaf4ba59ea9c5536a1bb02ab63366a57df7c971ed27de440b988efa19
    // harness=679b783420af1bfcfd2624d182e882b6051e65b521a2fe1a8350d28d9c323ec1
    // manifest=6e9aef86890b5fb8b965c9ed269dae670bf1fdf66aa574d3efbdb674a73ea6ef
    // report=report-seven-exact.json
    // report-sha=9035533ede8e0b7585b5a4625cec5e2e6abe049d79b8a7a5f4edaad89e9aec32
    // repeats=2. Published 0.1.1 binary 632ab7ca4ccd931b392593a9f0f5673cce543891d758fa7737bb859c8244ad27
    // is a diagnostic non-oracle: it omitted the anchor link on this newer input.
    // Regenerate only from a retained same-input differential proof, never from this test alone:
    // python3 benchmarks/tools/compare_parity.py --baseline <pre-session-servoshell-binary>
    //   --candidate <candidate-binary> --fixture minimal-static --repeat 2
    //   --out report-seven-exact.json
    // Copy new digests only after the report has zero problems, then update all provenance above.
    const PRE_SESSION_SCENE: &str =
        "sha256:a2854099b0a11e766cad6eaeca8a76f45d2d77654fa02bb8504294c16cefc4f2";
    const PRE_SESSION_PDF: &str =
        "sha256:9873076c43b0c76dca8fc54ad5721e5cd20ccee5deca6905425d49df068d7af8";
    const EXPECTED_LINK: &str = "https://pliego.dev/docs";

    // Exact pre-OXH-304 servoshell oracle: source=e2ad2c930d243b8a84a63503a1d3e73f35e7875e,
    // Linux binary=sha256:4c64919c959a712b28b4e6b5280fa74d742291b90e381b2c2dc1c014c2ecd4ab.
    const HYBRID_INPUT: &str =
        "sha256:57554089f5f96b3403ece0419869a71acb6c986ef1cd6c2633d284e63bf853e5";
    const PRE_SESSION_HYBRID_SCENE: &str =
        "sha256:bb176eb5db6433edba8edcdba7f1ff64b9f1784dad84b04f39296d1c4d5a41f8";
    const PRE_SESSION_HYBRID_PDF: &str =
        "sha256:fb58e9bea7d81dd047b79fa2a54173c730a30967cd9902b1f9b8ce82a4fe11a9";
    const CHARTJS_INPUT: &str =
        "sha256:2c5d37327bbde05b8369fcb5ea75cfec7fba437b1232848f1c2e20d5f2978995";
    const CHARTJS_UMD: &str = "ecc3cd1eeb8c34d2178e3f59fd63ec5a3d84358c11730af0b9958dc886d7652a";
    // Reviewed original-Au projection snapshot, not unchanged pre-session bytes.
    // See docs/pliego/chartjs-regression-snapshot.md for full differential provenance.
    const CHARTJS_SCENE: &str =
        "sha256:5711745bb1adb142493dd487b81ece7f521eea35df5873bd13baef15bb0b55bf";
    const CHARTJS_PDF: &str =
        "sha256:790dde4a113267ca3816bd048f3dd9ed68f98917b30dae44c587f094c44ed65e";
    const PRE_SESSION_CHARTJS_CANVAS: &str =
        "sha256:3625ec653c27b9e1c8d0fa969acbd88cc161804eeea4cd3046795d411e8118c9";

    #[test]
    fn repeated_body_delivery_is_fatally_bounded_before_interception() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://example.test/shared").unwrap(),
            destination: "Image".into(),
            load_role: WebResourceLoadRole::DocumentMetadata,
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };
        let delivered = super::checked_delivered_body_bytes(&request, 0, 3, 4).unwrap();
        let error = super::checked_delivered_body_bytes(&request, delivered, 3, 4).unwrap_err();
        assert_eq!(error.code, "RESOURCE_DELIVERY_LIMIT_EXCEEDED");
        assert!(error.fatal);
        assert_eq!(error.load_role, WebResourceLoadRole::DocumentMetadata);
        assert_eq!(delivered, 3);
    }

    #[test]
    fn evidence_metadata_limit_rejects_before_an_entry_is_retained() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://example.test/resource").unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentMetadata,
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-large",
            http::HeaderValue::from_str(&"x".repeat(4_096)).unwrap(),
        );
        let evidence = ResourceEvidence::loaded_response(
            request,
            ResourceSource::Http,
            200,
            None,
            b"",
            ResponseHeaderEvidence::from_headers(&headers).unwrap(),
        );
        assert!(evidence.metadata_bytes() > 4_096);
        let mut log = ResourceEvidenceLog {
            max_metadata_bytes: evidence
                .metadata_bytes()
                .saturating_add(RESOURCE_EVIDENCE_ENTRY_OVERHEAD_BYTES - 1),
            ..ResourceEvidenceLog::default()
        };
        log.begin_event(&evidence.request).unwrap();
        let error = log.push(evidence).unwrap_err();
        assert_eq!(error.code, "RESOURCE_METADATA_LIMIT_EXCEEDED");
        assert_eq!(error.load_role, WebResourceLoadRole::DocumentMetadata);
        assert!(error.fatal);
        assert!(log.entries.is_empty());
        assert_eq!(log.metadata_bytes, 0);
    }

    #[test]
    fn metadata_cancellations_are_evidenced_but_event_bounds_remain_fatal() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://denied.invalid/report.bin").unwrap(),
            destination: "Image".into(),
            load_role: WebResourceLoadRole::DocumentMetadata,
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };
        let cancellation = ResourceEvidence::cancelled(
            request.clone(),
            ResourcePolicyFailure::new(
                &request,
                "RESOURCE_DENIED",
                "denied",
                "network URL is outside the configured HTTP roots",
            )
            .nonfatal(),
        );
        assert_eq!(cancellation.status, "cancelled");
        assert!(!cancellation.fatal);
        assert_eq!(cancellation.source, None);
        assert_eq!(
            cancellation.failure.as_ref().unwrap().load_role,
            WebResourceLoadRole::DocumentMetadata
        );
        assert!(!cancellation.failure.as_ref().unwrap().fatal);
        assert_eq!(
            ResourceAccounting::from_evidence(std::slice::from_ref(&cancellation)),
            ResourceAccounting {
                requests: 1,
                loaded: 0,
                delegated: 0,
                failed: 1,
                body_bytes: 0,
                unavailable_bodies: 1,
            }
        );

        let mut log = ResourceEvidenceLog {
            max_entries: 1,
            ..ResourceEvidenceLog::default()
        };
        log.begin_event(&cancellation.request).unwrap();
        log.push(cancellation.clone()).unwrap();
        let error = log.begin_event(&cancellation.request).unwrap_err();
        assert_eq!(error.code, "RESOURCE_METADATA_LIMIT_EXCEEDED");
        assert_eq!(error.load_role, WebResourceLoadRole::DocumentMetadata);
        assert!(error.fatal);
        assert_eq!(log.entries.len(), 1);
    }

    #[test]
    fn denied_host_font_fails_before_a_document_outcome() {
        let capture = SceneCapture {
            scene: DocumentScene::new(Page {
                size: Size {
                    width: 1.0,
                    height: 1.0,
                },
                operations: vec![],
            }),
            fixed_point_authority: Default::default(),
            canvas_resources: vec![],
            embedded_image_resources: vec![],
            canvas_diagnostics: vec![],
            font_resources: vec![],
            font_instances: vec![],
            font_selections: vec![CapturedFontSelection {
                instance: "fixture-instance".into(),
                resource: "host:Fixture Sans".into(),
                face_index: 0,
                source: CapturedFontSource::Host,
                requested_families: vec!["Fixture Sans".into()],
                selected_family: Some("Fixture Sans".into()),
            }],
            font_warnings: vec![],
            unsupported_events: vec![],
            text_mapping_gaps: vec![],
        };

        let error = validate_host_font_policy(&capture, false)
            .expect_err("a denied host font must not produce a DocumentOutcome");
        assert_eq!(error.code, "HOST_FONT_POLICY_VIOLATION");
        assert_eq!(
            error.message,
            "Servo selected host font host:Fixture Sans while host fonts were disabled"
        );
        assert!(validate_host_font_policy(&capture, true).is_ok());
    }

    #[test]
    fn virtual_and_asset_bodies_fail_before_a_document_outcome() {
        let bundle = TempBundle::new("aggregate-limit");
        bundle.write("virtual.js", b"one");
        bundle.write("a.js", b"two");
        bundle.write("b.js", b"tri");
        let digest = |body: &[u8]| content_address(body)[7..].to_owned();
        bundle.write(
            "assets.json",
            serde_json::to_vec(&serde_json::json!({
                "schema": super::super::asset_cache::MANIFEST_SCHEMA,
                "version": 1,
                "assets": [
                    {
                        "url": "https://assets.test/a.js",
                        "path": "a.js",
                        "sha256": digest(b"two"),
                    },
                    {
                        "url": "https://assets.test/b.js",
                        "path": "b.js",
                        "sha256": digest(b"tri"),
                    },
                ],
            }))
            .unwrap(),
        );
        let policy = ResourcePolicy::resolve_with_budget(
            &ResourcePolicyConfig {
                virtual_resources: vec![VirtualResourceSpec {
                    url: url::Url::parse("pliego://host/virtual.js").unwrap(),
                    path: PathBuf::from("virtual.js"),
                }],
                asset_manifest: Some(PathBuf::from("assets.json")),
                ..ResourcePolicyConfig::default()
            },
            &bundle.0,
            8,
        );

        let error = validate_resource_policy(&policy)
            .expect_err("aggregate overflow must fail before a DocumentOutcome exists");
        assert_eq!(error.code, "ASSET_CACHE_LIMIT");
        assert!(error.message.contains("5-byte resident allowance"));
        assert_eq!(policy.resident_bytes, 3);
    }

    const INVOICE_INPUT: &str =
        "sha256:b0fa2d0b18e845e84c1229408622bd85e092ecf4d78b0878939006fb26926dce";
    // Current snapshot after reviewed paint-order and original-Au projection changes.
    // Historical hashes and same-input differential proof remain documented in
    // docs/pliego/invoice-regression-snapshot.md; these are not unchanged shell bytes.
    const INVOICE_SCENE: &str =
        "sha256:97997f0c5863c1ff27cf0511d7a96c22121216d6ca3320191ad369146af790c5";
    const INVOICE_PDF: &str =
        "sha256:952988f08a5be37dd7cc262326d2a50c592ff0a329c84ae82b9c1d5381f5f96e";

    fn retain_oracle_evidence(case: &str, input: &Path, outcome: &DocumentOutcome, scene: &[u8]) {
        let (environment, expected_input, expected_scene, expected_pdf, expected_pages, font_name) =
            match case {
                "invoice-oracle" => (
                    INVOICE_ORACLE_EVIDENCE_ENV,
                    INVOICE_INPUT,
                    INVOICE_SCENE,
                    INVOICE_PDF,
                    2,
                    "Ahem.ttf",
                ),
                "chartjs-report" => (
                    CHARTJS_ORACLE_EVIDENCE_ENV,
                    CHARTJS_INPUT,
                    CHARTJS_SCENE,
                    CHARTJS_PDF,
                    1,
                    "ReportSans.ttf",
                ),
                _ => panic!("unsupported retained oracle case: {case}"),
            };
        let Some(root) = std::env::var_os(environment) else {
            return;
        };
        let root = PathBuf::from(root);
        assert!(root.is_absolute(), "oracle evidence root must be absolute");
        fs::create_dir_all(&root).expect("create opt-in oracle evidence root");
        let directory = root.join(format!("{case}-{}", std::process::id()));
        fs::create_dir(&directory).expect("oracle evidence leaf must be fresh");
        let input_bytes = fs::read(input).expect("read exact oracle input");
        let font = fs::read(input.with_file_name(font_name)).expect("read original oracle font");
        let capture = serde_json::to_vec_pretty(&outcome.capture).unwrap();
        // SceneCapture's public JSON deliberately omits this internal ledger.
        // Preserve its original integer values separately without changing that contract.
        let authority = format!("{:#?}\n", outcome.capture.fixed_point_authority);
        let mut artifacts = Vec::new();
        let mut retain = |name: &str, bytes: &[u8]| {
            assert!(
                bytes.len() <= 16 * 1024 * 1024,
                "oracle evidence artifact exceeds 16 MiB: {name}"
            );
            fs::write(directory.join(name), bytes).expect("write exact oracle evidence");
            artifacts.push(serde_json::json!({
                "path": name, "bytes": bytes.len(), "sha256": content_address(bytes),
            }));
        };
        for (name, bytes) in [
            ("input.html", input_bytes.as_slice()),
            (font_name, font.as_slice()),
            ("scene.json", scene),
            ("document.pdf", outcome.pdf.as_slice()),
            ("capture.json", capture.as_slice()),
            ("fixed-point-authority.txt", authority.as_bytes()),
        ] {
            retain(name, bytes);
        }
        if case == "chartjs-report" {
            let root = input.parent().expect("Chart.js fixture has a bundle root");
            for (source, retained) in [
                ("package.json", "package.json"),
                ("package-lock.json", "package-lock.json"),
                ("node_modules/chart.js/dist/chart.umd.js", "chart.umd.js"),
            ] {
                retain(
                    retained,
                    &fs::read(root.join(source)).expect("read locked Chart.js fixture asset"),
                );
            }
            for (index, resource) in outcome.capture.canvas_resources.iter().enumerate() {
                retain(&format!("canvas-{index}.png"), &resource.png);
            }
            retain(
                "resources.txt",
                format!("{:#?}\n", outcome.resources).as_bytes(),
            );
            retain(
                "session-evidence.json",
                &serde_json::to_vec_pretty(&serde_json::json!({
                    "readiness": &outcome.readiness,
                    "resource_accounting": {
                        "requests": outcome.resource_accounting.requests,
                        "loaded": outcome.resource_accounting.loaded,
                        "delegated": outcome.resource_accounting.delegated,
                        "failed": outcome.resource_accounting.failed,
                        "body_bytes": outcome.resource_accounting.body_bytes,
                        "unavailable_bodies": outcome.resource_accounting.unavailable_bodies,
                    },
                }))
                .unwrap(),
            );
        }
        let input_hash = content_address(&input_bytes);
        let scene_hash = content_address(scene);
        let pdf_hash = content_address(&outcome.pdf);
        let manifest = serde_json::json!({
            "schema": if case == "invoice-oracle" { "pliego.invoice-oracle-diagnostic" } else { "pliego.chartjs-oracle-diagnostic" }, "version": 1,
            "test": ISOLATED_TEST, "case": case,
            "source_commit": env!("PLIEGO_SOURCE_COMMIT"),
            "target": env!("PLIEGO_BUILD_TARGET"),
            "expected": {"input_sha256": expected_input, "scene_sha256": expected_scene,
                "pdf_sha256": expected_pdf, "pages": expected_pages},
            "actual": {"input_sha256": input_hash, "scene_sha256": scene_hash,
                "pdf_sha256": pdf_hash, "pages": outcome.capture.scene.pages.len()},
            "matches": {"input": input_hash == expected_input,
                "scene": scene_hash == expected_scene,
                "pdf": pdf_hash == expected_pdf,
                "pages": outcome.capture.scene.pages.len() == expected_pages},
            "canvas": if case == "chartjs-report" { Some(serde_json::json!({
                "expected": PRE_SESSION_CHARTJS_CANVAS,
                "actual": outcome.capture.canvas_resources.iter().map(|resource| content_address(&resource.png)).collect::<Vec<_>>(),
                "chartjs_umd_expected_sha256": CHARTJS_UMD,
            })) } else { None },
            "artifacts": artifacts,
            "scope": "Original SceneCapture and emitted bytes; no raw layout snapshot is retained by DocumentOutcome",
        });
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .expect("write oracle mismatch manifest before assertions");
        eprintln!("{case} evidence retained at {}", directory.display());
    }

    struct TempBundle(PathBuf);

    impl TempBundle {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "pliego-document-session-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, body).unwrap();
            path
        }

        fn copy(&self, source: &Path, name: &str) {
            fs::copy(source.join(name), self.0.join(name)).unwrap();
        }
    }

    impl Drop for TempBundle {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FixtureServer {
        base_url: String,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<io::Result<()>>>,
    }

    impl FixtureServer {
        fn start() -> Self {
            Self::start_with_metadata_evidence(None)
        }

        fn start_for_metadata_evidence(observed: Arc<AtomicBool>) -> Self {
            Self::start_with_metadata_evidence(Some(observed))
        }

        fn start_with_metadata_evidence(observed: Option<Arc<AtomicBool>>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                let mut request_error = None;
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(error) = handle_fixture_request(stream, observed.as_deref())
                            {
                                request_error.get_or_insert(error);
                            }
                        },
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        },
                        Err(error) => return Err(error),
                    }
                }
                request_error.map_or(Ok(()), Err)
            });
            Self {
                base_url: format!("http://{address}/"),
                stop,
                thread: Some(thread),
            }
        }

        fn shutdown(&mut self) -> io::Result<()> {
            self.stop.store(true, Ordering::Relaxed);
            let Some(thread) = self.thread.take() else {
                return Ok(());
            };
            match thread.join() {
                Ok(result) => result,
                Err(_) => Err(io::Error::other("fixture server thread panicked")),
            }
        }
    }

    impl Drop for FixtureServer {
        fn drop(&mut self) {
            let _ = self.shutdown();
        }
    }

    fn handle_fixture_request(
        mut stream: TcpStream,
        metadata_evidence_observed: Option<&AtomicBool>,
    ) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut request = Vec::new();
        let mut buffer = [0; 2048];
        while request.len() < 8192 {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&request);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/");
        let cookie_seen = request.lines().any(|line| {
            line.to_ascii_lowercase().starts_with("cookie:") &&
                line.contains("pliego_session_seed=1")
        });
        let (status, content_type, body) = match path {
            "/allowed.js" => ("200 OK", "text/javascript", ALLOWED_HTTP_BODY.to_vec()),
            path if path.starts_with("/metadata-evidence.js?") => (
                "200 OK",
                "text/javascript",
                format!(
                    "window.__pliegoMetadataEvidence({});\n",
                    metadata_evidence_observed
                        .is_some_and(|observed| observed.load(Ordering::SeqCst))
                )
                .into_bytes(),
            ),
            "/timeout.js" => {
                std::thread::sleep(Duration::from_millis(250));
                (
                    "200 OK",
                    "text/javascript",
                    b"window.pliego.ready({ timed_out: false });\n".to_vec(),
                )
            },
            "/seed-frame.html" => (
                "200 OK",
                "text/html; charset=utf-8",
                b"<!doctype html><script>document.cookie = 'pliego_session_seed=1; Path=/'; parent.postMessage({ iframe_cookie_persisted: document.cookie.split('; ').includes('pliego_session_seed=1') }, '*');</script>".to_vec(),
            ),
            "/clean-frame.html" => (
                "200 OK",
                "text/html; charset=utf-8",
                format!(
                    "<!doctype html><script>parent.postMessage({{ iframe_cookie_present: document.cookie.split('; ').includes('pliego_session_seed=1'), cookie_header_seen: {cookie_seen} }}, '*');</script>"
                )
                .into_bytes(),
            ),
            _ => ("404 Not Found", "application/octet-stream", Vec::new()),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let result = stream
            .write_all(response.as_bytes())
            .and_then(|()| stream.write_all(&body));
        match result {
            Err(error)
                if path == "/timeout.js" &&
                    matches!(
                        error.kind(),
                        io::ErrorKind::BrokenPipe |
                            io::ErrorKind::ConnectionAborted |
                            io::ErrorKind::ConnectionReset
                    ) =>
            {
                Ok(())
            },
            result => result,
        }
    }

    fn run_isolated(case: &str, http_base: &str) -> Output {
        run_isolated_test(ISOLATED_TEST, ISOLATED_CASE_ENV, case, Some(http_base))
    }

    fn run_isolated_test(
        test: &str,
        case_env: &str,
        case: &str,
        http_base: Option<&str>,
    ) -> Output {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env(case_env, case)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(http_base) = http_base {
            command.env(HTTP_BASE_ENV, http_base);
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stdout_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut stdout = stdout;
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut stderr = stderr;
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + Duration::from_secs(75);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return Output {
                    status,
                    stdout: stdout_reader.join().unwrap().unwrap(),
                    stderr: stderr_reader.join().unwrap().unwrap(),
                };
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let status = child.wait().unwrap();
                let output = Output {
                    status,
                    stdout: stdout_reader.join().unwrap().unwrap(),
                    stderr: stderr_reader.join().unwrap().unwrap(),
                };
                panic!(
                    "isolated {case} fixture exceeded 75 seconds\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn metadata_evidence_fixture(
        case: &str,
        icon_href: &str,
        probe_base: &str,
        body: &str,
    ) -> String {
        let case = serde_json::to_string(case).unwrap();
        let icon_href = serde_json::to_string(icon_href).unwrap();
        let probe_url =
            serde_json::to_string(&format!("{probe_base}metadata-evidence.js")).unwrap();
        format!(
            r#"<!doctype html>{body}<script>
window.pliego?.defer();
(() => {{
  const fixture = {case};
  const iconHref = {icon_href};
  const probeUrl = {probe_url};
  let faviconStarted = false;
  let probeCount = 0;

  const fail = message => window.pliego?.fail({{
    code: "FIXTURE_METADATA_EVIDENCE_FAILED",
    message,
  }});
  const probe = () => {{
    probeCount += 1;
    if (probeCount > 64) {{
      fail("favicon evidence was not observed within 64 probes");
      return;
    }}
    const script = document.createElement("script");
    script.src = `${{probeUrl}}?probe=${{probeCount}}`;
    script.addEventListener("error", () => fail("metadata evidence probe failed"), {{ once: true }});
    script.addEventListener("load", () => script.remove(), {{ once: true }});
    document.head.append(script);
  }};

  window.__pliegoMetadataEvidence = observed => {{
    if (!faviconStarted) {{
      if (observed) {{
        fail("favicon evidence existed before the fixture started the favicon request");
        return;
      }}
      faviconStarted = true;
      const favicon = document.createElement("link");
      favicon.rel = "icon";
      favicon.href = iconHref;
      document.head.append(favicon);
      queueMicrotask(probe);
      return;
    }}
    if (!observed) {{
      queueMicrotask(probe);
      return;
    }}
    window.pliego?.ready({{
      fixture,
      metadataEvidenceBeforeRequest: false,
      metadataEvidenceAtReadiness: true,
      metadataEvidenceProbes: probeCount,
    }});
  }};

  addEventListener("load", probe, {{ once: true }});
}})();
</script>"#
        )
    }

    fn assert_metadata_evidence_precedes_readiness(readiness: &serde_json::Value, case: &str) {
        assert_eq!(readiness["payload"]["fixture"], case);
        assert_eq!(readiness["payload"]["metadataEvidenceBeforeRequest"], false);
        assert_eq!(readiness["payload"]["metadataEvidenceAtReadiness"], true);
        assert!(
            readiness["payload"]["metadataEvidenceProbes"]
                .as_u64()
                .is_some_and(|probes| probes >= 2),
            "the fixture must prove absence before waiting for recorded metadata evidence"
        );
    }

    fn a4() -> PageDefinition {
        PageDefinition::new(
            793.7008,
            1122.5197,
            PageMargins::new(45.3543, 60.4724, 45.3543, 60.4724),
        )
        .expect("A4 document geometry should be valid")
    }

    fn content_address(bytes: &[u8]) -> String {
        format!(
            "sha256:{}",
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }

    fn resolved_render_job(
        entrypoint: &str,
        entries: &[(&str, &str, &[u8])],
        diagnostic_retention: &str,
    ) -> Result<(serde_json::Value, crate::api2::ResolvedRenderJob), crate::api2::InvocationError>
    {
        #[derive(serde::Serialize)]
        struct Manifest<'a> {
            schema: &'static str,
            version: u32,
            url_root: &'static str,
            entries: Vec<ManifestEntry<'a>>,
        }

        #[derive(serde::Serialize)]
        struct ManifestEntry<'a> {
            path: &'a str,
            media_type: &'a str,
            sha256: String,
            bytes: u64,
        }

        let mut ordered_entries = entries.to_vec();
        ordered_entries.sort_unstable_by_key(|(path, _, _)| *path);
        let manifest = Manifest {
            schema: "pliego.input-manifest",
            version: 1,
            url_root: FROZEN_INPUT_URL_ROOT,
            entries: ordered_entries
                .iter()
                .map(|(path, media_type, body)| ManifestEntry {
                    path,
                    media_type,
                    sha256: content_address(body),
                    bytes: body.len() as u64,
                })
                .collect(),
        };
        let mut canonical_manifest = serde_json::to_vec(&manifest).unwrap();
        canonical_manifest.push(b'\n');

        let mut request: serde_json::Value = serde_json::from_slice(API2_REQUEST).unwrap();
        request["input"]["entrypoint"] = serde_json::Value::from(entrypoint);
        request["input"]["manifest"]["sha256"] =
            serde_json::Value::from(content_address(&canonical_manifest));
        request["input"]["manifest"]["bytes"] =
            serde_json::Value::from(canonical_manifest.len() as u64);
        request["diagnostics"]["retention"] = serde_json::Value::from(diagnostic_retention);

        let mut bodies = BTreeMap::new();
        for (path, _, body) in entries {
            assert!(bodies.insert((*path).to_owned(), body.to_vec()).is_none());
        }
        let job = crate::api2::resolve_render_job_for_test(&request, &canonical_manifest, bodies)?;
        Ok((request, job))
    }

    fn fixture_png() -> Vec<u8> {
        BASE64_STANDARD
            .decode(FIXTURE_PNG_DATA_URL.split_once(',').unwrap().1)
            .unwrap()
    }

    fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
        image::load_from_memory(bytes)
            .expect("retained stable frame should decode as an image")
            .to_rgba8()
            .dimensions()
    }

    fn session_fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/document-session")
            .join(name)
            .canonicalize()
            .expect("committed DocumentSession fixture should exist")
    }

    #[test]
    fn missing_document_returns_session_error() {
        let error = DocumentSession::new(
            "__pliego_missing_document__.html",
            RenderEnvironment::default(),
            a4(),
            ResourcePolicyConfig::default(),
            false,
            ReadinessPolicy::default(),
        )
        .err()
        .expect("missing input should return a typed error");

        assert_eq!(error.code, "INVALID_REQUEST");
        assert!(error.message.contains("document is unavailable"));
    }

    #[test]
    fn invalid_resource_timeout_is_rejected_before_servo_starts() {
        let error = DocumentSession::new(
            "__pliego_missing_document__.html",
            RenderEnvironment::default(),
            a4(),
            ResourcePolicyConfig {
                timeout_ms: 0,
                ..ResourcePolicyConfig::default()
            },
            false,
            ReadinessPolicy::default(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "INVALID_REQUEST");
        assert_eq!(
            error.message,
            "resource timeout must be between 1 and 60000 milliseconds"
        );
    }

    #[test]
    fn invalid_controlled_resource_timeout_is_rejected_before_servo_starts() {
        let bundle = TempBundle::new("invalid-controlled-resource-timeout");
        let input = bundle.write("input.html", "<!doctype html><p>timeout</p>");

        for timeout_ms in [0, MAX_RESOURCE_TIMEOUT_MS + 1] {
            let error = DocumentSession::new_controlled(
                &input,
                RenderEnvironment::default(),
                a4(),
                ResourcePolicyConfig {
                    timeout_ms,
                    ..ResourcePolicyConfig::default()
                },
                false,
                ReadinessPolicy::default(),
                DeterministicRuntimePolicy::default(),
            )
            .err()
            .expect("invalid controlled resource timeout must fail before Servo starts");
            assert_eq!(error.code, "INVALID_REQUEST");
            assert_eq!(
                error.message,
                "resource timeout must be between 1 and 60000 milliseconds"
            );
        }
    }

    #[test]
    fn session_host_deadline_is_one_non_resetting_budget() {
        let readiness = ReadinessPolicy {
            timeout_ms: 60_000,
            wait_for_fonts: true,
        };
        let timeout = session_host_timeout(readiness).unwrap();
        assert_eq!(
            timeout,
            super::TIMEOUT + std::time::Duration::from_millis(readiness.timeout_ms)
        );

        let started = Instant::now();
        let host_deadline = SessionHostDeadline::from_started(started, timeout).unwrap();
        let after_document_load = started + std::time::Duration::from_secs(20);
        let after_stable_render = after_document_load + std::time::Duration::from_secs(35);

        assert_eq!(
            host_deadline.remaining_at(after_document_load),
            timeout - std::time::Duration::from_secs(20)
        );
        assert_eq!(
            host_deadline.remaining_at(after_stable_render),
            timeout - std::time::Duration::from_secs(55)
        );
        assert_eq!(host_deadline.deadline, started + timeout);
        assert!(!host_deadline.is_elapsed_at(started + timeout - Duration::from_nanos(1)));
        assert!(host_deadline.is_elapsed_at(started + timeout));
    }

    #[test]
    fn resolved_resource_policy_root_mismatch_fails_before_servo_construction() {
        let document_bundle = TempBundle::new("resolved-root-document");
        document_bundle.write("input.html", "<!doctype html><p>document</p>");
        let policy_bundle = TempBundle::new("resolved-root-policy");
        let document = LocalDocument::resolve(&document_bundle.0, "input.html").unwrap();
        let matching_policy =
            ResourcePolicy::resolve(&ResourcePolicyConfig::default(), document.root());
        assert_eq!(
            matching_policy.resolved_document_root(),
            Some(document.root())
        );
        assert!(validate_resolved_resource_policy(&document, &matching_policy).is_ok());
        let resource_policy =
            ResourcePolicy::resolve(&ResourcePolicyConfig::default(), &policy_bundle.0);

        let error = DocumentSession::from_resolved(
            &document,
            resource_policy,
            RenderEnvironment::default(),
            a4(),
            false,
            ReadinessPolicy::default(),
        )
        .err()
        .expect("mismatched roots must fail before Servo construction");

        assert_eq!(error.code, "INVALID_REQUEST");
        assert_eq!(
            error.message,
            "resource policy document root does not match the resolved document root"
        );
        assert!(error.resources.is_empty());
        assert!(error.console.is_empty());
    }

    #[test]
    fn console_evidence_is_bounded_before_retention() {
        let mut events = ConsoleEvidenceLog::default();
        for index in 0..MAX_CONSOLE_EVENTS {
            events.push("info".into(), index.to_string());
        }
        assert_eq!(events.entries.len(), MAX_CONSOLE_EVENTS);
        assert!(!events.limit_exceeded);
        events.push("info".into(), "excess".into());
        assert_eq!(events.entries.len(), MAX_CONSOLE_EVENTS);
        assert!(events.limit_exceeded);

        let mut bytes = ConsoleEvidenceLog::default();
        bytes.push("info".into(), "x".repeat(MAX_CONSOLE_BYTES as usize));
        assert!(bytes.entries.is_empty());
        assert!(bytes.limit_exceeded);
    }

    #[test]
    fn console_levels_use_the_serialized_contract_names() {
        for (level, expected) in [
            (ConsoleLogLevel::Log, "log"),
            (ConsoleLogLevel::Debug, "debug"),
            (ConsoleLogLevel::Info, "info"),
            (ConsoleLogLevel::Warn, "warn"),
            (ConsoleLogLevel::Error, "error"),
            (ConsoleLogLevel::Trace, "trace"),
            (ConsoleLogLevel::Dir, "dir"),
        ] {
            assert_eq!(console_log_level_name(level), expected);
        }
    }

    #[test]
    fn resource_and_readiness_fixtures_are_evidenced_and_fail_closed() {
        let mut server = FixtureServer::start();
        for case in [
            "constructor-recovery",
            "console-evidence",
            "console-overflow",
            "local-success",
            "resolved-root",
            "virtual-success",
            "asset-cache",
            "allowed-url",
            "raster-local",
            "raster-data",
            "invalid-data-url",
            "oversized-raster",
            "denied-url",
            "http-timeout",
            "explicit-fail",
            "defer-timeout",
            "environment",
            "invoice-oracle",
            "hybrid-canvas",
            "hybrid-canvas", // a fresh process must reproduce the exact oracle
            "unsupported-canvas",
            "canvas-retention-budget",
            "canvas-missing-snapshot",
            "state-seed",
            "state-clean",
        ] {
            let output = run_isolated(case, &server.base_url);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "isolated {case} fixture failed\nstdout:\n{}\nstderr:\n{}",
                stdout,
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(
                stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
                "isolated {case} filter did not execute exactly one passing child test:\n{stdout}",
            );
        }
        server.shutdown().unwrap();
    }

    #[test]
    fn resource_load_roles_are_source_assigned_and_cache_isolated() {
        for case in [
            "metadata-denied-non-icon",
            "content-favicon-name",
            "same-url-role-split",
            "preload-image-content",
            "metadata-allowed-icon",
        ] {
            let output = run_isolated(case, "http://127.0.0.1:1/");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "isolated {case} fixture failed\nstdout:\n{}\nstderr:\n{}",
                stdout,
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(
                stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
                "isolated {case} filter did not execute exactly one passing child test:\n{stdout}",
            );
        }
    }

    #[test]
    fn controlled_session_bridges_api1_readiness_before_generation_bound_capture() {
        for case in [
            "controlled-local-success",
            "controlled-resize-observer",
            "controlled-finite",
            "controlled-paint-mutation",
            "controlled-readiness-retry",
            "controlled-readiness-fail",
            "controlled-readiness-timeout",
            "controlled-interval",
        ] {
            let output = run_isolated(case, "http://127.0.0.1:1/");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "isolated {case} fixture failed\nstdout:\n{}\nstderr:\n{}",
                stdout,
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(
                stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
                "isolated {case} filter did not execute exactly one passing child test:\n{stdout}",
            );
            if case == "controlled-finite" {
                assert!(
                    stdout.contains("controlled-capture-complete"),
                    "controlled capture did not finish Servo teardown:\n{stdout}",
                );
            }
            if case == "controlled-paint-mutation" {
                assert!(
                    stdout.contains("controlled-paint-mutation-rejected"),
                    "Paint mutation was not rejected before readback:\n{stdout}",
                );
            }
        }
    }

    #[test]
    fn api2_execution_constructor_accepts_only_a_resolved_job() {
        let _: fn(crate::api2::ResolvedRenderJob) -> Result<Api2Execution, SessionError> =
            DocumentSession::start_api2_execution;
    }

    #[test]
    fn pliego_input_url_uses_only_frozen_virtual_resources_and_fails_closed() {
        for case in ["success", "unlisted", "scheme"] {
            let output = run_isolated_test(
                PLIEGO_INPUT_ISOLATED_TEST,
                PLIEGO_INPUT_CASE_ENV,
                case,
                None,
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "isolated pliego-input {case} fixture failed\nstdout:\n{}\nstderr:\n{}",
                stdout,
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(
                stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
                "isolated pliego-input {case} filter did not execute exactly one passing child test:\n{stdout}",
            );
        }
    }

    #[test]
    #[ignore = "launched in a fresh process by the pliego-input fixture orchestrator"]
    fn isolated_pliego_input_url_fixture() {
        let case =
            std::env::var(PLIEGO_INPUT_CASE_ENV).expect("pliego-input fixture case should be set");
        let input_url = url::Url::parse("pliego-input:///entry.html").unwrap();
        let css_url = url::Url::parse("pliego-input:///styles.css").unwrap();
        let font_url = url::Url::parse("pliego-input:///Ahem.ttf").unwrap();
        let payload_url = url::Url::parse("pliego-input:///payload.json").unwrap();
        let missing_url = url::Url::parse("pliego-input:///missing.js").unwrap();
        let file_url = url::Url::parse("file:///definitely-host-owned/pliego-secret.js").unwrap();
        let css = br#"@font-face {
  font-family: "Pliego API2 Ahem";
  src: url("Ahem.ttf");
}
#marker {
  color: rgb(1, 2, 3);
  font: 16px/20px "Pliego API2 Ahem";
}
"#;
        let payload = br#"{"expediente":"host-path-free"}"#;
        let input = match case.as_str() {
            "success" => br#"<!doctype html>
<meta charset="utf-8">
<link rel="stylesheet" href="styles.css">
<p id="marker">PLIEGO_INPUT_MARKER</p>
<script>
window.pliego.defer();
document.fonts.ready
  .then(() => fetch("payload.json"))
  .then(response => response.text())
  .then(body => requestAnimationFrame(() => window.pliego.ready({
    href: location.href,
    body,
    color: getComputedStyle(document.getElementById("marker")).color,
  })))
  .catch(error => window.pliego.fail({
    code: "PLIEGO_INPUT_FETCH_FAILED",
    message: String(error),
  }));
</script>
"#
            .as_slice(),
            "unlisted" => br#"<!doctype html>
<meta charset="utf-8">
<script src="missing.js"></script>
<p>this document must never reach readiness</p>
<script>window.pliego.ready({});</script>
"#
            .as_slice(),
            "scheme" => br#"<!doctype html>
<meta charset="utf-8">
<script src="file:///definitely-host-owned/pliego-secret.js"></script>
<p>this document must never reach readiness</p>
<script>window.pliego.ready({});</script>
"#
            .as_slice(),
            _ => panic!("unknown pliego-input fixture case: {case}"),
        };
        let mut entries = vec![("entry.html", "text/html;charset=utf-8", input)];
        if case == "success" {
            entries.extend([
                ("Ahem.ttf", "font/ttf", API2_AHEM_TTF),
                ("styles.css", "text/css;charset=utf-8", css.as_slice()),
                ("payload.json", "application/json", payload.as_slice()),
            ]);
        }
        let diagnostic_retention = match case.as_str() {
            "success" => "none",
            "unlisted" => "on-failure",
            "scheme" => "always",
            _ => unreachable!(),
        };
        let (expected_request, job) =
            resolved_render_job("entry.html", &entries, diagnostic_retention)
                .expect("the in-memory fixture should pass the full API 2 input validators");

        let execution = DocumentSession::start_api2_execution(job)
            .expect("the resolved API 2 input should construct a frozen URL session");
        assert_eq!(
            expected_request["page"]["geometry_authority"],
            "request-only-v1"
        );
        let controlled = execution.controlled;
        assert!(
            controlled
                .session
                .delegate
                .bundle_root
                .as_os_str()
                .is_empty()
        );
        assert!(
            controlled
                .session
                .delegate
                .resource_policy
                .resolved_document_root()
                .is_none()
        );
        let frozen = controlled
            .session
            .delegate
            .frozen_input_authority
            .as_ref()
            .expect("the API 2 path must own an exclusive frozen input authority");
        let mut expected_urls = entries
            .iter()
            .map(|(path, _, _)| format!("{FROZEN_INPUT_URL_ROOT}{path}"))
            .collect::<Vec<_>>();
        expected_urls.sort();
        assert_eq!(
            frozen.resources.keys().cloned().collect::<Vec<_>>(),
            expected_urls
        );

        if case != "success" {
            let denied_url = if case == "unlisted" {
                &missing_url
            } else {
                &file_url
            };
            let error = controlled
                .prepare_capture_candidate()
                .err()
                .expect("an unlisted or foreign-scheme resource must fail closed");
            assert_eq!(error.code, "RESOURCE_DENIED");
            let failure = error
                .resource_failure
                .as_ref()
                .expect("denial should retain structured failure evidence");
            assert_eq!(failure.url, denied_url.as_str());
            assert!(failure.fatal);
            assert_eq!(error.resource_accounting.loaded, 1);
            assert_eq!(error.resource_accounting.failed, 1);
            assert_eq!(
                error.resource_store.resolve_url(input_url.as_str()),
                Some(content_address(input))
            );
            assert!(
                error
                    .resource_store
                    .resolve_url(denied_url.as_str())
                    .is_none()
            );
            let entry_evidence = error
                .resources
                .iter()
                .find(|evidence| evidence.request.url == input_url)
                .expect("the frozen entrypoint must retain exact evidence before denial");
            assert_eq!(
                entry_evidence.content_type.as_deref(),
                Some("text/html;charset=utf-8")
            );
            assert_eq!(
                entry_evidence.content_address.as_deref(),
                Some(content_address(input).as_str())
            );
            return;
        }

        let outcome = controlled
            .prepare_capture_candidate()
            .expect("frozen entrypoint, CSS, and fetch should settle")
            .capture()
            .expect("the host-path-free pliego-input document should capture");
        let encoded = crate::api2::encode_profile_null_scene(
            &expected_request,
            &outcome.capture,
            |resource| outcome.resource_store.resolve_content(resource),
        )
        .expect("the exact controlled capture should encode as a profile-null API 2 scene");
        assert_eq!(
            encoded.media_type,
            "application/vnd.pliego.document-scene+json"
        );
        assert_eq!(encoded.sha256, content_address(&encoded.bytes));
        let scene: serde_json::Value = serde_json::from_slice(&encoded.bytes)
            .expect("the canonical scene should be valid JSON");
        assert_eq!(scene["schema"], "pliego.document-scene");
        assert_eq!(scene["version"], 2);
        assert_eq!(scene["request_page"], expected_request["page"]);
        assert!(scene["semantic_layer"].is_null());
        let operations = scene["pages"]
            .as_array()
            .expect("the canonical scene should contain pages")
            .iter()
            .flat_map(|page| {
                page["operations"]
                    .as_array()
                    .expect("every canonical page should contain operations")
            })
            .collect::<Vec<_>>();
        let marker = operations
            .iter()
            .find(|operation| {
                operation["type"] == "text" &&
                    operation["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("PLIEGO_INPUT_MARKER"))
            })
            .expect("the canonical scene should retain the marker text");
        let marker_font_resource = marker["font"]["resource"]
            .as_str()
            .expect("the marker text should bind a public font resource");
        assert_eq!(content_address(API2_AHEM_TTF), AHEM_SOURCE_RESOURCE);
        assert_eq!(marker_font_resource, AHEM_CAPTURED_RESOURCE);
        let marker_font = encoded
            .resources
            .get(marker_font_resource)
            .expect("the marker font resource should be retained");
        assert_eq!(marker_font.media_type, "application/octet-stream");
        assert_eq!(content_address(&marker_font.bytes), marker_font_resource);
        assert!(encoded.bytes.ends_with(b"\n"));
        assert_eq!(outcome.readiness["status"], "ready");
        assert_eq!(outcome.readiness["payload"]["href"], input_url.as_str());
        assert_eq!(
            outcome.readiness["payload"]["body"],
            String::from_utf8_lossy(payload).as_ref()
        );
        assert_eq!(outcome.readiness["payload"]["color"], "rgb(1, 2, 3)");
        assert_eq!(outcome.resource_accounting.requests, 4);
        assert_eq!(outcome.resource_accounting.loaded, 4);
        assert_eq!(outcome.resource_accounting.delegated, 0);
        assert_eq!(outcome.resource_accounting.failed, 0);

        for (url, media_type, body) in [
            (&input_url, "text/html;charset=utf-8", input),
            (&font_url, "font/ttf", API2_AHEM_TTF),
            (&css_url, "text/css;charset=utf-8", css.as_slice()),
            (&payload_url, "application/json", payload.as_slice()),
        ] {
            let evidence = outcome
                .resources
                .iter()
                .find(|evidence| evidence.request.url == *url)
                .expect("each frozen virtual resource should retain evidence");
            let address = content_address(body);
            assert_eq!(evidence.source, Some(ResourceSource::VirtualResource));
            assert_eq!(evidence.status, "loaded");
            assert_eq!(evidence.content_type.as_deref(), Some(media_type));
            assert_eq!(evidence.sha256.as_deref(), Some(&address[7..]));
            assert_eq!(evidence.content_address.as_deref(), Some(address.as_str()));
            assert_eq!(
                outcome.resource_store.resolve_url(url.as_str()),
                Some(address.clone())
            );
            assert_eq!(outcome.resource_store.resolve_content(&address), Some(body));
            assert_eq!(url.scheme(), "pliego-input");
            assert!(url.host_str().is_none());
        }
    }

    #[test]
    fn resolved_input_job_rejects_non_utf8_canonical_html() {
        let error = resolved_render_job(
            "entry.html",
            &[("entry.html", "text/html;charset=utf-8", &[0xff, 0xfe])],
            "always",
        )
        .expect_err("a canonical UTF-8 HTML declaration must reject non-UTF-8 bytes");
        assert!(
            error
                .to_string()
                .contains("entrypoint \"entry.html\" is not valid UTF-8")
        );
    }

    #[test]
    fn frozen_input_transfers_a_body_above_half_the_inclusive_content_bound() {
        let body_len = (crate::api2::INPUT_CONTENT_MAX_BYTES / 2 + 1) as usize;
        let body = vec![b' '; body_len];
        let (_, job) = resolved_render_job(
            "entry.html",
            &[("entry.html", "text/html;charset=utf-8", &body)],
            "always",
        )
        .expect("the body should be inside the inclusive API 2 content bound");
        let parts = job.into_parts();
        let (input_url, authority) = super::FrozenInputAuthority::from_resolved_job(parts.input)
            .expect("the validated body should map into the frozen authority");
        let mut store =
            super::owned_resource_store_for_session(&ResourcePolicy::default(), Some(&authority))
                .expect(
                    "frozen input should own the initial body without reserving it in the store",
                );
        assert_eq!(store.resident_bytes(), 0);

        let request = ResourceRequest {
            method: "GET".into(),
            url: input_url.clone(),
            destination: "Document".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: None,
            is_for_main_frame: true,
            is_redirect: false,
        };
        let mut head_request = request.clone();
        head_request.method = "HEAD".into();
        let (head, _) = authority.resolve(&head_request, &store).unwrap();
        assert!(head.body.is_empty());
        assert_eq!(
            authority.resources[input_url.as_str()]
                .body
                .borrow()
                .as_ref()
                .map(Vec::len),
            Some(body_len)
        );

        let expected_address = content_address(&body);
        let (first, first_headers) = authority.resolve(&request, &store).unwrap();
        assert_eq!(first.body.len(), body_len);
        assert_eq!(content_address(&first.body), expected_address);
        assert!(
            authority.resources[input_url.as_str()]
                .body
                .borrow()
                .is_none()
        );
        store
            .retain_with_source(
                &request,
                ResourceSource::VirtualResource,
                first,
                &first_headers,
            )
            .unwrap();
        assert_eq!(store.resident_bytes(), body_len as u64);

        let (repeated, repeated_headers) = authority.resolve(&request, &store).unwrap();
        assert_eq!(repeated.body.len(), body_len);
        assert_eq!(content_address(&repeated.body), expected_address);
        assert_eq!(repeated_headers, first_headers);
        store
            .retain_with_source(
                &request,
                ResourceSource::VirtualResource,
                repeated,
                &repeated_headers,
            )
            .unwrap();
        assert_eq!(store.resident_bytes(), body_len as u64);
        assert_eq!(
            store.resolve_url(input_url.as_str()),
            Some(expected_address)
        );
    }

    #[test]
    #[ignore = "requires the lockfile-installed Chart.js fixture input"]
    fn installed_chartjs_fixture_matches_the_reviewed_snapshot_twice() {
        let input = std::env::var(CHARTJS_INPUT_ENV)
            .expect("PLIEGO_DOCUMENT_SESSION_CHARTJS_INPUT should name the installed fixture");
        assert!(Path::new(&input).is_file());
        for _ in 0..2 {
            let output = run_isolated("chartjs-report", "http://127.0.0.1:1/");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "isolated Chart.js fixture failed\nstdout:\n{}\nstderr:\n{}",
                stdout,
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(
                stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
                "isolated Chart.js filter did not execute exactly one passing child test:\n{stdout}",
            );
        }
    }

    #[test]
    #[ignore = "launched in a fresh process by the fixture orchestrator"]
    fn isolated_resource_and_readiness_fixture() {
        let case = std::env::var(ISOLATED_CASE_ENV).expect("isolated fixture case should be set");
        let http_base = std::env::var(HTTP_BASE_ENV).expect("HTTP fixture base should be set");
        let mut readiness = ReadinessPolicy {
            timeout_ms: match case.as_str() {
                "defer-timeout" | "controlled-readiness-timeout" => 25,
                "metadata-denied-non-icon" | "same-url-role-split" | "metadata-allowed-icon" => {
                    10_000
                },
                _ => 1_000,
            },
            wait_for_fonts: false,
        };
        let mut resources = ResourcePolicyConfig::default();
        let metadata_evidence_observed = matches!(
            case.as_str(),
            "metadata-denied-non-icon" | "same-url-role-split" | "metadata-allowed-icon"
        )
        .then(|| Arc::new(AtomicBool::new(false)));
        let _metadata_probe_server = metadata_evidence_observed
            .as_ref()
            .map(|observed| FixtureServer::start_for_metadata_evidence(Arc::clone(observed)));
        let metadata_probe_base = _metadata_probe_server
            .as_ref()
            .map(|server| server.base_url.as_str())
            .unwrap_or(http_base.as_str());
        if let Some(server) = _metadata_probe_server.as_ref() {
            resources
                .allowed_http_roots
                .push(url::Url::parse(&server.base_url).unwrap());
        }
        let mut environment = RenderEnvironment::default();
        let mut allow_host_fonts = false;
        let mut _bundle = None;
        let input = match case.as_str() {
            "constructor-recovery" => Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/deterministic-environment/index.html")
                .canonicalize()
                .unwrap(),
            "local-success" | "denied-url" | "explicit-fail" | "defer-timeout" => {
                session_fixture(&format!("{case}.html"))
            },
            "console-evidence" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><script>console.info('capture-first');console.error('capture-second');window.pliego?.ready({fixture:'console-evidence'});</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "console-overflow" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    format!(
                        "<!doctype html><script>for(let index=0;index<={MAX_CONSOLE_EVENTS};index+=1){{console.info('overflow-'+index);}}window.pliego?.ready({{fixture:'console-overflow'}});</script>"
                    ),
                );
                _bundle = Some(bundle);
                input
            },
            "controlled-local-success" => {
                let bundle = TempBundle::new(case.as_str());
                let fixture_root = session_fixture("local-success.html")
                    .parent()
                    .unwrap()
                    .to_path_buf();
                bundle.copy(&fixture_root, "local-success.html");
                bundle.copy(&fixture_root, "local-success.js");
                let input = bundle.0.join("local-success.html");
                _bundle = Some(bundle);
                input
            },
            "controlled-resize-observer" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    r#"<!doctype html>
<div id="target" style="width:96px;height:48px"></div>
<script>
window.pliego.defer();
document.fonts.ready.then(() => {
    requestAnimationFrame(() => {
        requestAnimationFrame(() => {
            const observer = new ResizeObserver(() => {});
            observer.observe(document.getElementById("target"));
            window.controlledResizeObserver = observer;
            window.pliego.ready({ fixture: "controlled-resize-observer" });
        });
    });
});
</script>"#,
                );
                _bundle = Some(bundle);
                input
            },
            "controlled-finite" | "controlled-paint-mutation" => {
                let bundle = TempBundle::new(case.as_str());
                bundle.copy(
                    &Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../benchmarks/fixtures/minimal-static"),
                    "Ahem.ttf",
                );
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><style>@font-face{font-family:Ahem;src:url('Ahem.ttf')}#state{font:12px/16px Ahem}</style><p id='state'>pending</p><script>window.pliego.defer();const paintObserver=new PerformanceObserver(list=>{const fcp=list.getEntries().find(entry=>entry.name==='first-contentful-paint');if(!fcp)return;document.getElementById('state').textContent=`PLIEGO_FCP_${fcp.startTime}`;console.info(`controlled-paint-observed:${fcp.name}:${fcp.startTime}:${fcp.duration}:${performance.now()}`);paintObserver.disconnect();});paintObserver.observe({type:'paint'});console.info(`controlled-start:${Date.now()}:${performance.now()}`);requestAnimationFrame(()=>console.info('controlled-frame'));setTimeout(()=>{document.getElementById('state').textContent='PLIEGO_POST5MS_UNIT_7C4E';console.info(`controlled-end:${Date.now()}:${performance.now()}`);window.pliego.ready({fixture:'controlled-finite',page:{rows:7,label:'authored'}});},5);</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "controlled-readiness-retry" => {
                let bundle = TempBundle::new(case.as_str());
                bundle.copy(
                    &Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../benchmarks/fixtures/minimal-static"),
                    "Ahem.ttf",
                );
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><style>@font-face{font-family:Ahem;src:url('Ahem.ttf')}#state{font:12px/16px Ahem}</style><p id='state'>revision 1</p><script>window.readinessPayload={fixture:'controlled-readiness-retry',revision:1};window.pliego.ready(window.readinessPayload);</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "controlled-readiness-fail" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><script>window.pliego.fail({code:'CONTROLLED_FIXTURE_FAILED',message:'controlled readiness rejected'});</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "controlled-readiness-timeout" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><script>window.pliego.defer();</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "controlled-interval" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><script>window.pliego.ready({fixture:'controlled-interval'});setInterval(()=>{},100);</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "resolved-root" => {
                let bundle = TempBundle::new(case.as_str());
                fs::create_dir_all(bundle.0.join("sub")).unwrap();
                bundle.write(
                    "sibling.js",
                    "window.pliego?.ready({fixture:'resolved-root'});",
                );
                let input = bundle.write(
                    "sub/input.html",
                    "<!doctype html><script src='../sibling.js'></script>",
                );
                _bundle = Some(bundle);
                input
            },
            "virtual-success" => {
                resources.virtual_resources.push(VirtualResourceSpec {
                    url: url::Url::parse("https://virtual.invalid/local-success.js").unwrap(),
                    path: PathBuf::from("local-success.js"),
                });
                session_fixture("virtual-success.html")
            },
            "asset-cache" => {
                let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/asset-cache")
                    .canonicalize()
                    .unwrap();
                let bundle = TempBundle::new(case.as_str());
                for name in ["index.html", "assets.json", "first.js", "renamed.js"] {
                    bundle.copy(&source, name);
                }
                resources.asset_manifest = Some(PathBuf::from("assets.json"));
                let input = bundle.0.join("index.html");
                _bundle = Some(bundle);
                input
            },
            "raster-local" => {
                let bundle = TempBundle::new(case.as_str());
                bundle.write("pixel.png", fixture_png());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><img src='pixel.png' alt=''><script>window.pliego?.ready({fixture:'raster-local'});</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "raster-data" => {
                readiness = ReadinessPolicy::default();
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/text-scene/index.html")
                    .canonicalize()
                    .unwrap()
            },
            "metadata-denied-non-icon" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    metadata_evidence_fixture(
                        case.as_str(),
                        "https://denied.invalid/report.bin",
                        metadata_probe_base,
                        "",
                    ),
                );
                _bundle = Some(bundle);
                input
            },
            "content-favicon-name" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><img src='https://denied.invalid/favicon.ico' alt=''><script>window.pliego?.ready({fixture:'content-favicon-name'});</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "same-url-role-split" => {
                let bundle = TempBundle::new(case.as_str());
                bundle.write("shared.png", fixture_png());
                let input = bundle.write(
                    "input.html",
                    metadata_evidence_fixture(
                        case.as_str(),
                        "shared.png",
                        metadata_probe_base,
                        "<img src='shared.png' width='1' height='1' alt=''>",
                    ),
                );
                _bundle = Some(bundle);
                input
            },
            "preload-image-content" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><link rel='preload' as='image' href='https://denied.invalid/preload.png'><script>window.pliego?.ready({fixture:'preload-image-content'});</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "metadata-allowed-icon" => {
                let bundle = TempBundle::new(case.as_str());
                bundle.write("icon.png", fixture_png());
                let input = bundle.write(
                    "input.html",
                    metadata_evidence_fixture(case.as_str(), "icon.png", metadata_probe_base, ""),
                );
                _bundle = Some(bundle);
                input
            },
            "invalid-data-url" => {
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><img src='data:image/png;base64,!'><script>window.pliego?.ready({fixture:'invalid-data-url'});</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "oversized-raster" => {
                let bundle = TempBundle::new(case.as_str());
                let mut png = fixture_png();
                png[16..20].copy_from_slice(&16_385_u32.to_be_bytes());
                bundle.write("oversized.png", png);
                let input = bundle.write(
                    "input.html",
                    "<!doctype html><img src='oversized.png' alt=''><script>window.pliego?.ready({fixture:'oversized-raster'});</script>",
                );
                _bundle = Some(bundle);
                input
            },
            "allowed-url" | "http-timeout" | "state-seed" | "state-clean" => {
                let template = session_fixture(&format!("{case}.html"));
                let body = fs::read_to_string(template)
                    .unwrap()
                    .replace("__BASE_URL__", &http_base);
                let bundle = TempBundle::new(case.as_str());
                let input = bundle.write("input.html", body);
                resources
                    .allowed_http_roots
                    .push(url::Url::parse(&http_base).unwrap());
                if case == "http-timeout" {
                    resources.timeout_ms = 25;
                }
                _bundle = Some(bundle);
                input
            },
            "environment" => {
                environment = RenderEnvironment {
                    locale: "es-MX",
                    timezone: "PST8PDT",
                };
                allow_host_fonts = true;
                readiness.wait_for_fonts = true;
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/deterministic-environment/index.html")
                    .canonicalize()
                    .unwrap()
            },
            "invoice-oracle" => {
                readiness = ReadinessPolicy::default();
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../benchmarks/fixtures/invoice-showcase/input.html")
                    .canonicalize()
                    .unwrap()
            },
            "hybrid-canvas" | "canvas-missing-snapshot" => {
                readiness = ReadinessPolicy::default();
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/hybrid-canvas/live.html")
                    .canonicalize()
                    .unwrap()
            },
            "unsupported-canvas" => session_fixture("unsupported-canvas.html"),
            "canvas-retention-budget" => session_fixture("canvas-retention-budget.html"),
            "chartjs-report" => {
                readiness = ReadinessPolicy::default();
                PathBuf::from(
                    std::env::var(CHARTJS_INPUT_ENV)
                        .expect("installed Chart.js fixture input should be set"),
                )
                .canonicalize()
                .expect("installed Chart.js fixture input should exist")
            },
            other => panic!("unknown isolated fixture case: {other}"),
        };
        if case == "constructor-recovery" {
            let error = DocumentSession::new(
                &input,
                RenderEnvironment {
                    locale: "en-US",
                    timezone: "UTC\0invalid",
                },
                a4(),
                resources.clone(),
                false,
                readiness,
            )
            .err()
            .expect("invalid timezone must fail session construction");
            assert_eq!(error.code, "ENVIRONMENT_CONFIGURATION_FAILED");

            let session = DocumentSession::new(
                &input,
                RenderEnvironment {
                    locale: "en-US",
                    timezone: "PST8PDT",
                },
                a4(),
                resources.clone(),
                false,
                readiness,
            )
            .expect("a fallible setup failure must not consume layout configuration");

            let different_page =
                PageDefinition::new(816.0, 1056.0, PageMargins::new(48.0, 48.0, 48.0, 48.0))
                    .unwrap();
            let conflict = DocumentSession::new(
                &input,
                RenderEnvironment::default(),
                different_page,
                resources,
                false,
                readiness,
            )
            .err()
            .expect("a second session must be rejected before mutating process state");
            assert_eq!(conflict.code, "LAYOUT_CONFIGURATION_FAILED");

            let outcome = session
                .render()
                .expect("the reserved session must still render after a rejected competitor");
            assert_eq!(outcome.readiness["payload"]["localHour"], 4);
            return;
        }
        let page = match case.as_str() {
            "hybrid-canvas" |
            "unsupported-canvas" |
            "canvas-retention-budget" |
            "canvas-missing-snapshot" => {
                PageDefinition::new(200.0, 160.0, PageMargins::new(10.0, 10.0, 10.0, 10.0)).unwrap()
            },
            "chartjs-report" => {
                PageDefinition::new(760.0, 840.0, PageMargins::new(28.0, 28.0, 28.0, 28.0)).unwrap()
            },
            _ => a4(),
        };
        if matches!(
            case.as_str(),
            "controlled-local-success" |
                "controlled-resize-observer" |
                "controlled-finite" |
                "controlled-paint-mutation" |
                "controlled-readiness-retry" |
                "controlled-readiness-fail" |
                "controlled-readiness-timeout" |
                "controlled-interval"
        ) {
            // Controlled fixtures exercise the production-default font readiness policy.
            readiness.wait_for_fonts = true;
            let bundle_files = || {
                let mut files = fs::read_dir(input.parent().unwrap())
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>();
                files.sort();
                files
            };
            let files_before = bundle_files();
            let mut runtime_policy = DeterministicRuntimePolicy::default();
            if matches!(
                case.as_str(),
                "controlled-local-success" | "controlled-resize-observer"
            ) {
                runtime_policy.settlement.limits.ordinary_tasks = 256;
            }
            let controlled = DocumentSession::new_controlled(
                &input,
                environment,
                page,
                resources,
                allow_host_fonts,
                readiness,
                runtime_policy,
            )
            .expect("controlled fixture should construct with the API 1 readiness shim");
            match case.as_str() {
                "controlled-resize-observer" => {
                    let candidate = controlled.prepare_capture_candidate().expect(
                        "a late no-op ResizeObserver should reach generation-bound candidate evidence",
                    );
                    let authored_readiness = candidate.readiness.clone();
                    assert_eq!(authored_readiness["status"], "ready");
                    assert_eq!(authored_readiness["font_status"], "loaded");
                    assert_eq!(
                        authored_readiness["payload"]["fixture"],
                        "controlled-resize-observer"
                    );
                    let outcome = candidate
                        .capture()
                        .expect("a late no-op ResizeObserver should complete controlled capture");
                    assert_eq!(outcome.readiness, authored_readiness);
                    assert!(outcome.capture.unsupported_events.is_empty());
                    assert!(outcome.capture.text_mapping_gaps.is_empty());
                },
                "controlled-local-success" => {
                    let candidate = controlled.prepare_capture_candidate().expect(
                        "static local-success should reach generation-bound candidate evidence",
                    );
                    let surface = candidate.precondition().surface();
                    assert_eq!(surface.device_pixel_scale(), 1.0);
                    assert_eq!(
                        (surface.viewport().width, surface.viewport().height),
                        (794, 1123)
                    );
                    assert_eq!(surface.capture_rect().min.x, 0);
                    assert_eq!(surface.capture_rect().min.y, 0);
                    assert_eq!(surface.capture_rect().max.x, surface.viewport().width);
                    assert_eq!(surface.capture_rect().max.y, surface.viewport().height);

                    let outcome = candidate
                        .capture()
                        .expect("static local-success should complete controlled capture");
                    assert_eq!(outcome.readiness["status"], "ready");
                    assert_eq!(outcome.readiness["font_status"], "loaded");
                    assert_eq!(outcome.readiness["payload"]["local_loaded"], true);
                    assert_eq!(outcome.resource_accounting.requests, 2);
                    assert_eq!(outcome.resource_accounting.loaded, 2);
                    assert_eq!(outcome.resource_accounting.delegated, 0);
                    assert_eq!(outcome.resource_accounting.failed, 0);
                    assert_eq!(outcome.resources.len(), 2);
                    assert_eq!(
                        png_dimensions(&outcome.stable_image_png),
                        (
                            surface.capture_rect().width() as u32,
                            surface.capture_rect().height() as u32,
                        )
                    );
                    assert!(outcome.capture.unsupported_events.is_empty());
                    assert!(outcome.capture.text_mapping_gaps.is_empty());
                },
                "controlled-finite" => {
                    let handshake_before = controlled_readiness_handshake_counts();
                    let candidate = controlled
                        .prepare_capture_candidate()
                        .expect("finite controlled work should reach opaque candidate evidence");
                    assert_successful_readiness_handshake(handshake_before);
                    // The 5 ms timeout runs first, then the pending animation frame consumes
                    // ScriptThread's deterministic 20 ms rendering-opportunity deadline.
                    assert_eq!(candidate.precondition().now().as_nanos(), 20_000_000);
                    assert_eq!(candidate.precondition().pending_events(), 0);
                    assert!(candidate.precondition().producers().snapshot.is_empty());
                    assert_eq!(
                        candidate.precondition().producers().stability,
                        embedder_traits::DocumentProducerStability::StableEmpty
                    );
                    assert!(candidate.precondition().next_deadline().is_none());
                    assert!(candidate.precondition().sources().sources().iter().all(
                        |source| matches!(
                            source.disposition(),
                            embedder_traits::DocumentSettlementSourceDisposition::Inert
                        )
                    ));
                    assert!(candidate.precondition().execution().terminal.is_none());
                    let surface = candidate.precondition().surface();
                    assert_eq!(surface.device_pixel_scale(), 1.0);
                    assert_eq!(
                        (surface.viewport().width, surface.viewport().height),
                        (794, 1123)
                    );
                    assert_eq!(surface.capture_rect().min.x, 0);
                    assert_eq!(surface.capture_rect().min.y, 0);
                    assert_eq!(surface.capture_rect().max.x, surface.viewport().width);
                    assert_eq!(surface.capture_rect().max.y, surface.viewport().height);
                    assert_eq!(
                        candidate.trace().first(),
                        Some(&ControlledSettlementStep::Observe)
                    );
                    assert_eq!(
                        candidate.trace().last(),
                        Some(&ControlledSettlementStep::PrepareCapture)
                    );
                    assert!(
                        candidate
                            .trace()
                            .iter()
                            .filter(|step| **step == ControlledSettlementStep::DriveOneTurn)
                            .count() >=
                            2
                    );
                    assert!(
                        candidate
                            .trace()
                            .contains(&ControlledSettlementStep::AdvanceTo)
                    );
                    assert!(
                        candidate
                            .trace()
                            .contains(&ControlledSettlementStep::PrepareCapture)
                    );
                    let console = candidate.session.session.delegate.console.borrow();
                    let messages = console
                        .entries
                        .iter()
                        .filter_map(|(_, message)| {
                            message
                                .starts_with("controlled-")
                                .then_some(message.as_str())
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        messages
                            .iter()
                            .filter(|message| **message == "controlled-frame")
                            .count(),
                        1
                    );
                    assert_eq!(
                        messages
                            .into_iter()
                            .filter(|message| {
                                message.starts_with("controlled-start:") ||
                                    message.starts_with("controlled-end:")
                            })
                            .collect::<Vec<_>>(),
                        vec![
                            "controlled-start:946684800000:0",
                            "controlled-end:946684800005:5",
                        ]
                    );
                    drop(console);
                    let authored_readiness = candidate.readiness.clone();
                    assert_eq!(authored_readiness["status"], "ready");
                    assert_eq!(authored_readiness["font_status"], "loaded");
                    assert_eq!(
                        authored_readiness["payload"],
                        serde_json::json!({
                            "fixture": "controlled-finite",
                            "page": { "rows": 7, "label": "authored" },
                        })
                    );
                    let outcome = candidate
                        .capture()
                        .expect("the retained candidate and Paint presentation should commit");
                    assert_eq!(outcome.readiness, authored_readiness);
                    assert!(outcome.readiness["payload"].get("source").is_none());
                    assert!(
                        outcome.readiness["payload"]
                            .get("document_time_ns")
                            .is_none()
                    );
                    assert_eq!(
                        outcome
                            .console
                            .iter()
                            .filter(|(_, message)| {
                                message ==
                                    "controlled-paint-observed:first-contentful-paint:20:0:20"
                            })
                            .count(),
                        1,
                        "controlled capture did not execute exactly one deterministic paint observer: {:?}",
                        outcome.console,
                    );
                    let layout_marker_count = outcome
                        .layout_debug
                        .get("fragments")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|fragment| {
                            fragment
                                .get("text_run")
                                .and_then(|run| run.get("text"))
                                .and_then(serde_json::Value::as_str) ==
                                Some("PLIEGO_FCP_20")
                        })
                        .count();
                    assert_eq!(
                        layout_marker_count, 1,
                        "controlled layout did not contain exactly one paint-observer marker: {}",
                        outcome.layout_debug,
                    );
                    let scene_marker_count = outcome
                        .capture
                        .scene
                        .pages
                        .iter()
                        .flat_map(|page| &page.operations)
                        .filter(|operation| {
                            matches!(
                                operation,
                                Operation::Text { text, .. }
                                    if text == "PLIEGO_FCP_20"
                            )
                        })
                        .count();
                    assert_eq!(scene_marker_count, 1);
                    assert!(outcome.stable_image_png.starts_with(b"\x89PNG\r\n\x1a\n"));
                    assert!(outcome.capture.unsupported_events.is_empty());
                    assert!(outcome.capture.text_mapping_gaps.is_empty());
                    println!("controlled-capture-complete");
                },
                "controlled-paint-mutation" => {
                    let candidate = controlled
                        .prepare_capture_candidate()
                        .expect("finite controlled work should reach candidate evidence");
                    let authored_readiness = candidate.readiness.clone();
                    let error =
                        match candidate.capture_with_paint_hook(|webview, _| webview.paint()) {
                            Ok(_) => panic!("a Paint mutation after reservation exposed pixels"),
                            Err(error) => error,
                        };
                    assert_eq!(error.code, "CONTROLLED_PAINT_FINALIZE_FAILED");
                    assert!(error.capture_evidence.stable_image_png.is_none());
                    assert!(error.capture_evidence.layout_debug.is_none());
                    assert_eq!(
                        error.capture_evidence.readiness.as_ref(),
                        Some(&authored_readiness)
                    );
                    assert!(error.capture_evidence.controlled_runtime_ms.is_some());
                    assert!(
                        error
                            .resources
                            .iter()
                            .any(|evidence| evidence.request.is_for_main_frame)
                    );
                    println!("controlled-paint-mutation-rejected");
                },
                "controlled-readiness-retry" => {
                    let candidate = controlled
                        .prepare_capture_candidate()
                        .expect("the first page should reach candidate evidence");
                    assert_eq!(candidate.readiness["payload"]["revision"], 1);
                    let retry_handshake_before = controlled_readiness_handshake_counts();
                    let hook_calls = Rc::new(Cell::new(0));
                    let callback_hook_calls = hook_calls.clone();
                    let mutation_evaluated = Rc::new(Cell::new(false));
                    let callback_evaluated = mutation_evaluated.clone();
                    let outcome = candidate
                        .capture_with_document_work_queued_hook(move |webview| {
                            callback_hook_calls.set(callback_hook_calls.get() + 1);
                            webview.evaluate_javascript(
                                "window.readinessPayload.revision=2;document.getElementById('state').textContent='revision 2'",
                                move |_| {
                                    callback_evaluated.set(true);
                                },
                            );
                        })
                        .expect(
                            "queued authored work should settle and replace the stale readiness snapshot",
                        );
                    assert_successful_readiness_handshake(retry_handshake_before);
                    assert_eq!(hook_calls.get(), 1);
                    assert!(mutation_evaluated.get());
                    assert_eq!(outcome.readiness["status"], "ready");
                    assert_eq!(
                        outcome.readiness["payload"],
                        serde_json::json!({
                            "fixture": "controlled-readiness-retry",
                            "revision": 2,
                        })
                    );
                    let scene_text = outcome
                        .capture
                        .scene
                        .pages
                        .iter()
                        .flat_map(|page| &page.operations)
                        .filter_map(|operation| match operation {
                            Operation::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        scene_text.concat(),
                        "revision 2",
                        "replacement scene text operations are stale: {scene_text:?}"
                    );
                },
                "controlled-readiness-fail" => {
                    let handshake_before = controlled_readiness_handshake_counts();
                    let error = match controlled.prepare_capture_candidate() {
                        Ok(_) => panic!("an explicitly failed page issued candidate evidence"),
                        Err(error) => error,
                    };
                    assert_eq!(
                        readiness_handshake_delta(handshake_before),
                        (1, 1, 0),
                        "terminal readiness must decode exactly once without a freshness settlement"
                    );
                    assert_eq!(error.code, "CONTROLLED_FIXTURE_FAILED");
                    assert_eq!(error.message, "controlled readiness rejected");
                    let readiness = error.capture_evidence.readiness.as_ref().unwrap();
                    assert_eq!(readiness["status"], "failed");
                    assert_eq!(readiness["error"]["code"], "CONTROLLED_FIXTURE_FAILED");
                    assert_eq!(
                        readiness["error"]["message"],
                        "controlled readiness rejected"
                    );
                    assert!(error.capture_evidence.controlled_runtime_ms.is_some());
                    assert!(
                        error
                            .resources
                            .iter()
                            .any(|evidence| evidence.request.is_for_main_frame)
                    );
                },
                "controlled-readiness-timeout" => {
                    let handshake_before = controlled_readiness_handshake_counts();
                    let error = match controlled.prepare_capture_candidate() {
                        Ok(_) => panic!("a deferred page issued candidate evidence"),
                        Err(error) => error,
                    };
                    assert_eq!(
                        readiness_handshake_delta(handshake_before),
                        (1, 1, 0),
                        "terminal readiness timeout must decode exactly once without a freshness settlement"
                    );
                    assert_eq!(error.code, "READINESS_TIMEOUT");
                    assert_eq!(error.message, "Document readiness timed out after 25 ms");
                    let readiness = error.capture_evidence.readiness.as_ref().unwrap();
                    assert_eq!(readiness["status"], "failed");
                    assert_eq!(readiness["error"]["code"], "READINESS_TIMEOUT");
                    assert_eq!(
                        readiness["error"]["message"],
                        "Document readiness timed out after 25 ms"
                    );
                    assert!(error.capture_evidence.controlled_runtime_ms.is_some());
                    assert!(
                        error
                            .resources
                            .iter()
                            .any(|evidence| evidence.request.is_for_main_frame)
                    );
                },
                "controlled-interval" => {
                    let handshake_before = controlled_readiness_handshake_counts();
                    let error = match controlled.prepare_capture_candidate() {
                        Ok(_) => panic!("an interval issued candidate evidence"),
                        Err(error) => error,
                    };
                    assert_eq!(
                        readiness_handshake_delta(handshake_before),
                        (0, 0, 0),
                        "pre-readiness settlement failure must not enqueue readiness JavaScript"
                    );
                    assert_eq!(error.code, "SETTLEMENT_FAILED");
                    assert!(error.message.contains("OpenEndedSource"));
                    assert!(error.capture_evidence.readiness.is_none());
                    assert!(error.capture_evidence.controlled_runtime_ms.is_some());
                    assert!(
                        error
                            .resources
                            .iter()
                            .any(|evidence| evidence.request.is_for_main_frame)
                    );
                },
                _ => unreachable!(),
            }
            assert_eq!(bundle_files(), files_before);
            return;
        }
        let expected_metadata_outcome = match case.as_str() {
            "metadata-denied-non-icon" => Some((
                url::Url::parse("https://denied.invalid/report.bin").unwrap(),
                "cancelled",
            )),
            "same-url-role-split" => Some((
                url::Url::from_file_path(input.parent().unwrap().join("shared.png")).unwrap(),
                "loaded",
            )),
            "metadata-allowed-icon" => Some((
                url::Url::from_file_path(input.parent().unwrap().join("icon.png")).unwrap(),
                "loaded",
            )),
            _ => None,
        };
        let session = if case == "resolved-root" {
            let root = input
                .parent()
                .and_then(Path::parent)
                .expect("resolved-root fixture should have a nested input");
            let document = LocalDocument::resolve(root, "sub/input.html").unwrap();
            let resource_policy = ResourcePolicy::resolve(&resources, document.root());
            DocumentSession::from_resolved(
                &document,
                resource_policy,
                environment,
                page,
                allow_host_fonts,
                readiness,
            )
        } else if case == "canvas-retention-budget" {
            DocumentSession::new_with_canvas_retention_limits(
                &input,
                environment,
                page,
                resources,
                allow_host_fonts,
                readiness,
                (64, 8, 64),
            )
        } else {
            DocumentSession::new(
                &input,
                environment,
                page,
                resources,
                allow_host_fonts,
                readiness,
            )
        };
        if let (Ok(session), Some(observed), Some((target, status))) = (
            session.as_ref(),
            metadata_evidence_observed.as_ref(),
            expected_metadata_outcome,
        ) {
            let observed = Arc::clone(observed);
            session.set_resource_evidence_observer(Rc::new(move |evidence| {
                if evidence.request.url == target &&
                    evidence.request.load_role == WebResourceLoadRole::DocumentMetadata &&
                    evidence.request.destination == "Image" &&
                    evidence.status == status
                {
                    observed.store(true, Ordering::SeqCst);
                }
            }));
        }
        if case == "console-evidence" {
            let outcome = session
                .expect("console evidence fixture should construct")
                .render()
                .expect("console evidence fixture should render");
            let console = outcome
                .console
                .iter()
                .filter(|(_, message)| message.starts_with("capture-"))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                console,
                vec![
                    ("info".into(), "capture-first".into()),
                    ("error".into(), "capture-second".into()),
                ]
            );
            assert!(outcome.pdf.starts_with(b"%PDF-"));
            assert_eq!(outcome.readiness["payload"]["fixture"], "console-evidence");
            return;
        }
        let result = session.and_then(|session| {
            if case == "canvas-missing-snapshot" {
                session.render_with_canvas_freezer(|keys| {
                    let mut keys = keys.to_vec();
                    keys.push((u32::MAX, u32::MAX));
                    servo_canvas::retained_canvas::freeze_canvas_snapshots(&keys)
                })
            } else {
                session.render()
            }
        });

        match case.as_str() {
            "console-overflow" => {
                let Err(error) = result else {
                    panic!("console overflow returned a DocumentOutcome containing PDF bytes")
                };
                assert_eq!(error.code, "CONSOLE_OUTPUT_LIMIT_EXCEEDED");
                assert_eq!(error.console.len(), MAX_CONSOLE_EVENTS);
                assert_eq!(
                    error
                        .console
                        .first()
                        .map(|(level, message)| (level.as_str(), message.as_str())),
                    Some(("info", "overflow-0"))
                );
                let expected_last = format!("overflow-{}", MAX_CONSOLE_EVENTS - 1);
                assert_eq!(
                    error
                        .console
                        .last()
                        .map(|(level, message)| (level.as_str(), message.as_str())),
                    Some(("info", expected_last.as_str()))
                );
                assert!(
                    !error
                        .console
                        .iter()
                        .any(|(_, message)| message == &format!("overflow-{MAX_CONSOLE_EVENTS}"))
                );
            },
            "local-success" => {
                let outcome = result.expect("local resource fixture should render");
                let script = session_fixture("local-success.js");
                assert!(outcome.pdf.starts_with(b"%PDF-"));
                assert_eq!(outcome.readiness["status"], "ready");
                assert_eq!(outcome.readiness["payload"]["local_loaded"], true);
                assert_eq!(
                    outcome.resource_accounting,
                    ResourceAccounting {
                        requests: 2,
                        loaded: 2,
                        delegated: 0,
                        failed: 0,
                        body_bytes: (std::fs::metadata(&input).unwrap().len() +
                            std::fs::metadata(&script).unwrap().len()),
                        unavailable_bodies: 0,
                    }
                );
                for path in [&input, &script] {
                    let url = url::Url::from_file_path(path).unwrap();
                    let resource = outcome
                        .resources
                        .iter()
                        .find(|resource| resource.request.url == url)
                        .expect("local request should retain evidence");
                    let body = std::fs::read(path).unwrap();
                    assert_eq!(resource.source, Some(ResourceSource::DocumentRoot));
                    assert_eq!(resource.status, "loaded");
                    assert_eq!(resource.response_status, Some(200));
                    assert_eq!(resource.bytes, Some(body.len() as u64));
                    assert_eq!(
                        resource.sha256.as_deref(),
                        Some(&content_address(&body)[7..])
                    );
                }
            },
            "resolved-root" => {
                let outcome = result.expect("resolved root fixture should render");
                assert_eq!(outcome.readiness["payload"]["fixture"], "resolved-root");
                let sibling = input
                    .parent()
                    .and_then(Path::parent)
                    .unwrap()
                    .join("sibling.js");
                let sibling_url = url::Url::from_file_path(&sibling).unwrap();
                let evidence = outcome
                    .resources
                    .iter()
                    .find(|resource| resource.request.url == sibling_url)
                    .expect("sibling resource under the resolved document root should load");
                assert_eq!(evidence.source, Some(ResourceSource::DocumentRoot));
                assert_eq!(evidence.status, "loaded");
            },
            "virtual-success" => {
                let outcome = result.expect("virtual resource fixture should render");
                assert_eq!(outcome.readiness["payload"]["local_loaded"], true);
                let resource = outcome
                    .resources
                    .iter()
                    .find(|resource| {
                        resource.request.url.as_str() == "https://virtual.invalid/local-success.js"
                    })
                    .expect("virtual resource should retain evidence");
                assert_eq!(resource.source, Some(ResourceSource::VirtualResource));
                assert_eq!(resource.status, "loaded");
                assert_eq!(resource.response_status, Some(200));
                assert!(resource.bytes.is_some_and(|bytes| bytes > 0));
                assert!(resource.sha256.is_some());
            },
            "asset-cache" => {
                let outcome = result.expect("asset cache fixture should render");
                assert_eq!(outcome.readiness["payload"]["fixture"], "asset-cache");
                assert_eq!(outcome.readiness["payload"]["executions"], 2);
                assert_eq!(outcome.resources.len(), 4);
                let document = outcome
                    .resources
                    .iter()
                    .filter(|resource| resource.source == Some(ResourceSource::DocumentRoot))
                    .collect::<Vec<_>>();
                assert_eq!(document.len(), 1);
                assert_eq!(document[0].request.method, "GET");
                assert_eq!(document[0].request.destination, "Document");
                assert!(document[0].request.is_for_main_frame);
                assert_eq!(
                    document[0].request.url,
                    url::Url::from_file_path(&input).unwrap()
                );
                let assets = outcome
                    .resources
                    .iter()
                    .filter(|resource| {
                        matches!(resource.source, Some(ResourceSource::AssetCache(_)))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(assets.len(), 3);
                let asset_body = fs::read(input.parent().unwrap().join("first.js")).unwrap();
                let asset_sha = content_address(&asset_body);
                assert!(assets.iter().all(|resource| {
                    resource.request.method == "GET" &&
                        resource.request.destination == "Script" &&
                        !resource.request.is_for_main_frame &&
                        resource.status == "loaded" &&
                        resource.response_status == Some(200) &&
                        resource.bytes == Some(asset_body.len() as u64) &&
                        resource.sha256.as_deref() == Some(&asset_sha[7..])
                }));
                let first = assets
                    .iter()
                    .filter(|resource| resource.request.url.path() == "/first.js")
                    .collect::<Vec<_>>();
                assert_eq!(first.len(), 1);
                assert_eq!(first[0].source, Some(ResourceSource::AssetCache("miss")));
                let renamed = assets
                    .iter()
                    .filter(|resource| resource.request.url.path() == "/renamed.js")
                    .collect::<Vec<_>>();
                assert_eq!(renamed.len(), 2);
                assert!(renamed.iter().all(|resource| {
                    resource.source == Some(ResourceSource::AssetCache("hit"))
                }));
                assert_eq!(outcome.resource_accounting.requests, 4);
                assert_eq!(outcome.resource_accounting.loaded, 4);
                assert_eq!(outcome.resource_accounting.failed, 0);
            },
            "allowed-url" => {
                let outcome = result.expect("allowed HTTP resource fixture should render");
                assert!(outcome.pdf.starts_with(b"%PDF-"));
                assert_eq!(outcome.readiness["payload"]["http_loaded"], true);
                let url = url::Url::parse(&format!("{http_base}allowed.js")).unwrap();
                let resource = outcome
                    .resources
                    .iter()
                    .find(|resource| resource.request.url == url)
                    .expect("allowed HTTP request should retain evidence");
                assert_eq!(resource.source, Some(ResourceSource::Http));
                assert_eq!(resource.status, "loaded");
                assert_eq!(resource.response_status, Some(200));
                assert_eq!(resource.content_type.as_deref(), Some("text/javascript"));
                assert_eq!(resource.bytes, Some(ALLOWED_HTTP_BODY.len() as u64));
                assert_eq!(
                    resource.sha256.as_deref(),
                    Some(&content_address(ALLOWED_HTTP_BODY)[7..])
                );
                assert_eq!(outcome.resource_accounting.requests, 2);
                assert_eq!(outcome.resource_accounting.loaded, 2);
                assert_eq!(outcome.resource_accounting.failed, 0);
            },
            "raster-local" | "raster-data" => {
                let outcome = result.expect("owned raster resource fixture should render");
                assert!(outcome.pdf.starts_with(b"%PDF-"));
                let expected = fixture_png();
                let expected_address = content_address(&expected);
                let scene_images = outcome
                    .capture
                    .scene
                    .pages
                    .iter()
                    .flat_map(|page| &page.operations)
                    .filter_map(|operation| match operation {
                        Operation::Image { resource, .. } => Some(resource),
                        _ => None,
                    })
                    .filter(|resource| outcome.resource_store.resolve_content(resource).is_some())
                    .collect::<Vec<_>>();
                assert_eq!(scene_images, vec![&expected_address]);
                assert_eq!(
                    outcome.resource_store.resolve_content(&expected_address),
                    Some(expected.as_slice())
                );
                let source = if case == "raster-data" {
                    ResourceSource::DataUrl
                } else {
                    ResourceSource::DocumentRoot
                };
                let evidence = outcome
                    .resources
                    .iter()
                    .find(|resource| {
                        resource.source == Some(source) &&
                            resource.content_address.as_deref() ==
                                Some(expected_address.as_str())
                    })
                    .expect("raster evidence should reference its owned exact bytes");
                assert_eq!(evidence.status, "loaded");
                assert_eq!(evidence.bytes, Some(expected.len() as u64));
                assert_eq!(
                    outcome
                        .resource_store
                        .resolve_url(evidence.request.url.as_str()),
                    Some(expected_address)
                );
                assert_eq!(outcome.resource_accounting.delegated, 0);
                assert!(outcome.resource_store.resident_bytes() >= expected.len() as u64);
            },
            "metadata-denied-non-icon" => {
                let outcome = result.expect("denied document metadata must not abort rendering");
                assert!(outcome.pdf.starts_with(b"%PDF-"));
                assert_metadata_evidence_precedes_readiness(&outcome.readiness, case.as_str());
                let evidence = outcome
                    .resources
                    .iter()
                    .find(|resource| {
                        resource.request.url.as_str() == "https://denied.invalid/report.bin"
                    })
                    .expect("metadata cancellation should retain evidence");
                assert_eq!(
                    evidence.request.load_role,
                    WebResourceLoadRole::DocumentMetadata
                );
                assert_eq!(evidence.request.destination, "Image");
                assert_eq!(evidence.status, "cancelled");
                assert!(!evidence.fatal);
                assert_eq!(evidence.source, None);
                let failure = evidence.failure.as_ref().unwrap();
                assert_eq!(failure.code, "RESOURCE_DENIED");
                assert_eq!(failure.status, "denied");
                assert!(!failure.fatal);
                assert_eq!(failure.load_role, WebResourceLoadRole::DocumentMetadata);
                assert_eq!(outcome.resource_accounting.failed, 1);
                assert_eq!(
                    outcome.resource_accounting.requests,
                    outcome.resource_accounting.loaded +
                        outcome.resource_accounting.delegated +
                        outcome.resource_accounting.failed
                );
                assert!(outcome.resource_accounting.loaded >= 3);
            },
            "content-favicon-name" => {
                let Err(error) = result else {
                    panic!("content became optional from its filename")
                };
                let failure = error.resource_failure.as_ref().unwrap();
                assert_eq!(failure.code, "RESOURCE_DENIED");
                assert_eq!(failure.url, "https://denied.invalid/favicon.ico");
                assert_eq!(failure.destination, "Image");
                assert!(failure.fatal);
                assert_eq!(failure.load_role, WebResourceLoadRole::DocumentContent);
            },
            "same-url-role-split" => {
                let outcome =
                    result.expect("same-URL metadata and content loads should both render");
                assert_metadata_evidence_precedes_readiness(&outcome.readiness, case.as_str());
                let shared =
                    url::Url::from_file_path(input.parent().unwrap().join("shared.png")).unwrap();
                let roles = outcome
                    .resources
                    .iter()
                    .filter(|resource| resource.request.url == shared)
                    .map(|resource| resource.request.load_role)
                    .collect::<std::collections::HashSet<_>>();
                assert_eq!(
                    roles,
                    std::collections::HashSet::from([
                        WebResourceLoadRole::DocumentContent,
                        WebResourceLoadRole::DocumentMetadata,
                    ])
                );
            },
            "preload-image-content" => {
                let Err(error) = result else {
                    panic!("an image preload denial became optional")
                };
                let failure = error.resource_failure.as_ref().unwrap();
                assert_eq!(failure.code, "RESOURCE_DENIED");
                assert_eq!(failure.url, "https://denied.invalid/preload.png");
                assert_eq!(failure.destination, "Image");
                assert!(failure.fatal);
                assert_eq!(failure.load_role, WebResourceLoadRole::DocumentContent);
            },
            "metadata-allowed-icon" => {
                let outcome = result.expect("an allowed icon should load normally");
                assert_metadata_evidence_precedes_readiness(&outcome.readiness, case.as_str());
                let icon =
                    url::Url::from_file_path(input.parent().unwrap().join("icon.png")).unwrap();
                let evidence = outcome
                    .resources
                    .iter()
                    .find(|resource| resource.request.url == icon)
                    .expect("allowed icon should retain loaded evidence");
                assert_eq!(
                    evidence.request.load_role,
                    WebResourceLoadRole::DocumentMetadata
                );
                assert_eq!(evidence.status, "loaded");
                assert_eq!(evidence.source, Some(ResourceSource::DocumentRoot));
                assert!(evidence.failure.is_none());
            },
            "invalid-data-url" => {
                let Err(error) = result else {
                    panic!("invalid data URL returned a DocumentOutcome containing PDF bytes")
                };
                let failure = error
                    .resource_failure
                    .as_ref()
                    .expect("invalid data URL should retain a structured resource failure");
                assert_eq!(error.code, "RESOURCE_DATA_URL_INVALID");
                assert_eq!(failure.status, "invalid");
                assert_eq!(failure.destination, "Image");
                assert_eq!(error.resource_accounting.failed, 1);
                assert!(
                    error
                        .resource_store
                        .resolve_url("data:image/png;base64,!")
                        .is_none()
                );
            },
            "oversized-raster" => {
                let Err(error) = result else {
                    panic!("oversized raster returned a DocumentOutcome containing PDF bytes")
                };
                let failure = error
                    .resource_failure
                    .as_ref()
                    .expect("oversized raster should retain a structured resource failure");
                assert_eq!(error.code, "RESOURCE_IMAGE_LIMIT_EXCEEDED");
                assert_eq!(failure.status, "denied");
                assert!(failure.reason.contains("declared-width"));
                assert_eq!(error.resource_accounting.failed, 1);
                assert!(error.resource_store.resolve_url(&failure.url).is_none());
            },
            "denied-url" => {
                let Err(error) = result else {
                    panic!("denied resource returned a DocumentOutcome containing PDF bytes")
                };
                let failure = error
                    .resource_failure
                    .as_ref()
                    .expect("denial should retain the structured policy failure");
                assert_eq!(error.code, "RESOURCE_DENIED");
                assert_eq!(failure.status, "denied");
                assert_eq!(failure.url, "https://denied.invalid/blocked.js");
                assert_eq!(failure.method, "GET");
                assert_eq!(failure.destination, "Script");
                assert!(!failure.is_for_main_frame);
                assert!(!failure.is_redirect);
                assert_eq!(error.resource_accounting.requests, 2);
                assert_eq!(error.resource_accounting.loaded, 1);
                assert_eq!(error.resource_accounting.failed, 1);
                assert_eq!(error.resource_accounting.unavailable_bodies, 1);
                assert_eq!(
                    error.resource_accounting.requests,
                    error.resource_accounting.loaded +
                        error.resource_accounting.delegated +
                        error.resource_accounting.failed
                );
                assert_eq!(error.resources.len(), 1);
            },
            "http-timeout" => {
                let Err(error) = result else {
                    panic!("HTTP timeout returned a DocumentOutcome containing PDF bytes")
                };
                let failure = error
                    .resource_failure
                    .as_ref()
                    .expect("timeout should retain the structured policy failure");
                assert_eq!(error.code, "RESOURCE_TIMEOUT");
                assert_eq!(failure.status, "timeout");
                assert_eq!(failure.url, format!("{http_base}timeout.js"));
                assert_eq!(failure.method, "GET");
                assert_eq!(failure.destination, "Script");
                assert_eq!(error.resource_accounting.requests, 2);
                assert_eq!(error.resource_accounting.loaded, 1);
                assert_eq!(error.resource_accounting.failed, 1);
                assert_eq!(error.resource_accounting.unavailable_bodies, 1);
            },
            "explicit-fail" => {
                let Err(error) = result else {
                    panic!(
                        "explicit readiness failure returned a DocumentOutcome containing PDF bytes"
                    )
                };
                assert_eq!(error.code, "FIXTURE_READINESS_FAILED");
                assert_eq!(error.message, "expected explicit failure");
                assert!(error.resource_failure.is_none());
                assert_eq!(error.resource_accounting.requests, 1);
                assert_eq!(error.resource_accounting.loaded, 1);
                assert_eq!(error.resource_accounting.failed, 0);
                assert_eq!(
                    png_dimensions(
                        error
                            .capture_evidence
                            .stable_image_png
                            .as_deref()
                            .expect("failed readiness should retain the stable frame")
                    ),
                    (794, 1123)
                );
                assert_eq!(
                    error.capture_evidence.readiness.as_ref().unwrap()["status"],
                    "failed"
                );
                assert!(error.capture_evidence.layout_debug.is_none());
                assert!(
                    error
                        .capture_evidence
                        .controlled_runtime_ms
                        .is_some_and(|milliseconds| milliseconds.is_finite() && milliseconds >= 0.0)
                );
                assert!(error.capture_evidence.scene_capture_ms.is_none());
            },
            "defer-timeout" => {
                let Err(error) = result else {
                    panic!("deferred timeout returned a DocumentOutcome containing PDF bytes")
                };
                assert_eq!(error.code, "READINESS_TIMEOUT");
                assert_eq!(error.message, "Document readiness timed out after 25 ms");
                assert!(error.resource_failure.is_none());
                assert_eq!(error.resource_accounting.requests, 1);
                assert_eq!(error.resource_accounting.loaded, 1);
                assert_eq!(error.resource_accounting.failed, 0);
            },
            "environment" => {
                let outcome = result.expect("explicit environment fixture should render");
                assert_eq!(outcome.environment, environment);
                assert_eq!(
                    outcome.environment.artifact(),
                    serde_json::json!({
                        "locale": { "requested": "es-MX", "resolved": "es-MX" },
                        "timezone": { "requested": "PST8PDT", "resolved": "PST8PDT" },
                    })
                );
                assert!(outcome.allow_host_fonts);
                assert_eq!(outcome.readiness["payload"]["navigatorLanguage"], "es-MX");
                assert_eq!(outcome.readiness["payload"]["localHour"], 4);
                assert!(
                    outcome
                        .capture
                        .font_selections
                        .iter()
                        .any(|selection| selection.source == CapturedFontSource::Host),
                    "host-font opt-in did not reach Servo preferences"
                );
            },
            "invoice-oracle" => {
                let outcome = result.expect("invoice oracle fixture should render");
                let scene = outcome.capture.scene.normalized_json().unwrap();
                retain_oracle_evidence("invoice-oracle", &input, &outcome, &scene);
                assert_eq!(
                    content_address(&fs::read(&input).unwrap()),
                    INVOICE_INPUT,
                    "same-source invoice input changed"
                );
                assert_eq!(outcome.capture.scene.pages.len(), 2);
                assert_eq!(
                    content_address(&scene),
                    INVOICE_SCENE,
                    "direct invoice scene differs from the reviewed current regression snapshot"
                );
                assert_eq!(
                    content_address(&outcome.pdf),
                    INVOICE_PDF,
                    "direct invoice PDF differs from the reviewed current regression snapshot"
                );
                assert_eq!(outcome.environment, RenderEnvironment::default());
                assert!(!outcome.allow_host_fonts);
            },
            "hybrid-canvas" => {
                let outcome = result.expect("hybrid Canvas fixture should render");
                assert_eq!(content_address(&fs::read(&input).unwrap()), HYBRID_INPUT);
                assert_eq!(
                    outcome.readiness["payload"]["fixture"],
                    "live-hybrid-canvas"
                );
                assert_eq!(outcome.readiness["payload"]["readbackBytes"], 16);
                assert!(outcome.capture.unsupported_events.is_empty());
                assert!(outcome.capture.text_mapping_gaps.is_empty());
                assert_eq!(outcome.capture.canvas_resources.len(), 1);
                assert!(outcome.capture.embedded_image_resources.is_empty());
                assert_eq!(outcome.capture.canvas_diagnostics.len(), 1);
                let diagnostics = &outcome.capture.canvas_diagnostics[0].diagnostics;
                assert_eq!(diagnostics.vector_operation_count, 3);
                assert_eq!(diagnostics.rasterized_area_px, 4);
                assert_eq!(diagnostics.fallbacks.len(), 1);
                assert_eq!(diagnostics.fallbacks[0].area_px, 4);
                assert_eq!(
                    diagnostics.fallbacks[0].reason,
                    pliego::hybrid_canvas::CanvasFallbackReason::PixelReadback
                );
                let paths = outcome.capture.scene.pages[0]
                    .operations
                    .iter()
                    .filter(|operation| matches!(operation, Operation::Path { .. }))
                    .count();
                let images = outcome.capture.scene.pages[0]
                    .operations
                    .iter()
                    .filter_map(|operation| match operation {
                        Operation::Image { resource, .. } => Some(resource),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(paths, 3);
                assert_eq!(images.len(), 1);
                let resource = &outcome.capture.canvas_resources[0];
                assert_eq!(images[0], &resource.resource);
                assert_eq!(content_address(&resource.png), resource.resource);
                assert!(resource.png.starts_with(b"\x89PNG\r\n\x1a\n"));
                assert_eq!(
                    content_address(&outcome.capture.scene.normalized_json().unwrap()),
                    PRE_SESSION_HYBRID_SCENE
                );
                assert_eq!(content_address(&outcome.pdf), PRE_SESSION_HYBRID_PDF);
            },
            "unsupported-canvas" => {
                let Err(error) = result else {
                    panic!("unsupported Canvas returned a DocumentOutcome containing PDF bytes")
                };
                assert_eq!(error.code, "SCENE_CAPTURE_FAILED");
                assert!(error.message.contains("fill_rect"), "{}", error.message);
                assert!(error.message.contains("transform"), "{}", error.message);
                assert_eq!(
                    png_dimensions(
                        error
                            .capture_evidence
                            .stable_image_png
                            .as_deref()
                            .expect("scene failure should retain the stable frame")
                    ),
                    (200, 160)
                );
                assert_eq!(
                    error.capture_evidence.readiness.as_ref().unwrap()["status"],
                    "ready"
                );
                assert!(
                    error
                        .capture_evidence
                        .layout_debug
                        .as_ref()
                        .is_some_and(serde_json::Value::is_object)
                );
                assert!(
                    error
                        .capture_evidence
                        .controlled_runtime_ms
                        .is_some_and(|milliseconds| milliseconds.is_finite() && milliseconds >= 0.0)
                );
                assert!(
                    error
                        .capture_evidence
                        .scene_capture_ms
                        .is_some_and(|milliseconds| milliseconds.is_finite() && milliseconds >= 0.0)
                );
            },
            "canvas-retention-budget" => {
                let Err(error) = result else {
                    panic!(
                        "over-budget Canvas retention returned a DocumentOutcome containing PDF bytes"
                    )
                };
                assert_eq!(error.code, "SCENE_CAPTURE_FAILED");
                assert_eq!(
                    error.message,
                    "live Canvas retention exceeded the session budget"
                );
            },
            "canvas-missing-snapshot" => {
                let Err(error) = result else {
                    panic!(
                        "a missing retained Canvas key returned a DocumentOutcome containing PDF bytes"
                    )
                };
                assert_eq!(error.code, "SCENE_CAPTURE_FAILED");
                assert!(
                    error.message.contains("4294967295:4294967295"),
                    "{}",
                    error.message
                );
            },
            "chartjs-report" => {
                let outcome = result.expect("Chart.js fixture should render");
                let scene = outcome.capture.scene.normalized_json().unwrap();
                retain_oracle_evidence("chartjs-report", &input, &outcome, &scene);
                assert_eq!(content_address(&fs::read(&input).unwrap()), CHARTJS_INPUT);
                assert_eq!(outcome.readiness["payload"]["fixture"], "chartjs-report");
                assert_eq!(outcome.readiness["payload"]["chartVersion"], "4.5.1");
                assert_eq!(outcome.readiness["payload"]["canvasWidth"], 678);
                assert_eq!(outcome.readiness["payload"]["canvasHeight"], 250);
                assert_eq!(outcome.readiness["payload"]["datasetCount"], 2);
                assert_eq!(outcome.readiness["payload"]["dataPointCount"], 6);
                assert_eq!(outcome.readiness["payload"]["readbackBytes"], 678 * 250 * 4);
                assert!(outcome.capture.unsupported_events.is_empty());
                assert!(outcome.capture.text_mapping_gaps.is_empty());
                assert_eq!(outcome.capture.canvas_resources.len(), 1);
                assert!(outcome.capture.embedded_image_resources.is_empty());
                assert_eq!(outcome.capture.canvas_diagnostics.len(), 1);
                let diagnostics = &outcome.capture.canvas_diagnostics[0].diagnostics;
                assert_eq!(diagnostics.rasterized_area_px, 678 * 250);
                assert_eq!(diagnostics.fallbacks.len(), 1);
                assert_eq!(
                    diagnostics.fallbacks[0].reason,
                    pliego::hybrid_canvas::CanvasFallbackReason::PixelReadback
                );
                let images = outcome.capture.scene.pages[0]
                    .operations
                    .iter()
                    .filter_map(|operation| match operation {
                        Operation::Image { resource, .. } => Some(resource),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(images.len(), 1);
                let resource = &outcome.capture.canvas_resources[0];
                assert_eq!(images[0], &resource.resource);
                assert_eq!(content_address(&resource.png), resource.resource);
                assert!(resource.png.starts_with(b"\x89PNG\r\n\x1a\n"));
                assert_eq!(
                    u32::from_be_bytes(resource.png[16..20].try_into().unwrap()),
                    678
                );
                assert_eq!(
                    u32::from_be_bytes(resource.png[20..24].try_into().unwrap()),
                    250
                );
                assert_eq!(resource.resource, PRE_SESSION_CHARTJS_CANVAS);
                assert_eq!(content_address(&scene), CHARTJS_SCENE);
                assert_eq!(content_address(&outcome.pdf), CHARTJS_PDF);
                let chartjs = outcome
                    .resources
                    .iter()
                    .find(|resource| resource.request.url.path().ends_with("/chart.umd.js"))
                    .expect("the controlled resource evidence should include Chart.js");
                assert_eq!(chartjs.status, "loaded");
                assert_eq!(chartjs.sha256.as_deref(), Some(CHARTJS_UMD));
                assert_eq!(outcome.resource_accounting.failed, 0);
            },
            "state-seed" => {
                let outcome = result.expect("state seed fixture should render");
                assert_eq!(
                    outcome.readiness["payload"]["iframe_cookie_persisted"],
                    true
                );
                assert_eq!(
                    outcome.readiness["payload"]["window_name"],
                    "pliego-seed-page"
                );
            },
            "state-clean" => {
                let outcome = result.expect("fresh-state fixture should render");
                assert_eq!(outcome.readiness["payload"]["iframe_cookie_present"], false);
                assert_eq!(outcome.readiness["payload"]["cookie_header_seen"], false);
                assert_eq!(outcome.readiness["payload"]["window_name"], "");
            },
            other => panic!("unknown isolated fixture case: {other}"),
        }
    }

    #[test]
    fn minimal_static_matches_pre_session_servoshell_oracle() -> Result<(), SessionError> {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmarks/fixtures/minimal-static/input.html")
            .canonicalize()
            .expect("committed minimal-static fixture should exist");
        let ahem = input
            .parent()
            .expect("fixture should have a bundle root")
            .join("Ahem.ttf")
            .canonicalize()
            .expect("committed Ahem font should exist");
        assert_eq!(
            content_address(&std::fs::read(&input).expect("fixture should be readable")),
            FIXTURE_INPUT,
        );
        assert_eq!(
            content_address(&std::fs::read(&ahem).expect("Ahem should be readable")),
            AHEM_SOURCE_RESOURCE,
        );

        let capture = DocumentSession::new(
            &input,
            RenderEnvironment::default(),
            a4(),
            ResourcePolicyConfig::default(),
            false,
            ReadinessPolicy::default(),
        )?
        .capture()?;
        assert_eq!(png_dimensions(&capture.stable_image_png), (794, 1123));
        assert!(capture.layout_debug.is_object());
        assert!(capture.controlled_runtime_ms.is_finite());
        assert!(capture.controlled_runtime_ms >= 0.0);
        assert!(capture.scene_capture_ms.is_finite());
        assert!(capture.scene_capture_ms >= 0.0);
        let outcome = capture.render()?;
        assert_eq!(outcome.readiness["status"], "ready");
        assert_eq!(outcome.readiness["payload"], serde_json::Value::Null);
        assert_eq!(outcome.readiness["font_status"], "loaded");
        assert_eq!(outcome.capture.scene.pages.len(), 1);
        assert_eq!(
            content_address(&outcome.capture.scene.normalized_json().unwrap()),
            PRE_SESSION_SCENE,
        );
        assert_eq!(content_address(&outcome.pdf), PRE_SESSION_PDF);
        let mut served_resources = outcome
            .resources
            .iter()
            .filter(|resource| resource.status == "loaded")
            .map(|resource| resource.request.url.to_file_path().unwrap())
            .collect::<Vec<_>>();
        served_resources.sort();
        let mut expected_resources = vec![input, ahem.clone()];
        expected_resources.sort();
        assert_eq!(served_resources, expected_resources);

        let text_operations = outcome.capture.scene.pages[0]
            .operations
            .iter()
            .filter_map(|operation| match operation {
                Operation::Text { text, font, .. } => Some((text.as_str(), font.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let text = text_operations
            .iter()
            .map(|(text, _)| *text)
            .collect::<String>();
        assert!(text.contains("Hello, Pliego"));
        let links = outcome.capture.scene.pages[0]
            .operations
            .iter()
            .filter_map(|operation| match operation {
                Operation::Link { target, .. } => Some(target.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(links, vec![EXPECTED_LINK]);

        let ahem_selections = outcome
            .capture
            .font_selections
            .iter()
            .filter(|selection| {
                selection.source == CapturedFontSource::Bundled &&
                    selection.resource == AHEM_CAPTURED_RESOURCE &&
                    selection.selected_family.as_deref() == Some("Ahem") &&
                    selection
                        .requested_families
                        .iter()
                        .any(|family| family == "Ahem")
            })
            .collect::<Vec<_>>();
        assert!(!ahem_selections.is_empty());
        assert!(text_operations.iter().all(|(_, font)| {
            ahem_selections
                .iter()
                .any(|selection| *font == selection.instance)
        }));
        assert_eq!(
            outcome
                .capture
                .font_resources
                .iter()
                .map(|resource| resource.resource.as_str())
                .collect::<Vec<_>>(),
            vec![AHEM_CAPTURED_RESOURCE],
        );
        assert!(ahem_selections.iter().all(|selection| {
            outcome.capture.font_instances.iter().any(|instance| {
                instance.id == selection.instance && instance.resource == AHEM_CAPTURED_RESOURCE
            })
        }));

        Ok(())
    }
}
