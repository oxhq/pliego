# Final-candidate Aureus visual acceptance

Reviewed 2026-09-05 from the actual retained PDFs and rendered pages in
[preflight run 33964829162](https://github.com/oxhq/pliego/actions/runs/33964829162).
This permits exact-identity comparison campaigns; it is not release approval,
a timing result, public-package consumer proof, or independent adoption.

## Bound identity

- Native 0.4.0 source: `4a99c0e73d2e9dfb7a84ccb5b0d8705fd3d76287`.
- Linux executable: 198,070,176 bytes, SHA-256 `1f104acf55349cfb6067ddbe133ef5f10d230d1c6f7bd0c992cd4ce400abbc41`.
- Package push run 33964018057, Linux artifact 9969067014; outer ZIP SHA-256 `20222e97d7cfdd5b7e11a4dd3329933a66bc70f810aebdcd01b4110fe6a2227e`.
- Workflow source: proof-only `782d62b988a3bf2338e14c3d68a5359079556aab`, parent exact native source. The registered performance workflow is a dispatch overlay only; do not merge it.
- Actual incumbent: Aureus dompdf 3.1.6 / Laravel-dompdf 3.1.2; shared approved dependency lock `82133507ad710cc2748d95cba0ea3dfe5d375728c0d8d3587303e91d027c5fae`, with passing audit and no bypass.
- Ledger proof artifact 9969125770: 7,275,101 bytes, SHA-256 `4a01cf5837ec56e597b8db49ee52595cfa4281fa6b92791b67db9969b1211774`, 101 inventory files.
- Manufacturing proof artifact 9969124350: 3,723,375 bytes, SHA-256 `39f70d8355f3cdcc57c1c3a4569989f613cb33819d165fa38fa582c2a683cf73`, 115 inventory files.

Both transport digests, safe extraction inventories, exact run/source/job facts,
and moved campaign verification passed. The current v2 used-font policy is
required for native outputs; original whole-font bytes are required for dompdf.
The JSON files alongside this review bind the complete runtime/corpus/oracle
identity and the exact reviewed PDF and layout hashes. The original ZIPs and
local recheck are retained outside Git under the run-numbered evidence folder.

## Reviewed output

| Document | Provider | Pages | PDF bytes |
| --- | --- | ---: | ---: |
| 300-entry general ledger | Pliego | 13 | 83,480 |
| 300-entry general ledger | dompdf | 15 | 1,313,404 |
| Manufacturing work order | Pliego | 1 | 28,919 |
| Manufacturing work order | dompdf | 1 | 880,583 |

The ledger oracle checks every ordered entry, date/partner/account association,
recomputed debit/credit/running balance, total, repeated heading, page counter,
numeric column and footer clearance. Native first/middle/last pages 1/7/13 and
legacy 1/8/15 were visually reviewed: readable fonts, aligned amounts, distinct
row shading, unclipped contents and clear footers. Different row density and
pagination are accepted functional layout, not pixel parity.

Both complete manufacturing pages were visually reviewed: readable metadata,
quantities and operations; clean table structure; intact barcode areas and
unobstructed footer. The oracle independently decodes all three Code128 values
and verifies component quantities and operation durations. This is a one-page
work order, not a long operational report. It uses the same frozen capture for
both providers, without application regeneration in the measured interval.

Each provider/document has one successful untimed preflight. There are no
timed observations in these archives and no latency or reliability ratio is
claimed here. Subsequent runs must retain all scheduled observations and their
typed failures, validate every PDF, and match these reviewed layouts. Keep
families and independent host repeats separate. Failed or incomplete campaigns
must not contribute a success-only speed ratio.
