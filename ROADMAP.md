# Pliego roadmap

This roadmap describes a September 2026-August 2027 planning horizon. It is a set
of evidence gates, not a promise of dates or features. The sequence and scope may
change with technical findings and available funding.

## Starting point

Pliego v0.1.1 is the current stable release. It exposes engine API 1 and targets
trusted, application-owned HTML for invoices, statements, and operational reports.
Its exact support and deployment boundary is documented in the
[support profile](docs/pliego/support-profile.md).

The repository also contains work toward controlled capture and an unreleased API 2
executable foundation. Its probe, strict request decoder, and SDK tuple validator
advertise no render contract and cannot render through API 2. None of this is a
released v0.1.1 capability. Comparative benchmark results have not yet been
published from the repository's benchmark protocol.

## Versioning policy and intended release train

Pliego uses one coordinated version line for the native binary and its supported PHP
and Laravel SDKs. Before 1.0, a minor release introduces a new public runtime,
protocol, document-profile, or compatibility boundary. A patch release is reserved
for backwards-compatible fixes, packaging or documentation corrections, and internal
refactors that do not change accepted inputs, required artifacts, output semantics, or
the advertised protocol. If a cleanup changes one of those surfaces, it moves to the
next minor release instead of being disguised as a patch.

Project policy treats published tags and release assets as append-only: a failed build
gets a new version, and published objects are not replaced. A version is an intention
until every named gate passes on the exact source object; the project does not retag a
failed build or promote SDK packages independently of the native runtime they were
tested against. Documentation-only evidence may land without forcing a binary release.

These product and SDK versions are independent of Servo's workspace/package version
and of the integer API and schema versions carried inside protocol documents. A Servo
`0.4.x` dependency, for example, does not imply a Pliego `0.4.x` release or API 4.
Pliego owns the production document runtime and its document invariants; Servo remains
the upstream engine.

| Intended version | Public purpose | Release gate |
| --- | --- | --- |
| **0.2.0 — Pliego-owned controlled document runtime** | Make the default `render` command and supported SDK path use the Pliego-owned `DocumentSession` controlled transaction. The normal Pliego package graph has no `servoshell` dependency, fallback, or runtime entry. | Existing v0.1 supported inputs are either truthfully settled by the controlled runtime or covered by an explicit migration boundary; controlled time, generation-bound capture, and fail-closed publication pass from packaged binaries; API 1 compatibility tests remain green. The current explicit `render-controlled` candidate and its Linux packaged proof are prerequisites, not the completed cutover or a cross-platform byte-determinism claim. |
| **0.3.0 — API 2 and a precisely named accessible-PDF profile** | Graduate the empty-contract executable foundation into one complete advertised and frozen tuple, migrate the PHP/Laravel render path to exact-tuple negotiation, and expose the first supported accessible-PDF profile through that versioned contract. This is not a blanket claim of “PDF/UA support.” | `--contract-probe`, canonical request/result transport, accepted and rejected goldens, package smoke tests, cross-platform conformance, and API 1 migration behavior pass against packaged binaries; OXH-339 selects PDF/UA-1, PDF/UA-2, or another exact target; OXH-346 freezes the corresponding semantic and evidence contract; OXH-326 freezes the complete API 2 contract; semantic structure, language, alternate text, validator versions, failure policy, fixtures, and retained evidence are independently reproducible. |
| **0.3.1 — Pliego package-surface reduction** | Remove features, dependencies, adapters, and Pliego-owned paths proven unreachable from the supported product. This does not mean deleting or renaming upstream-tracked Servo modules merely to make the fork look smaller. | Public inputs, artifacts, protocol tuples, and contract-visible rendering behavior remain compatible; expected version/source-identity changes are recorded; package graphs, archives, and native inventories measurably shrink; the Servo directory topology and an upstream-sync rehearsal remain intact. Any other public behavior change promotes this work to a minor release instead. |
| **0.3.2 — code hygiene and bloat reduction** | Remove unused imports, dead Pliego paths, redundant dependencies, and unjustified warning suppressions while retaining the existing deny-warnings gates. Necessary interop, generated-code, and safety allowances remain documented rather than being deleted mechanically. | Clippy, Tidy, checked-release `-D warnings`, dependency audits, and supported package targets stay green; dependency, archive, and binary-size deltas are recorded; contract-visible outputs and conformance outcomes remain stable except for explicitly versioned engine/source identity fields. Behavior changes are excluded from this patch line. |
| **0.4.0 — hardened operational support boundary** | Promote the 0.3 contract into documented security, deployment, upstream-maintenance, consumer-support, and comparative-evidence commitments for a pre-1.0 release candidate. | The reviewed threat model, upstream-sync report, dependency/native notice audit, deployment limits, packaged Laravel consumer, conformance matrix, and correctness-gated internal/dompdf/Browsershot reports are public and reproducible against exact tagged binaries. |
| **1.0.0 — stable supported contract** | Freeze the documented runtime, protocol, SDK, compatibility, security-reporting, and maintenance commitments. | Every 1.0 gate below passes on immutable source, package, checksum, notice, SDK, and consumer artifacts. An unmet gate keeps Pliego on 0.x. |

