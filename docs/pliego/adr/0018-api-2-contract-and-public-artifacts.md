# ADR 0018: API 2 contract and public artifacts

- Status: Proposed
- Date: 2026-08-09

## Context

Pliego's API 1 surface grew around command-line options, path-bearing summary JSON, PHP value
objects, and a private artifact directory. ADR 0014 deliberately kept `DocumentScene` version 1
internal: unknown object members were accepted, diagnostic source references could enter the scene,
and the artifact bundle did not yet define a stable public closure. Those choices were appropriate
while the direct document runtime and its resource ownership were still being proved, but they are
not a safe 1.0 compatibility boundary.

The production renderer is also changing implementation route. A stable API must describe a
one-shot Pliego document process without promising the legacy servoshell orchestration, a daemon,
or hidden process reuse. It must separate deterministic deliverables from useful but unstable
diagnostics, and it must make failure incapable of looking like partial success.

This ADR freezes the proposed contract before runtime implementation. The schemas and goldens in
[`contracts/api2`](../../../contracts/api2) are review artifacts only. They do not change the CLI,
PHP SDK, package version, renderer, or publication behavior, and they are not evidence that API 2 is
implemented or released.

## Decision

### Version and process model

The next production protocol is Pliego API `2`. Each JSON document also carries its own schema
identifier and schema version `1`:

| Document | `schema` | `version` |
| --- | --- | ---: |
| Render request | `pliego.render-request` | 1 |
| Render result | `pliego.render-result` | 1 |
| Document scene | `pliego.document-scene` | 1 |

API and schema versions are independent. An implementation must reject an API integer or schema
version it does not implement; it must not guess, downgrade, or ignore the value. Every object in
these three schemas rejects unknown members. Adding a public member, operation, or result branch is
therefore a compatibility decision requiring a new schema version.

Production API 2 performs exactly one render in one process. The direct Pliego-owned document
runtime creates one isolated document session, emits one terminal result after accepting a request,
and exits. Servoshell is not a production fallback, and persistent daemon, browser-pool, or hidden
session reuse semantics are not part of this API.

Filesystem locations used to transport the input bundle, receive stdout, stage diagnostics, or
publish the caller's final file are invocation concerns outside the normalized JSON request. They
must not enter render identity, scene identity, or deterministic artifact hashes.

### Normalized RenderRequest

`RenderRequest` is the normalized behavioral input, not a bag of convenience options. It contains:

- the safe relative HTML entrypoint and a path-independent SHA-256 identity for the complete input
  bundle;
- the controlled locale and timezone;
- one page definition;
- the network, host-font, and resource-timeout policy; and
- the diagnostic retention policy.

CLI and SDK facades may offer omitted convenience options, but they must materialize them before
constructing this envelope. In particular, the native page default is the named `A4` form with
48 CSS-pixel margins. `A4` resolves to `793.7008 x 1122.5197` CSS pixels. A request may instead use
explicit positive width and height in CSS pixels. Named and explicit size forms are mutually
exclusive; API 2 does not infer a page name from explicit dimensions.

Network access and host fonts remain denied by the zero-configuration facade. Allowed HTTP roots
are normalized, sorted, unique roots. The resource timeout is a real host safety bound, not
document-observable time. The diagnostics policy affects retention only and is excluded from render
and artifact identity.

The input bundle identity is SHA-256 over a canonical manifest of safe relative paths, byte lengths,
and per-file SHA-256 values sorted by path. Absolute roots, directory timestamps, permissions, and
archive container metadata do not participate. The entrypoint must be one of those manifest paths.

A request that cannot be decoded, validated, and normalized is rejected as an invocation error with
no `RenderResult` and no delivery. The unified result contract begins only after a valid normalized
request exists; this is why both terminal result branches can require the exact normalized request.

### Unified RenderResult

Every accepted request terminates with one `RenderResult`. Both the `success` and `failed` branches
contain:

- the normalized request;
- path-independent engine and runtime identity;
- the diagnostic inventory; and
- exactly one of deterministic delivery or a typed error.

Engine identity names Pliego, its package version, API integer, source commit, one-shot runtime mode,
target triple, binary SHA-256, and Servo base commit. It never contains an executable path, process
identifier, hostname, temporary directory, or wall-clock timestamp.

A successful result requires nonempty PDF, scene, and bundle descriptors and has a null error. A
failed result requires a typed error and has a null delivery. There is no public partial-success
branch and no `allow_partial_scene` escape hatch. Diagnostics may be retained after failure, but a
diagnostic PDF, preview, scene fragment, or last frame is not a published deliverable and must not be
reported under `delivery`.

Publication is fail-closed. The engine must finish and validate the scene, close the resource bundle,
serialize and validate the PDF, and construct the deterministic manifest before any caller-owned
delivery is committed. A failure before that commit publishes none of the three deliverables. A
publication failure also returns `failed`, removes or leaves unreachable any staged delivery, and
does not return paths that a consumer could mistake for success.

### Deterministic bundle and diagnostics

The successful `delivery` contains relative artifact descriptors for:

