# Invobook shared application-repair track

This experiment preserves the original application baseline and tests the actual
invoice business action after one shared application repair. It is not an
independent-adoption, completed-migration, or performance result.

## Repair and delivery boundary

Use a separate checkout at `e5f666cef63543beffadfcc045f6af673408a02e` and apply
`invobook-currency.patch`: persist the invoice's `currency_code` into the required
`invoice_items.currency` column. The runner rejects any other tracked source
change. `GenerateInvoicePdf`, Blade templates, and dependency locks stay unchanged.
Each run retains the exact patch and source hashes.

The runner invokes `GenerateInvoicePdf` in its existing HTML mode, then delivers
the returned view through either app-locked Browsershot or the PHP `DocumentEngine`.
Both providers use the same original HTML, A4 with zero margins, and Laravel's
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
denies live network access. No resource or media-policy adaptation is performed.

`delivered_pending_pdf_acceptance` means render, local storage/readback, and invoice
database facts passed. It does not mean the document has passed visual/fidelity
acceptance. Run the existing untimed PDF oracle and review page images; the
original simple template's screen-only stylesheet does not apply in Chromium
print. Its largely unstyled output cannot establish intended visual fidelity.
The runner's wall time is diagnostic only and begins at the business action,
after fixture/database boot. Do not turn it into an end-to-end speed comparison.

## Focused tests

```sh
PLIEGO_INVOBOOK_REPAIRED_APP=/work/invobook-repaired \
PLIEGO_INVOBOOK_PHP=/path/to/php \
python -m unittest discover -s benchmarks/integration -p 'test_invobook_repaired_workflow.py' -v
```

These opt-in tests execute the real HTML action, assert totals and rollback,
preserve default/elegant failures, check repeatable simple HTML and ensure existing
evidence is not overwritten. Without explicit installed paths they report skips,
not application proof. Renderer, storage failure injection and release-package
checks are separate gates.
