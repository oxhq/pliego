<?php

declare(strict_types=1);

use Composer\InstalledVersions;
use Illuminate\Console\Command;
use Illuminate\Support\Facades\Artisan;
use Pliego\Laravel\Facades\Document;
use Pliego\Laravel\ManagedRuntime;
use Pliego\Php\Exception\RenderException;
use Pliego\Php\JobRetention;
use Symfony\Component\Process\Process;

final class PliegoQueueRehearsal
{
    private const PLAN = [
        ['sequence' => 1, 'scenario' => 'offline', 'expected' => 'success'],
        ['sequence' => 2, 'scenario' => 'denial', 'expected' => 'RESOURCE_DENIED'],
        ['sequence' => 3, 'scenario' => 'offline', 'expected' => 'success'],
        ['sequence' => 4, 'scenario' => 'live', 'expected' => 'success'],
        ['sequence' => 5, 'scenario' => 'timeout', 'expected' => 'RENDER_TIMEOUT'],
        ['sequence' => 6, 'scenario' => 'offline', 'expected' => 'success'],
    ];

    private const PACKAGES = ['oxhq/pliego-php', 'oxhq/pliego-laravel'];

    public static function run(Command $command): int
    {
        if ((bool) $command->option('self-test')) {
            self::selfTest();
            $command->info('Pliego six-job queue rehearsal self-test passed.');

            return Command::SUCCESS;
        }

        self::require(PHP_OS_FAMILY === 'Linux', 'the production rehearsal only runs on Linux');
        self::require(is_executable('/usr/bin/time'), 'GNU /usr/bin/time is required for peak RSS evidence');

        $expectedVersion = self::requiredString($command, 'release-version');
        self::verifyPublicComposerInstall($expectedVersion);

        $binary = app(ManagedRuntime::class)->binary();
        self::require(self::isAbsolutePath($binary) && is_file($binary) && is_executable($binary), 'managed Pliego runtime must be an executable absolute path');
        $binarySha256 = self::requiredHash($command, 'binary-sha256');
        self::require(hash_file('sha256', $binary) === $binarySha256, 'published Pliego runtime checksum differs');

        $offlineFont = self::requiredString($command, 'offline-font');
        self::require(self::isAbsolutePath($offlineFont) && is_file($offlineFont), 'offline font must be an absolute file path');
        $offlineFontSha256 = self::requiredHash($command, 'offline-font-sha256');
        self::require(hash_file('sha256', $offlineFont) === $offlineFontSha256, 'offline WOFF2 checksum differs');
        self::require(file_get_contents($offlineFont, false, null, 0, 4) === 'wOF2', 'offline font is not WOFF2');

        $cssUrl = self::requiredString($command, 'css-url');
        $fontUrl = self::requiredString($command, 'font-url');
        $cssOrigin = self::localOrigin($cssUrl, 'CSS');
        $fontOrigin = self::localOrigin($fontUrl, 'font');
        self::require($cssOrigin !== $fontOrigin, 'live CSS and WOFF2 must use two origins');
        $cssSha256 = self::requiredHash($command, 'css-sha256');
        $fontSha256 = self::requiredHash($command, 'font-sha256');
        $css = self::fetchLocalFixture($cssUrl);
        $font = self::fetchLocalFixture($fontUrl);
        self::require(hash('sha256', $css) === $cssSha256 && str_contains($css, $fontUrl), 'live CSS fixture identity differs');
        self::require(hash('sha256', $font) === $fontSha256 && str_starts_with($font, 'wOF2'), 'live WOFF2 fixture identity differs');

        $connection = trim((string) ($command->option('connection') ?: config('queue.default')));
        self::require($connection !== '' && config("queue.connections.{$connection}") !== null, 'queue connection is not configured');
        $driver = (string) config("queue.connections.{$connection}.driver");
        self::require(!in_array($driver, ['sync', 'deferred'], true), 'rehearsal requires a durable queue connection');

        $renderTimeout = (int) config('pliego.timeout_seconds');
        self::require($renderTimeout >= 1 && $renderTimeout <= 15, 'PLIEGO_TIMEOUT_SECONDS must be between 1 and 15 for the timeout job');
        $workerTimeout = $renderTimeout + 30;
        $retryAfter = config("queue.connections.{$connection}.retry_after");
        self::require(!is_numeric($retryAfter) || (int) $retryAfter > $workerTimeout, 'queue retry_after must exceed the worker timeout');

        $run = gmdate('Ymd\THis\Z').'-'.bin2hex(random_bytes(4));
        $report = (string) ($command->option('report') ?: storage_path("app/pliego-rehearsals/{$run}"));
        $workRoot = storage_path("app/pliego-rehearsal-work/{$run}");
        self::require(self::isAbsolutePath($report) && !file_exists($report), 'report directory must be a new absolute path');
        self::require(!file_exists($workRoot), 'rehearsal work root already exists');
        self::require(!str_starts_with($report.DIRECTORY_SEPARATOR, $workRoot.DIRECTORY_SEPARATOR), 'report directory must be outside the pruned work root');
        self::mkdir($report);
        self::mkdir($workRoot);

        $queue = "pliego-rehearsal-{$run}";
        $baselineProcesses = self::processSnapshot();
        $baselineBytes = self::treeBytes($workRoot);
        $jobOptions = [
            '--report' => $report,
            '--work-root' => $workRoot,
            '--offline-font' => $offlineFont,
            '--offline-font-sha256' => $offlineFontSha256,
            '--css-url' => $cssUrl,
            '--css-sha256' => $cssSha256,
            '--font-url' => $fontUrl,
            '--font-sha256' => $fontSha256,
        ];
        foreach (self::PLAN as $job) {
            Artisan::queue('pliego:rehearsal-job', [
                'run' => $run,
                'sequence' => $job['sequence'],
                'scenario' => $job['scenario'],
                ...$jobOptions,
            ])->onConnection($connection)->onQueue($queue);
        }

        $queued = app('queue')->connection($connection)->size($queue);
        self::require($queued === count(self::PLAN), "expected six queued jobs, found {$queued}");

        $workerLog = "{$report}/worker.log";
        $workerRusage = "{$report}/worker-rusage.txt";
        $worker = new Process([
            '/usr/bin/time',
            '-v',
            '-o',
            $workerRusage,
            PHP_BINARY,
            base_path('artisan'),
            'queue:work',
            $connection,
            "--queue={$queue}",
            '--stop-when-empty',
            '--max-jobs=6',
            '--tries=1',
            '--sleep=0',
            "--timeout={$workerTimeout}",
        ], base_path(), ['LANG' => 'C', 'LC_ALL' => 'C']);
        $worker->setTimeout((6 * $workerTimeout) + 60);
        $worker->run();
        file_put_contents($workerLog, $worker->getOutput().$worker->getErrorOutput(), LOCK_EX);
        self::require($worker->isSuccessful(), "queue worker failed; inspect {$workerLog}");
        self::require(app('queue')->connection($connection)->size($queue) === 0, 'rehearsal queue did not drain');

        $records = self::readRecords($report);
        self::validateRecords($records, $cssSha256, $fontSha256);
        $retainedBytes = self::treeBytes($workRoot);
        $afterProcesses = self::waitForProcessExit($baselineProcesses);
        $leaks = array_values(array_diff(array_keys($afterProcesses), array_keys($baselineProcesses)));
        self::require($leaks === [], 'Pliego/Xvfb child processes remain: '.implode(', ', $leaks));

        foreach ($records as $record) {
            $jobPath = (string) ($record['job_path'] ?? '');
            self::require(realpath(dirname($jobPath)) === realpath($workRoot), "job escaped rehearsal root: {$jobPath}");
            self::require(preg_match('/^[0-9a-f]{32}$/D', basename($jobPath)) === 1, "invalid retained job path: {$jobPath}");
            self::require(touch($jobPath.DIRECTORY_SEPARATOR.JobRetention::STATUS_FILE, time() - 2), "cannot age retained job: {$jobPath}");
        }
        $prune = (new JobRetention())->prune($workRoot, 0, 0);
        $prunedBytes = self::treeBytes($workRoot);
        self::require($prune['jobs'] === 6 && $prunedBytes === $baselineBytes, 'pruning did not return retained disk usage to baseline');

        $manifest = [
            'schema' => 'pliego.queue-rehearsal.v1',
            'status' => 'passed',
            'run' => $run,
            'release' => [
                'version' => $expectedVersion,
                'binary' => realpath($binary),
                'sha256' => $binarySha256,
                'packages' => array_fill_keys(self::PACKAGES, $expectedVersion),
            ],
            'queue' => ['connection' => $connection, 'driver' => $driver, 'name' => $queue, 'concurrency' => 1, 'jobs' => 6],
            'limits' => ['render_timeout_seconds' => $renderTimeout, 'worker_timeout_seconds' => $workerTimeout],
            'metrics' => [
                'peak_rss_kib' => self::peakRss($workerRusage),
                'cold_duration_ms' => $records[0]['duration_ms'],
                'warm_duration_ms' => array_column(array_slice($records, 1), 'duration_ms'),
                'disk_bytes' => ['baseline' => $baselineBytes, 'retained' => $retainedBytes, 'after_prune' => $prunedBytes],
            ],
            'processes' => ['baseline' => $baselineProcesses, 'after' => $afterProcesses, 'leaks' => $leaks],
            'live_resources' => [
                ['url' => $cssUrl, 'origin' => $cssOrigin, 'sha256' => $cssSha256],
                ['url' => $fontUrl, 'origin' => $fontOrigin, 'sha256' => $fontSha256],
            ],
            'prune' => $prune,
            'jobs' => $records,
        ];
        self::writeJson("{$report}/manifest.json", $manifest);
        $command->info("Pliego six-job queue rehearsal passed: {$report}/manifest.json");

        return Command::SUCCESS;
    }

