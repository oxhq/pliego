# Benchmark methodology

Pliego's benchmark work is designed to answer two separate questions:

1. Does a candidate preserve the required document result?
2. For results that pass that gate, what time and resource cost does each supported
   execution model have?

The repository contains an implemented Pliego engine harness, pinned fixture
protocol, and a first target-neutral competitor slice for dompdf and Browsershot.
Only `minimal-static` has a competitor correctness mapping. Authoritative timing
remains N/A until canonical Poppler identities and externally attested immutable
competitor images are pinned. A separate manual workflow can produce measured,
directional `github-hosted-exploratory` snapshots; those can never validate as a
dedicated baseline or support a general production ranking. Every other
competitor/fixture pair is an explicit `not-applicable` record with a reason. The
repository contains **no committed performance snapshot yet**, so no comparative
speed, memory, or CPU conclusion should be inferred from this document.

The detailed harness contract, fixture inventory, metric definitions, and target
manifest live in the [benchmark source directory](../../benchmarks/README.md).

## Prepare the Pliego baseline target

Run these commands from the repository root on a dedicated Linux x86_64 host that
meets the environment requirements in the source benchmark protocol:

```sh
python3 benchmarks/tools/generate_fixtures.py

(cd ports/pliego/tests/fixtures/chartjs-report && \
  npm ci && \
  cp ../../../../../components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf \
    ReportSans.ttf)

cache="$HOME/.cache/pliego-benchmarks"
binary="$(python3 benchmarks/tools/resolve_release.py \
  --cache "$cache" \
  --metadata-out "$cache/verified-release.json")"

python3 benchmarks/tools/run_benchmark.py \
  --binary "$binary" \
  --dedicated \
  --out benchmarks/baselines/pliego-0.3.3-linux-x86_64.json

python3 benchmarks/tools/validate_result.py \
  benchmarks/baselines/pliego-0.3.3-linux-x86_64.json
```

`resolve_release.py` verifies the release archive and binary against the hashes in
`benchmarks/manifest.toml` and the retained byte-identical promoted runtime
manifest. The monorepo Laravel manifest intentionally remains pre-promotion.
Until the manifest pins the canonical Poppler runtime,
`run_benchmark.py` emits an explicit `not-applicable` result instead of numbers.
After those pins exist, it applies the committed warm-up, sample, randomization,
and cgroup protocol. `--dedicated` records the host assertion and requires the
benchmark source tree to be clean. A local output file is not a published result;
review it together with the verified-release metadata and host evidence.

To compare a candidate binary's stable outcomes with the verified release binary:

```sh
python3 benchmarks/tools/compare_parity.py \
  --baseline "$binary" \
  --candidate path/to/pliego-candidate \
  --out path/to/parity-report.json
```

This command compares the stable outcomes produced by two distinct Pliego binaries.
It is not a comparison of benchmark-result JSON files, and it is not a dompdf or
Browsershot comparison.

## Reproduce the implemented competitor slice

Install the exact Composer and npm lock files as described in the source benchmark
guide, then run on the same dedicated cgroup-v2 host:

```sh
python3 benchmarks/tools/run_benchmark.py \
  --target dompdf-3.1.6 \
  --fixture minimal-static \
  --dedicated \
  --out path/to/dompdf-minimal-static.json

BROWSERSHOT_NODE_BINARY=/usr/bin/node \
BROWSERSHOT_CHROME_PATH=/opt/chrome/chrome \
python3 benchmarks/tools/run_benchmark.py \
  --target browsershot-5.4.0-puppeteer-25.8.0 \
  --fixture minimal-static \
  --dedicated \
  --out path/to/browsershot-minimal-static.json
```

