<?php

declare(strict_types=1);

// Deprecated API 1-only example. API 2 requires callers to prefetch and bundle every remote byte.

use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\RenderException;
use Pliego\Php\RenderOptions;

require_once dirname(__DIR__).'/vendor/autoload.php';

$binary = getenv('PLIEGO_BINARY');
$command = $pliegoCommand ?? [is_string($binary) && $binary !== '' ? $binary : 'pliego'];
$workRoot = $pliegoWorkRoot ?? sys_get_temp_dir().'/pliego-quickstart';
if (!is_dir($workRoot) && !mkdir($workRoot, 0700, true)) {
    throw new RuntimeException("cannot create {$workRoot}");
}
$job = rtrim($workRoot, '/\\').DIRECTORY_SEPARATOR.bin2hex(random_bytes(16));
mkdir($job, 0700);

try {
    $googleResult = (new CliRenderer($command, $pliegoTimeoutSeconds ?? 60))->render(
        <<<'HTML'
        <!doctype html>
        <meta charset="utf-8">
        <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;700&display=swap">
        <style>body { font-family: "Inter", sans-serif; }</style>
        <p>The Google Fonts link is passed to Pliego unchanged.</p>
        HTML,
        $job.'/input',
        $job.'/document.pdf',
        $job.'/artifacts',
        new RenderOptions(allowedHttpRoots: [
            'https://fonts.googleapis.com/',
            'https://fonts.gstatic.com/s/',
        ]),
    );
    echo "PDF: {$googleResult->pdfPath}\n";
    echo "Resource hashes: {$googleResult->artifactsPath}/resources.jsonl\n";
} catch (RenderException $error) {
    fwrite(STDERR, "{$error->errorCode}: {$error->getMessage()}\n");
    fwrite(STDERR, "Input: {$error->inputBundlePath}\nArtifacts: {$error->artifactsPath}\n");
    exit(1);
} catch (Throwable $error) {
    fwrite(STDERR, "Host setup failed: {$error->getMessage()}\n");
    exit(1);
}