    public static function runJob(Command $command): int
    {
        $run = (string) $command->argument('run');
        self::require(preg_match('/^\d{8}T\d{6}Z-[0-9a-f]{8}$/D', $run) === 1, 'invalid rehearsal run identifier');
        $sequence = (int) $command->argument('sequence');
        $scenario = (string) $command->argument('scenario');
        $planned = array_values(array_filter(
            self::PLAN,
            static fn (array $job): bool => $job['sequence'] === $sequence && $job['scenario'] === $scenario,
        ));
        self::require(count($planned) === 1, "unknown rehearsal job {$sequence}:{$scenario}");
        $expected = $planned[0]['expected'];

        $report = self::requiredString($command, 'report');
        $workRoot = self::requiredString($command, 'work-root');
        $offlineFont = self::requiredString($command, 'offline-font');
        $offlineFontSha256 = self::requiredHash($command, 'offline-font-sha256');
        $cssUrl = self::requiredString($command, 'css-url');
        $fontUrl = self::requiredString($command, 'font-url');
        $cssSha256 = self::requiredHash($command, 'css-sha256');
        $fontSha256 = self::requiredHash($command, 'font-sha256');
        self::require(is_dir($report) && is_dir($workRoot), 'rehearsal paths are unavailable');
        self::require(hash_file('sha256', $offlineFont) === $offlineFontSha256, 'offline WOFF2 changed after dispatch');
        config(['pliego.work_dir' => $workRoot]);

        $started = hrtime(true);
        $record = [
            'run' => $run,
            'sequence' => $sequence,
            'scenario' => $scenario,
            'expected' => $expected,
        ];
        try {
            $document = Document::view('invoice', [
                'rows' => range(1, 32),
                'rehearsalMode' => $scenario,
                'rehearsalCssUrl' => $cssUrl,
                'rehearsalFontFile' => 'rehearsal.woff2',
                'rehearsalFontFormat' => 'woff2',
            ])
                ->locale('es-MX')
                ->timezone('PST8PDT');
            if ($scenario === 'live') {
                $document->allowHttpRoot(self::localOrigin($cssUrl, 'CSS'))
                    ->allowHttpRoot(self::localOrigin($fontUrl, 'font'));
            } else {
                $document->denyNetwork()->asset('assets/rehearsal.woff2', $offlineFont);
            }
            $result = $document->render('invoice.pdf');
            self::require($expected === 'success', "{$scenario} unexpectedly rendered");
            $record += self::successRecord($result, $scenario, $offlineFontSha256, $cssUrl, $cssSha256, $fontUrl, $fontSha256);
        } catch (RenderException $error) {
            $record += [
                'outcome' => 'failure',
                'error_code' => $error->errorCode,
                'exit_code' => $error->exitCode,
                'job_path' => $error->jobPath,
                'input_bundle' => $error->inputBundlePath,
                'artifacts' => $error->artifactsPath,
                'stderr' => ['bytes' => strlen($error->stderr), 'sha256' => hash('sha256', $error->stderr)],
            ];
            if ($error->errorCode !== $expected) {
                self::finishJobRecord($report, $record, $started);
                throw new RuntimeException("{$scenario} returned {$error->errorCode}, expected {$expected}", previous: $error);
            }
        } catch (Throwable $error) {
            $record += ['outcome' => 'unexpected-failure', 'error' => $error::class, 'message' => $error->getMessage()];
            self::finishJobRecord($report, $record, $started);
            throw $error;
        }

        self::finishJobRecord($report, $record, $started);

        return Command::SUCCESS;
    }

