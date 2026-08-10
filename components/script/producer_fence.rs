/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! ScriptThread-side adapters for document producer fences.

use std::fmt;
use std::sync::{Arc, Mutex};

use net_traits::image_cache::{
    ImageCacheResponseCallback, ImageCacheResponseMessage, ImageResponse,
};
use net_traits::{BoxedFetchCallback, FetchResponseMsg};
use timers::{DocumentProducerFence, DocumentProducerGuard, DocumentProducerKind};

/// A local event-loop message that keeps its producer live through ScriptThread handling.
pub(crate) struct DocumentProducerEnvelope<T> {
    pub(crate) message: T,
    _guard: Option<DocumentProducerGuard>,
}

impl<T: fmt::Debug> fmt::Debug for DocumentProducerEnvelope<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentProducerEnvelope")
            .field("message", &self.message)
            .field("producer_guarded", &self._guard.is_some())
            .finish()
    }
}

impl<T> DocumentProducerEnvelope<T> {
    pub(crate) fn new(message: T, guard: Option<DocumentProducerGuard>) -> Self {
        Self {
            message,
            _guard: guard,
        }
    }

    pub(crate) fn into_parts(self) -> (T, Option<DocumentProducerGuard>) {
        (self.message, self._guard)
    }
}

/// Keep a resource ticket live until the terminal callback has queued its ScriptThread work.
pub(crate) fn fence_fetch_callback(
    fence: &DocumentProducerFence,
    mut callback: BoxedFetchCallback,
    is_terminal: impl Fn(&FetchResponseMsg) -> bool + Send + 'static,
) -> BoxedFetchCallback {
    let mut guard = Some(
        fence
            .begin(DocumentProducerKind::Resource)
            .expect("document resource producer sequence exhausted"),
    );
    Box::new(move |message| {
        if guard.is_none() {
            return;
        }
        let terminal = is_terminal(&message);
        callback(message);
        if terminal {
            guard.take();
        }
    })
}

pub(crate) fn fence_fetch_until_eof(
    fence: &DocumentProducerFence,
    callback: BoxedFetchCallback,
) -> BoxedFetchCallback {
    fence_fetch_callback(fence, callback, |message| {
        matches!(message, FetchResponseMsg::ProcessResponseEOF(..))
    })
}

/// Keep an image ticket live through its terminal callback's ScriptThread enqueue.
pub(crate) fn fence_image_callback(
    fence: &DocumentProducerFence,
    callback: impl Fn(DocumentProducerEnvelope<ImageCacheResponseMessage>) + Send + Sync + 'static,
) -> ImageCacheResponseCallback {
    let fence = fence.clone();
    let guard = Arc::new(Mutex::new(Some(
        fence
            .begin(DocumentProducerKind::Image)
            .expect("document image producer sequence exhausted"),
    )));
    Box::new(move |message| {
        let terminal = match &message {
            ImageCacheResponseMessage::VectorImageRasterizationComplete(..) => true,
            ImageCacheResponseMessage::NotifyPendingImageLoadStatus(response) => matches!(
                &response.response,
                ImageResponse::Loaded(..) | ImageResponse::FailedToLoadOrDecode
            ),
        };
        let message_guard = {
            let mut guard = guard
                .lock()
                .expect("document image producer guard poisoned");
            if guard.is_none() {
                return;
            }
            if terminal {
                guard.take()
            } else {
                Some(
                    fence
                        .begin(DocumentProducerKind::Image)
                        .expect("document image message sequence exhausted"),
                )
            }
        };
        callback(DocumentProducerEnvelope::new(message, message_guard));
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use net_traits::request::RequestId;
    use net_traits::{ResourceFetchTiming, ResourceTimingType};
    use timers::{
        DocumentProducerCheckpoint, DocumentProducerObservation, DocumentProducerObserver,
    };

    use super::*;

    #[test]
    fn resource_completion_is_recorded_after_the_terminal_callback() {
        let fence = DocumentProducerFence::default();
        let callback_saw_pending = Arc::new(Mutex::new(Vec::new()));
        let observations = callback_saw_pending.clone();
        let callback_fence = fence.clone();
        let mut callback = fence_fetch_until_eof(
            &fence,
            Box::new(move |_| {
                observations
                    .lock()
                    .unwrap()
                    .push(callback_fence.snapshot().pending());
            }),
        );

        assert_eq!(fence.snapshot().pending(), 1);
        callback(FetchResponseMsg::ProcessRequestBody(RequestId::default()));
        assert_eq!(fence.snapshot().pending(), 1);
        callback(FetchResponseMsg::ProcessResponseEOF(
            RequestId::default(),
            Ok(()),
            ResourceFetchTiming::new(ResourceTimingType::Resource),
        ));

        assert_eq!(*callback_saw_pending.lock().unwrap(), vec![1, 1]);
        let complete = fence.snapshot();
        assert!(complete.is_empty());
        assert_eq!(
            complete
                .for_kind(DocumentProducerKind::Resource)
                .completed(),
            1
        );
    }

    #[test]
    fn dropping_an_abandoned_callback_completes_its_ticket() {
        let fence = DocumentProducerFence::default();
        let callback = fence_fetch_until_eof(&fence, Box::new(|_| {}));
        assert_eq!(fence.snapshot().pending(), 1);

        drop(callback);

        assert!(fence.snapshot().is_empty());
    }

    #[test]
    fn dropping_an_abandoned_image_listener_completes_its_ticket() {
        let fence = DocumentProducerFence::default();
        let callback = fence_image_callback(&fence, |_| {});
        assert_eq!(
            fence
                .snapshot()
                .for_kind(DocumentProducerKind::Image)
                .pending(),
            1
        );

        drop(callback);

        let complete = fence.snapshot();
        assert!(complete.is_empty());
        assert_eq!(
            complete.for_kind(DocumentProducerKind::Image).completed(),
            1
        );
    }

    fn queued_message_cannot_look_empty_after_its_producer_is_dropped(kind: DocumentProducerKind) {
        let fence = DocumentProducerFence::default();
        let producer_guard = fence.begin(kind).unwrap();
        let envelope =
            DocumentProducerEnvelope::new("queued message", Some(fence.begin(kind).unwrap()));
        drop(producer_guard);
        let mut observer = DocumentProducerObserver::default();
        let first = DocumentProducerCheckpoint::ZERO.checked_next().unwrap();
        let second = first.checked_next().unwrap();
        let third = second.checked_next().unwrap();
        let fourth = third.checked_next().unwrap();

        assert!(matches!(
            observer.observe(&fence, first),
            Ok(DocumentProducerObservation::Busy(_))
        ));
        assert!(matches!(
            observer.observe(&fence, second),
            Ok(DocumentProducerObservation::Busy(_))
        ));

        let (message, producer_guard) = envelope.into_parts();
        assert_eq!(message, "queued message");
        drop(producer_guard);

        assert!(matches!(
            observer.observe(&fence, third),
            Ok(DocumentProducerObservation::FirstEmpty(_))
        ));
        assert!(matches!(
            observer.observe(&fence, fourth),
            Ok(DocumentProducerObservation::StableEmpty(_))
        ));
    }

    #[test]
    fn queued_navigation_message_retains_its_resource_lease_after_callback_drop() {
        queued_message_cannot_look_empty_after_its_producer_is_dropped(
            DocumentProducerKind::Resource,
        );
    }

    #[test]
    fn queued_image_message_retains_its_image_lease_after_listener_drop() {
        queued_message_cannot_look_empty_after_its_producer_is_dropped(DocumentProducerKind::Image);
    }
}
