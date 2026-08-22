/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(feature = "document-session", feature = "shell-oracle")))]
compile_error!(
    "the pliego binary requires either the default document-session runtime or the explicit shell-oracle feature"
);

// Rust keeps type-checking after `compile_error!`. These fallback page types keep the no-runtime
// CI contract focused on the deliberate feature error instead of emitting secondary type errors.
#[cfg(not(any(feature = "document-session", feature = "shell-oracle")))]
mod no_runtime_page_types {
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(super) struct PageMargins {
        pub(super) top: f32,
        pub(super) right: f32,
        pub(super) bottom: f32,
        pub(super) left: f32,
    }

    impl PageMargins {
        pub(super) const fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
            Self {
                top,
                right,
                bottom,
                left,
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(super) struct PageDefinition {
        width: f32,
        height: f32,
        margins: PageMargins,
    }

    impl PageDefinition {
        pub(super) fn new(
            width: f32,
            height: f32,
            margins: PageMargins,
        ) -> Result<Self, &'static str> {
            if !width.is_finite() || width <= 0.0 || !height.is_finite() || height <= 0.0 {
                return Err("invalid page size");
            }
            if [margins.top, margins.right, margins.bottom, margins.left]
                .into_iter()
                .any(|margin| !margin.is_finite() || margin < 0.0) ||
                margins.left + margins.right >= width ||
                margins.top + margins.bottom >= height
            {
                return Err("invalid page margins");
            }
            Ok(Self {
                width,
                height,
                margins,
            })
        }

        pub(super) const fn width(&self) -> f32 {
            self.width
        }

        pub(super) const fn height(&self) -> f32 {
            self.height
        }

        pub(super) const fn margins(&self) -> PageMargins {
            self.margins
        }
    }
}

// The direct binary is now Servo's top-level Windows embedder. Own the GPU
// selection symbols instead of inheriting them from a shell. Surfman's macro
// stores its export directives in a named `.drectve` static, which `lld-link`
// can consume while leaving the static's retention reference unresolved.
// `build.rs` exports these data symbols without that intermediary owner.
#[cfg(all(
    target_os = "windows",
    target_env = "msvc",
    feature = "document-session"
))]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut NvOptimusEnablement: i32 = 1;

#[cfg(all(
    target_os = "windows",
    target_env = "msvc",
    feature = "document-session"
))]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut AmdPowerXpressRequestHighPerformance: i32 = 1;

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
use std::cell::{Cell, OnceCell, RefCell};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::collections::BTreeMap;
#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
use std::collections::HashMap;
use std::ffi::OsString;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
use std::rc::Rc;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use base64::Engine as _;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use embedder_traits::WebResourceLoadRole;
#[cfg(any(feature = "document-session", feature = "shell-oracle"))]
use layout::pages::{PageDefinition, PageMargins};
#[cfg(not(any(feature = "document-session", feature = "shell-oracle")))]
use no_runtime_page_types::{PageDefinition, PageMargins};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::Operation;
#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
use pliego::capture::capture_document_scene_with_canvas;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::capture::{CapturedFontSource, SceneCapture, UnsupportedPaintKind};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::pdf::{CSS_PX_TO_PDF_PT, PdfFontResource, PdfFontVariation, render_document_pdf};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use pliego::raster::{RasterFontResource, RasterFontVariation, render_pages_png_with_images};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use readiness::{Readiness, ReadinessPolicy, parse_snapshot};
use session::{LocalDocument, SessionArtifacts};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use session::{
    MAX_PROMOTION_TREE_ENTRIES, PreparedDocumentPdf, PreparedPublicationError, PublicationJournal,
    PublicationRecoveryState, validate_publication_outcome_bytes,
};
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use sha2::{Digest, Sha256};

mod api2;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
mod asset_cache;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
mod controlled_settlement;
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
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
mod render_supervisor;
mod resource_policy;
mod runtime_policy;
mod session;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
mod supervised_artifact_contract;

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
use document_session::{
    ControlledDocumentSession, DocumentCaptureOutcome, DocumentSession,
    PreparedDocumentCaptureCandidate, SessionError,
};
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
#[cfg(all(
    any(feature = "shell-oracle", test),
    not(any(target_os = "android", target_env = "ohos"))
))]
use resource_policy::ResourceRequest;
#[cfg(all(test, not(any(target_os = "android", target_env = "ohos"))))]
use resource_policy::classify_controlled_http_status;
#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
use resource_policy::{
    ControlledResource, ResourcePolicyDecision, create_controlled_http_client,
    fetch_controlled_http, http_root_allows,
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
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use resource_policy::{
    RESOURCE_POLICY_ID, ResourcePolicy, ResourcePolicyFailure, ResourcePolicySetupFailure,
};
use runtime_policy::DeterministicRuntimePolicy;

const SERVO_BASE_SHA: &str = "313b6d5ecc113b08010ce434140db3ca5abcc71c";
const PLIEGO_API_VERSION: u32 = 2;
const SERVO_BUILD_VERSION: &str = concat!(
    "Servo ",
    env!("PLIEGO_SERVO_VERSION"),
    "-",
    env!("PLIEGO_GIT_SHA")
);
const SESSION_CREATE_ATTEMPTS: u32 = 32;
const DEFAULT_PAGE_WIDTH_CSS_PX: f32 = 793.7008;
const DEFAULT_PAGE_HEIGHT_CSS_PX: f32 = 1122.5197;
const DEFAULT_PAGE_MARGIN_VERTICAL_CSS_PX: f32 = 45.3543;
const DEFAULT_PAGE_MARGIN_HORIZONTAL_CSS_PX: f32 = 60.4724;
const RENDER_ID_SCHEMA_MARKER: &[u8] = b"pliego.render-id.v2";
// Runtime identity is part of recovery identity. This version deliberately invalidates prepared
// transactions from the pre-cutover fingerprint instead of resuming them under another runtime.
const PUBLICATION_REQUEST_SCHEMA_MARKER: &[u8] = b"pliego.publication-request.v4";
const CONTROLLED_CAPTURE_RENDER_ID_SCHEMA_MARKER: &[u8] = b"pliego.render-id.controlled-capture.v2";

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationRuntimeIdentity {
    #[cfg(feature = "document-session")]
    DocumentSession,
    #[cfg(feature = "document-session")]
    DocumentSessionControlledCapture,
    #[cfg(feature = "shell-oracle")]
    ServoshellOracle,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl PublicationRuntimeIdentity {
    fn fingerprint_field(self) -> &'static [u8] {
        match self {
            #[cfg(feature = "document-session")]
            Self::DocumentSession => b"document-session",
            #[cfg(feature = "document-session")]
            Self::DocumentSessionControlledCapture => b"document-session-controlled-capture-v1",
            #[cfg(feature = "shell-oracle")]
            Self::ServoshellOracle => b"servoshell-oracle",
        }
    }

    #[cfg(feature = "shell-oracle")]
    fn uses_shell_oracle(self) -> bool {
        self == Self::ServoshellOracle
    }
}
const RESOLVED_INPUT_HASH_SCHEMA_MARKER: &[u8] = b"pliego.resolved-input.v1";

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
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

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn decide_resource_policy(
    policy: &ResourcePolicy,
    document_root: &Path,
    request: &servoshell::WebResourceRequest,
) -> ResourcePolicyDecision {
    policy.decide(document_root, &resource_request(request))
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn first_fatal_policy_failure(
    failures: &[ResourcePolicyFailure],
) -> Option<&ResourcePolicyFailure> {
    failures
        .iter()
        .find(|failure| !failure.is_optional_metadata_failure())
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
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
    ContractProbe,
    RenderApi2,
    Api2InvocationError(String),
    Render(RenderRequest),
    RenderControlled(RenderRequest),
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
    if matches!(args.as_slice(), [flag] if flag == "--contract-probe") {
        return Ok(Command::ContractProbe);
    }
    if matches!(args.as_slice(), [command] if command == "render-api2") {
        return Ok(Command::RenderApi2);
    }
    if args
        .first()
        .is_some_and(|argument| argument == "render-api2")
    {
        return Ok(Command::Api2InvocationError(
            "`pliego render-api2` accepts no command-line options or paths".into(),
        ));
    }
    if args
        .first()
        .is_some_and(|argument| argument == "--contract-probe")
    {
        return Ok(Command::Api2InvocationError(
            "`pliego --contract-probe` accepts no additional arguments".into(),
        ));
    }

    let controlled_render = args
        .first()
        .is_some_and(|argument| argument == "render-controlled");
    let explicit_render =
        controlled_render || args.first().is_some_and(|argument| argument == "render");
    let explicit_command = if controlled_render {
        "pliego render-controlled"
    } else {
        "pliego render"
    };
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
            output: output.ok_or_else(|| format!("`{explicit_command}` requires --output"))?,
            artifacts: artifacts
                .ok_or_else(|| format!("`{explicit_command}` requires --artifacts"))?,
        })
    } else {
        None
    };
    let input = input.ok_or_else(|| "a document path is required".to_owned())?;
    if controlled_render && allow_partial_scene {
        return Err("`pliego render-controlled` does not permit --allow-partial-scene".into());
    }
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
    let request = RenderRequest {
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
        runtime_policy: DeterministicRuntimePolicy::default(),
        allow_host_fonts,
        allow_partial_scene,
        explicit_paths,
    };
    Ok(if controlled_render {
        Command::RenderControlled(request)
    } else {
        Command::Render(request)
    })
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

#[cfg(target_os = "linux")]
fn configure_linux_software_rendering() {
    // SAFETY: `main` calls this before Servo initialization or any application thread is
    // created. Pinning Mesa here, rather than only on the supervised worker command, keeps
    // the public API 2 stderr channel clean when a headless host would otherwise probe
    // unavailable DRI/Zink devices during parent-process initialization.
    unsafe {
        std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    configure_linux_software_rendering();

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    render_supervisor::run_worker_from_environment();

    let command = parse_args(std::env::args_os().skip(1).collect())
        .unwrap_or_else(|error| invalid_request(&error));

    match command {
        Command::Help => {
            print_help();
            std::process::ExitCode::SUCCESS
        },
        Command::Version => {
            print_version();
            std::process::ExitCode::SUCCESS
        },
        Command::ContractProbe => {
            let mut stdout = std::io::stdout().lock();
            if let Err(error) = api2::write_contract_probe(&mut stdout, SERVO_BASE_SHA) {
                api2_invocation_error(&error);
            }
            std::process::ExitCode::SUCCESS
        },
        Command::RenderApi2 => {
            #[cfg(all(
                feature = "document-session",
                not(any(target_os = "android", target_env = "ohos"))
            ))]
            {
                let mut stdin = std::io::stdin().lock();
                let outcome = api2::execute_render(&mut stdin, SERVO_BASE_SHA)
                    .unwrap_or_else(|error| api2_invocation_error(&error));
                match outcome {
                    api2::Api2CommandOutcome::Result { stdout, success } => {
                        let mut output = std::io::stdout().lock();
                        match emit_api2_result(&mut output, &stdout, success) {
                            Ok(exit) => exit,
                            Err(error) => api2_transport_error(&error),
                        }
                    },
                    api2::Api2CommandOutcome::TransportFailure { diagnostic } => {
                        api2_transport_error(&diagnostic);
                    },
                }
            }
            #[cfg(not(all(
                feature = "document-session",
                not(any(target_os = "android", target_env = "ohos"))
            )))]
            {
                api2_invocation_error(&api2::InvocationError::unsupported());
            }
        },
        Command::Api2InvocationError(message) => {
            api2_invocation_error(&api2::InvocationError::new(message));
        },
        Command::Render(request) => match render_command(request) {
            Ok(outcome) => {
                let mut stdout = std::io::stdout().lock();
                write_render_outcome(&mut stdout, &outcome)
                    .expect("failed to write the render outcome to stdout");
                std::process::ExitCode::SUCCESS
            },
            Err(error) => print_render_error(&error),
        },
        Command::RenderControlled(request) => match render_controlled_command(request) {
            Ok(outcome) => {
                let mut stdout = std::io::stdout().lock();
                write_render_outcome(&mut stdout, &outcome)
                    .expect("failed to write the render outcome to stdout");
                std::process::ExitCode::SUCCESS
            },
            Err(error) => print_render_error(&error),
        },
    }
}

fn render_command(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    #[cfg(feature = "document-session")]
    {
        DocumentEngine::render_controlled(request)
    }
    #[cfg(all(not(feature = "document-session"), feature = "shell-oracle"))]
    {
        DocumentEngine::render_with_shell_oracle(request)
    }
    #[cfg(not(any(feature = "document-session", feature = "shell-oracle")))]
    {
        let _ = request;
        unreachable!("the crate-level runtime selection error prevents this binary from running")
    }
}

fn render_controlled_command(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    #[cfg(feature = "document-session")]
    {
        DocumentEngine::render_controlled(request)
    }
    #[cfg(not(feature = "document-session"))]
    {
        let _ = request;
        Err(RenderError::without_publication(
            "CONTROLLED_CAPTURE_UNAVAILABLE",
            "controlled capture requires the document-session runtime",
            1,
        ))
    }
}

fn active_runtime_name() -> &'static str {
    #[cfg(feature = "document-session")]
    {
        "document-session"
    }
    #[cfg(all(not(feature = "document-session"), feature = "shell-oracle"))]
    {
        "servoshell-oracle"
    }
    #[cfg(not(any(feature = "document-session", feature = "shell-oracle")))]
    {
        unreachable!("the crate-level runtime selection error prevents this binary from running")
    }
}

fn write_render_outcome(
    writer: &mut impl std::io::Write,
    outcome: &RenderOutcome,
) -> std::io::Result<()> {
    writer.write_all(&outcome.cli_bytes)
}

fn print_help() {
    #[cfg(all(not(feature = "document-session"), feature = "shell-oracle"))]
    println!(
        "NONPRODUCTION ORACLE BUILD — servoshell is enabled only for explicit parity diagnostics."
    );
    println!(
        "Pliego — native document rendering on Servo\nRuntime: {}\n\nUsage:\n  pliego render <document.html> --output <document.pdf> --artifacts <directory> [options]\n  pliego render-controlled <document.html> --output <document.pdf> --artifacts <directory> [options]\n  pliego [options] <document.html>\n  pliego --version\n  pliego --contract-probe\n  pliego render-api2\n\nOptions:\n  --locale en-US|es-MX\n  --timezone UTC|PST8PDT\n  --page-size WIDTHxHEIGHT\n  --page-margins TOP,RIGHT,BOTTOM,LEFT\n  --allow-host-fonts          Opt in to observable system-font resolution\n  --allow-partial-scene       Retain diagnostic output (render only; rejected by render-controlled)\n  --allow-http-root URL       Allow GET/HEAD below one explicit http(s) URL root\n  --virtual-resource URL=FILE Serve one exact URL from a host-provided file\n  --asset-manifest FILE       Verify and cache manifest-backed assets locally\n  --resource-timeout-ms MS    Bound controlled network connection time (1..60000)\n\nAPI 2 accepts one canonical JSON request on stdin, resolves its input manifest from the cwd-v1 job root, and writes one terminal result on stdout. The advertised 0.3 tuple is profile-null only; semantic and accessible-PDF profiles remain unadvertised.\n\nThe API 1 render routes remain available as compatibility entry points. The default render route uses the fail-closed controlled transaction. render-controlled remains an explicit alias with narrower syntax: it rejects --allow-partial-scene. Host fonts, partial scenes, network, redirects, and asset caching are disabled by default. Page geometry is expressed in CSS pixels.",
        active_runtime_name()
    );
}

