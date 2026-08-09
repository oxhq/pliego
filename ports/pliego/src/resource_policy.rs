/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::path::Path;
use std::path::PathBuf;

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use sha2::{Digest, Sha256};

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use super::asset_cache;

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) const RESOURCE_POLICY_ID: &str = "pliego.resource-policy.v1";
pub(crate) const DEFAULT_RESOURCE_TIMEOUT_MS: u64 = 10_000;
pub(crate) const MAX_RESOURCE_TIMEOUT_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq)]
pub struct ResourcePolicyConfig {
    pub allowed_http_roots: Vec<url::Url>,
    pub virtual_resources: Vec<VirtualResourceSpec>,
    pub asset_manifest: Option<PathBuf>,
    pub timeout_ms: u64,
}

impl Default for ResourcePolicyConfig {
    fn default() -> Self {
        Self {
            allowed_http_roots: Vec::new(),
            virtual_resources: Vec::new(),
            asset_manifest: None,
            timeout_ms: DEFAULT_RESOURCE_TIMEOUT_MS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualResourceSpec {
    pub url: url::Url,
    pub path: PathBuf,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug)]
pub(crate) struct ResourcePolicy {
    pub(crate) allowed_http_roots: Vec<url::Url>,
    pub(crate) virtual_resources: Vec<VirtualResource>,
    pub(crate) asset_manifest: Option<PathBuf>,
    pub(crate) assets: Option<asset_cache::AssetStore>,
    pub(crate) asset_error: Option<asset_cache::AssetError>,
    pub(crate) resident_bytes: u64,
    aggregate_limit: Option<u64>,
    pub(crate) timeout_ms: u64,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            allowed_http_roots: Vec::new(),
            virtual_resources: Vec::new(),
            asset_manifest: None,
            assets: None,
            asset_error: None,
            resident_bytes: 0,
            aggregate_limit: None,
            timeout_ms: DEFAULT_RESOURCE_TIMEOUT_MS,
        }
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug)]
pub(crate) struct VirtualResource {
    pub(crate) url: url::Url,
    pub(crate) body: Result<Vec<u8>, LocalResourceReadError>,
    content_type: &'static str,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug)]
pub(crate) enum LocalResourceReadError {
    AggregateLimit,
    Unavailable(String),
    TooLarge,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceRequest {
    pub(crate) method: String,
    pub(crate) url: url::Url,
    pub(crate) destination: String,
    pub(crate) referrer_url: Option<url::Url>,
    pub(crate) is_for_main_frame: bool,
    pub(crate) is_redirect: bool,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceSource {
    AssetCache(&'static str),
    DataUrl,
    DocumentRoot,
    #[cfg(feature = "document-session")]
    Http,
    VirtualResource,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ControlledResource {
    pub(crate) status: u16,
    pub(crate) content_type: Option<String>,
    pub(crate) body: Vec<u8>,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) struct ControlledHttpResponse {
    pub(crate) status: http::StatusCode,
    pub(crate) headers: http::HeaderMap,
    pub(crate) body: Vec<u8>,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceEvidence {
    pub(crate) request: ResourceRequest,
    pub(crate) source: ResourceSource,
    pub(crate) status: &'static str,
    pub(crate) response_status: Option<u16>,
    pub(crate) content_type: Option<String>,
    pub(crate) bytes: Option<u64>,
    pub(crate) sha256: Option<String>,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl ResourceEvidence {
    pub(crate) fn loaded(
        request: ResourceRequest,
        source: ResourceSource,
        content_type: &'static str,
        body: &[u8],
    ) -> Self {
        let bytes = body.len() as u64;
        let sha256 = sha256_hex(&body);
        Self {
            request,
            source,
            status: "loaded",
            response_status: Some(200),
            content_type: Some(content_type.into()),
            bytes: Some(bytes),
            sha256: Some(sha256),
        }
    }

    pub(crate) fn delegated(request: ResourceRequest, source: ResourceSource) -> Self {
        Self {
            request,
            source,
            status: "delegated",
            response_status: None,
            content_type: None,
            bytes: None,
            sha256: None,
        }
    }

    pub(crate) fn loaded_http(request: ResourceRequest, response: &ControlledHttpResponse) -> Self {
        let bytes = response.body.len() as u64;
        let sha256 = sha256_hex(&response.body);
        Self {
            request,
            source: ResourceSource::Http,
            status: "loaded",
            response_status: Some(response.status.as_u16()),
            content_type: response
                .headers
                .get(http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            bytes: Some(bytes),
            sha256: Some(sha256),
        }
    }
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResourceAccounting {
    pub(crate) requests: usize,
    pub(crate) loaded: usize,
    pub(crate) delegated: usize,
    pub(crate) failed: usize,
    pub(crate) body_bytes: u64,
    pub(crate) unavailable_bodies: usize,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl ResourceAccounting {
    pub(crate) fn from_evidence(resources: &[ResourceEvidence]) -> Self {
        Self {
            requests: resources.len(),
            loaded: resources
                .iter()
                .filter(|resource| resource.status == "loaded")
                .count(),
            delegated: resources
                .iter()
                .filter(|resource| resource.status == "delegated")
                .count(),
            failed: 0,
            body_bytes: resources.iter().filter_map(|resource| resource.bytes).sum(),
            unavailable_bodies: resources
                .iter()
                .filter(|resource| resource.bytes.is_none())
                .count(),
        }
    }

    pub(crate) fn with_failure(mut self) -> Self {
        self.requests += 1;
        self.failed += 1;
        self.unavailable_bodies += 1;
        self
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourcePolicyFailure {
    pub(crate) code: &'static str,
    pub(crate) status: &'static str,
    pub(crate) url: String,
    pub(crate) method: String,
    pub(crate) destination: String,
    pub(crate) referrer_url: Option<String>,
    pub(crate) is_for_main_frame: bool,
    pub(crate) is_redirect: bool,
    pub(crate) reason: String,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl ResourcePolicyFailure {
    pub(crate) fn new(
        request: &ResourceRequest,
        code: &'static str,
        status: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            code,
            status,
            url: request.url.to_string(),
            method: request.method.clone(),
            destination: request.destination.clone(),
            referrer_url: request.referrer_url.as_ref().map(ToString::to_string),
            is_for_main_frame: request.is_for_main_frame,
            is_redirect: request.is_redirect,
            reason: reason.into(),
        }
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) enum ResourcePolicyDecision {
    Allow {
        #[cfg_attr(not(feature = "document-session"), allow(dead_code))]
        source: ResourceSource,
    },
    FetchHttp,
    Synthesize {
        body: Vec<u8>,
        content_type: &'static str,
        #[cfg_attr(not(feature = "document-session"), allow(dead_code))]
        source: ResourceSource,
    },
    Fail(ResourcePolicyFailure),
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl ResourcePolicy {
    pub(crate) fn resolve(config: &ResourcePolicyConfig, document_root: &Path) -> Self {
        Self::resolve_with_budget(config, document_root, asset_cache::MAX_CACHE_BYTES)
    }

    pub(crate) fn resolve_with_budget(
        config: &ResourcePolicyConfig,
        document_root: &Path,
        max_resident_bytes: u64,
    ) -> Self {
        let mut resident_bytes = 0u64;
        let mut aggregate_limit_exceeded = false;
        let virtual_resources = config
            .virtual_resources
            .iter()
            .map(|resource| {
                let path = if resource.path.is_absolute() {
                    resource.path.clone()
                } else {
                    document_root.join(&resource.path)
                };
                let body = if aggregate_limit_exceeded {
                    Err(LocalResourceReadError::AggregateLimit)
                } else {
                    read_bounded_local_resource(&path).and_then(|body| {
                        let next = resident_bytes
                            .checked_add(body.len() as u64)
                            .filter(|bytes| *bytes <= max_resident_bytes)
                            .ok_or(LocalResourceReadError::AggregateLimit)?;
                        resident_bytes = next;
                        Ok(body)
                    })
                };
                if matches!(body, Err(LocalResourceReadError::AggregateLimit)) {
                    aggregate_limit_exceeded = true;
                }
                VirtualResource {
                    url: resource.url.clone(),
                    body,
                    content_type: resource_content_type(&resource.url),
                }
            })
            .collect();
        let asset_manifest = config.asset_manifest.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                document_root.join(path)
            }
        });
        let (assets, asset_error) = match (aggregate_limit_exceeded, asset_manifest.as_deref()) {
            (false, Some(path)) => match asset_cache::AssetStore::load_with_budget(
                path,
                max_resident_bytes.saturating_sub(resident_bytes),
            ) {
                Ok(assets) => (Some(assets), None),
                Err(error) => (None, Some(error)),
            },
            _ => (None, None),
        };
        if let Some(assets) = &assets {
            resident_bytes += assets.resident_bytes();
        }
        Self {
            allowed_http_roots: config.allowed_http_roots.clone(),
            virtual_resources,
            asset_manifest,
            assets,
            asset_error,
            resident_bytes,
            aggregate_limit: aggregate_limit_exceeded.then_some(max_resident_bytes),
            timeout_ms: config.timeout_ms,
        }
    }

    pub(crate) fn aggregate_limit_error(&self) -> Option<(&'static str, String)> {
        self.aggregate_limit.map(|max_resident_bytes| {
            (
                "RESOURCE_DENIED",
                aggregate_limit_message(max_resident_bytes),
            )
        })
    }

    pub(crate) fn decide(
        &self,
        document_root: &Path,
        request: &ResourceRequest,
    ) -> ResourcePolicyDecision {
        let failure = |code, status, reason: String| {
            ResourcePolicyDecision::Fail(ResourcePolicyFailure::new(request, code, status, reason))
        };

        if request.is_redirect {
            return failure("RESOURCE_DENIED", "denied", "redirects are disabled".into());
        }

        if !matches!(request.method.as_str(), "GET" | "HEAD") {
            return failure(
                "RESOURCE_DENIED",
                "denied",
                "only GET and HEAD resource requests are allowed".into(),
            );
        }

        let synthesize = |body: &[u8], content_type, source| ResourcePolicyDecision::Synthesize {
            body: if request.method == "HEAD" {
                Vec::new()
            } else {
                body.to_vec()
            },
            content_type,
            source,
        };

        if let Some(resource) = self
            .assets
            .as_ref()
            .and_then(|assets| assets.get(&request.url))
        {
            return synthesize(
                &resource.body,
                resource_content_type(&resource.url),
                ResourceSource::AssetCache(resource.cache_result.as_str()),
            );
        }

        if let Some(resource) = self
            .virtual_resources
            .iter()
            .find(|resource| resource.url == request.url)
        {
            return match &resource.body {
                Ok(body) => {
                    synthesize(body, resource.content_type, ResourceSource::VirtualResource)
                },
                Err(LocalResourceReadError::AggregateLimit) => failure(
                    "RESOURCE_DENIED",
                    "denied",
                    aggregate_limit_message(
                        self.aggregate_limit.unwrap_or(asset_cache::MAX_CACHE_BYTES),
                    ),
                ),
                Err(LocalResourceReadError::Unavailable(reason)) => failure(
                    "RESOURCE_NOT_FOUND",
                    "not_found",
                    format!("host virtual resource is unavailable: {reason}"),
                ),
                Err(LocalResourceReadError::TooLarge) => failure(
                    "RESOURCE_DENIED",
                    "denied",
                    format!(
                        "host virtual resource exceeds the {}-byte limit",
                        asset_cache::MAX_CACHE_BYTES
                    ),
                ),
            };
        }

        match request.url.scheme() {
            "data" => ResourcePolicyDecision::Allow {
                source: ResourceSource::DataUrl,
            },
            "file" => {
                let Ok(path) = request.url.to_file_path() else {
                    return failure(
                        "RESOURCE_DENIED",
                        "denied",
                        "file URL cannot be resolved".into(),
                    );
                };
                match path.canonicalize() {
                    Ok(path) if path.starts_with(document_root) => {
                        match read_bounded_local_resource(&path) {
                            Ok(body) => synthesize(
                                &body,
                                resource_content_type(&request.url),
                                ResourceSource::DocumentRoot,
                            ),
                            Err(LocalResourceReadError::AggregateLimit) => failure(
                                "RESOURCE_DENIED",
                                "denied",
                                aggregate_limit_message(asset_cache::MAX_CACHE_BYTES),
                            ),
                            Err(LocalResourceReadError::Unavailable(error)) => failure(
                                "RESOURCE_NOT_FOUND",
                                "not_found",
                                format!("file inside the document root is unavailable: {error}"),
                            ),
                            Err(LocalResourceReadError::TooLarge) => failure(
                                "RESOURCE_DENIED",
                                "denied",
                                format!(
                                    "local resource exceeds the {}-byte limit",
                                    asset_cache::MAX_CACHE_BYTES
                                ),
                            ),
                        }
                    },
                    Ok(_) => failure(
                        "RESOURCE_DENIED",
                        "denied",
                        "file is outside the document root".into(),
                    ),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && nearest_existing_ancestor(&path)
                                .is_some_and(|ancestor| ancestor.starts_with(document_root)) =>
                    {
                        failure(
                            "RESOURCE_NOT_FOUND",
                            "not_found",
                            "file does not exist inside the document root".into(),
                        )
                    },
                    Err(_) => failure(
                        "RESOURCE_DENIED",
                        "denied",
                        "file is outside the document root or unavailable".into(),
                    ),
                }
            },
            "http" | "https"
                if self
                    .allowed_http_roots
                    .iter()
                    .any(|root| http_root_allows(root, &request.url)) =>
            {
                ResourcePolicyDecision::FetchHttp
            },
            "http" | "https" => failure(
                "RESOURCE_DENIED",
                "denied",
                "network URL is outside the configured HTTP roots".into(),
            ),
            _ => failure(
                "RESOURCE_DENIED",
                "denied",
                "URL scheme is not allowed".into(),
            ),
        }
    }

    pub(crate) fn artifact(&self, render_id: &str) -> serde_json::Value {
        let mut artifact = serde_json::json!({
            "schema": RESOURCE_POLICY_ID,
            "version": 1,
            "render_id": render_id,
            "network": if self.allowed_http_roots.is_empty() { "deny" } else { "configured-roots" },
            "http_roots": self.allowed_http_roots.iter().map(url::Url::as_str).collect::<Vec<_>>(),
            "filesystem": "document-root",
            "data_urls": "allow",
            "redirects": "deny",
            "timeout_ms": self.timeout_ms,
            "virtual_resources": self.virtual_resources.iter().map(|resource| serde_json::json!({
                "url": resource.url,
                "content_type": resource.content_type,
                "available": resource.body.is_ok(),
                "bytes": resource.body.as_ref().ok().map(Vec::len),
                "sha256": resource.body.as_ref().ok().map(|body| sha256_hex(body)),
            })).collect::<Vec<_>>(),
        });
        if let Some(value) = match (&self.assets, &self.asset_error, &self.asset_manifest) {
            (Some(assets), _, _) => Some(assets.artifact()),
            (_, Some(error), Some(path)) => Some(error.artifact(path)),
            _ => None,
        } {
            artifact["asset_manifest"] = value;
        }
        artifact
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn create_controlled_http_client() -> net::connector::ServoClient {
    net::connector::create_http_client(net::connector::create_tls_config(
        net::connector::CACertificates::Default,
        false,
        net::connector::CertificateErrorOverrideManager::new(),
    ))
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn fetch_controlled_http(
    client: &net::connector::ServoClient,
    request: &ResourceRequest,
    headers: &http::HeaderMap,
    timeout_ms: u64,
) -> Result<ControlledHttpResponse, ResourcePolicyFailure> {
    use std::time::Duration;

    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;

    let failure = |code, status, reason: String, is_redirect| {
        let mut failure = ResourcePolicyFailure::new(request, code, status, reason);
        failure.is_redirect = is_redirect;
        failure
    };
    let body = Empty::<Bytes>::new()
        .map_err(|error: std::convert::Infallible| match error {})
        .boxed();
    let mut outbound = http::Request::builder()
        .method(request.method.as_str())
        .uri(request.url.as_str())
        .body(body)
        .map_err(|error| {
            failure(
                "RESOURCE_DENIED",
                "denied",
                format!("controlled HTTP request is invalid: {error}"),
                false,
            )
        })?;
    *outbound.headers_mut() = headers.clone();

    let client = client.clone();
    let is_head = request.method == "HEAD";
    let fetched = net::async_runtime::spawn_blocking_task::<_, ()>(async move {
        tokio::time::timeout(Duration::from_millis(timeout_ms), async move {
            let mut response = client
                .request(outbound)
                .await
                .map_err(|error| (false, error.to_string()))?;
            let status = response.status();
            let mut headers = response.headers().clone();
            headers.remove(http::header::CONNECTION);
            headers.remove(http::header::TRANSFER_ENCODING);
            if classify_controlled_http_status(status).is_some() {
                return Ok((status, headers, Vec::new()));
            }
            if headers
                .get(http::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|bytes| bytes > asset_cache::MAX_CACHE_BYTES)
            {
                return Err((true, String::new()));
            }
            if is_head {
                return Ok((status, headers, Vec::new()));
            }
            let mut body = Vec::new();
            while let Some(frame) = response.body_mut().frame().await {
                let frame = frame.map_err(|error| (false, error.to_string()))?;
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                if body
                    .len()
                    .checked_add(data.len())
                    .is_none_or(|bytes| bytes as u64 > asset_cache::MAX_CACHE_BYTES)
                {
                    return Err((true, String::new()));
                }
                body.extend_from_slice(&data);
            }
            Ok::<_, (bool, String)>((status, headers, body))
        })
        .await
    });

    let (status, headers, body) = match fetched {
        Err(_) => {
            return Err(failure(
                "RESOURCE_TIMEOUT",
                "timeout",
                "controlled HTTP resource exceeded its configured deadline".into(),
                false,
            ));
        },
        Ok(Err((too_large, error))) => {
            return Err(if too_large {
                failure(
                    "RESOURCE_DENIED",
                    "denied",
                    format!(
                        "controlled HTTP resource exceeds the {}-byte limit",
                        asset_cache::MAX_CACHE_BYTES
                    ),
                    false,
                )
            } else {
                failure(
                    "RESOURCE_NOT_FOUND",
                    "not_found",
                    format!("controlled HTTP resource is unavailable: {error}"),
                    false,
                )
            });
        },
        Ok(Ok(response)) => response,
    };

    if let Some((code, failure_status, reason, is_redirect)) =
        classify_controlled_http_status(status)
    {
        return Err(failure(code, failure_status, reason.into(), is_redirect));
    }

    Ok(ControlledHttpResponse {
        status,
        headers,
        body,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn classify_controlled_http_status(
    status: http::StatusCode,
) -> Option<(&'static str, &'static str, &'static str, bool)> {
    if status.is_redirection() {
        Some(("RESOURCE_DENIED", "denied", "redirects are disabled", true))
    } else if matches!(status.as_u16(), 404 | 410) {
        Some((
            "RESOURCE_NOT_FOUND",
            "not_found",
            "controlled HTTP resource was not found",
            false,
        ))
    } else if matches!(status.as_u16(), 408 | 504) {
        Some((
            "RESOURCE_TIMEOUT",
            "timeout",
            "controlled HTTP resource reported a timeout",
            false,
        ))
    } else {
        None
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn retain_controlled_resource(
    resources: &mut std::collections::BTreeMap<(String, String), ControlledResource>,
    resident_bytes: &mut u64,
    request: &ResourceRequest,
    resource: ControlledResource,
) -> Result<(), ResourcePolicyFailure> {
    let key = (request.method.clone(), request.url.to_string());
    if let Some(existing) = resources.get(&key) {
        if existing != &resource {
            return Err(ResourcePolicyFailure::new(
                request,
                "RESOURCE_CHANGED_DURING_RENDER",
                "changed",
                "controlled URL returned different bytes during one render",
            ));
        }
        return Ok(());
    }

    let next_resident_bytes = resident_bytes
        .checked_add(resource.body.len() as u64)
        .filter(|bytes| *bytes <= asset_cache::MAX_CACHE_BYTES)
        .ok_or_else(|| {
            ResourcePolicyFailure::new(
                request,
                "RESOURCE_DENIED",
                "denied",
                aggregate_limit_message(asset_cache::MAX_CACHE_BYTES),
            )
        })?;
    resources.insert(key, resource);
    *resident_bytes = next_resident_bytes;
    Ok(())
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn aggregate_limit_message(max_resident_bytes: u64) -> String {
    format!("resident resources exceed the {max_resident_bytes}-byte aggregate bound")
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn http_root_allows(root: &url::Url, requested: &url::Url) -> bool {
    requested.username().is_empty()
        && requested.password().is_none()
        && root.scheme() == requested.scheme()
        && root.host_str() == requested.host_str()
        && root.port_or_known_default() == requested.port_or_known_default()
        && requested.path().starts_with(root.path())
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find_map(|ancestor| ancestor.canonicalize().ok())
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn read_bounded_local_resource(path: &Path) -> Result<Vec<u8>, LocalResourceReadError> {
    let (file, metadata) = asset_cache::open_regular_file(path)
        .map_err(|error| LocalResourceReadError::Unavailable(error.to_string()))?;
    if metadata.len() > asset_cache::MAX_CACHE_BYTES {
        return Err(LocalResourceReadError::TooLarge);
    }

    let mut file = std::io::Read::take(file, asset_cache::MAX_CACHE_BYTES + 1);
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut body)
        .map_err(|error| LocalResourceReadError::Unavailable(error.to_string()))?;
    if body.len() as u64 > asset_cache::MAX_CACHE_BYTES {
        Err(LocalResourceReadError::TooLarge)
    } else {
        Ok(body)
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn resource_content_type(url: &url::Url) -> &'static str {
    let extension = Path::new(url.path())
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("css") => "text/css",
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("otf") => "font/otf",
        Some("ttf") => "font/ttf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(all(test, not(any(target_os = "android", target_env = "ohos"))))]
mod tests {
    #[cfg(unix)]
    use std::ffi::CString;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn make_fifo(path: &Path) {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    #[test]
    #[cfg(unix)]
    fn local_and_virtual_special_files_are_rejected_without_blocking() {
        let sandbox = std::env::temp_dir().join(format!(
            "pliego-resource-special-file-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = sandbox.join("root");
        fs::create_dir_all(&root).unwrap();
        let fifo = root.join("asset.fifo");
        make_fifo(&fifo);
        let virtual_url = url::Url::parse("https://virtual.test/asset.fifo").unwrap();
        let policy = ResourcePolicy::resolve(
            &ResourcePolicyConfig {
                virtual_resources: vec![VirtualResourceSpec {
                    url: virtual_url.clone(),
                    path: fifo.clone(),
                }],
                ..ResourcePolicyConfig::default()
            },
            &root,
        );
        let request = |url| ResourceRequest {
            method: "GET".into(),
            url,
            destination: "Unknown".into(),
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };

        for url in [url::Url::from_file_path(&fifo).unwrap(), virtual_url] {
            let ResourcePolicyDecision::Fail(error) = policy.decide(&root, &request(url)) else {
                panic!("a special-file resource must fail closed")
            };
            assert_eq!(error.code, "RESOURCE_NOT_FOUND");
            assert!(error.reason.contains("not a regular file"));
        }

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn preserves_fail_closed_request_and_budget_contract() {
        let sandbox = std::env::temp_dir().join(format!(
            "pliego-resource-policy-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = sandbox.join("root");
        let outside = sandbox.join("outside.css");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("inside.css"), b"body {}").unwrap();
        fs::write(&outside, b"outside {}").unwrap();
        let root = root.canonicalize().unwrap();
        let allowed = url::Url::parse("https://example.test/assets/style.css").unwrap();
        let policy = ResourcePolicy::resolve(
            &ResourcePolicyConfig {
                allowed_http_roots: vec![url::Url::parse("https://example.test/assets/").unwrap()],
                timeout_ms: 321,
                ..ResourcePolicyConfig::default()
            },
            &root,
        );
        let request = |method: &str, url: url::Url, is_redirect| ResourceRequest {
            method: method.into(),
            url,
            destination: "Style".into(),
            referrer_url: Some(url::Url::parse("file:///document.html").unwrap()),
            is_for_main_frame: false,
            is_redirect,
        };

        let inside = url::Url::from_file_path(root.join("inside.css")).unwrap();
        let ResourcePolicyDecision::Synthesize {
            body, content_type, ..
        } = policy.decide(&root, &request("GET", inside.clone(), false))
        else {
            panic!("inside-root GET should synthesize")
        };
        assert_eq!(body, b"body {}");
        assert_eq!(content_type, "text/css");

        let ResourcePolicyDecision::Fail(method) =
            policy.decide(&root, &request("POST", inside, false))
        else {
            panic!("POST should fail before reading")
        };
        assert_eq!(method.code, "RESOURCE_DENIED");
        assert_eq!(method.method, "POST");
        assert_eq!(method.destination, "Style");
        assert_eq!(
            method.referrer_url.as_deref(),
            Some("file:///document.html")
        );

        let ResourcePolicyDecision::Fail(outside) = policy.decide(
            &root,
            &request("GET", url::Url::from_file_path(outside).unwrap(), false),
        ) else {
            panic!("outside-root file should fail")
        };
        assert_eq!(outside.code, "RESOURCE_DENIED");

        let ResourcePolicyDecision::Fail(redirect) =
            policy.decide(&root, &request("GET", allowed.clone(), true))
        else {
            panic!("redirect should fail before fetching")
        };
        assert!(redirect.is_redirect);
        assert_eq!(redirect.reason, "redirects are disabled");
        assert!(matches!(
            policy.decide(&root, &request("GET", allowed, false)),
            ResourcePolicyDecision::FetchHttp
        ));

        let artifact = policy.artifact("sha256:test");
        assert_eq!(artifact["timeout_ms"], 321);
        assert_eq!(artifact["filesystem"], "document-root");
        assert_eq!(artifact["redirects"], "deny");
        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    #[cfg(feature = "document-session")]
    fn evidence_preserves_resource_status_hash_and_accounting() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("file:///document.css").unwrap(),
            destination: "Style".into(),
            referrer_url: Some(url::Url::parse("file:///document.html").unwrap()),
            is_for_main_frame: false,
            is_redirect: false,
        };
        let loaded = ResourceEvidence::loaded(
            request.clone(),
            ResourceSource::DocumentRoot,
            "text/css",
            b"body {}",
        );
        let delegated = ResourceEvidence::delegated(request, ResourceSource::DataUrl);
        assert_eq!(loaded.source, ResourceSource::DocumentRoot);
        assert_eq!(loaded.status, "loaded");
        assert_eq!(loaded.response_status, Some(200));
        assert_eq!(loaded.content_type.as_deref(), Some("text/css"));
        assert_eq!(loaded.bytes, Some(7));
        assert_eq!(
            loaded.sha256.as_deref(),
            Some(sha256_hex(b"body {}").as_str())
        );
        assert_eq!(delegated.source, ResourceSource::DataUrl);
        assert_eq!(delegated.status, "delegated");
        assert_eq!(delegated.response_status, None);
        assert_eq!(delegated.content_type, None);
        assert_eq!(delegated.bytes, None);
        assert_eq!(delegated.sha256, None);
        let accounting = ResourceAccounting::from_evidence(&[loaded, delegated]);
        assert_eq!(
            accounting,
            ResourceAccounting {
                requests: 2,
                loaded: 1,
                delegated: 1,
                failed: 0,
                body_bytes: 7,
                unavailable_bodies: 1,
            }
        );
        assert_eq!(
            accounting.requests,
            accounting.loaded + accounting.delegated + accounting.failed
        );
        assert_eq!(
            accounting.unavailable_bodies,
            accounting.delegated + accounting.failed
        );

        let failed = accounting.with_failure();
        assert_eq!(failed.requests, 3);
        assert_eq!(failed.failed, 1);
        assert_eq!(failed.unavailable_bodies, 2);
        assert_eq!(
            failed.requests,
            failed.loaded + failed.delegated + failed.failed
        );
        assert_eq!(failed.unavailable_bodies, failed.delegated + failed.failed);
    }

    #[test]
    fn repeated_controlled_resource_must_not_change_during_one_render() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://example.test/stable.js").unwrap(),
            destination: "Script".into(),
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };
        let original = ControlledResource {
            status: 200,
            content_type: Some("text/javascript".into()),
            body: b"window.stable = true;".to_vec(),
        };
        let mut resources = std::collections::BTreeMap::new();
        let mut resident_bytes = 0;
        retain_controlled_resource(
            &mut resources,
            &mut resident_bytes,
            &request,
            original.clone(),
        )
        .unwrap();
        retain_controlled_resource(
            &mut resources,
            &mut resident_bytes,
            &request,
            original.clone(),
        )
        .unwrap();
        assert_eq!(resident_bytes, original.body.len() as u64);

        let error = retain_controlled_resource(
            &mut resources,
            &mut resident_bytes,
            &request,
            ControlledResource {
                body: b"window.stable = false;".to_vec(),
                ..original
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "RESOURCE_CHANGED_DURING_RENDER");
        assert_eq!(error.status, "changed");
        assert_eq!(resources.len(), 1);
    }

    #[test]
    #[cfg(feature = "document-session")]
    fn head_evidence_hashes_the_exact_empty_response_body() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/css"),
        );
        let evidence = ResourceEvidence::loaded_http(
            ResourceRequest {
                method: "HEAD".into(),
                url: url::Url::parse("https://example.test/style.css").unwrap(),
                destination: "Style".into(),
                referrer_url: None,
                is_for_main_frame: false,
                is_redirect: false,
            },
            &ControlledHttpResponse {
                status: http::StatusCode::OK,
                headers,
                body: Vec::new(),
            },
        );
        assert_eq!(evidence.response_status, Some(200));
        assert_eq!(evidence.bytes, Some(0));
        assert_eq!(
            evidence.sha256.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }
}
