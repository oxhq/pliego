/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Exact resource bytes owned by one direct document session.

use std::collections::BTreeMap;
use std::sync::Arc;

use data_url::DataUrl;
use data_url::forgiving_base64::DecodeError;
use embedder_traits::WebResourceLoadRole;
use pliego::{IMAGE_LIMITS, ImageLimit};

use super::asset_cache;
use super::resource_policy::{
    ControlledResource, MAX_RESOURCE_EVENTS, MAX_RESOURCE_METADATA_BYTES, ResourcePolicyFailure,
    ResourceRequest, ResponseHeaderEvidence, normalized_url, sha256_hex,
};

const MAX_DATA_URL_OVERHEAD_BYTES: u64 = 4 * 1024;
const METADATA_ENTRY_OVERHEAD_BYTES: u64 = 256;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RequestIdentity {
    method: String,
    url: String,
    destination: String,
    load_role: WebResourceLoadRole,
}

impl RequestIdentity {
    fn metadata_bytes(&self) -> u64 {
        (self.method.len() + self.url.len() + self.destination.len()) as u64
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResponseIdentity {
    status: u16,
    content_type: Option<String>,
    content_address: String,
    response_headers: ResponseHeaderEvidence,
}

impl ResponseIdentity {
    fn metadata_bytes(&self) -> u64 {
        self.content_type.as_ref().map_or(0, String::len) as u64 +
            self.content_address.len() as u64 +
            self.response_headers.retained_metadata_bytes()
    }
}

impl RequestIdentity {
    fn new(request: &ResourceRequest) -> Self {
        Self {
            method: request.method.clone(),
            url: normalized_url(&request.url),
            destination: request.destination.clone(),
            load_role: request.load_role,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OwnedResource {
    status: u16,
    content_type: Option<String>,
    content_address: String,
    response_headers: ResponseHeaderEvidence,
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

    pub(crate) fn response_headers(&self) -> &ResponseHeaderEvidence {
        &self.response_headers
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GifInfo {
    frames: usize,
    logical_width: u64,
    logical_height: u64,
    max_frame_width: u64,
    max_frame_height: u64,
    max_frame_pixels: u64,
    rectangles_within_screen: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OwnedResourceStore {
    requests: BTreeMap<RequestIdentity, OwnedResource>,
    url_to_resource: BTreeMap<String, ResponseIdentity>,
    resources: BTreeMap<String, Arc<[u8]>>,
    image_costs: BTreeMap<String, ImageCost>,
    resident_bytes: u64,
    decoded_image_pixels: u64,
    decompressed_image_bytes: u64,
    metadata_bytes: u64,
    max_identities: usize,
    max_metadata_bytes: u64,
}

impl Default for OwnedResourceStore {
    fn default() -> Self {
        Self::new(0)
    }
}

impl OwnedResourceStore {
    pub(crate) fn new(reserved_bytes: u64) -> Self {
        Self::with_limits(
            reserved_bytes,
            MAX_RESOURCE_EVENTS,
            MAX_RESOURCE_METADATA_BYTES,
        )
    }

    fn with_limits(reserved_bytes: u64, max_identities: usize, max_metadata_bytes: u64) -> Self {
        Self {
            requests: BTreeMap::new(),
            url_to_resource: BTreeMap::new(),
            resources: BTreeMap::new(),
            image_costs: BTreeMap::new(),
            resident_bytes: reserved_bytes,
            decoded_image_pixels: 0,
            decompressed_image_bytes: 0,
            metadata_bytes: 0,
            max_identities,
            max_metadata_bytes,
        }
    }

    pub(crate) fn retain(
        &mut self,
        request: &ResourceRequest,
        resource: ControlledResource,
        headers: &http::HeaderMap,
    ) -> Result<OwnedResource, ResourcePolicyFailure> {
        let identity = RequestIdentity::new(request);
        let response_headers = ResponseHeaderEvidence::from_headers(headers).map_err(|reason| {
            resource_failure(
                request,
                "RESOURCE_METADATA_LIMIT_EXCEEDED",
                "denied",
                reason,
            )
        })?;
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
        let header_content_type = headers
            .get(http::header::CONTENT_TYPE)
            .map(|value| value.to_str())
            .transpose()
            .map_err(|error| {
                resource_failure(
                    request,
                    "RESOURCE_METADATA_INVALID",
                    "invalid",
                    format!("response Content-Type header is invalid: {error}"),
                )
            })?;
        if header_content_type != content_type.as_deref() {
            return Err(resource_failure(
                request,
                "RESOURCE_METADATA_INVALID",
                "invalid",
                "response Content-Type metadata does not match the intercepted headers".into(),
            ));
        }
        let content_address = format!("sha256:{}", sha256_hex(&body));

        if let Some(existing) = self.requests.get(&identity) {
            return if existing.status == status &&
                existing.content_type.as_deref() == content_type.as_deref() &&
                existing.content_address == content_address &&
                existing.response_headers == response_headers &&
                existing.body() == body
            {
                Ok(existing.clone())
            } else {
                Err(changed_resource_failure(request))
            };
        }

        let response_identity = ResponseIdentity {
            status,
            content_type: content_type.clone(),
            content_address: content_address.clone(),
            response_headers: response_headers.clone(),
        };
        if request.method != "HEAD" &&
            self.url_to_resource
                .get(&identity.url)
                .is_some_and(|existing| existing != &response_identity)
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
        if self.requests.len() >= self.max_identities {
            return Err(resource_failure(
                request,
                "RESOURCE_METADATA_LIMIT_EXCEEDED",
                "denied",
                format!(
                    "controlled resources exceed the {}-identity bound",
                    self.max_identities
                ),
            ));
        }
        let response_metadata_bytes = response_identity.metadata_bytes();
        let mut additional_metadata_bytes = identity
            .metadata_bytes()
            .saturating_add(response_metadata_bytes)
            .saturating_add(METADATA_ENTRY_OVERHEAD_BYTES);
        if request.method != "HEAD" && !self.url_to_resource.contains_key(&identity.url) {
            additional_metadata_bytes = additional_metadata_bytes
                .saturating_add(identity.url.len() as u64)
                .saturating_add(response_metadata_bytes)
                .saturating_add(METADATA_ENTRY_OVERHEAD_BYTES);
        }
        if !self.resources.contains_key(&content_address) {
            additional_metadata_bytes = additional_metadata_bytes
                .saturating_add(content_address.len() as u64)
                .saturating_add(METADATA_ENTRY_OVERHEAD_BYTES);
        }
        if image_cost.is_some() {
            additional_metadata_bytes = additional_metadata_bytes
                .saturating_add(identity.url.len() as u64)
                .saturating_add(METADATA_ENTRY_OVERHEAD_BYTES);
        }
        let next_metadata_bytes = self
            .metadata_bytes
            .checked_add(additional_metadata_bytes)
            .filter(|bytes| *bytes <= self.max_metadata_bytes)
            .ok_or_else(|| {
                resource_failure(
                    request,
                    "RESOURCE_METADATA_LIMIT_EXCEEDED",
                    "denied",
                    format!(
                        "controlled resource metadata exceeds the {}-byte aggregate bound",
                        self.max_metadata_bytes
                    ),
                )
            })?;

        self.resident_bytes = next_resident_bytes;
        self.decoded_image_pixels = next_pixels;
        self.decompressed_image_bytes = next_image_bytes;
        self.metadata_bytes = next_metadata_bytes;
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
                .insert(identity.url.clone(), response_identity);
        }
        let owned = OwnedResource {
            status,
            content_type,
            content_address,
            response_headers,
            body,
        };
        self.requests.insert(identity, owned.clone());
        Ok(owned)
    }

    pub(crate) fn resolve_url(&self, url: &str) -> Option<String> {
        let url = url::Url::parse(url).ok()?;
        self.url_to_resource
            .get(&normalized_url(&url))
            .map(|resource| resource.content_address.clone())
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
    let declares_image = media_type.is_some_and(|value| value.starts_with("image/"));
    if (!declares_image && request.destination != "Image") ||
        media_type.is_some_and(|value| value.eq_ignore_ascii_case("image/svg+xml"))
    {
        return Ok(None);
    }

    let decompressed_bytes_per_pixel = if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        if body.get(24) == Some(&16) { 8 } else { 4 }
    } else if body.starts_with(b"GIF87a") ||
        body.starts_with(b"GIF89a") ||
        (body.len() >= 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP")
    {
        4
    } else if body.starts_with(b"\xff\xd8\xff") {
        // Servo decodes the image before capture even though Krilla can retain JPEG bytes.
        4
    } else if !declares_image && media_type.is_some() {
        // Fetch destinations describe browser intent, not the response body. For example,
        // `<link rel="icon" href="data:,">` is requested as Image but is an empty
        // text/plain resource. Servo treats it as a broken, non-rendered image; retain its
        // exact bytes and evidence, but do not invent raster decode cost for it.
        return Ok(None);
    } else {
        return Err(resource_failure(
            request,
            "RESOURCE_IMAGE_INVALID",
            "invalid",
            "raster image is not PNG, JPEG, GIF, or WebP".into(),
        ));
    };
    let gif = if body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a") {
        Some(gif_info(body).map_err(|reason| {
            resource_failure(
                request,
                "RESOURCE_IMAGE_INVALID",
                "invalid",
                format!("raster image container is invalid: {reason}"),
            )
        })?)
    } else {
        None
    };
    let has_multiple_frames = match gif {
        Some(info) => info.frames > 1,
        None => raster_has_multiple_frames(body).map_err(|reason| {
            resource_failure(
                request,
                "RESOURCE_IMAGE_INVALID",
                "invalid",
                format!("raster image container is invalid: {reason}"),
            )
        })?,
    };
    if has_multiple_frames {
        return Err(resource_failure(
            request,
            "RESOURCE_IMAGE_ANIMATION_UNSUPPORTED",
            "unsupported",
            "animated raster images require deterministic document-time settlement".into(),
        ));
    }
    let (width, height) = match gif {
        Some(info) => (info.logical_width, info.logical_height),
        None => {
            let dimensions = imagesize::blob_size(body).map_err(|error| {
                resource_failure(
                    request,
                    "RESOURCE_IMAGE_INVALID",
                    "invalid",
                    format!("raster image dimensions are invalid: {error}"),
                )
            })?;
            (
                u64::try_from(dimensions.width).unwrap_or(u64::MAX),
                u64::try_from(dimensions.height).unwrap_or(u64::MAX),
            )
        },
    };
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
    if let Some(info) = gif {
        check_image_limit(
            request,
            ImageLimit::DeclaredWidth,
            IMAGE_LIMITS.declared_dimension,
            info.max_frame_width,
        )?;
        check_image_limit(
            request,
            ImageLimit::DeclaredHeight,
            IMAGE_LIMITS.declared_dimension,
            info.max_frame_height,
        )?;
        check_image_limit(
            request,
            ImageLimit::DecodedPixels,
            IMAGE_LIMITS.decoded_pixels,
            info.max_frame_pixels,
        )?;
        if !info.rectangles_within_screen {
            return Err(resource_failure(
                request,
                "RESOURCE_IMAGE_INVALID",
                "invalid",
                "GIF image descriptor extends beyond its logical screen".into(),
            ));
        }
    }
    let decoded_pixels = width.checked_mul(height).unwrap_or(u64::MAX);
    check_image_limit(
        request,
        ImageLimit::DecodedPixels,
        IMAGE_LIMITS.decoded_pixels,
        decoded_pixels,
    )?;
    let decompressed_bytes = match gif {
        // image's GIF AnimationDecoder retains the prior logical screen, allocates the
        // descriptor buffer, and may allocate a second logical screen while compositing.
        Some(info) => decoded_pixels
            .checked_mul(4)
            .and_then(|screen| screen.checked_mul(2))
            .and_then(|screen| {
                info.max_frame_pixels
                    .checked_mul(4)
                    .and_then(|frame| screen.checked_add(frame))
            })
            .unwrap_or(u64::MAX),
        None => decoded_pixels
            .checked_mul(decompressed_bytes_per_pixel)
            .unwrap_or(u64::MAX),
    };
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

fn raster_has_multiple_frames(body: &[u8]) -> Result<bool, &'static str> {
    if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        png_has_multiple_frames(body)
    } else if body.len() >= 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP" {
        webp_has_multiple_frames(body)
    } else {
        Ok(false)
    }
}

fn png_has_multiple_frames(body: &[u8]) -> Result<bool, &'static str> {
    let mut offset = 8usize;
    while offset < body.len() {
        let header_end = offset.checked_add(8).ok_or("PNG chunk offset overflow")?;
        if header_end > body.len() {
            return Err("truncated PNG chunk header");
        }
        let length = u32::from_be_bytes(
            body[offset..offset + 4]
                .try_into()
                .map_err(|_| "truncated PNG chunk length")?,
        ) as usize;
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
            .ok_or("PNG chunk length overflow")?;
        if chunk_end > body.len() {
            return Err("truncated PNG chunk body");
        }
        let kind = &body[offset + 4..header_end];
        if kind == b"acTL" {
            if length != 8 {
                return Err("invalid APNG animation-control chunk");
            }
            let frames = u32::from_be_bytes(
                body[header_end..header_end + 4]
                    .try_into()
                    .map_err(|_| "truncated APNG frame count")?,
            );
            if frames == 0 {
                return Err("APNG declares zero frames");
            }
            return Ok(frames > 1);
        }
        if kind == b"IEND" {
            return Ok(false);
        }
        offset = chunk_end;
    }
    Err("PNG has no end chunk")
}

fn gif_info(body: &[u8]) -> Result<GifInfo, &'static str> {
    if body.len() < 13 {
        return Err("truncated GIF logical screen descriptor");
    }
    let logical_width = u64::from(u16::from_le_bytes([body[6], body[7]]));
    let logical_height = u64::from(u16::from_le_bytes([body[8], body[9]]));
    if logical_width == 0 || logical_height == 0 {
        return Err("GIF logical screen dimensions are zero");
    }
    let packed = body[10];
    let mut offset = 13usize;
    if packed & 0x80 != 0 {
        let table_bytes = 3usize
            .checked_mul(1usize << ((packed & 0x07) + 1))
            .ok_or("GIF global color table overflow")?;
        offset = offset
            .checked_add(table_bytes)
            .ok_or("GIF global color table overflow")?;
        if offset > body.len() {
            return Err("truncated GIF global color table");
        }
    }

    let mut frames = 0usize;
    let mut max_frame_width = 0u64;
    let mut max_frame_height = 0u64;
    let mut max_frame_pixels = 0u64;
    let mut rectangles_within_screen = true;
    loop {
        let marker = *body.get(offset).ok_or("GIF has no trailer")?;
        offset += 1;
        match marker {
            0x2c => {
                frames += 1;
                let descriptor_end = offset
                    .checked_add(9)
                    .ok_or("GIF image descriptor overflow")?;
                if descriptor_end > body.len() {
                    return Err("truncated GIF image descriptor");
                }
                let left = u64::from(u16::from_le_bytes([body[offset], body[offset + 1]]));
                let top = u64::from(u16::from_le_bytes([body[offset + 2], body[offset + 3]]));
                let width = u64::from(u16::from_le_bytes([body[offset + 4], body[offset + 5]]));
                let height = u64::from(u16::from_le_bytes([body[offset + 6], body[offset + 7]]));
                if width == 0 || height == 0 {
                    return Err("GIF image descriptor dimensions are zero");
                }
                let pixels = width
                    .checked_mul(height)
                    .ok_or("GIF image descriptor pixel count overflow")?;
                max_frame_width = max_frame_width.max(width);
                max_frame_height = max_frame_height.max(height);
                max_frame_pixels = max_frame_pixels.max(pixels);
                rectangles_within_screen &= left
                    .checked_add(width)
                    .is_some_and(|right| right <= logical_width) &&
                    top.checked_add(height)
                        .is_some_and(|bottom| bottom <= logical_height);
                let packed = body[offset + 8];
                offset = descriptor_end;
                if packed & 0x80 != 0 {
                    let table_bytes = 3usize
                        .checked_mul(1usize << ((packed & 0x07) + 1))
                        .ok_or("GIF local color table overflow")?;
                    offset = offset
                        .checked_add(table_bytes)
                        .ok_or("GIF local color table overflow")?;
                    if offset > body.len() {
                        return Err("truncated GIF local color table");
                    }
                }
                offset = offset.checked_add(1).ok_or("GIF image data overflow")?;
                if offset > body.len() {
                    return Err("truncated GIF LZW code size");
                }
                offset = skip_gif_sub_blocks(body, offset)?;
            },
            0x21 => {
                offset = offset.checked_add(1).ok_or("GIF extension overflow")?;
                if offset > body.len() {
                    return Err("truncated GIF extension label");
                }
                offset = skip_gif_sub_blocks(body, offset)?;
            },
            0x3b => {
                return Ok(GifInfo {
                    frames,
                    logical_width,
                    logical_height,
                    max_frame_width,
                    max_frame_height,
                    max_frame_pixels,
                    rectangles_within_screen,
                });
            },
            _ => return Err("invalid GIF block marker"),
        }
    }
}

fn skip_gif_sub_blocks(body: &[u8], mut offset: usize) -> Result<usize, &'static str> {
    loop {
        let length = *body.get(offset).ok_or("truncated GIF sub-block")? as usize;
        offset += 1;
        if length == 0 {
            return Ok(offset);
        }
        offset = offset
            .checked_add(length)
            .ok_or("GIF sub-block length overflow")?;
        if offset > body.len() {
            return Err("truncated GIF sub-block body");
        }
    }
}

fn webp_has_multiple_frames(body: &[u8]) -> Result<bool, &'static str> {
    if body.len() < 12 {
        return Err("truncated WebP header");
    }
    let declared =
        u32::from_le_bytes(body[4..8].try_into().map_err(|_| "truncated WebP length")?) as usize;
    let end = declared
        .checked_add(8)
        .ok_or("WebP container length overflow")?;
    if end > body.len() || end < 12 {
        return Err("truncated WebP container");
    }

    let mut offset = 12usize;
    while offset < end {
        let header_end = offset.checked_add(8).ok_or("WebP chunk overflow")?;
        if header_end > end {
            return Err("truncated WebP chunk header");
        }
        let kind = &body[offset..offset + 4];
        let length = u32::from_le_bytes(
            body[offset + 4..header_end]
                .try_into()
                .map_err(|_| "truncated WebP chunk length")?,
        ) as usize;
        let payload_end = header_end
            .checked_add(length)
            .ok_or("WebP chunk length overflow")?;
        if payload_end > end {
            return Err("truncated WebP chunk body");
        }
        if kind == b"ANIM" || kind == b"ANMF" {
            return Ok(true);
        }
        if kind == b"VP8X" && length >= 1 && body[header_end] & 0x02 != 0 {
            return Ok(true);
        }
        offset = payload_end
            .checked_add(length & 1)
            .ok_or("WebP padding overflow")?;
        if offset > end {
            return Err("truncated WebP chunk padding");
        }
    }
    Ok(false)
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
            load_role: WebResourceLoadRole::DocumentContent,
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

    fn headers(content_type: &str) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_str(content_type).unwrap(),
        );
        headers
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
                &headers("application/octet-stream"),
            )
            .unwrap();
        let second = store
            .retain(
                &second_request,
                resource("application/octet-stream", b"same".to_vec()),
                &headers("application/octet-stream"),
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
                &headers("application/octet-stream"),
            )
            .unwrap();
        let error = changed
            .retain(
                &first_request,
                resource("application/octet-stream", b"two".to_vec()),
                &headers("application/octet-stream"),
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
                &headers("application/octet-stream"),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_DENIED");
        assert!(full.requests.is_empty());
        assert!(full.resources.is_empty());
        assert_eq!(full.resident_bytes(), asset_cache::MAX_CACHE_BYTES - 1);
    }

    #[test]
    fn response_headers_and_cross_destination_metadata_are_one_url_identity() {
        let mut store = OwnedResourceStore::default();
        let url = "https://example.test/stable.css";
        let mut original_headers = headers("text/css");
        original_headers.insert(
            http::header::CONTENT_SECURITY_POLICY,
            http::HeaderValue::from_static("default-src 'none'"),
        );
        store
            .retain(
                &request("GET", url, "Style"),
                resource("text/css", b"body {}".to_vec()),
                &original_headers,
            )
            .unwrap();

        let mut changed_headers = headers("text/css");
        changed_headers.insert(
            http::header::CONTENT_SECURITY_POLICY,
            http::HeaderValue::from_static("default-src *"),
        );
        let error = store
            .retain(
                &request("GET", url, "Style"),
                resource("text/css", b"body {}".to_vec()),
                &changed_headers,
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_CHANGED_DURING_RENDER");

        let mut changed_status = resource("text/css", b"body {}".to_vec());
        changed_status.status = 201;
        let error = store
            .retain(
                &request("GET", url, "Image"),
                changed_status,
                &original_headers,
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_CHANGED_DURING_RENDER");
        assert_eq!(store.requests.len(), 1);
    }

    #[test]
    fn repeated_url_across_destinations_charges_the_url_entry_once() {
        let url = "https://example.test/shared.css";
        let first_request = request("GET", url, "Style");
        let second_request = request("GET", url, "Script");
        let body = b"body {}".to_vec();
        let response_headers = ResponseHeaderEvidence::from_headers(&headers("text/css")).unwrap();
        let response_identity = ResponseIdentity {
            status: 200,
            content_type: Some("text/css".into()),
            content_address: format!("sha256:{}", sha256_hex(&body)),
            response_headers,
        };

        let mut probe = OwnedResourceStore::default();
        probe
            .retain(
                &first_request,
                resource("text/css", body.clone()),
                &headers("text/css"),
            )
            .unwrap();
        let exact_limit = probe
            .metadata_bytes
            .checked_add(RequestIdentity::new(&second_request).metadata_bytes())
            .and_then(|bytes| bytes.checked_add(response_identity.metadata_bytes()))
            .and_then(|bytes| bytes.checked_add(METADATA_ENTRY_OVERHEAD_BYTES))
            .unwrap();

        let mut bounded = OwnedResourceStore::with_limits(0, 2, exact_limit);
        bounded
            .retain(
                &first_request,
                resource("text/css", body.clone()),
                &headers("text/css"),
            )
            .unwrap();
        bounded
            .retain(
                &second_request,
                resource("text/css", body),
                &headers("text/css"),
            )
            .unwrap();
        assert_eq!(bounded.metadata_bytes, exact_limit);
        assert_eq!(bounded.url_to_resource.len(), 1);
        assert_eq!(bounded.requests.len(), 2);
    }

    #[test]
    fn request_identity_and_metadata_are_bounded_before_insertion() {
        let mut identities = OwnedResourceStore::with_limits(0, 1, u64::MAX);
        identities
            .retain(
                &request("GET", "https://example.test/one", "Script"),
                resource("text/javascript", Vec::new()),
                &headers("text/javascript"),
            )
            .unwrap();
        let error = identities
            .retain(
                &request("GET", "https://example.test/two", "Script"),
                resource("text/javascript", Vec::new()),
                &headers("text/javascript"),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_METADATA_LIMIT_EXCEEDED");
        assert_eq!(identities.requests.len(), 1);

        let mut metadata = OwnedResourceStore::with_limits(0, usize::MAX, 1);
        let error = metadata
            .retain(
                &request("GET", "https://example.test/metadata", "Script"),
                resource("text/javascript", Vec::new()),
                &headers("text/javascript"),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_METADATA_LIMIT_EXCEEDED");
        assert!(metadata.requests.is_empty());
        assert_eq!(metadata.metadata_bytes, 0);
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
    fn non_image_response_to_image_destination_is_owned_without_raster_decode_cost() {
        let mut store = OwnedResourceStore::default();
        let favicon = request("GET", "data:,", "Image");
        let owned = store
            .retain(
                &favicon,
                resource("text/plain;charset=US-ASCII", Vec::new()),
                &headers("text/plain;charset=US-ASCII"),
            )
            .unwrap();

        assert!(owned.body().is_empty());
        assert_eq!(store.requests.len(), 1);
        assert!(store.image_costs.is_empty());
        assert_eq!(store.decoded_image_pixels, 0);
        assert_eq!(store.decompressed_image_bytes, 0);
    }

    #[test]
    fn raster_dimensions_and_document_decode_cost_are_bounded_transactionally() {
        let mut store = OwnedResourceStore::default();
        let valid = fixture_png();
        store
            .retain(
                &request("GET", "https://example.test/valid.png", "Image"),
                resource("image/png", valid.clone()),
                &headers("image/png"),
            )
            .unwrap();

        let mut too_wide = valid.clone();
        too_wide[16..20].copy_from_slice(&16_385_u32.to_be_bytes());
        let error = store
            .retain(
                &request("GET", "https://example.test/wide.png", "Image"),
                resource("image/png", too_wide.clone()),
                &headers("image/png"),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_IMAGE_LIMIT_EXCEEDED");
        assert_eq!(store.image_costs.len(), 1);

        let mut changed_destination = OwnedResourceStore::default();
        changed_destination
            .retain(
                &request("GET", "https://example.test/destination.png", "Script"),
                resource("application/octet-stream", too_wide.clone()),
                &headers("application/octet-stream"),
            )
            .unwrap();
        let error = changed_destination
            .retain(
                &request("GET", "https://example.test/destination.png", "Image"),
                resource("application/octet-stream", too_wide),
                &headers("application/octet-stream"),
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
                &headers("image/png"),
            )
            .unwrap();
        let error = aggregate
            .retain(
                &request("GET", "https://example.test/large-b.png", "Image"),
                resource("image/png", second_large),
                &headers("image/png"),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_IMAGE_LIMIT_EXCEEDED");
        assert!(error.reason.contains("document-decoded-pixel"));
        assert_eq!(aggregate.resources.len(), 1);
        assert_eq!(aggregate.image_costs.len(), 1);
    }

    #[test]
    fn animated_rasters_fail_before_servo_can_retain_every_frame() {
        let static_gif = BASE64_STANDARD
            .decode(b"R0lGODdhAQABAIEAAP8AAAAAAAAAAAAAACwAAAAAAQABAAAIBAABBAQAOw==")
            .unwrap();
        let animated_gif = BASE64_STANDARD
            .decode(b"R0lGODlhAQABAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQACgAAACwAAAAAAQABAAAIBAABBAQAIfkEAQoAAQAsAAAAAAEAAQCBAAD/AAAAAAAAAAAACAQAAQQEADs=")
            .unwrap();
        let static_info = gif_info(&static_gif).unwrap();
        assert_eq!(static_info.frames, 1);
        assert_eq!(static_info.max_frame_pixels, 1);
        assert_eq!(
            image_cost(
                &request("GET", "https://example.test/static.gif", "Image"),
                Some("image/gif"),
                &static_gif,
            )
            .unwrap(),
            Some(ImageCost {
                decoded_pixels: 1,
                decompressed_bytes: 12,
            })
        );
        assert_eq!(gif_info(&animated_gif).unwrap().frames, 2);

        let mut animated_png = b"\x89PNG\r\n\x1a\n".to_vec();
        animated_png.extend_from_slice(&8u32.to_be_bytes());
        animated_png.extend_from_slice(b"acTL");
        animated_png.extend_from_slice(&2u32.to_be_bytes());
        animated_png.extend_from_slice(&0u32.to_be_bytes());
        animated_png.extend_from_slice(&0u32.to_be_bytes());
        assert!(png_has_multiple_frames(&animated_png).unwrap());

        let mut animated_webp = b"RIFF".to_vec();
        animated_webp.extend_from_slice(&18u32.to_le_bytes());
        animated_webp.extend_from_slice(b"WEBP");
        animated_webp.extend_from_slice(b"ANIM");
        animated_webp.extend_from_slice(&6u32.to_le_bytes());
        animated_webp.extend_from_slice(&[0; 6]);
        assert!(webp_has_multiple_frames(&animated_webp).unwrap());

        for (content_type, body) in [
            ("image/gif", animated_gif),
            ("image/png", animated_png),
            ("image/webp", animated_webp),
        ] {
            let mut store = OwnedResourceStore::default();
            let error = store
                .retain(
                    &request("GET", "https://example.test/animated", "Image"),
                    resource(content_type, body),
                    &headers(content_type),
                )
                .unwrap_err();
            assert_eq!(error.code, "RESOURCE_IMAGE_ANIMATION_UNSUPPORTED");
            assert!(store.requests.is_empty());
            assert!(store.resources.is_empty());
        }
    }

    #[test]
    fn gif_frame_allocations_are_bounded_before_servo_decode() {
        let mut oversized = b"GIF89a\x01\x00\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00".to_vec();
        oversized.extend_from_slice(&u16::MAX.to_le_bytes());
        oversized.extend_from_slice(&u16::MAX.to_le_bytes());
        oversized.extend_from_slice(b"\x00\x02\x01\x00\x00\x3b");
        let info = gif_info(&oversized).unwrap();
        assert_eq!((info.logical_width, info.logical_height), (1, 1));
        assert_eq!(
            (info.max_frame_width, info.max_frame_height),
            (u64::from(u16::MAX), u64::from(u16::MAX))
        );

        let mut store = OwnedResourceStore::default();
        let error = store
            .retain(
                &request("GET", "https://example.test/oversized.gif", "Image"),
                resource("image/gif", oversized),
                &headers("image/gif"),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_IMAGE_LIMIT_EXCEEDED");
        assert!(error.reason.contains("declared-width"));
        assert!(store.requests.is_empty());
        assert!(store.resources.is_empty());

        let outside_screen =
            b"GIF89a\x01\x00\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x02\x00\x01\x00\x00\x02\x01\x00\x00\x3b";
        let error = store
            .retain(
                &request("GET", "https://example.test/outside.gif", "Image"),
                resource("image/gif", outside_screen.to_vec()),
                &headers("image/gif"),
            )
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_IMAGE_INVALID");
        assert!(error.reason.contains("logical screen"));
        assert!(store.requests.is_empty());
        assert!(store.resources.is_empty());
    }
}
