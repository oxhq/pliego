# Dependency advisory status

Reviewed on 2026-09-05 for the unreleased 0.4.0 native candidate
`aaf41109035608ad1b84356d434bfade0bddd582`. This is a bounded dependency assessment,
not an advisory-free or independently security-audited release claim. A successful
CI advisory check accepts the exceptions in `deny.toml`; it does not eliminate them.

The supported product remains application-owned PDF rendering under the
[threat model](threat-model.md). It is not a hostile-input sandbox or a private-key
cryptography service. Templates, scripts, fonts and images must be controlled by
the operator, and inserted data must be validated and escaped.

## RSA: known unresolved timing side channel

The default native graph includes `rsa 0.10.0-rc.18`. Servo's WebCrypto integration
uses it for private-key operations, including RSA-OAEP decryption. This is not an
unused or build-only dependency. [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)
reports possible private-key recovery from observable timing and currently lists
no patched version. The prerelease version is not evidence that the advisory is fixed.

Do not supply application signing or decryption keys to the renderer, and do not
use document JavaScript to perform private-key cryptography. Perform those
operations outside Pliego with a separately assessed cryptographic implementation.
In particular, do not expose RSA private operations whose timings an attacker can
observe. Offline resources, process isolation and timeouts do not fix a timing side
channel. This is an operator restriction: the WebCrypto APIs remain present, and
the engine does not enforce this prohibition.

The exception is retained for the restricted PDF-rendering profile, not accepted
for general WebCrypto use. Reassess it on the next crypto/Servo dependency update
and before qualifying any workflow that requires private-key operations. A
required such workflow needs a fix or a separately reviewed implementation; it
cannot be admitted by copying the current exception.

## wnaf: exact-source applicability exception

The `wnaf 0.14.0` yank concerns scalar representation endianness. The reviewed
Pliego graph uses the big-endian P256/P384/P521 types, not the little-endian types
affected by that defect. The [exact-source review](wnaf-0.14.0-review.md) records
the source/checksum evidence and the CI guard. It is a yank-only exception, not a
general cryptographic safety finding. Reassess/remove it at the next crypto update
and before 0.5, whichever comes first; source or lock drift fails the guard.

## quick-xml: workspace build-tool findings

The workspace lock includes `quick-xml 0.39.4`, affected by
[quadratic attribute checking](https://rustsec.org/advisories/RUSTSEC-2026-0194.html)
and [unbounded namespace allocation](https://rustsec.org/advisories/RUSTSEC-2026-0195.html).
Both advisories list fixes in 0.41.0 or later. The sole locked parent is the
`wayland-scanner 0.31.10` protocol code-generation proc macro.

The four candidate package jobs' default
`cargo tree -p pliego --edges normal --prefix none --locked` outputs do not contain
quick-xml, wayland-scanner or winit. Their absence is specific to those default
packages, not all workspace members, features or build dependencies. The actual
SVG document path parses through usvg/roxmltree; SVG support alone does not
establish a quick-xml runtime path.

The scanner's checked attributes can encounter the CPU issue with malicious
compile-time protocol XML; its plain Reader does not use the affected namespace
resolver. Keep build inputs controlled and reassess these findings when changing
packaging/features or upgrading the Wayland toolchain. Do not extend this
assessment to opt-in shell-oracle builds.

## Maintenance notices

Nine ignored RustSec IDs concern unmaintained crates. They are selected in the
default graphs, but the notices do not themselves establish nine exploitable bugs.

| Packages | Current role | Follow-up |
| --- | --- | --- |
| `paste 1.0.15` | Procedural macro used during compilation | Replace through the owning dependencies. |
| Five `unic-* 0.9.0` packages | Runtime Unicode identifier support through urlpattern | Follow upstream Unicode/parser migration. |
| `bincode 1.3.3` | WebRender dependency | Review replacement with the pinned Servo/WebRender update. |
| `ttf-parser 0.25.1`, `rustybuzz 0.20.1` | Runtime font parsing and SVG text shaping | Prioritize maintained upstream replacements; do not treat supplied fonts as harmless bytes. |

Pliego maintainers own these dependency reviews during the next Servo/security
update. Each retained exception must keep an applicability reason and reassessment
trigger; a new advisory or changed input/feature boundary requires a fresh review.
Maintenance debt is not removed merely by leaving an advisory ID in the ignore list.

## Evidence and limits

The exact native package run is [33972319821](https://github.com/oxhq/pliego/actions/runs/33972319821).
Default graph/build evidence is retained in its
[Linux x64](https://github.com/oxhq/pliego/actions/runs/33972319821/job/101323047154),
[Windows x64](https://github.com/oxhq/pliego/actions/runs/33972319821/job/101323047257),
[macOS ARM64](https://github.com/oxhq/pliego/actions/runs/33972319821/job/101323047133)
and [macOS x64](https://github.com/oxhq/pliego/actions/runs/33972319821/job/101323047121)
jobs. The lock SHA-256 is
`cd3e85600546db2db1017f30eecd1c9e933f0898745450ecf3b986211be3a5d7`.

This review does not cover every reachable native call, prove constant-time
cryptography, replace live advisory checks, or audit an application's Composer,
Node.js or browser dependencies. A comparison application's advisory count is not
Pliego's advisory count. Use the private [reporting process](../../SECURITY.md)
for a suspected vulnerability.
