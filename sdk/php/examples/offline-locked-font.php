<?php

declare(strict_types=1);

// Deprecated API 1 compatibility example. New integrations should use DocumentEngine as shown in README.md.

use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\RenderException;
use Pliego\Php\RenderOptions;

require_once dirname(__DIR__).'/vendor/autoload.php';

$fontPath = $fontPath ?? ($argv[1] ?? '');
if (!is_string($fontPath) || !is_file($fontPath)) {
    fwrite(STDERR, "Usage: php examples/offline-locked-font.php /absolute/path/to/font.woff2\n");
    exit(2);
}
$binary = getenv('PLIEGO_BINARY');
$command = $pliegoCommand ?? [is_string($binary) && $binary !== '' ? $binary : 'pliego'];
$workRoot = $pliegoWorkRoot ?? sys_get_temp_dir().'/pliego-quickstart';
if (!is_dir($workRoot) && !mkdir($workRoot, 0700, true)) {
    throw new RuntimeException("cannot create {$workRoot}");
}
$job = rtrim($workRoot, '/\\').DIRECTORY_SEPARATOR.bin2hex(random_bytes(16));
mkdir($job, 0700);

try {
    $offlineResult = (new CliRenderer($command, $pliegoTimeoutSeconds ?? 60))->render(
        <<<'HTML'
        <!doctype html>
        <meta charset="utf-8">
        <style>
          @font-face {
            font-family: "Quickstart Locked";
            src: url("fonts/quickstart.woff2") format("woff2");
          }
          body { font-family: "Quickstart Locked", sans-serif; }
        </style>
        <p>This font is copied into the locked input bundle.</p>
        HTML,
        $job.'/input',
        $job.'/document.pdf',
        $job.'/artifacts',
        new RenderOptions(), // Network remains denied.
        ['fonts/quickstart.woff2' => $fontPath],
    );
    echo "PDF: {$offlineResult->pdfPath}\n";
    echo "Locked asset hash: {$offlineResult->inputBundlePath}/input-bundle.json\n";
    echo "Resource hashes: {$offlineResult->artifactsPath}/resources.jsonl\n";
} catch (RenderException $error) {
    fwrite(STDERR, "{$error->errorCode}: {$error->getMessage()}\n");
    fwrite(STDERR, "Input: {$error->inputBundlePath}\nArtifacts: {$error->artifactsPath}\n");
    exit(1);
} catch (Throwable $error) {
    fwrite(STDERR, "Host setup failed: {$error->getMessage()}\n");
    exit(1);
}
