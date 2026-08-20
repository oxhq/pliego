# ADR 0018: API 2 contract and public artifacts

- Status: Proposed
- Date: 2026-08-09

## Context

Pliego's API 1 surface grew around command-line options, path-bearing summary JSON, PHP value
objects, and a private artifact directory. ADR 0014 deliberately kept `DocumentScene` version 1
internal. It used floating-point geometry, accepted unknown object members, and did not define a
stable public resource closure. Those choices are not a safe Pliego 1.0 compatibility boundary.

The production renderer is also changing implementation route. A stable API must describe one
Pliego-owned document process without promising servoshell, a daemon, a browser pool, or hidden
session reuse. It must bind exact input bytes to exact output bytes, separate deterministic delivery
from diagnostics, and make every failure incapable of looking like partial success.

This ADR defines a proposed render contract whose executable foundation is now under development. In
addition to the schemas, byte fixtures, goldens, and dependency-free self-test in
[`contracts/api2`](../../../contracts/api2), the repository contains an unreleased probe, strict
request decoder, and PHP tuple validator. The probe advertises `contracts: []`: those components do
not make an API 2 render transaction, profile, package version, or release available, and they do not
change the API 1 renderer or publication flow.

These schemas are explicitly **pre-R3.5 scaffolding**, not the contract freeze requested by OXH-326.
R3.5 defines the semantic document model, profile decision, validation result, and deterministic
evidence needed for PDF/UA. OXH-346 blocks the contract freeze. OXH-339, not this ADR, decides whether
the first supported target is PDF/UA-1, PDF/UA-2, or another precisely named profile. This ADR only
reserves strict versioned extension points so that R3.5 does not require a breaking retrofit.

## Decision

### Versions and exact runtime pairing

The next production protocol is Pliego API integer `2`. Each public JSON document has an independent
schema identifier and version `1`:

| Document | `schema` | `version` |
| --- | --- | ---: |
| Input manifest | `pliego.input-manifest` | 1 |
| Render request | `pliego.render-request` | 1 |
| Render result | `pliego.render-result` | 1 |
| Document scene | `pliego.document-scene` | 1 |
| Bundle manifest | `pliego.bundle-manifest` | 1 |
| Runtime contract probe | `pliego.runtime-contract` | 1 |

Every object in every schema rejects unknown members. A decoder rejects an unsupported API integer,
schema identifier, schema version, variant, or exact protocol tuple. It does not guess, downgrade,
ignore a field, or form a cross-product from independently advertised versions.

Before a facade renders, it invokes the executable's contract-probe mode and validates one
`pliego.runtime-contract` object. The probe reports the exact tuple of input-manifest, request,
result, scene, and bundle-manifest schema versions supported by that executable, plus the exact engine
identity that will appear in results. API 2 schema version 1 is usable only when that complete tuple
is advertised. Advertising request version 1 and result version 2 in separate tuples does not imply
that request 1/result 2 is supported.

The `contracts` array may be empty. An empty array truthfully reports an executable foundation whose
probe and decoder exist but which does not yet accept a complete API 2 render transaction. A facade
must treat it as no supported API 2 tuple; it must not infer capability from the engine API field,
schema files, or command availability.

Each exact tuple also advertises an ordered array of supported versioned profile references, sorted
ascending by schema identifier and then version. Complete tuples themselves are unique and
canonically ordered by their compact JSON bytes. The
pre-R3.5 fixture advertises an empty array. A profile is usable only when its exact `{schema, version}`
reference appears inside the selected API tuple; profile names are not inferred from PDF metadata or
formed as a cross-product with protocol versions. One protocol tuple may appear only once, so two
different profile arrays cannot make capability negotiation ambiguous.

The executable-foundation probe invocation is `pliego --contract-probe`: on success it writes exactly
one compact, typed-field-order JSON object followed by one line feed to stdout, leaves stderr empty,
and exits zero. A PHP facade validates the exact framing and tuple rather than inferring support. A
complete render tuple and successful API 2 render transport remain unavailable.

