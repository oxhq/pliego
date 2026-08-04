# oxhq/pliego-php

PHP 8.3+ client for rendering one document per native Pliego process.

```sh
composer require oxhq/pliego-php:^0.1.0
composer test
```

Network access is denied unless `RenderOptions::allowedHttpRoots` names explicit
HTTP(S) roots. Local files must live inside the document input bundle. Host-font
fallback and redirects remain disabled by default.

## Rendering profile

Static HTML needs no readiness calls. Pliego infers readiness after page load and
waits for `document.fonts.ready`. Call `window.pliego.defer()` only when JavaScript
continues changing the DOM or a canvas after load, followed by `ready()` after the
final change or `fail()` on error.

Chart.js 4.5.1 is covered for fixed, non-animated charts that complete a synchronous
full-canvas `getImageData(0, 0, canvas.width, canvas.height)` readback before
`ready()`. The retained pixels are authoritative for that canvas; the claim does
not extend to every Chart.js configuration or Canvas API.

PDF paint currently retains resolved sRGB text colors, solid backgrounds,
uniform-color sharp axis-aligned solid borders, and uniform solid collapsed-table
borders. CSS gradients and background-image layers, shadows, rounded or mixed-color
borders, clips, non-solid and image borders, transforms, opacity, filters, and blend
modes are explicitly unsupported and reported rather than approximated.

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
