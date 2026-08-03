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

Migrating a production document? Request a fixed-scope private preflight. All
generic renderer and package capabilities remain public OSS.
