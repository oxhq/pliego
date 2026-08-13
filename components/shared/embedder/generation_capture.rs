/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Same-build protocol types for generation-bound document capture preparation.
//!
//! A [`DocumentCapturePrecondition`] is immutable candidate evidence for the ScriptThread
//! observation from which it was issued; it neither authenticates its own origin nor freezes any
//! observed state. The same-build consume protocol binds that candidate to an exact Paint
//! presentation ticket, reobserves every bound dimension around layout serialization, and consumes
//! the retained candidate once before Paint performs its final exact-generation readback.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use servo_base::Epoch;
use servo_base::id::PipelineId;
use servo_geometry::{DeviceIndependentIntRect, DeviceIndependentIntSize};
use timers::{
    DocumentClockId, DocumentExecutionObservation, DocumentProducerCheckpoint, DocumentTime,
    TimerDeadlineSnapshot,
};

use crate::document_time::{
    DocumentProducerStability, DocumentTimeControlTarget, DocumentTimeDocumentObservation,
    DocumentTimeProducerObservation, DocumentTimeReadinessBlocker,
};
/// Stable identity for one ScriptThread-issued capture precondition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct DocumentCapturePreconditionId(u64);

impl DocumentCapturePreconditionId {
    /// Construct an identifier from a checked ScriptThread sequence.
    #[doc(hidden)]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Return the underlying ScriptThread sequence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Monotonic identity of one source inventory taken by a capture-preparation command.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DocumentSettlementSourceEpoch(u64);

impl DocumentSettlementSourceEpoch {
    /// Construct an epoch from a checked ScriptThread sequence.
    #[doc(hidden)]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Return the underlying ScriptThread sequence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Two-fresh-checkpoint qualification over an exact, caller-defined quiet identity.
///
/// The identity deliberately excludes the producer checkpoint and producer-stability label:
/// those must change from `FirstEmpty` to `StableEmpty`. ScriptThread supplies the exact clock,
/// input, producer-snapshot, execution, source, and document generations as `R`.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct DocumentCaptureQuietFence<R> {
    last_checkpoint: Option<DocumentProducerCheckpoint>,
    last_revision: Option<R>,
    last_revision_qualified: bool,
    quiet_candidate: Option<R>,
    stable_quiet_revision: Option<R>,
}

impl<R> Default for DocumentCaptureQuietFence<R> {
    fn default() -> Self {
        Self {
            last_checkpoint: None,
            last_revision: None,
            last_revision_qualified: false,
            quiet_candidate: None,
            stable_quiet_revision: None,
        }
    }
}

impl<R: Clone + Eq> DocumentCaptureQuietFence<R> {
    /// Observe one checkpoint. A changed, qualified revision at a fresh checkpoint is candidate
    /// one; the next fresh checkpoint with the same revision qualifies it. Re-observing one
    /// checkpoint can never strengthen the fence.
    pub fn observe(
        &mut self,
        checkpoint: DocumentProducerCheckpoint,
        revision: R,
        mechanically_qualified: bool,
    ) -> bool {
        let revision_changed = self.last_revision.as_ref() != Some(&revision) ||
            self.last_revision_qualified != mechanically_qualified;
        if revision_changed || !mechanically_qualified {
            self.quiet_candidate = None;
            self.stable_quiet_revision = None;
        }

        let fresh_checkpoint = checkpoint != DocumentProducerCheckpoint::ZERO &&
            self.last_checkpoint
                .is_none_or(|previous| checkpoint > previous);
        if fresh_checkpoint {
            self.last_checkpoint = Some(checkpoint);
            if mechanically_qualified {
                if !revision_changed && self.quiet_candidate.as_ref() == Some(&revision) {
                    self.stable_quiet_revision = Some(revision.clone());
                } else {
                    self.quiet_candidate = Some(revision.clone());
                }
            }
        }

        let qualified =
            mechanically_qualified && self.stable_quiet_revision.as_ref() == Some(&revision);
        self.last_revision = Some(revision);
        self.last_revision_qualified = mechanically_qualified;
        qualified
    }

    /// Invalidate the retained revision across navigation or candidate loss while preserving the
    /// monotonic checkpoint watermark, so an old checkpoint cannot become fresh again.
    pub fn invalidate_revision(&mut self) {
        self.last_revision = None;
        self.last_revision_qualified = false;
        self.quiet_candidate = None;
        self.stable_quiet_revision = None;
    }

    /// Return whether the supplied exact revision remains qualified without advancing the fence.
    pub fn is_qualified(&self, revision: &R) -> bool {
        self.last_revision_qualified &&
            self.last_revision.as_ref() == Some(revision) &&
            self.stable_quiet_revision.as_ref() == Some(revision)
    }
}

/// Exact viewport, capture rectangle, and device-pixel scale requested by a future coordinator.
///
/// The scale is carried by its IEEE-754 bits so equality is exact and does not silently normalize
/// one capture surface into another. ScriptThread revalidates deserialized values before issuing a
/// precondition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentCaptureSurfaceFingerprint {
    viewport: DeviceIndependentIntSize,
    capture_rect: DeviceIndependentIntRect,
    device_pixel_scale_bits: u32,
}

impl DocumentCaptureSurfaceFingerprint {
    /// Construct a checked capture surface.
    pub fn new(
        viewport: DeviceIndependentIntSize,
        capture_rect: DeviceIndependentIntRect,
        device_pixel_scale: f32,
    ) -> Result<Self, DocumentCaptureSurfaceError> {
        let fingerprint = Self {
            viewport,
            capture_rect,
            device_pixel_scale_bits: device_pixel_scale.to_bits(),
        };
        fingerprint.validate()?;
        Ok(fingerprint)
    }

    /// Return the exact viewport bound by this fingerprint.
    pub const fn viewport(&self) -> DeviceIndependentIntSize {
        self.viewport
    }

    /// Return the exact capture rectangle bound by this fingerprint.
    pub const fn capture_rect(&self) -> DeviceIndependentIntRect {
        self.capture_rect
    }

    /// Return the exact device-pixel scale bound by this fingerprint.
    pub fn device_pixel_scale(&self) -> f32 {
        f32::from_bits(self.device_pixel_scale_bits)
    }

    /// Revalidate a value which may have crossed a serialization boundary.
    #[doc(hidden)]
    pub fn validate(&self) -> Result<(), DocumentCaptureSurfaceError> {
        if self.viewport.width <= 0 || self.viewport.height <= 0 {
            return Err(DocumentCaptureSurfaceError::EmptyViewport);
        }
        if self.capture_rect.min.x < 0 ||
            self.capture_rect.min.y < 0 ||
            self.capture_rect.max.x <= self.capture_rect.min.x ||
            self.capture_rect.max.y <= self.capture_rect.min.y
        {
            return Err(DocumentCaptureSurfaceError::InvalidCaptureRect);
        }
        if self.capture_rect.max.x > self.viewport.width ||
            self.capture_rect.max.y > self.viewport.height
        {
            return Err(DocumentCaptureSurfaceError::CaptureRectOutsideViewport);
        }
        let scale = self.device_pixel_scale();
        if !scale.is_finite() || scale <= 0.0 {
            return Err(DocumentCaptureSurfaceError::InvalidDevicePixelScale);
        }
        Ok(())
    }
}

/// Why a capture surface cannot name one exact finite raster target.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentCaptureSurfaceError {
    /// The viewport has a non-positive dimension.
    EmptyViewport,
    /// The rectangle is empty or begins before the viewport origin.
    InvalidCaptureRect,
    /// The rectangle extends beyond the supplied viewport.
    CaptureRectOutsideViewport,
    /// The scale is non-finite, zero, or negative.
    InvalidDevicePixelScale,
}

/// Exact state of a retained CSS animation entry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentCssAnimationState {
    /// Created or in its initial delay phase.
    Pending,
    /// Currently advancing with the document timeline.
    Running,
    /// Retained at an exact progress value, represented by raw `f64` bits.
    Paused(u64),
    /// Completed and inert.
    Finished,
    /// Canceled and inert.
    Canceled,
}

/// Exact iteration state of a retained CSS animation entry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentCssIterationState {
    /// Current and maximum iteration values, represented by raw `f64` bits.
    Finite { current_bits: u64, max_bits: u64 },
    /// Current iteration value for an unbounded animation.
    Infinite { current_bits: u64 },
}

/// Retained owner-tracker class whose live presence is part of capture preparation.
///
/// These entries deliberately use one typed identity plus an exact active-object count. Every
/// non-zero count blocks capture, so per-object identity would add protocol surface without making
/// qualification stronger. Open and close transitions still change the canonical source vector.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentTrackedSourceKind {
    /// WebSockets retained by the owning Document.
    WebSocket,
    /// EventSource objects retained by the Window GlobalScope.
    EventSource,
    /// BroadcastChannel objects retained by the Window GlobalScope.
    BroadcastChannel,
    /// MessagePort objects retained by the Window GlobalScope.
    MessagePort,
    /// A visible document control awaiting an embedder or file-manager response.
    EmbedderControl,
    /// Explicit MediaSession action handlers retained by an already-created MediaSession.
    MediaSessionActionHandler,
    /// Dedicated workers retained by the Window GlobalScope.
    DedicatedWorker,
    /// A ScriptThread worklet pool has been created.
    Worklet,
    /// The Window GlobalScope has created its IndexedDB factory.
    IndexedDb,
    /// Live WebGPU devices retained by the Window GlobalScope.
    GpuDevice,
    /// Registered Window storage-event listeners tracked by the GlobalScope.
    StorageEventListener,
    /// CookieStore requests awaiting a backend response.
    CookieStorePending,
    /// ServiceWorker algorithms awaiting a backend response.
    ServiceWorkerPending,
    /// A retained ServiceWorker backend callback can deliver unsolicited worker messages.
    ServiceWorkerMessaging,
    /// WebXR was exposed to script without a complete pending-callback inventory.
    WebXr,
    /// StorageManager was exposed to script without a complete pending-callback inventory.
    StorageManager,
    /// Web Bluetooth was exposed to script without a complete callback/subscription inventory.
    Bluetooth,
    /// WebRTC was invoked without an owned callback/source lifecycle.
    RtcPeerConnection,
    /// A media-stream source was exposed without an owned media-clock lifecycle.
    MediaStream,
    /// Web Audio was invoked without an owned audio-clock/callback lifecycle.
    WebAudio,
    /// A Notification may have untracked resource decoding or delivery callbacks.
    Notification,
    /// WebGPU was exposed before adapter/device request lifecycles were producer-guarded.
    WebGpuApi,
    /// An OffscreenCanvas was created without a script/render/Paint generation binding.
    OffscreenCanvas,
}

