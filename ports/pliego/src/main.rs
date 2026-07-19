/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::collections::HashMap;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::ffi::CString;
use std::ffi::OsString;
use std::path::PathBuf;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use readiness::{Readiness, parse_snapshot};
use session::{LocalDocument, SessionArtifacts};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use sha2::{Digest, Sha256};

mod readiness;
mod session;

const SERVO_BASE_SHA: &str = "313b6d5ecc113b08010ce434140db3ca5abcc71c";
const READINESS_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_LOCALE: &str = "en-US";
const DEFAULT_TIMEZONE: &str = "UTC";

#[cfg(unix)]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn tzset();
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RenderEnvironment {
    locale: &'static str,
    timezone: &'static str,
}

impl Default for RenderEnvironment {
    fn default() -> Self {
        Self {
            locale: DEFAULT_LOCALE,
            timezone: DEFAULT_TIMEZONE,
        }
    }
}

impl RenderEnvironment {
    fn artifact(self) -> serde_json::Value {
        serde_json::json!({
            "locale": {
                "requested": self.locale,
                "resolved": self.locale,
            },
            "timezone": {
                "requested": self.timezone,
                "resolved": self.timezone,
            },
        })
    }
}

#[derive(Debug, PartialEq)]
struct RenderRequest {
    input: PathBuf,
    environment: RenderEnvironment,
}

#[derive(Debug, PartialEq)]
enum Command {
    Help,
    Version,
    Render(RenderRequest),
}

fn parse_args(args: Vec<OsString>) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Help);
    }
    if matches!(args.as_slice(), [flag] if flag == "-h" || flag == "--help") {
        return Ok(Command::Help);
    }
    if matches!(args.as_slice(), [flag] if flag == "-V" || flag == "--version" || flag == "--verbose-version")
    {
        return Ok(Command::Version);
    }

    let mut input = None;
    let mut locale = None;
    let mut timezone = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        if argument == "--locale" {
            if locale.is_some() {
                return Err("--locale may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--locale requires a value".to_owned())?;
            locale = Some(parse_locale(&value)?);
        } else if argument == "--timezone" {
            if timezone.is_some() {
                return Err("--timezone may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--timezone requires a value".to_owned())?;
            timezone = Some(parse_timezone(&value)?);
        } else if argument.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option: {}", argument.to_string_lossy()));
        } else if input.replace(PathBuf::from(argument)).is_some() {
            return Err("exactly one document path is required".into());
        }
    }

    let input = input.ok_or_else(|| "a document path is required".to_owned())?;
    Ok(Command::Render(RenderRequest {
        input,
        environment: RenderEnvironment {
            locale: locale.unwrap_or(DEFAULT_LOCALE),
            timezone: timezone.unwrap_or(DEFAULT_TIMEZONE),
        },
    }))
}

fn parse_locale(value: &OsString) -> Result<&'static str, String> {
    match value.to_str() {
        Some(DEFAULT_LOCALE) => Ok(DEFAULT_LOCALE),
        Some(value) => Err(format!(
            "unsupported locale {value:?}; supported locale: {DEFAULT_LOCALE}"
        )),
        None => Err("locale must be valid UTF-8".into()),
    }
}

fn parse_timezone(value: &OsString) -> Result<&'static str, String> {
    match value.to_str() {
        Some(DEFAULT_TIMEZONE) => Ok(DEFAULT_TIMEZONE),
        Some("PST8PDT") => Ok("PST8PDT"),
        Some(value) => Err(format!(
            "unsupported timezone {value:?}; supported timezones: UTC, PST8PDT"
        )),
        None => Err("timezone must be valid UTF-8".into()),
    }
}

fn main() {
    let command = parse_args(std::env::args_os().skip(1).collect())
        .unwrap_or_else(|error| invalid_request(&error));

    match command {
        Command::Help => print_help(),
        Command::Version => print_version(),
        Command::Render(request) => render(request),
    }
}

fn print_help() {
    println!(
        "Pliego — native document rendering on Servo\n\nUsage:\n  pliego [--locale en-US] [--timezone UTC|PST8PDT] <document.html>\n  pliego --version"
    );
}

fn print_version() {
    println!(
        "pliego {}\n{}\nServo base {}",
        env!("CARGO_PKG_VERSION"),
        servoshell::VERSION,
        SERVO_BASE_SHA
    );
}

