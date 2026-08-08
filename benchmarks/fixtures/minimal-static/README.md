# minimal-static

Pure startup fixture: one small static page with one bundled Ahem font and no
scripts or images. Measures the fixed cost of `pliego render` — process
spawn, Servo initialization, load, layout, capture, PDF, persistence, publish.
`Ahem.ttf` is byte-identical to the existing benchmark asset (SHA-256
`b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448`).

Expected (see `manifest.toml`): 1 page, contains "Minimal".
