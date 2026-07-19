# Pliego third-party notices

This inventory supplements the generated Cargo dependency-license report described in
[ADR 0012](adr/0012-license-and-notice-strategy.md). A Pliego distribution containing the PDF
backend must include this file and the corresponding full license texts. Links below are pinned to
the exact Krilla upstream revision published as version 0.8.2 unless noted otherwise.

## Krilla 0.8.2

- Component: `krilla` 0.8.2, used by Pliego's `DocumentScene` PDF adapter with default features
  disabled and the `raster-images` feature enabled.
- Source: [LaurenzV/krilla at `3ffdf0588cf98050aad6edba51ca70162e1fb5b5`](https://github.com/LaurenzV/krilla/tree/3ffdf0588cf98050aad6edba51ca70162e1fb5b5).
- License expression: `MIT OR Apache-2.0`.
- Copyright: Copyright (c) 2024 Laurenz Stampfl.
- License texts: [MIT](https://github.com/LaurenzV/krilla/blob/3ffdf0588cf98050aad6edba51ca70162e1fb5b5/LICENSE_MIT) and [Apache-2.0](https://github.com/LaurenzV/krilla/blob/3ffdf0588cf98050aad6edba51ca70162e1fb5b5/LICENSE_APACHE).
- Upstream acknowledgements: [Krilla `NOTICE.md`](https://github.com/LaurenzV/krilla/blob/3ffdf0588cf98050aad6edba51ca70162e1fb5b5/NOTICE.md).

Krilla's upstream notice records code copied or adapted from these projects:

| Source | License cited in Krilla 0.8.2 notice | Material identified by Krilla |
| --- | --- | --- |
| [resvg](https://github.com/RazrFalcon/resvg) | MPL-2.0 | The contents of `content_draw_path` and the resvg test suite in `assets/svgs`. |
| [Typst](https://github.com/typst/typst) | Apache-2.0 | `GroupByKey`, `SliceExt`, `Prehashed`, `SipHashable`, CID-keyed font writing, and PDF metadata writing. |
| [svg2pdf](https://github.com/typst/svg2pdf) | Apache-2.0 | The SVG conversion implementation. |
| [Vello](https://github.com/linebender/vello) | Apache-2.0 | Bitmap-glyph logic in `bitmap.rs`. |

Pliego currently depends on `krilla`, not `krilla-svg`. The complete upstream acknowledgement list
is retained here so packaging cannot silently lose upstream notices. Removing an entry based on
features or packaged paths requires a separate source and artifact audit.

The published Krilla 0.8.2 crate identifies the pinned revision above but does not package the
repository-root `LICENSE_MIT`, `LICENSE_APACHE`, or `NOTICE.md` files. Pliego release packaging must
therefore retain the pinned license texts and acknowledgement notice explicitly rather than relying
on the crate archive alone.
