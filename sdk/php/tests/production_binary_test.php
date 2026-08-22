<?php

declare(strict_types=1);

use Pliego\Php\CliRenderer;
use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\EngineRenderException;
use Pliego\Php\RenderOptions;

require dirname(__DIR__).'/vendor/autoload.php';

function expectProductionBridge(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function normalizeProductionBridgePathSpelling(string $path): string
{
    $path = str_replace('\\', '/', $path);
    if (str_starts_with($path, '//?/UNC/')) {
        $path = '//'.substr($path, 8);
    } elseif (str_starts_with($path, '//?/')) {
        $path = substr($path, 4);
    }

    return PHP_OS_FAMILY === 'Windows' ? strtolower($path) : $path;
}

function productionBridgePathIdentity(mixed $path): string
{
    expectProductionBridge(is_string($path), 'CLI metadata path is not a string');
    $portable = normalizeProductionBridgePathSpelling($path);
    $resolved = realpath($portable);
    expectProductionBridge(is_string($resolved), "CLI metadata path does not exist: {$path}");

    return normalizeProductionBridgePathSpelling($resolved);
}

function removeProductionBridgeFixture(string $path): void
{
    if (is_link($path) || is_file($path)) {
        unlink($path);

        return;
    }
    if (!is_dir($path)) {
        return;
    }
    foreach (scandir($path) ?: [] as $entry) {
        if ($entry !== '.' && $entry !== '..') {
            removeProductionBridgeFixture("{$path}/{$entry}");
        }
    }
    rmdir($path);
}

$binary = $argv[1] ?? '';
$binary = $binary === '' ? false : realpath($binary);
expectProductionBridge(is_string($binary) && is_file($binary), 'a built Pliego binary is required');

$root = sys_get_temp_dir().'/pliego-production-bridge-'.getmypid().'-'.bin2hex(random_bytes(4));
expectProductionBridge(mkdir($root, 0700), 'cannot create the production bridge fixture');
$engine = new DocumentEngine([$binary], "{$root}/api2-work", timeoutSeconds: 120);
$renderer = new CliRenderer([$binary], timeoutSeconds: 120);

try {
    $api2 = $engine->render(
        '<!doctype html><meta charset="utf-8"><title>API 2 production gate</title>'
            .'<p>Pliego API 2 production gate</p>',
        new RenderOptions(pageSize: 'A4', diagnosticsRetention: 'always'),
    );
    expectProductionBridge($api2->metadata['status'] === 'success', 'API 2 result is not successful');
    expectProductionBridge(
        $api2->metadata['request']['profile'] === null
            && $api2->metadata['request']['page']['size'] === ['name' => 'A4'],
        'production binary did not echo the selected profile-null API 2 request',
    );
    expectProductionBridge(str_starts_with($api2->bytes(), '%PDF-'), 'API 2 production PDF is not readable');
    expectProductionBridge(
        is_file($api2->scenePath) && is_file($api2->bundlePath),
        'API 2 production delivery is incomplete',
    );
    $api2Scene = json_decode((string) file_get_contents($api2->scenePath), true, flags: JSON_THROW_ON_ERROR);
    $api2Bundle = json_decode((string) file_get_contents($api2->bundlePath), true, flags: JSON_THROW_ON_ERROR);
    expectProductionBridge(
        ($api2Scene['schema'] ?? null) === 'pliego.document-scene'
            && ($api2Scene['version'] ?? null) === 2,
        'API 2 production scene has the wrong contract identity',
    );
    expectProductionBridge(
        ($api2Bundle['schema'] ?? null) === 'pliego.bundle-manifest'
            && ($api2Bundle['version'] ?? null) === 1,
        'API 2 production bundle has the wrong contract identity',
    );
    expectProductionBridge(
        is_file($api2->diagnosticsPath.'/environment.json'),
        'API 2 production diagnostics were not retained',
    );

    // Explicit API 1 compatibility coverage for the deprecated CliRenderer follows.
    $expectedReadiness = [
        'fixture' => 'php-production-bridge',
        'phase' => 'post-5ms',
        'marker' => 'PLIEGO_POST5MS_7C4E',
        'canvasWidth' => 48,
        'canvasHeight' => 24,
        'readbackBytes' => 4608,
        'epochUnixMs' => 946684800005,
        'performanceNowMs' => 5,
    ];
    $fontBytes = file_get_contents(
        dirname(__DIR__, 3).'/components/fonts/tests/support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf',
    );
    expectProductionBridge(is_string($fontBytes), 'cannot read the pinned controlled-capture font');
    $fontBase64 = base64_encode($fontBytes);
    $successHtml = str_replace(
        '__PLIEGO_PRODUCTION_BRIDGE_FONT__',
        $fontBase64,
        <<<'HTML'
<!doctype html>
<style>
@font-face {
  font-family: "Pliego Production Bridge";
  src: url("data:font/ttf;base64,__PLIEGO_PRODUCTION_BRIDGE_FONT__") format("truetype");
}
html,
body {
  margin: 0;
  width: 100%;
  min-height: 100%;
}
body {
  background: rgb(17, 34, 51);
}
#marker {
  display: block;
  box-sizing: border-box;
  width: 176px;
  height: 28px;
  margin: 12px;
  background: rgb(197, 48, 80);
  color: rgb(255, 255, 255);
  font: 12px/28px "Pliego Production Bridge";
  text-align: center;
}
#canvas {
  display: block;
  width: 48px;
  height: 24px;
  margin: 0 12px 12px;
}
</style>
<div id="marker">PLIEGO_FONT_PREWARM</div>
<canvas id="canvas" width="48" height="24"></canvas>
<script>
window.pliego.defer();
const canvas = document.getElementById("canvas");
const context = canvas.getContext("2d");
context.fillStyle = "rgb(12, 180, 96)";
context.fillRect(0, 0, canvas.width, canvas.height);
context.fillStyle = "rgb(244, 196, 48)";
context.fillRect(8, 6, 20, 10);
const pixels = context.getImageData(0, 0, canvas.width, canvas.height);
const paintObserver = new PerformanceObserver((list) => {
  const fcp = list.getEntries().find((entry) => entry.name === "first-contentful-paint");
  if (!fcp) {
    return;
  }
  const marker = document.getElementById("marker");
  marker.textContent = `PLIEGO_FCP_${fcp.startTime}`;
  console.info(
    `php-production-paint-observed:${fcp.name}:${fcp.startTime}:${fcp.duration}:${performance.now()}`,
  );
  paintObserver.disconnect();
});
paintObserver.observe({ type: "paint" });
requestAnimationFrame(() => {
  console.info("php-production-frame");
});
setTimeout(() => {
  const marker = document.getElementById("marker");
  marker.textContent = "PLIEGO_POST5MS_7C4E";
  document.body.style.background = "rgb(36, 104, 172)";
  document.body.dataset.controlledTime = `${Date.now()}:${performance.now()}`;
  console.info(`php-production-ready:${Date.now()}:${performance.now()}`);
  window.pliego.ready({
    fixture: "php-production-bridge",
    phase: "post-5ms",
    marker: marker.textContent,
    canvasWidth: canvas.width,
    canvasHeight: canvas.height,
    readbackBytes: pixels.data.length,
    epochUnixMs: Date.now(),
    performanceNowMs: performance.now(),
  });
}, 5);
</script>
HTML,
    );
    $success = $renderer->render(
        $successHtml,
        "{$root}/success-input",
        "{$root}/success.pdf",
        "{$root}/success-artifacts",
        new RenderOptions(pageSize: '200x160', pageMargins: '0,0,0,0'),
    );
    expectProductionBridge(str_starts_with($success->bytes(), '%PDF-'), 'production PDF is not readable');
    expectProductionBridge(
        ($success->metadata['environment']['runtime']['adapter'] ?? null) === 'document-session',
        'PHP bridge did not execute the production document-session runtime',
    );
    expectProductionBridge(
        ($success->metadata['readiness'] ?? null) === $expectedReadiness,
        'PHP bridge did not preserve the authored ready payload at the controlled epoch',
    );
    $readinessBytes = file_get_contents("{$root}/success-artifacts/readiness.json");
    expectProductionBridge(is_string($readinessBytes), 'production readiness evidence is missing');
    $readiness = json_decode($readinessBytes, true, flags: JSON_THROW_ON_ERROR);
    expectProductionBridge(
        is_array($readiness)
            && ($readiness['status'] ?? null) === 'ready'
            && ($readiness['font_status'] ?? null) === 'loaded'
            && ($readiness['payload'] ?? null) === $expectedReadiness
            && ($readiness['render_id'] ?? null) === ($success->metadata['render_id'] ?? null),
        'retained readiness evidence does not bind the authored payload to the rendered result',
    );
    expectProductionBridge(
        productionBridgePathIdentity($success->metadata['document_pdf'] ?? null)
            === productionBridgePathIdentity("{$root}/success.pdf"),
        'PHP bridge and CLI disagree on the published output path',
    );
    expectProductionBridge(
        productionBridgePathIdentity($success->metadata['artifacts'] ?? null)
            === productionBridgePathIdentity("{$root}/success-artifacts"),
        'PHP bridge and CLI disagree on the artifact root',
    );

    $preflightPath = "{$root}/preflight-output-and-artifacts";
    try {
        $renderer->render(
            '<!doctype html><div>preflight overlap</div>',
            "{$root}/preflight-input",
            $preflightPath,
            $preflightPath,
        );
        throw new RuntimeException('expected the overlapping output and artifact path to fail');
    } catch (EngineRenderException $error) {
        expectProductionBridge(
            $error->errorCode === 'OUTPUT_ARTIFACTS_OVERLAP',
            'PHP bridge changed the typed preflight code',
        );
        expectProductionBridge($error->exitCode === 1, 'PHP bridge changed the typed preflight exit code');
        expectProductionBridge(
            $error->stderr
                === "pliego: OUTPUT_ARTIFACTS_OVERLAP: requested output must be outside the artifact directory\n",
            'PHP bridge changed the typed preflight diagnostic',
        );
        expectProductionBridge($error->artifactsPath === $preflightPath, 'PHP bridge changed the requested artifact path');
        expectProductionBridge(
            $error->inputBundlePath === "{$root}/preflight-input" && is_dir($error->inputBundlePath),
            'PHP bridge did not retain the preflight input bundle',
        );
        expectProductionBridge(
            !file_exists($preflightPath) && !is_link($preflightPath),
            'typed preflight failure created a public artifact tree or output',
        );
        expectProductionBridge(
            (glob("{$root}/.pliego-runtime-*") ?: []) === [],
            'typed preflight failure retained a private runtime container',
        );
    }

    file_put_contents("{$root}/blocked.js", 'document.body.dataset.blocked = "1";');
    try {
        $renderer->render(
            '<!doctype html><script src="../blocked.js"></script><div></div>',
            "{$root}/failure-input",
            "{$root}/failure.pdf",
            "{$root}/failure-artifacts",
        );
        throw new RuntimeException('expected the outside-root resource to fail');
    } catch (EngineRenderException $error) {
        expectProductionBridge($error->errorCode === 'RESOURCE_DENIED', 'typed CLI failure changed');
        expectProductionBridge($error->exitCode === 1, 'typed CLI failure exit code changed');
        expectProductionBridge(!is_file("{$root}/failure.pdf"), 'failed render published a PDF');
        expectProductionBridge(
            is_file("{$root}/failure-artifacts/environment.json"),
            'failed render did not retain its artifact evidence',
        );
    }
} finally {
    removeProductionBridgeFixture($root);
}

echo "Production API 2 DocumentEngine and deprecated API 1 compatibility integration passed\n";
