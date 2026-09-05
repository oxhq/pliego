# Reviewed Chart.js regression snapshot

Reviewed on 2026-09-05. This records a compatibility Scene v1/PDF golden update,
not API 2 Chart.js qualification, a release approval, or a performance result.

## Same-input reference

The old bytes are from Linux x86_64 source
`974f7eedb61d2a3175fa5bbc8993b7a46145253a`,
[run 33946559265](https://github.com/oxhq/pliego/actions/runs/33946559265),
artifact `9964054083` (`pliego-direct-controlled-capture-proof`). Its three Chart.js
routes each match the exact old scene, PDF and canvas hashes, and all 20 entries of
each bundle pass size/hash verification. The overall run failed later API 2 startup
checks; the Chart.js subproof passed. This is not an overall CI success claim.

The changed bytes are from Linux x86_64 source
`3a8262ec9f8acb6154d127c757e6ee6da6d3c17d`,
[run 33953010625](https://github.com/oxhq/pliego/actions/runs/33953010625),
artifact `9965559600` (`pliego-direct-chartjs-oracle`). Its ZIP SHA-256 is
`5a243dee842c0f94c2fb638e091baa88aa9804a6f2cb831f7b4bad954283597e`.
The test retains original scene/PDF/canvas/assets and fixed-point authority before
asserting any golden. Every manifest entry was independently hash/size checked.
The run failed the old scene assertion; no paginated-footer change was in this source.

These inputs and outputs remain byte-identical:

| Resource | SHA-256 |
| --- | --- |
| Original HTML, 9,801 bytes | `2c5d37327bbde05b8369fcb5ea75cfec7fba437b1232848f1c2e20d5f2978995` |
| ReportSans/DejaVu Sans 2.37, 757,076 bytes | `7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954` |
| Chart.js 4.5.1 UMD, 208,518 bytes | `ecc3cd1eeb8c34d2178e3f59fd63ec5a3d84358c11730af0b9958dc886d7652a` |
| Canvas PNG, 14,197 bytes | `3625ec653c27b9e1c8d0fa969acbd88cc161804eeea4cd3046795d411e8118c9` |

Readiness still reports a 678x250 canvas, two datasets, six points, 678,000 readback
bytes, 49,879 painted pixels and six chromatic buckets. All five resource requests
load successfully, without delegated, failed or unavailable bodies.

## Complete change boundary

Both scenes have one identical 760x840 CSS-pixel page and the same 216 operations
in the same order: 86 paths, 129 text runs and one image. All text, glyph positions,
font references, colors, chart pixels and image placement are unchanged.

Exactly ten existing background rectangles change their width or height: operation
indices 19/22/25/28 (table header), 88/101/112 (metadata), 123/134/143 (KPIs). There
are eleven scalar dimension changes and ten corresponding path-string changes.
No rectangle is added, removed, recolored, repositioned or reordered. No root-canvas
extent changes. The maximum delta is 0.0000247955322265625 CSS pixels.

The original-Au solid-color producer (`88a99c742c`) and exact intersection/projection
path (`7e1c398d60`) explain every change. Previously the rectangle size came from
subtracting floating maximum/minimum coordinates. Now it comes directly from the
original integer size. Independently matching old paint events to their retained
layout fragments and adding the unchanged authored padding/borders reconstructs
the current integer authority and the entire current scene byte-for-byte.

For example, a header's original content height 891 Au plus 780 Au of authored
padding/border is 1671 Au. Its direct f32 projection is 27.850000381469727px;
the previous f32 endpoint subtraction yields 27.8499755859375px. All ten rectangles
were checked using both formulas, not accepted by a blanket tolerance.

The PDF comparison finds only six corresponding changed graphics-state blocks
(twelve numeric operands, each differing by 0.00004 or 0.00002 points). The other
four scene changes round to unchanged PDF endpoints. Complete object comparison
finds only the content stream, its content-derived XMP IDs, and trailer IDs changed.
All text extraction, other operators and resources remain identical. This diagnostic
object inspection used pypdf 6.10.0; it is not the pinned API 2 annotation gate.

Poppler 26.07.0 renders have zero changed pixels at both 144 DPI (1140x1260) and
288 DPI (2280x2520). Full-page visual review confirms unchanged coherent title,
metadata, KPIs, chart, account table and footer. This does not imply equality at
every resolution or accessibility conformity.

## Snapshot hashes

| Artifact | Historical SHA-256 | Reviewed current SHA-256 |
| --- | --- | --- |
| Scene | `7649335813f3638eecfb8836e04374f98d5b62bfcea530c1787bfdee60964fde` | `13afbbf91aae65fe9c1befdde9d50f99a34efe94ae67f1281d62cdcd329b4b47` |
| PDF | `c6f00765c85aace6cc6f2eacbb7c314ea579a4de66b6c2fe43793fc3ef546c9f` | `1a6aa73f9fb3bfe50949c804ac1bf484b27f977c3a60e56a29b268f4486e0e91` |

The active constants are deliberately named `CHARTJS_SCENE` and `CHARTJS_PDF`,
not `PRE_SESSION_*`. Input, dependency, font and canvas expectations are unchanged.
The twice-fresh-session regression and all direct/packaged gates must run again
after this reviewed update. Explained golden drift is not a waiver of those gates.
