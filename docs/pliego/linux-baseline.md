# Pliego Linux baseline

This baseline proves that the unmodified Servo source in the Pliego fork can
bootstrap, build, and pass Servo's smoketest on a clean GitHub-hosted Linux
runner. It does not claim byte-for-byte reproducible artifacts.

## Pinned baseline

- Fork base: `313b6d5ecc113b08010ce434140db3ca5abcc71c`
- Runner: `ubuntu-22.04`
- Rust: pinned by `rust-toolchain.toml`
- Python: pinned by `.python-version`
- Cargo dependencies: pinned by `Cargo.lock`
- Build profile: `checked-release`

The fork-owned workflow is `.github/workflows/pliego-linux-baseline.yml`. It is
triggered when that workflow file changes and can also be run manually. The
inherited Servo workflows remain unadopted while they are audited.

## Clean Linux x86_64 run

Install `uv` and `rustup` as described in Servo's `README.md`, then run the
same project commands used by CI:

```bash
./mach bootstrap --yes --skip-lints --skip-nextest
cargo install --path support/crown
./mach build --use-crown --locked --profile checked-release
xvfb-run ./mach smoketest --profile checked-release
```

Trigger the hosted baseline for a pushed branch with:

```bash
gh workflow run pliego-linux-baseline.yml \
  --repo oxhq/pliego \
  --ref <branch>
```

The retained `pliego-linux-baseline-<sha>` artifact contains the bootstrap,
Crown, build, and smoketest logs, plus the exact source SHA, runner image,
toolchain versions, operating system, and disk usage. Evidence is retained for
seven days.

## Proven run

[GitHub Actions run 29381566622](https://github.com/oxhq/pliego/actions/runs/29381566622)
passed on 2026-07-15 for commit
`4e157c21233dd0309c13b342584639fae1213279`:

- Servo base: `313b6d5ecc113b08010ce434140db3ca5abcc71c`
- Runner image: `ubuntu22` version `20260705.219.1` (Ubuntu 22.04.5 LTS)
- Rust/Cargo: `1.95.0`
- Python: `3.11.15`
- uv: `0.11.16`
- Bootstrap, Crown install, checked-release build, and smoketest: passed
- Cargo build time: 23m49s; complete job time: 26m17s
- Disk available before/after build: 85 GB / 80 GB
- Retained artifact:
  `pliego-linux-baseline-4e157c21233dd0309c13b342584639fae1213279`
  (expires 2026-07-22)

## Selected upstream test gate

After the unchanged checked-release build and smoketest, the same clean runner
executes the Linux script, unit, clippy, and tidy checks used by Servo CI:

```bash
./mach test-scripts
./mach test-unit --profile checked-release --nextest-profile ci
./mach clippy --locked --github-annotations -- -- --deny warnings
./mach test-tidy --no-progress --all --github-annotations
```

The workflow retains one log per command and Nextest's JUnit report. A failed
command fails the job; no upstream failure is suppressed or fixed here.

At Servo base `313b6d5ecc113b08010ce434140db3ca5abcc71c`, the selected gate
passed in both upstream runs:

- [Merge-group run 29372601544](https://github.com/servo/servo/actions/runs/29372601544)
- [Push run 29377645459](https://github.com/servo/servo/actions/runs/29377645459)

In both, 56 script tests and 941 unit tests passed, while clippy and tidy each
passed as one command-level check. The latter run still failed in full WPT on
`css/css-values/rex-invalidation.html`; a `same-document-refresh.html` result
was classified as flaky. Both are outside this ticket's selected gate.

Those upstream self-hosted runs skipped shellcheck and the tshark-backed script
check. The fork-owned hosted job installs both, an intentional coverage delta
that is recorded with their versions.

## Scope boundary

This job intentionally has no cache, secrets, write permission, WPT, unit-test
matrix, package publication, or Pliego rendering code. Those belong to later
milestones after the unchanged fork baseline is proven.

## Windows checkout note

Servo's WPT tree contains paths longer than the legacy Windows limit. Configure
long-path support in the clone before restoring the worktree:

```powershell
git config core.longpaths true
```

This is a checkout requirement, not a source regression.
