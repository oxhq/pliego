# font-image-heavy

Font embedding, image decode, and resource I/O. The `input.html` is generated
deterministically by `benchmarks/tools/generate_fixtures.py`: six forced pages
with 48 unique 320x180 inline PNG charts (stdlib only) plus `Ahem.ttf` via
`@font-face`. `Ahem.ttf` is a committed copy of
`ports/pliego/tests/fixtures/text-scene/Ahem.ttf`
(SHA-256 `b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448`).

Expected (see `manifest.toml`): 6 pages. The exact count is an estimate and is
pinned to the measured value by the first accepted baseline.