/// Why an otherwise visible source can produce work without a finite terminal opportunity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentOpenEndedSourceReason {
    /// A CSS animation has an infinite iteration count.
    InfiniteCssAnimation,
    /// A JavaScript interval remains registered.
    Interval,
    /// A retained WebSocket can receive network events without a terminal document-time deadline.
    WebSocket,
    /// A retained EventSource can reconnect and receive events indefinitely.
    EventSource,
    /// A retained BroadcastChannel can receive messages indefinitely.
    BroadcastChannel,
    /// A retained MessagePort can receive messages indefinitely.
    MessagePort,
    /// A visible embedder control can await user or host input indefinitely.
    EmbedderControl,
    /// A MediaSession action handler can be invoked by external media controls indefinitely.
    MediaSessionActionHandler,
    /// A registered storage-event listener can receive cross-document events indefinitely.
    StorageEventListener,
}

/// Unsupported source class which makes capture preparation fail closed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentUnsupportedSourceReason {
    /// CSS timing fields were non-finite, negative, internally inconsistent, or overflowed time.
    InvalidCssTiming,
    /// A Window timer queue contained a worker-owned timer.
    NonWindowTimer,
    /// A non-DOM callback uses the Window timer queue and has not been classified by this slice.
    NonDomTimerCallback,
    /// An audio or video element can advance from a media clock not owned by this slice.
    MediaElement,
    /// A 2D canvas has no script/render/Paint generation binding in this slice.
    Canvas2d,
    /// A bitmap-renderer canvas has no script/render/Paint generation binding in this slice.
    BitmapRendererCanvas,
    /// A WebGL canvas can mutate through a graphics source not owned by this slice.
    WebGlCanvas,
    /// A WebGPU canvas can mutate through a graphics source not owned by this slice.
    WebGpuCanvas,
    /// A transferred/offscreen canvas can mutate outside the owned Window source inventory.
    OffscreenCanvas,
    /// A dedicated worker runs outside this slice's controlled Window execution ledger.
    DedicatedWorker,
    /// Worklet execution is not owned by this slice's controlled Window execution ledger.
    Worklet,
    /// IndexedDB backend replies are not guarded by a producer spanning request to reply.
    IndexedDb,
    /// WebGPU device loss/error callbacks are not owned by this slice's producer fence.
    GpuDevice,
    /// CookieStore backend replies are not guarded by a producer spanning request to reply.
    CookieStorePending,
    /// ServiceWorker backend replies are not guarded by a producer spanning request to reply.
    ServiceWorkerPending,
    /// A retained ServiceWorker callback can receive worker messages outside the producer fence.
    ServiceWorkerMessaging,
    /// WebXR callbacks and sessions are not owned by this slice's source inventory.
    WebXr,
    /// StorageManager backend callbacks are not owned by this slice's producer fence.
    StorageManager,
    /// Web Bluetooth callbacks and subscriptions are not owned by this slice's producer fence.
    Bluetooth,
    /// WebRTC callbacks and media state are not owned by this slice's source inventory.
    RtcPeerConnection,
    /// Media-stream clocks and callbacks are not owned by this slice's source inventory.
    MediaStream,
    /// Web Audio clocks and callbacks are not owned by this slice's source inventory.
    WebAudio,
    /// Notification resource decoding and delivery are not fully producer-guarded.
    Notification,
    /// WebGPU adapter/device requests are not guarded from invocation through backend reply.
    WebGpuApi,
}

/// Result of synchronizing one DOM-owned Canvas 2D context with retained capture state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentCanvasCaptureStatus {
    Ready,
    RetentionDisabled,
    MissingCanvas,
    MissingImageKey,
    ImageKeyMismatch,
    SizeMismatch,
    RetentionBudgetExceeded,
    UnsupportedTranscript,
    ObservationUnavailable,
    MixedGeneration,
}

/// Portable image-key identity used by the generation-bound Canvas capture binding.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DocumentCanvasImageKey {
    namespace: u32,
    key: u32,
}

impl DocumentCanvasImageKey {
    #[doc(hidden)]
    pub const fn new_internal(namespace: u32, key: u32) -> Self {
        Self { namespace, key }
    }

    pub const fn namespace(self) -> u32 {
        self.namespace
    }

    pub const fn key(self) -> u32 {
        self.key
    }
}

/// Mechanical disposition of one exact source entry at the preparation observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentSettlementSourceDisposition {
    /// Retained identity with no work to drive at this observation.
    Inert,
    /// Work becomes eligible at one exact document-clock deadline.
    FiniteDeadline(DocumentTime),
    /// One rendering opportunity is needed, but it has no independent timer deadline.
    FiniteRenderingOpportunity,
    /// Work can continue forever without a terminal deadline.
    OpenEnded(DocumentOpenEndedSourceReason),
    /// This slice cannot prove how the source settles.
    Unsupported(DocumentUnsupportedSourceReason),
}

/// Canonical identity of one source. Slots are retained because names and properties may repeat.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentSettlementSourceIdentity {
    /// One entry in a node/pseudo-element CSS animation vector.
    CssAnimation {
        pipeline_id: PipelineId,
        node_id: usize,
        pseudo_element: Option<u16>,
        slot: u64,
        name: String,
    },
    /// One entry in a node/pseudo-element CSS transition vector.
    CssTransition {
        pipeline_id: PipelineId,
        node_id: usize,
        pseudo_element: Option<u16>,
        slot: u64,
        property: String,
    },
    /// One exact entry in a Window's ordered timer queue.
    Timer {
        pipeline_id: PipelineId,
        scheduler_handle: i32,
        dom_handle: Option<i32>,
        creation_sequence: u64,
    },
    /// One exact requestAnimationFrame callback slot.
    AnimationFrame {
        pipeline_id: PipelineId,
        callback_id: u32,
    },
    /// One retained live-object tracker class.
    TrackedPresence {
        pipeline_id: PipelineId,
        kind: DocumentTrackedSourceKind,
    },
    /// One DOM-owned Canvas 2D context and its Canvas-paint-thread identity.
    Canvas2d {
        pipeline_id: PipelineId,
        node_id: usize,
        canvas_id: u64,
    },
    /// One DOM node whose source class is intentionally unsupported.
    UnsupportedDomNode {
        pipeline_id: PipelineId,
        node_id: usize,
        reason: DocumentUnsupportedSourceReason,
    },
}

/// Full state fingerprint paired with one canonical source identity.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentSettlementSourceState {
    /// Raw CSS animation timing and iteration facts.
    CssAnimation {
        state: DocumentCssAnimationState,
        iteration: DocumentCssIterationState,
        started_at_bits: u64,
        duration_bits: u64,
        delay_bits: u64,
        timeline_now_bits: u64,
    },
    /// Raw CSS transition timing facts.
    CssTransition {
        state: DocumentCssAnimationState,
        start_time_bits: u64,
        duration_bits: u64,
        delay_bits: u64,
        timeline_now_bits: u64,
    },
    /// Exact timer deadline and repeat classification.
    Timer {
        deadline: DocumentTime,
        is_interval: bool,
    },
    /// Whether the retained rAF slot still contains a callback.
    AnimationFrame { active: bool },
    /// Exact blocking presence/count retained by one owner-tracker class.
    TrackedPresence { blocking_count: u64 },
    /// Exact retained registry state observed after the Canvas command barrier.
    Canvas2d {
        image_key: Option<DocumentCanvasImageKey>,
        width: u32,
        height: u32,
        registry_generation: Option<u64>,
        status: DocumentCanvasCaptureStatus,
    },
    /// Unsupported DOM node class; identity still prevents a count-only snapshot.
    UnsupportedDomNode,
}

/// One complete, identity-bearing source entry.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DocumentSettlementSource {
    identity: DocumentSettlementSourceIdentity,
    state: DocumentSettlementSourceState,
    disposition: DocumentSettlementSourceDisposition,
}

