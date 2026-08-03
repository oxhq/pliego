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
$command = json_decode(
    (string) file_get_contents(dirname($report['smoke_pdf']).'/artifacts/command.json'),
    true,
    flags: JSON_THROW_ON_ERROR,
);
doctorExpect(!isset($command['options']['--allow-http-root']), 'doctor smoke passes no network root');

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
