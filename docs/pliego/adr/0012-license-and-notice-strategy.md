# ADR 0012: License and notice strategy

- Status: Accepted
- Date: 2026-07-17

## Context

The normative source is *Pliego*, Draft 0.1 (2026-07-14), §§32, 46.6, 52, and §54 open decision 12.
The fork inherits Servo's MPL-2.0 default, historical files with other attached licenses, the
BSD-3-Clause `LICENSE_WHATWG_SPECS`, nested third-party notices, and a Cargo dependency notice report.
The license boundary must be explicit before Pliego-owned source is added.

## Decision

### License by category

| Material | License rule |
| --- | --- |
| Existing Servo, WPT, asset, and third-party files | Keep each file's existing license and notices. |
| New Pliego-owned files outside `sdk/**` | MPL-2.0. |
| Independently authored client SDK files under `sdk/**` | MIT. |
| Vendored or generated third-party material | Keep the source material's licenses and notices. |

This table sets defaults; it does not relicense inherited exceptions. An SDK file that copies or
modifies MPL-covered code remains MPL-2.0 and is not eligible for the MIT default.

### Headers and metadata

- Preserve existing license, copyright, patent, warranty, and attribution notices without cosmetic
  rewrites.
- New MPL source files use Servo's existing MPL Exhibit A header. Package manifests declare
  `MPL-2.0`.
- New MIT SDK source files use `SPDX-License-Identifier: MIT`; each SDK package declares `MIT` and
  ships a package-local copy of the MIT license.
- Imported code records its provenance and keeps every notice required by its source license.

Do not mass-rewrite inherited headers. The first SDK package will add the MIT text and the smallest
path-scoped tidy rule needed for `sdk/**`; no SDK source exists yet.

### Contributions

Inbound terms equal outbound terms for each changed file. A change spanning multiple categories uses
the applicable license for each file. Commits authored for and submitted directly to Pliego certify
the right to submit under the Developer Certificate of Origin 1.1 and add a `Signed-off-by` line.
Imported upstream history is exempt and retains its provenance. Pliego requires no contributor
license agreement or copyright assignment; rights remain with the applicable holder.

### Distribution and dependency notices

An engine distribution includes the root [`LICENSE`](../../../LICENSE), applicable inherited and
third-party notices, the retained Cargo dependency report, and a reasonable link to the exact
MPL-covered source revision. `LICENSE_WHATWG_SPECS` remains with distributions containing its covered
material. SDK packages ship their own MIT text.

The existing `cargo about generate` flow uses [`about.toml`](../../../about.toml) and
[`etc/about.hbs`](../../../etc/about.hbs), with its tracked output at
[`resources/resource_protocol/license.html`](../../../resources/resource_protocol/license.html).
`cargo deny check licenses` remains the dependency-license gate. Cargo tooling does not replace
notices for WPT, assets, fonts, copied specifications, or other non-Cargo material.

A binary release is not license-complete until copied native libraries, including platform GStreamer
bundles, have an artifact-specific license and source-notice inventory.

## Consequences

- Pliego introduces no custom, source-available, or dual-license scheme.
- The MIT boundary stays package-local and cannot silently relicense MPL-covered engine code.
- Pliego cannot unilaterally relicense third-party contributions later; that requires the relevant
  rights holders' consent.
- Pricing, support, hosting, warranties, and commercial contracts remain separate from source
  licensing.

## References

- *Pliego*, Draft 0.1 (2026-07-14), §§32, 46.6, 52, and §54 open decision 12.
- [Mozilla Public License 2.0](https://www.mozilla.org/en-US/MPL/2.0/)
- [Mozilla MPL 2.0 FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/)
- [The MIT License](https://opensource.org/license/mit)
- [GitHub Terms §D.6: contributions under repository license][github-terms]
- [Developer Certificate of Origin 1.1](https://developercertificate.org/)

[github-terms]: https://docs.github.com/en/site-policy/github-terms/github-terms-of-service
