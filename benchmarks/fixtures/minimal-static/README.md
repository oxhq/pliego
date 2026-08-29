# minimal-static

Pure startup fixture: one small static page with one bundled Ahem font and no
scripts or images. Measures the fixed cost of one native API 2 process — process
spawn, Servo initialization, load, layout, capture, PDF, persistence, publish.
`Ahem.ttf` is byte-identical to the existing benchmark asset (SHA-256
`b719ecb31c5b21fc573c03f6421c74ac63c271a5a3ff841e34f9705fb94b8448`).

Expected (see `manifest.toml`): one A4 page, exact normalized document text,
one embedded Ahem font, and a retained normalized raster signature. Link
annotations are deliberately absent because they are outside the advertised
v0.3.3 API 2 profile. Every heading and paragraph uses normal-weight Ahem so a
target cannot pass by substituting its default bold heading face.

The comparator uses `comparator.html`. The pre-existing `input.html` remains the
engine regression fixture, including its link operation and retained scene oracle;
benchmark work must not rewrite that independent proof.
