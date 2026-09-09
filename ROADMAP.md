# Pliego roadmap

This roadmap is a sequence of evidence gates, not a promise of dates or features.
The sequence and scope may change with technical findings, real-consumer evidence,
and available funding.

## Starting point

This roadmap started from Pliego v0.1.1. The v0.2.x line established the controlled
API 1 runtime for trusted, application-owned HTML. Pliego v0.3.0 then released the
exact profile-null API 2 tuple while retaining API 1 as a temporary compatibility
route. **v0.3.3 is the current recommended API 2 build**. Compatible maintenance
continues on 0.3.x while 0.4 targets dependable business documents through API 2. GitHub
Releases is authoritative for the latest exact version and publication status. The
support and deployment boundary is documented in the
[support profile](docs/pliego/support-profile.md).

API 2 uses a strict probe, request decoder, fixed cwd-v1 input closure, one-shot
render, canonical result and bundle, and exact SDK tuple negotiation. It does not
advertise a semantic or accessible-PDF profile.
The published [v0.3.3 minimal-static comparison](docs/benchmarks/results/v0.3.3-minimal-static-github-hosted/all-repeats.md)
retains three GitHub-hosted repeats and 900 timed PDFs that passed the shared
correctness oracle, with version-locked dompdf and Browsershot adapter graphs.
This is evidence for that fixture and environment, not a general production ranking
or proof of real-document coverage.

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
| **0.2.0 — Pliego-owned controlled document runtime (released)** | Make the default `render` command and supported SDK path use the Pliego-owned `DocumentSession` controlled transaction. The normal Pliego package graph has no `servoshell` dependency, fallback, or runtime entry. | Released with packaged controlled-time, generation-bound capture, fail-closed publication, and API 1 compatibility evidence. |
| **0.2.x — compatible runtime and SDK fixes (released)** | Correct API 1 failure-evidence promotion and add the Laravel durable-storage handoff without inventing a new product milestone. | v0.2.1 and v0.2.2 shipped as backwards-compatible patches with focused package and consumer evidence. |
| **0.3.0 — public API 2 base engine (released)** | Graduate the empty-contract executable foundation into one complete advertised profile-null tuple and make exact-tuple negotiation the preferred PHP/Laravel path. API 1 enters a documented deprecation period with a migration bridge. | Released with `--contract-probe`, canonical request/result transport, typed errors, artifact manifest, render identity, compatibility rules, goldens, package smoke tests, cross-platform conformance, deprecation behavior, and API 1 migration behavior. |
| **0.3.x — API 2 maintenance (active; v0.3.3 recommended)** | Preserve the published base through backwards-compatible packaging, platform, documentation, and SDK fixes. | Compatible findings pass packaged and clean-consumer checks; new accepted capabilities follow the minor-release policy. |
| **0.4.0 — dependable API 2 business documents** | Render, store, and retrieve representative invoices, long statements, and operational reports with supported links, paged tables, and bounded failures. | A frozen real-document corpus, reproducible application integration, comparison against the application's original PDF provider, actual API 2 failure tests, and four-platform packaged-consumer evidence pass. Independent adoption is not a release gate. |
| **0.5.0 — semantic document layer and one accessible profile** | Make a canonical semantic `DocumentScene` the source of one precisely named tagged/accessible-PDF profile and its diagnostics. This is not a blanket claim of “PDF/UA support.” | OXH-339 selects the exact profile and validator versions; OXH-346 freezes its semantic/evidence contract; live capture, structure, pagination, tagged output, authoring diagnostics, validator results, and reader/assistive-technology evidence are independently reproducible from the advertised API 2 tuple. Independent adoption is not a release gate. |
| **0.6.0 — hardened operational support boundary** | Make Pliego deployable and maintainable without project-author involvement: documented limits, supply-chain evidence, support diagnostics, repeated Servo synchronization, and correctness-first comparative reports. | The reviewed threat model, deployment/resource limits, signed and verifiable release assets, SBOM/notices/provenance, support bundle, security policy, repeated upstream-sync reports, packaged consumer, conformance matrix, and candidate-specific comparative reports are public and reproducible against exact tagged binaries. Independent adoption proceeds alongside this work and gates 1.0. |
| **1.0.0 — stable supported contract** | Freeze only the runtime, protocol, semantic profile, SDK, compatibility, security-reporting, and maintenance commitments that real consumers and repeated releases have shown the project can preserve. | Every 1.0 gate below passes on immutable artifacts; API 2 and the semantic profile have representative external use; installation and updates are routine; multiple Servo syncs have completed; and no known required contract change remains. An unmet gate keeps Pliego on 0.x. |

Package reduction, code hygiene, dependency security, benchmark infrastructure, and
Servo synchronization are continuous evidence tracks. They do not reserve product
version numbers. A compatible result may ship in a patch release with real fixes; a
public behavior change follows the minor-release rule above.

