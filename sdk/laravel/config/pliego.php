<?php

declare(strict_types=1);

return [
    // Experimental one-shot binary; this is not a daemon endpoint.
    'binary' => env('PLIEGO_BINARY', 'pliego'),
    'timeout_seconds' => env('PLIEGO_TIMEOUT_SECONDS', 60),
    'work_dir' => env('PLIEGO_WORK_DIR', storage_path('app/pliego')),
    'locale' => env('PLIEGO_LOCALE', 'en-US'),
    'timezone' => env('PLIEGO_TIMEZONE', 'UTC'),
    'page_size' => env('PLIEGO_PAGE_SIZE', '612x792'),
    'page_margins' => env('PLIEGO_PAGE_MARGINS', '36,36,36,36'),
];