### One-shot invocation and invocation errors

Production API 2 performs exactly one render in one process. The direct Pliego-owned runtime creates
one isolated `DocumentSession`, accepts at most one request, emits at most one terminal result, and
exits. Servoshell is not a production fallback. Daemon, browser-pool, and hidden process/session reuse
semantics are not part of API 2.

The dedicated executable selector is `pliego render-api2` with no render options on the command
line; the normalized request is supplied only on stdin. The probe reports
`job_root_transport: "cwd-v1"`. The caller starts the one-shot process inside a newly created,
exclusive job directory with this fixed layout:

```text
input-manifest.json
input/
delivery/      # absent before invocation; committed only on success
diagnostics/   # absent before invocation; retained only by request policy
```

`input-manifest.json` is the descriptor-bound canonical manifest, and `input/` contains exactly its
listed entries. `delivery/` and `diagnostics/` must not exist when the process starts. The runtime
does not accept an absolute job path in arguments or normalized JSON, does not derive identity from
the host current-working-directory string, and does not follow a symlink or reparse-point escape
from the fixed job layout. During the executable-foundation phase the probe still advertises no
contract tuple, so `render-api2` rejects every request as unavailable before inspecting this layout.
The layout becomes active only when a complete tuple is advertised and accepted.

The runtime probe fixes the render transport rather than leaving SDKs to infer it:

- one normalized request is a single JSON value on stdin;
- stdin is bounded to at most `1,048,576` bytes, inclusive; the probe reports that exact
  `request_max_bytes` value, and the runtime rejects an over-limit frame before allocating or reading
  an unbounded request;
- the path-free host transport is exactly `cwd-v1`; SDKs create one exclusive job root and never
  place host paths in normalized request or result bytes;
- `input-manifest.json` is bounded to `16,777,216` bytes, inclusive, and the probe reports that
  exact `input_manifest_max_bytes` value;
- every accepted request writes exactly one `RenderResult` JSON value to stdout;
- `success` exits `0`, while an accepted request whose result is `failed` exits `1`; and
- an argument, framing, decoding, normalization, manifest-pairing, or unsupported-contract error
  accepts no request, writes no stdout, writes one newline-terminated UTF-8 diagnostic line to stderr,
  and exits `64`.

An invocation error is deliberately not a `RenderResult`: there is no accepted normalized request to
echo and no engine render outcome. Stderr text is diagnostic, not a versioned machine contract. SDKs
map exit `64` plus empty stdout to their own invocation exception; they must not synthesize a failed
result or expose staged delivery. Any other exit/stdout combination is a transport failure.

Filesystem roots used to supply fixture bytes, stage output, retain diagnostics, or publish a final
caller path are invocation concerns. Absolute paths, process identifiers, hostnames, temporary
directories, and wall-clock timestamps never enter normalized request, scene, manifest, engine, or
artifact identity.

### Canonical JSON bytes

The three deterministic JSON artifacts (`input-manifest.json`, `scene.json`, and `bundle.json`) use
their typed schema field order, array order, compact JSON separators, UTF-8 without a BOM, and exactly
one trailing line-feed byte. Duplicate object names, floating-point literals, non-finite values, and
the lexical integer `-0` are rejected before typed decoding. Hashes and byte lengths are computed over
those exact bytes, not reparsed or pretty-printed equivalents.

Public numeric geometry is integer fixed point. One CSS pixel is exactly `60` signed app units. This
removes NaN, infinity, rounding-mode drift, and positive/negative-zero ambiguity from public bytes.
Producer conversions from CSS values occur before serialization under the engine's declared app-unit
rounding rule; a consumer never repeats floating-point layout math to recover scene geometry.

### Canonical input manifest and deterministic resource authority

