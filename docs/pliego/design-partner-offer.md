# Pliego production-document design partnership

Status: fixed-scope M4 validation offer. Pliego and its PHP/Laravel bridge are
experimental; this is not a general-availability, compatibility, or hosted-service
claim.

This partnership is for a Laravel team with one production document that is painful
to maintain or deploy with DOMPDF, Browsershot, wkhtmltopdf, or a remote rendering
service. The purpose is to prove a real migration and production-assurance workflow,
not to sell broad HTML-to-PDF compatibility.

## Fixed offer

| Term | Commitment |
| --- | --- |
| Price | **$7,500 fixed** |
| Deposit | **50% ($3,750)** to schedule the engagement, credited toward the total |
| Balance | **$3,750** when the agreed technical acceptance package is delivered and passes the frozen baseline |
| Document scope | **Exactly one document family** |
| Deployment scope | **Exactly one production deployment** |
| Proof | One frozen regression baseline and its retained evidence |
| Support | **90 calendar days of priority support** after acceptance |

A document family is one business purpose, data contract, and template lineage, such
as an invoice and the explicitly listed invoice variants. A statement, certificate,
contract, or unrelated template is another family. The kickoff record freezes the
included variants.

One deployment is one Laravel application, operating-system and architecture target,
and Pliego configuration. A staging replica used only to prove that same production
deployment is not a second deployment. Another application, platform, region-specific
runtime, or independently operated installation is out of scope.

The deposit is requested only after preflight is complete and the parties freeze the
scope, acceptance baseline, privacy handling, and written disposition of every known
unsupported-feature or deployment risk. The signed agreement controls invoicing, tax,
and any remedy if the frozen acceptance baseline cannot be met; failure does not
silently become acceptance.

## Qualification and intake

The engagement should not start unless all of these are true:

- [ ] A Laravel team owns a real production document and can authorize access to it.
- [ ] One decision-maker or acceptance owner is named.
- [ ] Exactly one document family and one deployment are selected.
- [ ] The team can provide a representative template, assets, permitted fonts,
  redacted sample data, and current output.
- [ ] The current failure or operational cost is concrete and reproducible.
- [ ] Preflight compares the template with the current
  [paged-table compatibility boundary](./paged-table-compatibility.md), the
  [experimental Laravel bridge boundary](./laravel-cli-bridge.md), and the
  [pinned Laravel invoice proof application](../../tests/pliego/laravel-invoice/README.md).
- [ ] Every known unsupported feature or unproved deployment assumption has a written
  disposition in the preflight record before any deposit is requested.
- [ ] The customer can approve a $3,750 deposit after preflight.

Use this checklist to prepare the preflight:

### Business and ownership

- [ ] Document purpose, audience, and production owner.
- [ ] Current renderer and the reason migration matters now.
- [ ] Acceptance owner and authorized technical contacts.
- [ ] Required launch date and any immovable operational constraint.
- [ ] Current render volume and concurrency for sizing only; price is not per page.

### Application and deployment

- [ ] Laravel, PHP, package, operating-system, and architecture versions.
- [ ] Current render entrypoint, queue/job path, and download or storage behavior.
- [ ] The single production deployment and its staging-equivalent path.
- [ ] Installation restrictions, container/base image, and process permissions.
- [ ] Expected timeout, memory, concurrency, and artifact-retention policy.

### Template and resources

- [ ] Blade/HTML entrypoint and every included variant.
- [ ] CSS, images, SVG, and font files with redistribution/use authorization.
- [ ] Redacted representative data, including page-boundary and failure cases.
- [ ] Current expected PDF and screenshots of known defects.
- [ ] Page size, margins, locale, timezone, and writing direction.
- [ ] Network resources and the explicit allow-or-bundle decision for each one.
- [ ] JavaScript readiness behavior and any asynchronous data or chart construction.
- [ ] Required text, totals, table continuation, repeated headers/footers, links,
  and other document invariants.

### Privacy and evidence

- [ ] Data classification and fields that must be removed before transfer.
- [ ] Authorized transfer channel, named recipients, retention period, and deletion
  confirmation.
- [ ] Whether local-only inspection is required.
- [ ] Permission, if any, to create and publish a synthetic minimal reproducer.
- [ ] The redacted evidence reference format used in the validation ledger.

## Technical acceptance baseline

The parties fill and sign one acceptance record before implementation. It identifies:

