/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::cell::{Cell, OnceCell, RefCell};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::rc::Rc;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use base64::Engine as _;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use embedder_traits::WebResourceLoadRole;
use layout::pages::{PageDefinition, PageMargins};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::Operation;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::capture::{CapturedFontSource, SceneCapture, capture_document_scene_with_canvas};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::pdf::{CSS_PX_TO_PDF_PT, PdfFontResource, PdfFontVariation, render_document_pdf};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::raster::{RasterFontResource, RasterFontVariation, render_pages_png_with_images};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use readiness::{Readiness, ReadinessPolicy, parse_snapshot};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use session::PreparedPublicationError;
use session::{LocalDocument, SessionArtifacts};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use sha2::{Digest, Sha256};

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
mod asset_cache;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[allow(dead_code)]
mod document_session;
mod engine;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
mod owned_resource_store;
mod readiness;
mod render_environment;
mod resource_policy;
mod session;

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
use document_session::{DocumentCaptureOutcome, DocumentSession, SessionError};
use engine::{
    DocumentEngine, ExplicitRenderPaths, RenderEnvironment, RenderError, RenderOutcome,
    RenderRequest,
};
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
use owned_resource_store::OwnedResourceStore;
use render_environment::{DEFAULT_LOCALE, DEFAULT_TIMEZONE};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use render_environment::{apply_timezone, unexpected_host_font};
#[cfg(all(test, not(any(target_os = "android", target_env = "ohos"))))]
use resource_policy::classify_controlled_http_status;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use resource_policy::{
    ControlledResource, RESOURCE_POLICY_ID, ResourcePolicy, ResourcePolicyDecision,
    ResourcePolicyFailure, ResourcePolicySetupFailure, ResourceRequest,
    create_controlled_http_client, fetch_controlled_http, http_root_allows,
    retain_controlled_resource as retain_shared_controlled_resource,
};
use resource_policy::{
    DEFAULT_RESOURCE_TIMEOUT_MS, MAX_RESOURCE_TIMEOUT_MS, ResourcePolicyConfig, VirtualResourceSpec,
};
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
use resource_policy::{
    MAX_RESOURCE_METADATA_BYTES, ResourceAccounting, ResourceEvidence, ResourceSource,
};

