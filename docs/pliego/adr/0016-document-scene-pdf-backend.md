# ADR 0016: Krilla as the initial DocumentScene PDF backend

- Status: Proposed
- Date: 2026-07-19

## Context

Pliego needs a mature PDF foundation that consumes `DocumentScene` without reshaping text or
performing a second layout. The first backend must accept positioned glyphs and exact font
resources, preserve Unicode selection, draw vector paths and images, and add link annotations.
Writing a PDF serializer is explicitly outside the initial scope.

The current scene still lacks validated glyph-to-source byte ranges, and the workspace dependency
policy does not yet admit Krilla's complete graph. Those are implementation gates, so this decision
remains Proposed until both are represented in code and the focused PDF structural test passes.

## Proposed decision

Use Krilla 0.8.2 with only its raster-image support:

```toml
krilla = { version = "0.8.2", default-features = false, features = ["raster-images"] }
```

The adapter will:

- call `Surface::draw_glyphs` with scene glyph IDs, positions, source text, and validated UTF-8
  cluster ranges; it will not invoke Krilla's simple-text shaping path;
- construct each font from the scene's exact bytes, face index, and variation coordinates;
- map scene paths, images, and links directly to Krilla surfaces and annotations; and
- keep coordinate conversion inside the adapter so PDF choices cannot affect layout or scene
  geometry.

Krilla's positioned-glyph API delegates bidi, fallback, shaping, and layout to the caller. Its
glyph-to-text mapping emits `ToUnicode` data and uses `ActualText` where one-to-one mapping is not
sufficient. This matches Pliego's retained-layout boundary.

Primary implementation references are the tagged
[`draw_glyphs`](https://github.com/LaurenzV/krilla/blob/3ffdf0588cf98050aad6edba51ca70162e1fb5b5/crates/krilla/src/surface.rs#L270-L367),
[`Glyph`](https://github.com/LaurenzV/krilla/blob/3ffdf0588cf98050aad6edba51ca70162e1fb5b5/crates/krilla/src/text/glyph/mod.rs#L88-L183), and
[`Font`](https://github.com/LaurenzV/krilla/blob/3ffdf0588cf98050aad6edba51ca70162e1fb5b5/crates/krilla/src/text/font.rs#L27-L45)
contracts at commit `3ffdf0588cf98050aad6edba51ca70162e1fb5b5`.

## Dependency-policy gates

Krilla 0.8.2 declares Rust 1.92 while the Servo workspace declares Rust 1.88. The active Pliego
toolchain is newer, but accepting the backend requires a Pliego-package-only Rust 1.92 declaration;
the Servo-wide declaration must not be raised for this adapter.

Krilla also requires `yoke` 0.8 while the workspace retains 0.7.5. `cargo deny check bans` rejects
that duplicate unless Pliego adds a documented, version-specific exception. Upgrading Servo's
ICU4X graph solely for the PDF adapter is disproportionate. The exception and its removal condition
must land with the dependency, not before it.

Krilla is MIT OR Apache-2.0. Its NOTICE identifies adapted MPL and Apache code and must be included
in Pliego's distribution notice inventory.

## Security boundary

Before calling the backend, scene validation must reject cluster ranges that are out of bounds or
not UTF-8 boundaries. The adapter must enforce resource-size and decoded-image limits, apply an
explicit link-scheme policy, and return a typed failure when font embedding rights prohibit output.
Backend panics or silent omission are not compatibility behavior.

## Rejected alternatives

- `pdf-writer` and `lopdf` expose serialization building blocks but leave Pliego implementing the
  font, Unicode, graphics, annotation, and object policy that this milestone is meant to buy.
- `resvg`, Typst, and Cairo introduce a second rendering/layout model or discard the document
  semantics required by the canonical scene.

## Acceptance gate

Change this ADR to Accepted only when the dependency-policy changes, glyph cluster validation, and
one focused `DocumentScene -> PDF` structural test land together. That test must prove the page box,
extracted Unicode, embedded font, vector path, image object, and link annotation. PDF byte identity
is not required.

## References

- OXH-245
- [`ports/pliego/src/scene.rs`](../../../ports/pliego/src/scene.rs)
- [Krilla 0.8.2 package](https://crates.io/crates/krilla/0.8.2)
- [Krilla repository at the reviewed tag](https://github.com/LaurenzV/krilla/tree/3ffdf0588cf98050aad6edba51ca70162e1fb5b5)
