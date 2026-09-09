<?php

declare(strict_types=1);

use Composer\InstalledVersions;
use Illuminate\Config\Repository;
use Illuminate\Filesystem\FilesystemAdapter;
use Illuminate\Filesystem\FilesystemServiceProvider;
use Illuminate\Foundation\Application;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\Facade;
use Illuminate\View\ViewServiceProvider;
use Pliego\Laravel\DocumentFactory;
use Pliego\Laravel\Exception\DocumentStorageException;
use Pliego\Laravel\PendingDocument;
use Pliego\Laravel\PliegoServiceProvider;
use Pliego\Laravel\StoredDocument;
use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\JobRetention;

// Usage: PLIEGO_TEST_AUTOLOAD=/consumer/vendor/autoload.php php this.php native-binary fresh-proof-directory
// Use an independently bounded runner. No application database, network, queue,
// native process, Blade service or successful filesystem write is faked here.
// Only the explicitly named false/throw disks inject a storage fault.
$autoload = getenv('PLIEGO_TEST_AUTOLOAD');
require is_string($autoload) && $autoload !== '' ? $autoload : dirname(__DIR__).'/vendor/autoload.php';

function nativeStorageExpect(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

function nativeStorageWrite(string $path, string $bytes): void
{
    nativeStorageExpect(file_put_contents($path, $bytes) === strlen($bytes), 'cannot retain '.$path);
}

function nativeStorageJson(string $path, array $value): void
{
    nativeStorageWrite($path, json_encode($value, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)."\n");
}

function nativeStorageSceneText(string $path): string
{
    $scene = json_decode((string) file_get_contents($path), true, flags: JSON_THROW_ON_ERROR);
    $text = '';
    foreach ($scene['pages'] as $page) {
        foreach ($page['operations'] as $operation) {
            if ($operation['type'] === 'text') {
                $text .= $operation['text'];
            }
        }
    }

    return $text;
}

final class NativeStorageFaultDisk extends FilesystemAdapter
{
    public array $attempts = [];

    public mixed $lastStream = null;

    public function __construct(private readonly string $mode)
    {
        nativeStorageExpect(in_array($mode, ['false', 'throw'], true), 'unknown injected fault');
    }

    public function writeStream($path, $resource, array $options = [])
    {
        $this->lastStream = $resource;
        nativeStorageExpect(is_resource($resource), 'store did not pass an open stream');
        $metadata = stream_get_meta_data($resource);
        nativeStorageExpect(str_starts_with((string) fread($resource, 5), '%PDF-'), 'fault disk received no PDF');
        nativeStorageExpect(rewind($resource), 'cannot rewind validation stream');
        $this->attempts[] = [
            'path' => $path,
            'options' => $options,
            'source_sha256' => hash_file('sha256', $metadata['uri']),
        ];
        if ($this->mode === 'throw') {
            throw new RuntimeException('injected storage write exception');
        }

        return false;
    }
}

$binary = isset($argv[1]) ? realpath($argv[1]) : false;
$requestedRoot = $argv[2] ?? '';
nativeStorageExpect(count($argv) === 3 && is_string($binary) && is_file($binary), 'supply native binary and fresh proof directory');
$magic = file_get_contents($binary, false, null, 0, 4);
nativeStorageExpect(is_string($magic) && (
    str_starts_with($magic, 'MZ') || $magic === "\x7fELF"
    || in_array($magic, ["\xfe\xed\xfa\xce", "\xce\xfa\xed\xfe", "\xfe\xed\xfa\xcf", "\xcf\xfa\xed\xfe", "\xca\xfe\xba\xbe", "\xbe\xba\xfe\xca"], true)
), 'executable must be native, not a script wrapper');
nativeStorageExpect(class_exists(Application::class), 'real laravel/framework is required');
nativeStorageExpect($requestedRoot !== '' && ! file_exists($requestedRoot) && ! is_link($requestedRoot), 'proof directory already exists');
nativeStorageExpect(is_dir(dirname($requestedRoot)) && mkdir($requestedRoot, 0700), 'proof parent must exist');
$root = realpath($requestedRoot);
nativeStorageExpect(is_string($root), 'cannot resolve proof directory');
$report = [
    'schema' => 'pliego.laravel-native-storage-proof.v1',
    'outcome' => 'running',
    'php' => PHP_VERSION,
    'framework' => Application::VERSION,
    'flysystem' => InstalledVersions::getPrettyVersion('league/flysystem'),
    'os' => PHP_OS_FAMILY,
    'binary' => $binary,
    'binary_sha256' => 'sha256:'.hash_file('sha256', $binary),
    'test_sha256' => hash_file('sha256', __FILE__),
    'sdk_sources' => [],
    'cases' => [],
    'boundary' => 'Real native API 2, real Laravel Blade/provider/local storage, byte readback, typed failure and recovery. Explicit injected false/throw storage faults and a real non-directory path obstruction. Not ACL denial, remote partial-write safety, queue/concurrency/cancellation, independent adoption or public package installation proof.',
];
foreach ([DocumentEngine::class, DocumentFactory::class, PendingDocument::class] as $class) {
    $path = (new ReflectionClass($class))->getFileName();
    $report['sdk_sources'][$class] = ['path' => $path, 'sha256' => hash_file('sha256', $path)];
}
nativeStorageJson($root.'/report.json', $report);

try {
    foreach (['bootstrap/cache', 'resources/views', 'storage/framework/views', 'storage/app/pdfs', 'records'] as $path) {
        nativeStorageExpect(mkdir($root.'/'.$path, 0700, true), 'cannot create '.$path);
    }
    $font = dirname((new ReflectionClass(DocumentEngine::class))->getFileName(), 2).'/resources/HasubiMono-Regular.woff2';
    nativeStorageExpect(is_file($font), 'packaged fixture font is missing');
    $prefix = '<!doctype html><meta charset="utf-8"><style>'
        .'@font-face{font-family:Proof;src:url("proof.woff2") format("woff2")}'
        .'body{font:12px Proof;margin:0}</style>';
    nativeStorageWrite($root.'/resources/views/invoice.blade.php', $prefix.'<p>PLIEGO STORAGE {{ $number }}</p>');
    nativeStorageWrite($root.'/resources/views/rejected.blade.php', $prefix.'<img src="missing.png" width="20" height="20">');
    nativeStorageWrite($root.'/storage/app/pdfs/blocked', 'existing file must remain unchanged');

    $app = new Application($root);
    $app->instance('request', Request::create('http://localhost/'));
    $app->instance('config', new Repository([
        'app' => ['name' => 'Pliego native storage proof', 'env' => 'testing', 'locale' => 'en'],
        'view' => ['paths' => [$root.'/resources/views'], 'compiled' => $root.'/storage/framework/views'],
        'filesystems' => ['default' => 'local', 'disks' => [
            'local' => ['driver' => 'local', 'root' => $root.'/storage/app/pdfs', 'throw' => true],
            'false' => ['driver' => 'proof-fault', 'mode' => 'false'],
            'throw' => ['driver' => 'proof-fault', 'mode' => 'throw'],
        ]],
        'pliego' => ['binary' => $binary, 'work_dir' => $root.'/jobs'],
    ]));
    Facade::setFacadeApplication($app);
    $app->register(FilesystemServiceProvider::class);
    $app->register(ViewServiceProvider::class);
    $app->register(PliegoServiceProvider::class);
    $app->boot();
    $app->make('filesystem')->extend('proof-fault', fn ($app, array $config) => new NativeStorageFaultDisk($config['mode']));
    $engine = $app->make(DocumentEngine::class);
    $report['contract'] = $engine->contract()->toArray();
    nativeStorageExpect($report['contract']['engine']['runtime']['binary_sha256'] === $report['binary_sha256'], 'native contract binary hash mismatch');
    $factory = $app->make(DocumentFactory::class);
    $store = static function (string $case, string $disk, string $path, string $view = 'invoice') use ($factory, $font, $root): StoredDocument {
        $pending = $factory->view($view, ['number' => $case])->asset('proof.woff2', $font);
        if ($case === 'initial') {
            $pending->pageSize('67351x47622au')->margins('2268,2268,5669,2268au');
        }
        $stored = $pending->store($path, $disk, ['visibility' => 'private']);
        // Application records are committed only after the actual store returns.
        nativeStorageJson($root.'/records/'.$case.'.json', ['disk' => $stored->disk, 'path' => $stored->path, 'delivery_identity' => $stored->renderResult->deliveryIdentity]);

        return $stored;
    };

    foreach (['initial', 'false', 'throw', 'blocked', 'rejected', 'recovery'] as $case) {
        $started = hrtime(true);
        $path = $case === 'blocked' ? 'blocked/invoice.pdf' : 'unusual path ü/'.$case.'.pdf';
        $disk = in_array($case, ['false', 'throw'], true) ? $case : 'local';
        $expectedFailure = in_array($case, ['false', 'throw', 'blocked', 'rejected'], true);
        $result = null;
        $error = null;
        try {
            $result = $store($case, $disk, $path, $case === 'rejected' ? 'rejected' : 'invoice');
        } catch (Throwable $caught) {
            $error = $caught;
        }
        $record = ['wall_ms' => (hrtime(true) - $started) / 1_000_000, 'path' => $path, 'disk' => $disk];
        if (! $expectedFailure) {
            nativeStorageExpect($error === null && $result instanceof StoredDocument, $case.' did not store: '.($error?->getMessage() ?? 'no result'));
            $pdf = $result->renderResult;
            $storedPath = $app->make('filesystem')->disk($disk)->path($path);
            nativeStorageExpect(hash_file('sha256', $storedPath) === hash_file('sha256', $pdf->pdfPath), 'stored bytes differ');
            nativeStorageExpect(nativeStorageSceneText($pdf->scenePath) === 'PLIEGO STORAGE '.$case, 'real Blade/scene text mismatch');
            if ($case === 'initial') {
                $scene = json_decode((string) file_get_contents($pdf->scenePath), true, flags: JSON_THROW_ON_ERROR);
                nativeStorageExpect(count($scene['pages']) === 1, 'landscape storage fixture changed page count');
                $page = $scene['pages'][0];
                nativeStorageExpect($page['style_source'] === 'request-defaults'
                    && $page['size_app_units'] === ['width' => 67_351, 'height' => 47_622]
                    && $page['margins_app_units'] === ['top' => 2_268, 'right' => 2_268, 'bottom' => 5_669, 'left' => 2_268],
                    'Laravel exact app-unit geometry differs from native scene authority');
                $record['exact_page'] = ['size_app_units' => $page['size_app_units'], 'margins_app_units' => $page['margins_app_units']];
            }
            nativeStorageExpect(trim((string) file_get_contents($pdf->jobPath.'/'.JobRetention::STATUS_FILE)) === 'success', 'success job state missing');
            $record += ['status' => 'stored', 'pdf_sha256' => hash_file('sha256', $storedPath), 'pdf_bytes' => filesize($storedPath), 'job_path' => $pdf->jobPath];
        } else {
            nativeStorageExpect($result === null && $error !== null, $case.' unexpectedly returned a stored document');
            nativeStorageExpect(! file_exists($root.'/records/'.$case.'.json'), 'failure committed a success record');
            nativeStorageExpect(! file_exists($root.'/storage/app/pdfs/'.$path), 'failure published a local PDF');
            $record += ['status' => 'expected_failure', 'exception' => $error::class, 'message' => $error->getMessage()];
            if ($case === 'rejected') {
                nativeStorageExpect($error instanceof RenderFailedException, 'missing resource did not retain typed native failure');
                nativeStorageExpect(is_dir($error->jobPath), 'render rejection lost job evidence');
                nativeStorageExpect($error->kind === 'resource', 'missing resource changed failure kind');
                nativeStorageExpect(trim((string) file_get_contents($error->jobPath.'/'.JobRetention::STATUS_FILE)) === 'failure', 'render rejection has no failed job state');
                nativeStorageExpect(! file_exists($error->runtimeJobPath.'/delivery/document.pdf'), 'rejected render left a PDF');
                nativeStorageExpect(! file_exists($error->runtimeJobPath.'/delivery/bundle.json'), 'rejected render left a bundle');
                nativeStorageJson($root.'/rejected-result.json', $error->result);
                $record += ['kind' => $error->kind, 'job_path' => $error->jobPath, 'native_pdf_absent' => true, 'native_bundle_absent' => true];
            } else {
                nativeStorageExpect($error instanceof DocumentStorageException, 'storage failure lost typed error');
                nativeStorageExpect($error->disk === $disk && $error->path === $path, 'storage error identity changed');
                nativeStorageExpect(is_file($error->renderResult->pdfPath), 'storage error lost validated native PDF');
                nativeStorageExpect(is_file($error->renderResult->bundlePath), 'storage error lost validated bundle');
                nativeStorageExpect(trim((string) file_get_contents($error->renderResult->jobPath.'/'.JobRetention::STATUS_FILE)) === 'success', 'storage failure changed successful native job state');
                nativeStorageExpect(nativeStorageSceneText($error->renderResult->scenePath) === 'PLIEGO STORAGE '.$case, 'storage fault did not follow real render');
                $record += ['job_path' => $error->renderResult->jobPath, 'pdf_sha256' => hash_file('sha256', $error->renderResult->pdfPath), 'cause' => $error->getPrevious()?->getMessage()];
                if ($disk !== 'local') {
                    $fault = $app->make('filesystem')->disk($disk);
                    nativeStorageExpect(count($fault->attempts) === 1 && ! is_resource($fault->lastStream), 'fault attempt count or stream closure mismatch');
                    nativeStorageExpect($fault->attempts[0]['source_sha256'] === $record['pdf_sha256'], 'fault disk received different bytes');
                    $expectedCause = $case === 'throw' ? 'injected storage write exception' : 'filesystem write returned false';
                    nativeStorageExpect($record['cause'] === $expectedCause, 'wrong injected storage failure');
                    $record['fault_attempts'] = $fault->attempts;
                }
            }
        }
        $report['cases'][$case] = $record;
        nativeStorageJson($root.'/report.json', $report);
    }
    nativeStorageExpect(file_get_contents($root.'/storage/app/pdfs/blocked') === 'existing file must remain unchanged', 'obstructing file was modified');
    nativeStorageExpect(count(glob($root.'/records/*.json')) === 2, 'success record denominator changed');
    $report['outcome'] = 'passed';
} catch (Throwable $error) {
    $report['outcome'] = 'failed';
    $report['failure'] = ['class' => $error::class, 'message' => $error->getMessage()];
}
nativeStorageJson($root.'/report.json', $report);
echo json_encode(['outcome' => $report['outcome'], 'report' => $root.'/report.json'], JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES).PHP_EOL;
exit($report['outcome'] === 'passed' ? 0 : 1);
