<?php

declare(strict_types=1);

/**
 * Separately labelled repaired Invobook action + Laravel storage experiment.
 * No original URL/auth-preview, queue, packaged-install or correctness claim.
 * Use --provider html to census all unchanged templates before PDF execution.
 */

function workflowWrite(string $path, string $bytes): void
{
    if (file_put_contents($path, $bytes, LOCK_EX) !== strlen($bytes)) {
        throw new RuntimeException('Artifact write failed: '.$path);
    }
}

function workflowGit(string $root, array $arguments): string
{
    $process = new Symfony\Component\Process\Process(['git', '-C', $root, ...$arguments]);
    $process->mustRun();

    return $process->getOutput();
}

function workflowFile(string $path): string
{
    $file = realpath($path);
    if ($file === false || !is_file($file)) {
        throw new RuntimeException('Required file missing: '.$path);
    }

    return $file;
}

function workflowSameBytes(string $first, string $second): bool
{
    return filesize($first) === filesize($second) && hash_file('sha256', $first) === hash_file('sha256', $second);
}

$options = getopt('', ['app:', 'output:', 'template:', 'provider:', 'sdk:', 'binary:', 'chrome:', 'node:', 'node-modules:']);
$phase = 'setup';
$output = null;
$report = ['schema' => 'pliego.invobook-repaired-workflow.v1', 'status' => 'setup_failure',
    'track' => 'shared-currency-repair-html-delivery', 'phpVersion' => PHP_VERSION, 'performanceQualified' => false];
