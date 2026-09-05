<?php

declare(strict_types=1);

use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\Exception\TransportException;
use Pliego\Php\InputAsset;
use Pliego\Php\JobRetention;
use Pliego\Php\RenderOptions;
use Pliego\Php\RenderResult;

require dirname(__DIR__).'/vendor/autoload.php';

// Usage: php production_deadline_test.php /path/to/native/pliego /fresh/proof-directory [1|2]
// The optional caller timeout selects engine budgets 999ms or 1500ms, respectively.
// Run under an independent, bounded process watchdog. This script deliberately
// exercises a synchronous infinite JS turn, not a fake binary or a sleep wrapper.
// The local publication callback below is not Laravel/remote-storage evidence.

function deadlineExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function deadlineWrite(string $path, string $bytes): void
{
    deadlineExpect(file_put_contents($path, $bytes) === strlen($bytes), "cannot retain {$path}");
}

function deadlineJson(string $path, array $document): void
{
    deadlineWrite($path, json_encode($document, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n");
}

function deadlineStatus(string $jobPath): ?string
{
    $path = $jobPath.DIRECTORY_SEPARATOR.JobRetention::STATUS_FILE;

    return is_file($path) ? trim((string) file_get_contents($path)) : null;
}

/** The same success-only publication boundary is exercised by all three calls. */
function deadlineRenderAndPublish(
    DocumentEngine $engine,
    string $html,
    RenderOptions $options,
    array $assets,
    string $root,
    string $case,
): RenderResult {
    $result = $engine->render($html, $options, $assets);
    deadlineExpect(($result->metadata['status'] ?? null) === 'success', 'render returned no successful result');
    deadlineExpect(deadlineStatus($result->jobPath) === 'success', 'successful job is not marked success');
    deadlineExpect(str_starts_with($result->bytes(), '%PDF-'), 'validated PDF is not readable');
    deadlineExpect(is_file($result->scenePath) && is_file($result->bundlePath), 'delivery closure is missing');
    if ($case !== 'timeout') {
        $scene = json_decode((string) file_get_contents($result->scenePath), true, flags: JSON_THROW_ON_ERROR);
        $text = '';
        foreach ($scene['pages'] as $page) {
            foreach ($page['operations'] as $operation) {
                if ($operation['type'] === 'text') {
                    $text .= $operation['text'];
                }
            }
        }
        deadlineExpect(
            $text === 'PLIEGO DEADLINE NORMAL',
            'normal fixture JavaScript mutation did not reach the public scene',
        );
    }

    $target = "{$root}/public/{$case}.pdf";
    $source = fopen($result->pdfPath, 'rb');
    $destination = fopen($target, 'xb');
    deadlineExpect(is_resource($source) && is_resource($destination), 'cannot open local publication streams');
    try {
        $written = stream_copy_to_stream($source, $destination);
        deadlineExpect(is_int($written) && $written === filesize($result->pdfPath), 'local PDF publication was incomplete');
    } finally {
        fclose($source);
        fclose($destination);
    }
    deadlineExpect(hash_file('sha256', $target) === hash_file('sha256', $result->pdfPath), 'stored PDF readback differs');
    deadlineJson("{$root}/storage-records/{$case}.json", [
        'status' => 'success',
        'path' => $target,
        'sha256' => hash_file('sha256', $target),
        'delivery_identity' => $result->deliveryIdentity,
    ]);
    deadlineJson("{$root}/{$case}-result.json", $result->metadata);

    return $result;
}

$binary = isset($argv[1]) ? realpath($argv[1]) : false;
$requestedRoot = $argv[2] ?? '';
deadlineExpect(in_array(count($argv), [3, 4], true) && is_string($binary) && is_file($binary), 'supply one native executable and a fresh proof directory');
$callerArgument = $argv[3] ?? '1';
deadlineExpect(in_array($callerArgument, ['1', '2'], true), 'caller timeout must be 1 or 2 seconds');
$magic = file_get_contents($binary, false, null, 0, 4);
deadlineExpect(is_string($magic) && (
    str_starts_with($magic, 'MZ') || $magic === "\x7fELF"
    || in_array($magic, ["\xfe\xed\xfa\xce", "\xce\xfa\xed\xfe", "\xfe\xed\xfa\xcf", "\xcf\xfa\xed\xfe", "\xca\xfe\xba\xbe", "\xbe\xba\xfe\xca"], true)
), 'the executable must be native, not a script or command wrapper');
deadlineExpect($requestedRoot !== '' && !file_exists($requestedRoot) && !is_link($requestedRoot), 'proof directory must not already exist');
deadlineExpect(is_dir(dirname($requestedRoot)) && mkdir($requestedRoot, 0700), 'proof parent must exist and directory creation must succeed');
$root = realpath($requestedRoot);
deadlineExpect(is_string($root), 'cannot resolve new proof directory');
foreach (['inputs', 'public', 'storage-records'] as $directory) {
    deadlineExpect(mkdir("{$root}/{$directory}", 0700), 'cannot create proof subdirectory');
}

$callerSeconds = (int) $callerArgument;
$engineWallMs = $callerSeconds === 1 ? 999 : 1500;
$maximumElapsedSeconds = 6;
$report = [
    'schema' => 'pliego.php-api2-caller-deadline-proof',
    'version' => 1,
    'status' => 'running',
    'php_version' => PHP_VERSION,
    'php_binary' => PHP_BINARY,
    'php_pid' => getmypid(),
    'os' => PHP_OS_FAMILY,
    'binary' => $binary,
    'binary_sha256' => 'sha256:'.hash_file('sha256', $binary),
    'test_sha256' => hash_file('sha256', __FILE__),
    'document_engine_sha256' => hash_file('sha256', dirname(__DIR__).'/src/DocumentEngine.php'),
    'limits' => [
        'caller_timeout_seconds' => $callerSeconds,
        'engine_host_wall_ms' => $engineWallMs,
        'asserted_render_elapsed_upper_seconds' => $maximumElapsedSeconds,
        'note' => 'SDK requires engine host wall below caller timeout; actual termination cause is recorded separately.',
    ],
    'proof_boundary' => 'Local native API 2 caller deadline, retained failure, success-only local publication, and fresh-process recovery. Not descendant cancellation, concurrency, Laravel storage, remote storage, full operations, or performance evidence.',
];
deadlineJson("{$root}/report.json", $report);

try {
    $font = dirname(__DIR__).'/resources/HasubiMono-Regular.woff2';
    deadlineExpect(is_file($font), 'pinned SDK font is missing');
    $assets = [new InputAsset('font.woff2', $font, 'font/woff2')];
    $prefix = '<!doctype html><meta charset="utf-8"><style>'
        .'@font-face{font-family:Proof;src:url("font.woff2") format("woff2")}'
        .'body{font:12px Proof;margin:0}</style>';
    $normalHtml = $prefix.'<p id="marker">JS NOT EXECUTED</p>'
        .'<script>document.getElementById("marker").textContent="PLIEGO DEADLINE NORMAL";</script>';
    $infiniteHtml = $prefix.'<p>PLIEGO DEADLINE NEVER PUBLISH</p>'
        .'<script>console.info("PLIEGO_INFINITE_LOOP_ENTERED"); while (true) {}</script>';
    deadlineWrite("{$root}/inputs/normal.html", $normalHtml);
    deadlineWrite("{$root}/inputs/synchronous-infinite.html", $infiniteHtml);
    deadlineWrite("{$root}/inputs/font.woff2", (string) file_get_contents($font));
    $options = new RenderOptions(pageSize: 'A4', hostWallMilliseconds: $engineWallMs, diagnosticsRetention: 'always');
    deadlineJson("{$root}/options.json", get_object_vars($options));
    $engine = new DocumentEngine([$binary], "{$root}/jobs", timeoutSeconds: $callerSeconds, probeTimeoutSeconds: 10);
    $report['contract'] = $engine->contract()->toArray();
    deadlineExpect(
        $report['contract']['engine']['runtime']['binary_sha256'] === $report['binary_sha256'],
        'probed engine identity does not match the supplied executable bytes',
    );
    deadlineJson("{$root}/report.json", $report);

    $preflight = deadlineRenderAndPublish($engine, $normalHtml, $options, $assets, $root, 'preflight');
    $report['preflight'] = ['status' => 'success', 'job_path' => $preflight->jobPath, 'pdf_sha256' => hash_file('sha256', $preflight->pdfPath)];
    deadlineJson("{$root}/report.json", $report);

    $failure = null;
    $unexpected = null;
    $started = hrtime(true);
    try {
        $unexpected = deadlineRenderAndPublish($engine, $infiniteHtml, $options, $assets, $root, 'timeout');
    } catch (Throwable $error) {
        $failure = $error;
    }
    $elapsed = (hrtime(true) - $started) / 1_000_000_000;
    $jobPath = $failure !== null && property_exists($failure, 'jobPath') ? $failure->jobPath : $unexpected?->jobPath;
    $runtimePath = $failure !== null && property_exists($failure, 'runtimeJobPath') ? $failure->runtimeJobPath : $unexpected?->runtimeJobPath;
    $report['deadline'] = [
        'elapsed_seconds' => $elapsed,
        'exception_class' => $failure !== null ? $failure::class : null,
        'message' => $failure?->getMessage(),
        'exit_code' => $failure instanceof TransportException || $failure instanceof RenderFailedException ? $failure->exitCode : null,
        'engine_failure_kind' => $failure instanceof RenderFailedException ? $failure->kind : null,
        'job_path' => $jobPath,
        'runtime_job_path' => $runtimePath,
        'retention_status' => is_string($jobPath) ? deadlineStatus($jobPath) : null,
        'caller_deadline_observed' => $failure instanceof TransportException && $failure->getMessage() === "Pliego render-api2 exceeded {$callerSeconds} seconds",
        'bounded_failure_observed' => $failure !== null && $elapsed < $maximumElapsedSeconds,
        'public_pdf_absent' => !file_exists("{$root}/public/timeout.pdf"),
        'storage_record_absent' => !file_exists("{$root}/storage-records/timeout.json"),
        'native_pdf_absent' => is_string($runtimePath) && !file_exists("{$runtimePath}/delivery/document.pdf"),
        'native_bundle_absent' => is_string($runtimePath) && !file_exists("{$runtimePath}/delivery/bundle.json"),
        'loop_entry_observation' => 'Console marker is authored; API 2 terminal failure currently exposes no console journal. Normal preflight independently verifies JS executes.',
    ];
    deadlineJson("{$root}/deadline-error.json", $report['deadline']);
    if ($failure instanceof TransportException) {
        deadlineWrite("{$root}/deadline.stdout", $failure->stdout);
        deadlineWrite("{$root}/deadline.stderr", $failure->stderr);
    } elseif ($failure instanceof RenderFailedException) {
        deadlineJson("{$root}/deadline-engine-result.json", $failure->result);
    }
    deadlineJson("{$root}/report.json", $report);

    // Each render already creates a new native process; also create a new SDK
    // instance to avoid mistaking cached result state for recovery.
    $recoveryEngine = new DocumentEngine([$binary], "{$root}/recovery-jobs", timeoutSeconds: $callerSeconds, probeTimeoutSeconds: 10);
    $recovery = deadlineRenderAndPublish($recoveryEngine, $normalHtml, $options, $assets, $root, 'recovery');
    $report['recovery'] = ['status' => 'success', 'job_path' => $recovery->jobPath, 'pdf_sha256' => hash_file('sha256', $recovery->pdfPath)];
    deadlineExpect($report['deadline']['retention_status'] === 'failure', 'deadline job is not retained as failure');
    foreach (['caller_deadline_observed', 'bounded_failure_observed', 'public_pdf_absent', 'storage_record_absent', 'native_pdf_absent', 'native_bundle_absent'] as $assertion) {
        deadlineExpect($report['deadline'][$assertion], "deadline assertion failed: {$assertion}");
    }
    $report['status'] = 'passed';
} catch (Throwable $error) {
    $report['status'] = 'failed';
    $report['failure'] = ['class' => $error::class, 'message' => $error->getMessage()];
} finally {
    deadlineJson("{$root}/report.json", $report);
}

echo "Native API 2 caller deadline proof {$report['status']}; evidence retained at {$root}\n";
exit($report['status'] === 'passed' ? 0 : 1);
