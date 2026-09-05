# Bounded native regression comparison

This is a separate **minimal-static** candidate-versus-published-v0.3.3 track.
It is not part of the Aureus/Invobook business-document denominators, and it
does not establish ERP capacity, browser equivalence, or worker scaling.

The new coordinator reuses the existing PHP API2 runner for direct serial
renders. A small PHP broker uses that same runner's staging and `run_engine`
functions for a fixed two-child launcher. There is one sampler invocation and
one owned cgroup for a batch. The existing exclusive sampler lock, cgroup
schema, SDK and native renderer remain unchanged.

## Boundaries and eligibility

- Frozen `minimal-static/comparator.html` and original Ahem bytes; no HTML repair.
- Strict canonical request/result, scene, bundle, resource and PDF closure;
  the existing complete text/page/font/raster oracle checks every PDF.
- Requests must remain byte-identical across both targets and populations.
- Each native child has its own identity-bound `sandbox/{job,temporary}`.
  Pre/post storage identities and raw streams remain in the archive.
- The launcher records native PID/start-ticks, actual executable device/inode,
  owned cgroup membership and one point with **both pidfds still live**. This
  proves overlapping process lifetimes, not simultaneous CPU instructions.
- Executable hashing, staging, PDF oracles and evidence copying are untimed.
  Native deadline is 60,000ms; measured root deadline is 65,000ms. The 180-second
  outer coordinator bound is an emergency campaign abort, not a retry budget.
- An absent overlap witness, failed child, output overflow, timeout, changed
  storage, uncertain cleanup or failed oracle stops the campaign. No retry or
  later observation is scheduled. Failed raw attempts stay retained.
- A successful batch needs both complete outputs and clean cgroup drain.
  Descendant kill/drain remains the existing sampler's responsibility.
  A cleanly drained nonzero native result still retains both staged job trees,
  requests and available raw worker outcomes before success is rejected. Unknown
  cleanup never authorizes racing reads or an oracle against potentially live jobs.

Run preflight first. The four observations are one serial and one two-worker
batch for each native target (six PDFs). No warmup or timing population runs by
default. A failed preflight returns a nonzero exit and retains its prefix.

## Hosted setup and commands

Use the same isolated Linux x86_64 root-broker service as the existing benchmark:
root-owned immutable checkout and executables; the dedicated non-root
`pliego-benchmark-engine` account; delegated cgroup v2; controlled ext4 staging
root with existing sync/dirsync/noatime requirements; root-bound tmpfs capture;
the pinned Python/PDF oracle dependencies; network isolation; and an outer
systemd `KillMode=control-group` service. Preserve `GITHUB_ACTIONS` and
`GITHUB_RUN_ID` in that service for honest hosted classification.

The launcher must have executable mode (`chmod 0755` during checked hosted
setup, or Git mode 100755 when committed). Its interpreter is the separately
hash-bound `/usr/bin/python3`. No Rust build or application/database boot occurs.

Resolve the candidate with `resolve_development_candidate.py` from the exact
retained Linux package artifact, and the baseline with
`benchmarks/tools/resolve_release.py --target pliego-0.3.3 --metadata-out ...`.
Both existing resolvers verify package bytes before this coordinator is invoked.
The candidate metadata must identify native 0.4.0, not a locally rebuilt 0.3.3.
Package retention alone does not prove the final release matrix passed.

The manual `pliego-native-regression.yml` workflow is preflight-only: its only
inputs are the exact candidate artifact and source SHA. It downloads the
published baseline without an app install, first runs six synthetic Linux
lifecycle controls, then retains the six-PDF native preflight and its offline
verification. It has a 900-second owned-service bound and a 35-minute job bound.
There is no timed-mode dispatch input or automatically generated acceptance.

```sh
python benchmarks/integration/run_native_regression.py \
  --candidate /verified/candidate/pliego \
  --candidate-metadata /verified/candidate/candidate-identity.json \
  --baseline /verified/v033/pliego \
  --baseline-metadata /verified/v033/verified-release.json \
  --php /usr/bin/php --repeat 1 --out /evidence/native-preflight-001

python benchmarks/integration/run_native_regression.py \
  --verify /evidence/native-preflight-001
```