    public static function selfTest(): void
    {
        $hash = str_repeat('a', 64);
        $fontHash = str_repeat('b', 64);
        $identity = [
            'pdf' => ['bytes' => 100, 'sha256' => 'sha256:pdf'],
            'document_sha256' => 'sha256:document',
            'render_id' => 'sha256:render',
            'resolved_input_hash' => 'sha256:resolved',
            'font_resources' => ['sha256:font'],
        ];
        $records = [];
        foreach (self::PLAN as $job) {
            $record = [
                ...$job,
                'duration_ms' => 10 + $job['sequence'],
                'job_path' => '/tmp/'.str_repeat((string) $job['sequence'], 32),
            ];
            if ($job['expected'] === 'success') {
                $record += ['outcome' => 'success', ...$identity];
            } else {
                $record += ['outcome' => 'failure', 'error_code' => $job['expected']];
            }
            if ($job['scenario'] === 'live') {
                $record['resources'] = [
                    ['url' => 'http://127.0.0.1:1/family.css', 'origin' => 'http://127.0.0.1:1/', 'sha256' => $hash],
                    ['url' => 'http://127.0.0.1:2/fixture.woff2', 'origin' => 'http://127.0.0.1:2/', 'sha256' => $fontHash],
                ];
            }
            $records[] = $record;
        }
        self::validateRecords($records, $hash, $fontHash);
        self::require(array_column(self::PLAN, 'scenario') === ['offline', 'denial', 'offline', 'live', 'timeout', 'offline'], 'six-job order changed');
    }

