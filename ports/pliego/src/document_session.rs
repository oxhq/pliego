/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pliego's minimal one-document Servo owner.
//!
//! This remains an internal migration seam while the published binary uses the
//! shell adapter. The screenshot is only a stable-render barrier. Retained
//! layout still comes through Servo's temporary, doc-hidden
//! `debug_layout_snapshot` hook until that upstream seam is made stable.

use std::cell::{Cell, OnceCell, RefCell};
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

use super::engine::RenderEnvironment;
use super::readiness::{self, Readiness, ReadinessPolicy};
use super::render_environment::{apply_timezone, unexpected_host_font};
use super::resource_policy::{
    ControlledResource, MAX_RESOURCE_TIMEOUT_MS, ResourceAccounting, ResourceEvidence,
    ResourcePolicy, ResourcePolicyConfig, ResourcePolicyDecision, ResourcePolicyFailure,
    ResourceRequest, create_controlled_http_client, fetch_controlled_http,
    retain_controlled_resource,
};

const TIMEOUT: Duration = Duration::from_secs(30);

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
    pub(crate) environment: RenderEnvironment,
    pub(crate) allow_host_fonts: bool,
    pub(crate) readiness: serde_json::Value,
    pub(crate) resources: Vec<ResourceEvidence>,
    pub(crate) resource_accounting: ResourceAccounting,
}

pub(crate) struct DocumentSession {
    webview: WebView,
    servo: Servo,
    delegate: Rc<DocumentDelegate>,
    environment: RenderEnvironment,
    allow_host_fonts: bool,
    stable_render_timeout: Duration,
    _rendering_context: Rc<SoftwareRenderingContext>,
}

