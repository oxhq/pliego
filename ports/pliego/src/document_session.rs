/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pliego's minimal one-document Servo owner.
//!
//! This remains an internal migration seam while the published binary uses the
//! shell adapter. The screenshot is only a stable-render barrier. Retained
//! layout still comes through Servo's temporary, doc-hidden
//! `debug_layout_snapshot` hook until that upstream seam is made stable.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use dpi::PhysicalSize;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};
use layout::pages::{PageDefinition, configure_for_process};
use pliego::capture::{SceneCapture, capture_document_scene};
use pliego::pdf::{PdfFontResource, PdfFontVariation, render_document_pdf};
use servo::{
    JSValue, LoadStatus, Preferences, RenderingContext, Servo, ServoBuilder,
    SoftwareRenderingContext, UserContentManager, UserScript, WebResourceLoad, WebResourceResponse,
    WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

use super::readiness::{self, Readiness, ReadinessPolicy};
use super::resource_policy::{
    ResourceAccounting, ResourceEvidence, ResourcePolicy, ResourcePolicyConfig,
    ResourcePolicyDecision, ResourcePolicyFailure, ResourceRequest,
};

const TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(unix)]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn tzset();
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SessionError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) resource_failure: Option<ResourcePolicyFailure>,
    pub(crate) resources: Vec<ResourceEvidence>,
    pub(crate) resource_accounting: ResourceAccounting,
}

impl SessionError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            resource_failure: None,
            resources: Vec::new(),
            resource_accounting: ResourceAccounting::default(),
        }
    }

    fn from_resource_failure(failure: ResourcePolicyFailure) -> Self {
        Self {
            code: failure.code.into(),
            message: format!("{}: {}", failure.reason, failure.url),
            resource_failure: Some(failure),
            resources: Vec::new(),
            resource_accounting: ResourceAccounting::default(),
        }
    }

    fn with_resources(mut self, resources: Vec<ResourceEvidence>) -> Self {
        self.resource_accounting = ResourceAccounting::from_evidence(&resources);
        // Successful/delegated rows stay in `resources`; the separate first failure is one request.
        if self.resource_failure.is_some() {
            self.resource_accounting = self.resource_accounting.with_failure();
        }
        self.resources = resources;
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
    pub(crate) readiness: serde_json::Value,
    pub(crate) resources: Vec<ResourceEvidence>,
    pub(crate) resource_accounting: ResourceAccounting,
}

pub(crate) struct DocumentSession {
    webview: WebView,
    servo: Servo,
    delegate: Rc<DocumentDelegate>,
    stable_render_timeout: Duration,
    _rendering_context: Rc<SoftwareRenderingContext>,
}

impl DocumentSession {
    pub(crate) fn new(
        input: impl AsRef<Path>,
        page: PageDefinition,
        resources: ResourcePolicyConfig,
        readiness: ReadinessPolicy,
    ) -> Result<Self, SessionError> {
        let stable_render_timeout = stable_render_timeout(readiness)?;
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
        if let Some(error) = resource_policy.asset_error.as_ref() {
            return Err(SessionError::new(error.code, error.message.clone()));
        }
        configure_for_process(page).map_err(|_| {
            SessionError::new(
                "LAYOUT_CONFIGURATION_FAILED",
                "paged layout was already configured for this process",
            )
        })?;
        configure_utc()?;

        let rendering_context = Rc::new(
            SoftwareRenderingContext::new(PhysicalSize::new(
                page.width().ceil() as u32,
                page.height().ceil() as u32,
            ))
            .map_err(|error| {
                SessionError::new(
                    "RENDER_CONTEXT_FAILED",
                    format!("cannot create software rendering context: {error:?}"),
                )
            })?,
        );
        rendering_context.make_current().map_err(|error| {
            SessionError::new(
                "RENDER_CONTEXT_FAILED",
                format!("cannot activate software rendering context: {error:?}"),
            )
        })?;

        let mut preferences = Preferences::default();
        preferences.fonts_host_enabled = false;
        preferences.intl_locale_override = "en-US".into();
        preferences.network_http_proxy_uri.clear();
        preferences.network_https_proxy_uri.clear();

        let servo = ServoBuilder::default().preferences(preferences).build();
        let user_content_manager = Rc::new(UserContentManager::new(&servo));
        user_content_manager
            .add_script(Rc::new(UserScript::from(readiness.document_start_script())));
        let delegate = Rc::new(DocumentDelegate {
            bundle_root,
            resource_policy,
            ..Default::default()
        });
        let input_url = Url::from_file_path(&input).map_err(|_| {
            SessionError::new(
                "INVALID_REQUEST",
                format!(
                    "cannot convert document path to a file URL: {}",
                    input.display()
                ),
            )
        })?;
        let webview = WebViewBuilder::new(&servo, rendering_context.clone())
            .delegate(delegate.clone())
            .user_content_manager(user_content_manager)
            .url(input_url)
            .build();

        Ok(Self {
            webview,
            servo,
            delegate,
            stable_render_timeout,
            _rendering_context: rendering_context,
        })
    }

