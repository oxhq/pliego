# Invobook shared application-repair track

This experiment preserves the original application baseline and tests the actual
invoice business action after a shared application repair. It is not an
independent-adoption, completed-migration, or performance result.

## Repair and delivery boundary

Use a separate checkout at `e5f666cef63543beffadfcc045f6af673408a02e` and apply
`invobook-currency.patch`: persist the invoice's `currency_code` into the required
`invoice_items.currency` column. The runner rejects any other tracked source
change. Application-owned action/templates and dependency locks stay unchanged on
disk. Each run retains the exact patch and source hashes. Separately labelled,
opt-in simple-template media and quantity/font repairs are described below; only
the latter executes an additional, exact action-source override.

The runner invokes `GenerateInvoicePdf` in its existing HTML mode, then delivers
the returned view through either app-locked Browsershot or the PHP `DocumentEngine`.
Both providers use the same original or explicitly repaired HTML, A4 with zero margins, and Laravel's
actual local filesystem `writeStream` and byte/hash readback. The transaction
commits only after delivery/readback; failure rolls back invoice rows and attempts
cleanup of that run's local object. This is not crash-atomic database/filesystem
publication or a guarantee for remote storage adapters.

The original URL/authenticated Livewire preview is not exercised. No route or
application action has been changed to use this runner. Integrating that delivery
selection into the actual UI remains a migration step. The original `elegant`
template expects an Eloquent/Livewire context and is retained as a failure in this
HTML-action track, not replaced with a different invoice template.

Invobook locks Laravel 11.31.0; the candidate Laravel SDK targets Illuminate 12/13.
The published v0.3.3 SDK still requires Illuminate 13; candidate framework checks
do not change that public package's support claim.
This runner does **not** manually autoload that SDK or bypass Composer constraints.
It exercises the PHP SDK plus Illuminate filesystem. Laravel SDK `store()`,
public-package installation, queue workers, and supported-version compatibility
remain separate proof requirements.

## Reproduce

Install the pinned application dependencies and Vite build using the main
integration README. Apply the patch only to the separate repair checkout:

```sh
git -C /work/invobook-repaired apply /work/pliego/benchmarks/integration/invobook-currency.patch
php /work/pliego/benchmarks/integration/invobook_repaired_workflow.php \
  --app /work/invobook-repaired --output /evidence/simple-html \
  --template simple --provider html
```

Repeat with `default` and `elegant` for the application census. Use a fresh output
directory every time. The runner owns an isolated SQLite database, synthetic work
sessions, caches, logs and local disk. No production credentials or services are
needed. The original Vite bytes must be built or copied with their provenance;
do not replace templates or remove their network resources to make a case pass.

For PDFs, replace `--provider html` with one of:

```sh
--provider browsershot --chrome /path/to/chrome --node /path/to/node \
  --node-modules /work/pliego/benchmarks/adapters/browsershot/node_modules

--provider pliego --sdk /work/pliego/sdk --binary /path/to/pliego
```

Provide absolute tool/SDK paths. Browsershot uses the same blocked-external-request
policy as the original prepared-HTML census; failed resources and script errors
prevent delivery. This is not an OS network sandbox. The Pliego input manifest
denies live network access. By default, no resource or media-policy adaptation is
performed.

## Shared simple-template media repair

The default track preserves the original `media="screen"` stylesheet, which does
not apply in Chromium print. To exercise the authored PDF styles, add the same
explicit option to **both** provider commands:

```sh
--template simple --simple-pdf-media all
```

This selects `shared-currency-and-simple-media-repair-html-delivery`; it does not
replace the default `shared-currency-repair-html-delivery` track. The runner
requires the pinned simple Blade source and replaces exactly one
`<style type="text/css" media="screen">` with `media="all"`. It writes an
evidence-owned Blade override and prepends its view namespace before invoking the
unchanged business action. No provider-specific HTML rewriting is performed.
Other templates reject this option.

Each adapted run retains the original and repaired Blade bytes, exact
`template-repair.patch`, both source hashes, patch hash, replacement count, and
the original application's README attribution/license declaration. The
application checkout itself remains currency-repair-only. Require identical
`inputSha256` and repair provenance before comparing the two provider outcomes;
do not pool original and adapted tracks.

The first local shared-media browser check applies the intended margins, table
rules and alignment and passes one-page A4/text facts and local storage readback.
That is not full document acceptance. Visual/source inspection also finds:

- The original universal `DejaVu Sans` declaration has no supplied font; this
  Windows Chromium run embeds Times New Roman instead. Font closure and intended
  typography remain unresolved.
