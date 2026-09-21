# unsupported-paint

Measures and verifies the API 2 fail-closed path. The document uses a CSS gradient,
a box shadow, and a rounded border, all explicitly outside the current support
profile. The v0.3.3 API 2 render must fail with `SCENE_ENCODING_FAILED` and must
**not** publish a partial PDF.

Expected (see `manifest.toml`): typed failure, `pdf_published = false`.
