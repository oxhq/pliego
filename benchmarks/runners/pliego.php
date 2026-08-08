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
 * Timing: this foundation records process wall time only. The separate audited
 * Linux sampler owns summed process-tree RSS/PSS, CPU, I/O, and lifecycle data.
 *
 * Publishable host contract: Linux x86_64, released `checked-release` bundle.
 */

declare(strict_types=1);

const USAGE = <<<EOT
Usage: php pliego.php --binary <path> --input <file.html> --output <file.pdf> --artifacts <dir>
  [--samples N] [--warmup N] [--page-count N] [--text-contains a,b,c]
  [--expect-failure] [--expected-code CODE] [--page-size WxH] [--page-margins T,R,B,L]
  [--locale X] [--timezone Y] [--cwd DIR]
EOT;

function option(array $options, string $name): ?string
{
    return isset($options[$name]) && is_scalar($options[$name])
        ? (string) $options[$name]
        : null;
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
]);
if ($options === false) {
    fwrite(STDERR, USAGE . "\n");
    exit(2);
}

$binary = option($options, 'binary') ?? fail('--binary is required');
$input = option($options, 'input') ?? fail('--input is required');
$output = option($options, 'output') ?? 'document.pdf';
$artifacts = option($options, 'artifacts') ?? 'artifacts';
$samples = max(1, (int) (option($options, 'samples') ?? 1));
$warmup = max(0, (int) (option($options, 'warmup') ?? 0));
$pageCount = option($options, 'page-count') !== null ? (int) $options['page-count'] : null;
$textContains = option($options, 'text-contains') !== null
    ? array_values(array_filter(array_map('trim', explode(',', $options['text-contains']))))
    : [];
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
 *     sys_ms: float|null, peak_rss_kib: int|null, exit_code: int,
 *     stdout: string, stderr: string}
 */
function run_engine(array $command, string $cwd): array
{
    $nullDevice = PHP_OS_FAMILY === 'Windows' ? 'NUL' : '/dev/null';
    $stdoutTmp = tempnam(sys_get_temp_dir(), 'pliego-bench-out-');
    $stderrTmp = tempnam(sys_get_temp_dir(), 'pliego-bench-err-');
    $descriptors = [
        0 => ['file', $nullDevice, 'r'],
        1 => ['file', $stdoutTmp, 'w'],
        2 => ['file', $stderrTmp, 'w'],
    ];

    $wallStart = microtime(true);
    $process = proc_open($command, $descriptors, $pipes, $cwd);
    if (!is_resource($process)) {
        @unlink($stdoutTmp);
        @unlink($stderrTmp);
        return ['error' => 'proc_open failed for engine command'];
    }

    $exitCode = proc_close($process);
    $wallMs = (microtime(true) - $wallStart) * 1000.0;

    $stdout = (string) file_get_contents($stdoutTmp);
    $stderr = (string) file_get_contents($stderrTmp);
    @unlink($stdoutTmp);
    @unlink($stderrTmp);

    return [
        'wall_ms' => round($wallMs, 3),
        'user_ms' => null,
        'sys_ms' => null,
        'peak_rss_kib' => null,
        'rss_method' => 'unavailable',
        'exit_code' => $exitCode,
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
        $value = json_decode($line, true);
        if (is_array($value)) {
            return $value;
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
        $captureStatus = is_array($report['capture'] ?? null) ? $report['capture']['status'] : null;
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
        'rss_method' => $exec['rss_method'],
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