The explicit 0.2 API 1 migration boundary is that deterministic publication
preflight no longer creates a public artifact tree. Failure results still carry the
requested path strings and stable typed error, but `OUTPUT_ARTIFACTS_OVERLAP`,
`OUTPUT_ALREADY_EXISTS`, and `PUBLICATION_TRANSACTION_FAILED` leave caller-owned
paths unchanged and retain no private runtime container. SDK consumers must check
artifact-path existence before reading diagnostics. Post-start failure evidence is
public only after its staged contract validates and promotion succeeds.

New-publication paths that depend on unresolved symbolic-link traversal or prospective
Windows DOS 8.3 aliases are unsupported and rejected fail-closed before success;
callers must use direct, non-aliased paths with an already-resolvable external output
parent.

New artifact roots must leave enough destination-filesystem pathname headroom for
the owner-only private staging tree and its longest bounded resource descendant.
The v0.2 supervisor proves that boundary before starting the document process and
returns `ARTIFACTS_CREATE_FAILED` without a public artifact root or PDF when the
probe fails. The filesystem's path rules, rather than a portable character count,
define the limit.

Reproducible internal, dompdf, and Browsershot benchmark reports are an evidence track,
not a reason by themselves to increment the product version. They must name the exact
tagged Pliego binary they measure. If benchmark work changes runtime behavior or the
public contract, that change follows the minor/patch rules above.

## Milestones

### 1. Pliego 0.2.0 controlled-runtime candidate (September-October 2026)

Goal: make the final captured document generation explicit and fail closed when
the engine cannot prove that it captured that generation.

Done means all of the following are public and reproducible:

- the default `render` command and supported PHP/Laravel path use the controlled
  transaction without a realtime or shell fallback, and API 1 migration and
  compatibility tests remain green;
- the controlled clock is installed before navigation and every admitted source of
  document-visible time uses it;
- readiness, font settlement, animation, callback, paint, and canvas state are
  checked at the capture boundary;
- a capture ticket is bound to one document generation, and stale or indeterminate
  state returns a typed failure rather than a PDF;
- a packaged Linux x86_64 production binary renders the font-and-script acceptance
  fixture twice with byte-identical required artifacts in a hosted check;
- the checked package contains no non-production shell oracle or fallback path; and
- the support profile names the verified boundary and every known exclusion.

Until those criteria pass on the packaged production path, Pliego 0.2.0 stays
unreleased.

### 2. Pliego 0.3.0 API 2 and accessible-PDF candidate (November 2026-March 2027)

Goal: decide whether the internal document protocol is ready to become a supported
public integration boundary and use it for one precisely named accessible-PDF profile.

Done means:

- the proposal in
  [ADR 0018](docs/pliego/adr/0018-api-2-contract-and-public-artifacts.md) is implemented;
