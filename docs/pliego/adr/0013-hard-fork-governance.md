# ADR 0013: Upstream-tracked hard-fork governance

- Status: Accepted
- Date: 2026-07-14

## Context

The normative source is *Pliego*, Draft 0.1 (2026-07-14), §§0, 17.1, 26, 44, 51, and §54 open
decision 13. It requires an ADR choosing Servo-main tracking or an LTS base before intentional
divergence.

At this decision, `oxhq/pliego` is a GitHub fork whose default `main` is the recorded Servo baseline.
This implementation checkout has only the `origin` remote, and the project has not yet created an
`upstream-main` mirror or `sync/*` branch. This ADR establishes their roles; the first auditable sync
will exercise them.

## Decision

### Base strategy

Pliego tracks Servo `main`, not a Servo LTS or release branch. This allows web-platform fixes and
security work to flow through one reviewable upstream history. A future `release/x.y` branch may
maintain a Pliego release, but it does not change the Servo base strategy.

### Branch and merge roles

- On adoption, `main` becomes the Pliego development branch.
- Once created, `upstream-main` must be an exact fast-forward mirror of `servo/servo` `main` and contain
  no Pliego work.
- Each sync uses a temporary `sync/servo-YYYY-MM-DD` branch created from `main` and deleted after its
  reviewed PR lands.

Upstream enters `main` through explicit merge commits of coherent ranges. Sync work does not
permanently rebase Pliego history or cherry-pick isolated web-platform changes without dependency
analysis. Each sync PR records the upstream range, security relevance, changed ownership zones,
conflicts and resolutions, WPT delta, Pliego reftest delta, performance delta, and follow-up work.

Conflict resolutions remain manual and reviewable. Recurring conflict regions are recorded before
refactoring, upstreaming, or automating them. No permanent sync SLA is set until measured sync cost
exists.

### Divergence boundary

Preserve Servo's directory shape and avoid cosmetic renames, moves, formatting, or stylistic rewrites
in upstream-derived code. Keep dispatch-point edits small and put substantial Pliego behavior in
Pliego-owned modules.

Servo's continuous-layout path remains available for upstream testing and merge validation. Paged
layout is added beside it; shared changes must not silently replace or break the continuous path.

### Upstream contribution

Contribute a change to Servo when it is generic to embedders, fixes web-platform correctness, improves
both continuous and paged layout, adds a clean extension point, or improves security or stability.
Keep document-scene, PDF, readiness, page-sequence, protocol, SDK, artifact, and operational behavior
downstream when it is specific to Pliego.

### Pull requests

Project policy requires every PR changing upstream-derived code to complete the ownership-zone and
upstream-conflict-risk fields in [`PULL_REQUEST_TEMPLATE.md`](../../../PULL_REQUEST_TEMPLATE.md). The PR
also explains why the change cannot live in a Pliego-owned module, whether it should be contributed
upstream, and relevant test, WPT/reftest, performance, artifact/schema, and security impact. GitHub does
not enforce these fields automatically; maintainers do not merge an upstream-derived change with
missing declarations.

## Consequences

- Pliego accepts regular Servo-main integration work instead of depending on a nominally stable LTS.
- Explicit merge commits and temporary sync branches preserve auditable upstream ranges.
- The ownership manifest and PR declarations expose likely conflict areas before review.
- Release-branch support policy, a permanent sync SLA, and automated conflict resolution remain
  undecided until measured evidence justifies them.

## References

- *Pliego*, Draft 0.1 (2026-07-14), §§0, 17.1, 26, 44, 51, and §54 open decision 13.
- [`ownership.toml`](../../../ownership.toml)
- [`PULL_REQUEST_TEMPLATE.md`](../../../PULL_REQUEST_TEMPLATE.md)