The installed base was small enough to release API 2 without waiting for broad API 1
adoption. That release is now complete. The 0.4 and 0.5 engineering gates use
representative application code and documents, including integrations operated by
the project. Independent adoption follows the semantic/accessibility release; its
three-application evidence target gates 1.0, not 0.4 or 0.5. API 1 remains only as
an explicit transition and rollback boundary. A later pre-1.0 minor may remove it
after packaged migration proof; removal is not
conditioned on reaching an arbitrary consumer count.

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

### 2. Pliego 0.2.x compatibility patches (released)

Goal: record the backwards-compatible API 1 and Laravel storage corrections shipped
before the API 2 release.

The v0.2.1 patch corrects validated failure-evidence promotion and strict
partial-capture failure retention by restoring the documented API 1 failure
boundary; it does not change accepted inputs or the support profile. Its package
and consumer evidence is recorded in the
[v0.2.1 release notes](docs/releases/v0.2.1.md).

The `oxhq/pliego-laravel` 0.2.2 package is released. A real Laravel consumer
rehearsal proved that `render()` and `download()` were not enough for business
documents that must outlive Pliego's prunable retained-job directory. Version 0.2.2
adds a first-class durable-storage handoff that:

- accepts an application storage path, optional Laravel disk, and write options;
- streams only a successfully validated PDF from `RenderResult::pdfPath`, without
  buffering the whole document in PHP memory;
- returns the durable disk/path together with the underlying render identity and
  retained evidence;
- distinguishes a storage write failure from a render failure and never reports a
  durable object when the write was false, short, or exceptional; and
- passes local/fake-disk, failed-write, large-stream, queue, and consumer tests while
  documenting that Pliego evidence and application storage have separate retention
  policies.

This is a Laravel SDK patch, not a reason to rebuild an unchanged native engine.
v0.2.2 was a Laravel-only patch under the pre-0.3 release policy. From 0.3 onward,
the native runtime and supported PHP/Laravel SDKs use the coordinated version line
described above.

API 2 is now the preferred public contract. Real-application engineering evidence
is part of 0.4; the independent-adoption target is part of the path from 0.5 to 1.0.

### 3. Pliego 0.3 public API 2 release line

Goal: make Pliego a versioned, discoverable document engine without combining that
contract freeze with the semantic/accessibility architecture.

Status: released. [v0.3.0](docs/releases/v0.3.0.md) introduced API 2,
[v0.3.1](docs/releases/v0.3.1.md) corrected the Linux headless stderr boundary, and
[v0.3.2](docs/releases/v0.3.2.md) corrected Windows private-job creation.
[v0.3.3](docs/releases/v0.3.3.md) durably flushes the staged API 2 resource,
delivery, and diagnostics directories before promotion while leaving the advertised
tuple, schemas, and rendering profile unchanged. The
[0.3 launch overview](docs/releases/v0.3.md) records the product-level outcome.

Released evidence includes:

- the profile-null base proposal in
  [ADR 0018](docs/pliego/adr/0018-api-2-contract-and-public-artifacts.md) is implemented;
- `--contract-probe` advertises exactly one complete supported tuple rather than an
  empty contract list or inferred capability;
- canonical request/result schemas, typed errors, artifact manifests, render identity,
  capability discovery, negotiation, compatibility rules, and version semantics are
  committed with accepted and rejected goldens;
- the tuple reserves reviewed typed extension points for future profile and semantic
  schemas without freezing an internal scene representation;
- PHP and Laravel prefer the exact tuple and expose the documented API 1 migration
  boundary in focused package checks; packaged native API 2 conformance passes on
  every supported target;
- publication remains atomic and fail closed, with retained conformance evidence; and
- the temporary API 1 route and its removal policy are explicit rather than inferred
  from consumer-count thresholds.

