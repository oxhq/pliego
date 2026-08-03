<?php

declare(strict_types=1);

use Pliego\Php\Experimental\Doctor;

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
$doctor = new Doctor([PHP_BINARY, __DIR__.'/fake_pliego.php'], 3);
$report = $doctor->run($root);
doctorExpect($report['version'] === '0.1.0', 'CLI version is reported');
doctorExpect($report['api_version'] === 1, 'Pliego API v1 is reported');
doctorExpect($report['platform'] !== '', 'platform is reported');
doctorExpect(str_starts_with((string) file_get_contents($report['smoke_pdf']), '%PDF-'), 'offline PDF smoke passes');
$manifest = json_decode(
    (string) file_get_contents(dirname($report['smoke_pdf']).'/input/input-bundle.json'),
    true,
    flags: JSON_THROW_ON_ERROR,
);
doctorExpect($manifest['environment']['network']['policy'] === 'deny', 'doctor smoke denies network');
doctorExpect(isset($manifest['assets']['assets/doctor.css']), 'doctor smoke bundles its style');
$font = dirname(__DIR__).'/resources/HasubiMono-Regular.woff2';
doctorExpect(is_file(dirname(__DIR__).'/resources/HasubiMono-OFL.txt'), 'bundled font retains its OFL license');
doctorExpect(
    ($manifest['assets']['assets/doctor.woff2']['sha256'] ?? null) === 'sha256:'.hash_file('sha256', $font),
    'doctor smoke bundles its licensed font',
);
$doctorCss = (string) file_get_contents(dirname($report['smoke_pdf']).'/input/assets/doctor.css');
doctorExpect(str_contains($doctorCss, 'url("doctor.woff2")'), 'doctor smoke selects its bundled font');
$command = json_decode(
    (string) file_get_contents(dirname($report['smoke_pdf']).'/artifacts/command.json'),
    true,
    flags: JSON_THROW_ON_ERROR,
);
doctorExpect(!isset($command['options']['--allow-http-root']), 'doctor smoke passes no network root');

putenv('PLIEGO_DOCTOR_FAKE_MODE=blank-pdf');
doctorFailure($doctor, $root, 'evidence is incomplete');
putenv('PLIEGO_DOCTOR_FAKE_MODE=host-font');
doctorFailure($doctor, $root, 'evidence is incomplete');
putenv('PLIEGO_DOCTOR_FAKE_MODE');

doctorFailure(new Doctor([$root.'/missing-pliego']), $root, 'not found');
$notExecutable = $root.'/not-executable.txt';
file_put_contents($notExecutable, "not executable\n");
doctorFailure(new Doctor([$notExecutable]), $root, 'not executable');

putenv('PLIEGO_DOCTOR_FAKE_MODE=incompatible');
doctorFailure($doctor, $root, 'compatible executable');
putenv('PLIEGO_DOCTOR_FAKE_MODE');

putenv('PLIEGO_DOCTOR_FAKE_MODE=api-missing');
doctorFailure($doctor, $root, 'requires Pliego API v1');
putenv('PLIEGO_DOCTOR_FAKE_MODE=api-mismatch');
doctorFailure($doctor, $root, 'unsupported Pliego API v2');
putenv('PLIEGO_DOCTOR_FAKE_MODE');

$filesystemRoot = DIRECTORY_SEPARATOR === '\\'
    ? substr((string) realpath($root), 0, 3)
    : DIRECTORY_SEPARATOR;
doctorFailure($doctor, $filesystemRoot, 'unsafe filesystem root');
$notDirectory = $root.'/not-a-directory';
file_put_contents($notDirectory, "file\n");
doctorFailure($doctor, $notDirectory, 'cannot be created');

echo "Pliego doctor focused self-test passed; evidence retained at {$root}\n";