    private static function successRecord(
        object $result,
        string $scenario,
        string $offlineFontSha256,
        string $cssUrl,
        string $cssSha256,
        string $fontUrl,
        string $fontSha256,
    ): array {
        $pdf = file_get_contents($result->pdfPath);
        self::require(is_string($pdf) && str_starts_with($pdf, '%PDF-'), 'successful job produced an unreadable PDF');
        $manifest = self::readJson($result->inputBundlePath.'/input-bundle.json');
        $scene = self::readJson($result->artifactsPath.'/scene.json');
        $fonts = self::readJson($result->artifactsPath.'/fonts.json');
        $text = '';
        foreach ($scene['pages'] ?? [] as $page) {
            foreach ($page['operations'] ?? [] as $operation) {
                if (($operation['type'] ?? null) === 'text') {
                    $text .= (string) ($operation['text'] ?? '');
                }
            }
        }
        self::require(str_contains($text, 'INVOICE PLG-2026-001'), 'successful PDF scene omitted invoice text');

        $fontResources = array_values(array_unique(array_filter(array_map(
            static fn (mixed $selection): mixed => is_array($selection) ? ($selection['resource'] ?? null) : null,
            $fonts['selections'] ?? [],
        ), static fn (mixed $resource): bool => is_string($resource) && str_starts_with($resource, 'sha256:'))));
        sort($fontResources, SORT_STRING);
        self::require($fontResources !== [], 'successful job omitted selected font identity');

        $record = [
            'outcome' => 'success',
            'job_path' => $result->jobPath,
            'input_bundle' => $result->inputBundlePath,
            'artifacts' => $result->artifactsPath,
            'pdf' => ['bytes' => strlen($pdf), 'sha256' => 'sha256:'.hash('sha256', $pdf)],
            'document_sha256' => $manifest['document_sha256'] ?? null,
            'render_id' => $result->metadata['render_id'] ?? null,
            'resolved_input_hash' => $result->metadata['resolved_input_hash'] ?? null,
            'font_resources' => $fontResources,
        ];
        self::require(is_string($record['document_sha256']) && str_starts_with($record['document_sha256'], 'sha256:'), 'input document identity is absent');
        self::require(is_string($record['render_id']) && str_starts_with($record['render_id'], 'sha256:'), 'render identity is absent');
        self::require(is_string($record['resolved_input_hash']) && str_starts_with($record['resolved_input_hash'], 'sha256:'), 'resolved input identity is absent');

        if ($scenario !== 'live') {
            self::require(($manifest['assets']['assets/rehearsal.woff2']['sha256'] ?? null) === 'sha256:'.$offlineFontSha256, 'offline WOFF2 identity differs');

            return $record;
        }

        $rows = self::readJsonLines($result->artifactsPath.'/resources.jsonl');
        $resources = [];
        foreach ([[$cssUrl, $cssSha256], [$fontUrl, $fontSha256]] as [$url, $sha256]) {
            $matches = array_values(array_filter($rows, static fn (array $row): bool => ($row['status'] ?? null) === 'loaded' && ($row['url'] ?? null) === $url));
            self::require($matches !== [], "live resource log omitted {$url}");
            $row = $matches[array_key_last($matches)];
            self::require(($row['sha256'] ?? null) === $sha256, "live resource checksum differs for {$url}");
            $artifact = $result->artifactsPath.'/'.(string) ($row['artifact'] ?? '');
            self::require(is_file($artifact) && hash_file('sha256', $artifact) === $sha256, "retained live resource differs for {$url}");
            $resources[] = ['url' => $url, 'origin' => self::localOrigin($url, 'resource'), 'sha256' => $sha256];
        }
        self::require(count(array_unique(array_column($resources, 'origin'))) === 2, 'live job did not retain two origins');
        $record['resources'] = $resources;

        return $record;
    }

