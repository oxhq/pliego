<?php

declare(strict_types=1);

require dirname(__DIR__).'/vendor/autoload.php';

use Illuminate\Container\Container;
use Illuminate\Events\Dispatcher;
use Illuminate\Filesystem\Filesystem;
use Illuminate\View\Compilers\BladeCompiler;
use Illuminate\View\Engines\CompilerEngine;
use Illuminate\View\Engines\EngineResolver;
use Illuminate\View\Factory;
use Illuminate\View\FileViewFinder;
use Pliego\Laravel\DocumentFactory;
use Pliego\Php\DocumentEngine;
use Pliego\Php\RenderOptions;

function check(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

$root = sys_get_temp_dir().'/pliego-laravel-quickstart-'.bin2hex(random_bytes(8));
mkdir("{$root}/views", 0700, true);
mkdir("{$root}/cache", 0700, true);
file_put_contents("{$root}/views/invoice.blade.php", '<h1>Invoice {{ $number }}</h1>'."\n");
file_put_contents("{$root}/invoice.woff2", 'rights-cleared-font');

$files = new Filesystem;
$container = new Container;
$resolver = new EngineResolver;
$resolver->register('blade', fn () => new CompilerEngine(
    new BladeCompiler($files, "{$root}/cache"),
    $files,
));
$views = new Factory(
    $resolver,
    new FileViewFinder($files, ["{$root}/views"]),
    new Dispatcher($container),
);
$views->setContainer($container);

$result = (new DocumentFactory(
    $views,
    new DocumentEngine([PHP_BINARY, __DIR__.'/fake_api2.php'], "{$root}/jobs"),
    new RenderOptions,
))->view('invoice', ['number' => 42])
    ->denyNetwork()
    ->asset('assets/invoice.woff2', "{$root}/invoice.woff2")
    ->render();

$manifest = json_decode(
    (string) file_get_contents("{$result->runtimeJobPath}/input-manifest.json"),
    true,
    flags: JSON_THROW_ON_ERROR,
);
$assets = array_column($manifest['entries'], null, 'path');
check(str_starts_with($result->bytes(), '%PDF-1.7'), 'quickstart did not return a PDF');
check($result->metadata['request']['resources']['network'] === 'deny', 'network was not denied');
check(
    $assets['assets/invoice.woff2']['sha256'] === 'sha256:'.hash('sha256', 'rights-cleared-font'),
    'bundled font hash was not recorded',
);
check(basename($result->pdfPath) === 'document.pdf', 'API 2 engine-owned PDF path changed');
check(dirname($result->runtimeJobPath) === $result->jobPath, 'Laravel allocated an API 2 runtime path');

try {
    (new DocumentFactory(
        $views,
        new DocumentEngine([PHP_BINARY, __DIR__.'/fake_api2.php'], "{$root}/network-jobs"),
        new RenderOptions,
    ))->view('invoice', ['number' => 43])->allowHttpRoot('https://example.test/');
    throw new RuntimeException('API 2 live network convenience was silently accepted');
} catch (BadMethodCallException $error) {
    check(str_contains($error->getMessage(), 'prefetch'), 'live network migration error is not actionable');
}

echo "Pliego Laravel focused quickstart passed; evidence retained at {$root}\n";