`RenderRequest.input` contains a portable HTML entrypoint and the exact descriptor of
`input-manifest.json`. The versioned input manifest is the complete byte authority visible to the
renderer. Its entries contain a relative path, canonical lowercase media type, byte length, and
SHA-256 content address. The manifest itself is not one of its entries, avoiding recursive identity.

Entry paths are ascending ASCII byte order and unique. Paths are ASCII POSIX-relative strings no
longer than 240 bytes; each segment is at most 100 bytes and may not be empty, `.`, `..`, end in dot or
space, use a Windows device basename, contain a backslash, or collide with another path after ASCII
case folding. A file path may not also be a directory prefix. These rules make the same manifest
addressable on case-sensitive and case-insensitive supported hosts.

Version 1 accepts at most `16,384` entries and at most `16,777,216` canonical manifest bytes. The
limits match the controlled runtime's existing publication-manifest and bounded-tree envelope. They
are deliberately paired: even 16,384 entries with maximum-length version-1 paths, media types,
hashes, and byte counts serialize to `10,338,393` bytes, so the entry ceiling cannot be reduced
accidentally by the byte ceiling. The older 64 MiB asset-cache allowance is a content/cache budget,
not the API 2 metadata transport limit.

The only document URL root is the literal `pliego-input:///`. Relative document URLs resolve under
that root to manifest paths. The entrypoint must be one of the manifest entries. `file:`, a host path,
an alternate custom-scheme authority, or a second URL root is not accepted.

The deterministic API 2 core always carries:

```json
{"network":"deny","host_fonts":"deny"}
```

It never performs a live HTTP lookup, DNS resolution, system-font enumeration, or host-font fallback.
A CLI or SDK facade may offer fetch and host-font conveniences, but it must fetch or discover bytes
before constructing the normalized request, rewrite references into the canonical input URL root,
and list every resulting byte in the manifest. A timeout or redirect belongs to that preflight and
cannot perturb an already accepted render. If a document requests a byte absent from the manifest,
the render fails as `resource`; it does not fall back to the host.

### Reserved R3.5 profile, semantic-layer, and evidence boundary

Four generic strict extension points are present before their domain schemas are chosen:

- `RenderRequest.profile` is either null or a versioned `pliego.profile.*` reference;
- each runtime API tuple has an exact ordered `profiles` array;
- `DocumentScene.semantic_layer` is either null or a versioned, content-addressed
  `pliego.document-semantics.*` resource reference bound to the requested profile; and
- `RenderResult.conformance` echoes the requested profile, records `not-requested`, `satisfied`,
  `failed`, or `not-evaluated`, and carries either null or a versioned, content-addressed
  `pliego.conformance-evidence.*` resource reference.

All profile, semantic-layer, and evidence references are null, and the advertised profile array is
empty, in current accepted fixtures. Their envelopes reject unknown members, but the future profile,
semantic-layer, and evidence documents have independent schema identifiers and
versions so R3.5 can define them without adding opaque maps to these envelopes. A semantic layer or
evidence document is a deterministic `resources/<sha256>` bundle entry, not a diagnostic attachment.

Null has exact meaning: a null request profile asks for no conformance profile; a null semantic layer
contains no public semantic-document claim; and null evidence cannot prove conformance. A result may
use `satisfied` only when the request profile is non-null, the exact profile is echoed, and non-null
deterministic evidence is bound by the bundle. `not-requested` requires null profile and null evidence.
Therefore today's null/empty fixtures make no PDF/UA or other accessibility-conformance claim.

This boundary does not choose a tag vocabulary, structure-tree model, reading-order rules, artifact
classification, language/alternate-text policy, PDF/UA variant, validator, or evidence fields. Those
decisions and goldens belong to OXH-339 through OXH-346. Contract freeze remains blocked until they
land and independently review this boundary.

Pre-R3.5 paint operations contain no role, label, structure identifier, or generic metadata map. The
single top-level `semantic_layer` reference is the only scene extension point reserved here; R3.5
owns the semantic document and its mapping back to paint operations.

