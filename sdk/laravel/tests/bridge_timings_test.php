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
use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\RenderOptions;

$autoload = getenv('PLIEGO_TEST_AUTOLOAD');
require is_string($autoload) && $autoload !== ''
    ? $autoload
    : dirname(__DIR__).'/vendor/autoload.php';

function bridgeExpect(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

/** @param array<string, mixed> $timings */
function bridgeReconciles(array $timings): bool
{
    $sum = array_sum(array_filter($timings['phases_ms'], is_float(...)));

    return abs($sum - $timings['total_ms']) < 0.02
        && ($timings['schema'] ?? null) === 'pliego.php-bridge-timings'
        && ($timings['version'] ?? null) === 2
        && ($timings['measurement_boundary'] ?? null) === 'api2-render-invocation-before-timing-diagnostic';
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

$container = new Container;
$files = new Filesystem;
$resolver = new EngineResolver;
$compiler = new BladeCompiler($files, $cachePath);
$resolver->register('blade', static fn (): CompilerEngine => new CompilerEngine($compiler, $files));
$views = new ViewFactory(
    $resolver,
    new FileViewFinder($files, [$viewsPath]),
    new Dispatcher($container),
);
$views->setContainer($container);
$container->singleton(DocumentFactory::class, static function () use ($views, $workPath): DocumentFactory {
    $runtimeStartedAt = hrtime(true);
    $binary = realpath(PHP_BINARY);
    bridgeExpect(is_string($binary), 'PHP runtime resolves');

    return new DocumentFactory(
        $views,
        new DocumentEngine(
            [$binary, __DIR__.'/fake_api2.php'],
            $workPath,
            runtimeResolutionNanoseconds: (int) (hrtime(true) - $runtimeStartedAt),
        ),
        new RenderOptions,
    );
});
Facade::setFacadeApplication($container);
bridgeExpect(
    realpath((string) (new ReflectionClass(DocumentFactory::class))->getFileName())
        === realpath(dirname(__DIR__).'/src/DocumentFactory.php'),
    'proof uses this split package source',
);

$startedAt = hrtime(true);
$result = Document::view('invoice', ['title' => 'Invoice', 'rows' => ['A', 'B']])
    ->asset('assets/test.txt', $asset)
    ->render();
$wallMilliseconds = (hrtime(true) - $startedAt) / 1_000_000;
$timings = $result->bridgeTimings;
bridgeExpect(is_float($timings['phases_ms']['view_render']), 'Blade render is measured');
bridgeExpect(is_float($timings['setup_ms']['runtime_resolution']), 'runtime resolution is measured');
bridgeExpect($timings['setup_ms']['runtime_install'] === null, 'install is outside render');
bridgeExpect(bridgeReconciles($timings), 'Laravel phases reconcile');
$coldMilliseconds = $timings['total_ms'] + $timings['setup_ms']['runtime_resolution'];
bridgeExpect(
    $coldMilliseconds <= $wallMilliseconds + 1
        && $wallMilliseconds - $coldMilliseconds < 50,
    'render total plus cold setup is contained by fresh facade wall time',
);
bridgeExpect(str_contains((string) file_get_contents($result->inputBundlePath.'/document.html'), 'Invoice'), 'Blade output rendered');

$warm = Document::view('invoice', ['title' => 'Warm', 'rows' => []])
    ->asset('assets/test.txt', $asset)
    ->render();
bridgeExpect($warm->bridgeTimings['setup_ms']['runtime_resolution'] === 0.0, 'cached runtime costs zero');

try {
    Document::view('invoice', ['title' => 'FAIL_ENGINE', 'rows' => []])
        ->asset('assets/test.txt', $asset)
        ->render();
    throw new RuntimeException('expected typed Laravel failure');
} catch (RenderFailedException $error) {
    bridgeExpect($error->kind === 'resource', 'typed Laravel API 2 failure preserved');
    bridgeExpect(is_float($error->bridgeTimings['phases_ms']['view_render']), 'failed Blade render measured');
    bridgeExpect(bridgeReconciles($error->bridgeTimings), 'failed Laravel phases reconcile');
}

$runtimeSetupMilliseconds = $timings['setup_ms']['runtime_resolution'];
echo sprintf(
    "Pliego Laravel facade timing proof passed; wall=%.3fms render=%.3fms setup=%.3fms cold=%.3fms residual=%.3fms; evidence retained at %s\n",
    $wallMilliseconds,
    $timings['total_ms'],
    $runtimeSetupMilliseconds,
    $coldMilliseconds,
    abs($coldMilliseconds - $wallMilliseconds),
    $root,
);
