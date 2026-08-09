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
│   ├── benchmark-result.v1.json
│   └── benchmark-host-proof.v1.json
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
│   ├── benchmark_host_preflight.py Dedicated-host gate and command wrapper
│   ├── process_tree_sampler.py Linux cgroup-v2 containment and accounting
│   ├── run_benchmark.py       Orchestrator: manifest → runner → aggregates → result file
│   ├── validate_host_proof.py Validate host proof plus retained raw evidence
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
* `php-cli` ≥ 8.1 (runner), `python3` ≥ 3.11 (orchestrator/validator; stdlib only),
  and `poppler-utils` (`pdftotext` for text correctness checks).
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
the binary digest again before starting a sample.

Subset or override with `--fixture invoice-showcase`, `--samples 50`,
`--warmup 10`, `--php /usr/bin/php`. The orchestrator:

1. checks the fixture surface (inputs, fonts, chartjs prep) and the binary;
2. runs `runners/pliego.php` per fixture — warmup discarded, then samples;
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

## Dedicated-host proof

Publishable timing runs must enter through the manual-only
`Pliego dedicated benchmark host` workflow. Production dispatches select runner
group `Pliego dedicated benchmarks` with labels `self-hosted`, `Linux`, `X64`,
and `pliego-benchmark-pinned-v1`. The wrapper rejects before the chronology's
`samples.started` event unless the run is on protected `main`, the workflow,
checkout, and live branch all name one immutable SHA, and the GitHub API proves
the exact online/busy runner, group, and labels.

The host administrator owns `/etc/pliego-benchmark-host.v1.json`. Its pinned
values must describe the real host; this abbreviated example shows the complete
contract shape:

```json
{
  "schema": "pliego.benchmark-host-config",
  "version": 1,
  "runner": {"name": "pliego-pinned-01", "group_id": 123},
  "cpu": {
    "set": "2-3",
    "topology": {
      "2": {"package": 0, "core": 1, "siblings": "2-3"},
      "3": {"package": 0, "core": 1, "siblings": "2-3"}
    }
  },
  "controls": {"boost": "disabled", "smt": "enabled", "aslr": 2},
  "sensors": {
    "thermal_paths": ["class/thermal/thermal_zone0/temp"],
    "throttle_paths": ["devices/system/cpu/cpu2/thermal_throttle/core_throttle_count"]
  },
  "limits": {
    "max_load_one_per_cpu": 0.25,
    "max_cpu_psi_avg10": 0.1,
    "max_memory_psi_avg10": 0.1,
    "max_io_psi_avg10": 0.1,
    "max_temperature_millic": 70000,
    "max_temperature_drift_millic": 5000,
    "max_interrupt_delta": 100000
  },
  "lock_path": "/var/lock/pliego-benchmark-host.lock"
}
```

The wrapper holds the exclusive lock through the child command; requires the
runner process affinity to equal the configured isolated whole-core set;
rejects competing user workloads; checks topology, performance governors,
boost, SMT, and ASLR; and captures load, CPU/memory/I/O PSI, interrupts,
temperatures, and throttle counters before and after the command. Control drift,
new throttle counts, or configured load/pressure/temperature drift makes the
run non-publishable.

Every attempt retains `benchmark-host-proof.v1.json`, raw NDJSON chronology,
command stdout/stderr, diagnostics, and `SHA256SUMS`. The workflow's controlled
`negative-github-hosted` and `negative-missing-thermal` modes must retain a
valid rejection with no `samples.started` event.

This repository supplies only the software gate. Hardware procurement and
configuration, runner-group assignment, the read-only
`OXHQ_BENCHMARK_PROOF_TOKEN`, sensor permissions, and a live production dispatch
require external administrator authority. The token belongs only in the
`Pliego dedicated benchmarks` GitHub Environment, whose deployment-branch
policy must allow protected `main` and no other ref. Give it only the read-only
organization `Self-hosted runners` permission, with no repository or mutation
scopes. The collector never logs or persists the token and removes it from the
child benchmark process. OXH-336 therefore remains incomplete until those
external steps and their hosted proof exist.

## Protocol (from `manifest.toml`)

* 10 warm-up iterations, 50 samples for short documents, 20 for long ones.
* Random order between runners (seed recorded). Raw samples stored.
* Same host, same binary, same fonts/assets. Network disabled.
* Results record host info and versions; a baseline is signed by commit/tag.
* All aggregate and observer percentiles use `nearest-rank-v1`.

## Metrics

This foundation records wall latency (p50/p95/p99/min/max/mean), serial
throughput, per-page wall time, PDF and artifact bytes, page count, required
text, capture status, PDF hash variation, and typed failure publication state.
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