- `document.pdf`;
- `scene.json`; and
- `bundle.json`, whose entries bind the PDF, scene, and every referenced binary resource.

Each descriptor records a media type, byte length, and `sha256:<lowercase-hex>` content address.
Bundle entry paths are safe, unique, and lexicographically ordered. Binary resource bytes live at
exactly `resources/<lowercase-sha256>`, and an entry's content address must equal its path digest.
The bundle does not list itself, avoiding recursive identity.

Diagnostic descriptors use relative paths below `diagnostics/`. Their inventory is public so a
caller can retain or discard it deliberately, but diagnostic contents are not compatibility or
determinism promises. Diagnostics are excluded from `bundle.json`, render identity, scene identity,
PDF identity, and success correctness. Host timings, logs, source provenance, layout snapshots,
readiness traces, previews, and failure evidence belong on this side of the boundary.

### Public DocumentScene

API 2 promotes the ordered `pliego.document-scene` version 1 shape to a public artifact. Page order
is pagination order. Operation order is the single retained paint traversal order. Readers must not
sort, coalesce, or reconstruct operations from DOM order, geometry, type, or process-local IDs.

The public operations are `text`, `path`, `image`, and `link`. Text retains positioned glyph IDs and
UTF-8 source ranges; paths retain explicit geometry and paint; images name content-addressed bytes;
links retain their target; and operation metadata may contain stable semantics. Runtime handles,
absolute source paths, line/column provenance, debug join IDs, and timestamps are diagnostics and
are not public scene members.

Every font and image reference is a `sha256:<digest>` address. A valid successful bundle must contain
exactly matching bytes at `resources/<digest>` for every address used by the scene. Missing bytes,
digest/path disagreement, an unreferenced substituted resource, an unknown operation, an unknown
member, invalid geometry, or invalid glyph text mapping fails the render before delivery.

The normalized scene hash is SHA-256 over the exact compact UTF-8 serialization produced from the
validated typed scene. Sequence order and declared struct field order are significant. Pretty
printing, diagnostics, filesystem paths, and resource bytes outside the scene are not directly
hashed; the resource addresses inside the scene bind those external bytes through the bundle
closure.

### Compatibility and proof

Schema version 1 is intentionally closed. Consumers may rely on unknown members and variants being
rejected. A producer must emit exactly one supported schema shape; it must not conditionally add
undocumented fields for debug builds or platforms. API negotiation happens before rendering, never
after a consumer has accepted artifacts.

The accepted goldens demonstrate the named A4 request, explicit page dimensions, both terminal
result branches, deterministic bundle closure, diagnostic exclusion, and the ordered scene. Rejected
goldens demonstrate strict unknown-member handling, unsupported API rejection, partial-delivery
rejection, and missing resource closure. The dependency-free self-test validates only these contract
artifacts. Runtime decode/encode tests, CLI and PHP integration, cross-platform deterministic bytes,
atomic publication, release packaging, and consumer installation remain separate acceptance gates.

## Consequences

- API 2 has one strict, auditable request/result boundary instead of several path-shaped summaries.
- A4 is an explicit native default while custom physical layouts remain possible through dimensions.
- A consumer can distinguish deterministic deliverables from retained evidence without interpreting
  filenames or runtime internals.
- DocumentScene becomes a public ordered artifact whose resource references are closed by the bundle.
- Failures cannot carry a PDF or scene that a caller could accidentally publish.
- Strict unknown-field rejection makes future additions deliberate schema-version changes.
- The runtime, SDKs, version declarations, and release process still need implementation and their
  own proof before API 2 or Pliego 1.0 can be claimed.

## Rejected alternatives

- **Keep API 1 summary JSON and document it.** It mixes transport paths, engine facts, diagnostics,
  and delivery state, and it has no recursively strict schema.
- **Expose servoshell or a daemon protocol.** That would make a temporary orchestration route and
  lifecycle model part of the 1.0 promise.
- **Make explicit dimensions the only page form.** It would preserve SDK/native default drift and
  hide the intended A4 default behind magic numbers.
- **Publish a PDF when scene capture is partial.** A last frame or partial scene is diagnostic
  evidence, not a successful document.
- **Put diagnostics in the deterministic bundle.** Host timings and debug evidence would perturb
  artifact identity without changing the document.
- **Allow unknown fields for forward compatibility.** With no negotiated reader capability, this
  silently changes semantics; an explicit schema version is safer.

## References

- [ADR 0014: Document scene v1 and canonical ordering](0014-document-scene-v1-and-canonical-ordering.md)
- [`ports/pliego/src/engine.rs`](../../../ports/pliego/src/engine.rs)
- [`ports/pliego/src/scene.rs`](../../../ports/pliego/src/scene.rs)
- [`ports/pliego/src/session.rs`](../../../ports/pliego/src/session.rs)
- [`sdk/php/src/CliRenderer.php`](../../../sdk/php/src/CliRenderer.php)
- [OXH-326: Freeze versioned RenderRequest, DocumentScene, error, and artifact contracts](https://linear.app/oxhq/issue/OXH-326)
