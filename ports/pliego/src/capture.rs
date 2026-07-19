/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{DocumentScene, Glyph, Operation, OperationMeta, Page, Rect, Size};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SceneCapture {
    pub scene: DocumentScene,
    pub font_resources: Vec<CapturedFontResource>,
    pub font_instances: Vec<CapturedFontInstance>,
    pub unsupported_events: Vec<UnsupportedPaintEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CapturedFontResource {
    pub resource: String,
    pub bytes_base64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CapturedFontInstance {
    pub id: String,
    pub resource: String,
    pub face_index: u32,
    pub variations: Vec<CapturedFontVariation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CapturedFontVariation {
    pub tag: u32,
    pub value: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnsupportedPaintEvent {
    pub sequence: usize,
    pub kind: UnsupportedPaintKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnsupportedPaintKind {
    Box,
    RootBackground,
    Outline,
    CollapsedTableBorders,
    Iframe,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureError {
    InvalidJson(String),
    NonDensePaintEvents {
        expected: usize,
        actual: usize,
    },
    UnbalancedStackingContext {
        sequence: usize,
    },
    UnclosedStackingContexts {
        count: usize,
    },
    UnknownPaintEvent {
        sequence: usize,
        kind: String,
    },
    DuplicateFragmentJoin,
    MissingFragmentJoin {
        sequence: usize,
        kind: String,
    },
    FragmentKindMismatch {
        sequence: usize,
        event_kind: String,
        fragment_kind: String,
    },
    FragmentTagMismatch {
        sequence: usize,
    },
    MissingTextRun {
        sequence: usize,
    },
    MissingTextFontInstance {
        sequence: usize,
    },
    UnknownFontInstance {
        sequence: usize,
        instance: String,
    },
    MissingImageRect {
        sequence: usize,
    },
    MissingImageUrl {
        sequence: usize,
    },
    UnresolvedImage {
        sequence: usize,
        url: String,
    },
    MissingLinkRect {
        sequence: usize,
    },
    DuplicateLinkTag,
    DuplicateFontResource(String),
    DuplicateFontInstance(String),
    MissingFontResource {
        instance: String,
        resource: String,
    },
    InvalidFontResourceBase64(String),
    FontResourceDigestMismatch {
        resource: String,
        actual: String,
    },
    FontInstanceIdMismatch {
        instance: String,
        actual: String,
    },
    InvalidContentAddress(String),
    InvalidFontVariation(String),
    InvalidScene(&'static str),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(message) => {
                write!(formatter, "invalid layout snapshot JSON: {message}")
            },
            Self::NonDensePaintEvents { expected, actual } => write!(
                formatter,
                "paint event sequence is not dense: expected {expected}, found {actual}"
            ),
            Self::UnbalancedStackingContext { sequence } => write!(
                formatter,
                "stacking-context leave at paint event {sequence} has no matching enter"
            ),
            Self::UnclosedStackingContexts { count } => {
                write!(
                    formatter,
                    "paint capture has {count} unclosed stacking contexts"
                )
            },
            Self::UnknownPaintEvent { sequence, kind } => {
                write!(formatter, "paint event {sequence} has unknown kind {kind}")
            },
            Self::DuplicateFragmentJoin => {
                write!(
                    formatter,
                    "layout snapshot repeats a paint fragment join key"
                )
            },
            Self::MissingFragmentJoin { sequence, kind } => write!(
                formatter,
                "{kind} paint event {sequence} has no matching fragment"
            ),
            Self::FragmentKindMismatch {
                sequence,
                event_kind,
                fragment_kind,
            } => write!(
                formatter,
                "{event_kind} paint event {sequence} joined fragment kind {fragment_kind}"
            ),
            Self::FragmentTagMismatch { sequence } => write!(
                formatter,
                "paint event {sequence} and its fragment disagree on the capture-local tag"
            ),
            Self::MissingTextRun { sequence } => {
                write!(
                    formatter,
                    "text paint event {sequence} has no retained text run"
                )
            },
            Self::MissingTextFontInstance { sequence } => write!(
                formatter,
                "text paint event {sequence} has no stable font instance"
            ),
            Self::UnknownFontInstance { sequence, instance } => write!(
                formatter,
                "text paint event {sequence} references unknown font instance {instance}"
            ),
            Self::MissingImageRect { sequence } => {
                write!(
                    formatter,
                    "image paint event {sequence} has no retained rectangle"
                )
            },
            Self::MissingImageUrl { sequence } => {
                write!(
                    formatter,
                    "image paint event {sequence} has no retained source URL"
                )
            },
            Self::UnresolvedImage { sequence, url } => write!(
                formatter,
                "image paint event {sequence} has no content address for {url}"
            ),
            Self::MissingLinkRect { sequence } => write!(
                formatter,
                "linked paint event {sequence} has no retained rectangle"
            ),
            Self::DuplicateLinkTag => {
                write!(formatter, "layout snapshot repeats a link tag join key")
            },
            Self::DuplicateFontResource(resource) => {
                write!(
                    formatter,
                    "layout snapshot repeats font resource {resource}"
                )
            },
            Self::DuplicateFontInstance(instance) => {
                write!(
                    formatter,
                    "layout snapshot repeats font instance {instance}"
                )
            },
            Self::MissingFontResource { instance, resource } => write!(
                formatter,
                "font instance {instance} references missing resource {resource}"
            ),
            Self::InvalidFontResourceBase64(resource) => write!(
                formatter,
                "font resource {resource} is not canonical padded base64"
            ),
            Self::FontResourceDigestMismatch { resource, actual } => write!(
                formatter,
                "font resource {resource} contains bytes addressed as {actual}"
            ),
            Self::FontInstanceIdMismatch { instance, actual } => write!(
                formatter,
                "font instance {instance} has derived identity {actual}"
            ),
            Self::InvalidContentAddress(resource) => {
                write!(
                    formatter,
                    "resource is not a lowercase SHA-256 content address: {resource}"
                )
            },
            Self::InvalidFontVariation(instance) => {
                write!(
                    formatter,
                    "font instance {instance} has a non-finite variation"
                )
            },
            Self::InvalidScene(message) => {
                write!(formatter, "captured document scene is invalid: {message}")
            },
        }
    }
}

impl std::error::Error for CaptureError {}

/// Convert the retained layout snapshot JSON into a one-page canonical document scene.
///
/// The converter consumes only retained capture data. Capture-local fragment, tag, spatial-node,
/// clip, and diagnostic font identifiers are used only for joins and never enter the returned
/// scene or reports.
pub fn capture_document_scene(
    snapshot_json: &[u8],
    mut resolve_image: impl FnMut(&str) -> Option<String>,
) -> Result<SceneCapture, CaptureError> {
    let capture: LayoutCapture = serde_json::from_slice(snapshot_json)
        .map_err(|error| CaptureError::InvalidJson(error.to_string()))?;

    let font_resources = collect_font_resources(capture.font_resources)?;
    let font_instances = collect_font_instances(capture.font_instances, &font_resources)?;
    let fragments = collect_fragments(capture.fragments)?;
    let links = collect_links(capture.links)?;
    let mut emitted_links = HashSet::new();
    let mut unsupported_events = Vec::new();
    let mut operations = Vec::new();
    let mut stacking_context_depth = 0usize;

    for (expected_sequence, event) in capture.paint_events.iter().enumerate() {
        if event.sequence != expected_sequence {
            return Err(CaptureError::NonDensePaintEvents {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }

        match event.kind.as_str() {
            "stacking-context-enter" => stacking_context_depth += 1,
            "stacking-context-leave" => {
                stacking_context_depth = stacking_context_depth.checked_sub(1).ok_or(
                    CaptureError::UnbalancedStackingContext {
                        sequence: event.sequence,
                    },
                )?;
            },
            "text" => {
                let (fragment_id, fragment) = joined_fragment(event, &fragments)?;
                if fragment.kind != "text" {
                    return Err(CaptureError::FragmentKindMismatch {
                        sequence: event.sequence,
                        event_kind: event.kind.clone(),
                        fragment_kind: fragment.kind.clone(),
                    });
                }
                let text_run = fragment
                    .text_run
                    .as_ref()
                    .ok_or(CaptureError::MissingTextRun {
                        sequence: event.sequence,
                    })?;
                let font = text_run.font_instance_id.as_ref().ok_or(
                    CaptureError::MissingTextFontInstance {
                        sequence: event.sequence,
                    },
                )?;
                if !font_instances.contains_key(font) {
                    return Err(CaptureError::UnknownFontInstance {
                        sequence: event.sequence,
                        instance: font.clone(),
                    });
                }
                operations.push(Operation::Text {
                    text: text_run.text.clone(),
                    font: font.clone(),
                    font_size: f64::from(text_run.font_size),
                    glyphs: text_run
                        .glyphs
                        .iter()
                        .map(|glyph| Glyph {
                            id: glyph.id,
                            x: f64::from(glyph.x),
                            y: f64::from(glyph.y),
                            advance: f64::from(glyph.advance),
                        })
                        .collect(),
                    meta: OperationMeta::default(),
                });
                append_link(
                    &mut operations,
                    &mut emitted_links,
                    &links,
                    fragment_id,
                    fragment,
                    event.sequence,
                )?;
            },
            "image" => {
                let (fragment_id, fragment) = joined_fragment(event, &fragments)?;
                if fragment.kind != "image" {
                    return Err(CaptureError::FragmentKindMismatch {
                        sequence: event.sequence,
                        event_kind: event.kind.clone(),
                        fragment_kind: fragment.kind.clone(),
                    });
                }
                let bounds = fragment
                    .rect
                    .as_ref()
                    .ok_or(CaptureError::MissingImageRect {
                        sequence: event.sequence,
                    })?
                    .into_scene_rect();
                let url = fragment
                    .image_url
                    .as_deref()
                    .ok_or(CaptureError::MissingImageUrl {
                        sequence: event.sequence,
                    })?;
                let resource = resolve_image(url).ok_or_else(|| CaptureError::UnresolvedImage {
                    sequence: event.sequence,
                    url: url.into(),
                })?;
                ensure_content_address(&resource)?;
                operations.push(Operation::Image {
                    bounds,
                    resource,
                    meta: OperationMeta::default(),
                });
                append_link(
                    &mut operations,
                    &mut emitted_links,
                    &links,
                    fragment_id,
                    fragment,
                    event.sequence,
                )?;
            },
            kind @ ("box"
            | "root-background"
            | "outline"
            | "collapsed-table-borders"
            | "iframe") => {
                unsupported_events.push(UnsupportedPaintEvent {
                    sequence: event.sequence,
                    kind: match kind {
                        "box" => UnsupportedPaintKind::Box,
                        "root-background" => UnsupportedPaintKind::RootBackground,
                        "outline" => UnsupportedPaintKind::Outline,
                        "collapsed-table-borders" => UnsupportedPaintKind::CollapsedTableBorders,
                        "iframe" => UnsupportedPaintKind::Iframe,
                        _ => unreachable!(),
                    },
                });

                if let Some((fragment_id, fragment)) = optionally_joined_fragment(event, &fragments)
                {
                    append_link(
                        &mut operations,
                        &mut emitted_links,
                        &links,
                        fragment_id,
                        fragment,
                        event.sequence,
                    )?;
                }
            },
            kind => {
                return Err(CaptureError::UnknownPaintEvent {
                    sequence: event.sequence,
                    kind: kind.into(),
                });
            },
        }
    }

    if stacking_context_depth != 0 {
        return Err(CaptureError::UnclosedStackingContexts {
            count: stacking_context_depth,
        });
    }

    let scene = DocumentScene::new(Page {
        size: Size {
            width: f64::from(capture.paint_content_width),
            height: f64::from(capture.paint_content_height),
        },
        operations,
    });
    scene.validate().map_err(CaptureError::InvalidScene)?;

    Ok(SceneCapture {
        scene,
        font_resources: font_resources.into_values().collect(),
        font_instances: font_instances.into_values().collect(),
        unsupported_events,
    })
}

fn collect_font_resources(
    resources: Vec<CaptureFontResource>,
) -> Result<BTreeMap<String, CapturedFontResource>, CaptureError> {
    let mut by_id = BTreeMap::new();
    for resource in resources {
        let expected_digest = content_address_digest(&resource.resource)?;
        let bytes = BASE64_STANDARD
            .decode(&resource.bytes_base64)
            .map_err(|_| CaptureError::InvalidFontResourceBase64(resource.resource.clone()))?;
        if BASE64_STANDARD.encode(&bytes) != resource.bytes_base64 {
            return Err(CaptureError::InvalidFontResourceBase64(resource.resource));
        }
        let actual_digest: [u8; 32] = Sha256::digest(&bytes).into();
        if actual_digest != expected_digest {
            return Err(CaptureError::FontResourceDigestMismatch {
                resource: resource.resource,
                actual: content_address(&actual_digest),
            });
        }
        let id = resource.resource.clone();
        if by_id
            .insert(
                id.clone(),
                CapturedFontResource {
                    resource: resource.resource,
                    bytes_base64: resource.bytes_base64,
                },
            )
            .is_some()
        {
            return Err(CaptureError::DuplicateFontResource(id));
        }
    }
    Ok(by_id)
}

fn collect_font_instances(
    instances: Vec<CaptureFontInstance>,
    resources: &BTreeMap<String, CapturedFontResource>,
) -> Result<BTreeMap<String, CapturedFontInstance>, CaptureError> {
    let mut by_id = BTreeMap::new();
    for instance in instances {
        ensure_content_address(&instance.id)?;
        ensure_content_address(&instance.resource)?;
        if !resources.contains_key(&instance.resource) {
            return Err(CaptureError::MissingFontResource {
                instance: instance.id,
                resource: instance.resource,
            });
        }

        let mut variations = instance
            .variations
            .into_iter()
            .map(|variation| {
                if !variation.value.is_finite() {
                    return Err(CaptureError::InvalidFontVariation(instance.id.clone()));
                }
                Ok(CapturedFontVariation {
                    tag: variation.tag,
                    value: if variation.value == 0.0 {
                        0.0
                    } else {
                        variation.value
                    },
                })
            })
            .collect::<Result<Vec<_>, CaptureError>>()?;
        variations.sort_unstable_by_key(|variation| (variation.tag, variation.value.to_bits()));

        let resource_digest = content_address_digest(&instance.resource)?;
        let actual_id = font_instance_id(&resource_digest, instance.face_index, &variations);
        if actual_id != instance.id {
            return Err(CaptureError::FontInstanceIdMismatch {
                instance: instance.id,
                actual: actual_id,
            });
        }

        let id = instance.id.clone();
        if by_id
            .insert(
                id.clone(),
                CapturedFontInstance {
                    id: instance.id,
                    resource: instance.resource,
                    face_index: instance.face_index,
                    variations,
                },
            )
            .is_some()
        {
            return Err(CaptureError::DuplicateFontInstance(id));
        }
    }
    Ok(by_id)
}

fn collect_fragments(
    fragments: Vec<CaptureFragment>,
) -> Result<HashMap<usize, CaptureFragment>, CaptureError> {
    let mut by_id = HashMap::new();
    for fragment in fragments {
        let Some(id) = fragment.paint_fragment_id else {
            continue;
        };
        if by_id.insert(id, fragment).is_some() {
            return Err(CaptureError::DuplicateFragmentJoin);
        }
    }
    Ok(by_id)
}

fn collect_links(links: Vec<CaptureLink>) -> Result<HashMap<u64, String>, CaptureError> {
    let mut by_tag = HashMap::new();
    for link in links {
        if by_tag.insert(link.tag_id, link.url).is_some() {
            return Err(CaptureError::DuplicateLinkTag);
        }
    }
    Ok(by_tag)
}

fn joined_fragment<'a>(
    event: &CapturePaintEvent,
    fragments: &'a HashMap<usize, CaptureFragment>,
) -> Result<(usize, &'a CaptureFragment), CaptureError> {
    let fragment_id = event
        .fragment_id
        .ok_or_else(|| CaptureError::MissingFragmentJoin {
            sequence: event.sequence,
            kind: event.kind.clone(),
        })?;
    let fragment =
        fragments
            .get(&fragment_id)
            .ok_or_else(|| CaptureError::MissingFragmentJoin {
                sequence: event.sequence,
                kind: event.kind.clone(),
            })?;
    if event.tag_id != fragment.tag_id {
        return Err(CaptureError::FragmentTagMismatch {
            sequence: event.sequence,
        });
    }
    Ok((fragment_id, fragment))
}

fn optionally_joined_fragment<'a>(
    event: &CapturePaintEvent,
    fragments: &'a HashMap<usize, CaptureFragment>,
) -> Option<(usize, &'a CaptureFragment)> {
    let fragment_id = event.fragment_id?;
    let fragment = fragments.get(&fragment_id)?;
    (event.tag_id == fragment.tag_id).then_some((fragment_id, fragment))
}

fn append_link(
    operations: &mut Vec<Operation>,
    emitted: &mut HashSet<(usize, u64)>,
    links: &HashMap<u64, String>,
    fragment_id: usize,
    fragment: &CaptureFragment,
    sequence: usize,
) -> Result<(), CaptureError> {
    let Some(tag_id) = fragment.tag_id else {
        return Ok(());
    };
    let Some(target) = links.get(&tag_id) else {
        return Ok(());
    };
    if !emitted.insert((fragment_id, tag_id)) {
        return Ok(());
    }
    let bounds = fragment
        .rect
        .as_ref()
        .ok_or(CaptureError::MissingLinkRect { sequence })?
        .into_scene_rect();
    operations.push(Operation::Link {
        bounds,
        target: target.clone(),
        meta: OperationMeta::default(),
    });
    Ok(())
}

fn ensure_content_address(resource: &str) -> Result<(), CaptureError> {
    content_address_digest(resource).map(|_| ())
}

fn content_address_digest(resource: &str) -> Result<[u8; 32], CaptureError> {
    let Some(digest) = resource.strip_prefix("sha256:") else {
        return Err(CaptureError::InvalidContentAddress(resource.into()));
    };
    if digest.len() != 64 {
        return Err(CaptureError::InvalidContentAddress(resource.into()));
    }

    let mut decoded = [0u8; 32];
    for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
        let Some(high) = lowercase_hex_nibble(pair[0]) else {
            return Err(CaptureError::InvalidContentAddress(resource.into()));
        };
        let Some(low) = lowercase_hex_nibble(pair[1]) else {
            return Err(CaptureError::InvalidContentAddress(resource.into()));
        };
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

fn lowercase_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn font_instance_id(
    resource_digest: &[u8; 32],
    face_index: u32,
    variations: &[CapturedFontVariation],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"pliego-font-instance-v1\0");
    hasher.update(resource_digest);
    hasher.update(face_index.to_be_bytes());
    hasher.update((variations.len() as u64).to_be_bytes());
    for variation in variations {
        hasher.update(variation.tag.to_be_bytes());
        hasher.update(variation.value.to_bits().to_be_bytes());
    }
    let digest: [u8; 32] = hasher.finalize().into();
    content_address(&digest)
}

fn content_address(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutCapture {
    #[serde(rename = "boxes")]
    _boxes: Vec<CaptureBox>,
    fragments: Vec<CaptureFragment>,
    font_resources: Vec<CaptureFontResource>,
    font_instances: Vec<CaptureFontInstance>,
    paint_events: Vec<CapturePaintEvent>,
    #[serde(rename = "paint_epoch")]
    _paint_epoch: u32,
    paint_content_width: f32,
    paint_content_height: f32,
    #[serde(rename = "paint_scroll_node_count")]
    _paint_scroll_node_count: usize,
    #[serde(rename = "paintable")]
    _paintable: bool,
    #[serde(rename = "contentful")]
    _contentful: bool,
    #[serde(rename = "first_reflow")]
    _first_reflow: bool,
    links: Vec<CaptureLink>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureBox {
    #[serde(rename = "depth")]
    _depth: usize,
    #[serde(rename = "kind")]
    _kind: String,
    #[serde(rename = "tag_id")]
    _tag_id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFragment {
    #[serde(rename = "depth")]
    _depth: usize,
    kind: String,
    rect: Option<CaptureRect>,
    tag_id: Option<u64>,
    paint_fragment_id: Option<usize>,
    text_run: Option<CaptureTextRun>,
    image_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturePaintEvent {
    sequence: usize,
    kind: String,
    fragment_id: Option<usize>,
    tag_id: Option<u64>,
    #[serde(rename = "spatial_node_id")]
    _spatial_node_id: usize,
    #[serde(rename = "clip_id")]
    _clip_id: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureTextRun {
    text: String,
    font_instance_id: Option<String>,
    #[serde(rename = "font_identifier")]
    _font_identifier: serde_json::Value,
    font_size: f32,
    glyphs: Vec<CaptureGlyph>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureGlyph {
    id: u32,
    x: f32,
    y: f32,
    advance: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl CaptureRect {
    fn into_scene_rect(&self) -> Rect {
        Rect {
            x: f64::from(self.x),
            y: f64::from(self.y),
            width: f64::from(self.width),
            height: f64::from(self.height),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureLink {
    tag_id: u64,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFontResource {
    resource: String,
    bytes_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFontInstance {
    id: String,
    resource: String,
    face_index: u32,
    variations: Vec<CaptureFontVariation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFontVariation {
    tag: u32,
    value: f32,
}
