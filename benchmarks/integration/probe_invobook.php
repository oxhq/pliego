<?php

declare(strict_types=1);

/**
 * Exercise Invobook's original invoice action without launching a PDF renderer.
 *
 * Usage: php probe_invobook.php --app PATH --output NEW_DIRECTORY
 * The output directory owns the SQLite database, Laravel caches/logs, and report.
 * Application/template failures are evidence, not repaired or counted as PDFs.
 */

final class InvobookProbeRendererBlocked extends RuntimeException {}

final class InvobookProbeBlockedBrowsershot
{
    public static function __callStatic(string $method, array $arguments): never
    {
        throw new InvobookProbeRendererBlocked('Browsershot::'.$method.' is forbidden in the HTML-only baseline probe.');
    }
}

function probeWrite(string $path, string $bytes): void
{
    if (file_put_contents($path, $bytes, LOCK_EX) !== strlen($bytes)) {
        throw new RuntimeException('Could not write probe artifact: '.$path);
    }
}

function probeError(Throwable $error, string $appPath): array
{
    return [
        'class' => get_class($error),
        'message' => $error->getMessage(),
        'file' => str_replace(str_replace('\\', '/', $appPath).'/', '', str_replace('\\', '/', $error->getFile())),
        'line' => $error->getLine(),
    ];
}

$options = getopt('', ['app:', 'output:']);
if (!isset($options['app'], $options['output']) || !is_string($options['app']) || !is_string($options['output'])) {
    fwrite(STDERR, "Usage: php probe_invobook.php --app PATH --output NEW_DIRECTORY\n");
    exit(2);
}

$appPath = realpath($options['app']);
if ($appPath === false || !is_file($appPath.'/vendor/autoload.php') || !is_file($appPath.'/bootstrap/app.php')) {
    fwrite(STDERR, "--app must identify an installed Invobook checkout.\n");
    exit(2);
}
if (file_exists($options['output']) || is_link($options['output'])) {
    fwrite(STDERR, "--output must be a new directory; existing evidence is never overwritten.\n");
    exit(2);
}
$outputParent = realpath(dirname($options['output']));
if ($outputParent === false) {
    fwrite(STDERR, "The parent of --output must already exist.\n");
    exit(2);
}
$outputPath = $outputParent.DIRECTORY_SEPARATOR.basename($options['output']);
$normalizedApp = strtolower(str_replace('\\', '/', $appPath));
$normalizedOutput = strtolower(str_replace('\\', '/', $outputPath));
if ($normalizedOutput === $normalizedApp || str_starts_with($normalizedOutput, $normalizedApp.'/')) {
    fwrite(STDERR, "--output must be outside the application checkout.\n");
    exit(2);
}
if (!mkdir($outputPath, 0777)) {
    fwrite(STDERR, "Could not create the output directory.\n");
    exit(2);
}

$templates = ['default', 'simple', 'elegant'];
$sourceFiles = [
    'composer.lock',
    'app/Actions/GenerateInvoicePdf.php',
    'app/Actions/CreateInvoice.php',
    'app/Models/Invoice.php',
    'app/Filament/App/Resources/InvoiceResource/Pages/InvoiceTemplatePreview.php',
    'resources/views/components/layouts/invoice.blade.php',
];
foreach ($templates as $template) {
    $sourceFiles[] = 'resources/views/vendor/invoices/templates/'.$template.'.blade.php';
}
$sourceHashes = [];
$report = [
    'schema' => 'pliego.invobook-original-action-probe.v1',
    'php_version' => PHP_VERSION,
    'app_path' => $appPath,
    'status' => 'not_started',
    'phase' => 'prepare',
    'proof_boundary' => [
        'original_action' => 'App\\Actions\\GenerateInvoicePdf',
        'return_mode' => 'App\\Enums\\InvoiceResponseType::HTML',
        'url_delivery_exercised' => false,
        'pdf_renderer_invoked' => false,
        'browsershot_guard' => 'class alias throws before any renderer method can execute',
        'application_repairs' => [],
        'app_environment' => 'benchmark (production dependencies; avoids Livewire testing-only Mockery dependency)',
        'external_services' => 'none; SQLite and process-local array/sync drivers',
        'case_isolation' => 'original action inside a rolled-back fixture transaction',
    ],
    'cases' => array_map(fn (string $template): array => ['template' => $template, 'status' => 'not_run'], $templates),
];

