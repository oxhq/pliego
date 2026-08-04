# ADR 0015: Direct CPU preview from DocumentScene

- Status: Accepted
- Date: 2026-07-19

## Context

Pliego needs a visible preview of the canonical document scene without re-entering Servo
layout or requiring a browser compositor. ADR 0014 establishes `DocumentScene` as the boundary after
layout and paint-order capture. The first preview backend must preserve that boundary so it tests the
scene Pliego intends to share with later document backends, rather than creating another source of
layout truth.

The current scene does not yet encode enough paint, variable-font, or physical-output data to
claim CSS visual parity. The preview therefore has an intentionally narrow rendering contract
whose unsupported cases are visible to callers.

## Decision

### Adapter boundary

The structural preview is a direct `vello_cpu` adapter exposed by the first-page compatibility
entry points and `render_pages_png_with_images`. It accepts a validated `DocumentScene` and resource
resolvers. It does not accept DOM nodes, Servo layout objects, a WebRender display list, or a live
graphics context, and it does not initiate layout.

The resolvers supply the exact font bytes and face index named by a scene text operation and, for the
image-aware entry point, the exact PNG bytes named by a scene image operation. This keeps resource
lookup outside the renderer while leaving `DocumentScene` as the only geometry and operation input.

### Raster semantics

One scene coordinate unit maps directly to one structural raster pixel. Page
extents are rounded up to whole pixels; there is no DPI, device-scale, zoom, or physical-page
conversion. This mapping is useful for inspecting structure and repeatability, not for asserting
print dimensions.

Text is painted in opaque black. The adapter sends each recorded glyph ID and position directly to
`vello_cpu`; it does not shape the operation's text again and does not recalculate advances. A link
operation is non-painting metadata and therefore emits no pixels.

Painted `Path` operations are parsed from their retained SVG path data and rendered with the scene's
fill rule, fill color, and stroke. Malformed path data returns `InvalidPath` at its operation index.
The image-aware entry point decodes exact content-addressed PNG bytes once per resource and maps the
intrinsic bitmap into the retained scene bounds with deterministic low-quality sampling. The
compatibility entry point still returns `UnsupportedOperation` for images when no resolver is
provided. Missing, malformed, non-PNG, or oversized image resources fail explicitly. A font resource
with nonempty variation coordinates returns `UnsupportedFontVariations`; missing fonts, invalid
geometry, out-of-range values, and PNG encoding failures likewise remain typed errors.

### Rejected routes

- **WebRender readback:** this would require live pipeline and epoch state, compositor integration,
  Surfman/GL context management, frame submission, and readback. That is the browser rendering path,
  not a standalone consumer of the canonical scene.
- **Servo Canvas wrappers:** the relevant CPU draw-target abstractions are private to the canvas
  implementation, and canvas font loading depends on Servo runtime font identifiers. Opening those
  internals would couple preview to a second subsystem without improving the scene contract.
- **resvg:** this would require translating the scene into SVG first and risks reshaping text from
  strings. The extra representation would weaken the rule that recorded glyph IDs and positions are
  authoritative.
- **GPU Vello:** device initialization and driver-dependent execution add complexity and variability
  that the headless structural preview does not need. The CPU backend is sufficient for this scope.

### Deliberate limits

Text color and richer box paint semantics remain unavailable until represented in the scene. DPI
and physical-output scaling require a separate contract rather than changing the one-to-one mapping
implicitly. JPEG, GIF, WebP, SVG, normalized variable-font behavior, and visual parity with
Servo/WebRender or the PDF backend are outside this adapter. The structural PNG is not an exact PDF
preview.

## Consequences

- The adapter can render a deterministic, headless structural PNG from positioned scene glyphs
  without DOM, layout, compositor, or GPU state.
- The preview exercises the canonical scene boundary directly, so a successful render cannot hide a
  second layout or shaping pass.
- Text previews are deliberately monochrome and one-to-one in scene units and pixels;
  retained vector paths preserve their explicit fill and stroke colors.
- Content-addressed PNG resources paint into their retained image bounds without DOM or layout
  access.
- Unsupported content stops preview generation with a specific error instead of disappearing from
  the output.
- Adding color, scaling, or operation coverage requires an explicit scene-to-raster mapping and
  corresponding tests; it must not be presented as parity by default.

## References

- Implementation commit `038d50b32605d8d5b77895093b8faa592f54e6bd`
  (`feat(pliego): rasterize positioned scene glyphs`)
- [ADR 0014: Document scene v1 and canonical ordering](0014-document-scene-v1-and-canonical-ordering.md)
- [`ports/pliego/src/raster.rs`](../../../ports/pliego/src/raster.rs)
- [`ports/pliego/src/scene.rs`](../../../ports/pliego/src/scene.rs)
- [`ports/pliego/tests/scene_raster.rs`](../../../ports/pliego/tests/scene_raster.rs)
- [`ports/pliego/Cargo.toml`](../../../ports/pliego/Cargo.toml)
