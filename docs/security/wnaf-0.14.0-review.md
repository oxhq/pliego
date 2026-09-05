# Narrow wnaf 0.14.0 yank review

Reviewed 2026-09-05 for native source `aaf41109035608ad1b84356d434bfade0bddd582`.
This is static applicability evidence for one exact dependency graph, **not a
cryptographic runtime audit or a general safety claim**.

## Upstream reason and actual inclusion

The [registry record](https://crates.io/crates/wnaf/0.14.0) marks 0.14.0 yanked;
its audit record dates the yank to 2026-09-03 19:10:31 UTC. The
[0.14.1 release](https://github.com/RustCrypto/elliptic-curves/releases/tag/wnaf%2Fv0.14.1)
and [upstream fix #1913](https://github.com/RustCrypto/elliptic-curves/pull/1913)
explain the defect: 0.14.0 unconditionally reverses `PrimeField::to_repr()` as
though every scalar representation were big-endian. That misinterprets
little-endian scalar types such as `bignp256`. The replacement introduces
explicit `primefield` representation bounds; the old version was yanked to
require those technically breaking bounds.

This dependency is not unused or merely lock-only. The successful Linux package
[job 101323047154](https://github.com/oxhq/pliego/actions/runs/33972319821/job/101323047154)
prints wnaf in the default `cargo tree -p pliego --edges normal --prefix none
--locked` graph. WebCrypto's ECDSA/ECDH implementation uses the three named NIST
curve types below. The exact lock's only reverse edges are:

`wnaf 0.14.0 <- primeorder 0.14.0 <- {p256, p384, p521} 0.14.0`

No `bignp256` or other primeorder consumer appears in this lock.

## Exact published source applicability

The original published source archives were independently checked against these
Cargo.lock checksums. All versions are 0.14.0 from crates.io.

| Crate | Archive SHA256 |
| --- | --- |
| wnaf | `ab12e7090f27e2ffd9322651492942d50c2926094af30601e1964337db39daf1` |
| primeorder | `5c9f42978c78a00e3d68f69fc03e57a234debae69da4020a4fb588fcdcd07b06` |
| primefield | `c555a6e4eb7d4e158fcb028c835c3b8642206ddc279b5c6b202ef9a8bdb592f4` |
| p256 | `d2c9239b2dbc807adbbe147e8cf72ea7450c3a0aabe62cb8e75ff4ec22e1f72a` |
| p384 | `d17b851e6b3e378ab4ecb07fa2ed23f4d15f075735f8fec9fa1e7bdce5f8301f` |
| p521 | `4ad64cc32c2dc466317c12ee5853e61f159f9eab1fe7efade0395dc2e7b43449` |

- `wnaf/src/lib.rs:197–201` reverses `to_repr`; `src/scalar.rs:29` passes the
  result to `from_le_bytes`. `primeorder/src/projective.rs:142–143,506,527`
  invokes this for variable-time multiplication and both linear-combination
  paths.
- `p256/src/arithmetic/scalar.rs:314–315` delegates `to_repr` to `to_bytes`,
  whose implementation at lines64–65 uses `to_be_byte_array`.
- P384/P521 scalar parameters explicitly declare `ByteOrder::BigEndian` at
  `p384/src/arithmetic/scalar.rs:48` and `p521/src/arithmetic/scalar.rs:30`.
  `primefield/src/macros.rs:52,345–346` binds that byte order and delegates
  representation to the inner Montgomery field. `src/monty.rs:498–499` calls
  `to_bytes`; lines259–263 select BigEndian and `to_be_byte_array`, removing
  only leading padding where required.

Therefore the known little-endian defect does not apply to these pinned scalar
types: reversing their big-endian representation produces the required
little-endian bytes. This conclusion is limited to this source/graph and this
reported defect; it is not evidence of absent future advisories or proof of all
cryptographic behavior.

## Exception enforcement and removal

The `deny.toml` entry is an exact `wnaf@0.14.0` **yank-only** exception using
[cargo-deny's package-specific ignore mechanism](https://embarkstudios.github.io/cargo-deny/checks/advisories/cfg.html#the-ignore-field-optional).
Existing RustSec exceptions, live advisory fetching and global yank settings
remain unchanged. Tidy/cargo-deny and Clippy still run; none is replaced by the
new guard.

`python/check_pliego_wnaf_exception.py` runs before Clippy. It verifies the exact
exception and seven source hashes: full Cargo.lock, workspace/Pliego/script
manifests, and the three reviewed WebCrypto operation/common files. Missing,
unreadable or changed files fail closed without a fallback to committed bytes.
The full lock binds the six versions/checksums and both reverse-parent sets;
the source hashes bind selected features and known usage. Normal and optimized
Python tests cover corrupted pins/configuration and missing files.

Full-lock pinning deliberately also blocks unrelated dependency refreshes. Do
not update the hashes mechanically to preserve this exception. Reassess the
actual graph, source and advisory status, or remove the exception and guard.
An updated native dependency changes candidate identity and requires fresh
relevant package/runtime qualification; previous binary evidence must not be
relabeled.

**Remove/reassess at the next crypto dependency update and before 0.5, whichever
comes first.** Any earlier pin/usage drift also requires review. The reviewed
non-yanked 0.14.1 release exists; this bounded exception is not a reason to defer
the next planned crypto update indefinitely.
