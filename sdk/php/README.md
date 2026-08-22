# oxhq/pliego-php

PHP 8.3+ 64-bit client for deterministic, one-document-per-process Pliego rendering.

```sh
composer require oxhq/pliego-php:^0.3.0
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

API 2 always denies live network access and host-font discovery. Fetch remote resources before the
render and supply their bytes through `assets`. `allowedHttpRoots` remains only for the deprecated
API 1 compatibility client and is rejected by `DocumentEngine` with a migration message.

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

Accepted render failures throw `RenderFailedException` with a stable failure `kind` and validated
diagnostic inventory. Exit 64 throws `InvocationException`. Framing, timeout, identity, tampering,
and closure failures throw `TransportException`; an unavailable exact tuple throws
`UnsupportedContractException`. All invoked failures retain `jobPath` evidence.

## API 1 compatibility

`CliRenderer` is deprecated in 0.3.0 but remains available explicitly for one migration release. It
continues to invoke `pliego render` and preserve its existing `RenderResult` and exception behavior.
New integrations and framework bindings should use `DocumentEngine`; API 1 is not the default.

See the project [support profile](https://github.com/oxhq/pliego/blob/main/docs/pliego/support-profile.md)
for the current rendering and resource boundaries.
