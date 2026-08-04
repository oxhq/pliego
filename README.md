# Pliego

Pliego is an open-source native HTML-to-PDF engine built on Servo for
application-owned invoices, statements, and operational reports. It turns HTML and
CSS into paginated PDFs without Chromium, Node.js, or Java in the runtime.

Pliego focuses on predictable document workflows:

- authored page breaks, paged tables, repeated headers, and row constraints;
- selectable text, links, and embedded TTF, OTF, WOFF, and WOFF2 fonts;
- network-denied rendering by default, with explicit URL allowlists for remote
  stylesheets, images, and fonts;
- typed failures and retained input, resource, scene, PDF, and diagnostic artifacts;
  and
- native bundles for Linux x86_64, Windows x86_64, macOS x86_64, and macOS arm64.

## Laravel quick start

```sh
composer require oxhq/pliego-laravel:^0.1.0-alpha.4 oxhq/pliego-php:^0.1.0-alpha.3
php artisan pliego:install
php artisan pliego:doctor
```

Render a Blade view from a controller:

```php
use Pliego\Laravel\Experimental\Facades\Document;

return Document::view('invoices.show', ['invoice' => $invoice])
    ->pageSize('612x792')
    ->margins('36,36,36,36')
    ->locale('es-MX')
    ->timezone('PST8PDT')
    ->denyNetwork()
    ->asset('fonts/invoice.woff2', resource_path('fonts/invoice.woff2'))
    ->download('invoice.pdf');
```

Signal readiness from the Blade view after its fonts finish loading:

```html
<script>
document.fonts.ready.then(() => window.pliego?.ready());
</script>
```

`download()` returns a Laravel file response. Use `render()` instead to receive a
result with the PDF, input-bundle, and retained-artifact paths.

Ubuntu 22.04 x86_64 needs `ca-certificates`, `libfontconfig1`, `libegl1`, and
`libgl1-mesa-dri`. Headless containers also need a writable mode-0700
`XDG_RUNTIME_DIR`; no display server or Xvfb is required.
Windows x64 requires the latest
[Microsoft Visual C++ v14 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170).
macOS Intel and Apple Silicon bundles require macOS 13 or newer. The Intel
binary is unsigned and Apple Silicon is ad-hoc signed; neither is Developer ID
signed or notarized.

`pliego:install` downloads the package-pinned runtime for the current platform and
verifies its size and SHA-256 before installation. `pliego:doctor` checks the engine
API, writable storage, bundled font, and an offline PDF render.

Network access remains opt-in. A Google Fonts stylesheet needs explicit roots for
both `https://fonts.googleapis.com/` and `https://fonts.gstatic.com/s/`.

See the [Laravel package guide](sdk/laravel/README.md) for Blade rendering, local
assets, controlled URLs, typed failures, and artifact retention.

## Native CLI

Download a bundle from [Releases](https://github.com/oxhq/pliego/releases), verify
its adjacent SHA-256 file, and run:

```sh
pliego render document.html --output document.pdf --artifacts artifacts
```

Host-font fallback, network access, redirects, and asset caching are disabled by
default. The [support profile](docs/pliego/support-profile.md) defines the current
capability, resource, and failure boundaries.

## Release evidence

The `v0.1.0-alpha.2` native bundles are built and API-smoked on
all four targets in the
[package matrix](https://github.com/oxhq/pliego/actions/runs/30874336010). The
[PHP package](https://packagist.org/packages/oxhq/pliego-php) and
[Laravel package](https://packagist.org/packages/oxhq/pliego-laravel) are available
on Packagist and pass focused hosted package checks.

These checks prove the packaged binaries start with the expected engine API and the
Composer distributions pass their focused contracts. The support profile remains
the boundary; Pliego does not claim browser-wide compatibility or safe rendering of
untrusted HTML.

Every native archive includes the project and specification licenses, an exact tagged
source pointer, the generated Cargo dependency report, and pinned notices for copied
or linked native code. Windows archives additionally inventory their ANGLE DLLs and
the exact mozangle, Chromium, Khronos/Vulkan, Bison, and zlib notices they require.

## Building from source

```sh
./mach bootstrap
cargo build -p pliego --locked --profile checked-release
```

On Windows, run `./mach` as `./mach.bat` or `python mach` from a configured Servo
build environment.

## Servo relationship

Pliego preserves Servo's source layout so upstream security and web-platform fixes
can be reviewed without rewriting the fork. The `upstream-main` branch mirrors
Servo `main`; temporary `sync/servo-YYYY-MM-DD` branches carry reviewed updates into
Pliego. Servo build documentation remains available in the
[Servo Book](https://book.servo.org/).

## License and contributing

The engine is MPL-2.0. The PHP and Laravel packages are MIT-licensed. See
[CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and the
[third-party notices](docs/pliego/third-party-notices.md).
