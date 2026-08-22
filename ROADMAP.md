# Pliego roadmap

This roadmap is a sequence of evidence gates, not a promise of dates or features.
The sequence and scope may change with technical findings, real-consumer evidence,
and available funding.

## Starting point

This roadmap started from Pliego v0.1.1. The current public v0.2.x line retains
engine API 1 and targets trusted, application-owned HTML for invoices, statements,
and operational reports. GitHub Releases is authoritative for the latest exact
version and publication status. The support and deployment boundary is documented
in the [support profile](docs/pliego/support-profile.md).

The v0.2 source line adds controlled capture. The repository also contains an
unreleased API 2 executable foundation whose probe, strict request decoder, and SDK
tuple validator advertise no render contract and cannot render through API 2.
Comparative benchmark results have not yet been published from the repository's
benchmark protocol.

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
| **0.2.0 — Pliego-owned controlled document runtime** | Make the default `render` command and supported SDK path use the Pliego-owned `DocumentSession` controlled transaction. The normal Pliego package graph has no `servoshell` dependency, fallback, or runtime entry. | Existing v0.1 supported inputs are either truthfully settled by the controlled runtime or covered by an explicit migration boundary; controlled time, generation-bound capture, and fail-closed publication pass from packaged binaries; API 1 compatibility tests remain green. The source cutover is not release proof: the exact packaged and hosted gates must still pass, and they do not establish cross-platform byte determinism. |
| **0.2.x — adoption and compatibility** | Put the released API 1 runtime into representative PHP/Laravel applications while API 2 proceeds. Patch releases carry backwards-compatible fixes discovered through installation, provisioning, fonts, paths, containers, CI, queues, concurrency, permissions, and binary updates. | Representative applications, Linux container/CI, and at least one desktop package exercise install, doctor, render, typed failure, artifacts, queue/concurrency, and update flows. This evidence channel informs compatible 0.2.x fixes but does not block 0.3.0. |
| **0.3.0 — public API 2 base engine** | Graduate the empty-contract executable foundation into one complete advertised profile-null tuple and make exact-tuple negotiation the preferred PHP/Laravel path. API 1 enters a documented deprecation period with a migration bridge. | `--contract-probe`, canonical request/result transport, typed errors, artifact manifest, render identity, compatibility rules, accepted and rejected goldens, package smoke tests, cross-platform conformance, deprecation behavior, and API 1 migration behavior pass against packaged binaries. The base tuple reserves typed semantic/profile extension slots without freezing or claiming accessible-PDF semantics. |
| **0.4.0 — semantic document layer and one accessible profile** | Make a canonical semantic `DocumentScene` the source of one precisely named tagged/accessible-PDF profile and its diagnostics. This is not a blanket claim of “PDF/UA support.” | OXH-339 selects the exact profile and validator versions; OXH-346 freezes its semantic/evidence contract; structure, language, alternate text, reading order, provenance, pagination, authoring diagnostics, validator output, failure policy, fixtures, and retained evidence are independently reproducible from the advertised API 2 tuple. |
| **0.5.0 — hardened operational support boundary** | Make Pliego deployable and maintainable without project-author involvement: documented limits, supply-chain evidence, support diagnostics, repeated Servo synchronization, and correctness-first comparative reports. | The reviewed threat model, deployment/resource limits, signed and verifiable release assets, SBOM/notices/provenance, support bundle, security policy, repeated upstream-sync reports, packaged consumer, conformance matrix, and internal/dompdf/Browsershot reports are public and reproducible against exact tagged binaries. |
| **1.0.0 — stable supported contract** | Freeze only the runtime, protocol, semantic profile, SDK, compatibility, security-reporting, and maintenance commitments that real consumers and repeated releases have shown the project can preserve. | Every 1.0 gate below passes on immutable artifacts; API 2 and the semantic profile have representative external use; installation and updates are routine; multiple Servo syncs have completed; and no known required contract change remains. An unmet gate keeps Pliego on 0.x. |

Package reduction, code hygiene, dependency security, benchmark infrastructure, and
Servo synchronization are continuous evidence tracks. They do not reserve product
version numbers. A compatible result may ship in a patch release with real fixes; a
public behavior change follows the minor-release rule above.