### Normalized page policy and CSS `@page`

The native facade default is named `A4` with 48 CSS-pixel margins. At 60 app units per CSS pixel,
those margins are `2880` app units. Native A4 resolves once to the nearest app-unit page box:
`47622 x 67351`. A request may instead contain explicit positive `width_app_units` and
`height_app_units`; named and explicit forms are mutually exclusive.

The request page is a default, not an instruction to delete document CSS. The only version-1
precedence value is `css-page-over-request-defaults`:

1. a matching CSS `@page` size or margin declaration supplies that effective value;
2. an omitted CSS value inherits the corresponding normalized request default; and
3. an absent matching `@page` rule uses the request size and margins in full.

`DocumentScene.request_page` is an exact copy of the accepted request policy. Every emitted page
records its contiguous one-based number, effective integer size and margins, and `style_source` of
`request-defaults` or `css-page`. A `request-defaults` page must equal the resolved request values.
This binds the request to the scene while preserving standard document-owned `@page` styling.

### Deterministic document time and settlement policy

Schema version 1 includes clock and settlement policy now so later runtime work cannot add hidden
defaults to a closed request. `time` records policy version `1`, an explicit integer Unix epoch in
milliseconds, and initial virtual offset `0`. The native default epoch is
`2000-01-01T00:00:00Z` (`946684800000`). The page never observes host wall time implicitly.

`settlement` records policy version `1`, the fail-closed infinite-source policy, exactly two fenced
empty checkpoints, and every version-1 convergence limit:

| Limit | Native default |
| --- | ---: |
| Virtual span | 86,400,000 ms |
| Ordinary tasks | 100,000 |
| Microtasks | 1,000,000 |
| Rendering opportunities | 10,000 |
| Mutations | 1,000,000 |
| Post-readiness resources | 1,024 |
| Process CPU | 30,000 ms |
| Host wall time | 60,000 ms |

Every override is normalized into request identity. Real CPU and wall limits are host safety bounds;
they do not become document-observable time. Exhausting a limit returns a failed result and no
delivery. The detailed clock driver, causal ordering, producer fences, animation behavior, and proof
remain owned by the deterministic-time and visual-settlement work; this schema does not claim that
they are implemented.

### Unified terminal result and stable errors

Every accepted request terminates with one `RenderResult`. Both `success` and `failed` branches carry
the exact normalized request, the exact engine/runtime identity, and the diagnostic inventory.
Engine identity contains Pliego name and version, API integer, source commit, one-shot mode, canonical
target triple, binary SHA-256, and Servo base commit. A target triple is 3 or 4 lowercase ASCII
components separated by hyphens, with lowercase alphanumeric or underscore-separated component
atoms; paths, spaces, uppercase aliases, and vendor display names are rejected.

The public failure object contains only one small stable kind:

- `resource`;
- `readiness`;
- `settlement`;
- `capture`;
- `artifact`;
- `conformance`; or
- `internal`.

Detailed engine codes, messages, source URLs, causal traces, and backtraces are diagnostic data. They
may be stored in an inventoried diagnostic artifact but do not become stable public error enums.
Invocation errors remain outside `RenderResult` as described above.

Both terminal branches carry `conformance`. It must echo `RenderRequest.profile`. A successful
delivery with a requested profile requires `satisfied` plus non-null deterministic evidence. A failed
render may report `failed` or `not-evaluated`; it cannot claim `satisfied`. Profile or evidence
mismatch fails result validation rather than allowing a caller to attach proof for another contract.

A successful result requires nonempty PDF, scene, and bundle descriptors and has `error: null`. A
failed result requires one error kind and has `delivery: null`. There is no partial-success branch and
no allow-partial option. A diagnostic PDF, scene fragment, preview, or last frame is not delivery.

Publication is fail-closed. The engine validates input closure, settlement, capture, scene, resource
closure, PDF, and canonical bundle manifest before committing any caller-owned delivery. A failure
before or during commit publishes none of the deterministic artifacts and returns no delivery path.

