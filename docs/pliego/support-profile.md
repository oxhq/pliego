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
| Trusted Blade or HTML | Supported | Application-owned documents with explicit locale, timezone, page geometry, and resources. |
| Authored page breaks and keep-together | Supported | Unsupported fragmentation reports a failure or diagnostic instead of silently dropping content. |
| Paged tables and repeated headers | Supported | Rows remain unique across pages; see [paged-table compatibility](paged-table-compatibility.md). |
| Rowspans contained in one page fragment | Supported | The complete touched-row run must fit together. |
| Cross-page rowspan continuation | Rejected | Pliego does not split or synthesize spanning cells across pages. |
| Table borders | Partial | Solid horizontal LTR table borders are covered; complex border styles and writing modes are outside the current contract. |
| Local TTF, OTF, WOFF, and WOFF2 | Supported | Declared fonts are embedded or subset with Unicode mappings; missing fonts do not silently fall back to the host. |
| Fonts without embedding rights | Rejected | The application must supply an authorized face. |
| Allowlisted CSS, images, and fonts | Supported | Every permitted root is explicit and fetched bytes contribute to the resolved input identity. |
| Google Fonts stylesheet links | Supported | Both stylesheet and font roots must be allowlisted. |
| Denied URLs | Supported failure | The render records `RESOURCE_DENIED` and publishes no final PDF. |
| Redirects | Rejected | Redirects produce a typed resource failure. |
| Variable-font axes | Partial | Use an authorized static instance when an axis combination is not covered. |
| JavaScript readiness | Partial | Explicit readiness and font readiness are supported; general browser lifecycle parity is not. |
| Selectable text and links | Supported | Unsafe or oversized link targets are rejected. |
| SVG, Canvas, and complex scripts | Partial | Only behavior covered by focused fixtures is included. |

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
