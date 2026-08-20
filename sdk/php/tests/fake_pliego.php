<?php

declare(strict_types=1);

$engineStartedAt = hrtime(true);
$engineTiming = static fn (int|float $totalMilliseconds): array => [
    'schema' => 'pliego.engine-timings',
    'version' => 1,
    'unit' => 'milliseconds',
    'measurement_boundary' => 'before_timings_artifact_write',
    'total_ms' => $totalMilliseconds,
];
$mode = getenv('PLIEGO_DOCTOR_FAKE_MODE');

if (($argv[1] ?? null) === '--contract-probe') {
    $api2Mode = getenv('PLIEGO_API2_FAKE_MODE') ?: 'empty';
    if ($api2Mode === 'slow-probe') {
        usleep(1_100_000);
    }
    $profiles = $api2Mode === 'profile'
        ? [['schema' => 'pliego.profile.test', 'version' => 1]]
        : [];
    $tuple = [
        'api' => 2,
        'input_manifest' => ['schema' => 'pliego.input-manifest', 'version' => 1],
        'request' => ['schema' => 'pliego.render-request', 'version' => 1],
        'result' => ['schema' => 'pliego.render-result', 'version' => 1],
        'document_scene' => ['schema' => 'pliego.document-scene', 'version' => 2],
        'bundle_manifest' => ['schema' => 'pliego.bundle-manifest', 'version' => 1],
        'profiles' => $profiles,
    ];
    $contract = [
        'schema' => 'pliego.runtime-contract',
        'version' => 1,
        'engine' => [
            'name' => 'pliego',
            'version' => '0.2.0-dev',
            'api' => 2,
            'source_commit' => str_repeat('1', 40),
            'runtime' => [
                'mode' => 'one-shot',
                'target' => 'x86_64-unknown-linux-gnu',
                'binary_sha256' => 'sha256:'.str_repeat('2', 64),
                'servo_base' => str_repeat('3', 40),
            ],
        ],
        'contracts' => in_array($api2Mode, ['available', 'profile'], true) ? [$tuple] : [],
        'invocation' => [
            'request_transport' => 'stdin-single-json',
            'request_max_bytes' => 1_048_576,
            'job_root_transport' => 'cwd-v1',
            'input_manifest_max_bytes' => 16_777_216,
            'input_content_max_bytes' => 67_108_864,
            'result_transport' => 'stdout-single-json',
            'invocation_error_transport' => 'stderr-utf8-line',
            'success_exit_code' => 0,
            'failed_exit_code' => 1,
            'invocation_error_exit_code' => 64,
        ],
    ];

    if ($api2Mode === 'out-of-order') {
        $contract = [
            'version' => $contract['version'],
            'schema' => $contract['schema'],
            'engine' => $contract['engine'],
            'contracts' => $contract['contracts'],
            'invocation' => $contract['invocation'],
        ];
    } elseif ($api2Mode === 'unknown-member') {
        $contract['build_path'] = 'C:/tmp/pliego.exe';
    }

    if ($api2Mode === 'exit-64') {
        fwrite(STDERR, "invalid probe invocation\n");
        exit(64);
    } elseif ($api2Mode === 'adversarial-stderr') {
        fwrite(STDERR, "first\r\nsecond\xFF".str_repeat('x', 300));
        exit(65);
    }
    fwrite(STDOUT, json_encode($contract, JSON_UNESCAPED_SLASHES)."\n");
    if ($api2Mode === 'stderr') {
        fwrite(STDERR, "unexpected diagnostic\n");
    } elseif ($api2Mode === 'second-frame') {
        fwrite(STDOUT, "{}\n");
    }
    exit(0);
}

if (($argv[1] ?? null) === '--version') {
    if ($mode === 'incompatible') {
        fwrite(STDERR, "wrong platform\n");
        exit(193);
    }
    $apiVersion = $mode === 'api-mismatch' ? "pliego-api 2\n" : "pliego-api 1\n";
    if ($mode === 'api-missing') {
        $apiVersion = '';
    }
    fwrite(STDOUT, "pliego 0.1.0\n{$apiVersion}ServoShell fake\nServo base fake\n");
    exit(0);
}

if (($argv[1] ?? null) !== 'render' || ($argv[2] ?? null) !== 'document.html') {
    fwrite(STDOUT, json_encode([
        'status' => 'failed',
        'error' => ['code' => 'INVALID_REQUEST', 'message' => 'unexpected fake command'],
    ])."\n");
    exit(2);
}