The final all-profile 1.0 contract tracked by
[OXH-326](https://linear.app/oxhq/issue/OXH-326) is not reused as the 0.3 base-tuple
gate. Issue tracking must keep those two freezes distinct.

### 4. Pliego 0.3.x API 2 maintenance

Goal: preserve the released API 2 base while the next document capability is developed.
Compatible findings ship as 0.3.x patches with packaged and clean-consumer proof.
New accepted inputs or output semantics belong in a minor release.

A fresh public-only Windows Laravel 13 consumer now proves the v0.3.3
install/doctor/API 2 render/store/read-stream/typed-failure path in 53 focused
assertions. The installed PHP and Laravel packages resolve to their published v0.3.3
GitHub dist references, and the native runtime reports the published v0.3.3 source
commit. This remains one release-consumer path, not three-application adoption
evidence; the earlier v0.3.2 run remains useful historical proof.

### 5. Continuous engineering and comparative evidence

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

### 6. Pliego 0.4.0 dependable API 2 business documents

Goal: make representative business documents reliable through the actual
PHP/Laravel render, storage, retrieval, and queue workflow.

Status: the source tree prepares an **unreleased 0.4.0 candidate**. Bounded
table, link, solid-paint and physical-page-footer implementations and their
native oracles are present; source implementation is not completion of the
release gates below. The selected operational family is a one-page Aureus
manufacturing work order with three Code128 barcodes; the separate 300-entry
ledger supplies long-document coverage. Neither replaces the invoice gate.
See [candidate notes](docs/releases/v0.4.0.md) for the remaining evidence boundary.

Done means:

- a frozen corpus covers an invoice, a long statement, and an operational report,
  with pinned source templates, data, fonts/assets, expected document facts, and a
  recorded compatibility blocker inventory;
- the declared API 2 boundary preserves required text, totals, rows, pagination,
  fonts/images, and PDF links, including link geometry and destinations;
- document-driven table, layout, and paint blockers are closed, with remaining
  exclusions and every template adaptation recorded alongside the original input;
- a real PHP/Laravel application's selected PDF workflows run through Pliego while
  retaining their original provider for comparison; all PDF entry points in that
  selected scope are inventoried, and existing application tests are preserved;
- a reproducible comparison runs Pliego and that original provider on pinned
  eligible inputs, retains their PDFs and diagnostics, and verifies required text,
  totals, pagination, links, and reviewed visual output before performance claims;
- comparison results report correct, incorrect, unsupported, baseline/setup,
  timeout/crash, and delivery/storage outcomes with their denominators. Raw timing
  samples, p50/p95 wall time, stated-concurrency throughput, CPU, process-tree peak
  memory, I/O, and PDF size are retained where measurable. End-to-end workflow and
  renderer-only costs, cold/warm modes, and template adaptations stay separate;
- install/update, `doctor`, render, store/readback, typed failure, evidence access,
  fonts, paths, permissions, queues, retention, and concurrency pass through the
  actual API 2 SDK route on Linux container/CI and a supported desktop;
- one documented deployment recipe demonstrates an outer process deadline and
  cancellation, accounts for relevant descendants, states host resource and
  retention settings, and proves that failed invocations do not return successful
  delivery or stored-document records;
- storage readback matches the validated PDF; denied writes and adapter false/throw
  outcomes retain actionable failures. Remote object verification, visibility,
  retry, and partial-object cleanup policies are explicit for the selected adapter;
- candidate measurements also compare against v0.3.3 on shared eligible inputs,
  retaining incompatibilities and failures without calculating speedups from them; and
- all four native package targets, API 2 conformance, PHP/Laravel package consumers,
  upgrade/rollback, and coordinated publication checks pass on the exact candidate.

The existing benchmark harness and minimal-static report are a starting point;
they do not replace this real-document comparison. No performance win is required.
An internally operated fork provides external-code engineering evidence, not
independent adoption. Neither an independent maintainer nor a three-application
quota is required for 0.4. Full resource-exhaustion, signing, and support programs
remain part of 0.6.

### 7. Pliego 0.5.0 semantic document layer and accessible profile

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

The live semantic model, pagination associations, tagged writer, and validation
pipeline are implementation checkpoints toward this release. A schema-only result
does not satisfy the profile gate. Reproducible application and assistive-technology
tests establish this release boundary; independent application adoption follows it.

### 8. Pliego 0.6.0 operational support boundary

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

Independent adoption proceeds after 0.5 alongside this operational work. The full
three-application evidence target below gates 1.0, rather than automatically blocking
0.6 publication. A first bounded Servo-sync rehearsal starts before deep semantic
work; repeated completed synchronizations remain required for operational closure.

### 9. Pliego 1.0 decision

Goal: release 1.0 only if the supported product surface has durable evidence.

Done means:

- every preceding 0.2.0-0.6.0 gate remains satisfied on the release candidate,
  including the correctness-gated internal, dompdf, and Browsershot evidence;
- a release candidate passes native package and engine-API smoke checks on Linux
  x86_64, Windows x86_64, macOS x86_64, and macOS arm64;
- at least three real PHP/Laravel applications in distinct deployment environments
  exercise API 2, with representative semantic-profile use and at least one
  independently maintained or design-partner consumer. Across the set, Linux
  container/CI and a supported desktop cover install/update, `doctor`, render,
  store/readback, typed failures, evidence, fonts, paths, permissions, queues, and
  concurrency; an internally operated fork alone does not establish independent use;
- versioned PHP/Laravel consumers exercise the documented install, update, doctor,
  render, failure, and artifact paths using the packaged runtime;
- controlled-capture acceptance and protocol conformance artifacts are reproducible
  for the declared fixture set from published commands;
- compatibility, support, security-reporting, and maintenance policies are published;
- the final versioned contract has no known mandatory breaking change remaining;
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