    private static function finishJobRecord(string $report, array &$record, int $started): void
    {
        $record['duration_ms'] = round((hrtime(true) - $started) / 1_000_000, 3);
        $jobPath = $record['job_path'] ?? null;
        $record['retained_bytes'] = is_string($jobPath) && is_dir($jobPath) ? self::treeBytes($jobPath) : 0;
        self::writeJson(sprintf('%s/jobs/%02d-%s.json', $report, $record['sequence'], $record['scenario']), $record);
    }

    private static function validateRecords(array $records, string $cssSha256, string $fontSha256): void
    {
        self::require(count($records) === count(self::PLAN), 'rehearsal did not produce six job records');
        foreach (self::PLAN as $index => $planned) {
            $record = $records[$index];
            self::require(($record['sequence'] ?? null) === $planned['sequence'] && ($record['scenario'] ?? null) === $planned['scenario'], 'job execution order differs');
            self::require(($record['expected'] ?? null) === $planned['expected'], 'job expected outcome differs');
            self::require(is_numeric($record['duration_ms'] ?? null) && $record['duration_ms'] > 0, 'job duration is absent');
            if ($planned['expected'] === 'success') {
                self::require(($record['outcome'] ?? null) === 'success' && ($record['pdf']['bytes'] ?? 0) > 0, 'expected successful job failed');
            } else {
                self::require(($record['outcome'] ?? null) === 'failure' && ($record['error_code'] ?? null) === $planned['expected'], 'typed failure differs');
            }
        }

        $outcomes = array_column($records, 'outcome');
        self::require(count(array_filter($outcomes, static fn (string $outcome): bool => $outcome === 'success')) === 4, 'success count differs');
        self::require($records[2]['outcome'] === 'success' && $records[5]['outcome'] === 'success', 'post-failure recovery job failed');

        $offline = [$records[0], $records[2], $records[5]];
        foreach (['pdf', 'document_sha256', 'render_id', 'resolved_input_hash', 'font_resources'] as $field) {
            self::require($offline[0][$field] === $offline[1][$field] && $offline[1][$field] === $offline[2][$field], "offline recovery identity differs: {$field}");
        }
        $live = $records[3]['resources'] ?? [];
        self::require(array_column($live, 'sha256') === [$cssSha256, $fontSha256], 'live CSS/WOFF2 identities differ');
        self::require(count(array_unique(array_column($live, 'origin'))) === 2, 'live resource origins differ');
    }

