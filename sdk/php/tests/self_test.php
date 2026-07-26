<?php

declare(strict_types=1);

use Pliego\Php\Experimental\CliRenderer;
use Pliego\Php\Experimental\Exception\EngineRenderException;
use Pliego\Php\Experimental\RenderOptions;

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
        pageSize: '612x792',
        pageMargins: '36,36,36,36',
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
expect(!isset($command['options']['--allow-http-root']), 'deny mode adds no network roots');

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

echo "Pliego PHP experimental bridge self-test passed; evidence retained at {$root}\n";
