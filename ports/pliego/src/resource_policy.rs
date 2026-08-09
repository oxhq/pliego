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
    Unavailable(String),
    TooLarge,
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResourceRequest {
    pub(crate) method: String,
    pub(crate) url: url::Url,
    pub(crate) destination: String,
    pub(crate) referrer_url: Option<url::Url>,
    pub(crate) is_for_main_frame: bool,
    pub(crate) is_redirect: bool,
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
    Allow,
    FetchHttp,
    Synthesize {
        body: Vec<u8>,
        content_type: &'static str,
    },
    Fail(ResourcePolicyFailure),
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl ResourcePolicy {
    pub(crate) fn resolve(config: &ResourcePolicyConfig, document_root: &Path) -> Self {
        let virtual_resources = config
            .virtual_resources
            .iter()
            .map(|resource| {
                let path = if resource.path.is_absolute() {
                    resource.path.clone()
                } else {
                    document_root.join(&resource.path)
                };
                VirtualResource {
                    url: resource.url.clone(),
                    body: read_bounded_local_resource(&path),
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
        let (assets, asset_error) = match asset_manifest.as_deref() {
            Some(path) => match asset_cache::AssetStore::load(path) {
                Ok(assets) => (Some(assets), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        Self {
            allowed_http_roots: config.allowed_http_roots.clone(),
            virtual_resources,
            asset_manifest,
            assets,
            asset_error,
            timeout_ms: config.timeout_ms,
        }
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

        let synthesize = |body: &[u8], content_type| ResourcePolicyDecision::Synthesize {
            body: if request.method == "HEAD" {
                Vec::new()
            } else {
                body.to_vec()
            },
            content_type,
        };

        if let Some(resource) = self
            .assets
            .as_ref()
            .and_then(|assets| assets.get(&request.url))
        {
            return synthesize(&resource.body, resource_content_type(&resource.url));
        }

        if let Some(resource) = self
            .virtual_resources
            .iter()
            .find(|resource| resource.url == request.url)
        {
            return match &resource.body {
                Ok(body) => synthesize(body, resource.content_type),
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
            "data" => ResourcePolicyDecision::Allow,
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
                            Ok(body) => synthesize(&body, resource_content_type(&request.url)),
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
    let metadata = path
        .metadata()
        .map_err(|error| LocalResourceReadError::Unavailable(error.to_string()))?;
    if metadata.len() > asset_cache::MAX_CACHE_BYTES {
        return Err(LocalResourceReadError::TooLarge);
    }

    let file = std::fs::File::open(path)
        .map_err(|error| LocalResourceReadError::Unavailable(error.to_string()))?;
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

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
        let ResourcePolicyDecision::Synthesize { body, content_type } =
            policy.decide(&root, &request("GET", inside.clone(), false))
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
}
