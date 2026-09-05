# Final public-package consumer proof

This manual workflow is for **after publication** of native Pliego `v0.4.0` and
both Composer packages. It is not a release/promotion workflow, and preparing or
passing its source tests does not prove that public packages have been installed.

Dispatch `.github/workflows/pliego-public-consumer.yml` only with the three exact,
reviewed 40-character native/PHP/Laravel release source commits. There are no
default candidate identities, credentials, source repositories, source fallbacks,
SDK overlays, dependency advisory exceptions, or binary overrides. The workflow
verifies that those public versions actually exist before executing their code.

## Fixed consumer sequence

Linux x64 and Windows x64 each use PHP 8.4, Composer 2.10.1, CPython 3.12, and
hash-pinned pypdf 6.16.2. Laravel 13.30.1 is the unchanged host application for:

1. Public `0.3.3`: fresh dependency resolution, install, doctor, Blade render,
   durable local storage and hash readback, typed missing-resource rejection.
2. Public `0.4.0`: update only the two Pliego packages in that same application;
   repeat the consumer and independently parsed PDF checks. Any changed locked
   application dependency fails the proof rather than silently upgrading the app.
3. Public `0.3.3`: restore the original manifest and lock bytes, install that exact
   lock, select the original native binary path/hash, repeat the checks, and
   revalidate all earlier stored PDF hashes. Do not delete the `0.4.0` runtime to
   manufacture the rollback.

Both hosts then create a separate fresh Laravel 12.69.1 / Pliego `0.4.0` consumer.
There is no Laravel 12 / Pliego `0.3.3` upgrade claim. Fresh Composer platform,
strict manifest/lock, and security audit checks are mandatory in every stage;
an advisory or unavailable pin can stop the run. Do not disable those checks.

Each stage verifies Packagist source/dist references, public GitHub release/tag
identity, the native release manifest, and the installed packages against every
file in freshly downloaded public SDK ZIPs. PHP reflection binds used SDK classes
to that exact vendor tree. The public managed installer and doctor run normally;
contract source/version and executable SHA-256 are checked. The four simple PDFs
are independently checked for exact page count and text. Their visual review is
explicitly left pending in the aggregate report.

During the successful `0.4.0` upgrade stage, the workflow additionally executes
the unchanged `production_storage_test.php` from the verified public Laravel
distribution on both hosts, and `production_queue_test.php` on Linux. Only these
commands receive `PLIEGO_TEST_AUTOLOAD=<public-app>/vendor/autoload.php`. This is
not a source loader and is cleared for all subsequent consumer commands. The
tests retain their actual local filesystem/SQLite queue, native render, failure,
recovery and concurrency evidence. Linux's existing 90-second storage and
120-second queue outer limits remain unchanged. The PHP package's separate
binary/deadline/cancellation tests are not silently patched to work around their
package-local autoload assumptions; their source-hosted evidence remains separate.

## Execution and evidence boundaries

The whole job has a 25-minute cap; the consumer step has a 20-minute cap to leave
time for `always()` artifact retention. SDK requests retain the 65-second caller
and default 60-second engine limits; managed install and doctor keep their normal
limits. `run.py` is a fixed hosted sequence, not a generic process supervisor or
authorization to perform an unbounded local Windows installation. It refuses to
run outside GitHub Actions. A command failure stops the sequence. A terminated
job, a missing final report, or an `outcome: running` prefix is incomplete evidence.
Artifact retention after a hard job cancellation is best effort, never a pass.

The upload is an explicit positive list: tool/dependency records, command logs,
public SDK ZIPs and metadata, all stage locks, synthetic inputs, render diagnostics,
stored PDFs, and production storage/queue proof. It excludes dependency `vendor`,
Composer homes/caches, managed native archives and extracted executables. No
credentials are printed or retained. The public ZIPs and synthetic hidden
diagnostics are deliberately included; this is not a broad workspace upload.

Passing automated checks proves these public installed consumers only. It is not
independent adoption, a performance result, remote-storage atomicity, hostile-HTML
sandboxing, a replacement for the three-family corpus, or final visual approval.
Source helper hashes and the workflow checkout commit are recorded separately
from the explicitly supplied native and SDK publication commits.

## Local source-only verification

```text
python tests/pliego/public-consumer/test_recipe.py
python -O tests/pliego/public-consumer/test_recipe.py
php -d auto_prepend_file= -l tests/pliego/public-consumer/consumer.php
```

The five original prepared helpers were ported without altering their proof
semantics, except one reviewed report correction: API 2 has a typed `error.kind`,
not `error.code`. The port omits that nonexistent array-key lookup; it still
requires a resource-kind exception, retains the full rejected result and failed
job, and rejects publication/storage of a failed PDF. A focused source contract
test guards the removed undefined-key access. The original preparation evidence
outside Git remains unchanged.
