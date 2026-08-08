<?php

declare(strict_types=1);

use Illuminate\Container\Container;
use Illuminate\Events\Dispatcher;
use Illuminate\Filesystem\Filesystem;
use Illuminate\Support\Facades\Facade;
use Illuminate\View\Compilers\BladeCompiler;
use Illuminate\View\Engines\CompilerEngine;
use Illuminate\View\Engines\EngineResolver;
use Illuminate\View\Factory as ViewFactory;
use Illuminate\View\FileViewFinder;
use Pliego\Laravel\DocumentFactory;
use Pliego\Laravel\Facades\Document;
use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\EngineRenderException;
use Pliego\Php\RenderOptions;

require dirname(__DIR__).'/vendor/autoload.php';
$localPhpAutoload = dirname(__DIR__, 2).'/php/vendor/autoload.php';
if (is_file($localPhpAutoload)) {
    require $localPhpAutoload;
}

function bridgeExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

/** @param array<string, mixed> $timings */
function bridgeReconciles(array $timings): bool
{
    $sum = array_sum(array_filter($timings['phases_ms'], is_float(...)));

    return abs($sum - $timings['total_ms']) < 0.02
        && is_float($timings['native_engine_ms'])
        && abs($timings['native_engine_ms'] + $timings['bridge_overhead_ms'] - $timings['total_ms']) < 0.002;
}

$root = sys_get_temp_dir().'/pliego-laravel-timings-'.getmypid().'-'.bin2hex(random_bytes(4));
$viewsPath = "{$root}/views";
$cachePath = "{$root}/cache";
$workPath = "{$root}/jobs";
mkdir($viewsPath, 0700, true);
mkdir($cachePath, 0700, true);
$asset = "{$root}/source.txt";
file_put_contents($asset, "asset\n");
file_put_contents(
    "{$viewsPath}/invoice.blade.php",
    '<h1>{{ $title }}</h1>@foreach ($rows as $row)<p>{{ $row }}</p>@endforeach',
);

$container = new Container();
$files = new Filesystem();
$resolver = new EngineResolver();
$compiler = new BladeCompiler($files, $cachePath);
$resolver->register('blade', static fn (): CompilerEngine => new CompilerEngine($compiler, $files));
$views = new ViewFactory(
    $resolver,
    new FileViewFinder($files, [$viewsPath]),
    new Dispatcher($container),
);
$views->setContainer($container);
$fake = dirname(__DIR__, 2).'/php/tests/fake_pliego.php';
$container->singleton(DocumentFactory::class, static function () use ($views, $workPath, $fake): DocumentFactory {
    $runtimeStartedAt = hrtime(true);
    $binary = realpath(PHP_BINARY);
    bridgeExpect(is_string($binary), 'PHP runtime resolves');

    return new DocumentFactory(
        $views,
        new CliRenderer(
            [$binary, $fake],
            runtimeResolutionNanoseconds: hrtime(true) - $runtimeStartedAt,
        ),
        $workPath,
        new RenderOptions(),
    );
});
Facade::setFacadeApplication($container);
if (is_file($localPhpAutoload)) {
    bridgeExpect(
        realpath((string) (new ReflectionClass(CliRenderer::class))->getFileName())
            === realpath(dirname(__DIR__, 2).'/php/src/CliRenderer.php'),
        'proof uses local PHP bridge source',
    );
}

$startedAt = hrtime(true);
$result = Document::view('invoice', ['title' => 'Invoice', 'rows' => ['A', 'B']])
    ->asset('assets/test.txt', $asset)
    ->render();
$wallMilliseconds = (hrtime(true) - $startedAt) / 1_000_000;
$timings = $result->bridgeTimings;
bridgeExpect(is_float($timings['phases_ms']['view_render']), 'Blade render is measured');
bridgeExpect(is_float($timings['phases_ms']['runtime_resolution']), 'runtime resolution is measured');
bridgeExpect($timings['phases_ms']['runtime_install'] === null, 'install is outside render');
bridgeExpect(bridgeReconciles($timings), 'Laravel phases reconcile');
bridgeExpect(abs($timings['total_ms'] - $wallMilliseconds) < 5, 'timings reconcile to facade wall time');
bridgeExpect(str_contains((string) file_get_contents($result->inputBundlePath.'/document.html'), 'Invoice'), 'Blade output rendered');

$warm = Document::view('invoice', ['title' => 'Warm', 'rows' => []])
    ->asset('assets/test.txt', $asset)
    ->render();
bridgeExpect($warm->bridgeTimings['phases_ms']['runtime_resolution'] === 0.0, 'cached runtime costs zero');

try {
    Document::view('invoice', ['title' => 'FAIL_ENGINE', 'rows' => []])
        ->asset('assets/test.txt', $asset)
        ->render();
    throw new RuntimeException('expected typed Laravel failure');
} catch (EngineRenderException $error) {
    bridgeExpect($error->errorCode === 'RESOURCE_DENIED', 'typed Laravel failure preserved');
    bridgeExpect(is_float($error->bridgeTimings['phases_ms']['view_render']), 'failed Blade render measured');
    bridgeExpect(bridgeReconciles($error->bridgeTimings), 'failed Laravel phases reconcile');
}

echo sprintf(
    "Pliego Laravel facade timing proof passed; wall=%.3fms bridge=%.3fms delta=%.3fms; evidence retained at %s\n",
    $wallMilliseconds,
    $timings['total_ms'],
    abs($timings['total_ms'] - $wallMilliseconds),
    $root,
);
