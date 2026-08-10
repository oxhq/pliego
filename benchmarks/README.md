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
│   ├── benchmark-host-proof.v1.json
│   ├── benchmark-observer-proof.v1.json
│   ├── benchmark-publication-attestation.v1.json
│   └── benchmark-setup-diagnostic.v1.json
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
│   ├── run_benchmark.py       Unprivileged orchestrator: manifest → staged candidate
│   ├── create_publication_attestation.py Protected-context publication MAC
│   ├── publish_benchmark.py   Host-proof binding and atomic baseline replacement
│   ├── publication_promotion.py Exact-run artifact verification and PR preparation
│   ├── observer_ab.py         Host-bound full-record observer A/B proof
│   ├── benchmark_setup_evidence.py Always-retained setup status
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
* A fixed root-owned broker at `/usr/local/libexec/pliego-cgroup-broker`, whose
  mode and SHA-256 match the checked-out broker source, in a cgroup-v2 domain
  parent delegated by the host service with
  `cpu`, `io`, `memory`, and `pids` enabled, plus a fixed non-root account named
  `pliego-benchmark-engine`. The broker must run in the parent's sole direct
  child, `harness`; set `PLIEGO_BENCHMARK_CGROUP_PARENT` to the canonical,
  empty root-owned parent. The runner must be a supplementary member of the
  engine group but have a different UID. The sampler does not provision the
  service/account. Its fixed `/usr/bin/python3 -I` entrypoint ignores Python
  environment injection before privileged code loads.
* A repository-shaped fixture mirror whose directories and files are root-owned
  and not group/other writable. Candidate input and cwd are opened from this
  mirror before launch; an unprivileged orchestrator cannot replace them.
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

## Staging and publishing a baseline

```sh
cache="$HOME/.cache/pliego-benchmarks"
binary="$(python3 benchmarks/tools/resolve_release.py \
  --cache "$cache" --metadata-out "$cache/verified-release.json")"
candidate="$(realpath /var/tmp/pliego-benchmark/candidate.json)"
frozen="$(realpath /var/lib/pliego-benchmark-fixtures/current)"
proof="$(realpath /var/tmp/pliego-benchmark/host-proof)"
observer_raw="$(realpath -m /var/tmp/pliego-benchmark/observer-measurements.json)"
observer_proof="$(realpath -m /var/tmp/pliego-benchmark/observer-proof.json)"
attestation="/var/lib/pliego-benchmark-attestations/${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}.json"

python3 benchmarks/tools/benchmark_host_preflight.py \
  --mode production --output-dir "$proof" \
  -- python3 benchmarks/tools/run_benchmark.py \
    --binary "$binary" \
    --frozen-fixture-root "$frozen" \
    --staging-out "$candidate" \
    --failure-evidence-dir /var/tmp/pliego-benchmark/failures/run \
    --observer-measurements-out "$observer_raw"

python3 benchmarks/tools/observer_ab.py bind \
  --measurements "$observer_raw" \
  --host-proof "$proof/benchmark-host-proof.v1.json" \
  --out "$observer_proof"

attestation_staging="$(realpath -m "/var/tmp/pliego-benchmark/${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}.json")"
read -r -s -p "Protected publication HMAC key: " attestation_key_hex
printf '\n'
PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX="$attestation_key_hex" \
  python3 benchmarks/tools/create_publication_attestation.py \
  --candidate "$candidate" \
  --host-proof "$proof/benchmark-host-proof.v1.json" \
  --observer-proof "$observer_proof" \
  --output-basename pliego-0.1.1-linux-x86_64.json \
  --operation bootstrap \
  --out "$attestation_staging"
sudo install -d -o root -g root -m 0755 "$(dirname "$attestation")"
sudo install -o root -g root -m 0444 "$attestation_staging" "$attestation"

PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX="$attestation_key_hex" \
  python3 benchmarks/tools/publish_benchmark.py \
  --candidate "$candidate" \
  --host-proof "$proof/benchmark-host-proof.v1.json" \
  --observer-proof "$observer_proof" \
  --attestation "$attestation" \
  --out benchmarks/baselines/pliego-0.1.1-linux-x86_64.json \
  --operation bootstrap \
  --failure-evidence /var/tmp/pliego-benchmark/failures/publish
unset attestation_key_hex
```

