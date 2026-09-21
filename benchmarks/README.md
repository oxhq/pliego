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
│   ├── benchmark-interleaved-run.v1.json
│   ├── benchmark-hosted-comparison.v1.json
│   ├── benchmark-hosted-series.v1.json
│   ├── benchmark-evidence-archive.v1.json
│   ├── benchmark-report-data.v1.json
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
│   └── pliego.php             Target-neutral adapter loop + internal phase entrypoints
├── tools/
│   ├── generate_fixtures.py   Deterministic generation of long/image fixtures
│   ├── process_tree_sampler.py Linux cgroup-v2 containment and accounting
│   ├── pdf_oracle.py           Shared untimed PDF correctness checks
│   ├── report_data.py         Canonical latency cells + provenance-bound Markdown
│   ├── comparison_metrics.py Full metric aggregation for hosted comparisons
│   ├── run_benchmark.py       Orchestrator: manifest → runner → aggregates → result file
│   ├── run_comparison.py      GitHub-hosted three-target snapshot coordinator
│   ├── summarize_comparisons.py Sealed three-repeat spread report
│   ├── package_hosted_evidence.py Canonical durable series archive + validator
│   ├── test_process_tree_sampler.py Fixture, live cgroup, bridge, and overhead proof
│   ├── validate_interleaved_run.py Cross-target schedule/raw-sample validator
│   └── validate_result.py     Stdlib-only JSON Schema check for result files
├── baselines/                 Released-runtime baselines
└── reports/                   Comparison reports land here
```

These `v1` evidence schemas remain pre-publication contracts until the first
immutable hosted campaign is released. No earlier public evidence bundle is
accepted as a compatibility baseline for these required storage and
output-capture bindings.

## Prerequisites

* Authoritative baselines require dedicated or self-hosted **Linux x86_64,
  kernel 6.1 or newer**, with unified cgroup v2. GitHub-hosted Actions may
  produce explicitly labeled `github-hosted-exploratory` snapshots, but those
  snapshots cannot validate as dedicated evidence or support general production
  rankings. Linux 6.1 is required because the retained accounting contract
  requires both `memory.peak` and `pids.peak`.
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
* A dedicated root-owned mode-0711 ext4 directory whose `FS_NOATIME_FL`,
  `FS_SYNC_FL`, and `FS_DIRSYNC_FL` flags are set before any samples. Set
  `PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT` to that root. Every measured engine or PHP
  adapter receives a fresh mode-0700 child as `TMPDIR`; temporary database writes remain
  block-I/O-accounted while inherited no-atime and synchronous file/directory
  updates prevent scratch access or deletion from escaping the zero-dirty gate.
  Every target publishes its PDF below the same inherited-flags storage root,
  so synchronous publication cost is part of each measured target. Native API
  2 additionally keeps its job input and publication tree beside `TMPDIR` under
  one fresh inherited-flags sandbox; the sampler retains and validates those
  pre/post path identities in each sample. This binding assumes the exact
  hash-pinned renderer is trusted; it detects path replacement but is not a
  sandbox against a renderer deliberately attempting inode-reuse attacks.
  Engine stdout and stderr use a memory-backed approximation of production pipe
  transport: the root runner creates distinct mode-0600 files directly below
  canonical root-owned mode-01777 `/dev/shm`, and the sampler requires that mount
  to be `tmpfs`, opens each path with `O_NOFOLLOW|O_SYNC`, and retains matching
  owner, mode, device, inode, link-count, and post-run byte-count evidence. A
  sample is rejected if either stream exceeds 16 MiB; each file remains present
  until the sampler exits. Capture
  writes by the measured process tree are included in descendant CPU and the
  sampler-lifecycle wall interval; writes completed before the root exits are also
  inside engine wall time. Their shmem pages can contribute to cgroup memory, but
  their traffic is excluded from block-device `io.stat`. Capture-file creation and
  post-sampler read/unlink are outside `one_shot_wall_ms`; sampler binding and
  revalidation are inside it. Here `O_SYNC` describes synchronous tmpfs
  regular-file writes, not durable storage. Output volume remains target/protocol
  work, so these measurements are not renderer-core-only comparisons.
  Browsershot additionally receives fresh private `HOME` and XDG roots below
  that same measured scratch directory. Its adapter, PHP `TMPDIR`, explicit
  Chromium profile, artifacts, and PDF therefore remain on ext4 and contribute
  to block-device `io.stat`. Pliego and dompdf retain the exact
  `/nonexistent/pliego-benchmark-engine` account home and receive no XDG roots.
  The adapter gives Puppeteer an explicit fresh profile inside that tree so its
  normal temporary-profile deletion cannot make dirty file-backed pages
  unreachable. After Chromium returns, the measured adapter durability-syncs
  the private runtime tree, clears every runtime entry while preserving the
  bound private root and `HOME`/XDG directory identities, and syncs those
  deletions before publishing the PDF. The sampler independently requires those
  directories to remain bound and empty after the cgroup drains. Puppeteer 25.8.0
  unconditionally launches Chromium with `--disable-dev-shm-usage`; to prevent
  immediately unlinked Chromium shared-memory files from leaving an
  unobservable ext4 dirty tail, the root sampler also creates one protected
  private `/dev/shm/pliego-bench-shm-<32-hex>/tmp` directory and supplies it only
  as the Node/Chromium subprocess `TMPDIR`. The sampler proves the root,
  container, and empty-directory identities and exact entries before and after
  execution, then removes the hierarchy. Those shmem pages remain charged to
  cgroup memory, while their traffic is necessarily absent from block-device
  `io.stat`. Browser teardown and all CPU and wall cost stay inside the sample;
  block-I/O totals cover the ext4-backed Browser state listed above, not this
  disclosed memory-backed carve-out. The adapter remains responsible for its
  explicit file/tree durability sync. The sampler performs no path-based or
  browser-specific flush; the generic post-exit cgroup reclaim disclosed below
  applies identically to every target.
  The sampler and both hosted workflows reject any other passwd home and require
  that exact path to be absent or non-writable to the engine account.
  This is a deliberate non-default benchmark condition; its synchronous
  metadata cost remains included in the retained wall-time and block-device I/O totals.
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
  --out benchmarks/baselines/pliego-0.3.3-linux-x86_64.json
```