- The original HTML action supplies hours and subtotal but not quantity. The
  second synthetic row displays quantity 1, unit price EUR 125, and subtotal
  EUR 250. Correct invoice totals do not make that row equation acceptable.
- The authored extra Seller/Buyer line remains; the media repair does not remove
  or redesign it.

These issues are retained, not silently corrected. The early native candidate
at `8938b207f974e54525d23ee675479d73bf9e1b15` rejects the identical input with
`artifact/SCENE_ENCODING_FAILED` for collapsed-table borders and publishes no
stored PDF or invoice record. This is development-candidate evidence, not a claim
about later candidates. No speed ratio is eligible until both real PDFs pass
document-level semantic, font and visual acceptance.

## Shared quantity and original-font closure

A further opt-in track closes the two material simple-invoice acceptance issues
above without changing the original or media-only tracks:

```sh
--template simple --simple-pdf-repair quantity-fonts
```

This selects `shared-currency-media-quantity-font-repair-html-delivery` and includes
the shared media repair. Apply the identical option to both providers. It adds
only these declared changes:

- An output-owned copy of the pinned `GenerateInvoicePdf` source adds
  `->quantity($item->total_duration / 3600)` immediately after `pricePerUnit`.
  The runner rejects an already-loaded original class and loads the retained
  PHP file without `eval`. Before/after bytes, exact patch and hashes are kept.
  Retained patches use minimal hunks; verify them with
  `git apply --check --unidiff-zero` against the pinned source.
  The remaining action code, data query, subtotal calculation and persistence
  stay unchanged.
- Two `@font-face` rules bind the existing `DejaVu Sans` declaration to regular
  400 and bold 700 bytes already bundled by the application's locked
  `dompdf/dompdf` v2.0.4 dependency. There is no replacement family or host-font
  fallback. The runner checks exact font hashes, retains copies, and extracts
  their original copyright/license name-table records. Actual name-table version
  2.37 is recorded; the dependency README's older version prose is not used as
  font identity. Relative font URLs and original bytes are identical for both
  providers; the PHP SDK stages them through its existing API 2 asset input.

The fixture must still show 1 hour at EUR 125 = EUR 125 and 2 hours at EUR 125 =
EUR 250, with EUR 375 subtotal, EUR 75 tax, EUR 450 total. The runner asserts these
action item facts and emits complete row-text and exact embedded-font expectations
for the PDF oracle. It does not change expected amounts to hide the original bug.

This is not a general billing repair: the original query rounds subtotals to
whole currency units and groups descriptions without including rate. Fractional
hour, mixed-rate, discount and broader currency policies remain unqualified. The
authored extra Seller/Buyer line is visually reviewed and retained as expected
appearance, not redesigned.

The first local browser PDF for this track passes its one-page A4, item-row,
total and exact DejaVu font checks, and rendered-page review finds no clipping or
overlap. Actual local stream storage/readback also passes. That establishes a
bounded browser baseline for this repaired fixture, not renderer parity. The
paired early `8938b207...` native candidate still rejects collapsed-table borders;
both runs retain identical HTML and repair/resource provenance. Performance stays
unqualified until a native candidate also produces an accepted PDF, followed by
separate repeated measurements.

## Acceptance boundary

`delivered_pending_pdf_acceptance` means render, local storage/readback, and invoice
database facts passed. It does not mean the document has passed visual/fidelity
acceptance. Run the existing untimed PDF oracle and review page images; the
original simple template's screen-only stylesheet does not apply in Chromium
print. Its largely unstyled output cannot establish intended visual fidelity,
and passing a repaired track does not retroactively qualify the original or
media-only outputs.
The runner's wall time is diagnostic only and begins at the business action,
after fixture/database boot. Do not turn it into an end-to-end speed comparison.

## Focused tests

```sh
PLIEGO_INVOBOOK_REPAIRED_APP=/work/invobook-repaired \
PLIEGO_INVOBOOK_PHP=/path/to/php \
python -m unittest discover -s benchmarks/integration -p 'test_invobook_repaired_workflow.py' -v
```

These opt-in tests execute the real HTML action, assert totals and rollback,
preserve default/elegant failures, check repeatable simple HTML, verify that the
media override changes only its one allowlisted attribute, verify the additional
quantity setter and exact original font bytes/notices, reject repair options for
other templates, and ensure existing evidence is not overwritten. Without
explicit installed paths they report skips,
not application proof. Renderer, storage failure injection and release-package
checks are separate gates.