fn invalid_request(message: &str) -> ! {
    println!(
        "{}",
        serde_json::json!({
            "status": "failed",
            "error": {
                "code": "INVALID_REQUEST",
                "message": message,
            },
        })
    );
    eprintln!("pliego: INVALID_REQUEST: {message}");
    std::process::exit(2)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
/// Sets the process-global timezone before Servo starts any worker threads.
/// This is deliberately scoped to Pliego's one-render-per-process CLI model.
fn apply_timezone(timezone: &str) -> Result<(), String> {
    let variable = CString::new("TZ").unwrap();
    let value = CString::new(timezone).map_err(|_| "timezone contains a null byte")?;

    #[cfg(target_os = "windows")]
    let result = unsafe { libc::putenv_s(variable.as_ptr(), value.as_ptr()) };
    #[cfg(unix)]
    let result = unsafe { libc::setenv(variable.as_ptr(), value.as_ptr(), 1) };
    #[cfg(not(any(target_os = "windows", unix)))]
    return Err("timezone overrides are unsupported on this desktop target".into());

    #[cfg(any(target_os = "windows", unix))]
    {
        if result != 0 {
            return Err(format!(
                "cannot set process timezone to {timezone}: platform error {result}"
            ));
        }

        // Keep the C runtime and SpiderMonkey's later cache reset on the same value.
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

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn render(request: RenderRequest) {
    let document = LocalDocument::resolve(".", &request.input).unwrap_or_else(|error| {
        eprintln!("pliego: {error}");
        std::process::exit(2)
    });
    let input_url = url::Url::from_file_path(document.path()).unwrap_or_else(|_| {
        eprintln!("pliego: cannot convert document path to a file URL");
        std::process::exit(2)
    });
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let artifacts = SessionArtifacts::create(
        std::env::temp_dir().join(format!("pliego-session-{}-{unique}", std::process::id())),
    )
    .unwrap_or_else(|error| {
        eprintln!("pliego: cannot create session artifacts: {error}");
        std::process::exit(1)
    });
    let proof = artifacts.directory().join("render.png");
    let environment_path = artifacts.directory().join("environment.json");
    let environment = request.environment.artifact();
    record_artifact(artifacts.write_environment(&environment));
    apply_timezone(request.environment.timezone).unwrap_or_else(|error| {
        fail_session(&artifacts, "ENVIRONMENT_CONFIGURATION_FAILED", &error)
    });
    let userscripts = artifacts.directory().join("userscripts");
    record_artifact(std::fs::create_dir_all(&userscripts));
    record_artifact(std::fs::write(
        userscripts.join("00-pliego-readiness.js"),
        readiness::document_start_script(READINESS_TIMEOUT_MS),
    ));

    record_artifact(artifacts.record_state("started", None));

    let servo_args = [
        "--headless".into(),
        "--exit".into(),
        "--output".into(),
        proof.to_string_lossy().into_owned(),
        "--userscripts".into(),
        userscripts.to_string_lossy().into_owned(),
        "--pref".into(),
        format!("intl_locale_override={}", request.environment.locale),
        input_url.to_string(),
    ];
    let result = servoshell::run_with_stable_javascript_and_console(
        &servo_args,
        readiness::HOST_EVALUATION_EXPRESSION,
    )
    .unwrap_or_else(|error| {
        fail_session(
            &artifacts,
            "READINESS_EVALUATION_FAILED",
            &error.to_string(),
        )
    });

    for message in result.console {
        record_artifact(artifacts.record_console(
            &format!("{:?}", message.level).to_ascii_lowercase(),
            &message.message,
        ));
    }
    let missing_resource = record_resources(&artifacts, result.resources);

    let snapshot_json = match result.value {
        servoshell::JSValue::String(json) => json,
        value => fail_session(
            &artifacts,
            "READINESS_INVALID_RESULT",
            &format!("expected readiness JSON string, got {value:?}"),
        ),
    };
    let readiness = parse_snapshot(&snapshot_json)
        .unwrap_or_else(|error| fail_session(&artifacts, "READINESS_INVALID_RESULT", &error));
    let readiness_json: serde_json::Value =
        serde_json::from_str(&snapshot_json).unwrap_or_else(|error| {
            fail_session(&artifacts, "READINESS_INVALID_RESULT", &error.to_string())
        });
    record_artifact(artifacts.write_readiness(&readiness_json));
    let readiness_payload = match readiness {
        Readiness::Ready { payload } => {
            if let Some(url) = missing_resource {
                fail_session(
                    &artifacts,
                    "RESOURCE_LOAD_FAILED",
                    &format!("local resource did not load: {url}"),
                );
            }
            payload
        },
        Readiness::Failed { error } => fail_session(&artifacts, &error.code, &error.message),
        Readiness::Pending => fail_session(
            &artifacts,
            "READINESS_PENDING",
            "document remained pending after stable capture",
        ),
    };
    let layout_debug_json = result.layout_debug.unwrap_or_else(|| {
        fail_session(
            &artifacts,
            "LAYOUT_CAPTURE_UNAVAILABLE",
            "Servo did not return cached layout data",
        )
    });
    let layout_debug: serde_json::Value =
        serde_json::from_str(&layout_debug_json).unwrap_or_else(|error| {
            fail_session(&artifacts, "LAYOUT_CAPTURE_INVALID", &error.to_string())
        });
    record_artifact(artifacts.write_layout_debug(&layout_debug));
    let layout_debug_path = artifacts.directory().join("layout-debug.json");

    let rendered_bytes = std::fs::metadata(&proof)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if rendered_bytes == 0 {
        fail_session(
            &artifacts,
            "RENDER_OUTPUT_MISSING",
            "Servo did not produce a rendered image",
        );
    }
    record_artifact(artifacts.record_state("rendered", None));

    println!(
        "{}",
        serde_json::json!({
            "artifacts": artifacts.directory().to_string_lossy(),
            "engine": "pliego",
            "document_root": document.root().to_string_lossy(),
            "environment": environment,
            "environment_artifact": environment_path.to_string_lossy(),
            "input": document.path().to_string_lossy(),
            "layout_debug": layout_debug_path.to_string_lossy(),
            "readiness": readiness_payload,
            "rendered_image": proof.to_string_lossy(),
            "servo_base_sha": SERVO_BASE_SHA,
            "servo_build": servoshell::VERSION,
            "rendered_bytes": rendered_bytes,
            "status": "rendered"
        })
    );
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, Default, PartialEq)]
struct PendingResource {
    urls: Vec<String>,
    response_status: Option<u16>,
    content_type: Option<String>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl PendingResource {
    fn observe_url(&mut self, url: String) -> bool {
        if self.urls.iter().any(|observed| observed == &url) {
            return false;
        }
        self.urls.push(url);
        true
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, PartialEq)]
struct CompletedResource {
    urls: Vec<String>,
    response_status: Option<u16>,
    content_type: Option<String>,
    sha256: String,
    body: Vec<u8>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn record_resources(
    artifacts: &SessionArtifacts,
    resources: Vec<servoshell::ResourceEvent>,
) -> Option<String> {
    let mut pending: HashMap<String, PendingResource> = HashMap::new();

    for resource in resources {
        match resource.event {
            servoshell::NetworkEvent::HttpRequest(request)
            | servoshell::NetworkEvent::HttpRequestUpdate(request) => {
                let url = request.url.into_string();
                let pending_resource = pending.entry(resource.request_id.clone()).or_default();
                if pending_resource.observe_url(url.clone()) {
                    record_artifact(artifacts.record_resource_request(&resource.request_id, &url));
                }
            },
            servoshell::NetworkEvent::HttpResponse(response) => {
                if let Some(pending_resource) = pending.get_mut(&resource.request_id) {
                    pending_resource.response_status = Some(response.status.raw_code());
                    if let Some(content_type) = response
                        .headers
                        .as_ref()
                        .and_then(|headers| headers.get("content-type"))
                        .and_then(|value| value.to_str().ok())
                    {
                        pending_resource.content_type = Some(content_type.to_owned());
                    }
                }

                let Some(completed) = complete_resource(
                    &mut pending,
                    &resource.request_id,
                    response.body.map(|body| body.0),
                ) else {
                    continue;
                };
                record_artifact(artifacts.record_loaded_resource(
                    &resource.request_id,
                    &completed.urls,
                    completed.response_status,
                    completed.content_type.as_deref(),
                    &completed.sha256,
                    &completed.body,
                ));
            },
            servoshell::NetworkEvent::SecurityInfo(_) => {},
        }
    }

    pending
        .into_values()
        .flat_map(|resource| resource.urls)
        .filter(|url| url.starts_with("file:"))
        .min()
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn complete_resource(
    pending: &mut HashMap<String, PendingResource>,
    request_id: &str,
    body: Option<Vec<u8>>,
) -> Option<CompletedResource> {
    let body = body?;
    let pending = pending.remove(request_id)?;
    let sha256 =
        Sha256::digest(&body)
            .iter()
            .fold(String::with_capacity(64), |mut output, byte| {
                output.push_str(&format!("{byte:02x}"));
                output
            });
    Some(CompletedResource {
        urls: pending.urls,
        response_status: pending.response_status,
        content_type: pending.content_type,
        sha256,
        body,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn record_artifact(result: std::io::Result<()>) {
    if let Err(error) = result {
        eprintln!("pliego: cannot write session artifact: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn fail_session(artifacts: &SessionArtifacts, code: &str, message: &str) -> ! {
    let failure = serde_json::json!({
        "status": "failed",
        "error": {
            "code": code,
            "message": message,
        }
    });
    if let Err(error) = artifacts.write_readiness(&failure) {
        eprintln!("pliego: warning: cannot write readiness artifact: {error}");
    }
    if let Err(error) = artifacts.record_state("failed", Some(message)) {
        eprintln!("pliego: warning: cannot record failed session state: {error}");
    }
    println!(
        "{}",
        serde_json::json!({
            "artifacts": artifacts.directory().to_string_lossy(),
            "engine": "pliego",
            "error": failure["error"],
            "status": "failed",
        })
    );
    eprintln!("pliego: {code}: {message}");
    std::process::exit(1)
}

#[cfg(any(target_os = "android", target_env = "ohos"))]
fn render(_request: RenderRequest) {
    eprintln!("pliego: the command-line renderer is only available on desktop targets");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        Command, DEFAULT_LOCALE, DEFAULT_TIMEZONE, PendingResource, RenderEnvironment,
        RenderRequest, complete_resource, parse_args,
    };
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn accepts_only_help_version_or_one_document() {
        assert_eq!(parse_args(vec![]).unwrap(), Command::Help);
        assert_eq!(
            parse_args(vec![OsString::from("--version")]).unwrap(),
            Command::Version
        );
        assert_eq!(
            parse_args(vec![OsString::from("invoice.html")]).unwrap(),
            Command::Render(RenderRequest {
                input: PathBuf::from("invoice.html"),
                environment: RenderEnvironment {
                    locale: DEFAULT_LOCALE,
                    timezone: DEFAULT_TIMEZONE,
                },
            })
        );
        assert!(
            parse_args(vec![
                OsString::from("invoice.html"),
                OsString::from("extra")
            ])
            .is_err()
        );
    }

    #[test]
    fn validates_and_resolves_the_deterministic_environment() {
        assert_eq!(
            parse_args(vec![
                OsString::from("--timezone"),
                OsString::from("PST8PDT"),
                OsString::from("--locale"),
                OsString::from("en-US"),
                OsString::from("invoice.html"),
            ])
            .unwrap(),
            Command::Render(RenderRequest {
                input: PathBuf::from("invoice.html"),
                environment: RenderEnvironment {
                    locale: "en-US",
                    timezone: "PST8PDT",
                },
            })
        );

        let invalid_locale = parse_args(vec![
            OsString::from("--locale"),
            OsString::from("de-DE"),
            OsString::from("invoice.html"),
        ])
        .unwrap_err();
        assert!(invalid_locale.contains("unsupported locale"));

        let invalid_timezone = parse_args(vec![
            OsString::from("--timezone"),
            OsString::from("America/Tijuana"),
            OsString::from("invoice.html"),
        ])
        .unwrap_err();
        assert!(invalid_timezone.contains("unsupported timezone"));
    }

    #[test]
    fn completes_a_resource_only_when_its_body_arrives_and_hashes_exact_bytes() {
        let mut resource = PendingResource {
            response_status: Some(200),
            content_type: Some("text/css; charset=utf-8".to_owned()),
            ..PendingResource::default()
        };
        assert!(resource.observe_url("file:///style.css".to_owned()));
        assert!(!resource.observe_url("file:///style.css".to_owned()));
        assert!(resource.observe_url("file:///theme.css".to_owned()));
        let mut pending = HashMap::from([("request-1".to_owned(), resource)]);

        assert_eq!(complete_resource(&mut pending, "request-1", None), None);
        assert!(pending.contains_key("request-1"));
        let completed = complete_resource(&mut pending, "request-1", Some(b"hello".to_vec()))
            .expect("a body completes the resource");
        assert_eq!(completed.urls, ["file:///style.css", "file:///theme.css"]);
        assert_eq!(completed.response_status, Some(200));
        assert_eq!(
            completed.content_type.as_deref(),
            Some("text/css; charset=utf-8")
        );
        assert_eq!(
            completed.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(completed.body, b"hello");
        assert!(pending.is_empty());
    }
}
