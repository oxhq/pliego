<?php

declare(strict_types=1);

return [
    // One-shot binary path; this is not a daemon endpoint.
    'binary' => env('PLIEGO_BINARY'),
    'runtime_dir' => env('PLIEGO_RUNTIME_DIR', storage_path('app/pliego-runtime')),
    'timeout_seconds' => env('PLIEGO_TIMEOUT_SECONDS', 60),
    'success_retention_seconds' => env('PLIEGO_SUCCESS_RETENTION_SECONDS', 86400),
    'failure_retention_seconds' => env('PLIEGO_FAILURE_RETENTION_SECONDS', 604800),
    'work_dir' => env('PLIEGO_WORK_DIR', storage_path('app/pliego')),
    'locale' => env('PLIEGO_LOCALE', 'en-US'),
    'timezone' => env('PLIEGO_TIMEZONE', 'UTC'),
    'page_size' => env('PLIEGO_PAGE_SIZE', '816x1056'),
    'page_margins' => env('PLIEGO_PAGE_MARGINS', '48,48,48,48'),
];
