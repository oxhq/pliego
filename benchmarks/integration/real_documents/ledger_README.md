# Frozen Aureus ledger correctness gate

`ledger/` is the shared-repaired 300-entry General Ledger generated through the original pinned Aureus action. Both providers must render the same `document.blade.html` and four original font assets. The exact source, MIT/DejaVu notices, two-declaration footer repair, original dompdf options, dependency repair and synthetic-accounting provenance are retained. No application vendor, database, credentials or host-specific source paths are required by this oracle. `$APP` and `$TEMP` in `source-action-facts.json` describe relocatable source settings, not literal executable paths.

The input SHA-256 is `ccb11d4c713a66d71fe7798e03bfbeb3b7fe90b74229790c7e9c1a512a7aa105`. `closure.json` binds every fixture file's size and SHA-256 and rejects missing, extra or changed files. The fixture-scoped `.gitattributes` preserves the original bytes across Git checkouts, including line endings. The campaign must additionally pin that closure and the oracle source hashes in its own manifest; a self-reported closure is not a signature.

## Portable commands

Use the campaign's pinned Python environment. The tested direct dependencies are pypdf6.16.2, pdfplumber0.11.9 and fonttools4.60.0. pdfplumber's PDF extraction dependency is pdfminer.six20251230. Root campaign setup owns the common requirements and platform wheel/hash retention; no hardcoded Windows executable path is in the oracle.

```sh
python benchmarks/integration/real_documents/ledger_test.py
python benchmarks/integration/real_documents/ledger_test.py --legacy-pdf /retained/legacy.pdf

python benchmarks/integration/real_documents/ledger_oracle.py \
  --fixture benchmarks/integration/real_documents/ledger \
  --provider dompdf --pdf /retained/document.pdf --output /fresh/oracle

python benchmarks/integration/real_documents/ledger_oracle.py \
  --fixture benchmarks/integration/real_documents/ledger \
  --provider pliego --pdf /retained/delivery/document.pdf \
  --scene /retained/delivery/scene.json --bundle /retained/delivery/bundle.json \
  --output /fresh/oracle
```

For actual-PDF review, add `--render-pages --pdftoppm /explicit/path/to/pdftoppm`. The checker retains first/middle/last PNGs and the executable hash. Output must be fresh and outside the fixture. `qualify(fixture, pdf, provider, scene=None, bundle=None)` is importable by the coordinator and returns `(report, extracted_text)` without launching a renderer. Every oracle runs outside timing.

## Shared font helpers

`ledger_fonts.py` exposes the same strict primitives to the other frozen DejaVu families. It does not require using the ledger's business or pagination oracle:

```python
from pypdf import PdfReader
from ledger_fonts import qualify_fonts

font_report = qualify_fonts(
    PdfReader(pdf, strict=True), font_fixture, "pliego", scene, bundle, pdf,
    required_faces={"DejaVuSans.ttf", "DejaVuSans-Bold.ttf"},
)
```

`font_fixture` contains `font-closure.json` with `assets: [{file, sha256, ...}]` and `resources/<file>` original bytes. Alternatively pass `source_fonts={"DejaVuSans.ttf": (original_path, expected_sha256), ...}` to reuse another corpus's existing paths without copying. The caller must bind these to its actual input resource closure. Source paths/bytes are rechecked on every call, before cached parsing. Omitting `required_faces` deliberately keeps the ledger's normal/bold/oblique expectation; passing a different set must be backed by that document's reviewed style use. `provider="dompdf"` checks original whole-font bytes with the same exact face-set expectation. Neither mode accepts Chrome/Browsershot font encoding by analogy.

The lower-level importable functions are `source_font(path: str, expected_sha256: str) -> (facts, TTFont)`, `glyph_fingerprints(font)`, `semantic_font_identity(font, fingerprints)`, `match_subset_program(raw: bytes, sources: dict[str, tuple[dict, TTFont]], required_faces: set[str]) -> (source_name, TTFont)`, and `checked_bundle(bundle_path, scene_path, pdf_path) -> (scene, verified_resource_paths)`. `match_subset_program` verifies original named glyph programs/metrics only; it does not itself prove a PDF's CID/Unicode mapping. `qualify_fonts` adds the existing bounded native Identity-H/Identity/ToUnicode and scene mapping gate. A browser that drops original glyph names or remaps CIDs needs separately verified mapping, not a font-name fallback.