try {
    foreach ($sourceFiles as $source) {
        $hash = hash_file('sha256', $appPath.'/'.$source);
        if ($hash === false) {
            throw new RuntimeException('Missing source file: '.$source);
        }
        $sourceHashes[$source] = $hash;
    }
    foreach (['environment', 'bootstrap/cache', 'storage/app/public', 'storage/framework/cache/data', 'storage/framework/sessions', 'storage/framework/views', 'storage/logs'] as $directory) {
        if (!mkdir($outputPath.'/'.$directory, 0777, true) && !is_dir($outputPath.'/'.$directory)) {
            throw new RuntimeException('Could not create isolated Laravel directory: '.$directory);
        }
    }
    $databasePath = $outputPath.'/invobook.sqlite';
    probeWrite($databasePath, '');
    $environment = [
        'APP_BASE_PATH' => $appPath,
        'APP_ENV' => 'benchmark',
        'APP_DEBUG' => 'false',
        'APP_URL' => 'http://invobook.invalid',
        'APP_KEY' => 'base64:'.base64_encode(str_repeat('p', 32)),
        'DB_CONNECTION' => 'sqlite',
        'DB_DATABASE' => $databasePath,
        'DB_FOREIGN_KEYS' => 'true',
        'DATABASE_URL' => '',
        'CACHE_DRIVER' => 'array',
        'CACHE_STORE' => 'array',
        'SESSION_DRIVER' => 'array',
        'QUEUE_CONNECTION' => 'sync',
        'MAIL_MAILER' => 'array',
        'BROADCAST_DRIVER' => 'log',
        'TELESCOPE_ENABLED' => 'false',
        'LOG_CHANNEL' => 'single',
        'VIEW_COMPILED_PATH' => $outputPath.'/storage/framework/views',
        'APP_CONFIG_CACHE' => $outputPath.'/bootstrap/cache/config.php',
        'APP_SERVICES_CACHE' => $outputPath.'/bootstrap/cache/services.php',
        'APP_PACKAGES_CACHE' => $outputPath.'/bootstrap/cache/packages.php',
        'APP_ROUTES_CACHE' => $outputPath.'/bootstrap/cache/routes.php',
        'APP_EVENTS_CACHE' => $outputPath.'/bootstrap/cache/events.php',
    ];
    foreach ($environment as $name => $value) {
        putenv($name.'='.$value);
        $_ENV[$name] = $value;
        $_SERVER[$name] = $value;
    }
    chdir($appPath);
    require $appPath.'/vendor/autoload.php';
    if (class_exists(Spatie\Browsershot\Browsershot::class, false)) {
        throw new RuntimeException('Browsershot was loaded before the no-renderer guard could be installed.');
    }
    if (!class_alias(InvobookProbeBlockedBrowsershot::class, Spatie\Browsershot\Browsershot::class)) {
        throw new RuntimeException('Could not install the no-renderer guard.');
    }
    $report['phase'] = 'bootstrap';
    $app = require $appPath.'/bootstrap/app.php';
    if (preg_match('/^[A-Za-z]:/', $outputPath) === 1) {
        // Laravel 11's cache path prefix list omits Windows drive letters.
        $app->addAbsoluteCachePathPrefix(substr($outputPath, 0, 2));
        $app->instance(Illuminate\Foundation\PackageManifest::class, new Illuminate\Foundation\PackageManifest(
            new Illuminate\Filesystem\Filesystem(), $appPath, $app->getCachedPackagesPath(),
        ));
    }
    $app->useStoragePath($outputPath.'/storage');
    $app->useEnvironmentPath($outputPath.'/environment');
    $kernel = $app->make(Illuminate\Contracts\Console\Kernel::class);
    $kernel->bootstrap();
    Illuminate\Support\Facades\Http::preventStrayRequests();
    Carbon\Carbon::setTestNow(Carbon\Carbon::parse('2026-09-04 12:00:00', config('app.timezone')));

    $report['phase'] = 'migrate';
    $migrationExit = $kernel->call('migrate', ['--force' => true, '--no-interaction' => true]);
    probeWrite($outputPath.'/migration.log', $kernel->output());
    if ($migrationExit !== 0) {
        throw new RuntimeException('Original migrations failed with exit code '.$migrationExit.'; see migration.log.');
    }

    $report['phase'] = 'seed';
    $sellerUser = App\Models\User::create([
        'id' => '01K4A000000000000000000001',
        'name' => 'Fixture Seller',
        'email' => 'seller@example.test',
        'password' => password_hash('probe-only-local-fixture', PASSWORD_BCRYPT, ['cost' => 4]),
        'email_verified_at' => now(),
    ]);
    auth()->login($sellerUser);
    $team = App\Models\Team::create([
        'id' => '01K4A000000000000000000002',
        'name' => 'Fixture Team',
        'user_id' => $sellerUser->id,
    ]);
    Filament\Facades\Filament::setCurrentPanel(Filament\Facades\Filament::getPanel('app'));
    Filament\Facades\Filament::setTenant($team, true);
    $client = App\Models\Client::create([
        'id' => '01K4A000000000000000000003',
        'name' => 'Fixture Buyer',
        'email' => 'buyer@example.test',
        'address' => "123 Fixture Street\nExample City",
        'user_id' => $sellerUser->id,
        'team_id' => $team->id,
    ]);
    foreach ([['Fixture consultation', 3600, '09:00:00', '10:00:00'], ['Fixture implementation', 7200, '10:00:00', '12:00:00']] as [$description, $duration, $start, $end]) {
        App\Models\WorkSession::create([
            'description' => $description,
            'duration' => $duration,
            'start' => '2026-09-01 '.$start,
            'end' => '2026-09-01 '.$end,
            'rate_in_cents' => 12500,
            'currency' => 'EUR',
            'user_id' => $sellerUser->id,
            'team_id' => $team->id,
            'task_id' => null,
            'project_id' => null,
        ]);
    }
    $items = App\Models\WorkSession::query()->selectWorkSessions()->withSubtotal()->withTotalDuration()->get();
    $report['fixture'] = [
        'work_sessions' => $items->map(fn ($item): array => ['item' => $item->item, 'seconds' => (int) $item->total_duration, 'rate_in_cents' => (int) $item->rate_in_cents, 'subtotal' => (float) $item->subtotal])->all(),
        'expected_subtotal_eur' => 375,
        'timezone' => config('app.timezone'),
        'frozen_now' => now()->toIso8601String(),
    ];
    if ($items->count() !== 2 || (float) $items->sum('subtotal') !== 375.0) {
        throw new RuntimeException('Synthetic work-session query did not produce the expected two rows and EUR 375 subtotal.');
    }

    $report['phase'] = 'original_action_html';
    foreach ($templates as $index => $template) {
        $case = ['template' => $template, 'phase' => 'invoke_original_action', 'status' => 'not_run'];
        Illuminate\Support\Facades\DB::beginTransaction();
        try {
            $result = (new App\Actions\GenerateInvoicePdf())(
                $items,
                ['client_id' => $client->id, 'template' => $template, 'vat' => 0, 'notes' => 'Pliego compatibility fixture'],
                ['from' => '2026-09-01', 'to' => '2026-09-30'],
                App\Enums\InvoiceResponseType::HTML,
            );
            $case['action_returned'] = true;
            $case['phase'] = 'render_returned_view';
            $html = $result instanceof Illuminate\Contracts\View\View ? $result->render() : $result;
            if (!is_string($html) || $html === '') {
                throw new RuntimeException('Original HTML action returned no nonempty HTML.');
            }
            probeWrite($outputPath.'/'.$template.'.html', $html);
            $case['status'] = 'html_rendered';
            $case['html_sha256'] = hash('sha256', $html);
            $case['html_bytes'] = strlen($html);
        } catch (Throwable $error) {
            $case['status'] = $error instanceof InvobookProbeRendererBlocked ? 'forbidden_renderer_attempt' : 'baseline_application_failure';
            $case['error'] = probeError($error, $appPath);
        } finally {
            Illuminate\Support\Facades\DB::rollBack();
            $case['fixture_transaction_rolled_back'] = true;
        }
        $report['cases'][$index] = $case;
    }
    $report['status'] = count(array_filter($report['cases'], fn (array $case): bool => $case['status'] !== 'html_rendered')) > 0
        ? 'completed_with_application_failures'
        : 'completed_html_only';
} catch (Throwable $error) {
    $report['status'] = 'environment_or_setup_blocked';
    $report['error'] = probeError($error, $appPath);
} finally {
    $report['source_sha256_before'] = $sourceHashes;
    $report['source_files_unchanged'] = true;
    foreach ($sourceHashes as $source => $hash) {
        if (hash_file('sha256', $appPath.'/'.$source) !== $hash) {
            $report['source_files_unchanged'] = false;
        }
    }
    $json = json_encode($report, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE | JSON_THROW_ON_ERROR)."\n";
    probeWrite($outputPath.'/report.json', $json);
    fwrite(STDOUT, $json);
}

exit($report['status'] === 'environment_or_setup_blocked' || !$report['source_files_unchanged'] ? 1 : 0);
