<?php

declare(strict_types=1);

use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\TransportException;
use Pliego\Php\InputAsset;
use Pliego\Php\JobRetention;
use Pliego\Php\RenderOptions;

require dirname(__DIR__).'/vendor/autoload.php';

// Internal driver for check_production_cancellation.py, not a standalone timeout test.
// RUN and RECOVER stdin handshakes keep probes/preflight/recovery out of cancellation.
function cancellationExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function cancellationWrite(string $path, string $bytes): void
{
    cancellationExpect(file_put_contents($path, $bytes) === strlen($bytes), "cannot retain {$path}");
}

function cancellationJson(string $path, array $document): void
{
    cancellationWrite($path, json_encode($document, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n");
}

function cancellationEvent(string $phase): void
{
    echo json_encode(['phase' => $phase, 'php_pid' => getmypid()], JSON_THROW_ON_ERROR)."\n";
    fflush(STDOUT);
}

function cancellationHandshake(string $expected): void
{
    stream_set_timeout(STDIN, 90);
    cancellationExpect(fgets(STDIN) === $expected."\n", "missing orchestrator handshake {$expected}");
}

function cancellationStatus(string $path): ?string
{
    $status = $path.'/'.JobRetention::STATUS_FILE;

    return is_file($status) ? trim((string) file_get_contents($status)) : null;
}

/** Same success-only local publication callback for preflight, cancellation and recovery. */
function cancellationRender(
    DocumentEngine $engine,
    string $html,
    RenderOptions $options,
    array $assets,
    string $root,
    string $case,
): array {
    $result = $engine->render($html, $options, $assets);
    cancellationExpect(($result->metadata['status'] ?? null) === 'success', 'render did not return success');
    cancellationExpect(cancellationStatus($result->jobPath) === 'success', 'successful job was not marked success');
    cancellationExpect(str_starts_with($result->bytes(), '%PDF-'), 'validated PDF is missing');
    cancellationExpect(is_file($result->scenePath) && is_file($result->bundlePath), 'delivery closure is missing');
    if ($case !== 'cancelled') {
        $scene = json_decode((string) file_get_contents($result->scenePath), true, flags: JSON_THROW_ON_ERROR);
        $text = '';
        foreach ($scene['pages'] as $page) {
            foreach ($page['operations'] as $operation) {
                if ($operation['type'] === 'text') {
                    $text .= $operation['text'];
                }
            }
        }
        cancellationExpect($text === 'PLIEGO CANCELLATION NORMAL', 'preflight/recovery JS mutation did not reach the scene');
    }
    $target = "{$root}/public/{$case}.pdf";
    $source = fopen($result->pdfPath, 'rb');
    $destination = fopen($target, 'xb');
    cancellationExpect(is_resource($source) && is_resource($destination), 'cannot open local publication streams');
    try {
        $written = stream_copy_to_stream($source, $destination);
        cancellationExpect($written === filesize($result->pdfPath), 'local publication was incomplete');
    } finally {
        fclose($source);
        fclose($destination);
    }
    cancellationExpect(hash_file('sha256', $target) === hash_file('sha256', $result->pdfPath), 'stored readback differs');
    $record = ['status' => 'success', 'job_path' => $result->jobPath, 'pdf_sha256' => hash_file('sha256', $target),
        'delivery_identity' => $result->deliveryIdentity];
    cancellationJson("{$root}/storage-records/{$case}.json", $record);
    cancellationJson("{$root}/{$case}-result.json", $result->metadata);

    return $record;
}

$binary = isset($argv[1]) ? realpath($argv[1]) : false;
$requestedRoot = $argv[2] ?? '';
cancellationExpect(PHP_OS_FAMILY === 'Linux', 'this proof requires the Linux pidfd orchestrator');
cancellationExpect(count($argv) === 3 && is_string($binary) && is_file($binary), 'supply a native executable and a fresh proof directory');
cancellationExpect(file_get_contents($binary, false, null, 0, 4) === "\x7fELF", 'native ELF executable required, no wrapper');
cancellationExpect($requestedRoot !== '' && !file_exists($requestedRoot) && !is_link($requestedRoot), 'proof directory must be fresh');
cancellationExpect(is_dir(dirname($requestedRoot)) && mkdir($requestedRoot, 0700), 'proof parent must exist');
$root = realpath($requestedRoot);
cancellationExpect(is_string($root), 'cannot resolve proof directory');
foreach (['inputs', 'public', 'storage-records'] as $directory) {
    cancellationExpect(mkdir("{$root}/{$directory}", 0700), 'cannot create proof subdirectory');
}
$report = [
    'schema' => 'pliego.php-api2-forced-cancellation-proof', 'version' => 1, 'status' => 'running',
    'php_pid' => getmypid(), 'php_version' => PHP_VERSION, 'php_binary' => PHP_BINARY,
    'binary' => $binary, 'binary_sha256' => 'sha256:'.hash_file('sha256', $binary),
    'test_sha256' => hash_file('sha256', __FILE__),
    'document_engine_sha256' => hash_file('sha256', dirname(__DIR__).'/src/DocumentEngine.php'),
    'limits' => ['engine_host_wall_ms' => 60_000, 'sdk_timeout_seconds' => 65, 'probe_timeout_seconds' => 10],
    'proof_boundary' => 'Real API 2 externally killed process, typed transport failure, retained failed job, success-only local stream publication/readback, and fresh-process recovery. Python pidfd census separately accounts for observed processes. Not graceful cancellation, Laravel/remote storage, historical escaped descendants, or performance evidence.',
];
cancellationJson("{$root}/report.json", $report);
try {
    cancellationEvent('boot');
    cancellationHandshake('START');
    $font = dirname(__DIR__).'/resources/HasubiMono-Regular.woff2';
    $assets = [new InputAsset('font.woff2', $font, 'font/woff2')];
    $prefix = '<!doctype html><meta charset="utf-8"><style>'
        .'@font-face{font-family:Proof;src:url("font.woff2") format("woff2")}body{font:12px Proof;margin:0}</style>';
    $normal = $prefix.'<p id="marker">JS NOT EXECUTED</p>'
        .'<script>document.getElementById("marker").textContent="PLIEGO CANCELLATION NORMAL";</script>';
    $infinite = $prefix.'<p>PLIEGO CANCELLATION NEVER PUBLISH</p><script>while (true) {}</script>';
    cancellationWrite("{$root}/inputs/normal.html", $normal);
    cancellationWrite("{$root}/inputs/synchronous-infinite.html", $infinite);
    cancellationWrite("{$root}/inputs/font.woff2", (string) file_get_contents($font));
    $options = new RenderOptions(pageSize: 'A4', hostWallMilliseconds: 60_000, diagnosticsRetention: 'always');
    cancellationJson("{$root}/options.json", get_object_vars($options));
    $engine = new DocumentEngine([$binary], "{$root}/preflight-jobs", timeoutSeconds: 65, probeTimeoutSeconds: 10);
    $report['contract'] = $engine->contract()->toArray();
    cancellationExpect($report['contract']['engine']['runtime']['binary_sha256'] === $report['binary_sha256'], 'binary/probe identity mismatch');
    $report['preflight'] = cancellationRender($engine, $normal, $options, $assets, $root, 'preflight');
    $cancelEngine = new DocumentEngine([$binary], "{$root}/cancel-jobs", timeoutSeconds: 65, probeTimeoutSeconds: 10);
    cancellationExpect($cancelEngine->contract()->toArray() === $report['contract'], 'cancellation contract drift');
    cancellationJson("{$root}/report.json", $report);
    cancellationEvent('ready');
    cancellationHandshake('RUN');
    cancellationEvent('rendering');
    $started = hrtime(true);
    $failure = null;
    try {
        cancellationRender($cancelEngine, $infinite, $options, $assets, $root, 'cancelled');
    } catch (Throwable $error) {
        $failure = $error;
    }
    $elapsed = (hrtime(true) - $started) / 1_000_000_000;
    $job = $failure instanceof TransportException ? $failure->jobPath : null;
    $runtime = $failure instanceof TransportException ? $failure->runtimeJobPath : null;
    $report['cancellation'] = [
        'elapsed_seconds' => $elapsed, 'exception_class' => $failure !== null ? $failure::class : null,
        'message' => $failure?->getMessage(), 'exit_code' => $failure instanceof TransportException ? $failure->exitCode : null,
        'job_path' => $job, 'runtime_job_path' => $runtime,
        'retention_status' => is_string($job) ? cancellationStatus($job) : null,
        'transport_failure' => $failure instanceof TransportException,
        'not_sdk_deadline' => $failure instanceof TransportException && !str_contains($failure->getMessage(), 'exceeded 65 seconds'),
        'before_engine_deadline' => $elapsed < 30,
        'public_pdf_absent' => !file_exists("{$root}/public/cancelled.pdf"),
        'storage_record_absent' => !file_exists("{$root}/storage-records/cancelled.json"),
        'native_pdf_absent' => is_string($runtime) && !file_exists("{$runtime}/delivery/document.pdf"),
        'native_scene_absent' => is_string($runtime) && !file_exists("{$runtime}/delivery/scene.json"),
        'native_bundle_absent' => is_string($runtime) && !file_exists("{$runtime}/delivery/bundle.json"),
    ];
    if ($failure instanceof TransportException) {
        cancellationWrite("{$root}/cancellation.stdout", $failure->stdout);
        cancellationWrite("{$root}/cancellation.stderr", $failure->stderr);
    }
    cancellationJson("{$root}/cancellation-error.json", $report['cancellation']);
    cancellationJson("{$root}/report.json", $report);
    cancellationExpect($report['cancellation']['retention_status'] === 'failure', 'cancelled job not retained as failure');
    foreach (['transport_failure', 'not_sdk_deadline', 'before_engine_deadline', 'public_pdf_absent', 'storage_record_absent', 'native_pdf_absent', 'native_scene_absent', 'native_bundle_absent'] as $check) {
        cancellationExpect($report['cancellation'][$check], "cancellation assertion failed: {$check}");
    }
    cancellationEvent('cancelled');
    cancellationHandshake('RECOVER');
    $recovery = new DocumentEngine([$binary], "{$root}/recovery-jobs", timeoutSeconds: 65, probeTimeoutSeconds: 10);
    $report['recovery'] = cancellationRender($recovery, $normal, $options, $assets, $root, 'recovery');
    $report['status'] = 'passed';
} catch (Throwable $error) {
    $report['status'] = 'failed';
    $report['failure'] = ['class' => $error::class, 'message' => $error->getMessage()];
} finally {
    cancellationJson("{$root}/report.json", $report);
}
cancellationEvent($report['status']);
exit($report['status'] === 'passed' ? 0 : 1);