## What a pass proves

- Independently recomputes all300 amounts (`10000 + 25*i` cents), every-fifth refund, dates,240 sales/60 credit notes, opening-125000, debits828750, credits3300000 and closing-2596250 cents. Frozen HTML and PDF rows must conserve markers/order, dates, actual frozen account references, partner, debit-versus-credit placement and running balances. Draft/future/opening-control markers may not leak into expanded period details.
- Preserves primary/opening/total summaries; seven repeated headings and header-cell backgrounds; exact row-to-column association and reviewed right alignment; no missing/duplicate/split rows or overlapping details. Numeric bands come from the actual seven original-color header-cell rectangles, not a broad fixed x tolerance. Axis-aligned closed rectangle paths are accepted alongside PDF `re` operators; arbitrary path bounding boxes are not.
- Requires A4 landscape, sequential original `Page n` footer content and fixed generated-at text, an identifiable full-width footer rule, no table/footer overlap or off-page footer. It records complete page/row/column/footer facts and a layout fingerprint. The machine gate intentionally does not require Pliego to have the same page count as dompdf before candidate review.
- Legacy fonts must be exactly the original unsubsetted normal, bold and oblique font bytes. Listing fonts in the input or matching PDF names is insufficient.
- Pliego must bind the exact PDF, scene and resource bytes through its bundle hashes. Each captured font resource must cryptographically match exactly one original face's complete glyph-order, decomposed contour coordinates, on-curve flags, horizontal metrics, Unicode cmap, units/em and style identity. OTS repair of stale source glyf bounding-box metadata is allowed because those metadata are not contour coordinates. Changed contours/metrics/cmap/style are not allowed. Face-index/variation/synthetic-bold substitutions are excluded for this fixture.
- Every embedded candidate subset must match original contour/metric fingerprints. Its Identity-H/Identity CID mapping and ToUnicode glyph identities must equal those captured in the hash-bound scene. This is a bounded TrueType/Krilla font surface, not arbitrary PDF font equivalence or permission to substitute a similar font.

## Proof boundaries and review

The retained legacy PDF passes locally with15 pages and300 rows; sampled pages1/8/15 were rendered for review. Eleven portable tests cover positive closure/math, malformed facts/HTML, seven numeric-column corruptions, missing/wrong/duplicate footer content, bundle hash/path/duplicate records, bbox-only versus outline changes, subset/metric/wrong-face changes, source-file replacement after caching and a foreign embedded legacy font. The optional real-PDF test reads a supplied PDF and never changes it.

All authoritative requirements are explicit function checks and remain active under `python -O`; a static test prohibits optimized-away assertion statements. Optimized subprocess tests rerun negative helper cases and verify corrupt input produces a failing CLI report. Artifact paths inspect every lexical ancestor before resolution and reject symlinks/Windows reparse points, non-files, traversal, drive/UNC/ADS aliases, reserved/noncanonical names and case-folded duplicates. Tests exercise actual file/directory/root/manifest/scene symlinks when the host permits creation and otherwise report a skip. Inputs should use unaliased paths without parent traversal; this is a retained-artifact integrity gate, not race-proof hostile-filesystem containment.

The actual new candidate ledger remains a separate native gate. Strict page/header/footer and font mapping assumptions may expose a real unsupported surface; do not weaken them just to obtain a timing result. This oracle never sets `visualQualified` or `benchmarkQualified` true. The campaign must retain actual-PDF review and freeze accepted page/row/spacing facts or layout fingerprints before timing; a new unexplained variant withholds qualification. It must also bind the exact candidate source/binary/contract and run actual application/storage proof separately. A machine pass does not establish accessibility, independent adoption, cross-platform layout identity or a speedup.
