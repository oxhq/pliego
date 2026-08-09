/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Exact resource bytes owned by one direct document session.

use std::collections::BTreeMap;
use std::sync::Arc;

use data_url::DataUrl;
use data_url::forgiving_base64::DecodeError;
use pliego::{IMAGE_LIMITS, ImageLimit};

use super::asset_cache;
use super::resource_policy::{
    ControlledResource, ResourcePolicyFailure, ResourceRequest, sha256_hex,
};

const MAX_DATA_URL_OVERHEAD_BYTES: u64 = 4 * 1024;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RequestIdentity {
    method: String,
    url: String,
    destination: String,
}

impl RequestIdentity {
    fn new(request: &ResourceRequest) -> Self {
        Self {
            method: request.method.clone(),
            url: normalized_url(&request.url),
            destination: request.destination.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OwnedResource {
    status: u16,
    content_type: Option<String>,
    content_address: String,
    body: Arc<[u8]>,
}

impl OwnedResource {
    pub(crate) fn status(&self) -> u16 {
        self.status
    }

    pub(crate) fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    pub(crate) fn content_address(&self) -> &str {
        &self.content_address
    }

    pub(crate) fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImageCost {
    decoded_pixels: u64,
    decompressed_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OwnedResourceStore {
    requests: BTreeMap<RequestIdentity, OwnedResource>,
    url_to_resource: BTreeMap<String, String>,
    resources: BTreeMap<String, Arc<[u8]>>,
    image_costs: BTreeMap<String, ImageCost>,
    resident_bytes: u64,
    decoded_image_pixels: u64,
    decompressed_image_bytes: u64,
}

impl Default for OwnedResourceStore {
    fn default() -> Self {
        Self::new(0)
    }
}

impl OwnedResourceStore {
    pub(crate) fn new(reserved_bytes: u64) -> Self {
        Self {
            requests: BTreeMap::new(),
            url_to_resource: BTreeMap::new(),
            resources: BTreeMap::new(),
            image_costs: BTreeMap::new(),
            resident_bytes: reserved_bytes,
            decoded_image_pixels: 0,
            decompressed_image_bytes: 0,
        }
    }

    pub(crate) fn retain(
        &mut self,
        request: &ResourceRequest,
        resource: ControlledResource,
    ) -> Result<OwnedResource, ResourcePolicyFailure> {
        let identity = RequestIdentity::new(request);
        if resource.body.len() as u64 > asset_cache::MAX_CACHE_BYTES {
            return Err(resource_failure(
                request,
                "RESOURCE_DENIED",
                "denied",
                format!(
                    "controlled resource exceeds the {}-byte per-resource bound",
                    asset_cache::MAX_CACHE_BYTES
                ),
            ));
        }

        let ControlledResource {
            status,
            content_type,
            body,
        } = resource;
        let content_address = format!("sha256:{}", sha256_hex(&body));

        if let Some(existing) = self.requests.get(&identity) {
            return if existing.status == status
                && existing.content_type.as_deref() == content_type.as_deref()
                && existing.content_address == content_address
                && existing.body() == body
            {
                Ok(existing.clone())
            } else {
                Err(changed_resource_failure(request))
            };
        }

        if request.method != "HEAD"
            && self
                .url_to_resource
                .get(&identity.url)
                .is_some_and(|existing| existing != &content_address)
        {
            return Err(changed_resource_failure(request));
        }

        if self
            .resources
            .get(&content_address)
            .is_some_and(|existing| existing.as_ref() != body)
        {
            return Err(resource_failure(
                request,
                "RESOURCE_CONTENT_ADDRESS_COLLISION",
                "hash_collision",
                "different resource bytes produced the same SHA-256 content address".into(),
            ));
        }

        let image_cost = if request.method == "HEAD" || self.image_costs.contains_key(&identity.url)
        {
            None
        } else {
            image_cost(request, content_type.as_deref(), &body)?
        };
        let next_resident_bytes = if self.resources.contains_key(&content_address) {
            self.resident_bytes
        } else {
            self.resident_bytes
                .checked_add(body.len() as u64)
                .filter(|bytes| *bytes <= asset_cache::MAX_CACHE_BYTES)
                .ok_or_else(|| {
                    resource_failure(
                        request,
                        "RESOURCE_DENIED",
                        "denied",
                        format!(
                            "resident resources exceed the {}-byte aggregate bound",
                            asset_cache::MAX_CACHE_BYTES
                        ),
                    )
                })?
        };
        let (next_pixels, next_image_bytes) = match image_cost {
            Some(cost) => (
                checked_image_sum(
                    request,
                    ImageLimit::DocumentDecodedPixels,
                    self.decoded_image_pixels,
                    cost.decoded_pixels,
                    IMAGE_LIMITS.document_decoded_pixels,
                )?,
                checked_image_sum(
                    request,
                    ImageLimit::DocumentDecompressedBytes,
                    self.decompressed_image_bytes,
                    cost.decompressed_bytes,
                    IMAGE_LIMITS.document_decompressed_bytes,
                )?,
            ),
            None => (self.decoded_image_pixels, self.decompressed_image_bytes),
        };

        self.resident_bytes = next_resident_bytes;
        self.decoded_image_pixels = next_pixels;
        self.decompressed_image_bytes = next_image_bytes;
        let body = self
            .resources
            .get(&content_address)
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::from(body));
        self.resources
            .entry(content_address.clone())
            .or_insert_with(|| Arc::clone(&body));
        if let Some(cost) = image_cost {
            self.image_costs.insert(identity.url.clone(), cost);
        }
        if request.method != "HEAD" {
            self.url_to_resource
                .insert(identity.url.clone(), content_address.clone());
        }
        let owned = OwnedResource {
            status,
            content_type,
            content_address,
            body,
        };
        self.requests.insert(identity, owned.clone());
        Ok(owned)
    }

    pub(crate) fn resolve_url(&self, url: &str) -> Option<String> {
        let url = url::Url::parse(url).ok()?;
        self.url_to_resource.get(&normalized_url(&url)).cloned()
    }

    pub(crate) fn resolve_content(&self, resource: &str) -> Option<&[u8]> {
        self.resources.get(resource).map(AsRef::as_ref)
    }

    #[cfg(test)]
    pub(crate) fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }
}

pub(crate) fn decode_bounded_data_url(
    request: &ResourceRequest,
) -> Result<ControlledResource, ResourcePolicyFailure> {
    decode_data_url_with_limit(request, asset_cache::MAX_CACHE_BYTES)
}

fn decode_data_url_with_limit(
    request: &ResourceRequest,
    max_body_bytes: u64,
) -> Result<ControlledResource, ResourcePolicyFailure> {
    let source = request.url.as_str();
    let max_encoded_bytes = max_body_bytes
        .saturating_mul(4)
        .div_ceil(3)
        .saturating_add(MAX_DATA_URL_OVERHEAD_BYTES);
    if source.len() as u64 > max_encoded_bytes {
        return Err(resource_failure(
            request,
            "RESOURCE_DENIED",
            "denied",
            format!("data URL exceeds the {max_encoded_bytes}-byte encoded bound"),
        ));
    }
    let parsed = DataUrl::process(source).map_err(|error| {
        resource_failure(
            request,
            "RESOURCE_DATA_URL_INVALID",
            "invalid",
            format!("data URL cannot be parsed: {error}"),
        )
    })?;
    let content_type = parsed.mime_type().to_string();
    let mut body = Vec::new();
    if request.method != "HEAD" {
        match parsed.decode(|bytes| {
            let next = body
                .len()
                .checked_add(bytes.len())
                .filter(|bytes| *bytes as u64 <= max_body_bytes)
                .ok_or(())?;
            body.reserve(next - body.len());
            body.extend_from_slice(bytes);
            Ok(())
        }) {
            Ok(_) => {},
            Err(DecodeError::InvalidBase64(error)) => {
                return Err(resource_failure(
                    request,
                    "RESOURCE_DATA_URL_INVALID",
                    "invalid",
                    format!("data URL has an invalid base64 payload: {error}"),
                ));
            },
            Err(DecodeError::WriteError(())) => {
                return Err(resource_failure(
                    request,
                    "RESOURCE_DENIED",
                    "denied",
                    format!("decoded data URL exceeds the {max_body_bytes}-byte resource bound"),
                ));
            },
        }
    }
    Ok(ControlledResource {
        status: 200,
        content_type: Some(content_type),
        body,
    })
}

fn image_cost(
    request: &ResourceRequest,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<Option<ImageCost>, ResourcePolicyFailure> {
    let media_type = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    let is_image = request.destination == "Image"
        || media_type.is_some_and(|value| value.starts_with("image/"));
    if !is_image || media_type.is_some_and(|value| value.eq_ignore_ascii_case("image/svg+xml")) {
        return Ok(None);
    }

    let decompressed_bytes_per_pixel = if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        if body.get(24) == Some(&16) { 8 } else { 4 }
    } else if body.starts_with(b"GIF87a")
        || body.starts_with(b"GIF89a")
        || (body.len() >= 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP")
    {
        4
    } else if body.starts_with(b"\xff\xd8\xff") {
        // Servo decodes the image before capture even though Krilla can retain JPEG bytes.
        4
    } else {
        return Err(resource_failure(
            request,
            "RESOURCE_IMAGE_INVALID",
            "invalid",
            "raster image is not PNG, JPEG, GIF, or WebP".into(),
        ));
    };
    let dimensions = imagesize::blob_size(body).map_err(|error| {
        resource_failure(
            request,
            "RESOURCE_IMAGE_INVALID",
            "invalid",
            format!("raster image dimensions are invalid: {error}"),
        )
    })?;
    let width = u64::try_from(dimensions.width).unwrap_or(u64::MAX);
    let height = u64::try_from(dimensions.height).unwrap_or(u64::MAX);
    check_image_limit(
        request,
        ImageLimit::DeclaredWidth,
        IMAGE_LIMITS.declared_dimension,
        width,
    )?;
    check_image_limit(
        request,
        ImageLimit::DeclaredHeight,
        IMAGE_LIMITS.declared_dimension,
        height,
    )?;
    let decoded_pixels = width.checked_mul(height).unwrap_or(u64::MAX);
    check_image_limit(
        request,
        ImageLimit::DecodedPixels,
        IMAGE_LIMITS.decoded_pixels,
        decoded_pixels,
    )?;
    let decompressed_bytes = decoded_pixels
        .checked_mul(decompressed_bytes_per_pixel)
        .unwrap_or(u64::MAX);
    check_image_limit(
        request,
        ImageLimit::DecompressedBytes,
        IMAGE_LIMITS.decompressed_bytes,
        decompressed_bytes,
    )?;
    Ok(Some(ImageCost {
        decoded_pixels,
        decompressed_bytes,
    }))
}

fn checked_image_sum(
    request: &ResourceRequest,
    limit: ImageLimit,
    current: u64,
    additional: u64,
    configured: u64,
) -> Result<u64, ResourcePolicyFailure> {
    let observed = current.checked_add(additional).unwrap_or(u64::MAX);
    check_image_limit(request, limit, configured, observed)?;
    Ok(observed)
}

fn check_image_limit(
    request: &ResourceRequest,
    limit: ImageLimit,
    configured: u64,
    observed: u64,
) -> Result<(), ResourcePolicyFailure> {
    if observed <= configured {
        Ok(())
    } else {
        Err(resource_failure(
            request,
            "RESOURCE_IMAGE_LIMIT_EXCEEDED",
            "denied",
            format!(
                "raster image exceeds the {limit} limit (configured {configured}, observed {observed})"
            ),
        ))
    }
}

fn normalized_url(url: &url::Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    normalized.to_string()
}

fn changed_resource_failure(request: &ResourceRequest) -> ResourcePolicyFailure {
    resource_failure(
        request,
        "RESOURCE_CHANGED_DURING_RENDER",
        "changed",
        "controlled URL returned different bytes or metadata during one render".into(),
    )
}

fn resource_failure(
    request: &ResourceRequest,
    code: &'static str,
    status: &'static str,
    reason: String,
) -> ResourcePolicyFailure {
    ResourcePolicyFailure::new(request, code, status, reason)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    use super::*;

    fn request(method: &str, url: &str, destination: &str) -> ResourceRequest {
        ResourceRequest {
            method: method.into(),
            url: url::Url::parse(url).unwrap(),
            destination: destination.into(),
            referrer_url: None,
            is_for_main_frame: false,
            is_redirect: false,
        }
    }

    fn resource(content_type: &str, body: impl Into<Vec<u8>>) -> ControlledResource {
        ControlledResource {
            status: 200,
            content_type: Some(content_type.into()),
            body: body.into(),
        }
    }

    fn fixture_png() -> Vec<u8> {
        BASE64_STANDARD
            .decode(
                b"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
            )
            .unwrap()
    }

    #[test]
    fn exact_bytes_are_shared_and_url_fragments_use_one_fetch_identity() {
        let mut store = OwnedResourceStore::default();
        let first_request = request("GET", "https://example.test/a.bin#first", "Script");
        let second_request = request("GET", "https://example.test/b.bin", "Script");
        let first = store
            .retain(
                &first_request,
                resource("application/octet-stream", b"same".to_vec()),
            )
            .unwrap();
        let second = store
            .retain(
                &second_request,
                resource("application/octet-stream", b"same".to_vec()),
            )
            .unwrap();

        assert_eq!(first.content_address(), second.content_address());
        assert!(Arc::ptr_eq(&first.body, &second.body));
        assert_eq!(
            store.resolve_url("https://example.test/a.bin#second"),
            Some(first.content_address().into())
        );
        assert_eq!(store.resources.len(), 1);
        assert_eq!(store.resident_bytes(), 4);
    }

    #[test]
    fn changed_url_and_aggregate_overflow_fail_without_mutating_the_store() {
        let mut changed = OwnedResourceStore::default();
        let first_request = request("GET", "https://example.test/a.bin", "Script");
        changed
            .retain(
                &first_request,
                resource("application/octet-stream", b"one".to_vec()),
            )
            .unwrap();
        let error = changed
            .retain(
                &first_request,
                resource("application/octet-stream", b"two".to_vec()),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_CHANGED_DURING_RENDER");
        assert_eq!(changed.requests.len(), 1);
        assert_eq!(changed.resident_bytes(), 3);

        let mut full = OwnedResourceStore::new(asset_cache::MAX_CACHE_BYTES - 1);
        let error = full
            .retain(
                &request("GET", "https://example.test/two.bin", "Script"),
                resource("application/octet-stream", b"12".to_vec()),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_DENIED");
        assert!(full.requests.is_empty());
        assert!(full.resources.is_empty());
        assert_eq!(full.resident_bytes(), asset_cache::MAX_CACHE_BYTES - 1);
    }

    #[test]
    fn data_urls_are_decoded_exactly_and_bounded_before_interception() {
        let percent = decode_data_url_with_limit(
            &request("GET", "data:text/plain,hello%20world", "Script"),
            64,
        )
        .unwrap();
        assert_eq!(percent.content_type.as_deref(), Some("text/plain"));
        assert_eq!(percent.body, b"hello world");

        let base64 = decode_data_url_with_limit(
            &request("GET", "data:application/octet-stream;base64,AAEC", "Image"),
            64,
        )
        .unwrap();
        assert_eq!(base64.body, [0, 1, 2]);

        let invalid =
            decode_data_url_with_limit(&request("GET", "data:image/png;base64,!", "Image"), 64)
                .unwrap_err();
        assert_eq!(invalid.code, "RESOURCE_DATA_URL_INVALID");

        let oversized =
            decode_data_url_with_limit(&request("GET", "data:text/plain,12345", "Script"), 4)
                .unwrap_err();
        assert_eq!(oversized.code, "RESOURCE_DENIED");
    }

    #[test]
    fn raster_dimensions_and_document_decode_cost_are_bounded_transactionally() {
        let mut store = OwnedResourceStore::default();
        let valid = fixture_png();
        store
            .retain(
                &request("GET", "https://example.test/valid.png", "Image"),
                resource("image/png", valid.clone()),
            )
            .unwrap();

        let mut too_wide = valid.clone();
        too_wide[16..20].copy_from_slice(&16_385_u32.to_be_bytes());
        let error = store
            .retain(
                &request("GET", "https://example.test/wide.png", "Image"),
                resource("image/png", too_wide.clone()),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_IMAGE_LIMIT_EXCEEDED");
        assert_eq!(store.image_costs.len(), 1);

        let mut changed_destination = OwnedResourceStore::default();
        changed_destination
            .retain(
                &request("GET", "https://example.test/destination.png", "Script"),
                resource("application/octet-stream", too_wide.clone()),
            )
            .unwrap();
        let error = changed_destination
            .retain(
                &request("GET", "https://example.test/destination.png", "Image"),
                resource("application/octet-stream", too_wide),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_IMAGE_LIMIT_EXCEEDED");

        let mut first_large = valid.clone();
        first_large[16..20].copy_from_slice(&8_192_u32.to_be_bytes());
        first_large[20..24].copy_from_slice(&8_192_u32.to_be_bytes());
        first_large.push(1);
        let second_large = first_large.clone();
        let mut aggregate = OwnedResourceStore::default();
        aggregate
            .retain(
                &request("GET", "https://example.test/large-a.png", "Image"),
                resource("image/png", first_large),
            )
            .unwrap();
        let error = aggregate
            .retain(
                &request("GET", "https://example.test/large-b.png", "Image"),
                resource("image/png", second_large),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_IMAGE_LIMIT_EXCEEDED");
        assert!(error.reason.contains("document-decoded-pixel"));
        assert_eq!(aggregate.resources.len(), 1);
        assert_eq!(aggregate.image_costs.len(), 1);
    }
}
