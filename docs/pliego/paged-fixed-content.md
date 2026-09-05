# Candidate paginated fixed-content profile

This is the bounded implementation and regression contract for the 0.4 development
line. It is not a claim that 0.4 has been released or that the real-document corpus
has passed. Native proof and the exact packaged release remain separate gates.

## Page-owned fixed text

Normal flow determines the page count before initial-containing-block fixed content
is realized. A fixed sibling before, between, or after normal-flow children must
neither consume flow space nor change their forced breaks or continuation indices.
A fixed subtree cannot start another pagination claim or create additional pages.

The candidate profile covers horizontal, page-root `position: fixed` Flow containers
with an explicit `top` or `bottom` inset. Their descendants must be static Flow,
text, or ordinary inline boxes. Each physical page receives newly constructed layout
boxes and text runs. Normal Servo shaping, alignment and sizing position the result;
no glyphs are synthesized or relabeled in the PDF writer. Later replicas retain the
original DOM placeholder's paint position, with separate fragment identities.

The containing block is the page content rectangle. For example, with a 60px bottom
page margin, a 20px fixed line with `bottom: -40px` occupies the physical page's
260–280px region on a 300px page. Authors must reserve sufficient margin themselves;
fixed text does not automatically push normal content away.

## Decimal physical page counters

Generated `counter(page)` and `counter(pages)`, including an explicit `decimal`
style, resolve to the 1-based physical page number and final physical page count.
Typed source ranges distinguish those tokens from literal zeros. Page-specific
text is shaped afresh, including the wider number at the 9-to-10 transition.

This is not the general CSS counter-state machine. Unknown counter names,
`counters(...)`, other counter styles, transformed or security-obscured counter
text, and counters outside an admitted fixed subtree remain unresolved and prevent
successful API 2 delivery. Parsed `counter-reset` or `counter-increment` changes to
`page` or `pages` are also rejected, including on other retained boxes in scope.
Servo's existing CSS parser policy is unchanged: currently preference-disabled
counter declarations, and properties it does not implement such as `counter-set`,
may be ignored as unsupported CSS rather than retained for this rejection check.
No claim of arbitrary counter declaration validation is made.

Tables, flex/grid, replaced content, floats, nested positioning, relative inline
descendants, vertical writing modes and both block-axis insets being `auto` are
outside this fixed-content profile. Existing paint and exact-geometry exclusions
still apply. An excluded retained fixed subtree records `paged-fixed-content`;
unresolved retained counter text records `unresolved-page-counter`. API 2 must return
an artifact failure with `SCENE_ENCODING_FAILED` and no success delivery, never a
plausible PDF with missing footers or repeated page-one numbers.

The pinned Stylo `layout.writing-mode.enabled` preference is disabled. Authored
`writing-mode: vertical-rl` therefore does not create a vertical computed style;
it follows the ordinary horizontal fallback. The native fallback case records
actual `CSS.supports` false and an unavailable CSSOM property, then checks the
full horizontal scene/PDF geometry. A separate Rust predicate test rejects
computed vertical/sideways writing modes. This is not vertical-layout support
or a claim that the ignored author declaration produces an API 2 rejection.

Continuous Servo layout is unchanged. The page-root path requires explicit Pliego
page configuration; a viewport-fixed element in an ordinary document is not a
printed-page replica.

## Contributor evidence

```sh
python ports/pliego/tests/check_api2_fixed_content.py --self-test
python ports/pliego/tests/check_api2_fixed_content.py \
  --binary /absolute/path/to/pliego \
  --source-commit FULL_SOURCE_SHA \
  --out /absolute/fresh/proof-directory --require-pdf-text
```

The native suite checks twelve forced pages, exact Ahem font and integer app-unit
geometry, body order, per-page footer text, centering, top/bottom placement, literal
zero preservation, wrapper/table siblings and explicit exclusions. Pure negative
checks mutate missing/duplicated/aliased text, placement, font and page count so the
oracle cannot accept them. The direct-debug caller allowance is 180 seconds;
optimized bundles use 65 seconds. The engine uses the API default 60-second
host-wall budget; see the [qualification-budget decision](api2-qualification-budgets.md).
These functional timings are not performance-comparison samples.

The four-platform package workflow retains every case's input, canonical request,
result, artifact hashes and caller outcome. Linux additionally requires PDF text
extraction. Passing this focused suite does not by itself qualify the real ledger,
the provider comparison, or general paged-media compatibility.