    pub(crate) fn render(self) -> Result<DocumentOutcome, SessionError> {
        self.render_inner()
            .map_err(|error| error.with_resources(self.delegate.resources.borrow().clone()))
    }

    fn render_inner(&self) -> Result<DocumentOutcome, SessionError> {
        self.webview.show();
        self.spin_until("document load", || self.delegate.load_complete.get())?;

        let screenshot = Rc::new(RefCell::new(None));
        let screenshot_result = screenshot.clone();
        self.webview.take_screenshot(None, move |result| {
            *screenshot_result.borrow_mut() =
                Some(result.map(|_| ()).map_err(|error| format!("{error:?}")));
        });
        self.spin_until_for("stable render", self.stable_render_timeout, || {
            screenshot.borrow().is_some()
        })?;
        screenshot
            .borrow_mut()
            .take()
            .ok_or_else(|| {
                SessionError::new(
                    "STABLE_RENDER_FAILED",
                    "stable-render callback completed without a result",
                )
            })?
            .map_err(|message| SessionError::new("STABLE_RENDER_FAILED", message))?;
        if !self.delegate.frame_ready.get() {
            return Err(SessionError::new(
                "STABLE_RENDER_FAILED",
                "Servo completed the barrier without producing a frame",
            ));
        }
        let readiness = self.evaluate_readiness()?;

        let snapshot = self.webview.debug_layout_snapshot().ok_or_else(|| {
            SessionError::new(
                "SCENE_CAPTURE_UNAVAILABLE",
                "Servo did not expose a retained layout snapshot",
            )
        })?;
        let capture = capture_document_scene(snapshot.as_bytes(), |_| None)
            .map_err(|error| SessionError::new("SCENE_CAPTURE_FAILED", error.to_string()))?;
        capture
            .scene
            .validate()
            .map_err(|message| SessionError::new("SCENE_CAPTURE_INVALID", message))?;
        if !capture.unsupported_events.is_empty() || !capture.text_mapping_gaps.is_empty() {
            return Err(SessionError::new(
                "SCENE_CAPTURE_INCOMPLETE",
                "captured scene contains unsupported paint or text mapping gaps",
            ));
        }

        let decoded_resources = capture
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
        let instances = capture
            .font_instances
            .iter()
            .map(|instance| (instance.id.as_str(), instance))
            .collect::<BTreeMap<_, _>>();
        let variations = capture
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
        let pdf = render_document_pdf(
            &capture.scene,
            |font| {
                let instance = instances.get(font)?;
                Some(PdfFontResource {
                    bytes: decoded_resources.get(instance.resource.as_str())?,
                    face_index: instance.face_index,
                    variations: variations.get(font)?,
                    synthetic_bold: instance.synthetic_bold,
                })
            },
            |_| None,
        )
        .map_err(|error| SessionError::new("DOCUMENT_PDF_GENERATION_FAILED", error.to_string()))?;

        let resources = self.delegate.resources.borrow().clone();
        Ok(DocumentOutcome {
            capture,
            pdf,
            readiness,
            resource_accounting: ResourceAccounting::from_evidence(&resources),
            resources,
        })
    }