$options = [];
for ($index = 3; $index < count($argv); $index += 2) {
    $options[$argv[$index]][] = $argv[$index + 1] ?? null;
}
$html = file_get_contents('document.html');
if ($html === false) {
    fwrite(STDOUT, json_encode([
        'status' => 'failed',
        'error' => ['code' => 'INVALID_REQUEST', 'message' => 'missing rooted document'],
    ])."\n");
    exit(2);
}
if (str_contains($html, 'FAIL_ENGINE')) {
    fwrite(STDOUT, json_encode([
        'status' => 'failed',
        'error' => ['code' => 'RESOURCE_DENIED', 'message' => 'synthetic denial'],
        'engine_timings' => $engineTiming((hrtime(true) - $engineStartedAt) / 1_000_000),
    ])."\n");
    fwrite(STDERR, "pliego: RESOURCE_DENIED: synthetic denial\n");
    exit(1);
}
if (str_contains($html, 'FILL_STDERR')) {
    fwrite(STDERR, str_repeat('x', 1024 * 1024));
}

$output = $options['--output'][0] ?? null;
$artifacts = $options['--artifacts'][0] ?? null;
if (
    !is_string($output)
    || !is_string($artifacts)
    || (!is_file('assets/test.txt') && !is_file('assets/doctor.css'))
) {
    fwrite(STDOUT, json_encode([
        'status' => 'failed',
        'error' => ['code' => 'INVALID_REQUEST', 'message' => 'missing fake paths or asset'],
    ])."\n");
    exit(2);
}
if (str_contains($html, 'SLOW_RENDER')) {
    fwrite(STDERR, "SLOW_RENDER_STARTED\n");
    fflush(STDERR);
    sleep(5);
}

mkdir($artifacts, 0700, true);
if (str_contains($html, 'PARTIAL_CAPTURE')) {
    fwrite(STDOUT, json_encode([
        'status' => 'failed',
        'error' => [
            'code' => 'SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS',
            'message' => 'synthetic partial capture',
        ],
    ])."\n");
    exit(1);
}
$pdf = $mode === 'blank-pdf' ? "%PDF-1.7\n" : "%PDF-1.7\n% fake Pliego self-test\n";
file_put_contents($output, $pdf);
file_put_contents(
    "{$artifacts}/command.json",
    json_encode(['cwd' => getcwd(), 'options' => $options], JSON_PRETTY_PRINT)."\n",
);
file_put_contents("{$artifacts}/scene.json", "{}\n");
file_put_contents("{$artifacts}/fonts.json", json_encode([
    'schema' => 'pliego.font-report',
    'version' => 1,
    'selections' => [[
        'source' => $mode === 'host-font' ? 'host' : 'bundled',
        'requested_families' => ['Pliego Doctor'],
        'selected_family' => 'Pliego Doctor',
    ]],
], JSON_PRETTY_PRINT)."\n");
file_put_contents("{$artifacts}/pdf-structure.json", json_encode([
    'schema' => 'pliego.pdf-structure',
    'version' => 1,
    'pdf' => [
        'sha256' => 'sha256:'.hash('sha256', $pdf),
        'bytes' => strlen($pdf),
    ],
    'pages' => [[
        'expected_extracted_unicode' => $mode === 'blank-pdf' ? '' : 'Pliego doctor',
        'operation_counts' => ['text' => $mode === 'blank-pdf' ? 0 : 1],
    ]],
], JSON_PRETTY_PRINT)."\n");

$summary = [
    'status' => 'rendered',
    'engine' => 'pliego',
    'scene' => [
        'capture_status' => 'complete',
        'capture_code' => null,
    ],
    'document_pdf' => $output,
    'artifacts' => $artifacts,
    'scene_artifact' => "{$artifacts}/scene.json",
    'pdf_structure' => "{$artifacts}/pdf-structure.json",
];
if (str_contains($html, 'INVALID_ENGINE_TIMING_CONTRACT')) {
    $summary['engine_timings'] = $engineTiming((hrtime(true) - $engineStartedAt) / 1_000_000);
    $summary['engine_timings']['version'] = 2;
} elseif (str_contains($html, 'INVALID_ENGINE_TIMINGS')) {
    $summary['engine_timings'] = $engineTiming(-1);
} elseif (str_contains($html, 'OUT_OF_BOUND_ENGINE_TIMINGS')) {
    $summary['engine_timings'] = $engineTiming(60_000);
} elseif (str_contains($html, 'LEGACY_ENGINE_TIMINGS')) {
    $summary['phase_timings_ms'] = [
        'total_engine' => (hrtime(true) - $engineStartedAt) / 1_000_000,
    ];
} elseif (!str_contains($html, 'NO_ENGINE_TIMINGS')) {
    $summary['engine_timings'] = $engineTiming((hrtime(true) - $engineStartedAt) / 1_000_000);
}
fwrite(STDOUT, json_encode($summary)."\n");
