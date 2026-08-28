#!/usr/bin/env php
<?php

/**
 * Target-neutral benchmark runner — one adapter process per sample.
 *
 * Executes one target process per sample and runs the shared PDF oracle after
 * timing. Pliego uses native `render-api2`; competitor adapters retain the
 * benchmark `render INPUT ...` contract. On Linux, cgroup-v2 supplies
 * authoritative CPU, memory, and I/O accounting for the adapter and every
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
 *
 * Publishable host contract: Linux x86_64, released `checked-release` bundle.
 */

declare(strict_types=1);

const ENGINE_ACCOUNT = 'pliego-benchmark-engine';
const SAMPLER_PYTHON = '/usr/bin/python3';

const USAGE = <<<EOT
Usage: php pliego.php --binary <path> --input <file.html> --output <file.pdf> --artifacts <dir>
  [--samples N] [--warmup N] [--page-count N] [--text-contains TEXT]...
  [--text-equals TEXT] [--font-family NAME]... [--raster-sha256 HASH]
  [--link-target URL]... [--page-width-points N] [--page-height-points N]
  [--dimension-tolerance-points N] [--require-scene-report]
  --fixture-input-sha256 HASH --fixture-bundle-sha256 HASH [--fixture-asset PATH]...
  [--native-api2] [--isolate-network]
  [--expect-failure] [--expected-code CODE] [--page-size WxH] [--page-margins T,R,B,L]
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

