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
composer require oxhq/pliego-laravel:^0.1.0-alpha.2 oxhq/pliego-php:^0.1.0-alpha.2
php artisan pliego:install
php artisan pliego:doctor
```

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

The current native release publishes checksummed archives built and API-smoked on
all four targets in the
[package matrix](https://github.com/oxhq/pliego/actions/runs/30855634783). The
[PHP package](https://packagist.org/packages/oxhq/pliego-php) and
[Laravel package](https://packagist.org/packages/oxhq/pliego-laravel) are available
on Packagist and pass focused hosted package checks.

These checks prove the packaged binaries start with the expected engine API and the
Composer distributions pass their focused contracts. The support profile remains
the boundary; Pliego does not claim browser-wide compatibility or safe rendering of
untrusted HTML.

Redistributors should note that the `v0.1.0-alpha.1` native archives include the
root MPL-2.0 license and GitHub provides the exact tagged source, but the archives do
not yet bundle the full dependency and copied-native-library notice set. A
notice-complete archive is needed before redistributing those bundles.

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
