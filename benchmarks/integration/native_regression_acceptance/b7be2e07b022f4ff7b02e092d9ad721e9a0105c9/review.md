# Native regression preflight review

Accepted for the fixed minimal-static timing protocol, not as performance,
scaling or release-publication evidence.

- Native source: `b7be2e07b022f4ff7b02e092d9ad721e9a0105c9`.
- [Preflight run 34185938743](https://github.com/oxhq/pliego/actions/runs/34185938743),
  attempt 1, successful job 101934154869.
- Original proof artifact 10040525082: 504,542 bytes, SHA256
  `85073909b83b0ebaa944b4252d38260a22df958ad19f213245b751b107cc24bf`.
- Candidate Linux artifact 10040112871 from package run 34183772850; native
  SHA256 `db2037ebc9ecd91f9fb6462a0dbf862667bcf89117a6690bce182e071ab21902`.
- Published v0.3.3 native SHA256
  `2045867c2a7928bb2de9b4695cfb9678523faeaacb507bf43b31ecd0a2e6347f`.

The original ZIP, 340-file inventory, requests, raw outcomes and immutable
source closure were independently checked. All 123 verifier/source files match
the committed candidate. The hosted strict verifier passed; a moved-archive
recheck also passed, including each PDF's complete oracle JSON. The additional
local check used Windows Poppler 26.07, not the original Ubuntu Poppler 24.02
environment; no tool identity or original evidence was rewritten.

Four untimed observations produce six PDFs: one serial render and one two-worker
batch per native target. All six PDFs have SHA256
`1c7f8a259bc70efefb1eed49291b5f76933fb0777385710cde2c24a73242a3ef`.
Their unique A4 page was rendered and visually reviewed: the deliberate Ahem
heading and two text lines are separated, within the page, and unclipped.
Exact text, embedded font, scene/resource closure and normalized raster checks
pass. The block-shaped Ahem glyphs are the fixture's intended appearance.

All six separate synthetic controls were reviewed against their raw records and
committed assertions: one failed child, a 1,000 ms root timeout, early launcher
death with a separate-session descendant, bounded-output overflow, unchanged
`CGROUP_BUSY`, and subsequent recovery. Overflow retains at most 2,097,152 bytes
per stream and explicitly marks incomplete workers. Four full sampler records
have drained cgroups and zero final populated/dirty/writeback counters. Timeout
and busy outcomes intentionally have no full counter JSON; their cleanup is
source-bound to the successful hosted assertions, not a separate retained
post-cleanup process census. The controls use CPython, not native PDF samples.

Both actual native batches have distinct worker PID/start identities, exact
executable inodes, unchanged private storage bindings and a live pidfd overlap
witness inside both worker lifetimes. Every worker exits successfully with
complete streams; both groups drain under the unchanged 65,000 ms root bound.
This establishes overlapping lifetimes, not simultaneous CPU execution.

Timed runs must retain this exact preflight and run the existing retrieval,
source, oracle, lifecycle and acceptance checks before measurement. Keep all
three repeats and the serial/concurrency-2 populations separate; a failure or
new identity does not inherit this acceptance. No native, schema, sampler,
correctness threshold or fixture was changed by these review records.
