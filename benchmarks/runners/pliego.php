#!/usr/bin/env php
<?php

/**
 * Pliego benchmark runner — one engine process per sample.
 *
 * Executes `pliego render` once per sample against a published binary, records
 * wall time, reads the engine's `scene-report.json` and stdout summary, checks
 * the fixture's correctness contract, and emits one JSON object per sample
 * (NDJSON) on stdout. CPU and process-tree memory remain unavailable until the
 * audited Linux sampler is integrated. Warmup samples are executed and
 * discarded before real samples. Aggregation and schema validation happen in
 * tools/run_benchmark.py.
 *
 * Invocation contract: the engine resolves the input relative to the process
 * cwd and rejects absolute or parent-traversing paths, so the runner is given
 * the bare input file name plus `--cwd <input directory>` and validates the
 * file against that cwd. The requested output is placed in a sibling temp
 * directory, never inside the artifact directory.
 *
 * Timing: Linux delegates the engine launch to process_tree_sampler.py, which
 * creates one new session and polls every inherited member through procfs.
 * Non-Linux runs retain wall/exit data and mark process-tree counters null.
 *
 * Publishable host contract: Linux x86_64, released `checked-release` bundle.
 */

declare(strict_types=1);

const USAGE = <<<EOT
Usage: php pliego.php --binary <path> --input <file.html> --output <file.pdf> --artifacts <dir>
  [--samples N] [--warmup N] [--page-count N] [--text-contains TEXT]...
  [--expect-failure] [--expected-code CODE] [--page-size WxH] [--page-margins T,R,B,L]
  [--locale X] [--timezone Y] [--cwd DIR] [--self-test]
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

function fail(string $message, int $code = 2): never
{
    fwrite(STDERR, "pliego.php: {$message}\n");
    exit($code);
}

