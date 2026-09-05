# Real-document comparison: development gates

This is an executable comparison harness, **not published numerical evidence**.
The historical `minimal-static` benchmark remains a separate population.

`real_documents.json` pins three shared, repaired application inputs:

| Track | Document | Actual incumbent |
| --- | --- | --- |
| `invobook-simple-repaired` | One-page invoice | Browsershot 5.0.5; harness Puppeteer 25.8.0 |
| `aureus-ledger-300-repaired` | 300-entry General Ledger | dompdf 3.1.6 |
| `aureus-manufacturing-008-font-closed` | One-page manufacturing work order | dompdf 3.1.6 |

The work order is not a long operational report. These are internally operated
external applications, not independent adoption. Each corpus retains original
source, licenses, disclosed repairs, expected business facts and font provenance.
Template/font licenses remain separate from the engine's license; these files
do not belong in native runtime bundles or PHP/Laravel package archives.

## Correctness before timing

The coordinator renders the identical frozen HTML/fonts through both providers,
using the existing PHP runner and Linux cgroup sampler. It retains every attempt,
typed failure, process output, delivered PDF, resource accounting and oracle
report. Business facts, geometry, font programs/mappings and native bundle
integrity must pass. Actual PDF visual review is a separate required step.

Current native font proof uses the v2 used-glyph policy: Type0/Identity-H fonts,
original source and sanitized/subset outline/metric closure, and exact ordered
per-page glyph/Unicode equality with the scene. A balanced inline ActualText
override qualifies only for one text-show containing one CID. Unused font/CMap
entries cannot manufacture painted evidence; unsupported encodings, invisible
text, nested or multi-glyph overrides and Form XObjects fail closed. Dedicated
corruption tests run before hosted campaigns. Older developmental v1 preflights
remain unchanged and use their exact preserved verifier; they do not satisfy
the current timing gate.

The default is two untimed preflights. Timed mode additionally requires a
`pliego.real-document-visual-acceptance.v1` record bound to the complete runtime,
corpus, tool and oracle identity, with reviewed PDF hashes and accepted layout
fingerprints for both providers. A new layout variant is not silently accepted.

Each planned repeat contains 10 warmups and 100 timed samples **per provider**,
interleaved with a fixed seed. Three repeats per family are separate populations;
do not pool families or choose the fastest repeat. Any failed warmup/timed sample
stops that campaign and preserves the unattempted denominator. Incomplete
populations have no qualified aggregate or speed ratio. Infrastructure failures
or unverified process cleanup stop immediately, including during preflight.

Root-process wall time, root-plus-descendant-drain time and sampler lifecycle
time are different measurements. CPU, memory and I/O use existing cgroup
accounting. Oracle execution and retained-output copying occur after measurement.
These are cold renderer processes, not end-to-end Laravel request, database,
Blade rendering or durable storage latency. Application/storage and comparative
concurrency proofs remain separate gates.

The inherited measurement recipe deliberately uses fresh per-attempt temporary
state on ext4 with synchronous file and directory updates (`FS_SYNC_FL` and
`FS_DIRSYNC_FL`). Pliego and dompdf retain the absent, non-writable engine-account
home. This **non-default storage condition** includes font preparation and
synchronous metadata/publication costs; it is not a typical warm-cache Laravel
deployment. Disclose it alongside every latency comparison, together with CPU,
whole-cgroup memory and I/O. See the [full sampler boundary](../README.md).
GitHub-hosted repeats are exploratory, not dedicated-host production rankings;
neither host variation nor a timeout alone proves its underlying cause.

## Hosted preflight

`pliego-real-document-preflight.yml` accepts an exact development Linux package
artifact ID and its full source SHA. The resolver verifies GitHub/archive hashes,
package contents and executable identity; the runner also checks contract
discovery. A development artifact is never promotion evidence, even when its
executable was compiled in release mode.

The workflow defaults to the two Aureus tracks, installs their explicitly
repaired lock without scripts/plugins, audits dependencies and runs in a bounded
delegated-cgroup service. Invobook is selectable separately, but its original
frozen dependency audit must pass before installation. There is no advisory
bypass or automatic unreviewed lock update. An audit failure is a setup outcome,
not a renderer benchmark failure.

The owned engine account, read-only closures, private network namespace and
process deadlines are part of this synthetic measurement recipe. They are not a
general hostile-HTML sandbox guarantee. All failed setup/preflight artifacts are
retained. The scoped upload includes hidden staging directories needed to explain
failures, but excludes application vendors, credentials and downloaded binaries.

Local pure checks and portable archive verification:

```sh
python benchmarks/integration/test_real_document_comparison.py -v
python benchmarks/integration/run_real_document_comparison.py --verify /retained/campaign
```

Use `real_document_requirements.txt` in a dedicated CPython 3.12 Linux x86-64
environment. It pins the complete wheel set by version/hash, including Pillow
11.3.0 for the manufacturing barcode oracle. Per-family READMEs document their
independent correctness checks. No timing or release-readiness claim follows
from passing these local tests alone.
