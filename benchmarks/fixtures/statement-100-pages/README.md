# statement-100-pages

100-page statement: scaling behavior, memory, and per-page cost. The
`input.html` is generated deterministically by
`benchmarks/tools/generate_fixtures.py` (2,500 rows; regenerate on a fresh
checkout — output is byte-identical on the same revision).

Expected (see `manifest.toml`): 100 pages. The exact count is an estimate and
is pinned to the measured value by the first signed baseline.
