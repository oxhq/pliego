/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::collections::{BTreeMap, HashMap};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::ffi::CString;
use std::ffi::OsString;
use std::path::PathBuf;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use base64::Engine as _;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use layout::pages::{PageDefinition, PageMargins};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::Operation;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::capture::{SceneCapture, capture_document_scene};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::pdf::{CSS_PX_TO_PDF_PT, PdfFontResource, PdfFontVariation, render_document_pdf};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::raster::{RasterFontResource, RasterFontVariation, render_first_page_png_with_images};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use readiness::{Readiness, parse_snapshot};
use session::{LocalDocument, SessionArtifacts};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use sha2::{Digest, Sha256};

mod readiness;
mod session;

const SERVO_BASE_SHA: &str = "313b6d5ecc113b08010ce434140db3ca5abcc71c";
const READINESS_TIMEOUT_MS: u64 = 10_000;
const SESSION_CREATE_ATTEMPTS: u32 = 32;
const DEFAULT_LOCALE: &str = "en-US";
const DEFAULT_TIMEZONE: &str = "UTC";
const DEFAULT_PAGE_WIDTH_CSS_PX: f32 = 793.7008;
const DEFAULT_PAGE_HEIGHT_CSS_PX: f32 = 1122.5197;
const DEFAULT_PAGE_MARGIN_VERTICAL_CSS_PX: f32 = 45.3543;
const DEFAULT_PAGE_MARGIN_HORIZONTAL_CSS_PX: f32 = 60.4724;
const RENDER_ID_SCHEMA_MARKER: &[u8] = b"pliego.render-id.v1";

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
struct ExplicitRenderPaths {
    output: PathBuf,
    artifacts: PathBuf,
}