    fn evaluate_readiness(&self) -> Result<serde_json::Value, SessionError> {
        let deadline = Instant::now()
            .checked_add(self.stable_render_timeout)
            .ok_or_else(|| SessionError::new("INVALID_REQUEST", "timeout is too large"))?;
        loop {
            let result = Rc::new(RefCell::new(None));
            let callback_result = result.clone();
            self.webview
                .evaluate_javascript(readiness::HOST_EVALUATION_EXPRESSION, move |value| {
                    *callback_result.borrow_mut() = Some(value)
                });
            self.spin_until_for(
                "readiness evaluation",
                deadline.saturating_duration_since(Instant::now()),
                || result.borrow().is_some(),
            )?;
            let value = result.borrow_mut().take().ok_or_else(|| {
                SessionError::new(
                    "READINESS_EVALUATION_FAILED",
                    "readiness callback completed without a result",
                )
            })?;
            let value = value.map_err(|error| {
                SessionError::new("READINESS_EVALUATION_FAILED", format!("{error:?}"))
            })?;
            let snapshot = match value {
                JSValue::String(snapshot) => snapshot,
                value => {
                    return Err(SessionError::new(
                        "READINESS_INVALID_RESULT",
                        format!("expected readiness JSON string, got {value:?}"),
                    ));
                },
            };
            let evidence = serde_json::from_str(&snapshot).map_err(|error| {
                SessionError::new("READINESS_INVALID_RESULT", error.to_string())
            })?;
            match readiness::parse_snapshot(&snapshot)
                .map_err(|error| SessionError::new("READINESS_INVALID_RESULT", error))?
            {
                Readiness::Ready { .. } => return Ok(evidence),
                Readiness::Failed { error } => {
                    return Err(SessionError::new(error.code, error.message));
                },
                Readiness::Pending if Instant::now() >= deadline => {
                    return Err(SessionError::new(
                        "READINESS_TIMEOUT",
                        "document readiness did not settle before the host deadline",
                    ));
                },
                Readiness::Pending => {
                    self.servo.spin_event_loop();
                    std::thread::sleep(Duration::from_millis(1));
                },
            }
        }
    }

    fn spin_until(&self, label: &str, done: impl Fn() -> bool) -> Result<(), SessionError> {
        self.spin_until_for(label, TIMEOUT, done)
    }

    fn spin_until_for(
        &self,
        label: &str,
        timeout: Duration,
        done: impl Fn() -> bool,
    ) -> Result<(), SessionError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| SessionError::new("INVALID_REQUEST", "timeout is too large"))?;
        while !done() {
            self.check_failure(label)?;
            if Instant::now() >= deadline {
                return Err(SessionError::new(
                    "RENDER_TIMEOUT",
                    format!("timed out waiting for {label}"),
                ));
            }
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(1));
        }
        self.check_failure(label)
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
        Ok(())
    }
}

fn stable_render_timeout(readiness: ReadinessPolicy) -> Result<Duration, SessionError> {
    TIMEOUT
        .checked_add(Duration::from_millis(readiness.timeout_ms))
        .ok_or_else(|| SessionError::new("INVALID_REQUEST", "readiness timeout is too large"))
}

fn configure_utc() -> Result<(), SessionError> {
    #[cfg(target_os = "windows")]
    let result = unsafe { libc::putenv_s(c"TZ".as_ptr(), c"UTC".as_ptr()) };
    #[cfg(unix)]
    let result = unsafe { libc::setenv(c"TZ".as_ptr(), c"UTC".as_ptr(), 1) };
    #[cfg(not(any(target_os = "windows", unix)))]
    return Err(SessionError::new(
        "ENVIRONMENT_CONFIGURATION_FAILED",
        "UTC timezone configuration is unsupported on this desktop target",
    ));

    #[cfg(any(target_os = "windows", unix))]
    {
        if result != 0 {
            return Err(SessionError::new(
                "ENVIRONMENT_CONFIGURATION_FAILED",
                format!("cannot configure UTC timezone: platform error {result}"),
            ));
        }
        #[cfg(target_os = "windows")]
        unsafe {
            libc::tzset()
        };
        #[cfg(unix)]
        unsafe {
            tzset()
        };
        Ok(())
    }
}

#[derive(Default)]
struct DocumentDelegate {
    bundle_root: PathBuf,
    resource_policy: ResourcePolicy,
    crashed: RefCell<Option<String>>,
    frame_ready: Cell<bool>,
    load_complete: Cell<bool>,
    resource_failure: RefCell<Option<ResourcePolicyFailure>>,
    resources: RefCell<Vec<ResourceEvidence>>,
}