fn print_version() {
    println!(
        "pliego {}\npliego-api {}\n{}\nServo base {}",
        env!("CARGO_PKG_VERSION"),
        PLIEGO_API_VERSION,
        SERVO_BUILD_VERSION,
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

fn api2_invocation_error(error: &api2::InvocationError) -> ! {
    let mut stderr = std::io::stderr().lock();
    let _ = error.write_stderr_line(&mut stderr);
    std::process::exit(api2::INVOCATION_ERROR_EXIT_CODE)
}

fn emit_api2_result(
    writer: &mut impl std::io::Write,
    stdout: &[u8],
    success: bool,
) -> Result<std::process::ExitCode, String> {
    writer
        .write_all(stdout)
        .map_err(|error| format!("cannot write the accepted API 2 result frame: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush the accepted API 2 result frame: {error}"))?;
    Ok(if success {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(1)
    })
}

fn api2_transport_error(message: &str) -> ! {
    let message = message.replace(['\r', '\n'], " ");
    let message = message.trim();
    let message = if message.is_empty() {
        "accepted API 2 request failed at the result transport boundary"
    } else {
        message
    };
    eprintln!("pliego: API2_TRANSPORT_ERROR: {message}");
    std::process::exit(api2::TRANSPORT_ERROR_EXIT_CODE)
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

fn cli_render_stderr(error: &RenderError, terminal: &str) -> String {
    let mut stderr = error
        .warnings
        .iter()
        .map(|warning| format!("pliego: warning: {warning}"))
        .collect::<Vec<_>>();
    stderr.push(terminal.to_owned());
    stderr.join("\n")
}

fn print_render_error(error: &RenderError) -> std::process::ExitCode {
    let output = cli_render_error(error);
    if let Some(stdout) = output.stdout {
        println!("{stdout}");
    }
    eprintln!("{}", cli_render_stderr(error, &output.stderr));
    std::process::ExitCode::from(error.exit_code)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
struct PublicationTransaction {
    artifacts: SessionArtifacts,
    journal: Option<PublicationJournal>,
    proof: PathBuf,
    #[cfg(feature = "shell-oracle")]
    userscripts: Option<PathBuf>,
    document_pdf_path: PathBuf,
    environment: serde_json::Value,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) struct WorkerPublicationPaths {
    pub(crate) staging_container: PathBuf,
    pub(crate) staging_artifacts: PathBuf,
    pub(crate) public_artifacts: PathBuf,
    pub(crate) public_output: PathBuf,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
enum PublicationStart {
    New(PublicationTransaction),
    Recovered(RenderOutcome),
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn publication_recovery_required_message(state: &PublicationRecoveryState) -> String {
    let state = match state {
        PublicationRecoveryState::Planned => "planned",
        PublicationRecoveryState::Committed { .. } => "committed",
    };
    format!("publication transaction requires recovery before rendering: {state}")
}

#[cfg(all(
    test,
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn begin_publication(
    request: &RenderRequest,
    resource_policy: &ResourcePolicy,
    render_id: &str,
    resolved_input: &Path,
) -> Result<PublicationStart, RenderError> {
    begin_publication_for_runtime(
        request,
        resource_policy,
        render_id,
        resolved_input,
        PublicationRuntimeIdentity::DocumentSession,
    )
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn begin_shell_oracle_publication(
    request: &RenderRequest,
    resource_policy: &ResourcePolicy,
    render_id: &str,
    resolved_input: &Path,
) -> Result<PublicationStart, RenderError> {
    begin_publication_for_runtime(
        request,
        resource_policy,
        render_id,
        resolved_input,
        PublicationRuntimeIdentity::ServoshellOracle,
    )
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn begin_publication_for_runtime(
    request: &RenderRequest,
    resource_policy: &ResourcePolicy,
    render_id: &str,
    resolved_input: &Path,
    runtime: PublicationRuntimeIdentity,
) -> Result<PublicationStart, RenderError> {
    #[cfg(feature = "document-session")]
    let worker_paths = render_supervisor::worker_publication_paths();
    #[cfg(not(feature = "document-session"))]
    let worker_paths: Option<&WorkerPublicationPaths> = None;
    let request_fingerprint = publication_request_fingerprint_with_runtime_policy(
        runtime,
        render_id,
        request.allow_partial_scene,
        &request.input,
        resolved_input,
        resource_policy.summary_asset_manifest_path(),
        request.runtime_policy,
    );
    let (artifacts, resuming) = if let Some(paths) = worker_paths {
        (
            SessionArtifacts::create_staged_with_render_id(
                &paths.staging_artifacts,
                &paths.public_artifacts,
                render_id,
            )
            .map_err(|error| {
                RenderError::session(
                    &paths.public_artifacts,
                    &paths.public_output,
                    render_id,
                    "ARTIFACTS_CREATE_FAILED",
                    format!("cannot create private worker artifact directory: {error}"),
                )
            })?,
            false,
        )
    } else if let Some(paths) = &request.explicit_paths {
        match SessionArtifacts::create_with_render_id(&paths.artifacts, render_id) {
            Ok(artifacts) => (artifacts, false),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
                SessionArtifacts::open_for_publication_recovery(&paths.artifacts, render_id)
                    .map_err(|recovery_error| {
                        RenderError::session(
                            &paths.artifacts,
                            &paths.output,
                            render_id,
                            "PUBLICATION_RECOVERY_FAILED",
                            format!(
                                "existing artifact root cannot be opened for publication recovery: {recovery_error}"
                            ),
                        )
                    })?,
                true,
            ),
            Err(error) => {
                return Err(RenderError::session(
                    &paths.artifacts,
                    &paths.output,
                    render_id,
                    "ARTIFACTS_CREATE_FAILED",
                    format!(
                        "cannot create exclusive artifact directory {}: {error}",
                        paths.artifacts.display()
                    ),
                ));
            },
        }
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
        (
            create_session_artifacts(session_path.clone(), render_id).map_err(|error| {
                RenderError::session(
                    &session_path,
                    &session_path.join("document.pdf"),
                    render_id,
                    "ARTIFACTS_CREATE_FAILED",
                    format!("cannot create session artifacts: {error}"),
                )
            })?,
            false,
        )
    };
    let public_artifacts = worker_paths
        .map(|paths| paths.public_artifacts.as_path())
        .unwrap_or_else(|| artifacts.directory());
    let public_output = worker_paths
        .map(|paths| paths.public_output.as_path())
        .or_else(|| {
            request
                .explicit_paths
                .as_ref()
                .map(|paths| paths.output.as_path())
        });
    if request.explicit_paths.is_some() {
        let output = public_output.expect("explicit render must have a public output path");
        let overlap = if worker_paths.is_some() {
            output_overlaps_uncreated_artifacts(
                output,
                public_artifacts,
                &worker_paths
                    .expect("worker publication paths were checked")
                    .staging_container,
            )
        } else {
            output_overlaps_artifacts(output, public_artifacts)
        };
        match overlap {
            Ok(false) => {},
            Ok(true) => {
                return Err(fail_session(
                    &artifacts,
                    output,
                    "OUTPUT_ARTIFACTS_OVERLAP",
                    "requested output must be outside the artifact directory",
                ));
            },
            Err(error) => {
                return Err(fail_session(
                    &artifacts,
                    output,
                    "OUTPUT_PATH_CHECK_FAILED",
                    &format!("cannot compare output and artifact paths: {error}"),
                ));
            },
        }
    }
    let proof = artifacts.directory().join("render.png");
    let document_pdf_path = worker_paths
        .map(|paths| paths.public_output.clone())
        .or_else(|| {
            request
                .explicit_paths
                .as_ref()
                .map(|paths| paths.output.clone())
        })
        .unwrap_or_else(|| artifacts.directory().join("document.pdf"));
    if resuming {
        let journal = artifacts
            .resume_publication(&document_pdf_path, &request_fingerprint)
            .map_err(|error| {
                RenderError::session(
                    artifacts.public_directory(),
                    &document_pdf_path,
                    render_id,
                    "PUBLICATION_RECOVERY_FAILED",
                    format!("cannot resume publication transaction: {error}"),
                )
            })?;
        return match journal.recover().map_err(|error| {
            RenderError::session(
                artifacts.public_directory(),
                &document_pdf_path,
                render_id,
                "PUBLICATION_RECOVERY_FAILED",
                format!("cannot recover publication transaction: {error}"),
            )
        })? {
            PublicationRecoveryState::Planned => Err(RenderError::session(
                artifacts.public_directory(),
                &document_pdf_path,
                render_id,
                "PUBLICATION_RESTART_REQUIRED",
                "publication stopped before sealing; choose a new artifact path to restart without mutating partial evidence",
            )),
            PublicationRecoveryState::Committed {
                summary, cli_bytes, ..
            } => Ok(PublicationStart::Recovered(RenderOutcome::from_sealed(
                summary, cli_bytes,
            ))),
        };
    }
    let record_session_artifact = |result| record_artifact(&artifacts, &document_pdf_path, result);
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
    let journal = if worker_paths.is_some() {
        None
    } else {
        let journal = artifacts
            .begin_publication(&document_pdf_path, &request_fingerprint)
            .map_err(|error| {
                RenderError::session(
                    artifacts.public_directory(),
                    &document_pdf_path,
                    render_id,
                    "PUBLICATION_TRANSACTION_FAILED",
                    format!("cannot begin publication transaction: {error}"),
                )
            })?;
        let recovery_state = journal.recover().map_err(|error| {
            RenderError::session(
                artifacts.public_directory(),
                &document_pdf_path,
                render_id,
                "PUBLICATION_RECOVERY_FAILED",
                format!("cannot inspect publication recovery state: {error}"),
            )
        })?;
        if !matches!(recovery_state, PublicationRecoveryState::Planned) {
            return Err(RenderError::session(
                artifacts.public_directory(),
                &document_pdf_path,
                render_id,
                "PUBLICATION_RECOVERY_REQUIRED",
                publication_recovery_required_message(&recovery_state),
            ));
        }
        Some(journal)
    };
    apply_timezone(request.environment.timezone).map_err(|error| {
        fail_session(
            &artifacts,
            &document_pdf_path,
            "ENVIRONMENT_CONFIGURATION_FAILED",
            &error,
        )
    })?;
    #[cfg(feature = "shell-oracle")]
    let userscripts = if runtime.uses_shell_oracle() {
        let userscripts = artifacts.directory().join("userscripts");
        record_session_artifact(std::fs::create_dir_all(&userscripts))?;
        record_session_artifact(std::fs::write(
            userscripts.join("00-pliego-readiness.js"),
            ReadinessPolicy::default().document_start_script(),
        ))?;
        Some(userscripts)
    } else {
        None
    };
    record_session_artifact(artifacts.record_state("started", None))?;

    Ok(PublicationStart::New(PublicationTransaction {
        artifacts,
        journal,
        proof,
        #[cfg(feature = "shell-oracle")]
        userscripts,
        document_pdf_path,
        environment,
    }))
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn render_with_shell_oracle(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    request
        .runtime_policy
        .validate()
        .map_err(|error| RenderError::request("INVALID_REQUEST", error.to_string()))?;
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
    let render_id = stable_render_id_with_runtime_policy(
        &input_bytes,
        request.environment,
        request.page,
        &resource_policy,
        request.allow_host_fonts,
        request.runtime_policy,
    );
    let input_url = url::Url::from_file_path(document.path()).map_err(|_| {
        RenderError::request(
            "INVALID_REQUEST",
            "cannot convert document path to a file URL",
        )
    })?;
    let publication = match begin_shell_oracle_publication(
        &request,
        &resource_policy,
        &render_id,
        document.path(),
    )? {
        PublicationStart::New(publication) => publication,
        PublicationStart::Recovered(outcome) => return Ok(outcome),
    };
    let PublicationTransaction {
        artifacts,
        journal,
        proof,
        userscripts,
        document_pdf_path,
        mut environment,
    } = publication;
    let journal = journal.expect("direct publication must own its journal");
    let userscripts = userscripts.expect("shell oracle publication must prepare its userscripts");
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
    if request.allow_partial_scene || scene_capture_code(&scene_capture).is_none() {
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
    }
    publish_captured_document(
        &request,
        &document,
        &render_id,
        PublicationTransaction {
            artifacts,
            journal: Some(journal),
            proof,
            userscripts: Some(userscripts),
            document_pdf_path,
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
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeferredCapturedPublication {
    pub(crate) schema: String,
    pub(crate) version: u32,
    pub(crate) render_id: String,
    pub(crate) readiness_sha256: String,
    pub(crate) readiness_bytes: u64,
    pub(crate) resolved_input_hash: String,
    pub(crate) controlled_runtime_ms: f64,
    pub(crate) scene_capture_ms: f64,
    pub(crate) scene_schema: String,
    pub(crate) scene_version: u32,
    pub(crate) scene_hash: String,
    pub(crate) page_count: usize,
    pub(crate) preview_count: usize,
    pub(crate) capture_status: String,
    pub(crate) capture_code: Option<String>,
    pub(crate) preview_status: String,
    pub(crate) unsupported_event_count: usize,
    pub(crate) text_mapping_gap_count: usize,
    pub(crate) pdf_status: String,
    pub(crate) pdf_structure_status: String,
    pub(crate) scene_setup_ms: f64,
    pub(crate) preview_ms: f64,
    pub(crate) pdf_ms: f64,
    pub(crate) rendered_bytes: u64,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl DeferredCapturedPublication {
    pub(crate) fn validate(&self, expected_render_id: &str) -> Result<(), &'static str> {
        if self.schema != "pliego.deferred-captured-publication" ||
            self.version != 1 ||
            self.render_id != expected_render_id ||
            !is_sha256_content_address(&self.render_id) ||
            !is_sha256_content_address(&self.readiness_sha256) ||
            !is_sha256_content_address(&self.resolved_input_hash) ||
            !is_sha256_content_address(&self.scene_hash) ||
            self.readiness_bytes == 0 ||
            self.readiness_bytes > 1024 * 1024 ||
            self.rendered_bytes == 0 ||
            self.page_count == 0 ||
            u64::try_from(self.page_count).unwrap_or(u64::MAX) > MAX_PROMOTION_TREE_ENTRIES ||
            u64::try_from(self.preview_count).unwrap_or(u64::MAX) > MAX_PROMOTION_TREE_ENTRIES ||
            self.preview_count > self.page_count ||
            !matches!(self.capture_status.as_str(), "complete" | "partial") ||
            !matches!(self.preview_status.as_str(), "rendered" | "unsupported") ||
            !matches!(self.pdf_status.as_str(), "rendered" | "failed") ||
            !matches!(self.pdf_structure_status.as_str(), "rendered" | "failed") ||
            (self.preview_status == "rendered" && self.preview_count != self.page_count) ||
            (self.preview_status == "unsupported" && self.preview_count != 0) ||
            [
                self.controlled_runtime_ms,
                self.scene_capture_ms,
                self.scene_setup_ms,
                self.preview_ms,
                self.pdf_ms,
            ]
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err("deferred capture receipt is inconsistent");
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn is_sha256_content_address(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 &&
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn publication_summary(
    request: &RenderRequest,
    document_root: &Path,
    resolved_input: &Path,
    render_id: &str,
    public_artifacts: &Path,
    document_pdf_path: &Path,
    environment: &serde_json::Value,
    readiness_payload: &serde_json::Value,
    deferred: &DeferredCapturedPublication,
) -> serde_json::Value {
    let scene_previews = if deferred.preview_count == 0 {
        Vec::new()
    } else if deferred.preview_count == 1 {
        vec![public_artifacts.join("scene-preview.png")]
    } else {
        (1..=deferred.preview_count)
            .map(|page| {
                public_artifacts
                    .join("pages")
                    .join(format!("page-{page:04}.png"))
            })
            .collect::<Vec<_>>()
    };
    let scene_previews = scene_previews
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let scene_preview = scene_previews.first().cloned();
    serde_json::json!({
        "artifacts": public_artifacts.to_string_lossy(),
        "bundle": public_artifacts.join("bundle.json").to_string_lossy(),
        "engine": "pliego",
        "document_root": document_root.to_string_lossy(),
        "environment": environment,
        "environment_artifact": public_artifacts.join("environment.json").to_string_lossy(),
        "input": request.input.to_string_lossy(),
        "resolved_input": resolved_input.to_string_lossy(),
        "layout_debug": public_artifacts.join("layout-debug.json").to_string_lossy(),
        "pages_artifact": public_artifacts.join("pages.json").to_string_lossy(),
        "readiness": readiness_payload,
        "render_id": render_id,
        "resolved_input_hash": deferred.resolved_input_hash,
        "rendered_image": public_artifacts.join("render.png").to_string_lossy(),
        "scene": {
            "schema": deferred.scene_schema,
            "version": deferred.scene_version,
            "hash": deferred.scene_hash,
            "validation": "valid",
            "capture_status": deferred.capture_status,
            "capture_code": deferred.capture_code,
            "preview_status": deferred.preview_status,
            "unsupported_event_count": deferred.unsupported_event_count,
            "text_mapping_gap_count": deferred.text_mapping_gap_count,
        },
        "scene_artifact": public_artifacts.join("scene.json").to_string_lossy(),
        "fonts_artifact": public_artifacts.join("fonts.json").to_string_lossy(),
        "scene_report": public_artifacts.join("scene-report.json").to_string_lossy(),
        "scene_preview": scene_preview,
        "scene_previews": scene_previews,
        "document_pdf": document_pdf_path.to_string_lossy(),
        "document_pdf_status": deferred.pdf_status,
        "pdf_structure": public_artifacts.join("pdf-structure.json").to_string_lossy(),
        "pdf_structure_status": deferred.pdf_structure_status,
        "phase_timings_ms": {
            "controlled_runtime": deferred.controlled_runtime_ms,
            "scene_capture": deferred.scene_capture_ms,
            "scene_setup": deferred.scene_setup_ms,
            "preview_raster": deferred.preview_ms,
            "pdf_serialize": deferred.pdf_ms,
        },
        "servo_base_sha": SERVO_BASE_SHA,
        "servo_build": SERVO_BUILD_VERSION,
        "rendered_bytes": deferred.rendered_bytes,
        "status": "rendered"
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn finalize_persisted_publication(
    request: &RenderRequest,
    document_root: &Path,
    resolved_input: &Path,
    render_id: &str,
    artifacts: SessionArtifacts,
    journal: PublicationJournal,
    document_pdf_path: PathBuf,
    mut environment: serde_json::Value,
    readiness_payload: serde_json::Value,
    deferred: DeferredCapturedPublication,
    preserve_staged_readiness: bool,
    prepared_output: Option<PreparedDocumentPdf>,
) -> Result<RenderOutcome, RenderError> {
    let fail = |code: &str, message: &str| {
        fail_session_with_readiness_policy(
            &artifacts,
            &document_pdf_path,
            code,
            message,
            preserve_staged_readiness,
        )
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

    let mut prepared_output = match prepared_output {
        Some(prepared_output) => prepared_output,
        None => artifacts
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
            })?,
    };
    set_document_pdf_environment(
        &mut environment,
        &document_pdf_path,
        &deferred.pdf_status,
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
    let public_artifacts = artifacts.public_directory();
    let summary = publication_summary(
        request,
        document_root,
        resolved_input,
        render_id,
        public_artifacts,
        &document_pdf_path,
        &environment,
        &readiness_payload,
        &deferred,
    );
    let outcome = match RenderOutcome::from_summary(summary) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(fail_before_output_commit(
                &mut environment,
                "PUBLICATION_PREPARE_FAILED",
                format!("cannot serialize publication outcome: {error}"),
                None,
            ));
        },
    };
    let prepared_receipt = match journal.record_prepared(
        &prepared_output,
        &prepared_bundle,
        &outcome.cli_bytes,
    ) {
        Ok(receipt) => receipt,
        Err(error) => {
            let bundle_cleanup_warning = prepared_bundle.discard().err().map(|cleanup_error| {
                format!(
                    "cannot remove invalidated owned bundle after failed publication preparation: {cleanup_error}"
                )
            });
            return Err(fail_before_output_commit(
                &mut environment,
                "PUBLICATION_PREPARE_FAILED",
                format!("cannot seal prepared publication receipt: {error}"),
                bundle_cleanup_warning,
            ));
        },
    };
    if let Err(error) = prepared_output.commit(&prepared_bundle) {
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
        prepared_output.preserve_for_recovery();
        prepared_bundle.preserve();
        return Err(RenderError::session(
            public_artifacts,
            &document_pdf_path,
            render_id,
            "OUTPUT_COMMIT_RECOVERY_REQUIRED",
            format!("sealed publication could not commit ({code}): {message}"),
        ));
    }
    if let Err(error) = journal.record_committed(&prepared_receipt, Some(&prepared_bundle)) {
        prepared_bundle.preserve();
        return Err(RenderError::session(
            public_artifacts,
            &document_pdf_path,
            render_id,
            "OUTPUT_COMMIT_RECOVERY_REQUIRED",
            format!(
                "output is visible but its committed publication receipt could not be recorded: {error}"
            ),
        ));
    }
    prepared_bundle.preserve();
    Ok(outcome)
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
        journal,
        proof,
        #[cfg(feature = "shell-oracle")]
            userscripts: _,
        document_pdf_path,
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
    let finish_failure = |error| finish_document_worker_failure(error);
    let record_session_artifact = |result: std::io::Result<()>| {
        result.map_err(|error| {
            finish_failure(fail(
                "SESSION_ARTIFACT_WRITE_FAILED",
                &format!("cannot write session artifact: {error}"),
            ))
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
            finish_failure(error)
        };

    match stage_resolved_input_hash(&mut environment, &resolved_input_hash) {
        Ok(true) => record_session_artifact(artifacts.write_environment(&environment))?,
        Ok(false) => {},
        Err(error) => return Err(finish_failure(fail(error.code, &error.message))),
    }
    if let Some(resource) = unexpected_host_font(&scene_capture, request.allow_host_fonts) {
        return Err(finish_failure(fail(
            "HOST_FONT_POLICY_VIOLATION",
            &format!(
                "Servo selected host font {} while host fonts were disabled",
                resource
            ),
        )));
    }
    if !request.allow_partial_scene &&
        let Some(failure) = rejected_scene_capture(&scene_capture)
    {
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
        return Err(finish_failure(error));
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
            return Err(finish_failure(failure));
        },
    };
    let rendered_bytes = std::fs::metadata(&proof)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if rendered_bytes == 0 {
        return Err(finish_failure(fail(
            "RENDER_OUTPUT_MISSING",
            "Servo did not produce a rendered image",
        )));
    }
    let (readiness_sha256, readiness_bytes) = artifacts
        .artifact_identity("readiness.json")
        .map_err(|error| {
            fail_before_output_commit(
                &mut environment,
                "PUBLICATION_PREPARE_FAILED",
                format!("cannot bind deferred readiness evidence: {error}"),
                None,
            )
        })?;
    let deferred = DeferredCapturedPublication {
        schema: "pliego.deferred-captured-publication".into(),
        version: 1,
        render_id: render_id.to_owned(),
        readiness_sha256,
        readiness_bytes,
        resolved_input_hash: resolved_input_hash.clone(),
        controlled_runtime_ms,
        scene_capture_ms,
        scene_schema: scene_capture.scene.schema.to_owned(),
        scene_version: scene_capture.scene.version,
        scene_hash: scene_artifacts.scene_hash.clone(),
        page_count: scene_capture.scene.pages.len(),
        preview_count: scene_artifacts.preview_paths.len(),
        capture_status: scene_artifacts.capture_status.into(),
        capture_code: scene_artifacts.capture_code.map(str::to_owned),
        preview_status: scene_artifacts.preview_status.into(),
        unsupported_event_count: scene_capture.unsupported_events.len(),
        text_mapping_gap_count: scene_capture.text_mapping_gaps.len(),
        pdf_status: scene_artifacts.pdf_status.into(),
        pdf_structure_status: scene_artifacts.pdf_structure_status.into(),
        scene_setup_ms: scene_artifacts.scene_setup_ms,
        preview_ms: scene_artifacts.preview_ms,
        pdf_ms: scene_artifacts.pdf_ms,
        rendered_bytes,
    };
    #[cfg(feature = "document-session")]
    if render_supervisor::is_worker_process() {
        render_supervisor::finish_captured_worker(deferred);
    }
    finalize_persisted_publication(
        request,
        document.root(),
        document.path(),
        render_id,
        artifacts,
        journal.expect("direct publication must own its journal"),
        document_pdf_path,
        environment,
        readiness_payload,
        deferred,
        preserve_staged_readiness,
        None,
    )
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) struct ExpectedInputIdentity {
    pub(crate) url: url::Url,
    pub(crate) sha256: String,
    pub(crate) content_address: String,
    pub(crate) bytes: u64,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn expected_input_identity(
    path: &std::path::Path,
    input_bytes: &[u8],
) -> Result<ExpectedInputIdentity, RenderError> {
    let url = url::Url::from_file_path(path).map_err(|_| {
        RenderError::request(
            "INVALID_REQUEST",
            "cannot convert document path to a file URL",
        )
    })?;
    let sha256 = sha256_hex(input_bytes);
    Ok(ExpectedInputIdentity {
        url,
        content_address: format!("sha256:{sha256}"),
        sha256,
        bytes: input_bytes.len() as u64,
    })
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
enum PreparedDocumentSessionStart {
    New(PreparedDocumentSessionRender),
    Recovered(RenderOutcome),
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) struct SupervisorRenderIdentity {
    pub(crate) render_id: String,
    pub(crate) request_fingerprint: String,
    pub(crate) document_root: PathBuf,
    pub(crate) resolved_input: PathBuf,
    pub(crate) locale: &'static str,
    pub(crate) timezone: &'static str,
    pub(crate) page: serde_json::Value,
    pub(crate) resource_policy: serde_json::Value,
    pub(crate) resolved_resource_policy: ResourcePolicy,
    pub(crate) expected_input: ExpectedInputIdentity,
    pub(crate) allow_host_fonts: bool,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) fn supervisor_render_identity(
    request: &RenderRequest,
    controlled: bool,
) -> Result<SupervisorRenderIdentity, RenderError> {
    request
        .runtime_policy
        .validate()
        .map_err(|error| RenderError::request("INVALID_REQUEST", error.to_string()))?;
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
    let base_render_id = stable_render_id_with_runtime_policy(
        &input_bytes,
        request.environment,
        request.page,
        &resource_policy,
        request.allow_host_fonts,
        request.runtime_policy,
    );
    let runtime = if controlled {
        PublicationRuntimeIdentity::DocumentSessionControlledCapture
    } else {
        PublicationRuntimeIdentity::DocumentSession
    };
    let render_id = if controlled {
        controlled_capture_render_id(&base_render_id)
    } else {
        base_render_id
    };
    let request_fingerprint = publication_request_fingerprint_with_runtime_policy(
        runtime,
        &render_id,
        request.allow_partial_scene,
        &request.input,
        document.path(),
        resource_policy.summary_asset_manifest_path(),
        request.runtime_policy,
    );
    let expected_input = expected_input_identity(document.path(), &input_bytes)?;
    let page = page_artifact(request.page);
    let resource_policy_artifact = resource_policy.artifact(&render_id);
    Ok(SupervisorRenderIdentity {
        render_id,
        request_fingerprint,
        document_root: document.root().to_owned(),
        resolved_input: document.path().to_owned(),
        locale: request.environment.locale,
        timezone: request.environment.timezone,
        page,
        resource_policy: resource_policy_artifact,
        resolved_resource_policy: resource_policy,
        expected_input,
        allow_host_fonts: request.allow_host_fonts,
    })
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) fn recover_supervised_publication(
    artifacts: SessionArtifacts,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<RenderOutcome, RenderError> {
    let journal = artifacts
        .resume_publication(&paths.public_output, &identity.request_fingerprint)
        .map_err(|error| {
            RenderError::session(
                &paths.public_artifacts,
                &paths.public_output,
                &identity.render_id,
                "PUBLICATION_RECOVERY_FAILED",
                format!("cannot resume publication transaction: {error}"),
            )
        })?;
    match journal.recover().map_err(|error| {
        RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "PUBLICATION_RECOVERY_FAILED",
            format!("cannot recover publication transaction: {error}"),
        )
    })? {
        PublicationRecoveryState::Committed {
            summary, cli_bytes, ..
        } => Ok(RenderOutcome::from_sealed(summary, cli_bytes)),
        PublicationRecoveryState::Planned => Err(RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "PUBLICATION_RESTART_REQUIRED",
            "publication stopped before sealing; choose a new artifact path to restart without mutating partial evidence",
        )),
    }
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) fn preflight_supervised_publication_outcome(
    request: &RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
    deferred: &DeferredCapturedPublication,
) -> Result<(), RenderError> {
    let rejected = || {
        RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "RUNTIME_TERMINATED",
            "document runtime terminated before returning a trusted result",
        )
    };
    let artifacts = SessionArtifacts::open_staged_for_publication(
        &paths.staging_artifacts,
        &paths.public_artifacts,
        &identity.render_id,
    )
    .map_err(|_| rejected())?;
    let readiness = artifacts
        .read_json_artifact(
            "readiness.json",
            &deferred.readiness_sha256,
            deferred.readiness_bytes,
        )
        .map_err(|_| rejected())?;
    let readiness_payload = match parse_snapshot(&readiness.to_string()) {
        Ok(Readiness::Ready { payload }) => payload,
        _ => {
            return Err(rejected());
        },
    };
    let (environment_sha256, environment_bytes) = artifacts
        .artifact_identity("environment.json")
        .map_err(|_| rejected())?;
    let mut environment = artifacts
        .read_json_artifact("environment.json", &environment_sha256, environment_bytes)
        .map_err(|_| rejected())?;
    set_document_pdf_environment(
        &mut environment,
        &paths.public_output,
        &deferred.pdf_status,
        None,
    );
    let summary = publication_summary(
        request,
        &identity.document_root,
        &identity.resolved_input,
        &identity.render_id,
        &paths.public_artifacts,
        &paths.public_output,
        &environment,
        &readiness_payload,
        deferred,
    );
    let outcome = RenderOutcome::from_summary(summary).map_err(|error| {
        RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "PUBLICATION_PREPARE_FAILED",
            format!("cannot serialize publication outcome: {error}"),
        )
    })?;
    validate_publication_outcome_bytes(&outcome.cli_bytes).map_err(|error| {
        RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "PUBLICATION_PREPARE_FAILED",
            format!("cannot seal prepared publication receipt: {error}"),
        )
    })
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) fn finalize_supervised_publication(
    request: RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
    deferred: DeferredCapturedPublication,
    prepared_output: Option<PreparedDocumentPdf>,
) -> Result<RenderOutcome, RenderError> {
    deferred.validate(&identity.render_id).map_err(|message| {
        RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "RUNTIME_TERMINATED",
            message,
        )
    })?;
    let artifacts = SessionArtifacts::open_for_publication_recovery(
        &paths.public_artifacts,
        &identity.render_id,
    )
    .map_err(|error| {
        RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "PUBLICATION_RECOVERY_FAILED",
            format!("cannot bind promoted artifact root: {error}"),
        )
    })?;
    let readiness = artifacts
        .read_json_artifact(
            "readiness.json",
            &deferred.readiness_sha256,
            deferred.readiness_bytes,
        )
        .map_err(|error| {
            fail_session_with_readiness_policy(
                &artifacts,
                &paths.public_output,
                "PUBLICATION_PREPARE_FAILED",
                &format!("cannot validate deferred readiness evidence: {error}"),
                true,
            )
        })?;
    let readiness_payload = match parse_snapshot(&readiness.to_string()) {
        Ok(Readiness::Ready { payload }) => payload,
        _ => {
            return Err(fail_session_with_readiness_policy(
                &artifacts,
                &paths.public_output,
                "READINESS_INVALID_RESULT",
                "deferred readiness evidence is not a ready snapshot",
                true,
            ));
        },
    };
    let (environment_sha256, environment_bytes) = artifacts
        .artifact_identity("environment.json")
        .map_err(|error| {
        fail_session_with_readiness_policy(
            &artifacts,
            &paths.public_output,
            "PUBLICATION_PREPARE_FAILED",
            &format!("cannot bind deferred environment evidence: {error}"),
            true,
        )
    })?;
    let environment = artifacts
        .read_json_artifact("environment.json", &environment_sha256, environment_bytes)
        .map_err(|error| {
            fail_session_with_readiness_policy(
                &artifacts,
                &paths.public_output,
                "PUBLICATION_PREPARE_FAILED",
                &format!("cannot validate deferred environment evidence: {error}"),
                true,
            )
        })?;
    let journal = artifacts
        .resume_publication(&paths.public_output, &identity.request_fingerprint)
        .map_err(|error| {
            fail_session_with_readiness_policy(
                &artifacts,
                &paths.public_output,
                "PUBLICATION_RECOVERY_FAILED",
                &format!("cannot resume deferred publication transaction: {error}"),
                true,
            )
        })?;
    if !matches!(
        journal.recover().map_err(|error| {
            fail_session_with_readiness_policy(
                &artifacts,
                &paths.public_output,
                "PUBLICATION_RECOVERY_FAILED",
                &format!("cannot inspect deferred publication transaction: {error}"),
                true,
            )
        })?,
        PublicationRecoveryState::Planned
    ) {
        return Err(fail_session_with_readiness_policy(
            &artifacts,
            &paths.public_output,
            "PUBLICATION_RECOVERY_REQUIRED",
            "deferred publication transaction is not in its planned state",
            true,
        ));
    }
    finalize_persisted_publication(
        &request,
        &identity.document_root,
        &identity.resolved_input,
        &identity.render_id,
        artifacts,
        journal,
        paths.public_output.clone(),
        environment,
        readiness_payload,
        deferred,
        true,
        prepared_output,
    )
}
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[allow(dead_code)]
// Retained for the explicit realtime diagnostic pairing; the public CLI selects controlled capture.
fn render(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    #[cfg(test)]
    {
        return render_document_session_in_process(request);
    }
    #[cfg(not(test))]
    {
        render_supervisor::render(request, false)
    }
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn render_document_session_in_process(
    request: RenderRequest,
) -> Result<RenderOutcome, RenderError> {
    let prepared = match prepare_document_session_render(request)? {
        PreparedDocumentSessionStart::New(prepared) => prepared,
        PreparedDocumentSessionStart::Recovered(outcome) => return Ok(outcome),
    };
    let PreparedDocumentSessionRender {
        request,
        document,
        resource_policy,
        render_id,
        expected_input,
        publication,
    } = prepared;
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
fn render_controlled(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    #[cfg(test)]
    {
        return render_controlled_document_session_in_process(request);
    }
    #[cfg(not(test))]
    {
        render_supervisor::render(request, true)
    }
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn render_controlled_document_session_in_process(
    request: RenderRequest,
) -> Result<RenderOutcome, RenderError> {
    render_controlled_transaction(request, PreparedDocumentCaptureCandidate::capture)
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn render_controlled_transaction<C>(
    request: RenderRequest,
    capture_candidate: C,
) -> Result<RenderOutcome, RenderError>
where
    C: FnOnce(PreparedDocumentCaptureCandidate) -> Result<DocumentCaptureOutcome, SessionError>,
{
    let prepared = match prepare_document_session_render_for_runtime(
        request,
        PublicationRuntimeIdentity::DocumentSessionControlledCapture,
    )? {
        PreparedDocumentSessionStart::New(prepared) => prepared,
        PreparedDocumentSessionStart::Recovered(outcome) => return Ok(outcome),
    };
    let PreparedDocumentSessionRender {
        request,
        document,
        resource_policy,
        render_id,
        expected_input,
        publication,
    } = prepared;
    let result = DocumentSession::from_resolved_controlled(
        &document,
        resource_policy,
        request.environment,
        request.page,
        request.allow_host_fonts,
        ReadinessPolicy::default(),
        request.runtime_policy,
    )
    .and_then(ControlledDocumentSession::prepare_capture_candidate)
    .and_then(capture_candidate);

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
    test,
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn render_controlled_with_post_reservation_paint_invalidation_for_test(
    request: RenderRequest,
) -> Result<RenderOutcome, RenderError> {
    render_controlled_transaction(request, |candidate| {
        candidate.capture_with_paint_hook(|webview, _ticket| webview.paint())
    })
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn prepare_document_session_render(
    request: RenderRequest,
) -> Result<PreparedDocumentSessionStart, RenderError> {
    prepare_document_session_render_for_runtime(
        request,
        PublicationRuntimeIdentity::DocumentSession,
    )
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
fn prepare_document_session_render_for_runtime(
    request: RenderRequest,
    runtime: PublicationRuntimeIdentity,
) -> Result<PreparedDocumentSessionStart, RenderError> {
    request
        .runtime_policy
        .validate()
        .map_err(|error| RenderError::request("INVALID_REQUEST", error.to_string()))?;
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
    let base_render_id = stable_render_id_with_runtime_policy(
        &input_bytes,
        request.environment,
        request.page,
        &resource_policy,
        request.allow_host_fonts,
        request.runtime_policy,
    );
    let render_id = match runtime {
        PublicationRuntimeIdentity::DocumentSessionControlledCapture => {
            controlled_capture_render_id(&base_render_id)
        },
        _ => base_render_id,
    };
    let expected_input = expected_input_identity(document.path(), &input_bytes)?;
    let publication = match begin_publication_for_runtime(
        &request,
        &resource_policy,
        &render_id,
        document.path(),
        runtime,
    )? {
        PublicationStart::New(publication) => publication,
        PublicationStart::Recovered(outcome) => {
            return Ok(PreparedDocumentSessionStart::Recovered(outcome));
        },
    };

    Ok(PreparedDocumentSessionStart::New(
        PreparedDocumentSessionRender {
            request,
            document,
            resource_policy,
            render_id,
            expected_input,
            publication,
        },
    ))
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
        (request.allow_partial_scene || scene_capture_code(&capture).is_none())
            .then_some(&layout_debug),
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
    finish_document_worker_failure(fail_session_with_readiness_policy(
        &publication.artifacts,
        &publication.document_pdf_path,
        code,
        message,
        true,
    ))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn finish_document_worker_failure(error: RenderError) -> RenderError {
    #[cfg(feature = "document-session")]
    if render_supervisor::is_worker_process() {
        render_supervisor::finish_failed_worker(error);
    }
    error
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

#[cfg(all(test, not(any(target_os = "android", target_env = "ohos"))))]
fn stable_render_id(
    input_bytes: &[u8],
    environment: RenderEnvironment,
    page: PageDefinition,
    resource_policy: &ResourcePolicy,
    allow_host_fonts: bool,
) -> String {
    stable_render_id_with_runtime_policy(
        input_bytes,
        environment,
        page,
        resource_policy,
        allow_host_fonts,
        DeterministicRuntimePolicy::default(),
    )
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn stable_render_id_with_runtime_policy(
    input_bytes: &[u8],
    environment: RenderEnvironment,
    page: PageDefinition,
    resource_policy: &ResourcePolicy,
    allow_host_fonts: bool,
    runtime_policy: DeterministicRuntimePolicy,
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
    hash_runtime_policy(&mut hasher, runtime_policy);
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
fn controlled_capture_render_id(base_render_id: &str) -> String {
    let mut hasher = Sha256::new();
    update_hash_field(&mut hasher, CONTROLLED_CAPTURE_RENDER_ID_SCHEMA_MARKER);
    update_hash_field(&mut hasher, base_render_id.as_bytes());
    format!("sha256:{}", lowercase_hex(&hasher.finalize()))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
/// Bind every `RenderRequest` identity field before reusing a sealed outcome.
///
/// `runtime` prevents a transaction from crossing between the production document session and
/// the explicit shell oracle. `render_id` binds the input bytes, environment, page, semantic
/// resource content, `allow_host_fonts`, and deterministic runtime policy. The runtime policy is
/// also bound directly here so recovery remains fail-closed if render-ID composition changes.
/// The requested and canonical input paths are bound separately so a recovered summary cannot
/// report a different request spelling. The captured manifest path is also bound because the
/// sealed resource-policy artifact names it even when two manifests have identical asset content.
/// `allow_partial_scene` is the remaining render-policy field.
/// `explicit_paths` is bound by the publication plan's artifact root, requested output, canonical
/// output, and directory identities. When `RenderRequest` gains a field, its render semantics and
/// sealed-summary representation must be covered here, by `render_id`, or by the immutable
/// publication plan before recovery may reuse an outcome.
#[cfg(test)]
fn publication_request_fingerprint(
    runtime: PublicationRuntimeIdentity,
    render_id: &str,
    allow_partial_scene: bool,
    requested_input: &Path,
    resolved_input: &Path,
    summary_asset_manifest: Option<&Path>,
) -> String {
    publication_request_fingerprint_with_runtime_policy(
        runtime,
        render_id,
        allow_partial_scene,
        requested_input,
        resolved_input,
        summary_asset_manifest,
        DeterministicRuntimePolicy::default(),
    )
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn publication_request_fingerprint_with_runtime_policy(
    runtime: PublicationRuntimeIdentity,
    render_id: &str,
    allow_partial_scene: bool,
    requested_input: &Path,
    resolved_input: &Path,
    summary_asset_manifest: Option<&Path>,
    runtime_policy: DeterministicRuntimePolicy,
) -> String {
    let mut hasher = Sha256::new();
    update_hash_field(&mut hasher, PUBLICATION_REQUEST_SCHEMA_MARKER);
    update_hash_field(&mut hasher, b"runtime");
    update_hash_field(&mut hasher, runtime.fingerprint_field());
    update_hash_field(&mut hasher, b"render-id");
    update_hash_field(&mut hasher, render_id.as_bytes());
    hash_runtime_policy(&mut hasher, runtime_policy);
    update_hash_field(&mut hasher, b"requested-input");
    update_hash_path(&mut hasher, requested_input);
    update_hash_field(&mut hasher, b"resolved-input");
    update_hash_path(&mut hasher, resolved_input);
    update_hash_field(&mut hasher, b"asset-manifest");
    match summary_asset_manifest {
        None => update_hash_field(&mut hasher, b"none"),
        Some(path) => {
            update_hash_field(&mut hasher, b"some");
            update_hash_path(&mut hasher, path);
        },
    }
    update_hash_field(&mut hasher, b"allow-partial-scene");
    update_hash_field(
        &mut hasher,
        if allow_partial_scene {
            b"partial"
        } else {
            b"complete"
        },
    );
    format!("sha256:{}", lowercase_hex(&hasher.finalize()))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn hash_runtime_policy(hasher: &mut Sha256, runtime_policy: DeterministicRuntimePolicy) {
    let runtime_policy::DeterministicRuntimePolicy {
        time:
            runtime_policy::DocumentTimePolicy {
                epoch_unix_ms,
                initial_offset_ns,
            },
        settlement:
            runtime_policy::DocumentSettlementPolicy {
                infinite_source_policy,
                empty_checkpoints,
                limits:
                    runtime_policy::DocumentSettlementLimits {
                        virtual_span_ms,
                        ordinary_tasks,
                        microtasks,
                        rendering_opportunities,
                        mutations,
                        post_readiness_resources,
                        process_cpu_ms,
                        host_wall_ms,
                    },
            },
    } = runtime_policy;
    update_hash_field(hasher, b"pliego.runtime-policy.v1");
    update_hash_field(hasher, &[runtime_policy::API2_TIME_POLICY_VERSION]);
    update_hash_field(hasher, &epoch_unix_ms.to_be_bytes());
    update_hash_field(hasher, &initial_offset_ns.to_be_bytes());
    update_hash_field(hasher, &[runtime_policy::API2_SETTLEMENT_POLICY_VERSION]);
    update_hash_field(
        hasher,
        match infinite_source_policy {
            runtime_policy::InfiniteSourcePolicy::Fail => b"fail",
        },
    );
    update_hash_field(hasher, &[empty_checkpoints]);
    for value in [
        virtual_span_ms,
        ordinary_tasks,
        microtasks,
        rendering_opportunities,
        mutations,
        post_readiness_resources,
        process_cpu_ms,
        host_wall_ms,
    ] {
        update_hash_field(hasher, &value.to_be_bytes());
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_env = "ohos"))))]
fn update_hash_path(hasher: &mut Sha256, path: &Path) {
    use std::os::unix::ffi::OsStrExt;

    update_hash_field(hasher, b"pliego.os-path.unix-bytes.v1");
    update_hash_field(hasher, path.as_os_str().as_bytes());
}

#[cfg(all(windows, not(any(target_os = "android", target_env = "ohos"))))]
fn update_hash_path(hasher: &mut Sha256, path: &Path) {
    use std::os::windows::ffi::OsStrExt;

    update_hash_field(hasher, b"pliego.os-path.windows-utf16le.v1");
    let mut encoded = Vec::new();
    for unit in path.as_os_str().encode_wide() {
        encoded.extend_from_slice(&unit.to_le_bytes());
    }
    update_hash_field(hasher, &encoded);
}

#[cfg(all(
    not(any(unix, windows)),
    not(any(target_os = "android", target_env = "ohos"))
))]
fn update_hash_path(hasher: &mut Sha256, path: &Path) {
    update_hash_field(hasher, b"pliego.os-path.encoded-bytes.v1");
    update_hash_field(hasher, path.as_os_str().as_encoded_bytes());
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
    let output_absolute = raw_absolute_path(output)?;
    let output = lexical_absolute_path(&output_absolute)?;
    let artifacts_lexical = lexical_absolute_path(artifacts)?;
    if output.starts_with(&artifacts_lexical) {
        return Ok(true);
    }
    let artifacts = artifacts.canonicalize()?;
    match output_absolute.parent().map(Path::canonicalize) {
        Some(Ok(parent)) => Ok(parent.starts_with(artifacts)),
        Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Some(Err(error)) => Err(error),
        None => Ok(false),
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn output_overlaps_uncreated_artifacts(
    output: &Path,
    artifacts: &Path,
    probe_container: &Path,
) -> std::io::Result<bool> {
    let output_absolute = raw_absolute_path(output)?;
    let output = lexical_absolute_path(&output_absolute)?;
    let artifacts_lexical = lexical_absolute_path(artifacts)?;
    if output.starts_with(&artifacts_lexical) {
        return Ok(true);
    }
    if future_artifact_scaffold_contains(&output_absolute, &artifacts_lexical, probe_container)? {
        return Ok(true);
    }

    match output_absolute.parent().map(Path::canonicalize) {
        Some(Ok(parent)) => match artifacts.canonicalize() {
            Ok(artifacts) => Ok(parent.starts_with(artifacts)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                future_artifact_scaffold_contains(&parent, &artifacts_lexical, probe_container)
            },
            Err(error) => Err(error),
        },
        Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(parent) = output_absolute.parent() else {
                return Ok(false);
            };
            if future_artifact_scaffold_contains(parent, &artifacts_lexical, probe_container)? {
                return Ok(true);
            }
            let Some(mut target) = unresolved_symlink_target(parent)? else {
                return Ok(false);
            };
            for _ in 0..40 {
                if future_artifact_scaffold_contains(&target, &artifacts_lexical, probe_container)?
                {
                    return Ok(true);
                }
                let Some(next) = unresolved_symlink_target(&target)? else {
                    return Ok(false);
                };
                target = next;
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "output parent contains too many unresolved symbolic links",
            ))
        },
        Some(Err(error)) => Err(error),
        None => Ok(false),
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn future_artifact_scaffold_contains(
    candidate: &Path,
    artifacts: &Path,
    probe_container: &Path,
) -> std::io::Result<bool> {
    let artifact_components = artifacts.components().collect::<Vec<_>>();
    let candidate_components = candidate.components().collect::<Vec<_>>();
    if artifact_components.is_empty() || candidate_components.len() < artifact_components.len() {
        return Ok(false);
    }

    let artifact_parent = artifact_components[..artifact_components.len() - 1]
        .iter()
        .map(|component| component.as_os_str())
        .collect::<PathBuf>();
    let candidate_parent = candidate_components[..artifact_components.len() - 1]
        .iter()
        .map(|component| component.as_os_str())
        .collect::<PathBuf>();
    match same_file::is_same_file(&artifact_parent, &candidate_parent) {
        Ok(true) => {},
        Ok(false) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }

    let Some(artifact_leaf) = artifacts.file_name() else {
        return Ok(false);
    };
    let std::path::Component::Normal(candidate_leaf) =
        candidate_components[artifact_components.len() - 1]
    else {
        return Ok(false);
    };
    #[cfg(windows)]
    if candidate_components.len() == artifact_components.len() &&
        prospective_dos_short_name(candidate_leaf)
    {
        // NTFS short-name allocation and tunneling are destination-directory dependent, so a
        // private probe cannot prove this unresolved spelling will remain distinct after rename.
        return Ok(true);
    }

    let probe_root = probe_container.join(".pliego-path-lookup");
    let probe_artifacts = probe_root.join(artifact_leaf);
    std::fs::create_dir(&probe_root)?;
    if let Err(error) = std::fs::create_dir(&probe_artifacts) {
        let _ = std::fs::remove_dir(&probe_root);
        return Err(error);
    }
    let probe_resources = probe_artifacts.join("resources");
    if let Err(error) = std::fs::create_dir(&probe_resources) {
        let _ = std::fs::remove_dir(&probe_artifacts);
        let _ = std::fs::remove_dir(&probe_root);
        return Err(error);
    }
    let result = (|| {
        let candidate_probe = probe_root.join(candidate_leaf);
        match same_file::is_same_file(&probe_artifacts, &candidate_probe) {
            Ok(true) => {},
            Ok(false) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        }
        let mut inside_resources = false;
        for component in &candidate_components[artifact_components.len()..] {
            match component {
                std::path::Component::CurDir => {},
                std::path::Component::Normal(name) if !inside_resources => {
                    match same_file::is_same_file(&probe_resources, candidate_probe.join(name)) {
                        Ok(true) => inside_resources = true,
                        Ok(false) => return Ok(false),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(false);
                        },
                        Err(error) => return Err(error),
                    }
                },
                std::path::Component::ParentDir if inside_resources => {
                    inside_resources = false;
                },
                std::path::Component::ParentDir => {
                    // The direct API resolves this against real public ancestors after it creates
                    // the artifact scaffold. Relocating the suffix under the private probe cannot
                    // reproduce an escape and possible re-entry safely, so reject it as overlap.
                    return Ok(true);
                },
                _ => return Ok(false),
            }
        }
        Ok(true)
    })();
    let cleanup = std::fs::remove_dir(&probe_resources)
        .and_then(|()| std::fs::remove_dir(&probe_artifacts))
        .and_then(|()| std::fs::remove_dir(&probe_root));
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[cfg(all(windows, not(any(target_os = "android", target_env = "ohos"))))]
fn prospective_dos_short_name(name: &std::ffi::OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    let name = name.encode_wide().collect::<Vec<_>>();
    let dots = name
        .iter()
        .enumerate()
        .filter_map(|(index, unit)| (*unit == u16::from(b'.')).then_some(index))
        .collect::<Vec<_>>();
    if dots.len() > 1 {
        return false;
    }
    let (base, extension) = dots.first().map_or_else(
        || (name.as_slice(), None),
        |dot| (&name[..*dot], Some(&name[*dot + 1..])),
    );
    if base.is_empty() ||
        base.len() > 8 ||
        extension.is_some_and(|extension| extension.is_empty() || extension.len() > 3) ||
        !base.iter().all(|unit| valid_dos_short_name_unit(*unit)) ||
        extension.is_some_and(|extension| {
            !extension
                .iter()
                .all(|unit| valid_dos_short_name_unit(*unit))
        })
    {
        return false;
    }
    let Some(tilde) = base.iter().rposition(|unit| *unit == u16::from(b'~')) else {
        return false;
    };
    let prefix = &base[..tilde];
    let sequence = &base[tilde + 1..];
    !prefix.is_empty() &&
        !sequence.is_empty() &&
        sequence.len() <= 6 &&
        sequence
            .iter()
            .all(|unit| (u16::from(b'0')..=u16::from(b'9')).contains(unit))
}

#[cfg(all(windows, not(any(target_os = "android", target_env = "ohos"))))]
fn valid_dos_short_name_unit(unit: u16) -> bool {
    unit > 0x20 &&
        !matches!(
            unit,
            0x22 | 0x2b |
                0x2c |
                0x2e |
                0x2f |
                0x3a |
                0x3b |
                0x3c |
                0x3d |
                0x3e |
                0x3f |
                0x5b |
                0x5c |
                0x5d |
                0x7c
        )
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn unresolved_symlink_target(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut current = path;
    let mut suffix = std::collections::VecDeque::new();
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(_) => {
                let target = match std::fs::read_link(current) {
                    Ok(target) if target.is_absolute() => target,
                    Ok(target) => current
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(target),
                    Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                        return Ok(None);
                    },
                    #[cfg(windows)]
                    Err(error) if error.raw_os_error() == Some(4390) => return Ok(None),
                    Err(error) => return Err(error),
                };
                return Ok(Some(
                    suffix
                        .into_iter()
                        .fold(target, |path, component| path.join(component)),
                ));
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = current.file_name() else {
                    return Ok(None);
                };
                suffix.push_front(name.to_os_string());
                let Some(parent) = current.parent() else {
                    return Ok(None);
                };
                current = parent;
            },
            Err(error) => return Err(error),
        }
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
fn raw_absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    {
        // Match the direct publication path for drive-relative spellings and Win32 normalization
        // of dot components plus terminal dots/spaces. Verbatim paths remain verbatim.
        return std::path::absolute(path);
    }
    #[cfg(not(windows))]
    {
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
        Ok(std::env::current_dir()?.join(path))
    }
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

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[derive(Debug, Default, PartialEq)]
struct PendingResource {
    urls: Vec<String>,
    method: Option<String>,
    response_status: Option<u16>,
    content_type: Option<String>,
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl PendingResource {
    fn observe_url(&mut self, url: String) -> bool {
        if self.urls.iter().any(|observed| observed == &url) {
            return false;
        }
        self.urls.push(url);
        true
    }
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[derive(Debug, PartialEq)]
struct CompletedResource {
    urls: Vec<String>,
    method: Option<String>,
    response_status: Option<u16>,
    content_type: Option<String>,
    sha256: String,
    body: Vec<u8>,
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
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
    #[cfg(feature = "shell-oracle")]
    failure: Option<ResourcePolicyFailure>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl ResourceCapture {
    #[cfg(feature = "shell-oracle")]
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

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[derive(Debug, PartialEq)]
struct ResourceMapConflict {
    url: String,
    first: String,
    second: String,
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl std::fmt::Display for ResourceMapConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "observed URL {} resolved to both {} and {}",
            self.url, self.first, self.second
        )
    }
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl std::error::Error for ResourceMapConflict {}

#[cfg(all(
    any(feature = "shell-oracle", test),
    not(any(target_os = "android", target_env = "ohos"))
))]
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

#[cfg(all(
    any(feature = "shell-oracle", test),
    not(any(target_os = "android", target_env = "ohos"))
))]
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

#[cfg(all(
    any(feature = "shell-oracle", test),
    not(any(target_os = "android", target_env = "ohos"))
))]
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

#[cfg(all(
    any(feature = "shell-oracle", test),
    not(any(target_os = "android", target_env = "ohos"))
))]
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
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

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
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

#[cfg(all(
    feature = "shell-oracle",
    not(any(target_os = "android", target_env = "ohos"))
))]
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
fn scene_capture_code(capture: &SceneCapture) -> Option<&'static str> {
    if !capture.text_mapping_gaps.is_empty() {
        Some("SCENE_CAPTURE_LIMITATIONS")
    } else if !capture.unsupported_events.is_empty() {
        Some("SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS")
    } else {
        None
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
const fn unsupported_paint_kind_name(kind: UnsupportedPaintKind) -> &'static str {
    match kind {
        UnsupportedPaintKind::Box => "box",
        UnsupportedPaintKind::RootBackground => "root-background",
        UnsupportedPaintKind::Outline => "outline",
        UnsupportedPaintKind::CollapsedTableBorders => "collapsed-table-borders",
        UnsupportedPaintKind::Iframe => "iframe",
        UnsupportedPaintKind::TextEffects => "text-effects",
        UnsupportedPaintKind::ContentGeometry => "content-geometry",
        UnsupportedPaintKind::SvgAnimation => "svg-animation",
        UnsupportedPaintKind::SvgCompositing => "svg-compositing",
        UnsupportedPaintKind::SvgStroke => "svg-stroke",
        UnsupportedPaintKind::SvgPaint => "svg-paint",
        UnsupportedPaintKind::SvgImage => "svg-image",
        UnsupportedPaintKind::SvgText => "svg-text",
        UnsupportedPaintKind::SvgInvalidPath => "svg-invalid-path",
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn rejected_scene_capture(capture: &SceneCapture) -> Option<SceneArtifactError> {
    let code = scene_capture_code(capture)?;
    let mut limitations = Vec::new();
    if !capture.unsupported_events.is_empty() {
        let mut kinds = Vec::new();
        for event in &capture.unsupported_events {
            let kind = unsupported_paint_kind_name(event.kind);
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        limitations.push(format!("unsupported paint kinds: {}", kinds.join(", ")));
    }
    if !capture.text_mapping_gaps.is_empty() {
        limitations.push(format!(
            "text mapping gaps: {}",
            capture.text_mapping_gaps.len()
        ));
    }
    Some(SceneArtifactError::new(
        code,
        format!(
            "document scene capture is incomplete ({}); rerun with --allow-partial-scene to inspect scene diagnostics",
            limitations.join("; ")
        ),
    ))
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
                        artifacts
                            .public_directory()
                            .join("resources")
                            .join(digest)
                            .display()
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

    let capture_code = scene_capture_code(capture);
    let capture_status = if capture_code.is_none() {
        "complete"
    } else {
        "partial"
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
                    format!(
                        "cannot write {}: {error}",
                        artifacts.public_directory().join("document.pdf").display()
                    ),
                )
            })?;
            pdf_written = true;

            let structure = document_pdf_structure(&capture.scene, &pdf);
            artifacts.write_pdf_structure(&structure).map_err(|error| {
                SceneArtifactError::new(
                    "DOCUMENT_PDF_STRUCTURE_WRITE_FAILED",
                    format!(
                        "cannot write {}: {error}",
                        artifacts
                            .public_directory()
                            .join("pdf-structure.json")
                            .display()
                    ),
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
            "artifact": artifacts.public_directory().join("document.pdf").to_string_lossy(),
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
            "artifact": artifacts.public_directory().join("pdf-structure.json").to_string_lossy(),
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
        artifacts.public_directory(),
        document_pdf,
        &artifacts.render_id(),
        code,
        message,
    );
    error.warnings = warnings;
    error
}

#[cfg(all(
    feature = "document-session",
    any(target_os = "android", target_env = "ohos")
))]
fn render(_request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    Err(RenderError::request(
        "UNSUPPORTED_TARGET",
        "the command-line renderer is only available on desktop targets",
    ))
}

#[cfg(all(
    feature = "shell-oracle",
    any(target_os = "android", target_env = "ohos")
))]
fn render_with_shell_oracle(_request: RenderRequest) -> Result<RenderOutcome, RenderError> {
    Err(RenderError::request(
        "UNSUPPORTED_TARGET",
        "the command-line renderer is only available on desktop targets",
    ))
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "shell-oracle")]
    use std::cell::Cell;
    #[cfg(feature = "shell-oracle")]
    use std::collections::{BTreeMap, HashMap};
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
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
    use sha2::{Digest as _, Sha256};

    #[cfg(all(
        feature = "document-session",
        feature = "shell-oracle",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    use super::begin_publication_for_runtime;
    #[cfg(not(any(target_os = "android", target_env = "ohos")))]
    use super::controlled_capture_render_id;
    #[cfg(feature = "document-session")]
    use super::{
        CapturedPublication, ExpectedInputIdentity, RenderOutcome, finish_document_session_render,
        publish_captured_document,
    };
    use super::{
        Command, DEFAULT_LOCALE, DEFAULT_TIMEZONE, DeterministicRuntimePolicy, ExplicitRenderPaths,
        PageDefinition, PageMargins, RenderEnvironment, RenderError, RenderRequest,
        ResourceCapture, ResourcePolicy, ResourcePolicyConfig, ResourcePolicyFailure,
        ResourceRequest, WebResourceLoadRole, classify_controlled_http_status, cli_render_error,
        cli_render_stderr, create_session_artifacts, default_page, emit_api2_result, lowercase_hex,
        page_artifact, parse_args, persist_scene_capture, print_render_error,
        resolve_scene_resource, runtime_policy, set_document_pdf_environment, sha256_hex,
        stable_render_id, update_hash_field,
    };
    #[cfg(feature = "shell-oracle")]
    use super::{
        PendingResource, ResourcePolicyDecision, SERVO_BUILD_VERSION, complete_resource,
        decide_resource_policy, first_fatal_policy_failure, incomplete_resource_failure,
        retain_controlled_resource,
    };
    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    use super::{
        PublicationRuntimeIdentity, PublicationStart, PublicationTransaction, begin_publication,
        publication_recovery_required_message, publication_request_fingerprint,
        publication_request_fingerprint_with_runtime_policy, render,
        render_controlled_with_post_reservation_paint_invalidation_for_test,
        stable_render_id_with_runtime_policy, supervisor_render_identity, write_render_outcome,
    };
    #[cfg(feature = "document-session")]
    use crate::document_session::{DocumentCaptureOutcome, SessionError};
    #[cfg(feature = "document-session")]
    use crate::owned_resource_store::OwnedResourceStore;
    #[cfg(any(feature = "document-session", feature = "shell-oracle"))]
    use crate::resource_policy::ControlledResource;
    #[cfg(feature = "document-session")]
    use crate::resource_policy::{ResourceAccounting, ResourceEvidence, ResourceSource};
    use crate::session::SessionArtifacts;
    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    use crate::session::{LocalDocument, PublicationRecoveryState};

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

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    fn expect_new_publication(start: PublicationStart) -> PublicationTransaction {
        match start {
            PublicationStart::New(publication) => publication,
            PublicationStart::Recovered(_) => panic!("fixture unexpectedly recovered publication"),
        }
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    fn recovery_process_request(root: &std::path::Path) -> RenderRequest {
        RenderRequest {
            input: PathBuf::from("input.html"),
            environment: RenderEnvironment::default(),
            page: default_page(),
            resources: ResourcePolicyConfig::default(),
            runtime_policy: DeterministicRuntimePolicy::default(),
            allow_host_fonts: false,
            allow_partial_scene: false,
            explicit_paths: Some(ExplicitRenderPaths {
                output: root.join("output.pdf"),
                artifacts: root.join("artifacts"),
            }),
        }
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn publication_recovery_error_uses_stable_non_sensitive_state_labels() {
        assert_eq!(
            publication_recovery_required_message(&PublicationRecoveryState::Planned),
            "publication transaction requires recovery before rendering: planned"
        );

        let sensitive_path = "/private/customer/invoice.pdf";
        let sensitive_bytes = "sealed customer outcome";
        let state = PublicationRecoveryState::Committed {
            summary: serde_json::json!({ "document_pdf": sensitive_path }),
            cli_bytes: sensitive_bytes.as_bytes().to_vec(),
            recovered: true,
        };
        let message = publication_recovery_required_message(&state);
        assert_eq!(
            message,
            "publication transaction requires recovery before rendering: committed"
        );
        assert!(!message.contains(sensitive_path));
        assert!(!message.contains(sensitive_bytes));
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    fn snapshot_test_tree(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        fn visit(
            root: &std::path::Path,
            directory: &std::path::Path,
            snapshot: &mut Vec<(String, Vec<u8>)>,
        ) {
            let mut entries: Vec<_> = fs::read_dir(directory)
                .unwrap()
                .map(Result::unwrap)
                .collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let metadata = fs::symlink_metadata(&path).unwrap();
                if metadata.is_dir() {
                    snapshot.push((format!("{relative}/"), Vec::new()));
                    visit(root, &path, snapshot);
                } else {
                    snapshot.push((relative, fs::read(path).unwrap()));
                }
            }
        }

        let mut snapshot = Vec::new();
        visit(root, root, &mut snapshot);
        snapshot
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    struct PreservedRecoveryTransaction {
        artifact_path: PathBuf,
        staging: PathBuf,
        staging_before: Vec<u8>,
        document_pdf_path: PathBuf,
        transaction_before: Vec<(String, Vec<u8>)>,
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    fn write_equivalent_asset_manifest(directory: &std::path::Path) -> PathBuf {
        let asset = b"body { color: #123; }";
        fs::create_dir(directory).unwrap();
        fs::write(directory.join("asset.css"), asset).unwrap();
        let manifest = directory.join("assets.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": "pliego.asset-manifest",
                "version": 1,
                "assets": [{
                    "url": "https://assets.test/shared.css",
                    "path": "asset.css",
                    "sha256": sha256_hex(asset),
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        manifest.canonicalize().unwrap()
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    fn recovery_request_with_manifest(
        root: &std::path::Path,
        manifest: &std::path::Path,
    ) -> RenderRequest {
        let mut request = recovery_process_request(root);
        request.resources.asset_manifest = Some(manifest.to_owned());
        request
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    fn preserve_recovery_transaction(
        request: &RenderRequest,
        resource_policy: &ResourcePolicy,
        render_id: &str,
        document: &LocalDocument,
    ) -> PreservedRecoveryTransaction {
        let PublicationTransaction {
            artifacts,
            journal,
            document_pdf_path,
            ..
        } = expect_new_publication(
            begin_publication(request, resource_policy, render_id, document.path()).unwrap(),
        );
        let journal = journal.expect("direct publication must own its journal");
        artifacts
            .write_document_pdf(b"%PDF-manifest-identity")
            .unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&document_pdf_path).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let outcome = RenderOutcome::from_summary(serde_json::json!({
            "environment": {
                "resource_policy": resource_policy.artifact(render_id),
            },
            "input": request.input.to_string_lossy(),
            "render_id": render_id,
            "status": "rendered",
        }))
        .unwrap();
        journal
            .record_prepared(&prepared, &bundle, &outcome.cli_bytes)
            .unwrap();
        let prepared_receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(artifacts.directory().join("publication/prepared.json")).unwrap(),
        )
        .unwrap();
        let staging = PathBuf::from(prepared_receipt["staging"]["path"].as_str().unwrap());
        prepared.preserve_for_recovery();
        bundle.preserve();
        let artifact_path = artifacts.directory().to_owned();
        drop(journal);
        drop(artifacts);
        assert!(!document_pdf_path.exists());
        assert!(staging.exists());

        PreservedRecoveryTransaction {
            transaction_before: snapshot_test_tree(&artifact_path),
            staging_before: fs::read(&staging).unwrap(),
            artifact_path,
            staging,
            document_pdf_path,
        }
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    fn assert_recovery_identity_rejected_without_mutation(
        fixture: &PreservedRecoveryTransaction,
        request: &RenderRequest,
        resource_policy: &ResourcePolicy,
        render_id: &str,
        document: &LocalDocument,
    ) {
        let error = match begin_publication(request, resource_policy, render_id, document.path()) {
            Ok(_) => panic!("different summary identity must not resume a transaction"),
            Err(error) => error,
        };
        assert_eq!(error.code, "PUBLICATION_RECOVERY_FAILED");
        assert_eq!(
            snapshot_test_tree(&fixture.artifact_path),
            fixture.transaction_before
        );
        assert_eq!(fs::read(&fixture.staging).unwrap(), fixture.staging_before);
        assert!(!fixture.document_pdf_path.exists());
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn recovery_rejects_equivalent_assets_from_a_different_manifest_without_mutation() {
        let root = temporary_artifacts("pliego-manifest-recovery-identity");
        fs::create_dir(&root).unwrap();
        let input = b"<!doctype html><title>Manifest recovery identity</title>";
        fs::write(root.join("input.html"), input).unwrap();
        let first_manifest = write_equivalent_asset_manifest(&root.join("first-assets"));
        let second_manifest = write_equivalent_asset_manifest(&root.join("second-assets"));
        assert_ne!(first_manifest, second_manifest);
        assert_eq!(
            fs::read(&first_manifest).unwrap(),
            fs::read(&second_manifest).unwrap()
        );

        let document = LocalDocument::resolve(&root, "input.html").unwrap();
        let first_request = recovery_request_with_manifest(&root, &first_manifest);
        let second_request = recovery_request_with_manifest(&root, &second_manifest);
        let first_policy = ResourcePolicy::resolve(&first_request.resources, document.root());
        let second_policy = ResourcePolicy::resolve(&second_request.resources, document.root());
        assert_eq!(
            first_policy.summary_asset_manifest_path(),
            Some(first_manifest.as_path())
        );
        assert_eq!(
            second_policy.summary_asset_manifest_path(),
            Some(second_manifest.as_path())
        );
        let first_render_id = stable_render_id(
            input,
            first_request.environment,
            first_request.page,
            &first_policy,
            first_request.allow_host_fonts,
        );
        let second_render_id = stable_render_id(
            input,
            second_request.environment,
            second_request.page,
            &second_policy,
            second_request.allow_host_fonts,
        );
        assert_eq!(first_render_id, second_render_id);
        assert_ne!(
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &first_render_id,
                first_request.allow_partial_scene,
                &first_request.input,
                document.path(),
                first_policy.summary_asset_manifest_path(),
            ),
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &second_render_id,
                second_request.allow_partial_scene,
                &second_request.input,
                document.path(),
                second_policy.summary_asset_manifest_path(),
            )
        );

        let fixture = preserve_recovery_transaction(
            &first_request,
            &first_policy,
            &first_render_id,
            &document,
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(fixture.artifact_path.join("publication/outcome.json")).unwrap()
            )
            .unwrap()["environment"]["resource_policy"]["asset_manifest"]["manifest"],
            serde_json::json!(first_manifest)
        );
        assert_recovery_identity_rejected_without_mutation(
            &fixture,
            &second_request,
            &second_policy,
            &second_render_id,
            &document,
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(all(
        feature = "document-session",
        unix,
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn recovery_identity_uses_the_manifest_captured_before_symlink_retarget() {
        use std::os::unix::fs::symlink;

        let root = temporary_artifacts("pliego-manifest-symlink-recovery-identity");
        fs::create_dir(&root).unwrap();
        let input = b"<!doctype html><title>Manifest symlink identity</title>";
        fs::write(root.join("input.html"), input).unwrap();
        let first_manifest = write_equivalent_asset_manifest(&root.join("first-assets"));
        let second_manifest = write_equivalent_asset_manifest(&root.join("second-assets"));
        let requested_manifest = root.join("current-assets.json");
        symlink(&first_manifest, &requested_manifest).unwrap();

        let document = LocalDocument::resolve(&root, "input.html").unwrap();
        let first_request = recovery_request_with_manifest(&root, &requested_manifest);
        let first_policy = ResourcePolicy::resolve(&first_request.resources, document.root());
        assert_eq!(
            first_policy.summary_asset_manifest_path(),
            Some(first_manifest.as_path())
        );

        fs::remove_file(&requested_manifest).unwrap();
        symlink(&second_manifest, &requested_manifest).unwrap();
        let second_request = recovery_request_with_manifest(&root, &requested_manifest);
        let second_policy = ResourcePolicy::resolve(&second_request.resources, document.root());
        assert_eq!(first_request.resources, second_request.resources);
        assert_eq!(
            second_policy.summary_asset_manifest_path(),
            Some(second_manifest.as_path())
        );

        let first_render_id = stable_render_id(
            input,
            first_request.environment,
            first_request.page,
            &first_policy,
            first_request.allow_host_fonts,
        );
        let second_render_id = stable_render_id(
            input,
            second_request.environment,
            second_request.page,
            &second_policy,
            second_request.allow_host_fonts,
        );
        assert_eq!(first_render_id, second_render_id);
        assert_ne!(
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &first_render_id,
                first_request.allow_partial_scene,
                &first_request.input,
                document.path(),
                first_policy.summary_asset_manifest_path(),
            ),
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &second_render_id,
                second_request.allow_partial_scene,
                &second_request.input,
                document.path(),
                second_policy.summary_asset_manifest_path(),
            )
        );

        let fixture = preserve_recovery_transaction(
            &first_request,
            &first_policy,
            &first_render_id,
            &document,
        );
        assert_recovery_identity_rejected_without_mutation(
            &fixture,
            &second_request,
            &second_policy,
            &second_render_id,
            &document,
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn fresh_process_cli_path_recovers_prepared_transaction_with_exact_stdout() {
        const CHILD_MARKER: &str = "PLIEGO_TEST_PUBLICATION_RECOVERY_CHILD";
        const ROOT_ENV: &str = "PLIEGO_TEST_PUBLICATION_RECOVERY_ROOT";
        const CAPTURE_ENV: &str = "PLIEGO_TEST_PUBLICATION_RECOVERY_CAPTURE";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("child root is set"));
            let capture =
                PathBuf::from(std::env::var_os(CAPTURE_ENV).expect("child capture is set"));
            let outcome = render(recovery_process_request(&root)).unwrap();
            let sealed_cli_bytes =
                fs::read(root.join("artifacts/publication/outcome.json")).unwrap();
            assert_eq!(
                outcome.cli_bytes, sealed_cli_bytes,
                "fresh process entered rendering instead of returning the sealed recovery outcome"
            );
            let mut capture_file = std::fs::File::create(capture).unwrap();
            write_render_outcome(&mut capture_file, &outcome).unwrap();
            return;
        }

        let root = temporary_artifacts("pliego-fresh-process-recovery");
        fs::create_dir(&root).unwrap();
        let input = b"<!doctype html><title>Prepared recovery</title>";
        fs::write(root.join("input.html"), input).unwrap();
        let request = recovery_process_request(&root);
        let document = LocalDocument::resolve(&root, "input.html").unwrap();
        let resource_policy = ResourcePolicy::resolve(&request.resources, document.root());
        let render_id = stable_render_id(
            input,
            request.environment,
            request.page,
            &resource_policy,
            request.allow_host_fonts,
        );
        let PublicationTransaction {
            artifacts,
            journal,
            document_pdf_path,
            ..
        } = expect_new_publication(
            begin_publication(&request, &resource_policy, &render_id, document.path()).unwrap(),
        );
        let journal = journal.expect("direct publication must own its journal");
        artifacts.write_document_pdf(b"%PDF-fresh-process").unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let prepared = artifacts.prepare_document_pdf(&document_pdf_path).unwrap();
        let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
        let summary = serde_json::json!({
            "artifacts": artifacts.directory().to_string_lossy(),
            "bundle": bundle.path().to_string_lossy(),
            "document_pdf": document_pdf_path.to_string_lossy(),
            "input": request.input.to_string_lossy(),
            "render_id": render_id,
            "status": "rendered",
        });
        let original_outcome = RenderOutcome::from_summary(summary.clone()).unwrap();
        let mut expected_stdout = Vec::new();
        write_render_outcome(&mut expected_stdout, &original_outcome).unwrap();
        journal
            .record_prepared(&prepared, &bundle, &original_outcome.cli_bytes)
            .unwrap();
        let prepared_receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(artifacts.directory().join("publication/prepared.json")).unwrap(),
        )
        .unwrap();
        let staging = PathBuf::from(prepared_receipt["staging"]["path"].as_str().unwrap());
        prepared.preserve_for_recovery();
        bundle.preserve();
        let artifact_path = artifacts.directory().to_owned();
        drop(journal);
        drop(artifacts);
        assert!(!document_pdf_path.exists());
        assert!(staging.exists());

        let transaction_before = snapshot_test_tree(&artifact_path);
        let staging_before = fs::read(&staging).unwrap();
        let mut partial_request = recovery_process_request(&root);
        partial_request.allow_partial_scene = true;
        let partial_error = match begin_publication(
            &partial_request,
            &resource_policy,
            &render_id,
            document.path(),
        ) {
            Ok(_) => panic!("changed partial-scene policy must not resume a transaction"),
            Err(error) => error,
        };
        assert_eq!(partial_error.code, "PUBLICATION_RECOVERY_FAILED");
        assert_eq!(snapshot_test_tree(&artifact_path), transaction_before);
        assert_eq!(fs::read(&staging).unwrap(), staging_before);
        assert!(!document_pdf_path.exists());

        let mut other_spelling_request = recovery_process_request(&root);
        other_spelling_request.input = PathBuf::from("./input.html");
        let same_document = LocalDocument::resolve(&root, "./input.html").unwrap();
        assert_eq!(same_document.path(), document.path());
        let same_render_id = stable_render_id(
            input,
            other_spelling_request.environment,
            other_spelling_request.page,
            &resource_policy,
            other_spelling_request.allow_host_fonts,
        );
        assert_eq!(same_render_id, render_id);
        assert_ne!(
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &render_id,
                request.allow_partial_scene,
                &request.input,
                document.path(),
                resource_policy.summary_asset_manifest_path(),
            ),
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &same_render_id,
                other_spelling_request.allow_partial_scene,
                &other_spelling_request.input,
                same_document.path(),
                resource_policy.summary_asset_manifest_path(),
            )
        );
        let other_spelling_error = match begin_publication(
            &other_spelling_request,
            &resource_policy,
            &same_render_id,
            same_document.path(),
        ) {
            Ok(_) => panic!("different requested input spelling must not resume a transaction"),
            Err(error) => error,
        };
        assert_eq!(other_spelling_error.code, "PUBLICATION_RECOVERY_FAILED");
        assert_eq!(snapshot_test_tree(&artifact_path), transaction_before);
        assert_eq!(fs::read(&staging).unwrap(), staging_before);
        assert!(!document_pdf_path.exists());

        fs::write(root.join("same-bytes-copy.html"), input).unwrap();
        let mut other_input_request = recovery_process_request(&root);
        other_input_request.input = PathBuf::from("same-bytes-copy.html");
        let other_document = LocalDocument::resolve(&root, "same-bytes-copy.html").unwrap();
        let other_policy =
            ResourcePolicy::resolve(&other_input_request.resources, other_document.root());
        let other_render_id = stable_render_id(
            input,
            other_input_request.environment,
            other_input_request.page,
            &other_policy,
            other_input_request.allow_host_fonts,
        );
        assert_eq!(other_render_id, render_id);
        assert_ne!(
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &render_id,
                request.allow_partial_scene,
                &request.input,
                document.path(),
                resource_policy.summary_asset_manifest_path(),
            ),
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                &other_render_id,
                other_input_request.allow_partial_scene,
                &other_input_request.input,
                other_document.path(),
                other_policy.summary_asset_manifest_path(),
            )
        );
        let other_input_error = match begin_publication(
            &other_input_request,
            &other_policy,
            &other_render_id,
            other_document.path(),
        ) {
            Ok(_) => panic!("different canonical input must not resume a transaction"),
            Err(error) => error,
        };
        assert_eq!(other_input_error.code, "PUBLICATION_RECOVERY_FAILED");
        assert_eq!(snapshot_test_tree(&artifact_path), transaction_before);
        assert_eq!(fs::read(&staging).unwrap(), staging_before);
        assert!(!document_pdf_path.exists());

        let test_executable = std::env::current_exe().unwrap();
        let test_name =
            "tests::fresh_process_cli_path_recovers_prepared_transaction_with_exact_stdout";
        let first_capture = root.join("recovered-stdout-1.json");
        let first = std::process::Command::new(&test_executable)
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .current_dir(&root)
            .env(CHILD_MARKER, "1")
            .env(ROOT_ENV, &root)
            .env(CAPTURE_ENV, &first_capture)
            .status()
            .unwrap();
        assert!(first.success());
        assert_eq!(fs::read(&first_capture).unwrap(), expected_stdout);
        assert_eq!(
            fs::read(artifact_path.join("publication/outcome.json")).unwrap(),
            expected_stdout
        );
        assert_eq!(fs::read(&document_pdf_path).unwrap(), b"%PDF-fresh-process");
        assert!(!staging.exists());
        let committed_path = artifact_path.join("publication/committed.json");
        let committed_bytes = fs::read(&committed_path).unwrap();

        let second_capture = root.join("recovered-stdout-2.json");
        let second = std::process::Command::new(&test_executable)
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .current_dir(&root)
            .env(CHILD_MARKER, "1")
            .env(ROOT_ENV, &root)
            .env(CAPTURE_ENV, &second_capture)
            .status()
            .unwrap();
        assert!(second.success());
        assert_eq!(fs::read(&second_capture).unwrap(), expected_stdout);
        assert_eq!(fs::read(&committed_path).unwrap(), committed_bytes);

        fs::remove_file(document_pdf_path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(all(
        feature = "document-session",
        feature = "shell-oracle",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn cross_runtime_publication_recovery_is_rejected_without_mutation() {
        fn begin_for_runtime(
            runtime: PublicationRuntimeIdentity,
            request: &RenderRequest,
            resource_policy: &ResourcePolicy,
            render_id: &str,
            resolved_input: &std::path::Path,
        ) -> Result<PublicationStart, RenderError> {
            begin_publication_for_runtime(
                request,
                resource_policy,
                render_id,
                resolved_input,
                runtime,
            )
        }

        for (source_runtime, recovery_runtime, label) in [
            (
                PublicationRuntimeIdentity::DocumentSession,
                PublicationRuntimeIdentity::ServoshellOracle,
                "oracle-after-direct",
            ),
            (
                PublicationRuntimeIdentity::ServoshellOracle,
                PublicationRuntimeIdentity::DocumentSession,
                "direct-after-oracle",
            ),
            (
                PublicationRuntimeIdentity::DocumentSession,
                PublicationRuntimeIdentity::DocumentSessionControlledCapture,
                "controlled-after-realtime",
            ),
            (
                PublicationRuntimeIdentity::DocumentSessionControlledCapture,
                PublicationRuntimeIdentity::DocumentSession,
                "realtime-after-controlled",
            ),
            (
                PublicationRuntimeIdentity::ServoshellOracle,
                PublicationRuntimeIdentity::DocumentSessionControlledCapture,
                "controlled-after-oracle",
            ),
            (
                PublicationRuntimeIdentity::DocumentSessionControlledCapture,
                PublicationRuntimeIdentity::ServoshellOracle,
                "oracle-after-controlled",
            ),
        ] {
            let root = temporary_artifacts(&format!("pliego-cross-runtime-{label}"));
            fs::create_dir(&root).unwrap();
            let input = b"<!doctype html><title>Cross runtime recovery</title>";
            fs::write(root.join("input.html"), input).unwrap();
            let request = recovery_process_request(&root);
            let document = LocalDocument::resolve(&root, "input.html").unwrap();
            let resource_policy = ResourcePolicy::resolve(&request.resources, document.root());
            let render_id = stable_render_id(
                input,
                request.environment,
                request.page,
                &resource_policy,
                request.allow_host_fonts,
            );
            assert_ne!(
                publication_request_fingerprint(
                    source_runtime,
                    &render_id,
                    request.allow_partial_scene,
                    &request.input,
                    document.path(),
                    resource_policy.summary_asset_manifest_path(),
                ),
                publication_request_fingerprint(
                    recovery_runtime,
                    &render_id,
                    request.allow_partial_scene,
                    &request.input,
                    document.path(),
                    resource_policy.summary_asset_manifest_path(),
                ),
            );

            let PublicationTransaction {
                artifacts,
                journal,
                document_pdf_path,
                ..
            } = expect_new_publication(
                begin_for_runtime(
                    source_runtime,
                    &request,
                    &resource_policy,
                    &render_id,
                    document.path(),
                )
                .unwrap(),
            );
            let journal = journal.expect("direct publication must own its journal");
            let pdf_bytes = format!("%PDF-cross-runtime-{label}").into_bytes();
            artifacts.write_document_pdf(&pdf_bytes).unwrap();
            artifacts.record_state("rendered", None).unwrap();
            let prepared = artifacts.prepare_document_pdf(&document_pdf_path).unwrap();
            let bundle = artifacts.write_prepared_bundle(&prepared).unwrap();
            let outcome = RenderOutcome::from_summary(serde_json::json!({
                "artifacts": artifacts.directory().to_string_lossy(),
                "bundle": bundle.path().to_string_lossy(),
                "document_pdf": document_pdf_path.to_string_lossy(),
                "input": request.input.to_string_lossy(),
                "render_id": render_id,
                "status": "rendered",
            }))
            .unwrap();
            journal
                .record_prepared(&prepared, &bundle, &outcome.cli_bytes)
                .unwrap();
            let artifact_root = artifacts.directory().to_owned();
            let publication_root = artifact_root.join("publication");
            let planned_path = publication_root.join("plan.json");
            let prepared_path = publication_root.join("prepared.json");
            let prepared_receipt: serde_json::Value =
                serde_json::from_slice(&fs::read(&prepared_path).unwrap()).unwrap();
            let staging = PathBuf::from(prepared_receipt["staging"]["path"].as_str().unwrap());
            prepared.preserve_for_recovery();
            bundle.preserve();
            drop(journal);
            drop(artifacts);

            let artifact_tree_before = snapshot_test_tree(&artifact_root);
            let staging_before = fs::read(&staging).unwrap();
            let output_before = document_pdf_path
                .try_exists()
                .unwrap()
                .then(|| fs::read(&document_pdf_path).unwrap());
            let planned_before = fs::read(&planned_path).unwrap();
            let prepared_before = fs::read(&prepared_path).unwrap();

            let error = match begin_for_runtime(
                recovery_runtime,
                &request,
                &resource_policy,
                &render_id,
                document.path(),
            ) {
                Ok(_) => panic!("{label} must not recover a transaction from another runtime"),
                Err(error) => error,
            };
            assert_eq!(error.code, "PUBLICATION_RECOVERY_FAILED");
            assert_eq!(snapshot_test_tree(&artifact_root), artifact_tree_before);
            assert_eq!(fs::read(&staging).unwrap(), staging_before);
            assert_eq!(
                document_pdf_path
                    .try_exists()
                    .unwrap()
                    .then(|| fs::read(&document_pdf_path).unwrap()),
                output_before,
            );
            assert_eq!(fs::read(&planned_path).unwrap(), planned_before);
            assert_eq!(fs::read(&prepared_path).unwrap(), prepared_before);

            fs::remove_dir_all(root).unwrap();
        }
    }

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
                runtime_policy: DeterministicRuntimePolicy::default(),
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
    fn api2_selectors_with_extra_arguments_stay_in_api2_invocation_framing() {
        assert_eq!(
            parse_args(vec![OsString::from("--contract-probe")]).unwrap(),
            Command::ContractProbe
        );
        assert_eq!(
            parse_args(vec![OsString::from("render-api2")]).unwrap(),
            Command::RenderApi2
        );
        assert!(matches!(
            parse_args(vec![
                OsString::from("--contract-probe"),
                OsString::from("extra")
            ]),
            Ok(Command::Api2InvocationError(_))
        ));
        assert!(matches!(
            parse_args(vec![
                OsString::from("render-api2"),
                OsString::from("--anything")
            ]),
            Ok(Command::Api2InvocationError(_))
        ));
    }

    #[test]
    fn accepted_api2_stdout_failure_is_transport_not_invocation_framing() {
        struct BrokenPipe;

        impl std::io::Write for BrokenPipe {
            fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "closed result reader",
                ))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let error = emit_api2_result(&mut BrokenPipe, b"{}\n", true).unwrap_err();
        assert!(error.contains("accepted API 2 result frame"));
        assert_ne!(
            super::api2::TRANSPORT_ERROR_EXIT_CODE,
            super::api2::INVOCATION_ERROR_EXIT_CODE
        );
        assert_eq!(super::api2::TRANSPORT_ERROR_EXIT_CODE, 74);
    }

    #[test]
    fn accepted_api2_stdout_flush_failure_is_transport_not_invocation_framing() {
        struct FlushFailure {
            written: Vec<u8>,
        }

        impl std::io::Write for FlushFailure {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                self.written.extend_from_slice(buffer);
                Ok(buffer.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "closed result reader while flushing",
                ))
            }
        }

        let mut writer = FlushFailure {
            written: Vec::new(),
        };
        let error = emit_api2_result(&mut writer, b"{}\n", true).unwrap_err();
        assert_eq!(writer.written, b"{}\n");
        assert!(error.contains("cannot flush the accepted API 2 result frame"));
        assert_ne!(
            super::api2::TRANSPORT_ERROR_EXIT_CODE,
            super::api2::INVOCATION_ERROR_EXIT_CODE
        );
    }

    #[test]
    fn runtime_selection_is_explicit_and_direct_wins_combined_builds() {
        #[cfg(feature = "document-session")]
        assert_eq!(super::active_runtime_name(), "document-session");
        #[cfg(all(not(feature = "document-session"), feature = "shell-oracle"))]
        assert_eq!(super::active_runtime_name(), "servoshell-oracle");
    }

    #[cfg(all(
        target_os = "windows",
        target_env = "msvc",
        feature = "document-session"
    ))]
    #[test]
    fn direct_windows_binary_owns_surfman_gpu_selection_symbols() {
        let nvidia = unsafe { std::ptr::addr_of!(super::NvOptimusEnablement).read_volatile() };
        let amd = unsafe {
            std::ptr::addr_of!(super::AmdPowerXpressRequestHighPerformance).read_volatile()
        };
        assert_eq!(nvidia, 1);
        assert_eq!(amd, 1);
    }

    #[cfg(feature = "shell-oracle")]
    #[test]
    fn pliego_owned_servo_build_identity_matches_the_shell_oracle() {
        assert_eq!(SERVO_BUILD_VERSION, servoshell::VERSION);
        assert!(SERVO_BUILD_VERSION.starts_with(concat!(
            "Servo ",
            env!("PLIEGO_SERVO_VERSION"),
            "-"
        )));
        assert!(!SERVO_BUILD_VERSION.ends_with("-nogit"));
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
    fn render_failure_emitter_returns_the_requested_exit_code() {
        let error = RenderError::without_publication(
            "RESOURCE_DENIED",
            "controlled resource was denied",
            1,
        );

        assert_eq!(print_render_error(&error), std::process::ExitCode::from(1));
    }

    #[test]
    fn render_failure_stderr_orders_warnings_before_the_terminal_line() {
        let mut error = RenderError::session(
            Path::new("artifacts"),
            Path::new("document.pdf"),
            &format!("sha256:{}", "1".repeat(64)),
            "PUBLICATION_FAILED",
            "publication failed",
        );
        error.warnings = vec!["first warning".into(), "second warning".into()];
        let output = cli_render_error(&error);

        assert_eq!(
            cli_render_stderr(&error, &output.stderr),
            "pliego: warning: first warning\npliego: warning: second warning\npliego: PUBLICATION_FAILED: publication failed"
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
                resources: ResourcePolicyConfig::default(),
                runtime_policy: DeterministicRuntimePolicy::default(),
                allow_host_fonts: false,
                allow_partial_scene: false,
                explicit_paths: Some(ExplicitRenderPaths {
                    output: PathBuf::from("requested/invoice.pdf"),
                    artifacts: PathBuf::from("diagnostics/render-1"),
                }),
            })
        );

        let Command::RenderControlled(controlled) = parse_args(vec![
            OsString::from("render-controlled"),
            OsString::from("invoice.html"),
            OsString::from("--output"),
            OsString::from("requested/controlled.pdf"),
            OsString::from("--artifacts"),
            OsString::from("diagnostics/controlled"),
        ])
        .unwrap() else {
            panic!("render-controlled did not select the controlled production route")
        };
        assert_eq!(controlled.input, PathBuf::from("invoice.html"));
        assert_eq!(
            controlled.runtime_policy,
            DeterministicRuntimePolicy::default()
        );
        assert_eq!(
            controlled.explicit_paths,
            Some(ExplicitRenderPaths {
                output: PathBuf::from("requested/controlled.pdf"),
                artifacts: PathBuf::from("diagnostics/controlled"),
            })
        );
        let controlled_missing_artifacts = parse_args(vec![
            OsString::from("render-controlled"),
            OsString::from("invoice.html"),
            OsString::from("--output"),
            OsString::from("controlled.pdf"),
        ])
        .unwrap_err();
        assert_eq!(
            controlled_missing_artifacts,
            "`pliego render-controlled` requires --artifacts"
        );
        let controlled_partial = parse_args(vec![
            OsString::from("render-controlled"),
            OsString::from("invoice.html"),
            OsString::from("--output"),
            OsString::from("controlled.pdf"),
            OsString::from("--artifacts"),
            OsString::from("diagnostics/controlled"),
            OsString::from("--allow-partial-scene"),
        ])
        .unwrap_err();
        assert_eq!(
            controlled_partial,
            "`pliego render-controlled` does not permit --allow-partial-scene"
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

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn controlled_paint_invalidation_reaches_the_real_failure_publication() {
        const ISOLATED_TEST: &str =
            "tests::isolated_controlled_paint_invalidation_reaches_the_real_failure_publication";
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", ISOLATED_TEST, "--ignored", "--nocapture"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "isolated controlled publication failed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            stdout.contains("controlled-publication-paint-invalidation-rejected"),
            "isolated controlled publication did not reach the typed failure path:\n{stdout}",
        );
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    #[ignore = "launched in a fresh process by the controlled publication test"]
    fn isolated_controlled_paint_invalidation_reaches_the_real_failure_publication() {
        let root = temporary_artifacts("pliego-controlled-paint-publication");
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("input.html"),
            "<!doctype html><p id='state'>pending</p><script>window.pliego.defer();requestAnimationFrame(()=>{});setTimeout(()=>{document.getElementById('state').textContent='done';window.pliego.ready({fixture:'controlled-paint-publication'});},5);</script>",
        )
        .unwrap();
        let output = root.join("requested.pdf");
        let artifacts = root.join("artifacts");
        let request = RenderRequest {
            input: PathBuf::from("input.html"),
            environment: RenderEnvironment::default(),
            page: default_page(),
            resources: ResourcePolicyConfig::default(),
            runtime_policy: DeterministicRuntimePolicy::default(),
            allow_host_fonts: false,
            allow_partial_scene: false,
            explicit_paths: Some(ExplicitRenderPaths {
                output: output.clone(),
                artifacts: artifacts.clone(),
            }),
        };

        let original_directory = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        let result = render_controlled_with_post_reservation_paint_invalidation_for_test(request);
        std::env::set_current_dir(original_directory).unwrap();
        let error = result.unwrap_err();

        assert_eq!(error.code, "CONTROLLED_PAINT_FINALIZE_FAILED");
        assert!(
            error.message.contains("PresentationInvalidated"),
            "Paint mutation produced the wrong terminal reason: {}",
            error.message,
        );
        assert_eq!(error.document_pdf.as_deref(), Some(output.as_path()));
        assert_eq!(error.artifacts.as_deref(), Some(artifacts.as_path()));
        for path in [
            output.clone(),
            artifacts.join("bundle.json"),
            artifacts.join("document.pdf"),
            artifacts.join("render.png"),
            artifacts.join("layout-debug.json"),
            artifacts.join("fonts.json"),
            artifacts.join("pages.json"),
            artifacts.join("scene.json"),
            artifacts.join("scene-report.json"),
            artifacts.join("scene-preview.png"),
            artifacts.join("pdf-structure.json"),
            artifacts.join("pages"),
            artifacts.join("publication/outcome.json"),
            artifacts.join("publication/prepared.json"),
            artifacts.join("publication/committed.json"),
        ] {
            assert!(
                !path.exists(),
                "failed capture exposed success artifact {}",
                path.display(),
            );
        }
        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(artifacts.join("failure.json")).unwrap()).unwrap();
        assert_eq!(failure["status"], "failed");
        assert_eq!(failure["error"]["code"], "CONTROLLED_PAINT_FINALIZE_FAILED");
        assert!(
            failure["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("PresentationInvalidated"))
        );
        assert_eq!(failure["render_id"], error.render_id.unwrap());
        let readiness: serde_json::Value =
            serde_json::from_slice(&fs::read(artifacts.join("readiness.json")).unwrap()).unwrap();
        assert_eq!(readiness["status"], "ready");
        assert_eq!(
            readiness["payload"],
            serde_json::json!({"fixture": "controlled-paint-publication"})
        );
        println!("controlled-publication-paint-invalidation-rejected");

        fs::remove_dir_all(root).unwrap();
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
            "sha256:a89b2616c570d9ff69c7b12dd97721a6289a1a4f872873e50121565eb5606f04"
        );
        assert_ne!(
            render_id,
            stable_render_id(input, first.environment, first.page, &policy, true)
        );
    }

    #[cfg(not(any(target_os = "android", target_env = "ohos")))]
    #[test]
    fn controlled_capture_has_a_distinct_stable_render_identity() {
        let base = "sha256:7e771884747878e76c9e45b6fdb4ad5bf59b15ff33cfe0d9ef0db140fad2f52f";
        let controlled = controlled_capture_render_id(base);
        let mut v1_hasher = Sha256::new();
        update_hash_field(&mut v1_hasher, b"pliego.render-id.controlled-capture.v1");
        update_hash_field(&mut v1_hasher, base.as_bytes());
        let v1 = format!("sha256:{}", lowercase_hex(&v1_hasher.finalize()));

        assert_eq!(
            v1,
            "sha256:fe262370f31b605420bfc97bfdc76c344d5ff3d3b9e44bec0ad1c4bd5e026dfe"
        );
        assert_eq!(
            controlled,
            "sha256:fd4813cfdea8db625b9e3650361128f24b7aaf711e0b5ff7ec76d349a274fe02"
        );
        assert_eq!(controlled, controlled_capture_render_id(base));
        assert_ne!(controlled, base);
        assert!(controlled.starts_with("sha256:"));
        assert_eq!(controlled.len(), 71);
        assert_ne!(
            controlled,
            controlled_capture_render_id(
                "sha256:8e771884747878e76c9e45b6fdb4ad5bf59b15ff33cfe0d9ef0db140fad2f52f"
            )
        );
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn every_runtime_policy_override_changes_render_and_recovery_identity() {
        let input = b"<!doctype html><title>Controlled</title>";
        let resources = ResourcePolicy::default();
        let environment = RenderEnvironment::default();
        let page = default_page();
        let baseline_policy = DeterministicRuntimePolicy::default();
        let baseline_render_id = stable_render_id_with_runtime_policy(
            input,
            environment,
            page,
            &resources,
            false,
            baseline_policy,
        );
        let baseline_recovery_id = publication_request_fingerprint_with_runtime_policy(
            PublicationRuntimeIdentity::DocumentSession,
            &baseline_render_id,
            false,
            Path::new("invoice.html"),
            Path::new("/canonical/invoice.html"),
            None,
            baseline_policy,
        );

        let mut variants = Vec::new();
        let mut policy = baseline_policy;
        policy.time.epoch_unix_ms += 1;
        variants.push(policy);
        let mutate_limits: [fn(&mut runtime_policy::DocumentSettlementLimits); 8] = [
            |limits: &mut runtime_policy::DocumentSettlementLimits| limits.virtual_span_ms += 1,
            |limits: &mut runtime_policy::DocumentSettlementLimits| limits.ordinary_tasks += 1,
            |limits: &mut runtime_policy::DocumentSettlementLimits| limits.microtasks += 1,
            |limits: &mut runtime_policy::DocumentSettlementLimits| {
                limits.rendering_opportunities += 1
            },
            |limits: &mut runtime_policy::DocumentSettlementLimits| limits.mutations += 1,
            |limits: &mut runtime_policy::DocumentSettlementLimits| {
                limits.post_readiness_resources += 1
            },
            |limits: &mut runtime_policy::DocumentSettlementLimits| limits.process_cpu_ms += 1,
            |limits: &mut runtime_policy::DocumentSettlementLimits| limits.host_wall_ms += 1,
        ];
        for mutate in mutate_limits {
            let mut policy = baseline_policy;
            mutate(&mut policy.settlement.limits);
            variants.push(policy);
        }

        for policy in variants {
            policy.validate().unwrap();
            let render_id = stable_render_id_with_runtime_policy(
                input,
                environment,
                page,
                &resources,
                false,
                policy,
            );
            assert_ne!(render_id, baseline_render_id);
            assert_ne!(
                publication_request_fingerprint_with_runtime_policy(
                    PublicationRuntimeIdentity::DocumentSession,
                    &baseline_render_id,
                    false,
                    Path::new("invoice.html"),
                    Path::new("/canonical/invoice.html"),
                    None,
                    policy,
                ),
                baseline_recovery_id
            );
        }
    }

    #[cfg(all(feature = "document-session", unix))]
    #[test]
    fn publication_request_fingerprint_is_lossless_for_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;

        let root = temporary_artifacts("pliego-non-utf8-request-fingerprint");
        fs::create_dir(&root).unwrap();
        let first_requested =
            PathBuf::from(std::ffi::OsString::from_vec(vec![b'i', 0x80, b'.', b'h']));
        let second_requested =
            PathBuf::from(std::ffi::OsString::from_vec(vec![b'i', 0x81, b'.', b'h']));
        let first = root.join(&first_requested);
        let second = root.join(&second_requested);
        fs::write(&first, b"identical").unwrap();
        fs::write(&second, b"identical").unwrap();
        let first = first.canonicalize().unwrap();
        let second = second.canonicalize().unwrap();

        assert_ne!(
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                "sha256:same-content",
                false,
                &first_requested,
                &first,
                None,
            ),
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                "sha256:same-content",
                false,
                &second_requested,
                &first,
                None,
            )
        );
        assert_ne!(
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                "sha256:same-content",
                false,
                &first_requested,
                &first,
                None,
            ),
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                "sha256:same-content",
                false,
                &first_requested,
                &second,
                None,
            )
        );
        assert_ne!(
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                "sha256:same-content",
                false,
                &first_requested,
                &first,
                Some(&first),
            ),
            publication_request_fingerprint(
                PublicationRuntimeIdentity::DocumentSession,
                "sha256:same-content",
                false,
                &first_requested,
                &first,
                Some(&second),
            )
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn supervisor_identity_retains_pre_spawn_artifact_contract_inputs() {
        let root_name = temporary_artifacts("pliego-supervisor-artifact-contract-identity")
            .file_name()
            .unwrap()
            .to_owned();
        let root = std::env::current_dir().unwrap().join(&root_name);
        fs::create_dir(&root).unwrap();
        let input = root.join("input.html");
        let original = b"<!doctype html><p>original</p>";
        fs::write(&input, original).unwrap();
        let request = RenderRequest {
            input: PathBuf::from(root_name).join("input.html"),
            environment: RenderEnvironment {
                locale: "es-MX",
                timezone: "PST8PDT",
            },
            page: default_page(),
            resources: ResourcePolicyConfig::default(),
            runtime_policy: DeterministicRuntimePolicy::default(),
            allow_host_fonts: false,
            allow_partial_scene: false,
            explicit_paths: None,
        };

        let identity = supervisor_render_identity(&request, true).unwrap();
        assert_eq!(identity.locale, "es-MX");
        assert_eq!(identity.timezone, "PST8PDT");
        assert_eq!(identity.page, page_artifact(request.page));
        assert_eq!(identity.resource_policy["render_id"], identity.render_id);
        assert_eq!(
            identity.expected_input.url,
            url::Url::from_file_path(input.canonicalize().unwrap()).unwrap()
        );
        assert_eq!(identity.expected_input.sha256, sha256_hex(original));
        assert_eq!(
            identity.expected_input.content_address,
            format!("sha256:{}", sha256_hex(original))
        );
        assert_eq!(identity.expected_input.bytes, original.len() as u64);

        fs::write(&input, b"changed after identity resolution").unwrap();
        assert_eq!(identity.expected_input.sha256, sha256_hex(original));
        assert_eq!(identity.expected_input.bytes, original.len() as u64);
        fs::remove_dir_all(root).unwrap();
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
                runtime_policy: DeterministicRuntimePolicy::default(),
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

    #[cfg(feature = "shell-oracle")]
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

    #[cfg(feature = "shell-oracle")]
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

    #[cfg(feature = "shell-oracle")]
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

    #[cfg(feature = "shell-oracle")]
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

    #[cfg(feature = "shell-oracle")]
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

    #[cfg(feature = "shell-oracle")]
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

    #[cfg(feature = "shell-oracle")]
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
            fixed_point_authority: Default::default(),
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
            fixed_point_authority: Default::default(),
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
            fixed_point_authority: Default::default(),
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

    #[cfg(feature = "shell-oracle")]
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
            runtime_policy: DeterministicRuntimePolicy::default(),
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
        let publication = expect_new_publication(
            begin_publication(&request, &resource_policy, &render_id, document.path()).unwrap(),
        );
        DirectPublicationFixture {
            root,
            request,
            document,
            render_id,
            expected_input,
            publication,
        }
    }

    #[cfg(all(feature = "document-session", feature = "shell-oracle"))]
    #[test]
    fn combined_direct_publication_does_not_prepare_shell_userscripts() {
        let fixture = direct_publication_fixture(
            "pliego-combined-direct-publication",
            b"<!doctype html><title>Combined direct</title>",
        );
        assert!(fixture.publication.userscripts.is_none());
        assert!(!fixture.root.join("artifacts/userscripts").exists());

        let root = fixture.root.clone();
        drop(fixture);
        fs::remove_dir_all(root).unwrap();
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
            runtime_policy: DeterministicRuntimePolicy::default(),
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
        let publication = expect_new_publication(
            begin_publication(&request, &resource_policy, &render_id, document.path()).unwrap(),
        );
        let artifact_root = publication.artifacts.directory().to_owned();
        fs::write(&publication.proof, stable_png()).unwrap();
        publication
            .artifacts
            .write_readiness(&serde_json::json!({ "status": "ready" }))
            .unwrap();

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

        let publication_directory = artifact_root.join("publication");
        let sealed_cli_bytes = fs::read(publication_directory.join("outcome.json")).unwrap();
        let parsed_sealed_summary: serde_json::Value =
            serde_json::from_slice(&sealed_cli_bytes).unwrap();
        assert_eq!(parsed_sealed_summary["status"], "rendered");
        assert!(parsed_sealed_summary["phase_timings_ms"]["scene_setup"].is_number());
        assert_eq!(outcome.summary, parsed_sealed_summary);
        let first_summary = outcome.summary.clone();

        let mut ordinary_cli_bytes = Vec::new();
        write_render_outcome(&mut ordinary_cli_bytes, &outcome).unwrap();
        assert_eq!(ordinary_cli_bytes, sealed_cli_bytes);
        assert_eq!(outcome.cli_bytes, sealed_cli_bytes);

        let request_fingerprint = publication_request_fingerprint(
            PublicationRuntimeIdentity::DocumentSession,
            &render_id,
            request.allow_partial_scene,
            &request.input,
            document.path(),
            resource_policy.summary_asset_manifest_path(),
        );
        let document_pdf = artifact_root.join("document.pdf");
        let committed_before = fs::read(publication_directory.join("committed.json")).unwrap();
        let tree_before = snapshot_test_tree(&artifact_root);
        for recovery_index in 0..2 {
            let artifacts =
                SessionArtifacts::open_for_publication_recovery(&artifact_root, &render_id)
                    .unwrap();
            let journal = artifacts
                .resume_publication(&document_pdf, &request_fingerprint)
                .unwrap();
            let PublicationRecoveryState::Committed {
                summary,
                cli_bytes,
                recovered,
            } = journal.recover().unwrap()
            else {
                panic!("sealed shorthand publication must recover as committed");
            };
            assert!(!recovered, "recovery {recovery_index} must be idempotent");
            assert_eq!(summary, first_summary);
            assert_eq!(cli_bytes, sealed_cli_bytes);
            let recovered_outcome = RenderOutcome::from_sealed(summary, cli_bytes);
            let mut recovered_cli_bytes = Vec::new();
            write_render_outcome(&mut recovered_cli_bytes, &recovered_outcome).unwrap();
            assert_eq!(recovered_cli_bytes, sealed_cli_bytes);
            drop(journal);
            drop(artifacts);
            assert_eq!(snapshot_test_tree(&artifact_root), tree_before);
            assert_eq!(
                fs::read(publication_directory.join("committed.json")).unwrap(),
                committed_before
            );
        }

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
            runtime_policy: DeterministicRuntimePolicy::default(),
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
        let publication = expect_new_publication(
            begin_publication(&request, &resource_policy, &render_id, document.path()).unwrap(),
        );
        let artifact_root = publication.artifacts.directory().to_owned();
        fs::write(&publication.proof, stable_png()).unwrap();
        publication
            .artifacts
            .write_readiness(&serde_json::json!({ "status": "ready" }))
            .unwrap();

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
            fixed_point_authority: Default::default(),
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
