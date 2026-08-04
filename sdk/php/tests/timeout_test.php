<?php

declare(strict_types=1);

use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\EngineRenderException;
use Pliego\Php\JobRetention;

require dirname(__DIR__).'/vendor/autoload.php';

function expect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

$root = sys_get_temp_dir().'/pliego-php-timeout-test-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($root, 0700);
$asset = "{$root}/source.txt";
file_put_contents($asset, "asset\n");
$renderer = new CliRenderer([PHP_BINARY, __DIR__.'/fake_pliego.php'], timeoutSeconds: 1);

$started = hrtime(true);
try {
    $renderer->render(
        'SLOW_RENDER',
        "{$root}/slow-input",
        "{$root}/slow.pdf",
        "{$root}/slow-artifacts",
        assets: ['assets/test.txt' => $asset],
    );
    throw new RuntimeException('expected the render timeout');
} catch (EngineRenderException $error) {
    expect($error->errorCode === 'RENDER_TIMEOUT', 'timeout has a typed error code');
    expect($error->jobPath === $root, 'timeout exposes the retained job');
    expect($error->inputBundlePath === "{$root}/slow-input", 'timeout exposes the retained input');
    expect($error->artifactsPath === "{$root}/slow-artifacts", 'timeout exposes retained artifacts');
    expect(trim((string) file_get_contents("{$root}/".JobRetention::STATUS_FILE)) === 'failure', 'timeout is marked failed');
    expect(str_contains($error->stderr, 'SLOW_RENDER_STARTED'), 'fake engine started before timeout');
    expect((hrtime(true) - $started) / 1_000_000_000 < 3, 'timeout is wall-clock bounded');
    expect(!is_file("{$root}/slow.pdf"), 'timed-out render publishes no PDF');
}

$recovered = $renderer->render(
    '<p>recovered</p>',
    "{$root}/recovered-input",
    "{$root}/recovered.pdf",
    "{$root}/recovered-artifacts",
    assets: ['assets/test.txt' => $asset],
);
expect(str_starts_with($recovered->bytes(), '%PDF-1.7'), 'the next render succeeds');

echo "Pliego PHP timeout recovery self-test passed; evidence retained at {$root}\n";