The resolver accepts only the committed Linux x86_64 release name, size,
archive SHA-256, exact file set, binary SHA-256, native commit, and Servo build.
Use `--offline` after the verified archive is cached. The orchestrator checks
the binary digest again before starting a sample.

`--operation bootstrap` is an explicit, MAC-bound authorization for the first
creation only. It atomically refuses an existing basename. In the workflow that
file is still ephemeral until its verified promotion PR is merged; only then can
a later protected run use `replace`. Both operations are proposed through a PR,
and neither can escape the one canonical `benchmarks/baselines` destination.
The HMAC key reaches only the attestation, publisher, and clean verification
step shells and their exact Python entry points
(`create_publication_attestation.py`, `publish_benchmark.py`, and
`publication_promotion.py verify`). Each process consumes the key from its
environment before any possible child process; artifact staging, PR preparation,
and repository mutation never receive it.

Those commands describe the contract; publishable runs enter through the
manual dedicated-host workflow, which supplies the exact paths. Local
diagnostics may subset or override with `--fixture invoice-showcase`,
`--samples 50`, `--warmup 10`, `--php /usr/bin/php`, but that is not the
canonical publishable command. The orchestrator:

1. checks the fixture surface (inputs, fonts, chartjs prep) and the binary;
2. runs `runners/pliego.php` per fixture — warmup discarded, then samples;
3. aggregates p50/p95/p99/min/max/mean, determinism, correctness, failures;
4. validates the staged result against the exact frozen fixture and command
   context, including source/frozen asset-bundle hashes and file identities;
5. runs the retained 20-pair randomized observation-off/on prerequisite while
   still inside the dedicated-host gate;
6. atomically stages a supported candidate outside `benchmarks/baselines`
   (raw samples kept, not just averages).

It never runs the release binary with `--version`; version, commit, Servo build,
archive digest, and binary digest come from the pinned manifest and verified
release metadata. The orchestrator, PHP correctness verifier, PDF tools, and
candidate runtime all refuse root. Failed correctness is retained under a
separate attempt directory and cannot replace an official baseline. Its sample
outputs are moved into that attempt, rejected if they contain links or special
files, and covered by a SHA-256 manifest. The runner makes every entry it owns
read-only; the dedicated workflow then seals the full attempt root-owned.
The publisher revalidates the staged candidate, host-proof bundle, and
separately bound observer proof. Those self-contained files prove only the
software gate. Setting `host.dedicated=true` additionally requires a root-owned
attestation under `/var/lib/pliego-benchmark-attestations` whose HMAC uses a
256-bit context key supplied only through the protected GitHub Environment. The
candidate, engine, and host-wrapped benchmark command never receive that key, so
rehashing or editing their own bundle cannot authenticate it. The publisher cross-binds the
GitHub run/job/workflow, runner, revision, candidate, observer, host proof, and
output basename. Official publication binds the canonical `baselines` parent
with a non-following directory descriptor, fsyncs the new temporary, and
hard-links the previous single-link baseline into a same-directory rollback
journal before replacement. Detected post-rename authority loss restores the
old baseline; abrupt process death can leave its old bytes in the rollback
journal for recovery, as can a best-effort journal cleanup failure after the
new baseline is already durable. It never resolves the destination leaf or redirects a
write through a swapped path.