The current commands deliberately return `not-applicable` for competitor timing:
the manifest does not yet pin immutable OCI image digests, canonical
dependency/runtime hashes and paths, or canonical Poppler identities, and no
out-of-process image attestation path exists. The correctness workflow installs
both locked graphs and directly renders `minimal-static` through Pliego API 2 and
each adapter, then runs real Poppler checks. A separate manual performance
workflow runs all three targets in one GitHub-hosted VM with 100 timed, globally
interleaved samples per target and retains exact cgroup CPU/memory/I/O plus raw
sample provenance. Its output is measurement evidence for that exact hosted run,
but not an authoritative or generalized benchmark. Three independent jobs are
sealed into one no-selection series: all repeats, per-metric p50 ranges, and
repeat-to-repeat spread are retained, so the published report cannot silently
choose the most favorable VM.

Each target gets a fresh private on-disk `TMPDIR` below the same ext4 scratch
root. The workflow sets and verifies inherited Linux `FS_NOATIME_FL`,
`FS_SYNC_FL`, and `FS_DIRSYNC_FL`, so scratch reads do not create incidental
atime dirtiness while transient file data and unlink/rmdir metadata are
synchronous. Scratch stays off tmpfs and its block I/O remains in the retained
cgroup totals. The sampler binds the scratch inode, revalidates the storage
contract after descendant drain, and records it in every sample. These are
deliberate non-default benchmark conditions; synchronous-write cost is included
in wall and I/O measurements.
`HOME`, `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
`XDG_RUNTIME_DIR`, and `XDG_STATE_HOME` are fresh private children of that same
measured scratch root for every target invocation; browser state cannot escape
into a host account.

After immutable images are pinned, each target uses the same order: one discarded correctness preflight, discarded
warmups, then cold one-shot timed samples. The adapter root and every descendant
(including PHP, Node, and Chromium) remain in the existing retained cgroup-v2
accounting subtree. After timing, every output passes the same Poppler-based oracle:
PDF envelope/parser acceptance, A4 dimensions, one page, complete normalized
text, one embedded Ahem family, and a shared normalized raster signature. A target
is never marked supported unless every timed sample
passes that oracle. The common A4 expectation allows at most 0.75 points of
print-grid quantization for all targets.

Supported results retain canonical paths, versions, and SHA-256 values for the
adapter and runtime executables, installed dependency-tree snapshots, hashes of
the committed Composer/npm locks, and the manifest-pinned image digest. Every
snapshot must match a canonical manifest hash/path, and the root plus each
protected path must resolve on a read-only mount; mutable dependency directories
alone do not. A caller-supplied image-digest string is not attestation, so
adapter timing stays N/A until an external launcher proves the running image.
All four Poppler tool identities must also match manifest pins. Browsershot samples require a fresh Linux network namespace
with only `lo`; Chromium flags and HTTP(S) request blocking are not treated as
network isolation.
The authoritative CLI commands run one target at a time. The committed seed randomizes
fixture traversal within each target. The harness now contains a deterministic
SHA-256-ranked cross-target schedule plus phase-separated runner entrypoints.
Its hash input is the ASCII encoding of compact JSON with every non-ASCII code
point JSON-escaped, for
`["pliego.cross-target-schedule.v1", seed, fixture, phase, iteration,
target_id]`; entries sort by digest bytes and then target-ID UTF-8 bytes. It can
execute preflight, warmup, and indexed timed rounds without batching a target's
samples. The public CLI does not yet construct attested multi-target contexts
or persist an attested multi-target run, so these commands remain implementation
proof, not an authoritative cross-engine report. The separate hosted coordinator
does persist exact three-target runs under its narrower exploratory claim. The
executor produces a
`pliego.benchmark-interleaved-run` version 1 envelope marked
`prerequisite-only`: its global timed order embeds the unmodified raw samples,
with content-bound sample IDs and a digest content-addressing the entire
envelope. The
stdlib validator regenerates the schedule, applies the existing benchmark
sample schema, and verifies every binding and hash. This makes later report
references unambiguous; it does not itself supply target attestation or Linux
measurement evidence.

The companion `report_data.py` generates only `wall_ms`
min/p50/p95/p99/max/mean cells, and only when every timed sample in the validated
artifact passed correctness. Each cell contains the artifact digest and the exact
contributing sample IDs in schedule order; the report source also binds fixture,
targets, and the complete schedule digest. Validation recomputes every value and
rejects provenance drift. Its `render` command emits a deterministic Markdown
table only after that validation, links every displayed value to its full cell ID,
and retains the contributing sample IDs. Both artifacts remain explicitly
`prerequisite-only`; the renderer adds no rankings, narrative, or authority to
publish comparative claims.

The hosted lane instead uses `comparison_metrics.py` and `run_comparison.py` to
aggregate every top-level performance/output metric plus available phase and
bridge timings from 100 correctness-passing samples per target. Lower-level
cgroup diagnostics remain in the raw samples. The lane binds those values to
exact runtime and sample identities and seals three complete repeats without
selecting a winner. That is real measurement for the named GitHub-hosted run,
but it does not satisfy or weaken the authoritative gates above.

## Implemented slice and remaining comparative protocol

| Target | Runner status | Eligible public claim today |
| --- | --- | --- |
| Pliego v0.3.3 API 2 | Implemented and pinned | Published bundle and correctness harness can be reproduced; no committed hosted snapshot yet |
| Pliego candidate | Stable-outcome parity comparator only; arbitrary candidate performance runs are not implemented | Parity can be checked locally; no candidate performance claim |
| dompdf 3.1.6 | Locked one-shot adapter; configured Ubuntu/Poppler smoke | Authoritative timing N/A pending image attestation and oracle pins; eligible only for exact-run hosted snapshots |
| Browsershot 5.4.0 + Puppeteer 25.8.0 | Locked one-shot adapter; configured network-isolated Ubuntu/Poppler smoke | Authoritative timing N/A pending image attestation and oracle pins; eligible only for exact-run hosted snapshots |

Before an authoritative cross-engine baseline or generalized ranking is
published, every target must follow these rules:

- Pin the engine, wrapper, runtime, operating-system image, native dependencies, and
  container or host identity.
- Use byte-identical HTML, assets, fonts, page dimensions, margins, and network
  policy. Record any required target-specific transformation as an exclusion or a
  separate test, not as an invisible adapter behavior.
- Count the full process tree. Browsershot measurements must include Chromium, Node,
  PHP, and relevant descendants rather than only the launcher process.
- Gate every timed fixture on declared correctness and parity checks. A crash,
  unsupported result, blank page, missing resource, or partial output is a failure,
  not a fast sample.
- Compare only fixtures whose required result can be produced by every target in that
  comparison. Publish excluded fixtures and the reason for each exclusion.
- Keep cold, one-process-per-document measurements separate from any warm,
  persistent, pooled, or cached execution mode.
- Run on the same dedicated host with the committed sample counts, warm-ups, seeded
  random order, resource controls, and percentile method.
- Publish raw samples, hashes, commands, logs, environment metadata, validation
  output, and the report-generation code with the summary.

The remaining dedicated-Linux gates are fully attested target contexts, a
canonical Poppler runtime, a genuine dedicated run artifact, and durable
retention of that artifact with its validated cells and generated table.
Single-target serial throughput now uses the retained outer one-shot wall interval
from runner process open through sampler exit, which includes descendant drain and
accounting settlement; it is still not an authoritative cross-engine comparison
until the dedicated identity and evidence gates pass.

## Authoritative publication gate

A dedicated baseline or generalized benchmark report is publishable only when a fresh checkout can verify target
identity, regenerate fixtures, reproduce the commands, validate every included
result from the exact recorded clean harness commit, verify the retained oracle
script and all four Poppler executable identities, and trace each chart or table
cell back to raw data. If a target runner or
correctness adapter is incomplete, the report must say so and omit that comparison.

Funding the missing comparator adapters or dedicated measurement infrastructure does
not predetermine a result. The project will retain and report negative, neutral, or
inconclusive findings under the same protocol.
