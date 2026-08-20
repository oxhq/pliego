# Pliego project overview

Pliego is an open-source native HTML-to-PDF engine built on Servo for trusted,
application-owned documents such as invoices, statements, and operational reports.
It is intended for teams that want a native packaged runtime instead of shipping
Chromium, Node.js, or Java with an application.

## Current status

| Item | Current public boundary |
| --- | --- |
| Source release line | v0.2.0 controlled-runtime cutover |
| Publication status | The exact tag and assets on GitHub Releases are authoritative; a source branch is not a release |
| Engine protocol | API 1 |
| Supported input trust | Application-owned HTML and assets |
| Runtime network | Denied by default; explicit URL roots are opt-in |
| Native bundles | Linux x86_64, Windows x86_64, macOS x86_64, macOS arm64 |
| Primary integration | PHP and Laravel packages |
| Hostile HTML | Unsupported; Pliego is not a security sandbox |
| API 2 | Unreleased probe/decoder foundation; advertises no render contract |

The [support profile](pliego/support-profile.md) is authoritative for current CSS,
paint, resource, platform, and operational limits.

## The problem Pliego addresses

Business documents need pagination, selectable text, links, embedded fonts, stable
assets, and failures an application can handle. General-purpose browser automation
can provide broad web compatibility, but it also brings a browser process tree and
an execution model that is larger than many document workflows need. Pliego is
developing a narrower, explicit document path on Servo.

That narrower scope is deliberate. Unsupported paint features fail before a requested
PDF is published by default. Remote resources and host-font fallback are disabled
unless the caller opts into them. The application receives typed status and retained
artifacts instead of a success-shaped response when the supported path cannot finish.

## How it fits together

1. An application or SDK provides HTML, document settings, and explicitly authorized
   assets.
2. The native Pliego process loads and lays out the document through Servo.
3. Pliego records the supported document scene and rejects unsupported paint by
   default.
4. The PDF backend writes searchable text, links, images, paths, and embedded font
   data from that scene.
5. The caller receives either a completed PDF and artifact metadata or a typed
   failure.

For the v0.2 API 1 compatibility boundary, failure path fields identify the caller's
requested locations but do not guarantee that those locations exist. Deterministic
publication preflight failures create no public artifact tree, leave an existing
output unchanged, and retain no private runtime container. A public failure tree
exists only when staged engine evidence passes the supervisor contract and can be
promoted atomically.

The stable CLI and SDK contract is engine API 1. The internal `DocumentScene` format
is versioned for repository use but is not currently a stable public interchange
format. [ADR 0014](pliego/adr/0014-document-scene-v1-and-canonical-ordering.md)
records that boundary.

## What the repository currently proves

- The v0.2 source surface and native package targets are versioned in the source
  tree; GitHub Releases remains authoritative for publication status.
- The package workflow defines native builds, checksum and notice artifacts, and
  engine-API smoke checks for the four published targets.
- Focused fixtures cover supported pagination, fonts, images, links, Chart.js usage,
  and unsupported-paint failure behavior.
- The Laravel package exposes installation, environment diagnosis, rendering, typed
  failures, and retained artifacts for the supported path.

These are implementation and release-mechanism claims. They are not evidence of
market adoption, arbitrary-page compatibility, hostile-input isolation, or
performance leadership. The repository currently contains no publishable comparative
benchmark results; see the [benchmark methodology](benchmarks/README.md).

## Governance and upstream relationship

Pliego retains Servo's source layout so reviewed upstream synchronization remains
possible. The accepted
[hard-fork governance ADR](pliego/adr/0013-hard-fork-governance.md) defines the
intended mirror-and-sync process: generic fixes should be contributed upstream when
they fit Servo, while document-specific behavior remains downstream. The repository
does not currently publish a fixed synchronization cadence or service level.

The engine is MPL-2.0. The PHP and Laravel SDKs are MIT-licensed. Contributions use
the repository's [contribution guide](../CONTRIBUTING.md), and vulnerabilities should
be reported through the private process in [SECURITY.md](../SECURITY.md).

## Plans and ways to evaluate the project

- Review the evidence-gated [roadmap](../ROADMAP.md).
- Reproduce the existing engine benchmark harness using the
  [benchmark guide](benchmarks/README.md).
- Evaluate deployment risks against the [threat model](security/threat-model.md).
- Review the public assumptions in the [2026 funding plan](funding/2026.md).
- Try the published Laravel or native CLI path named by the root
  [README](../README.md) and Releases page, then
  report the exact fixture, platform, command, and retained artifacts for any failure.
