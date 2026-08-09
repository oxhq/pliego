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
use servo::{
    LoadStatus, Preferences, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext,
    WebResourceLoad, WebResourceResponse, WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

use pliego::capture::{SceneCapture, capture_document_scene};
use pliego::pdf::{PdfFontResource, PdfFontVariation, render_document_pdf};

const TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(unix)]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn tzset();
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SessionError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl SessionError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
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
    pub(crate) served_resources: Vec<PathBuf>,
}

pub(crate) struct DocumentSession {
    webview: WebView,
    servo: Servo,
    delegate: Rc<DocumentDelegate>,
    _rendering_context: Rc<SoftwareRenderingContext>,
}

impl DocumentSession {
    pub(crate) fn new(input: impl AsRef<Path>, page: PageDefinition) -> Result<Self, SessionError> {
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
        let delegate = Rc::new(DocumentDelegate {
            bundle_root,
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
            .url(input_url)
            .build();

        Ok(Self {
            webview,
            servo,
            delegate,
            _rendering_context: rendering_context,
        })
    }

    pub(crate) fn render(self) -> Result<DocumentOutcome, SessionError> {
        self.render_inner()
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
        self.spin_until("stable render", || screenshot.borrow().is_some())?;
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

        Ok(DocumentOutcome {
            capture,
            pdf,
            served_resources: self.delegate.served_resources.borrow().clone(),
        })
    }

    fn spin_until(&self, label: &str, done: impl Fn() -> bool) -> Result<(), SessionError> {
        let deadline = Instant::now() + TIMEOUT;
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
        if let Some(reason) = self.delegate.resource_failure.borrow().as_deref() {
            return Err(SessionError::new("RESOURCE_DENIED", reason));
        }
        Ok(())
    }
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
    crashed: RefCell<Option<String>>,
    frame_ready: Cell<bool>,
    load_complete: Cell<bool>,
    resource_failure: RefCell<Option<String>>,
    served_resources: RefCell<Vec<PathBuf>>,
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
        let url = load.request().url.clone();
        let resource = (|| {
            if url.scheme() != "file" {
                return Err(format!("non-file resource is disabled: {url}"));
            }
            let path = url
                .to_file_path()
                .map_err(|_| format!("resource is not a local file: {url}"))?
                .canonicalize()
                .map_err(|error| format!("resource is unavailable: {url}: {error}"))?;
            if !path.starts_with(&self.bundle_root) {
                return Err(format!("resource is outside the document bundle: {url}"));
            }
            let content_type = match path.extension().and_then(|extension| extension.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("ttf") => "font/ttf",
                _ => return Err(format!("resource type is unsupported: {url}")),
            };
            let body = std::fs::read(&path)
                .map_err(|error| format!("resource is unreadable: {url}: {error}"))?;
            Ok((path, content_type, body))
        })();

        let Ok((path, content_type, body)) = resource else {
            let error = resource.unwrap_err();
            if self.resource_failure.borrow().is_none() {
                *self.resource_failure.borrow_mut() = Some(error);
            }
            load.intercept(WebResourceResponse::new(url)).cancel();
            return;
        };

        self.served_resources.borrow_mut().push(path);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        let mut intercepted = load.intercept(WebResourceResponse::new(url).headers(headers));
        intercepted.send_body_data(body);
        intercepted.finish();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use layout::pages::{PageDefinition, PageMargins};
    use sha2::{Digest, Sha256};

    use pliego::Operation;
    use pliego::capture::CapturedFontSource;

    use super::{DocumentSession, SessionError};

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

    #[test]
    fn missing_document_returns_session_error() {
        let error = DocumentSession::new("__pliego_missing_document__.html", a4())
            .err()
            .expect("missing input should return a typed error");

        assert_eq!(error.code, "INVALID_REQUEST");
        assert!(error.message.contains("document is unavailable"));
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

        let outcome = DocumentSession::new(&input, a4())?.render()?;
        assert_eq!(outcome.capture.scene.pages.len(), 1);
        assert_eq!(
            content_address(&outcome.capture.scene.normalized_json().unwrap()),
            PRE_SESSION_SCENE,
        );
        assert_eq!(content_address(&outcome.pdf), PRE_SESSION_PDF);
        let mut served_resources = outcome.served_resources.clone();
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
                selection.source == CapturedFontSource::Bundled
                    && selection.resource == AHEM_CAPTURED_RESOURCE
                    && selection.selected_family.as_deref() == Some("Ahem")
                    && selection
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
