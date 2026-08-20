# Benchmark methodology

Pliego's benchmark work is designed to answer two separate questions:

1. Does a candidate preserve the required document result?
2. For results that pass that gate, what time and resource cost does each supported
   execution model have?

The repository contains an implemented Pliego engine harness, pinned fixture
protocol, and a first target-neutral competitor slice for dompdf and Browsershot.
Only `minimal-static` has a competitor correctness mapping. All timing remains
N/A until canonical Poppler identities are pinned; competitor timing also needs
externally attested immutable images. Every other competitor/fixture pair is an
explicit `not-applicable` record with a reason. The repository contains **no
committed publishable performance results**, so no comparative speed, memory, or CPU
conclusion should be inferred from this document.

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
  --out benchmarks/baselines/pliego-0.1.1-linux-x86_64.json

python3 benchmarks/tools/validate_result.py \
  benchmarks/baselines/pliego-0.1.1-linux-x86_64.json
```

`resolve_release.py` verifies the release archive and binary against the hashes in
`benchmarks/manifest.toml`. Until the manifest pins the canonical Poppler runtime,
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
out-of-process image attestation path exists. The hosted benchmark workflow is configured
to install both locked graphs and directly render
`minimal-static` through each adapter, then runs real Poppler checks. That is a
correctness smoke, not measurement evidence; hosted execution of the new smoke
must still pass at the exact commit before it counts as hosted proof.

After immutable images are pinned, each target uses the same order: one discarded correctness preflight, discarded
warmups, then cold one-shot timed samples. The adapter root and every descendant
(including PHP, Node, and Chromium) remain in the existing retained cgroup-v2
accounting subtree. After timing, every output passes the same Poppler-based oracle:
PDF envelope/parser acceptance, A4 dimensions, one page, complete normalized
text, one embedded Ahem family, a shared normalized raster signature, and the
authored link target. A target is never marked supported unless every timed sample
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
The current commands run one target at a time. The committed seed randomizes
fixture traversal within each target, but not target order between individual
samples. These commands are implementation proof, not a publishable cross-engine
report; the next Linux orchestration gate must interleave target order with that
seed.

## Implemented slice and remaining comparative protocol

| Target | Runner status | Eligible public claim today |
| --- | --- | --- |
| Pliego v0.1.1 | Implemented and pinned | Harness can be reproduced; no committed result |
| Pliego candidate | Stable-outcome parity comparator only; arbitrary candidate performance runs are not implemented | Parity can be checked locally; no candidate performance claim |
| dompdf 3.1.6 | Locked one-shot adapter; configured Ubuntu/Poppler smoke | Timing N/A pending image attestation and oracle pins |
| Browsershot 5.4.0 + Puppeteer 25.8.0 | Locked one-shot adapter; configured network-isolated Ubuntu/Poppler smoke | Timing N/A pending image attestation and oracle pins |

Before a cross-engine comparison is published, every target must follow these rules:

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

The remaining dedicated-Linux gates are seeded cross-target sample
interleaving, descendant-drain-inclusive throughput, and automatic traceability
from every report cell to retained raw samples. Until those pass, the repository
publishes no comparative numbers.

## Publication gate

A benchmark report is publishable only when a fresh checkout can verify target
identity, regenerate fixtures, reproduce the commands, validate every included
result from the exact recorded clean harness commit, verify the retained oracle
script and all four Poppler executable identities, and trace each chart or table
cell back to raw data. If a target runner or
correctness adapter is incomplete, the report must say so and omit that comparison.

Funding the missing comparator adapters or dedicated measurement infrastructure does
not predetermine a result. The project will retain and report negative, neutral, or
inconclusive findings under the same protocol.