The current installed base is small enough that waiting for a broad API 1 adoption
sample would preserve the weaker boundary without buying meaningful compatibility
evidence. Therefore 0.3.0 is the immediate planned product release after 0.2.1.
The representative-application target remains useful parallel evidence, but it is not
a prerequisite for API 2. The 0.3 packages make API 2 preferred, deprecate API 1 with
an explicit migration path, and retain the old route only as a temporary transition
and rollback boundary. A later pre-1.0 minor may remove API 1 after packaged migration
proof; removal is not conditioned on reaching an arbitrary consumer count.

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

### 1. Pliego 0.2.0 controlled-runtime release (August 2026)

Goal: make the final captured document generation explicit and fail closed when
the engine cannot prove that it captured that generation.

Status: released. The native, PHP, and Laravel v0.2.0 packages are public, and
the release and package evidence is recorded in the
[v0.2.0 release notes](docs/releases/v0.2.0.md).

The release gate required all of the following to be public and reproducible:

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

### 2. Pliego 0.2.x adoption and compatibility channel

Goal: learn from representative applications without delaying the API 2 contract.

The v0.2.1 patch corrects validated failure-evidence promotion and strict
partial-capture failure retention by restoring the documented API 1 failure
boundary; it does not change accepted inputs or the support profile. Its package
and consumer evidence is recorded in the
[v0.2.1 release notes](docs/releases/v0.2.1.md).

The parallel evidence target is:

- at least three real PHP/Laravel applications cover distinct deployment shapes,
  including at least one independently maintained or design-partner consumer;
- the evidence set includes Linux container/CI and at least one supported desktop
  package rather than counting three copies of the same environment;
- install, provisioning, binary update, `doctor`, render, typed failure, artifacts,
  font discovery, unusual paths, permissions, queue, and concurrent-render behavior
  are exercised without fixture-only assumptions;
- backwards-compatible findings ship as 0.2.x fixes with packaged and consumer proof;
  and
- API 2 implementation proceeds in parallel; adoption findings can still produce
  compatible 0.2.x patches, but they do not hold the 0.3.0 release train.

### 3. Pliego 0.3.0 public API 2 base engine

Goal: make Pliego a versioned, discoverable document engine without combining that
contract freeze with the semantic/accessibility architecture.

Done means:

- the profile-null base proposal in
  [ADR 0018](docs/pliego/adr/0018-api-2-contract-and-public-artifacts.md) is implemented;
- `--contract-probe` advertises exactly one complete supported tuple rather than an
  empty contract list or inferred capability;
- canonical request/result schemas, typed errors, artifact manifests, render identity,
  capability discovery, negotiation, compatibility rules, and version semantics are
  committed with accepted and rejected goldens;
- the tuple reserves reviewed typed extension points for future profile and semantic
  schemas without freezing an internal scene representation;
- PHP and Laravel prefer the exact tuple, emit a documented API 1 deprecation signal,
  and pass API 1 migration/compatibility tests against packaged binaries on every
  supported target;
- publication remains atomic and fail closed, with retained conformance evidence; and
- the temporary API 1 route and its removal policy are explicit rather than inferred
  from consumer-count thresholds.