Archive verification is read-only, rechecks native/artifact identities and
re-runs the exact pinned PDF oracle. It uses the original campaign's source
closure; do not verify old records with silently changed verifier bytes.
Runtime qualification and archive verification explicitly reject Python `-O`;
optimized portable tests check fail-closed entry guards, not optimized evidence
qualification through dependencies whose assertions may be required.

Before timed execution, independently review final-candidate correctness and
the Linux lifecycle controls: one child failure, one hung child, early launcher
exit/remaining descendant, bounded-output overflow, scoped cleanup, and the
unchanged `CGROUP_BUSY` behavior for a competing sampler. These are **not yet
provided by the portable unit tests**. Do not set an acceptance field merely
because the source tests passed.

`test_native_batch_lifecycle.py` runs the actual fixed launcher and unchanged
sampler against an immutable CPython executable with generated scripts in two
separate owned jobs. The scripts intentionally fail, hang, kill their launcher
while leaving a descendant, overflow output, or recover; a held real sampler
lock tests `CGROUP_BUSY`. These are explicitly synthetic control records, not
PDFs, native timing observations or manufactured native overlap. Their shorter
1,000/5,000ms test deadlines do not change the native campaign's 65,000ms bound.
Raw command/output, process identities and both job trees are retained only
after scoped cleanup is established. An outer timeout stops the control suite.

Only after those gates, retain an owner-reviewed JSON acceptance record:

```json
{
  "schema": "pliego.native-regression.v1.acceptance",
  "identity_sha256": "<exact preflight summary identity>",
  "preflight_directory": "/evidence/native-preflight-001",
  "preflight_manifest_sha256": "<exact manifest byte hash>",
  "reviewed": true,
  "linux_lifecycle_controls_reviewed": true,
  "evidence": "<retained review and lifecycle proof reference>"
}
```

The same run command with `--mode timed --acceptance /evidence/acceptance.json`
executes fresh preflights before warmups/timing. Each repeat is one independently
scheduled host job; use repeats 1, 2 and 3 and keep all three separately.
The exact reviewed preflight is copied into `acceptance-preflight/` inside the
timed archive. Offline verification rechecks the review flags, identity, manifest
hash, successful preflight mode and full proof using that local copy, not the
original host's absolute path. A missing, failed or differently identified
preflight cannot qualify a timed archive after hash resealing.

| Population, per target per repeat | Discarded warmup | Timed observations | Timed PDFs |
| --- | ---: | ---: | ---: |
| Direct native serial | 10 | 100 single roots | 100 |
| Fixed concurrency 2 | 5 two-job batches | 50 launcher/tree roots | 100 |

Across two targets and three repeats this is 1,200 timed PDFs, not 1,200 batch
observations. Batch p50/p95 use 50 observations; only serial has 100-observation
p99. Whole-batch peak memory/CPU/I/O are not divided by two. Throughput explicitly
uses the complete sampler-lifecycle boundary. Root wall, descendant drain and
sampler lifecycle remain distinct in retained measurement records. Ratios are
candidate versus v0.3.3 **within** each population; no cross-population scaling
ratio is calculated. Incomplete/incorrect populations have no aggregate ratio.

## Source tests

```sh
python benchmarks/integration/test_native_regression.py
python -O benchmarks/integration/test_native_regression.py
python benchmarks/integration/test_native_batch_lifecycle.py --self-test
php -l benchmarks/integration/native_batch_broker.php
ruff check benchmarks/integration/native_batch_launcher.py \
  benchmarks/integration/run_native_regression.py \
  benchmarks/integration/test_native_regression.py
```

These portable tests exercise corruption guards, strict counts/percentiles,
storage aliases, bound raw streams, actual-exec witness predicates, outer-timeout
retention and optimization-safe rejection. Synthetic sampler/worker records in
the tests are explicitly not native overlap or measurement proof.
