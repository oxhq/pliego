<?php

use Illuminate\Support\Facades\Artisan;
use Pliego\Laravel\Experimental\Facades\Document;

Artisan::command('pliego:invoice', function (): int {
    $result = Document::view('invoice', ['rows' => range(1, 32)])
        ->pageSize('612x792')
        ->margins('36,36,36,36')
        ->locale('es-MX')
        ->timezone('PST8PDT')
        ->denyNetwork()
        ->asset(
            'assets/Ahem.ttf',
            base_path('../../../ports/pliego/tests/fixtures/text-scene/Ahem.ttf'),
        )
        ->render('invoice.pdf');

    $this->line(json_encode([
        'status' => 'rendered',
        'pdf' => $result->pdfPath,
        'artifacts' => $result->artifactsPath,
        'input_bundle' => $result->inputBundlePath,
        'scene' => $result->metadata['scene_artifact'] ?? null,
        'pdf_structure' => $result->metadata['pdf_structure'] ?? null,
    ], JSON_UNESCAPED_SLASHES));

    return 0;
})->purpose('Render the pinned synthetic invoice with Pliego');
