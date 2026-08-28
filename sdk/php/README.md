# oxhq/pliego-php

PHP 8.3+ 64-bit client for controlled, one-document-per-process Pliego rendering.
The current stable package is 0.3.2; new integrations should use `DocumentEngine` and API 2.

```sh
composer require oxhq/pliego-php:^0.3.2
```

## API 2 rendering

`DocumentEngine` probes the executable, selects the exact public API 2 tuple, stages a canonical
input closure, and validates the returned PDF, DocumentScene, bundle manifest, diagnostics, and
their real bytes before returning.

```php
use Pliego\Php\DocumentEngine;
use Pliego\Php\InputAsset;
use Pliego\Php\RenderOptions;

$engine = new DocumentEngine(
    command: ['/opt/pliego/bin/pliego'],
    workDirectory: '/var/lib/my-app/pliego',
);

$result = $engine->render(
    html: '<!doctype html><link rel="stylesheet" href="assets/invoice.css"><h1>Invoice</h1>',
    options: new RenderOptions(locale: 'en-US', timezone: 'UTC'),
    assets: [
        new InputAsset(
            path: 'assets/invoice.css',
            sourcePath: '/srv/my-app/resources/invoice.css',
            mediaType: 'text/css;charset=utf-8',
        ),
    ],
);

file_put_contents('/srv/my-app/storage/invoice.pdf', $result->bytes());
```

The 0.3.2 client negotiates only Pliego's profile-null API 2 tuple. It requests no semantic or
accessibility conformance profile and makes no PDF/UA claim. API 2 always denies live network access
and host-font discovery. Fetch remote resources before the render, stage them as local files, and
pass their paths through `InputAsset` entries in `assets`. `allowedHttpRoots` remains only for the
deprecated API 1 compatibility client and is rejected by `DocumentEngine` with a migration message.

The advertised v0.3.2 API 2 surface excludes link annotations, collapsed-table-border capture, and
the current upstream Chart.js 4.5.1 fixture. Unsupported scene operations fail closed and do not
deliver a partial PDF. See the support profile below for the exact rendering boundary. This package
makes no comparative performance claim.

The default page remains the previous 816 by 1056 CSS-pixel Letter surface with 48-pixel margins;
the SDK converts it to exact 60-app-unit geometry. `pageSize: 'A4'` selects named A4. The canonical
timezones are `UTC` and `America/Tijuana`; `PST8PDT` is accepted as a deprecated input alias and
normalized to `America/Tijuana` in the request.

Successful results expose:

- `pdfPath`, `scenePath`, `bundlePath`, and `deliveryPath`;
- `jobPath`, the SDK-owned retained job containing `.pliego-status`;
- `runtimeJobPath`, the isolated native `cwd-v1` directory;
- `inputPath` and `diagnosticsPath`;
- the validated terminal result in `metadata`; and
- `deliveryIdentity`, the SHA-256 descriptor of canonical `bundle.json`.

`inputBundlePath` and `artifactsPath` remain compatibility aliases for `inputPath` and
`diagnosticsPath`. A caller can copy or stream `pdfPath` into durable application storage and later
prune the private render job independently.

The SDK creates `runtimeJobPath` with exact Unix mode `0700`. On Windows it fails closed unless the
standard `%SystemRoot%\\System32\\whoami.exe` and `icacls.exe` tools can establish and verify one
protected, current-user-only DACL before any input is staged; the tools are invoked directly, never
through a command shell.

Accepted render failures throw `RenderFailedException` with a stable failure `kind` and validated
diagnostic inventory. Exit 64 throws `InvocationException`. The advertised exit 74 boundary throws
`TransportException` with its validated one-line engine diagnostic while retaining any unusable
partial stdout for evidence. Other process, framing, timeout, identity, tampering, and closure
failures also throw `TransportException`; an unavailable exact tuple throws
`UnsupportedContractException`. All invoked failures retain `jobPath` evidence.

## API 1 compatibility

`CliRenderer` is deprecated since 0.3.0 and remains available only as an explicit 0.3.x migration
boundary. It continues to invoke `pliego render` and preserve its existing `RenderResult` and
exception behavior. New integrations and framework bindings should use `DocumentEngine`; API 1 is
not the default.

See the project [support profile](https://github.com/oxhq/pliego/blob/main/docs/pliego/support-profile.md)
for the current rendering and resource boundaries.
