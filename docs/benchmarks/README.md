# Benchmark methodology

Pliego's benchmark work is designed to answer two separate questions:

1. Does a candidate preserve the required document result?
2. For results that pass that gate, what time and resource cost does each supported
   execution model have?

The repository currently contains an implemented Pliego engine harness and pinned
fixture protocol. It does **not** contain committed publishable performance results,
and it does not yet contain implemented dompdf or Browsershot runners. No comparative
speed, memory, or CPU conclusion should be inferred from this document.

The detailed harness contract, fixture inventory, metric definitions, and target
manifest live in the [benchmark source directory](../../benchmarks/README.md).

## Reproduce the implemented Pliego baseline

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
`benchmarks/manifest.toml`. `run_benchmark.py` applies the committed warm-up, sample,
randomization, and cgroup protocol. `--dedicated` records the host assertion and
requires the benchmark source tree to be clean. A local output file is not a
published result; review it together with the verified-release metadata and host
evidence.

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

## Planned comparative protocol

| Target | Runner status | Eligible public claim today |
| --- | --- | --- |
| Pliego v0.1.1 | Implemented and pinned | Harness can be reproduced; no committed result |
| Pliego candidate | Stable-outcome parity comparator only; arbitrary candidate performance runs are not implemented | Parity can be checked locally; no candidate performance claim |
| dompdf | Not implemented | Methodology proposal only |
| Browsershot | Not implemented | Methodology proposal only |

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

## Publication gate

A benchmark report is publishable only when a fresh checkout can verify target
identity, regenerate fixtures, reproduce the commands, validate every included
result, and trace each chart or table cell back to raw data. If a target runner or
correctness adapter is incomplete, the report must say so and omit that comparison.

Funding the missing comparator adapters or dedicated measurement infrastructure does
not predetermine a result. The project will retain and report negative, neutral, or
inconclusive findings under the same protocol.
