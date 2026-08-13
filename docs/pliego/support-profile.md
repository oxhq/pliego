# Pliego 0.1 support profile

This page defines the current public behavior of Pliego 0.1. Behavior outside this
profile is not implied by Servo or by successful rendering of a different document.

## Intended documents

Pliego renders trusted, application-owned HTML such as Blade invoices, statements,
and operational reports. Each document runs in its own native process. It is not a
sandbox for hostile or tenant-authored markup and does not promise general browser
compatibility.

Published bundles target Linux x86_64, Windows x86_64, macOS x86_64, and macOS
arm64. The same engine API is checked after unpacking on each target; document-level
regression coverage is deepest on Linux x86_64.

## Controlled-capture release candidate

The repository contains an unreleased `render-controlled` route. It is not part of
the stable v0.1.1/API 1 contract, and the PHP and Laravel packages continue to use
the compatibility `render` route. `render-controlled` creates the document clock
before navigation, settles the sources it owns, and consumes one ScriptThread capture
candidate together with one Paint presentation ticket bound to the same generation.
It has no realtime or shell fallback: stale capture state, a lost or indeterminate
consume outcome, and open-ended or unsupported sources all fail without publishing
the requested PDF or any success-only artifact.

The release-candidate package gate is configured to unpack the Linux x86_64 checked-release
archive and run its binary in two fresh processes against one pinned offline font, script, and
visible retained-subset Canvas 2D fixture. The gate requires byte-identical pixels, normalized
semantic layout, scene,
and PDF; exact timeout, `requestAnimationFrame`, paint-observer, and document-time
evidence; matching Script, layout, and presented Paint epochs; PDF text extraction;
and a separate interval-based open-ended-source rejection. The proof manifest records
SHA-256 hashes of the binary, fixture template, materialized input, font, and retained
comparison artifacts. Other packaged targets currently have engine API, native
publication primitives, help, version, dependency-boundary, and archive smoke coverage,
not this document-level controlled-capture proof.

The implemented candidate boundary is deliberately narrow:

- it captures one fully active painted pipeline; multi-pipeline and cross-event-loop
  frame trees are unsupported;
- the packaged fixture is offline and uses a pinned inline font; it does not establish
  a controlled network-resource or arbitrary-page contract;
- intervals, infinite animations, WebSocket, EventSource, BroadcastChannel,
  MessagePort, storage-event listeners, visible embedder controls, and retained media
  action handlers are open ended and prevent capture;
- Canvas 2D contexts are accepted only when their paint-thread observation, image key,
  retained transcript, and registry generation can be bound atomically to the consumed
  candidate; bitmap renderer, WebGL/WebGL2, WebGPU, OffscreenCanvas, unsealed unsupported
  commands, attempts to seal unsupported state with a partial readback, and persistent clip state
  are rejected. A zero-area Canvas does not establish a retained paint-image binding and therefore
  also fails closed. Media elements and streams,
  dedicated workers, worklets, IndexedDB, pending CookieStore requests, ServiceWorker backend
  work or messaging, WebXR, StorageManager, Bluetooth, WebRTC, Web Audio, and notifications are
  rejected rather than settled;
- invalid CSS timing, worker-owned timers, and unclassified non-DOM timer callbacks
  are rejected; a future Servo event source has no controlled support until it gains
  an explicit typed disposition and whatever owned lifecycle or producer-fence
  evidence its execution model requires;
- host- or cross-process-supplied performance timestamps, cross-event-loop navigation,
  and auxiliary WebViews fail closed; the controlled ledger does not interrupt one
  already-running JavaScript turn, so deployments still need an outer process
  deadline; and
- this candidate is not API 2, a hostile-input sandbox, a cross-platform determinism
  claim, or a replacement for the stable compatibility route.

The exact transaction and source inventory are documented in
[generation capture preconditions](generation-capture-precondition.md) and the
[controlled execution ledger](controlled-execution-ledger.md). Supporting an excluded
source requires a typed inventory entry plus owned lifecycle evidence and, for external
producers, producer-fence coverage; merely rendering one page that uses it does not
expand this profile.

Unless a section explicitly names `render-controlled`, the remaining capability and
operational boundaries on this page describe the stable compatibility `render` route.

## Resource modes

- **Offline:** network denied, assets supplied by the application, and exact bytes
  recorded in the input and evidence manifests. This is the repeatable default.
- **Allowlisted:** the caller opts in to explicit HTTP(S) roots. Successful requests
  record URL, status, content type, byte count, and SHA-256. Repeatability depends on
  the provider returning the same bytes.

Host fonts, network access, redirects, and asset caching are disabled by default.
Google Fonts requires both `https://fonts.googleapis.com/` and
`https://fonts.gstatic.com/s/`; allowing the stylesheet origin does not implicitly
allow the font origin.

## Capabilities

