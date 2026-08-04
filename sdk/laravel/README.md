# oxhq/pliego-laravel

Laravel 13 integration for application-owned Blade documents.

```sh
composer require oxhq/pliego-laravel:^0.1.0-alpha.2 oxhq/pliego-php:^0.1.0-alpha.2
php artisan pliego:install
php artisan pliego:doctor
```

Both Composer constraints are explicit so applications do not need to change their
global minimum stability. `pliego:install` selects the pinned
runtime for Linux x64, Windows x64, or macOS Intel/Apple Silicon, verifies its size
and SHA-256, and installs it under `storage/app/pliego-runtime`.

Set `PLIEGO_RUNTIME_DIR` to move the managed directory. `PLIEGO_BINARY` is an
explicit override for system packages and air-gapped deployments; unset it when
testing managed installation.

## Rendering a Blade view

```php
use Pliego\Laravel\Experimental\Facades\Document;

return Document::view('invoice', ['rows' => $rows])
    ->pageSize('612x792')
    ->margins('36,36,36,36')
    ->locale('es-MX')
    ->timezone('PST8PDT')
    ->denyNetwork()
    ->asset('fonts/invoice.woff2', resource_path('fonts/invoice.woff2'))
    ->download('invoice.pdf');
```

Blade is rendered first. The package creates a private input directory, copies only
declared relative assets, records their hashes, and launches one `pliego render`
process with explicit locale, timezone, page geometry, and resource policy.

For Google Fonts, keep the stylesheet `<link>` in the Blade view and allow both
origins:

```php
$pdf = Document::view('invoice')
    ->allowHttpRoot('https://fonts.googleapis.com/')
    ->allowHttpRoot('https://fonts.gstatic.com/s/')
    ->render();
```

## Failures and retained evidence

Catch `Pliego\Php\Experimental\Exception\RenderException` for typed failures. The
exception preserves the engine code, process exit code, stderr, and retained input
and artifact paths. Failed renders do not publish a final PDF.

Successful jobs are retained for one day and failed jobs for seven days by default.
Preview or apply cleanup with:

```sh
php artisan pliego:prune --dry-run
php artisan pliego:prune
```

See the project [support profile](../../docs/pliego/support-profile.md) for the
current rendering and resource boundaries.