The final all-profile 1.0 contract tracked by
[OXH-326](https://linear.app/oxhq/issue/OXH-326) is not reused as the 0.3 base-tuple
gate. Issue tracking must keep those two freezes distinct.

### 4. Continuous engineering and comparative evidence

Goal: reduce maintenance cost and publish useful comparisons without inventing product
releases for internal work or comparing unlike execution models.

Done means:

- removed features, dependencies, adapters, and Pliego-owned dead paths are proven
  unreachable from the supported runtime and SDK surface;
- package graphs, archives, native dependencies, build timings, and binary sizes record
  before/after deltas instead of treating fewer source files as proof;
- Clippy, Tidy, checked-release `-D warnings`, dependency/security audits, and supported
  packages stay green;
- upstream-sync rehearsals are repeated before deep semantic work and before each
  support-boundary release, preserving upstream topology unless a reviewed merge
  justifies a change;
- tagged Pliego, operating-system image, dependencies, fonts, fixtures, commands, and
  resource limits are pinned for every benchmark report;
- dompdf and Browsershot adapters run only eligible equivalent fixtures, include all
  descendant processes, and pass correctness/output gates before timing; and
- raw samples, explicit N/A exclusions, hashes, environment metadata, and the report
  generator are published together.

The current methodology and honest evidence boundary are in the
[benchmark guide](docs/benchmarks/README.md). Compatible cleanup may accompany a patch
release; any public contract or behavior change requires the next minor release.

### 5. Pliego 0.4.0 semantic document layer and accessible profile

Goal: make Pliego understand and retain document semantics, then generate one precisely
named accessible-PDF profile from that canonical source.

Done means:

- [OXH-339](https://linear.app/oxhq/issue/OXH-339) and an accepted ADR name the exact
  conformance target and pinned validator versions;
- [OXH-346](https://linear.app/oxhq/issue/OXH-346) freezes the independently versioned
  semantic and evidence contract;
- a canonical semantic `DocumentScene` carries required structure, language,
  alternate text, reading order, provenance, and pagination facts without opaque
  escape maps;
- visual PDF, tagged PDF, diagnostics, and retained semantic evidence derive from the
  same canonical scene for the supported profile;
- unsupported authoring or indeterminate semantics fail with typed diagnostics rather
  than a false conformance claim;
- the PDF, semantic input, validator output, profile identity, tool versions, and
  assistive-technology observations are retained for the declared corpus; and
- the supported API 2 tuple advertises this exact profile without changing the frozen
  profile-null base semantics.

Reusable scenes, alternate rendering backends, and additional archival/accessibility
profiles remain later work until demand and a concrete contract justify them.

### 6. Pliego 0.5.0 operational support boundary

Goal: make the trust, deployment, supply-chain, support, and maintenance boundary
independently operable without project-author involvement.

Done means:

- the [threat model](docs/security/threat-model.md) is reviewed against the released
  architecture and linked from release documentation;
- deployment guidance demonstrates process, filesystem, network, concurrency, CPU,
  memory, and container limits for the supported trusted-input use case;
- signed/checksummed binaries, SBOM/native notices, provenance, and install/update
  verification are reproducible in the package matrix;
- `doctor`, meaningful exit codes, opt-in structured timings, crash evidence, and a
  support bundle cover the documented failure boundary;
- multiple public upstream-sync reports record ranges, conflicts, security changes,
  and retained downstream patches;
- a versioned packaged consumer passes install, update, doctor, render, failure, and
  artifact flows;
- the API 2/profile conformance matrix passes on all supported package targets;
- correctness-gated internal, dompdf, and Browsershot reports are refreshed against
  the exact candidate; and
- if funded, an independent assessment is published with sensitive exploit details
  withheld until remediation.

This milestone does not turn Pliego into a sandbox for hostile HTML.

### 7. Pliego 1.0 decision

Goal: release 1.0 only if the supported product surface has durable evidence.

Done means:

- every preceding 0.2.0-0.5.0 gate remains satisfied on the release candidate,
  including the correctness-gated internal, dompdf, and Browsershot evidence;
- a release candidate passes native package and engine-API smoke checks on Linux
  x86_64, Windows x86_64, macOS x86_64, and macOS arm64;
- representative real consumers exercise API 2 and the semantic profile, including at
  least one independently maintained application rather than only repository fixtures;
- versioned PHP/Laravel consumers exercise the documented install, update, doctor,
  render, failure, and artifact paths using the packaged runtime;
- controlled-capture acceptance and protocol conformance artifacts are reproducible
  for the declared fixture set from published commands;
- compatibility, support, security-reporting, and maintenance policies are published;
- more than one Servo synchronization has completed through the documented process;
  and
- immutable source, native archives, checksums, notices, and SDK versions are promoted
  through the documented release gates.

If any required gate is unmet, the project remains on a 0.x release. Adoption is
evidence that the contract is usable and preservable, not a popularity quota or a date.

## Explicit non-goals for this horizon

- claiming safe execution of hostile or tenant-controlled HTML;
- browser-wide HTML, CSS, Canvas, or JavaScript parity;
- a hosted multi-tenant rendering service;
- GPU output parity for the CPU scene preview; or
- performance leadership without reproducible, correctness-gated measurements.

Funding assumptions and possible work-package resourcing are documented in the
[2026 funding plan](docs/funding/2026.md).
