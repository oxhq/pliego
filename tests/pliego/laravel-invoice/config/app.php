<?php

return [
    'name' => env('APP_NAME', 'Pliego Laravel invoice fixture'),
    'env' => env('APP_ENV', 'testing'),
    'debug' => (bool) env('APP_DEBUG', true),
    'url' => env('APP_URL', 'http://localhost'),
    'timezone' => 'UTC',
    'locale' => 'en',
    'fallback_locale' => 'en',
    'cipher' => 'AES-256-CBC',
    'key' => env('APP_KEY'),
    'previous_keys' => [],
    'maintenance' => ['driver' => 'file', 'store' => null],
];
