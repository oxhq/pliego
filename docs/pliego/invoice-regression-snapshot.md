# Reviewed invoice regression snapshot

The direct-session invoice test uses a current regression snapshot, not a claim
that its bytes remain identical to the historical pre-session shell output.
The snapshot was reviewed on 2026-09-05 after two intentional capture changes.
This synthetic Ahem fixture is separate from API 2 business-document qualification.

## Retained same-platform inputs and outputs

[Historical Linux diagnostic run](https://github.com/oxhq/pliego/actions/runs/33950773395)
at `cc88194bea3f4824c4355590c6e76f67873e3930` uses production source
`8938b207f974e54525d23ee675479d73bf9e1b15` plus only a test retention hook and
narrow diagnostic workflow. The diagnostic branch must not be merged.
Artifact `9964858702`, `pliego-historical-invoice-oracle`, has ZIP SHA-256
`dc7814fb6d9ceca1da386905193e7de9c33eba6d59ea7ea678dc57e7d3dc4cdd`.
It reproduced both original expected hashes exactly.

[Current Linux diagnostic run](https://github.com/oxhq/pliego/actions/runs/33950195004)
at `3cd155d90a7d842ffded9f37608f1c84be4a0176` retained the changed output in
`pliego-direct-invoice-oracle` before the old golden assertion failed.
Both inputs have SHA-256
`b0fa2d0b18e845e84c1229408622bd85e092ecf4d78b0878939006fb26926dce`,
and both original Ahem fonts have SHA-256
`b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448`.

| Artifact | Historical SHA-256 | Current SHA-256 |
| --- | --- | --- |
| Diagnostic scene | `c1874a92a71ecde580f15075fe7d07ad6e5739ec794ad79291c9ba5b9bce1681` | `97997f0c5863c1ff27cf0511d7a96c22121216d6ca3320191ad369146af790c5` |
| PDF | `401e756f43adad12a137478cf36abe8273e89405e998b9d537ab62056d2face9` | `952988f08a5be37dd7cc262326d2a50c592ff0a329c84ae82b9c1d5381f5f96e` |

## Explained differences

- Page 1 is unchanged. Page 2 retains the same 238 operations. Stable original
  paint-sequence sorting (`88a99c742c`) moves four repeated-header text operations
  after 153 body-border operations: 157 indices change, no payload is lost.
  This preserves the ordering needed to keep opaque backgrounds below headers.
- Original-app-unit page-local rectangle projection (`5824152dc3`) changes only
  113 page-2 border Y values and their path strings. Ninety-nine changes are
  +0.000030517578125 CSS px; fourteen are +0.00002288818359375 CSS px. All come
  from original integer authority, not integers recovered from floats. No edge
  height, clipping, glyph or resource changes in this fixture.
- For page height 67,351 Au and local coordinate `y`, the old projection is
  `f64(f32((y + 67351) / 60)) - f64(f32(67351 / 60))`; the new projection is
  `f64(f32(y / 60))`. Every changed coordinate was checked against retained Au.
- Applying only the paint permutation to historical bytes reproduces the
  intermediate scene hash
  `f4e58d6bced2cce5241753fd012c134d2605607fe21b3cb98854b4fa748f6796`
  from run `33949042527`. Adding the exact projections reproduces current bytes.
- All 143 text-operation payloads, fonts, resources, page geometry, 32 rows
  (16 per page) and final total are unchanged. Seventeen repeated-header
  operations additionally retain internal Au authority (`a3af6fe525`), without
  adding that ledger to the diagnostic scene JSON.
- Decoded PDF differences are the corresponding page-2 drawing order/path Y
  operands plus content-derived Krilla XMP/trailer identifiers. The PDF changes
  from 9,392 to 9,380 bytes; no text or embedded-resource difference remains.

Read-only PDF-object comparison used pypdf 6.10.0. Both PDFs were rasterized with
Poppler 26.07.0 at 144 dpi and every page visually inspected. Each page pair is
pixel-identical: zero changed pixels in each 1,191 x 1,684 raster. Page PNG hashes
are `8cd78f1937a751d91ef5e0eebe8083f8f84d110a5b196ce24582ec3949608012`
and `4fda02a8fdb2ba42a8ca6cf825407daec64f26de6d9bfa37ac742d88a693355a`.
This is sampled visual evidence, not equality at every resolution or a benchmark.

Keep these provenance facts, original input, conservation checks and focused
paint-order/authority tests when refreshing this snapshot. A new hash from a
failing test alone is never sufficient justification. Native API 2, packaged
consumer and release gates remain independent requirements.
