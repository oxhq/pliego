<?php

declare(strict_types=1);

use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\InvocationException;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\Exception\TransportException;
use Pliego\Php\Exception\UnsupportedContractException;
use Pliego\Php\InputAsset;
use Pliego\Php\JobRetention;
use Pliego\Php\RenderOptions;

require dirname(__DIR__).'/vendor/autoload.php';

function api2RenderExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function api2RenderStatus(string $jobPath): string
{
    return trim((string) file_get_contents($jobPath.DIRECTORY_SEPARATOR.JobRetention::STATUS_FILE));
}

$root = sys_get_temp_dir().'/pliego-php-api2-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($root, 0700);
$asset = $root.DIRECTORY_SEPARATOR.'app.css';
file_put_contents($asset, "body { color: #123; }\n");
$engine = new DocumentEngine(
    [PHP_BINARY, __DIR__.'/fake_api2.php'],
    $root.DIRECTORY_SEPARATOR.'work',
);

try {
    api2RenderExpect(
        $engine->contract()->select(DocumentEngine::requiredProtocol()) !== null,
        'DocumentEngine selects the exact API 2 tuple',
    );

    foreach (['empty', 'profile'] as $probeMode) {
        putenv("PLIEGO_API2_PROBE_FAKE_MODE={$probeMode}");
        try {
            (new DocumentEngine(
                [PHP_BINARY, __DIR__.'/fake_api2.php'],
                $root.DIRECTORY_SEPARATOR."unsupported-{$probeMode}-work",
            ))->contract();
            throw new RuntimeException("expected unsupported API 2 tuple for {$probeMode}");
        } catch (UnsupportedContractException) {
        } finally {
            putenv('PLIEGO_API2_PROBE_FAKE_MODE');
        }
    }

    putenv('PLIEGO_API2_RENDER_FAKE_MODE=success');
    $result = $engine->render(
        '<!doctype html><link rel="stylesheet" href="assets/app.css"><p>invoice</p>',
        new RenderOptions(locale: 'es-MX', timezone: 'PST8PDT'),
        [new InputAsset('assets/app.css', $asset, 'text/css;charset=utf-8')],
    );
    api2RenderExpect(str_starts_with($result->bytes(), '%PDF-'), 'API 2 PDF bytes are readable');
    api2RenderExpect($result->metadata['status'] === 'success', 'API 2 success is retained');
    api2RenderExpect($result->jobPath !== $result->runtimeJobPath, 'retention state is outside runtime cwd-v1');
    api2RenderExpect(dirname($result->runtimeJobPath) === $result->jobPath, 'runtime root belongs to retained job');
    api2RenderExpect(api2RenderStatus($result->jobPath) === 'success', 'successful outer job is marked');
    api2RenderExpect(
        !file_exists($result->runtimeJobPath.DIRECTORY_SEPARATOR.JobRetention::STATUS_FILE),
        'runtime cwd-v1 contains no retention marker',
    );
    api2RenderExpect($result->pdfPath === $result->deliveryPath.DIRECTORY_SEPARATOR.'document.pdf', 'fixed PDF path');
    api2RenderExpect(is_file($result->scenePath) && is_file($result->bundlePath), 'scene and bundle are exposed');
    api2RenderExpect(
        $result->deliveryIdentity === $result->metadata['delivery']['bundle']['sha256'],
        'bundle descriptor hash is the delivery identity',
    );
    api2RenderExpect(
        ($result->bridgeTimings['schema'] ?? null) === 'pliego.php-bridge-timings'
            && ($result->bridgeTimings['version'] ?? null) === 2,
        'API 2 bridge timings are retained outside cwd-v1',
    );

    $request = $result->metadata['request'];
    api2RenderExpect($request['environment']['timezone'] === 'America/Tijuana', 'legacy timezone is normalized');
    api2RenderExpect(
        $request['page']['size'] === ['width_app_units' => 48_960, 'height_app_units' => 63_360],
        'legacy Letter defaults become exact app units',
    );
    api2RenderExpect(
        $request['page']['margins_app_units'] === ['top' => 2_880, 'right' => 2_880, 'bottom' => 2_880, 'left' => 2_880],
        'legacy margins become exact app units',
    );
    api2RenderExpect(
        $request['settlement']['limits'] === [
            'virtual_span_ms' => 86_400_000,
            'ordinary_tasks' => 100_000,
            'microtasks' => 1_000_000,
            'rendering_opportunities' => 10_000,
            'mutations' => 1_000_000,
            'host_wall_ms' => 60_000,
        ],
        'request emits only enforced settlement limits',
    );
    $manifest = json_decode(
        (string) file_get_contents($result->runtimeJobPath.DIRECTORY_SEPARATOR.'input-manifest.json'),
        true,
        flags: JSON_THROW_ON_ERROR,
    );
    api2RenderExpect(
        array_column($manifest['entries'], 'path') === ['assets/app.css', 'document.html'],
        'input manifest entries use ascending ASCII order',
    );
    api2RenderExpect(
        $manifest['entries'][0]['sha256'] === 'sha256:'.hash_file('sha256', $asset),
        'input manifest binds staged asset bytes',
    );

    $a4 = $engine->render(
        '<!doctype html><p>A4</p>',
        new RenderOptions(pageSize: 'A4', diagnosticsRetention: 'none'),
    );
    api2RenderExpect($a4->metadata['request']['page']['size'] === ['name' => 'A4'], 'named A4 is supported');
    api2RenderExpect(!is_dir($a4->diagnosticsPath), 'diagnostics none retains no runtime directory');

    putenv('PLIEGO_API2_RENDER_FAKE_MODE=failed');
    try {
        $engine->render('<!doctype html><p>fail</p>');
        throw new RuntimeException('expected accepted API 2 failure');
    } catch (RenderFailedException $error) {
        api2RenderExpect($error->kind === 'resource', 'stable failure kind is exposed');
        api2RenderExpect($error->result['delivery'] === null, 'failed result has no delivery');
        api2RenderExpect(is_file($error->diagnosticsPath.DIRECTORY_SEPARATOR.'failure.json'), 'failure evidence retained');
        api2RenderExpect(api2RenderStatus($error->jobPath) === 'failure', 'accepted failure is marked');
        api2RenderExpect(
            ($error->bridgeTimings['schema'] ?? null) === 'pliego.php-bridge-timings'
                && ($error->bridgeTimings['version'] ?? null) === 2,
            'accepted failure retains bridge timings',
        );
    }

    putenv('PLIEGO_API2_RENDER_FAKE_MODE=exit64');
    try {
        $engine->render('<!doctype html><p>invalid invocation</p>');
        throw new RuntimeException('expected invocation exception');
    } catch (InvocationException $error) {
        api2RenderExpect($error->exitCode === 64, 'invocation exception preserves exit 64');
        api2RenderExpect($error->getMessage() === 'invalid API 2 fixture request', 'invocation diagnostic preserved');
        api2RenderExpect(is_string($error->jobPath), 'invocation exception exposes retained job');
        api2RenderExpect(api2RenderStatus($error->jobPath) === 'failure', 'invocation failure is marked');
    }

    putenv('PLIEGO_API2_RENDER_FAKE_MODE=exit74');
    try {
        $engine->render('<!doctype html><p>transport failure</p>');
        throw new RuntimeException('expected accepted transport exception');
    } catch (TransportException $error) {
        api2RenderExpect($error->exitCode === 74, 'transport exception preserves exit 74');
        api2RenderExpect(
            $error->getMessage() === 'pliego: API2_TRANSPORT_ERROR: fixture terminal write failed',
            'transport diagnostic line becomes the exception message',
        );
        api2RenderExpect(
            $error->stdout === '{"schema":"pliego.render-' && str_ends_with($error->stderr, "\n"),
            'accepted transport failure retains unusable partial stdout and canonical stderr',
        );
        api2RenderExpect(is_string($error->jobPath), 'transport exception exposes retained job');
        api2RenderExpect(api2RenderStatus($error->jobPath) === 'failure', 'transport failure is marked');
    }

    foreach ([
        'tamper-pdf' => 'does not match retained bytes',
        'tamper-bundle' => 'does not match retained bytes',
        'extra-delivery' => 'unmanifested entries',
        'engine-mismatch' => 'does not exactly match the probed executable',
        'unknown-result-member' => 'unsupported or out-of-order members',
        'noncanonical-result' => 'one compact JSON object',
        'stderr-success' => 'unsupported API 2 exit/stderr combination',
        'wrong-exit' => 'unsupported API 2 exit/stderr combination',
        'malformed-exit74' => 'malformed API 2 transport error',
    ] as $mode => $message) {
        putenv("PLIEGO_API2_RENDER_FAKE_MODE={$mode}");
        try {
            $engine->render("<!doctype html><p>{$mode}</p>");
            throw new RuntimeException("expected transport rejection for {$mode}");
        } catch (TransportException $error) {
            api2RenderExpect(str_contains($error->getMessage(), $message), "{$mode} rejection is actionable");
            api2RenderExpect(is_string($error->jobPath), "{$mode} retains a job path");
            api2RenderExpect(api2RenderStatus($error->jobPath) === 'failure', "{$mode} is marked failed");
        }
    }

    putenv('PLIEGO_API2_RENDER_FAKE_MODE=success');
    try {
        $engine->render(
            '<!doctype html><p>network</p>',
            new RenderOptions(allowedHttpRoots: ['https://example.test/assets/']),
        );
        throw new RuntimeException('expected API 2 network convenience rejection');
    } catch (InvalidArgumentException $error) {
        api2RenderExpect(str_contains($error->getMessage(), 'prefetch'), 'network migration is actionable');
    }

    try {
        $engine->render(
            '<!doctype html><p>collision</p>',
            assets: [
                new InputAsset('A', $asset),
                new InputAsset('a/child.css', $asset),
            ],
        );
        throw new RuntimeException('expected case-insensitive path-prefix rejection');
    } catch (InvalidArgumentException $error) {
        api2RenderExpect(
            str_contains($error->getMessage(), 'also a directory prefix'),
            'input closure rejects case-insensitive file/directory prefix collisions',
        );
    }

    $timeoutEngine = new DocumentEngine(
        [PHP_BINARY, __DIR__.'/fake_api2.php'],
        $root.DIRECTORY_SEPARATOR.'timeout-work',
        timeoutSeconds: 1,
    );
    putenv('PLIEGO_API2_RENDER_FAKE_MODE=timeout');
    $started = hrtime(true);
    try {
        $timeoutEngine->render(
            '<!doctype html><p>timeout</p>',
            new RenderOptions(hostWallMilliseconds: 500),
        );
        throw new RuntimeException('expected API 2 process timeout');
    } catch (TransportException $error) {
        api2RenderExpect(str_contains($error->getMessage(), 'exceeded 1 seconds'), 'timeout is typed');
        api2RenderExpect(api2RenderStatus($error->jobPath) === 'failure', 'timeout is retained as failure');
        api2RenderExpect((hrtime(true) - $started) / 1_000_000_000 < 3, 'timeout is wall-clock bounded');
    }
} finally {
    putenv('PLIEGO_API2_RENDER_FAKE_MODE');
    putenv('PLIEGO_API2_PROBE_FAKE_MODE');
}

echo "Pliego PHP API 2 render transaction self-test passed; evidence retained at {$root}\n";
