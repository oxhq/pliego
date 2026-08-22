<?php

declare(strict_types=1);

require dirname(__DIR__).'/vendor/autoload.php';

use Illuminate\Container\Container;
use Illuminate\Contracts\Filesystem\Factory as FilesystemFactory;
use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Events\Dispatcher;
use Illuminate\Filesystem\Filesystem;
use Illuminate\Filesystem\FilesystemAdapter;
use Illuminate\Filesystem\FilesystemManager;
use Illuminate\Support\Arr;
use Illuminate\Support\Facades\Facade;
use Illuminate\Support\Facades\Storage;
use Illuminate\Testing\ParallelTesting;
use Illuminate\View\Compilers\BladeCompiler;
use Illuminate\View\Engines\CompilerEngine;
use Illuminate\View\Engines\EngineResolver;
use Illuminate\View\Factory;
use Illuminate\View\FileViewFinder;
use Pliego\Laravel\DocumentFactory;
use Pliego\Laravel\Exception\DocumentStorageException;
use Pliego\Laravel\StoredDocument;
use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\RenderOptions;

final class StorageTestApplication extends Container
{
    public function __construct(private readonly string $storageRoot) {}

    public function storagePath(string $path = ''): string
    {
        return $this->storageRoot.($path === '' ? '' : DIRECTORY_SEPARATOR.$path);
    }
}

if (! function_exists('storage_path')) {
    function storage_path(string $path = ''): string
    {
        $app = Container::getInstance();
        if (! $app instanceof StorageTestApplication) {
            throw new RuntimeException('storage test application is not configured');
        }

        return $app->storagePath($path);
    }
}

/** @implements ArrayAccess<string, mixed> */
final class StorageTestConfig implements ArrayAccess
{
    /** @param array<string, mixed> $items */
    public function __construct(private array $items) {}

    public function get(string $key, mixed $default = null): mixed
    {
        return Arr::get($this->items, $key, $default);
    }

    public function offsetExists(mixed $offset): bool
    {
        return is_string($offset) && Arr::has($this->items, $offset);
    }

    public function offsetGet(mixed $offset): mixed
    {
        return is_string($offset) ? $this->get($offset) : null;
    }

    public function offsetSet(mixed $offset, mixed $value): void
    {
        if (! is_string($offset)) {
            throw new InvalidArgumentException('test config keys must be strings');
        }
        Arr::set($this->items, $offset, $value);
    }

    public function offsetUnset(mixed $offset): void
    {
        if (is_string($offset)) {
            Arr::forget($this->items, $offset);
        }
    }
}

final class StorageWriteFailureDisk extends FilesystemAdapter
{
    public bool $writeAttempted = false;

    public string $writtenPath = '';

    /** @var array<string, mixed> */
    public array $writtenOptions = [];

    /** @var resource|null */
    public mixed $sourceStream = null;

    public function __construct(private readonly string $failureMode) {}

    /** @param resource $resource */
    public function writeStream($path, $resource, array $options = [])
    {
        $this->writeAttempted = true;
        $this->writtenPath = (string) $path;
        $this->writtenOptions = $options;
        $this->sourceStream = $resource;

        if ($this->failureMode === 'throw') {
            throw new RuntimeException('synthetic throwing storage write');
        }

        return false;
    }
}

final class StorageWriteFailureFactory implements FilesystemFactory
{
    public mixed $requestedDisk = null;

    public function __construct(public readonly StorageWriteFailureDisk $storage) {}

    public function disk($name = null): StorageWriteFailureDisk
    {
        $this->requestedDisk = $name;

        return $this->storage;
    }
}

final readonly class StorageQueuedConsumer implements ShouldQueue
{
    public function __construct(
        public int $invoiceNumber,
        public string $path,
        public string $disk,
    ) {}

    public function handle(DocumentFactory $documents): StoredDocument
    {
        return $documents->view('invoice', ['number' => $this->invoiceNumber])->store(
            $this->path,
            $this->disk,
            ['visibility' => 'private'],
        );
    }
}

function storageExpect(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

function removeStorageFixture(string $path): void
{
    if (! file_exists($path)) {
        return;
    }
    if (is_file($path) || is_link($path)) {
        unlink($path);

        return;
    }

    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($path, FilesystemIterator::SKIP_DOTS),
        RecursiveIteratorIterator::CHILD_FIRST,
    );
    foreach ($iterator as $entry) {
        $entry->isDir() && ! $entry->isLink()
            ? rmdir($entry->getPathname())
            : unlink($entry->getPathname());
    }
    rmdir($path);
}

