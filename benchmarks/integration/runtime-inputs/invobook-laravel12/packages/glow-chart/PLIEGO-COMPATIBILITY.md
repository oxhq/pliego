# Local Laravel 12 compatibility fork

This is not an upstream stable release. Invobook's explicit local path package
version is `dev-pliego-laravel12`, with mirroring rather than symlinking.

Upstream source is Hasnayeen/glow-chart commit
`f33530c231116a8f4eb3957a779d5b6012ba195d`, the original Invobook lock identity:
https://github.com/Hasnayeen/glow-chart/tree/f33530c231116a8f4eb3957a779d5b6012ba195d

The compatibility patch raises the PHP floor to 8.2, allows Illuminate 12, and
uses the stable laravel-trend 0.5 line, which declares Laravel 12 compatibility.
Runtime source, public namespaces, Filament 3 integration, compiled JavaScript,
and MIT license are retained. No widgets are removed. The application-owned
`App\Support\Trend` is distinct from the dependency and must be tested separately.

Source inspection alone does not establish chart, browser, PDF or security proof.
The enclosing application's lock and dated local evidence record the actually
resolved versions, audit and compatibility-test results.
