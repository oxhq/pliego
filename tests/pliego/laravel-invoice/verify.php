<?php

declare(strict_types=1);

use Illuminate\Contracts\Console\Kernel;
use Illuminate\Foundation\Application;
use Pliego\Laravel\Experimental\Facades\Document;
use Symfony\Component\HttpFoundation\BinaryFileResponse;

require __DIR__.'/vendor/autoload.php';

/** @var Application $app */
$app = require __DIR__.'/bootstrap/app.php';
$app->make(Kernel::class)->bootstrap();

$response = Document::view('invoice', ['rows' => range(1, 32)])
    ->pageSize('612x792')
    ->margins('36,36,36,36')
    ->locale('es-MX')
    ->timezone('PST8PDT')
    ->denyNetwork()
    ->asset(
        'assets/Ahem.ttf',
        base_path('../../../ports/pliego/tests/fixtures/text-scene/Ahem.ttf'),
    )
    ->download('invoice.pdf');

if (!$response instanceof BinaryFileResponse || $response->getStatusCode() !== 200) {
    throw new RuntimeException('Laravel did not return a successful PDF download');
}

$pdf = $response->getFile()->getPathname();
$disposition = $response->headers->get('Content-Disposition');
$job = dirname($pdf);
$manifestPath = "{$job}/input/input-bundle.json";
$scenePath = "{$job}/artifacts/scene.json";
$structurePath = "{$job}/artifacts/pdf-structure.json";
$manifest = json_decode(
    (string) file_get_contents($manifestPath),
    true,
    flags: JSON_THROW_ON_ERROR,
);

if (
    !str_starts_with((string) file_get_contents($pdf), '%PDF-')
    || !is_file($scenePath)
    || !is_file($structurePath)
    || ($manifest['environment']['locale'] ?? null) !== 'es-MX'
    || ($manifest['environment']['timezone'] ?? null) !== 'PST8PDT'
    || ($manifest['environment']['network']['policy'] ?? null) !== 'deny'
    || !isset($manifest['assets']['assets/Ahem.ttf']['sha256'])
    || !is_string($disposition)
    || !str_contains($disposition, 'invoice.pdf')
) {
    throw new RuntimeException('retained Laravel render evidence is incomplete');
}

echo json_encode([
    'status' => 'verified',
    'pdf' => $pdf,
    'input_bundle' => "{$job}/input",
    'scene' => $scenePath,
    'pdf_structure' => $structurePath,
    'content_disposition' => $disposition,
], JSON_UNESCAPED_SLASHES)."\n";
