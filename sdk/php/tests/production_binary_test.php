<?php

declare(strict_types=1);

use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\EngineRenderException;

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
$renderer = new CliRenderer([$binary], timeoutSeconds: 120);

try {
    $success = $renderer->render(
        '<!doctype html><style>div{width:96px;height:48px;background:#1463ff}</style><div></div>',
        "{$root}/success-input",
        "{$root}/success.pdf",
        "{$root}/success-artifacts",
    );
    expectProductionBridge(str_starts_with($success->bytes(), '%PDF-'), 'production PDF is readable');
    expectProductionBridge(
        ($success->metadata['environment']['runtime']['adapter'] ?? null) === 'document-session',
        'PHP bridge did not execute the production document-session runtime',
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

echo "Production DocumentSession PHP bridge integration passed\n";
