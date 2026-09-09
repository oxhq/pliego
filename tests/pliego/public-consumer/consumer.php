<?php

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

declare(strict_types=1);

// Public-dist API 2 consumer. No source SDK autoload or binary override.
use Composer\InstalledVersions;
use Illuminate\Config\Repository;
use Illuminate\Console\Application as Artisan;
use Illuminate\Filesystem\FilesystemServiceProvider;
use Illuminate\Foundation\Application;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\Facade;
use Illuminate\View\ViewServiceProvider;
use Pliego\Laravel\DocumentFactory;
use Pliego\Laravel\ManagedRuntime;
use Pliego\Laravel\PliegoServiceProvider;
use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\JobRetention;
use Symfony\Component\Console\Output\BufferedOutput;

function demand(bool $ok, string $message): void
{
    if (! $ok) {
        throw new RuntimeException($message);
    }
}
function retain(string $path, array $value): void
{
    $bytes = json_encode($value, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)."\n";
    demand(file_put_contents($path, $bytes) === strlen($bytes), 'Cannot retain '.$path);
}

demand(count($argv) === 4, 'Usage: php consumer.php application-directory stage-name public-identity.json');
$appRoot = realpath($argv[1]);
$stage = $argv[2];
demand(is_string($appRoot) && is_dir($appRoot) && ! is_link($argv[1]), 'Application must exist without a path alias');
demand(in_array($stage, ['initial', 'upgrade', 'rollback', 'fresh-12'], true), 'Unknown stage');
$expected = json_decode((string) file_get_contents($argv[3]), true, flags: JSON_THROW_ON_ERROR);
$version = in_array($stage, ['initial', 'rollback'], true) ? '0.3.3' : '0.4.0';
demand(($expected['schema'] ?? null) === 'pliego.final-public-package-identity.v1'
    && ($expected['version'] ?? null) === $version, 'Wrong independently verified public stage identity');
demand(($expected['application'] ?? null) === $appRoot, 'Public identity belongs to another application');
foreach (['PLIEGO_BINARY', 'PLIEGO_TEST_AUTOLOAD', 'COMPOSER_AUTH', 'GH_TOKEN', 'GITHUB_TOKEN'] as $name) {
    demand(! getenv($name), 'Clear '.$name.' before a public-only proof');
}
$proof = $appRoot.'/evidence/'.$stage;
demand(! file_exists($proof), 'Stage already exists; preserve prior evidence');
demand(mkdir($proof, 0700, true), 'Cannot create fresh stage evidence');
$report = ['schema' => 'pliego.final-public-consumer.v1', 'outcome' => 'running',
    'stage' => $stage, 'version' => $version, 'php' => PHP_VERSION, 'os' => PHP_OS_FAMILY,
    'driver_sha256' => hash_file('sha256', __FILE__), 'public_identity_sha256' => hash_file('sha256', $argv[3]),
    'boundary' => 'Public dist SDKs and managed native install; common API 2 Blade/local storage/rejection/rollback proof. Not benchmark, independent adoption, remote storage or cancellation proof.'];
