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
The initial successful case also uses Laravel's fluent exact app-unit page and
margin settings and requires those integers in the native scene; recovery uses
the unchanged default geometry.

The local path includes spaces and a non-ASCII character. The obstructing file
must remain unchanged. This is not an OS ACL test or a remote partial-write
guarantee. Queue execution, concurrency, caller cancellation, independent
adoption and public-package installation require separate evidence.

## Laravel database queues and concurrent storage

`sdk/laravel/tests/production_queue_test.php` uses the same native executable,
framework dependencies and isolated application pattern. It requires PHP 8.4+
and SQLite with Laravel's `IMMEDIATE` transactions, WAL journaling, `FULL`
synchronization and a 15-second busy timeout. The test creates only a fresh
proof-owned database; it never loads a consumer `.env` or migrates an existing
application database.

```sh
PLIEGO_TEST_AUTOLOAD=/consumer/vendor/autoload.php \
  timeout --kill-after=5s 120s php sdk/laravel/tests/production_queue_test.php \
  /runtime/pliego /evidence/fresh-queue-proof
```

Six jobs are serialized into the actual Laravel database queue before two
standard `queue:work` command processes start. Each named queue contains a valid
render, a missing-resource failure and a recovery render. A pipe handshake
releases the first two reserved jobs together; a positive measured overlap of
the actual `store()` calls is required, not merely two process IDs.

The result must retain six dequeue events, four validated stored/readback PDFs
and application records, two typed resource failures persisted by Laravel in
`failed_jobs`, and an empty pending queue. Every UUID, input, native job and
storage target must be distinct and tied back to its initial durable payload.
Both workers must recover after their own failed job and exit successfully.

The declared limits are 60 seconds for the engine, 65 for the SDK, 75 per worker
job, 90 for worker maximum runtime, 100 per worker process and 120 for queue
`retry_after`. An outer 120-second watchdog with a five-second kill grace bounds
the whole test. Windows has no `pcntl` worker alarm; its SDK and parent-process
bounds remain active. The Linux package recipe enables `pcntl` explicitly.

This proves concurrent native storage calls through two named queues sharing a
database, job root and local disk. It does not prove shared-queue contention,
crash/retry exactly-once delivery, descendant cancellation, independent
application adoption, public-package installation or a performance comparison.

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

## Forced caller cancellation

The Linux-only `sdk/php/tests/check_production_cancellation.py` orchestrator and
its PHP driver complement the deadline check. They require a real infinite-JS
API 2 request, observed CPU growth in the same native ScriptThread, forced
termination through Linux pidfds, exit proof for every observed native process,
typed PHP transport failure without delivery/storage success, and fresh-process
recovery. The PHP caller remains alive during native cancellation. This is
SIGKILL cancellation, not graceful shutdown or a new SDK cancellation method.

See [the exact recipe](../../sdk/php/tests/PRODUCTION_CANCELLATION.md) for process
identity, timing/resource observations, cleanup and excluded claims. The package
gate adds an independent 200-second outer watchdog with 15-second kill grace.
The test does not claim historical/escaped descendant completeness, daemon or
hostile-input containment, remote storage guarantees or performance evidence.

## Hosted package boundary

The package matrix runs all four checks against the unpacked optimized Linux
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