The resolver accepts only the committed Linux x86_64 release name, size,
archive SHA-256, exact file set, binary SHA-256, native commit, and Servo build.
It also verifies the byte-identical promoted `runtimes.json` retained under
`benchmarks/releases/`; the monorepo Laravel manifest deliberately remains a
non-publishable pre-promotion template.
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

## Running the GitHub-hosted comparison snapshot

`.github/workflows/pliego-performance.yml` is a manual, three-repeat measurement
lane. Each repeat uses one fresh `ubuntu-24.04` VM and runs all three targets in
the same job. It executes one correctness preflight, 10 discarded warmups, and
100 timed cold-process samples per target through the seeded global interleaving
schedule. One hundred samples are required so nearest-rank p99 is not merely the
maximum of the old 50-sample short-document population.

Every target, including Pliego and dompdf, runs in a fresh private network
namespace containing only loopback. The workflow provisions the existing root
cgroup-v2 broker and retains exact descendant CPU, `memory.peak`, block-device
`io.stat`, sampled RSS lower bounds, raw cadence-dependent PSS observations,
engine wall time, sampler-lifecycle one-shot wall time, output size,
correctness, and PDF-hash variation. It also records the GitHub run and runner
image, host and pressure snapshots, verified Pliego release metadata, Poppler
paths/versions/hashes, adapter/runtime paths/versions/hashes, the complete raw
schedule, and every sample ID.

