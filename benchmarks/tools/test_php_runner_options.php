<?php

declare(strict_types=1);

// Pure helper/copy tests; no native render, process measurement, or PDF qualification.
define('PLIEGO_BENCHMARK_RUNNER_LIBRARY_ONLY', true);
require dirname(__DIR__) . '/runners/pliego.php';

function check(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function rejects(callable $operation): void
{
    try {
        $operation();
    } catch (RuntimeException) {
        return;
    }
    throw new RuntimeException('expected rejection');
}

if (($argv[1] ?? '') === 'reject-size') {
    api2_page_size($argv[2]);
    exit(0);
}
if (($argv[1] ?? '') === 'reject-margins') {
    api2_page_margins($argv[2]);
    exit(0);
}
if (($argv[1] ?? '') === 'reject-deadline') {
    root_wall_timeout_option($argv[2]);
    exit(0);
}

check(api2_page_size(null) === ['width_app_units' => 48960, 'height_app_units' => 63360], 'default page changed');
check(api2_page_size('A4') === ['name' => 'A4'], 'named A4 changed');
check(api2_page_size('793.7008x1122.52') === ['width_app_units' => 47622, 'height_app_units' => 67351], 'CSS conversion changed');
check(api2_page_size('67351x47622au') === ['width_app_units' => 67351, 'height_app_units' => 47622], 'Au authority lost');
check(api2_page_size('2147483647x1au')['width_app_units'] === 2147483647, 'i32 maximum lost');
check(api2_page_margins(null) === ['top' => 2880, 'right' => 2880, 'bottom' => 2880, 'left' => 2880], 'default margins changed');
check(api2_page_margins('0,12.5,0,12.5') === ['top' => 0, 'right' => 750, 'bottom' => 0, 'left' => 750], 'CSS margins changed');
check(api2_page_margins('2268,2268,5669,0au') === ['top' => 2268, 'right' => 2268, 'bottom' => 5669, 'left' => 0], 'exact margins lost');
check(root_wall_timeout_option(null) === null && root_wall_timeout_option('1.5') === 1.5, 'deadline option changed');
check(is_browsershot_adapter_path('/repo/benchmarks/adapters/invobook-browsershot/adapter.php'), 'Invobook classification missing');
check(is_browsershot_adapter_path('C:\repo\benchmarks\adapters\invobook-browsershot\adapter.php'), 'Windows classification missing');
foreach (['/repo/invobook-browsershot/adapter.php', '/repo/benchmarks/adapters/invobook-browsershot/adapter.php.bad', '/repo/benchmarks/adapters/aureus-dompdf/adapter.php'] as $path) {
    check(!is_browsershot_adapter_path($path), 'overbroad browser classification');
}

foreach ([
    'reject-size' => ['0x1au', '-1x2au', '01x2au', '1.5x2au', '2147483648x1au', '999999999999999999999x1au', '1aux2au', 'A4au'],
    'reject-margins' => ['-1,0,0,0au', '1.5,0,0,0au', '0,0,0au', '0,0,0,2147483648au', '0au,0,0,0au'],
    'reject-deadline' => ['0', '-1', 'NaN', 'INF', '1e3', '01'],
] as $mode => $values) {
    foreach ($values as $value) {
        $process = proc_open([PHP_BINARY, __FILE__, $mode, $value], [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']], $pipes);
        check(is_resource($process), 'cannot start negative helper test');
        fclose($pipes[0]);
        $stdout = stream_get_contents($pipes[1]);
        $stderr = stream_get_contents($pipes[2]);
        fclose($pipes[1]);
        fclose($pipes[2]);
        check(proc_close($process) === 2 && $stdout === '' && str_contains($stderr, 'pliego.php:'), "accepted invalid {$mode}: {$value}");
    }
}

$temporary = sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'pliego-retention-test-' . bin2hex(random_bytes(8));
check(mkdir($temporary, 0700), 'cannot create test directory');
try {
    $fixture = $temporary . DIRECTORY_SEPARATOR . 'fixture';
    $source = $temporary . DIRECTORY_SEPARATOR . 'source';
    check(mkdir($fixture, 0700) && mkdir($source, 0700), 'cannot create fixture roots');
    file_put_contents($fixture . '/input.html', '<p>synthetic</p>');
    file_put_contents($source . '/document.pdf', 'synthetic PDF bytes, not a render proof');
    rejects(fn () => prepare_retention_root($fixture, $fixture));
    rejects(fn () => prepare_retention_root($fixture . '/nested', $fixture));
    rejects(fn () => prepare_retention_root('relative-proof', $fixture));
    $retained = prepare_retention_root($temporary . '/proof', $fixture);
    $state = ['retainRoot' => $retained, 'binary' => PHP_BINARY, 'binarySha256' => hash_file('sha256', PHP_BINARY),
        'fixtureInputSha256' => str_repeat('a', 64), 'fixtureBundleSha256' => str_repeat('b', 64), 'rootWallTimeoutMs' => 1000.0];
    $sample = ['index' => 0, 'ok' => true];
    $exec = ['wall_ms' => 100.25, 'one_shot_wall_ms' => 210.5, 'resource_usage' => (object) ['drain_ms' => 2.125],
        'stdout' => "result\n", 'stderr' => '', 'sampler_stdout' => "{}\n", 'sampler_stderr' => '', 'request' => "{}\n"];
    check(retain_benchmark_attempt(array_replace($state, ['retainRoot' => null]), 0, $sample, $exec, []) === $sample, 'disabled retention changed sample');
    $copied = retain_benchmark_attempt($state, 0, $sample, $exec, ['output' => $source]);
    check(array_keys($copied['retained']) === ['artifacts_dir', 'output_dir'], 'retention broke the existing sample schema');
    check($copied['ok'] === true && is_file($source . '/document.pdf'), 'retention changed outcome or source');
    $root = dirname($copied['retained']['artifacts_dir']);
    $manifest = json_decode(file_get_contents($root . '/manifest.json'), true, flags: JSON_THROW_ON_ERROR);
    check($manifest['phase'] === 'timed' && $manifest['timing']['tree_wall_ms'] === 102.375, 'timing boundary lost');
    check($manifest['timing']['sampler_lifecycle_wall_ms'] === 210.5, 'sampler boundary changed');
    foreach ($manifest['files'] as $path => $descriptor) {
        check(hash_file('sha256', $root . '/' . $path) === $descriptor['sha256'], 'retained hash mismatch');
        check(filesize($root . '/' . $path) === $descriptor['bytes'], 'retained size mismatch');
    }
    check(json_decode(file_get_contents($root . '/sample.json'), true)['retained'] === $copied['retained'], 'sample path closure mismatch');
    rejects(fn () => retain_benchmark_attempt($state, 0, $sample, $exec, ['output' => $source]));
    $failure = retain_benchmark_attempt($state, -1000000, ['ok' => false, 'error' => 'ROOT_WALL_TIMEOUT'], ['stderr' => 'timeout'], ['job' => $source]);
    $failedManifest = json_decode(file_get_contents(dirname($failure['retained']['artifacts_dir']) . '/manifest.json'), true);
    check($failedManifest['phase'] === 'preflight' && $failedManifest['timing']['root_wall_ms'] === null
        && $failedManifest['timing']['tree_wall_ms'] === null && $failure['ok'] === false, 'failure fabricated measured success');
    check(benchmark_timing_boundaries(['wall_ms' => 1, 'one_shot_wall_ms' => 1])['tree_wall_ms'] === null, 'unavailable tree metric fabricated');
    if (@link($source . '/document.pdf', $source . '/hardlink.pdf')) {
        rejects(fn () => retain_benchmark_attempt($state, 1, $sample, $exec, ['output' => $source]));
        unlink($source . '/hardlink.pdf');
    } else {
        fwrite(STDERR, "hardlink rejection unavailable on this filesystem\n");
    }
    if (@symlink($fixture . '/input.html', $source . '/link.html')) {
        rejects(fn () => retain_benchmark_attempt($state, 2, $sample, $exec, ['output' => $source]));
        unlink($source . '/link.html');
    } else {
        fwrite(STDERR, "symlink rejection requires hosted filesystem proof\n");
    }
} finally {
    rrmdir($temporary);
}
fwrite(STDOUT, "PHP benchmark Au, retention, deadline and classification helper tests passed\n");
