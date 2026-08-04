<?php

declare(strict_types=1);

use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\EngineRenderException;
use Pliego\Php\Exception\InvalidRequestException;
use Pliego\Php\RenderOptions;

require dirname(__DIR__).'/vendor/autoload.php';

function expect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

$root = sys_get_temp_dir().'/pliego-php-self-test-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($root, 0700);
$asset = "{$root}/source.txt";
file_put_contents($asset, "rooted asset\n");

$renderer = new CliRenderer([PHP_BINARY, __DIR__.'/fake_pliego.php']);
$result = $renderer->render(
    '<!doctype html><p>invoice</p>',
    "{$root}/input",
    "{$root}/invoice.pdf",
    "{$root}/artifacts",
    new RenderOptions(
        locale: 'es-MX',
        timezone: 'PST8PDT',
    ),
    ['assets/test.txt' => $asset],
);

expect(str_starts_with($result->bytes(), '%PDF-1.7'), 'rendered PDF is readable');
$manifest = json_decode(
    (string) file_get_contents("{$root}/input/input-bundle.json"),
    true,
    flags: JSON_THROW_ON_ERROR,
);
expect($manifest['environment']['network']['policy'] === 'deny', 'network deny is explicit');
expect($manifest['environment']['locale'] === 'es-MX', 'locale is retained');
expect(
    $manifest['assets']['assets/test.txt']['sha256'] === 'sha256:'.hash_file('sha256', $asset),
    'asset hash is retained',
);
$command = json_decode(
    (string) file_get_contents("{$root}/artifacts/command.json"),
    true,
    flags: JSON_THROW_ON_ERROR,
);
expect($command['cwd'] === realpath("{$root}/input"), 'engine runs inside the input root');
expect($command['options']['--timezone'] === ['PST8PDT'], 'timezone reaches the CLI');
expect($command['options']['--page-size'] === ['816x1056'], 'default page size is US Letter');
expect($command['options']['--page-margins'] === ['48,48,48,48'], 'default margins are half an inch');
expect(!isset($command['options']['--allow-http-root']), 'deny mode adds no network roots');

$protected = "{$root}/protected.pdf";
file_put_contents($protected, "caller-owned\n");
try {
    $renderer->render(
        '<p>must not replace</p>',
        "{$root}/protected-input",
        $protected,
        "{$root}/protected-artifacts",
        assets: ['assets/test.txt' => $asset],
    );
    throw new RuntimeException('expected existing output rejection');
} catch (InvalidArgumentException) {
}
expect(file_get_contents($protected) === "caller-owned\n", 'existing output is preserved');

try {
    $renderer->render(
        '<p>PARTIAL_CAPTURE</p>',
        "{$root}/partial-input",
        "{$root}/partial.pdf",
        "{$root}/partial-artifacts",
        assets: ['assets/test.txt' => $asset],
    );
    throw new RuntimeException('expected partial scene capture to fail');
} catch (EngineRenderException $error) {
    expect(
        $error->errorCode === 'SCENE_CAPTURE_UNSUPPORTED_PAINT_EVENTS',
        'partial scene has a typed error',
    );
    expect(!is_file("{$root}/partial.pdf"), 'partial scene PDF is not published');
}

$allowed = $renderer->render(
    '<p>FILL_STDERR</p>',
    "{$root}/allowed-input",
    "{$root}/allowed.pdf",
    "{$root}/allowed-artifacts",
    new RenderOptions(allowedHttpRoots: ['https://example.test/assets']),
    ['assets/test.txt' => $asset],
);
expect(str_starts_with($allowed->bytes(), '%PDF-1.7'), 'large stderr cannot deadlock success');
$allowedManifest = json_decode(
    (string) file_get_contents("{$root}/allowed-input/input-bundle.json"),
    true,
    flags: JSON_THROW_ON_ERROR,
);
expect(
    $allowedManifest['environment']['network']['roots'] === ['https://example.test/assets/'],
    'HTTP root is normalized in retained input',
);
$allowedCommand = json_decode(
    (string) file_get_contents("{$root}/allowed-artifacts/command.json"),
    true,
    flags: JSON_THROW_ON_ERROR,
);
expect(
    $allowedCommand['options']['--allow-http-root'] === ['https://example.test/assets/'],
    'normalized HTTP root reaches the CLI',
);