$transaction = false;
$storagePath = null;
$sourceHashes = [];
try {
    foreach (['app', 'output', 'template', 'provider'] as $key) {
        if (!isset($options[$key]) || !is_string($options[$key]) || $options[$key] === '') {
            throw new RuntimeException('Missing --'.$key);
        }
    }
    $appPath = realpath($options['app']);
    if ($appPath === false || !is_dir($appPath)) {
        throw new RuntimeException('Application checkout missing');
    }
    $template = $report['template'] = $options['template'];
    $provider = $report['provider'] = $options['provider'];
    if (!in_array($template, ['default', 'simple', 'elegant'], true) || !in_array($provider, ['html', 'browsershot', 'pliego'], true)) {
        throw new RuntimeException('Unsupported template/provider');
    }
    $parent = realpath(dirname($options['output']));
    if ($parent === false || file_exists($options['output']) || is_link($options['output'])) {
        throw new RuntimeException('Output must be a fresh directory under an existing parent');
    }
    $output = $parent.DIRECTORY_SEPARATOR.basename($options['output']);
    if (str_starts_with(strtolower(str_replace('\\', '/', $output)).'/', strtolower(str_replace('\\', '/', $appPath)).'/')) {
        throw new RuntimeException('Evidence must be outside the application checkout');
    }
    require workflowFile($appPath.'/vendor/autoload.php');
    $pin = 'e5f666cef63543beffadfcc045f6af673408a02e';
    if (trim(workflowGit($appPath, ['rev-parse', 'HEAD'])) !== $pin) {
        throw new RuntimeException('Application must be pinned to '.$pin);
    }
    $modified = trim(workflowGit($appPath, ['diff', 'HEAD', '--name-only']));
    if ($modified !== 'app/Actions/CreateInvoice.php') {
        throw new RuntimeException('Only the recorded CreateInvoice currency repair is allowed');
    }
    $original = workflowGit($appPath, ['show', 'HEAD:app/Actions/CreateInvoice.php']);
    $needle = "                'amount_in_cents' => \$item->sub_total_price * 100,\n";
    $expected = str_replace($needle, $needle."                'currency' => \$data->currency_code,\n", $original, $count);
    $actual = str_replace("\r\n", "\n", file_get_contents($appPath.'/app/Actions/CreateInvoice.php'));
    if ($count !== 1 || $actual !== $expected) {
        throw new RuntimeException('Application repair does not exactly match the single recorded currency line');
    }
    mkdir($output, 0700);
    workflowWrite($output.'/application.patch', workflowGit($appPath, ['diff', '--no-ext-diff', '--no-color', 'HEAD', '--', 'app/Actions/CreateInvoice.php']));
    foreach (['app/Actions/CreateInvoice.php', 'app/Actions/GenerateInvoicePdf.php', 'composer.lock', 'package-lock.json',
        'resources/views/vendor/invoices/templates/'.$template.'.blade.php', 'resources/views/components/layouts/invoice.blade.php'] as $relative) {
        $sourceHashes[$relative] = hash_file('sha256', workflowFile($appPath.'/'.$relative));
    }
    $builtAssets = [];
    foreach ([$appPath.'/public/build/manifest.json', ...glob($appPath.'/public/build/assets/*')] as $file) {
        if (is_file($file)) {
            $builtAssets[substr(str_replace('\\', '/', $file), strlen(str_replace('\\', '/', $appPath)) + 1)] = hash_file('sha256', $file);
        }
    }
    $report['provenance'] = ['applicationCommit' => $pin, 'applicationPath' => $appPath,
        'runnerSha256' => hash_file('sha256', __FILE__), 'builtAssetSha256' => $builtAssets,
        'originalCreateInvoiceSha256' => hash('sha256', $original), 'repairPatchSha256' => hash_file('sha256', $output.'/application.patch'),
        'applicationSources' => $sourceHashes, 'templateChanged' => false, 'generateActionChanged' => false];
    $report['proofBoundary'] = [
        'businessAction' => 'unchanged App\\Actions\\GenerateInvoicePdf in HTML mode; CreateInvoice currency line repaired for both providers',
        'deliveryAdaptation' => 'returned Blade view delivered in-process; original URL, browser authentication and Livewire preview are not exercised',
        'storage' => 'isolated Laravel local disk; database commit only after stored bytes match render bytes',
        'sdk' => 'PHP SDK development source; Laravel SDK requires Illuminate ^13 while this app locks Laravel 11.31.0; no constraint bypass',
        'correctness' => 'storage integrity and database facts only; PDF facts and visual acceptance remain separate gates',
        'queue' => 'synchronous command; no queue-worker proof',
    ];
    foreach (['environment', 'bootstrap/cache', 'storage/app/documents', 'storage/framework/views', 'storage/logs'] as $relative) {
        mkdir($output.'/'.$relative, 0700, true);
    }
    workflowWrite($output.'/invobook.sqlite', '');
    $env = [
        'APP_BASE_PATH' => $appPath, 'APP_ENV' => 'benchmark', 'APP_DEBUG' => 'false', 'APP_URL' => 'http://invobook.invalid',
        'APP_KEY' => 'base64:'.base64_encode(str_repeat('p', 32)), 'DB_CONNECTION' => 'sqlite', 'DB_DATABASE' => $output.'/invobook.sqlite',
        'DB_FOREIGN_KEYS' => 'true', 'DATABASE_URL' => '', 'CACHE_DRIVER' => 'array', 'CACHE_STORE' => 'array',
        'SESSION_DRIVER' => 'array', 'QUEUE_CONNECTION' => 'sync', 'MAIL_MAILER' => 'array', 'BROADCAST_DRIVER' => 'log',
        'TELESCOPE_ENABLED' => 'false', 'LOG_CHANNEL' => 'single', 'VIEW_COMPILED_PATH' => $output.'/storage/framework/views',
    ];
    foreach (['CONFIG', 'SERVICES', 'PACKAGES', 'ROUTES', 'EVENTS'] as $cache) {
        $env['APP_'.$cache.'_CACHE'] = $output.'/bootstrap/cache/'.strtolower($cache).'.php';
    }
    foreach ($env as $key => $value) {
        putenv($key.'='.$value);
        $_ENV[$key] = $_SERVER[$key] = $value;
    }
    chdir($appPath);
    $app = require $appPath.'/bootstrap/app.php';
    if (preg_match('/^[A-Za-z]:/', $output) === 1) {
        $app->addAbsoluteCachePathPrefix(substr($output, 0, 2));
        $app->instance(Illuminate\Foundation\PackageManifest::class, new Illuminate\Foundation\PackageManifest(
            new Illuminate\Filesystem\Filesystem(), $appPath, $app->getCachedPackagesPath(),
        ));
    }
    $app->useStoragePath($output.'/storage')->useEnvironmentPath($output.'/environment');
    $kernel = $app->make(Illuminate\Contracts\Console\Kernel::class);
    $kernel->bootstrap();
    Illuminate\Support\Facades\Http::preventStrayRequests();
    Carbon\Carbon::setTestNow(Carbon\Carbon::parse('2026-09-04 12:00:00', config('app.timezone')));
    config(['filesystems.disks.benchmark' => ['driver' => 'local', 'root' => $output.'/storage/app/documents', 'throw' => true]]);
    $migrationExit = $kernel->call('migrate', ['--force' => true, '--no-interaction' => true]);
    workflowWrite($output.'/migration.log', $kernel->output());
    if ($migrationExit !== 0) {
        throw new RuntimeException('Original migrations failed');
    }
    $user = App\Models\User::create(['id' => '01K4A000000000000000000001', 'name' => 'Fixture Seller', 'email' => 'seller@example.test',
        'password' => password_hash('local-fixture-only', PASSWORD_BCRYPT, ['cost' => 4]), 'email_verified_at' => now()]);
    auth()->login($user);
    $team = App\Models\Team::create(['id' => '01K4A000000000000000000002', 'name' => 'Fixture Team', 'user_id' => $user->id]);
    Filament\Facades\Filament::setCurrentPanel(Filament\Facades\Filament::getPanel('app'));
    Filament\Facades\Filament::setTenant($team, true);
    $client = App\Models\Client::create(['id' => '01K4A000000000000000000003', 'name' => 'Fixture Buyer', 'email' => 'buyer@example.test',
        'address' => "123 Fixture Street\nExample City", 'user_id' => $user->id, 'team_id' => $team->id]);
    foreach ([['Fixture consultation', 3600, '09:00:00', '10:00:00'], ['Fixture implementation', 7200, '10:00:00', '12:00:00']] as [$description, $duration, $start, $end]) {
        App\Models\WorkSession::create(['description' => $description, 'duration' => $duration, 'start' => '2026-09-01 '.$start,
            'end' => '2026-09-01 '.$end, 'rate_in_cents' => 12500, 'currency' => 'EUR', 'user_id' => $user->id,
            'team_id' => $team->id, 'task_id' => null, 'project_id' => null]);
    }
    $items = App\Models\WorkSession::query()->selectWorkSessions()->withSubtotal()->withTotalDuration()->get();
    if ($items->count() !== 2 || (float) $items->sum('subtotal') !== 375.0) {
        throw new RuntimeException('Work-session fixture query did not produce EUR 375');
    }
    $sequence = 100;
    Illuminate\Support\Str::createUlidsUsing(static function () use (&$sequence) {
        return new Symfony\Component\Uid\Ulid('01K4A'.str_pad((string) $sequence++, 21, '0', STR_PAD_LEFT));
    });
    session()->put('_token', 'pliego-invobook-repaired-fixture');
    $phase = 'application_action';
    Illuminate\Support\Facades\DB::beginTransaction();
    $transaction = true;
    $started = hrtime(true);
    $view = (new App\Actions\GenerateInvoicePdf())($items,
        ['client_id' => $client->id, 'template' => $template, 'vat' => 75, 'notes' => 'Pliego compatibility fixture'],
        ['from' => '2026-09-01', 'to' => '2026-09-30'], App\Enums\InvoiceResponseType::HTML);
    if (!$view instanceof Illuminate\View\View) {
        throw new RuntimeException('Action did not return a Laravel view');
    }
    $phase = 'view_render';
    $html = $view->render();
    workflowWrite($output.'/input.html', $html);
    $report['inputSha256'] = hash('sha256', $html);
    $report['expectedPdfFacts'] = ['Fixture Seller', 'Fixture Buyer', 'Fixture consultation', 'Fixture implementation', '€450.00'];
    $report['databaseBeforeDelivery'] = App\Models\Invoice::firstOrFail()->only(['subtotal_in_cents', 'vat_in_cents', 'total_in_cents']);
    if (array_map('intval', array_values($report['databaseBeforeDelivery'])) !== [37500, 7500, 45000]
        || App\Models\InvoiceItem::count() !== 2 || App\Models\InvoiceItem::where('currency', 'EUR')->count() !== 2) {
        throw new RuntimeException('Repaired action produced incorrect persisted invoice facts');
    }
    if ($provider === 'html') {
        $report['status'] = 'html_action_passed';
        Illuminate\Support\Facades\DB::rollBack();
        $transaction = false;
    } else {
        $storagePath = 'invoices/fixture-'.$template.'.pdf';
        $disk = Illuminate\Support\Facades\Storage::disk('benchmark');
        $phase = 'render_and_store';
        if ($provider === 'pliego') {
            $sdk = realpath($options['sdk'] ?? '');
            if ($sdk === false) {
                throw new RuntimeException('--sdk must point to the Pliego sdk directory');
            }
            require workflowFile($sdk.'/php/vendor/autoload.php');
            $engine = new Pliego\Php\DocumentEngine([workflowFile($options['binary'] ?? '')], $output.'/jobs', 65, 65);
            $rendered = $engine->render($html,
                new Pliego\Php\RenderOptions(pageSize: 'A4', pageMargins: '0,0,0,0', diagnosticsRetention: 'always'));
            $sourcePdf = $rendered->pdfPath;
            $report['retainedJobPath'] = $rendered->jobPath;
            $report['renderApi'] = 'Pliego\\Php\\DocumentEngine::render';
        } else {
            $modules = realpath($options['node-modules'] ?? '');
            if ($modules === false || !is_file($modules.'/puppeteer/package.json')) {
                throw new RuntimeException('--node-modules must contain installed Puppeteer');
            }
            putenv('NODE_PATH='.$modules);
            $_SERVER['NODE_PATH'] = $modules;
            $shot = Spatie\Browsershot\Browsershot::htmlFromFilePath(str_replace('\\', '/', $output.'/input.html'))
                ->setChromePath(workflowFile($options['chrome'] ?? ''))->setNodeBinary(workflowFile($options['node'] ?? ''))
                ->setNodeModulePath($modules)->setCustomTempPath($output)->setUserDataDir($output.'/chrome-profile')
                ->format('A4')->margins(0, 0, 0, 0)->showBackground()->timeout(65)
                ->blockUrls(['http://', 'https://', 'ws://', 'wss://', 'ftp://'])->disableRedirects()
                ->addChromiumArguments(['disable-background-networking', 'no-first-run']);
            $sourcePdf = $output.'/rendered.pdf';
            $shot->savePdf($sourcePdf);
            $browser = $shot->getOutput();
            $requests = $browser?->getRequestsList() ?? [];
            $blocked = array_values(array_filter($requests, static fn (array $request): bool => in_array(
                strtolower((string) parse_url($request['url'], PHP_URL_SCHEME)), ['http', 'https', 'ws', 'wss', 'ftp'], true)));
            $report['browser'] = ['blockedExternalRequests' => $blocked, 'failedRequests' => $browser?->getFailedRequests() ?? [],
                'pageErrors' => $browser?->getPageErrors() ?? []];
            if ($blocked !== [] || $report['browser']['failedRequests'] !== [] || $report['browser']['pageErrors'] !== []) {
                throw new RuntimeException('Browser resource/script failure; PDF is retained but not delivered');
            }
            $report['renderApi'] = 'Spatie\\Browsershot\\Browsershot::savePdf';
        }
        $phase = 'storage';
        $stream = fopen($sourcePdf, 'rb');
        try {
            if (!is_resource($stream) || !$disk->writeStream($storagePath, $stream)) {
                throw new RuntimeException('Laravel local disk writeStream failed');
            }
        } finally {
            if (is_resource($stream)) {
                fclose($stream);
            }
        }
        $report['storageApi'] = 'Illuminate filesystem local disk writeStream';
        $phase = 'storage_readback';
        if (!workflowSameBytes($sourcePdf, $disk->path($storagePath))) {
            throw new RuntimeException('Stored PDF differs from validated/rendered source');
        }
        $report['storage'] = ['path' => $disk->path($storagePath), 'sha256' => hash_file('sha256', $sourcePdf),
            'bytes' => filesize($sourcePdf), 'readbackVerified' => true];
        Illuminate\Support\Facades\DB::commit();
        $transaction = false;
        $report['status'] = 'delivered_pending_pdf_acceptance';
    }
    $report['workflowWallMs'] = (hrtime(true) - $started) / 1e6;
} catch (Throwable $error) {
    $report['status'] = $phase === 'setup' ? 'setup_failure' : 'workflow_failure';
    $report['failurePhase'] = $phase;
    $report['error'] = ['class' => get_class($error), 'message' => $error->getMessage()];
    if ($error instanceof Pliego\Php\Exception\RenderFailedException) {
        $report['error']['kind'] = $error->kind;
        $report['retainedJobPath'] = $error->jobPath;
        $report['nativeResult'] = $error->result;
    }
} finally {
    if ($transaction) {
        Illuminate\Support\Facades\DB::rollBack();
    }
    if (isset($app) && $phase !== 'setup') {
        $report['persistedInvoiceCount'] = App\Models\Invoice::count();
        $report['persistedInvoiceItemCount'] = App\Models\InvoiceItem::count();
        if ($report['status'] === 'workflow_failure' && $storagePath !== null) {
            $disk = Illuminate\Support\Facades\Storage::disk('benchmark');
            $report['partialStoredObjectExisted'] = $disk->exists($storagePath);
            if ($report['partialStoredObjectExisted']) {
                $report['partialStoredObjectRemoved'] = $disk->delete($storagePath);
            }
        }
    }
    $report['sourceFilesUnchangedDuringRun'] = true;
    foreach ($sourceHashes as $relative => $hash) {
        if (hash_file('sha256', $appPath.'/'.$relative) !== $hash) {
            $report['sourceFilesUnchangedDuringRun'] = false;
        }
    }
    $json = json_encode($report, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE | JSON_THROW_ON_ERROR)."\n";
    if ($output !== null && is_dir($output)) {
        workflowWrite($output.'/report.json', $json);
    }
    fwrite(STDOUT, $json);
}

exit(in_array($report['status'], ['html_action_passed', 'delivered_pending_pdf_acceptance'], true) && $report['sourceFilesUnchangedDuringRun'] ? 0 : 1);
