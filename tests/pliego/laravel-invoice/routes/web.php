<?php

use Illuminate\Support\Facades\Route;
use Pliego\Laravel\Facades\Document;

Route::get('/invoice.pdf', function () {
    return Document::view('invoice', ['rows' => range(1, 32)])
        ->locale('es-MX')
        ->timezone('PST8PDT')
        ->denyNetwork()
        ->asset(
            'assets/Ahem.ttf',
            base_path('../../../ports/pliego/tests/fixtures/text-scene/Ahem.ttf'),
        )
        ->download('invoice.pdf');
});