impl DocumentSettlementSource {
    /// Construct and classify one exact CSS animation entry.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn css_animation_internal(
        pipeline_id: PipelineId,
        node_id: usize,
        pseudo_element: Option<u16>,
        slot: u64,
        name: String,
        state: DocumentCssAnimationState,
        iteration: DocumentCssIterationState,
        started_at: f64,
        duration: f64,
        delay: f64,
        timeline_now: f64,
        document_now: DocumentTime,
    ) -> Self {
        let disposition = classify_css_animation(
            state,
            iteration,
            started_at,
            duration,
            delay,
            timeline_now,
            document_now,
        );
        Self {
            identity: DocumentSettlementSourceIdentity::CssAnimation {
                pipeline_id,
                node_id,
                pseudo_element,
                slot,
                name,
            },
            state: DocumentSettlementSourceState::CssAnimation {
                state,
                iteration,
                started_at_bits: started_at.to_bits(),
                duration_bits: duration.to_bits(),
                delay_bits: delay.to_bits(),
                timeline_now_bits: timeline_now.to_bits(),
            },
            disposition,
        }
    }

    /// Construct and classify one exact CSS transition entry.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn css_transition_internal(
        pipeline_id: PipelineId,
        node_id: usize,
        pseudo_element: Option<u16>,
        slot: u64,
        property: String,
        state: DocumentCssAnimationState,
        start_time: f64,
        duration: f64,
        delay: f64,
        timeline_now: f64,
        document_now: DocumentTime,
    ) -> Self {
        let disposition = classify_css_transition(
            state,
            start_time,
            duration,
            delay,
            timeline_now,
            document_now,
        );
        Self {
            identity: DocumentSettlementSourceIdentity::CssTransition {
                pipeline_id,
                node_id,
                pseudo_element,
                slot,
                property,
            },
            state: DocumentSettlementSourceState::CssTransition {
                state,
                start_time_bits: start_time.to_bits(),
                duration_bits: duration.to_bits(),
                delay_bits: delay.to_bits(),
                timeline_now_bits: timeline_now.to_bits(),
            },
            disposition,
        }
    }

    /// Construct one exact DOM timer entry.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn timer_internal(
        pipeline_id: PipelineId,
        scheduler_handle: i32,
        dom_handle: i32,
        creation_sequence: u64,
        deadline: DocumentTime,
        is_interval: bool,
    ) -> Self {
        Self {
            identity: DocumentSettlementSourceIdentity::Timer {
                pipeline_id,
                scheduler_handle,
                dom_handle: Some(dom_handle),
                creation_sequence,
            },
            state: DocumentSettlementSourceState::Timer {
                deadline,
                is_interval,
            },
            disposition: if is_interval {
                DocumentSettlementSourceDisposition::OpenEnded(
                    DocumentOpenEndedSourceReason::Interval,
                )
            } else {
                DocumentSettlementSourceDisposition::FiniteDeadline(deadline)
            },
        }
    }

    /// Construct one exact unsupported timer-queue entry.
    #[doc(hidden)]
    pub fn unsupported_timer_internal(
        pipeline_id: PipelineId,
        scheduler_handle: i32,
        creation_sequence: u64,
        deadline: DocumentTime,
        reason: DocumentUnsupportedSourceReason,
    ) -> Self {
        Self {
            identity: DocumentSettlementSourceIdentity::Timer {
                pipeline_id,
                scheduler_handle,
                dom_handle: None,
                creation_sequence,
            },
            state: DocumentSettlementSourceState::Timer {
                deadline,
                is_interval: false,
            },
            disposition: DocumentSettlementSourceDisposition::Unsupported(reason),
        }
    }

    /// Construct one exact requestAnimationFrame callback slot.
    #[doc(hidden)]
    pub fn animation_frame_internal(
        pipeline_id: PipelineId,
        callback_id: u32,
        active: bool,
    ) -> Self {
        Self {
            identity: DocumentSettlementSourceIdentity::AnimationFrame {
                pipeline_id,
                callback_id,
            },
            state: DocumentSettlementSourceState::AnimationFrame { active },
            disposition: if active {
                DocumentSettlementSourceDisposition::FiniteRenderingOpportunity
            } else {
                DocumentSettlementSourceDisposition::Inert
            },
        }
    }

    /// Construct one retained tracker-presence source.
    #[doc(hidden)]
    pub fn tracked_presence_internal(
        pipeline_id: PipelineId,
        kind: DocumentTrackedSourceKind,
        blocking_count: u64,
    ) -> Self {
        let disposition = if blocking_count == 0 {
            DocumentSettlementSourceDisposition::Inert
        } else {
            match kind {
                DocumentTrackedSourceKind::WebSocket => {
                    DocumentSettlementSourceDisposition::OpenEnded(
                        DocumentOpenEndedSourceReason::WebSocket,
                    )
                },
                DocumentTrackedSourceKind::EventSource => {
                    DocumentSettlementSourceDisposition::OpenEnded(
                        DocumentOpenEndedSourceReason::EventSource,
                    )
                },
                DocumentTrackedSourceKind::BroadcastChannel => {
                    DocumentSettlementSourceDisposition::OpenEnded(
                        DocumentOpenEndedSourceReason::BroadcastChannel,
                    )
                },
                DocumentTrackedSourceKind::MessagePort => {
                    DocumentSettlementSourceDisposition::OpenEnded(
                        DocumentOpenEndedSourceReason::MessagePort,
                    )
                },
                DocumentTrackedSourceKind::EmbedderControl => {
                    DocumentSettlementSourceDisposition::OpenEnded(
                        DocumentOpenEndedSourceReason::EmbedderControl,
                    )
                },
                DocumentTrackedSourceKind::MediaSessionActionHandler => {
                    DocumentSettlementSourceDisposition::OpenEnded(
                        DocumentOpenEndedSourceReason::MediaSessionActionHandler,
                    )
                },
                DocumentTrackedSourceKind::StorageEventListener => {
                    DocumentSettlementSourceDisposition::OpenEnded(
                        DocumentOpenEndedSourceReason::StorageEventListener,
                    )
                },
                DocumentTrackedSourceKind::DedicatedWorker => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::DedicatedWorker,
                    )
                },
                DocumentTrackedSourceKind::Worklet => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::Worklet,
                    )
                },
                DocumentTrackedSourceKind::IndexedDb => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::IndexedDb,
                    )
                },
                DocumentTrackedSourceKind::GpuDevice => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::GpuDevice,
                    )
                },
                DocumentTrackedSourceKind::CookieStorePending => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::CookieStorePending,
                    )
                },
                DocumentTrackedSourceKind::ServiceWorkerPending => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::ServiceWorkerPending,
                    )
                },
                DocumentTrackedSourceKind::ServiceWorkerMessaging => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::ServiceWorkerMessaging,
                    )
                },
                DocumentTrackedSourceKind::WebXr => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::WebXr,
                    )
                },
                DocumentTrackedSourceKind::StorageManager => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::StorageManager,
                    )
                },
                DocumentTrackedSourceKind::Bluetooth => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::Bluetooth,
                    )
                },
                DocumentTrackedSourceKind::RtcPeerConnection => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::RtcPeerConnection,
                    )
                },
                DocumentTrackedSourceKind::MediaStream => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::MediaStream,
                    )
                },
                DocumentTrackedSourceKind::WebAudio => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::WebAudio,
                    )
                },
                DocumentTrackedSourceKind::Notification => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::Notification,
                    )
                },
                DocumentTrackedSourceKind::WebGpuApi => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::WebGpuApi,
                    )
                },
                DocumentTrackedSourceKind::OffscreenCanvas => {
                    DocumentSettlementSourceDisposition::Unsupported(
                        DocumentUnsupportedSourceReason::OffscreenCanvas,
                    )
                },
            }
        };
        Self {
            identity: DocumentSettlementSourceIdentity::TrackedPresence { pipeline_id, kind },
            state: DocumentSettlementSourceState::TrackedPresence { blocking_count },
            disposition,
        }
    }

    /// Construct one Canvas 2D source from a Canvas-paint-thread barrier observation.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn canvas_2d_internal(
        pipeline_id: PipelineId,
        node_id: usize,
        canvas_id: u64,
        image_key: Option<DocumentCanvasImageKey>,
        width: u32,
        height: u32,
        registry_generation: Option<u64>,
        status: DocumentCanvasCaptureStatus,
    ) -> Self {
        let mechanically_ready = status == DocumentCanvasCaptureStatus::Ready &&
            image_key.is_some() &&
            registry_generation.is_some();
        Self {
            identity: DocumentSettlementSourceIdentity::Canvas2d {
                pipeline_id,
                node_id,
                canvas_id,
            },
            state: DocumentSettlementSourceState::Canvas2d {
                image_key,
                width,
                height,
                registry_generation,
                status,
            },
            disposition: if mechanically_ready {
                DocumentSettlementSourceDisposition::Inert
            } else {
                DocumentSettlementSourceDisposition::Unsupported(
                    DocumentUnsupportedSourceReason::Canvas2d,
                )
            },
        }
    }

    /// Construct one exact unsupported DOM-node source.
    #[doc(hidden)]
    pub fn unsupported_dom_node_internal(
        pipeline_id: PipelineId,
        node_id: usize,
        reason: DocumentUnsupportedSourceReason,
    ) -> Self {
        Self {
            identity: DocumentSettlementSourceIdentity::UnsupportedDomNode {
                pipeline_id,
                node_id,
                reason,
            },
            state: DocumentSettlementSourceState::UnsupportedDomNode,
            disposition: DocumentSettlementSourceDisposition::Unsupported(reason),
        }
    }

    /// Return this entry's full identity.
    pub const fn identity(&self) -> &DocumentSettlementSourceIdentity {
        &self.identity
    }

    /// Return the exact state fingerprint used by classification.
    pub const fn state(&self) -> &DocumentSettlementSourceState {
        &self.state
    }

    /// Return the mechanical disposition of this source.
    pub const fn disposition(&self) -> DocumentSettlementSourceDisposition {
        self.disposition
    }
}

#[derive(Clone, Copy)]
struct ExactDyadic {
    coefficient: u128,
    binary_exponent: i32,
}

impl ExactDyadic {
    const ZERO: Self = Self {
        coefficient: 0,
        binary_exponent: 0,
    };
}

fn scaled_abs_f64_nanoseconds(value: f64) -> Option<ExactDyadic> {
    let bits = value.to_bits() & !(1_u64 << 63);
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction_bits = bits & ((1_u64 << 52) - 1);
    let (significand, binary_exponent) = if exponent_bits == 0 {
        (u128::from(fraction_bits), -1074)
    } else {
        (
            u128::from((1_u64 << 52) | fraction_bits),
            exponent_bits - 1023 - 52,
        )
    };
    Some(ExactDyadic {
        coefficient: significand.checked_mul(1_000_000_000)?,
        binary_exponent,
    })
}

fn compare_positive_dyadics(left: ExactDyadic, right: ExactDyadic) -> Ordering {
    match (left.coefficient == 0, right.coefficient == 0) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        (false, false) => {},
    }

    let left_top_bit =
        (u128::BITS - 1 - left.coefficient.leading_zeros()) as i32 + left.binary_exponent;
    let right_top_bit =
        (u128::BITS - 1 - right.coefficient.leading_zeros()) as i32 + right.binary_exponent;
    match left_top_bit.cmp(&right_top_bit) {
        Ordering::Equal => {},
        order => return order,
    }

    match left.binary_exponent.cmp(&right.binary_exponent) {
        Ordering::Equal => left.coefficient.cmp(&right.coefficient),
        Ordering::Greater => {
            let shift = (left.binary_exponent - right.binary_exponent) as u32;
            debug_assert!(shift < u128::BITS);
            (left.coefficient << shift).cmp(&right.coefficient)
        },
        Ordering::Less => {
            let shift = (right.binary_exponent - left.binary_exponent) as u32;
            debug_assert!(shift < u128::BITS);
            left.coefficient.cmp(&(right.coefficient << shift))
        },
    }
}

fn integer_and_fraction(value: ExactDyadic) -> (Option<u128>, ExactDyadic) {
    if value.coefficient == 0 {
        return (Some(0), ExactDyadic::ZERO);
    }
    if value.binary_exponent >= 0 {
        let shift = value.binary_exponent as u32;
        let integer = (shift < u128::BITS && value.coefficient <= (u128::MAX >> shift))
            .then(|| value.coefficient << shift);
        return (integer, ExactDyadic::ZERO);
    }

    let shift = (-value.binary_exponent) as u32;
    if shift >= u128::BITS {
        return (Some(0), value);
    }
    let integer = value.coefficient >> shift;
    let remainder_mask = (1_u128 << shift) - 1;
    (
        Some(integer),
        ExactDyadic {
            coefficient: value.coefficient & remainder_mask,
            binary_exponent: value.binary_exponent,
        },
    )
}

fn checked_shifted_integer_difference(larger: ExactDyadic, smaller: ExactDyadic) -> Option<u128> {
    let common_exponent = larger.binary_exponent.min(smaller.binary_exponent);
    if common_exponent < 0 {
        return None;
    }

    let shift_coefficient = |value: ExactDyadic| {
        let shift = (value.binary_exponent - common_exponent) as u32;
        if shift >= u128::BITS || value.coefficient > (u128::MAX >> shift) {
            return None;
        }
        Some(value.coefficient << shift)
    };
    let difference = shift_coefficient(larger)?.checked_sub(shift_coefficient(smaller)?)?;
    if difference == 0 {
        return Some(0);
    }
    let shift = common_exponent as u32;
    if shift >= u128::BITS || difference > (u128::MAX >> shift) {
        return None;
    }
    Some(difference << shift)
}

fn dyadic_fraction_sum_exceeds_one(left: ExactDyadic, right: ExactDyadic) -> bool {
    if left.coefficient == 0 || right.coefficient == 0 {
        return false;
    }
    debug_assert!(left.binary_exponent < 0 && right.binary_exponent < 0);

    // With both denominators at least 2^128 and both numerators below 2^83, their sum is below 1.
    if left.binary_exponent <= -(u128::BITS as i32) && right.binary_exponent <= -(u128::BITS as i32)
    {
        return false;
    }
    let (base, other) = if left.binary_exponent > -(u128::BITS as i32) {
        (left, right)
    } else {
        (right, left)
    };
    let denominator_shift = (-base.binary_exponent) as u32;
    let complement = ExactDyadic {
        coefficient: (1_u128 << denominator_shift) - base.coefficient,
        binary_exponent: base.binary_exponent,
    };
    compare_positive_dyadics(other, complement) == Ordering::Greater
}

