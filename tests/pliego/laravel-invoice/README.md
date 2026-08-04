# Pliego Laravel invoice fixture

This pinned Laravel 13.22.0 application exercises the Blade-to-Pliego integration
without Node, Chromium, or Java.

```sh
cp .env.example .env
composer install
php artisan pliego:install
php artisan pliego:doctor
composer verify
```

`composer verify` exercises the Laravel download response and reports the retained
PDF, input bundle, scene, and PDF-structure paths. `composer render` runs the
equivalent Artisan render command, and `GET /invoice.pdf` exposes the same download
through a route.

The committed fixture uses local path repositories for source-tree development. A
clean consumer check must install the public Composer packages without those path
repositories before it can count as distribution evidence.
