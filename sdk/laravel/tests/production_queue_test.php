<?php

declare(strict_types=1);

use Illuminate\Bus\BusServiceProvider;
use Illuminate\Cache\CacheServiceProvider;
use Illuminate\Config\Repository;
use Illuminate\Contracts\Debug\ExceptionHandler;
use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Database\DatabaseServiceProvider;
use Illuminate\Filesystem\FilesystemServiceProvider;
use Illuminate\Foundation\Application;
use Illuminate\Foundation\Exceptions\Handler;
use Illuminate\Http\Request;
use Illuminate\Queue\Console\WorkCommand;
use Illuminate\Queue\Events\JobFailed;
use Illuminate\Queue\Events\JobProcessed;
use Illuminate\Queue\Events\JobProcessing;
use Illuminate\Queue\InteractsWithQueue;
use Illuminate\Queue\QueueServiceProvider;
use Illuminate\Support\Facades\Facade;
use Illuminate\View\ViewServiceProvider;
use Pliego\Laravel\DocumentFactory;
use Pliego\Laravel\PliegoServiceProvider;
use Pliego\Php\DocumentEngine;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\JobRetention;
use Symfony\Component\Console\Input\ArrayInput;
use Symfony\Component\Console\Output\ConsoleOutput;
use Symfony\Component\Process\InputStream;
use Symfony\Component\Process\Process;

// Usage: PLIEGO_TEST_AUTOLOAD=/consumer/vendor/autoload.php php this.php native-binary fresh-proof-directory
// No consumer application bootstrap, .env, default database, fake native process,
// replacement queue or sleep-based completion oracle is used. Run under an outer watchdog.
// This SQLite IMMEDIATE transaction recipe requires PHP 8.4 or newer in Laravel.
$autoload = getenv('PLIEGO_TEST_AUTOLOAD');
require is_string($autoload) && $autoload !== '' ? $autoload : dirname(__DIR__).'/vendor/autoload.php';

