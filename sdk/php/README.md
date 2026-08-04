# oxhq/pliego-php

PHP 8.3+ client for rendering one document per native Pliego process.

```sh
composer require oxhq/pliego-php:^0.1.0-alpha.3
composer test
```

Network access is denied unless `RenderOptions::allowedHttpRoots` names explicit
HTTP(S) roots. Local files must live inside the document input bundle. Host-font
fallback and redirects remain disabled by default.

Resource examples:

- [Offline local WOFF2](examples/offline-locked-font.php) copies a declared font
  into the rooted input bundle.
- [Allowlisted Google Fonts](examples/google-fonts.php) keeps the public `<link>`
  unchanged and permits both the stylesheet and font roots.

Results and typed render exceptions expose `jobPath`, `inputBundlePath`, and
`artifactsPath`. These directories may contain private input, PDFs, extracted text,
URLs, and diagnostics; remove them after acceptance when the evidence is no longer
needed.

See the project [support profile](../../docs/pliego/support-profile.md) for the
current rendering and resource boundaries.
