<?php

declare(strict_types=1);

if (($argv[1] ?? null) === 'render') {
    $options = [];
    for ($index = 3; $index < count($argv); $index += 2) {
        $options[$argv[$index]][] = $argv[$index + 1] ?? null;
    }
    $output = $options['--output'][0] ?? '';
    $artifacts = $options['--artifacts'][0] ?? '';
    mkdir($artifacts, 0700, true);
    file_put_contents($output, "%PDF-1.7\n% fake quickstart\n");
    file_put_contents($artifacts.'/command.json', json_encode($options, JSON_PRETTY_PRINT)."\n");
    file_put_contents($artifacts.'/resources.jsonl', "{\"sha256\":\"sha256:fake\"}\n");
    echo json_encode(['status' => 'rendered'])."\n";
    exit(0);
}

function quickstartExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

$root = sys_get_temp_dir().'/pliego-resource-examples-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($root, 0700);
$fontPath = $root.'/fixture.woff2';
file_put_contents($fontPath, "fake woff2 bytes\n");
$pliegoCommand = [PHP_BINARY, __FILE__];
$pliegoTimeoutSeconds = 3;
$pliegoWorkRoot = $root.'/jobs';

ob_start();
require __DIR__.'/offline-locked-font.php';
ob_end_clean();
$offlineManifest = json_decode(
    (string) file_get_contents($offlineResult->inputBundlePath.'/input-bundle.json'),
    true,
    flags: JSON_THROW_ON_ERROR,
);
quickstartExpect($offlineManifest['environment']['network']['policy'] === 'deny', 'offline example denies network');
quickstartExpect(isset($offlineManifest['assets']['fonts/quickstart.woff2']['sha256']), 'offline font is explicitly hashed');
quickstartExpect(
    str_contains((string) file_get_contents($offlineResult->inputBundlePath.'/document.html'), 'url("fonts/quickstart.woff2")'),
    'offline HTML uses the declared font path',
);

ob_start();
require __DIR__.'/google-fonts.php';
ob_end_clean();
$googleManifest = json_decode(
    (string) file_get_contents($googleResult->inputBundlePath.'/input-bundle.json'),
    true,
    flags: JSON_THROW_ON_ERROR,
);
quickstartExpect($googleManifest['environment']['network'] === [
    'policy' => 'allow-roots',
    'roots' => ['https://fonts.googleapis.com/', 'https://fonts.gstatic.com/s/'],
], 'Google Fonts example allowlists both exact roots');
quickstartExpect(
    str_contains(
        (string) file_get_contents($googleResult->inputBundlePath.'/document.html'),
        '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;700&display=swap">',
    ),
    'Google Fonts link is retained unchanged',
);
quickstartExpect(
    !str_contains((string) file_get_contents($offlineResult->inputBundlePath.'/document.html'), 'window.pliego')
        && !str_contains((string) file_get_contents($googleResult->inputBundlePath.'/document.html'), 'window.pliego'),
    'static font examples use zero-config readiness',
);

echo "Pliego resource-mode examples self-check passed; no live network was used. Evidence: {$root}\n";
