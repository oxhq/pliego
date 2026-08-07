# minimal-static

Pure startup fixture: one small static page with no scripts, no declared
fonts, and no images. Measures the fixed cost of `pliego render` — process
spawn, Servo initialization, load, layout, capture, PDF, persistence, publish.

Expected (see `manifest.toml`): 1 page, contains "Minimal".
