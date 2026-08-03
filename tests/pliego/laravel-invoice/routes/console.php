<?php

use Illuminate\Support\Facades\Artisan;
use Pliego\Laravel\Experimental\Facades\Document;

require_once base_path('rehearsal.php');

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

Artisan::command('pliego:rehearsal-job
    {run}
    {sequence}
    {scenario}
    {--report=}
    {--work-root=}
    {--offline-font=}
    {--offline-font-sha256=}
    {--css-url=}
    {--css-sha256=}
    {--font-url=}
    {--font-sha256=}', function (): int {
    return PliegoQueueRehearsal::runJob($this);
})->purpose('Run one internal job in the six-job Pliego queue rehearsal');

Artisan::command('pliego:rehearse-queue
    {--release-version=0.1.0-alpha.1}
    {--connection=}
    {--report=}
    {--binary-sha256=}
    {--offline-font=}
    {--offline-font-sha256=}
    {--css-url=}
    {--css-sha256=}
    {--font-url=}
    {--font-sha256=}
    {--self-test}', function (): int {
    return PliegoQueueRehearsal::run($this);
})->purpose('Drain the focused six-job Pliego production rehearsal with one queue worker');
