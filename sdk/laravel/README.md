# oxhq/pliego-laravel

Laravel 13 integration for application-owned Blade documents.

The v0.3 package uses Pliego API 2 and pins `oxhq/pliego-php` 0.3.2:

```sh
composer require oxhq/pliego-laravel:^0.3.2
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

The Laravel package installs the API 2 PHP client as its dependency. `pliego:install`
selects the pinned runtime for Linux x64, Windows x64, or macOS Intel/Apple
Silicon, verifies its size and SHA-256, and installs it under
`storage/app/pliego-runtime`.

Managed installation accepts only finalized package metadata and verifies the
package-pinned archive size, SHA-256, and file inventory. An unfinalized package
fails before download. Set `PLIEGO_RUNTIME_DIR` to move the managed directory.
`PLIEGO_BINARY` remains an explicit override for a reviewed system or air-gapped
installation; unset it when testing managed installation.

## Rendering a Blade view

The default render path is:

```php
use Pliego\Laravel\Facades\Document;

return Document::view('invoice', compact('rows'))->download();
```

Add locale, timezone, and reviewed local assets only when the view needs them:

```php
use Pliego\Laravel\Facades\Document;

return Document::view('invoice', ['rows' => $rows])
    ->locale('es-MX')
    ->timezone('America/Tijuana')
    ->denyNetwork()
    ->asset('fonts/invoice.woff2', resource_path('fonts/invoice.woff2'))
    ->download('invoice.pdf');
```

Use Laravel Storage when the PDF must outlive Pliego's prunable render job:

```php
$stored = Document::view('invoice', ['rows' => $rows])->store(
    path: 'invoices/42.pdf',
    disk: 'private',
    options: ['visibility' => 'private'],
);

$stored->disk;         // private
$stored->path;         // invoices/42.pdf
$stored->renderResult; // retained Pliego render and diagnostic paths
```

`store()` renders once and passes an open PDF stream to the selected Laravel
filesystem disk. Omitting `disk` uses the application's configured default disk.
The render job is retained under the normal success/failure retention policy; it
is not deleted after the durable write.

Retrieve or download the document through the same Laravel disk:

```php
use Illuminate\Support\Facades\Storage;

return Storage::disk($stored->disk)->download(
    $stored->path,
    'invoice.pdf',
);
```

The API 2 engine owns its private job, input, diagnostics, and delivery paths.
Laravel uses the requested filename only for the HTTP download name; the retained
delivery is always `delivery/document.pdf`.

Omit `disk` to use `filesystems.default`, including a local disk. After installing
Laravel's S3 adapter, pass `s3` for that disk. MinIO, Cloudflare R2, and other
S3-compatible services work through the normal Laravel `endpoint` and
`use_path_style_endpoint` disk settings; Pliego does not read cloud credentials or
bypass Laravel's filesystem adapter.

Queue scalar document IDs, the destination path, and disk name, then resolve and
render the document inside the job's `handle()` method. Do not serialize a
`PendingDocument`, `RenderResult`, or open stream into the queue payload:

```php
use Illuminate\Contracts\Queue\ShouldQueue;
use Pliego\Laravel\DocumentFactory;

final readonly class StoreInvoicePdf implements ShouldQueue
{
    public function __construct(public int $invoiceId) {}

    public function handle(DocumentFactory $documents): void
    {
        $invoice = Invoice::findOrFail($this->invoiceId);

        $documents->view('invoice', compact('invoice'))->store(
            path: "invoices/{$invoice->id}.pdf",
            disk: 'private',
        );
    }
}
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

The broader controlled-capture regression corpus contains a fixed, non-animated
Chart.js 4.5.1 fixture with a synchronous full-canvas readback. That fixture has not
passed the narrower v0.3.2 API 2 scene-encoding gate, so Chart.js is not advertised
as a current API 2 package capability.

`render()` and `download()` reject partial scene capture instead of returning a PDF
with unsupported paint omitted. Retained artifacts remain available on the typed
exception.

PDF paint retains resolved sRGB text colors, solid backgrounds, and uniform-color
sharp axis-aligned solid borders. In v0.3.2 API 2, an unsupported
`collapsed-table-borders` capture event fails closed; use separated table borders.
Link annotations are also outside the advertised v0.3.2 API 2 profile. CSS
gradients and background-image layers, box and text shadows, text decorations,
rounded or mixed-color borders, clips, non-solid and image borders, transforms,
opacity, filters, and blend modes are explicitly unsupported and reported rather
than approximated.

Blade is rendered first. The API 2 PHP client creates the private cwd-v1 job,
copies only declared relative assets, records their hashes in the canonical input
manifest, negotiates the exact public contract, and launches one
`pliego render-api2` process. `download()` returns a Laravel file response;
`render()` returns the retained PDF, scene v2, bundle, input, diagnostics, and
job paths.

API 2 profile-null denies live network and host-font discovery. Prefetch every
stylesheet, font, image, or script and pass it with `asset()`. The legacy
`allowHttpRoot()` method is deprecated and throws an actionable exception; it
never silently ignores the requested origin.

## Failures and retained evidence

Catch `Pliego\Php\Exception\RenderFailedException` when an accepted API 2
request produces a validated failed result. It preserves the stable error kind,
canonical result, retained job, runtime and diagnostics paths, and bridge timings.
`InvocationException` identifies a rejected invocation; `TransportException`
identifies process, framing, or artifact-integrity failure. Failed renders never
publish a delivery PDF.

Catch `Pliego\Laravel\Exception\DocumentStorageException` when rendering succeeds
but durable storage fails. It preserves the requested disk and path, the original
`RenderResult`, and the filesystem exception as its previous error.

Successful jobs are retained for one day and failed jobs for seven days by default.
Preview or apply cleanup with:

```sh
php artisan pliego:prune --dry-run
php artisan pliego:prune
```
