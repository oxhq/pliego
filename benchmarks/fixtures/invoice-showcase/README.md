# invoice-showcase

Static freeze of the real Laravel invoice showcase
(`tests/pliego/laravel-invoice/resources/views/invoice.blade.php`) rendered to
plain HTML: the same 32-row ledger, `Ahem.ttf` via `@font-face`, the authored
page break after row 16, and the `TOTAL 32 MXN 5280.00` row. `Ahem.ttf` is a
committed copy of `ports/pliego/tests/fixtures/text-scene/Ahem.ttf`
(SHA-256 `b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448`).

Expected (see `manifest.toml`): 2 pages, contains "INVOICE PLG-2026-001",
"TOTAL", and "5280.00".
