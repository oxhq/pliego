# oxhq/pliego-php

Experimental PHP 8.3+ bridge for one Pliego process per document. See the
[alpha support profile](../../docs/pliego/support-profile.md) before production use.

```sh
composer require oxhq/pliego-php:^0.1@alpha
composer test
```

Network is denied unless `RenderOptions::allowedHttpRoots` names explicit HTTP(S)
roots. The engine, packages, URL/font support, and generic fixes are public OSS;
paid work covers private migration and production assurance.

Resource quickstarts:

- [Offline/locked local WOFF2](examples/offline-locked-font.php) copies an explicitly declared font into the input bundle.
- [Allowlisted Google Fonts](examples/google-fonts.php) keeps the public `<link>` unchanged and permits only the stylesheet and font roots.

Both examples print the retained `resources.jsonl` path for resource hashes and
handle typed render failures. The repository self-check uses a fake process and
does not prove live access to Google.

Each result or typed render exception exposes `jobPath`, `inputBundlePath`, and
`artifactsPath`. These may contain private input, PDFs, extracted text, URLs, and
diagnostics; delete the job directory after acceptance when that evidence is no
longer needed.