- the exact document family and included variants;
- the application and one deployment target;
- pinned PHP, Laravel, Pliego, operating-system, font, and asset versions;
- explicit page geometry, locale, timezone, readiness, filesystem, font, and network
  policies;
- the representative success, page-boundary, and expected-failure inputs;
- the required text, totals, page count or page rules, table behavior, links, and
  other observable document invariants; and
- the private location of customer inputs plus the redacted evidence identifiers.

Acceptance requires all of the following:

1. The agreed Laravel path returns, downloads, or stores a readable PDF in the one
   deployment without an external Node, Chromium, or Java renderer.
2. Every frozen fixture renders with no missing or duplicated required text, rows,
   totals, or declared resources.
3. Selectable text, embedded fonts, page geometry, and required table continuation
   match the signed baseline. Unsupported behavior is not accepted as a silent
   fallback.
4. Repeated runs with pinned inputs match the agreed canonical-scene hash or the
   explicitly documented normalized structural baseline.
5. A denied resource, invalid request, or other agreed failure produces the expected
   typed error and retained diagnostic evidence.
6. The evidence package retains the rooted input manifest, resolved environment,
   scene, PDF-structure report, PDF, and the baseline comparison without exposing
   private customer content publicly.
7. The production owner signs the acceptance record. Subjective changes requested
   after the frozen baseline are not acceptance defects.

The baseline is a regression contract for this engagement, not a claim that Pliego
supports every document or browser feature.

## Privacy and customer-template boundary

Customer templates, source data, credentials, business rules, and produced documents
remain private customer material. They are not committed to the Pliego repository,
copied into the public validation ledger, used as a public benchmark, or retained
beyond the agreed handling period without written permission.

Private artifacts may contain rendered text, URLs, fonts, or source-node information.
They use the same access and deletion policy as the source template. Public issues and
commits use a synthetic minimal reproducer or a description that cannot reconstruct
customer content. The public ledger uses redacted identifiers and aggregate counts,
never names, email addresses, repository URLs, or template contents.

## Open-source covenant

Payment buys migration work and production assurance, not a private rendering fork.
Generic renderer, layout, compatibility, security, and diagnostics fixes land in the
public Pliego repository with synthetic regression coverage under each changed file's
applicable existing license. Inherited Servo, WPT, asset, and third-party files retain
their existing licenses; new Pliego-owned engine files outside `sdk/**` use MPL-2.0;
and independently authored SDK files under `sdk/**` use MIT. A file that copies or
modifies MPL-covered code remains MPL-2.0.
[ADR 0012](./adr/0012-license-and-notice-strategy.md) defines the file-level boundary.
No generic fix or supported capability is withheld behind a commercial gate.

Customer-specific template adaptation, deployment configuration, and private runbooks
may remain private. They do not change the license or availability of the engine or
SDK.

## Included 90-day priority support

Priority support begins on the signed acceptance date and applies only to the frozen
document family, deployment, and regression baseline. It includes:

- priority triage ahead of ordinary community reports;
- reproduction and baseline reruns for reported regressions;
- guidance for the accepted deployment and retained evidence; and
- public generic fixes when the defect belongs in Pliego.

Priority does not mean 24/7 coverage, a guaranteed resolution time, a new feature,
another document family, or another deployment.

After 90 days, the customer may request a separately signed, fixed-scope and
fixed-price assurance renewal for the same baseline. A renewal may cover pinned
upgrade review, baseline reruns, incident triage, and deployment runbook review. No
follow-on work starts without an agreed price and scope, and a renewal never changes
the open-source licenses or gates access to Pliego.

## Explicit exclusions

The $7,500 partnership does not include or promise:

- Pliego Cloud, managed hosting, or a customer-facing rendering service;
- open-core restrictions, proprietary rendering features, or a private generic fork;
- per-page, per-document, volume, or runtime licensing;
- a paid SDK license or paid access to SDK capabilities;
- unpriced bespoke consulting or open-ended feature work;
- a second document family or second deployment;
- broad browser or Chromium parity;
- 24/7 incident response, a response-time SLA, or compliance certification; or
- publication of customer templates or data.

Additional work requires a new written fixed scope and price before it starts. This
document does not authorize prospect contact; outreach requires the designated human
owner's approval.

## Decision-gate evidence

Market validation is recorded only in the
[redacted validation ledger](./design-partner-validation-ledger.md). Likes, stars,
waitlist entries, verbal interest, unpaid pilots, and issued-but-unpaid invoices are
not deposits and do not satisfy the gate.