PSS remains a raw diagnostic rather than a comparative aggregate. Its 250 ms
sampling cadence can miss a renderer that exits before the first observation,
so including it would make metric availability depend on VM speed and could
compute percentiles from a changing subset. The complete comparative memory
population uses exact cgroup `memory.peak`; sampled RSS remains an explicitly
lower-bound diagnostic for every timed sample.

Each repeat directory contains `interleaved-run.v1.json`,
`hosted-comparison.v1.json`, `all-metrics.md`, `verified-release.json`, and
`SHA256SUMS`. `run_comparison.py --validate DIRECTORY` requires that exact file
set, verifies the release metadata, checksums, and deterministic Markdown, then
recomputes the schedule, sample hashes, comparison digest, and every aggregate
from the raw samples. It rejects fewer than 100 samples, correctness failures,
partial-null comparative metrics, host-network access, cgroup counter
inconsistencies, renderer identity changes, non-cgroup accounting, or any
attempt to mark the host as dedicated.

After all three jobs finish, `summarize_comparisons.py` requires repeats 1, 2,
and 3 from the same workflow run, revision, runner image, fixture, protocol,
oracle, and target identities. It retains every repeat rather than selecting the
best one, and emits `hosted-series.v1.json`, `all-repeats.md`, and `SHA256SUMS`
with per-metric p50 ranges and relative spread. Selected renderer runtimes,
dependency-tree hashes, and the GitHub runner image identity are captured, but
the runner is not a manifest-pinned OCI environment. That is one reason this
evidence remains directional rather than an authoritative baseline.

### Package the final hosted evidence

After downloading the final `pliego-hosted-performance-series-*` artifact as one
tree containing the series bundle and all three retained comparison bundles,
build the two release-ready evidence assets with:

```sh
python3 benchmarks/tools/package_hosted_evidence.py build \
  path/to/downloaded-series-artifact \
  --out path/to/evidence-assets
```

For GitHub run `RUN_ID`, attempt `ATTEMPT`, the command emits exactly:

```text
pliego-benchmark-v0.3.3-minimal-static-gh-run-RUN_ID-attempt-ATTEMPT.tar.gz
pliego-benchmark-v0.3.3-minimal-static-gh-run-RUN_ID-attempt-ATTEMPT.tar.gz.sha256
```

The intended non-latest tag is
`benchmark-v0.3.3-minimal-static-gh-RUN_ID-aATTEMPT`. Inside the archive, one
same-named root contains `evidence-manifest.v1.json`, `series/`, and
`repeats/repeat-{1,2,3}/`. The manifest binds the exact source revision, GitHub
run and attempt, Pliego v0.3.3, `minimal-static`, the GitHub-hosted evidence
class, every nested evidence seal, and every retained file hash and size.

Validate a downloaded pair without extracting it yourself:

```sh
python3 benchmarks/tools/package_hosted_evidence.py validate \
  path/to/pliego-benchmark-v0.3.3-minimal-static-gh-run-RUN_ID-attempt-ATTEMPT.tar.gz
```

Validation rejects links, hardlinks, special files, path traversal, duplicate or
case-colliding paths, unexpected entries, metadata drift, checksum/name/root
drift, and any nested bundle failure. It also rebuilds the canonical USTAR +
gzip stream and requires byte equality. Packaging is checksum-bound evidence;
it does not publish a GitHub release or activate repository release immutability.

### Public snapshot gate

The temporary Actions artifact is not the public source. After the three-repeat
tree passes its validators, `package_hosted_evidence.py` packages the complete
series and all three raw repeat bundles into the canonical archive named by its
evidence manifest. The release uses:

```text
benchmark-v0.3.3-minimal-static-gh-<run-id>-a<attempt>
```

and contains only the canonical `.tar.gz` asset and its `.sha256` companion.
Create that release as a non-latest draft and upload those two assets. Before
publishing, `prepublish` proves that the server-digested Actions ZIP derives the
archive, the lightweight tag targets the measured revision, the draft contains
exactly the two checksum-matching assets, and the benchmark is not the
repository Latest release. Repository release immutability is enabled for new
releases; after publication the public-surface gate requires GitHub to report
`immutable: true`, rechecks the non-latest boundary, downloads both assets
again, and verifies their bytes before the snapshot can be published.