### Bundle descriptor, manifest, and real byte closure

`delivery.bundle` is an ordinary descriptor of `bundle.json`; it does not inline manifest entries.
The independently versioned `pliego.bundle-manifest` is loaded from those exact bytes. The manifest
does not list itself and permits only these entry paths:

- exactly one `document.pdf`;
- exactly one `scene.json`; and
- zero or more `resources/<64-lowercase-hex-sha256>` entries.

Entries are unique and ascending by ASCII path bytes. Every resource entry's content address equals
the digest in its path. The actual delivery directory contains exactly `bundle.json` plus the listed
entries: an extra file, missing file, reordered entry, hash mismatch, byte-length mismatch,
case/prefix collision, diagnostic path, or unreferenced resource fails closure.

The allowed resource set is the exact union of public scene references, the optional semantic-layer
reference, and optional deterministic conformance-evidence reference. A profile report is not allowed
to smuggle arbitrary unreferenced resources into delivery.

PDF and scene descriptors in `RenderResult` exactly equal their corresponding manifest entries. The
bundle descriptor hashes the manifest bytes separately. Diagnostics live below `diagnostics/`, may be
inventoried by the result, and are excluded from input, scene, PDF, bundle-manifest, render, and
success identity.

### Public ordered DocumentScene

Page order is pagination order. Operation order is the retained paint traversal order and participates
in scene identity. Readers do not sort, coalesce, or reconstruct operations from DOM order, geometry,
type, or process-local IDs.

The public operations are `text`, `path`, `image`, and `link`. All geometry is signed 32-bit integer
app units; widths, heights, and font/stroke sizes use the corresponding nonnegative or positive
subsets. RGBA channels are integers from 0 through 255. Glyph identifiers and both UTF-8 byte-range
bounds are bounded by unsigned 32-bit integers. Every range is nonempty and aligned to UTF-8
boundaries, and both its start and end are nondecreasing in glyph order.

Path data is a canonical flattened SVG subset. It begins with uppercase absolute `M` and then uses
only uppercase absolute `M`, `L`, `Q`, `C`, and `Z`. Coordinates are signed base-10 i32 app units.
Tokens have exactly one ASCII space; commas, plus signs, fractions, exponents, leading zeros, `-0`,
relative commands, shorthand commands, and arcs are rejected. Producers flatten other SVG commands
and transforms to this representation before serialization.

Link targets are resolved before capture and stored as canonical absolute `http`, `https`, or
`mailto` URLs. HTTP(S) targets require lowercase scheme and ASCII host, no userinfo, no default port,
no dot segments, canonical uppercase percent escapes, and a nonempty absolute path. `mailto` requires
a nonempty address and lowercase domain. Relative URLs, host-dependent bases, and unsafe schemes are
not public scene data.

Every text font and image is a SHA-256 content address. The bundle manifest contains exactly matching
bytes under `resources/<digest>` for the set referenced by the scene, including an optional semantic
layer. Missing, substituted, or extra resource bytes fail the render before delivery. Runtime handles,
source paths, line/column provenance, debug join IDs, host timings, and timestamps remain diagnostics.

## Contract evidence and proof boundary

The checked-in contract fixtures contain real bytes for the input tree, input manifest, PDF, scene,
resources, bundle manifest, and diagnostics. The self-test recomputes their byte lengths and SHA-256
values, verifies exact file closure, and checks that canonical JSON bytes match their parsed values.
The small font payload exists to exercise manifest/resource byte identity; it is not a font-decoder or
renderer fixture.