#[derive(Debug, PartialEq)]
struct RenderRequest {
    input: PathBuf,
    environment: RenderEnvironment,
    page: PageDefinition,
    explicit_paths: Option<ExplicitRenderPaths>,
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
        explicit_paths,
    }))
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
        "Pliego — native document rendering on Servo\n\nUsage:\n  pliego render <document.html> --output <document.pdf> --artifacts <directory> [--locale en-US] [--timezone UTC|PST8PDT] [--page-size WIDTHxHEIGHT] [--page-margins TOP,RIGHT,BOTTOM,LEFT]\n  pliego [--locale en-US] [--timezone UTC|PST8PDT] [--page-size WIDTHxHEIGHT] [--page-margins TOP,RIGHT,BOTTOM,LEFT] <document.html>\n  pliego --version\n\nThe shorthand form writes all outputs to a temporary artifact directory. Page geometry is expressed in CSS pixels. The default is A4 with 12mm vertical and 16mm horizontal margins."
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
    layout::pages::configure_for_process(request.page).unwrap_or_else(|_| {
        eprintln!("pliego: paged layout was already configured for this process");
        std::process::exit(2)
    });
    let document = LocalDocument::resolve(".", &request.input).unwrap_or_else(|error| {
        eprintln!("pliego: {error}");
        std::process::exit(2)
    });
    let input_bytes = std::fs::read(document.path()).unwrap_or_else(|error| {
        eprintln!(
            "pliego: cannot read input document {}: {error}",
            document.path().display()
        );
        std::process::exit(2)
    });
    let render_id = stable_render_id(&input_bytes, request.environment, request.page);
    let input_url = url::Url::from_file_path(document.path()).unwrap_or_else(|_| {
        eprintln!("pliego: cannot convert document path to a file URL");
        std::process::exit(2)
    });
    let artifacts = if let Some(paths) = &request.explicit_paths {
        SessionArtifacts::create_with_render_id(&paths.artifacts, &render_id).unwrap_or_else(
            |error| {
                let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "ARTIFACTS_ALREADY_EXISTS"
                } else {
                    "ARTIFACTS_CREATE_FAILED"
                };
                fail_render_request(
                    &paths.artifacts,
                    &paths.output,
                    &render_id,
                    code,
                    &format!(
                        "cannot create exclusive artifact directory {}: {error}",
                        paths.artifacts.display()
                    ),
                )
            },
        )
    } else {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let session_path =
            std::env::temp_dir().join(format!("pliego-session-{}-{unique}", std::process::id()));
        create_session_artifacts(session_path.clone(), &render_id).unwrap_or_else(|error| {
            fail_render_request(
                &session_path,
                &session_path.join("document.pdf"),
                &render_id,
                "ARTIFACTS_CREATE_FAILED",
                &format!("cannot create session artifacts: {error}"),
            )
        })
    };
    let proof = artifacts.directory().join("render.png");
    let document_pdf_path = request
        .explicit_paths
        .as_ref()
        .map(|paths| paths.output.clone())
        .unwrap_or_else(|| artifacts.directory().join("document.pdf"));
    let environment_path = artifacts.directory().join("environment.json");
    let mut environment = request.environment.artifact();
    environment["page"] = page_artifact(request.page);
    set_document_pdf_environment(&mut environment, &document_pdf_path, "pending", None);
    record_artifact(artifacts.write_environment(&environment));
    if request.explicit_paths.is_some() {
        match document_pdf_path.try_exists() {
            Ok(false) => {},
            Ok(true) => fail_session(
                &artifacts,
                &document_pdf_path,
                "OUTPUT_ALREADY_EXISTS",
                &format!(
                    "requested output already exists: {}",
                    document_pdf_path.display()
                ),
            ),
            Err(error) => fail_session(
                &artifacts,
                &document_pdf_path,
                "OUTPUT_PATH_CHECK_FAILED",
                &format!(
                    "cannot check requested output {}: {error}",
                    document_pdf_path.display()
                ),
            ),
        }
    }
    apply_timezone(request.environment.timezone).unwrap_or_else(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "ENVIRONMENT_CONFIGURATION_FAILED",
            &error,
        )
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
        "--window-size".into(),
        format!(
            "{}x{}",
            request.page.width().ceil() as u32,
            request.page.height().ceil() as u32
        ),
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
            &document_pdf_path,
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
    let resource_capture = record_resources(&artifacts, result.resources).unwrap_or_else(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "SCENE_CAPTURE_RESOURCE_MAP_CONFLICT",
            &error.to_string(),
        )
    });

    let snapshot_json = match result.value {
        servoshell::JSValue::String(json) => json,
        value => fail_session(
            &artifacts,
            &document_pdf_path,
            "READINESS_INVALID_RESULT",
            &format!("expected readiness JSON string, got {value:?}"),
        ),
    };
    let readiness = parse_snapshot(&snapshot_json).unwrap_or_else(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "READINESS_INVALID_RESULT",
            &error,
        )
    });
    let readiness_json: serde_json::Value =
        serde_json::from_str(&snapshot_json).unwrap_or_else(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "READINESS_INVALID_RESULT",
                &error.to_string(),
            )
        });
    record_artifact(artifacts.write_readiness(&readiness_json));
    let readiness_payload = match readiness {
        Readiness::Ready { payload } => {
            if let Some(url) = &resource_capture.missing_local_resource {
                fail_session(
                    &artifacts,
                    &document_pdf_path,
                    "RESOURCE_LOAD_FAILED",
                    &format!("local resource did not load: {url}"),
                );
            }
            payload
        },
        Readiness::Failed { error } => {
            fail_session(&artifacts, &document_pdf_path, &error.code, &error.message)
        },
        Readiness::Pending => fail_session(
            &artifacts,
            &document_pdf_path,
            "READINESS_PENDING",
            "document remained pending after stable capture",
        ),
    };
    let layout_debug_json = result.layout_debug.unwrap_or_else(|| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "SCENE_CAPTURE_UNAVAILABLE",
            "Servo did not return cached layout data",
        )
    });
    let layout_debug: serde_json::Value =
        serde_json::from_str(&layout_debug_json).unwrap_or_else(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "SCENE_CAPTURE_LAYOUT_JSON_INVALID",
                &error.to_string(),
            )
        });
    artifacts
        .write_layout_debug(&layout_debug)
        .unwrap_or_else(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "SCENE_CAPTURE_LAYOUT_WRITE_FAILED",
                &error.to_string(),
            )
        });
    let layout_debug_path = artifacts.directory().join("layout-debug.json");
    let mut resource_resolution_error = None;
    let scene_capture = capture_document_scene(layout_debug_json.as_bytes(), |url| {
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
    });
    if let Some(error) = resource_resolution_error {
        fail_session(&artifacts, &document_pdf_path, error.code, &error.message);
    }
    let scene_capture = scene_capture.unwrap_or_else(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "SCENE_CAPTURE_CONVERSION_FAILED",
            &error.to_string(),
        )
    });
    let scene_artifacts = match persist_scene_capture(&artifacts, &scene_capture) {
        Ok(summary) => summary,
        Err(error) => {
            if error.code.starts_with("DOCUMENT_PDF_") {
                set_document_pdf_environment(
                    &mut environment,
                    &document_pdf_path,
                    "failed",
                    Some(&error),
                );
                if let Err(write_error) = artifacts.write_environment(&environment) {
                    eprintln!(
                        "pliego: warning: cannot record failed PDF environment state: {write_error}"
                    );
                }
            }
            fail_session(&artifacts, &document_pdf_path, error.code, &error.message)
        },
    };
    let rendered_bytes = std::fs::metadata(&proof)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if rendered_bytes == 0 {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "RENDER_OUTPUT_MISSING",
            "Servo did not produce a rendered image",
        );
    }
    if request.explicit_paths.is_some() {
        if let Err(error) = artifacts.publish_document_pdf(&document_pdf_path) {
            let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
                "OUTPUT_ALREADY_EXISTS"
            } else {
                "OUTPUT_PUBLISH_FAILED"
            };
            let failure = SceneArtifactError::new(
                code,
                format!(
                    "cannot publish requested output {}: {error}",
                    document_pdf_path.display()
                ),
            );
            set_document_pdf_environment(
                &mut environment,
                &document_pdf_path,
                "failed",
                Some(&failure),
            );
            if let Err(write_error) = artifacts.write_environment(&environment) {
                eprintln!(
                    "pliego: warning: cannot record failed PDF publication state: {write_error}"
                );
            }
            fail_session(
                &artifacts,
                &document_pdf_path,
                failure.code,
                &failure.message,
            )
        }
    }
    set_document_pdf_environment(
        &mut environment,
        &document_pdf_path,
        scene_artifacts.pdf_status,
        None,
    );
    artifacts
        .write_environment(&environment)
        .unwrap_or_else(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "DOCUMENT_PDF_ENVIRONMENT_WRITE_FAILED",
                &error.to_string(),
            )
        });
    let scene_preview = scene_artifacts
        .preview_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    record_artifact(artifacts.record_state("rendered", None));
    let bundle_path = artifacts
        .write_bundle(&document_pdf_path)
        .unwrap_or_else(|error| {
            fail_session(
                &artifacts,
                &document_pdf_path,
                "BUNDLE_WRITE_FAILED",
                &error.to_string(),
            )
        });

    println!(
        "{}",
        serde_json::json!({
            "artifacts": artifacts.directory().to_string_lossy(),
            "bundle": bundle_path.to_string_lossy(),
            "engine": "pliego",
            "document_root": document.root().to_string_lossy(),
            "environment": environment,
            "environment_artifact": environment_path.to_string_lossy(),
            "input": request.input.to_string_lossy(),
            "resolved_input": document.path().to_string_lossy(),
            "layout_debug": layout_debug_path.to_string_lossy(),
            "readiness": readiness_payload,
            "render_id": render_id,
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
            "document_pdf": document_pdf_path.to_string_lossy(),
            "document_pdf_status": scene_artifacts.pdf_status,
            "pdf_structure": scene_artifacts.pdf_structure_path.to_string_lossy(),
            "pdf_structure_status": scene_artifacts.pdf_structure_status,
            "servo_base_sha": SERVO_BASE_SHA,
            "servo_build": servoshell::VERSION,
            "rendered_bytes": rendered_bytes,
            "status": "rendered"
        })
    );
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
fn stable_render_id(
    input_bytes: &[u8],
    environment: RenderEnvironment,
    page: PageDefinition,
) -> String {
    fn update_field(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    let margins = page.margins();
    let mut hasher = Sha256::new();
    update_field(&mut hasher, RENDER_ID_SCHEMA_MARKER);
    update_field(&mut hasher, input_bytes);
    update_field(&mut hasher, environment.locale.as_bytes());
    update_field(&mut hasher, environment.timezone.as_bytes());
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
    format!("sha256:{}", lowercase_hex(&hasher.finalize()))
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
#[derive(Debug, Default, PartialEq)]
struct ResourceCapture {
    url_to_resource: BTreeMap<String, String>,
    missing_local_resource: Option<String>,
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
) -> Result<ResourceCapture, ResourceMapConflict> {
    let mut pending: HashMap<String, PendingResource> = HashMap::new();
    let mut capture = ResourceCapture::default();

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
                capture.retain_completed(&completed)?;
            },
            servoshell::NetworkEvent::SecurityInfo(_) => {},
        }
    }

    capture.missing_local_resource = pending
        .into_values()
        .flat_map(|resource| resource.urls)
        .filter(|url| url.starts_with("file:"))
        .min();
    Ok(capture)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn complete_resource(
    pending: &mut HashMap<String, PendingResource>,
    request_id: &str,
    body: Option<Vec<u8>>,
) -> Option<CompletedResource> {
    if body.is_none()
        && !pending
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
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            use std::fmt::Write as _;
            write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        },
    )
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
    preview_path: Option<PathBuf>,
    pdf_path: PathBuf,
    pdf_status: &'static str,
    pdf_structure_path: PathBuf,
    pdf_structure_status: &'static str,
    capture_status: &'static str,
    capture_code: Option<&'static str>,
    preview_status: &'static str,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Debug, PartialEq, serde::Serialize)]