Only after those exact assets exist may the buyer-facing snapshot be staged:

```bash
set -euo pipefail
repo=oxhq/pliego
run_id=RUN_ID
attempt=ATTEMPT
source_revision=SOURCE_REVISION
release_tag="benchmark-v0.3.3-minimal-static-gh-${run_id}-a${attempt}"
artifact_name="pliego-hosted-performance-series-${source_revision}-${run_id}-${attempt}"
archive="path/to/pliego-benchmark-v0.3.3-minimal-static-gh-run-${run_id}-attempt-${attempt}.tar.gz"
checksum="${archive}.sha256"
live="path/to/live-origin"
mkdir "$live"

api=(gh api --header 'Accept: application/vnd.github+json' \
  --header 'X-GitHub-Api-Version: 2026-03-10')
"${api[@]}" "repos/$repo/actions/runs/$run_id/attempts/$attempt" \
  > "$live/run.json"
"${api[@]}" --method GET "repos/$repo/actions/runs/$run_id/artifacts" \
  -f "name=$artifact_name" -f per_page=100 > "$live/artifact.json"
artifact_id=$(jq -er \
  --arg name "$artifact_name" \
  '.artifacts | map(select(.name == $name)) | select(length == 1) | .[0].id' \
  "$live/artifact.json")
"${api[@]}" "repos/$repo/actions/artifacts/$artifact_id/zip" \
  > "$live/artifact.zip"
"${api[@]}" --method POST "repos/$repo/git/refs" \
  -f "ref=refs/tags/$release_tag" -f "sha=$source_revision" \
  > "$live/tag-created.json"
"${api[@]}" "repos/$repo/git/ref/tags/$release_tag" > "$live/tag.json"

gh release create "$release_tag" "$archive" "$checksum" \
  --repo "$repo" --draft --verify-tag --latest=false \
  --title "Pliego v0.3.3 minimal-static hosted benchmark" \
  --notes "GitHub-hosted exploratory evidence; not a production ranking."
"${api[@]}" --paginate --slurp --method GET "repos/$repo/releases" \
  -f per_page=100 > "$live/release-pages.json"
draft_release_id=$(jq -er --arg tag "$release_tag" '
  [.[][] | select(.tag_name == $tag and .draft == true)]
  | select(length == 1) | .[0].id
' "$live/release-pages.json")
"${api[@]}" "repos/$repo/releases/$draft_release_id" \
  > "$live/draft-release.json"
"${api[@]}" "repos/$repo/releases/latest" > "$live/latest-release.json"

python3 benchmarks/tools/public_hosted_benchmark.py prepublish \
  "$archive" \
  --checksum "$checksum" \
  --run-metadata "$live/run.json" \
  --artifact-metadata "$live/artifact.json" \
  --artifact-zip "$live/artifact.zip" \
  --tag-metadata "$live/tag.json" \
  --draft-release-metadata "$live/draft-release.json" \
  --latest-release-metadata "$live/latest-release.json"

gh release edit "$release_tag" --repo "$repo" --draft=false --latest=false
"${api[@]}" "repos/$repo/git/ref/tags/$release_tag" > "$live/tag.json"
"${api[@]}" "repos/$repo/releases/tags/$release_tag" > "$live/release.json"
"${api[@]}" "repos/$repo/releases/latest" > "$live/latest-release.json"

python3 benchmarks/tools/public_hosted_benchmark.py stage \
  "$archive" \
  --checksum "$checksum" \
  --run-metadata "$live/run.json" \
  --artifact-metadata "$live/artifact.json" \
  --artifact-zip "$live/artifact.zip" \
  --tag-metadata "$live/tag.json" \
  --release-metadata "$live/release.json" \
  --latest-release-metadata "$live/latest-release.json"
```

