#!/usr/bin/env php
<?php

/**
 * Target-neutral benchmark runner — one adapter process per sample.
 *
 * Executes one target process per sample and runs the shared PDF oracle after
 * timing. Pliego uses native `render-api2`; competitor adapters retain the
 * benchmark `render INPUT ...` contract. On Linux, cgroup-v2 supplies
 * authoritative CPU, memory, and block-device I/O accounting for the adapter and every
 * descendant. One correctness preflight and all warmups are discarded before
 * real samples. Internal phase entrypoints let the Python coordinator execute
 * one preflight, warmup, or indexed timed sample at a time when it owns a
 * cross-target schedule. Aggregation and schema validation happen in
 * run_benchmark.py.
 *
 * In native API 2 mode, the runner freezes the declared fixture closure into
 * an exclusive cwd-v1 job root, sends one canonical request on stdin, and
 * consumes the typed result plus `delivery/document.pdf`. Fixture paths remain
 * relative to the fixture cwd and are never exposed to API 2.
 *
 * Timing: Linux delegates the engine launch to process_tree_sampler.py. The
 * sampler requires a root broker in an externally delegated cgroup-v2 parent.
 * It launches the engine as the fixed unprivileged account in a fresh root-owned
 * leaf and treats procfs RSS/PSS polling only as lower-bound diagnostics.
 * Non-Linux runs retain wall/exit data but are not publishable.
 * wall_ms ends at root exit; resource_usage.drain_ms extends it to subtree
 * drain. one_shot_wall_ms includes sampler startup, drain, accounting settle
 * and exit. Optional evidence copying and PDF oracles are outside all three.
 *
 * Publishable host contract: Linux x86_64, released `checked-release` bundle.
 */

declare(strict_types=1);

const ENGINE_ACCOUNT = 'pliego-benchmark-engine';
const SAMPLER_PYTHON = '/usr/bin/python3';
const ENGINE_TEMP_ROOT_ENV = 'PLIEGO_BENCHMARK_ENGINE_TEMP_ROOT';
const LINUX_UNIX_SOCKET_PATH_MAX_BYTES = 107;
const GOOGLE_CHROME_RUNTIME_SOCKET_SUFFIX = '/com.google.Chrome.XXXXXX/SingletonSocket';
const CHROMIUM_RUNTIME_SOCKET_SUFFIX = '/org.chromium.Chromium.XXXXXX/SingletonSocket';
const BROWSER_RUNTIME_TEMP_MAX_BYTES = 62;
const ENGINE_OUTPUT_CAPTURE_MAX_BYTES = 16 * 1024 * 1024;
const BROWSER_SHARED_MEMORY_ROOT = '/dev/shm';
const BROWSER_SHARED_MEMORY_CONTAINER_PREFIX = 'pliego-bench-shm-';
const BROWSER_SHARED_MEMORY_DIRECTORY = 'tmp';
const BROWSER_SHARED_MEMORY_CONTRACT = 'bound-private-tmpfs-browser-shared-memory-v1';
const BROWSER_SHARED_MEMORY_SEMANTICS = 'puppeteer-node-chrome-temporary-storage-v1';

const USAGE = <<<EOT
Usage: php pliego.php --binary <path> --input <file.html> --output <file.pdf> --artifacts <dir>
  [--samples N] [--warmup N] [--page-count N] [--text-contains TEXT]...
  [--text-equals TEXT] [--font-family NAME]... [--raster-sha256 HASH]
  [--link-target URL]... [--page-width-points N] [--page-height-points N]
  [--dimension-tolerance-points N] [--require-scene-report]
  --fixture-input-sha256 HASH --fixture-bundle-sha256 HASH [--fixture-asset PATH]...
  [--native-api2] [--isolate-network]
  [--expect-failure] [--expected-code CODE] [--page-size WxH[au]] [--page-margins T,R,B,L[au]]
  [--retain-root FRESH_DIRECTORY] [--root-wall-timeout-ms N (Linux only)]
  [--locale X] [--timezone Y] [--cwd DIR]
  [--runner-phase full|preflight|warmup|timed] [--sample-index N] [--self-test]
EOT;

function option(array $options, string $name): ?string
{
    return isset($options[$name]) && is_scalar($options[$name])
        ? (string) $options[$name]
        : null;
}

/** @return list<string> */
function text_contains_options(mixed $value): array
{
    $values = is_array($value) ? $value : [$value];
    $fragments = array_map(
        static fn (mixed $fragment): string => is_scalar($fragment) ? trim((string) $fragment) : '',
        $values
    );
    return array_values(array_filter(
        $fragments,
        static fn (string $fragment): bool => $fragment !== ''
    ));
}

function is_bare_input_name(string $value): bool
{
    return $value !== '' && $value !== '.' && $value !== '..'
        && !str_contains($value, '/') && !str_contains($value, '\\')
        && preg_match('/^[A-Za-z]:/', $value) !== 1;
}

function is_windows_absolute_path(string $value): bool
{
    return preg_match('~^[A-Za-z]:[\\\\/]~D', $value) === 1;
}

function is_safe_fixture_path(string $value): bool
{
    if ($value === '' || str_starts_with($value, '/') || str_starts_with($value, '\\')
        || preg_match('/^[A-Za-z]:/', $value) === 1) {
        return false;
    }
    foreach (preg_split('~[\\\\/]~', $value) ?: [] as $part) {
        if ($part === '' || $part === '.' || $part === '..') {
            return false;
        }
    }
    return true;
}

/** @param list<string> $assets @return array{0: string, 1: string} */
function fixture_identity(string $cwd, string $input, array $assets): array
{
    $paths = [$input, ...$assets];
    sort($paths, SORT_STRING);
    $bundle = hash_init('sha256');
    $inputHash = null;
    foreach ($paths as $relative) {
        if (!is_safe_fixture_path($relative)) {
            fail("unsafe fixture path: {$relative}");
        }
        $path = realpath($cwd . DIRECTORY_SEPARATOR . str_replace('/', DIRECTORY_SEPARATOR, $relative));
        $prefix = rtrim($cwd, DIRECTORY_SEPARATOR) . DIRECTORY_SEPARATOR;
        if ($path === false || !is_file($path) || !str_starts_with($path, $prefix)) {
            fail("fixture path is unavailable or escaped cwd: {$relative}");
        }
        $fileHash = hash_file('sha256', $path);
        if (!is_string($fileHash)) {
            fail("cannot hash fixture path: {$relative}");
        }
        hash_update($bundle, str_replace('\\', '/', $relative) . "\0" . hex2bin($fileHash));
        if ($relative === $input) {
            $inputHash = $fileHash;
        }
    }
    return [$inputHash ?? fail('fixture identity omitted input'), hash_final($bundle)];
}

function assert_fixture_identity(array $state): void
{
    [$inputHash, $bundleHash] = fixture_identity($state['cwd'], $state['input'], $state['fixtureAssets']);
    if (!hash_equals($state['fixtureInputSha256'], $inputHash)
        || !hash_equals($state['fixtureBundleSha256'], $bundleHash)) {
        fail('fixture identity changed before or during rendering', 1);
    }
}

function fail(string $message, int $code = 2): never
{
    fwrite(STDERR, "pliego.php: {$message}\n");
    exit($code);
}

function sampler_interpreter(): ?string
{
    if (PHP_OS_FAMILY !== 'Linux') {
        return null;
    }
    $resolved = realpath(SAMPLER_PYTHON);
    $mode = $resolved !== false ? fileperms($resolved) : false;
    if ($resolved === false || !str_starts_with($resolved, DIRECTORY_SEPARATOR)
        || !is_file($resolved) || !is_executable($resolved)
        || fileowner($resolved) !== 0 || $mode === false || ($mode & 0022) !== 0) {
        return null;
    }
    return $resolved;
}

// Focused helper tests load declarations without executing a benchmark.
if (defined('PLIEGO_BENCHMARK_RUNNER_LIBRARY_ONLY') && PLIEGO_BENCHMARK_RUNNER_LIBRARY_ONLY) {
    return;
}

$options = getopt('', [
    'binary:', 'input:', 'output:', 'artifacts:', 'samples:', 'warmup:',
    'page-count:', 'text-contains:', 'text-equals:', 'font-family:', 'raster-sha256:',
    'expect-failure', 'expected-code:',
    'link-target:', 'page-width-points:', 'page-height-points:',
    'dimension-tolerance-points:', 'require-scene-report',
    'page-size:', 'page-margins:', 'locale:', 'timezone:', 'cwd:',
    'fixture-input-sha256:', 'fixture-bundle-sha256:', 'fixture-asset:',
    'native-api2', 'isolate-network', 'runner-phase:', 'sample-index:',
    'retain-root:', 'root-wall-timeout-ms:',
    'self-test',
]);
if ($options === false) {
    fwrite(STDERR, USAGE . "\n");
    exit(2);
}

if (array_key_exists('self-test', $options)) {
    $fragments = text_contains_options($options['text-contains'] ?? []);
    if ($fragments !== ['Revenue, net', 'Total', '0']) {
        fail('text-contains self-test failed', 1);
    }
    if (!is_bare_input_name('input.html') || is_bare_input_name('../input.html')
        || is_bare_input_name('..\\input.html') || is_bare_input_name('/input.html')
        || is_bare_input_name('C:\\input.html')
        || !is_windows_absolute_path('C:\\Windows') || !is_windows_absolute_path('D:/tools')
        || is_windows_absolute_path('/tmp')
        || !is_browsershot_adapter_path('/repo/benchmarks/adapters/browsershot/adapter.php')
        || is_browsershot_adapter_path('/repo/benchmarks/adapters/dompdf/adapter.php')
        || !browser_runtime_path_within_budget(str_repeat('x', BROWSER_RUNTIME_TEMP_MAX_BYTES))
        || browser_runtime_path_within_budget(str_repeat('x', BROWSER_RUNTIME_TEMP_MAX_BYTES + 1))
        || BROWSER_RUNTIME_TEMP_MAX_BYTES + strlen(CHROMIUM_RUNTIME_SOCKET_SUFFIX)
            !== LINUX_UNIX_SOCKET_PATH_MAX_BYTES
        || BROWSER_RUNTIME_TEMP_MAX_BYTES + strlen(GOOGLE_CHROME_RUNTIME_SOCKET_SUFFIX)
            > LINUX_UNIX_SOCKET_PATH_MAX_BYTES) {
        fail('bare input self-test failed', 1);
    }
    $summary = parse_api2_result(
        "{\"schema\":\"pliego.render-result\",\"version\":1,\"api\":2,\"status\":\"failed\"}\n"
    );
    if (($summary['status'] ?? null) !== 'failed'
        || parse_api2_result("[]\n") !== null
        || parse_api2_result("{}\n") !== null
        || parse_api2_result("{\"api\":2}\n[]\n") !== null
        || parse_api2_result("{ \"api\":2}\n") !== null) {
        fail('API 2 result framing self-test failed', 1);
    }
    $page = api2_page_size('793.7008x1122.52');
    $margins = api2_page_margins('0,12.5,0,12.5');
    if ($page !== ['width_app_units' => 47622, 'height_app_units' => 67351]
        || $margins !== ['top' => 0, 'right' => 750, 'bottom' => 0, 'left' => 750]
        || api2_media_type('assets/font.TTF') !== 'font/ttf'
        || !is_api2_portable_path('node_modules/chart.js/dist/chart.umd.js')
        || is_api2_portable_path('../escaped.html')
        || !api2_descriptor_matches([
            'path' => 'document.pdf',
            'media_type' => 'application/pdf',
            'sha256' => 'sha256:' . str_repeat('a', 64),
            'bytes' => 42,
        ], 'document.pdf', 'application/pdf', 42, str_repeat('a', 64))) {
        fail('API 2 request normalization self-test failed', 1);
    }
    $browserContainer = '/dev/shm/pliego-bench-shm-0123456789abcdef0123456789abcdef';
    $browserDirectory = $browserContainer . '/tmp';
    $capture = [
        'root' => [
            'path' => '/dev/shm',
            'identity' => ['device' => 17, 'inode' => 100],
            'owner_uid' => 0,
            'owner_gid' => 0,
            'mode' => 01777,
        ],
        'streams' => [
            'stdout' => ['identity' => ['device' => 17, 'inode' => 101]],
            'stderr' => ['identity' => ['device' => 17, 'inode' => 102]],
        ],
    ];
    $snapshot = [
        'root' => $capture['root'],
        'container' => [
            'path' => $browserContainer,
            'identity' => ['device' => 17, 'inode' => 103],
            'owner_uid' => 0,
            'owner_gid' => 0,
            'mode' => 0711,
            'link_count' => 3,
        ],
        'directory' => [
            'path' => $browserDirectory,
            'identity' => ['device' => 17, 'inode' => 104],
            'owner_uid' => 1001,
            'owner_gid' => 1002,
            'mode' => 0700,
            'link_count' => 2,
        ],
        'container_entries' => ['tmp'],
        'directory_entries' => [],
    ];
    $browserProof = [
        'contract' => BROWSER_SHARED_MEMORY_CONTRACT,
        'filesystem' => 'tmpfs',
        'semantics' => BROWSER_SHARED_MEMORY_SEMANTICS,
        'pre' => $snapshot,
        'post' => $snapshot,
    ];
    $browserMeasurement = [
        'launch_security' => [
            'uid' => 1001,
            'gid' => 1002,
            'launch_context' => ['browser_tmpdir' => $browserDirectory],
            'temporary_storage' => ['browser_shared_memory' => $browserProof],
        ],
    ];
    $genericMeasurement = [
        'launch_security' => [
            'uid' => 1001,
            'gid' => 1002,
            'launch_context' => ['browser_tmpdir' => null],
            'temporary_storage' => ['browser_shared_memory' => null],
        ],
    ];
    $browserCommand = ['/repo/benchmarks/adapters/browsershot/adapter.php', 'render'];
    $genericCommand = ['/repo/benchmarks/adapters/dompdf/adapter.php', 'render'];
    $missingProof = $browserMeasurement;
    unset($missingProof['launch_security']['temporary_storage']['browser_shared_memory']);
    $malformedProof = $browserMeasurement;
    $malformedProof['launch_security']['temporary_storage']['browser_shared_memory']['pre']['container']['mode'] = 0700;
    $malformedProof['launch_security']['temporary_storage']['browser_shared_memory']['post']['container']['mode'] = 0700;
    $malformedEnvelope = $browserMeasurement;
    $malformedEnvelope['launch_security']['temporary_storage']['browser_shared_memory']['contract'] = 'wrong';
    $coupledProof = $genericMeasurement;
    $coupledProof['launch_security']['temporary_storage']['browser_shared_memory'] = $browserProof;
    if (sampler_browser_shared_memory_proof_error($browserMeasurement, $browserCommand, $capture) !== null
        || sampler_browser_shared_memory_proof_error($genericMeasurement, $genericCommand, $capture) !== null
        || sampler_browser_shared_memory_proof_error($missingProof, $browserCommand, $capture) === null
        || sampler_browser_shared_memory_proof_error($malformedProof, $browserCommand, $capture) === null
        || sampler_browser_shared_memory_proof_error($malformedEnvelope, $browserCommand, $capture) === null
        || sampler_browser_shared_memory_proof_error($coupledProof, $genericCommand, $capture) === null) {
        fail('browser shared-memory sampler proof self-test failed', 1);
    }
    if (PHP_OS_FAMILY === 'Linux' && sampler_interpreter() === null) {
        fail('sampler interpreter self-test failed', 1);
    }
    fwrite(STDOUT, "Pliego PHP runner self-test passed\n");
    exit(0);
}

