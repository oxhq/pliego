# oxhq/pliego-laravel

Experimental Laravel 13 bridge for application-owned Blade documents. See the
[alpha support profile](../../docs/pliego/support-profile.md) and
[CLI bridge guide](../../docs/pliego/laravel-cli-bridge.md).

```sh
composer require oxhq/pliego-laravel:^0.1@alpha
```

Offline assets are the reproducible default. Live URLs are opt-in with
`allowHttpRoot()`; a Google Fonts stylesheet needs explicit roots for both
`fonts.googleapis.com` and `fonts.gstatic.com/s/`.

Completed jobs are retained for one day and failed jobs for seven days by
default. Their inputs, PDFs, extracted text, URLs, and diagnostics may contain
private data. Preview or apply cleanup with:

```sh
php artisan pliego:prune --dry-run
php artisan pliego:prune
```

Set both `PLIEGO_SUCCESS_RETENTION_SECONDS=0` and
`PLIEGO_FAILURE_RETENTION_SECONDS=0`, then prune, to delete completed evidence
immediately after acceptance.

Migrating a production document? Request a fixed-scope private preflight. All
generic renderer and package capabilities remain public OSS.