The prepublish and staging commands do not accept numbers or prose. They run the
archive validator and prove the exact Actions run attempt, exact-name artifact
listing and ZIP, lightweight tag, non-latest state, and exact draft/final
release assets before copying the archive's
evidence manifest, sealed series, deterministic report, source-provenance
receipt, and exact checksum into `docs/benchmarks/results/`. It then derives the
compact README table from the sealed series. The public-surface check recomputes
that table and report. Hosted CI also downloads the named release assets and
requires byte-for-byte equality with the committed evidence view. Until that
gate passes, the README must continue to say that no performance snapshot is
committed.

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

The runner also has internal `preflight`, `warmup`, and indexed `timed` phase
entrypoints. `run_benchmark.py` owns a language-neutral
`pliego.cross-target-schedule.v1` primitive that ranks every target once per
round. The rank input is the ASCII encoding of compact JSON (no spaces, with
every non-ASCII code point JSON-escaped) for `["pliego.cross-target-schedule.v1", seed, fixture,
phase, iteration, target_id]`; entries sort by raw SHA-256 digest bytes and then
target-ID UTF-8 bytes. Its executor interleaves preflights, warmups, and timed
samples and fails if a returned timed index differs from the schedule. It emits
a `pliego.benchmark-interleaved-run` version 1 envelope marked
`publication_status = "prerequisite-only"`. Timed samples remain unmodified and
in global schedule order. Each is bound to its schedule position by a SHA-256
of the sample and a content-bound `sample_id`; an `artifact_sha256` seals the
complete envelope by content (it is not a signature or provenance proof).

Those hashes use ASCII JSON from Python's `json.dumps` with sorted keys,
`ensure_ascii=True`, `allow_nan=False`, and compact separators. The artifact
hash omits only its own `artifact_sha256` field. The sample ID hashes compact
JSON for `["pliego.benchmark-raw-sample-id.v1", fixture_id, target_id,
iteration, schedule_position, sample_sha256]`. This identity encoding is part
of the v1 contract. The standalone `validate_interleaved_run.py` rechecks the
envelope schema, every raw sample against the existing
`benchmark-result.v1#/definitions/sample` contract, the regenerated schedule,
every schedule/sample binding, and all hashes:

```sh
python3 benchmarks/tools/validate_interleaved_run.py path/to/interleaved-run.json
```

This is a retention and report-traceability primitive for the future comparison
coordinator, not a standalone publication command. The public CLI remains
single-target and retains all identity, oracle, dedicated-host, and N/A gates.

`report_data.py` implements the next deliberately narrow layer. It accepts one
validated interleaved artifact and emits `pliego.benchmark-report-data` version
1, also marked `prerequisite-only`. The only cells frozen in v1 are `wall_ms`
min/p50/p95/p99/max/mean for every scheduled target. Generation fails unless
every timed sample passes both the runner and correctness gates. Every cell
repeats the source artifact digest and lists all contributing sample IDs in
timed schedule order; its content-stable cell ID also binds artifact, fixture,
target, metric, and statistic. The source record binds the exact schedule by
digest. Both hashes use the v1 canonical JSON encoding above. A cell ID hashes
`["pliego.benchmark-report-cell-id.v1", artifact_sha256, fixture_id,
target_id, "wall_ms", statistic]`. Generation and validation use only synthetic
data in the contract self-test:

```sh
python3 benchmarks/tools/report_data.py generate \
  path/to/interleaved-run.json --out path/to/report-data.json
python3 benchmarks/tools/report_data.py validate \
  path/to/report-data.json --artifact path/to/interleaved-run.json
python3 benchmarks/tools/report_data.py render \
  path/to/report-data.json --artifact path/to/interleaved-run.json \
  --out path/to/latency-table.md
```

