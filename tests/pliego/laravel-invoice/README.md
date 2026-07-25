# Pliego Laravel invoice fixture

This is a pinned Laravel 13.22.0 application with a PHP 8.3-compatible lockfile
and no Node, Chromium, or Java dependency. It exercises the experimental
one-shot Pliego bridge.

```sh
cp .env.example .env
composer install
# Set PLIEGO_BINARY to an absolute checked-release Pliego binary.
composer verify
```

The verification command exercises the Laravel download response and prints
JSON paths for the retained PDF, input bundle, scene, and PDF-structure report.
`composer render` exercises the equivalent Artisan render command, and `GET
/invoice.pdf` exposes the same download through a route.
