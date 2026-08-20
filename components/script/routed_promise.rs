/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::rc::Rc;

use js::context::JSContext;
use script_bindings::reflector::DomObject;
use serde::Serialize;
use serde::de::DeserializeOwned;
use servo_base::generic_channel::GenericCallback;

use crate::dom::bindings::refcounted::{Trusted, TrustedPromise};
use crate::dom::promise::Promise;
use crate::task_source::TaskSource;

pub(crate) trait RoutedPromiseListener<R: Serialize + DeserializeOwned + Send> {
    fn handle_response(&self, cx: &mut JSContext, response: R, promise: &Rc<Promise>);
}

pub(crate) struct RoutedPromiseContext<
    R: Serialize + DeserializeOwned + Send,
    T: RoutedPromiseListener<R> + DomObject,
> {
    trusted: TrustedPromise,
    receiver: Trusted<T>,
    _phantom: std::marker::PhantomData<R>,
}

impl<R: Serialize + DeserializeOwned + Send, T: RoutedPromiseListener<R> + DomObject>
    RoutedPromiseContext<R, T>
{
    fn response(self, cx: &mut JSContext, response: R) {
        let promise = self.trusted.root();
        self.receiver.root().handle_response(cx, response, &promise);
    }
}

pub(crate) fn callback_promise<
    R: Serialize + DeserializeOwned + Send + 'static,
    T: RoutedPromiseListener<R> + DomObject + 'static,
>(
    promise: &Rc<Promise>,
    receiver: &T,
    task_source: TaskSource,
) -> GenericCallback<R> {
    let task_source = task_source.to_sendable();
    // Acquire before the callback can escape to another thread/process. On success the callback
    // queues a normal Task lease synchronously before releasing this external-callback lease. If
    // callback creation, delivery, or the task handoff fails, ordinary RAII drop closes the lease.
    let mut external_callback = task_source.begin_external_callback();
    let mut trusted: Option<TrustedPromise> = Some(TrustedPromise::new(promise.clone()));
    let trusted_receiver = Trusted::new(receiver);
    GenericCallback::new(move |message| {
        let external_callback = external_callback.take();
        let trusted = if let Some(trusted) = trusted.take() {
            trusted
        } else {
            error!("RoutedPromiseListener callback called twice!");
            return;
        };

        let response = match message {
            Ok(response) => response,
            Err(error) => {
                warn!("Error receiving a routed promise response: {error:?}");
                return;
            },
        };

        let context = RoutedPromiseContext {
            trusted,
            receiver: trusted_receiver.clone(),
            _phantom: Default::default(),
        };
        task_source.queue(task!(routed_promise_task: move|cx| {
            context.response(cx, response);
        }));
        // `SendableTaskSource::queue` acquires the normal Task lease synchronously when accepted.
        // Keep the callback lease alive until that handoff call has returned.
        drop(external_callback);
    })
    .expect("Could not create callback in script.")
}
