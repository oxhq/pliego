# Native regression preflight acceptance

Main reviewed this exact preflight on 2026-09-05. It authorizes the planned
serial and fixed-two-job timing populations only after the timed Linux workflow
revalidates the committed origin, archive, full strict oracle and acceptance.
It is not release approval, a performance result or general scaling evidence.

## Exact retained authority

- Successful [run 33969575451](https://github.com/oxhq/pliego/actions/runs/33969575451), attempt 1,
  proof source `0ea2fef62d9a94e74cbac80f68fadc5b788ab44e`.
- Artifact `9970529197`, 499,956 ZIP bytes; SHA256
  `07994cf49d3d4bae3c1932cc716ea1af1820bf544de66d4077bf27da1b04c29b`.
- All 340 extracted files / 9,357,566 bytes and all 120 frozen source files
  were checked. The original artifact is unchanged.
- Full campaign identity:
  `ae52620586626e4e6c9d537c3b4ae2d79da91fd6e16fb2a820849161a906d8e1`.
- Manifest SHA256:
  `e7b44e8b38fd34013617f3ed0b24b9619c3894b64db8150f689fb5d32ae717a0`.
- Candidate Linux executable:
  `7fe9ed8ea5bd870745f01358234b88234ea469f31f6d8f5f2260f806251ff23b`,
  native source `e179440540527261e96d303b1c42499b0ac031be`, version 0.4.0.
- Public 0.3.3 executable:
  `2045867c2a7928bb2de9b4695cfb9678523faeaacb507bf43b31ecd0a2e6347f`,
  source `41c6cf0e9cf1c73f4f70eba9d413fa97063a3154`.

## Correctness and actual visual review

Four observations produced six successful, byte-identical one-page PDFs: one
serial render and one two-worker batch for each target. Each PDF has 3,925 bytes
and SHA256 `1c7f8a259bc70efefb1eed49291b5f76933fb0777385710cde2c24a73242a3ef`.
All requests also match exactly. The unmodified strict verifier passed in the
original hosted service, including the pinned Linux Poppler 24.02.0 oracle.
Original normalized raster SHA256 is
`0f9edcf2b796a110d1a68efed00ce684921fec9ff7b52ccdc68bb718d1b93444`.

For visual inspection only, main rendered all six retained PDFs at 144 DPI with
Windows Poppler 26.07.0 outside the original proof. All six resulting PNGs also
have identical bytes. Main inspected that byte-identical representative against
the frozen HTML: the heading block and two body lines have the expected Ahem
rectangular glyph shapes, margins and spacing, with no clipping or overlap.
Ahem deliberately draws blocks; these are not missing ordinary business-font
glyphs. This local visual aid does not reproduce or replace the hosted oracle.
No local full-verifier waiver or dependency substitution was used.

## Lifecycle acceptance

Main reviewed the six actual Linux synthetic control outcomes: failed child,
hung child, early launcher death with a remaining descendant, output overflow,
competing-sampler rejection and recovery. All passed; cgroup children returned
to the original `harness` inventory, observed process identities were accounted
for, and the owned service ended inactive. The shorter 1,000/5,000 ms synthetic
control limits do not alter the 65,000 ms native root limit.

The actual native two-job observations prove overlapping bound process lifetimes
with both pidfds live, complete output and clean drain. They do not prove
simultaneous CPU instructions or linear scaling. Known graphics-warning bytes
are retained and classified; protocol/PDF success remains mandatory.

## Timed boundary

Retain all three independently scheduled repeats and every failed or unattempted
outcome. Serial uses 100 observations per target/repeat; fixed concurrency two
uses 50 two-document batches. Whole-batch memory/CPU/I/O is not divided by two.
Keep this minimal-static native regression separate from real-document,
legacy-provider and SDK/storage populations. The inherited synchronous ext4,
fresh temporary state and absent HOME recipe is deliberately non-default;
these hosted observations are exploratory, not warm Laravel production latency.