$binary = option($options, 'binary') ?? fail('--binary is required');
$input = option($options, 'input') ?? fail('--input is required');
$output = option($options, 'output') ?? 'document.pdf';
$artifacts = option($options, 'artifacts') ?? 'artifacts';
$samples = max(1, (int) (option($options, 'samples') ?? 1));
$warmup = max(0, (int) (option($options, 'warmup') ?? 0));
$runnerPhase = option($options, 'runner-phase') ?? 'full';
$sampleIndexRaw = option($options, 'sample-index');
if ($sampleIndexRaw !== null
    && (preg_match('/^(0|[1-9][0-9]*)$/', $sampleIndexRaw) !== 1
        || (string) (int) $sampleIndexRaw !== $sampleIndexRaw)) {
    fail('--sample-index must be a canonical nonnegative integer');
}
$sampleIndex = $sampleIndexRaw !== null ? (int) $sampleIndexRaw : null;
if (!in_array($runnerPhase, ['full', 'preflight', 'warmup', 'timed'], true)) {
    fail('--runner-phase must be full, preflight, warmup, or timed');
}
if (in_array($runnerPhase, ['warmup', 'timed'], true)) {
    if ($sampleIndex === null || $sampleIndex < 0) {
        fail("--runner-phase {$runnerPhase} requires a nonnegative --sample-index");
    }
    if (array_key_exists('samples', $options) || array_key_exists('warmup', $options)) {
        fail("--runner-phase {$runnerPhase} does not accept --samples or --warmup");
    }
} elseif ($sampleIndex !== null) {
    fail("--sample-index is not accepted for --runner-phase {$runnerPhase}");
}
if ($runnerPhase === 'preflight'
    && (array_key_exists('samples', $options) || array_key_exists('warmup', $options))) {
    fail('--runner-phase preflight does not accept --samples or --warmup');
}
$pageCount = option($options, 'page-count') !== null ? (int) $options['page-count'] : null;
$textContains = text_contains_options($options['text-contains'] ?? []);
$textEquals = option($options, 'text-equals');
$fontFamilies = text_contains_options($options['font-family'] ?? []);
$rasterSha256 = option($options, 'raster-sha256');
$linkTargets = text_contains_options($options['link-target'] ?? []);
$pageWidthPoints = option($options, 'page-width-points') !== null
    ? (float) $options['page-width-points']
    : null;
$pageHeightPoints = option($options, 'page-height-points') !== null
    ? (float) $options['page-height-points']
    : null;
$dimensionTolerancePoints = (float) (option($options, 'dimension-tolerance-points') ?? 0.5);
$requireSceneReport = array_key_exists('require-scene-report', $options);
$expectFailure = array_key_exists('expect-failure', $options);
$expectedCode = option($options, 'expected-code');
$pageSize = option($options, 'page-size');
$pageMargins = option($options, 'page-margins');
$locale = option($options, 'locale');
$timezone = option($options, 'timezone');
$cwd = option($options, 'cwd') ?? dirname($input);
$fixtureInputSha256 = option($options, 'fixture-input-sha256') ?? fail('--fixture-input-sha256 is required');
$fixtureBundleSha256 = option($options, 'fixture-bundle-sha256') ?? fail('--fixture-bundle-sha256 is required');
$fixtureAssets = text_contains_options($options['fixture-asset'] ?? []);
$nativeApi2 = array_key_exists('native-api2', $options);
$isolateNetwork = array_key_exists('isolate-network', $options);
$retainRoot = option($options, 'retain-root');
$rootWallTimeoutMs = root_wall_timeout_option(option($options, 'root-wall-timeout-ms'));
if ($rootWallTimeoutMs !== null && PHP_OS_FAMILY !== 'Linux') {
    fail('--root-wall-timeout-ms requires the Linux cgroup sampler');
}

if (!is_file($binary)) {
    fail("binary not found: {$binary}");
}
$resolvedBinary = realpath($binary);
if ($resolvedBinary === false || !is_executable($resolvedBinary)) {
    fail("binary must be a canonical executable: {$binary}");
}
$binary = $resolvedBinary;
$binarySha256 = hash_file('sha256', $binary);
if (!is_string($binarySha256)) {
    fail("cannot hash benchmark target binary: {$binary}");
}
if (!is_dir($cwd)) {
    fail("cwd not found: {$cwd}");
}
$resolvedCwd = realpath($cwd);
if ($resolvedCwd === false) {
    fail("cwd must be canonical: {$cwd}");
}
$cwd = $resolvedCwd;
// Every adapter gets the same bare input name and cwd. Reject the historical
// absolute/relative validation split instead of validating one path and
// executing another.
if (!is_bare_input_name($input)) {
    fail('--input must be one bare file name resolved inside --cwd');
}
$inputFull = $cwd . DIRECTORY_SEPARATOR . $input;
$resolvedInput = realpath($inputFull);
if ($resolvedInput === false || !is_file($resolvedInput) || dirname($resolvedInput) !== $cwd) {
    fail("input must resolve to a regular file directly inside cwd: {$inputFull}");
}
if (($pageWidthPoints === null) !== ($pageHeightPoints === null)
    || $dimensionTolerancePoints < 0) {
    fail('page dimensions must be supplied together and tolerance cannot be negative');
}
if (preg_match('/^[0-9a-f]{64}$/D', $fixtureInputSha256) !== 1
    || preg_match('/^[0-9a-f]{64}$/D', $fixtureBundleSha256) !== 1) {
    fail('fixture hashes must be lowercase SHA-256 values');
}
if ($rasterSha256 !== null && preg_match('/^[0-9a-f]{64}$/D', $rasterSha256) !== 1) {
    fail('--raster-sha256 must be a lowercase SHA-256 value');
}

$engineUid = null;
$engineGid = null;
if (PHP_OS_FAMILY === 'Linux') {
    if (!function_exists('posix_getuid') || !function_exists('posix_geteuid')
        || posix_getuid() !== 0 || posix_geteuid() !== 0) {
        fail('publishable cgroup measurement requires a real/effective root broker');
    }
    $account = function_exists('posix_getpwnam') ? posix_getpwnam(ENGINE_ACCOUNT) : false;
    if (!is_array($account) || (int) ($account['uid'] ?? 0) <= 0 || (int) ($account['gid'] ?? 0) <= 0) {
        fail('required non-root account is absent or unsafe: ' . ENGINE_ACCOUNT);
    }
    $engineUid = (int) $account['uid'];
    $engineGid = (int) $account['gid'];
    $cgroupParent = getenv('PLIEGO_BENCHMARK_CGROUP_PARENT');
    $resolvedParent = is_string($cgroupParent) && $cgroupParent !== '' ? realpath($cgroupParent) : false;
    if ($resolvedParent === false || $resolvedParent !== $cgroupParent || !is_dir($resolvedParent)) {
        fail('PLIEGO_BENCHMARK_CGROUP_PARENT must name a canonical existing directory');
    }
}

/**
 * @param list<string> $command
 * @return array{error: string}|array{wall_ms: float, one_shot_wall_ms: float, user_ms: float|null,
 *     sys_ms: float|null, memory_current_bytes: int|null, memory_peak_bytes: int|null,
 *     sampled_peak_rss_kib_lower_bound: int|null, sampled_peak_pss_kib_lower_bound: int|null,
 *     read_bytes: int|null, write_bytes: int|null, read_operations: int|null,
 *     write_operations: int|null, measurement_method: string,
 *     signal: int|null, resource_usage: object|null,
 *     exit_code: int, stdout: string, stderr: string}
 */
function engine_output_capture_root(): ?string
{
    $candidate = PHP_OS_FAMILY === 'Linux' ? '/dev/shm' : sys_get_temp_dir();
    $resolved = realpath($candidate);
    if ($resolved === false || !is_dir($resolved)
        || (PHP_OS_FAMILY === 'Linux' && ($resolved !== $candidate || is_link($candidate)))) {
        return null;
    }
    return $resolved;
}

/** @return array{error: string}|array{binding: array<string, mixed>} */
function engine_output_stream_binding(string $path, int $rootDevice, bool $requireEmpty): array
{
    clearstatcache(true, $path);
    $resolved = realpath($path);
    $metadata = @lstat($path);
    if ($resolved === false || $resolved !== $path || dirname($path) !== '/dev/shm'
        || !is_array($metadata) || ($metadata['mode'] & 0170000) !== 0100000
        || (int) $metadata['uid'] !== 0 || (int) $metadata['gid'] !== 0
        || ($metadata['mode'] & 07777) !== 0600 || (int) $metadata['nlink'] !== 1
        || (int) $metadata['dev'] !== $rootDevice || ($requireEmpty && (int) $metadata['size'] !== 0)) {
        return ['error' => "unsafe engine output capture path: {$path}"];
    }
    return ['binding' => [
        'path' => $path,
        'identity' => ['device' => (int) $metadata['dev'], 'inode' => (int) $metadata['ino']],
        'owner_uid' => (int) $metadata['uid'],
        'owner_gid' => (int) $metadata['gid'],
        'mode' => $metadata['mode'] & 07777,
        'link_count' => (int) $metadata['nlink'],
        'size_bytes' => (int) $metadata['size'],
    ]];
}

/** @return array{error: string}|array{snapshot: array<string, mixed>} */
function bind_engine_output_capture(string $stdoutPath, string $stderrPath): array
{
    $root = engine_output_capture_root();
    $rootMetadata = $root === null ? false : @lstat($root);
    if ($root !== '/dev/shm' || !is_array($rootMetadata)
        || ($rootMetadata['mode'] & 0170000) !== 0040000
        || (int) $rootMetadata['uid'] !== 0 || (int) $rootMetadata['gid'] !== 0
        || ($rootMetadata['mode'] & 07777) !== 01777) {
        return ['error' => 'engine output capture root must be canonical root-owned mode-01777 /dev/shm'];
    }
    $rootDevice = (int) $rootMetadata['dev'];
    $stdout = engine_output_stream_binding($stdoutPath, $rootDevice, true);
    $stderr = engine_output_stream_binding($stderrPath, $rootDevice, true);
    if (isset($stdout['error']) || isset($stderr['error'])) {
        return ['error' => (string) ($stdout['error'] ?? $stderr['error'])];
    }
    $stdoutBinding = $stdout['binding'];
    $stderrBinding = $stderr['binding'];
    if (!str_starts_with(basename($stdoutPath), 'pliego-bench-out-')
        || !str_starts_with(basename($stderrPath), 'pliego-bench-err-')
        || $stdoutBinding['identity'] === $stderrBinding['identity']) {
        return ['error' => 'engine stdout and stderr captures must be distinct prefixed files'];
    }
    return ['snapshot' => [
        'root' => [
            'path' => $root,
            'identity' => ['device' => $rootDevice, 'inode' => (int) $rootMetadata['ino']],
            'owner_uid' => (int) $rootMetadata['uid'],
            'owner_gid' => (int) $rootMetadata['gid'],
            'mode' => $rootMetadata['mode'] & 07777,
        ],
        'streams' => ['stdout' => $stdoutBinding, 'stderr' => $stderrBinding],
    ]];
}

/** @param array<string, mixed> $binding */
function stable_engine_output_binding(array $binding): array
{
    unset($binding['size_bytes']);
    return $binding;
}

/** @param list<string> $expected */
function has_exact_sampler_object_keys(mixed $value, array $expected): bool
{
    if (!is_array($value)) {
        return false;
    }
    $actual = array_keys($value);
    sort($actual, SORT_STRING);
    sort($expected, SORT_STRING);
    return $actual === $expected;
}

function browser_shared_memory_identity_is_valid(mixed $identity): bool
{
    return has_exact_sampler_object_keys($identity, ['device', 'inode'])
        && is_int($identity['device']) && $identity['device'] >= 0
        && is_int($identity['inode']) && $identity['inode'] >= 1;
}

function browser_shared_memory_binding_is_valid(mixed $binding, bool $withLinkCount): bool
{
    $keys = ['path', 'identity', 'owner_uid', 'owner_gid', 'mode'];
    if ($withLinkCount) {
        $keys[] = 'link_count';
    }
    return has_exact_sampler_object_keys($binding, $keys)
        && is_string($binding['path']) && $binding['path'] !== ''
        && browser_shared_memory_identity_is_valid($binding['identity'])
        && is_int($binding['owner_uid']) && $binding['owner_uid'] >= 0
        && is_int($binding['owner_gid']) && $binding['owner_gid'] >= 0
        && is_int($binding['mode']) && $binding['mode'] >= 0
        && (!$withLinkCount || (is_int($binding['link_count']) && $binding['link_count'] >= 1));
}

/**
 * @param array<string, mixed> $measurement
 * @param list<string> $command
 * @param array<string, mixed> $expectedOutputCapture
 */