function queueProofExpect(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

function queueProofJson(string $path, array $data, bool $exclusive = false): void
{
    $bytes = json_encode($data, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)."\n";
    $stream = fopen($path, $exclusive ? 'xb' : 'wb');
    queueProofExpect(is_resource($stream), 'cannot retain '.$path);
    try {
        queueProofExpect(fwrite($stream, $bytes) === strlen($bytes), 'incomplete evidence write');
    } finally {
        fclose($stream);
    }
}

function queueProofRead(string $path): array
{
    return json_decode((string) file_get_contents($path), true, flags: JSON_THROW_ON_ERROR);
}

function queueProofApp(string $root, string $binary): Application
{
    queueProofExpect(PHP_VERSION_ID >= 80400, 'SQLite IMMEDIATE queue recipe requires PHP 8.4+');
    $database = $root.'/queue.sqlite';
    queueProofExpect(realpath($root) === $root && ! is_link($root), 'unsafe proof root');
    queueProofExpect(is_file($database) && ! is_link($database) && realpath(dirname($database)) === $root, 'unsafe queue database');
    $app = new Application($root);
    $app->instance('env', 'testing');
    $app->instance('request', Request::create('http://localhost/'));
    $app->instance('config', new Repository([
        'app' => ['name' => 'Pliego native queue proof', 'env' => 'testing', 'locale' => 'en', 'timezone' => 'UTC'],
        'view' => ['paths' => [$root.'/resources/views'], 'compiled' => $root.'/storage/framework/views'],
        'database' => ['default' => 'proof', 'connections' => ['proof' => [
            'driver' => 'sqlite', 'database' => $database, 'prefix' => '', 'foreign_key_constraints' => true,
            'busy_timeout' => 15000, 'journal_mode' => 'WAL', 'synchronous' => 'FULL', 'transaction_mode' => 'IMMEDIATE',
        ]]],
        'queue' => ['default' => 'proof', 'connections' => ['proof' => [
            'driver' => 'database', 'connection' => 'proof', 'table' => 'jobs', 'queue' => 'lane-a', 'retry_after' => 120,
        ]], 'failed' => ['driver' => 'database-uuids', 'database' => 'proof', 'table' => 'failed_jobs']],
        'cache' => ['default' => 'array', 'stores' => ['array' => ['driver' => 'array']]],
        'logging' => ['default' => 'proof', 'channels' => ['proof' => ['driver' => 'single', 'path' => $root.'/laravel.log']]],
        'filesystems' => ['default' => 'local', 'disks' => ['local' => [
            'driver' => 'local', 'root' => $root.'/storage/app/pdfs', 'throw' => true,
        ]]],
        // PendingDocument's public API currently fixes the engine budget at 60 s.
        'pliego' => ['binary' => $binary, 'work_dir' => $root.'/native-jobs', 'timeout_seconds' => 65],
    ]));
    Facade::setFacadeApplication($app);
    $app->singleton(ExceptionHandler::class, Handler::class);
    foreach ([DatabaseServiceProvider::class, CacheServiceProvider::class, BusServiceProvider::class,
        QueueServiceProvider::class, FilesystemServiceProvider::class, ViewServiceProvider::class, PliegoServiceProvider::class] as $provider) {
        $app->register($provider);
    }
    $app->boot();
    $resolved = $app['db']->connection()->select('pragma database_list');
    queueProofExpect(count($resolved) === 1 && realpath($resolved[0]->file) === realpath($database), 'database escaped proof root');

    return $app;
}

final class NativeQueueProofJob implements ShouldQueue
{
    use InteractsWithQueue;

    public int $tries = 1;

    public function __construct(public string $case, public bool $reject, public bool $barrier) {}

    public function handle(DocumentFactory $factory): void
    {
        $app = Application::getInstance();
        $root = $app->basePath();
        $record = ['case' => $this->case, 'pid' => getmypid(), 'queue' => $this->job->getQueue(),
            'queue_id' => $this->job->getJobId(), 'uuid' => $this->job->uuid(), 'attempts' => $this->attempts(),
            'path' => 'queued documents ü/'.$this->case.'.pdf', 'outcome' => 'started'];
        queueProofJson($root.'/cases/'.$this->case.'.started.json', $record, true);
        if ($this->barrier) {
            echo 'QUEUE_READY '.$this->case."\n";
            flush();
            queueProofExpect(fgets(STDIN) === "GO\n", 'coordinator did not release reserved job');
        }
        $pending = $factory->view('invoice', ['number' => $this->case, 'reject' => $this->reject])
            ->asset('proof.woff2', $root.'/proof.woff2');
        $record['store_started_ns'] = hrtime(true);
        try {
            $stored = $pending->store($record['path'], 'local', ['visibility' => 'private']);
            $record['store_finished_ns'] = hrtime(true);
            $result = $stored->renderResult;
            $path = $app['filesystem']->disk('local')->path($stored->path);
            queueProofExpect(is_file($path) && hash_file('sha256', $path) === hash_file('sha256', $result->pdfPath), 'stored readback mismatch');
            $scene = queueProofRead($result->scenePath);
            $text = '';
            foreach ($scene['pages'] as $page) {
                foreach ($page['operations'] as $operation) {
                    if ($operation['type'] === 'text') {
                        $text .= $operation['text'];
                    }
                }
            }
            queueProofExpect($text === 'PLIEGO QUEUE '.$this->case, 'queued Blade/scene identity mismatch');
            $record += ['job_path' => $result->jobPath, 'runtime_job_path' => $result->runtimeJobPath,
                'pdf_sha256' => hash_file('sha256', $path), 'pdf_bytes' => filesize($path), 'scene_text' => $text,
                'delivery_identity' => $result->deliveryIdentity];
            // This application record is written only after the real store and readback.
            $app['db']->table('success_records')->insert(['case_name' => $this->case, 'queue_uuid' => $record['uuid'],
                'path' => $stored->path, 'pdf_sha256' => $record['pdf_sha256'], 'native_job_path' => $result->jobPath]);
            $record['outcome'] = 'stored';
        } catch (Throwable $error) {
            $record['store_finished_ns'] ??= hrtime(true);
            $record['outcome'] = 'failed';
            $record['exception'] = $error::class;
            $record['message'] = $error->getMessage();
            if ($error instanceof RenderFailedException) {
                $record += ['kind' => $error->kind, 'job_path' => $error->jobPath, 'runtime_job_path' => $error->runtimeJobPath];
                queueProofJson($root.'/cases/'.$this->case.'.native-result.json', $error->result, true);
            }
            throw $error;
        } finally {
            $record['store_finished_ns'] ??= hrtime(true);
            queueProofJson($root.'/cases/'.$this->case.'.json', $record, true);
        }
    }
}

if (($argv[1] ?? '') === '--worker') {
    $root = realpath($argv[2] ?? '');
    $lane = $argv[3] ?? '';
    queueProofExpect(count($argv) === 4 && is_string($root) && in_array($lane, ['lane-a', 'lane-b'], true), 'invalid worker arguments');
    $settings = queueProofRead($root.'/settings.json');
    $app = queueProofApp($root, $settings['binary']);
    $contract = $app->make(DocumentEngine::class)->contract()->toArray();
    queueProofExpect($contract === $settings['contract'], 'worker native identity changed');
    queueProofJson($root.'/'.$lane.'.identity.json', ['pid' => getmypid(), 'contract' => $contract,
        'database' => $app['config']->get('database.connections.proof.database'), 'pcntl' => extension_loaded('pcntl')], true);
    foreach ([JobProcessing::class, JobProcessed::class, JobFailed::class] as $eventClass) {
        $app['events']->listen($eventClass, static function ($event) use ($root, $lane): void {
            $record = ['event' => $event::class, 'uuid' => $event->job->uuid(), 'id' => $event->job->getJobId(),
                'attempts' => $event->job->attempts(), 'pid' => getmypid(), 'at_ns' => hrtime(true)];
            $line = json_encode($record, JSON_THROW_ON_ERROR)."\n";
            queueProofExpect(file_put_contents($root.'/'.$lane.'.events.jsonl', $line, FILE_APPEND | LOCK_EX) === strlen($line), 'cannot retain queue event');
        });
    }
    $command = new WorkCommand($app['queue.worker'], $app['cache.store']);
    $command->setLaravel($app);
    exit($command->run(new ArrayInput(['connection' => 'proof', '--queue' => $lane, '--name' => $lane,
        '--stop-when-empty' => true, '--max-jobs' => 3, '--max-time' => 90, '--timeout' => 75,
        '--tries' => 1, '--sleep' => 0, '--rest' => 0, '--force' => true, '--json' => true], $command->getDefinition()), new ConsoleOutput));
}

$binary = isset($argv[1]) ? realpath($argv[1]) : false;
$requestedRoot = $argv[2] ?? '';
queueProofExpect(count($argv) === 3 && is_string($binary) && is_file($binary), 'supply native binary and fresh proof directory');
$magic = file_get_contents($binary, false, null, 0, 4);
queueProofExpect(is_string($magic) && (str_starts_with($magic, 'MZ') || $magic === "\x7fELF"
    || in_array($magic, ["\xfe\xed\xfa\xce", "\xce\xfa\xed\xfe", "\xfe\xed\xfa\xcf", "\xcf\xfa\xed\xfe", "\xca\xfe\xba\xbe", "\xbe\xba\xfe\xca"], true)), 'binary must be native');
queueProofExpect($requestedRoot !== '' && ! file_exists($requestedRoot) && ! is_link($requestedRoot), 'proof directory already exists');
queueProofExpect(is_dir(dirname($requestedRoot)) && mkdir($requestedRoot, 0700), 'proof parent must exist');
$root = realpath($requestedRoot);
queueProofExpect(is_string($root), 'cannot resolve proof root');
$report = ['schema' => 'pliego.laravel-native-queue-proof.v1', 'outcome' => 'running', 'php' => PHP_VERSION,
    'framework' => Application::VERSION, 'os' => PHP_OS_FAMILY, 'binary' => $binary,
    'binary_sha256' => 'sha256:'.hash_file('sha256', $binary), 'test_sha256' => hash_file('sha256', __FILE__),
    'limits' => ['engine_host_wall_seconds' => 60, 'sdk_seconds' => 65, 'worker_job_seconds' => 75,
        'retry_after_seconds' => 120, 'worker_max_time_seconds' => 90, 'worker_max_jobs' => 3, 'parent_process_seconds' => 100],
    'boundary' => 'Real SQLite database queue serialization, two standard Laravel worker processes, concurrent real API 2 store calls, typed native failure and recovery. Two named queues, shared database/native job root/local storage. Not shared-queue contention, crash/retry exactly-once, descendant cancellation, external app adoption, public-package installation or performance comparison. Windows lacks pcntl job alarms; SDK and parent process limits remain active.',
    'sqlite_recipe' => ['minimum_php' => '8.4', 'transaction_mode' => 'IMMEDIATE', 'journal_mode' => 'WAL', 'synchronous' => 'FULL', 'busy_timeout_ms' => 15000],
    'cases' => [], 'workers' => []];
$processes = [];
queueProofJson($root.'/report.json', $report);
try {
    foreach (['bootstrap/cache', 'resources/views', 'storage/framework/views', 'storage/app/pdfs', 'cases'] as $path) {
        queueProofExpect(mkdir($root.'/'.$path, 0700, true), 'cannot create '.$path);
    }
    queueProofExpect(touch($root.'/queue.sqlite'), 'cannot create isolated SQLite database');
    $font = dirname((new ReflectionClass(DocumentEngine::class))->getFileName(), 2).'/resources/HasubiMono-Regular.woff2';
    queueProofExpect(copy($font, $root.'/proof.woff2'), 'cannot copy supplied font');
    $template = '<!doctype html><meta charset="utf-8"><style>@font-face{font-family:Proof;src:url("proof.woff2") format("woff2")}body{font:12px Proof;margin:0}</style>'
        .'<p>PLIEGO QUEUE {{ $number }}</p>@if($reject)<img src="missing.png" width="20" height="20">@endif';
    queueProofExpect(file_put_contents($root.'/resources/views/invoice.blade.php', $template) === strlen($template), 'cannot create Blade input');
    $app = queueProofApp($root, $binary);
    $db = $app['db']->connection();
    $db->statement('CREATE TABLE jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, queue VARCHAR(255) NOT NULL, payload TEXT NOT NULL, attempts INTEGER NOT NULL, reserved_at INTEGER NULL, available_at INTEGER NOT NULL, created_at INTEGER NOT NULL)');
    $db->statement('CREATE INDEX jobs_queue_index ON jobs (queue)');
    $db->statement('CREATE TABLE failed_jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, uuid VARCHAR(255) NOT NULL UNIQUE, connection TEXT NOT NULL, queue TEXT NOT NULL, payload TEXT NOT NULL, exception TEXT NOT NULL, failed_at DATETIME NOT NULL)');
    $db->statement('CREATE TABLE success_records (case_name TEXT PRIMARY KEY, queue_uuid TEXT NOT NULL UNIQUE, path TEXT NOT NULL UNIQUE, pdf_sha256 TEXT NOT NULL, native_job_path TEXT NOT NULL UNIQUE)');
    $contract = $app->make(DocumentEngine::class)->contract()->toArray();
    queueProofExpect($contract['engine']['api'] === 2 && $contract['engine']['runtime']['binary_sha256'] === $report['binary_sha256'], 'native contract identity mismatch');
    $report['contract'] = $contract;
    foreach ([DocumentEngine::class, DocumentFactory::class, PliegoServiceProvider::class, WorkCommand::class] as $class) {
        $path = (new ReflectionClass($class))->getFileName();
        $report['sources'][$class] = ['path' => $path, 'sha256' => hash_file('sha256', $path)];
    }
    queueProofJson($root.'/settings.json', ['binary' => $binary, 'contract' => $contract], true);
    foreach (['lane-a', 'lane-b'] as $lane) {
        foreach (['valid', 'missing', 'recovery'] as $kind) {
            $app['queue']->connection('proof')->push(new NativeQueueProofJob($lane.'-'.$kind, $kind === 'missing', $kind === 'valid'), '', $lane);
        }
    }
    $queued = array_map(static fn ($row) => (array) $row, $db->table('jobs')->orderBy('id')->get()->all());
    queueProofExpect(count($queued) === 6 && $db->table('success_records')->count() === 0, 'jobs did not persist before workers started');
    $expected = [];
    foreach ($queued as $row) {
        $payload = json_decode($row['payload'], true, flags: JSON_THROW_ON_ERROR);
        queueProofExpect($row['attempts'] === 0 && $row['reserved_at'] === null && $payload['data']['commandName'] === NativeQueueProofJob::class
            && str_starts_with($payload['data']['command'], 'O:'), 'queue payload is not an unattempted serialized job');
        $job = unserialize($payload['data']['command'], ['allowed_classes' => [NativeQueueProofJob::class]]);
        queueProofExpect($job instanceof NativeQueueProofJob && ! isset($expected[$job->case]), 'duplicate serialized case');
        $expected[$job->case] = ['id' => $row['id'], 'uuid' => $payload['uuid'], 'queue' => $row['queue']];
    }
    queueProofJson($root.'/queued-before-workers.json', $queued, true);
    $db->disconnect();
    queueProofExpect($db->table('jobs')->count() === 6, 'queue did not survive database reconnection');
    $inputs = [];
    foreach (['lane-a', 'lane-b'] as $lane) {
        $inputs[$lane] = new InputStream;
        $processes[$lane] = new Process([PHP_BINARY, __FILE__, '--worker', $root, $lane], $root, null, $inputs[$lane], 100);
        $processes[$lane]->start();
    }
    foreach ($processes as $lane => $process) {
        $output = '';
        $ready = false;
        // The iterator includes output buffered before this lane is visited.
        foreach ($process->getIterator(Process::ITER_KEEP_OUTPUT) as $data) {
            $output .= $data;
            if (str_contains($output, 'QUEUE_READY '.$lane.'-valid')) {
                $ready = true;
                break;
            }
        }
        queueProofExpect($ready, $lane.' stopped before reserving its first job');
    }
    $reserved = $db->table('jobs')->whereNotNull('reserved_at')->get()->all();
    queueProofExpect(count($reserved) === 2 && $db->table('success_records')->count() === 0, 'two distinct jobs were not reserved before release');
    queueProofJson($root.'/reserved-before-release.json', array_map(static fn ($row) => (array) $row, $reserved), true);
    foreach ($inputs as $input) {
        $input->write("GO\n");
        $input->close();
    }
    // Pump both standard Process inputs before waiting for either worker to exit.
    foreach ([1, 2] as $_) {
        foreach ($processes as $process) {
            $process->isRunning();
        }
    }
    foreach ($processes as $lane => $process) {
        queueProofExpect($process->wait() === 0, $lane.' worker failed');
    }
    $report['counts'] = ['queued' => count($queued), 'pending' => $db->table('jobs')->count(),
        'failed' => $db->table('failed_jobs')->count(), 'stored' => $db->table('success_records')->count()];
    queueProofJson($root.'/failed-jobs.json', array_map(static fn ($row) => (array) $row, $db->table('failed_jobs')->get()->all()), true);
    queueProofJson($root.'/success-records.json', array_map(static fn ($row) => (array) $row, $db->table('success_records')->get()->all()), true);
    queueProofExpect($report['counts'] === ['queued' => 6, 'pending' => 0, 'failed' => 2, 'stored' => 4], 'queue outcome denominator mismatch');
    $nativePaths = $uuids = $inputHashes = [];
    $processing = $processed = $failed = 0;
    foreach (['lane-a', 'lane-b'] as $lane) {
        $identity = queueProofRead($root.'/'.$lane.'.identity.json');
        $report['workers'][$lane] = $identity;
        foreach (file($root.'/'.$lane.'.events.jsonl', FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES) as $line) {
            $event = json_decode($line, true, flags: JSON_THROW_ON_ERROR);
            queueProofExpect($event['attempts'] === 1 && $event['pid'] === $identity['pid'], 'queue retry or worker identity changed');
            $processing += $event['event'] === JobProcessing::class ? 1 : 0;
            $processed += $event['event'] === JobProcessed::class ? 1 : 0;
            $failed += $event['event'] === JobFailed::class ? 1 : 0;
        }
        foreach (['valid', 'missing', 'recovery'] as $kind) {
            $case = $lane.'-'.$kind;
            $record = queueProofRead($root.'/cases/'.$case.'.json');
            $report['cases'][$case] = $record;
            queueProofExpect($record['case'] === $case && $record['pid'] === $identity['pid'] && $record['attempts'] === 1, 'job identity or attempt mismatch');
            queueProofExpect($record['queue_id'] === $expected[$case]['id'] && $record['uuid'] === $expected[$case]['uuid']
                && $record['queue'] === $expected[$case]['queue'] && $record['path'] === 'queued documents ü/'.$case.'.pdf', 'dequeued payload or target differs from enqueued identity');
            $nativePaths[] = $record['job_path'];
            $uuids[] = $record['uuid'];
            $html = (string) file_get_contents($record['runtime_job_path'].'/input/document.html');
            queueProofExpect(str_contains($html, 'PLIEGO QUEUE '.$case), 'queued input identity mismatch');
            $inputHashes[] = hash('sha256', $html);
            $jobState = trim((string) file_get_contents($record['job_path'].'/'.JobRetention::STATUS_FILE));
            if ($kind === 'missing') {
                queueProofExpect($record['outcome'] === 'failed' && $record['exception'] === RenderFailedException::class && $record['kind'] === 'resource'
                    && $jobState === 'failure', 'missing resource did not produce retained typed failure');
                queueProofExpect($db->table('failed_jobs')->where('uuid', $record['uuid'])->count() === 1, 'Laravel did not persist failed UUID');
                queueProofExpect(! is_file($root.'/storage/app/pdfs/'.$record['path'])
                    && ! is_file($record['runtime_job_path'].'/delivery/document.pdf')
                    && ! is_file($record['runtime_job_path'].'/delivery/bundle.json'), 'failed queued render published an artifact');
                queueProofExpect($db->table('success_records')->where('case_name', $case)->count() === 0, 'failed render committed success');
            } else {
                queueProofExpect($record['outcome'] === 'stored' && $jobState === 'success', 'valid queued render was not stored');
                $stored = $db->table('success_records')->where('case_name', $case)->first();
                queueProofExpect($stored !== null && $stored->queue_uuid === $record['uuid'] && $stored->pdf_sha256 === $record['pdf_sha256']
                    && hash_file('sha256', $root.'/storage/app/pdfs/'.$record['path']) === $record['pdf_sha256'], 'success record/readback changed');
            }
        }
        queueProofExpect($report['cases'][$lane.'-recovery']['store_started_ns'] > $report['cases'][$lane.'-missing']['store_finished_ns'], 'recovery did not follow failure on the same worker');
    }
    $report['counts'] += ['dequeued' => $processing, 'processed_events' => $processed, 'failed_events' => $failed];
    queueProofExpect($processing === 6 && $processed === 4 && $failed === 2, 'Laravel lifecycle event count changed');
    queueProofExpect(count(array_unique($nativePaths)) === 6 && count(array_unique($uuids)) === 6 && count(array_unique($inputHashes)) === 6, 'native job, UUID or input collision');
    queueProofExpect(count(glob($root.'/native-jobs/*', GLOB_ONLYDIR)) === 6, 'unexpected native job count');
    queueProofExpect(count($app['filesystem']->disk('local')->allFiles()) === 4, 'unexpected stored file count');
    queueProofExpect($report['workers']['lane-a']['pid'] !== $report['workers']['lane-b']['pid'], 'not two worker processes');
    $a = $report['cases']['lane-a-valid'];
    $b = $report['cases']['lane-b-valid'];
    $overlap = min($a['store_finished_ns'], $b['store_finished_ns']) - max($a['store_started_ns'], $b['store_started_ns']);
    $report['store_call_overlap_ns'] = $overlap;
    queueProofExpect($overlap > 0, 'real API 2 store calls did not overlap');
    $report['outcome'] = 'passed';
} catch (Throwable $error) {
    $report['outcome'] = 'failed';
    $report['failure'] = ['class' => $error::class, 'message' => $error->getMessage()];
} finally {
    foreach ($processes as $lane => $process) {
        try {
            if ($process->isRunning()) {
                $process->stop(1);
            }
        } catch (Throwable $error) {
            $report['outcome'] = 'failed';
            $report['cleanup_failures'][$lane] = $error->getMessage();
        }
    }
    foreach ($processes as $lane => $process) {
        try {
            if ($process->isStarted()) {
                foreach (['stdout' => $process->getOutput(), 'stderr' => $process->getErrorOutput()] as $suffix => $bytes) {
                    queueProofExpect(file_put_contents($root.'/'.$lane.'.'.$suffix, $bytes) === strlen($bytes), 'cannot retain worker '.$suffix);
                }
                $report['worker_exit_codes'][$lane] = $process->getExitCode();
            }
        } catch (Throwable $error) {
            $report['outcome'] = 'failed';
            $report['retention_failures'][$lane] = $error->getMessage();
        }
    }
    queueProofJson($root.'/report.json', $report);
}
echo json_encode(['outcome' => $report['outcome'], 'report' => $root.'/report.json'], JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES).PHP_EOL;
exit($report['outcome'] === 'passed' ? 0 : 1);