fn ceil_exact_dyadic_sum(left: ExactDyadic, right: ExactDyadic) -> Option<u128> {
    let (left_integer, left_fraction) = integer_and_fraction(left);
    let (right_integer, right_fraction) = integer_and_fraction(right);
    let integer_sum = left_integer?.checked_add(right_integer?)?;
    let fractional_ceiling = match (
        left_fraction.coefficient == 0,
        right_fraction.coefficient == 0,
    ) {
        (true, true) => 0,
        (true, false) | (false, true) => 1,
        (false, false) if dyadic_fraction_sum_exceeds_one(left_fraction, right_fraction) => 2,
        (false, false) => 1,
    };
    integer_sum.checked_add(fractional_ceiling)
}

fn ceil_exact_dyadic_difference(larger: ExactDyadic, smaller: ExactDyadic) -> Option<u128> {
    debug_assert!(compare_positive_dyadics(larger, smaller) != Ordering::Less);
    let (larger_integer, larger_fraction) = integer_and_fraction(larger);
    let (smaller_integer, smaller_fraction) = integer_and_fraction(smaller);
    if let (Some(larger_integer), Some(smaller_integer)) = (larger_integer, smaller_integer) {
        let integer_difference = larger_integer.checked_sub(smaller_integer)?;
        return integer_difference.checked_add(u128::from(
            compare_positive_dyadics(larger_fraction, smaller_fraction) == Ordering::Greater,
        ));
    }

    checked_shifted_integer_difference(larger, smaller)
}

fn exact_f64_difference_ceiling_nanos(timeline_time: f64, terminal_time: f64) -> Option<u128> {
    if !timeline_time.is_finite() || !terminal_time.is_finite() {
        return None;
    }
    if terminal_time <= timeline_time {
        return Some(0);
    }

    let timeline = scaled_abs_f64_nanoseconds(timeline_time)?;
    let terminal = scaled_abs_f64_nanoseconds(terminal_time)?;
    match (
        terminal_time.is_sign_negative(),
        timeline_time.is_sign_negative(),
    ) {
        (false, false) => ceil_exact_dyadic_difference(terminal, timeline),
        (true, true) => ceil_exact_dyadic_difference(timeline, terminal),
        (false, true) => ceil_exact_dyadic_sum(terminal, timeline),
        (true, false) => None,
    }
}

fn finite_deadline_from_seconds(
    document_now: DocumentTime,
    timeline_now: f64,
    terminal_time: f64,
) -> Option<DocumentTime> {
    // A document deadline is an integer nanosecond boundary. Rounding to nearest (or down) can
    // make a source eligible before its represented terminal time. Subtract the represented f64
    // inputs as exact binary rationals and ceil their nanosecond difference only once. This avoids
    // f64 subtraction/multiplication round-down as well as u64 narrowing.
    let duration_nanos = exact_f64_difference_ceiling_nanos(timeline_now, terminal_time)?;
    document_now
        .as_nanos()
        .checked_add(duration_nanos)
        .map(DocumentTime::from_nanos)
}

fn classify_css_animation(
    state: DocumentCssAnimationState,
    iteration: DocumentCssIterationState,
    started_at: f64,
    duration: f64,
    delay: f64,
    timeline_now: f64,
    document_now: DocumentTime,
) -> DocumentSettlementSourceDisposition {
    if matches!(
        state,
        DocumentCssAnimationState::Finished | DocumentCssAnimationState::Canceled
    ) {
        return DocumentSettlementSourceDisposition::Inert;
    }
    if matches!(state, DocumentCssAnimationState::Paused(_)) {
        return DocumentSettlementSourceDisposition::Inert;
    }
    if matches!(iteration, DocumentCssIterationState::Infinite { .. }) {
        return DocumentSettlementSourceDisposition::OpenEnded(
            DocumentOpenEndedSourceReason::InfiniteCssAnimation,
        );
    }

    let DocumentCssIterationState::Finite {
        current_bits,
        max_bits,
    } = iteration
    else {
        unreachable!("infinite CSS animations returned above")
    };
    let current = f64::from_bits(current_bits);
    let max = f64::from_bits(max_bits);
    // Stylo stores `started_at` in seconds and already incorporates CSS delay. `delay` remains in
    // the fingerprint to detect replacement but must not be added to the terminal time again.
    if !started_at.is_finite() ||
        !duration.is_finite() ||
        !delay.is_finite() ||
        !current.is_finite() ||
        !max.is_finite() ||
        duration < 0.0 ||
        current < 0.0 ||
        max < current
    {
        return DocumentSettlementSourceDisposition::Unsupported(
            DocumentUnsupportedSourceReason::InvalidCssTiming,
        );
    }
    let Some(terminal_time) = duration
        .mul_add(max - current, started_at)
        .is_finite()
        .then(|| duration.mul_add(max - current, started_at))
    else {
        return DocumentSettlementSourceDisposition::Unsupported(
            DocumentUnsupportedSourceReason::InvalidCssTiming,
        );
    };
    finite_deadline_from_seconds(document_now, timeline_now, terminal_time)
        .map(DocumentSettlementSourceDisposition::FiniteDeadline)
        .unwrap_or(DocumentSettlementSourceDisposition::Unsupported(
            DocumentUnsupportedSourceReason::InvalidCssTiming,
        ))
}

fn classify_css_transition(
    state: DocumentCssAnimationState,
    start_time: f64,
    duration: f64,
    delay: f64,
    timeline_now: f64,
    document_now: DocumentTime,
) -> DocumentSettlementSourceDisposition {
    if matches!(
        state,
        DocumentCssAnimationState::Finished |
            DocumentCssAnimationState::Canceled |
            DocumentCssAnimationState::Paused(_)
    ) {
        return DocumentSettlementSourceDisposition::Inert;
    }
    // Stylo stores `start_time` in seconds and already incorporates transition delay.
    if !start_time.is_finite() || !duration.is_finite() || !delay.is_finite() || duration < 0.0 {
        return DocumentSettlementSourceDisposition::Unsupported(
            DocumentUnsupportedSourceReason::InvalidCssTiming,
        );
    }
    let terminal_time = start_time + duration;
    finite_deadline_from_seconds(document_now, timeline_now, terminal_time)
        .map(DocumentSettlementSourceDisposition::FiniteDeadline)
        .unwrap_or(DocumentSettlementSourceDisposition::Unsupported(
            DocumentUnsupportedSourceReason::InvalidCssTiming,
        ))
}

/// Canonically ordered, typed inventory of the source classes owned by this preparation slice.
///
/// This vector is not an exhaustive inventory of every rendering prerequisite. In particular,
/// current animated-image frame opportunities are represented by ScriptThread scheduler deadlines
/// and document rendering-readiness observations rather than separate typed entries here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentSettlementSourceSnapshot {
    source_epoch: DocumentSettlementSourceEpoch,
    sources: Vec<DocumentSettlementSource>,
}

/// Canonical Canvas registry generation and image-key set consumed by a capture candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentCanvasCaptureBinding {
    registry_generation: u64,
    image_keys: Vec<DocumentCanvasImageKey>,
}

impl DocumentCanvasCaptureBinding {
    pub const fn registry_generation(&self) -> u64 {
        self.registry_generation
    }

    pub fn image_keys(&self) -> &[DocumentCanvasImageKey] {
        &self.image_keys
    }
}

/// A Canvas source vector was not a single mechanically ready registry generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentCanvasCaptureBindingError {
    MalformedSource,
    SourceNotReady,
    MissingImageKey,
    MissingGeneration,
    MixedGeneration,
    DuplicateImageKey,
}

impl DocumentSettlementSourceSnapshot {
    /// Construct and canonically order one internally observed source inventory.
    #[doc(hidden)]
    pub fn new_internal(
        source_epoch: DocumentSettlementSourceEpoch,
        mut sources: Vec<DocumentSettlementSource>,
    ) -> Self {
        sources.sort_unstable();
        Self {
            source_epoch,
            sources,
        }
    }

    /// Return this preparation observation's monotonic source epoch.
    pub const fn source_epoch(&self) -> DocumentSettlementSourceEpoch {
        self.source_epoch
    }

    /// Return every source entry retained in this snapshot, in canonical order.
    pub fn sources(&self) -> &[DocumentSettlementSource] {
        &self.sources
    }

    /// Derive the exact Canvas token retained by this canonical source snapshot.
    pub fn canvas_capture_binding(
        &self,
    ) -> Result<Option<DocumentCanvasCaptureBinding>, DocumentCanvasCaptureBindingError> {
        let mut registry_generation = None;
        let mut image_keys = Vec::new();
        for source in &self.sources {
            if !matches!(
                &source.identity,
                DocumentSettlementSourceIdentity::Canvas2d { .. }
            ) {
                continue;
            }
            let DocumentSettlementSourceState::Canvas2d {
                image_key,
                registry_generation: observed_generation,
                status,
                ..
            } = &source.state
            else {
                return Err(DocumentCanvasCaptureBindingError::MalformedSource);
            };
            if *status != DocumentCanvasCaptureStatus::Ready ||
                source.disposition != DocumentSettlementSourceDisposition::Inert
            {
                return Err(DocumentCanvasCaptureBindingError::SourceNotReady);
            }
            let image_key = (*image_key)
                .ok_or(DocumentCanvasCaptureBindingError::MissingImageKey)?;
            let observed_generation = (*observed_generation)
                .ok_or(DocumentCanvasCaptureBindingError::MissingGeneration)?;
            match registry_generation {
                Some(expected) if expected != observed_generation => {
                    return Err(DocumentCanvasCaptureBindingError::MixedGeneration);
                },
                None => registry_generation = Some(observed_generation),
                Some(_) => {},
            }
            image_keys.push(image_key);
        }
        let Some(registry_generation) = registry_generation else {
            return Ok(None);
        };
        image_keys.sort_unstable();
        if image_keys.windows(2).any(|keys| keys[0] == keys[1]) {
            return Err(DocumentCanvasCaptureBindingError::DuplicateImageKey);
        }
        Ok(Some(DocumentCanvasCaptureBinding {
            registry_generation,
            image_keys,
        }))
    }

    /// Return whether no inventoried source needs another deadline or rendering opportunity.
    pub fn is_settled(&self) -> bool {
        self.sources
            .iter()
            .all(|source| source.disposition == DocumentSettlementSourceDisposition::Inert)
    }

    fn has_finite_work(&self) -> bool {
        self.sources.iter().any(|source| {
            matches!(
                source.disposition,
                DocumentSettlementSourceDisposition::FiniteDeadline(_) |
                    DocumentSettlementSourceDisposition::FiniteRenderingOpportunity
            )
        })
    }