function sampler_browser_shared_memory_proof_error(
    array $measurement,
    array $command,
    array $expectedOutputCapture
): ?string {
    $launch = $measurement['launch_security'] ?? null;
    $launchContext = is_array($launch) ? ($launch['launch_context'] ?? null) : null;
    $temporaryStorage = is_array($launch) ? ($launch['temporary_storage'] ?? null) : null;
    if (!is_array($launch) || !is_array($launchContext) || !is_array($temporaryStorage)
        || !array_key_exists('browser_tmpdir', $launchContext)
        || !array_key_exists('browser_shared_memory', $temporaryStorage)) {
        return 'cgroup-v2 sampler omitted browser shared-memory proof coupling fields';
    }

    $browserTmpdir = $launchContext['browser_tmpdir'];
    $proof = $temporaryStorage['browser_shared_memory'];
    $expectsBrowserProof = isset($command[0]) && is_string($command[0])
        && is_browsershot_adapter_path($command[0]);
    if (!$expectsBrowserProof) {
        return $proof === null && $browserTmpdir === null
            ? null
            : 'cgroup-v2 sampler attached browser shared-memory storage to a non-Browsershot target';
    }
    if (!is_string($browserTmpdir) || $browserTmpdir === '' || !is_array($proof)) {
        return 'cgroup-v2 sampler omitted Browsershot shared-memory storage';
    }
    if (!has_exact_sampler_object_keys($proof, ['contract', 'filesystem', 'semantics', 'pre', 'post'])
        || $proof['contract'] !== BROWSER_SHARED_MEMORY_CONTRACT
        || $proof['filesystem'] !== 'tmpfs'
        || $proof['semantics'] !== BROWSER_SHARED_MEMORY_SEMANTICS
        || !is_array($proof['pre']) || !is_array($proof['post'])
        || $proof['pre'] !== $proof['post']) {
        return 'cgroup-v2 sampler returned invalid browser shared-memory proof envelope';
    }

    $snapshot = $proof['pre'];
    if (!has_exact_sampler_object_keys(
        $snapshot,
        ['root', 'container', 'directory', 'container_entries', 'directory_entries']
    ) || !browser_shared_memory_binding_is_valid($snapshot['root'] ?? null, false)
        || !browser_shared_memory_binding_is_valid($snapshot['container'] ?? null, true)
        || !browser_shared_memory_binding_is_valid($snapshot['directory'] ?? null, true)) {
        return 'cgroup-v2 sampler returned invalid browser shared-memory binding shape';
    }
    $root = $snapshot['root'];
    $container = $snapshot['container'];
    $directory = $snapshot['directory'];
    $expectedRoot = $expectedOutputCapture['root'] ?? null;
    $engineUid = $launch['uid'] ?? null;
    $engineGid = $launch['gid'] ?? null;
    if (!is_array($expectedRoot) || $root !== $expectedRoot
        || $root['path'] !== BROWSER_SHARED_MEMORY_ROOT
        || $root['owner_uid'] !== 0 || $root['owner_gid'] !== 0 || $root['mode'] !== 01777
        || !is_int($engineUid) || $engineUid <= 0 || !is_int($engineGid) || $engineGid <= 0
        || $container['owner_uid'] !== 0 || $container['owner_gid'] !== 0
        || $container['mode'] !== 0711 || $container['link_count'] !== 3
        || $directory['owner_uid'] !== $engineUid || $directory['owner_gid'] !== $engineGid
        || $directory['mode'] !== 0700 || $directory['link_count'] !== 2) {
        return 'cgroup-v2 sampler returned unsafe browser shared-memory ownership, mode, or links';
    }

    $containerPath = $container['path'];
    $directoryPath = $directory['path'];
    if (preg_match(
        '~\A/dev/shm/pliego-bench-shm-[0-9a-f]{32}\z~',
        $containerPath
    ) !== 1 || $directoryPath !== $containerPath . '/' . BROWSER_SHARED_MEMORY_DIRECTORY
        || $browserTmpdir !== $directoryPath
        || $snapshot['container_entries'] !== [BROWSER_SHARED_MEMORY_DIRECTORY]
        || $snapshot['directory_entries'] !== []) {
        return 'cgroup-v2 sampler returned invalid browser shared-memory paths or topology';
    }

    $bindings = [$root, $container, $directory];
    $devices = [];
    $identities = [];
    foreach ($bindings as $binding) {
        $identity = $binding['identity'];
        $devices[] = $identity['device'];
        $identities[] = $identity['device'] . ':' . $identity['inode'];
    }
    if (count(array_unique($devices, SORT_REGULAR)) !== 1
        || count(array_unique($identities, SORT_STRING)) !== 3) {
        return 'cgroup-v2 sampler returned invalid browser shared-memory device or identities';
    }
    foreach (['stdout', 'stderr'] as $stream) {
        $outputIdentity = $expectedOutputCapture['streams'][$stream]['identity'] ?? null;
        if (!browser_shared_memory_identity_is_valid($outputIdentity)) {
            return 'runner output-capture binding is incomplete for browser shared-memory validation';
        }
        $key = $outputIdentity['device'] . ':' . $outputIdentity['inode'];
        if (in_array($key, $identities, true)) {
            return 'cgroup-v2 sampler reused an engine output identity for browser shared-memory storage';
        }
    }
    return null;
}

/** @param array<string, mixed> $expected
 *  @return array{error: string}|array{content: string, binding: array<string, mixed>}
 */
function read_bound_engine_output_capture(string $path, array $expected, int $rootDevice): array
{
    $observed = engine_output_stream_binding($path, $rootDevice, false);
    if (isset($observed['error'])) {
        return ['error' => $observed['error']];
    }
    $binding = $observed['binding'];
    if (stable_engine_output_binding($binding) !== stable_engine_output_binding($expected)) {
        return ['error' => "engine output capture identity changed before read: {$path}"];
    }
    if ($binding['size_bytes'] > ENGINE_OUTPUT_CAPTURE_MAX_BYTES) {
        return ['error' => "engine output capture exceeded the per-stream byte limit: {$path}"];
    }
    $content = @file_get_contents($path, false, null, 0, ENGINE_OUTPUT_CAPTURE_MAX_BYTES + 1);
    if (!is_string($content) || strlen($content) !== $binding['size_bytes']
        || strlen($content) > ENGINE_OUTPUT_CAPTURE_MAX_BYTES) {
        return ['error' => "engine output capture changed or exceeded its limit during read: {$path}"];
    }
    return ['content' => $content, 'binding' => $binding];
}

/** @param array<string, mixed> $snapshot */
function cleanup_bound_engine_output_capture(array $snapshot): ?string
{
    $rootDevice = (int) $snapshot['root']['identity']['device'];
    $errors = [];
    foreach ($snapshot['streams'] as $label => $expected) {
        $path = (string) $expected['path'];
        $observed = engine_output_stream_binding($path, $rootDevice, false);
        if (isset($observed['error'])
            || stable_engine_output_binding($observed['binding'] ?? []) !== stable_engine_output_binding($expected)
            || !@unlink($path)) {
            $errors[] = "cannot remove bound engine {$label} capture";
            continue;
        }
        clearstatcache(true, $path);
        if (@lstat($path) !== false) {
            $errors[] = "bound engine {$label} capture remained after removal";
        }
    }
    return $errors === [] ? null : implode('; ', $errors);
}

/** @param array<string, mixed>|null $snapshot */
function cleanup_engine_output_capture_files(mixed $stdoutPath, mixed $stderrPath, ?array $snapshot): ?string
{
    if ($snapshot !== null) {
        return cleanup_bound_engine_output_capture($snapshot);
    }
    $errors = [];
    foreach ([$stdoutPath, $stderrPath] as $path) {
        if (!is_string($path) || @lstat($path) === false) {
            continue;
        }
        if (!@unlink($path)) {
            $errors[] = "cannot remove engine output capture: {$path}";
            continue;
        }
        clearstatcache(true, $path);
        if (@lstat($path) !== false) {
            $errors[] = "engine output capture remained after removal: {$path}";
        }
    }
    return $errors === [] ? null : implode('; ', $errors);
}

function run_engine(
    array $command,
    string $cwd,
    bool $isolateNetwork,
    ?string $stdinPath,
    ?string $temporaryDirectory = null,
    ?float $rootWallTimeoutMs = null
): array
{
    $nullDevice = PHP_OS_FAMILY === 'Windows' ? 'NUL' : '/dev/null';
    $linux = PHP_OS_FAMILY === 'Linux';
    $captureRoot = engine_output_capture_root();
    if ($captureRoot === null) {
        return ['error' => 'cannot resolve engine output capture root'];
    }
    $stdoutTmp = @tempnam($captureRoot, 'pliego-bench-out-');
    $stderrTmp = @tempnam($captureRoot, 'pliego-bench-err-');
    if ($stdoutTmp === false || $stderrTmp === false
        || realpath($stdoutTmp) !== $stdoutTmp || realpath($stderrTmp) !== $stderrTmp
        || dirname($stdoutTmp) !== $captureRoot || dirname($stderrTmp) !== $captureRoot
        || !is_file($stdoutTmp) || !is_file($stderrTmp)
        || is_link($stdoutTmp) || is_link($stderrTmp) || $stdoutTmp === $stderrTmp) {
        $cleanupError = cleanup_engine_output_capture_files($stdoutTmp, $stderrTmp, null);
        return ['error' => 'cannot create bound engine output capture files'
            . ($cleanupError === null ? '' : "; {$cleanupError}")];
    }
    $captureBefore = null;
    if ($linux) {
        $captureBinding = bind_engine_output_capture($stdoutTmp, $stderrTmp);
        if (isset($captureBinding['error'])) {
            $cleanupError = cleanup_engine_output_capture_files($stdoutTmp, $stderrTmp, null);
            return ['error' => $captureBinding['error'] . ($cleanupError === null ? '' : "; {$cleanupError}")];
        }
        $captureBefore = $captureBinding['snapshot'];
    }
    $samplerResultTmp = $linux ? tempnam(sys_get_temp_dir(), 'pliego-bench-cgroup-') : null;
    $samplerErrorTmp = $linux ? tempnam(sys_get_temp_dir(), 'pliego-bench-cgroup-err-') : null;
    if ($linux && ($samplerResultTmp === false || $samplerErrorTmp === false)) {
        $cleanupError = cleanup_engine_output_capture_files($stdoutTmp, $stderrTmp, $captureBefore);
        if (is_string($samplerResultTmp)) {
            @unlink($samplerResultTmp);
        }
        if (is_string($samplerErrorTmp)) {
            @unlink($samplerErrorTmp);
        }
        return ['error' => 'cannot create sampler output files' . ($cleanupError === null ? '' : "; {$cleanupError}")];
    }

    $launchedCommand = $command;
    $processEnvironment = null;
    if ($linux) {
        $sampler = dirname(__DIR__) . '/tools/process_tree_sampler.py';
        if (!is_file($sampler)) {
            $cleanupError = cleanup_engine_output_capture_files($stdoutTmp, $stderrTmp, $captureBefore);
            @unlink($samplerResultTmp);
            @unlink($samplerErrorTmp);
            return ['error' => "cgroup-v2 sampler not found: {$sampler}" . ($cleanupError === null ? '' : "; {$cleanupError}")];
        }
        $interpreter = sampler_interpreter();
        if ($interpreter === null) {
            $cleanupError = cleanup_engine_output_capture_files($stdoutTmp, $stderrTmp, $captureBefore);
            @unlink($samplerResultTmp);
            @unlink($samplerErrorTmp);
            return ['error' => 'sampler interpreter is not a canonical root-owned, non-writable executable: '
                . SAMPLER_PYTHON . ($cleanupError === null ? '' : "; {$cleanupError}")];
        }
        $launchedCommand = [
            $interpreter, '-I', $sampler,
            '--cwd', $cwd,
            ...($temporaryDirectory !== null ? ['--temporary-directory', $temporaryDirectory] : []),
            '--stdout', $stdoutTmp,
            '--stderr', $stderrTmp,
            ...($stdinPath !== null ? ['--stdin', $stdinPath] : []),
            ...($rootWallTimeoutMs !== null ? ['--root-wall-timeout-ms', (string) $rootWallTimeoutMs] : []),
            ...($isolateNetwork ? ['--isolate-network'] : []),
            '--',
            ...$command,
        ];
        $processEnvironment = [
            'PLIEGO_BENCHMARK_CGROUP_PARENT' => (string) getenv('PLIEGO_BENCHMARK_CGROUP_PARENT'),
        ];
        foreach (['BROWSERSHOT_CHROME_PATH', 'BROWSERSHOT_NODE_BINARY'] as $name) {
            $value = getenv($name);
            if (is_string($value) && $value !== '') {
                $processEnvironment[$name] = $value;
            }
        }
    }
    $descriptors = [
        0 => ['file', $stdinPath ?? $nullDevice, 'r'],
        1 => ['file', $linux ? $samplerResultTmp : $stdoutTmp, 'w'],
        2 => ['file', $linux ? $samplerErrorTmp : $stderrTmp, 'w'],
    ];

    $wallStart = microtime(true);
    $process = proc_open($launchedCommand, $descriptors, $pipes, $cwd, $processEnvironment);
    if (!is_resource($process)) {
        $cleanupError = cleanup_engine_output_capture_files($stdoutTmp, $stderrTmp, $captureBefore);
        if (is_string($samplerResultTmp)) {
            @unlink($samplerResultTmp);
        }
        if (is_string($samplerErrorTmp)) {
            @unlink($samplerErrorTmp);
        }
        return ['error' => 'proc_open failed for engine command' . ($cleanupError === null ? '' : "; {$cleanupError}")];
    }

    $launcherExitCode = proc_close($process);
    $wallMs = (microtime(true) - $wallStart) * 1000.0;
    $captureAfter = null;
    $readError = null;
    if ($linux) {
        $rootDevice = (int) $captureBefore['root']['identity']['device'];
        $stdoutRead = read_bound_engine_output_capture(
            $stdoutTmp,
            $captureBefore['streams']['stdout'],
            $rootDevice
        );
        $stderrRead = read_bound_engine_output_capture(
            $stderrTmp,
            $captureBefore['streams']['stderr'],
            $rootDevice
        );
        $readError = $stdoutRead['error'] ?? $stderrRead['error'] ?? null;
        $stdout = isset($stdoutRead['content']) ? $stdoutRead['content'] : '';
        $stderr = isset($stderrRead['content']) ? $stderrRead['content'] : '';
        if ($readError === null) {
            $captureAfter = [
                'root' => $captureBefore['root'],
                'streams' => [
                    'stdout' => $stdoutRead['binding'],
                    'stderr' => $stderrRead['binding'],
                ],
            ];
        }
    } else {
        $stdoutRead = @file_get_contents($stdoutTmp, false, null, 0, ENGINE_OUTPUT_CAPTURE_MAX_BYTES + 1);
        $stderrRead = @file_get_contents($stderrTmp, false, null, 0, ENGINE_OUTPUT_CAPTURE_MAX_BYTES + 1);
        if (!is_string($stdoutRead) || !is_string($stderrRead)
            || strlen($stdoutRead) > ENGINE_OUTPUT_CAPTURE_MAX_BYTES
            || strlen($stderrRead) > ENGINE_OUTPUT_CAPTURE_MAX_BYTES) {
            $readError = 'cannot read bounded engine output capture';
        }
        $stdout = is_string($stdoutRead) ? $stdoutRead : '';
        $stderr = is_string($stderrRead) ? $stderrRead : '';
    }
    $cleanupError = cleanup_engine_output_capture_files($stdoutTmp, $stderrTmp, $captureBefore);
    if ($readError !== null || $cleanupError !== null) {
        if ($linux) {
            @unlink($samplerResultTmp);
            @unlink($samplerErrorTmp);
        }
        return ['error' => implode('; ', array_filter([$readError, $cleanupError]))];
    }

    if ($linux) {
        $measurementJson = (string) file_get_contents($samplerResultTmp);
        $measurement = json_decode($measurementJson, true);
        $resourceUsage = json_decode($measurementJson);
        $samplerError = trim((string) file_get_contents($samplerErrorTmp));
        @unlink($samplerResultTmp);
        @unlink($samplerErrorTmp);
        if ($launcherExitCode !== 0 || !is_array($measurement) || !is_object($resourceUsage)) {
            return [
                'error' => 'cgroup-v2 sampler failed: ' . ($samplerError ?: "exit {$launcherExitCode}"),
                'stdout' => $stdout,
                'stderr' => $stderr,
                'sampler_stdout' => $measurementJson,
                'sampler_stderr' => $samplerError,
                'one_shot_wall_ms' => round($wallMs, 3),
            ];
        }
        foreach ([
            'wall_ms', 'cpu_user_ms', 'cpu_sys_ms', 'memory_current_bytes', 'memory_peak_bytes',
            'read_bytes', 'write_bytes', 'read_operations', 'write_operations', 'method', 'exit_code',
            'cgroup_drained', 'cleanup', 'launch_security', 'sampled_diagnostics',
        ] as $field) {
            if (!array_key_exists($field, $measurement)) {
                return ['error' => "cgroup-v2 sampler omitted {$field}"];
            }
        }
        $captureProof = $measurement['launch_security']['output_capture'] ?? null;
        $expectedCaptureProof = [
            'contract' => 'root-bound-tmpfs-engine-output-v1',
            'filesystem' => 'tmpfs',
            'max_bytes_per_stream' => ENGINE_OUTPUT_CAPTURE_MAX_BYTES,
            'write_sync' => 'O_SYNC',
            'pre' => $captureBefore,
            'post' => $captureAfter,
        ];
        if (!is_array($captureProof) || $captureProof !== $expectedCaptureProof) {
            return ['error' => 'cgroup-v2 sampler returned invalid engine output capture proof'];
        }
        $browserProofError = sampler_browser_shared_memory_proof_error(
            $measurement,
            $command,
            $captureBefore
        );
        if ($browserProofError !== null) {
            return ['error' => $browserProofError];
        }
        $diagnostics = $measurement['sampled_diagnostics'];
        if (!is_array($diagnostics)) {
            return ['error' => 'cgroup-v2 sampler returned invalid sampled_diagnostics'];
        }
        return [
            'sampler_stdout' => $measurementJson,
            'sampler_stderr' => $samplerError,
            'wall_ms' => (float) $measurement['wall_ms'],
            // Unlike engine wall time, this includes sampler launch, capture
            // validation, descendant drain, retained-counter settlement, and
            // sampler exit. Serial throughput uses this sampler-lifecycle boundary.
            'one_shot_wall_ms' => round($wallMs, 3),
            'user_ms' => (float) $measurement['cpu_user_ms'],
            'sys_ms' => (float) $measurement['cpu_sys_ms'],
            'memory_current_bytes' => (int) $measurement['memory_current_bytes'],
            'memory_peak_bytes' => (int) $measurement['memory_peak_bytes'],
            'sampled_peak_rss_kib_lower_bound' => isset($diagnostics['sampled_peak_summed_rss_kib_lower_bound'])
                ? (int) $diagnostics['sampled_peak_summed_rss_kib_lower_bound']
                : null,
            'sampled_peak_pss_kib_lower_bound' => isset($diagnostics['sampled_peak_summed_pss_kib_lower_bound'])
                ? (int) $diagnostics['sampled_peak_summed_pss_kib_lower_bound']
                : null,
            'read_bytes' => (int) $measurement['read_bytes'],
            'write_bytes' => (int) $measurement['write_bytes'],
            'read_operations' => (int) $measurement['read_operations'],
            'write_operations' => (int) $measurement['write_operations'],
            'measurement_method' => (string) $measurement['method'],
            'signal' => isset($measurement['signal']) ? (int) $measurement['signal'] : null,
            'resource_usage' => $resourceUsage,
            'exit_code' => (int) $measurement['exit_code'],
            'stdout' => $stdout,
            'stderr' => $stderr,
        ];
    }

    return [
        'wall_ms' => round($wallMs, 3),
        'one_shot_wall_ms' => round($wallMs, 3),
        'user_ms' => null,
        'sys_ms' => null,
        'memory_current_bytes' => null,
        'memory_peak_bytes' => null,
        'sampled_peak_rss_kib_lower_bound' => null,
        'sampled_peak_pss_kib_lower_bound' => null,
        'read_bytes' => null,
        'write_bytes' => null,
        'read_operations' => null,
        'write_operations' => null,
        'measurement_method' => 'unavailable',
        'signal' => null,
        'resource_usage' => null,
        'exit_code' => $launcherExitCode,
        'stdout' => $stdout,
        'stderr' => $stderr,
    ];
}

