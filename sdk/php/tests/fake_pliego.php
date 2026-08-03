<?php

declare(strict_types=1);

if (($argv[1] ?? null) === '--version') {
    $mode = getenv('PLIEGO_DOCTOR_FAKE_MODE');
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
    file_put_contents($output, "%PDF-1.7\n% incomplete\n");
    fwrite(STDERR, "SLOW_RENDER_STARTED\n");
    fflush(STDERR);
    sleep(5);
}

mkdir($artifacts, 0700, true);
file_put_contents($output, "%PDF-1.7\n% fake Pliego self-test\n");
file_put_contents(
    "{$artifacts}/command.json",
    json_encode(['cwd' => getcwd(), 'options' => $options], JSON_PRETTY_PRINT)."\n",
);
file_put_contents("{$artifacts}/scene.json", "{}\n");
file_put_contents("{$artifacts}/pdf-structure.json", "{}\n");

fwrite(STDOUT, json_encode([
    'status' => 'rendered',
    'engine' => 'pliego',
    'document_pdf' => $output,
    'artifacts' => $artifacts,
    'scene_artifact' => "{$artifacts}/scene.json",
    'pdf_structure' => "{$artifacts}/pdf-structure.json",
])."\n");
