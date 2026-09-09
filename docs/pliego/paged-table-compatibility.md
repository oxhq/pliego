# Paged-table compatibility

This page records bounded retained-layout behavior, including the unreleased
0.4.0 API 2 additions below. Published v0.3.3 API 2 still rejects unsupported
`collapsed-table-borders` capture; API 1 regression coverage is not API 2 delivery
proof. See the [support profiles](support-profile.md) for the release distinction.

Pliego paginates retained table rows only when it can preserve the laid-out grid.
Unsupported cases fall back with a typed diagnostic before a partial row plan is
applied.

## Row and rowspan behavior

| Table case | Paged behavior |
| --- | --- |
| Horizontal LTR table without rowspans | Rows use the retained-grid pagination path. |
| Rowspans whose touched rows form a run that fits in one fragment | The maximal contiguous run stays on one page. |
| Repeated header or terminal footer with a contained rowspan | The run must fit below the repeated header and cannot cross a header, body, or footer boundary. |
| Rowspan run taller than the available fragment | Pagination falls back with an unsupported-rowspan diagnostic. |
| Forced row or row-group break inside a rowspan run | Pagination falls back before translating fragments. |
| Cross-page rowspan continuation | Unsupported; Pliego does not split or synthesize a spanning cell across pages. |

Every physical row in the supported subset retains at least one cell originating in
that row. Diagnostics include the affected row range, laid-out size, available size,
and whether an internal forced break caused the fallback.

## Border behavior

The supported border profile covers horizontal LTR tables using
`border-collapse: collapse` or `border-collapse: separate` with finite,
positive-width `solid` borders. Repeated headers keep their original coordinates,
widths, and colors on each page. Shared fragment seams and repeated-header edges are
emitted once.

The 0.4 development API 2 profile also retains partial horizontal amount rules:
adjacent visible resolved tracks form one segment, and zero-width gaps remain
unpainted. Colspans preserve their resolved track extents. All visible segments on
one boundary must have the same positive solid width and color; vertical edges
must have zero width. A positive-width invisible edge alongside visible segments
is excluded because it can change border joins. This is not arbitrary collapsed
border conflict or colgroup-layout support. Exact native checks are in
`ports/pliego/tests/check_api2_headerless_tables.py`; publication status still
depends on the exact release artifacts, not this development document.

Entirely nonpainting collapsed borders retain row layout and pagination without
synthesizing border paths. Uniform visible collapsed borders without a header
are admitted only when no row translation is required; their multipage case is
still rejected. The repeated-header path retains original integer repeat and
page geometry rather than recovering coordinates from floating-point paint.
Rowspans are excluded from this collapsed-grid path even when contained on one
page; the broader retained-layout rowspan behavior above does not override it.

Solid table-track backgrounds use the owning cell's original border rectangle
and retain column-group, column, row-group and row paint order. The ordinary
solid-color, sharp/unclipped/untransformed restrictions still apply. This is not
support for background images, general border conflict resolution or arbitrary
table layout.

Complex border styles, border images, radii, mixed resolved styles, vertical or RTL
table writing modes, bordered oversized rows, and cross-page rowspans are outside
this profile. They must remain observable as a fallback rather than being silently
approximated.

Focused contributor checks:

```sh
python ports/pliego/tests/check_table_rowspans.py <pliego-binary>
python ports/pliego/tests/check_table_borders.py <pliego-binary>
```