/** @return array<string, mixed>|null */
function read_json_file(string $path): ?array
{
    if (!is_file($path)) {
        return null;
    }
    $value = json_decode((string) file_get_contents($path), true);
    return is_array($value) ? $value : null;
}

/** @return array<string, mixed>|null */
function parse_api2_result(string $stdout): ?array
{
    if ($stdout === '' || str_contains($stdout, "\r") || !str_ends_with($stdout, "\n")
        || substr_count($stdout, "\n") !== 1) {
        return null;
    }
    $frame = substr($stdout, 0, -1);
    try {
        $value = json_decode($frame, true, flags: JSON_THROW_ON_ERROR);
    } catch (JsonException) {
        return null;
    }
    if (!is_array($value) || array_is_list($value) || $value === []) {
        return null;
    }
    $canonical = json_encode(
        $value,
        JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_LINE_TERMINATORS
    );
    return is_string($canonical) && hash_equals($canonical, $frame) ? $value : null;
}

/** @return array<string, mixed>|null */
function parse_adapter_summary(string $stdout): ?array
{
    foreach (array_reverse(preg_split('/\r?\n/', $stdout) ?: []) as $line) {
        $line = trim($line);
        if ($line === '') {
            continue;
        }
        if (is_object(json_decode($line))) {
            $value = json_decode($line, true);
            if (is_array($value) && $value !== []) {
                return $value;
            }
        }
    }
    return null;
}

function api2_descriptor_matches(
    mixed $descriptor,
    string $path,
    string $mediaType,
    ?int $bytes,
    ?string $sha256
): bool {
    return is_array($descriptor)
        && count($descriptor) === 4
        && array_key_exists('path', $descriptor)
        && array_key_exists('media_type', $descriptor)
        && array_key_exists('sha256', $descriptor)
        && array_key_exists('bytes', $descriptor)
        && ($descriptor['path'] ?? null) === $path
        && ($descriptor['media_type'] ?? null) === $mediaType
        && ($descriptor['bytes'] ?? null) === $bytes
        && ($descriptor['sha256'] ?? null) === (is_string($sha256) ? 'sha256:' . $sha256 : null);
}

function path_contains_symlink(string $root, string $relative): bool
{
    $current = rtrim($root, DIRECTORY_SEPARATOR);
    foreach (explode('/', $relative) as $segment) {
        $current .= DIRECTORY_SEPARATOR . $segment;
        if (is_link($current)) {
            return true;
        }
    }
    return false;
}

/** @param list<mixed> $descriptors */
function api2_diagnostics_match(array $descriptors, string $jobRoot): bool
{
    $diagnosticsRoot = $jobRoot . DIRECTORY_SEPARATOR . 'diagnostics';
    $resolvedJobRoot = realpath($jobRoot);
    $resolvedDiagnosticsRoot = realpath($diagnosticsRoot);
    if ($resolvedJobRoot === false || $resolvedDiagnosticsRoot === false
        || !is_dir($resolvedDiagnosticsRoot) || is_link($diagnosticsRoot)) {
        return false;
    }
    $described = [];
    foreach ($descriptors as $descriptor) {
        if (!is_array($descriptor) || count($descriptor) !== 4
            || !array_key_exists('path', $descriptor)
            || !array_key_exists('media_type', $descriptor)
            || !array_key_exists('sha256', $descriptor)
            || !array_key_exists('bytes', $descriptor)) {
            return false;
        }
        $path = $descriptor['path'];
        $relative = is_string($path) && str_starts_with($path, 'diagnostics/')
            ? substr($path, strlen('diagnostics/'))
            : '';
        if (!is_api2_portable_path($relative)
            || isset($described[$path])
            || path_contains_symlink($jobRoot, $path)
            || !is_string($descriptor['media_type'])
            || strlen($descriptor['media_type']) < 3
            || strlen($descriptor['media_type']) > 255
            || !is_int($descriptor['bytes'])
            || $descriptor['bytes'] < 1
            || !is_string($descriptor['sha256'])
            || preg_match('/^sha256:[0-9a-f]{64}$/D', $descriptor['sha256']) !== 1) {
            return false;
        }
        $candidate = $jobRoot . DIRECTORY_SEPARATOR . str_replace('/', DIRECTORY_SEPARATOR, $path);
        $resolved = realpath($candidate);
        $prefix = $resolvedDiagnosticsRoot . DIRECTORY_SEPARATOR;
        if ($resolved === false || !is_file($resolved) || !str_starts_with($resolved, $prefix)) {
            return false;
        }
        $bytes = filesize($resolved);
        $sha256 = hash_file('sha256', $resolved);
        if ($bytes !== $descriptor['bytes'] || !is_string($sha256)
            || !hash_equals('sha256:' . $sha256, $descriptor['sha256'])) {
            return false;
        }
        $described[$path] = true;
    }

    $actual = [];
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($resolvedDiagnosticsRoot, FilesystemIterator::SKIP_DOTS)
    );
    foreach ($iterator as $entry) {
        if ($entry->isLink() || !$entry->isFile()) {
            return false;
        }
        $resolved = realpath($entry->getPathname());
        if ($resolved === false || !str_starts_with($resolved, $resolvedJobRoot . DIRECTORY_SEPARATOR)) {
            return false;
        }
        $relative = str_replace('\\', '/', substr($resolved, strlen($resolvedJobRoot) + 1));
        $actual[$relative] = true;
    }
    ksort($actual, SORT_STRING);
    ksort($described, SORT_STRING);
    return $actual === $described;
}

function rrmdir(string $path): void
{
    if (!is_dir($path)) {
        return;
    }
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($path, FilesystemIterator::SKIP_DOTS),
        RecursiveIteratorIterator::CHILD_FIRST
    );
    foreach ($iterator as $entry) {
        if ($entry->isDir()) {
            @rmdir($entry->getPathname());
        } else {
            @unlink($entry->getPathname());
        }
    }
    @rmdir($path);
}

function canonical_json_frame(array $value, string $label): string
{
    try {
        return json_encode(
            $value,
            JSON_UNESCAPED_SLASHES
                | JSON_UNESCAPED_UNICODE
                | JSON_UNESCAPED_LINE_TERMINATORS
                | JSON_THROW_ON_ERROR
        ) . "\n";
    } catch (JsonException $error) {
        fail("cannot encode canonical {$label}: {$error->getMessage()}");
    }
}

function api2_media_type(string $path): string
{
    return match (strtolower(pathinfo($path, PATHINFO_EXTENSION))) {
        'css' => 'text/css;charset=utf-8',
        'html', 'htm' => 'text/html;charset=utf-8',
        'js', 'mjs' => 'text/javascript;charset=utf-8',
        'json' => 'application/json',
        'svg' => 'image/svg+xml',
        'gif' => 'image/gif',
        'jpg', 'jpeg' => 'image/jpeg',
        'png' => 'image/png',
        'webp' => 'image/webp',
        'woff' => 'font/woff',
        'woff2' => 'font/woff2',
        'ttf' => 'font/ttf',
        'otf' => 'font/otf',
        default => 'application/octet-stream',
    };
}

function is_api2_portable_path(string $path): bool
{
    if (strlen($path) < 1 || strlen($path) > 240
        || preg_match('/^[A-Za-z0-9](?:[A-Za-z0-9._\/-]*[A-Za-z0-9_-])?$/D', $path) !== 1) {
        return false;
    }
    $segments = explode('/', $path);
    if (count($segments) > 32) {
        return false;
    }
    foreach ($segments as $segment) {
        if ($segment === '' || $segment === '.' || $segment === '..' || strlen($segment) > 100
            || $segment !== rtrim($segment, '. ')
            || preg_match('/^(?:CON|PRN|AUX|NUL|CLOCK\$|COM[1-9]|LPT[1-9])(?:\.|$)/iD', $segment) === 1) {
            return false;
        }
    }
    return true;
}

function css_number_to_app_units(string $value, string $label, bool $allowZero = false): int
{
    if (preg_match('/^(?:0|[1-9][0-9]*)(?:\.[0-9]+)?$/D', $value) !== 1) {
        fail("{$label} must be a canonical nonnegative CSS-pixel number");
    }
    $number = (float) $value;
    if (!is_finite($number) || $number > 2_147_483_647 / 60) {
        fail("{$label} exceeds the API 2 app-unit range");
    }
    $appUnits = (int) round($number * 60, 0, PHP_ROUND_HALF_UP);
    if ((!$allowZero && $appUnits < 1) || ($allowZero && $appUnits < 0)) {
        fail("{$label} is outside the API 2 app-unit range");
    }
    return $appUnits;
}

/** @return array{name: string}|array{width_app_units: int, height_app_units: int} */
function api2_page_size(?string $value): array
{
    if ($value === 'A4') {
        return ['name' => 'A4'];
    }
    $value ??= '816x1056';
    $exact = str_ends_with($value, 'au');
    if ($exact) {
        $value = substr($value, 0, -2);
    }
    if (preg_match('/^([^x]+)x([^x]+)$/D', $value, $match) !== 1) {
        fail('--page-size must be A4, WIDTHxHEIGHT in CSS pixels, or WIDTHxHEIGHTau');
    }
    $convert = $exact ? 'exact_app_units' : 'css_number_to_app_units';
    return [
        'width_app_units' => $convert($match[1], 'page width'),
        'height_app_units' => $convert($match[2], 'page height'),
    ];
}

/** @return array{top: int, right: int, bottom: int, left: int} */
function api2_page_margins(?string $value): array
{
    $value ??= '48,48,48,48';
    $exact = str_ends_with($value, 'au');
    if ($exact) {
        $value = substr($value, 0, -2);
    }
    $parts = explode(',', $value);
    if (count($parts) !== 4) {
        fail('--page-margins must contain TOP,RIGHT,BOTTOM,LEFT CSS pixels or an integer tuple ending in au');
    }
    $convert = $exact ? 'exact_app_units' : 'css_number_to_app_units';
    return [
        'top' => $convert($parts[0], 'top margin', true),
        'right' => $convert($parts[1], 'right margin', true),
        'bottom' => $convert($parts[2], 'bottom margin', true),
        'left' => $convert($parts[3], 'left margin', true),
    ];
}

function exact_app_units(string $value, string $label, bool $allowZero = false): int
{
    if (preg_match('/^(0|[1-9][0-9]*)$/D', $value) !== 1
        || strlen($value) > 10 || (int) $value > 2_147_483_647
        || (!$allowZero && $value === '0')) {
        fail("{$label} must be a canonical " . ($allowZero ? 'nonnegative' : 'positive') . ' i32 app-unit integer');
    }
    return (int) $value;
}

function root_wall_timeout_option(?string $value): ?float
{
    if ($value === null) {
        return null;
    }
    if (preg_match('/^(?:0|[1-9][0-9]*)(?:\.[0-9]+)?$/D', $value) !== 1
        || !is_finite((float) $value) || (float) $value <= 0) {
        fail('--root-wall-timeout-ms must be a finite positive number');
    }
    return (float) $value;
}

