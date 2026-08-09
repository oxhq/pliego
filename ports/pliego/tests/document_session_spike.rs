/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};
use layout::pages::{PageDefinition, PageMargins, configure_for_process};
use pliego::Operation;
use pliego::capture::capture_document_scene;
use servo::{
    LoadStatus, Preferences, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext,
    WebResourceLoad, WebResourceResponse, WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct DocumentDelegate {
    bundle_root: PathBuf,
    crashed: RefCell<Option<String>>,
    frame_ready: Cell<bool>,
    load_complete: Cell<bool>,
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
        if load.request().url.scheme() != "file" {
            return;
        }
        let url = load.request().url.clone();
        let resource = url
            .to_file_path()
            .ok()
            .and_then(|path| path.canonicalize().ok())
            .filter(|path| path.starts_with(&self.bundle_root))
            .and_then(|path| {
                let content_type = match path.extension().and_then(|extension| extension.to_str()) {
                    Some("html") => "text/html; charset=utf-8",
                    Some("ttf") => "font/ttf",
                    _ => return None,
                };
                std::fs::read(path).ok().map(|body| (content_type, body))
            });
        let Some((content_type, body)) = resource else {
            load.intercept(WebResourceResponse::new(url)).cancel();
            return;
        };
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        let mut intercepted = load.intercept(WebResourceResponse::new(url).headers(headers));
        intercepted.send_body_data(body);
        intercepted.finish();
    }
}

fn spin_until(servo: &Servo, delegate: &DocumentDelegate, label: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        if let Some(reason) = delegate.crashed.borrow().as_deref() {
            panic!("direct Servo document session crashed while waiting for {label}: {reason}");
        }
        assert!(
            Instant::now() < deadline,
            "direct Servo document session timed out waiting for {label}"
        );
        servo.spin_event_loop();
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn minimal_static_reaches_document_scene_without_servoshell() {
    let page = PageDefinition::new(
        793.7008,
        1122.5197,
        PageMargins::new(45.3543, 60.4724, 45.3543, 60.4724),
    )
    .expect("A4 document geometry should be valid");
    assert!(
        configure_for_process(page).is_ok(),
        "page geometry should be configured before Servo starts"
    );

    let rendering_context = Rc::new(
        SoftwareRenderingContext::new(PhysicalSize::new(794, 1123))
            .expect("software rendering context should be available"),
    );
    rendering_context
        .make_current()
        .expect("software rendering context should become current");

    let mut preferences = Preferences::default();
    preferences.fonts_host_enabled = false;
    preferences.intl_locale_override = "en-US".into();
    preferences.network_http_proxy_uri.clear();
    preferences.network_https_proxy_uri.clear();

    let servo = ServoBuilder::default().preferences(preferences).build();
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/fixtures/minimal-static/input.html")
        .canonicalize()
        .expect("committed minimal-static fixture should exist");
    let delegate = Rc::new(DocumentDelegate {
        bundle_root: input
            .parent()
            .expect("fixture should have a bundle root")
            .to_path_buf(),
        ..Default::default()
    });
    let input_url = Url::from_file_path(&input).expect("fixture path should form a file URL");

    let webview = WebViewBuilder::new(&servo, rendering_context)
        .delegate(delegate.clone())
        .url(input_url)
        .build();
    webview.show();

    spin_until(&servo, &delegate, "document load", || {
        delegate.load_complete.get()
    });

    let screenshot = Rc::new(RefCell::new(None));
    let screenshot_result = screenshot.clone();
    webview.take_screenshot(None, move |result| {
        *screenshot_result.borrow_mut() = Some(result.map(|_| ()));
    });
    spin_until(&servo, &delegate, "stable render", || {
        screenshot.borrow().is_some()
    });
    screenshot
        .borrow_mut()
        .take()
        .expect("stable-render callback should complete")
        .unwrap_or_else(|error| panic!("stable-render barrier failed: {error:?}"));

    let snapshot = webview
        .debug_layout_snapshot()
        .expect("direct Servo session should expose a retained layout snapshot");
    let captured = capture_document_scene(snapshot.as_bytes(), |_| None)
        .expect("retained layout should convert to DocumentScene");
    captured
        .scene
        .validate()
        .expect("captured DocumentScene should be valid");
    assert_eq!(captured.scene.pages.len(), 1);
    let captured_text = captured.scene.pages[0]
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(
        captured_text.contains("Hello, Pliego"),
        "captured DocumentScene should contain fixture text, got {captured_text:?}"
    );
    assert!(
        !captured.font_resources.is_empty() && !captured.font_instances.is_empty(),
        "direct session should retain the bundled font used by fixture text"
    );
    assert!(!captured.scene.normalized_json().unwrap().is_empty());
    assert!(delegate.frame_ready.get(), "Servo should produce a frame");

    drop(webview);
    drop(servo);
}
