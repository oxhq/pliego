# Historical benchmark identity reference

`v0.3.3-minimal-static/` contains seven inert source files (133,677 bytes), not
installed dependencies or an alternative runnable benchmark. The exact bytes
come from capture source `48e8192992d1e9b66eed8ded164ba67cc25a3e7c` and are identical
at publication commit `b7815fe3399ba5daee7f0ae4aa46084a8a9667db` in `oxhq/pliego`.

The fixed source tuple, file inventory, byte lengths and SHA-256 values live in
`benchmarks/tools/benchmark_references.py`. Only the exact historical run
`33243607869`, attempt 1 (and its existing repeats 1–3), selects this reference.
The associated evidence release is
`benchmark-v0.3.3-minimal-static-gh-33243607869-a1`.

The current validators still verify every existing schema, raw-sample binding,
aggregate, nested checksum and canonical archive. They compare historical
adapter/lock/oracle identities with these original bytes, without importing or
executing them. Current runs continue to bind current source files. New runs
must not update this reference or any immutable published record/hash.

The five public result files and released archive remain unchanged. This
reference preserves evidence verification; it does not rerender the historical
PDFs or convert exploratory hosted evidence into a production benchmark claim.