function prepare_retention_root(string $path, string $fixtureCwd): string
{
    $parent = realpath(dirname($path));
    $name = basename($path);
    if ((!str_starts_with($path, DIRECTORY_SEPARATOR) && !is_windows_absolute_path($path))
        || $parent === false || !is_dir($parent) || !is_bare_input_name($name)
        || file_exists($path) || is_link($path)) {
        throw new RuntimeException('--retain-root must be a fresh absolute directory with an existing parent');
    }
    $resolved = $parent . DIRECTORY_SEPARATOR . $name;
    $comparison = PHP_OS_FAMILY === 'Windows' ? strtolower($resolved) : $resolved;
    $fixture = PHP_OS_FAMILY === 'Windows' ? strtolower($fixtureCwd) : $fixtureCwd;
    if ($comparison === $fixture || str_starts_with($comparison, $fixture . DIRECTORY_SEPARATOR)) {
        throw new RuntimeException('--retain-root must be outside the frozen fixture cwd');
    }
    if (!mkdir($resolved, 0700)) {
        throw new RuntimeException('cannot exclusively create retention root');
    }
    return $resolved;
}

/** Wall intervals, not CPU accounting scopes; null means no complete measurement. */
function benchmark_timing_boundaries(array $exec): array
{
    $usage = $exec['resource_usage'] ?? null;
    $root = isset($exec['wall_ms']) ? (float) $exec['wall_ms'] : null;
    $drain = is_object($usage) && isset($usage->drain_ms) ? (float) $usage->drain_ms : null;
    return [
        'root_wall_ms' => $root,
        'tree_wall_ms' => $root !== null && $drain !== null ? round($root + $drain, 3) : null,
        'sampler_lifecycle_wall_ms' => $exec['one_shot_wall_ms'] ?? null,
        'semantics' => [
            'root_wall_ms' => 'Linux: measured root SIGCONT through pidfd-observed root exit; non-Linux: proc_open through proc_close',
            'tree_wall_ms' => 'Linux: root wall plus observed descendant drain, excluding accounting settle; unavailable elsewhere',
            'sampler_lifecycle_wall_ms' => 'PHP proc_open through proc_close, including Linux sampler startup, drain, accounting settle and exit',
            'excluded' => 'input staging, PDF correctness oracles, retained evidence copying, application storage',
        ],
    ];
}

/** Copy post-execution regular files only. Failure leaves both original and partial proof intact. */
function retain_sample_evidence(array $state, int $index, array $sample, array $exec, array $directories): string
{
    $root = $state['retainRoot'] . DIRECTORY_SEPARATOR . 'sample-' . $index;
    if (!@mkdir($root, 0700)) {
        throw new RuntimeException("retained sample already exists or cannot be created: {$root}");
    }
    $inventory = [];
    $totalBytes = 0;
    $write = static function (string $relative, string $bytes) use ($root, &$inventory, &$totalBytes): void {
        if (strlen($bytes) > 64 * 1024 * 1024 || $totalBytes + strlen($bytes) > 512 * 1024 * 1024
            || count($inventory) >= 8192 || !is_safe_fixture_path($relative)) {
            throw new RuntimeException('retained evidence exceeds file, byte, or path bounds');
        }
        $target = $root . DIRECTORY_SEPARATOR . str_replace('/', DIRECTORY_SEPARATOR, $relative);
        $parent = dirname($target);
        if (!is_dir($parent) && !mkdir($parent, 0700, true)) {
            throw new RuntimeException('cannot create retained evidence subdirectory');
        }
        $stream = fopen($target, 'xb');
        if ($stream === false) {
            throw new RuntimeException('cannot exclusively create retained evidence file');
        }
        try {
            if (fwrite($stream, $bytes) !== strlen($bytes)) {
                throw new RuntimeException('short retained evidence write');
            }
        } finally {
            fclose($stream);
        }
        $hash = hash('sha256', $bytes);
        if (hash_file('sha256', $target) !== $hash) {
            throw new RuntimeException('retained evidence readback mismatch');
        }
        $inventory[$relative] = ['bytes' => strlen($bytes), 'sha256' => $hash];
        $totalBytes += strlen($bytes);
    };
    foreach ($directories as $name => $directory) {
        if (!file_exists($directory)) {
            continue;
        }
        $source = realpath($directory);
        if ($source === false || is_link($directory) || !is_dir($source) || !is_bare_input_name($name)) {
            throw new RuntimeException('unsafe retained evidence source directory');
        }
        if (str_starts_with($root, $source . DIRECTORY_SEPARATOR)
            || str_starts_with($source, $root . DIRECTORY_SEPARATOR) || $root === $source) {
            throw new RuntimeException('retained evidence source and destination overlap');
        }
        if (!mkdir($root . DIRECTORY_SEPARATOR . $name, 0700)) {
            throw new RuntimeException('cannot create retained source directory');
        }
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($source, FilesystemIterator::SKIP_DOTS),
            RecursiveIteratorIterator::SELF_FIRST
        );
        foreach ($iterator as $entry) {
            $path = $entry->getPathname();
            $resolved = realpath($path);
            if ($entry->isLink() || $resolved === false || !str_starts_with($resolved, $source . DIRECTORY_SEPARATOR)) {
                throw new RuntimeException('retained evidence source contains a link or escaped path');
            }
            if ($entry->isDir()) {
                continue;
            }
            $metadata = lstat($path);
            if (!$entry->isFile() || $metadata === false || ($metadata['nlink'] ?? 0) !== 1
                || $entry->getSize() > 64 * 1024 * 1024) {
                throw new RuntimeException('retained evidence source must contain bounded non-hardlinked regular files');
            }
            $bytes = file_get_contents($path, false, null, 0, 64 * 1024 * 1024 + 1);
            if (!is_string($bytes)) {
                throw new RuntimeException('cannot read retained evidence source');
            }
            $write($name . '/' . str_replace('\\', '/', substr($path, strlen($source) + 1)), $bytes);
            if (hash_file('sha256', $path) !== hash('sha256', $bytes)) {
                throw new RuntimeException('retained evidence source changed during copy');
            }
        }
    }
    foreach (['stdout', 'stderr', 'sampler_stdout', 'sampler_stderr', 'request'] as $name) {
        if (isset($exec[$name]) && is_string($exec[$name])) {
            $write($name === 'request' ? 'request.json' : $name . '.txt', $exec[$name]);
        }
    }
    $write('sample.json', json_encode($sample, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE | JSON_THROW_ON_ERROR) . "\n");
    ksort($inventory, SORT_STRING);
    $manifest = [
        'schema' => 'pliego.benchmark-retained-sample.v1',
        'index' => $index,
        'phase' => $index === -1000000 ? 'preflight' : ($index < 0 ? 'warmup' : 'timed'),
        'binary' => $state['binary'],
        'binary_sha256' => $state['binarySha256'],
        'fixture_input_sha256' => $state['fixtureInputSha256'],
        'fixture_bundle_sha256' => $state['fixtureBundleSha256'],
        'root_wall_timeout_ms' => $state['rootWallTimeoutMs'],
        'timing' => benchmark_timing_boundaries($exec),
        'files' => $inventory,
    ];
    $write('manifest.json', json_encode($manifest, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR) . "\n");
    return $root;
}

function retain_benchmark_attempt(array $state, int $index, array $sample, array $exec, array $directories): array
{
    if ($state['retainRoot'] === null) {
        return $sample;
    }
    $root = $state['retainRoot'] . DIRECTORY_SEPARATOR . 'sample-' . $index;
    $sample['retained'] = [
        'artifacts_dir' => $root . DIRECTORY_SEPARATOR . (isset($directories['job']) ? 'job' : 'artifacts'),
        'output_dir' => $root . DIRECTORY_SEPARATOR . (isset($directories['job']) ? 'job/delivery' : 'output'),
    ];
    retain_sample_evidence($state, $index, $sample, $exec, $directories);
    return $sample;
}

/** @param list<string> $command */
function run_windows_acl_tool(array $command, string $label): string
{
    $descriptors = [
        0 => ['file', 'NUL', 'r'],
        1 => ['pipe', 'w'],
        2 => ['pipe', 'w'],
    ];
    $process = proc_open($command, $descriptors, $pipes, null, null, ['bypass_shell' => true]);
    if (!is_resource($process)) {
        fail("cannot start Windows {$label}");
    }
    $stdout = stream_get_contents($pipes[1]);
    $stderr = stream_get_contents($pipes[2]);
    fclose($pipes[1]);
    fclose($pipes[2]);
    $exitCode = proc_close($process);
    if ($exitCode !== 0) {
        fail("Windows {$label} failed with exit {$exitCode}: " . trim((string) ($stderr ?: $stdout)));
    }
    return (string) $stdout;
}

function harden_windows_job_root(string $path): void
{
    $systemRoot = getenv('SystemRoot');
    if (!is_string($systemRoot) || !is_windows_absolute_path($systemRoot)) {
        fail('SystemRoot does not identify an absolute Windows directory');
    }
    $system32 = rtrim($systemRoot, "\\/") . DIRECTORY_SEPARATOR . 'System32' . DIRECTORY_SEPARATOR;
    $whoami = realpath($system32 . 'whoami.exe');
    $icacls = realpath($system32 . 'icacls.exe');
    if (!is_string($whoami) || !is_string($icacls)) {
        fail('required Windows ACL tools are unavailable');
    }
    $identity = run_windows_acl_tool([$whoami, '/user', '/fo', 'csv', '/nh'], 'current-user lookup');
    if (preg_match('/(?<![A-Za-z0-9-])(S-1-(?:[0-9]+-)+[0-9]+)(?![A-Za-z0-9-])/', $identity, $match) !== 1) {
        fail('whoami.exe did not return one current-user SID');
    }
    $sid = $match[1];
    foreach ([
        [[$icacls, $path, '/reset', '/q'], 'DACL reset'],
        [[$icacls, $path, '/inheritance:r', '/q'], 'DACL inheritance removal'],
        [[$icacls, $path, '/grant:r', "*{$sid}:(OI)(CI)F", '/q'], 'owner-only DACL assignment'],
        [[$icacls, $path, '/setowner', "*{$sid}", '/q'], 'owner assignment'],
    ] as [$command, $label]) {
        run_windows_acl_tool($command, $label);
    }
}

function sync_benchmark_path(string $path, bool $directory): void
{
    if (!function_exists('fsync')) {
        fail('PHP fsync support is required to seal benchmark staging');
    }
    $stream = @fopen($path, $directory ? 'rb' : 'r+b');
    if ($stream === false) {
        fail("cannot durably seal benchmark staging path: {$path}");
    }
    if (!$directory) {
        while (!feof($stream)) {
            $bytes = fread($stream, 1_048_576);
            if ($bytes === false || ($bytes === '' && !feof($stream))) {
                fclose($stream);
                fail("cannot pre-read benchmark staging path: {$path}");
            }
        }
    }
    if (!fsync($stream)) {
        if (is_resource($stream)) {
            fclose($stream);
        }
        fail("cannot durably seal benchmark staging path: {$path}");
    }
    fclose($stream);
}

function verify_staged_bytes(string $path, string $expected, string $label): void
{
    $actual = file_get_contents($path);
    if (!is_string($actual) || $actual !== $expected) {
        fail("staged benchmark {$label} differs from its canonical bytes");
    }
}

function seal_benchmark_tree(string $path): void
{
    $root = realpath($path);
    if ($root === false || !is_dir($root) || is_link($path)) {
        fail("benchmark staging root is unavailable or unsafe: {$path}");
    }
    $files = [];
    $directories = [$root];
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS),
        RecursiveIteratorIterator::CHILD_FIRST
    );
    foreach ($iterator as $entry) {
        $entryPath = $entry->getPathname();
        if ($entry->isLink()) {
            fail("benchmark staging tree contains a symbolic link: {$entryPath}");
        }
        if ($entry->isFile()) {
            $files[] = $entryPath;
        } elseif ($entry->isDir()) {
            $directories[] = $entryPath;
        } else {
            fail("benchmark staging tree contains a special file: {$entryPath}");
        }
    }
    sort($files, SORT_STRING);
    usort($directories, static function (string $left, string $right): int {
        $depth = substr_count($right, DIRECTORY_SEPARATOR) <=> substr_count($left, DIRECTORY_SEPARATOR);
        return $depth !== 0 ? $depth : strcmp($left, $right);
    });
    foreach ($files as $file) {
        sync_benchmark_path($file, false);
    }
    foreach ($directories as $directory) {
        sync_benchmark_path($directory, true);
    }
}