Accepted goldens cover both page forms, both result branches, the exact runtime tuple, ordered scene,
and both manifests. Rejected goldens cover unsupported API/schema pairing, unknown members, live
network, negative zero, u32 overflow, noncanonical path/link/target/root values, portable path
collisions, missing/extra/reordered manifest entries, and partial delivery. In-memory adversarial
checks additionally prove descriptor/manifest separation, public/internal error separation, page
binding, geometry and glyph-range bounds, host-font denial, typed JSON member order, unique protocol
tuple negotiation, operation-order identity, the absence of premature operation semantics, generic
profile advertisement, semantic-layer/profile binding, result/profile echoing, and the ban on
evidence-free conformance.

The executable-foundation tests separately exercise strict runtime request decoding, canonical probe
framing, exact PHP tuple negotiation, invocation-error framing, and packaged binary/source/target
identity. They deliberately require an empty advertised contract array and terminal rejection for
every API 2 render request. They do not prove facade prefetch/rewrite, a successful API 2 render,
deterministic renderer output, atomic API 2 publication, cross-platform byte identity, consumer
installation, or release availability. API 2 rendering and Pliego 1.0 remain blocked on those
independent implementation and hosted proof gates. The schema is also not frozen: OXH-346 and its
R3.5 prerequisites must complete before OXH-326 can freeze the contract.

## Consequences

- API 2 has one strict, path-independent request/result boundary and an exact binary contract probe.
- The core renderer cannot silently depend on live network or a host font database.
- Page geometry and public scene bytes no longer contain floating-point ambiguity.
- CSS `@page` remains authoritative where declared, while the request supplies explicit defaults.
- Input and delivery identities are reproducible from actual bytes and portable paths.
- Stable public error kinds can survive internal error-code refactors.
- R3.5 can add a versioned semantic layer and deterministic profile evidence without an opaque map or
  an unversioned field retrofit.
- Failures cannot carry a PDF or scene that a caller could accidentally publish.
- Runtime, SDK, version, package, and release work remain separate review units.

## Rejected alternatives

- **Keep API 1 summary JSON and document it.** It mixes paths, engine facts, diagnostics, and delivery
  state without recursively strict schemas or byte manifests.
- **Let the core fetch allow-listed URLs.** Host timing, redirects, DNS, and remote mutation would
  enter the deterministic render after request acceptance.
- **Allow host-font fallback.** Installed fonts and discovery order differ across machines; a facade
  can materialize the chosen bytes instead.
- **Use floating-point CSS pixels in public JSON.** Equivalent geometry can serialize differently,
  and negative zero survives schema bounds unexpectedly.
- **Inline bundle entries in the result descriptor.** It conflates the identity of `bundle.json`
  with the manifest it describes and leaves actual manifest bytes unproved.
- **Publish a PDF when scene capture is partial.** A last frame or partial scene is diagnostic
  evidence, not a successful document.
- **Expose every internal error code.** It turns implementation details into a permanent public enum.
- **Treat malformed input as a failed result.** No exact normalized request exists to echo, so such a
  result would violate the accepted-request invariant.
- **Allow unknown fields for forward compatibility.** Without negotiated reader capability this
  silently changes semantics; exact schema tuples are safer.

## References

- [ADR 0014: Document scene v1 and canonical ordering](0014-document-scene-v1-and-canonical-ordering.md)
- [`ports/pliego/src/engine.rs`](../../../ports/pliego/src/engine.rs)
- [`ports/pliego/src/scene.rs`](../../../ports/pliego/src/scene.rs)
- [`ports/pliego/src/document_session.rs`](../../../ports/pliego/src/document_session.rs)
- [`sdk/php/src/CliRenderer.php`](../../../sdk/php/src/CliRenderer.php)
- [OXH-310: deterministic document time and visual settlement](https://linear.app/oxhq/issue/OXH-310)
- [OXH-326: freeze versioned public contracts](https://linear.app/oxhq/issue/OXH-326)
- [OXH-339: choose the PDF/UA target and profile contract](https://linear.app/oxhq/issue/OXH-339)
- [OXH-346: freeze the R3.5 semantic and evidence contract](https://linear.app/oxhq/issue/OXH-346)
