# Pliego paged-table compatibility

Pliego paginates retained table rows only when it can preserve the laid-out grid. The current
rowspan subset is deliberately conservative:

| Table case | Paged behavior |
| --- | --- |
| Horizontal LTR, separate-border table without rowspans | Rows use the existing retained-grid pagination path. |
| One or more rowspans whose touched rows form a run that fits in one fragment | The maximal contiguous run of rows touched by any rowspan stays on one page. Adjacent spans may therefore be kept together. |
| Repeated header or terminal footer with a contained rowspan | A body or footer run must fit below the repeated header and cannot cross a header/body/footer placement boundary. |
| Rowspan run taller than its fragment capacity | Retained-row pagination falls back. Ordinary tables emit `unsupported-table-rowspan-pagination`; repeated-header or terminal-footer tables preserve `unsupported-table-group-pagination` with reason `rowspan`. |
| Forced row or row-group break inside a rowspan run | Retained-row pagination falls back before translating fragments, using the same ordinary/group diagnostic split above. |
| Cross-page rowspan continuation | Unsupported. Pliego does not split or synthesize a spanning cell across pages. |

Every physical row in the supported subset must retain at least one cell that originates in that
row. Collapsed borders, vertical or RTL table writing modes, and captions keep their existing
compatibility boundaries.

The diagnostic records the half-open row range, its laid-out block size, the available block size,
and whether an internal forced break caused the fallback. It is emitted before retained fragments
are translated, so the generic table fallback cannot partially apply the row plan.

Run the focused checker with an existing Pliego binary:

```bash
python ports/pliego/tests/check_table_rowspans.py <pliego-binary>
```

The checker renders both `fixtures/table-rowspans/index.html` and
`fixtures/table-rowspans/forced-cross-page.html`.