- [OXH-339](https://linear.app/oxhq/issue/OXH-339) and a corresponding accepted ADR
  name the exact accessible-PDF conformance target and validator versions;
- an accepted public ADR defines and freezes the semantic and evidence contract
  tracked by [OXH-346](https://linear.app/oxhq/issue/OXH-346);
- [OXH-326](https://linear.app/oxhq/issue/OXH-326) freezes the complete API 2
  contract after the OXH-346 dependency is satisfied;
- an implemented engine protocol can be discovered and negotiated by supported SDKs;
- versioned schemas, canonical examples, invalid-input cases, compatibility rules,
  and cross-platform conformance artifacts are committed;
- the PHP and Laravel SDKs pass migration and compatibility tests against packaged
  binaries;
- documentation clearly separates the stable public contract from internal scene
  representations;
- the semantic layer carries every required structural, language, alternate-text,
  reading-order, and provenance fact without opaque escape maps;
- unsupported or indeterminate semantics fail with typed evidence rather than a false
  conformance claim; and
- the PDF, semantic input, validator output, profile identity, and tool versions are
  retained together for the declared fixture corpus.

The API 2 executable foundation remains unavailable for rendering, and the complete
API 2 contract and accessible-PDF profile remain proposed until all of these criteria
pass.

### 3. Pliego 0.3.1-0.3.2 internal cleanup (March-April 2027)

Goal: reduce the production package and downstream maintenance surface without harming
the public contract or Servo upstream synchronization.

Done means:

- the removed features, dependencies, adapters, and Pliego-owned dead paths are proven
  unreachable from the supported runtime and SDK surface;
- package graphs, archives, native dependencies, and build timings record before/after
  deltas rather than assuming that fewer source files means a smaller product;
- upstream-tracked Servo directory topology and modules remain available for reviewed
  synchronization unless a separately audited upstream merge justifies their removal;
- existing deny-warnings checks stay enabled while unused imports, redundant
  dependencies, and unjustified lint suppressions are removed; and
- contract-visible outputs and conformance outcomes remain stable, except for
  explicitly versioned engine/source identity fields whose expected deltas are
  recorded.

### 4. Reproducible comparative evidence (after an eligible tagged 0.2.0-or-later runtime)

Goal: publish useful performance evidence without weakening correctness or comparing
unlike execution models.

Done means:

- the tagged Pliego release, operating-system image, dependencies, fonts, fixture
  bytes, commands, and resource limits are pinned;
- implemented dompdf and Browsershot adapters run the same eligible fixtures and
  page settings, with all descendant processes included in resource accounting;
- correctness and output-parity gates run before timing, and failures are not counted
  as fast samples;
- cold one-shot results are kept separate from any explicitly documented warm or
  persistent mode; and
- raw samples, exclusions, hashes, environment metadata, and the report generator are
  published together.

The current methodology and honest evidence boundary are in the
[benchmark guide](docs/benchmarks/README.md).

### 5. Pliego 0.4.0 operational support boundary (May-July 2027)

Goal: make the supported trust boundary and maintenance practice independently
reviewable.

Done means:

- the [threat model](docs/security/threat-model.md) is reviewed against the released
  architecture and linked from release documentation;
- a public upstream-sync report records the Servo range reviewed, conflicts resolved,
  security-relevant changes, and retained downstream patches;
- release dependencies and native notices are regenerated and audited in the package
  matrix;
- deployment guidance demonstrates process, filesystem, network, and resource limits
  for the supported trusted-input use case;
- a versioned Laravel consumer passes install, doctor, render, failure, and artifact
  flows against the packaged runtime;
- the API 2/profile conformance matrix passes on all supported package targets;
- correctness-gated internal, dompdf, and Browsershot reports are refreshed against
  the exact 0.4.0 candidate; and
- if funded, an independent assessment is published with sensitive exploit details
  withheld until remediation.

This milestone does not turn Pliego into a sandbox for hostile HTML.

The 0.3.1 and 0.3.2 cleanup releases may occur only after the 0.3.0 public surface is
frozen. They must preserve that surface and the explicit Servo-upstream synchronization
model. The broader 0.4.0 candidate includes the reviewed maintenance, benchmark, and
deployment evidence above.

### 6. Pliego 1.0 decision (August 2027 or later)

Goal: release 1.0 only if the supported product surface has durable evidence.

Done means:

- every preceding 0.2.0-0.4.0 gate remains satisfied on the release candidate,
  including the correctness-gated internal, dompdf, and Browsershot evidence;
- a release candidate passes native package and engine-API smoke checks on Linux
  x86_64, Windows x86_64, macOS x86_64, and macOS arm64;
- at least one versioned Laravel consumer fixture exercises the documented install,
  doctor, render, failure, and artifact paths using the packaged runtime;
- controlled-capture acceptance and protocol conformance artifacts are reproducible
  for the declared fixture set from published commands;
- compatibility, support, security-reporting, and maintenance policies are published;
  and
- immutable source, native archives, checksums, notices, and SDK versions are promoted
  through the documented release gates.

If any required gate is unmet, the project remains on a 0.x release rather than
using the 1.0 label as a schedule target.

## Explicit non-goals for this horizon

- claiming safe execution of hostile or tenant-controlled HTML;
- browser-wide HTML, CSS, Canvas, or JavaScript parity;
- a hosted multi-tenant rendering service;
- GPU output parity for the CPU scene preview; or
- performance leadership without reproducible, correctness-gated measurements.

Funding assumptions and possible work-package resourcing are documented in the
[2026 funding plan](docs/funding/2026.md).