$options = getopt('', [
    'binary:', 'input:', 'output:', 'artifacts:', 'samples:', 'warmup:',
    'page-count:', 'text-contains:', 'expect-failure', 'expected-code:',
    'page-size:', 'page-margins:', 'locale:', 'timezone:', 'cwd:',
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
    $summary = parse_stdout_summary(
        "{\"phase_timings_ms\":{\"layout\":1},\"error\":{\"code\":\"TEST\"}}\n[]\n"
    );
    if (($summary['phase_timings_ms']['layout'] ?? null) !== 1
        || ($summary['error']['code'] ?? null) !== 'TEST'
        || parse_stdout_summary("[]\n") !== null
        || parse_stdout_summary("{}\n") !== null) {
        fail('stdout summary self-test failed', 1);
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
$pageCount = option($options, 'page-count') !== null ? (int) $options['page-count'] : null;
$textContains = text_contains_options($options['text-contains'] ?? []);
$expectFailure = array_key_exists('expect-failure', $options);
$expectedCode = option($options, 'expected-code');
$pageSize = option($options, 'page-size');
$pageMargins = option($options, 'page-margins');
$locale = option($options, 'locale');
$timezone = option($options, 'timezone');
$cwd = option($options, 'cwd') ?? dirname($input);

if (!is_file($binary)) {
    fail("binary not found: {$binary}");
}
if (!is_dir($cwd)) {
    fail("cwd not found: {$cwd}");
}
// The engine resolves the input relative to the process cwd and rejects
// absolute or parent-traversing paths (mirroring the PHP SDK). Validate
// against the run cwd here, but keep the bare relative name for the engine
// command so it resolves inside `cwd`.
$inputFull = (str_starts_with($input, '/')
        || str_starts_with($input, '\\')
        || preg_match('/^[A-Za-z]:/', $input) === 1)
    ? $input
    : rtrim($cwd, '/\\') . DIRECTORY_SEPARATOR . $input;
if (!is_file($inputFull)) {
    fail("input not found: {$inputFull}");
}

/**
 * @param list<string> $command
 * @return array{error: string}|array{wall_ms: float, user_ms: float|null,
 *     sys_ms: float|null, peak_rss_kib: int|null, peak_pss_kib: int|null,
 *     read_bytes: int|null, write_bytes: int|null, rss_method: string,
 *     signal: int|null, process_tree: array<string, mixed>|null,
 *     exit_code: int, stdout: string, stderr: string}
 */
function run_engine(array $command, string $cwd): array
{
    $nullDevice = PHP_OS_FAMILY === 'Windows' ? 'NUL' : '/dev/null';
    $stdoutTmp = tempnam(sys_get_temp_dir(), 'pliego-bench-out-');
    $stderrTmp = tempnam(sys_get_temp_dir(), 'pliego-bench-err-');
    if ($stdoutTmp === false || $stderrTmp === false) {
        return ['error' => 'cannot create engine output files'];
    }
    $linux = PHP_OS_FAMILY === 'Linux';
    $samplerResultTmp = $linux ? tempnam(sys_get_temp_dir(), 'pliego-bench-proc-') : null;
    $samplerErrorTmp = $linux ? tempnam(sys_get_temp_dir(), 'pliego-bench-proc-err-') : null;
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
    if ($linux) {
        $sampler = dirname(__DIR__) . '/tools/process_tree_sampler.py';
        if (!is_file($sampler)) {
            @unlink($stdoutTmp);
            @unlink($stderrTmp);
            @unlink($samplerResultTmp);
            @unlink($samplerErrorTmp);
            return ['error' => "process-tree sampler not found: {$sampler}"];
        }
        $launchedCommand = [
            'python3', $sampler,
            '--cwd', $cwd,
            '--stdout', $stdoutTmp,
            '--stderr', $stderrTmp,
            '--',
            ...$command,
        ];
    }
    $descriptors = [
        0 => ['file', $nullDevice, 'r'],
        1 => ['file', $linux ? $samplerResultTmp : $stdoutTmp, 'w'],
        2 => ['file', $linux ? $samplerErrorTmp : $stderrTmp, 'w'],
    ];

    $wallStart = microtime(true);
    $process = proc_open($launchedCommand, $descriptors, $pipes, $cwd);
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
        $measurement = json_decode((string) file_get_contents($samplerResultTmp), true);
        $samplerError = trim((string) file_get_contents($samplerErrorTmp));
        @unlink($samplerResultTmp);
        @unlink($samplerErrorTmp);
        if ($launcherExitCode !== 0 || !is_array($measurement)) {
            return ['error' => 'process-tree sampler failed: ' . ($samplerError ?: "exit {$launcherExitCode}")];
        }
        foreach (['wall_ms', 'cpu_user_ms', 'cpu_sys_ms', 'peak_summed_rss_kib', 'method', 'exit_code'] as $field) {
            if (!array_key_exists($field, $measurement)) {
                return ['error' => "process-tree sampler omitted {$field}"];
            }
        }
        return [
            'wall_ms' => (float) $measurement['wall_ms'],
            'user_ms' => (float) $measurement['cpu_user_ms'],
            'sys_ms' => (float) $measurement['cpu_sys_ms'],
            'peak_rss_kib' => (int) $measurement['peak_summed_rss_kib'],
            'peak_pss_kib' => isset($measurement['peak_summed_pss_kib'])
                ? (int) $measurement['peak_summed_pss_kib']
                : null,
            'read_bytes' => isset($measurement['read_bytes']) ? (int) $measurement['read_bytes'] : null,
            'write_bytes' => isset($measurement['write_bytes']) ? (int) $measurement['write_bytes'] : null,
            'rss_method' => (string) $measurement['method'],
            'signal' => isset($measurement['signal']) ? (int) $measurement['signal'] : null,
            'process_tree' => $measurement,
            'exit_code' => (int) $measurement['exit_code'],
            'stdout' => $stdout,
            'stderr' => $stderr,
        ];
    }

    return [
        'wall_ms' => round($wallMs, 3),
        'user_ms' => null,
        'sys_ms' => null,
        'peak_rss_kib' => null,
        'peak_pss_kib' => null,
        'read_bytes' => null,
        'write_bytes' => null,
        'rss_method' => 'unavailable',
        'signal' => null,
        'process_tree' => null,
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
function parse_stdout_summary(string $stdout): ?array
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

function pdftotext_available(): bool
{
    $lines = [];
    $code = 0;
    exec(PHP_OS_FAMILY === 'Windows' ? 'where pdftotext 2>NUL' : 'command -v pdftotext 2>/dev/null', $lines, $code);
    return $code === 0;
}

function pdf_text(string $pdfPath): ?string
{
    $tmp = tempnam(sys_get_temp_dir(), 'pliego-bench-txt-');
    $lines = [];
    $code = 0;
    exec('pdftotext ' . escapeshellarg($pdfPath) . ' ' . escapeshellarg($tmp), $lines, $code);
    $text = null;
    if ($code === 0 && is_file($tmp)) {
        $text = (string) file_get_contents($tmp);
    }
    @unlink($tmp);
    return $text;
}

/** @return array{index: int, ok: bool, exit_code: int, wall_ms: float,
 *     user_ms: float|null, sys_ms: float|null, peak_rss_kib: int|null,
 *     peak_pss_kib: int|null, read_bytes: int|null, write_bytes: int|null,
 *     phase_timings_ms: array<string, float>|null, output: array<string, mixed>,
 *     correctness: array{pass: bool, checks: list<array{name: string, status: string, detail?: string}>},
 *     failure: array{code: string|null, message: string|null, published_pdf: bool},
 *     retained?: array{artifacts_dir: string, output_dir: string},
 *     summary: array<string, mixed>|null} */
function run_sample(array $state, int $index): array
{
    $artifactsDir = sys_get_temp_dir() . '/pliego-bench-' . bin2hex(random_bytes(8));
    $outDir = sys_get_temp_dir() . '/pliego-bench-out-' . bin2hex(random_bytes(8));
    if (!mkdir($outDir, 0777, true) && !is_dir($outDir)) {
        fail("cannot create output dir: {$outDir}");
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

    $exec = run_engine($command, $state['cwd']);
    if (isset($exec['error'])) {
        rrmdir($artifactsDir);
        fail("engine run failed: {$exec['error']}");
    }

    $report = read_json_file($artifactsDir . DIRECTORY_SEPARATOR . 'scene-report.json');
    $summary = parse_stdout_summary($exec['stdout']);
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
    $pageCount = null;
    if (is_array($report) && is_array($report['preview'] ?? null)) {
        $pageCount = $report['preview']['page_count'] ?? null;
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
    if (is_array($exec['process_tree'])) {
        $treeComplete = $exec['process_tree']['tree_complete'] ?? false;
        $observedPids = $exec['process_tree']['observed_pids'] ?? [];
        $treeMeasured = $treeComplete && is_array($observedPids) && $observedPids !== [];
        $checks[] = [
            'name' => 'process_tree_complete',
            'status' => $treeMeasured ? 'pass' : 'fail',
            'detail' => 'observed_pids=' . implode(',', is_array($observedPids) ? $observedPids : []),
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
        $checks[] = [
            'name' => 'capture_complete',
            'status' => $captureStatus === 'complete' ? 'pass' : 'fail',
            'detail' => (string) ($captureStatus ?? '(no report)'),
        ];
        $checks[] = [
            'name' => 'pdf_published',
            'status' => $pdfPublished ? 'pass' : 'fail',
        ];
        if ($state['pageCount'] !== null) {
            $checks[] = [
                'name' => 'page_count',
                'status' => $pageCount === $state['pageCount'] ? 'pass' : 'fail',
                'detail' => "expected={$state['pageCount']} actual=" . ($pageCount ?? 'n/a'),
            ];
        }
        if ($state['textContains'] !== [] && $pdfPublished) {
            if (pdftotext_available()) {
                $text = pdf_text($pdfPath);
                if ($text === null) {
                    $checks[] = ['name' => 'text', 'status' => 'fail', 'detail' => 'pdftotext produced no output'];
                } else {
                    $normalizedText = preg_replace('/\s+/u', ' ', trim($text)) ?? $text;
                    foreach ($state['textContains'] as $fragment) {
                        $normalizedFragment = preg_replace('/\s+/u', ' ', trim($fragment)) ?? $fragment;
                        $checks[] = [
                            'name' => "text:{$fragment}",
                            'status' => str_contains($normalizedText, $normalizedFragment) ? 'pass' : 'fail',
                        ];
                    }
                }
            } else {
                $checks[] = ['name' => 'text', 'status' => 'fail', 'detail' => 'pdftotext unavailable'];
            }
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
        'user_ms' => $exec['user_ms'],
        'sys_ms' => $exec['sys_ms'],
        'peak_rss_kib' => $exec['peak_rss_kib'],
        'peak_pss_kib' => $exec['peak_pss_kib'],
        'read_bytes' => $exec['read_bytes'],
        'write_bytes' => $exec['write_bytes'],
        'rss_method' => $exec['rss_method'],
        'signal' => $exec['signal'],
        'process_tree' => $exec['process_tree'],
        'phase_timings_ms' => $phaseTimings,
        'output' => [
            'pdf_bytes' => $pdfBytes,
            'pdf_sha256' => $pdfSha256,
            'page_count' => $pageCount,
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

$state = [
    'binary' => $binary,
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
];

for ($iteration = 0; $iteration < $warmup; $iteration++) {
    $sample = run_sample($state, -1 - $iteration);
    if (!$sample['ok']) {
        fail('warmup failed; evidence retained at ' . $sample['retained']['artifacts_dir']);
    }
}
for ($iteration = 0; $iteration < $samples; $iteration++) {
    $sample = run_sample($state, $iteration);
    echo json_encode($sample, JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE) . "\n";
}