The validator rejects an invalid source artifact, source schedule/target/fixture
drift, stale artifact digests, non-canonical cell order, changed aggregates, and
missing, extra, reordered, or duplicate sample IDs. This contract is machine
report data, not authority to run an unattested target. The Markdown renderer
accepts only a report that validates against its exact interleaved artifact. It
copies only those cells, labels the output `prerequisite-only`, binds the artifact
and schedule digests, and links every displayed value to its full cell ID and the
contributing raw-sample IDs. It produces no ranking, narrative, chart, or
publication claim.

Each sample gets a fresh root-owned, non-delegated child cgroup. A root launcher
first stops in a staging cgroup, drops supplementary groups, all real/effective/
saved IDs, every capability set, and its bounding capabilities, then sets
`no_new_privs`. The broker verifies those `/proc` fields, PID/start identity,
executable hash/argv, and denied migration-interface writes before moving the
stopped launcher into the clean measurement leaf and starting engine wall time.
All later descendants, including new sessions, remain contained. The retained
final `cpu.stat`, `io.stat`, `memory.current`, `memory.peak`, and `pids.peak`
counters are the accounting source. Engine wall time ends with the root process.
The sampler also verifies that each private per-invocation file-backed
temporary directory is ext4-backed and carries inherited `FS_NOATIME_FL`,
`FS_SYNC_FL`, and
`FS_DIRSYNC_FL` both before launch and after descendant drain; that storage
remains on disk, inside the measured process tree, and is not replaced by
tmpfs. Each sample retains the centrally classified runtime target and
environment contract. Browsershot evidence additionally binds normalized
HOME/XDG child paths to unique filesystem device/inode identities before
launch and revalidates the same identities after descendant drain. It separately
retains the protected tmpfs Node/Chromium `TMPDIR` topology and exact empty-entry
state described above; non-browser samples must not carry either private-browser
proof.
Descendant drain and accounting-settle durations are recorded separately. The
runner also retains a sampler-lifecycle one-shot interval from process open
through sampler exit; it includes output-capture binding and revalidation but
excludes capture-file creation and post-sampler read/unlink. Serial throughput is
derived only from that boundary, so leaked or slow descendants cannot inflate
the rate.

The `minimal-static` oracle declares ISO A4 in points and permits at most 0.75
points of print-grid quantization. All text explicitly uses normal-weight Ahem.
The oracle requires the complete normalized document text, exactly one embedded
Ahem family, and a shared full-page 24x32 monochrome occupancy signature with
quantized ink area. It preserves page-relative position and scale within an
explicit coarse tolerance instead of cropping and rescaling the ink.
The same expectations apply to all targets.