The dedicated/root-capable job keeps `contents: read` and uploads an immutable
source artifact whose exact ID, archive digest, run, attempt, revision,
operation, schema, basename, and subject digests are checked by a fresh
GitHub-hosted verifier. That verifier has the HMAC but no repository-write token
and emits a new two-file verified artifact. A separate promotion job has no HMAC
or host proof token. It rechecks the verified artifact by exact ID and digest,
checks bootstrap absence or the replacement's exact old mode-`100644` Git blob,
and prepares only `benchmarks/baselines/pliego-0.1.1-linux-x86_64.json`.
Its write token is exposed only to the final mutation step, which creates or
reuses a run-specific branch based on the benchmarked `main` SHA and opens a
reviewable PR; it never pushes `main`. Both bootstrap and replace follow this
lifecycle. If `main` advances before a new PR is opened, the job refuses stale
evidence and requires a new benchmark run. Exact open or merged retries are
idempotent; a closed PR, divergent branch, extra path, or old-blob mismatch is a
hard refusal.

Each sample gets a fresh root-owned, non-delegated child cgroup. The narrow
broker opens the canonical executable, copies its bytes to a sealed memfd, and
binds cwd, input, workspace, output directory, and artifact directory by file
descriptor.
It never executes candidate/version/PDF inspection while privileged. A stopped
launcher enters a staging cgroup, creates a private network namespace, drops
supplementary groups, all real/effective/saved IDs, every capability set and
bounding capability, sets `no_new_privs`, and proves both path-based and
inherited-FD cgroup migration writes fail. The broker then moves that exact
PID/start identity into an empty measurement leaf and starts it with
`execveat(AT_EMPTY_PATH)` from the sealed bytes. Output arguments are rewritten
to inherited `/proc/self/fd/...` handles; stdout/stderr use bounded anonymous
captures. `SIGKILL`-on-parent-death is re-armed and verified after the identity
drop. All descendants, including new sessions, remain contained.

`root_wall_ms` ends when the root process exits. `tree_wall_ms`, the latency
published as sample `wall_ms`, ends only when `cgroup.events` reports
`populated=0`. After that boundary, the broker flushes the already-bound output
filesystem and retains `measurement_complete_ms` only after dirty/writeback are
zero and two interval-separated `cpu.stat`/`io.stat` reads match. The final
`cpu.stat`, `io.stat`, `memory.current`, `memory.peak`, and `pids.peak` values
are the authoritative tree counters.

Publication fails unless `cgroup.events` drains recursively. After it empties,
`memory.stat` dirty/writeback must reach zero and two interval-separated
`cpu.stat` and `io.stat` reads must match. A bounded `cgroup.kill` cleans leaked
descendants, but its use fails a passing sample. Launcher setup and privilege
removal happen in the staging leaf, so the measurement leaf starts with only
the stopped unprivileged launcher and zero CPU/I/O/memory counters before exec.

Periodic `/proc` PID/start-time, membership, RSS, and PSS observations are
retained only as sequential time-smeared diagnostics. They are neither
simultaneous bounds nor authoritative accounting, and their field names say so.
Short-lived processes may be missed there without weakening cgroup accounting.
As root, with `PLIEGO_BENCHMARK_CGROUP_PARENT` exported, run
`/usr/bin/python3 benchmarks/tools/test_process_tree_sampler.py --live --php-integration`
inside the delegated `harness` child for the containment, cleanup, counter, and
PHP-to-Python proof. On a dedicated benchmark host, add
`--acceptance-overhead` for the 20-pair randomized observation-off/on gate. The
retained proof stores and revalidates all 40 raw broker records, then compares
normalized UID/GID/groups/capability state, namespace,
migration denials, executable and frozen-input identities, cwd, cgroup
boundary, canonical argv after substituting only fresh output paths, quiet
successful outcome, and static cleanup/accounting controls for every pair. The declared
differences are limited to observation diagnostics, timing/accounting values,
and fresh per-run cgroup/process/workspace identities. Cgroup path,
`(PID,start_ticks)`, and workspace/output/artifact FD identities must be
globally unique across all 40 executions. It uses the protocol's
`nearest-rank-v1` percentiles and requires p95 wall overhead below 2%; sampler
CPU share remains a separate diagnostic.

## Dedicated-host proof