const SERVO_BASE_SHA: &str = "313b6d5ecc113b08010ce434140db3ca5abcc71c";
const PLIEGO_API_VERSION: u32 = 1;
const SESSION_CREATE_ATTEMPTS: u32 = 32;
const DEFAULT_PAGE_WIDTH_CSS_PX: f32 = 793.7008;
const DEFAULT_PAGE_HEIGHT_CSS_PX: f32 = 1122.5197;
const DEFAULT_PAGE_MARGIN_VERTICAL_CSS_PX: f32 = 45.3543;
const DEFAULT_PAGE_MARGIN_HORIZONTAL_CSS_PX: f32 = 60.4724;
const RENDER_ID_SCHEMA_MARKER: &[u8] = b"pliego.render-id.v1";
const RESOLVED_INPUT_HASH_SCHEMA_MARKER: &[u8] = b"pliego.resolved-input.v1";

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn resource_request(request: &servoshell::WebResourceRequest) -> ResourceRequest {
    ResourceRequest {
        method: request.method.to_string(),
        url: request.url.clone(),
        destination: format!("{:?}", request.destination),
        load_role: request.load_role,
        referrer_url: request.referrer_url.clone(),
        is_for_main_frame: request.is_for_main_frame,
        is_redirect: request.is_redirect,
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn decide_resource_policy(
    policy: &ResourcePolicy,
    document_root: &Path,
    request: &servoshell::WebResourceRequest,
) -> ResourcePolicyDecision {
    policy.decide(document_root, &resource_request(request))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn first_fatal_policy_failure(
    failures: &[ResourcePolicyFailure],
) -> Option<&ResourcePolicyFailure> {
    failures
        .iter()
        .find(|failure| !failure.is_optional_metadata_failure())
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn policy_failure_for_pending(url: String, response_status: Option<u16>) -> ResourcePolicyFailure {
    let (code, status, reason) =
        if response_status.is_some_and(|status| matches!(status, 404 | 410)) {
            (
                "RESOURCE_NOT_FOUND",
                "not_found",
                "controlled HTTP resource was not found",
            )
        } else {
            (
                "RESOURCE_TIMEOUT",
                "timeout",
                "controlled HTTP resource did not complete before the render deadline",
            )
        };
    ResourcePolicyFailure {
        code,
        status,
        fatal: true,
        url,
        method: "GET".into(),
        destination: "Unknown".into(),
        load_role: WebResourceLoadRole::DocumentContent,
        referrer_url: None,
        is_for_main_frame: false,
        is_redirect: false,
        reason: reason.into(),
    }
}

#[derive(Debug, PartialEq)]
enum Command {
    Help,
    Version,
    Render(RenderRequest),
}

fn parse_args(mut args: Vec<OsString>) -> Result<Command, String> {
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

    let explicit_render = args.first().is_some_and(|argument| argument == "render");
    if explicit_render {
        args.remove(0);
    }
    let mut input = None;
    let mut locale = None;
    let mut timezone = None;
    let mut page_size = None;
    let mut page_margins = None;
    let mut output = None;
    let mut artifacts = None;
    let mut allowed_http_roots = Vec::new();
    let mut virtual_resources = Vec::new();
    let mut asset_manifest = None;
    let mut resource_timeout_ms = None;
    let mut allow_host_fonts = false;
    let mut allow_partial_scene = false;
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
        } else if argument == "--page-size" {
            if page_size.is_some() {
                return Err("--page-size may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--page-size requires a value".to_owned())?;
            page_size = Some(parse_page_size(&value)?);
        } else if argument == "--page-margins" {
            if page_margins.is_some() {
                return Err("--page-margins may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--page-margins requires a value".to_owned())?;
            page_margins = Some(parse_page_margins(&value)?);
        } else if argument == "--output" {
            if !explicit_render {
                return Err("--output is only valid with `pliego render`".into());
            }
            if output.is_some() {
                return Err("--output may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--output requires a value".to_owned())?;
            if value.is_empty() {
                return Err("--output may not be empty".into());
            }
            output = Some(PathBuf::from(value));
        } else if argument == "--artifacts" {
            if !explicit_render {
                return Err("--artifacts is only valid with `pliego render`".into());
            }
            if artifacts.is_some() {
                return Err("--artifacts may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--artifacts requires a value".to_owned())?;
            if value.is_empty() {
                return Err("--artifacts may not be empty".into());
            }
            artifacts = Some(PathBuf::from(value));
        } else if argument == "--allow-http-root" {
            let value = args
                .next()
                .ok_or_else(|| "--allow-http-root requires a value".to_owned())?;
            allowed_http_roots.push(parse_http_root(&value)?);
        } else if argument == "--virtual-resource" {
            let value = args
                .next()
                .ok_or_else(|| "--virtual-resource requires URL=FILE".to_owned())?;
            virtual_resources.push(parse_virtual_resource(&value)?);
        } else if argument == "--asset-manifest" {
            if asset_manifest.is_some() {
                return Err("--asset-manifest may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--asset-manifest requires a file".to_owned())?;
            if value.is_empty() {
                return Err("--asset-manifest may not be empty".into());
            }
            asset_manifest = Some(PathBuf::from(value));
        } else if argument == "--resource-timeout-ms" {
            if resource_timeout_ms.is_some() {
                return Err("--resource-timeout-ms may only be specified once".into());
            }
            let value = args
                .next()
                .ok_or_else(|| "--resource-timeout-ms requires a value".to_owned())?;
            resource_timeout_ms = Some(parse_resource_timeout(&value)?);
        } else if argument == "--allow-host-fonts" {
            allow_host_fonts = true;
        } else if argument == "--allow-partial-scene" {
            allow_partial_scene = true;
        } else if argument.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option: {}", argument.to_string_lossy()));
        } else if input.replace(PathBuf::from(argument)).is_some() {
            return Err("exactly one document path is required".into());
        }
    }

    let explicit_paths = if explicit_render {
        Some(ExplicitRenderPaths {
            output: output.ok_or_else(|| "`pliego render` requires --output".to_owned())?,
            artifacts: artifacts
                .ok_or_else(|| "`pliego render` requires --artifacts".to_owned())?,
        })
    } else {
        None
    };
    let input = input.ok_or_else(|| "a document path is required".to_owned())?;
    allowed_http_roots.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    allowed_http_roots.dedup();
    virtual_resources.sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
    if virtual_resources
        .windows(2)
        .any(|resources| resources[0].url == resources[1].url)
    {
        return Err("--virtual-resource URLs must be unique".into());
    }
    let page = match (page_size, page_margins) {
        (None, None) => default_page(),
        (page_size, page_margins) => {
            let (page_width, page_height) =
                page_size.unwrap_or((DEFAULT_PAGE_WIDTH_CSS_PX, DEFAULT_PAGE_HEIGHT_CSS_PX));
            PageDefinition::new(
                page_width,
                page_height,
                page_margins.unwrap_or_else(default_page_margins),
            )
            .map_err(|error| format!("invalid page geometry: {error}"))?
        },
    };
    Ok(Command::Render(RenderRequest {
        input,
        environment: RenderEnvironment {
            locale: locale.unwrap_or(DEFAULT_LOCALE),
            timezone: timezone.unwrap_or(DEFAULT_TIMEZONE),
        },
        page,
        resources: ResourcePolicyConfig {
            allowed_http_roots,
            virtual_resources,
            asset_manifest,
            timeout_ms: resource_timeout_ms.unwrap_or(DEFAULT_RESOURCE_TIMEOUT_MS),
        },
        allow_host_fonts,
        allow_partial_scene,
        explicit_paths,
    }))
}

fn parse_http_root(value: &OsString) -> Result<url::Url, String> {
    let value = value
        .to_str()
        .ok_or_else(|| "HTTP root must be valid UTF-8".to_owned())?;
    let mut root = url::Url::parse(value).map_err(|error| format!("invalid HTTP root: {error}"))?;
    if !matches!(root.scheme(), "http" | "https") ||
        root.host_str().is_none() ||
        !root.username().is_empty() ||
        root.password().is_some() ||
        root.query().is_some() ||
        root.fragment().is_some()
    {
        return Err(
            "HTTP root must be an http(s) URL without credentials, query, or fragment".into(),
        );
    }
    if !root.path().ends_with('/') {
        root.set_path(&format!("{}/", root.path()));
    }
    Ok(root)
}

fn parse_virtual_resource(value: &OsString) -> Result<VirtualResourceSpec, String> {
    let value = value
        .to_str()
        .ok_or_else(|| "virtual resource must be valid UTF-8".to_owned())?;
    let (url, path) = value
        .split_once('=')
        .ok_or_else(|| "virtual resource must be URL=FILE".to_owned())?;
    if path.is_empty() {
        return Err("virtual resource file may not be empty".into());
    }
    let url = url::Url::parse(url).map_err(|error| format!("invalid virtual URL: {error}"))?;
    if matches!(url.scheme(), "data" | "file") {
        return Err("virtual resource URL must not use the data or file scheme".into());
    }
    Ok(VirtualResourceSpec {
        url,
        path: PathBuf::from(path),
    })
}

fn parse_resource_timeout(value: &OsString) -> Result<u64, String> {
    let value = value
        .to_str()
        .ok_or_else(|| "resource timeout must be valid UTF-8".to_owned())?;
    let timeout = value
        .parse::<u64>()
        .map_err(|_| "resource timeout must be an integer in milliseconds".to_owned())?;
    if !(1..=MAX_RESOURCE_TIMEOUT_MS).contains(&timeout) {
        return Err(format!(
            "resource timeout must be between 1 and {MAX_RESOURCE_TIMEOUT_MS} milliseconds"
        ));
    }
    Ok(timeout)
}

fn default_page_margins() -> PageMargins {
    PageMargins::new(
        DEFAULT_PAGE_MARGIN_VERTICAL_CSS_PX,
        DEFAULT_PAGE_MARGIN_HORIZONTAL_CSS_PX,
        DEFAULT_PAGE_MARGIN_VERTICAL_CSS_PX,
        DEFAULT_PAGE_MARGIN_HORIZONTAL_CSS_PX,
    )
}

fn default_page() -> PageDefinition {
    PageDefinition::new(
        DEFAULT_PAGE_WIDTH_CSS_PX,
        DEFAULT_PAGE_HEIGHT_CSS_PX,
        default_page_margins(),
    )
    .expect("built-in A4 page geometry is valid")
}

fn parse_page_size(value: &OsString) -> Result<(f32, f32), String> {
    let value = value
        .to_str()
        .ok_or_else(|| "page size must be valid UTF-8".to_owned())?;
    let (width, height) = value
        .split_once('x')
        .ok_or_else(|| "page size must be WIDTHxHEIGHT in CSS pixels".to_owned())?;
    Ok((
        parse_page_number(width, "page width")?,
        parse_page_number(height, "page height")?,
    ))
}

fn parse_page_margins(value: &OsString) -> Result<PageMargins, String> {
    let value = value
        .to_str()
        .ok_or_else(|| "page margins must be valid UTF-8".to_owned())?;
    let values = value
        .split(',')
        .map(|part| parse_page_number(part, "page margin"))
        .collect::<Result<Vec<_>, _>>()?;
    let [top, right, bottom, left] = values.as_slice() else {
        return Err("page margins must be TOP,RIGHT,BOTTOM,LEFT in CSS pixels".into());
    };
    Ok(PageMargins::new(*top, *right, *bottom, *left))
}

fn parse_page_number(value: &str, name: &str) -> Result<f32, String> {
    value
        .parse::<f32>()
        .map_err(|_| format!("{name} must be a number in CSS pixels"))
}

fn parse_locale(value: &OsString) -> Result<&'static str, String> {
    match value.to_str() {
        Some(DEFAULT_LOCALE) => Ok(DEFAULT_LOCALE),
        Some("es-MX") => Ok("es-MX"),
        Some(value) => Err(format!(
            "unsupported locale {value:?}; supported locales: {DEFAULT_LOCALE}, es-MX"
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
        Command::Render(request) => match DocumentEngine::render(request) {
            Ok(outcome) => println!("{}", outcome.summary),
            Err(error) => print_render_error(&error),
        },
    }
}

fn print_help() {
    println!(
        "Pliego — native document rendering on Servo\n\nUsage:\n  pliego render <document.html> --output <document.pdf> --artifacts <directory> [options]\n  pliego [options] <document.html>\n  pliego --version\n\nOptions:\n  --locale en-US|es-MX\n  --timezone UTC|PST8PDT\n  --page-size WIDTHxHEIGHT\n  --page-margins TOP,RIGHT,BOTTOM,LEFT\n  --allow-host-fonts          Opt in to observable system-font resolution\n  --allow-partial-scene       Retain diagnostic output for unsupported paint\n  --allow-http-root URL       Allow GET/HEAD below one explicit http(s) URL root\n  --virtual-resource URL=FILE Serve one exact URL from a host-provided file\n  --asset-manifest FILE       Verify and cache manifest-backed assets locally\n  --resource-timeout-ms MS    Bound controlled network connection time (1..60000)\n\nHost fonts, partial scenes, network, redirects, and asset caching are disabled by default. The shorthand form writes outputs to a temporary artifact directory. Page geometry is expressed in CSS pixels."
    );
}

fn print_version() {
    println!(
        "pliego {}\npliego-api {}\n{}\nServo base {}",
        env!("CARGO_PKG_VERSION"),
        PLIEGO_API_VERSION,
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

#[derive(Debug, PartialEq)]
struct CliRenderError {
    stdout: Option<String>,
    stderr: String,
}

fn cli_render_error(error: &RenderError) -> CliRenderError {
    if let (Some(artifacts), Some(document_pdf), Some(render_id)) = (
        error.artifacts.as_deref(),
        error.document_pdf.as_deref(),
        error.render_id.as_deref(),
    ) {
        CliRenderError {
            stdout: Some(
                serde_json::json!({
                    "artifacts": artifacts.to_string_lossy(),
                    "document_pdf": document_pdf.to_string_lossy(),
                    "engine": "pliego",
                    "error": {
                        "code": &error.code,
                        "message": &error.message,
                    },
                    "render_id": render_id,
                    "status": "failed",
                })
                .to_string(),
            ),
            stderr: format!("pliego: {}: {}", error.code, error.message),
        }
    } else {
        CliRenderError {
            stdout: None,
            stderr: format!("pliego: {}", error.message),
        }
    }
}

fn print_render_error(error: &RenderError) -> ! {
    for warning in &error.warnings {
        eprintln!("pliego: warning: {warning}");
    }
    let output = cli_render_error(error);
    if let Some(stdout) = output.stdout {
        println!("{stdout}");
    }
    eprintln!("{}", output.stderr);
    std::process::exit(error.exit_code)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
struct PublicationTransaction {
    artifacts: SessionArtifacts,
    proof: PathBuf,
    userscripts: PathBuf,
    document_pdf_path: PathBuf,
    environment_path: PathBuf,
    environment: serde_json::Value,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn begin_publication(
    request: &RenderRequest,
    resource_policy: &ResourcePolicy,
    render_id: &str,
) -> Result<PublicationTransaction, RenderError> {
    let artifacts = if let Some(paths) = &request.explicit_paths {
        SessionArtifacts::create_with_render_id(&paths.artifacts, render_id).map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
                "ARTIFACTS_ALREADY_EXISTS"
            } else {
                "ARTIFACTS_CREATE_FAILED"
            };
            RenderError::session(
                &paths.artifacts,
                &paths.output,
                render_id,
                code,
                format!(
                    "cannot create exclusive artifact directory {}: {error}",
                    paths.artifacts.display()
                ),
            )
        })?
    } else {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary_root = std::env::temp_dir().canonicalize().map_err(|error| {
            RenderError::request(
                "ARTIFACTS_CREATE_FAILED",
                format!("cannot resolve the system temporary directory: {error}"),
            )
        })?;
        let session_path =
            temporary_root.join(format!("pliego-session-{}-{unique}", std::process::id()));
        create_session_artifacts(session_path.clone(), render_id).map_err(|error| {
            RenderError::session(
                &session_path,
                &session_path.join("document.pdf"),
                render_id,
                "ARTIFACTS_CREATE_FAILED",
                format!("cannot create session artifacts: {error}"),
            )
        })?
    };
    if let Some(paths) = &request.explicit_paths {
        match output_overlaps_artifacts(&paths.output, artifacts.directory()) {
            Ok(false) => {},
            Ok(true) => {
                return Err(RenderError::session(
                    artifacts.directory(),
                    &paths.output,
                    render_id,
                    "OUTPUT_ARTIFACTS_OVERLAP",
                    "requested output must be outside the artifact directory",
                ));
            },
            Err(error) => {
                return Err(RenderError::session(
                    artifacts.directory(),
                    &paths.output,
                    render_id,
                    "OUTPUT_PATH_CHECK_FAILED",
                    format!("cannot compare output and artifact paths: {error}"),
                ));
            },
        }
    }
    let proof = artifacts.directory().join("render.png");
    let document_pdf_path = request
        .explicit_paths
        .as_ref()
        .map(|paths| paths.output.clone())
        .unwrap_or_else(|| artifacts.directory().join("document.pdf"));
    let record_session_artifact = |result| record_artifact(&artifacts, &document_pdf_path, result);
    let environment_path = artifacts.directory().join("environment.json");
    let mut environment = request.environment.artifact();
    environment["page"] = page_artifact(request.page);
    environment["resource_policy"] = resource_policy.artifact(render_id);
    environment["fonts"] = serde_json::json!({
        "host_fonts": if request.allow_host_fonts { "allowed" } else { "denied" },
    });
    set_document_pdf_environment(&mut environment, &document_pdf_path, "pending", None);
    record_session_artifact(artifacts.write_environment(&environment))?;
    match resource_policy.setup_failure() {
        Some(ResourcePolicySetupFailure::Asset { error, manifest }) => {
            record_session_artifact(artifacts.record_asset_failure(
                error.code,
                manifest,
                error.url.as_deref(),
                &error.message,
                error.expected.as_deref(),
                error.actual.as_deref(),
            ))?;
            return Err(fail_session(
                &artifacts,
                &document_pdf_path,
                error.code,
                &error.message,
            ));
        },
        Some(ResourcePolicySetupFailure::Aggregate { code, message }) => {
            return Err(fail_session(&artifacts, &document_pdf_path, code, &message));
        },
        None => {},
    }
    if request.explicit_paths.is_some() {
        match document_pdf_path.try_exists() {
            Ok(false) => {},
            Ok(true) => {
                return Err(fail_session(
                    &artifacts,
                    &document_pdf_path,
                    "OUTPUT_ALREADY_EXISTS",
                    &format!(
                        "requested output already exists: {}",
                        document_pdf_path.display()
                    ),
                ));
            },
            Err(error) => {
                return Err(fail_session(
                    &artifacts,
                    &document_pdf_path,
                    "OUTPUT_PATH_CHECK_FAILED",
                    &format!(
                        "cannot check requested output {}: {error}",
                        document_pdf_path.display()
                    ),
                ));
            },
        }
    }
    apply_timezone(request.environment.timezone).map_err(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "ENVIRONMENT_CONFIGURATION_FAILED",
            &error,
        )
    })?;
    let userscripts = artifacts.directory().join("userscripts");
    record_session_artifact(std::fs::create_dir_all(&userscripts))?;
    record_session_artifact(std::fs::write(
        userscripts.join("00-pliego-readiness.js"),
        ReadinessPolicy::default().document_start_script(),
    ))?;
    record_session_artifact(artifacts.record_state("started", None))?;

    Ok(PublicationTransaction {
        artifacts,
        proof,
        userscripts,
        document_pdf_path,
        environment_path,
        environment,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn render(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    layout::pages::configure_for_process(request.page).map_err(|_| {
        RenderError::request(
            "LAYOUT_CONFIGURATION_FAILED",
            "paged layout was already configured for this process",
        )
    })?;
    let document = LocalDocument::resolve(".", &request.input)
        .map_err(|error| RenderError::request("INVALID_REQUEST", error.to_string()))?;
    let input_bytes = std::fs::read(document.path()).map_err(|error| {
        RenderError::request(
            "INVALID_REQUEST",
            format!(
                "cannot read input document {}: {error}",
                document.path().display()
            ),
        )
    })?;
    let resource_policy = Rc::new(ResourcePolicy::resolve(&request.resources, document.root()));
    let render_id = stable_render_id(
        &input_bytes,
        request.environment,
        request.page,
        &resource_policy,
        request.allow_host_fonts,
    );
    let input_url = url::Url::from_file_path(document.path()).map_err(|_| {
        RenderError::request(
            "INVALID_REQUEST",
            "cannot convert document path to a file URL",
        )
    })?;
    let PublicationTransaction {
        artifacts,
        proof,
        userscripts,
        document_pdf_path,
        environment_path,
        mut environment,
    } = begin_publication(&request, &resource_policy, &render_id)?;
    let record_session_artifact = |result| record_artifact(&artifacts, &document_pdf_path, result);

    let servo_args = [
        "--headless".into(),
        "--exit".into(),
        "--output".into(),
        proof.to_string_lossy().into_owned(),
        "--userscripts".into(),
        userscripts.to_string_lossy().into_owned(),
        "--window-size".into(),
        format!(
            "{}x{}",
            request.page.width().ceil() as u32,
            request.page.height().ceil() as u32
        ),
        "--pref".into(),
        format!("intl_locale_override={}", request.environment.locale),
        "--pref".into(),
        format!("fonts_host_enabled={}", request.allow_host_fonts),
        "--pref".into(),
        format!(
            "network_connection_timeout={}",
            resource_policy.timeout_ms.div_ceil(1000)
        ),
        input_url.to_string(),
    ];
    let policy_failures = Rc::new(RefCell::new(Vec::new()));
    let captured_policy_failures = Rc::clone(&policy_failures);
    let controlled_resources = Rc::new(RefCell::new(BTreeMap::new()));
    let captured_controlled_resources = Rc::clone(&controlled_resources);
    let captured_controlled_resource_bytes = Rc::new(Cell::new(resource_policy.resident_bytes));
    let document_root = document.root().to_owned();
    let active_resource_policy = Rc::clone(&resource_policy);
    let controlled_http_client = Rc::new(OnceCell::new());
    let _canvas_retention = servo_canvas::retained_canvas::start_retaining_canvas_commands();
    let controlled_runtime_started = Instant::now();
    let result = servoshell::run_with_stable_javascript_and_console_and_web_resource_policy(
        &servo_args,
        readiness::HOST_EVALUATION_EXPRESSION,
        move |request| match decide_resource_policy(
            &active_resource_policy,
            &document_root,
            request,
        ) {
            ResourcePolicyDecision::Allow { .. } => servoshell::WebResourcePolicyDecision::Allow,
            ResourcePolicyDecision::FetchHttp => {
                let client = controlled_http_client
                    .get_or_init(create_controlled_http_client)
                    .clone();
                match fetch_controlled_http(
                    &client,
                    &resource_request(request),
                    &request.headers,
                    active_resource_policy.timeout_ms,
                ) {
                    Ok(response) => {
                        let fetched = ControlledResource {
                            status: response.status.as_u16(),
                            content_type: response
                                .headers
                                .get(http::header::CONTENT_TYPE)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned),
                            body: response.body.clone(),
                        };
                        match retain_controlled_resource(
                            &mut captured_controlled_resources.borrow_mut(),
                            &captured_controlled_resource_bytes,
                            request,
                            fetched,
                        ) {
                            Ok(()) => servoshell::WebResourcePolicyDecision::Synthesize {
                                response: Box::new(
                                    servoshell::WebResourceResponse::new(request.url.clone())
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
                                ),
                                body: response.body,
                            },
                            Err(failure) => {
                                captured_policy_failures.borrow_mut().push(failure);
                                servoshell::WebResourcePolicyDecision::Cancel
                            },
                        }
                    },
                    Err(failure) => {
                        captured_policy_failures.borrow_mut().push(failure);
                        servoshell::WebResourcePolicyDecision::Cancel
                    },
                }
            },
            ResourcePolicyDecision::Synthesize {
                body, content_type, ..
            } => {
                let resource = ControlledResource {
                    status: 200,
                    content_type: Some(content_type.to_owned()),
                    body: body.clone(),
                };
                if let Err(failure) = retain_controlled_resource(
                    &mut captured_controlled_resources.borrow_mut(),
                    &captured_controlled_resource_bytes,
                    request,
                    resource,
                ) {
                    captured_policy_failures.borrow_mut().push(failure);
                    return servoshell::WebResourcePolicyDecision::Cancel;
                }
                let mut headers = http::HeaderMap::new();
                headers.insert(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static(content_type),
                );
                servoshell::WebResourcePolicyDecision::Synthesize {
                    response: Box::new(
                        servoshell::WebResourceResponse::new(request.url.clone()).headers(headers),
                    ),
                    body,
                }
            },
            ResourcePolicyDecision::Fail(failure) => {
                captured_policy_failures.borrow_mut().push(failure);
                servoshell::WebResourcePolicyDecision::Cancel
            },
        },
    );
    let controlled_runtime_ms = elapsed_milliseconds(controlled_runtime_started);
    let policy_failures = std::mem::take(&mut *policy_failures.borrow_mut());
    for failure in &policy_failures {
        record_session_artifact(artifacts.record_resource_failure(
            failure.code,
            failure.status,
            &failure.url,
            &failure.method,
            &failure.destination,
            failure.load_role,
            failure.fatal,
            failure.referrer_url.as_deref(),
            failure.is_for_main_frame,
            failure.is_redirect,
            &failure.reason,
        ))?;
    }
    if let Some(failure) = first_fatal_policy_failure(&policy_failures) {
        return Err(fail_session(
            &artifacts,
            &document_pdf_path,
            failure.code,
            &format!("{}: {}", failure.reason, failure.url),
        ));
    }
    let result = result.map_err(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "READINESS_EVALUATION_FAILED",
            &error.to_string(),
        )
    })?;

    for message in result.console {
        record_session_artifact(artifacts.record_console(
            &format!("{:?}", message.level).to_ascii_lowercase(),
            &message.message,
        ))?;
    }
    let resource_capture = {
        let controlled_resources = controlled_resources.borrow();
        record_resources(
            &artifacts,
            result.resources,
            &resource_policy,
            &controlled_resources,
            &policy_failures,
            &document_pdf_path,
        )
    }?;
    if let Some(failure) = &resource_capture.failure {
        record_session_artifact(artifacts.record_resource_failure(
            failure.code,
            failure.status,
            &failure.url,
            &failure.method,
            &failure.destination,
            failure.load_role,
            failure.fatal,
            failure.referrer_url.as_deref(),
            failure.is_for_main_frame,
            failure.is_redirect,
            &failure.reason,
        ))?;
        return Err(fail_session(
            &artifacts,
            &document_pdf_path,
            failure.code,
            &format!("{}: {}", failure.reason, failure.url),
        ));
    }
    let resolved_input_hash = resolved_input_hash(&render_id, &resource_capture.url_to_resource);
    stage_resolved_input_hash(&mut environment, &resolved_input_hash).map_err(|error| {
        fail_session(&artifacts, &document_pdf_path, error.code, &error.message)
    })?;
    record_session_artifact(artifacts.write_environment(&environment))?;

    let snapshot_json = match result.value {
        servoshell::JSValue::String(json) => json,
        value => {
            return Err(fail_session(
                &artifacts,
                &document_pdf_path,
                "READINESS_INVALID_RESULT",
                &format!("expected readiness JSON string, got {value:?}"),
            ));
        },
    };
    let readiness = parse_snapshot(&snapshot_json).map_err(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "READINESS_INVALID_RESULT",
            &error,
        )
    })?;
    let readiness_json: serde_json::Value =
        serde_json::from_str(&snapshot_json).map_err(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "READINESS_INVALID_RESULT",
                &error.to_string(),
            )
        })?;
    record_session_artifact(artifacts.write_readiness(&readiness_json))?;
    let readiness_payload = match readiness {
        Readiness::Ready { payload } => payload,
        Readiness::Failed { error } => {
            return Err(fail_session(
                &artifacts,
                &document_pdf_path,
                &error.code,
                &error.message,
            ));
        },
        Readiness::Pending => {
            return Err(fail_session(
                &artifacts,
                &document_pdf_path,
                "READINESS_PENDING",
                "document remained pending after stable capture",
            ));
        },
    };
    let layout_debug_json = result.layout_debug.ok_or_else(|| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "SCENE_CAPTURE_UNAVAILABLE",
            "Servo did not return cached layout data",
        )
    })?;
    let layout_debug: serde_json::Value =
        serde_json::from_str(&layout_debug_json).map_err(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "SCENE_CAPTURE_LAYOUT_JSON_INVALID",
                &error.to_string(),
            )
        })?;
    artifacts
        .write_layout_debug(&layout_debug)
        .map_err(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "SCENE_CAPTURE_LAYOUT_WRITE_FAILED",
                &error.to_string(),
            )
        })?;
    let mut resource_resolution_error = None;
    let scene_capture_started = Instant::now();
    let scene_capture = capture_document_scene_with_canvas(
        layout_debug_json.as_bytes(),
        |url| {
            if resource_resolution_error.is_some() {
                return None;
            }
            match resolve_scene_resource(&artifacts, &resource_capture, url) {
                Ok(resource) => resource,
                Err(error) => {
                    resource_resolution_error = Some(error);
                    None
                },
            }
        },
        servo_canvas::retained_canvas::freeze_canvas_snapshots,
    );
    if let Some(error) = resource_resolution_error {
        return Err(fail_session(
            &artifacts,
            &document_pdf_path,
            error.code,
            &error.message,
        ));
    }
    let scene_capture = scene_capture.map_err(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "SCENE_CAPTURE_CONVERSION_FAILED",
            &error.to_string(),
        )
    })?;
    let scene_capture_ms = elapsed_milliseconds(scene_capture_started);
    publish_captured_document(
        &request,
        &document,
        &render_id,
        PublicationTransaction {
            artifacts,
            proof,
            userscripts,
            document_pdf_path,
            environment_path,
            environment,
        },
        CapturedPublication {
            scene_capture,
            readiness_payload,
            resolved_input_hash,
            controlled_runtime_ms,
            scene_capture_ms,
            preserve_staged_readiness: false,
        },
    )
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
struct CapturedPublication {
    scene_capture: SceneCapture,
    readiness_payload: serde_json::Value,
    resolved_input_hash: String,
    controlled_runtime_ms: f64,
    scene_capture_ms: f64,
    preserve_staged_readiness: bool,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn publish_captured_document(
    request: &RenderRequest,
    document: &LocalDocument,
    render_id: &str,
    transaction: PublicationTransaction,
    captured: CapturedPublication,
) -> Result<RenderOutcome, RenderError> {
    let PublicationTransaction {
        artifacts,
        proof,
        userscripts: _,
        document_pdf_path,
        environment_path,
        mut environment,
    } = transaction;
    let CapturedPublication {
        scene_capture,
        readiness_payload,
        resolved_input_hash,
        controlled_runtime_ms,
        scene_capture_ms,
        preserve_staged_readiness,
    } = captured;
    let fail = |code: &str, message: &str| {
        fail_session_with_readiness_policy(
            &artifacts,
            &document_pdf_path,
            code,
            message,
            preserve_staged_readiness,
        )
    };
    let record_session_artifact = |result: std::io::Result<()>| {
        result.map_err(|error| {
            fail(
                "SESSION_ARTIFACT_WRITE_FAILED",
                &format!("cannot write session artifact: {error}"),
            )
        })
    };
    let fail_before_output_commit =
        |environment: &mut serde_json::Value,
         code: &'static str,
         message: String,
         bundle_cleanup_warning: Option<String>| {
            let failure = SceneArtifactError::new(code, message);
            let mut warnings = Vec::new();
            if let Some(warning) = bundle_cleanup_warning {
                warnings.push(warning);
            }
            set_document_pdf_environment(environment, &document_pdf_path, "failed", Some(&failure));
            if let Err(write_error) = artifacts.write_environment(environment) {
                warnings.push(format!(
                    "cannot record failed PDF publication state: {write_error}"
                ));
            }
            let mut error = fail(failure.code, &failure.message);
            warnings.append(&mut error.warnings);
            error.warnings = warnings;
            error
        };

    match stage_resolved_input_hash(&mut environment, &resolved_input_hash) {
        Ok(true) => record_session_artifact(artifacts.write_environment(&environment))?,
        Ok(false) => {},
        Err(error) => return Err(fail(error.code, &error.message)),
    }
    let layout_debug_path = artifacts.directory().join("layout-debug.json");
    if let Some(resource) = unexpected_host_font(&scene_capture, request.allow_host_fonts) {
        return Err(fail(
            "HOST_FONT_POLICY_VIOLATION",
            &format!(
                "Servo selected host font {} while host fonts were disabled",
                resource
            ),
        ));
    }
    let scene_artifacts = match persist_scene_capture(
        &artifacts,
        &scene_capture,
        request.allow_host_fonts,
        request.allow_partial_scene,
    ) {
        Ok(summary) => summary,
        Err(error) => {
            let mut warning = None;
            if error.code.starts_with("DOCUMENT_PDF_") {
                set_document_pdf_environment(
                    &mut environment,
                    &document_pdf_path,
                    "failed",
                    Some(&error),
                );
                if let Err(write_error) = artifacts.write_environment(&environment) {
                    warning = Some(format!(
                        "cannot record failed PDF environment state: {write_error}"
                    ));
                }
            }
            let mut failure = fail(error.code, &error.message);
            if let Some(warning) = warning {
                failure.warnings.insert(0, warning);
            }
            return Err(failure);
        },
    };
    if scene_artifacts.capture_status != "complete" && !request.allow_partial_scene {
        let failure = SceneArtifactError::new(
            scene_artifacts
                .capture_code
                .unwrap_or("SCENE_CAPTURE_INCOMPLETE"),
            format!(
                "document uses paint outside the supported profile; inspect {}",
                scene_artifacts.report_path.display()
            ),
        );
        set_document_pdf_environment(
            &mut environment,
            &document_pdf_path,
            "failed",
            Some(&failure),
        );
        let warning = artifacts
            .write_environment(&environment)
            .err()
            .map(|error| format!("cannot record rejected PDF state: {error}"));
        let mut error = fail(failure.code, &failure.message);
        if let Some(warning) = warning {
            error.warnings.insert(0, warning);
        }
        return Err(error);
    }
    let rendered_bytes = std::fs::metadata(&proof)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if rendered_bytes == 0 {
        return Err(fail(
            "RENDER_OUTPUT_MISSING",
            "Servo did not produce a rendered image",
        ));
    }
    let prepared_output = artifacts
        .prepare_document_pdf(&document_pdf_path)
        .map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
                "OUTPUT_ALREADY_EXISTS"
            } else {
                "OUTPUT_PUBLISH_FAILED"
            };
            fail_before_output_commit(
                &mut environment,
                code,
                format!(
                    "cannot prepare requested output {}: {error}",
                    document_pdf_path.display()
                ),
                None,
            )
        })?;
    set_document_pdf_environment(
        &mut environment,
        &document_pdf_path,
        scene_artifacts.pdf_status,
        None,
    );
    if let Err(error) = artifacts.write_environment(&environment) {
        return Err(fail_before_output_commit(
            &mut environment,
            "DOCUMENT_PDF_ENVIRONMENT_WRITE_FAILED",
            error.to_string(),
            None,
        ));
    }
    let scene_previews = scene_artifacts
        .preview_paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let scene_preview = scene_previews.first().cloned();
    if let Err(error) = artifacts.record_state("rendered", None) {
        return Err(fail_before_output_commit(
            &mut environment,
            "SESSION_ARTIFACT_WRITE_FAILED",
            format!("cannot write session artifact: {error}"),
            None,
        ));
    }
    let prepared_bundle = match artifacts.write_prepared_bundle(&prepared_output) {
        Ok(bundle) => bundle,
        Err(error) => {
            return Err(fail_before_output_commit(
                &mut environment,
                "BUNDLE_WRITE_FAILED",
                error.to_string(),
                None,
            ));
        },
    };
    let bundle_path = prepared_bundle.path().to_owned();

    let outcome = RenderOutcome {
        summary: serde_json::json!({
            "artifacts": artifacts.directory().to_string_lossy(),
            "bundle": bundle_path.to_string_lossy(),
            "engine": "pliego",
            "document_root": document.root().to_string_lossy(),
            "environment": environment.clone(),
            "environment_artifact": environment_path.to_string_lossy(),
            "input": request.input.to_string_lossy(),
            "resolved_input": document.path().to_string_lossy(),
            "layout_debug": layout_debug_path.to_string_lossy(),
            "pages_artifact": scene_artifacts.pages_path.to_string_lossy(),
            "readiness": readiness_payload,
            "render_id": render_id,
            "resolved_input_hash": resolved_input_hash,
            "rendered_image": proof.to_string_lossy(),
            "scene": {
                "schema": scene_capture.scene.schema,
                "version": scene_capture.scene.version,
                "hash": scene_artifacts.scene_hash,
                "validation": "valid",
                "capture_status": scene_artifacts.capture_status,
                "capture_code": scene_artifacts.capture_code,
                "preview_status": scene_artifacts.preview_status,
                "unsupported_event_count": scene_capture.unsupported_events.len(),
                "text_mapping_gap_count": scene_capture.text_mapping_gaps.len(),
            },
            "scene_artifact": scene_artifacts.scene_path.to_string_lossy(),
            "fonts_artifact": scene_artifacts.fonts_path.to_string_lossy(),
            "scene_report": scene_artifacts.report_path.to_string_lossy(),
            "scene_preview": scene_preview,
            "scene_previews": scene_previews,
            "document_pdf": document_pdf_path.to_string_lossy(),
            "document_pdf_status": scene_artifacts.pdf_status,
            "pdf_structure": scene_artifacts.pdf_structure_path.to_string_lossy(),
            "pdf_structure_status": scene_artifacts.pdf_structure_status,
            "phase_timings_ms": {
                "controlled_runtime": controlled_runtime_ms,
                "scene_capture": scene_capture_ms,
                "scene_setup": scene_artifacts.scene_setup_ms,
                "preview_raster": scene_artifacts.preview_ms,
                "pdf_serialize": scene_artifacts.pdf_ms,
            },
            "servo_base_sha": SERVO_BASE_SHA,
            "servo_build": servoshell::VERSION,
            "rendered_bytes": rendered_bytes,
            "status": "rendered"
        }),
    };
    if let Err(error) = prepared_output.commit(&prepared_bundle) {
        let bundle_cleanup_warning = prepared_bundle.discard().err().map(|cleanup_error| {
            format!(
                "cannot remove invalidated owned bundle after failed PDF publication: {cleanup_error}"
            )
        });
        let (code, message) = match error {
            PreparedPublicationError::Bundle(error) => (
                "BUNDLE_INVALID",
                format!("prepared bundle changed before output publication: {error}"),
            ),
            PreparedPublicationError::Output(error) => {
                let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "OUTPUT_ALREADY_EXISTS"
                } else {
                    "OUTPUT_PUBLISH_FAILED"
                };
                (
                    code,
                    format!(
                        "cannot publish requested output {}: {error}",
                        document_pdf_path.display()
                    ),
                )
            },
        };
        return Err(fail_before_output_commit(
            &mut environment,
            code,
            message,
            bundle_cleanup_warning,
        ));
    }
    prepared_bundle.preserve();
    Ok(outcome)
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
struct ExpectedInputIdentity {
    url: url::Url,
    sha256: String,
    content_address: String,
    bytes: u64,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
struct PreparedDocumentSessionRender {
    request: RenderRequest,
    document: LocalDocument,
    resource_policy: ResourcePolicy,
    render_id: String,
    expected_input: ExpectedInputIdentity,
    publication: PublicationTransaction,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[allow(dead_code)]
fn render_with_document_session(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    let PreparedDocumentSessionRender {
        request,
        document,
        resource_policy,
        render_id,
        expected_input,
        publication,
    } = prepare_document_session_render(request)?;
    let result = DocumentSession::from_resolved(
        &document,
        resource_policy,
        request.environment,
        request.page,
        request.allow_host_fonts,
        ReadinessPolicy::default(),
    )
    .and_then(DocumentSession::capture);

    finish_document_session_render(
        &request,
        &document,
        &render_id,
        &expected_input,
        publication,
        result,
    )
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn prepare_document_session_render(
    request: RenderRequest,
) -> Result<PreparedDocumentSessionRender, RenderError> {
    let document = LocalDocument::resolve(".", &request.input)
        .map_err(|error| RenderError::request("INVALID_REQUEST", error.to_string()))?;
    let input_bytes = std::fs::read(document.path()).map_err(|error| {
        RenderError::request(
            "INVALID_REQUEST",
            format!(
                "cannot read input document {}: {error}",
                document.path().display()
            ),
        )
    })?;
    let resource_policy = ResourcePolicy::resolve(&request.resources, document.root());
    let render_id = stable_render_id(
        &input_bytes,
        request.environment,
        request.page,
        &resource_policy,
        request.allow_host_fonts,
    );
    let input_url = url::Url::from_file_path(document.path()).map_err(|_| {
        RenderError::request(
            "INVALID_REQUEST",
            "cannot convert document path to a file URL",
        )
    })?;
    let input_sha256 = sha256_hex(&input_bytes);
    let expected_input = ExpectedInputIdentity {
        url: input_url,
        content_address: format!("sha256:{input_sha256}"),
        sha256: input_sha256,
        bytes: input_bytes.len() as u64,
    };
    let publication = begin_publication(&request, &resource_policy, &render_id)?;

    Ok(PreparedDocumentSessionRender {
        request,
        document,
        resource_policy,
        render_id,
        expected_input,
        publication,
    })
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn finish_document_session_render(
    request: &RenderRequest,
    document: &LocalDocument,
    render_id: &str,
    expected_input: &ExpectedInputIdentity,
    mut publication: PublicationTransaction,
    result: Result<DocumentCaptureOutcome, SessionError>,
) -> Result<RenderOutcome, RenderError> {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(fail_document_session(
                &mut publication,
                render_id,
                expected_input,
                error,
            ));
        },
    };
    let DocumentCaptureOutcome {
        capture,
        stable_image_png,
        layout_debug,
        environment,
        allow_host_fonts,
        readiness,
        console,
        resources,
        resource_accounting,
        resource_store,
        controlled_runtime_ms,
        scene_capture_ms,
    } = outcome;
    let resource_capture = match persist_document_session_evidence(
        &mut publication,
        Some(&stable_image_png),
        Some(&readiness),
        Some(&layout_debug),
        Some(controlled_runtime_ms),
        Some(scene_capture_ms),
        &console,
        &resources,
        resource_accounting,
        &resource_store,
        None,
    ) {
        Ok(capture) => capture,
        Err(error) => {
            return Err(fail_document_session_publication(
                &publication,
                error.code,
                &error.message,
            ));
        },
    };
    if environment != request.environment || allow_host_fonts != request.allow_host_fonts {
        return Err(fail_document_session_publication(
            &publication,
            "SESSION_CAPTURE_IDENTITY_MISMATCH",
            "direct capture environment does not match the prepared render identity",
        ));
    }
    if let Err(error) = stage_document_session_input(
        &mut publication,
        render_id,
        expected_input,
        &resources,
        &resource_store,
    ) {
        return Err(fail_document_session_publication(
            &publication,
            error.code,
            &error.message,
        ));
    }
    let readiness_payload = match parse_snapshot(&readiness.to_string()) {
        Ok(Readiness::Ready { payload }) => payload,
        Ok(Readiness::Failed { error }) => {
            return Err(fail_document_session_publication(
                &publication,
                &error.code,
                &error.message,
            ));
        },
        Ok(Readiness::Pending) => {
            return Err(fail_document_session_publication(
                &publication,
                "READINESS_PENDING",
                "document remained pending after stable capture",
            ));
        },
        Err(error) => {
            return Err(fail_document_session_publication(
                &publication,
                "READINESS_INVALID_RESULT",
                &error,
            ));
        },
    };
    let resolved_input_hash = resolved_input_hash(render_id, &resource_capture.url_to_resource);

    publish_captured_document(
        request,
        document,
        render_id,
        publication,
        CapturedPublication {
            scene_capture: capture,
            readiness_payload,
            resolved_input_hash,
            controlled_runtime_ms,
            scene_capture_ms,
            preserve_staged_readiness: true,
        },
    )
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn fail_document_session(
    publication: &mut PublicationTransaction,
    render_id: &str,
    expected_input: &ExpectedInputIdentity,
    error: SessionError,
) -> RenderError {
    let SessionError {
        code,
        message,
        resource_failure,
        resources,
        resource_accounting,
        resource_store,
        console,
        capture_evidence,
    } = error;
    let evidence_result = persist_document_session_evidence(
        publication,
        capture_evidence.stable_image_png.as_deref(),
        capture_evidence.readiness.as_ref(),
        capture_evidence.layout_debug.as_ref(),
        capture_evidence.controlled_runtime_ms,
        capture_evidence.scene_capture_ms,
        &console,
        &resources,
        resource_accounting,
        &resource_store,
        resource_failure.as_ref(),
    );
    if let Err(evidence_error) = evidence_result {
        return fail_document_session_publication(
            publication,
            evidence_error.code,
            &evidence_error.message,
        );
    }
    if resources
        .iter()
        .any(|evidence| evidence.request.is_for_main_frame)
    {
        if let Err(binding_error) = stage_document_session_input(
            publication,
            render_id,
            expected_input,
            &resources,
            &resource_store,
        ) {
            return fail_document_session_publication(
                publication,
                binding_error.code,
                &binding_error.message,
            );
        }
    }
    fail_document_session_publication(publication, &code, &message)
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn stage_document_session_input(
    publication: &mut PublicationTransaction,
    render_id: &str,
    expected_input: &ExpectedInputIdentity,
    resources: &[ResourceEvidence],
    resource_store: &OwnedResourceStore,
) -> Result<(), SceneArtifactError> {
    let input_binding = bind_document_session_input(expected_input, resources, resource_store)?;
    publication.environment["input_resource"] = serde_json::json!({
        "render_id": render_id,
        "url": input_binding.url,
        "sha256": input_binding.sha256,
        "resource": input_binding.content_address,
        "bytes": input_binding.bytes,
        "source": "document_root",
        "main_frame": true,
    });
    publication
        .artifacts
        .write_environment(&publication.environment)
        .map_err(|error| {
            SceneArtifactError::new(
                "SESSION_ARTIFACT_WRITE_FAILED",
                format!("cannot persist direct-session input binding: {error}"),
            )
        })
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn fail_document_session_publication(
    publication: &PublicationTransaction,
    code: &str,
    message: &str,
) -> RenderError {
    fail_session_with_readiness_policy(
        &publication.artifacts,
        &publication.document_pdf_path,
        code,
        message,
        true,
    )
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[allow(clippy::too_many_arguments)]
fn persist_document_session_evidence(
    publication: &mut PublicationTransaction,
    stable_image_png: Option<&[u8]>,
    readiness: Option<&serde_json::Value>,
    layout_debug: Option<&serde_json::Value>,
    controlled_runtime_ms: Option<f64>,
    scene_capture_ms: Option<f64>,
    console: &[(String, String)],
    resources: &[ResourceEvidence],
    resource_accounting: ResourceAccounting,
    resource_store: &OwnedResourceStore,
    resource_failure: Option<&ResourcePolicyFailure>,
) -> Result<ResourceCapture, SceneArtifactError> {
    let artifact_error = |label: &str, error: std::io::Error| {
        SceneArtifactError::new(
            "SESSION_ARTIFACT_WRITE_FAILED",
            format!("cannot persist direct-session {label}: {error}"),
        )
    };
    if let Some(png) = stable_image_png {
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(SceneArtifactError::new(
                "STABLE_RENDER_FAILED",
                "direct session returned an invalid stable PNG",
            ));
        }
        publication
            .artifacts
            .write_render_image(png)
            .map_err(|error| artifact_error("stable PNG", error))?;
    }
    if let Some(readiness) = readiness {
        publication
            .artifacts
            .write_readiness(readiness)
            .map_err(|error| artifact_error("readiness evidence", error))?;
    }
    if let Some(layout_debug) = layout_debug {
        publication
            .artifacts
            .write_layout_debug(layout_debug)
            .map_err(|error| artifact_error("layout evidence", error))?;
    }
    for (level, message) in console {
        publication
            .artifacts
            .record_console(level, message)
            .map_err(|error| artifact_error("console evidence", error))?;
    }
    let resource_capture = persist_document_session_resources(
        &publication.artifacts,
        resources,
        resource_store,
        resource_failure,
    )?;
    if let Some(failure) = resource_failure {
        publication
            .artifacts
            .record_resource_failure(
                failure.code,
                failure.status,
                &failure.url,
                &failure.method,
                &failure.destination,
                failure.load_role,
                failure.fatal,
                failure.referrer_url.as_deref(),
                failure.is_for_main_frame,
                failure.is_redirect,
                &failure.reason,
            )
            .map_err(|error| artifact_error("fatal resource evidence", error))?;
    }
    let expected_resource_accounting = if resource_failure.is_some() {
        ResourceAccounting::from_evidence(resources).with_failure()
    } else {
        ResourceAccounting::from_evidence(resources)
    };
    if resource_accounting != expected_resource_accounting {
        return Err(invalid_resource_evidence(
            "resource accounting does not match the persisted terminal evidence",
        ));
    }
    for (label, value) in [
        ("controlled runtime", controlled_runtime_ms),
        ("scene capture", scene_capture_ms),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(SceneArtifactError::new(
                "SESSION_CAPTURE_EVIDENCE_INVALID",
                format!("direct-session {label} timing is not finite and non-negative"),
            ));
        }
    }
    publication.environment["runtime"] = serde_json::json!({
        "adapter": "document-session",
    });
    publication.environment["resource_accounting"] = serde_json::json!({
        "requests": resource_accounting.requests,
        "loaded": resource_accounting.loaded,
        "delegated": resource_accounting.delegated,
        "failed": resource_accounting.failed,
        "body_bytes": resource_accounting.body_bytes,
        "unavailable_bodies": resource_accounting.unavailable_bodies,
    });
    publication.environment["phase_timings_ms"] = serde_json::json!({
        "controlled_runtime": controlled_runtime_ms,
        "scene_capture": scene_capture_ms,
    });
    publication
        .artifacts
        .write_environment(&publication.environment)
        .map_err(|error| artifact_error("environment evidence", error))?;
    Ok(resource_capture)
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn persist_document_session_resources(
    artifacts: &SessionArtifacts,
    resources: &[ResourceEvidence],
    resource_store: &OwnedResourceStore,
    resource_failure: Option<&ResourcePolicyFailure>,
) -> Result<ResourceCapture, SceneArtifactError> {
    let post_retain_failure = resource_failure.filter(|failure| {
        failure.code == "RESOURCE_METADATA_LIMIT_EXCEEDED" &&
            failure.status == "denied" &&
            failure.fatal &&
            failure.reason ==
                format!(
                    "resource evidence exceeds the {}-byte metadata bound",
                    MAX_RESOURCE_METADATA_BYTES
                )
    });
    if !resource_store.loaded_evidence_is_complete(resources, post_retain_failure) {
        return Err(invalid_resource_evidence(
            "loaded resource rows do not exactly represent the owned request occurrences",
        ));
    }
    let mut capture = ResourceCapture::default();
    for (index, evidence) in resources.iter().enumerate() {
        let body = validate_document_session_resource(evidence, resource_store)?;
        let request_id = format!("document-session:{index:06}");
        let url = evidence.request.url.to_string();
        artifacts
            .record_resource_request(&request_id, &url)
            .map_err(|error| {
                SceneArtifactError::new(
                    "SESSION_ARTIFACT_WRITE_FAILED",
                    format!("cannot persist direct-session resource request: {error}"),
                )
            })?;
        let (source, cache_result) = document_session_resource_source(evidence.source);
        let response_headers = evidence.response_headers.as_ref().map(|headers| {
            serde_json::json!({
                "count": headers.count,
                "bytes": headers.bytes,
                "names": headers.names,
                "sha256": headers.sha256,
            })
        });
        let failure = evidence.failure.as_ref().map(|failure| {
            serde_json::json!({
                "code": failure.code,
                "status": failure.status,
                "fatal": failure.fatal,
                "reason": failure.reason,
            })
        });
        let artifact = if evidence.status == "loaded" {
            let content_address = evidence
                .content_address
                .as_deref()
                .expect("validated loaded evidence has a content address");
            let body = body.expect("validated loaded evidence has an owned body");
            let artifact = artifacts
                .write_content_addressed_resource(content_address, body)
                .map_err(|error| {
                    SceneArtifactError::new(
                        "SESSION_ARTIFACT_WRITE_FAILED",
                        format!("cannot persist direct-session resource body: {error}"),
                    )
                })?;
            if evidence.request.method != "HEAD" {
                retain_resource_address(&mut capture, &url, content_address)?;
            }
            Some(artifact)
        } else {
            None
        };
        artifacts
            .record_resource_evidence(serde_json::json!({
                "request_id": request_id,
                "url": url,
                "urls": [url],
                "status": evidence.status,
                "code": evidence.failure.as_ref().map(|failure| failure.code),
                "method": evidence.request.method,
                "destination": evidence.request.destination,
                "load_role": evidence.request.load_role,
                "fatal": evidence.fatal,
                "cancelled": evidence.status == "cancelled",
                "referrer_url": evidence.request.referrer_url,
                "is_for_main_frame": evidence.request.is_for_main_frame,
                "is_redirect": evidence.request.is_redirect,
                "source": source,
                "response_status": evidence.response_status,
                "content_type": evidence.content_type,
                "bytes": evidence.bytes,
                "sha256": evidence.sha256,
                "resource": evidence.content_address,
                "content_hash": evidence.content_address,
                "response_headers": response_headers,
                "cache_result": cache_result,
                "artifact": artifact,
                "failure": failure,
            }))
            .map_err(|error| {
                SceneArtifactError::new(
                    "SESSION_ARTIFACT_WRITE_FAILED",
                    format!("cannot persist direct-session resource evidence: {error}"),
                )
            })?;
    }
    Ok(capture)
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn validate_document_session_resource<'a>(
    evidence: &ResourceEvidence,
    resource_store: &'a OwnedResourceStore,
) -> Result<Option<&'a [u8]>, SceneArtifactError> {
    let has_response_metadata = evidence.response_status.is_some() ||
        evidence.content_type.is_some() ||
        evidence.bytes.is_some() ||
        evidence.sha256.is_some() ||
        evidence.content_address.is_some() ||
        evidence.response_headers.is_some();
    match evidence.status {
        "loaded" => {
            if !matches!(evidence.request.method.as_str(), "GET" | "HEAD") ||
                evidence.request.is_redirect ||
                evidence.source.is_none() ||
                evidence.fatal ||
                evidence.failure.is_some() ||
                evidence.response_status.is_none() ||
                evidence.bytes.is_none() ||
                evidence.sha256.is_none() ||
                evidence.content_address.is_none() ||
                evidence.response_headers.is_none()
            {
                return Err(invalid_resource_evidence(
                    "loaded resource has an invalid terminal evidence shape",
                ));
            }
            let owned = resource_store
                .resolve_request(&evidence.request)
                .ok_or_else(|| {
                    invalid_resource_evidence(
                        "loaded resource request is absent from the owned store",
                    )
                })?;
            let content_address = evidence
                .content_address
                .as_deref()
                .expect("loaded shape checked above");
            let digest = evidence
                .sha256
                .as_deref()
                .expect("loaded shape checked above");
            let body = owned.body();
            if evidence.response_status != Some(owned.status()) ||
                evidence.content_type.as_deref() != owned.content_type() ||
                content_address != owned.content_address() ||
                evidence.response_headers.as_ref() != Some(owned.response_headers()) ||
                evidence.source != Some(owned.source()) ||
                evidence.bytes != Some(body.len() as u64) ||
                sha256_hex(body) != digest ||
                content_address != format!("sha256:{digest}")
            {
                return Err(invalid_resource_evidence(
                    "loaded resource metadata does not match its owned response",
                ));
            }
            if evidence.request.method != "HEAD" &&
                resource_store
                    .resolve_url(evidence.request.url.as_str())
                    .as_deref() !=
                    Some(content_address)
            {
                return Err(invalid_resource_evidence(
                    "loaded resource URL is not bound to its owned content address",
                ));
            }
            Ok(Some(body))
        },
        "delegated" => Err(invalid_resource_evidence(
            "delegated resource evidence has no owned source provenance",
        )),
        "cancelled" => {
            let failure = evidence.failure.as_ref().ok_or_else(|| {
                invalid_resource_evidence("cancelled resource has no failure evidence")
            })?;
            let referrer = evidence.request.referrer_url.as_ref().map(url::Url::as_str);
            if evidence.source.is_some() ||
                evidence.fatal != failure.fatal ||
                !failure.is_optional_metadata_failure() ||
                failure.url != evidence.request.url.as_str() ||
                failure.method != evidence.request.method ||
                failure.destination != evidence.request.destination ||
                failure.load_role != evidence.request.load_role ||
                failure.referrer_url.as_deref() != referrer ||
                failure.is_for_main_frame != evidence.request.is_for_main_frame ||
                failure.is_redirect != evidence.request.is_redirect ||
                has_response_metadata
            {
                return Err(invalid_resource_evidence(
                    "cancelled resource has an invalid terminal evidence shape",
                ));
            }
            Ok(None)
        },
        _ => Err(invalid_resource_evidence(
            "resource evidence has an unknown terminal status",
        )),
    }
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn invalid_resource_evidence(message: impl Into<String>) -> SceneArtifactError {
    SceneArtifactError::new("RESOURCE_EVIDENCE_INVALID", message)
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn document_session_resource_source(
    source: Option<ResourceSource>,
) -> (Option<&'static str>, Option<&'static str>) {
    match source {
        Some(ResourceSource::AssetCache(result)) => (Some("asset_cache"), Some(result)),
        Some(ResourceSource::DataUrl) => (Some("data_url"), None),
        Some(ResourceSource::DocumentRoot) => (Some("document_root"), None),
        Some(ResourceSource::Http) => (Some("http"), None),
        Some(ResourceSource::VirtualResource) => (Some("virtual_resource"), None),
        None => (None, None),
    }
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn retain_resource_address(
    capture: &mut ResourceCapture,
    url: &str,
    resource: &str,
) -> Result<(), SceneArtifactError> {
    if let Some(existing) = capture.url_to_resource.get(url) {
        if existing != resource {
            return Err(SceneArtifactError::new(
                "SCENE_CAPTURE_RESOURCE_MAP_CONFLICT",
                format!("observed URL {url} resolved to both {existing} and {resource}"),
            ));
        }
        return Ok(());
    }
    capture
        .url_to_resource
        .insert(url.to_owned(), resource.to_owned());
    Ok(())
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
struct BoundInputIdentity {
    url: String,
    sha256: String,
    content_address: String,
    bytes: u64,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn bind_document_session_input(
    expected: &ExpectedInputIdentity,
    resources: &[ResourceEvidence],
    resource_store: &OwnedResourceStore,
) -> Result<BoundInputIdentity, SceneArtifactError> {
    let mut main_frames = resources
        .iter()
        .filter(|evidence| evidence.request.is_for_main_frame);
    let evidence = main_frames.next().ok_or_else(|| {
        SceneArtifactError::new(
            "INPUT_RESOURCE_EVIDENCE_MISSING",
            "direct session did not retain main-frame input evidence",
        )
    })?;
    if main_frames.next().is_some() {
        return Err(SceneArtifactError::new(
            "INPUT_RESOURCE_EVIDENCE_AMBIGUOUS",
            "direct session retained more than one main-frame input identity",
        ));
    }
    validate_document_session_resource(evidence, resource_store)?;
    let body = evidence
        .content_address
        .as_deref()
        .and_then(|resource| resource_store.resolve_content(resource));
    let stored_identity = resource_store.resolve_url(expected.url.as_str());
    let matches = evidence.request.method == "GET" &&
        evidence.request.url == expected.url &&
        evidence.request.destination == "Document" &&
        evidence.request.load_role == WebResourceLoadRole::DocumentContent &&
        evidence.request.referrer_url.is_none() &&
        !evidence.request.is_redirect &&
        evidence.source == Some(ResourceSource::DocumentRoot) &&
        evidence.status == "loaded" &&
        !evidence.fatal &&
        evidence.failure.is_none() &&
        evidence.response_status == Some(200) &&
        evidence.bytes == Some(expected.bytes) &&
        evidence.sha256.as_deref() == Some(expected.sha256.as_str()) &&
        evidence.content_address.as_deref() == Some(expected.content_address.as_str()) &&
        stored_identity.as_deref() == Some(expected.content_address.as_str()) &&
        body.is_some_and(|body| {
            body.len() as u64 == expected.bytes && sha256_hex(body) == expected.sha256
        });
    if !matches {
        return Err(SceneArtifactError::new(
            "INPUT_RESOURCE_IDENTITY_MISMATCH",
            "main-frame bytes or request identity differ from the pre-read render identity",
        ));
    }
    Ok(BoundInputIdentity {
        url: expected.url.to_string(),
        sha256: expected.sha256.clone(),
        content_address: expected.content_address.clone(),
        bytes: expected.bytes,
    })
}

fn page_artifact(page: PageDefinition) -> serde_json::Value {
    let margins = page.margins();
    serde_json::json!({
        "size_css_px": {
            "width": page.width(),
            "height": page.height(),
        },
        "margins_css_px": {
            "top": margins.top,
            "right": margins.right,
            "bottom": margins.bottom,
            "left": margins.left,
        },
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn stage_resolved_input_hash(
    environment: &mut serde_json::Value,
    resolved_input_hash: &str,
) -> Result<bool, SceneArtifactError> {
    match environment.get("resolved_input_hash") {
        None => {
            environment["resolved_input_hash"] = serde_json::json!(resolved_input_hash);
            Ok(true)
        },
        Some(serde_json::Value::String(existing)) if existing == resolved_input_hash => Ok(false),
        Some(_) => Err(SceneArtifactError::new(
            "SESSION_CAPTURE_IDENTITY_MISMATCH",
            "resolved input hash differs from the value already staged for publication",
        )),
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn stable_render_id(
    input_bytes: &[u8],
    environment: RenderEnvironment,
    page: PageDefinition,
    resource_policy: &ResourcePolicy,
    allow_host_fonts: bool,
) -> String {
    let margins = page.margins();
    let mut hasher = Sha256::new();
    update_hash_field(&mut hasher, RENDER_ID_SCHEMA_MARKER);
    update_hash_field(&mut hasher, input_bytes);
    update_hash_field(&mut hasher, environment.locale.as_bytes());
    update_hash_field(&mut hasher, environment.timezone.as_bytes());
    for value in [
        page.width(),
        page.height(),
        margins.top,
        margins.right,
        margins.bottom,
        margins.left,
    ] {
        hasher.update(value.to_bits().to_be_bytes());
    }
    if allow_host_fonts {
        update_hash_field(&mut hasher, b"pliego.host-fonts.v1");
    }
    if !resource_policy.allowed_http_roots.is_empty() ||
        !resource_policy.virtual_resources.is_empty() ||
        resource_policy.asset_manifest.is_some() ||
        resource_policy.timeout_ms != DEFAULT_RESOURCE_TIMEOUT_MS
    {
        update_hash_field(&mut hasher, RESOURCE_POLICY_ID.as_bytes());
        update_hash_field(&mut hasher, &resource_policy.timeout_ms.to_be_bytes());
        for root in &resource_policy.allowed_http_roots {
            update_hash_field(&mut hasher, root.as_str().as_bytes());
        }
        for resource in &resource_policy.virtual_resources {
            update_hash_field(&mut hasher, resource.url.as_str().as_bytes());
            match &resource.body {
                Ok(body) => update_hash_field(&mut hasher, body),
                Err(_) => update_hash_field(&mut hasher, b"missing"),
            }
        }
        if let Some(assets) = &resource_policy.assets {
            for (url, content_hash) in assets.identity_entries() {
                update_hash_field(&mut hasher, url.as_bytes());
                update_hash_field(&mut hasher, content_hash.as_bytes());
            }
        } else if let Some(error) = &resource_policy.asset_error {
            update_hash_field(&mut hasher, error.code.as_bytes());
            update_hash_field(&mut hasher, error.message.as_bytes());
        }
    }
    format!("sha256:{}", lowercase_hex(&hasher.finalize()))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn resolved_input_hash(render_id: &str, resources: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    update_hash_field(&mut hasher, RESOLVED_INPUT_HASH_SCHEMA_MARKER);
    update_hash_field(&mut hasher, render_id.as_bytes());
    for (url, resource) in resources {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        update_hash_field(&mut hasher, url.as_bytes());
        update_hash_field(&mut hasher, resource.as_bytes());
    }
    format!("sha256:{}", lowercase_hex(&hasher.finalize()))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn update_hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn set_document_pdf_environment(
    environment: &mut serde_json::Value,
    path: &std::path::Path,
    status: &str,
    error: Option<&SceneArtifactError>,
) {
    environment["document_pdf"] = serde_json::json!({
        "artifact": path.to_string_lossy(),
        "status": status,
        "error": error.map(|error| serde_json::json!({
            "code": error.code,
            "message": &error.message,
        })),
    });
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn output_overlaps_artifacts(output: &Path, artifacts: &Path) -> std::io::Result<bool> {
    let output = lexical_absolute_path(output)?;
    let artifacts_lexical = lexical_absolute_path(artifacts)?;
    if output.starts_with(&artifacts_lexical) {
        return Ok(true);
    }

    let artifacts = artifacts.canonicalize()?;
    match output.parent().map(Path::canonicalize) {
        Some(Ok(parent)) => Ok(parent.starts_with(artifacts)),
        Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Some(Err(error)) => Err(error),
        None => Ok(false),
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn lexical_absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in std::path::absolute(path)?.components() {
        match component {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => {
                normalized.pop();
            },
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn create_session_artifacts(base: PathBuf, render_id: &str) -> std::io::Result<SessionArtifacts> {
    let file_name = base.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session artifact path has no final component",
        )
    })?;
    for attempt in 0..SESSION_CREATE_ATTEMPTS {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            let mut retry_name = file_name.to_os_string();
            retry_name.push(format!("-{attempt}"));
            base.with_file_name(retry_name)
        };
        match SessionArtifacts::create_with_render_id(candidate, render_id) {
            Ok(artifacts) => return Ok(artifacts),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "all {SESSION_CREATE_ATTEMPTS} session artifact IDs already exist for {}",
            base.display()
        ),
    ))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, Default, PartialEq)]
struct PendingResource {
    urls: Vec<String>,
    method: Option<String>,
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
    method: Option<String>,
    response_status: Option<u16>,
    content_type: Option<String>,
    sha256: String,
    body: Vec<u8>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn retain_controlled_resource(
    resources: &mut BTreeMap<(String, String), ControlledResource>,
    resident_bytes: &Cell<u64>,
    request: &servoshell::WebResourceRequest,
    resource: ControlledResource,
) -> Result<(), ResourcePolicyFailure> {
    let mut bytes = resident_bytes.get();
    retain_shared_controlled_resource(resources, &mut bytes, &resource_request(request), resource)?;
    resident_bytes.set(bytes);
    Ok(())
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, Default, PartialEq)]
struct ResourceCapture {
    url_to_resource: BTreeMap<String, String>,
    failure: Option<ResourcePolicyFailure>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl ResourceCapture {
    fn retain_completed(
        &mut self,
        completed: &CompletedResource,
    ) -> Result<(), ResourceMapConflict> {
        let resource = format!("sha256:{}", completed.sha256);
        for url in &completed.urls {
            if let Some(existing) = self.url_to_resource.get(url) {
                if existing != &resource {
                    return Err(ResourceMapConflict {
                        url: url.clone(),
                        first: existing.clone(),
                        second: resource,
                    });
                }
            }
        }
        for url in &completed.urls {
            self.url_to_resource.insert(url.clone(), resource.clone());
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, PartialEq)]
struct ResourceMapConflict {
    url: String,
    first: String,
    second: String,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl std::fmt::Display for ResourceMapConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "observed URL {} resolved to both {} and {}",
            self.url, self.first, self.second
        )
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl std::error::Error for ResourceMapConflict {}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn resolve_scene_resource(
    artifacts: &SessionArtifacts,
    capture: &ResourceCapture,
    url: &str,
) -> Result<Option<String>, SceneArtifactError> {
    if let Some(resource) = capture.url_to_resource.get(url) {
        return Ok(Some(resource.clone()));
    }
    let Some(bytes) = decode_data_url(url)
        .map_err(|message| SceneArtifactError::new("SCENE_CAPTURE_DATA_URL_INVALID", message))?
    else {
        return Ok(None);
    };
    let resource = format!("sha256:{}", sha256_hex(&bytes));
    artifacts
        .write_content_addressed_resource(&resource, &bytes)
        .map_err(|error| {
            SceneArtifactError::new(
                "SCENE_CAPTURE_DATA_URL_WRITE_FAILED",
                format!("cannot persist decoded data URL resource: {error}"),
            )
        })?;
    Ok(Some(resource))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn decode_data_url(url: &str) -> Result<Option<Vec<u8>>, String> {
    let Some(scheme) = url.get(..5) else {
        return Ok(None);
    };
    if !scheme.eq_ignore_ascii_case("data:") {
        return Ok(None);
    }
    let (metadata, payload) = url[5..]
        .split_once(',')
        .ok_or_else(|| "data URL has no comma separator".to_owned())?;
    let payload = percent_decode_data(payload)?;
    let is_base64 = metadata
        .rsplit(';')
        .next()
        .is_some_and(|encoding| encoding.eq_ignore_ascii_case("base64"));
    if is_base64 {
        BASE64_STANDARD
            .decode(payload)
            .map(Some)
            .map_err(|error| format!("data URL has invalid base64 payload: {error}"))
    } else {
        Ok(Some(payload))
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn percent_decode_data(value: &str) -> Result<Vec<u8>, String> {
    let input = value.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'%' {
            output.push(input[index]);
            index += 1;
            continue;
        }
        let high = input
            .get(index + 1)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| format!("data URL has invalid percent escape at byte {index}"))?;
        let low = input
            .get(index + 2)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| format!("data URL has invalid percent escape at byte {index}"))?;
        output.push((high << 4) | low);
        index += 3;
    }
    Ok(output)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn record_resources(
    artifacts: &SessionArtifacts,
    resources: Vec<servoshell::ResourceEvent>,
    policy: &ResourcePolicy,
    controlled_resources: &BTreeMap<(String, String), ControlledResource>,
    policy_failures: &[ResourcePolicyFailure],
    document_pdf: &Path,
) -> Result<ResourceCapture, RenderError> {
    let mut pending: HashMap<String, PendingResource> = HashMap::new();
    let mut capture = ResourceCapture::default();
    let record_session_artifact = |result| record_artifact(artifacts, document_pdf, result);

    for resource in resources {
        match resource.event {
            servoshell::NetworkEvent::HttpRequest(request) |
            servoshell::NetworkEvent::HttpRequestUpdate(request) => {
                let method = request.method.to_string();
                let url = request.url.into_string();
                let pending_resource = pending.entry(resource.request_id.clone()).or_default();
                pending_resource.method = Some(method);
                if pending_resource.observe_url(url.clone()) {
                    record_session_artifact(
                        artifacts.record_resource_request(&resource.request_id, &url),
                    )?;
                }
            },
            servoshell::NetworkEvent::HttpResponse(response) => {
                if let Some(pending_resource) = pending.get_mut(&resource.request_id) {
                    pending_resource.response_status = Some(response.status.raw_code());
                    if matches!(response.status.raw_code(), 404 | 408 | 410 | 504) {
                        if let Some(url) = pending_resource.urls.last() {
                            capture.failure.get_or_insert_with(|| {
                                policy_failure_for_pending(
                                    url.clone(),
                                    pending_resource.response_status,
                                )
                            });
                        }
                    }
                    if let Some(content_type) = response
                        .headers
                        .as_ref()
                        .and_then(|headers| headers.get("content-type"))
                        .and_then(|value| value.to_str().ok())
                    {
                        pending_resource.content_type = Some(content_type.to_owned());
                    }
                }

                let Some(mut completed) = complete_resource(
                    &mut pending,
                    &resource.request_id,
                    response.body.map(|body| body.0),
                ) else {
                    continue;
                };
                let cached_asset =
                    policy.assets.as_ref().and_then(|assets| {
                        completed.urls.iter().rev().find_map(|url| {
                            url::Url::parse(url).ok().and_then(|url| assets.get(&url))
                        })
                    });
                if let Some(asset) = cached_asset {
                    // Intercepted response events omit their body. The verified bytes supplied by
                    // this process are authoritative for cache provenance and scene resources.
                    if completed.body.is_empty() && !asset.body.is_empty() {
                        completed.body.clone_from(&asset.body);
                        completed.sha256 = sha256_hex(&completed.body);
                    }
                    if asset.content_hash != format!("sha256:{}", completed.sha256) {
                        capture
                            .failure
                            .get_or_insert_with(|| ResourcePolicyFailure {
                                code: "ASSET_HASH_MISMATCH",
                                status: "hash_mismatch",
                                fatal: true,
                                url: asset.url.to_string(),
                                method: "GET".into(),
                                destination: "Unknown".into(),
                                load_role: WebResourceLoadRole::DocumentContent,
                                referrer_url: None,
                                is_for_main_frame: false,
                                is_redirect: false,
                                reason: "Servo observed bytes that differ from the verified asset"
                                    .into(),
                            });
                    }
                }
                if let Some(fetched) =
                    completed.method.as_ref().and_then(|method| {
                        completed.urls.iter().rev().find_map(|url| {
                            controlled_resources.get(&(method.clone(), url.clone()))
                        })
                    })
                {
                    completed.body.clone_from(&fetched.body);
                    completed.sha256 = sha256_hex(&completed.body);
                    if completed.content_type.is_none() {
                        completed.content_type.clone_from(&fetched.content_type);
                    }
                }
                record_session_artifact(artifacts.record_loaded_resource(
                    &resource.request_id,
                    &completed.urls,
                    completed.response_status,
                    completed.content_type.as_deref(),
                    &completed.sha256,
                    &completed.body,
                    cached_asset.map(|asset| asset.cache_result.as_str()),
                ))?;
                capture.retain_completed(&completed).map_err(|error| {
                    fail_session(
                        artifacts,
                        document_pdf,
                        "SCENE_CAPTURE_RESOURCE_MAP_CONFLICT",
                        &error.to_string(),
                    )
                })?;
            },
            servoshell::NetworkEvent::SecurityInfo(_) => {},
        }
    }

    for ((method, url), fetched) in controlled_resources {
        if capture.url_to_resource.contains_key(url) {
            continue;
        }
        let completed = CompletedResource {
            urls: vec![url.clone()],
            method: Some(method.clone()),
            response_status: Some(fetched.status),
            content_type: fetched.content_type.clone(),
            sha256: sha256_hex(&fetched.body),
            body: fetched.body.clone(),
        };
        let request_id = format!("controlled-resource:{}", sha256_hex(url.as_bytes()));
        record_session_artifact(artifacts.record_resource_request(&request_id, url))?;
        record_session_artifact(artifacts.record_loaded_resource(
            &request_id,
            &completed.urls,
            completed.response_status,
            completed.content_type.as_deref(),
            &completed.sha256,
            &completed.body,
            None,
        ))?;
        capture.retain_completed(&completed).map_err(|error| {
            fail_session(
                artifacts,
                document_pdf,
                "SCENE_CAPTURE_RESOURCE_MAP_CONFLICT",
                &error.to_string(),
            )
        })?;
    }

    if capture.failure.is_none() {
        let failure = incomplete_resource_failure(pending, &capture, policy, policy_failures);
        capture.failure = failure;
    }
    Ok(capture)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn incomplete_resource_failure(
    pending: HashMap<String, PendingResource>,
    capture: &ResourceCapture,
    policy: &ResourcePolicy,
    policy_failures: &[ResourcePolicyFailure],
) -> Option<ResourcePolicyFailure> {
    let mut cancelled_requests = HashMap::<(String, String), usize>::new();
    for failure in policy_failures
        .iter()
        .filter(|failure| failure.is_optional_metadata_failure())
    {
        *cancelled_requests
            .entry((failure.method.clone(), failure.url.clone()))
            .or_default() += 1;
    }

    let mut pending = pending.into_values().collect::<Vec<_>>();
    pending.sort_by(|left, right| {
        left.urls
            .cmp(&right.urls)
            .then_with(|| left.method.cmp(&right.method))
            .then_with(|| left.response_status.cmp(&right.response_status))
    });
    let mut incomplete = Vec::new();
    for resource in pending {
        let method = resource.method.unwrap_or_else(|| "GET".into());
        let cancellation = resource.urls.iter().find_map(|url| {
            let key = (method.clone(), url.clone());
            cancelled_requests
                .get(&key)
                .copied()
                .filter(|count| *count != 0)
                .map(|_| key)
        });
        if let Some(key) = cancellation {
            let count = cancelled_requests.get_mut(&key).unwrap();
            *count -= 1;
            continue;
        }

        for url in resource.urls {
            if capture.url_to_resource.contains_key(&url) {
                continue;
            }
            let controlled = url.starts_with("file:") ||
                policy.allowed_http_roots.iter().any(|root| {
                    url::Url::parse(&url).is_ok_and(|requested| http_root_allows(root, &requested))
                });
            if controlled {
                incomplete.push((url, method.clone(), resource.response_status));
            }
        }
    }
    incomplete.sort();
    incomplete
        .into_iter()
        .next()
        .map(|(url, method, response_status)| {
            if url.starts_with("file:") {
                ResourcePolicyFailure {
                    code: "RESOURCE_NOT_FOUND",
                    status: "not_found",
                    fatal: true,
                    url,
                    method,
                    destination: "Unknown".into(),
                    load_role: WebResourceLoadRole::DocumentContent,
                    referrer_url: None,
                    is_for_main_frame: false,
                    is_redirect: false,
                    reason: "local resource did not complete".into(),
                }
            } else {
                let mut failure = policy_failure_for_pending(url, response_status);
                failure.method = method;
                failure
            }
        })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn complete_resource(
    pending: &mut HashMap<String, PendingResource>,
    request_id: &str,
    body: Option<Vec<u8>>,
) -> Option<CompletedResource> {
    if body.is_none() &&
        !pending
            .get(request_id)?
            .response_status
            .is_some_and(|status| (200..300).contains(&status))
    {
        return None;
    }
    let body = body.unwrap_or_default();
    let pending = pending.remove(request_id)?;
    let sha256 = sha256_hex(&body);
    Some(CompletedResource {
        urls: pending.urls,
        method: pending.method,
        response_status: pending.response_status,
        content_type: pending.content_type,
        sha256,
        body,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn sha256_hex(bytes: &[u8]) -> String {
    lowercase_hex(&Sha256::digest(bytes))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug)]
struct SceneArtifactError {
    code: &'static str,
    message: String,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl SceneArtifactError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, PartialEq)]
struct SceneArtifactSummary {
    scene_hash: String,
    scene_path: PathBuf,
    fonts_path: PathBuf,
    report_path: PathBuf,
    pages_path: PathBuf,
    preview_paths: Vec<PathBuf>,
    pdf_path: PathBuf,
    pdf_status: &'static str,
    pdf_structure_path: PathBuf,
    pdf_structure_status: &'static str,
    capture_status: &'static str,
    capture_code: Option<&'static str>,
    preview_status: &'static str,
    scene_setup_ms: f64,
    preview_ms: f64,
    pdf_ms: f64,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn elapsed_milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, PartialEq, serde::Serialize)]
struct PreviewUnsupported {
    code: &'static str,
    page_index: usize,
    operation_index: usize,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    font: Option<String>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn persist_scene_capture(
    artifacts: &SessionArtifacts,
    capture: &SceneCapture,
    allow_host_fonts: bool,
    allow_partial_scene: bool,
) -> Result<SceneArtifactSummary, SceneArtifactError> {
    let scene_setup_started = Instant::now();
    let scene_bytes = capture.scene.normalized_json().map_err(|message| {
        SceneArtifactError::new("SCENE_CAPTURE_NORMALIZATION_FAILED", message)
    })?;
    let scene_hash = format!("sha256:{}", sha256_hex(&scene_bytes));

    let mut decoded_resources = BTreeMap::<String, Vec<u8>>::new();
    for resource in &capture.font_resources {
        let bytes = BASE64_STANDARD
            .decode(&resource.bytes_base64)
            .map_err(|error| {
                SceneArtifactError::new(
                    "SCENE_CAPTURE_FONT_DECODE_FAILED",
                    format!("cannot decode font resource {}: {error}", resource.resource),
                )
            })?;
        decoded_resources.insert(resource.resource.clone(), bytes);
    }

    let instances_by_id = capture
        .font_instances
        .iter()
        .map(|instance| (instance.id.clone(), instance))
        .collect::<BTreeMap<_, _>>();

    let mut image_resources = capture
        .canvas_resources
        .iter()
        .chain(capture.embedded_image_resources.iter())
        .map(|resource| (resource.resource.clone(), resource.png.clone()))
        .collect::<BTreeMap<_, _>>();
    for (resource, bytes) in &image_resources {
        artifacts
            .write_content_addressed_resource(resource, bytes)
            .map_err(|error| {
                SceneArtifactError::new(
                    "SCENE_CAPTURE_CANVAS_RESOURCE_WRITE_FAILED",
                    format!("cannot persist Canvas resource {resource}: {error}"),
                )
            })?;
    }
    let mut image_resource_errors = BTreeMap::<String, String>::new();
    for operation in capture
        .scene
        .pages
        .iter()
        .flat_map(|page| page.operations.iter())
    {
        let Operation::Image { resource, .. } = operation else {
            continue;
        };
        if image_resources.contains_key(resource) || image_resource_errors.contains_key(resource) {
            continue;
        }
        let result = resource
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                format!("scene image resource is not a SHA-256 content address: {resource}")
            })
            .and_then(|digest| {
                let resource_path = artifacts.directory().join("resources").join(digest);
                std::fs::read(&resource_path).map_err(|error| {
                    format!(
                        "cannot read captured image resource {resource} at {}: {error}",
                        resource_path.display()
                    )
                })
            });
        match result {
            Ok(bytes) => {
                image_resources.insert(resource.clone(), bytes);
            },
            Err(message) => {
                image_resource_errors.insert(resource.clone(), message);
            },
        }
    }

    let mut unsupported = Vec::new();
    for (page_index, page) in capture.scene.pages.iter().enumerate() {
        for (operation_index, operation) in page.operations.iter().enumerate() {
            match operation {
                Operation::Text { font, .. } => {
                    let Some(instance) = instances_by_id.get(font) else {
                        return Err(SceneArtifactError::new(
                            "SCENE_CAPTURE_FONT_INSTANCE_MISSING",
                            format!("scene references missing font instance {font}"),
                        ));
                    };
                    if !instance.variations.is_empty() {
                        unsupported.push(PreviewUnsupported {
                            code: "SCENE_CAPTURE_PREVIEW_UNSUPPORTED_FONT_VARIATIONS",
                            page_index,
                            operation_index,
                            kind: "text",
                            font: Some(font.clone()),
                        });
                    }
                },
                Operation::Image { resource, .. }
                    if image_resource_errors.contains_key(resource) =>
                {
                    unsupported.push(PreviewUnsupported {
                        code: "SCENE_CAPTURE_PREVIEW_UNSUPPORTED_OPERATION",
                        page_index,
                        operation_index,
                        kind: "image",
                        font: None,
                    });
                },
                Operation::Image { .. } => {},
                Operation::Path { .. } | Operation::Link { .. } => {},
            }
        }
    }

    artifacts.write_scene(&scene_bytes).map_err(|error| {
        SceneArtifactError::new("SCENE_CAPTURE_SCENE_WRITE_FAILED", error.to_string())
    })?;
    let manifest = capture
        .font_selections
        .iter()
        .filter(|selection| {
            matches!(
                selection.source,
                CapturedFontSource::Bundled | CapturedFontSource::Data | CapturedFontSource::Memory
            )
        })
        .collect::<Vec<_>>();
    let fonts = serde_json::json!({
        "schema": "pliego.font-report",
        "version": 1,
        "policy": {
            "host_fonts": if allow_host_fonts { "allowed" } else { "denied" },
        },
        "manifest": {
            "resolution": "css-order",
            "entries": manifest,
        },
        "font_resources": capture.font_resources,
        "font_instances": capture.font_instances,
        "selections": capture.font_selections,
        "warnings": capture.font_warnings,
    });
    artifacts.write_fonts(&fonts).map_err(|error| {
        SceneArtifactError::new("SCENE_CAPTURE_FONTS_WRITE_FAILED", error.to_string())
    })?;
    for (resource, bytes) in &decoded_resources {
        artifacts
            .write_content_addressed_resource(resource, bytes)
            .map_err(|error| {
                SceneArtifactError::new(
                    "SCENE_CAPTURE_FONT_RESOURCE_WRITE_FAILED",
                    format!("cannot persist font resource {resource}: {error}"),
                )
            })?;
    }

    let scene_setup_ms = elapsed_milliseconds(scene_setup_started);
    let preview_started = Instant::now();
    let preview_paths = if unsupported.is_empty() {
        let variations_by_instance = capture
            .font_instances
            .iter()
            .map(|instance| {
                (
                    instance.id.clone(),
                    instance
                        .variations
                        .iter()
                        .map(|variation| RasterFontVariation {
                            tag: variation.tag,
                            value: variation.value,
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let pngs = render_pages_png_with_images(
            &capture.scene,
            |font| {
                let instance = instances_by_id.get(font)?;
                let bytes = decoded_resources.get(&instance.resource)?;
                let variations = variations_by_instance.get(font)?;
                Some(RasterFontResource {
                    bytes,
                    face_index: instance.face_index,
                    variations,
                    synthetic_bold: instance.synthetic_bold,
                })
            },
            |image| image_resources.get(image).map(Vec::as_slice),
        )
        .map_err(|error| {
            SceneArtifactError::new("SCENE_CAPTURE_PREVIEW_FAILED", error.to_string())
        })?;
        artifacts.write_scene_previews(&pngs).map_err(|error| {
            SceneArtifactError::new("SCENE_CAPTURE_PREVIEW_WRITE_FAILED", error.to_string())
        })?
    } else {
        Vec::new()
    };

    let preview_pages = capture
        .scene
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let artifact = preview_paths
                .get(index)
                .map(|path| {
                    path.strip_prefix(artifacts.directory()).map_err(|error| {
                        SceneArtifactError::new(
                            "SCENE_CAPTURE_PREVIEW_PATH_INVALID",
                            error.to_string(),
                        )
                    })
                })
                .transpose()?
                .map(|path| path.to_string_lossy().replace('\\', "/"));
            Ok(serde_json::json!({
                "index": index,
                "artifact": artifact,
                "page_size": &page.size,
                "operation_counts": scene_operation_counts(&page.operations),
            }))
        })
        .collect::<Result<Vec<_>, SceneArtifactError>>()?;
    let pages = serde_json::json!({
        "schema": "pliego.pages",
        "version": 1,
        "page_count": capture.scene.pages.len(),
        "pages": &preview_pages,
    });
    artifacts.write_pages(&pages).map_err(|error| {
        SceneArtifactError::new("SCENE_CAPTURE_PAGES_WRITE_FAILED", error.to_string())
    })?;
    let preview_ms = elapsed_milliseconds(preview_started);

    let capture_status =
        if capture.unsupported_events.is_empty() && capture.text_mapping_gaps.is_empty() {
            "complete"
        } else {
            "partial"
        };
    let capture_code = if !capture.text_mapping_gaps.is_empty() {
        Some("SCENE_CAPTURE_LIMITATIONS")
    } else if !capture.unsupported_events.is_empty() {
        Some("SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS")
    } else {
        None
    };
    let render_pdf = capture_status == "complete" || allow_partial_scene;

    let pdf_path = artifacts.directory().join("document.pdf");
    let pdf_structure_path = artifacts.directory().join("pdf-structure.json");
    let mut pdf_written = false;
    let mut pdf_structure_written = false;
    let pdf_started = Instant::now();
    let pdf_result = if render_pdf {
        (|| -> Result<(), SceneArtifactError> {
            let variations_by_instance = capture
                .font_instances
                .iter()
                .map(|instance| {
                    (
                        instance.id.clone(),
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
            if let Some(message) = image_resource_errors.values().next() {
                return Err(SceneArtifactError::new(
                    "DOCUMENT_PDF_GENERATION_FAILED",
                    message.clone(),
                ));
            }

            let pdf = render_document_pdf(
                &capture.scene,
                |font| {
                    let instance = instances_by_id.get(font)?;
                    let bytes = decoded_resources.get(&instance.resource)?;
                    let variations = variations_by_instance.get(font)?;
                    Some(PdfFontResource {
                        bytes,
                        face_index: instance.face_index,
                        variations,
                        synthetic_bold: instance.synthetic_bold,
                    })
                },
                |image| image_resources.get(image).map(Vec::as_slice),
            )
            .map_err(|error| {
                SceneArtifactError::new(
                    "DOCUMENT_PDF_GENERATION_FAILED",
                    format!("cannot generate document.pdf from captured scene: {error}"),
                )
            })?;
            artifacts.write_document_pdf(&pdf).map_err(|error| {
                SceneArtifactError::new(
                    "DOCUMENT_PDF_WRITE_FAILED",
                    format!("cannot write {}: {error}", pdf_path.display()),
                )
            })?;
            pdf_written = true;

            let structure = document_pdf_structure(&capture.scene, &pdf);
            artifacts.write_pdf_structure(&structure).map_err(|error| {
                SceneArtifactError::new(
                    "DOCUMENT_PDF_STRUCTURE_WRITE_FAILED",
                    format!("cannot write {}: {error}", pdf_structure_path.display()),
                )
            })?;
            pdf_structure_written = true;
            Ok(())
        })()
    } else {
        Err(SceneArtifactError::new(
            capture_code.unwrap_or("SCENE_CAPTURE_INCOMPLETE"),
            "partial scene PDF was not published",
        ))
    };
    let pdf_ms = elapsed_milliseconds(pdf_started);
    let pdf_error = pdf_result.err();
    let pdf_status = if pdf_written { "rendered" } else { "failed" };
    let pdf_structure_status = if pdf_structure_written {
        "rendered"
    } else {
        "failed"
    };

    let preview_status = if preview_paths.len() == capture.scene.pages.len() {
        "rendered"
    } else {
        "unsupported"
    };
    let report = serde_json::json!({
        "scene": {
            "schema": capture.scene.schema,
            "version": capture.scene.version,
            "hash": scene_hash,
            "validation": "valid",
        },
        "capture": {
            "status": capture_status,
            "code": capture_code,
            "unsupported_events": capture.unsupported_events,
            "text_mapping_gaps": capture.text_mapping_gaps,
            "canvases": capture.canvas_diagnostics,
        },
        "preview": {
            "status": preview_status,
            "artifact": (preview_paths.len() == 1).then_some("scene-preview.png"),
            "page_count": preview_paths.len(),
            "pages": preview_pages,
            "page_size": &capture.scene.pages[0].size,
            "operation_counts": scene_operation_counts(&capture.scene.pages[0].operations),
            "unsupported": unsupported,
        },
        "document_pdf": {
            "status": pdf_status,
            "artifact": pdf_path.to_string_lossy(),
            "error": if pdf_written {
                None
            } else {
                pdf_error.as_ref().map(|error| serde_json::json!({
                    "code": error.code,
                    "message": &error.message,
                }))
            },
        },
        "pdf_structure": {
            "status": pdf_structure_status,
            "artifact": pdf_structure_path.to_string_lossy(),
            "error": if pdf_structure_written {
                None
            } else {
                pdf_error.as_ref().map(|error| serde_json::json!({
                    "code": error.code,
                    "message": &error.message,
                }))
            },
        },
    });
    artifacts.write_scene_report(&report).map_err(|error| {
        SceneArtifactError::new("SCENE_CAPTURE_REPORT_WRITE_FAILED", error.to_string())
    })?;
    if render_pdf {
        if let Some(error) = pdf_error {
            return Err(error);
        }
    }

    Ok(SceneArtifactSummary {
        scene_hash,
        scene_path: artifacts.directory().join("scene.json"),
        fonts_path: artifacts.directory().join("fonts.json"),
        report_path: artifacts.directory().join("scene-report.json"),
        pages_path: artifacts.directory().join("pages.json"),
        preview_paths,
        pdf_path,
        pdf_status,
        pdf_structure_path,
        pdf_structure_status,
        capture_status,
        capture_code,
        preview_status,
        scene_setup_ms,
        preview_ms,
        pdf_ms,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn document_pdf_structure(scene: &pliego::DocumentScene, pdf: &[u8]) -> serde_json::Value {
    let pages = scene
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let mut source_unicode = String::new();
            let mut embedded_font_ids = Vec::<String>::new();
            for operation in &page.operations {
                match operation {
                    Operation::Text {
                        text: run,
                        font,
                        glyphs,
                        ..
                    } => {
                        if glyphs.is_empty() {
                            continue;
                        }
                        source_unicode.push_str(run);
                        if !embedded_font_ids.contains(font) {
                            embedded_font_ids.push(font.clone());
                        }
                    },
                    Operation::Path { .. } | Operation::Image { .. } | Operation::Link { .. } => {},
                }
            }
            serde_json::json!({
                "index": index,
                "scene_page_size_css_px": &page.size,
                "media_box_pt": [
                    0.0,
                    0.0,
                    page.size.width * CSS_PX_TO_PDF_PT,
                    page.size.height * CSS_PX_TO_PDF_PT,
                ],
                "expected_extracted_unicode": source_unicode,
                "embedded_font_ids": embedded_font_ids,
                "operation_counts": scene_operation_counts(&page.operations),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema": "pliego.pdf-structure",
        "version": 1,
        "backend": "krilla",
        "pdf": {
            "artifact": "document.pdf",
            "sha256": format!("sha256:{}", sha256_hex(pdf)),
            "bytes": pdf.len(),
        },
        "page_count": pages.len(),
        "pages": pages,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn scene_operation_counts(operations: &[Operation]) -> serde_json::Value {
    let mut text = 0;
    let mut vector = 0;
    let mut image = 0;
    let mut link = 0;
    for operation in operations {
        match operation {
            Operation::Text { .. } => text += 1,
            Operation::Path { .. } => vector += 1,
            Operation::Image { .. } => image += 1,
            Operation::Link { .. } => link += 1,
        }
    }
    serde_json::json!({
        "text": text,
        "vector": vector,
        "image": image,
        "link": link,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn record_artifact(
    artifacts: &SessionArtifacts,
    document_pdf: &std::path::Path,
    result: std::io::Result<()>,
) -> Result<(), RenderError> {
    result.map_err(|error| {
        fail_session(
            artifacts,
            document_pdf,
            "SESSION_ARTIFACT_WRITE_FAILED",
            &format!("cannot write session artifact: {error}"),
        )
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn fail_session(
    artifacts: &SessionArtifacts,
    document_pdf: &std::path::Path,
    code: &str,
    message: &str,
) -> RenderError {
    fail_session_with_readiness_policy(artifacts, document_pdf, code, message, false)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn fail_session_with_readiness_policy(
    artifacts: &SessionArtifacts,
    document_pdf: &std::path::Path,
    code: &str,
    message: &str,
    preserve_existing_readiness: bool,
) -> RenderError {
    let failure = serde_json::json!({
        "status": "failed",
        "error": {
            "code": code,
            "message": message,
        }
    });
    let mut warnings = Vec::new();
    if let Err(error) = artifacts.write_failure(code, message) {
        warnings.push(format!("cannot write failure artifact: {error}"));
    }
    let readiness_exists = if preserve_existing_readiness {
        match artifacts.directory().join("readiness.json").try_exists() {
            Ok(exists) => exists,
            Err(error) => {
                warnings.push(format!("cannot inspect readiness artifact: {error}"));
                false
            },
        }
    } else {
        false
    };
    if !readiness_exists && let Err(error) = artifacts.write_readiness(&failure) {
        warnings.push(format!("cannot write readiness artifact: {error}"));
    }
    if let Err(error) = artifacts.record_state("failed", Some(message)) {
        warnings.push(format!("cannot record failed session state: {error}"));
    }
    let mut error = RenderError::session(
        artifacts.directory(),
        document_pdf,
        &artifacts.render_id(),
        code,
        message,
    );
    error.warnings = warnings;
    error
}

#[cfg(any(target_os = "android", target_env = "ohos"))]
fn render(_request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    Err(RenderError::request(
        "UNSUPPORTED_TARGET",
        "the command-line renderer is only available on desktop targets",
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, HashMap};
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use pliego::capture::{
        CapturedFontInstance, CapturedFontResource, CapturedFontSelection, CapturedFontSource,
        CapturedFontWarning, MissingTextMapping, SceneCapture, UnsupportedPaintEvent,
        UnsupportedPaintKind,
    };
    use pliego::{
        Color, DocumentScene, Glyph, Operation, OperationMeta, Page, Rect, Size, Utf8Range,
    };

    #[cfg(feature = "document-session")]
    use super::{
        CapturedPublication, ExpectedInputIdentity, PublicationTransaction, begin_publication,
        finish_document_session_render, publish_captured_document,
    };
    use super::{
        Command, ControlledResource, DEFAULT_LOCALE, DEFAULT_TIMEZONE, ExplicitRenderPaths,
        PageDefinition, PageMargins, PendingResource, RenderEnvironment, RenderError,
        RenderRequest, ResourceCapture, ResourcePolicy, ResourcePolicyConfig,
        ResourcePolicyDecision, ResourcePolicyFailure, ResourceRequest, WebResourceLoadRole,
        classify_controlled_http_status, cli_render_error, complete_resource,
        create_session_artifacts, decide_resource_policy, default_page, first_fatal_policy_failure,
        incomplete_resource_failure, page_artifact, parse_args, persist_scene_capture,
        resolve_scene_resource, retain_controlled_resource, set_document_pdf_environment,
        sha256_hex, stable_render_id,
    };
    #[cfg(feature = "document-session")]
    use crate::document_session::{DocumentCaptureOutcome, SessionError};
    #[cfg(feature = "document-session")]
    use crate::owned_resource_store::OwnedResourceStore;
    #[cfg(feature = "document-session")]
    use crate::resource_policy::{ResourceAccounting, ResourceEvidence, ResourceSource};
    #[cfg(feature = "document-session")]
    use crate::session::LocalDocument;
    use crate::session::SessionArtifacts;

    #[cfg(feature = "document-session")]
    struct RemoveFileOnDrop(PathBuf);

    #[cfg(feature = "document-session")]
    impl Drop for RemoveFileOnDrop {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    const DEJAVU_SANS: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf"
    ));

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
                page: default_page(),
                resources: ResourcePolicyConfig::default(),
                allow_host_fonts: false,
                allow_partial_scene: false,
                explicit_paths: None,
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
    fn missing_input_cli_error_remains_stderr_only() {
        let error = RenderError::request(
            "INVALID_REQUEST",
            "document is unavailable: C:\\workspace\\missing.html",
        );
        let output = cli_render_error(&error);

        assert_eq!(output.stdout, None);
        assert_eq!(
            output.stderr,
            "pliego: document is unavailable: C:\\workspace\\missing.html"
        );
        assert_eq!(error.exit_code, 2);
    }

    #[test]
    fn parses_the_explicit_render_contract_and_requires_exclusive_paths() {
        assert_eq!(
            parse_args(vec![
                OsString::from("render"),
                OsString::from("invoice.html"),
                OsString::from("--output"),
                OsString::from("requested/invoice.pdf"),
                OsString::from("--artifacts"),
                OsString::from("diagnostics/render-1"),
            ])
            .unwrap(),
            Command::Render(RenderRequest {
                input: PathBuf::from("invoice.html"),
                environment: RenderEnvironment::default(),
                page: default_page(),
                resources: ResourcePolicyConfig::default(),
                allow_host_fonts: false,
                allow_partial_scene: false,
                explicit_paths: Some(ExplicitRenderPaths {
                    output: PathBuf::from("requested/invoice.pdf"),
                    artifacts: PathBuf::from("diagnostics/render-1"),
                }),
            })
        );

        for (args, expected) in [
            (
                vec!["render", "invoice.html", "--output", "invoice.pdf"],
                "requires --artifacts",
            ),
            (
                vec!["render", "invoice.html", "--artifacts", "render-1"],
                "requires --output",
            ),
            (
                vec![
                    "render",
                    "invoice.html",
                    "--output",
                    "first.pdf",
                    "--output",
                    "second.pdf",
                    "--artifacts",
                    "render-1",
                ],
                "--output may only be specified once",
            ),
            (
                vec!["invoice.html", "--output", "invoice.pdf"],
                "only valid with `pliego render`",
            ),
        ] {
            let error = parse_args(args.into_iter().map(OsString::from).collect()).unwrap_err();
            assert!(error.contains(expected), "unexpected parser error: {error}");
        }

        let empty_output = parse_args(vec![
            OsString::from("render"),
            OsString::from("invoice.html"),
            OsString::from("--output"),
            OsString::new(),
            OsString::from("--artifacts"),
            OsString::from("render-1"),
        ])
        .unwrap_err();
        assert!(empty_output.contains("--output may not be empty"));
    }

    #[test]
    fn stable_render_ids_depend_on_content_and_normalized_options_not_paths() {
        let first = parse_args(vec![
            OsString::from("render"),
            OsString::from("first/invoice.html"),
            OsString::from("--output"),
            OsString::from("first/invoice.pdf"),
            OsString::from("--artifacts"),
            OsString::from("first/artifacts"),
        ])
        .unwrap();
        let second = parse_args(vec![
            OsString::from("render"),
            OsString::from("second/copy.html"),
            OsString::from("--output"),
            OsString::from("second/copy.pdf"),
            OsString::from("--artifacts"),
            OsString::from("second/diagnostics"),
        ])
        .unwrap();
        let (Command::Render(first), Command::Render(second)) = (first, second) else {
            panic!("explicit commands should parse as render requests")
        };
        assert_ne!(first.input, second.input);
        assert_ne!(first.explicit_paths, second.explicit_paths);

        let input = b"<!doctype html><title>Invoice</title>";
        let policy = ResourcePolicy::default();
        let render_id = stable_render_id(input, first.environment, first.page, &policy, false);
        assert_eq!(
            render_id,
            stable_render_id(input, second.environment, second.page, &policy, false)
        );
        assert!(render_id.starts_with("sha256:"));
        assert_eq!(render_id.len(), 71);
        assert_ne!(
            render_id,
            stable_render_id(
                b"<!doctype html><title>Changed</title>",
                first.environment,
                first.page,
                &policy,
                false,
            )
        );
        assert_ne!(
            render_id,
            stable_render_id(
                input,
                RenderEnvironment {
                    locale: DEFAULT_LOCALE,
                    timezone: "PST8PDT",
                },
                first.page,
                &policy,
                false,
            )
        );
        assert_ne!(
            render_id,
            stable_render_id(
                input,
                first.environment,
                PageDefinition::new(612.0, 792.0, PageMargins::new(72.0, 54.0, 36.0, 18.0),)
                    .unwrap(),
                &policy,
                false,
            )
        );
        let canonical_page =
            PageDefinition::new(612.0, 792.0, PageMargins::new(72.0, 54.0, 36.0, 18.0)).unwrap();
        assert_eq!(
            stable_render_id(
                input,
                RenderEnvironment::default(),
                canonical_page,
                &policy,
                false,
            ),
            "sha256:9ca553323bf5eb8118c51a46c00b174e1ef1febc7e6bbdb3d763ac2dcf5291a3"
        );
        assert_ne!(
            render_id,
            stable_render_id(input, first.environment, first.page, &policy, true)
        );
    }

    #[test]
    fn validates_and_resolves_the_deterministic_environment() {
        assert_eq!(
            parse_args(vec![
                OsString::from("--timezone"),
                OsString::from("PST8PDT"),
                OsString::from("--locale"),
                OsString::from("es-MX"),
                OsString::from("invoice.html"),
            ])
            .unwrap(),
            Command::Render(RenderRequest {
                input: PathBuf::from("invoice.html"),
                environment: RenderEnvironment {
                    locale: "es-MX",
                    timezone: "PST8PDT",
                },
                page: default_page(),
                resources: ResourcePolicyConfig::default(),
                allow_host_fonts: false,
                allow_partial_scene: false,
                explicit_paths: None,
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

        let Command::Render(opted_in) = parse_args(vec![
            OsString::from("--allow-host-fonts"),
            OsString::from("--allow-partial-scene"),
            OsString::from("invoice.html"),
        ])
        .unwrap() else {
            panic!("document should parse as a render request")
        };
        assert!(opted_in.allow_host_fonts);
        assert!(opted_in.allow_partial_scene);
    }

    #[test]
    fn parses_explicit_page_geometry_and_rejects_invalid_values() {
        let Command::Render(request) = parse_args(vec![
            OsString::from("--page-size"),
            OsString::from("612x792"),
            OsString::from("--page-margins"),
            OsString::from("72,54,36,18"),
            OsString::from("invoice.html"),
        ])
        .unwrap() else {
            panic!("page options should produce a render request")
        };
        assert_eq!(request.page.width(), 612.0);
        assert_eq!(request.page.height(), 792.0);
        assert_eq!(
            request.page.margins(),
            PageMargins::new(72.0, 54.0, 36.0, 18.0)
        );
        assert_eq!(page_artifact(request.page)["size_css_px"]["width"], 612.0);

        for args in [
            vec!["--page-size", "612", "invoice.html"],
            vec!["--page-size", "NaNx792", "invoice.html"],
            vec!["--page-margins", "1,2,3", "invoice.html"],
            vec!["--page-margins", "600,0,600,0", "invoice.html"],
        ] {
            assert!(
                parse_args(args.into_iter().map(OsString::from).collect()).is_err(),
                "invalid page options were accepted"
            );
        }
    }

    #[test]
    fn web_resource_load_roles_default_and_round_trip_through_serialization() {
        let legacy: servoshell::WebResourceRequest = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "headers": {},
            "url": "https://example.test/icon.png",
            "destination": "Image",
            "referrer_url": null,
            "is_for_main_frame": false,
            "is_redirect": true,
        }))
        .unwrap();
        assert_eq!(legacy.load_role, WebResourceLoadRole::DocumentContent);

        let mut metadata = legacy;
        metadata.load_role = WebResourceLoadRole::DocumentMetadata;
        let round_trip: servoshell::WebResourceRequest =
            serde_json::from_value(serde_json::to_value(metadata).unwrap()).unwrap();
        assert_eq!(round_trip.load_role, WebResourceLoadRole::DocumentMetadata);
        assert!(round_trip.is_redirect);
    }

    #[test]
    fn legacy_resource_path_ignores_metadata_denials_but_not_budget_failures() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://denied.invalid/report.bin").unwrap(),
            destination: "Image".into(),
            load_role: WebResourceLoadRole::DocumentMetadata,
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };
        let denial = ResourcePolicyFailure::new(
            &request,
            "RESOURCE_DENIED",
            "denied",
            "network URL is outside the configured HTTP roots",
        )
        .nonfatal();
        let budget = ResourcePolicyFailure::new(
            &request,
            "RESOURCE_METADATA_LIMIT_EXCEEDED",
            "denied",
            "resource evidence exceeds its configured bound",
        );

        assert!(first_fatal_policy_failure(std::slice::from_ref(&denial)).is_none());
        let failures = [denial, budget];
        let fatal = first_fatal_policy_failure(&failures).unwrap();
        assert_eq!(fatal.code, "RESOURCE_METADATA_LIMIT_EXCEEDED");
        assert_eq!(fatal.load_role, WebResourceLoadRole::DocumentMetadata);
        assert!(fatal.fatal);

        let mut content_request = request;
        content_request.load_role = WebResourceLoadRole::DocumentContent;
        let malformed_content = ResourcePolicyFailure::new(
            &content_request,
            "RESOURCE_DENIED",
            "denied",
            "malformed nonfatal content denial",
        )
        .nonfatal();
        assert!(first_fatal_policy_failure(std::slice::from_ref(&malformed_content)).is_some());
    }

    #[test]
    fn legacy_incomplete_capture_does_not_refatalize_a_metadata_cancellation() {
        let url = "file:///document/escape.css";
        let pending = |request_ids: &[&str]| {
            request_ids
                .iter()
                .map(|request_id| {
                    (
                        (*request_id).into(),
                        PendingResource {
                            urls: vec![url.into()],
                            method: Some("GET".into()),
                            ..PendingResource::default()
                        },
                    )
                })
                .collect::<HashMap<String, PendingResource>>()
        };
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse(url).unwrap(),
            destination: "Image".into(),
            load_role: WebResourceLoadRole::DocumentMetadata,
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };
        let cancellation = ResourcePolicyFailure::new(
            &request,
            "RESOURCE_DENIED",
            "denied",
            "file is outside the document root",
        )
        .nonfatal();
        let capture = ResourceCapture::default();
        let policy = ResourcePolicy::default();

        assert!(
            incomplete_resource_failure(
                pending(&["metadata"]),
                &capture,
                &policy,
                &[cancellation.clone()],
            )
            .is_none()
        );
        let mut malformed_content = cancellation.clone();
        malformed_content.load_role = WebResourceLoadRole::DocumentContent;
        assert!(
            incomplete_resource_failure(
                pending(&["content"]),
                &capture,
                &policy,
                &[malformed_content],
            )
            .is_some()
        );
        let content_failure = incomplete_resource_failure(
            pending(&["metadata", "content"]),
            &capture,
            &policy,
            &[cancellation],
        )
        .unwrap();
        assert_eq!(content_failure.code, "RESOURCE_NOT_FOUND");
        assert_eq!(
            content_failure.load_role,
            WebResourceLoadRole::DocumentContent
        );
        assert!(content_failure.fatal);
    }

    #[test]
    fn resource_policy_is_rooted_typed_and_can_synthesize_host_resources() {
        fn request_with_method(
            method: &str,
            url: url::Url,
            is_redirect: bool,
        ) -> servoshell::WebResourceRequest {
            serde_json::from_value(serde_json::json!({
                "method": method,
                "headers": {},
                "url": url,
                "destination": "Style",
                "referrer_url": null,
                "is_for_main_frame": false,
                "is_redirect": is_redirect,
            }))
            .unwrap()
        }

        fn request(url: url::Url, is_redirect: bool) -> servoshell::WebResourceRequest {
            request_with_method("GET", url, is_redirect)
        }

        let sandbox = temporary_artifacts("pliego-resource-policy");
        let root = sandbox.join("root");
        let inside = root.join("style.css");
        let font = root.join("FONT.OTF");
        let oversized = root.join("oversized.bin");
        let outside = sandbox.join("outside.css");
        fs::create_dir_all(&root).unwrap();
        fs::write(&inside, "body {}").unwrap();
        fs::write(&font, b"font").unwrap();
        fs::File::create(&oversized)
            .unwrap()
            .set_len(super::asset_cache::MAX_CACHE_BYTES + 1)
            .unwrap();
        fs::write(&outside, "body {}").unwrap();
        fs::write(root.join("asset.css"), b"asset {}").unwrap();
        let asset_url = url::Url::parse("https://assets.test/asset.css").unwrap();
        fs::write(
            root.join("assets.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": "pliego.asset-manifest",
                "version": 1,
                "assets": [{
                    "url": asset_url,
                    "path": "asset.css",
                    "sha256": sha256_hex(b"asset {}"),
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        let virtual_url = url::Url::parse("pliego://host/style.css").unwrap();
        let policy = ResourcePolicy::resolve(
            &ResourcePolicyConfig {
                allowed_http_roots: vec![url::Url::parse("https://example.test/assets/").unwrap()],
                virtual_resources: vec![super::VirtualResourceSpec {
                    url: virtual_url.clone(),
                    path: PathBuf::from("style.css"),
                }],
                asset_manifest: Some(PathBuf::from("assets.json")),
                timeout_ms: 500,
            },
            &root,
        );

        assert!(matches!(
            decide_resource_policy(
                &policy,
                &root,
                &request(
                    url::Url::parse("data:text/css,body%20%7B%7D").unwrap(),
                    false,
                )
            ),
            ResourcePolicyDecision::Allow { .. }
        ));
        for url in [
            url::Url::parse("data:text/plain,hello").unwrap(),
            url::Url::from_file_path(&inside).unwrap(),
            virtual_url.clone(),
            asset_url.clone(),
        ] {
            let ResourcePolicyDecision::Fail(failure) =
                decide_resource_policy(&policy, &root, &request_with_method("POST", url, false))
            else {
                panic!("unsupported method should fail before URL synthesis")
            };
            assert_eq!(
                failure.reason,
                "only GET and HEAD resource requests are allowed"
            );
        }
        let ResourcePolicyDecision::Synthesize {
            body, content_type, ..
        } = decide_resource_policy(
            &policy,
            &root,
            &request(url::Url::from_file_path(&inside).unwrap(), false),
        )
        else {
            panic!("inside-root file should be synthesized")
        };
        assert_eq!(body, b"body {}");
        assert_eq!(content_type, "text/css");
        let ResourcePolicyDecision::Synthesize {
            body, content_type, ..
        } = decide_resource_policy(
            &policy,
            &root,
            &request(url::Url::from_file_path(&font).unwrap(), false),
        )
        else {
            panic!("uppercase OTF should be synthesized")
        };
        assert_eq!(body, b"font");
        assert_eq!(content_type, "font/otf");
        let ResourcePolicyDecision::Fail(oversized_failure) = decide_resource_policy(
            &policy,
            &root,
            &request(url::Url::from_file_path(&oversized).unwrap(), false),
        ) else {
            panic!("oversized local resource should fail")
        };
        assert_eq!(oversized_failure.code, "RESOURCE_DENIED");
        let ResourcePolicyDecision::Fail(outside_failure) = decide_resource_policy(
            &policy,
            &root,
            &request(url::Url::from_file_path(&outside).unwrap(), false),
        ) else {
            panic!("outside-root file should fail")
        };
        assert_eq!(outside_failure.code, "RESOURCE_DENIED");

        let missing = root.join("missing.css");
        let ResourcePolicyDecision::Fail(missing_failure) = decide_resource_policy(
            &policy,
            &root,
            &request(url::Url::from_file_path(&missing).unwrap(), false),
        ) else {
            panic!("missing local file should fail")
        };
        assert_eq!(missing_failure.code, "RESOURCE_NOT_FOUND");

        assert!(matches!(
            decide_resource_policy(
                &policy,
                &root,
                &request(
                    url::Url::parse("https://example.test/assets/style.css").unwrap(),
                    false,
                ),
            ),
            ResourcePolicyDecision::FetchHttp
        ));
        let ResourcePolicyDecision::Fail(network_failure) = decide_resource_policy(
            &policy,
            &root,
            &request(
                url::Url::parse("https://example.test/private/style.css").unwrap(),
                false,
            ),
        ) else {
            panic!("out-of-root network URL should fail")
        };
        assert_eq!(network_failure.code, "RESOURCE_DENIED");

        let ResourcePolicyDecision::Fail(redirect_failure) = decide_resource_policy(
            &policy,
            &root,
            &request(
                url::Url::parse("https://example.test/assets/redirect.css").unwrap(),
                true,
            ),
        ) else {
            panic!("redirect should fail")
        };
        assert_eq!(redirect_failure.reason, "redirects are disabled");

        let ResourcePolicyDecision::Synthesize {
            body, content_type, ..
        } = decide_resource_policy(&policy, &root, &request(virtual_url.clone(), false))
        else {
            panic!("configured host resource should be synthesized")
        };
        assert_eq!(body, b"body {}");
        assert_eq!(content_type, "text/css");
        let ResourcePolicyDecision::Synthesize { body, .. } = decide_resource_policy(
            &policy,
            &root,
            &request_with_method("HEAD", virtual_url, false),
        ) else {
            panic!("HEAD should preserve synthesized response metadata")
        };
        assert!(body.is_empty());

        #[cfg(unix)]
        {
            let escape = root.join("escape.css");
            std::os::unix::fs::symlink(&outside, &escape).unwrap();
            let ResourcePolicyDecision::Fail(failure) = decide_resource_policy(
                &policy,
                &root,
                &request(url::Url::from_file_path(escape).unwrap(), false),
            ) else {
                panic!("symlink outside the root should fail")
            };
            assert_eq!(failure.code, "RESOURCE_DENIED");
        }

        let artifact = policy.artifact("sha256:fixture");
        assert_eq!(artifact["render_id"], "sha256:fixture");
        assert_eq!(artifact["redirects"], "deny");
        assert_eq!(artifact["virtual_resources"][0]["available"], true);

        let missing_virtual = ResourcePolicy::resolve(
            &ResourcePolicyConfig {
                virtual_resources: vec![super::VirtualResourceSpec {
                    url: url::Url::parse("pliego://host/missing.css").unwrap(),
                    path: PathBuf::from("missing.css"),
                }],
                ..ResourcePolicyConfig::default()
            },
            &root,
        );
        let ResourcePolicyDecision::Fail(virtual_failure) = decide_resource_policy(
            &missing_virtual,
            &root,
            &request(url::Url::parse("pliego://host/missing.css").unwrap(), false),
        ) else {
            panic!("missing host resource should fail")
        };
        assert_eq!(virtual_failure.code, "RESOURCE_NOT_FOUND");

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn classifies_controlled_http_failures_without_following_redirects() {
        for (status, code, failure_status, is_redirect) in [
            (http::StatusCode::FOUND, "RESOURCE_DENIED", "denied", true),
            (
                http::StatusCode::NOT_FOUND,
                "RESOURCE_NOT_FOUND",
                "not_found",
                false,
            ),
            (
                http::StatusCode::REQUEST_TIMEOUT,
                "RESOURCE_TIMEOUT",
                "timeout",
                false,
            ),
        ] {
            let classified = classify_controlled_http_status(status).unwrap();
            assert_eq!(
                (classified.0, classified.1, classified.3),
                (code, failure_status, is_redirect)
            );
        }
        assert!(classify_controlled_http_status(http::StatusCode::OK).is_none());
    }

    #[test]
    fn parses_bounded_resource_policy_options() {
        let Command::Render(request) = parse_args(
            [
                "invoice.html",
                "--allow-http-root",
                "https://example.test/assets",
                "--virtual-resource",
                "pliego://host/style.css=style.css",
                "--asset-manifest",
                "assets.json",
                "--resource-timeout-ms",
                "500",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap() else {
            panic!("resource options should produce a render request")
        };
        assert_eq!(
            request.resources.allowed_http_roots[0].as_str(),
            "https://example.test/assets/"
        );
        assert_eq!(request.resources.virtual_resources.len(), 1);
        assert_eq!(
            request.resources.asset_manifest,
            Some(PathBuf::from("assets.json"))
        );
        assert_eq!(request.resources.timeout_ms, 500);

        for args in [
            vec!["invoice.html", "--allow-http-root", "file:///tmp/"],
            vec!["invoice.html", "--resource-timeout-ms", "0"],
            vec![
                "invoice.html",
                "--asset-manifest",
                "one.json",
                "--asset-manifest",
                "two.json",
            ],
            vec![
                "invoice.html",
                "--virtual-resource",
                "pliego://host/style.css=one.css",
                "--virtual-resource",
                "pliego://host/style.css=two.css",
            ],
        ] {
            assert!(
                parse_args(args.into_iter().map(OsString::from).collect()).is_err(),
                "invalid policy options were accepted"
            );
        }
    }

    #[test]
    fn completes_a_resource_and_hashes_exact_bytes() {
        let mut resource = PendingResource {
            method: Some("GET".to_owned()),
            response_status: Some(200),
            content_type: Some("text/css; charset=utf-8".to_owned()),
            ..PendingResource::default()
        };
        assert!(resource.observe_url("file:///style.css".to_owned()));
        assert!(!resource.observe_url("file:///style.css".to_owned()));
        assert!(resource.observe_url("file:///theme.css".to_owned()));
        let mut pending = HashMap::from([("request-1".to_owned(), resource)]);

        let completed = complete_resource(&mut pending, "request-1", Some(b"hello".to_vec()))
            .expect("a body completes the resource");
        assert_eq!(completed.urls, ["file:///style.css", "file:///theme.css"]);
        assert_eq!(completed.method.as_deref(), Some("GET"));
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

        let expected_resource = format!("sha256:{}", completed.sha256);
        let mut capture = ResourceCapture::default();
        capture.retain_completed(&completed).unwrap();
        assert_eq!(
            capture.url_to_resource,
            [
                ("file:///style.css".to_owned(), expected_resource.clone()),
                ("file:///theme.css".to_owned(), expected_resource),
            ]
            .into_iter()
            .collect()
        );

        let conflict = super::CompletedResource {
            urls: vec!["file:///theme.css".to_owned()],
            method: Some("GET".to_owned()),
            response_status: Some(200),
            content_type: None,
            sha256: "0".repeat(64),
            body: vec![],
        };
        let error = capture.retain_completed(&conflict).unwrap_err();
        assert_eq!(error.url, "file:///theme.css");
        assert_eq!(capture.url_to_resource.len(), 2);
    }

    #[test]
    fn retains_synthesized_resource_bytes_and_rejects_changes() {
        let request: servoshell::WebResourceRequest = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "headers": {},
            "url": "file:///document/style.css",
            "destination": "Style",
            "referrer_url": "file:///document/index.html",
            "is_for_main_frame": false,
            "is_redirect": false,
        }))
        .unwrap();
        let original = ControlledResource {
            status: 200,
            content_type: Some("text/css".into()),
            body: b"body { color: black; }".to_vec(),
        };
        let key = ("GET".to_owned(), request.url.to_string());
        let mut resources = BTreeMap::new();
        let resident_bytes = Cell::new(0);

        retain_controlled_resource(&mut resources, &resident_bytes, &request, original.clone())
            .unwrap();
        retain_controlled_resource(&mut resources, &resident_bytes, &request, original.clone())
            .unwrap();
        assert_eq!(resources.get(&key), Some(&original));
        assert_eq!(resident_bytes.get(), original.body.len() as u64);

        let failure = retain_controlled_resource(
            &mut resources,
            &resident_bytes,
            &request,
            ControlledResource {
                body: b"body { color: red; }".to_vec(),
                ..original.clone()
            },
        )
        .unwrap_err();
        assert_eq!(failure.code, "RESOURCE_CHANGED_DURING_RENDER");
        assert_eq!(resources.get(&key), Some(&original));
    }

    #[test]
    fn represents_a_successful_response_without_a_body_as_zero_bytes() {
        let mut resource = PendingResource {
            response_status: Some(204),
            content_type: Some("image/svg+xml".to_owned()),
            ..PendingResource::default()
        };
        resource.observe_url("file:///empty.svg".to_owned());
        let mut pending = HashMap::from([("request-empty".to_owned(), resource)]);

        let completed = complete_resource(&mut pending, "request-empty", None)
            .expect("a successful bodyless response is a zero-byte resource");
        assert_eq!(completed.body, Vec::<u8>::new());
        assert_eq!(
            completed.sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(pending.is_empty());

        let mut failed = PendingResource {
            response_status: Some(404),
            ..PendingResource::default()
        };
        failed.observe_url("file:///missing.svg".to_owned());
        let mut pending = HashMap::from([("request-failed".to_owned(), failed)]);
        assert_eq!(
            complete_resource(&mut pending, "request-failed", None),
            None
        );
        assert!(pending.contains_key("request-failed"));
    }

    #[test]
    fn resolves_percent_encoded_and_base64_data_urls_to_their_exact_bytes() {
        let directory = temporary_artifacts("pliego-data-url");
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let capture = ResourceCapture::default();
        let expected = "hello café".as_bytes();
        let expected_resource = format!("sha256:{}", sha256_hex(expected));

        let percent_encoded = resolve_scene_resource(
            &artifacts,
            &capture,
            "data:text/plain;charset=utf-8,hello%20caf%C3%A9",
        )
        .unwrap();
        let base64 = resolve_scene_resource(
            &artifacts,
            &capture,
            "data:text/plain;charset=utf-8;base64,aGVsbG8gY2Fmw6k=",
        )
        .unwrap();
        assert_eq!(percent_encoded.as_deref(), Some(expected_resource.as_str()));
        assert_eq!(base64, percent_encoded);
        assert_eq!(
            fs::read(directory.join("resources").join(&expected_resource[7..])).unwrap(),
            expected
        );
        assert_eq!(
            resolve_scene_resource(&artifacts, &capture, "https://example.test/image.png").unwrap(),
            None
        );
        let invalid =
            resolve_scene_resource(&artifacts, &capture, "data:image/png,%not-hex").unwrap_err();
        assert_eq!(invalid.code, "SCENE_CAPTURE_DATA_URL_INVALID");

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn retries_an_exclusive_session_id_without_overwriting_the_collision() {
        let sandbox = temporary_artifacts("pliego-session-retry");
        fs::create_dir(&sandbox).unwrap();
        let base = sandbox.join("same-render-id");
        let first = SessionArtifacts::create(&base).unwrap();
        first.record_console("info", "preserve-me").unwrap();
        let original = fs::read(base.join("console.jsonl")).unwrap();

        let retried = create_session_artifacts(base.clone(), "sha256:stable-render-id").unwrap();
        assert_eq!(retried.directory(), sandbox.join("same-render-id-1"));
        assert_eq!(retried.render_id(), "sha256:stable-render-id");
        assert_eq!(fs::read(base.join("console.jsonl")).unwrap(), original);

        drop(retried);
        drop(first);
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn persists_multi_page_scene_previews_manifest_and_pdf() {
        let directory = temporary_artifacts("pliego-scene-multipage");
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let page = Page {
            size: Size {
                width: 64.0,
                height: 64.0,
            },
            operations: vec![],
        };
        let mut scene = DocumentScene::new(page.clone());
        scene.pages.push(page);
        let capture = SceneCapture {
            scene,
            canvas_resources: vec![],
            embedded_image_resources: vec![],
            canvas_diagnostics: vec![],
            font_resources: vec![],
            font_instances: vec![],
            font_selections: vec![],
            font_warnings: vec![],
            unsupported_events: vec![],
            text_mapping_gaps: vec![],
        };

        let summary = persist_scene_capture(&artifacts, &capture, false, false).unwrap();
        assert!(summary.scene_setup_ms.is_finite() && summary.scene_setup_ms >= 0.0);
        assert!(summary.preview_ms.is_finite() && summary.preview_ms >= 0.0);
        assert!(summary.pdf_ms.is_finite() && summary.pdf_ms >= 0.0);
        assert!(!directory.join("scene-preview.png").exists());
        assert_eq!(summary.preview_paths.len(), 2);
        assert_eq!(
            summary.preview_paths,
            [
                directory.join("pages/page-0001.png"),
                directory.join("pages/page-0002.png"),
            ]
        );
        for path in &summary.preview_paths {
            assert!(fs::read(path).unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
        }
        let pages: serde_json::Value =
            serde_json::from_slice(&fs::read(&summary.pages_path).unwrap()).unwrap();
        assert_eq!(pages["schema"], "pliego.pages");
        assert_eq!(pages["page_count"], 2);
        assert_eq!(pages["pages"][0]["artifact"], "pages/page-0001.png");
        assert_eq!(pages["pages"][1]["artifact"], "pages/page-0002.png");
        let structure: serde_json::Value =
            serde_json::from_slice(&fs::read(&summary.pdf_structure_path).unwrap()).unwrap();
        assert_eq!(structure["page_count"], 2);
        assert!(fs::read(&summary.pdf_path).unwrap().starts_with(b"%PDF-"));

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn persists_exact_scene_fonts_resources_report_and_preview() {
        let directory = temporary_artifacts("pliego-scene-persist");
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let resource = format!("sha256:{}", sha256_hex(DEJAVU_SANS));
        let font = format!("sha256:{}", sha256_hex(b"fixture-font-instance"));
        let capture = SceneCapture {
            scene: DocumentScene::new(Page {
                size: Size {
                    width: 64.0,
                    height: 64.0,
                },
                operations: vec![Operation::Text {
                    text: "notdef".into(),
                    font: font.clone(),
                    font_size: 32.0,
                    color: Color::default(),
                    glyphs: vec![Glyph {
                        id: 0,
                        x: 8.0,
                        y: 40.0,
                        advance: 20.0,
                        text_range: Some(Utf8Range { start: 0, end: 6 }),
                    }],
                    meta: OperationMeta::default(),
                }],
            }),
            canvas_resources: vec![],
            embedded_image_resources: vec![],
            canvas_diagnostics: vec![],
            font_resources: vec![CapturedFontResource {
                resource: resource.clone(),
                bytes_base64: BASE64_STANDARD.encode(DEJAVU_SANS),
            }],
            font_instances: vec![CapturedFontInstance {
                id: font.clone(),
                resource: resource.clone(),
                face_index: 0,
                variations: vec![],
                synthetic_bold: false,
            }],
            font_selections: vec![CapturedFontSelection {
                instance: font.clone(),
                resource: resource.clone(),
                face_index: 0,
                source: CapturedFontSource::Bundled,
                requested_families: vec!["Missing Preferred".into(), "DejaVu Sans".into()],
                selected_family: Some("DejaVu Sans".into()),
            }],
            font_warnings: vec![CapturedFontWarning {
                code: "FONT_FALLBACK_USED",
                instance: font,
                requested_family: "Missing Preferred".into(),
                selected_family: "DejaVu Sans".into(),
                fallback_chain: vec!["Missing Preferred".into(), "DejaVu Sans".into()],
            }],
            unsupported_events: vec![],
            text_mapping_gaps: vec![],
        };

        let summary = persist_scene_capture(&artifacts, &capture, false, false).unwrap();
        let exact_scene = capture.scene.normalized_json().unwrap();
        assert_eq!(summary.capture_status, "complete");
        assert_eq!(summary.capture_code, None);
        assert_eq!(summary.preview_status, "rendered");
        assert_eq!(summary.pdf_status, "rendered");
        assert_eq!(summary.pdf_structure_status, "rendered");
        assert_eq!(fs::read(&summary.scene_path).unwrap(), exact_scene);
        assert_eq!(
            summary.scene_hash,
            format!("sha256:{}", sha256_hex(&exact_scene))
        );
        assert_eq!(
            fs::read(directory.join("resources").join(&resource[7..])).unwrap(),
            DEJAVU_SANS
        );
        assert!(
            fs::read(&summary.preview_paths[0])
                .unwrap()
                .starts_with(b"\x89PNG\r\n\x1a\n")
        );
        assert!(fs::read(&summary.pdf_path).unwrap().starts_with(b"%PDF-"));
        let pdf = fs::read(&summary.pdf_path).unwrap();
        let pdf_structure: serde_json::Value =
            serde_json::from_slice(&fs::read(&summary.pdf_structure_path).unwrap()).unwrap();
        assert_eq!(pdf_structure["schema"], "pliego.pdf-structure");
        assert_eq!(pdf_structure["backend"], "krilla");
        assert_eq!(pdf_structure["page_count"], 1);
        assert_eq!(
            pdf_structure["pdf"]["sha256"],
            format!("sha256:{}", sha256_hex(&pdf))
        );
        assert_eq!(pdf_structure["pdf"]["bytes"], pdf.len());
        assert_eq!(
            pdf_structure["pages"][0]["scene_page_size_css_px"],
            serde_json::json!({ "width": 64.0, "height": 64.0 })
        );
        assert_eq!(
            pdf_structure["pages"][0]["media_box_pt"],
            serde_json::json!([0.0, 0.0, 48.0, 48.0])
        );
        assert_eq!(
            pdf_structure["pages"][0]["expected_extracted_unicode"],
            "notdef"
        );
        assert_eq!(
            pdf_structure["pages"][0]["embedded_font_ids"],
            serde_json::json!([&capture.font_instances[0].id])
        );
        assert_eq!(
            pdf_structure["pages"][0]["operation_counts"],
            serde_json::json!({ "text": 1, "vector": 0, "image": 0, "link": 0 })
        );
        let fonts: serde_json::Value =
            serde_json::from_slice(&fs::read(&summary.fonts_path).unwrap()).unwrap();
        assert_eq!(fonts["schema"], "pliego.font-report");
        assert_eq!(fonts["version"], 1);
        assert_eq!(fonts["policy"]["host_fonts"], "denied");
        assert_eq!(fonts["manifest"]["resolution"], "css-order");
        assert_eq!(fonts["manifest"]["entries"], fonts["selections"]);
        assert_eq!(fonts["font_resources"][0]["resource"], resource);
        assert_eq!(fonts["selections"][0]["source"], "bundled");
        assert_eq!(
            fonts["selections"][0]["requested_families"][0],
            "Missing Preferred"
        );
        assert_eq!(fonts["selections"][0]["selected_family"], "DejaVu Sans");
        assert_eq!(fonts["warnings"][0]["code"], "FONT_FALLBACK_USED");
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(&summary.report_path).unwrap()).unwrap();
        assert_eq!(report["scene"]["hash"], summary.scene_hash);
        assert_eq!(report["preview"]["status"], "rendered");
        assert_eq!(report["preview"]["unsupported"], serde_json::json!([]));
        assert_eq!(
            report["preview"]["page_size"],
            serde_json::json!({ "width": 64.0, "height": 64.0 })
        );
        assert_eq!(
            report["preview"]["operation_counts"],
            serde_json::json!({ "text": 1, "vector": 0, "image": 0, "link": 0 })
        );
        assert_eq!(report["document_pdf"]["status"], "rendered");
        assert_eq!(
            report["document_pdf"]["artifact"],
            serde_json::json!(summary.pdf_path.to_string_lossy())
        );
        assert_eq!(report["document_pdf"]["error"], serde_json::Value::Null);
        assert_eq!(report["pdf_structure"]["status"], "rendered");
        assert_eq!(
            report["pdf_structure"]["artifact"],
            serde_json::json!(summary.pdf_structure_path.to_string_lossy())
        );
        assert_eq!(report["pdf_structure"]["error"], serde_json::Value::Null);

        let mut environment = serde_json::json!({});
        set_document_pdf_environment(
            &mut environment,
            &summary.pdf_path,
            summary.pdf_status,
            None,
        );
        artifacts.write_environment(&environment).unwrap();
        let persisted_environment: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("environment.json")).unwrap()).unwrap();
        assert_eq!(
            persisted_environment["document_pdf"],
            serde_json::json!({
                "artifact": summary.pdf_path.to_string_lossy(),
                "status": "rendered",
                "error": null,
            })
        );

        let partial_directory = temporary_artifacts("pliego-scene-text-gap");
        let partial_artifacts = SessionArtifacts::create(&partial_directory).unwrap();
        let mut partial_capture = capture.clone();
        let Operation::Text { glyphs, .. } = &mut partial_capture.scene.pages[0].operations[0]
        else {
            unreachable!()
        };
        glyphs[0].text_range = None;
        partial_capture.text_mapping_gaps = vec![MissingTextMapping {
            sequence: 0,
            glyph_index: 0,
        }];
        let partial_summary =
            persist_scene_capture(&partial_artifacts, &partial_capture, false, false).unwrap();
        assert_eq!(partial_summary.capture_status, "partial");
        assert_eq!(
            partial_summary.capture_code,
            Some("SCENE_CAPTURE_LIMITATIONS")
        );
        assert_eq!(partial_summary.pdf_status, "failed");
        let partial_report: serde_json::Value =
            serde_json::from_slice(&fs::read(partial_directory.join("scene-report.json")).unwrap())
                .unwrap();
        assert_eq!(partial_report["capture"]["status"], "partial");
        assert_eq!(
            partial_report["capture"]["code"],
            "SCENE_CAPTURE_LIMITATIONS"
        );
        assert_eq!(
            partial_report["capture"]["text_mapping_gaps"][0],
            serde_json::json!({ "sequence": 0, "glyph_index": 0 })
        );
        assert_eq!(partial_report["document_pdf"]["status"], "failed");
        assert_eq!(
            partial_report["document_pdf"]["error"]["code"],
            "SCENE_CAPTURE_LIMITATIONS"
        );
        assert!(!partial_directory.join("document.pdf").exists());
        assert_eq!(partial_report["pdf_structure"]["status"], "failed");
        assert!(!partial_directory.join("pdf-structure.json").exists());

        drop(partial_artifacts);
        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(partial_directory).unwrap();
    }

    #[test]
    fn reports_unsupported_preview_operations_without_silently_rasterizing_them() {
        let directory = temporary_artifacts("pliego-scene-unsupported");
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let capture = SceneCapture {
            scene: DocumentScene::new(Page {
                size: Size {
                    width: 64.0,
                    height: 64.0,
                },
                operations: vec![Operation::Image {
                    bounds: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 10.0,
                        height: 10.0,
                    },
                    resource: format!("sha256:{}", "1".repeat(64)),
                    meta: OperationMeta::default(),
                }],
            }),
            canvas_resources: vec![],
            embedded_image_resources: vec![],
            canvas_diagnostics: vec![],
            font_resources: vec![],
            font_instances: vec![],
            font_selections: vec![],
            font_warnings: vec![],
            unsupported_events: vec![UnsupportedPaintEvent {
                sequence: 0,
                kind: UnsupportedPaintKind::Box,
            }],
            text_mapping_gaps: vec![],
        };

        let error = persist_scene_capture(&artifacts, &capture, false, true).unwrap_err();
        assert_eq!(error.code, "DOCUMENT_PDF_GENERATION_FAILED");
        assert!(
            error
                .message
                .contains("cannot read captured image resource")
        );
        assert!(!directory.join("scene-preview.png").exists());
        assert!(!directory.join("document.pdf").exists());
        assert!(!directory.join("pdf-structure.json").exists());
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("scene-report.json")).unwrap())
                .unwrap();
        assert_eq!(
            report["capture"]["code"],
            "SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS"
        );
        assert_eq!(report["capture"]["unsupported_events"][0]["kind"], "box");
        assert_eq!(
            report["preview"]["unsupported"][0]["code"],
            "SCENE_CAPTURE_PREVIEW_UNSUPPORTED_OPERATION"
        );
        assert_eq!(report["preview"]["unsupported"][0]["kind"], "image");
        assert_eq!(report["preview"]["status"], "unsupported");
        assert_eq!(
            report["preview"]["page_size"],
            serde_json::json!({ "width": 64.0, "height": 64.0 })
        );
        assert_eq!(
            report["preview"]["operation_counts"],
            serde_json::json!({ "text": 0, "vector": 0, "image": 1, "link": 0 })
        );
        assert_eq!(report["document_pdf"]["status"], "failed");
        assert_eq!(
            report["document_pdf"]["error"]["code"],
            "DOCUMENT_PDF_GENERATION_FAILED"
        );
        assert_eq!(report["pdf_structure"]["status"], "failed");

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn records_typed_pdf_failure_in_environment_artifact() {
        let directory = temporary_artifacts("pliego-pdf-environment-failure");
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let pdf_path = directory.join("document.pdf");
        let error = super::SceneArtifactError::new(
            "DOCUMENT_PDF_GENERATION_FAILED",
            "source-text mapping is missing",
        );
        let mut environment = serde_json::json!({});

        set_document_pdf_environment(&mut environment, &pdf_path, "failed", Some(&error));
        artifacts.write_environment(&environment).unwrap();

        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("environment.json")).unwrap()).unwrap();
        assert_eq!(
            persisted["document_pdf"],
            serde_json::json!({
                "artifact": pdf_path.to_string_lossy(),
                "status": "failed",
                "error": {
                    "code": "DOCUMENT_PDF_GENERATION_FAILED",
                    "message": "source-text mapping is missing",
                },
            })
        );

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_shell_failure_environment_keeps_the_resolved_input_hash() {
        let directory = temporary_artifacts("pliego-shell-failure-input-hash");
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let document_pdf = directory.join("document.pdf");
        let mut resources = BTreeMap::new();
        resources.insert(
            "file:///report.html".into(),
            format!("sha256:{}", "1".repeat(64)),
        );
        let expected = super::resolved_input_hash(&artifacts.render_id(), &resources);
        let mut environment = serde_json::json!({});
        environment["resolved_input_hash"] = serde_json::json!(expected);
        super::record_artifact(
            &artifacts,
            &document_pdf,
            artifacts.write_environment(&environment),
        )
        .unwrap();

        let error = super::fail_session(
            &artifacts,
            &document_pdf,
            "READINESS_PENDING",
            "document remained pending after stable capture",
        );

        assert_eq!(error.code, "READINESS_PENDING");
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("environment.json")).unwrap()).unwrap();
        assert_eq!(persisted["resolved_input_hash"], expected);

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_binds_main_frame_identity_and_uses_the_existing_transaction() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-success", b"direct input");
        let outcome = direct_capture_outcome(&fixture.document, b"direct input");

        let rendered = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap();

        assert_eq!(rendered.summary["status"], "rendered");
        assert_eq!(rendered.summary["render_id"], fixture.render_id);
        assert_eq!(
            rendered.summary["environment"]["runtime"]["adapter"],
            "document-session"
        );
        assert_eq!(
            rendered.summary["environment"]["input_resource"]["resource"],
            fixture.expected_input.content_address
        );
        assert!(
            fs::read(fixture.root.join("output.pdf"))
                .unwrap()
                .starts_with(b"%PDF-")
        );
        assert!(fixture.root.join("artifacts/render.png").is_file());
        assert!(fixture.root.join("artifacts/bundle.json").is_file());
        let published_pdf = fs::read(fixture.root.join("output.pdf")).unwrap();
        let bundle: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.root.join("artifacts/bundle.json")).unwrap())
                .unwrap();
        assert_eq!(
            bundle["output"]["sha256"],
            format!("sha256:{}", sha256_hex(&published_pdf))
        );
        assert_eq!(bundle["output"]["bytes"], published_pdf.len() as u64);
        assert!(fs::read_dir(&fixture.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));
        let resources = fs::read_to_string(fixture.root.join("artifacts/resources.jsonl")).unwrap();
        let terminal: serde_json::Value =
            serde_json::from_str(resources.lines().last().unwrap()).unwrap();
        assert_eq!(terminal["status"], "loaded");
        assert_eq!(terminal["is_for_main_frame"], true);
        assert_eq!(terminal["source"], "document_root");
        assert_eq!(terminal["resource"], fixture.expected_input.content_address);

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_does_not_publish_when_bundle_finalization_fails() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-bundle-failure", b"direct input");
        let outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let bundle_path = fixture.root.join("artifacts/bundle.json");
        fs::write(&bundle_path, b"caller bundle").unwrap();

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "BUNDLE_WRITE_FAILED");
        assert!(!fixture.root.join("output.pdf").exists());
        assert_eq!(fs::read(&bundle_path).unwrap(), b"caller bundle");
        let environment: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.root.join("artifacts/environment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(environment["document_pdf"]["status"], "failed");
        assert_eq!(
            environment["document_pdf"]["error"]["code"],
            "BUNDLE_WRITE_FAILED"
        );
        let states =
            fs::read_to_string(fixture.root.join("artifacts/session-state.jsonl")).unwrap();
        let terminal: serde_json::Value =
            serde_json::from_str(states.lines().last().unwrap()).unwrap();
        assert_eq!(terminal["state"], "failed");
        assert!(fs::read_dir(&fixture.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_does_not_publish_when_terminal_state_write_fails() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-state-failure", b"direct input");
        let outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let state_path = fixture.root.join("artifacts/session-state.jsonl");
        fs::remove_file(&state_path).unwrap();
        fs::create_dir(&state_path).unwrap();

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "SESSION_ARTIFACT_WRITE_FAILED");
        assert!(!fixture.root.join("output.pdf").exists());
        assert!(!fixture.root.join("artifacts/bundle.json").exists());
        let environment: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.root.join("artifacts/environment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(environment["document_pdf"]["status"], "failed");
        assert_eq!(
            environment["document_pdf"]["error"]["code"],
            "SESSION_ARTIFACT_WRITE_FAILED"
        );
        assert!(fs::read_dir(&fixture.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_fails_closed_when_main_frame_bytes_changed_after_pre_read() {
        let fixture = direct_publication_fixture("pliego-direct-publisher-input-race", b"pre-read");
        let outcome = direct_capture_outcome(&fixture.document, b"changed before Servo load");

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "INPUT_RESOURCE_IDENTITY_MISMATCH");
        assert_eq!(error.exit_code, 1);
        assert_eq!(error.render_id.as_deref(), Some(fixture.render_id.as_str()));
        assert!(!fixture.root.join("output.pdf").exists());
        assert!(!fixture.root.join("artifacts/scene.json").exists());
        assert!(fixture.root.join("artifacts/render.png").is_file());
        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.root.join("artifacts/failure.json")).unwrap())
                .unwrap();
        assert_eq!(failure["error"]["code"], "INPUT_RESOURCE_IDENTITY_MISMATCH");

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_rejects_ambiguous_main_frame_evidence() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-ambiguous-input", b"direct input");
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let duplicate = retain_loaded_test_resource(
            &mut outcome.resource_store,
            outcome.resources[0].request.clone(),
            ResourceSource::DocumentRoot,
            "text/html",
            b"direct input",
        );
        outcome.resources.push(duplicate);
        outcome.resource_accounting = ResourceAccounting::from_evidence(&outcome.resources);

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "INPUT_RESOURCE_EVIDENCE_AMBIGUOUS");
        assert_eq!(error.exit_code, 1);
        assert!(!fixture.root.join("output.pdf").exists());
        let resources = fs::read_to_string(fixture.root.join("artifacts/resources.jsonl")).unwrap();
        assert_eq!(
            resources
                .lines()
                .filter(|line| line.contains("\"status\":\"loaded\""))
                .count(),
            2
        );

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_rejects_resource_accounting_that_does_not_match_evidence() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-accounting", b"direct input");
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        outcome.resource_accounting.body_bytes += 1;

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "RESOURCE_EVIDENCE_INVALID");
        assert_eq!(error.exit_code, 1);
        assert!(!fixture.root.join("output.pdf").exists());
        assert!(fixture.root.join("artifacts/render.png").is_file());
        let resources = fs::read_to_string(fixture.root.join("artifacts/resources.jsonl")).unwrap();
        assert!(resources.contains("\"status\":\"loaded\""));

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_rejects_a_non_main_loaded_row_absent_from_the_owned_store() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-unowned-row", b"direct input");
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let mut unowned = outcome.resources[0].clone();
        unowned.request.url =
            url::Url::from_file_path(fixture.document.path().with_file_name("unowned.js")).unwrap();
        unowned.request.destination = "Script".into();
        unowned.request.is_for_main_frame = false;
        outcome.resources.push(unowned);
        outcome.resource_accounting = ResourceAccounting::from_evidence(&outcome.resources);

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "RESOURCE_EVIDENCE_INVALID");
        assert!(error.message.contains("owned request occurrences"));
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_rejects_a_duplicate_non_main_row_without_a_second_occurrence() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-duplicate-row", b"direct input");
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::from_file_path(fixture.document.path().with_file_name("script.js"))
                .unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: Some(fixture.expected_input.url.clone()),
            is_for_main_frame: false,
            is_redirect: false,
        };
        let evidence = retain_loaded_test_resource(
            &mut outcome.resource_store,
            request,
            ResourceSource::DocumentRoot,
            "text/javascript",
            b"console.log('subresource');",
        );
        outcome.resources.push(evidence.clone());
        outcome.resources.push(evidence);
        outcome.resource_accounting = ResourceAccounting::from_evidence(&outcome.resources);

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "RESOURCE_EVIDENCE_INVALID");
        assert!(error.message.contains("owned request occurrences"));
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_rejects_an_owned_loaded_occurrence_missing_from_rows() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-missing-row", b"direct input");
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::from_file_path(fixture.document.path().with_file_name("missing.js"))
                .unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: Some(fixture.expected_input.url.clone()),
            is_for_main_frame: false,
            is_redirect: false,
        };
        retain_loaded_test_resource(
            &mut outcome.resource_store,
            request,
            ResourceSource::DocumentRoot,
            "text/javascript",
            b"console.log('missing row');",
        );

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "RESOURCE_EVIDENCE_INVALID");
        assert!(error.message.contains("owned request occurrences"));
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_accepts_two_rows_backed_by_two_identical_occurrences() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-repeated-row", b"direct input");
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::from_file_path(fixture.document.path().with_file_name("repeated.js"))
                .unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: Some(fixture.expected_input.url.clone()),
            is_for_main_frame: false,
            is_redirect: false,
        };
        for _ in 0..2 {
            outcome.resources.push(retain_loaded_test_resource(
                &mut outcome.resource_store,
                request.clone(),
                ResourceSource::DocumentRoot,
                "text/javascript",
                b"console.log('repeated row');",
            ));
        }
        outcome.resource_accounting = ResourceAccounting::from_evidence(&outcome.resources);

        let rendered = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap();

        assert_eq!(rendered.summary["status"], "rendered");
        assert!(fixture.root.join("output.pdf").is_file());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_rejects_promoting_a_retained_subframe_to_main_frame() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-promoted-frame", b"direct input");
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        let mut store = OwnedResourceStore::new(0);
        let mut request = outcome.resources[0].request.clone();
        request.is_for_main_frame = false;
        let mut promoted = retain_loaded_test_resource(
            &mut store,
            request,
            ResourceSource::DocumentRoot,
            "text/html",
            b"direct input",
        );
        promoted.request.is_for_main_frame = true;
        outcome.resources = vec![promoted];
        outcome.resource_accounting = ResourceAccounting::from_evidence(&outcome.resources);
        outcome.resource_store = store;

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap_err();

        assert_eq!(error.code, "RESOURCE_EVIDENCE_INVALID");
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_resource_validator_rejects_contradictory_terminal_shapes() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://assets.example.test/report.js").unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: Some(url::Url::parse("https://assets.example.test/report.html").unwrap()),
            is_for_main_frame: false,
            is_redirect: false,
        };
        let body = b"console.log('owned');";
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/javascript"),
        );
        let mut store = OwnedResourceStore::new(0);
        store
            .retain_with_source(
                &request,
                ResourceSource::Http,
                ControlledResource {
                    status: 200,
                    content_type: Some("text/javascript".into()),
                    body: body.to_vec(),
                },
                &headers,
            )
            .unwrap();
        let loaded = ResourceEvidence::loaded(
            request.clone(),
            ResourceSource::Http,
            "text/javascript",
            body,
        );
        assert!(super::validate_document_session_resource(&loaded, &store).is_ok());

        for malformed in [
            {
                let mut value = loaded.clone();
                value.response_status = Some(201);
                value
            },
            {
                let mut value = loaded.clone();
                value.source = Some(ResourceSource::DocumentRoot);
                value
            },
            {
                let mut value = loaded.clone();
                value.source = None;
                value
            },
            {
                let mut value = loaded.clone();
                value.response_headers = None;
                value
            },
            {
                let mut value = loaded.clone();
                value.content_type = Some("text/css".into());
                value
            },
            {
                let mut value = loaded.clone();
                value.request.destination = "Style".into();
                value
            },
            {
                let mut value = loaded.clone();
                value.request.referrer_url =
                    Some(url::Url::parse("https://other.example.test/report.html").unwrap());
                value
            },
            {
                let mut value = loaded.clone();
                value.request.is_for_main_frame = true;
                value
            },
            {
                let mut value = loaded.clone();
                value.request.is_redirect = true;
                value
            },
            {
                let mut value = loaded.clone();
                value.request.url.set_fragment(Some("mutated"));
                value
            },
        ] {
            assert_eq!(
                super::validate_document_session_resource(&malformed, &store)
                    .unwrap_err()
                    .code,
                "RESOURCE_EVIDENCE_INVALID"
            );
        }

        let delegated = ResourceEvidence::delegated(request.clone(), ResourceSource::Http);
        assert_eq!(
            super::validate_document_session_resource(&delegated, &store)
                .unwrap_err()
                .code,
            "RESOURCE_EVIDENCE_INVALID"
        );

        let mut metadata_request = request;
        metadata_request.load_role = WebResourceLoadRole::DocumentMetadata;
        let failure = ResourcePolicyFailure::new(
            &metadata_request,
            "RESOURCE_DENIED",
            "denied",
            "optional metadata was blocked",
        )
        .nonfatal();
        let cancelled = ResourceEvidence::cancelled(metadata_request, failure);
        assert!(super::validate_document_session_resource(&cancelled, &store).is_ok());
        let mut contradictory = cancelled.clone();
        contradictory.source = Some(ResourceSource::Http);
        assert_eq!(
            super::validate_document_session_resource(&contradictory, &store)
                .unwrap_err()
                .code,
            "RESOURCE_EVIDENCE_INVALID"
        );
        let mut wrong_referrer = cancelled;
        wrong_referrer.request.referrer_url =
            Some(url::Url::parse("https://other.example.test/").unwrap());
        assert_eq!(
            super::validate_document_session_resource(&wrong_referrer, &store)
                .unwrap_err()
                .code,
            "RESOURCE_EVIDENCE_INVALID"
        );
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_main_input_requires_document_destination_and_no_referrer() {
        let fixture =
            direct_publication_fixture("pliego-direct-main-request-shape", b"direct input");
        let (resources, _) = direct_resource_evidence(&fixture.document, b"direct input");
        let mut wrong_destination = resources[0].request.clone();
        wrong_destination.destination = "Script".into();
        let mut wrong_referrer = resources[0].request.clone();
        wrong_referrer.referrer_url =
            Some(url::Url::parse("https://example.test/referrer").unwrap());

        for request in [wrong_destination, wrong_referrer] {
            let mut store = OwnedResourceStore::new(0);
            let evidence = retain_loaded_test_resource(
                &mut store,
                request,
                ResourceSource::DocumentRoot,
                "text/html",
                b"direct input",
            );
            let error =
                super::bind_document_session_input(&fixture.expected_input, &[evidence], &store)
                    .err()
                    .unwrap();
            assert_eq!(error.code, "INPUT_RESOURCE_IDENTITY_MISMATCH");
        }

        let root = fixture.root.clone();
        drop(fixture);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolved_input_hash_staging_is_insert_once_and_fail_closed() {
        let mut environment = serde_json::json!({"locale": "en-US"});
        assert!(super::stage_resolved_input_hash(&mut environment, "sha256:expected").unwrap());
        assert_eq!(environment["resolved_input_hash"], "sha256:expected");
        let staged = environment.clone();
        assert!(!super::stage_resolved_input_hash(&mut environment, "sha256:expected").unwrap());
        assert_eq!(environment, staged);

        for existing in [serde_json::json!("sha256:different"), serde_json::json!(42)] {
            let mut environment = serde_json::json!({"resolved_input_hash": existing});
            let before = environment.clone();
            let error =
                super::stage_resolved_input_hash(&mut environment, "sha256:expected").unwrap_err();
            assert_eq!(error.code, "SESSION_CAPTURE_IDENTITY_MISMATCH");
            assert_eq!(environment, before);
        }
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_preserves_partial_scene_opt_in_at_the_shared_publisher() {
        let mut fixture =
            direct_publication_fixture("pliego-direct-publisher-partial", b"direct input");
        fixture.request.allow_partial_scene = true;
        let mut outcome = direct_capture_outcome(&fixture.document, b"direct input");
        outcome.capture.unsupported_events = vec![UnsupportedPaintEvent {
            sequence: 0,
            kind: UnsupportedPaintKind::Box,
        }];

        let rendered = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Ok(outcome),
        )
        .unwrap();

        assert_eq!(rendered.summary["scene"]["capture_status"], "partial");
        assert_eq!(rendered.summary["scene"]["unsupported_event_count"], 1);
        assert!(fixture.root.join("output.pdf").is_file());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_persists_staged_session_error_before_typed_cli_conversion() {
        let fixture = direct_publication_fixture("pliego-direct-publisher-error", b"direct input");
        let (resources, resource_store) =
            direct_resource_evidence(&fixture.document, b"direct input");
        let mut session_error =
            SessionError::new("READINESS_TIMEOUT", "readiness deadline elapsed");
        session_error.capture_evidence.stable_image_png = Some(stable_png());
        session_error.capture_evidence.readiness = Some(serde_json::json!({
            "status": "pending",
            "font_status": "loading",
        }));
        session_error.capture_evidence.layout_debug = Some(serde_json::json!({
            "schema": "fixture-layout",
        }));
        session_error.capture_evidence.controlled_runtime_ms = Some(12.5);
        session_error.capture_evidence.scene_capture_ms = Some(1.25);
        session_error.resource_accounting = ResourceAccounting::from_evidence(&resources);
        session_error.resources = resources;
        session_error.resource_store = resource_store;
        session_error.console = vec![
            ("info".into(), "capture-first".into()),
            ("error".into(), "capture-second".into()),
        ];

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Err(session_error),
        )
        .unwrap_err();

        assert_eq!(error.code, "READINESS_TIMEOUT");
        assert_eq!(error.exit_code, 1);
        assert_eq!(
            error.artifacts.as_deref(),
            Some(fixture.root.join("artifacts").as_path())
        );
        let cli = cli_render_error(&error);
        let stdout: serde_json::Value =
            serde_json::from_str(cli.stdout.as_deref().unwrap()).unwrap();
        assert_eq!(stdout["error"]["code"], "READINESS_TIMEOUT");
        assert_eq!(stdout["render_id"], fixture.render_id);
        assert!(fixture.root.join("artifacts/render.png").is_file());
        let readiness: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.root.join("artifacts/readiness.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(readiness["status"], "pending");
        assert_eq!(readiness["render_id"], fixture.render_id);
        let layout: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.root.join("artifacts/layout-debug.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(layout["schema"], "fixture-layout");
        let environment: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.root.join("artifacts/environment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(environment["phase_timings_ms"]["controlled_runtime"], 12.5);
        assert_eq!(environment["phase_timings_ms"]["scene_capture"], 1.25);
        assert_eq!(environment["resource_accounting"]["loaded"], 1);
        assert_eq!(
            environment["input_resource"]["resource"],
            fixture.expected_input.content_address
        );
        let console = fs::read_to_string(fixture.root.join("artifacts/console.jsonl")).unwrap();
        let messages = console
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["message"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec![
                serde_json::json!("capture-first"),
                serde_json::json!("capture-second"),
            ]
        );
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_preserves_a_post_retain_evidence_limit_failure() {
        let fixture =
            direct_publication_fixture("pliego-direct-post-retain-failure", b"direct input");
        let (resources, mut resource_store) =
            direct_resource_evidence(&fixture.document, b"direct input");
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::from_file_path(fixture.document.path().with_file_name("limited.js"))
                .unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: Some(fixture.expected_input.url.clone()),
            is_for_main_frame: false,
            is_redirect: false,
        };
        retain_loaded_test_resource(
            &mut resource_store,
            request.clone(),
            ResourceSource::DocumentRoot,
            "text/javascript",
            b"console.log('retained before evidence limit');",
        );
        let reason = format!(
            "resource evidence exceeds the {}-byte metadata bound",
            super::MAX_RESOURCE_METADATA_BYTES
        );
        let failure = ResourcePolicyFailure::new(
            &request,
            "RESOURCE_METADATA_LIMIT_EXCEEDED",
            "denied",
            reason.clone(),
        );
        let mut session_error = SessionError::new(
            "RESOURCE_METADATA_LIMIT_EXCEEDED",
            format!("{reason}: {}", request.url),
        );
        session_error.resource_accounting =
            ResourceAccounting::from_evidence(&resources).with_failure();
        session_error.resource_failure = Some(failure);
        session_error.resources = resources;
        session_error.resource_store = resource_store;

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Err(session_error),
        )
        .unwrap_err();

        assert_eq!(error.code, "RESOURCE_METADATA_LIMIT_EXCEEDED");
        assert!(!fixture.root.join("output.pdf").exists());
        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.root.join("artifacts/failure.json")).unwrap())
                .unwrap();
        assert_eq!(failure["error"]["code"], "RESOURCE_METADATA_LIMIT_EXCEEDED");
        let environment: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.root.join("artifacts/environment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(environment["resource_accounting"]["loaded"], 1);
        assert_eq!(environment["resource_accounting"]["failed"], 1);
        assert_eq!(
            environment["input_resource"]["resource"],
            fixture.expected_input.content_address
        );

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_preserves_an_evidence_limit_failure_without_a_store_surplus() {
        let fixture =
            direct_publication_fixture("pliego-direct-evidence-only-failure", b"direct input");
        let (resources, resource_store) =
            direct_resource_evidence(&fixture.document, b"direct input");
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://denied.example.test/metadata.bin").unwrap(),
            destination: "Image".into(),
            load_role: WebResourceLoadRole::DocumentMetadata,
            referrer_url: Some(fixture.expected_input.url.clone()),
            is_for_main_frame: false,
            is_redirect: false,
        };
        let reason = format!(
            "resource evidence exceeds the {}-byte metadata bound",
            super::MAX_RESOURCE_METADATA_BYTES
        );
        let failure = ResourcePolicyFailure::new(
            &request,
            "RESOURCE_METADATA_LIMIT_EXCEEDED",
            "denied",
            reason.clone(),
        );
        let mut session_error = SessionError::new(
            "RESOURCE_METADATA_LIMIT_EXCEEDED",
            format!("{reason}: {}", request.url),
        );
        session_error.resource_accounting =
            ResourceAccounting::from_evidence(&resources).with_failure();
        session_error.resource_failure = Some(failure);
        session_error.resources = resources;
        session_error.resource_store = resource_store;

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Err(session_error),
        )
        .unwrap_err();

        assert_eq!(error.code, "RESOURCE_METADATA_LIMIT_EXCEEDED");
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_session_error_prefers_changed_main_frame_identity_over_the_later_error() {
        let fixture =
            direct_publication_fixture("pliego-direct-error-input-race", b"pre-read input");
        let (resources, resource_store) =
            direct_resource_evidence(&fixture.document, b"changed before Servo load");
        let mut session_error =
            SessionError::new("READINESS_TIMEOUT", "readiness deadline elapsed");
        session_error.capture_evidence.stable_image_png = Some(stable_png());
        session_error.resource_accounting = ResourceAccounting::from_evidence(&resources);
        session_error.resources = resources;
        session_error.resource_store = resource_store;

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Err(session_error),
        )
        .unwrap_err();

        assert_eq!(error.code, "INPUT_RESOURCE_IDENTITY_MISMATCH");
        assert!(!fixture.root.join("output.pdf").exists());
        assert!(fixture.root.join("artifacts/render.png").is_file());
        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.root.join("artifacts/failure.json")).unwrap())
                .unwrap();
        assert_eq!(failure["error"]["code"], "INPUT_RESOURCE_IDENTITY_MISMATCH");

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_session_error_prefers_ambiguous_main_frame_identity_over_the_later_error() {
        let fixture =
            direct_publication_fixture("pliego-direct-error-ambiguous-input", b"direct input");
        let (mut resources, mut resource_store) =
            direct_resource_evidence(&fixture.document, b"direct input");
        let duplicate = retain_loaded_test_resource(
            &mut resource_store,
            resources[0].request.clone(),
            ResourceSource::DocumentRoot,
            "text/html",
            b"direct input",
        );
        resources.push(duplicate);
        let mut session_error =
            SessionError::new("READINESS_TIMEOUT", "readiness deadline elapsed");
        session_error.resource_accounting = ResourceAccounting::from_evidence(&resources);
        session_error.resources = resources;
        session_error.resource_store = resource_store;

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Err(session_error),
        )
        .unwrap_err();

        assert_eq!(error.code, "INPUT_RESOURCE_EVIDENCE_AMBIGUOUS");
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_preload_error_without_main_frame_evidence_keeps_its_original_code() {
        let fixture = direct_publication_fixture("pliego-direct-preload-error", b"direct input");
        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Err(SessionError::new(
                "RENDER_CONTEXT_FAILED",
                "software context unavailable",
            )),
        )
        .unwrap_err();

        assert_eq!(error.code, "RENDER_CONTEXT_FAILED");
        assert!(!fixture.root.join("output.pdf").exists());

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn direct_publisher_keeps_artifact_write_failure_precedence_after_staging() {
        let fixture =
            direct_publication_fixture("pliego-direct-publisher-write-failure", b"direct input");
        let (resources, resource_store) =
            direct_resource_evidence(&fixture.document, b"direct input");
        let digest = resources[0].sha256.as_deref().unwrap();
        fs::create_dir(fixture.root.join("artifacts/resources").join(digest)).unwrap();
        let mut session_error =
            SessionError::new("READINESS_TIMEOUT", "readiness deadline elapsed");
        session_error.capture_evidence.stable_image_png = Some(stable_png());
        session_error.capture_evidence.readiness = Some(serde_json::json!({
            "status": "pending",
            "font_status": "loading",
        }));
        session_error.capture_evidence.layout_debug = Some(serde_json::json!({
            "schema": "fixture-layout",
        }));
        session_error.resource_accounting = ResourceAccounting::from_evidence(&resources);
        session_error.resources = resources;
        session_error.resource_store = resource_store;

        let error = finish_document_session_render(
            &fixture.request,
            &fixture.document,
            &fixture.render_id,
            &fixture.expected_input,
            fixture.publication,
            Err(session_error),
        )
        .unwrap_err();

        assert_eq!(error.code, "SESSION_ARTIFACT_WRITE_FAILED");
        assert_eq!(error.exit_code, 1);
        assert!(error.message.contains("direct-session resource body"));
        assert!(!fixture.root.join("output.pdf").exists());
        let readiness: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.root.join("artifacts/readiness.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(readiness["status"], "pending");
        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.root.join("artifacts/failure.json")).unwrap())
                .unwrap();
        assert_eq!(failure["error"]["code"], "SESSION_ARTIFACT_WRITE_FAILED");

        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(feature = "document-session")]
    struct DirectPublicationFixture {
        root: PathBuf,
        request: RenderRequest,
        document: LocalDocument,
        render_id: String,
        expected_input: ExpectedInputIdentity,
        publication: PublicationTransaction,
    }

    #[cfg(feature = "document-session")]
    fn direct_publication_fixture(prefix: &str, input_bytes: &[u8]) -> DirectPublicationFixture {
        let root = temporary_artifacts(prefix);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("input.html"), input_bytes).unwrap();
        let document = LocalDocument::resolve(&root, "input.html").unwrap();
        let request = RenderRequest {
            input: PathBuf::from("input.html"),
            environment: RenderEnvironment::default(),
            page: default_page(),
            resources: ResourcePolicyConfig::default(),
            allow_host_fonts: false,
            allow_partial_scene: false,
            explicit_paths: Some(ExplicitRenderPaths {
                output: root.join("output.pdf"),
                artifacts: root.join("artifacts"),
            }),
        };
        let resource_policy = ResourcePolicy::resolve(&request.resources, document.root());
        let render_id = stable_render_id(
            input_bytes,
            request.environment,
            request.page,
            &resource_policy,
            request.allow_host_fonts,
        );
        let sha256 = sha256_hex(input_bytes);
        let expected_input = ExpectedInputIdentity {
            url: url::Url::from_file_path(document.path()).unwrap(),
            content_address: format!("sha256:{sha256}"),
            sha256,
            bytes: input_bytes.len() as u64,
        };
        let publication = begin_publication(&request, &resource_policy, &render_id).unwrap();
        DirectPublicationFixture {
            root,
            request,
            document,
            render_id,
            expected_input,
            publication,
        }
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn shorthand_publication_keeps_the_owned_pdf_inside_its_artifact_bundle() {
        let root = temporary_artifacts("pliego-shorthand-publication");
        fs::create_dir(&root).unwrap();
        let input = b"<!doctype html><title>Shorthand</title>";
        fs::write(root.join("input.html"), input).unwrap();
        let document = LocalDocument::resolve(&root, "input.html").unwrap();
        let request = RenderRequest {
            input: PathBuf::from("input.html"),
            environment: RenderEnvironment::default(),
            page: default_page(),
            resources: ResourcePolicyConfig::default(),
            allow_host_fonts: false,
            allow_partial_scene: false,
            explicit_paths: None,
        };
        let resource_policy = ResourcePolicy::resolve(&request.resources, document.root());
        let render_id = stable_render_id(
            input,
            request.environment,
            request.page,
            &resource_policy,
            false,
        );
        let publication = begin_publication(&request, &resource_policy, &render_id).unwrap();
        let artifact_root = publication.artifacts.directory().to_owned();
        fs::write(&publication.proof, stable_png()).unwrap();

        let outcome = publish_captured_document(
            &request,
            &document,
            &render_id,
            publication,
            CapturedPublication {
                scene_capture: empty_scene_capture(),
                readiness_payload: serde_json::json!({ "status": "ready" }),
                resolved_input_hash: format!("sha256:{}", sha256_hex(input)),
                controlled_runtime_ms: 1.0,
                scene_capture_ms: 1.0,
                preserve_staged_readiness: false,
            },
        )
        .unwrap();

        assert_eq!(
            outcome.summary["document_pdf"],
            serde_json::json!(artifact_root.join("document.pdf").to_string_lossy())
        );
        assert!(artifact_root.join("document.pdf").is_file());
        assert!(artifact_root.join("bundle.json").is_file());
        assert!(fs::read_dir(&artifact_root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));

        fs::remove_dir_all(artifact_root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "document-session")]
    #[test]
    fn relative_output_spelling_matches_the_bundle_and_summary() {
        let root = temporary_artifacts("pliego-relative-publication");
        fs::create_dir(&root).unwrap();
        let input = b"<!doctype html><title>Relative</title>";
        fs::write(root.join("input.html"), input).unwrap();
        let document = LocalDocument::resolve(&root, "input.html").unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = PathBuf::from(format!(
            ".pliego-relative-output-{}-{unique}.pdf",
            std::process::id()
        ));
        let absolute_output = std::env::current_dir().unwrap().join(&output);
        let _output_cleanup = RemoveFileOnDrop(absolute_output.clone());
        let request = RenderRequest {
            input: PathBuf::from("input.html"),
            environment: RenderEnvironment::default(),
            page: default_page(),
            resources: ResourcePolicyConfig::default(),
            allow_host_fonts: false,
            allow_partial_scene: false,
            explicit_paths: Some(ExplicitRenderPaths {
                output: output.clone(),
                artifacts: root.join("artifacts"),
            }),
        };
        let resource_policy = ResourcePolicy::resolve(&request.resources, document.root());
        let render_id = stable_render_id(
            input,
            request.environment,
            request.page,
            &resource_policy,
            false,
        );
        let publication = begin_publication(&request, &resource_policy, &render_id).unwrap();
        let artifact_root = publication.artifacts.directory().to_owned();
        fs::write(&publication.proof, stable_png()).unwrap();

        let outcome = publish_captured_document(
            &request,
            &document,
            &render_id,
            publication,
            CapturedPublication {
                scene_capture: empty_scene_capture(),
                readiness_payload: serde_json::json!({ "status": "ready" }),
                resolved_input_hash: format!("sha256:{}", sha256_hex(input)),
                controlled_runtime_ms: 1.0,
                scene_capture_ms: 1.0,
                preserve_staged_readiness: false,
            },
        )
        .unwrap();
        let bundle: serde_json::Value =
            serde_json::from_slice(&fs::read(artifact_root.join("bundle.json")).unwrap()).unwrap();

        assert_eq!(
            outcome.summary["document_pdf"],
            serde_json::json!(output.to_string_lossy())
        );
        assert_eq!(
            bundle["output"]["path"],
            serde_json::json!(output.to_string_lossy())
        );
        assert!(absolute_output.is_file());

        fs::remove_file(absolute_output).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "document-session")]
    fn direct_capture_outcome(document: &LocalDocument, body: &[u8]) -> DocumentCaptureOutcome {
        let (resources, resource_store) = direct_resource_evidence(document, body);
        DocumentCaptureOutcome {
            capture: empty_scene_capture(),
            stable_image_png: stable_png(),
            layout_debug: serde_json::json!({ "schema": "fixture-layout" }),
            environment: RenderEnvironment::default(),
            allow_host_fonts: false,
            readiness: serde_json::json!({
                "status": "ready",
                "font_status": "loaded",
                "payload": null,
            }),
            console: vec![("info".into(), "direct capture".into())],
            resource_accounting: ResourceAccounting::from_evidence(&resources),
            resources,
            resource_store,
            controlled_runtime_ms: 12.5,
            scene_capture_ms: 1.25,
        }
    }

    #[cfg(feature = "document-session")]
    fn direct_resource_evidence(
        document: &LocalDocument,
        body: &[u8],
    ) -> (Vec<ResourceEvidence>, OwnedResourceStore) {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::from_file_path(document.path()).unwrap(),
            destination: "Document".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: None,
            is_for_main_frame: true,
            is_redirect: false,
        };
        let mut store = OwnedResourceStore::new(0);
        let evidence = retain_loaded_test_resource(
            &mut store,
            request,
            ResourceSource::DocumentRoot,
            "text/html",
            body,
        );
        (vec![evidence], store)
    }

    #[cfg(feature = "document-session")]
    fn retain_loaded_test_resource(
        store: &mut OwnedResourceStore,
        request: ResourceRequest,
        source: ResourceSource,
        content_type: &str,
        body: &[u8],
    ) -> ResourceEvidence {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_str(content_type).unwrap(),
        );
        store
            .retain_with_source(
                &request,
                source,
                ControlledResource {
                    status: 200,
                    content_type: Some(content_type.into()),
                    body: body.to_vec(),
                },
                &headers,
            )
            .unwrap();
        ResourceEvidence::loaded(request, source, content_type, body)
    }

    #[cfg(feature = "document-session")]
    fn empty_scene_capture() -> SceneCapture {
        SceneCapture {
            scene: DocumentScene::new(Page {
                size: Size {
                    width: 64.0,
                    height: 64.0,
                },
                operations: vec![],
            }),
            canvas_resources: vec![],
            embedded_image_resources: vec![],
            canvas_diagnostics: vec![],
            font_resources: vec![],
            font_instances: vec![],
            font_selections: vec![],
            font_warnings: vec![],
            unsupported_events: vec![],
            text_mapping_gaps: vec![],
        }
    }

    #[cfg(feature = "document-session")]
    fn stable_png() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\nfixture".to_vec()
    }

    #[test]
    fn artifact_write_failures_preserve_session_context_and_failure_evidence() {
        let directory = temporary_artifacts("pliego-artifact-write-failure");
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let document_pdf = directory.join("document.pdf");
        let render_id = artifacts.render_id();

        let error = super::record_artifact(
            &artifacts,
            &document_pdf,
            Err(std::io::Error::other("disk full")),
        )
        .unwrap_err();

        assert_eq!(error.code, "SESSION_ARTIFACT_WRITE_FAILED");
        assert_eq!(error.message, "cannot write session artifact: disk full");
        assert_eq!(error.exit_code, 1);
        assert_eq!(error.artifacts, Some(directory.clone()));
        assert_eq!(error.document_pdf, Some(document_pdf));
        assert_eq!(error.render_id, Some(render_id.clone()));
        assert!(error.warnings.is_empty());

        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("failure.json")).unwrap()).unwrap();
        assert_eq!(failure["status"], "failed");
        assert_eq!(failure["render_id"], render_id);
        assert_eq!(failure["error"]["code"], "SESSION_ARTIFACT_WRITE_FAILED");

        let readiness: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("readiness.json")).unwrap()).unwrap();
        assert_eq!(readiness["status"], "failed");
        assert_eq!(readiness["error"]["code"], "SESSION_ARTIFACT_WRITE_FAILED");

        let states = fs::read_to_string(directory.join("session-state.jsonl")).unwrap();
        let state: serde_json::Value =
            serde_json::from_str(states.lines().last().unwrap()).unwrap();
        assert_eq!(state["state"], "failed");
        assert_eq!(state["message"], "cannot write session artifact: disk full");

        drop(artifacts);
        fs::remove_dir_all(directory).unwrap();
    }

    fn temporary_artifacts(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("{prefix}-{}-{unique}", std::process::id()))
    }
}