    fn has_open_ended_work(&self) -> bool {
        self.sources.iter().any(|source| {
            matches!(
                source.disposition,
                DocumentSettlementSourceDisposition::OpenEnded(_)
            )
        })
    }

    fn has_unsupported_work(&self) -> bool {
        self.sources.iter().any(|source| {
            matches!(
                source.disposition,
                DocumentSettlementSourceDisposition::Unsupported(_)
            )
        })
    }
}

/// Exact script/layout generation bound by a capture precondition.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DocumentCaptureDocumentEpoch {
    /// Pipeline whose script/layout generation was observed.
    pub pipeline_id: PipelineId,
    /// Exact script rendering epoch; Paint presentation is deliberately not implied.
    pub script_rendering_epoch: Epoch,
    /// Exact readiness vector observed at issuance; qualification requires this to be empty.
    pub readiness_blockers: Vec<DocumentTimeReadinessBlocker>,
}

/// Mechanical reason a preparation observation cannot issue a capture precondition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DocumentCaptureBlocker {
    /// This first slice accepts exactly one fully-active painted pipeline.
    MultipleFullyActivePipelines,
    /// Buffered ordinary input remains.
    PendingInput,
    /// The bounded intake batch filled, so more ready input may exist.
    InputBatchSaturated,
    /// Producer identity/watermarks are not qualified StableEmpty.
    ProducerNotStableEmpty,
    /// Two fresh quiet checkpoints have not observed identical execution/source generations since
    /// the last change.
    QuietCheckpointsUnqualified,
    /// No controlled execution policy is installed.
    ExecutionPolicyUnavailable,
    /// The controlled execution ledger has a sticky terminal outcome.
    ExecutionTerminated,
    /// At least one document readiness predicate failed.
    DocumentNotReady,
    /// At least one fully-active document has no rendering epoch.
    DocumentEpochUnavailable,
    /// The timer scheduler still owns a finite deadline.
    FiniteTimerDeadline,
    /// A classified source still needs a finite deadline or rendering opportunity.
    ActiveFiniteSource,
    /// An interval or infinite animation has no terminal opportunity.
    OpenEndedSource,
    /// An observed source is outside this slice's settlement ownership.
    UnsupportedSource,
    /// The requested viewport, rectangle, or device scale is invalid.
    InvalidSurface,
}

/// Opaque, immutable candidate evidence for a later generation-bound capture attempt.
///
/// Private fields prevent callers from editing individual facts on a returned value, but they are
/// not an authenticity boundary: this same-build protocol is serializable and its doc-hidden
/// cross-crate constructor is public. ScriptThread retains the candidate it issued. A future
/// consumer must atomically match that retained value, reobserve every bound dimension, require the
/// matching presented Paint generation, and only then remove the candidate and capture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentCapturePrecondition {
    id: DocumentCapturePreconditionId,
    target: DocumentTimeControlTarget,
    clock_id: DocumentClockId,
    now: DocumentTime,
    input_revision: u64,
    pending_events: u64,
    input_batch_saturated: bool,
    producers: DocumentTimeProducerObservation,
    execution: DocumentExecutionObservation,
    next_deadline: Option<TimerDeadlineSnapshot>,
    sources: DocumentSettlementSourceSnapshot,
    documents: Vec<DocumentCaptureDocumentEpoch>,
    surface: DocumentCaptureSurfaceFingerprint,
}

impl DocumentCapturePrecondition {
    /// Construct candidate evidence from one mechanically qualified internal observation.
    ///
    /// This doc-hidden function is public only because ScriptThread and the protocol types live in
    /// different crates. It does not authenticate its caller; private fields make returned values
    /// opaque and immutable through the ordinary API, not unforgeable.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn new_internal(
        id: DocumentCapturePreconditionId,
        target: DocumentTimeControlTarget,
        clock_id: DocumentClockId,
        now: DocumentTime,
        input_revision: u64,
        pending_events: u64,
        input_batch_saturated: bool,
        producers: DocumentTimeProducerObservation,
        execution: DocumentExecutionObservation,
        next_deadline: Option<TimerDeadlineSnapshot>,
        sources: DocumentSettlementSourceSnapshot,
        mut documents: Vec<DocumentCaptureDocumentEpoch>,
        surface: DocumentCaptureSurfaceFingerprint,
    ) -> Self {
        documents.sort_unstable();
        Self {
            id,
            target,
            clock_id,
            now,
            input_revision,
            pending_events,
            input_batch_saturated,
            producers,
            execution,
            next_deadline,
            sources,
            documents,
            surface,
        }
    }

    /// Return this candidate's single-use ScriptThread sequence.
    pub const fn id(&self) -> DocumentCapturePreconditionId {
        self.id
    }

    /// Return the exact controlled target bound by this precondition.
    pub const fn target(&self) -> &DocumentTimeControlTarget {
        &self.target
    }

    /// Return the exact clock domain bound by this precondition.
    pub const fn clock_id(&self) -> DocumentClockId {
        self.clock_id
    }

    /// Return the exact document time bound by this precondition.
    pub const fn now(&self) -> DocumentTime {
        self.now
    }

    /// Return the ordinary-input revision bound by this precondition.
    pub const fn input_revision(&self) -> u64 {
        self.input_revision
    }

    /// Return the exact buffered ordinary-event count observed at issuance.
    pub const fn pending_events(&self) -> u64 {
        self.pending_events
    }

    /// Return whether bounded input intake saturated at issuance.
    pub const fn input_batch_saturated(&self) -> bool {
        self.input_batch_saturated
    }

    /// Return the qualified StableEmpty producer observation.
    pub const fn producers(&self) -> DocumentTimeProducerObservation {
        self.producers
    }

    /// Return the exact nonterminal execution observation.
    pub const fn execution(&self) -> DocumentExecutionObservation {
        self.execution
    }

    /// Return the exact finite scheduler deadline observation; issuance requires `None`.
    pub const fn next_deadline(&self) -> Option<TimerDeadlineSnapshot> {
        self.next_deadline
    }

    /// Return the canonical typed exact source snapshot.
    pub const fn sources(&self) -> &DocumentSettlementSourceSnapshot {
        &self.sources
    }

    /// Return canonically ordered script/layout document generations.
    pub fn documents(&self) -> &[DocumentCaptureDocumentEpoch] {
        &self.documents
    }

    /// Return the exact requested capture surface.
    pub const fn surface(&self) -> DocumentCaptureSurfaceFingerprint {
        self.surface
    }
}

/// Opaque identity of one Paint presentation reservation for a capture candidate.
///
/// This is a correlation value on Servo's trusted internal channels, not cryptographic authority.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct DocumentPaintPresentationTicketId(u64);

impl DocumentPaintPresentationTicketId {
    /// Construct an identifier from Paint's checked presentation sequence.
    #[doc(hidden)]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Return Paint's underlying presentation sequence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Paint's exact presentation correlation for one retained ScriptThread capture candidate.
///
/// Private fields provide ordinary API immutability and correlation only. Values remain
/// synthesizable through the doc-hidden cross-crate constructor: ScriptThread validates its
/// retained candidate, Paint validates its retained ticket, and trusted internal routing is an
/// explicit assumption rather than cryptographic authority.
#[doc(hidden)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentPaintPresentationTicket {
    id: DocumentPaintPresentationTicketId,
    candidate_id: DocumentCapturePreconditionId,
    target: DocumentTimeControlTarget,
    pipeline_id: PipelineId,
    script_rendering_epoch: Epoch,
    surface: DocumentCaptureSurfaceFingerprint,
    presentation_generation: u64,
    publish_generation: u64,
}

impl DocumentPaintPresentationTicket {
    /// Construct a ticket from Paint's checked presentation state.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub const fn new_internal(
        id: DocumentPaintPresentationTicketId,
        candidate_id: DocumentCapturePreconditionId,
        target: DocumentTimeControlTarget,
        pipeline_id: PipelineId,
        script_rendering_epoch: Epoch,
        surface: DocumentCaptureSurfaceFingerprint,
        presentation_generation: u64,
        publish_generation: u64,
    ) -> Self {
        Self {
            id,
            candidate_id,
            target,
            pipeline_id,
            script_rendering_epoch,
            surface,
            presentation_generation,
            publish_generation,
        }
    }

    /// Return this single-use Paint ticket's identity.
    pub const fn id(&self) -> DocumentPaintPresentationTicketId {
        self.id
    }

    /// Return the ScriptThread candidate identity reserved by Paint.
    pub const fn candidate_id(&self) -> DocumentCapturePreconditionId {
        self.candidate_id
    }

    /// Return the exact controlled target reserved by Paint.
    pub const fn target(&self) -> &DocumentTimeControlTarget {
        &self.target
    }

    /// Return the single fully-active pipeline whose presentation was reserved.
    pub const fn pipeline_id(&self) -> PipelineId {
        self.pipeline_id
    }

    /// Return the exact Script/Layout rendering epoch presented by Paint.
    pub const fn script_rendering_epoch(&self) -> Epoch {
        self.script_rendering_epoch
    }

    /// Return the exact raster surface reserved by Paint.
    pub const fn surface(&self) -> DocumentCaptureSurfaceFingerprint {
        self.surface
    }

    /// Return Paint's checked presentation-state generation.
    pub const fn presentation_generation(&self) -> u64 {
        self.presentation_generation
    }

    /// Return the exact renderer publish generation processed for the presentation.
    pub const fn publish_generation(&self) -> u64 {
        self.publish_generation
    }
}

/// Single-use request to consume an exact retained candidate against a Paint presentation ticket.
#[doc(hidden)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentCaptureConsumeRequest {
    precondition: Box<DocumentCapturePrecondition>,
    ticket: DocumentPaintPresentationTicket,
}

impl DocumentCaptureConsumeRequest {
    /// Bind caller-held candidate evidence to Paint's presentation correlation.
    #[doc(hidden)]
    pub fn new_internal(
        precondition: Box<DocumentCapturePrecondition>,
        ticket: DocumentPaintPresentationTicket,
    ) -> Self {
        Self {
            precondition,
            ticket,
        }
    }

    /// Return the exact caller-held precondition.
    pub const fn precondition(&self) -> &DocumentCapturePrecondition {
        &self.precondition
    }

    /// Return Paint's exact presentation ticket.
    pub const fn ticket(&self) -> &DocumentPaintPresentationTicket {
        &self.ticket
    }
}

/// ScriptThread's single-use commit of one cached, generation-bound layout snapshot.
///
/// A commit correlates the consumed candidate and Paint ticket with the exact cached layout bytes.
/// It does not by itself prove that a later artifact write or Paint finalization succeeded.
#[doc(hidden)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentCaptureCommit {
    candidate_id: DocumentCapturePreconditionId,
    ticket_id: DocumentPaintPresentationTicketId,
    target: DocumentTimeControlTarget,
    pipeline_id: PipelineId,
    script_rendering_epoch: Epoch,
    surface: DocumentCaptureSurfaceFingerprint,
    presentation_generation: u64,
    publish_generation: u64,
    layout_paint_epoch: Epoch,
    serialized_layout_snapshot: String,
}