/** @return array{sandbox: string, root: string, temporary: string, request: string, pdf: string, scene: string, bundle: string} */
function stage_api2_job(array $state): array
{
    $sandboxRoot = PHP_OS_FAMILY === 'Linux'
        ? benchmark_engine_temporary_path('pliego-bench-api2-')
        : sys_get_temp_dir() . '/pliego-bench-api2-' . bin2hex(random_bytes(8));
    $jobRoot = $sandboxRoot . DIRECTORY_SEPARATOR . 'job';
    $temporaryRoot = $sandboxRoot . DIRECTORY_SEPARATOR . 'temporary';
    if (PHP_OS_FAMILY === 'Linux') {
        prepare_engine_directory($sandboxRoot, $state['engineUid'], $state['engineGid']);
    } elseif (!mkdir($sandboxRoot, 0700) || !chmod($sandboxRoot, 0700)) {
        fail("cannot create private API 2 sandbox root: {$sandboxRoot}");
    }
    if (PHP_OS_FAMILY === 'Windows') {
        harden_windows_job_root($sandboxRoot);
    }

    $inputRoot = $jobRoot . DIRECTORY_SEPARATOR . 'input';
    if (!mkdir($jobRoot, 0700) || !mkdir($inputRoot, 0700)
        || (PHP_OS_FAMILY !== 'Linux' && !mkdir($temporaryRoot, 0700))) {
        rrmdir($sandboxRoot);
        fail("cannot create private API 2 sandbox directories below: {$sandboxRoot}");
    }
    $paths = [$state['input'], ...$state['fixtureAssets']];
    sort($paths, SORT_STRING);
    if (count($paths) > 16_384 || count(array_unique(array_map('strtolower', $paths))) !== count($paths)) {
        fail('API 2 fixture has too many entries or case-colliding paths');
    }

    $entries = [];
    $totalBytes = 0;
    $stagedBundle = hash_init('sha256');
    $stagedInputHash = null;
    foreach ($paths as $relative) {
        if (!is_api2_portable_path($relative)) {
            fail("fixture path is not portable under API 2: {$relative}");
        }
        $source = realpath($state['cwd'] . DIRECTORY_SEPARATOR . str_replace('/', DIRECTORY_SEPARATOR, $relative));
        $prefix = rtrim($state['cwd'], DIRECTORY_SEPARATOR) . DIRECTORY_SEPARATOR;
        if ($source === false || !is_file($source) || is_link($source) || !str_starts_with($source, $prefix)) {
            fail("cannot stage API 2 fixture path: {$relative}");
        }
        $destination = $inputRoot . DIRECTORY_SEPARATOR . str_replace('/', DIRECTORY_SEPARATOR, $relative);
        $parent = dirname($destination);
        if (!is_dir($parent) && !mkdir($parent, 0700, true)) {
            fail("cannot create API 2 fixture directory: {$parent}");
        }
        if (!copy($source, $destination) || !chmod($destination, 0600)) {
            fail("cannot copy API 2 fixture path: {$relative}");
        }
        $bytes = filesize($destination);
        $sha256 = hash_file('sha256', $destination);
        if (!is_int($bytes) || !is_string($sha256) || $bytes > 67_108_864) {
            fail("cannot inspect bounded API 2 fixture path: {$relative}");
        }
        $totalBytes += $bytes;
        if ($totalBytes > 67_108_864) {
            fail('API 2 fixture content exceeds 64 MiB in total');
        }
        hash_update($stagedBundle, $relative . "\0" . hex2bin($sha256));
        if ($relative === $state['input']) {
            $stagedInputHash = $sha256;
        }
        $entries[] = [
            'path' => $relative,
            'media_type' => api2_media_type($relative),
            'sha256' => 'sha256:' . $sha256,
            'bytes' => $bytes,
        ];
    }
    $stagedBundleHash = hash_final($stagedBundle);
    if (!is_string($stagedInputHash)
        || !hash_equals($state['fixtureInputSha256'], $stagedInputHash)
        || !hash_equals($state['fixtureBundleSha256'], $stagedBundleHash)) {
        fail('staged API 2 input closure differs from the declared fixture identity', 1);
    }

    $manifest = canonical_json_frame([
        'schema' => 'pliego.input-manifest',
        'version' => 1,
        'url_root' => 'pliego-input:///',
        'entries' => $entries,
    ], 'API 2 input manifest');
    if (strlen($manifest) > 16_777_216) {
        fail('canonical API 2 input manifest exceeds 16 MiB');
    }
    $manifestPath = $jobRoot . DIRECTORY_SEPARATOR . 'input-manifest.json';
    if (file_put_contents($manifestPath, $manifest, LOCK_EX) !== strlen($manifest)
        || !chmod($manifestPath, 0600)) {
        fail('cannot write canonical API 2 input manifest');
    }
    // Verify the canonical bytes here; seal_benchmark_tree performs a second,
    // complete read after the final ownership/mode changes and then fsyncs it.
    verify_staged_bytes($manifestPath, $manifest, 'input manifest');

    if (PHP_OS_FAMILY !== 'Windows') {
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($sandboxRoot, FilesystemIterator::SKIP_DOTS),
            RecursiveIteratorIterator::SELF_FIRST
        );
        foreach ($iterator as $entry) {
            $mode = $entry->isDir() ? 0700 : 0600;
            if (!chmod($entry->getPathname(), $mode)) {
                fail("cannot secure API 2 job node: {$entry->getPathname()}");
            }
            if (PHP_OS_FAMILY === 'Linux'
                && (!chown($entry->getPathname(), $state['engineUid'])
                    || !chgrp($entry->getPathname(), $state['engineGid']))) {
                fail("cannot delegate API 2 job node: {$entry->getPathname()}");
            }
        }
        seal_benchmark_tree($sandboxRoot);
    }

    $locale = $state['locale'] ?? 'en-US';
    $timezone = $state['timezone'] ?? 'UTC';
    $timezone = $timezone === 'PST8PDT' ? 'America/Tijuana' : $timezone;
    if (!in_array($locale, ['en-US', 'es-MX'], true)
        || !in_array($timezone, ['UTC', 'America/Tijuana'], true)) {
        fail('API 2 locale/timezone must be en-US|es-MX and UTC|America/Tijuana');
    }
    $manifestDescriptor = [
        'path' => 'input-manifest.json',
        'media_type' => 'application/vnd.pliego.input-manifest+json',
        'sha256' => 'sha256:' . hash('sha256', $manifest),
        'bytes' => strlen($manifest),
    ];
    $request = canonical_json_frame([
        'schema' => 'pliego.render-request',
        'version' => 1,
        'api' => 2,
        'profile' => null,
        'input' => [
            'entrypoint' => $state['input'],
            'manifest' => $manifestDescriptor,
        ],
        'environment' => [
            'locale' => $locale,
            'timezone' => $timezone,
        ],
        'page' => [
            'size' => api2_page_size($state['pageSize']),
            'margins_app_units' => api2_page_margins($state['pageMargins']),
            'geometry_authority' => 'request-only-v1',
        ],
        'resources' => [
            'network' => 'deny',
            'host_fonts' => 'deny',
        ],
        'time' => [
            'policy_version' => 1,
            'epoch_unix_ms' => 946_684_800_000,
            'initial_offset_ns' => 0,
        ],
        'settlement' => [
            'policy_version' => 1,
            'infinite_source_policy' => 'fail',
            'empty_checkpoints' => 2,
            'limits' => [
                'virtual_span_ms' => 86_400_000,
                'ordinary_tasks' => 100_000,
                'microtasks' => 1_000_000,
                'rendering_opportunities' => 10_000,
                'mutations' => 1_000_000,
                'host_wall_ms' => 60_000,
            ],
        ],
        'diagnostics' => [
            'retention' => 'always',
        ],
    ], 'API 2 render request');
    if (strlen($request) > 1_048_576) {
        fail('canonical API 2 render request exceeds 1 MiB');
    }
    return [
        'sandbox' => $sandboxRoot,
        'root' => $jobRoot,
        'temporary' => $temporaryRoot,
        'request' => $request,
        'pdf' => $jobRoot . DIRECTORY_SEPARATOR . 'delivery' . DIRECTORY_SEPARATOR . 'document.pdf',
        'scene' => $jobRoot . DIRECTORY_SEPARATOR . 'delivery' . DIRECTORY_SEPARATOR . 'scene.json',
        'bundle' => $jobRoot . DIRECTORY_SEPARATOR . 'delivery' . DIRECTORY_SEPARATOR . 'bundle.json',
    ];
}

function api2_request_file(string $request): string
{
    $root = sys_get_temp_dir() . '/pliego-bench-api2-request-' . bin2hex(random_bytes(8));
    if (!mkdir($root, 0700) || !chmod($root, 0700)) {
        fail('cannot create private API 2 stdin root');
    }
    if (PHP_OS_FAMILY === 'Windows') {
        harden_windows_job_root($root);
    }
    $path = $root . DIRECTORY_SEPARATOR . 'request.json';
    if (file_put_contents($path, $request, LOCK_EX) !== strlen($request)
        || (PHP_OS_FAMILY === 'Linux' && !chmod($path, 0400))) {
        rrmdir($root);
        fail('cannot create immutable API 2 stdin request');
    }
    verify_staged_bytes($path, $request, 'stdin request');
    if (PHP_OS_FAMILY === 'Linux') {
        seal_benchmark_tree($root);
    }
    $resolved = realpath($path);
    if ($resolved === false || $resolved !== $path) {
        rrmdir($root);
        fail('API 2 stdin request path is not canonical');
    }
    return $resolved;
}

/** @return array{pass: bool, page_count: int|null, page_dimensions_points: array|null,
 *     normalized_text: string|null, fonts: array, normalized_raster_sha256: string|null,
 *     checks: list<array<string, string>>} */
function run_pdf_oracle(array $state, string $pdfPath): array
{
    $script = dirname(__DIR__) . '/tools/pdf_oracle.py';
    $python = PHP_OS_FAMILY === 'Linux' ? sampler_interpreter() : 'python';
    if ($python === null || !is_file($script)) {
        return [
            'pass' => false,
            'page_count' => null,
            'page_dimensions_points' => null,
            'normalized_text' => null,
            'fonts' => [],
            'normalized_raster_sha256' => null,
            'checks' => [[
                'name' => 'pdf_oracle',
                'status' => 'fail',
                'detail' => 'oracle interpreter or script is unavailable',
            ]],
        ];
    }
    $command = [$python, '-I', $script, '--pdf', $pdfPath];
    if ($state['pageCount'] !== null) {
        array_push($command, '--page-count', (string) $state['pageCount']);
    }
    if ($state['pageWidthPoints'] !== null && $state['pageHeightPoints'] !== null) {
        array_push(
            $command,
            '--page-width-points',
            (string) $state['pageWidthPoints'],
            '--page-height-points',
            (string) $state['pageHeightPoints'],
            '--dimension-tolerance-points',
            (string) $state['dimensionTolerancePoints']
        );
    }
    foreach ($state['textContains'] as $fragment) {
        array_push($command, '--text-contains', $fragment);
    }
    if ($state['textEquals'] !== null) {
        array_push($command, '--text-equals', $state['textEquals']);
    }
    foreach ($state['fontFamilies'] as $family) {
        array_push($command, '--font-family', $family);
    }
    if ($state['rasterSha256'] !== null) {
        array_push($command, '--raster-sha256', $state['rasterSha256']);
    }
    foreach ($state['linkTargets'] as $target) {
        array_push($command, '--link-target', $target);
    }
    $descriptors = [
        0 => ['file', PHP_OS_FAMILY === 'Windows' ? 'NUL' : '/dev/null', 'r'],
        1 => ['pipe', 'w'],
        2 => ['pipe', 'w'],
    ];
    $process = proc_open($command, $descriptors, $pipes, $state['cwd']);
    if (!is_resource($process)) {
        return [
            'pass' => false,
            'page_count' => null,
            'page_dimensions_points' => null,
            'normalized_text' => null,
            'fonts' => [],
            'normalized_raster_sha256' => null,
            'checks' => [['name' => 'pdf_oracle', 'status' => 'fail', 'detail' => 'proc_open failed']],
        ];
    }
    $stdout = stream_get_contents($pipes[1]);
    $stderr = stream_get_contents($pipes[2]);
    fclose($pipes[1]);
    fclose($pipes[2]);
    $exitCode = proc_close($process);
    $decoded = json_decode((string) $stdout, true);
    if (!is_array($decoded) || ($decoded['contract'] ?? null) !== 'pliego.pdf-oracle.v1'
        || !is_bool($decoded['pass'] ?? null) || !is_array($decoded['checks'] ?? null)) {
        $oracleError = is_array($decoded) && is_string($decoded['error'] ?? null)
            ? $decoded['error']
            : null;
        return [
            'pass' => false,
            'page_count' => null,
            'page_dimensions_points' => null,
            'normalized_text' => null,
            'fonts' => [],
            'normalized_raster_sha256' => null,
            'checks' => [[
                'name' => 'pdf_oracle',
                'status' => 'fail',
                'detail' => $oracleError ?? (trim((string) $stderr) ?: "invalid oracle output (exit {$exitCode})"),
            ]],
        ];
    }
    return [
        'pass' => $decoded['pass'] && $exitCode === 0,
        'page_count' => isset($decoded['page_count']) ? (int) $decoded['page_count'] : null,
        'page_dimensions_points' => is_array($decoded['page_dimensions_points'] ?? null)
            ? $decoded['page_dimensions_points']
            : null,
        'normalized_text' => is_string($decoded['normalized_text'] ?? null)
            ? $decoded['normalized_text']
            : null,
        'fonts' => is_array($decoded['fonts'] ?? null) ? $decoded['fonts'] : [],
        'normalized_raster_sha256' => is_string($decoded['normalized_raster_sha256'] ?? null)
            ? $decoded['normalized_raster_sha256']
            : null,
        'checks' => $decoded['checks'],
    ];
}

function prepare_engine_directory(string $path, int $uid, int $gid): void
{
    if (!mkdir($path, 0700) || !chown($path, $uid) || !chgrp($path, $gid) || !chmod($path, 0700)) {
        rrmdir($path);
        fail("cannot create engine-owned directory: {$path}");
    }
}

function benchmark_engine_temporary_path(string $prefix): string
{
    if (PHP_OS_FAMILY !== 'Linux') {
        return sys_get_temp_dir() . DIRECTORY_SEPARATOR . $prefix . bin2hex(random_bytes(8));
    }
    $configured = getenv(ENGINE_TEMP_ROOT_ENV);
    $root = is_string($configured) && $configured !== '' ? realpath($configured) : false;
    $metadata = $root !== false ? lstat($root) : false;
    if ($root === false || $root !== $configured || !is_dir($root) || is_link($root)
        || !is_array($metadata) || (int) ($metadata['uid'] ?? -1) !== 0
        || (int) ($metadata['gid'] ?? -1) !== 0 || ((int) $metadata['mode'] & 07777) !== 0711) {
        fail(ENGINE_TEMP_ROOT_ENV . ' must name a canonical root-owned directory with mode 0711');
    }
    return $root . DIRECTORY_SEPARATOR . $prefix . bin2hex(random_bytes(8));
}

function is_browsershot_adapter_path(string $path): bool
{
    $normalized = str_replace('\\', '/', $path);
    return str_ends_with($normalized, '/benchmarks/adapters/browsershot/adapter.php')
        || str_ends_with($normalized, '/benchmarks/adapters/invobook-browsershot/adapter.php');
}

function browser_runtime_path_within_budget(string $path): bool
{
    return strlen($path) <= BROWSER_RUNTIME_TEMP_MAX_BYTES;
}

function benchmark_adapter_temporary_path(string $binary): string
{
    $path = benchmark_engine_temporary_path('r-');
    if (is_browsershot_adapter_path($binary) && !browser_runtime_path_within_budget($path)) {
        fail(ENGINE_TEMP_ROOT_ENV . ' is too long for Chromium runtime sockets');
    }
    return $path;
}

/** @return array{index: int, ok: bool, exit_code: int, wall_ms: float, one_shot_wall_ms: float,
 *     user_ms: float|null, sys_ms: float|null, memory_current_bytes: int|null,
 *     memory_peak_bytes: int|null, read_bytes: int|null, write_bytes: int|null,
 *     phase_timings_ms: array<string, float>|null, output: array<string, mixed>,
 *     correctness: array{pass: bool, checks: list<array{name: string, status: string, detail?: string}>},
 *     failure: array{code: string|null, message: string|null, published_pdf: bool},
 *     retained?: array{artifacts_dir: string, output_dir: string},
 *     summary: array<string, mixed>|null} */
