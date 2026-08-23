<?php

declare(strict_types=1);

use Pliego\Php\Doctor;

require dirname(__DIR__).'/vendor/autoload.php';

function doctorExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function doctorFailure(Doctor $doctor, string $root, string $message): void
{
    try {
        $doctor->run($root);
    } catch (RuntimeException $error) {
        doctorExpect(str_contains($error->getMessage(), $message), "actionable failure contains {$message}");

        return;
    }

    throw new RuntimeException("expected doctor failure: {$message}");
}

$root = sys_get_temp_dir().'/pliego-doctor-test-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($root, 0700);
$doctor = new Doctor([PHP_BINARY, __DIR__.'/fake_api2.php'], 3);

try {
    $report = $doctor->run($root);
    doctorExpect($report['version'] === '0.3.2', 'probed engine version is reported');
    doctorExpect($report['api_version'] === 2, 'Pliego API v2 is reported');
    doctorExpect($report['platform'] !== '', 'platform is reported');
    doctorExpect(str_starts_with((string) file_get_contents($report['smoke_pdf']), '%PDF-'), 'offline PDF smoke passes');
    doctorExpect(is_file($report['smoke_scene']), 'offline scene smoke passes');
    doctorExpect(is_file($report['smoke_bundle']), 'offline bundle smoke passes');
    doctorExpect(
        $report['delivery_identity'] === 'sha256:'.hash_file('sha256', $report['smoke_bundle']),
        'doctor reports the validated bundle identity',
    );
    $resultRoot = dirname(dirname($report['smoke_pdf']));
    $manifest = json_decode(
        (string) file_get_contents($resultRoot.'/input-manifest.json'),
        true,
        flags: JSON_THROW_ON_ERROR,
    );
    doctorExpect(
        array_column($manifest['entries'], 'path') === [
            'assets/doctor.css',
            'assets/doctor.woff2',
            'document.html',
        ],
        'doctor uses a canonical offline input closure',
    );
    doctorExpect(
        ($manifest['entries'][1]['sha256'] ?? null)
            === 'sha256:'.hash_file('sha256', dirname(__DIR__).'/resources/HasubiMono-Regular.woff2'),
        'doctor binds its licensed font bytes',
    );
    doctorExpect(
        (glob($root.'/.doctor-*') ?: []) === [],
        'doctor removes host-side staging without touching retained evidence',
    );

    putenv('PLIEGO_API2_RENDER_FAKE_MODE=failed');
    doctorFailure($doctor, $root, 'offline smoke failed');
    putenv('PLIEGO_API2_RENDER_FAKE_MODE=tamper-pdf');
    doctorFailure($doctor, $root, 'does not match retained bytes');
    putenv('PLIEGO_API2_RENDER_FAKE_MODE');

    putenv('PLIEGO_API2_PROBE_FAKE_MODE=empty');
    doctorFailure($doctor, $root, 'compatible API 2 contract');
    putenv('PLIEGO_API2_PROBE_FAKE_MODE=invalid');
    doctorFailure($doctor, $root, 'compatible API 2 contract');
    putenv('PLIEGO_API2_PROBE_FAKE_MODE');

    doctorFailure(new Doctor([$root.'/missing-pliego']), $root, 'not found');
    $notExecutable = $root.'/not-executable.txt';
    file_put_contents($notExecutable, "not executable\n");
    doctorFailure(new Doctor([$notExecutable]), $root, 'not executable');

    $filesystemRoot = DIRECTORY_SEPARATOR === '\\'
        ? substr((string) realpath($root), 0, 3)
        : DIRECTORY_SEPARATOR;
    doctorFailure($doctor, $filesystemRoot, 'filesystem root');
    $notDirectory = $root.'/not-a-directory';
    file_put_contents($notDirectory, "file\n");
    doctorFailure($doctor, $notDirectory, 'cannot create');
} finally {
    putenv('PLIEGO_API2_RENDER_FAKE_MODE');
    putenv('PLIEGO_API2_PROBE_FAKE_MODE');
}

echo "Pliego API 2 doctor focused self-test passed; evidence retained at {$root}\n";
