/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::collections::HashMap;
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

#[derive(Debug, PartialEq)]
enum Command {
    Help,
    Version,
    Render(PathBuf),
}

fn parse_args(args: Vec<OsString>) -> Result<Command, String> {
    match args.as_slice() {
        [] => Ok(Command::Help),
        [flag] if flag == "-h" || flag == "--help" => Ok(Command::Help),
        [flag] if flag == "-V" || flag == "--version" || flag == "--verbose-version" => {
            Ok(Command::Version)
        },
        [input] if !input.to_string_lossy().starts_with('-') => {
            Ok(Command::Render(PathBuf::from(input)))
        },
        _ => Err("usage: pliego <document.html>".into()),
    }
}

fn main() {
    let command = parse_args(std::env::args_os().skip(1).collect()).unwrap_or_else(|error| {
        eprintln!("pliego: {error}");
        std::process::exit(2)
    });

    match command {
        Command::Help => print_help(),
        Command::Version => print_version(),
        Command::Render(input) => render(input),
    }
}

fn print_help() {
    println!(
        "Pliego — native document rendering on Servo\n\nUsage:\n  pliego <document.html>\n  pliego --version"
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

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn render(input: PathBuf) {
    let document = LocalDocument::resolve(".", &input).unwrap_or_else(|error| {
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
fn render(_input: PathBuf) {
    eprintln!("pliego: the command-line renderer is only available on desktop targets");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Command, PendingResource, complete_resource, parse_args};
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
            Command::Render(PathBuf::from("invoice.html"))
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
