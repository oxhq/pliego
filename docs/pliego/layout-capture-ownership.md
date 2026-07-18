# Layout capture ownership

## Ownership chain

`ports/pliego` completes a stable-JavaScript session, then the capture follows this
single path:

`servoshell` stable-JavaScript callback -> `WebView::debug_layout_snapshot` ->
Constellation active-pipeline dispatch -> `ScriptThreadMessage::GetLayoutDebugSnapshot`
-> cached `Window::layout()` state -> `Layout::debug_snapshot` -> direct callback ->
`layout-debug.json`.

## Edited files

Upstream-derived fork surface:

- `components/constellation/constellation.rs`
- `components/constellation/tracing.rs`
- `components/layout/flow/root.rs`
- `components/layout/layout_impl.rs`
- `components/script/messaging.rs`
- `components/script/script_thread.rs`
- `components/servo/webview.rs`
- `components/shared/constellation/lib.rs`
- `components/shared/layout/lib.rs`
- `components/shared/script/lib.rs`
- `ports/servoshell/lib.rs`
- `ports/servoshell/running_app_state.rs`

Pliego-owned surface:

- `ownership.toml`
- `ports/pliego/src/main.rs`
- `ports/pliego/src/session.rs`
- `docs/pliego/layout-capture-ownership.md`

## Invariants and boundary

- No second layout is run. `Layout::debug_snapshot` reads the cached box, fragment,
  and stacking-context trees. It returns no snapshot when those caches require a new
  stacking-context tree or display list; it never requests reflow.
- The request uses `GenericCallback::new_blocking` and waits with a five-second
  `try_recv_timeout`. A missing pipeline, send failure, disconnected callback, or
  timeout produces no snapshot. The response travels directly from ScriptThread to
  the receiver and does not depend on pumping the Servo embedder event queue.
- The bounded wait can still delay stable-JavaScript completion or shutdown by up to
  five seconds.
- This is an internal debug artifact, not a stable Servo API or versioned file
  format. The Rust method is public only to cross the Servo-to-servoshell crate
  boundary. Field shape, kind labels, and process-local tag identifiers may change
  and must not be treated as compatibility or persistent-identity contracts.