retain($proof.'/report.json', $report);
try {
    require $appRoot.'/vendor/autoload.php';
    foreach ([DocumentEngine::class => 'pliego-php/src/DocumentEngine.php',
        ManagedRuntime::class => 'pliego-laravel/src/ManagedRuntime.php',
        DocumentFactory::class => 'pliego-laravel/src/DocumentFactory.php',
        PliegoServiceProvider::class => 'pliego-laravel/src/PliegoServiceProvider.php'] as $class => $relative) {
        $path = (new ReflectionClass($class))->getFileName();
        demand($path === realpath($appRoot.'/vendor/oxhq/'.$relative), 'Non-public class origin: '.$class);
        $report['sdk_classes'][$class] = ['path' => $path, 'sha256' => hash_file('sha256', $path)];
    }
    foreach (['pliego-php', 'pliego-laravel'] as $package) {
        demand(ltrim((string) InstalledVersions::getPrettyVersion('oxhq/'.$package), 'v') === $version, 'Wrong installed SDK version');
        demand(InstalledVersions::getReference('oxhq/'.$package) === $expected['packages'][$package]['reference'], 'Wrong installed SDK reference');
        demand(trim((string) file_get_contents($appRoot.'/vendor/oxhq/'.$package.'/VERSION')) === $version, 'Wrong packaged VERSION');
    }
    $manifestPath = $appRoot.'/vendor/oxhq/pliego-laravel/resources/runtimes.json';
    demand(hash_file('sha256', $manifestPath) === $expected['runtime_manifest_sha256'], 'Packaged native manifest differs from public release');
    foreach (['bootstrap/cache', 'resources/views', 'storage/framework/views', 'storage/app/pdfs', 'records'] as $path) {
        if (! is_dir($appRoot.'/'.$path)) {
            demand(mkdir($appRoot.'/'.$path, 0700, true), 'Cannot initialize '.$path);
        }
    }
    $font = $appRoot.'/vendor/oxhq/pliego-php/resources/HasubiMono-Regular.woff2';
    $prefix = '<!doctype html><meta charset="utf-8"><style>@font-face{font-family:Proof;src:url("proof.woff2") format("woff2")}body{font:12px Proof;margin:0}</style>';
    $view = $prefix.'<p>PLIEGO PUBLIC {{ $stage }} 450.00</p>';
    demand(file_put_contents($appRoot.'/resources/views/proof.blade.php', $view) === strlen($view), 'Cannot write synthetic view');
    $rejected = $prefix.'<img src="absent.png" width="20" height="20">';
    demand(file_put_contents($appRoot.'/resources/views/rejected.blade.php', $rejected) === strlen($rejected), 'Cannot write rejected view');
    $app = new Application($appRoot);
    $app->instance('request', Request::create('http://localhost/'));
    $app->instance('config', new Repository([
        'app' => ['name' => 'Public Pliego consumer', 'env' => 'testing', 'locale' => 'en'],
        'view' => ['paths' => [$appRoot.'/resources/views'], 'compiled' => $appRoot.'/storage/framework/views'],
        'filesystems' => ['default' => 'local', 'disks' => ['local' => ['driver' => 'local',
            'root' => $appRoot.'/storage/app/pdfs', 'throw' => true]]],
        'pliego' => ['binary' => null, 'runtime_dir' => $appRoot.'/storage/app/pliego-runtime',
            'work_dir' => $proof.'/jobs', 'timeout_seconds' => 65],
    ]));
    Facade::setFacadeApplication($app);
    $app->register(FilesystemServiceProvider::class);
    $app->register(ViewServiceProvider::class);
    $app->register(PliegoServiceProvider::class);
    $app->boot();
    $artisan = new Artisan($app, $app->make('events'), Application::VERSION);
    // Executes the real registered commands, including download/hash/install and
    // the real offline API 2 doctor. No HTTP fake or injected version probe.
    foreach (['pliego:install', 'pliego:doctor'] as $command) {
        $output = new BufferedOutput;
        $code = $artisan->call($command, [], $output);
        $text = $output->fetch();
        file_put_contents($proof.'/'.str_replace(':', '-', $command).'.txt', $text);
        demand($code === 0, $command.' failed: '.$text);
    }
    $runtime = $app->make(ManagedRuntime::class);
    $binary = realpath($runtime->binary());
    demand(is_string($binary) && str_contains(str_replace('\\', '/', $binary), '/'.$version.'/'), 'Wrong managed runtime version directory');
    $engine = $app->make(DocumentEngine::class);
    $contract = $engine->contract()->toArray();
    $identity = $contract['engine'];
    demand($identity['api'] === 2 && $identity['version'] === $version
        && $identity['source_commit'] === $expected['native_source_commit'], 'Wrong native contract source/version');
    demand($identity['runtime']['binary_sha256'] === 'sha256:'.hash_file('sha256', $binary), 'Wrong native byte identity');
    $report += ['framework' => Application::VERSION, 'binary' => $binary,
        'binary_sha256' => hash_file('sha256', $binary), 'contract' => $contract];
    retain($proof.'/report.json', $report);
    $factory = $app->make(DocumentFactory::class);
    $stored = $factory->view('proof', ['stage' => $stage])->asset('proof.woff2', $font)
        ->store('unusual path ü/'.$stage.'.pdf', 'local');
    $path = $app->make('filesystem')->disk('local')->path($stored->path);
    demand(hash_file('sha256', $path) === hash_file('sha256', $stored->renderResult->pdfPath), 'Stored bytes differ');
    $scene = json_decode((string) file_get_contents($stored->renderResult->scenePath), true, flags: JSON_THROW_ON_ERROR);
    $text = '';
    foreach ($scene['pages'] as $page) {
        foreach ($page['operations'] as $operation) {
            if ($operation['type'] === 'text') {
                $text .= $operation['text'];
            }
        }
    }
    demand(count($scene['pages']) === 1 && $text === 'PLIEGO PUBLIC '.$stage.' 450.00', 'Wrong native scene content');
    $record = ['path' => $path, 'pdf_sha256' => hash_file('sha256', $path), 'bytes' => filesize($path),
        'scene' => $stored->renderResult->scenePath, 'bundle' => $stored->renderResult->bundlePath,
        'delivery_identity' => $stored->renderResult->deliveryIdentity];
    retain($appRoot.'/records/'.$stage.'.json', $record);
    $report['stored'] = $record;
    try {
        $factory->view('rejected')->asset('proof.woff2', $font)->store('rejected-'.$stage.'.pdf');
        throw new RuntimeException('Missing resource was presented as success');
    } catch (RenderFailedException $error) {
        demand($error->kind === 'resource', 'Wrong rejection kind');
        demand(! is_file($appRoot.'/storage/app/pdfs/rejected-'.$stage.'.pdf'), 'Rejected PDF was stored');
        demand(! is_file($error->runtimeJobPath.'/delivery/document.pdf')
            && ! is_file($error->runtimeJobPath.'/delivery/bundle.json'), 'Rejected render published delivery');
        demand(trim((string) file_get_contents($error->jobPath.'/'.JobRetention::STATUS_FILE)) === 'failure', 'Missing failed-job state');
        retain($proof.'/rejected-result.json', $error->result);
        $report['rejection'] = ['exception' => $error::class, 'kind' => $error->kind,
            'job' => $error->jobPath, 'success_delivery_absent' => true];
    }
    // Stored application PDFs must survive SDK/native upgrades and rollback.
    $required = match ($stage) {
        'initial' => ['initial'], 'upgrade' => ['initial', 'upgrade'],
        'rollback' => ['initial', 'upgrade', 'rollback'], default => ['fresh-12'],
    };
    foreach ($required as $previous) {
        $old = json_decode((string) file_get_contents($appRoot.'/records/'.$previous.'.json'), true, flags: JSON_THROW_ON_ERROR);
        demand(hash_file('sha256', $old['path']) === $old['pdf_sha256'], 'Stored document lost through transition: '.$previous);
    }
    if ($stage === 'rollback') {
        $initial = json_decode((string) file_get_contents($appRoot.'/evidence/initial/report.json'), true, flags: JSON_THROW_ON_ERROR);
        $upgraded = json_decode((string) file_get_contents($appRoot.'/evidence/upgrade/report.json'), true, flags: JSON_THROW_ON_ERROR);
        demand($initial['binary_sha256'] === $report['binary_sha256']
            && $initial['binary'] === $report['binary'], 'Rollback did not select the original installed native bytes');
        demand($upgraded['version'] === '0.4.0' && $upgraded['binary_sha256'] !== $report['binary_sha256'], 'Upgrade did not exercise distinct native bytes');
        demand(file_get_contents($appRoot.'/composer.lock') === file_get_contents($appRoot.'/rollback-lock/composer.lock'), 'Rollback lock differs from original bytes');
        $report['original_lock_and_native_restored'] = true;
    }
    $report['persistent_documents_verified'] = $required;
    $report['outcome'] = 'passed';
    retain($proof.'/report.json', $report);
    echo json_encode($report, JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES)."\n";
} catch (Throwable $error) {
    $report['outcome'] = 'failed';
    $report['error'] = ['class' => $error::class, 'message' => $error->getMessage()];
    retain($proof.'/report.json', $report);
    throw $error;
}