function run_adapter_sample(array $state, int $index): array
{
    assert_fixture_identity($state);
    $artifactsDir = benchmark_engine_temporary_path('pliego-bench-');
    $temporaryDir = PHP_OS_FAMILY === 'Linux'
        // Chromium appends its branded temp directory and SingletonSocket to
        // TMPDIR; keep both Chrome and Chromium below Linux's 108-byte ceiling.
        ? benchmark_adapter_temporary_path($state['binary'])
        : null;
    $outDir = PHP_OS_FAMILY === 'Linux'
        ? benchmark_engine_temporary_path('pliego-bench-out-')
        : sys_get_temp_dir() . '/pliego-bench-out-' . bin2hex(random_bytes(8));
    if (PHP_OS_FAMILY === 'Linux') {
        prepare_engine_directory($outDir, $state['engineUid'], $state['engineGid']);
        prepare_engine_directory($temporaryDir, $state['engineUid'], $state['engineGid']);
        if (!$state['requireSceneReport']) {
            prepare_engine_directory($artifactsDir, $state['engineUid'], $state['engineGid']);
        }
    } elseif (!mkdir($outDir, 0777, true) && !is_dir($outDir)) {
        fail("cannot create output dir: {$outDir}");
    } elseif (!$state['requireSceneReport'] && !mkdir($artifactsDir, 0777, true) && !is_dir($artifactsDir)) {
        fail("cannot create artifacts dir: {$artifactsDir}");
    }
    // The engine requires the requested output to live outside the artifact
    // directory; publish into a sibling temp directory instead.
    $pdfPath = $outDir . DIRECTORY_SEPARATOR . basename((string) $state['output']);

    $command = [
        $state['binary'],
        'render',
        $state['input'],
        '--output',
        $pdfPath,
        '--artifacts',
        $artifactsDir,
    ];
    if ($state['pageSize'] !== null) {
        array_push($command, '--page-size', $state['pageSize']);
    }
    if ($state['pageMargins'] !== null) {
        array_push($command, '--page-margins', $state['pageMargins']);
    }
    if ($state['locale'] !== null) {
        array_push($command, '--locale', $state['locale']);
    }
    if ($state['timezone'] !== null) {
        array_push($command, '--timezone', $state['timezone']);
    }

    try {
        $exec = run_engine($command, $state['cwd'], $state['isolateNetwork'], null, $temporaryDir, $state['rootWallTimeoutMs']);
    } finally {
        if ($temporaryDir !== null) {
            rrmdir($temporaryDir);
        }
    }
    if (isset($exec['error'])) {
        retain_benchmark_attempt($state, $index, ['index' => $index, 'ok' => false, 'error' => $exec['error']], $exec,
            ['artifacts' => $artifactsDir, 'output' => $outDir]);
        rrmdir($artifactsDir);
        rrmdir($outDir);
        fail("engine run failed: {$exec['error']}");
    }
    assert_fixture_identity($state);

    $report = read_json_file($artifactsDir . DIRECTORY_SEPARATOR . 'scene-report.json');
    $summary = parse_adapter_summary($exec['stdout']);
    $phaseTimings = null;
    if (is_array($summary) && isset($summary['phase_timings_ms']) && is_array($summary['phase_timings_ms'])) {
        $phaseTimings = array_map(fn ($value) => (float) $value, $summary['phase_timings_ms']);
    }

    $pdfPublished = is_file($pdfPath) && filesize($pdfPath) > 0;
    $pdfBytes = $pdfPublished ? filesize($pdfPath) : null;
    $pdfSha256 = $pdfPublished ? hash_file('sha256', $pdfPath) : null;

    $captureCode = null;
    $captureStatus = null;
    if (is_array($report)) {
        $captureStatus = is_array($report['capture'] ?? null) ? ($report['capture']['status'] ?? null) : null;
        $captureCode = is_array($report['capture'] ?? null) ? ($report['capture']['code'] ?? null) : null;
    }
    $scenePageCount = null;
    if (is_array($report) && is_array($report['preview'] ?? null)) {
        $scenePageCount = $report['preview']['page_count'] ?? null;
    }

    $failureCode = null;
    $failureMessage = null;
    if ($captureCode !== null) {
        $failureCode = (string) $captureCode;
    } elseif (is_array($report) && is_array($report['document_pdf'] ?? null)
        && is_array($report['document_pdf']['error'] ?? null)) {
        $failureCode = $report['document_pdf']['error']['code'] ?? null;
        $failureMessage = $report['document_pdf']['error']['message'] ?? null;
    } elseif (is_array($summary) && is_array($summary['error'] ?? null)) {
        $failureCode = $summary['error']['code'] ?? null;
        $failureMessage = $summary['error']['message'] ?? null;
    }
    if ($exec['exit_code'] !== 0 && $failureMessage === null) {
        $stderr = trim($exec['stderr']);
        if ($stderr !== '') {
            $failureCode ??= 'engine_stderr';
            $failureMessage = substr($stderr, -4096);
        }
    }

    $artifactBytes = 0;
    if (is_dir($artifactsDir)) {
        $iterator = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($artifactsDir, FilesystemIterator::SKIP_DOTS));
        foreach ($iterator as $entry) {
            if ($entry->isFile()) {
                $artifactBytes += $entry->getSize();
            }
        }
    }

    $checks = [];
    $oracle = null;
    $pageCount = null;
    $pageDimensions = null;
    $normalizedText = null;
    $fonts = [];
    $normalizedRasterSha256 = null;
    if (is_object($exec['resource_usage'])) {
        $drained = $exec['resource_usage']->cgroup_drained ?? false;
        $cleanup = $exec['resource_usage']->cleanup ?? null;
        $killUsed = is_object($cleanup) ? ($cleanup->kill_used ?? true) : true;
        $checks[] = [
            'name' => 'cgroup_drained',
            'status' => $drained ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'cgroup_clean_exit',
            'status' => $killUsed ? 'fail' : 'pass',
            'detail' => 'cgroup.kill=' . ($killUsed ? 'used' : 'not-used'),
        ];
    }
    if ($state['expectFailure']) {
        $failed = $exec['exit_code'] !== 0 && !$pdfPublished;
        $checks[] = [
            'name' => 'render_failed_closed',
            'status' => $failed ? 'pass' : 'fail',
            'detail' => "exit={$exec['exit_code']} published=" . ($pdfPublished ? 'yes' : 'no'),
        ];
        $codeCheck = $state['expectedCode'] === null || $failureCode === $state['expectedCode'];
        $checks[] = [
            'name' => 'failure_code',
            'status' => $codeCheck ? 'pass' : 'fail',
            'detail' => $failureCode ?? '(no code)',
        ];
        $checks[] = [
            'name' => 'pdf_not_published',
            'status' => $pdfPublished ? 'fail' : 'pass',
        ];
    } else {
        $checks[] = [
            'name' => 'exit_code',
            'status' => $exec['exit_code'] === 0 ? 'pass' : 'fail',
            'detail' => "exit={$exec['exit_code']}",
        ];
        if ($state['requireSceneReport']) {
            $checks[] = [
                'name' => 'capture_complete',
                'status' => $captureStatus === 'complete' ? 'pass' : 'fail',
                'detail' => (string) ($captureStatus ?? '(no report)'),
            ];
        }
        $checks[] = [
            'name' => 'pdf_published',
            'status' => $pdfPublished ? 'pass' : 'fail',
        ];
        if ($pdfPublished) {
            $oracle = run_pdf_oracle($state, $pdfPath);
            $pageCount = $oracle['page_count'];
            $pageDimensions = $oracle['page_dimensions_points'];
            $normalizedText = $oracle['normalized_text'];
            $fonts = $oracle['fonts'];
            $normalizedRasterSha256 = $oracle['normalized_raster_sha256'];
            foreach ($oracle['checks'] as $oracleCheck) {
                $checks[] = $oracleCheck;
            }
        } else {
            $checks[] = [
                'name' => 'pdf_oracle',
                'status' => 'fail',
                'detail' => 'PDF was not published',
            ];
        }
        if ($state['requireSceneReport'] && $scenePageCount !== null) {
            $checks[] = [
                'name' => 'scene_pdf_page_count',
                'status' => $pageCount === $scenePageCount ? 'pass' : 'fail',
                'detail' => "scene={$scenePageCount} pdf=" . ($pageCount ?? 'n/a'),
            ];
        }
    }

    $pass = true;
    foreach ($checks as $check) {
        if ($check['status'] === 'fail') {
            $pass = false;
        }
    }

    $sample = [
        'index' => $index,
        'ok' => $pass,
        'exit_code' => $exec['exit_code'],
        'wall_ms' => $exec['wall_ms'],
        'one_shot_wall_ms' => $exec['one_shot_wall_ms'],
        'user_ms' => $exec['user_ms'],
        'sys_ms' => $exec['sys_ms'],
        'memory_current_bytes' => $exec['memory_current_bytes'],
        'memory_peak_bytes' => $exec['memory_peak_bytes'],
        'sampled_peak_rss_kib_lower_bound' => $exec['sampled_peak_rss_kib_lower_bound'],
        'sampled_peak_pss_kib_lower_bound' => $exec['sampled_peak_pss_kib_lower_bound'],
        'read_bytes' => $exec['read_bytes'],
        'write_bytes' => $exec['write_bytes'],
        'read_operations' => $exec['read_operations'],
        'write_operations' => $exec['write_operations'],
        'measurement_method' => $exec['measurement_method'],
        'signal' => $exec['signal'],
        'resource_usage' => $exec['resource_usage'],
        'phase_timings_ms' => $phaseTimings,
        'output' => [
            'pdf_bytes' => $pdfBytes,
            'pdf_sha256' => $pdfSha256,
            'page_count' => $pageCount,
            'page_dimensions_points' => $pageDimensions,
            'normalized_text_sha256' => is_string($normalizedText) ? hash('sha256', $normalizedText) : null,
            'font_families' => array_values(array_unique(array_map(
                static fn (array $font): string => (string) ($font['name'] ?? ''),
                $fonts
            ))),
            'normalized_raster_sha256' => $normalizedRasterSha256,
            'artifact_bytes' => $artifactBytes,
            'published_pdf' => $pdfPublished,
        ],
        'correctness' => ['pass' => $pass, 'checks' => $checks],
        'failure' => [
            'code' => $failureCode,
            'message' => $failureMessage,
            'published_pdf' => $pdfPublished,
        ],
        'summary' => $summary,
    ];

    $sample = retain_benchmark_attempt($state, $index, $sample, $exec,
        ['artifacts' => $artifactsDir, 'output' => $outDir]);
    if ($pass) {
        rrmdir($outDir);
        rrmdir($artifactsDir);
    } elseif ($state['retainRoot'] === null) {
        $sample['retained'] = ['artifacts_dir' => $artifactsDir, 'output_dir' => $outDir];
    }
    return $sample;
}


