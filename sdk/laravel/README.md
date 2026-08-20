# oxhq/pliego-laravel

Laravel 13 integration for application-owned Blade documents.

The v0.2.0 Laravel package is published on Packagist:

```sh
composer require oxhq/pliego-laravel:^0.2.0
php artisan pliego:install
php artisan pliego:doctor
```

Ubuntu 22.04 x86_64 needs `ca-certificates`, `libfontconfig1`, `libegl1`, and
`libgl1-mesa-dri`. Headless containers also need a writable mode-0700
`XDG_RUNTIME_DIR`; no display server or Xvfb is required.
Windows x64 requires the latest
[Microsoft Visual C++ v14 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170).
macOS Intel and Apple Silicon bundles require macOS 13 or newer. The Intel
binary is unsigned and Apple Silicon is ad-hoc signed; neither is Developer ID
signed or notarized.

The Laravel package installs the PHP bridge as its dependency. `pliego:install`
selects the pinned runtime for Linux x64, Windows x64, or macOS Intel/Apple
Silicon, verifies its size and SHA-256, and installs it under
`storage/app/pliego-runtime`.

Managed installation accepts only finalized package metadata and verifies the
package-pinned archive size, SHA-256, and file inventory. An unfinalized package
fails before download. `PLIEGO_BINARY` remains an explicit override for a reviewed
system or air-gapped installation.

Set `PLIEGO_RUNTIME_DIR` to move the managed directory. `PLIEGO_BINARY` is an
explicit override for system packages and air-gapped deployments; unset it when
testing managed installation.

## Rendering a Blade view

The default render path is:

```php
use Pliego\Laravel\Facades\Document;

return Document::view('invoice', compact('rows'))->download();
```

Add locale, resource policy, and local assets only when the view needs them:

```php
use Pliego\Laravel\Facades\Document;

return Document::view('invoice', ['rows' => $rows])
    ->locale('es-MX')
    ->timezone('PST8PDT')
    ->denyNetwork()
    ->asset('fonts/invoice.woff2', resource_path('fonts/invoice.woff2'))
    ->download('invoice.pdf');
```

Static Blade views need no readiness calls. Pliego infers readiness after page load
and waits for `document.fonts.ready`. Call `defer()` only when JavaScript continues
changing the document or a canvas after load, then finish with `ready()` or
`fail()`:

```html
<script>
window.pliego?.defer();
loadReportData()
    .then(drawReport)
    .then(() => window.pliego?.ready())
    .catch(error => window.pliego?.fail(error.message));
</script>
```

Chart.js 4.5.1 is covered for a fixed, non-animated chart that performs a
synchronous full-canvas `getImageData(0, 0, canvas.width, canvas.height)` readback
after its final draw and before `ready()`. The retained pixels become the
authoritative canvas result; other versions, modes, plugins, and Canvas APIs are not
implied.

`render()` and `download()` reject partial scene capture instead of returning a PDF
with unsupported paint omitted. Retained artifacts remain available on the typed
exception.

PDF paint retains resolved sRGB text colors, solid backgrounds, uniform-color sharp
axis-aligned solid borders, and uniform solid collapsed-table borders. CSS
gradients and background-image layers, box and text shadows, text decorations,
rounded or mixed-color borders, clips, non-solid and image borders, transforms,
opacity, filters, and blend modes are explicitly unsupported and reported rather
than approximated.

Blade is rendered first. The package creates a private input directory, copies only
declared relative assets, records their hashes, and launches one `pliego render`
process with explicit locale, timezone, page geometry, and resource policy.
`download()` returns a Laravel file response; `render()` returns the PDF,
input-bundle, and retained-artifact paths.

For Google Fonts, keep the stylesheet `<link>` in the Blade view and allow both
origins:

```php
$pdf = Document::view('invoice')
    ->allowHttpRoot('https://fonts.googleapis.com/')
    ->allowHttpRoot('https://fonts.gstatic.com/s/')
    ->render();
```

## Failures and retained evidence

Catch `Pliego\Php\Exception\RenderException` for typed failures. The
exception preserves the engine code, process exit code, stderr, and retained input
and artifact paths. Failed renders do not publish a final PDF.

On the v0.2 API 1 runtime, the exception's artifact path is a requested locator, not
an existence guarantee. Deterministic publication preflight failures create no
public artifact tree and leave an already-existing output unchanged. Check
`is_dir($error->artifactsPath)` before reading diagnostics; validated engine failure
evidence remains available when it can be promoted atomically.

Successful jobs are retained for one day and failed jobs for seven days by default.
Preview or apply cleanup with:

```sh
php artisan pliego:prune --dry-run
php artisan pliego:prune
```

See the project [support profile](../../docs/pliego/support-profile.md) for the
current rendering and resource boundaries.
