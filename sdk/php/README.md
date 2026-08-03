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

Each result or typed render exception exposes `jobPath`, `inputBundlePath`, and
`artifactsPath`. These may contain private input, PDFs, extracted text, URLs, and
diagnostics; delete the job directory after acceptance when that evidence is no
longer needed.
