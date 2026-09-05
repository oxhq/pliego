# Invobook fixture notices

Invobook source is attributed to Nehal Hasnayeen and its contributors, at
commit `e5f666cef63543beffadfcc045f6af673408a02e` of
https://github.com/Hasnayeen/invobook. Its original README declares MIT and
links to the MIT license, but that pinned tree contains no LICENSE file.
The complete original declaration and author attribution are retained in
`Invobook-MIT-declaration.README.md`; no missing copyright statement is invented.

The simple invoice template is derived from LaravelDaily/laravel-invoices
4.0.0 at `d9fa7eca22a7836fb9b09ee7fc2a97b4dfd7b228`, whose package manifest
declares **GPL-3.0-only** and identifies David Lun as author. Its complete
license is `LaravelDaily-GPL-3.0-only.txt`. The original and repaired Blade
sources, template repair patch, and rendered template-derived HTML are retained
as that isolated third-party fixture, not described as MIT or Pliego engine code.
Upstream: https://github.com/LaravelDaily/laravel-invoices

The two DejaVu Sans 2.37 font files are the exact original files bundled by
Invobook's dompdf 2.0.4 dependency. Each `.ttf.LICENSE.txt` retains copyright and
license information extracted from that font's name-table records 0 and 13.
Those font notices remain separate from either application's software license.

Fixture data is synthetic. The retained patches record the shared application
repairs; no original failure is reclassified as success. These are source-only
benchmark fixtures, not runtime/SDK package contents or an engine relicensing.
