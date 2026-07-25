# Paged table border support

## Ownership chain

Servo table layout resolves the table grid and collapsed-border conflicts. Pliego
pagination assigns rows and repeated headers to fragments. Scene capture then emits
the resulting visible borders once as filled, axis-aligned rectangle paths. Preview
and PDF consumers paint those scene paths; they do not reconstruct or deduplicate
table borders.

This keeps three responsibilities separate:

- table layout owns border style, width, color, and inline coordinates;
- pagination owns fragment-edge and repeated-header placement;
- scene capture owns page-local rectangle serialization.

## Initial supported profile

The first support state is `solid-visible`:

- horizontal left-to-right tables using `border-collapse: collapse` or
  `border-collapse: separate`;
- finite, positive-width, opaque or translucent `solid` borders;
- one resolved width and sRGBA color for every collapsed edge, and for every
  painted side of an individual separate-mode box (absent sides are allowed);
- stable table-grid column widths;
- a repeated `thead` followed by ordinary, fitting, non-rowspanning body rows;
- zero border spacing for the deterministic separate-border fixture.

Each supported border is a filled `path` operation with positive rectangle bounds
and no stroke. The table-border fixture contains no other path-producing content,
so every path in that fixture is part of the border contract.

`border-image`, mixed resolved widths or colors, non-solid styles, border radii,
nonzero border spacing, bordered oversized rows, collapsed tables without a
repeated header, rowspans, and painting effects outside this profile remain
unsupported. They must stay observable as a typed table fallback instead of being
silently approximated as solid borders.

## Fragment-edge ownership

- Exact duplicate rectangles on one page are forbidden.
- A collapsed edge is emitted only after Servo conflict resolution; pagination does
  not resolve the same edge again.
- The repeated header owns its block-end edge. A coincident block-start edge from
  the first continued body row is omitted, leaving one header/body seam.
- The first and last visible rows on a fragment own the fragment's outer block
  edges. No additional synthetic border is painted over them.
- In separate mode, authored edges remain separate. The supported fixture authors
  each shared edge once (table top/left, cell right/bottom). Table sides are emitted
  as row-owned segments so page gaps are never painted, and
  pagination must not add a second edge.
- A centered rectangle that barely crosses its owning physical page is clipped to
  that page; no remainder is carried through margins or repeated-header bands.
- Repeated-header borders use the same inline coordinates, widths, and colors as
  the source header. Only their page-local block coordinates change.

Rectangle identity for verification is the page index plus rounded
`(x, y, width, height)` geometry. Repeated runs must produce the same identities,
and every fragment of one table must expose the same set of vertical border
coordinates.

## Verification fixture

`ports/pliego/tests/fixtures/table-borders/index.html` contains one collapsed and
one separate table. Both span pages and repeat their headers.

`ports/pliego/tests/check_table_borders.py` verifies:

- every border rectangle is positive, visible, filled, and unique;
- vertical border x coordinates do not drift between fragments;
- exactly one continuous horizontal seam separates each header from its body;
- the separate table's grid-authored top and left sides remain present;
- collapsed and separate tables each span at least two fragments;
- two identical renders have identical normalized border geometry and page-raster
  hashes.
