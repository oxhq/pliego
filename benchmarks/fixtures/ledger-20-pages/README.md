# ledger-20-pages

~20 pages of paged-table fragmentation and repeated table headers. The
`input.html` is generated deterministically by
`benchmarks/tools/generate_fixtures.py` (250 rows; regenerate on a fresh
checkout — output is byte-identical on the same revision).

Expected (see `manifest.toml`): 20 pages. The exact count is an estimate and is
pinned to the measured value by the first accepted baseline.