impl DocumentSession {
    pub(crate) fn new(
        input: impl AsRef<Path>,
        environment: RenderEnvironment,
        page: PageDefinition,
        resources: ResourcePolicyConfig,
        allow_host_fonts: bool,
        readiness: ReadinessPolicy,
    ) -> Result<Self, SessionError> {
        let stable_render_timeout = stable_render_timeout(readiness)?;
        if !(1..=MAX_RESOURCE_TIMEOUT_MS).contains(&resources.timeout_ms) {
            return Err(SessionError::new(
                "INVALID_REQUEST",
                format!(
                    "resource timeout must be between 1 and {MAX_RESOURCE_TIMEOUT_MS} milliseconds"
                ),
            ));
        }
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
        apply_timezone(environment.timezone)
            .map_err(|error| SessionError::new("ENVIRONMENT_CONFIGURATION_FAILED", error))?;

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
        preferences.fonts_host_enabled = allow_host_fonts;
        preferences.intl_locale_override = environment.locale.into();
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
            environment,
            allow_host_fonts,
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
        validate_host_font_policy(&capture, self.allow_host_fonts)?;
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
            environment: self.environment,
            allow_host_fonts: self.allow_host_fonts,
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

fn stable_render_timeout(readiness: ReadinessPolicy) -> Result<Duration, SessionError> {
    TIMEOUT
        .checked_add(Duration::from_millis(readiness.timeout_ms))
        .ok_or_else(|| SessionError::new("INVALID_REQUEST", "readiness timeout is too large"))
}

#[derive(Default)]
struct DocumentDelegate {
    bundle_root: PathBuf,
    resource_policy: ResourcePolicy,
    controlled_http_client: OnceCell<net::connector::ServoClient>,
    controlled_resources: RefCell<BTreeMap<(String, String), ControlledResource>>,
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
                        let retained = ControlledResource {
                            status: response.status.as_u16(),
                            content_type: response
                                .headers
                                .get(CONTENT_TYPE)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned),
                            body: response.body.clone(),
                        };
                        if let Err(failure) = retain_controlled_resource(
                            &mut self.controlled_resources.borrow_mut(),
                            &request,
                            retained,
                        ) {
                            self.cancel_resource(load, failure);
                            return;
                        }
                        let evidence = ResourceEvidence::loaded_http(request.clone(), &response);
                        let mut intercepted = load.intercept(
                            WebResourceResponse::new(request.url)
                                .headers(response.headers)
                                .status_code(response.status)
                                .status_message(
                                    response
                                        .status
                                        .canonical_reason()
                                        .unwrap_or_default()
                                        .as_bytes()
                                        .to_vec(),
                                ),
                        );
                        intercepted.send_body_data(response.body);
                        intercepted.finish();
                        self.resources.borrow_mut().push(evidence);
                    },
                    Err(failure) => self.cancel_resource(load, failure),
                }
            },
            ResourcePolicyDecision::Synthesize {
                body,
                content_type,
                source,
            } => {
                let retained = ControlledResource {
                    status: 200,
                    content_type: Some(content_type.to_owned()),
                    body: body.clone(),
                };
                if let Err(failure) = retain_controlled_resource(
                    &mut self.controlled_resources.borrow_mut(),
                    &request,
                    retained,
                ) {
                    self.cancel_resource(load, failure);
                    return;
                }
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
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use layout::pages::{PageDefinition, PageMargins};
    use pliego::capture::{CapturedFontSelection, CapturedFontSource, SceneCapture};
    use pliego::{DocumentScene, Operation, Page, Size};
    use sha2::{Digest, Sha256};

    use super::super::resource_policy::{ResourceAccounting, ResourceSource, VirtualResourceSpec};
    use super::{
        DocumentSession, ReadinessPolicy, RenderEnvironment, ResourcePolicyConfig, SessionError,
        stable_render_timeout, validate_host_font_policy,
    };

    const ISOLATED_CASE_ENV: &str = "PLIEGO_DOCUMENT_SESSION_FIXTURE";
    const HTTP_BASE_ENV: &str = "PLIEGO_DOCUMENT_SESSION_HTTP_BASE";
    const ISOLATED_TEST: &str = "document_session::tests::isolated_resource_and_readiness_fixture";
    const ALLOWED_HTTP_BODY: &[u8] = b"window.pliego.ready({ http_loaded: true });\n";

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

    const INVOICE_INPUT: &str =
        "sha256:b0fa2d0b18e845e84c1229408622bd85e092ecf4d78b0878939006fb26926dce";
    const PRE_SESSION_INVOICE_SCENE: &str =
        "sha256:c1874a92a71ecde580f15075fe7d07ad6e5739ec794ad79291c9ba5b9bce1681";
    const PRE_SESSION_INVOICE_PDF: &str =
        "sha256:401e756f43adad12a137478cf36abe8273e89405e998b9d537ab62056d2face9";

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
        thread: Option<JoinHandle<()>>,
    }

    impl FixtureServer {
        fn start() -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => handle_fixture_request(stream),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        },
                        Err(error) => panic!("fixture server accept failed: {error}"),
                    }
                }
            });
            Self {
                base_url: format!("http://{address}/"),
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for FixtureServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.thread.take().unwrap().join().unwrap();
        }
    }

    fn handle_fixture_request(mut stream: TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 2048];
        while request.len() < 8192 {
            let read = stream.read(&mut buffer).unwrap_or(0);
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
            line.to_ascii_lowercase().starts_with("cookie:")
                && line.contains("pliego_session_seed=1")
        });
        let (status, content_type, body) = match path {
            "/allowed.js" => ("200 OK", "text/javascript", ALLOWED_HTTP_BODY.to_vec()),
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
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(&body);
    }

    fn run_isolated(case: &str, http_base: &str) -> Output {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", ISOLATED_TEST, "--ignored", "--nocapture"])
            .env(ISOLATED_CASE_ENV, case)
            .env(HTTP_BASE_ENV, http_base)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(75);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "isolated {case} fixture exceeded 75 seconds\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
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
        let server = FixtureServer::start();
        for case in [
            "local-success",
            "virtual-success",
            "asset-cache",
            "allowed-url",
            "denied-url",
            "http-timeout",
            "explicit-fail",
            "defer-timeout",
            "environment",
            "invoice-oracle",
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
    }

    #[test]
    #[ignore = "launched in a fresh process by the fixture orchestrator"]
    fn isolated_resource_and_readiness_fixture() {
        let case = std::env::var(ISOLATED_CASE_ENV).expect("isolated fixture case should be set");
        let http_base = std::env::var(HTTP_BASE_ENV).expect("HTTP fixture base should be set");
        let mut readiness = ReadinessPolicy {
            timeout_ms: if case == "defer-timeout" { 25 } else { 1_000 },
            wait_for_fonts: false,
        };
        let mut resources = ResourcePolicyConfig::default();
        let mut environment = RenderEnvironment::default();
        let mut allow_host_fonts = false;
        let mut _bundle = None;
        let input = match case.as_str() {
            "local-success" | "denied-url" | "explicit-fail" | "defer-timeout" => {
                session_fixture(&format!("{case}.html"))
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
            other => panic!("unknown isolated fixture case: {other}"),
        };
        let result = DocumentSession::new(
            &input,
            environment,
            a4(),
            resources,
            allow_host_fonts,
            readiness,
        )
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
                assert_eq!(resource.source, ResourceSource::VirtualResource);
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
                    .filter(|resource| resource.source == ResourceSource::DocumentRoot)
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
                    .filter(|resource| matches!(resource.source, ResourceSource::AssetCache(_)))
                    .collect::<Vec<_>>();
                assert_eq!(assets.len(), 3);
                let asset_body = fs::read(input.parent().unwrap().join("first.js")).unwrap();
                let asset_sha = content_address(&asset_body);
                assert!(assets.iter().all(|resource| {
                    resource.request.method == "GET"
                        && resource.request.destination == "Script"
                        && !resource.request.is_for_main_frame
                        && resource.status == "loaded"
                        && resource.response_status == Some(200)
                        && resource.bytes == Some(asset_body.len() as u64)
                        && resource.sha256.as_deref() == Some(&asset_sha[7..])
                }));
                let first = assets
                    .iter()
                    .filter(|resource| resource.request.url.path() == "/first.js")
                    .collect::<Vec<_>>();
                assert_eq!(first.len(), 1);
                assert_eq!(first[0].source, ResourceSource::AssetCache("miss"));
                let renamed = assets
                    .iter()
                    .filter(|resource| resource.request.url.path() == "/renamed.js")
                    .collect::<Vec<_>>();
                assert_eq!(renamed.len(), 2);
                assert!(
                    renamed
                        .iter()
                        .all(|resource| resource.source == ResourceSource::AssetCache("hit"))
                );
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
                assert_eq!(resource.source, ResourceSource::Http);
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
                assert_eq!(
                    content_address(&fs::read(&input).unwrap()),
                    INVOICE_INPUT,
                    "same-source invoice input changed"
                );
                assert_eq!(outcome.capture.scene.pages.len(), 2);
                assert_eq!(
                    content_address(&outcome.capture.scene.normalized_json().unwrap()),
                    PRE_SESSION_INVOICE_SCENE,
                    "direct invoice scene differs from the exact pre-session servoshell oracle"
                );
                assert_eq!(
                    content_address(&outcome.pdf),
                    PRE_SESSION_INVOICE_PDF,
                    "direct invoice PDF differs from the exact pre-session servoshell oracle"
                );
                assert_eq!(outcome.environment, RenderEnvironment::default());
                assert!(!outcome.allow_host_fonts);
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

        let outcome = DocumentSession::new(
            &input,
            RenderEnvironment::default(),
            a4(),
            ResourcePolicyConfig::default(),
            false,
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
