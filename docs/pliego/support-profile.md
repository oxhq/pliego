# Pliego 0.1 alpha support profile

Status: release contract for the sellable OSS MVP. A row marked **Supported** is
part of the alpha promise only after its named proof passes on the released
artifact. Until then, the proof column is the truthful current boundary.

## Buyer and deployment

Pliego 0.1 targets a trusted Laravel application rendering one application-owned
Latin-script invoice, statement, or operational-report family. Production uses one
native Pliego process per queued job at concurrency 1. The required bundles are
Linux x86_64, Windows x86_64, macOS x86_64, and macOS arm64; deep correctness is
qualified on Ubuntu 22.04 x86_64 and every other target runs an unpacked invoice
smoke.

## Resource modes

- **Offline/locked:** network denied, assets supplied by the application, exact
  bytes in the input/evidence manifests. This is the deterministic mode.
- **Live/allowlisted:** the caller opts in to explicit HTTP(S) roots. Successful
  requests record URL, status, content type, byte count, and SHA-256; those bytes
  contribute to `resolved_input_hash`. This mode is content-addressed, but it is
  repeatable only while the provider returns the same bytes.

Network, host fonts, and redirects remain denied by default. Google Fonts requires
both `https://fonts.googleapis.com/` and `https://fonts.gstatic.com/s/` roots; an
allowed stylesheet does not implicitly allow its font origin.

## Capability matrix

| Capability | Alpha state | Required behavior and proof |
| --- | --- | --- |
| Trusted Blade/HTML | Supported | One application-owned document; no hostile or tenant-authored markup claim. |
| Authored page breaks and basic keep-together | Supported | Invoice gate; unsupported fragmentation must warn or fail, never silently drop content. |
| Paged tables and repeated headers | Supported | Invoice plus first/middle/last statement pages; every required row occurs once. |
| Rowspan contained in one fragment | Supported | Contiguous touched rows stay together. |
| Cross-page rowspan continuation | Rejected | Typed/declared unsupported result; no synthesized or corrupted span. |
| Table borders | Supported | Sampled page geometry and duplicate-border checks. |
| Local TTF/OTF/WOFF/WOFF2 | Supported | Declared relative asset, selected and embedded/subset with `ToUnicode`; no host fallback. |
| Live allowlisted CSS/images/fonts | Supported | Two-origin local HTTP proof, readiness, typed failures, resource hashes, and resolved input identity. |
| Google Fonts stylesheet link | Supported | Same two explicit roots as above; one real-provider release smoke, not a per-PR dependency. |
| Missing or denied resource | Supported failure | Typed `RESOURCE_*` evidence and no published PDF. |
| Redirect | Rejected | Typed redirect/resource failure. Add bounded redirects only when a supported direct URL cannot work. |
| Variable-font axes | Partial | Static instance or authorized static face is the documented fallback. |
| JavaScript readiness | Partial | Explicit readiness and font readiness only; arbitrary SPA/browser lifecycle parity is not promised. |
| Selectable text and links | Supported | PDF structure/extraction gate; unsafe or oversized link targets are rejected. |
| SVG/Canvas and complex scripts | Partial | Only separately evidenced fixtures; not part of the invoice/report promise. |

## Limits and failure ownership

| Boundary | Alpha contract |
| --- | --- |
| Resource body/cache | 64 MiB maximum; oversize is a typed resource failure. |
| Resource connection | 10 seconds by default; configurable from 1 to 60,000 ms. |
| Whole render | Configurable PHP wall-clock timeout; timeout must terminate/reap the child, return `RENDER_TIMEOUT`, and publish no partial PDF. |
| Document length | 100 pages is the qualified statement ceiling, not an arbitrary-length promise. |
| HTML/input and memory | No engine hard cap is proven yet. The paid deployment must pin queue-payload and OS/container memory limits; engine enforcement remains a release gap. |
| Artifacts | Successful and failed jobs retain bounded diagnostics under the configured work root. Automatic TTL/deletion remains a release gap; the deployment owns cleanup until OXH-284 closes it. |

Invalid requests, denied resources, timeouts, and engine failures are typed. Pliego
must not publish a final PDF after a failed render or silently replace a declared
font/resource with a host value.

## Verification budget

Each change runs one affected package checker or fixture. Pull requests do not run
`cargo test --workspace`, the full Servo/WPT matrix, `mach test`, or every historical
Pliego gate by default. The native matrix builds/packages Pliego and checks the
unpacked artifact; the deep invoice/live-resource gate runs once on Linux. Broad
inherited suites are scheduled or pre-release exceptions when an upstream boundary
actually changed.

## OSS and paid boundary

The engine, native bundles, PHP/Laravel packages, local and live URL/font support,
generic fixes, diagnostics, examples, and release automation are public. The paid
offer covers private preflight, one template-family migration, acceptance evidence,
one deployment integration/runbook, and support. Payment never unlocks a generic
renderer capability.

Deferred: daemon/cloud, unrestricted or credentialed network, redirect chains,
untrusted HTML, broad browser parity, extra CPU targets, installers/auto-update,
variable-font guarantees, and M5+ compatibility breadth.
