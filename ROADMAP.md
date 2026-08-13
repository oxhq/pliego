# Pliego roadmap

This roadmap describes a September 2026-August 2027 planning horizon. It is a set
of evidence gates, not a promise of dates or features. The sequence and scope may
change with technical findings and available funding.

## Starting point

Pliego v0.1.1 is the current stable release. It exposes engine API 1 and targets
trusted, application-owned HTML for invoices, statements, and operational reports.
Its exact support and deployment boundary is documented in the
[support profile](docs/pliego/support-profile.md).

The repository also contains work toward controlled capture and a proposed API2.
Neither is a released v0.1.1 capability. Comparative benchmark results have not yet
been published from the repository's benchmark protocol.

## Milestones

### 1. Controlled capture release candidate (September-October 2026)

Goal: make the final captured document generation explicit and fail closed when
the engine cannot prove that it captured that generation.

Done means all of the following are public and reproducible:

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

Until those criteria pass on the packaged production path, controlled capture stays
unreleased.

### 2. Reproducible comparative evidence (November 2026-January 2027)

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

### 3. Stable protocol candidate (January-April 2027)

Goal: decide whether the internal document protocol is ready to become a supported
public integration boundary.

Done means:

- the open questions in [ADR 0018](docs/pliego/adr/0018-api-2-contract-and-public-artifacts.md)
  are resolved by an accepted architecture decision;
- an implemented engine protocol can be discovered and negotiated by supported SDKs;
- versioned schemas, canonical examples, invalid-input cases, compatibility rules,
  and cross-platform conformance artifacts are committed;
- the PHP and Laravel SDKs pass migration and compatibility tests against packaged
  binaries; and
- documentation clearly separates the stable public contract from internal scene
  representations.

API2 remains proposed until all of these criteria pass.

### 4. Security and maintenance evidence (April-June 2027)

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
  for the supported trusted-input use case; and
- if funded, an independent assessment is published with sensitive exploit details
  withheld until remediation.

This milestone does not turn Pliego into a sandbox for hostile HTML.

### 5. Pliego 1.0 decision (June-August 2027)

Goal: release 1.0 only if the supported product surface has durable evidence.

Done means:

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
