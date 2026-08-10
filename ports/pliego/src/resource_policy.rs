/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::path::Path;
use std::path::PathBuf;

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use embedder_traits::WebResourceLoadRole;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use sha2::{Digest, Sha256};

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use super::asset_cache;

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) const RESOURCE_POLICY_ID: &str = "pliego.resource-policy.v1";
pub(crate) const DEFAULT_RESOURCE_TIMEOUT_MS: u64 = 10_000;
pub(crate) const MAX_RESOURCE_TIMEOUT_MS: u64 = 60_000;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) const MAX_RESPONSE_HEADER_COUNT: usize = 256;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) const MAX_RESPONSE_HEADER_BYTES: u64 = 64 * 1024;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) const MAX_RESOURCE_EVENTS: usize = 16_384;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
pub(crate) const MAX_RESOURCE_METADATA_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
const RESPONSE_HEADER_ENTRY_OVERHEAD_BYTES: u64 = 64;

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
pub(crate) enum ResourcePolicySetupFailure<'a> {
    Asset {
        error: &'a asset_cache::AssetError,
        manifest: &'a Path,
    },
    Aggregate {
        code: &'static str,
        message: String,
    },
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
    OutsideRoot,
    Unavailable(String),
    TooLarge,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceRequest {
    pub(crate) method: String,
    pub(crate) url: url::Url,
    pub(crate) destination: String,
    pub(crate) load_role: WebResourceLoadRole,
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
pub(crate) struct ResponseHeaderEvidence {
    pub(crate) count: u64,
    pub(crate) bytes: u64,
    pub(crate) names: Vec<String>,
    pub(crate) sha256: String,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl ResponseHeaderEvidence {
    pub(crate) fn from_headers(headers: &http::HeaderMap) -> Result<Self, String> {
        let count = headers.len();
        if count > MAX_RESPONSE_HEADER_COUNT {
            return Err(format!(
                "response has {count} header values, exceeding the {MAX_RESPONSE_HEADER_COUNT}-value bound"
            ));
        }

        let mut bytes = 0u64;
        let mut entries = headers
            .iter()
            .enumerate()
            .map(|(index, (name, value))| {
                bytes = bytes
                    .checked_add(name.as_str().len() as u64)
                    .and_then(|bytes| bytes.checked_add(value.as_bytes().len() as u64))
                    .unwrap_or(u64::MAX);
                (name.as_str().as_bytes(), value.as_bytes(), index)
            })
            .collect::<Vec<_>>();
        if bytes > MAX_RESPONSE_HEADER_BYTES {
            return Err(format!(
                "response headers contain {bytes} bytes, exceeding the {MAX_RESPONSE_HEADER_BYTES}-byte bound"
            ));
        }

        // Header names are case-insensitive and cross-name insertion order is not semantic.
        // Preserve the original order of repeated values for the same name.
        entries.sort_by(|left, right| left.0.cmp(right.0).then_with(|| left.2.cmp(&right.2)));
        let mut names = entries
            .iter()
            .map(|(name, _, _)| String::from_utf8_lossy(name).into_owned())
            .collect::<Vec<_>>();
        names.dedup();
        let mut canonical = Vec::with_capacity(bytes as usize + entries.len() * 8 + 32);
        canonical.extend_from_slice(b"pliego.response-headers.v1\0");
        for (name, value, _) in entries {
            canonical.extend_from_slice(&(name.len() as u32).to_be_bytes());
            canonical.extend_from_slice(name);
            canonical.extend_from_slice(&(value.len() as u32).to_be_bytes());
            canonical.extend_from_slice(value);
        }
        Ok(Self {
            count: count as u64,
            bytes,
            names,
            sha256: sha256_hex(&canonical),
        })
    }

    pub(crate) fn retained_metadata_bytes(&self) -> u64 {
        self.sha256.len() as u64 +
            self.names.iter().map(|name| name.len() as u64).sum::<u64>() +
            self.names.len() as u64 * std::mem::size_of::<String>() as u64 +
            std::mem::size_of::<Self>() as u64
    }

    fn intercepted_metadata_bytes(&self) -> u64 {
        self.retained_metadata_bytes()
            .saturating_add(self.bytes)
            .saturating_add(
                self.count
                    .saturating_mul(RESPONSE_HEADER_ENTRY_OVERHEAD_BYTES),
            )
    }
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceEvidence {
    pub(crate) request: ResourceRequest,
    pub(crate) source: Option<ResourceSource>,
    pub(crate) status: &'static str,
    pub(crate) fatal: bool,
    pub(crate) failure: Option<ResourcePolicyFailure>,
    pub(crate) response_status: Option<u16>,
    pub(crate) content_type: Option<String>,
    pub(crate) bytes: Option<u64>,
    pub(crate) sha256: Option<String>,
    pub(crate) content_address: Option<String>,
    pub(crate) response_headers: Option<ResponseHeaderEvidence>,
}

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl ResourceEvidence {
    #[cfg(test)]
    pub(crate) fn loaded(
        request: ResourceRequest,
        source: ResourceSource,
        content_type: &str,
        body: &[u8],
    ) -> Self {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_str(content_type).unwrap(),
        );
        Self::loaded_response(
            request,
            source,
            200,
            Some(content_type),
            body,
            ResponseHeaderEvidence::from_headers(&headers).unwrap(),
        )
    }

    pub(crate) fn loaded_response(
        request: ResourceRequest,
        source: ResourceSource,
        response_status: u16,
        content_type: Option<&str>,
        body: &[u8],
        response_headers: ResponseHeaderEvidence,
    ) -> Self {
        let bytes = body.len() as u64;
        let sha256 = sha256_hex(&body);
        let content_address = format!("sha256:{sha256}");
        Self {
            request,
            source: Some(source),
            status: "loaded",
            fatal: false,
            failure: None,
            response_status: Some(response_status),
            content_type: content_type.map(str::to_owned),
            bytes: Some(bytes),
            sha256: Some(sha256),
            content_address: Some(content_address),
            response_headers: Some(response_headers),
        }
    }

    pub(crate) fn delegated(request: ResourceRequest, source: ResourceSource) -> Self {
        Self {
            request,
            source: Some(source),
            status: "delegated",
            fatal: false,
            failure: None,
            response_status: None,
            content_type: None,
            bytes: None,
            sha256: None,
            content_address: None,
            response_headers: None,
        }
    }

    pub(crate) fn cancelled(request: ResourceRequest, failure: ResourcePolicyFailure) -> Self {
        debug_assert!(!failure.fatal);
        Self {
            request,
            source: None,
            status: "cancelled",
            fatal: failure.fatal,
            failure: Some(failure),
            response_status: None,
            content_type: None,
            bytes: None,
            sha256: None,
            content_address: None,
            response_headers: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn loaded_http(request: ResourceRequest, response: &ControlledHttpResponse) -> Self {
        Self::loaded_response(
            request,
            ResourceSource::Http,
            response.status.as_u16(),
            response
                .headers
                .get(http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            &response.body,
            ResponseHeaderEvidence::from_headers(&response.headers).unwrap(),
        )
    }

    pub(crate) fn metadata_bytes(&self) -> u64 {
        let request = (self.request.method.len() as u64)
            .saturating_add(self.request.url.as_str().len() as u64)
            .saturating_add(self.request.destination.len() as u64)
            .saturating_add(
                self.request
                    .referrer_url
                    .as_ref()
                    .map_or(0, |url| url.as_str().len() as u64),
            );
        let response = (self.content_type.as_ref().map_or(0, String::len) as u64)
            .saturating_add(self.sha256.as_ref().map_or(0, String::len) as u64)
            .saturating_add(self.content_address.as_ref().map_or(0, String::len) as u64)
            .saturating_add(
                self.response_headers
                    .as_ref()
                    .map_or(0, ResponseHeaderEvidence::intercepted_metadata_bytes),
            )
            .saturating_add(
                self.failure
                    .as_ref()
                    .map_or(0, ResourcePolicyFailure::metadata_bytes),
            );
        request.saturating_add(response)
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
            failed: resources
                .iter()
                .filter(|resource| resource.failure.is_some())
                .count(),
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
    pub(crate) fatal: bool,
    pub(crate) url: String,
    pub(crate) method: String,
    pub(crate) destination: String,
    pub(crate) load_role: WebResourceLoadRole,
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
            fatal: true,
            url: request.url.to_string(),
            method: request.method.clone(),
            destination: request.destination.clone(),
            load_role: request.load_role,
            referrer_url: request.referrer_url.as_ref().map(ToString::to_string),
            is_for_main_frame: request.is_for_main_frame,
            is_redirect: request.is_redirect,
            reason: reason.into(),
        }
    }

    pub(crate) fn nonfatal(mut self) -> Self {
        self.fatal = false;
        self
    }

    #[cfg(feature = "document-session")]
    fn metadata_bytes(&self) -> u64 {
        self.url.len() as u64 +
            self.method.len() as u64 +
            self.destination.len() as u64 +
            self.referrer_url.as_ref().map_or(0, String::len) as u64 +
            self.reason.len() as u64 +
            std::mem::size_of::<Self>() as u64
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

    pub(crate) fn setup_failure(&self) -> Option<ResourcePolicySetupFailure<'_>> {
        if let (Some(error), Some(manifest)) =
            (self.asset_error.as_ref(), self.asset_manifest.as_deref())
        {
            return Some(ResourcePolicySetupFailure::Asset { error, manifest });
        }
        self.aggregate_limit_error()
            .map(|(code, message)| ResourcePolicySetupFailure::Aggregate { code, message })
    }

    pub(crate) fn decide(
        &self,
        document_root: &Path,
        request: &ResourceRequest,
    ) -> ResourcePolicyDecision {
        let denial = |code, status, reason: String| {
            let failure = ResourcePolicyFailure::new(request, code, status, reason);
            ResourcePolicyDecision::Fail(
                if request.load_role == WebResourceLoadRole::DocumentMetadata {
                    failure.nonfatal()
                } else {
                    failure
                },
            )
        };
        let fatal_failure = |code, status, reason: String| {
            ResourcePolicyDecision::Fail(ResourcePolicyFailure::new(request, code, status, reason))
        };

        if request.is_redirect {
            return denial("RESOURCE_DENIED", "denied", "redirects are disabled".into());
        }

        if !matches!(request.method.as_str(), "GET" | "HEAD") {
            return denial(
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
                Err(LocalResourceReadError::AggregateLimit) => fatal_failure(
                    "RESOURCE_DENIED",
                    "denied",
                    aggregate_limit_message(
                        self.aggregate_limit.unwrap_or(asset_cache::MAX_CACHE_BYTES),
                    ),
                ),
                Err(LocalResourceReadError::OutsideRoot) => denial(
                    "RESOURCE_DENIED",
                    "denied",
                    "host virtual resource resolved outside the document root".into(),
                ),
                Err(LocalResourceReadError::Unavailable(reason)) => denial(
                    "RESOURCE_NOT_FOUND",
                    "not_found",
                    format!("host virtual resource is unavailable: {reason}"),
                ),
                Err(LocalResourceReadError::TooLarge) => fatal_failure(
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
                    return denial(
                        "RESOURCE_DENIED",
                        "denied",
                        "file URL cannot be resolved".into(),
                    );
                };
                match path.canonicalize() {
                    Ok(path) if path.starts_with(document_root) => {
                        match read_bounded_document_resource(&path, document_root) {
                            Ok(body) => synthesize(
                                &body,
                                resource_content_type(&request.url),
                                ResourceSource::DocumentRoot,
                            ),
                            Err(LocalResourceReadError::AggregateLimit) => fatal_failure(
                                "RESOURCE_DENIED",
                                "denied",
                                aggregate_limit_message(asset_cache::MAX_CACHE_BYTES),
                            ),
                            Err(LocalResourceReadError::OutsideRoot) => denial(
                                "RESOURCE_DENIED",
                                "denied",
                                "file is outside the document root".into(),
                            ),
                            Err(LocalResourceReadError::Unavailable(error)) => denial(
                                "RESOURCE_NOT_FOUND",
                                "not_found",
                                format!("file inside the document root is unavailable: {error}"),
                            ),
                            Err(LocalResourceReadError::TooLarge) => fatal_failure(
                                "RESOURCE_DENIED",
                                "denied",
                                format!(
                                    "local resource exceeds the {}-byte limit",
                                    asset_cache::MAX_CACHE_BYTES
                                ),
                            ),
                        }
                    },
                    Ok(_) => denial(
                        "RESOURCE_DENIED",
                        "denied",
                        "file is outside the document root".into(),
                    ),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound &&
                            nearest_existing_ancestor(&path)
                                .is_some_and(|ancestor| ancestor.starts_with(document_root)) =>
                    {
                        denial(
                            "RESOURCE_NOT_FOUND",
                            "not_found",
                            "file does not exist inside the document root".into(),
                        )
                    },
                    Err(_) => denial(
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
            "http" | "https" => denial(
                "RESOURCE_DENIED",
                "denied",
                "network URL is outside the configured HTTP roots".into(),
            ),
            _ => denial(
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
    let target = normalized_url(&request.url);
    let mut outbound = http::Request::builder()
        .method(request.method.as_str())
        .uri(target)
        .body(body)
        .map_err(|error| {
            failure(
                "RESOURCE_DENIED",
                "denied",
                format!("controlled HTTP request is invalid: {error}"),
                false,
            )
        })?;
    *outbound.headers_mut() = controlled_request_headers(headers);

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

    let (status, mut headers, body) = match fetched {
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

    headers = normalize_controlled_response_headers(request, headers, body.len())?;

    Ok(ControlledHttpResponse {
        status,
        headers,
        body,
    })
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn controlled_request_headers(headers: &http::HeaderMap) -> http::HeaderMap {
    let mut controlled = http::HeaderMap::new();
    for name in [http::header::ACCEPT, http::header::ACCEPT_LANGUAGE] {
        for value in headers.get_all(&name) {
            controlled.append(name.clone(), value.clone());
        }
    }
    controlled.insert(
        http::header::ACCEPT_ENCODING,
        http::HeaderValue::from_static("identity"),
    );
    controlled
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn normalize_controlled_response_headers(
    request: &ResourceRequest,
    mut headers: http::HeaderMap,
    body_bytes: usize,
) -> Result<http::HeaderMap, ResourcePolicyFailure> {
    if headers
        .get_all(http::header::CONTENT_ENCODING)
        .iter()
        .any(|value| {
            value.to_str().map_or(true, |value| {
                value
                    .split(',')
                    .map(str::trim)
                    .any(|encoding| !encoding.eq_ignore_ascii_case("identity"))
            })
        })
    {
        return Err(ResourcePolicyFailure::new(
            request,
            "RESOURCE_ENCODING_UNSUPPORTED",
            "unsupported",
            "controlled HTTP resources must use identity content encoding",
        ));
    }
    headers.remove(http::header::CONTENT_ENCODING);
    headers.remove(http::header::CONTENT_LENGTH);
    headers.insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from_str(&body_bytes.to_string()).map_err(|error| {
            ResourcePolicyFailure::new(
                request,
                "RESOURCE_DENIED",
                "denied",
                format!("controlled HTTP response length is invalid: {error}"),
            )
        })?,
    );
    Ok(headers)
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
    let key = (request.method.clone(), normalized_url(&request.url));
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
    let root_path = root.path();
    let requested_path = requested.path();
    let path_allowed = if root_path.ends_with('/') {
        requested_path.starts_with(root_path)
    } else {
        requested_path == root_path ||
            requested_path
                .strip_prefix(root_path)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    requested.username().is_empty() &&
        requested.password().is_none() &&
        root.scheme() == requested.scheme() &&
        root.host_str() == requested.host_str() &&
        root.port_or_known_default() == requested.port_or_known_default() &&
        path_allowed
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn normalized_url(url: &url::Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    normalized.to_string()
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
    read_bounded_opened_resource(file, metadata)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn read_bounded_document_resource(
    path: &Path,
    document_root: &Path,
) -> Result<Vec<u8>, LocalResourceReadError> {
    let (file, metadata) = asset_cache::open_regular_file(path)
        .map_err(|error| LocalResourceReadError::Unavailable(error.to_string()))?;
    let opened_path = opened_regular_file_path(&file)
        .map_err(|error| LocalResourceReadError::Unavailable(error.to_string()))?;
    if !opened_path.starts_with(document_root) {
        return Err(LocalResourceReadError::OutsideRoot);
    }
    read_bounded_opened_resource(file, metadata)
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn read_bounded_opened_resource(
    file: std::fs::File,
    metadata: std::fs::Metadata,
) -> Result<Vec<u8>, LocalResourceReadError> {
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

#[cfg(target_os = "linux")]
fn opened_regular_file_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd as _;

    std::fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_vendor = "apple")]
#[allow(unsafe_code)]
fn opened_regular_file_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStringExt as _;

    let mut buffer = vec![0_u8; libc::PATH_MAX as usize];
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let length = buffer.iter().position(|byte| *byte == 0).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "opened file path is not null terminated",
        )
    })?;
    buffer.truncate(length);
    Ok(PathBuf::from(OsString::from_vec(buffer)))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn opened_regular_file_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let handle = file.as_raw_handle().cast();
    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    let mut buffer = vec![0_u16; 512];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
        };
        if length == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let length = length as usize;
        if length < buffer.len() {
            buffer.truncate(length);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        buffer.resize(length + 1, 0);
    }
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn opened_regular_file_path(_file: &std::fs::File) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opened file path verification is unavailable on this platform",
    ))
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
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("otf") => "font/otf",
        Some("ttf") => "font/ttf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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
            load_role: WebResourceLoadRole::DocumentContent,
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
    #[cfg(unix)]
    fn opened_document_handle_cannot_escape_the_root_through_a_symlink() {
        let sandbox = std::env::temp_dir().join(format!(
            "pliego-resource-handle-root-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = sandbox.join("root");
        let outside = sandbox.join("outside.css");
        fs::create_dir_all(&root).unwrap();
        fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape.css")).unwrap();
        let root = root.canonicalize().unwrap();

        assert!(matches!(
            read_bounded_document_resource(&root.join("escape.css"), &root),
            Err(LocalResourceReadError::OutsideRoot)
        ));

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn http_roots_require_a_path_segment_boundary() {
        let root = url::Url::parse("https://example.test/assets").unwrap();
        assert!(http_root_allows(&root, &root));
        assert!(http_root_allows(
            &root,
            &url::Url::parse("https://example.test/assets/style.css").unwrap()
        ));
        assert!(!http_root_allows(
            &root,
            &url::Url::parse("https://example.test/assets-private/style.css").unwrap()
        ));
        assert!(!http_root_allows(
            &root,
            &url::Url::parse("https://example.test/assetsX").unwrap()
        ));
    }

    #[test]
    fn synthesized_raster_content_types_match_supported_formats() {
        for (path, expected) in [
            ("image.jpg", "image/jpeg"),
            ("image.JPEG", "image/jpeg"),
            ("image.gif", "image/gif"),
            ("image.webp", "image/webp"),
            ("image.png", "image/png"),
        ] {
            let url = url::Url::parse(&format!("file:///bundle/{path}")).unwrap();
            assert_eq!(resource_content_type(&url), expected);
        }
    }

    #[test]
    fn setup_failure_precedence_is_shared_and_asset_first() {
        let manifest = PathBuf::from("assets.json");
        let policy = ResourcePolicy {
            asset_manifest: Some(manifest.clone()),
            asset_error: Some(asset_cache::AssetError {
                code: "ASSET_NOT_FOUND",
                url: None,
                message: "missing asset".into(),
                expected: None,
                actual: None,
            }),
            aggregate_limit: Some(1),
            ..ResourcePolicy::default()
        };

        let Some(ResourcePolicySetupFailure::Asset {
            error,
            manifest: selected_manifest,
        }) = policy.setup_failure()
        else {
            panic!("asset failure must precede aggregate failure")
        };
        assert_eq!(error.code, "ASSET_NOT_FOUND");
        assert_eq!(selected_manifest, manifest);
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
        let oversized = root.join("oversized.ico");
        fs::File::create(&oversized)
            .unwrap()
            .set_len(asset_cache::MAX_CACHE_BYTES + 1)
            .unwrap();
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
            load_role: WebResourceLoadRole::DocumentContent,
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

        let mut metadata_denial = request(
            "GET",
            url::Url::parse("https://denied.invalid/report.bin").unwrap(),
            false,
        );
        metadata_denial.load_role = WebResourceLoadRole::DocumentMetadata;
        let ResourcePolicyDecision::Fail(metadata_denial) = policy.decide(&root, &metadata_denial)
        else {
            panic!("metadata outside the configured roots should be denied")
        };
        assert!(!metadata_denial.fatal);

        let mut metadata_budget =
            request("GET", url::Url::from_file_path(oversized).unwrap(), false);
        metadata_budget.load_role = WebResourceLoadRole::DocumentMetadata;
        let ResourcePolicyDecision::Fail(metadata_budget) = policy.decide(&root, &metadata_budget)
        else {
            panic!("metadata must not bypass the per-resource body budget")
        };
        assert_eq!(metadata_budget.code, "RESOURCE_DENIED");
        assert!(metadata_budget.fatal);

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
            load_role: WebResourceLoadRole::DocumentContent,
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
        assert_eq!(loaded.source, Some(ResourceSource::DocumentRoot));
        assert_eq!(loaded.status, "loaded");
        assert_eq!(loaded.response_status, Some(200));
        assert_eq!(loaded.content_type.as_deref(), Some("text/css"));
        assert_eq!(loaded.bytes, Some(7));
        assert_eq!(
            loaded.sha256.as_deref(),
            Some(sha256_hex(b"body {}").as_str())
        );
        assert_eq!(
            loaded.content_address.as_deref(),
            Some(format!("sha256:{}", sha256_hex(b"body {}")).as_str())
        );
        assert_eq!(delegated.source, Some(ResourceSource::DataUrl));
        assert_eq!(delegated.status, "delegated");
        assert_eq!(delegated.response_status, None);
        assert_eq!(delegated.content_type, None);
        assert_eq!(delegated.bytes, None);
        assert_eq!(delegated.sha256, None);
        assert_eq!(delegated.content_address, None);
        assert!(loaded.response_headers.is_some());
        assert_eq!(delegated.response_headers, None);
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
            url: url::Url::parse("https://example.test/stable.js#first").unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentContent,
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

        let mut fragment_alias = request.clone();
        fragment_alias.url.set_fragment(Some("second"));
        let error = retain_controlled_resource(
            &mut resources,
            &mut resident_bytes,
            &fragment_alias,
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
                load_role: WebResourceLoadRole::DocumentContent,
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
        assert_eq!(
            evidence.content_address.as_deref(),
            Some("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        let header_evidence = evidence.response_headers.unwrap();
        assert_eq!(header_evidence.names, ["content-type"]);
        assert_eq!(header_evidence.count, 1);
    }

    #[test]
    #[cfg(feature = "document-session")]
    fn response_header_identity_is_order_stable_but_preserves_repeated_value_order() {
        let mut first = http::HeaderMap::new();
        first.insert("x-second", http::HeaderValue::from_static("2"));
        first.append("set-cookie", http::HeaderValue::from_static("a=1"));
        first.append("set-cookie", http::HeaderValue::from_static("b=2"));
        first.insert("x-first", http::HeaderValue::from_static("1"));
        let mut reordered_names = http::HeaderMap::new();
        reordered_names.insert("x-first", http::HeaderValue::from_static("1"));
        reordered_names.append("set-cookie", http::HeaderValue::from_static("a=1"));
        reordered_names.append("set-cookie", http::HeaderValue::from_static("b=2"));
        reordered_names.insert("x-second", http::HeaderValue::from_static("2"));
        let mut reversed_values = reordered_names.clone();
        reversed_values.remove("set-cookie");
        reversed_values.append("set-cookie", http::HeaderValue::from_static("b=2"));
        reversed_values.append("set-cookie", http::HeaderValue::from_static("a=1"));

        let first = ResponseHeaderEvidence::from_headers(&first).unwrap();
        let reordered = ResponseHeaderEvidence::from_headers(&reordered_names).unwrap();
        let reversed = ResponseHeaderEvidence::from_headers(&reversed_values).unwrap();
        assert_eq!(first, reordered);
        assert_ne!(first.sha256, reversed.sha256);
        assert_eq!(first.names, ["set-cookie", "x-first", "x-second"]);

        let mut too_many = http::HeaderMap::new();
        for _ in 0..=MAX_RESPONSE_HEADER_COUNT {
            too_many.append("x-value", http::HeaderValue::from_static("1"));
        }
        assert!(ResponseHeaderEvidence::from_headers(&too_many).is_err());
    }

    #[test]
    fn controlled_response_headers_are_identity_encoded_and_body_bound() {
        let request = ResourceRequest {
            method: "GET".into(),
            url: url::Url::parse("https://example.test/body").unwrap(),
            destination: "Script".into(),
            load_role: WebResourceLoadRole::DocumentContent,
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        };
        let mut encoded = http::HeaderMap::new();
        encoded.insert(
            http::header::CONTENT_ENCODING,
            http::HeaderValue::from_static("gzip"),
        );
        let error = normalize_controlled_response_headers(&request, encoded, 3).unwrap_err();
        assert_eq!(error.code, "RESOURCE_ENCODING_UNSUPPORTED");

        let mut identity = http::HeaderMap::new();
        identity.insert(
            http::header::CONTENT_ENCODING,
            http::HeaderValue::from_static("identity"),
        );
        identity.insert(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_static("999"),
        );
        let normalized = normalize_controlled_response_headers(&request, identity, 3).unwrap();
        assert!(!normalized.contains_key(http::header::CONTENT_ENCODING));
        assert_eq!(normalized[http::header::CONTENT_LENGTH], "3");

        let mut outbound = http::HeaderMap::new();
        outbound.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer secret"),
        );
        outbound.insert(
            http::header::COOKIE,
            http::HeaderValue::from_static("session=secret"),
        );
        outbound.insert(
            http::header::PROXY_AUTHORIZATION,
            http::HeaderValue::from_static("Basic secret"),
        );
        outbound.insert(
            http::header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("gzip"),
        );
        outbound.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("text/css,*/*;q=0.1"),
        );
        outbound.insert(
            http::header::ACCEPT_LANGUAGE,
            http::HeaderValue::from_static("en-US"),
        );
        outbound.insert(
            http::header::CONNECTION,
            http::HeaderValue::from_static("keep-alive"),
        );
        outbound.insert(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_static("123"),
        );
        outbound.insert(
            http::HeaderName::from_static("x-api-key"),
            http::HeaderValue::from_static("secret"),
        );
        let outbound = controlled_request_headers(&outbound);
        assert!(!outbound.contains_key(http::header::AUTHORIZATION));
        assert!(!outbound.contains_key(http::header::COOKIE));
        assert!(!outbound.contains_key(http::header::PROXY_AUTHORIZATION));
        assert!(!outbound.contains_key(http::header::CONNECTION));
        assert!(!outbound.contains_key(http::header::CONTENT_LENGTH));
        assert!(!outbound.contains_key("x-api-key"));
        assert_eq!(outbound[http::header::ACCEPT], "text/css,*/*;q=0.1");
        assert_eq!(outbound[http::header::ACCEPT_LANGUAGE], "en-US");
        assert_eq!(outbound[http::header::ACCEPT_ENCODING], "identity");
    }
}
