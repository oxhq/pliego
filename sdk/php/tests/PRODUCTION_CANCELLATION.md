# Linux API 2 forced-cancellation recipe

This test complements `production_deadline_test.php`; it does not replace that caller-timeout test. It exercises the real PHP `DocumentEngine` and native `render-api2`, not a fake binary, API 1 supervisor, wrapper renderer, or new SDK cancellation API. Hosted execution is required before claiming this recipe passes.

Prerequisites: Linux with accessible `/proc`, Python 3.10+ providing usable `os.pidfd_open` and `signal.pidfd_send_signal`, PHP 8.3+, the existing PHP SDK Composer autoloader, the supplied SDK font, and the candidate's native Linux binary. Kernel/pidfd availability is tested before starting PHP. A missing ScriptThread CPU observation is a failure, not a reduced gate.

From the repository root, with an existing writable parent and an output path that does not exist:

```sh
python3 sdk/php/tests/check_production_cancellation.py \
  --php /usr/bin/php \
  --binary /absolute/candidate/pliego \
  --output /absolute/fresh-parent/cancellation-proof
```

The orchestrator imposes a 180-second whole-test watchdog plus a bounded cleanup attempt. The real engine receives 60,000 ms host-wall and the SDK receives 65 seconds. Preflight/recovery phases allow 80 seconds each (the outer watchdog still wins); active native discovery and ScriptThread CPU observation allow 20 seconds, stopping/census 3 seconds, and native exit 5 seconds. These are functional test limits, not performance results or universal operating limits. The report includes inherited CPU affinity, selected resource limits and cgroup membership; no new cgroup or memory-admission policy is installed.

## Exact path

1. Python launches only the declared native PHP executable and pins its process identity before allowing it to spawn anything. PHP first performs a successful JavaScript-mutated API 2 render, validates scene/PDF delivery, and publishes/re-reads the same bytes through a local stream callback.
2. After contract probing and preflight have finished, the `RUN` handshake starts one synchronous infinite-JavaScript render. Python selects only a descendant of its pinned PHP child with the supplied engine device/inode identity, exactly `render-api2` as its sole argument, and the fresh cancellation job's exact `32-hex/runtime` working-directory shape. It never selects a contract probe by executable name alone.
3. A same-identity `Script#...` thread must accumulate at least 0.15 CPU seconds according to `/proc` counters. This is Servo's source-owned ScriptThread name. The input contains only the synchronous infinite loop after minimal HTML; the separate preflight demonstrates JavaScript execution. CPU counters do **not** prove an exact JS instruction, author a console event, or provide a causal journal.
4. Python briefly SIGSTOPs the pinned native process and its observed descendants, verifies their threads are stopped, and takes a fresh descendant census. It then sends **SIGKILL** through those pinned handles while the PHP caller remains alive. This is forced caller cancellation, not graceful shutdown. There is no name-based or numeric-PID signaling fallback.
5. Kernel pidfd exit readiness, not a sleep, proves every observed native identity terminated. The retained census preserves PID, start time, executable identity, command, ancestry and signal history; descendants already observed remain accounted for after reparenting. The PHP driver must receive `TransportException` before its own/engine deadline, retain failed job status, and create no success PDF, scene/bundle delivery, or stored-success record.
6. Only then does the `RECOVER` handshake allow a new SDK instance and native process to render, publish, and read back the valid fixture. Every observed PHP-descendant pidfd must have exited at completion. Failure/watchdog cleanup signals only pinned owned identities; all evidence remains retained.

## Proof boundaries

`report.json`, `process-census.jsonl`, `php.stdout`, `php.stderr`, and `php/` retain the observed process proof, exact inputs/font, contract identity, successful preflight/recovery outputs, cancellation error, failed native job and local storage records. A zero descendant count is reported as zero; the harness does not manufacture subprocesses to claim a stronger native result.

This proves only the actual observed run under this Linux recipe. It does not claim historical/escaped-process census completeness, arbitrary daemon containment, hostile-HTML isolation, graceful shutdown, remote/Laravel storage, all-platform cancellation, or performance. Fast processes that disappear before pidfd acquisition are labeled `transient-before-pidfd`, outside the pinned termination denominator. `/proc` census sampling uses readiness waits with a maximum 20 ms interval; passage of that interval never proves completion.

The selected APIs are documented by [Python's pidfd interface](https://docs.python.org/3/library/os.html#os.pidfd_open), [pidfd signal delivery](https://docs.python.org/3/library/signal.html#signal.pidfd_send_signal), and the [Linux pidfd exit-polling contract](https://man7.org/linux/man-pages/man2/pidfd_open.2.html).

Portable parser/safety units (including on Windows) run separately:

```sh
python3 -m unittest discover -s sdk/php/tests -p test_production_cancellation.py -v
php -l sdk/php/tests/production_cancellation_test.php
```

These local checks do not execute or qualify Linux-native cancellation. Retain hosted output separately and inspect the exact candidate identity before marking the deployment gate passed.
