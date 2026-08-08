# Pliego benchmarks

Reproducible, correctness-gated measurements for released and candidate Pliego
runtimes. The same corpus and protocol are reused so changes can be attributed.

## The three benchmark levels

Measurements stay separated into three levels:

1. **Core** — `DocumentScene → PDF` in-process (Criterion Rust bench). Not in
   this directory yet; a separate `ports/pliego` bench target.
2. **Engine** — *this harness*: released binary → PDF + artifacts, one process
   per render.
3. **Laravel end-to-end** — Blade → pliego-laravel → usable PDF. Lives with the
   Laravel SDK; this harness deliberately does not start PHP web apps.

This directory only implements level 2. Optimizing the PDF backend does not
necessarily improve the Laravel experience, and vice versa.

## Layout

```text
benchmarks/
├── README.md
├── manifest.toml              Single source of truth for fixtures, targets, protocol
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
│   └── pliego.php             One process per sample; NDJSON on stdout
├── tools/
│   ├── generate_fixtures.py   Deterministic generation of long/image fixtures
│   ├── run_benchmark.py       Orchestrator: manifest → runner → aggregates → result file
│   └── validate_result.py     Stdlib-only JSON Schema check for result files
├── baselines/                 Released-runtime baselines
└── reports/                   Comparison reports land here
```

## Prerequisites

* Dedicated or self-hosted **Linux x86_64** — GitHub-hosted Actions are for
  smoke checks only, never for publishable numbers.
* The **published bundle** (`checked-release` profile) downloaded from
  Releases and verified against its adjacent SHA-256. Never `cargo run`.
* `php-cli` ≥ 8.1 (runner), `python3` ≥ 3.11 (orchestrator/validator; stdlib only),
  and `poppler-utils` (`pdftotext` for text correctness checks).
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

## Running a baseline

```sh
python3 benchmarks/tools/run_benchmark.py \
  --binary /path/to/pliego \
  --out benchmarks/baselines/pliego-0.1.1-linux-x86_64.json
```

Subset or override with `--fixture invoice-showcase`, `--samples 50`,
`--warmup 10`, `--php /usr/bin/php`. The orchestrator:

1. checks the fixture surface (inputs, fonts, chartjs prep) and the binary;
2. runs `runners/pliego.php` per fixture — warmup discarded, then samples;
3. aggregates p50/p95/p99/min/max/mean, determinism, correctness, failures;
4. validates the result against `schema/benchmark-result.v1.json`;
5. writes the result file (raw samples kept, not just averages).

Each sample is one fresh `pliego render` process with the fixture's flags; the
runner captures wall time, the engine's `scene-report.json`, phase timings,
PDF bytes/hash, and correctness facts. CPU and process-tree memory fields stay
unavailable until an audited whole-tree sampler is used.

## Protocol (from `manifest.toml`)

* 10 warm-up iterations, 50 samples for short documents, 20 for long ones.
* Random order between runners (seed recorded). Raw samples stored.
* Same host, same binary, same fonts/assets. Network disabled.
* Results record host info and versions; a baseline is signed by commit/tag.

## Metrics

Latency (p50/p95/p99/min/max/mean), CPU (user/sys), whole-process-tree peak RSS,
scaling (per-page time and memory), throughput (serial
renders/minute; concurrent 2/4/8 is a later increment), I/O and artifact bytes,
runtime distribution, output bytes/page count, correctness (page count, text,
row uniqueness, totals, links, fonts), repeatability (scene hash / PDF hash
variation), and typed failures (code, timing, published/no PDF).

## Fixtures and correctness gates

Each fixture declares expected correctness in `manifest.toml`. A sample counts
toward performance only when its checks pass; a wrong result is not "faster".
Generated-fixture `page_count` targets are pinned to the published Linux 0.1.1
renderer.

| Fixture | Category | Purpose | Expected |
| --- | --- | --- | --- |
| `minimal-static` | startup | pure startup, no scripts/fonts/images | 1 page, text |
| `invoice-showcase` | static | fonts, paged table, totals, authored break | 2 pages, `5280.00` |
| `chartjs-showcase` | scripted | Chart.js 4.5.1 canvas + readiness | 1 page, report text |
| `ledger-20-pages` | scale | fragmentation, repeated headers | ~20 pages |
| `statement-100-pages` | scale | scaling, memory, per-page cost | ~100 pages |
| `font-image-heavy` | resources | decode, embed, I/O | ~6 pages |
| `unsupported-paint` | failure | fail-closed on unsupported paint | typed failure, no PDF |

## Current scope

* Concurrency >1 throughput sampling is not implemented yet (serial only).
* Core (Criterion) and Laravel e2e levels live outside this directory.
* Page-count expectations for generated fixtures are pinned by the first signed baseline.
* A multi-fixture `--out` file bundles one validated result object per fixture
  as a JSON array; each element conforms to `benchmark-result.v1.json`, the
  bundle itself is a container. Single-fixture runs write one result object.
