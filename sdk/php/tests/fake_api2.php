<?php

declare(strict_types=1);

/** @return array<string, mixed> */
function fakeApi2Engine(): array
{
    return [
        'name' => 'pliego',
        'version' => '0.3.0',
        'api' => 2,
        'source_commit' => str_repeat('1', 40),
        'runtime' => [
            'mode' => 'one-shot',
            'target' => 'x86_64-unknown-linux-gnu',
            'binary_sha256' => 'sha256:'.str_repeat('2', 64),
            'servo_base' => str_repeat('3', 40),
        ],
    ];
}

/** @param array<string, mixed> $value */
function fakeApi2Json(array $value): string
{
    return json_encode($value, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n";
}

/** @return array{path: string, media_type: string, sha256: string, bytes: int} */
function fakeApi2Descriptor(string $root, string $path, string $mediaType): array
{
    $file = $root.DIRECTORY_SEPARATOR.str_replace('/', DIRECTORY_SEPARATOR, $path);
    $bytes = filesize($file);
    $sha256 = hash_file('sha256', $file);
    if (!is_int($bytes) || !is_string($sha256)) {
        throw new RuntimeException("cannot describe fake API 2 artifact {$file}");
    }

    return [
        'path' => $path,
        'media_type' => $mediaType,
        'sha256' => 'sha256:'.$sha256,
        'bytes' => $bytes,
    ];
}

if (($argv[1] ?? null) === '--contract-probe') {
    $probeMode = getenv('PLIEGO_API2_PROBE_FAKE_MODE') ?: 'available';
    if ($probeMode === 'invalid') {
        fwrite(STDOUT, "not json\n");
        exit(0);
    }
    fwrite(STDOUT, fakeApi2Json([
        'schema' => 'pliego.runtime-contract',
        'version' => 1,
        'engine' => fakeApi2Engine(),
        'contracts' => $probeMode === 'empty' ? [] : [[
            'api' => 2,
            'input_manifest' => ['schema' => 'pliego.input-manifest', 'version' => 1],
            'request' => ['schema' => 'pliego.render-request', 'version' => 1],
            'result' => ['schema' => 'pliego.render-result', 'version' => 1],
            'document_scene' => ['schema' => 'pliego.document-scene', 'version' => 2],
            'bundle_manifest' => ['schema' => 'pliego.bundle-manifest', 'version' => 1],
            'profiles' => $probeMode === 'profile'
                ? [['schema' => 'pliego.profile.test', 'version' => 1]]
                : [],
        ]],
        'invocation' => [
            'request_transport' => 'stdin-single-json',
            'request_max_bytes' => 1_048_576,
            'job_root_transport' => 'cwd-v1',
            'input_manifest_max_bytes' => 16_777_216,
            'input_content_max_bytes' => 67_108_864,
            'result_transport' => 'stdout-single-json',
            'invocation_error_transport' => 'stderr-utf8-line',
            'transport_error_transport' => 'stderr-utf8-line',
            'success_exit_code' => 0,
            'failed_exit_code' => 1,
            'invocation_error_exit_code' => 64,
            'transport_error_exit_code' => 74,
        ],
    ]));
    exit(0);
}

if (($argv[1] ?? null) !== 'render-api2' || count($argv) !== 2) {
    fwrite(STDERR, "invalid fake API 2 command\n");
    exit(64);
}

$mode = getenv('PLIEGO_API2_RENDER_FAKE_MODE') ?: 'success';
if ($mode === 'timeout') {
    sleep(3);
}
if ($mode === 'exit64') {
    fwrite(STDERR, "invalid API 2 fixture request\n");
    exit(64);
}
if ($mode === 'exit74') {
    fwrite(STDOUT, '{"schema":"pliego.render-');
    fwrite(STDERR, "pliego: API2_TRANSPORT_ERROR: fixture terminal write failed\n");
    exit(74);
}
if ($mode === 'malformed-exit74') {
    fwrite(STDERR, 'transport diagnostic without newline');
    exit(74);
}

$root = getcwd();
$stdin = stream_get_contents(STDIN);
$request = is_string($stdin) ? json_decode(trim($stdin), true) : null;
$topLevel = array_values(array_filter(
    scandir($root) ?: [],
    static fn (string $entry): bool => $entry !== '.' && $entry !== '..',
));
sort($topLevel, SORT_STRING);
if (
    !is_array($request)
    || $topLevel !== ['input', 'input-manifest.json']
    || isset($request['settlement']['limits']['post_readiness_resources'])
    || isset($request['settlement']['limits']['process_cpu_ms'])
    || !isset($request['settlement']['limits']['host_wall_ms'])
) {
    fwrite(STDERR, "invalid canonical API 2 fixture request\n");
    exit(64);
}

$manifestPath = $root.DIRECTORY_SEPARATOR.'input-manifest.json';
$manifestBytes = file_get_contents($manifestPath);
if (
    !is_string($manifestBytes)
    || ($request['input']['manifest']['bytes'] ?? null) !== strlen($manifestBytes)
    || ($request['input']['manifest']['sha256'] ?? null) !== 'sha256:'.hash('sha256', $manifestBytes)
) {
    fwrite(STDERR, "input manifest descriptor mismatch\n");
    exit(64);
}

$failed = $mode === 'failed';
$retention = $request['diagnostics']['retention'] ?? 'always';
$retainDiagnostics = $retention === 'always' || ($retention === 'on-failure' && $failed);
$diagnostics = ['retained' => false, 'artifacts' => []];
if ($retainDiagnostics) {
    $diagnosticsRoot = $root.DIRECTORY_SEPARATOR.'diagnostics';
    mkdir($diagnosticsRoot, 0700);
    $diagnosticName = $failed ? 'failure.json' : 'environment.json';
    file_put_contents(
        $diagnosticsRoot.DIRECTORY_SEPARATOR.$diagnosticName,
        fakeApi2Json($failed ? ['kind' => 'resource'] : ['runtime' => 'fake-api2']),
    );
    $diagnostics = [
        'retained' => true,
        'artifacts' => [fakeApi2Descriptor(
            $root,
            'diagnostics/'.$diagnosticName,
            'application/json',
        )],
    ];
}

$delivery = null;
if (!$failed) {
    $deliveryRoot = $root.DIRECTORY_SEPARATOR.'delivery';
    mkdir($deliveryRoot, 0700);
    file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'document.pdf', "%PDF-1.7\n% fake api2\n");
    $requestedSize = $request['page']['size'];
    $pageSize = isset($requestedSize['name'])
        ? ['width' => 47_622, 'height' => 67_351]
        : [
            'width' => $requestedSize['width_app_units'],
            'height' => $requestedSize['height_app_units'],
        ];
    file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'scene.json', fakeApi2Json([
        'schema' => 'pliego.document-scene',
        'version' => 2,
        'app_units_per_css_px' => 60,
        'request_page' => $request['page'],
        'semantic_layer' => null,
        'pages' => [[
            'number' => 1,
            'style_source' => 'request-defaults',
            'size_app_units' => $pageSize,
            'margins_app_units' => $request['page']['margins_app_units'],
            'operations' => [],
        ]],
    ]));
    $pdf = fakeApi2Descriptor($deliveryRoot, 'document.pdf', 'application/pdf');
    $scene = fakeApi2Descriptor(
        $deliveryRoot,
        'scene.json',
        'application/vnd.pliego.document-scene+json',
    );
    file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'bundle.json', fakeApi2Json([
        'schema' => 'pliego.bundle-manifest',
        'version' => 1,
        'entries' => [$pdf, $scene],
    ]));
    $bundle = fakeApi2Descriptor(
        $deliveryRoot,
        'bundle.json',
        'application/vnd.pliego.bundle-manifest+json',
    );
    $delivery = ['pdf' => $pdf, 'scene' => $scene, 'bundle' => $bundle];

    if ($mode === 'tamper-pdf') {
        file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'document.pdf', 'tampered', FILE_APPEND);
    } elseif ($mode === 'tamper-bundle') {
        file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'bundle.json', "\n", FILE_APPEND);
    } elseif ($mode === 'extra-delivery') {
        file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'extra.txt', 'unmanifested');
    }
}

$engine = fakeApi2Engine();
if ($mode === 'engine-mismatch') {
    $engine['version'] = '0.3.1';
}
$result = [
    'schema' => 'pliego.render-result',
    'version' => 1,
    'api' => 2,
    'status' => $failed ? 'failed' : 'success',
    'request' => $request,
    'engine' => $engine,
    'delivery' => $delivery,
    'conformance' => [
        'requested' => null,
        'status' => 'not-requested',
        'evidence' => null,
    ],
    'diagnostics' => $diagnostics,
    'error' => $failed ? ['kind' => 'resource'] : null,
];
if ($mode === 'unknown-result-member') {
    $result['pid'] = getmypid();
}

if ($mode === 'noncanonical-result') {
    fwrite(STDOUT, json_encode($result, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)."\n");
} else {
    fwrite(STDOUT, fakeApi2Json($result));
}
if ($mode === 'stderr-success') {
    fwrite(STDERR, "unexpected render diagnostic\n");
}
exit($mode === 'wrong-exit' ? 2 : ($failed ? 1 : 0));