Publication fails unless `cgroup.events` drains recursively. After it empties,
the sampler captures a pre-reclaim accounting snapshot. If cgroup-charged dirty
or writeback file memory remains, it issues exactly one cgroup-local
[`memory.reclaim`](https://docs.kernel.org/admin-guide/cgroup-v2.html#memory-reclaim)
request equal to that snapshot's `memory.current`. The retained
`accounting_settle.reclaim` evidence records the requested bytes, whether the
write completed or the kernel reported under-reclaim, and the before/after
current, dirty, and writeback values. A completed write does not claim that the
kernel reclaimed exactly the requested amount. There is no retry, global sync,
or target-specific exception.
This post-exit step may settle or discard engine-charged file cache while
preserving the already measured `memory.peak`; writeback caused by reclaim stays
charged in the final cgroup `io.stat`. Reclaim time is included in both
`accounting_settle.duration_ms` and the sampler-lifecycle `one_shot_wall_ms`.
Publication still requires final dirty/writeback zero and two
interval-separated, identical `cpu.stat` and `io.stat` observations. A bounded
`cgroup.kill` cleans leaked descendants, but its use fails a passing sample.
Launcher setup and privilege removal happen in the staging leaf, so the
measurement leaf starts with only the stopped unprivileged launcher and zero
CPU/block-I/O/memory counters before exec.

Periodic `/proc` PID/start-time, summed RSS, and summed PSS observations are
retained only as sampled lower-bound diagnostics; short-lived processes may be
missed there without weakening cgroup accounting. PSS is deliberately excluded
from comparative aggregates because its slower cadence does not cover every
timed process. As root, with
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
  The separate hosted-comparison profile uses 100 timed `minimal-static`
  samples per target to support a distinct nearest-rank p99 observation.
* Every sample is a cold, one-shot process. The committed seed randomizes
  fixture traversal within a target, preserving the existing `sample_order =
  "random"` protocol. The deterministic cross-target schedule and phase-aware
  execution primitive produces a versioned, self-digesting schedule and
  raw-sample envelope. The hosted coordinator retains real three-target
  executions; the authoritative CLI remains single-target until its stricter
  identity gates are satisfied.
* Same host, same binary, same fonts/assets. Authoritative runs disable network;
  hosted comparison samples use an isolated namespace containing loopback only.
* Results record host info, the exact clean harness commit, the oracle script,
  and all Poppler executable identities; validation requires the matching
  checkout. A baseline is signed by commit/tag.
* All aggregate and observer percentiles use `nearest-rank-v1`.

## Metrics

This foundation records wall latency (p50/p95/p99/min/max/mean), serial
throughput, per-page wall time, PDF and artifact bytes, page count, page
dimensions, required text, link targets, capture status, PDF hash variation,
and typed failure publication state.
CPU, cgroup memory, and cgroup block-device I/O from `io.stat` are exact retained
counters. The I/O counters exclude memory-backed stdout/stderr for every target
and the disclosed Browsershot-only Node/Chromium tmpfs `TMPDIR`; those pages
remain cgroup-memory accounted. In the hosted comparison, sampled summed RSS is
an explicitly lower-bound comparative metric while PSS observations remain
available only in the raw diagnostics. The canonical single-target benchmark
may retain PSS aggregates when every passing sample has an observation. Runtime
archive size and deeper document checks remain separate audited increments
before a signed baseline is published.

## Fixtures and correctness gates

Each fixture declares expected correctness in `manifest.toml`. A sample counts
toward performance only when its checks pass; a wrong result is not "faster".
Generated-fixture `page_count` targets were originally pinned on Linux v0.1.1
and revalidated unchanged through the published Linux v0.2.0 renderer. For the
v0.3.3 API 2 comparator, only `minimal-static` has passed the exact native API 2
and shared-oracle smoke so far. Every other fixture must independently pass its
declared v0.3.3 API 2 correctness gate before it can contribute a sample.

| Fixture | Category | Purpose | Expected |
| --- | --- | --- | --- |
| `minimal-static` | startup | pure startup, one local font, no scripts/images | A4, 1 page, exact text/font/raster |
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
* The cross-target executor now has a GitHub-hosted coordinator that binds real
  target identities, exact raw samples, full metrics, and a deterministic report.
  It is deliberately limited to the `github-hosted-exploratory` evidence class;
  no authoritative multi-target baseline has been retained.
* Dedicated-Linux acceptance still needs immutable competitor images, canonical
  Poppler pins, a genuine retained run under all authoritative gates, and its
  validated raw samples and report as one durable evidence bundle. The hosted
  series cannot substitute for that missing evidence. Its throughput includes
  sampler startup, descendant drain, accounting settlement, and sampler exit,
  and remains a serial per-target diagnostic rather than a concurrent-capacity
  claim.
* The Ubuntu Pliego/API 2, adapter, and Poppler lane now passes as hosted
  `minimal-static` correctness proof. It is not performance evidence.
* Core (Criterion) and Laravel e2e levels live outside this directory.
* Page-count expectations for generated fixtures are pinned by the first signed baseline.
* A multi-fixture `--out` file bundles one validated result object per fixture
  as a JSON array; each element conforms to `benchmark-result.v1.json`, the
  bundle itself is a container. Single-fixture runs write one result object.