impl DocumentCaptureCommit {
    /// Construct a commit at ScriptThread's candidate-consumption linearization point.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn new_internal(
        candidate_id: DocumentCapturePreconditionId,
        ticket_id: DocumentPaintPresentationTicketId,
        target: DocumentTimeControlTarget,
        pipeline_id: PipelineId,
        script_rendering_epoch: Epoch,
        surface: DocumentCaptureSurfaceFingerprint,
        presentation_generation: u64,
        publish_generation: u64,
        layout_paint_epoch: Epoch,
        serialized_layout_snapshot: String,
    ) -> Self {
        Self {
            candidate_id,
            ticket_id,
            target,
            pipeline_id,
            script_rendering_epoch,
            surface,
            presentation_generation,
            publish_generation,
            layout_paint_epoch,
            serialized_layout_snapshot,
        }
    }

    /// Return the consumed ScriptThread candidate identity.
    pub const fn candidate_id(&self) -> DocumentCapturePreconditionId {
        self.candidate_id
    }

    /// Return the correlated Paint ticket identity.
    pub const fn ticket_id(&self) -> DocumentPaintPresentationTicketId {
        self.ticket_id
    }

    /// Return the exact controlled target consumed by ScriptThread.
    pub const fn target(&self) -> &DocumentTimeControlTarget {
        &self.target
    }

    /// Return the captured pipeline.
    pub const fn pipeline_id(&self) -> PipelineId {
        self.pipeline_id
    }

    /// Return the exact Script/Layout rendering epoch committed by ScriptThread.
    pub const fn script_rendering_epoch(&self) -> Epoch {
        self.script_rendering_epoch
    }

    /// Return the exact capture surface committed by ScriptThread.
    pub const fn surface(&self) -> DocumentCaptureSurfaceFingerprint {
        self.surface
    }

    /// Return Paint's correlated presentation-state generation.
    pub const fn presentation_generation(&self) -> u64 {
        self.presentation_generation
    }

    /// Return the correlated renderer publish generation.
    pub const fn publish_generation(&self) -> u64 {
        self.publish_generation
    }

    /// Return the exact paint epoch embedded in the cached layout snapshot.
    pub const fn layout_paint_epoch(&self) -> Epoch {
        self.layout_paint_epoch
    }

    /// Return the serialized cached layout snapshot.
    pub fn serialized_layout_snapshot(&self) -> &str {
        &self.serialized_layout_snapshot
    }
}

/// Result of one preparation command, whether or not it qualified for issuance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentCapturePreparation {
    /// Exact requested surface.
    pub surface: DocumentCaptureSurfaceFingerprint,
    /// Canonical source inventory collected at this preparation epoch.
    pub sources: DocumentSettlementSourceSnapshot,
    /// Canonically ordered reasons issuance was refused.
    pub blockers: Vec<DocumentCaptureBlocker>,
    /// Opaque candidate evidence emitted only when `blockers` is empty.
    pub precondition: Option<Box<DocumentCapturePrecondition>>,
}

/// Retain an already-qualified StableEmpty producer fact across a same-checkpoint preparation.
///
/// `PrepareCapture` performs no checkpoint, so the ordinary producer observer reports
/// `UnchangedCheckpoint`. The independent quiet fence decides whether two prior fresh checkpoints
/// still match every normalized capture dimension. This helper cannot make `FirstEmpty`, `Busy`,
/// or `NotCheckpointed` stable.
#[doc(hidden)]
pub fn capture_producers_internal(
    producers: DocumentTimeProducerObservation,
    quiet_checkpoints_qualified: bool,
) -> DocumentTimeProducerObservation {
    if quiet_checkpoints_qualified &&
        producers.snapshot.is_empty() &&
        matches!(
            producers.stability,
            DocumentProducerStability::StableEmpty | DocumentProducerStability::UnchangedCheckpoint
        )
    {
        DocumentTimeProducerObservation {
            stability: DocumentProducerStability::StableEmpty,
            ..producers
        }
    } else {
        producers
    }
}

/// Derive canonical blockers from one mechanically qualified ScriptThread observation.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn capture_blockers_internal(
    target: &DocumentTimeControlTarget,
    surface: &DocumentCaptureSurfaceFingerprint,
    pending_events: u64,
    input_batch_saturated: bool,
    producers: DocumentTimeProducerObservation,
    documents: &[DocumentTimeDocumentObservation],
    execution: Option<DocumentExecutionObservation>,
    has_finite_timer_deadline: bool,
    sources: &DocumentSettlementSourceSnapshot,
    quiet_checkpoints_qualified: bool,
) -> Vec<DocumentCaptureBlocker> {
    let facts = CaptureQualificationFacts {
        multiple_fully_active_pipelines: target.fully_active_pipelines.len() != 1,
        pending_input: pending_events != 0,
        input_batch_saturated,
        producer_not_stable_empty: producers.stability != DocumentProducerStability::StableEmpty ||
            !producers.snapshot.is_empty(),
        quiet_checkpoints_unqualified: !quiet_checkpoints_qualified,
        execution_policy_unavailable: execution.is_none(),
        execution_terminated: execution.is_some_and(|value| value.terminal.is_some()),
        document_not_ready: documents
            .iter()
            .any(|document| !document.readiness_blockers.is_empty()),
        document_epoch_unavailable: documents
            .iter()
            .any(|document| document.script_rendering_epoch.is_none()),
        finite_timer_deadline: has_finite_timer_deadline,
        active_finite_source: sources.has_finite_work(),
        open_ended_source: sources.has_open_ended_work(),
        unsupported_source: sources.has_unsupported_work(),
        invalid_surface: surface.validate().is_err(),
    };
    facts.blockers()
}

#[derive(Clone, Copy, Debug, Default)]
struct CaptureQualificationFacts {
    multiple_fully_active_pipelines: bool,
    pending_input: bool,
    input_batch_saturated: bool,
    producer_not_stable_empty: bool,
    quiet_checkpoints_unqualified: bool,
    execution_policy_unavailable: bool,
    execution_terminated: bool,
    document_not_ready: bool,
    document_epoch_unavailable: bool,
    finite_timer_deadline: bool,
    active_finite_source: bool,
    open_ended_source: bool,
    unsupported_source: bool,
    invalid_surface: bool,
}

