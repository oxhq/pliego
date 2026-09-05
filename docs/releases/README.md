# Pliego release procedure

Pliego's native archives embed their exact source commit, while the Laravel runtime
manifest embeds the hashes of those archives. The native release and Composer
packages therefore use an ordered release train instead of pretending one source
commit can contain its own archive hashes.

## Native release

1. Prepare the versioned native candidate. The monorepo Laravel manifest must keep
   `release_ready: false`, so an accidentally published source archive fails before
   downloading stale runtime metadata.
   Align the engine/core-benchmark Cargo manifests and lock entries, workspace
   Pliego dependency, both SDK `VERSION` files, Laravel's exact PHP requirement,
   runtime manifest version, and the invoice fixture's Composer aliases/lock.
   The Composer dry-run checks all fourteen coordinated version values. Servo's
   workspace version and the API/schema integers are independent; do not bump them.
2. Require the hosted package matrix to produce exactly four native bundles, four
   API 2 proofs, four render-supervisor proofs, and one Linux controlled-capture
   proof from one source commit.
3. Dispatch the promotion workflow with that successful run and matching version
   tag. It verifies the archives and their sidecars, generates
   `release_ready: true` `runtimes.json`, loads the release notes from the selected
   source commit, and creates a checksummed draft without rebuilding the archives.
4. Inspect the draft, then publish the native release. Promotion copies the notes
   from the exact native source, which can still describe an unreleased candidate.
   Review and update the **draft release body** with the completed native gates
   and still-pending Composer status before publication. Do not rebuild or replace
   native archives to change release prose. Finalize the source notes separately
   after the coordinated public-package and consumer evidence is available.
   Preserve the generated
   `runtimes.json` byte-for-byte for the Laravel package step.
   Verify GitHub's actual immutable-release setting separately before claiming
   host-enforced immutability; checksums and the append-only project policy alone
   are not that setting.

## Composer releases

1. Replace the monorepo Laravel manifest with the promoted `runtimes.json`. The
   Composer workflow must download the native release asset and prove the bundled
   file is byte-identical before its finalized gate can pass.
   In that explicit finalization change, update the bundled-manifest assertion in
   `sdk/laravel/tests/managed_runtime_test.php` from pending to finalized for the
   exact version. Do not merely permit either state or bypass
   `release:check-runtime`: its byte-identity and `release_ready: true` checks
   remain mandatory. Candidate package checks must reject premature finalization.
   Update the public-surface check's explicit candidate-state assertion in that
   same reviewed finalization change; it must continue requiring exact version and
   release evidence rather than treating a boolean alone as proof of publication.
2. Sync the PHP subtree to `oxhq/pliego-php`, require its public package workflow,
   tag the exact coordinated-version commit, and verify Packagist resolves that
   version.
3. Sync the Laravel subtree to `oxhq/pliego-laravel`. Before tagging, its public
   workflow must download the native `runtimes.json`, require
   `release_ready: true`, and compare it byte-for-byte with the bundled file.
4. Tag Laravel only after the matching PHP version resolves publicly, then verify
   Packagist and run the versioned Laravel consumer through install, doctor, render,
   typed failure, PDF, storage/retrieval, and artifact checks.

Before changing the public recommendation, retain fresh Linux and desktop
public-dist consumer evidence, the v0.3.3-to-candidate update/rollback rehearsal,
and exact GitHub/Packagist source and distribution references. Source-path SDK
tests and development `workflow_dispatch` bundles are not those proofs.
Candidate notes must not say publication completed before these gates pass.

Do not reuse or mutate an existing tag or release. A local build, a green source
workflow, or a Composer ZIP staged from `release_ready: false` is not publication
proof.
