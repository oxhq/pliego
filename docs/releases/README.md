# Pliego release procedure

Pliego's native archives embed their exact source commit, while the Laravel runtime
manifest embeds the hashes of those archives. The native release and Composer
packages therefore use an ordered release train instead of pretending one source
commit can contain its own archive hashes.

## Native release

1. Prepare the versioned native candidate. The monorepo Laravel manifest must keep
   `release_ready: false`, so an accidentally published source archive fails before
   downloading stale runtime metadata.
2. Require the hosted package matrix to produce exactly four native bundles, four
   API 2 proofs, four render-supervisor proofs, and one Linux controlled-capture
   proof from one source commit.
3. Dispatch the promotion workflow with that successful run and matching version
   tag. It verifies the archives and their sidecars, generates
   `release_ready: true` `runtimes.json`, loads the release notes from the selected
   source commit, and creates an immutable draft.
4. Inspect the draft, then publish the native release. Preserve the generated
   `runtimes.json` byte-for-byte for the Laravel package step.

## Composer releases

1. Replace the monorepo Laravel manifest with the promoted `runtimes.json`. The
   Composer workflow must download the native release asset and prove the bundled
   file is byte-identical before its finalized gate can pass.
2. Sync the PHP subtree to `oxhq/pliego-php`, require its public package workflow,
   tag the exact v0.2 commit, and verify Packagist resolves that version.
3. Sync the Laravel subtree to `oxhq/pliego-laravel`. Before tagging, its public
   workflow must download the native `runtimes.json`, require
   `release_ready: true`, and compare it byte-for-byte with the bundled file.
4. Tag Laravel only after PHP 0.2 resolves publicly, then verify Packagist and run
   the versioned Laravel consumer through install, doctor, render, typed failure,
   PDF, and artifact checks.

Do not reuse or mutate an existing tag or release. A local build, a green source
workflow, or a Composer ZIP staged from `release_ready: false` is not publication
proof.