$options = getopt('', [
    'binary:', 'input:', 'output:', 'artifacts:', 'samples:', 'warmup:',
    'page-count:', 'text-contains:', 'text-equals:', 'font-family:', 'raster-sha256:',
    'expect-failure', 'expected-code:',
    'link-target:', 'page-width-points:', 'page-height-points:',
    'dimension-tolerance-points:', 'require-scene-report',
    'page-size:', 'page-margins:', 'locale:', 'timezone:', 'cwd:',
    'fixture-input-sha256:', 'fixture-bundle-sha256:', 'fixture-asset:',
    'native-api2', 'isolate-network', 'runner-phase:', 'sample-index:',
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
        || is_windows_absolute_path('/tmp')) {
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
function run_engine(array $command, string $cwd, bool $isolateNetwork, ?string $stdinPath): array
{
    $nullDevice = PHP_OS_FAMILY === 'Windows' ? 'NUL' : '/dev/null';
    $stdoutTmp = tempnam(sys_get_temp_dir(), 'pliego-bench-out-');
    $stderrTmp = tempnam(sys_get_temp_dir(), 'pliego-bench-err-');
    if ($stdoutTmp === false || $stderrTmp === false) {
        return ['error' => 'cannot create engine output files'];
    }
    $linux = PHP_OS_FAMILY === 'Linux';
    $samplerResultTmp = $linux ? tempnam(sys_get_temp_dir(), 'pliego-bench-cgroup-') : null;
    $samplerErrorTmp = $linux ? tempnam(sys_get_temp_dir(), 'pliego-bench-cgroup-err-') : null;
    if ($linux && ($samplerResultTmp === false || $samplerErrorTmp === false)) {
        @unlink($stdoutTmp);
        @unlink($stderrTmp);
        if (is_string($samplerResultTmp)) {
            @unlink($samplerResultTmp);
        }
        if (is_string($samplerErrorTmp)) {
            @unlink($samplerErrorTmp);
        }
        return ['error' => 'cannot create sampler output files'];
    }

    $launchedCommand = $command;
    $processEnvironment = null;
    if ($linux) {
        $sampler = dirname(__DIR__) . '/tools/process_tree_sampler.py';
        if (!is_file($sampler)) {
            @unlink($stdoutTmp);
            @unlink($stderrTmp);
            @unlink($samplerResultTmp);
            @unlink($samplerErrorTmp);
            return ['error' => "cgroup-v2 sampler not found: {$sampler}"];
        }
        $interpreter = sampler_interpreter();
        if ($interpreter === null) {
            @unlink($stdoutTmp);
            @unlink($stderrTmp);
            @unlink($samplerResultTmp);
            @unlink($samplerErrorTmp);
            return ['error' => 'sampler interpreter is not a canonical root-owned, non-writable executable: ' . SAMPLER_PYTHON];
        }
        $launchedCommand = [
            $interpreter, '-I', $sampler,
            '--cwd', $cwd,
            '--stdout', $stdoutTmp,
            '--stderr', $stderrTmp,
            ...($stdinPath !== null ? ['--stdin', $stdinPath] : []),
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
        @unlink($stdoutTmp);
        @unlink($stderrTmp);
        if (is_string($samplerResultTmp)) {
            @unlink($samplerResultTmp);
        }
        if (is_string($samplerErrorTmp)) {
            @unlink($samplerErrorTmp);
        }
        return ['error' => 'proc_open failed for engine command'];
    }

    $launcherExitCode = proc_close($process);
    $wallMs = (microtime(true) - $wallStart) * 1000.0;
    $stdout = (string) file_get_contents($stdoutTmp);
    $stderr = (string) file_get_contents($stderrTmp);
    @unlink($stdoutTmp);
    @unlink($stderrTmp);

    if ($linux) {
        $measurementJson = (string) file_get_contents($samplerResultTmp);
        $measurement = json_decode($measurementJson, true);
        $resourceUsage = json_decode($measurementJson);
        $samplerError = trim((string) file_get_contents($samplerErrorTmp));
        @unlink($samplerResultTmp);
        @unlink($samplerErrorTmp);
        if ($launcherExitCode !== 0 || !is_array($measurement) || !is_object($resourceUsage)) {
            return ['error' => 'cgroup-v2 sampler failed: ' . ($samplerError ?: "exit {$launcherExitCode}")];
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
        $diagnostics = $measurement['sampled_diagnostics'];
        if (!is_array($diagnostics)) {
            return ['error' => 'cgroup-v2 sampler returned invalid sampled_diagnostics'];
        }
        return [
            'wall_ms' => (float) $measurement['wall_ms'],
            // Unlike engine wall time, this includes sampler launch, descendant
            // drain, retained-counter settlement, and sampler exit. Serial
            // throughput must use this complete one-shot boundary.
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
    if (preg_match('/^([^x]+)x([^x]+)$/D', $value, $match) !== 1) {
        fail('--page-size must be A4 or WIDTHxHEIGHT in CSS pixels');
    }
    return [
        'width_app_units' => css_number_to_app_units($match[1], 'page width'),
        'height_app_units' => css_number_to_app_units($match[2], 'page height'),
    ];
}

/** @return array{top: int, right: int, bottom: int, left: int} */
function api2_page_margins(?string $value): array
{
    $value ??= '48,48,48,48';
    $parts = explode(',', $value);
    if (count($parts) !== 4) {
        fail('--page-margins must contain TOP,RIGHT,BOTTOM,LEFT CSS-pixel values');
    }
    return [
        'top' => css_number_to_app_units($parts[0], 'top margin', true),
        'right' => css_number_to_app_units($parts[1], 'right margin', true),
        'bottom' => css_number_to_app_units($parts[2], 'bottom margin', true),
        'left' => css_number_to_app_units($parts[3], 'left margin', true),
    ];
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

/** @return array{root: string, request: string, pdf: string, scene: string, bundle: string} */
function stage_api2_job(array $state): array
{
    $jobRoot = sys_get_temp_dir() . '/pliego-bench-api2-' . bin2hex(random_bytes(8));
    if (PHP_OS_FAMILY === 'Linux') {
        prepare_engine_directory($jobRoot, $state['engineUid'], $state['engineGid']);
    } elseif (!mkdir($jobRoot, 0700) || !chmod($jobRoot, 0700)) {
        fail("cannot create private API 2 job root: {$jobRoot}");
    }
    if (PHP_OS_FAMILY === 'Windows') {
        harden_windows_job_root($jobRoot);
    }

    $inputRoot = $jobRoot . DIRECTORY_SEPARATOR . 'input';
    if (!mkdir($inputRoot, 0700)) {
        fail("cannot create API 2 input directory: {$inputRoot}");
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

    if (PHP_OS_FAMILY !== 'Windows') {
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($jobRoot, FilesystemIterator::SKIP_DOTS),
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
        'root' => $jobRoot,
        'request' => $request,
        'pdf' => $jobRoot . DIRECTORY_SEPARATOR . 'delivery' . DIRECTORY_SEPARATOR . 'document.pdf',
        'scene' => $jobRoot . DIRECTORY_SEPARATOR . 'delivery' . DIRECTORY_SEPARATOR . 'scene.json',
        'bundle' => $jobRoot . DIRECTORY_SEPARATOR . 'delivery' . DIRECTORY_SEPARATOR . 'bundle.json',
    ];
}

function api2_request_file(string $request): string
{
    $path = tempnam(sys_get_temp_dir(), 'pliego-bench-api2-request-');
    if ($path === false || file_put_contents($path, $request, LOCK_EX) !== strlen($request)
        || (PHP_OS_FAMILY === 'Linux' && !chmod($path, 0400))) {
        if (is_string($path)) {
            @unlink($path);
        }
        fail('cannot create immutable API 2 stdin request');
    }
    $resolved = realpath($path);
    if ($resolved === false || $resolved !== $path) {
        @unlink($path);
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
    $artifactsDir = sys_get_temp_dir() . '/pliego-bench-' . bin2hex(random_bytes(8));
    $outDir = sys_get_temp_dir() . '/pliego-bench-out-' . bin2hex(random_bytes(8));
    if (PHP_OS_FAMILY === 'Linux') {
        prepare_engine_directory($outDir, $state['engineUid'], $state['engineGid']);
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

    $exec = run_engine($command, $state['cwd'], $state['isolateNetwork'], null);
    if (isset($exec['error'])) {
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

    if ($pass) {
        rrmdir($outDir);
        rrmdir($artifactsDir);
    } else {
        $sample['retained'] = ['artifacts_dir' => $artifactsDir, 'output_dir' => $outDir];
    }
    return $sample;
}


function run_api2_sample(array $state, int $index): array
{
    assert_fixture_identity($state);
    $job = stage_api2_job($state);
    $requestPath = api2_request_file($job['request']);
    try {
        $exec = run_engine(
            [$state['binary'], 'render-api2'],
            $job['root'],
            $state['isolateNetwork'],
            $requestPath
        );
    } finally {
        @unlink($requestPath);
    }
    if (isset($exec['error'])) {
        rrmdir($job['root']);
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

    if ($pass) {
        rrmdir($job['root']);
    } else {
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
