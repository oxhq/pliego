# unsupported-paint

Measures and verifies the fail-closed path. The document uses a CSS gradient,
a box shadow, and a rounded border — all explicitly outside the Pliego 0.1
support profile. The default render must fail with
`SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS` (capture `status: partial`) and must
**not** publish a partial PDF.

Expected (see `manifest.toml`): typed failure, `pdf_published = false`.