Publishable timing runs must enter through the manual-only
`Pliego dedicated benchmark host` workflow. Production dispatches select runner
group `Pliego dedicated benchmarks` with labels `self-hosted`, `Linux`, `X64`,
and `pliego-benchmark-pinned-v1`. The wrapper rejects before the chronology's
`samples.started` event unless the run is on protected `main`, the workflow,
checkout, and live branch all name one immutable SHA, and the GitHub API proves
the exact online/busy runner, group, and labels. Accepted production evidence
also binds the exact
canonical full `run_benchmark.py` argv, its staged-candidate SHA-256 and byte
count, the raw observer-measurement digest, and the host chronology to that
clean checkout. The finalized observer proof is additionally bound to that
host-proof manifest digest, revision, runner, host-config digest, and matching
source/installed broker digests. It is a publication prerequisite, not a
substitute for timing the candidate itself.

Neither `evidence_source: live` nor a regenerated `SHA256SUMS` grants
publication authority. The protected-host attestation and independently
supplied protected-environment authentication context are mandatory. Without
that external context, local and fixture results remain software-gate-only and
the publisher cannot set `dedicated=true`.

The host administrator owns `/etc/pliego-benchmark-host.v1.json`; the live gate
requires its canonical file to be `root:root` and not group/other writable. Its
pinned values must describe the real host; this abbreviated example shows the
complete contract shape:

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
command stdout/stderr, diagnostics, and `SHA256SUMS`; successful production
attempts additionally retain the digest-bound staged candidate and atomically
published baseline. The workflow's controlled
`negative-github-hosted` and `negative-missing-thermal` modes must retain a
valid rejection with no `samples.started` event.

This repository supplies only the software gate; no dedicated-host publication
or promotion was proved locally. Hardware procurement and configuration,
runner-group assignment, the read-only
`OXHQ_BENCHMARK_PROOF_TOKEN`, the 256-bit
`PLIEGO_BENCHMARK_ATTESTATION_HMAC_KEY_HEX`, sensor permissions, and a live
production dispatch require external administrator authority. Both dedicated
secrets belong only in the
`Pliego dedicated benchmarks` GitHub Environment, whose deployment-branch
policy must allow protected `main` and no other ref. Give it only the read-only
organization `Self-hosted runners` permission, with no repository or mutation
scopes. The collector never logs or persists either secret and removes both from
the child benchmark process.

Promotion additionally requires a separate `Pliego benchmark promotion`
Environment containing `OXHQ_BENCHMARK_PROMOTION_TOKEN`, issued to a narrowly
scoped, non-bypass automation identity with only the repository contents and
pull-request permissions needed to create the proposal branch and PR. Restrict
that Environment's deployment branch policy to protected `main`. `main` must
have an administrator-enforced require-PR/no-force-push ruleset that the identity
cannot bypass. Without that Environment, token, and ruleset the lifecycle is
not operationally authorized and must stay fail-closed. OXH-336 therefore
remains incomplete until all external steps and their hosted proof exist.

## Protocol (from `manifest.toml`)

* 10 warm-up iterations, 50 samples for short documents, 20 for long ones.
* Random order between runners (seed recorded). Raw samples stored.
* Same host, same binary, same fonts/assets. Network disabled.
* Results record host info and versions; publication is digest-bound to the
  exact revision and retained external host proof. No cryptographic signature
  is claimed.
* All aggregate and observer percentiles use `nearest-rank-v1`.

## Metrics

This foundation records wall latency (p50/p95/p99/min/max/mean), serial
throughput, per-page wall time, PDF and artifact bytes, page count, required
text, capture status, PDF hash variation, and typed failure publication state.
CPU, cgroup memory, and cgroup I/O are exact retained counters. Sequential
sampled summed RSS/PSS remain explicitly non-authoritative diagnostics. Runtime archive size and deeper document
checks remain separate audited increments before an externally authorized baseline is published.

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
* Page-count expectations for generated fixtures are pinned by the first accepted baseline.
* A full staged candidate bundles one validated result object per fixture.
  `run_benchmark.py` cannot write inside `benchmarks/baselines`; only
  `publish_benchmark.py` can atomically replace an official baseline after
  correctness, host identity, command identity, candidate digest, and retained
  host-proof evidence all validate.
