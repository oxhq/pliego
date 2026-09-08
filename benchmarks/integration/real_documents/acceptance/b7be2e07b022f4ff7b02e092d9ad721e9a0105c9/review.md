# b7be real-document visual acceptance

Reviewed 2026-09-08. These six exact PDFs from
[preflight 34185936368](https://github.com/oxhq/pliego/actions/runs/34185936368)
pass their original document oracles and complete-page visual review. This
permits planned exact-identity timing, **not a performance or release claim**.
The adjacent JSON files bind each campaign, provider, PDF and layout fingerprint;
no earlier candidate's acceptance or timings are transferred.

## Exact evidence

Native and harness source: `b7be2e07b022f4ff7b02e092d9ad721e9a0105c9`.
Proof source `f89ec5f548dbf7745da58c74928c9b7eb0c3a652` changes only the registered
workflow mapping; it is not an engine change and must not merge.
The [Linux development package](https://github.com/oxhq/pliego/actions/runs/34183772850/artifacts/10040112871)
contains the 197,968,000-byte binary with SHA256
`db2037ebc9ecd91f9fb6462a0dbf862667bcf89117a6690bce182e071ab21902`.
Its outer ZIP is `dfbbb454c44c643697f204b51e289beb877af24a645cbd23c795072fcd82da2e`;
inner archive is `56e157010f630069465a0c4c5e8554f47ce5cf2424311f417a6ace178997c673`.

| Proof artifact | ZIP bytes | ZIP SHA256 |
| --- | ---: | --- |
| [Manufacturing 10040543091](https://github.com/oxhq/pliego/actions/runs/34185936368/artifacts/10040543091) | 3,717,058 | `27da5334bd89c53e24a7d47cdcbd8a52698516a6dc98b15c5dbcb19e02235914` |
| [Ledger 10040547931](https://github.com/oxhq/pliego/actions/runs/34185936368/artifacts/10040547931) | 7,274,315 | `0a579591117bd641fdd9330014c4f0496d7fd171d340af893418a2097c68644d` |
| [Invobook 10040557062](https://github.com/oxhq/pliego/actions/runs/34185936368/artifacts/10040557062) | 3,507,472 | `92bb81b5735b3954fe5b14509b7d6f552a117a9af3862b9ef2e5ccd68b85dc43` |

All 372 extracted files and original ZIP hashes were verified. All 90 unique
source/corpus files match committed b7be bytes and retained identities. The
unchanged committed campaign verifier passes all three moved archives: two
correct preflights each, zero warmups, zero timed samples, no qualified aggregate.

The incumbents are dompdf 3.1.6 and, for Invobook, Browsershot 5.4.0 with
Puppeteer 25.8.0 under the shared Laravel 12.69.1 baseline. Both Invobook providers
regenerate the same repaired HTML, SHA256
`afd286bca202309923fd66bee1f71e732bdd340d1b05a2307094feb535fa7195`.
Recorded locked and installed dependency audits pass; older dependency-graph
results do not qualify this baseline.

## Complete-page review

All **32 pages** were inspected: ledger 13 Pliego / 15 dompdf, manufacturing
one page per provider, and Invobook one page per provider. No clipping, missing
glyphs, broken rows, lost headings, obstructed totals or footer collisions were
observed at the reviewed scale. Ledger/manufacturing were rendered at 110 dpi
with Windows Poppler 26.07; Invobook used the retained full-page Linux oracle
rasters. This is functional layout acceptance, not pixel identity.

- **Ledger:** all 300 ordered detail entries, numeric columns and running
  balances pass, with repeated headings and clear page-owned footers. Independent
  word-box checks reproduce all 600 provider-row placements. Different 13/15-page
  pagination is accepted; neither output contains blank surplus pages.
- **Manufacturing:** three components, two ordered operations, quantities and
  all three barcode regions remain intact. The original Linux oracle decoded
  every expected Code128 value. This is a one-page work order, not a long report.
- **Invobook:** seller/buyer details, both line items, notes, dates and currency
  glyphs are readable; EUR375.00 taxable amount plus EUR75.00 tax yields the
  visible EUR450.00 total. Minor border/alignment differences are nonmaterial.

Original oracles use Linux Python 3.12.14, Poppler 24.02, pypdf 6.16.2,
fonttools 4.60.0 and, for barcode checks, zxing-cpp 2.3.0 / Pillow 11.3.0.
Supplementary Windows checks reproduce both ledger and both Invobook oracle
passes, including exact facts, font proofs and layout fingerprints. Their
Poppler 26.07 environment is not Ubuntu-equivalent. Manufacturing's local
supplementary checks stop before PDF inspection because zxingcpp is absent;
local Pillow also differs. Those environment failures remain failures, not
PDF failures or substituted passes. The original pinned Linux evidence remains
authoritative and unchanged.

## Timing boundary

Future samples must pass the same strict oracles and reviewed layouts. Keep
document families and host populations separate, retain all repeats and
failure/unattempted denominators, and distinguish engine from full-process wall
time. The controlled filesystem and cold-process setup are not typical warm
Laravel request/storage latency or a cold-OS-cache claim. These repaired inputs
do not establish independent adoption, other-template coverage, accessibility,
public-package consumption or publication readiness.
