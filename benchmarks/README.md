# Pliego benchmarks

Reproducible, correctness-gated measurements for released and candidate Pliego
runtimes. The same corpus and protocol are reused so changes can be attributed.

## The three benchmark levels

Measurements stay separated into three levels:

1. **Core** — `DocumentScene → PDF` in-process (Criterion Rust bench). Not in
   this directory yet; a separate `ports/pliego` bench target.
2. **Engine** — *this harness*: one-shot target adapter → PDF + artifacts, one
   fresh process tree per render.
3. **Laravel end-to-end** — Blade → pliego-laravel → usable PDF. Lives with the
   Laravel SDK; this harness deliberately does not start PHP web apps.

This directory only implements level 2. Optimizing the PDF backend does not
necessarily improve the Laravel experience, and vice versa.

## Layout

```text
benchmarks/
├── README.md
├── manifest.toml              Single source of truth for fixtures, targets, protocol
├── adapters/                  One-shot target adapters and committed dependency locks
│   ├── dompdf/                dompdf 3.1.6 / Composer lock
│   └── browsershot/           Browsershot 5.4.0 / Composer + Puppeteer locks
├── schema/
│   └── benchmark-result.v1.json
├── fixtures/                  Seven frozen fixtures
│   ├── minimal-static/        Pure startup
│   ├── invoice-showcase/      Two-page invoice: fonts, table, totals, authored break
│   ├── chartjs-showcase/      Managed reference to the Chart.js 4.5.1 report fixture
│   ├── ledger-20-pages/       ~20 pages of fragmentation + repeated headers (generated)
│   ├── statement-100-pages/   ~100 pages: scaling, memory, per-page cost (generated)
│   ├── font-image-heavy/      Font embedding, image decode, resources and I/O (generated)
│   └── unsupported-paint/     Fail-closed path for unsupported CSS paint
├── runners/
│   └── pliego.php             Target-neutral adapter loop; NDJSON on stdout
├── tools/
│   ├── generate_fixtures.py   Deterministic generation of long/image fixtures
│   ├── process_tree_sampler.py Linux cgroup-v2 containment and accounting
│   ├── pdf_oracle.py           Shared untimed PDF correctness checks
│   ├── run_benchmark.py       Orchestrator: manifest → runner → aggregates → result file
│   ├── test_process_tree_sampler.py Fixture, live cgroup, bridge, and overhead proof
│   └── validate_result.py     Stdlib-only JSON Schema check for result files
├── baselines/                 Released-runtime baselines
└── reports/                   Comparison reports land here
```

## Prerequisites

* Dedicated or self-hosted **Linux x86_64, kernel 6.1 or newer**, with unified
  cgroup v2. GitHub-hosted Actions are for smoke checks only, never for
  publishable numbers. Linux 6.1 is required because the retained accounting
  contract requires both `memory.peak` and `pids.peak`.
* The **published bundle** (`checked-release` profile) resolved by the pinned
  release verifier. Never `cargo run`.
* `php-cli` ≥ 8.3 with `dom`, `mbstring`, `fileinfo`, and `json`
  (runner/adapters), `python3` ≥ 3.11 (orchestrator/validator; stdlib only),
  and `poppler-utils` (`pdfinfo`, `pdftotext`, `pdffonts`, and `pdftoppm` for
  the shared correctness oracle). Publishable results also require the exact
  path, SHA-256, and version of all four tools to be pinned in `manifest.toml`;
  those pins are currently absent.
* For adapter correctness smoke, Composer 2; Browsershot additionally needs Node,
  Puppeteer installed from the committed npm lock, and one canonical Chromium
  executable. Exact PHP, Node, Chromium, adapter, and lock identities are
  retained in every supported result. Publishable competitor timing also
  requires a manifest-pinned OCI image digest, canonical expected hashes and
  paths for every dependency/runtime, read-only mounts for the root and every
  protected path, and out-of-process proof that the running image has the
  claimed digest. Those pins and that attestation path are absent in this slice,
  so competitor timing remains N/A.
* A root broker in a cgroup-v2 domain parent delegated by the host service with
  `cpu`, `io`, `memory`, and `pids` enabled, plus a fixed non-root account named
  `pliego-benchmark-engine`. The broker must run in the parent's sole direct
  child, `harness`; set `PLIEGO_BENCHMARK_CGROUP_PARENT` to the canonical,
  empty root-owned parent. The sampler does not provision the service/account.
* All resources local, network disabled, same fonts and assets for every run.

## Freezing and generating fixtures

Static fixtures (`minimal-static`, `invoice-showcase`, `unsupported-paint`) are
committed directly. `invoice-showcase` and `font-image-heavy` carry a committed
copy of `Ahem.ttf` (from `ports/pliego/tests/fixtures/text-scene/Ahem.ttf`,
SHA-256 `b719ecb3…`).

The scale fixtures are generated deterministically (no randomness, no clock):