function storageDocumentFactory(
    string $root,
    FilesystemFactory $filesystems,
    string $defaultDisk,
): DocumentFactory {
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

    return new DocumentFactory(
        $views,
        new DocumentEngine([PHP_BINARY, __DIR__.'/fake_api2.php'], "{$root}/jobs"),
        new RenderOptions,
        $filesystems,
        $defaultDisk,
    );
}

$root = sys_get_temp_dir().DIRECTORY_SEPARATOR.'pliego-laravel-storage-'.bin2hex(random_bytes(8));
storageExpect(mkdir("{$root}/views", 0700, true), 'cannot create storage test views');
storageExpect(mkdir("{$root}/cache", 0700, true), 'cannot create storage test cache');
file_put_contents("{$root}/views/invoice.blade.php", '<h1>Invoice {{ $number }}</h1>');
file_put_contents("{$root}/blocked-root", 'not a directory');
register_shutdown_function(static fn () => removeStorageFixture($root));

$app = new StorageTestApplication("{$root}/framework-storage");
$app->instance('config', new StorageTestConfig([
    'filesystems' => [
        'default' => 'local',
        'disks' => [
            'local' => [
                'driver' => 'local',
                'root' => "{$root}/local",
                'throw' => false,
            ],
            'archive' => [
                'driver' => 'local',
                'root' => "{$root}/archive",
                'throw' => false,
            ],
            'broken' => [
                'driver' => 'local',
                'root' => "{$root}/blocked-root",
                'throw' => false,
            ],
        ],
    ],
]));
$filesystems = new FilesystemManager($app);
$app->instance('filesystem', $filesystems);
$app->instance(ParallelTesting::class, new class
{
    public function token(): false
    {
        return false;
    }
});
Container::setInstance($app);
Facade::setFacadeApplication($app);

$factory = storageDocumentFactory($root, $filesystems, 'local');
Storage::fake('archive');

$memoryBefore = memory_get_usage(true);
memory_reset_peak_usage();
putenv('PLIEGO_LARAVEL_FAKE_LARGE_PDF=1');
try {
    $stored = $factory->view('invoice', ['number' => 42])->store(
        'invoices/42.pdf',
        'archive',
        ['visibility' => 'public'],
    );
} finally {
    putenv('PLIEGO_LARAVEL_FAKE_LARGE_PDF');
}
$additionalPeak = memory_get_peak_usage(true) - $memoryBefore;
storageExpect($stored instanceof StoredDocument, 'store did not return a typed result');
storageExpect($stored->disk === 'archive', 'stored disk identity changed');
storageExpect($stored->path === 'invoices/42.pdf', 'stored path identity changed');
storageExpect($stored->renderResult->pdfPath !== '', 'render result was not returned');
storageExpect(Storage::disk('archive')->exists($stored->path), 'Storage::fake did not receive the PDF');
storageExpect(
    Storage::disk('archive')->size($stored->path) === filesize($stored->renderResult->pdfPath),
    'stored PDF size changed',
);
storageExpect(
    hash_file('sha256', Storage::disk('archive')->path($stored->path))
        === hash_file('sha256', $stored->renderResult->pdfPath),
    'stored PDF bytes changed',
);
storageExpect(Storage::disk('archive')->getVisibility($stored->path) === 'public', 'storage options were not forwarded');
storageExpect($additionalPeak < 20 * 1024 * 1024, 'storage buffered the 32 MiB PDF in PHP memory');
storageExpect(is_file($stored->renderResult->pdfPath), 'successful storage deleted the retained PDF');
storageExpect(is_dir($stored->renderResult->jobPath), 'successful storage deleted the render job');

$local = $factory->view('invoice', ['number' => 43])->store('invoices/43.pdf');
storageExpect($local->disk === 'local', 'configured default storage disk was not retained');
storageExpect(is_file("{$root}/local/invoices/43.pdf"), 'local disk did not receive the PDF');
storageExpect(
    hash_file('sha256', "{$root}/local/invoices/43.pdf") === hash_file('sha256', $local->renderResult->pdfPath),
    'local disk changed the streamed PDF bytes',
);
storageExpect(is_dir($local->renderResult->jobPath), 'local storage deleted the render job');

