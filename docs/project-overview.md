# Pliego project overview

Pliego is an open-source native HTML-to-PDF engine built on Servo for trusted,
application-owned documents such as invoices, statements, and operational reports.
It is intended for teams that want a native packaged runtime instead of shipping
Chromium, Node.js, or Java with an application.

## Current status

| Item | Current public boundary |
| --- | --- |
| Source release line | v0.3.x API 2 base-engine line; v0.3.3 is the current recommended build |
| Publication status | GitHub Releases is authoritative for the latest exact tag and native assets |
| Engine protocol | API 2 profile-null tuple; API 1 compatibility commands retained temporarily |
| Supported input trust | Application-owned HTML and assets |
| Runtime resources | API 2 denies live network and host-font discovery; applications materialize authorized bytes into the input closure |
| Native bundles | Linux x86_64, Windows x86_64, macOS x86_64, macOS arm64 |
| Primary integration | PHP and Laravel packages |
| Composer packages | Packagist is authoritative for the latest stable `oxhq/pliego-php` and `oxhq/pliego-laravel` packages |
| Hostile HTML | Unsupported; Pliego is not a security sandbox |
| API 2 | Exact manifest v1/request v1/result v1/scene v2/bundle v1 tuple; no semantic profile advertised |

The [support profile](pliego/support-profile.md) is authoritative for current CSS,
paint, resource, platform, and operational limits.

## The problem Pliego addresses

Business documents need pagination, selectable text, embedded fonts, stable assets,
and failures an application can handle. Some also need link annotations; those are
outside the advertised v0.3.3 API 2 profile and currently fail closed.
General-purpose browser automation can provide broad web compatibility, but it also
brings a browser process tree and an execution model that is larger than many
document workflows need. Pliego is developing a narrower, explicit document path on
Servo.

That narrower scope is deliberate. Unsupported paint features fail before a requested
PDF is published by default. API 2 never fetches live network resources or discovers
host fonts; the application provides every authorized resource byte in the input
closure. The application receives typed status and retained artifacts instead of a
success-shaped response when the supported path cannot finish.

## How it fits together

1. An application or SDK provides HTML, document settings, and explicitly authorized
   assets.
2. The native Pliego process loads and lays out the document through Servo.
3. Pliego records the supported document scene and rejects unsupported paint by
   default.
4. The PDF backend writes searchable text, images, paths, and embedded font data
   from an accepted scene. Link annotations are not advertised by v0.3.3 API 2.
5. The caller receives either a completed PDF and artifact metadata or a typed
   failure.

The retained API 1 route has different path and resource semantics and exists only
as a deprecated migration boundary. Its exact failure/publication behavior remains
documented in the [support profile](pliego/support-profile.md).

The preferred CLI and SDK contract is engine API 2. Its public fixed-point
`DocumentScene` starts at version 2 and is delivered only through the exact negotiated
tuple. Internal API 1 `DocumentScene` version 1 remains an implementation and
compatibility artifact. [ADR 0018](pliego/adr/0018-api-2-contract-and-public-artifacts.md)
records the public boundary.

## What the repository currently proves

- Native releases target four platforms with checksum and notice artifacts;
  the latest exact tag and assets on GitHub Releases remain authoritative.
- The PHP and Laravel package lines are published on Packagist and pass their
  focused hosted package checks at release time.
- Laravel can stream a validated API 2 PDF into an application filesystem and
  retrieve it independently of Pliego's prunable retained-job lifetime.
- Focused fixtures cover the advertised API 2 pagination, font, image, and
  fail-closed behavior. The broader controlled-capture regression corpus also
  exercises links and Chart.js, but those cases have not passed the narrower
  v0.3.3 API 2 scene-encoding gate and are not current product claims.
- The Laravel package exposes installation, environment diagnosis, rendering, typed
  failures, and retained artifacts for the supported path.
- A fresh public-only Windows Laravel 13 consumer has exercised install, doctor,
  API 2 render, durable storage, stream retrieval, and typed failure against the
  exact published v0.3.3 PHP, Laravel, and native revisions. Its focused test passed
  53 assertions; it remains one release-consumer path rather than adoption evidence.

These are implementation and release-mechanism claims. They are not evidence of
market adoption, arbitrary-page compatibility, hostile-input isolation, or
performance leadership. The repository now contains a pinned three-target comparator
and one shared correctness slice, but still has no publishable comparative
performance results; see the [benchmark methodology](benchmarks/README.md).

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
- Evaluate one real application-owned document family and share non-confidential
  findings in [GitHub Discussions](https://github.com/oxhq/pliego/discussions).
