<?php

declare(strict_types=1);

use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\EngineRenderException;

require dirname(__DIR__).'/vendor/autoload.php';

function timingExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

/** @param array<string, mixed> $timings */
function assertTimings(array $timings): void
{
    timingExpect(($timings['schema'] ?? null) === 'pliego.php-bridge-timings', 'timing schema');
    timingExpect(($timings['version'] ?? null) === 1, 'timing version');
    timingExpect(is_float($timings['total_ms'] ?? null), 'total is measured');
    foreach ([
        'laravel_setup',
        'view_render',
        'bundle_staging',
        'asset_manifest_hash',
        'runtime_resolution',
        'runtime_install',
        'process_launch',
        'stdin_stdout',
        'native_wait',
        'result_parse',
        'publication_copy',
        'cleanup',
        'unattributed',
    ] as $phase) {
        timingExpect(array_key_exists($phase, $timings['phases_ms']), "missing {$phase}");
        $value = $timings['phases_ms'][$phase];
        timingExpect($value === null || (is_float($value) && $value >= 0), "invalid {$phase}");
    }
    $sum = array_sum(array_filter($timings['phases_ms'], is_float(...)));
    timingExpect(abs($sum - $timings['total_ms']) < 0.02, 'phases reconcile to bridge total');
    timingExpect(is_float($timings['native_engine_ms']), 'native engine total is carried');
    timingExpect(
        abs($timings['native_engine_ms'] + $timings['bridge_overhead_ms'] - $timings['total_ms']) < 0.002,
        'native engine and bridge overhead reconcile',
    );
    timingExpect(($timings['diagnostics']['retained'] ?? null) === true, 'diagnostics retained');
}

$root = sys_get_temp_dir().'/pliego-php-timings-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($root, 0700);
$asset = "{$root}/source.txt";
file_put_contents($asset, "asset\n");
$renderer = new CliRenderer([PHP_BINARY, __DIR__.'/fake_pliego.php']);

$result = $renderer->render(
    '<p>success</p>',
    "{$root}/success-input",
    "{$root}/success.pdf",
    "{$root}/success-artifacts",
    assets: ['assets/test.txt' => $asset],
);
assertTimings($result->bridgeTimings);
timingExpect($result->bridgeTimings['phases_ms']['view_render'] === null, 'standalone view is unavailable');
timingExpect($result->bridgeTimings['phases_ms']['runtime_resolution'] === null, 'standalone runtime is unavailable');
timingExpect($result->bridgeTimings['phases_ms']['runtime_install'] === null, 'install is outside render');
$diagnostics = "{$root}/.pliego-bridge-timings.json";
timingExpect(
    json_decode((string) file_get_contents($diagnostics), true, flags: JSON_THROW_ON_ERROR)
        === $result->bridgeTimings,
    'success diagnostics match RenderResult',
);
timingExpect(!str_contains(json_encode($result->bridgeTimings, JSON_THROW_ON_ERROR), $root), 'timings leak no paths');

$unavailable = $renderer->render(
    '<p>NO_ENGINE_TIMINGS</p>',
    "{$root}/unavailable-input",
    "{$root}/unavailable.pdf",
    "{$root}/unavailable-artifacts",
    assets: ['assets/test.txt' => $asset],
)->bridgeTimings;
timingExpect($unavailable['native_engine_ms'] === null, 'missing native total remains unavailable');
timingExpect($unavailable['bridge_overhead_ms'] === null, 'bridge overhead is not fabricated without native total');
timingExpect(
    ($unavailable['unavailable']['native_engine'] ?? null) === 'engine-did-not-report-total-engine',
    'missing native total has an explicit reason',
);

try {
    $renderer->render(
        'FAIL_ENGINE',
        "{$root}/failure-input",
        "{$root}/failure.pdf",
        "{$root}/failure-artifacts",
        assets: ['assets/test.txt' => $asset],
    );
    throw new RuntimeException('expected typed failure');
} catch (EngineRenderException $error) {
    assertTimings($error->bridgeTimings);
    timingExpect($error->errorCode === 'RESOURCE_DENIED', 'typed failure preserved');
    timingExpect(
        json_decode((string) file_get_contents($diagnostics), true, flags: JSON_THROW_ON_ERROR)
            === $error->bridgeTimings,
        'failure diagnostics match exception',
    );
    timingExpect(!str_contains(json_encode($error->bridgeTimings, JSON_THROW_ON_ERROR), $root), 'failure timings leak no paths');
}

echo "Pliego PHP bridge timing self-test passed; evidence retained at {$root}\n";
