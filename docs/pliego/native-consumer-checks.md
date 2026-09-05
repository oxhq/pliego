# Native API 2 consumer checks

These executable checks exercise the real native process. They are separate
from the fast SDK tests that substitute a fake API 2 process. A check definition
is not evidence that a particular release passed it: retain its report, source
identity, native checksum, framework lock and host details for each run.

## Laravel storage and recovery

`sdk/laravel/tests/production_storage_test.php` requires a real
`laravel/framework` Composer installation and both candidate SDKs. Set
`PLIEGO_TEST_AUTOLOAD` to that consumer's autoloader. Supply an optimized native
executable and a new proof directory whose parent already exists:

```sh
PLIEGO_TEST_AUTOLOAD=/consumer/vendor/autoload.php \
  timeout --kill-after=5s 90s php sdk/laravel/tests/production_storage_test.php \
  /runtime/pliego /evidence/fresh-storage-proof
```

The check creates an isolated minimal Laravel application, without booting the
consumer application's providers or connecting to its database. It uses the
actual Pliego service provider, Blade, supplied font, API 2 engine, local
filesystem and `PendingDocument::store()`.

It verifies a successful stored PDF and byte-for-byte readback, explicitly
injected filesystem `false` and exception outcomes, a real non-directory path
obstruction, a native missing-resource rejection, and a successful recovery.
The fault disks receive a validated PDF stream, and the stream must close even
when storage fails. Application success records are created only after storage
returns successfully. Native render evidence is retained after storage failure;
successful native rendering and failed durable delivery are distinct states.

The local path includes spaces and a non-ASCII character. The obstructing file
must remain unchanged. This is not an OS ACL test or a remote partial-write
guarantee. Queue execution, concurrency, caller cancellation, independent
adoption and public-package installation require separate evidence.

## Caller deadline

`sdk/php/tests/production_deadline_test.php` uses a real synchronous infinite
JavaScript turn. A one-second SDK caller limit, a 999 ms engine wall budget,
normal JavaScript preflight and fresh-process recovery distinguish caller
termination from a generic renderer error:

```sh
timeout --kill-after=5s 45s php sdk/php/tests/production_deadline_test.php \
  /runtime/pliego /evidence/fresh-deadline-proof 1
```

The retained report requires the typed caller-deadline error, failed job state,
and no successful public PDF, storage record, native PDF or delivery bundle.
This script's publication callback is local PHP stream-copy logic, not the
Laravel storage check above. It does not prove descendant cancellation. Its
durations are failure-containment measurements, not rendering benchmarks.

The package matrix runs both checks against the unpacked optimized Linux
bundle and retains their evidence inside its existing API 2 proof artifact.
The pinned framework fixture is installed without application scripts solely
to supply dependencies; that source-path installation is not public registry
consumer proof. The one-second deadline is not applied to the multi-gigabyte
unoptimized debug executable, whose self-hashing is a different startup cost.
Debug table/link checks separately disclose their caller process allowance;
the native request's engine budget and optimized package defaults are unchanged.

The built native archive is retained before subsequent executable acceptance
checks so a failed candidate can be reproduced. Its presence in Actions does
not qualify it for release. Promotion still requires the complete successful
package matrix, exact source/version and the existing proof inventory. Synthetic
API 2 proof uploads include hidden job-state/staging files for failure auditing.