```sh
python3 benchmarks/tools/generate_fixtures.py
```

Run it once after a fresh checkout; it emits `ledger-20-pages/input.html`,
`statement-100-pages/input.html`, and `font-image-heavy/input.html`. Regenerating
on the same revision is a no-op (byte-identical).

`chartjs-showcase` is a managed reference to
`ports/pliego/tests/fixtures/chartjs-report`. Before benchmarking it, prepare
once:

```sh
cd ports/pliego/tests/fixtures/chartjs-report
npm ci
cp ../../../../../components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf ReportSans.ttf
```

The orchestrator refuses to run it until both files exist.

## Preparing the locked competitor adapters

Install from the committed locks; do not update dependencies on the benchmark
host:

```sh
(cd benchmarks/adapters/dompdf && composer install --no-dev --classmap-authoritative)
(cd benchmarks/adapters/browsershot && \
  composer install --no-dev --classmap-authoritative && \
  PUPPETEER_SKIP_DOWNLOAD=1 npm ci --omit=dev)

export BROWSERSHOT_NODE_BINARY=/usr/bin/node
export BROWSERSHOT_CHROME_PATH=/opt/chrome/chrome
```

The two runtime paths must be canonical executables unavailable for mutation
by `pliego-benchmark-engine`. Their version, path, and SHA-256 are captured by
the adapter before sampling. Installed dependency-tree hashes are captured and
rechecked, but they become immutable evidence only when they match manifest
content pins and every executable/dependency path resolves on a read-only
mount in the pinned image. The launch-supplied image digest is retained as
provenance but is never accepted as attestation. No supported adapter result is
allowed until an out-of-process launcher can prove the running image. The
Browsershot adapter keeps Chromium's sandbox and request blocking as
defense in depth; the sampler's fresh network namespace, with only `lo`, is the
enforced no-network boundary.

## Running a baseline

```sh
cache="$HOME/.cache/pliego-benchmarks"
binary="$(python3 benchmarks/tools/resolve_release.py \
  --cache "$cache" --metadata-out "$cache/verified-release.json")"
python3 benchmarks/tools/run_benchmark.py \
  --binary "$binary" \
  --out benchmarks/baselines/pliego-0.1.1-linux-x86_64.json
```

The resolver accepts only the committed Linux x86_64 release name, size,
archive SHA-256, exact file set, binary SHA-256, native commit, and Servo build.
Use `--offline` after the verified archive is cached. The orchestrator checks
the binary digest again. With canonical Poppler pins currently absent, it emits
explicit `not-applicable` results before sampling rather than publish numbers
authorized by an unpinned oracle runtime.

The intended first competitor slice, once an out-of-process launcher attests a
committed `image_digest`, canonical `identity_sha256` and `identity_paths` maps,
read-only protected paths, and canonical Poppler pins, is:

```sh
python3 benchmarks/tools/run_benchmark.py \
  --target dompdf-3.1.6 \
  --fixture minimal-static \
  --out path/to/dompdf-minimal-static.json

python3 benchmarks/tools/run_benchmark.py \
  --target browsershot-5.4.0-puppeteer-25.8.0 \
  --fixture minimal-static \
  --out path/to/browsershot-minimal-static.json
```

With the current manifest, these commands emit explicit `not-applicable`
records because no immutable runtime image or canonical runtime/dependency
identity, external image attestation, or canonical Poppler identity is pinned.
Omitting `--fixture`
also emits explicit
`not-applicable` records and reasons for every unverified fixture. It does not
manufacture zero measurements for exclusions.

Subset with `--fixture invoice-showcase` or select PHP with
`--php /usr/bin/php`. `--samples` and `--warmup` are accepted only when they
equal the canonical manifest values; overrides cannot produce a result file.
The orchestrator:

1. fingerprints the fixture before preflight and rechecks it around every
   render; checks exact target/adapter/runtime identity;
2. runs one discarded correctness preflight, discarded warmups, then timed
   samples through the same single-sample adapter contract;
3. aggregates p50/p95/p99/min/max/mean, determinism, correctness, failures;
4. validates the result against `schema/benchmark-result.v1.json`;
5. writes the result file (raw samples kept, not just averages).

Each sample gets a fresh root-owned, non-delegated child cgroup. A root launcher
first stops in a staging cgroup, drops supplementary groups, all real/effective/
saved IDs, every capability set, and its bounding capabilities, then sets
`no_new_privs`. The broker verifies those `/proc` fields, PID/start identity,
executable hash/argv, and denied migration-interface writes before moving the
stopped launcher into the clean measurement leaf and starting engine wall time.
All later descendants, including new sessions, remain contained. The retained
final `cpu.stat`, `io.stat`, `memory.current`, `memory.peak`, and `pids.peak`
counters are the accounting source. Engine wall time ends with the root process;
descendant drain and accounting-settle durations are recorded separately.