impl WebViewDelegate for DocumentDelegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        self.frame_ready.set(true);
        webview.paint();
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
            referrer_url: load.request().referrer_url.clone(),
            is_for_main_frame: load.request().is_for_main_frame,
            is_redirect: load.request().is_redirect,
        };
        match self.resource_policy.decide(&self.bundle_root, &request) {
            ResourcePolicyDecision::Allow { source } => self
                .resources
                .borrow_mut()
                .push(ResourceEvidence::delegated(request, source)),
            ResourcePolicyDecision::FetchHttp => self.cancel_resource(
                load,
                ResourcePolicyFailure::new(
                    &request,
                    "RESOURCE_DENIED",
                    "denied",
                    "controlled HTTP fetching is not yet owned by DocumentSession",
                ),
            ),
            ResourcePolicyDecision::Synthesize {
                body,
                content_type,
                source,
            } => {
                let evidence =
                    ResourceEvidence::loaded(request.clone(), source, content_type, &body);
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
                let mut intercepted =
                    load.intercept(WebResourceResponse::new(request.url).headers(headers));
                intercepted.send_body_data(body);
                intercepted.finish();
                self.resources.borrow_mut().push(evidence);
            },
            ResourcePolicyDecision::Fail(failure) => self.cancel_resource(load, failure),
        }
    }
}

impl DocumentDelegate {
    fn cancel_resource(&self, load: WebResourceLoad, failure: ResourcePolicyFailure) {
        let url = load.request().url.clone();
        if self.resource_failure.borrow().is_none() {
            *self.resource_failure.borrow_mut() = Some(failure);
        }
        load.intercept(WebResourceResponse::new(url)).cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use layout::pages::{PageDefinition, PageMargins};
    use pliego::Operation;
    use pliego::capture::CapturedFontSource;
    use sha2::{Digest, Sha256};

    use super::super::resource_policy::{ResourceAccounting, ResourceSource};
    use super::{
        DocumentSession, ReadinessPolicy, ResourcePolicyConfig, SessionError, stable_render_timeout,
    };

    const ISOLATED_CASE_ENV: &str = "PLIEGO_DOCUMENT_SESSION_FIXTURE";
    const ISOLATED_TEST: &str = "document_session::tests::isolated_resource_and_readiness_fixture";

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
            a4(),
            ResourcePolicyConfig::default(),
            ReadinessPolicy::default(),
        )
        .err()
        .expect("missing input should return a typed error");

        assert_eq!(error.code, "INVALID_REQUEST");
        assert!(error.message.contains("document is unavailable"));
    }

    #[test]
    fn stable_render_timeout_covers_the_readiness_budget() {
        let readiness = ReadinessPolicy {
            timeout_ms: 60_000,
            wait_for_fonts: true,
        };
        assert!(
            stable_render_timeout(readiness).unwrap()
                > std::time::Duration::from_millis(readiness.timeout_ms)
        );
    }

    #[test]
    fn resource_and_readiness_fixtures_are_evidenced_and_fail_closed() {
        for case in [
            "local-success",
            "denied-url",
            "explicit-fail",
            "defer-timeout",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", ISOLATED_TEST, "--ignored", "--nocapture"])
                .env(ISOLATED_CASE_ENV, case)
                .output()
                .unwrap();
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
    #[ignore = "launched in a fresh process by the fixture orchestrator"]
    fn isolated_resource_and_readiness_fixture() {
        let case = std::env::var(ISOLATED_CASE_ENV).expect("isolated fixture case should be set");
        let readiness = ReadinessPolicy {
            timeout_ms: if case == "defer-timeout" { 25 } else { 1_000 },
            wait_for_fonts: false,
        };
        let input = session_fixture(&format!("{case}.html"));
        let result = DocumentSession::new(&input, a4(), ResourcePolicyConfig::default(), readiness)
            .and_then(DocumentSession::render);

        match case.as_str() {
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
                        body_bytes: (std::fs::metadata(&input).unwrap().len()
                            + std::fs::metadata(&script).unwrap().len()),
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
                    assert_eq!(resource.source, ResourceSource::DocumentRoot);
                    assert_eq!(resource.status, "loaded");
                    assert_eq!(resource.response_status, Some(200));
                    assert_eq!(resource.bytes, Some(body.len() as u64));
                    assert_eq!(
                        resource.sha256.as_deref(),
                        Some(&content_address(&body)[7..])
                    );
                }
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
                    error.resource_accounting.loaded
                        + error.resource_accounting.delegated
                        + error.resource_accounting.failed
                );
                assert_eq!(error.resources.len(), 1);
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

        let outcome = DocumentSession::new(
            &input,
            a4(),
            ResourcePolicyConfig::default(),
            ReadinessPolicy::default(),
        )?
        .render()?;
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
