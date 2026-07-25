# Retained SVG vector fixture

`live.html` loads `graphic.svg` as an external image after explicitly loading the repository's
bundled Ahem font. The SVG contains a transformed solid path with a simple stroke, one data-PNG,
one text run, one SMIL animation, and one filtered group. The representable nodes must remain
canonical vector, image, and text operations; the animation and filtered group must remain explicit
`svg-animation` and `svg-compositing` diagnostics.