struct PreviewUnsupported {
    code: &'static str,
    operation_index: usize,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    font: Option<String>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn persist_scene_capture(
    artifacts: &SessionArtifacts,
    capture: &SceneCapture,
) -> Result<SceneArtifactSummary, SceneArtifactError> {
    let page_count = capture.scene.pages.len();
    if page_count != 1 {
        return Err(SceneArtifactError::new(
            "SCENE_OUTPUT_MULTIPAGE_UNSUPPORTED",
            format!(
                "canonical scene contains {page_count} pages; preview and PDF currently support exactly one page"
            ),
        ));
    }
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

    let mut image_resources = BTreeMap::<String, Vec<u8>>::new();
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
    for (operation_index, operation) in capture.scene.pages[0].operations.iter().enumerate() {
        match operation {
            Operation::Text { font, .. } => {
                let instance = instances_by_id
                    .get(font)
                    .expect("capture validated scene font references");
                if !instance.variations.is_empty() {
                    unsupported.push(PreviewUnsupported {
                        code: "SCENE_CAPTURE_PREVIEW_UNSUPPORTED_FONT_VARIATIONS",
                        operation_index,
                        kind: "text",
                        font: Some(font.clone()),
                    });
                }
            },
            Operation::Image { resource, .. } if image_resource_errors.contains_key(resource) => {
                unsupported.push(PreviewUnsupported {
                    code: "SCENE_CAPTURE_PREVIEW_UNSUPPORTED_OPERATION",
                    operation_index,
                    kind: "image",
                    font: None,
                });
            },
            Operation::Image { .. } => {},
            Operation::Path { .. } | Operation::Link { .. } => {},
        }
    }

    artifacts.write_scene(&scene_bytes).map_err(|error| {
        SceneArtifactError::new("SCENE_CAPTURE_SCENE_WRITE_FAILED", error.to_string())
    })?;
    let fonts = serde_json::json!({
        "font_resources": capture.font_resources,
        "font_instances": capture.font_instances,
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

    let preview_path = if unsupported.is_empty() {
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
        let png = render_first_page_png_with_images(
            &capture.scene,
            |font| {
                let instance = instances_by_id.get(font)?;
                let bytes = decoded_resources.get(&instance.resource)?;
                let variations = variations_by_instance.get(font)?;
                Some(RasterFontResource {
                    bytes,
                    face_index: instance.face_index,
                    variations,
                })
            },
            |image| image_resources.get(image).map(Vec::as_slice),
        )
        .map_err(|error| {
            SceneArtifactError::new("SCENE_CAPTURE_PREVIEW_FAILED", error.to_string())
        })?;
        artifacts.write_scene_preview(&png).map_err(|error| {
            SceneArtifactError::new("SCENE_CAPTURE_PREVIEW_WRITE_FAILED", error.to_string())
        })?;
        Some(artifacts.directory().join("scene-preview.png"))
    } else {
        None
    };

    let pdf_path = artifacts.directory().join("document.pdf");
    let pdf_structure_path = artifacts.directory().join("pdf-structure.json");
    let mut pdf_written = false;
    let mut pdf_structure_written = false;
    let pdf_result = (|| -> Result<(), SceneArtifactError> {
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
    })();
    let pdf_error = pdf_result.err();
    let pdf_status = if pdf_written { "rendered" } else { "failed" };
    let pdf_structure_status = if pdf_structure_written {
        "rendered"
    } else {
        "failed"
    };

    let capture_status =
        if capture.unsupported_events.is_empty() && capture.text_mapping_gaps.is_empty() {
            "complete"
        } else {
            "partial"
        };
    let preview_status = if preview_path.is_some() {
        "rendered"
    } else {
        "unsupported"
    };
    let capture_code = if !capture.text_mapping_gaps.is_empty() {
        Some("SCENE_CAPTURE_LIMITATIONS")
    } else if !capture.unsupported_events.is_empty() {
        Some("SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS")
    } else {
        None
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
        },
        "preview": {
            "status": preview_status,
            "artifact": preview_path.as_ref().map(|_| "scene-preview.png"),
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
    if let Some(error) = pdf_error {
        return Err(error);
    }

    Ok(SceneArtifactSummary {
        scene_hash,
        scene_path: artifacts.directory().join("scene.json"),
        fonts_path: artifacts.directory().join("fonts.json"),
        report_path: artifacts.directory().join("scene-report.json"),
        preview_path,
        pdf_path,
        pdf_status,
        pdf_structure_path,
        pdf_structure_status,
        capture_status,
        capture_code,
        preview_status,
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
fn record_artifact(result: std::io::Result<()>) {
    if let Err(error) = result {
        eprintln!("pliego: cannot write session artifact: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn fail_render_request(
    artifacts: &std::path::Path,
    document_pdf: &std::path::Path,
    render_id: &str,
    code: &str,
    message: &str,
) -> ! {
    println!(
        "{}",
        serde_json::json!({
            "artifacts": artifacts.to_string_lossy(),
            "document_pdf": document_pdf.to_string_lossy(),
            "engine": "pliego",
            "error": {
                "code": code,
                "message": message,
            },
            "render_id": render_id,
            "status": "failed",
        })
    );
    eprintln!("pliego: {code}: {message}");
    std::process::exit(1)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn fail_session(
    artifacts: &SessionArtifacts,
    document_pdf: &std::path::Path,
    code: &str,
    message: &str,
) -> ! {
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
            "document_pdf": document_pdf.to_string_lossy(),
            "engine": "pliego",
            "error": failure["error"],
            "render_id": artifacts.render_id(),
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use pliego::capture::{
        CapturedFontInstance, CapturedFontResource, MissingTextMapping, SceneCapture,
        UnsupportedPaintEvent, UnsupportedPaintKind,
    };
    use pliego::{DocumentScene, Glyph, Operation, OperationMeta, Page, Rect, Size, Utf8Range};

    use super::{
        Command, DEFAULT_LOCALE, DEFAULT_TIMEZONE, ExplicitRenderPaths, PageDefinition,
        PageMargins, PendingResource, RenderEnvironment, RenderRequest, ResourceCapture,
        complete_resource, create_session_artifacts, default_page, page_artifact, parse_args,
        persist_scene_capture, resolve_scene_resource, set_document_pdf_environment, sha256_hex,
        stable_render_id,
    };
    use crate::session::SessionArtifacts;
    use std::ffi::OsString;
    use std::path::PathBuf;

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
        let render_id = stable_render_id(input, first.environment, first.page);
        assert_eq!(
            render_id,
            stable_render_id(input, second.environment, second.page)
        );
        assert!(render_id.starts_with("sha256:"));
        assert_eq!(render_id.len(), 71);
        assert_ne!(
            render_id,
            stable_render_id(
                b"<!doctype html><title>Changed</title>",
                first.environment,
                first.page,
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
            )
        );
        assert_ne!(
            render_id,
            stable_render_id(
                input,
                first.environment,
                PageDefinition::new(612.0, 792.0, PageMargins::new(72.0, 54.0, 36.0, 18.0),)
                    .unwrap(),
            )
        );
        let canonical_page =
            PageDefinition::new(612.0, 792.0, PageMargins::new(72.0, 54.0, 36.0, 18.0)).unwrap();
        assert_eq!(
            stable_render_id(input, RenderEnvironment::default(), canonical_page),
            "sha256:9ca553323bf5eb8118c51a46c00b174e1ef1febc7e6bbdb3d763ac2dcf5291a3"
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
                page: default_page(),
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
    fn completes_a_resource_and_hashes_exact_bytes() {
        let mut resource = PendingResource {
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

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn rejects_multi_page_scene_output_before_page_zero_rendering() {
        let directory = temporary_artifacts("pliego-scene-multipage-unsupported");
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
            font_resources: vec![],
            font_instances: vec![],
            unsupported_events: vec![],
            text_mapping_gaps: vec![],
        };

        let error = persist_scene_capture(&artifacts, &capture).unwrap_err();
        assert_eq!(error.code, "SCENE_OUTPUT_MULTIPAGE_UNSUPPORTED");
        assert!(error.message.contains("contains 2 pages"));
        assert!(!directory.join("scene-preview.png").exists());
        assert!(!directory.join("document.pdf").exists());
        assert!(!directory.join("pdf-structure.json").exists());

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
            font_resources: vec![CapturedFontResource {
                resource: resource.clone(),
                bytes_base64: BASE64_STANDARD.encode(DEJAVU_SANS),
            }],
            font_instances: vec![CapturedFontInstance {
                id: font,
                resource: resource.clone(),
                face_index: 0,
                variations: vec![],
            }],
            unsupported_events: vec![],
            text_mapping_gaps: vec![],
        };

        let summary = persist_scene_capture(&artifacts, &capture).unwrap();
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
            fs::read(summary.preview_path.as_ref().unwrap())
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
        assert_eq!(fonts["font_resources"][0]["resource"], resource);
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
        let partial_error =
            persist_scene_capture(&partial_artifacts, &partial_capture).unwrap_err();
        assert_eq!(partial_error.code, "DOCUMENT_PDF_GENERATION_FAILED");
        assert!(partial_error.message.contains("source-text mapping"));
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
            "DOCUMENT_PDF_GENERATION_FAILED"
        );
        assert!(!partial_directory.join("document.pdf").exists());
        assert_eq!(partial_report["pdf_structure"]["status"], "failed");
        assert!(!partial_directory.join("pdf-structure.json").exists());

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
            font_resources: vec![],
            font_instances: vec![],
            unsupported_events: vec![UnsupportedPaintEvent {
                sequence: 0,
                kind: UnsupportedPaintKind::Box,
            }],
            text_mapping_gaps: vec![],
        };

        let error = persist_scene_capture(&artifacts, &capture).unwrap_err();
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

        fs::remove_dir_all(directory).unwrap();
    }

    fn temporary_artifacts(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()))
    }
}
