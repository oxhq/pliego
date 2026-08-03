# Experimental Laravel CLI bridge

Status: experimental M4 revenue-wedge proof, introduced by OXH-272.

The release contract, including offline/locked and live/allowlisted resource modes,
is the [Pliego 0.1 alpha support profile](./support-profile.md).

This bridge renders one Blade view through one isolated Pliego process. It is
not the M8 daemon protocol and its PHP API may change. The generic engine and
layout work remains MPL-2.0; the PHP packages are MIT.

## Pinned proof application

`tests/pliego/laravel-invoice` pins Laravel 13.22.0 with a PHP 8.3-compatible
lockfile and the two local experimental Composer packages. It has no Node,
Chromium, or Java dependency.

```sh
cd tests/pliego/laravel-invoice
cp .env.example .env
composer install
# Source-tree verification may override the not-yet-published runtime.
PLIEGO_BINARY=/absolute/path/to/pliego composer verify
```

The command exercises the Laravel download response and prints the retained
PDF, rooted input bundle, scene, and PDF-structure report. `composer render`
exercises the equivalent Artisan command. The application also exposes `GET
/invoice.pdf`.

## API

```php
use Pliego\Laravel\Experimental\Facades\Document;

return Document::view('invoice', ['rows' => $rows])
    ->pageSize('612x792')
    ->margins('36,36,36,36')
    ->locale('es-MX')
    ->timezone('PST8PDT')
    ->denyNetwork()
    ->asset('assets/Ahem.ttf', resource_path('fonts/Ahem.ttf'))
    ->download('invoice.pdf');
```

Blade is rendered first. The bridge then creates a private input directory,
writes `document.html`, copies only declared relative assets, writes a
content-hashed `input-bundle.json`, and launches `pliego render` with that
directory as its working root. Locale, timezone, page geometry, and network
roots are always passed explicitly. Network access is denied unless an
`allowHttpRoot()` call adds a root.

Engine failures retain their Pliego error code, exit code, and stderr as either
`InvalidRequestException` or `EngineRenderException`. Successful results retain
the input bundle and all Pliego artifacts under the configured work directory.

## Native runtime installation

The public package manages the native executable separately from render jobs:

```sh
composer require oxhq/pliego-laravel:^0.1.0-alpha.2 oxhq/pliego-php:^0.1.0-alpha.2
php artisan pliego:install
php artisan pliego:doctor
```

The installer selects one package-pinned GitHub release archive for the current
OS and CPU, verifies the byte count and SHA-256 recorded in the Composer
package, extracts only its declared files, and checks the exact engine and API
versions before publishing it atomically under `storage/app/pliego-runtime`.
Repeated installation is idempotent. It never downloads during a render or
silently selects `latest`. `PLIEGO_BINARY` remains an explicit deployment
override.

## Migration boundary

- DOMPDF: replace `Pdf::loadView(...)->download(...)` with the fluent call
  above and declare every font/image asset copied into the rooted bundle.
- Browsershot: remove browser/Node setup, replace URL or arbitrary network
  loading with bundled assets or explicit HTTP roots, and keep the same Blade
  view.

This slice intentionally omits daemon reuse, browser fallback, stable SDK
compatibility, and worker lifecycle management. Add those only with the M8
protocol.

The fixed-scope M4 commercial validation boundary is documented in the
[production-document design-partner offer](./design-partner-offer.md). Its
[redacted validation ledger](./design-partner-validation-ledger.md) separates
paid evidence from interest and does not authorize outreach.
