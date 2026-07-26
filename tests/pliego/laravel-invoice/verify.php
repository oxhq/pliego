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
$documentPath = "{$job}/input/document.html";
$fontPath = "{$job}/input/assets/Ahem.ttf";
$manifest = json_decode(
    (string) file_get_contents($manifestPath),
    true,
    flags: JSON_THROW_ON_ERROR,
);
$scene = json_decode((string) file_get_contents($scenePath), true, flags: JSON_THROW_ON_ERROR);
$structure = json_decode((string) file_get_contents($structurePath), true, flags: JSON_THROW_ON_ERROR);

$headers = ['ITEM', 'QTY', 'DESCRIPTION', 'AMOUNT'];
$expectedPages = [['INVOICE PLG-2026-001', ...$headers], $headers];
foreach (range(1, 32) as $row) {
    $expectedPages[$row <= 16 ? 0 : 1] = [
        ...$expectedPages[$row <= 16 ? 0 : 1],
        sprintf('INV-%03d', $row),
        '1',
        sprintf('SERVICE-%03d', $row),
        ($row * 10).'.00',
    ];
}
$expectedPages[1] = [...$expectedPages[1], 'TOTAL', '32', 'MXN', '5280.00'];

$actualPages = [];
foreach ($scene['pages'] ?? [] as $page) {
    $actualPages[] = array_values(array_map(
        static fn (array $operation): string => $operation['text'],
        array_filter(
            $page['operations'] ?? [],
            static fn (mixed $operation): bool => is_array($operation)
                && ($operation['type'] ?? null) === 'text'
                && is_string($operation['text'] ?? null),
        ),
    ));
}
$structureText = array_map(
    static fn (array $page): string => (string) ($page['expected_extracted_unicode'] ?? ''),
    $structure['pages'] ?? [],
);
$expectedText = array_map(static fn (array $page): string => implode('', $page), $expectedPages);

if (
    !str_starts_with((string) file_get_contents($pdf), '%PDF-')
    || ($scene['schema'] ?? null) !== 'pliego.document-scene'
    || ($scene['version'] ?? null) !== 1
    || $actualPages !== $expectedPages
    || ($structure['schema'] ?? null) !== 'pliego.pdf-structure'
    || ($structure['version'] ?? null) !== 1
    || ($structure['page_count'] ?? null) !== 2
    || count($structure['pages'] ?? []) !== 2
    || $structureText !== $expectedText
    || ($structure['pdf']['sha256'] ?? null) !== 'sha256:'.hash_file('sha256', $pdf)
    || ($structure['pdf']['bytes'] ?? null) !== filesize($pdf)
    || ($manifest['environment']['locale'] ?? null) !== 'es-MX'
    || ($manifest['environment']['timezone'] ?? null) !== 'PST8PDT'
    || ($manifest['environment']['network']['policy'] ?? null) !== 'deny'
    || ($manifest['document_sha256'] ?? null) !== 'sha256:'.hash_file('sha256', $documentPath)
    || ($manifest['assets']['assets/Ahem.ttf']['sha256'] ?? null) !== 'sha256:'.hash_file('sha256', $fontPath)
    || !is_string($disposition)
    || !str_contains($disposition, 'invoice.pdf')
    || $response->headers->get('Content-Type') !== 'application/pdf'
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
