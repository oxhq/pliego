<?php

declare(strict_types=1);

use Composer\InstalledVersions;
use Illuminate\Config\Repository;
use Illuminate\Console\Application as Artisan;
use Illuminate\Filesystem\FilesystemServiceProvider;
use Illuminate\Foundation\Application;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\Facade;
use Illuminate\View\ViewServiceProvider;
use Pliego\Laravel\DocumentFactory;
use Pliego\Laravel\Facades\Document;
use Pliego\Laravel\ManagedRuntime;
use Pliego\Laravel\PliegoServiceProvider;
use Pliego\Php\DocumentEngine;
use Symfony\Component\HttpFoundation\BinaryFileResponse;

// Run against an isolated Composer consumer that requires laravel/framework.
// The fake native process proves framework integration, not PDF rendering fidelity.
$autoload = getenv('PLIEGO_TEST_AUTOLOAD');
require is_string($autoload) && $autoload !== ''
    ? $autoload
    : dirname(__DIR__).'/vendor/autoload.php';

if (! class_exists(Application::class)) {
    throw new RuntimeException('This test requires laravel/framework in an isolated Composer consumer');
}

$assertions = 0;
function frameworkExpect(bool $condition, string $message): void
{
    global $assertions;
    $assertions++;
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

$root = sys_get_temp_dir().'/pliego-framework-'.getmypid().'-'.bin2hex(random_bytes(4));
foreach (['bootstrap/cache', 'resources/views', 'storage/framework/views', 'storage/app/pdfs'] as $path) {
    mkdir($root.'/'.$path, 0700, true);
}
file_put_contents(
    $root.'/resources/views/invoice.blade.php',
    '<h1>Invoice {{ $number }}</h1><p>{{ $customer }}</p>',
);

$app = new Application($root);
$app->instance('request', Request::create('http://localhost/'));
$app->instance('config', new Repository([
    'app' => ['name' => 'Pliego framework proof', 'env' => 'testing', 'locale' => 'en'],
    'view' => [
        'paths' => [$root.'/resources/views'],
        'compiled' => $root.'/storage/framework/views',
    ],
    'filesystems' => [
        'default' => 'local',
        'disks' => ['local' => [
            'driver' => 'local',
            'root' => $root.'/storage/app/pdfs',
            'throw' => true,
        ]],
    ],
    'pliego' => ['binary' => PHP_BINARY, 'work_dir' => $root.'/jobs'],
]));
Facade::setFacadeApplication($app);
$app->register(FilesystemServiceProvider::class);
$app->register(ViewServiceProvider::class);
$app->register(PliegoServiceProvider::class);
$app->boot();

frameworkExpect($app->make(ManagedRuntime::class)->binary() === PHP_BINARY, 'provider lost binary override');
frameworkExpect($app['config']['pliego.timeout_seconds'] === 65, 'provider did not merge package defaults');
frameworkExpect($app->make(DocumentEngine::class) instanceof DocumentEngine, 'provider did not resolve engine');
frameworkExpect($app->make(DocumentEngine::class) === $app->make(DocumentEngine::class), 'engine is not a singleton');

// Keep the registered factory/facade and real Blade/filesystem/response services.
// Only the native process is replaced, just as in the existing focused SDK tests.
$app->instance(DocumentEngine::class, new DocumentEngine(
    [PHP_BINARY, __DIR__.'/fake_api2.php'],
    $root.'/jobs',
));
$factory = $app->make(DocumentFactory::class);
frameworkExpect($factory === $app->make(DocumentFactory::class), 'document factory is not a singleton');

$stored = Document::view('invoice', ['number' => 42, 'customer' => 'Synthetic Customer'])
    ->store('invoices/42.pdf');
$storedPath = $app->make('filesystem')->disk('local')->path('invoices/42.pdf');
frameworkExpect($stored->disk === 'local', 'configured storage disk changed');
frameworkExpect(is_file($storedPath), 'real local filesystem did not store the PDF');
frameworkExpect(
    hash_file('sha256', $storedPath) === hash_file('sha256', $stored->renderResult->pdfPath),
    'stored PDF differs from validated render bytes',
);
frameworkExpect(
    str_contains((string) file_get_contents($stored->renderResult->inputBundlePath.'/document.html'), 'Invoice 42'),
    'real Blade did not interpolate the invoice data',
);

$download = Document::view('invoice', ['number' => 43, 'customer' => 'Synthetic Customer'])
    ->download('invoice-43.pdf');
frameworkExpect($download instanceof BinaryFileResponse, 'download is not a Symfony file response');
frameworkExpect($download->headers->get('Content-Type') === 'application/pdf', 'download media type changed');
frameworkExpect(
    str_contains((string) $download->headers->get('Content-Disposition'), 'invoice-43.pdf'),
    'download filename was not forwarded',
);

$artisan = new Artisan($app, $app->make('events'), Application::VERSION);
foreach (['pliego:install', 'pliego:doctor', 'pliego:prune'] as $command) {
    frameworkExpect($artisan->has($command), 'provider did not register '.$command);
}

echo json_encode([
    'schema' => 'pliego.laravel-framework-compatibility.v1',
    'outcome' => 'passed',
    'assertions' => $assertions,
    'framework' => Application::VERSION,
    'symfony_http_foundation' => InstalledVersions::getPrettyVersion('symfony/http-foundation'),
    'php' => PHP_VERSION,
    'native' => 'fake-api2-process',
    'evidence' => $root,
], JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES).PHP_EOL;