$renderFailurePath = 'invoices/render-failure.pdf';
try {
    $factory->view('invoice', ['number' => 'render-failure'])->store($renderFailurePath, 'archive');
    throw new RuntimeException('render failure was converted into a stored document');
} catch (RenderFailedException $error) {
    storageExpect($error->kind === 'resource', 'render failure lost its stable API 2 kind');
    storageExpect(! Storage::disk('archive')->exists($renderFailurePath), 'render failure wrote a durable target');
    storageExpect(is_dir($error->jobPath), 'render failure did not retain its job evidence');
}

foreach (['false', 'throw'] as $failureMode) {
    $failureDisk = new StorageWriteFailureDisk($failureMode);
    $failureFilesystems = new StorageWriteFailureFactory($failureDisk);
    $failureFactory = storageDocumentFactory($root, $failureFilesystems, 'failure');
    $claimedDocument = null;

    try {
        $claimedDocument = $failureFactory
            ->view('invoice', ['number' => "storage-{$failureMode}"])
            ->store("invoices/{$failureMode}.pdf", options: ['visibility' => 'private']);
        throw new RuntimeException("{$failureMode} storage failure returned a durable document");
    } catch (DocumentStorageException $error) {
        $expectedCause = $failureMode === 'throw'
            ? 'synthetic throwing storage write'
            : 'filesystem write returned false';
        storageExpect($error->disk === 'failure', "{$failureMode} storage failure lost the disk identity");
        storageExpect($error->path === "invoices/{$failureMode}.pdf", "{$failureMode} storage failure lost the path");
        storageExpect($error->getPrevious()?->getMessage() === $expectedCause, "{$failureMode} storage failure lost its cause");
        storageExpect(is_file($error->renderResult->pdfPath), "{$failureMode} storage failure lost the rendered PDF");
        storageExpect(is_dir($error->renderResult->jobPath), "{$failureMode} storage failure lost the render job");
    }

    storageExpect($claimedDocument === null, "{$failureMode} storage failure claimed a durable object");
    storageExpect($failureFilesystems->requestedDisk === 'failure', "{$failureMode} write resolved the wrong disk");
    storageExpect($failureDisk->writeAttempted, "{$failureMode} write did not reach the filesystem");
    storageExpect($failureDisk->writtenPath === "invoices/{$failureMode}.pdf", "{$failureMode} write changed the path");
    storageExpect(
        $failureDisk->writtenOptions === ['visibility' => 'private'],
        "{$failureMode} write changed the options",
    );
    storageExpect(! is_resource($failureDisk->sourceStream), "{$failureMode} write left the source stream open");
}

$queuedPayload = serialize(new StorageQueuedConsumer(45, 'invoices/45.pdf', 'archive'));
$queuedConsumer = unserialize($queuedPayload, ['allowed_classes' => [StorageQueuedConsumer::class]]);
storageExpect($queuedConsumer instanceof StorageQueuedConsumer, 'queued storage consumer did not deserialize');
$queued = $queuedConsumer->handle($factory);
storageExpect($queued->disk === 'archive', 'queued storage consumer changed the disk');
storageExpect($queued->path === 'invoices/45.pdf', 'queued storage consumer changed the path');
storageExpect(Storage::disk('archive')->exists($queued->path), 'queued storage consumer did not persist the PDF');
storageExpect(is_dir($queued->renderResult->jobPath), 'queued storage consumer deleted the render job');

try {
    $factory->view('invoice', ['number' => 44])->store('invoices/44.pdf', 'broken');
    throw new RuntimeException('broken storage disk accepted the PDF');
} catch (DocumentStorageException $error) {
    storageExpect($error->disk === 'broken', 'storage failure lost the disk identity');
    storageExpect($error->path === 'invoices/44.pdf', 'storage failure lost the path identity');
    storageExpect($error->getPrevious() !== null, 'storage failure lost its cause');
    storageExpect(is_file($error->renderResult->pdfPath), 'storage failure deleted the retained PDF');
    storageExpect(is_dir($error->renderResult->jobPath), 'storage failure deleted the render job');
}

echo "Laravel storage: ok\n";
