# Pliego third-party notices

Every native release includes the generated Cargo report at
`THIRD_PARTY_LICENSES.html` and the pinned payloads under `licenses/`.

## Pinned sources

| Component | Locked package | Source revision | Packaged notices |
| --- | --- | --- | --- |
| Krilla | `krilla` 0.8.2, crates.io checksum `27da593198b20eeba65caeb73c2bbeec3e53ab08fa549898312ce81c4fce5e33` | `3ffdf0588cf98050aad6edba51ca70162e1fb5b5` | `licenses/krilla-0.8.2/` |
| mozangle | `mozangle` 0.6.0, crates.io checksum `60b428c032f0af701a3ca440c92e7b552c25b2a5af2b08a85077ee8e9d2ae699` | `7be30d4be68583169ced927e7b5dab7cca6f185f` | `licenses/mozangle-0.6.0/` |
| zlib | `libz-sys` 1.1.29, crates.io checksum `85bc9657773828b90eeb625adff10eeac83cc21bbfd8e23a03eaa8a33c9e28d9` | bundled zlib 1.3.2 sources | `licenses/zlib-1.3.2/` |

The mozangle crate identifies its vendored Mozilla source as
`FIREFOX_140_12_0esr_RELEASE` revision
`f8025617e815f21388b40baf189338d31a5f9a0a`. Its ANGLE checkout records marker
`6eb59c58d21b`.

## Krilla

Krilla is used with default features disabled and `raster-images` enabled. It
is licensed under `MIT OR Apache-2.0`. The published crate omits its
repository-level licenses and acknowledgement file, so Pliego carries the
pinned originals:

- `LICENSE_MIT`
- `LICENSE_APACHE`
- `NOTICE.md`
- `ICC_CC0-1.0.txt`, covering the bundled ICC profile

Krilla's notice acknowledges adapted code from resvg, Typst, svg2pdf, and
Vello. The complete upstream notice is distributed instead of attempting to
remove entries based on selected Cargo features.

## Windows ANGLE libraries

Windows archives include `libEGL.dll` and `libGLESv2.dll` built by mozangle.
The accompanying payload retains:

- mozangle's BSD-3-Clause `LICENSE`
- the vendored ANGLE `ANGLE_LICENSE`
- `THIRD_PARTY_NOTICES.md` for compiled Chromium, Apple SystemInfo, xxHash,
  volk, MurmurHash, Khronos/Vulkan, SwiftShader, GNU Bison-output, and zlib
  material
- zlib 1.3.2's exact `LICENSE`

This inventory comes from mozangle 0.6.0's locked `build_data.rs`, the Windows
conditional sources in `build.rs`, and the headers they include. Platform
libraries supplied by the operating system are not copied into Pliego's
archives and are outside this bundled-source inventory.

## Generated Cargo report

`resources/resource_protocol/license.html` is generated from `Cargo.lock`,
`about.toml`, and `etc/about.hbs`. The tracked report includes exact entries for
both `krilla 0.8.2` and `mozangle 0.6.0`; release checks reject an archive that
omits the report or pinned notice payloads.
