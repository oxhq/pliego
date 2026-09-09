# e179 Aureus visual acceptance

Main reviewed the actual retained rendered pages on 2026-09-05 from
[preflight 33967655301](https://github.com/oxhq/pliego/actions/runs/33967655301).
This permits exact-identity timed comparisons only. It is not release approval,
public-package consumer proof, independent adoption or a performance result.
This candidate-specific review supersedes 4a acceptance for the final release gate;
the earlier runs and acceptance files remain preserved as historical evidence.

## Exact identities

- Native 0.4.0 source: `e179440540527261e96d303b1c42499b0ac031be`.
- Linux binary: 198,066,000 bytes, SHA256 `7fe9ed8ea5bd870745f01358234b88234ea469f31f6d8f5f2260f806251ff23b`.
- Push package 33966565525, artifact 9969884992; outer ZIP 62,696,313 bytes,
  SHA256 `3af4dcf476f34a9fb4d37fba389763fe9e031b41e55c703bfc1d8b9522535f3c`.
- Native archive SHA256 `f0ef73b821fb50321ab4af62fd7b1e9ccdab88ea0c99c4f371867c7648502f72`.
- Proof-only workflow source: `6e5268a6156824d0b936bf2975b1e675b94ddf10`,
  parent exact native source. The registered workflow overlay must not be merged.
- Ledger proof 9969990939: 7,275,438 bytes, 101 files,
  ZIP SHA256 `60a89c5090303193c68008e59a833532c0d34df6d44a5be5667acf740e23ebc5`.
- Manufacturing proof 9969982929: 3,721,672 bytes, 115 files,
  ZIP SHA256 `47403437e8b8f4ba2a2cdb80e5d45a7b4380af168ccc0bf2b96aaec020e6bdaa`.
- Actual incumbent: dompdf 3.1.6 / Laravel-dompdf 3.1.2; approved shared lock
  `82133507ad710cc2748d95cba0ea3dfe5d375728c0d8d3587303e91d027c5fae`.
  Audit and platform checks passed without advisory bypass.

The original ZIPs, full inventories and moved-archive rechecks are retained in the
workspace's run-numbered evidence folder. Each verifier is bound to 27 exact code
files at the proof source and 20/19 corpus files. Full native API 2 result,
scene/bundle/resource/diagnostic closure passes. Hosted business/font/layout
oracles remain retained under their exact dependency environment; local metadata
rechecking does not pretend to rerun those oracles under different dependencies.

## Actual page review

| Document | Provider | Pages | PDF bytes |
| --- | --- | ---: | ---: |
| 300-entry ledger | Pliego |13|83,480|
| 300-entry ledger | dompdf |15|1,313,404|
| Manufacturing work order | Pliego |1|28,919|
| Manufacturing work order | dompdf |1|880,583|

Main inspected native ledger pages 1/7/13, legacy 1/8/15 and both complete
manufacturing pages. Ledger headings, amount columns, row shading, last totals
and page-owned footers are legible and unobstructed. All 300 entries and recomputed
balances pass the business oracle. Native text uses 23,374 painted glyphs and 107
verified scene/PDF mappings under strict actual-used-font v2.

Manufacturing quantities, two operations and component rows are legible; all
three barcode regions remain intact and decode correctly. Native uses 420 painted
glyphs and 76 mappings. Both documents fit their accepted page geometry without
clipping or overlap. Different density and pagination are functional-layout
acceptance, never cross-renderer pixel identity. Manufacturing is explicitly one
page, not a long operational report.

Each provider/document has only one untimed preflight here. The adjacent JSON
records bind full runtime/corpus/oracle identities and exact reviewed PDF/layout
hashes. Later observations must pass every oracle and match reviewed layouts;
retain all repeats and outcomes separately by family and host. No success-only
speed ratio may be computed from failed or incomplete populations.