function run_api2_sample(array $state, int $index): array
{
    assert_fixture_identity($state);
    $job = stage_api2_job($state);
    $requestPath = api2_request_file($job['request']);
    if (PHP_OS_FAMILY === 'Linux') {
        prepare_engine_directory($job['temporary'], $state['engineUid'], $state['engineGid']);
    }
    try {
        $exec = run_engine(
            [$state['binary'], 'render-api2'],
            $job['root'],
            $state['isolateNetwork'],
            $requestPath,
            $job['temporary'],
            $state['rootWallTimeoutMs']
        );
    } finally {
        rrmdir(dirname($requestPath));
        rrmdir($job['temporary']);
    }
    $exec['request'] = $job['request'];
    if (isset($exec['error'])) {
        retain_benchmark_attempt($state, $index, ['index' => $index, 'ok' => false, 'error' => $exec['error']], $exec,
            ['job' => $job['root']]);
        rrmdir($job['sandbox']);
        fail("engine run failed: {$exec['error']}");
    }
    assert_fixture_identity($state);

    $pdfPath = $job['pdf'];
    $report = read_json_file($job['scene']);
    $summary = parse_api2_result($exec['stdout']);
    $phaseTimings = null;

    $pdfPublished = is_file($pdfPath) && filesize($pdfPath) > 0;
    $pdfBytes = $pdfPublished ? filesize($pdfPath) : null;
    $pdfSha256 = $pdfPublished ? hash_file('sha256', $pdfPath) : null;

    $scenePublished = is_file($job['scene']) && filesize($job['scene']) > 0;
    $sceneBytes = $scenePublished ? filesize($job['scene']) : null;
    $sceneSha256 = $scenePublished ? hash_file('sha256', $job['scene']) : null;
    $bundleReport = read_json_file($job['bundle']);
    $bundlePublished = is_file($job['bundle']) && filesize($job['bundle']) > 0;
    $bundleBytes = $bundlePublished ? filesize($job['bundle']) : null;
    $bundleSha256 = $bundlePublished ? hash_file('sha256', $job['bundle']) : null;

    $sceneValid = is_array($report)
        && ($report['schema'] ?? null) === 'pliego.document-scene'
        && ($report['version'] ?? null) === 2
        && is_array($report['pages'] ?? null)
        && array_is_list($report['pages'])
        && $report['pages'] !== [];
    $scenePageCount = $sceneValid ? count($report['pages']) : null;
    $bundleValid = is_array($bundleReport)
        && ($bundleReport['schema'] ?? null) === 'pliego.bundle-manifest'
        && ($bundleReport['version'] ?? null) === 1
        && is_array($bundleReport['entries'] ?? null)
        && array_is_list($bundleReport['entries'])
        && $bundleReport['entries'] !== [];
    $engine = is_array($summary['engine'] ?? null) ? $summary['engine'] : [];
    $engineRuntime = is_array($engine['runtime'] ?? null) ? $engine['runtime'] : [];
    $conformance = is_array($summary['conformance'] ?? null) ? $summary['conformance'] : [];
    $diagnostics = is_array($summary['diagnostics'] ?? null) ? $summary['diagnostics'] : [];
    $engineValid = count($engine) === 5
        && array_key_exists('name', $engine)
        && array_key_exists('version', $engine)
        && array_key_exists('api', $engine)
        && array_key_exists('source_commit', $engine)
        && array_key_exists('runtime', $engine)
        && ($engine['name'] ?? null) === 'pliego'
        && is_string($engine['version'] ?? null)
        && preg_match('/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/D', $engine['version']) === 1
        && ($engine['api'] ?? null) === 2
        && is_string($engine['source_commit'] ?? null)
        && preg_match('/^[0-9a-f]{40}$/D', $engine['source_commit']) === 1;
    $runtimeValid = count($engineRuntime) === 4
        && array_key_exists('mode', $engineRuntime)
        && array_key_exists('target', $engineRuntime)
        && array_key_exists('binary_sha256', $engineRuntime)
        && array_key_exists('servo_base', $engineRuntime)
        && ($engineRuntime['mode'] ?? null) === 'one-shot'
        && is_string($engineRuntime['target'] ?? null)
        && preg_match('/^[a-z0-9]+(?:_[a-z0-9]+)*-[a-z0-9]+(?:_[a-z0-9]+)*-[a-z0-9]+(?:_[a-z0-9]+)*(?:-[a-z0-9]+(?:_[a-z0-9]+)*)?$/D', $engineRuntime['target']) === 1
        && ($engineRuntime['binary_sha256'] ?? null) === 'sha256:' . $state['binarySha256']
        && is_string($engineRuntime['servo_base'] ?? null)
        && preg_match('/^[0-9a-f]{40}$/D', $engineRuntime['servo_base']) === 1;
    $conformanceValid = count($conformance) === 3
        && array_key_exists('requested', $conformance)
        && array_key_exists('status', $conformance)
        && array_key_exists('evidence', $conformance)
        && $conformance['requested'] === null
        && ($conformance['status'] ?? null) === 'not-requested'
        && $conformance['evidence'] === null;
    $diagnosticsValid = count($diagnostics) === 2
        && array_key_exists('retained', $diagnostics)
        && array_key_exists('artifacts', $diagnostics)
        && ($diagnostics['retained'] ?? null) === true
        && is_array($diagnostics['artifacts'])
        && array_is_list($diagnostics['artifacts'])
        && api2_diagnostics_match($diagnostics['artifacts'], $job['root']);
    $typedResult = is_array($summary)
        && array_keys($summary) === [
            'schema', 'version', 'api', 'status', 'request', 'engine', 'delivery',
            'conformance', 'diagnostics', 'error',
        ]
        && ($summary['schema'] ?? null) === 'pliego.render-result'
        && ($summary['version'] ?? null) === 1
        && ($summary['api'] ?? null) === 2
        && in_array($summary['status'] ?? null, ['success', 'failed'], true)
        && $engineValid
        && $runtimeValid
        && $conformanceValid
        && $diagnosticsValid;
    $submittedRequest = json_decode($job['request'], true);
    $requestMatches = $typedResult && is_array($submittedRequest)
        && is_array($summary['request'] ?? null)
        && $summary['request'] === $submittedRequest;

    $failureCode = null;
    $failureMessage = null;
    $failure = read_json_file(
        $job['root'] . DIRECTORY_SEPARATOR . 'diagnostics' . DIRECTORY_SEPARATOR . 'failure.json'
    );
    if (is_array($failure)) {
        $failureCode = is_string($failure['code'] ?? null) ? $failure['code'] : null;
        $failureMessage = is_string($failure['message'] ?? null) ? $failure['message'] : null;
    }
    $failurePath = $job['root'] . DIRECTORY_SEPARATOR . 'diagnostics' . DIRECTORY_SEPARATOR . 'failure.json';
    $failurePublished = is_file($failurePath) && filesize($failurePath) > 0;
    $failureBytes = $failurePublished ? filesize($failurePath) : null;
    $failureSha256 = $failurePublished ? hash_file('sha256', $failurePath) : null;
    $failureDescriptor = null;
    foreach ($diagnostics['artifacts'] ?? [] as $descriptor) {
        if (is_array($descriptor) && ($descriptor['path'] ?? null) === 'diagnostics/failure.json') {
            $failureDescriptor = $descriptor;
            break;
        }
    }
    $failureDiagnosticsValid = is_array($failure)
        && array_keys($failure) === ['code', 'message']
        && is_string($failureCode) && $failureCode !== ''
        && is_string($failureMessage) && $failureMessage !== ''
        && api2_descriptor_matches(
            $failureDescriptor,
            'diagnostics/failure.json',
            'application/json',
            $failureBytes,
            is_string($failureSha256) ? $failureSha256 : null
        );

    $artifactBytes = 0;
    foreach (['delivery', 'diagnostics'] as $artifactDirectory) {
        $path = $job['root'] . DIRECTORY_SEPARATOR . $artifactDirectory;
        if (!is_dir($path)) {
            continue;
        }
        $iterator = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($path, FilesystemIterator::SKIP_DOTS));
        foreach ($iterator as $entry) {
            if ($entry->isFile() && $entry->getPathname() !== $pdfPath) {
                $artifactBytes += $entry->getSize();
            }
        }
    }

    $checks = [];
    $oracle = null;
    $pageCount = null;
    $pageDimensions = null;
    $normalizedText = null;
    $fonts = [];
    $normalizedRasterSha256 = null;
    if (is_object($exec['resource_usage'])) {
        $drained = $exec['resource_usage']->cgroup_drained ?? false;
        $cleanup = $exec['resource_usage']->cleanup ?? null;
        $killUsed = is_object($cleanup) ? ($cleanup->kill_used ?? true) : true;
        $checks[] = [
            'name' => 'cgroup_drained',
            'status' => $drained ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'cgroup_clean_exit',
            'status' => $killUsed ? 'fail' : 'pass',
            'detail' => 'cgroup.kill=' . ($killUsed ? 'used' : 'not-used'),
        ];
    }
    $checks[] = [
        'name' => 'api2_request_echo',
        'status' => $requestMatches ? 'pass' : 'fail',
    ];
    if ($state['expectFailure']) {
        $errorKinds = ['resource', 'readiness', 'settlement', 'capture', 'artifact', 'conformance', 'internal'];
        $error = is_array($summary['error'] ?? null) ? $summary['error'] : [];
        $failed = $exec['exit_code'] === 1
            && $typedResult
            && $requestMatches
            && ($summary['status'] ?? null) === 'failed'
            && array_key_exists('delivery', $summary)
            && $summary['delivery'] === null
            && count($error) === 1
            && array_key_exists('kind', $error)
            && in_array($error['kind'], $errorKinds, true)
            && $failureDiagnosticsValid
            && !$pdfPublished;
        $checks[] = [
            'name' => 'api2_typed_result',
            'status' => $typedResult ? 'pass' : 'fail',
            'detail' => is_array($summary) ? (string) ($summary['status'] ?? '(invalid)') : '(no result)',
        ];
        $checks[] = [
            'name' => 'render_failed_closed',
            'status' => $failed ? 'pass' : 'fail',
            'detail' => "exit={$exec['exit_code']} published=" . ($pdfPublished ? 'yes' : 'no'),
        ];
        $codeCheck = $state['expectedCode'] === null || $failureCode === $state['expectedCode'];
        $checks[] = [
            'name' => 'failure_code',
            'status' => $codeCheck ? 'pass' : 'fail',
            'detail' => $failureCode ?? '(no code)',
        ];
        $checks[] = [
            'name' => 'api2_failure_diagnostics',
            'status' => $failureDiagnosticsValid ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'api2_diagnostics_bound',
            'status' => $diagnosticsValid ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'pdf_not_published',
            'status' => $pdfPublished ? 'fail' : 'pass',
        ];
    } else {
        $delivery = is_array($summary['delivery'] ?? null) ? $summary['delivery'] : [];
        $deliveryValid = count($delivery) === 3
            && array_key_exists('pdf', $delivery)
            && array_key_exists('scene', $delivery)
            && array_key_exists('bundle', $delivery);
        $success = $typedResult && $requestMatches
            && ($summary['status'] ?? null) === 'success'
            && array_key_exists('error', $summary)
            && $summary['error'] === null
            && $deliveryValid;
        $checks[] = [
            'name' => 'exit_code',
            'status' => $exec['exit_code'] === 0 ? 'pass' : 'fail',
            'detail' => "exit={$exec['exit_code']}",
        ];
        $checks[] = [
            'name' => 'api2_typed_result',
            'status' => $success ? 'pass' : 'fail',
            'detail' => is_array($summary) ? (string) ($summary['status'] ?? '(invalid)') : '(no result)',
        ];
        $pdfDescriptorMatches = api2_descriptor_matches(
            $delivery['pdf'] ?? null,
            'document.pdf',
            'application/pdf',
            $pdfBytes,
            is_string($pdfSha256) ? $pdfSha256 : null
        );
        $sceneDescriptorMatches = api2_descriptor_matches(
            $delivery['scene'] ?? null,
            'scene.json',
            'application/vnd.pliego.document-scene+json',
            $sceneBytes,
            is_string($sceneSha256) ? $sceneSha256 : null
        );
        $bundleDescriptorMatches = api2_descriptor_matches(
            $delivery['bundle'] ?? null,
            'bundle.json',
            'application/vnd.pliego.bundle-manifest+json',
            $bundleBytes,
            is_string($bundleSha256) ? $bundleSha256 : null
        );
        $checks[] = [
            'name' => 'api2_scene_delivered',
            'status' => $sceneValid && $scenePublished ? 'pass' : 'fail',
            'detail' => $sceneValid ? "pages={$scenePageCount}" : '(invalid scene)',
        ];
        $checks[] = [
            'name' => 'api2_bundle_delivered',
            'status' => $bundleValid && $bundlePublished ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'api2_pdf_descriptor',
            'status' => $pdfDescriptorMatches ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'api2_scene_descriptor',
            'status' => $sceneDescriptorMatches ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'api2_bundle_descriptor',
            'status' => $bundleDescriptorMatches ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'api2_diagnostics_bound',
            'status' => $diagnosticsValid ? 'pass' : 'fail',
        ];
        $checks[] = [
            'name' => 'pdf_published',
            'status' => $pdfPublished ? 'pass' : 'fail',
        ];
        if ($pdfPublished) {
            $oracle = run_pdf_oracle($state, $pdfPath);
            $pageCount = $oracle['page_count'];
            $pageDimensions = $oracle['page_dimensions_points'];
            $normalizedText = $oracle['normalized_text'];
            $fonts = $oracle['fonts'];
            $normalizedRasterSha256 = $oracle['normalized_raster_sha256'];
            foreach ($oracle['checks'] as $oracleCheck) {
                $checks[] = $oracleCheck;
            }
        } else {
            $checks[] = [
                'name' => 'pdf_oracle',
                'status' => 'fail',
                'detail' => 'PDF was not published',
            ];
        }
        if ($state['requireSceneReport'] && $scenePageCount !== null) {
            $checks[] = [
                'name' => 'scene_pdf_page_count',
                'status' => $pageCount === $scenePageCount ? 'pass' : 'fail',
                'detail' => "scene={$scenePageCount} pdf=" . ($pageCount ?? 'n/a'),
            ];
        }
    }

    $pass = true;
    foreach ($checks as $check) {
        if ($check['status'] === 'fail') {
            $pass = false;
        }
    }

    $sample = [
        'index' => $index,
        'ok' => $pass,
        'exit_code' => $exec['exit_code'],
        'wall_ms' => $exec['wall_ms'],
        'one_shot_wall_ms' => $exec['one_shot_wall_ms'],
        'user_ms' => $exec['user_ms'],
        'sys_ms' => $exec['sys_ms'],
        'memory_current_bytes' => $exec['memory_current_bytes'],
        'memory_peak_bytes' => $exec['memory_peak_bytes'],
        'sampled_peak_rss_kib_lower_bound' => $exec['sampled_peak_rss_kib_lower_bound'],
        'sampled_peak_pss_kib_lower_bound' => $exec['sampled_peak_pss_kib_lower_bound'],
        'read_bytes' => $exec['read_bytes'],
        'write_bytes' => $exec['write_bytes'],
        'read_operations' => $exec['read_operations'],
        'write_operations' => $exec['write_operations'],
        'measurement_method' => $exec['measurement_method'],
        'signal' => $exec['signal'],
        'resource_usage' => $exec['resource_usage'],
        'phase_timings_ms' => $phaseTimings,
        'output' => [
            'pdf_bytes' => $pdfBytes,
            'pdf_sha256' => $pdfSha256,
            'page_count' => $pageCount,
            'page_dimensions_points' => $pageDimensions,
            'normalized_text_sha256' => is_string($normalizedText) ? hash('sha256', $normalizedText) : null,
            'font_families' => array_values(array_unique(array_map(
                static fn (array $font): string => (string) ($font['name'] ?? ''),
                $fonts
            ))),
            'normalized_raster_sha256' => $normalizedRasterSha256,
            'artifact_bytes' => $artifactBytes,
            'published_pdf' => $pdfPublished,
        ],
        'correctness' => ['pass' => $pass, 'checks' => $checks],
        'failure' => [
            'code' => $failureCode,
            'message' => $failureMessage,
            'published_pdf' => $pdfPublished,
        ],
        'summary' => $summary,
    ];

    $sample = retain_benchmark_attempt($state, $index, $sample, $exec, ['job' => $job['root']]);
    if ($pass) {
        rrmdir($job['sandbox']);
    } elseif ($state['retainRoot'] === null) {
        $sample['retained'] = [
            'artifacts_dir' => $job['root'],
            'output_dir' => $job['root'] . DIRECTORY_SEPARATOR . 'delivery',
        ];
    }
    return $sample;
}

function run_sample(array $state, int $index): array
{
    return $state['nativeApi2']
        ? run_api2_sample($state, $index)
        : run_adapter_sample($state, $index);
}

function failed_correctness_checks(array $sample): string
{
    $failures = [];
    foreach ($sample['correctness']['checks'] ?? [] as $check) {
        if (!is_array($check) || ($check['status'] ?? null) !== 'fail') {
            continue;
        }
        $failure = ['name' => (string) ($check['name'] ?? '(unnamed)')];
        if (isset($check['detail']) && is_scalar($check['detail'])) {
            $failure['detail'] = (string) $check['detail'];
        }
        $failures[] = $failure;
    }
    $engineFailure = $sample['failure'] ?? null;
    if (is_array($engineFailure)
        && (($engineFailure['code'] ?? null) !== null || ($engineFailure['message'] ?? null) !== null)) {
        $failures[] = [
            'name' => 'engine_failure',
            'detail' => json_encode([
                'code' => $engineFailure['code'] ?? null,
                'message' => $engineFailure['message'] ?? null,
            ], JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE),
        ];
    }
    $encoded = json_encode($failures, JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE);
    return is_string($encoded) ? $encoded : '[{"name":"(encoding-failed)"}]';
}

if ($retainRoot !== null) {
    try {
        $retainRoot = prepare_retention_root($retainRoot, $cwd);
    } catch (RuntimeException $error) {
        fail($error->getMessage());
    }
}

$state = [
    'binary' => $binary,
    'binarySha256' => $binarySha256,
    'input' => $input,
    'output' => $output,
    'pageSize' => $pageSize,
    'pageMargins' => $pageMargins,
    'locale' => $locale,
    'timezone' => $timezone,
    'cwd' => $cwd,
    'expectFailure' => $expectFailure,
    'expectedCode' => $expectedCode,
    'pageCount' => $pageCount,
    'textContains' => $textContains,
    'textEquals' => $textEquals,
    'fontFamilies' => $fontFamilies,
    'rasterSha256' => $rasterSha256,
    'linkTargets' => $linkTargets,
    'pageWidthPoints' => $pageWidthPoints,
    'pageHeightPoints' => $pageHeightPoints,
    'dimensionTolerancePoints' => $dimensionTolerancePoints,
    'requireSceneReport' => $requireSceneReport,
    'engineUid' => $engineUid,
    'engineGid' => $engineGid,
    'fixtureInputSha256' => $fixtureInputSha256,
    'fixtureBundleSha256' => $fixtureBundleSha256,
    'fixtureAssets' => $fixtureAssets,
    'nativeApi2' => $nativeApi2,
    'isolateNetwork' => $isolateNetwork,
    'retainRoot' => $retainRoot,
    'rootWallTimeoutMs' => $rootWallTimeoutMs,
];

assert_fixture_identity($state);
$runPreflight = static function () use ($state): void {
    $preflight = run_sample($state, -1000000);
    if (!$preflight['ok']) {
        fail(
            'untimed correctness preflight failed: ' . failed_correctness_checks($preflight)
            . '; evidence retained at ' . $preflight['retained']['artifacts_dir']
        );
    }
};

if ($runnerPhase === 'preflight') {
    $runPreflight();
    exit(0);
}
if ($runnerPhase === 'warmup') {
    $sample = run_sample($state, -1 - (int) $sampleIndex);
    if (!$sample['ok']) {
        fail(
            'warmup failed: ' . failed_correctness_checks($sample)
            . '; evidence retained at ' . $sample['retained']['artifacts_dir']
        );
    }
    exit(0);
}
if ($runnerPhase === 'timed') {
    $sample = run_sample($state, (int) $sampleIndex);
    echo json_encode($sample, JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE) . "\n";
    exit(0);
}

$runPreflight();

for ($iteration = 0; $iteration < $warmup; $iteration++) {
    $sample = run_sample($state, -1 - $iteration);
    if (!$sample['ok']) {
        fail(
            'warmup failed: ' . failed_correctness_checks($sample)
            . '; evidence retained at ' . $sample['retained']['artifacts_dir']
        );
    }
}
for ($iteration = 0; $iteration < $samples; $iteration++) {
    $sample = run_sample($state, $iteration);
    echo json_encode($sample, JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE) . "\n";
}
