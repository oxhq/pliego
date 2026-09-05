# Frozen repaired Invobook invoice

`fixtures/invobook` contains the exact one-page simple invoice from
[Invobook e5f666ce](https://github.com/Hasnayeen/invobook/tree/e5f666cef63543beffadfcc045f6af673408a02e).
Its original action rendered real synthetic work-session data. This is internally
operated external application code, not independent adoption or all-invoice
compatibility. No app boot, database, browser launch or dependency installation
is needed to verify an already produced PDF.

This is an explicitly repaired, shared-input track. The original missing item
currency was added to persistence; the action supplies duration-derived quantity;
the simple stylesheet applies to all media; and two original DejaVu font faces
are supplied. Original/repaired source and all three patches are retained.
The original extra Seller/Buyer line remains visible. Both providers receive
identical HTML and font bytes; the HTML SHA is
`afd286bca202309923fd66bee1f71e732bdd340d1b05a2307094feb535fa7195`.

The original app uses Browsershot 5.0.5. The selected Puppeteer 25.8.0 is a harness
dependency, not an upstream app lock. The manifest and provenance retain exact
app/Composer/renderer identities. Actual URL/authentication/Livewire preview are
not exercised by frozen HTML delivery. Legacy storage/readback and native SDK
integration are separate workflow gates, never included in this PDF oracle.

## Correctness oracle

```sh
python invobook_test.py
python invobook_font_test.py --legacy-pdf /absolute/reviewed-chrome-invoice.pdf
python invobook_oracle.py --pdf /absolute/invoice.pdf \
  --provider browsershot --poppler-dir /absolute/poppler/bin --output /absolute/fresh-output
python invobook_oracle.py --provider pliego --pdf /absolute/bundle/document.pdf \
  --scene /absolute/bundle/scene.json --bundle /absolute/bundle/bundle.json \
  --poppler-dir /absolute/poppler/bin --output /absolute/fresh-candidate-output
```

This oracle uses pinned pypdf 6.16.2/fonttools 4.60.0 plus explicit `pdftotext`, `pdffonts`
and `pdftoppm` executables (or `.exe` counterparts). It retains their hashes,
version output, commands and 30-second deadlines. Pin those tools in the hosted
campaign. `--corpus` relocates the complete fixture; `--assets` optionally verifies
a separately staged directory containing the two exact TTF files. The adjacent
`manufacturing_corpus.py` supplies only the existing hash/path guard helpers.
The default provider remains `browsershot`. The Pliego path uses
the shared `ledger_fonts.py` verifier; Chrome uses its separate `invobook_fonts.py`;
the oracle never installs dependencies. `manufacturing_font_test.py` exercises
both fixtures' independently pinned original regular/bold source bytes. Pliego
requires both `--scene` and `--bundle`; legacy invocations reject those flags.

It checks one A4 page with the original one-point paper tolerance, seller/buyer,
serial/dates/notes, exact regular/bold embedded Unicode DejaVu face names, and
the independently recorded equations in cents:

- 1 x 12500 = 12500; 2 x 12500 = 25000.
- 12500 + 25000 = 37500; 37500 + 7500 = 45000.

Every quantity and amount must belong to its exact description/summary row.
Rows must occur once, in order, without overlap. Quantities center below `Qty`;
prices/subtotals and summary labels/amounts align with their header column edges.
The table respects the authored 36pt body margins. There must be exactly seven
monetary cells, with totals preceding the original notes. The alignment tolerance
is 0.75pt (one CSS pixel), not a relaxed arbitrary column band. Actual overlapping
text boxes are rejected; the mixed-size total uses line-box center grouping.

Seventeen mutation checks run on each PDF, including wrong quantity/item/total,
missing or duplicated rows, overlap/reorder, moved/swapped monetary columns,
font substitution, missing embedding/Unicode, clipping and extra pages. Pure
tests also corrupt corpus bytes and staged fonts. The reference observation is
only a unit-test input; actual validation always extracts the supplied PDF.

For Pliego, machine success now requires exact bundle/PDF/scene hashes,
sanitized-font semantic equivalence to both original source faces, and PDF subset
outline/metric/CID/Unicode closure with captured scene glyphs. These are Krilla's
qualified encoding rules: they are not applied to Chromium. The separate Chrome
gate verifies the observed Identity-H, identity CID-to-GID mapping, scalar BMP
`bfchar`/`bfrange` Unicode map and the CIDs actually painted by active `Tf`/`Tj`
operations. Each mapped anonymous subset glyph must match its original Unicode
cmap glyph's contours and horizontal metrics, with matching face style and PDF
advance widths (five-decimal serialization tolerance). Other encodings, forms,
text operators and graphics-state font selection fail closed. Twelve mutations
of the actual retained Chrome PDF cover these boundaries. This proves the
observed producer profile, not every future Chromium subset format.

The current Pliego v2 font proof additionally checks actual active-font CID
shows in exact per-page scene order and multiplicity. It supports the observed
single-CID, single-show inline ActualText alias, with strict balanced scope.
Unused CMap entries do not count as painted text. The original invoice uses
this legitimate override when the same zero glyph has different text spans.
`ledger_font_runs_test.py` covers this alias and rejects resealed encoding,
scope, count, order, glyph and Unicode corruptions without changing input/PDF
bytes or weakening the source-font proof.

`report.json` retains `pdfSha256`, `fontProof`, and `layoutFingerprint`, computed
only from extracted page/text geometry and normalized font facts. Paths, timings,
metadata, object IDs and random subset prefixes are excluded. Timed samples must
match an explicitly visually reviewed preflight variant in the same environment,
not necessarily the competing provider's fingerprint. Visual review,
storage/readback, performance and release qualification remain separate.
Linux and actual native execution are separate gates; a legacy pass or synthetic
font unit is not candidate compatibility proof. `benchmarkQualified` stays false.

## Third-party boundary

Invobook's README declares MIT but its pinned tree has no LICENSE file.
The LaravelDaily-derived template is **GPL-3.0-only**, and the original fonts
have their own license. All notices and source provenance are retained in the
isolated fixture subtree. Do not call the whole corpus MIT/MPL or include it in
runtime/SDK release packages. See `fixtures/invobook/licenses/NOTICE.md`.