foreach ([
    'https://user:secret@example.test/assets/',
    'https://example.test/assets/?token=secret',
    'https://example.test/assets/#secret',
    'https:///missing-host/',
] as $invalidRoot) {
    try {
        new RenderOptions(allowedHttpRoots: [$invalidRoot]);
        throw new RuntimeException('expected the unsafe HTTP root to fail');
    } catch (InvalidArgumentException $error) {
        expect(str_contains($error->getMessage(), 'HTTP roots'), 'unsafe HTTP root is rejected');
    }
}

try {
    $renderer->render(
        'FAIL_ENGINE',
        "{$root}/failed-input",
        "{$root}/failed.pdf",
        "{$root}/failed-artifacts",
        assets: ['assets/test.txt' => $asset],
    );
    throw new RuntimeException('expected the engine failure');
} catch (EngineRenderException $error) {
    expect($error->errorCode === 'RESOURCE_DENIED', 'engine code is mapped');
    expect($error->exitCode === 1, 'engine exit code is mapped');
    expect(str_contains($error->stderr, 'RESOURCE_DENIED'), 'engine stderr is retained');
}

try {
    $renderer->render(
        '<p>invalid request</p>',
        "{$root}/invalid-input",
        "{$root}/invalid.pdf",
        "{$root}/invalid-artifacts",
    );
    throw new RuntimeException('expected the invalid request');
} catch (InvalidRequestException $error) {
    expect($error->errorCode === 'INVALID_REQUEST', 'invalid request code is mapped');
    expect($error->exitCode === 2, 'invalid request exit code is mapped');
}

try {
    $renderer->render(
        '<p>unsafe</p>',
        "{$root}/unsafe-input",
        "{$root}/unsafe.pdf",
        "{$root}/unsafe-artifacts",
        assets: ['../escape.txt' => $asset],
    );
    throw new RuntimeException('expected the unsafe asset path to fail');
} catch (InvalidArgumentException $error) {
    expect(str_contains($error->getMessage(), 'unsafe bundle asset path'), 'path escape is rejected');
}

foreach (['document.html', 'INPUT-BUNDLE.JSON'] as $index => $reserved) {
    try {
        $renderer->render(
            '<p>reserved</p>',
            "{$root}/reserved-input-{$index}",
            "{$root}/reserved-{$index}.pdf",
            "{$root}/reserved-artifacts-{$index}",
            assets: [$reserved => $asset],
        );
        throw new RuntimeException('expected the reserved asset path to fail');
    } catch (InvalidArgumentException $error) {
        expect(str_contains($error->getMessage(), 'reserved'), 'reserved bundle file is protected');
    }
}

try {
    $renderer->render(
        '<p>duplicate</p>',
        "{$root}/duplicate-input",
        "{$root}/duplicate.pdf",
        "{$root}/duplicate-artifacts",
        assets: ['assets\same.txt' => $asset, 'assets/same.txt' => $asset],
    );
    throw new RuntimeException('expected the normalized duplicate asset path to fail');
} catch (InvalidArgumentException $error) {
    expect(str_contains($error->getMessage(), 'duplicate'), 'portable asset collision is rejected');
}

foreach (['document.html::$DATA', 'document.html.', 'NUL', 'assets/aux.txt'] as $index => $unsafe) {
    try {
        $renderer->render(
            '<p>portable path</p>',
            "{$root}/portable-input-{$index}",
            "{$root}/portable-{$index}.pdf",
            "{$root}/portable-artifacts-{$index}",
            assets: [$unsafe => $asset],
        );
        throw new RuntimeException('expected the nonportable asset path to fail');
    } catch (InvalidArgumentException $error) {
        expect(str_contains($error->getMessage(), 'unsafe'), 'nonportable bundle path is rejected');
    }
}

echo "Pliego PHP bridge self-test passed; evidence retained at {$root}\n";