    private static function verifyPublicComposerInstall(string $version): void
    {
        $composer = self::readJson(base_path('composer.json'));
        foreach ($composer['repositories'] ?? [] as $repository) {
            self::require(!is_array($repository) || ($repository['type'] ?? null) !== 'path', 'path Composer repositories are forbidden in the release rehearsal');
        }
        self::require(($composer['config']['preferred-install'] ?? null) === 'dist', 'release rehearsal must prefer Composer distributions');
        $lock = self::readJson(base_path('composer.lock'));
        $packages = [];
        foreach ($lock['packages'] ?? [] as $package) {
            if (in_array($package['name'] ?? null, self::PACKAGES, true)) {
                $packages[$package['name']] = $package;
            }
        }
        foreach (self::PACKAGES as $name) {
            self::require(InstalledVersions::isInstalled($name), "{$name} is not installed");
            self::require(ltrim((string) InstalledVersions::getPrettyVersion($name), 'v') === $version, "{$name} version differs");
            $package = $packages[$name] ?? null;
            self::require(is_array($package) && ltrim((string) ($package['version'] ?? ''), 'v') === $version, "{$name} lock version differs");
            self::require(($package['dist']['type'] ?? null) === 'zip' && filter_var($package['dist']['url'] ?? null, FILTER_VALIDATE_URL), "{$name} was not locked from a public distribution");
        }
    }

    private static function readRecords(string $report): array
    {
        $paths = glob("{$report}/jobs/*.json") ?: [];
        sort($paths, SORT_STRING);

        return array_map(self::readJson(...), $paths);
    }

    private static function readJson(string $path): array
    {
        self::require(is_file($path), "missing JSON file: {$path}");
        $value = json_decode((string) file_get_contents($path), true, flags: JSON_THROW_ON_ERROR);
        self::require(is_array($value), "JSON file is not an object: {$path}");

        return $value;
    }

    private static function readJsonLines(string $path): array
    {
        self::require(is_file($path), "missing JSON lines file: {$path}");
        $rows = [];
        foreach (file($path, FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES) ?: [] as $line) {
            $value = json_decode($line, true, flags: JSON_THROW_ON_ERROR);
            self::require(is_array($value), "JSON line is not an object: {$path}");
            $rows[] = $value;
        }

        return $rows;
    }

    private static function writeJson(string $path, array $value): void
    {
        self::mkdir(dirname($path));
        $temporary = $path.'.'.bin2hex(random_bytes(4)).'.tmp';
        $json = json_encode($value, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)."\n";
        self::require(file_put_contents($temporary, $json, LOCK_EX) !== false && rename($temporary, $path), "cannot publish JSON evidence: {$path}");
    }

