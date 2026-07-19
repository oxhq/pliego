# Retained SVG vector fixture

`live.html` loads `graphic.svg` as an external image after explicitly loading the repository's
bundled Ahem font. The SVG contains a transformed solid path with a simple stroke, one data-PNG,
one text run, and one filtered group. The representable nodes must remain canonical vector, image,
and text operations; the filtered group must remain an explicit `svg-compositing` diagnostic.
