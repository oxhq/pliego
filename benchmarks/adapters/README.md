# Benchmark adapter contract v1

Every timed target is one executable with two commands:

```text
ADAPTER identity
ADAPTER render INPUT --output PDF --artifacts DIR \
  --page-size WIDTHxHEIGHT --page-margins TOP,RIGHT,BOTTOM,LEFT
```

`identity` emits one JSON object using contract
`pliego.benchmark-adapter.v1`. It records the adapter, exact installed
dependency-tree snapshots, and every runtime executable used by the adapter
with canonical path, version, and SHA-256. A dependency-tree hash is only a
snapshot: publishable execution additionally requires a manifest-pinned OCI
image, exact expected hashes and canonical paths in the manifest, and read-only
mounts for every protected path. It also requires out-of-process proof of the
running image digest; an environment variable is provenance, not attestation.
Mutable `vendor/` or `node_modules/` directories are never described as
immutable evidence.

`render` renders exactly one cold document and exits. `INPUT` is one bare file
name resolved directly inside the supplied process cwd; absolute paths,
subdirectories, traversal, and symlink escapes are rejected. `PDF` and
`DIR` are prepared, adapter-writable absolute locations. A successful adapter
atomically publishes a nonempty PDF without replacing an existing output. A
failure exits nonzero and must not publish the requested PDF. Adapters do not
pool, daemonize, cache across samples, transform fixture HTML, or fetch network
resources. Browsershot samples additionally run inside a fresh Linux network
namespace whose only interface is `lo`; URL interception and Chromium flags are
defense in depth, not the containment boundary.

The target-neutral PHP runner launches this command through the cgroup-v2
sampler. The adapter and all children—PHP, Node, Chromium, and their
descendants—therefore share one retained accounting subtree. After the subtree
drains, the shared untimed Poppler oracle checks the envelope, parser
acceptance, page count, page dimensions, exact normalized text, exact embedded
font family, normalized raster signature, and link target. One oracle-passing
preflight is discarded before warmups; every timed sample must pass the same
oracle before it can enter aggregates.

The committed competitor slice is deliberately small:

- `dompdf-3.1.6`: `dompdf/dompdf` 3.1.6 from `composer.lock`;
- `browsershot-5.4.0-puppeteer-25.8.0`: Browsershot 5.4.0 from
  `composer.lock` and Puppeteer 25.8.0 from `package-lock.json`.

Only `minimal-static` is mapped in contract v1. Until each target pins its OCI
image digest and canonical dependency/runtime identities in `manifest.toml`, an
external launcher attests that image, and Poppler identities are canonically
pinned,
even that fixture emits an explicit
`not-applicable` result. Every other fixture also emits a
`not-applicable` result with its manifest reason; it never emits zero timing or
a misleading failed/fast sample.