    private static function mkdir(string $path): void
    {
        self::require(is_dir($path) || mkdir($path, 0700, true), "cannot create directory: {$path}");
    }

    private static function treeBytes(string $root): int
    {
        $bytes = 0;
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS),
        );
        foreach ($iterator as $entry) {
            if ($entry->isFile() && !$entry->isLink()) {
                $bytes += $entry->getSize();
            }
        }

        return $bytes;
    }

    private static function processSnapshot(): array
    {
        $processes = [];
        foreach (glob('/proc/[0-9]*/comm') ?: [] as $path) {
            $name = trim((string) @file_get_contents($path));
            if (stripos($name, 'pliego') === false && $name !== 'Xvfb') {
                continue;
            }
            $pid = basename(dirname($path));
            $processes["{$pid}:{$name}"] = ['pid' => (int) $pid, 'name' => $name];
        }
        ksort($processes, SORT_STRING);

        return $processes;
    }

    private static function waitForProcessExit(array $baseline): array
    {
        for ($attempt = 0; $attempt < 20; $attempt++) {
            $snapshot = self::processSnapshot();
            if (array_diff(array_keys($snapshot), array_keys($baseline)) === []) {
                return $snapshot;
            }
            usleep(100_000);
        }

        return self::processSnapshot();
    }

    private static function peakRss(string $path): int
    {
        $contents = (string) file_get_contents($path);
        self::require(preg_match('/Maximum resident set size \(kbytes\):\s*(\d+)/', $contents, $match) === 1, 'GNU time omitted peak RSS');

        return (int) $match[1];
    }

    private static function fetchLocalFixture(string $url): string
    {
        $context = stream_context_create(['http' => ['follow_location' => 0, 'ignore_errors' => true, 'timeout' => 3]]);
        $contents = @file_get_contents($url, false, $context);
        self::require(is_string($contents) && str_contains($http_response_header[0] ?? '', ' 200 '), "cannot fetch local fixture: {$url}");

        return $contents;
    }

    private static function localOrigin(string $url, string $label): string
    {
        $parts = parse_url($url);
        self::require(
            is_array($parts)
            && ($parts['scheme'] ?? null) === 'http'
            && in_array($parts['host'] ?? null, ['127.0.0.1', 'localhost'], true)
            && isset($parts['port'])
            && ($parts['path'] ?? '/') !== '/'
            && !isset($parts['user'], $parts['pass'], $parts['query'], $parts['fragment']),
            "{$label} fixture must be an explicit local HTTP URL",
        );

        return "http://{$parts['host']}:{$parts['port']}/";
    }

    private static function requiredString(Command $command, string $name): string
    {
        $value = trim((string) $command->option($name));
        self::require($value !== '' && !str_contains($value, "\0"), "--{$name} is required");

        return $value;
    }

    private static function requiredHash(Command $command, string $name): string
    {
        $value = strtolower(self::requiredString($command, $name));
        $value = str_starts_with($value, 'sha256:') ? substr($value, 7) : $value;
        self::require(preg_match('/^[0-9a-f]{64}$/D', $value) === 1, "--{$name} must be SHA-256 hex");

        return $value;
    }

    private static function isAbsolutePath(string $path): bool
    {
        return str_starts_with($path, '/') || preg_match('/^[A-Za-z]:[\\\\\/]/', $path) === 1;
    }

    private static function require(bool $condition, string $message): void
    {
        if (!$condition) {
            throw new RuntimeException($message);
        }
    }
}

if (realpath((string) ($_SERVER['SCRIPT_FILENAME'] ?? '')) === __FILE__) {
    if (($argv[1] ?? null) !== '--self-test' || isset($argv[2])) {
        fwrite(STDERR, "usage: php rehearsal.php --self-test\n");
        exit(2);
    }
    PliegoQueueRehearsal::selfTest();
    fwrite(STDOUT, "Pliego six-job queue rehearsal self-test passed.\n");
}