The `minimal-static` oracle declares ISO A4 in points and permits at most 0.75
points of print-grid quantization. All text explicitly uses normal-weight Ahem.
The oracle requires the complete normalized document text, exactly one embedded
Ahem family, and a shared full-page 24x32 monochrome occupancy signature with
quantized ink area. It preserves page-relative position and scale within an
explicit coarse tolerance instead of cropping and rescaling the ink.
The same expectations apply to all targets.

Publication fails unless `cgroup.events` drains recursively. After it empties,
`memory.stat` dirty/writeback must reach zero and two interval-separated
`cpu.stat` and `io.stat` reads must match. A bounded `cgroup.kill` cleans leaked
descendants, but its use fails a passing sample. Launcher setup and privilege
removal happen in the staging leaf, so the measurement leaf starts with only
the stopped unprivileged launcher and zero CPU/I/O/memory counters before exec.

Periodic `/proc` PID/start-time, summed RSS, and summed PSS observations are
retained only as sampled lower-bound diagnostics; short-lived processes may be
missed there without weakening cgroup accounting. As root, with
`PLIEGO_BENCHMARK_CGROUP_PARENT` exported, run
`/usr/bin/python3 benchmarks/tools/test_process_tree_sampler.py --live --php-integration`
inside the delegated `harness` child for the containment, cleanup, counter, and
PHP-to-Python proof. On a dedicated benchmark host, add
`--acceptance-overhead` for the 20-pair randomized on/off gate. It uses the
protocol's `nearest-rank-v1` percentiles and requires p95 wall overhead below
2%; sampler CPU share remains a separate diagnostic.

## Protocol (from `manifest.toml`)

* One untimed correctness preflight, 10 warm-up iterations, then 50 timed
  samples for short documents or 20 for long ones.
* Every sample is a cold, one-shot process. The committed seed randomizes
  fixture traversal within a target, preserving the existing `sample_order =
  "random"` protocol. Cross-target sample interleaving is not implemented in
  this slice; raw samples and the seed are stored.
* Same host, same binary, same fonts/assets. Network disabled.
* Results record host info, the exact clean harness commit, the oracle script,
  and all Poppler executable identities; validation requires the matching
  checkout. A baseline is signed by commit/tag.
* All aggregate and observer percentiles use `nearest-rank-v1`.

## Metrics

This foundation records wall latency (p50/p95/p99/min/max/mean), serial
throughput, per-page wall time, PDF and artifact bytes, page count, page
dimensions, required text, link targets, capture status, PDF hash variation,
and typed failure publication state.
CPU, cgroup memory, and cgroup I/O are exact retained counters. Sampled summed
RSS/PSS are explicitly lower bounds. Runtime archive size and deeper document
checks remain separate audited increments before a signed baseline is published.

## Fixtures and correctness gates

Each fixture declares expected correctness in `manifest.toml`. A sample counts
toward performance only when its checks pass; a wrong result is not "faster".
Generated-fixture `page_count` targets are pinned to the published Linux 0.1.1
renderer.

| Fixture | Category | Purpose | Expected |
| --- | --- | --- | --- |
| `minimal-static` | startup | pure startup, one local font, no scripts/images | A4, 1 page, text, link |
| `invoice-showcase` | static | fonts, paged table, totals, authored break | 2 pages, `5280.00` |
| `chartjs-showcase` | scripted | Chart.js 4.5.1 canvas + readiness | 1 page, report text |
| `ledger-20-pages` | scale | fragmentation, repeated headers | ~20 pages |
| `statement-100-pages` | scale | scaling, memory, per-page cost | ~100 pages |
| `font-image-heavy` | resources | decode, embed, I/O | ~6 pages |
| `unsupported-paint` | failure | fail-closed on unsupported paint | typed failure, no PDF |

## Current scope

* Concurrency >1 throughput sampling is not implemented yet (serial only).
* dompdf and Browsershot have direct Ubuntu render + real Poppler smoke for
  `minimal-static`, but publishable measurements remain N/A until immutable
  image digests are pinned; all other fixtures are explicit exclusions.
* Cross-target sample interleaving and a report generator are not implemented;
  this slice does not publish comparative numbers.
* Dedicated-Linux acceptance still needs seeded cross-target interleaving,
  throughput whose wall boundary includes descendant drain, and report-cell to
  raw-sample traceability. The existing single-target throughput field is not a
  publishable cross-engine throughput claim.
* The Ubuntu adapter/Poppler smoke is configured in CI; it is not hosted proof
  until that workflow passes at the exact commit containing this change.
* Core (Criterion) and Laravel e2e levels live outside this directory.
* Page-count expectations for generated fixtures are pinned by the first signed baseline.
* A multi-fixture `--out` file bundles one validated result object per fixture
  as a JSON array; each element conforms to `benchmark-result.v1.json`, the
  bundle itself is a container. Single-fixture runs write one result object.
