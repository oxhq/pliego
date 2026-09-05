# Application PDF compatibility comparison

This is the first 0.4 compatibility census: preserve a real application's source,
exercise its original invoice action, then compare frozen template output through
Pliego API 2 and the application's locked Browsershot provider. It is not the
Linux hosted performance campaign, a complete application migration, or evidence
of independent adoption.

## Pinned application and boundaries

[Invobook](https://github.com/Hasnayeen/invobook/tree/e5f666cef63543beffadfcc045f6af673408a02e)
is pinned to `e5f666cef63543beffadfcc045f6af673408a02e`. Its lock resolves Laravel
11.31.0, LaravelDaily Invoices 4.0.0 and Browsershot 5.0.5. Invobook is MIT-licensed;
keep its license with copied source or redistributed fixture output. The fixture
builder reads the checkout instead of vendoring its templates into Pliego.

Invobook is a concrete invoice baseline, not the large multi-document application
originally considered. The 0.4 invoice/statement/report corpus still needs broader
coverage. Default/simple expect a LaravelDaily invoice; elegant expects the
application's Eloquent invoice plus a Livewire table. Do not make elegant pass by
replacing its table with invented HTML.

The scripts keep separate evidence:

- `probe_invobook.php`: original `GenerateInvoicePdf` action in HTML return mode,
  real migrations and synthetic work sessions in isolated SQLite, each case rolled
  back. Browsershot calls are guarded against execution. This probes the common
  pre-render path; authentication, URL delivery and queues are not exercised.
- `freeze_invobook.php`: unmodified templates rendered with fixed synthetic
  LaravelDaily data and the actual built Vite assets. Outputs include HTML/source
  hashes and data-contract failures. This is template-derived evidence, not a claim
  that the original action works.
- `run_comparison.py`: serial, alternating, cold-process runs; app-locked
  Browsershot versus Pliego API 2. The existing PDF oracle runs untimed. PDF bytes,
  stream-storage/readback hashes, requests, diagnostics and every attempt remain
  available. Expected resources are never removed to manufacture success.
- `reduce_invobook.php`: bounded native-only table/media/font probes for diagnosing
  the original simple-template rejection. These are reductions, never replacement
  compatibility inputs or comparison samples. It stops after a distinguishing pair.

## Prepare

Use a separate checkout and evidence directory. Do not use production credentials,
data, mail, carrier accounts or printers. Install from committed locks; do not update
them to make setup pass.

```sh
git clone https://github.com/Hasnayeen/invobook.git /work/invobook
git -C /work/invobook checkout --detach e5f666cef63543beffadfcc045f6af673408a02e
cd /work/invobook
composer install --no-dev --no-scripts --no-plugins --prefer-dist
npm ci --ignore-scripts
npm run build
```

Use PHP 8.3 or 8.4 for this application lock, not PHP 8.5. On Windows invoke the
chosen PHP executable directly with `composer.phar`; a Composer wrapper may use a
different PHP from PATH. `APP_ENV=benchmark` in the scripts deliberately avoids
Livewire's Mockery-dependent unit-test boot path; storage, mail and queue settings
remain isolated. PHP 8.4 can emit deprecation notices from the original dependencies.

In the Pliego checkout:

```sh
composer --working-dir=sdk/php install --no-scripts --no-plugins
cd benchmarks/adapters/browsershot
PUPPETEER_SKIP_DOWNLOAD=1 npm ci --ignore-scripts
```

The Node lock here supplies Puppeteer 25.8.0; this is an explicitly selected harness
dependency, because Invobook does not lock Puppeteer itself. PHP still loads
**Invobook's Browsershot 5.0.5**, not the separate benchmark adapter's PHP package.
Pass a Chrome executable explicitly. Use a checksum-verified released Pliego
bundle or an explicitly identified candidate. No Rust build is needed for the
released baseline. All four Poppler tools (`pdfinfo`, `pdftotext`, `pdffonts`,
`pdftoppm`) must be on PATH; some bundled distributions contain only two.

## Run from the Pliego checkout

Each output path must be fresh. Earlier failed harness runs remain diagnostics;
do not pool them into renderer compatibility or performance denominators.

```sh
php benchmarks/integration/probe_invobook.php --app /work/invobook --output /evidence/action-probe
php benchmarks/integration/freeze_invobook.php --app /work/invobook --output /evidence/fixtures
python benchmarks/integration/run_comparison.py \
  --fixture /evidence/fixtures/default --fixture /evidence/fixtures/simple \
  --output /evidence/comparison --repeats 3 --timeout-seconds 30 \
  --php /path/to/php \
  --app-autoload /work/invobook/vendor/autoload.php \
  --sdk-autoload sdk/php/vendor/autoload.php \
  --binary /path/to/pliego --chrome /path/to/chrome --node /path/to/node \
  --node-modules benchmarks/adapters/browsershot/node_modules
```

The comparison exits 1 when any attempted output fails qualification, while still
writing `report.json`, `report.md`, and raw attempts. Exit 2 identifies a failed
Node dependency preflight with no renderer samples. The original-action probe
returns success when it completed and retained application failures; inspect its
`status` and cases, not only the process exit code.

Automatic speed ratios are deliberately absent from this census, even when both
providers pass PDF facts. Joint visual/document acceptance must precede performance
qualification; raw diagnostic timings remain available.

Browser file input uses the public `htmlFromFilePath()` API. On Windows, Browsershot
5.0.5 does not propagate `setNodeModulePath()` into its Node subprocess; the adapter
sets process-local `NODE_PATH` without modifying the library or machine settings.
Node dependency verification runs once, outside sample timing.

## Reading results

This is a blocker census. Passing text and page checks does not by itself establish
visual fidelity, row conservation, intended fonts, accessibility or migration
success. Render retained PDFs to images and review them. The simple template's
styles are `media="screen"`; its original Chromium print output is largely
unstyled. Preserve that result instead of silently enabling its CSS.

Process wall time includes PHP startup, renderer/SDK work, local stream copy and
readback hashing. Blade/resource preparation is outside this track. No memory,
CPU, I/O, concurrency-throughput, remote Laravel storage, process-tree containment
or release claim follows from these Windows observations. Three repetitions are
an initial census, not a reliable tail-latency or operational reliability study.

Keep all failures and separate baseline application errors, setup failures,
blocked resources, renderer failures, incorrect PDFs and storage failures. Never
divide a legacy success time by a Pliego failure time. Later performance evidence
must use the exact 0.4 candidate and a jointly accepted corpus, including reviewed
visual output and a separate end-to-end application track.

## Focused checks

```sh
python -m unittest discover -s benchmarks/integration -p 'test_*.py' -v
python benchmarks/tools/test_pdf_oracle.py
php -l benchmarks/integration/compare.php
php -l benchmarks/integration/freeze_invobook.php
php -l benchmarks/integration/probe_invobook.php
php -l benchmarks/integration/reduce_invobook.php
```
