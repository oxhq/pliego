# Pliego Laravel invoice fixture

This is a pinned Laravel 13.22.0 application with a PHP 8.3-compatible lockfile
and no Node, Chromium, or Java dependency. It exercises the experimental
one-shot Pliego bridge.

```sh
cp .env.example .env
composer install
php artisan pliego:install
php artisan pliego:doctor
composer verify
```

The verification command exercises the Laravel download response and prints
JSON paths for the retained PDF, input bundle, scene, and PDF-structure report.
`composer render` exercises the equivalent Artisan render command, and `GET
/invoice.pdf` exposes the same download through a route.

`composer rehearse:self-test` checks the exact six-job order and outcome
contract without starting Pliego. The production-only `composer rehearse`
command requires Linux, one durable queue connection, public
`0.1.0-alpha.2` Composer distributions, the published runtime checksum, one
local WOFF2, and exact CSS/WOFF2 URLs and hashes from a running two-origin
fixture. It drains a unique queue with one worker and retains one manifest with
job identities, durations, peak RSS, process-leak evidence, disk usage, and
pruning proof. Use `php artisan pliego:rehearse-queue --help` for its focused
arguments.

The committed fixture still uses path repositories for source development, so
it cannot produce release acceptance evidence. Run the production command only
from the clean public Packagist installation built for OXH-286.