| Capability | Status | Boundary |
| --- | --- | --- |
| Trusted Blade or HTML | Supported | Application-owned documents. SDK defaults cover locale, timezone, and page geometry; callers may override them and must explicitly declare external resources. |
| Authored page breaks and keep-together | Supported | Unsupported fragmentation reports a failure or diagnostic instead of silently dropping content. |
| Paged tables and repeated headers | Supported | Rows remain unique across pages; see [paged-table compatibility](paged-table-compatibility.md). |
| Rowspans contained in one page fragment | Supported | The complete touched-row run must fit together. |
| Cross-page rowspan continuation | Rejected | Pliego does not split or synthesize spanning cells across pages. |
| Solid CSS paint | Supported | Resolved sRGB text colors, solid root and element backgrounds, uniform-color sharp axis-aligned solid borders, and uniform solid collapsed-table borders retain paint order. |
| Complex CSS paint | Explicitly unsupported | CSS gradients and background-image layers, box and text shadows, text decorations, border radii and rounded clips, mixed-color or non-solid borders, border images, negative paint origins, transforms, clips and `clip-path`, opacity, filters, and blend modes are reported instead of approximated. |
| Table borders | Partial | Uniform-color sharp ordinary solid borders and collapsed tables with a uniform solid color and width are verified. Non-uniform collapsed borders, mixed-color or non-solid styles, border images, rounded borders, transforms, clips, and unsupported writing modes remain outside the verified contract. |
| Local TTF, OTF, WOFF, and WOFF2 | Supported | Declared fonts are embedded or subset with Unicode mappings; missing fonts do not silently fall back to the host. |
| Fonts without embedding rights | Rejected | The application must supply an authorized face. |
| Allowlisted CSS, images, and fonts | Supported | Every permitted root is explicit and fetched bytes contribute to the resolved input identity. |
| Google Fonts stylesheet links | Supported | Both stylesheet and font roots must be allowlisted. |
| Denied URLs | Supported failure | The render records `RESOURCE_DENIED` and publishes no final PDF. |
| Redirects | Rejected | Redirects produce a typed resource failure. |
| Variable-font axes | Partial | Use an authorized static instance when an axis combination is not covered. |
| JavaScript readiness | Supported within profile | Static documents are zero-config: Pliego infers readiness after page load and waits for `document.fonts.ready`. Pages that continue asynchronous work after load call `window.pliego.defer()` before that work, then `ready()` or `fail()`. |
| Selectable text and links | Supported | Unsafe or oversized link targets are rejected. |
| Canvas | Partial | Retained Canvas 2D operations and bounded raster patches are covered. A synchronous full-canvas pixel readback can become the authoritative result; other Canvas APIs are not implied. |
| Chart.js 4.5.1 | Supported within fixture boundary | Covered with fixed dimensions, animations and events disabled, final draw completed, then synchronous full-canvas `getImageData(0, 0, width, height)` before `window.pliego.ready()`. Other versions, modes, plugins, and partial readbacks are not implied. |
| SVG and complex scripts | Partial | Only behavior covered by focused fixtures is included. |

Partial scene capture is a failure in the default CLI and SDK paths, and the
requested PDF is not published. `--allow-partial-scene` retains diagnostic output
for engine development; it is not a delivery mode.

## Readiness and final canvas state

A static HTML or Blade document does not call the readiness API. Once the page load
event has fired, Pliego waits for the document font set and proceeds automatically.
This is the default path, including documents with declared local or allowlisted
fonts.

Use `window.pliego.defer()` only when work that affects the PDF continues after
load, such as fetching application data or drawing a chart. Call it before starting
that work, call `window.pliego.ready()` only after the final DOM or canvas change,
and call `window.pliego.fail(error)` when the work cannot produce the document.

For the covered Chart.js path, the page performs a synchronous readback of the
entire canvas after `chart.update('none')`. The retained RGBA pixels replace the
preceding Canvas command stream as the authoritative final canvas. The covered
sequence ends at that readback; later drawing is outside this Chart.js proof.

## Operational limits

| Boundary | Current behavior |
| --- | --- |
| Resource body or cache | 64 MiB maximum; oversize is a typed resource failure. |
| Resource connection | 10 seconds by default; configurable from 1 to 60,000 ms. |
| Whole render | The PHP bridge has a configurable wall-clock timeout, terminates the child on timeout, and publishes no partial PDF. |
| Document length | Regression coverage includes a 100-page statement; this is not an arbitrary-length guarantee. |
| HTML size and memory | No engine-wide hard cap is defined. Deployments must set their own request and process limits. |
| Retained jobs | Successful jobs default to one day and failed jobs to seven days; `pliego:prune` applies configurable retention. |

Invalid requests, denied resources, timeouts, and engine failures are typed. A failed
render must not publish a final PDF or silently substitute a declared resource.

## Verification boundary

The release matrix builds, packages, and checks the unpacked engine API on every
published target. Focused fixtures cover document pagination, text extraction,
fonts, tables, resource policy, PDF structure, and repeatability. Composer checks
exercise the PHP and Laravel package contracts. These are release and regression
checks for the profile above, not a promise that arbitrary web pages will render.

Outside the current profile: persistent daemon or hosted rendering, credentialed or
unrestricted network access, redirect chains, untrusted HTML, browser-wide parity,
installers or automatic updates, and general variable-font guarantees.