impl CaptureQualificationFacts {
    fn blockers(self) -> Vec<DocumentCaptureBlocker> {
        let candidates = [
            (
                self.multiple_fully_active_pipelines,
                DocumentCaptureBlocker::MultipleFullyActivePipelines,
            ),
            (self.pending_input, DocumentCaptureBlocker::PendingInput),
            (
                self.input_batch_saturated,
                DocumentCaptureBlocker::InputBatchSaturated,
            ),
            (
                self.producer_not_stable_empty,
                DocumentCaptureBlocker::ProducerNotStableEmpty,
            ),
            (
                self.quiet_checkpoints_unqualified,
                DocumentCaptureBlocker::QuietCheckpointsUnqualified,
            ),
            (
                self.execution_policy_unavailable,
                DocumentCaptureBlocker::ExecutionPolicyUnavailable,
            ),
            (
                self.execution_terminated,
                DocumentCaptureBlocker::ExecutionTerminated,
            ),
            (
                self.document_not_ready,
                DocumentCaptureBlocker::DocumentNotReady,
            ),
            (
                self.document_epoch_unavailable,
                DocumentCaptureBlocker::DocumentEpochUnavailable,
            ),
            (
                self.finite_timer_deadline,
                DocumentCaptureBlocker::FiniteTimerDeadline,
            ),
            (
                self.active_finite_source,
                DocumentCaptureBlocker::ActiveFiniteSource,
            ),
            (
                self.open_ended_source,
                DocumentCaptureBlocker::OpenEndedSource,
            ),
            (
                self.unsupported_source,
                DocumentCaptureBlocker::UnsupportedSource,
            ),
            (self.invalid_surface, DocumentCaptureBlocker::InvalidSurface),
        ];
        candidates
            .into_iter()
            .filter_map(|(present, blocker)| present.then_some(blocker))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use euclid::{Box2D, Point2D, Size2D};
    use servo_base::id::TEST_PIPELINE_ID;
    use servo_geometry::DeviceIndependentPixel;
    use timers::DocumentProducerFence;

    use super::*;

    fn surface() -> DocumentCaptureSurfaceFingerprint {
        DocumentCaptureSurfaceFingerprint::new(
            Size2D::<i32, DeviceIndependentPixel>::new(800, 600),
            Box2D::new(Point2D::new(0, 0), Point2D::new(800, 600)),
            2.0,
        )
        .unwrap()
    }

    fn animation(
        state: DocumentCssAnimationState,
        iteration: DocumentCssIterationState,
        started_at: f64,
        duration: f64,
        timeline_now: f64,
    ) -> DocumentSettlementSourceDisposition {
        DocumentSettlementSource::css_animation_internal(
            TEST_PIPELINE_ID,
            7,
            None,
            0,
            "pulse".to_owned(),
            state,
            iteration,
            started_at,
            duration,
            0.0,
            timeline_now,
            DocumentTime::from_nanos(10_000_000_000),
        )
        .disposition()
    }

    #[test]
    fn css_animation_classification_is_table_driven_and_fail_closed() {
        let finite = DocumentCssIterationState::Finite {
            current_bits: 0.0f64.to_bits(),
            max_bits: 2.0f64.to_bits(),
        };
        let cases = [
            (
                DocumentCssAnimationState::Finished,
                finite,
                0.0,
                1.0,
                0.0,
                DocumentSettlementSourceDisposition::Inert,
            ),
            (
                DocumentCssAnimationState::Canceled,
                finite,
                0.0,
                1.0,
                0.0,
                DocumentSettlementSourceDisposition::Inert,
            ),
            (
                DocumentCssAnimationState::Paused(0.5f64.to_bits()),
                finite,
                0.0,
                1.0,
                0.0,
                DocumentSettlementSourceDisposition::Inert,
            ),
            (
                DocumentCssAnimationState::Paused(0.5f64.to_bits()),
                DocumentCssIterationState::Infinite {
                    current_bits: 3.0f64.to_bits(),
                },
                0.0,
                1.0,
                0.0,
                DocumentSettlementSourceDisposition::Inert,
            ),
            (
                DocumentCssAnimationState::Running,
                DocumentCssIterationState::Infinite {
                    current_bits: 3.0f64.to_bits(),
                },
                0.0,
                1.0,
                0.0,
                DocumentSettlementSourceDisposition::OpenEnded(
                    DocumentOpenEndedSourceReason::InfiniteCssAnimation,
                ),
            ),
            (
                DocumentCssAnimationState::Pending,
                finite,
                9.0,
                1.0,
                10.0,
                DocumentSettlementSourceDisposition::FiniteDeadline(DocumentTime::from_nanos(
                    11_000_000_000,
                )),
            ),
            (
                DocumentCssAnimationState::Running,
                finite,
                f64::NAN,
                1.0,
                0.0,
                DocumentSettlementSourceDisposition::Unsupported(
                    DocumentUnsupportedSourceReason::InvalidCssTiming,
                ),
            ),
        ];

        for (state, iteration, started_at, duration, timeline_now, expected) in cases {
            assert_eq!(
                animation(state, iteration, started_at, duration, timeline_now),
                expected
            );
        }
    }

    #[test]
    fn timer_raf_and_transition_sources_distinguish_finite_open_and_inert() {
        let deadline = DocumentTime::from_nanos(20);
        let timeout =
            DocumentSettlementSource::timer_internal(TEST_PIPELINE_ID, 1, 2, 3, deadline, false);
        let interval =
            DocumentSettlementSource::timer_internal(TEST_PIPELINE_ID, 4, 5, 6, deadline, true);
        let active_raf =
            DocumentSettlementSource::animation_frame_internal(TEST_PIPELINE_ID, 7, true);
        let canceled_raf =
            DocumentSettlementSource::animation_frame_internal(TEST_PIPELINE_ID, 8, false);
        let transition = DocumentSettlementSource::css_transition_internal(
            TEST_PIPELINE_ID,
            9,
            None,
            0,
            "opacity".to_owned(),
            DocumentCssAnimationState::Running,
            5.0,
            2.0,
            0.0,
            6.0,
            DocumentTime::from_nanos(10_000_000_000),
        );

        assert_eq!(
            timeout.disposition(),
            DocumentSettlementSourceDisposition::FiniteDeadline(deadline)
        );
        assert!(matches!(
            interval.disposition(),
            DocumentSettlementSourceDisposition::OpenEnded(DocumentOpenEndedSourceReason::Interval)
        ));
        assert_eq!(
            active_raf.disposition(),
            DocumentSettlementSourceDisposition::FiniteRenderingOpportunity
        );
        assert_eq!(
            canceled_raf.disposition(),
            DocumentSettlementSourceDisposition::Inert
        );
        assert_eq!(
            transition.disposition(),
            DocumentSettlementSourceDisposition::FiniteDeadline(DocumentTime::from_nanos(
                11_000_000_000
            ))
        );
    }

    #[test]
    fn css_delay_is_retained_but_not_added_twice_to_stylo_start_times() {
        let animation = DocumentSettlementSource::css_animation_internal(
            TEST_PIPELINE_ID,
            7,
            None,
            0,
            "delayed".to_owned(),
            DocumentCssAnimationState::Pending,
            DocumentCssIterationState::Finite {
                current_bits: 0.0f64.to_bits(),
                max_bits: 1.0f64.to_bits(),
            },
            12.0,
            2.0,
            5.0,
            10.0,
            DocumentTime::from_nanos(20_000_000_000),
        );
        let transition = DocumentSettlementSource::css_transition_internal(
            TEST_PIPELINE_ID,
            8,
            None,
            0,
            "opacity".to_owned(),
            DocumentCssAnimationState::Pending,
            12.0,
            2.0,
            5.0,
            10.0,
            DocumentTime::from_nanos(20_000_000_000),
        );

        let expected = DocumentSettlementSourceDisposition::FiniteDeadline(
            DocumentTime::from_nanos(24_000_000_000),
        );
        assert_eq!(animation.disposition(), expected);
        assert_eq!(transition.disposition(), expected);
    }

    #[test]
    fn css_deadlines_round_up_to_the_next_document_nanosecond() {
        let document_now = DocumentTime::from_nanos(50);
        assert_eq!(
            finite_deadline_from_seconds(document_now, 0.0, 0.000_000_000_1),
            Some(DocumentTime::from_nanos(51)),
            "a positive sub-nanosecond remainder must not become immediately eligible"
        );
        assert_eq!(
            finite_deadline_from_seconds(document_now, 0.0, 0.000_000_001_1),
            Some(DocumentTime::from_nanos(52)),
            "a deadline just beyond one nanosecond must round to two"
        );
        assert_eq!(
            finite_deadline_from_seconds(document_now, 1.0, 1.0),
            Some(document_now),
            "an already-reached deadline remains at the current document time"
        );
        let multiplication_rounds_down = f64::from_bits(0x408a_4582_1ac2_8a3a);
        assert_eq!(
            exact_f64_difference_ceiling_nanos(0.0, multiplication_rounds_down),
            Some(840_688_527_604),
            "the exact rational remainder must survive f64 multiplication round-down"
        );
        assert_eq!(
            finite_deadline_from_seconds(document_now, 0.0, multiplication_rounds_down),
            Some(DocumentTime::from_nanos(840_688_527_654)),
            "the production deadline path must use the exact ceiling"
        );
        assert_eq!(
            multiplication_rounds_down * 1_000_000_000.0,
            840_688_527_603.0,
            "the adversarial value must exercise the old early-deadline path"
        );
        const WIDE_DURATION_SECONDS: u128 = 20_000_000_000;
        const WIDE_DURATION_NANOS: u128 = WIDE_DURATION_SECONDS * 1_000_000_000;
        assert_eq!(
            finite_deadline_from_seconds(document_now, 0.0, WIDE_DURATION_SECONDS as f64),
            Some(DocumentTime::from_nanos(
                WIDE_DURATION_NANOS + document_now.as_nanos()
            )),
            "an exactly representable deadline beyond u64 nanoseconds must remain representable"
        );
        assert_eq!(
            finite_deadline_from_seconds(DocumentTime::from_nanos(u128::MAX), 0.0, 0.000_000_001),
            None,
            "checked addition must reject the widened DocumentTime boundary"
        );
        assert_eq!(
            finite_deadline_from_seconds(document_now, 0.0, f64::MAX),
            None,
            "an unrepresentable finite duration must fail closed"
        );
    }

    #[test]
    fn css_deadline_difference_is_exact_before_nanosecond_ceiling() {
        let timeline = f64::from_bits(0x424a_a15d_115c_fa35);
        let terminal = f64::from_bits(0x4485_a225_941a_92da);
        assert_eq!(
            exact_f64_difference_ceiling_nanos(timeline, terminal),
            Some(12_770_097_062_108_432_031_046_045_257_569),
            "represented-input subtraction must not round the deadline down"
        );
        assert_eq!(
            finite_deadline_from_seconds(DocumentTime::from_nanos(0), timeline, terminal),
            Some(DocumentTime::from_nanos(
                12_770_097_062_108_432_031_046_045_257_569
            ))
        );

        assert_eq!(exact_f64_difference_ceiling_nanos(0.0, -0.0), Some(0));
        assert_eq!(exact_f64_difference_ceiling_nanos(-0.0, 0.0), Some(0));
        assert_eq!(
            exact_f64_difference_ceiling_nanos(0.0, f64::from_bits(1)),
            Some(1),
            "the smallest positive subnormal still reaches the next nanosecond"
        );
        assert_eq!(exact_f64_difference_ceiling_nanos(1.0, -1.0), Some(0));
        assert_eq!(
            exact_f64_difference_ceiling_nanos(-0.25, 0.25),
            Some(500_000_000)
        );
        assert_eq!(
            exact_f64_difference_ceiling_nanos(-1.0, -0.25),
            Some(750_000_000)
        );
        assert_eq!(exact_f64_difference_ceiling_nanos(f64::NAN, 1.0), None);
        assert_eq!(exact_f64_difference_ceiling_nanos(0.0, f64::INFINITY), None);
        assert_eq!(
            exact_f64_difference_ceiling_nanos(0.0, f64::from_bits(0x4611_2e0b_e826_d694)),
            Some(340_282_366_920_938_414_287_957_786_624_000_000_000)
        );
        assert_eq!(
            exact_f64_difference_ceiling_nanos(0.0, f64::from_bits(0x4611_2e0b_e826_d695)),
            None,
            "the next represented duration exceeds the u128 nanosecond domain"
        );
    }

    #[test]
    fn tracked_external_open_and_close_transitions_change_the_canonical_vector() {
        let cases = [
            (
                DocumentTrackedSourceKind::WebSocket,
                DocumentOpenEndedSourceReason::WebSocket,
            ),
            (
                DocumentTrackedSourceKind::EventSource,
                DocumentOpenEndedSourceReason::EventSource,
            ),
            (
                DocumentTrackedSourceKind::BroadcastChannel,
                DocumentOpenEndedSourceReason::BroadcastChannel,
            ),
            (
                DocumentTrackedSourceKind::MessagePort,
                DocumentOpenEndedSourceReason::MessagePort,
            ),
            (
                DocumentTrackedSourceKind::EmbedderControl,
                DocumentOpenEndedSourceReason::EmbedderControl,
            ),
            (
                DocumentTrackedSourceKind::MediaSessionActionHandler,
                DocumentOpenEndedSourceReason::MediaSessionActionHandler,
            ),
            (
                DocumentTrackedSourceKind::StorageEventListener,
                DocumentOpenEndedSourceReason::StorageEventListener,
            ),
        ];

        for (kind, reason) in cases {
            let closed =
                DocumentSettlementSource::tracked_presence_internal(TEST_PIPELINE_ID, kind, 0);
            let open =
                DocumentSettlementSource::tracked_presence_internal(TEST_PIPELINE_ID, kind, 1);
            assert_ne!(closed, open);
            assert_eq!(
                closed.disposition(),
                DocumentSettlementSourceDisposition::Inert
            );
            assert_eq!(
                open.disposition(),
                DocumentSettlementSourceDisposition::OpenEnded(reason)
            );
        }
    }

    #[test]
    fn tracked_unsupported_presence_is_fail_closed() {
        let cases = [
            (
                DocumentTrackedSourceKind::DedicatedWorker,
                DocumentUnsupportedSourceReason::DedicatedWorker,
            ),
            (
                DocumentTrackedSourceKind::Worklet,
                DocumentUnsupportedSourceReason::Worklet,
            ),
            (
                DocumentTrackedSourceKind::IndexedDb,
                DocumentUnsupportedSourceReason::IndexedDb,
            ),
            (
                DocumentTrackedSourceKind::GpuDevice,
                DocumentUnsupportedSourceReason::GpuDevice,
            ),
            (
                DocumentTrackedSourceKind::CookieStorePending,
                DocumentUnsupportedSourceReason::CookieStorePending,
            ),
            (
                DocumentTrackedSourceKind::ServiceWorkerPending,
                DocumentUnsupportedSourceReason::ServiceWorkerPending,
            ),
            (
                DocumentTrackedSourceKind::ServiceWorkerMessaging,
                DocumentUnsupportedSourceReason::ServiceWorkerMessaging,
            ),
            (
                DocumentTrackedSourceKind::WebXr,
                DocumentUnsupportedSourceReason::WebXr,
            ),
            (
                DocumentTrackedSourceKind::StorageManager,
                DocumentUnsupportedSourceReason::StorageManager,
            ),
            (
                DocumentTrackedSourceKind::Bluetooth,
                DocumentUnsupportedSourceReason::Bluetooth,
            ),
            (
                DocumentTrackedSourceKind::RtcPeerConnection,
                DocumentUnsupportedSourceReason::RtcPeerConnection,
            ),
            (
                DocumentTrackedSourceKind::MediaStream,
                DocumentUnsupportedSourceReason::MediaStream,
            ),
            (
                DocumentTrackedSourceKind::WebAudio,
                DocumentUnsupportedSourceReason::WebAudio,
            ),
            (
                DocumentTrackedSourceKind::Notification,
                DocumentUnsupportedSourceReason::Notification,
            ),
            (
                DocumentTrackedSourceKind::WebGpuApi,
                DocumentUnsupportedSourceReason::WebGpuApi,
            ),
            (
                DocumentTrackedSourceKind::OffscreenCanvas,
                DocumentUnsupportedSourceReason::OffscreenCanvas,
            ),
        ];

        for (kind, reason) in cases {
            let absent =
                DocumentSettlementSource::tracked_presence_internal(TEST_PIPELINE_ID, kind, 0);
            let present =
                DocumentSettlementSource::tracked_presence_internal(TEST_PIPELINE_ID, kind, 1);
            assert_eq!(
                absent.disposition(),
                DocumentSettlementSourceDisposition::Inert
            );
            assert_eq!(
                present.disposition(),
                DocumentSettlementSourceDisposition::Unsupported(reason)
            );
        }
    }

    #[test]
    fn ready_canvas_sources_produce_one_canonical_capture_binding() {
        let first = DocumentSettlementSource::canvas_2d_internal(
            TEST_PIPELINE_ID,
            8,
            3,
            Some(DocumentCanvasImageKey::new_internal(4, 9)),
            320,
            200,
            Some(17),
            DocumentCanvasCaptureStatus::Ready,
        );
        let second = DocumentSettlementSource::canvas_2d_internal(
            TEST_PIPELINE_ID,
            9,
            4,
            Some(DocumentCanvasImageKey::new_internal(4, 2)),
            100,
            50,
            Some(17),
            DocumentCanvasCaptureStatus::Ready,
        );
        let snapshot = DocumentSettlementSourceSnapshot::new_internal(
            DocumentSettlementSourceEpoch::new(2),
            vec![first, second],
        );

        assert!(snapshot.is_settled());
        let binding = snapshot.canvas_capture_binding().unwrap().unwrap();
        assert_eq!(binding.registry_generation(), 17);
        assert_eq!(
            binding.image_keys(),
            &[
                DocumentCanvasImageKey::new_internal(4, 2),
                DocumentCanvasImageKey::new_internal(4, 9),
            ]
        );
    }

    #[test]
    fn unsupported_and_mixed_canvas_sources_fail_closed() {
        let unsupported = DocumentSettlementSource::canvas_2d_internal(
            TEST_PIPELINE_ID,
            8,
            3,
            Some(DocumentCanvasImageKey::new_internal(4, 9)),
            320,
            200,
            Some(17),
            DocumentCanvasCaptureStatus::UnsupportedTranscript,
        );
        assert_eq!(
            unsupported.disposition(),
            DocumentSettlementSourceDisposition::Unsupported(
                DocumentUnsupportedSourceReason::Canvas2d
            )
        );

        let first = DocumentSettlementSource::canvas_2d_internal(
            TEST_PIPELINE_ID,
            8,
            3,
            Some(DocumentCanvasImageKey::new_internal(4, 9)),
            320,
            200,
            Some(17),
            DocumentCanvasCaptureStatus::Ready,
        );
        let second = DocumentSettlementSource::canvas_2d_internal(
            TEST_PIPELINE_ID,
            9,
            4,
            Some(DocumentCanvasImageKey::new_internal(4, 10)),
            320,
            200,
            Some(18),
            DocumentCanvasCaptureStatus::Ready,
        );
        let snapshot = DocumentSettlementSourceSnapshot::new_internal(
            DocumentSettlementSourceEpoch::new(2),
            vec![first, second],
        );
        assert_eq!(
            snapshot.canvas_capture_binding(),
            Err(DocumentCanvasCaptureBindingError::MixedGeneration)
        );
    }

    #[test]
    fn snapshots_sort_full_identities_without_collapsing_duplicates() {
        let later = DocumentSettlementSource::animation_frame_internal(TEST_PIPELINE_ID, 2, false);
        let earlier =
            DocumentSettlementSource::animation_frame_internal(TEST_PIPELINE_ID, 1, false);
        let snapshot = DocumentSettlementSourceSnapshot::new_internal(
            DocumentSettlementSourceEpoch::new(4),
            vec![later, earlier.clone(), earlier],
        );

        assert_eq!(snapshot.sources().len(), 3);
        assert_eq!(snapshot.sources()[0], snapshot.sources()[1]);
        assert!(snapshot.sources()[1] < snapshot.sources()[2]);
        assert!(snapshot.is_settled());
    }

    #[test]
    fn quiet_fence_needs_two_fresh_identical_checkpoints_and_resets_on_change() {
        let checkpoint_1 = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let checkpoint_2 = checkpoint_1.checked_next().unwrap();
        let checkpoint_3 = checkpoint_2.checked_next().unwrap();
        let checkpoint_4 = checkpoint_3.checked_next().unwrap();
        let mut fence = DocumentCaptureQuietFence::default();
        let initial_revision = (DocumentTime::ZERO, 0_u64, 0_u64);

        assert!(!fence.observe(checkpoint_1, initial_revision, true));
        assert!(
            !fence.observe(checkpoint_1, initial_revision, true),
            "same-checkpoint Observe or PrepareCapture cannot strengthen candidate one"
        );
        assert!(fence.observe(checkpoint_2, initial_revision, true));

        let changed_source_revision = (DocumentTime::ZERO, 0_u64, 1_u64);
        assert!(
            !fence.observe(checkpoint_2, changed_source_revision, true),
            "work or a source change at the same checkpoint clears the stable revision"
        );
        assert!(
            !fence.observe(checkpoint_3, changed_source_revision, true),
            "the first fresh quiet checkpoint after the change is only candidate one"
        );
        assert!(fence.observe(checkpoint_4, changed_source_revision, true));
    }

    #[test]
    fn quiet_fence_treats_clock_movement_and_mechanical_failure_as_changes() {
        let checkpoint_1 = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let checkpoint_2 = checkpoint_1.checked_next().unwrap();
        let checkpoint_3 = checkpoint_2.checked_next().unwrap();
        let checkpoint_4 = checkpoint_3.checked_next().unwrap();
        let mut fence = DocumentCaptureQuietFence::default();
        let at_zero = (DocumentTime::ZERO, 0_u64);

        assert!(!fence.observe(checkpoint_1, at_zero, true));
        assert!(fence.observe(checkpoint_2, at_zero, true));

        let after_advance = (DocumentTime::from_nanos(1), 0_u64);
        assert!(!fence.observe(checkpoint_2, after_advance, true));
        assert!(!fence.observe(checkpoint_3, after_advance, false));
        assert!(
            !fence.observe(checkpoint_4, after_advance, true),
            "a failed mechanical observation cannot be one of the two quiet checkpoints"
        );
    }

    #[test]
    fn same_checkpoint_preparation_preserves_but_never_creates_stable_empty() {
        let fence = DocumentProducerFence::default();
        let checkpoint = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let observation = |stability| DocumentTimeProducerObservation {
            fence_id: fence.id(),
            checkpoint,
            snapshot: fence.snapshot(),
            stability,
        };

        let cases = [
            (
                DocumentProducerStability::FirstEmpty,
                true,
                DocumentProducerStability::FirstEmpty,
            ),
            (
                DocumentProducerStability::UnchangedCheckpoint,
                false,
                DocumentProducerStability::UnchangedCheckpoint,
            ),
            (
                DocumentProducerStability::UnchangedCheckpoint,
                true,
                DocumentProducerStability::StableEmpty,
            ),
            (
                DocumentProducerStability::StableEmpty,
                true,
                DocumentProducerStability::StableEmpty,
            ),
        ];
        for (observed, quiet_qualified, expected) in cases {
            assert_eq!(
                capture_producers_internal(observation(observed), quiet_qualified).stability,
                expected
            );
        }
    }

    #[test]
    fn qualification_blockers_are_canonical_and_complete() {
        let cases = [
            (
                CaptureQualificationFacts::default(),
                Vec::<DocumentCaptureBlocker>::new(),
            ),
            (
                CaptureQualificationFacts {
                    pending_input: true,
                    open_ended_source: true,
                    invalid_surface: true,
                    ..CaptureQualificationFacts::default()
                },
                vec![
                    DocumentCaptureBlocker::PendingInput,
                    DocumentCaptureBlocker::OpenEndedSource,
                    DocumentCaptureBlocker::InvalidSurface,
                ],
            ),
            (
                CaptureQualificationFacts {
                    multiple_fully_active_pipelines: true,
                    execution_policy_unavailable: true,
                    document_epoch_unavailable: true,
                    finite_timer_deadline: true,
                    active_finite_source: true,
                    unsupported_source: true,
                    ..CaptureQualificationFacts::default()
                },
                vec![
                    DocumentCaptureBlocker::MultipleFullyActivePipelines,
                    DocumentCaptureBlocker::ExecutionPolicyUnavailable,
                    DocumentCaptureBlocker::DocumentEpochUnavailable,
                    DocumentCaptureBlocker::FiniteTimerDeadline,
                    DocumentCaptureBlocker::ActiveFiniteSource,
                    DocumentCaptureBlocker::UnsupportedSource,
                ],
            ),
        ];

        for (facts, expected) in cases {
            assert_eq!(facts.blockers(), expected);
        }
    }

    #[test]
    fn surface_validation_rejects_nonfinite_and_out_of_bounds_values() {
        assert!(surface().validate().is_ok());
        assert_eq!(
            DocumentCaptureSurfaceFingerprint::new(
                Size2D::new(800, 600),
                Box2D::new(Point2D::new(0, 0), Point2D::new(801, 600)),
                1.0,
            ),
            Err(DocumentCaptureSurfaceError::CaptureRectOutsideViewport)
        );
        assert_eq!(
            DocumentCaptureSurfaceFingerprint::new(
                Size2D::new(800, 600),
                Box2D::new(Point2D::new(-1, 0), Point2D::new(800, 600)),
                1.0,
            ),
            Err(DocumentCaptureSurfaceError::InvalidCaptureRect)
        );
        assert_eq!(
            DocumentCaptureSurfaceFingerprint::new(
                Size2D::new(800, 600),
                Box2D::new(Point2D::new(0, 0), Point2D::new(800, 600)),
                f32::NAN,
            ),
            Err(DocumentCaptureSurfaceError::InvalidDevicePixelScale)
        );
    }
}
