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
