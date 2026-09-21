# Generation-bound capture preconditions

ScriptThread collects preparation evidence for Pliego's controlled document-capture coordinator.
A successful `PrepareCapture` response carries one opaque, immutable, single-use
candidate bound to the observed controlled target, clock and time, input state, producer snapshot,
execution counters, canonical source inventory, document rendering epoch, and requested raster
surface. It does not authenticate its own origin, freeze those dimensions, or establish that an
artifact corresponds to them.

Issuance is deliberately fail closed. It requires exactly one fully-active pipeline, no buffered or
possibly-undrained input, two fresh quiet checkpoints over the same normalized execution/source
generation, a final `StableEmpty` producer observation, no execution terminal, no readiness
blocker, no finite timer deadline, and only inert classified sources. A task, source, document,
clock, target, or input change restarts the two-checkpoint qualification. Any control command or
navigation invalidates a previously issued precondition; preparing again supersedes it.

The production consume operation first asks Paint to retain a presentation ticket for the exact
candidate pipeline, rendering epoch, raster surface, presentation revision, and WebRender publish
generation, without reading pixels. ScriptThread then matches the retained candidate and ticket,
reobserves every bound target/time/input/producer/execution/deadline/source/document/surface
dimension on both sides of layout-snapshot serialization, and consumes the candidate exactly once.
Only after that commit does Paint revalidate and consume the retained ticket and read pixels. A
mismatch or lost consume response is terminal for the session and cannot publish or retry the
candidate. Retained-value equality alone is not current-state or caller authentication. The
doc-hidden `new_internal` constructor is necessarily public across Servo crates; private fields
make returned values opaque and immutable through the ordinary API, not unforgeable.

The canonical source snapshot retains exact per-entry identities and states for finite and
DOM-node sources. For owner trackers where any nonzero population blocks issuance, it
retains a typed class identity and exact blocking presence/count. One object closing while another
opens cannot enable issuance because the class remains nonzero. It classifies CSS animations and
transitions, DOM timeouts and intervals, requestAnimationFrame slots, WebSockets, EventSource,
BroadcastChannel, MessagePort, visible embedder controls, explicit MediaSession action handlers,
storage-event listener, dedicated-worker and worklet presence, an
IndexedDB-factory sticky latch, live WebGPU devices, CookieStore's exact in-flight request count,
ServiceWorker's exact pending-algorithm count, and its retained unsolicited-message callback.
WebXR, StorageManager, Web Bluetooth, WebRTC, MediaDevices/media-stream creation, Web Audio,
Notification construction, and WebGPU access conservatively latch as unsupported when invoked or
first exposed because Servo does not retain a complete pending-callback lifecycle for them.
Canvas 2D contexts in the retained subset contribute an exact Canvas identity, image key, size,
capture status, and shared registry generation after a paint-thread tail barrier. Unsupported 2D
transcripts, missing or mixed generations, bitmap renderer, WebGL/WebGL2, WebGPU, and standalone or
transferred OffscreenCanvas remain unsupported. Finite CSS timings
use Stylo's seconds-based `started_at`/`start_time`, which already include CSS delay; delay remains
part of the fingerprint but is not added twice, and fractional nanosecond deadlines round upward.
Running or pending infinite animations and intervals are open ended; paused animations are inert
at their exact retained state. Non-DOM timer callbacks and detached or connected media elements are
unsupported and prevent issuance. Canvas ownership includes detached live nodes; only the
generation-bound Canvas 2D subset above may become inert.

Animated-image frame opportunities are not claimed as exhaustive typed entries in that source
vector. Servo's current `ImageAnimationManager` schedules the next frame through ScriptThread's
timer scheduler, so its finite deadline prevents issuance; after the callback fires, the document's
rendering-update readiness remains blocked until the frame update advances the script rendering
epoch. This is scheduler/readiness coverage, not typed-vector exhaustiveness. Any future animated
image path that bypasses both mechanisms must add an owned source or producer fence.

MediaSession action handlers use an exact, non-creating inspection path and are clearable when
script sets the handler to null. Mere MediaSession presence is inert. MediaSession default actions
remain covered by the live media-element owner tracker.

## Explicit nonclaims

- This precondition is not a screenshot, PDF, semantic-tree, or PDF/UA result.
- The preparation value by itself is not evidence that Paint presented the bound script/layout
  epoch. Only the complete retained-ticket, Script consume, Paint-finalize transaction described
  above binds the resulting pixels and serialized layout to that generation.
- The transaction does not make a distributed Script/Paint operation one mutex-critical section.
  It uses opaque single-use values plus full revalidation before each irreversible step, and fails
  closed without publication whenever an interleaving invalidates either retained value.
- It supports only one fully-active painted pipeline. Multi-pipeline frame-tree capture remains
  unsupported.
- It does not add settlement ownership for media clocks, graphics APIs outside the retained Canvas
  2D subset, workers, worklets, IndexedDB, or cross-event-loop frames. This inventory rejects their
  tracked presence instead; media/Canvas ownership tracking includes detached live nodes and
  adoption.
- It does not settle WebXR, StorageManager, Web Bluetooth, WebRTC, media streams, Web Audio,
  Notification image-decoding tails, or WebGPU adapter/device operations. The conservative sticky
  invocation/access latches described above instead prevent preparation from issuing after those
  reachable surfaces are used. Supporting such pages requires exact lifecycle hooks or
  owned producer guards that can replace the latches.
- It does not claim source coverage for future Servo features such as scroll/view timelines. Such
  a feature must add a typed unsupported or owned source entry before generation capture can claim
  it.
- Default Servo embedding behavior outside Pliego is unchanged. Pliego's default `render` route
  and its explicit `render-controlled` alias exercise this transaction without a realtime fallback.
