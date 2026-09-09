<?php

declare(strict_types=1);

// Template-derived compatibility inputs, not a replacement for the application action.
use App\Support\InvoiceItem;
use Carbon\Carbon;
use Illuminate\Contracts\Console\Kernel;
use LaravelDaily\Invoices\Classes\Party;
use LaravelDaily\Invoices\Invoice;
use Symfony\Component\Process\Process;

$args = getopt('', ['app:', 'output:']);
$appRoot = realpath($args['app'] ?? '');
$output = $args['output'] ?? '';
if (!$appRoot || !is_file($appRoot.'/vendor/autoload.php') || $output === '' || file_exists($output)) {
    fwrite(STDERR, "Usage: php freeze_invobook.php --app CHECKOUT --output NEW_DIRECTORY\n");
    exit(2);
}
require $appRoot.'/vendor/autoload.php';
$revision = new Process(['git', 'rev-parse', 'HEAD'], $appRoot);
$revision->mustRun();
$sha = trim($revision->getOutput());
if ($sha !== 'e5f666cef63543beffadfcc045f6af673408a02e') {
    throw new RuntimeException('This fixture builder requires the inspected Invobook commit.');
}

// Do not activate Livewire's Mockery-dependent unit-test boot path with production dependencies.
foreach (['APP_ENV' => 'benchmark', 'APP_KEY' => 'base64:'.base64_encode(str_repeat('f', 32)),
    'APP_URL' => 'http://fixture.invalid', 'CACHE_DRIVER' => 'array', 'CACHE_STORE' => 'array',
    'SESSION_DRIVER' => 'array', 'MAIL_MAILER' => 'array', 'QUEUE_CONNECTION' => 'sync',
    'DB_CONNECTION' => 'sqlite', 'DB_DATABASE' => ':memory:'] as $key => $value) {
    putenv($key.'='.$value);
    $_ENV[$key] = $_SERVER[$key] = $value;
}
$app = require $appRoot.'/bootstrap/app.php';
$app->make(Kernel::class)->bootstrap();
config(['app.locale' => 'en', 'app.timezone' => 'UTC', 'app.name' => 'Invobook Fixture']);
date_default_timezone_set('UTC');
Carbon::setTestNow(Carbon::parse('2026-09-04 12:00:00', 'UTC'));
$app['session']->driver()->put('_token', str_repeat('f', 40));

if (!mkdir($output, 0700, true)) {
    throw new RuntimeException('Cannot create output directory.');
}
$sources = ['composer.lock', 'package-lock.json', 'resources/css/app.css', 'resources/js/app.js',
    'vite.config.js', 'tailwind.config.js', 'app/Support/InvoiceItem.php',
    'app/Actions/GenerateInvoicePdf.php'];
$sourceHashes = [];
foreach ($sources as $source) {
    $sourceHashes[$source] = hash_file('sha256', $appRoot.'/'.$source);
}
$manifestPath = $appRoot.'/public/build/manifest.json';
$builtHashes = ['manifest.json' => hash_file('sha256', $manifestPath)];
foreach (glob($appRoot.'/public/build/assets/*') ?: [] as $file) {
    if (is_file($file)) {
        $builtHashes['assets/'.basename($file)] = hash_file('sha256', $file);
    }
}
$inventory = [];
foreach (['default', 'simple', 'elegant'] as $template) {
    $directory = $output.'/'.$template;
    mkdir($directory, 0700);
    $templatePath = 'resources/views/vendor/invoices/templates/'.$template.'.blade.php';
    $templateBytes = file_get_contents($appRoot.'/'.$templatePath);
    $gitTemplate = new Process(['git', 'show', $sha.':'.$templatePath], $appRoot);
    $gitTemplate->mustRun();
    // Accept checkout newline conversion only; do not permit authored changes.
    if (str_replace("\r\n", "\n", $templateBytes) !== str_replace("\r\n", "\n", $gitTemplate->getOutput())) {
        throw new RuntimeException('Template differs from the pinned upstream source: '.$template);
    }
    $provenance = [
        'repository' => 'https://github.com/Hasnayeen/invobook', 'commit' => $sha,
        'track' => 'template-derived-original-html', 'template' => $templatePath,
        'templateSha256' => hash('sha256', $templateBytes), 'sourceHashes' => $sourceHashes,
        'builtAssetHashes' => $builtHashes, 'phpVersion' => PHP_VERSION,
        'laravelVersion' => Illuminate\Foundation\Application::VERSION,
        'browsershotVersion' => Composer\InstalledVersions::getPrettyVersion('spatie/browsershot'),
        'adaptations' => [], 'applicationActionExercised' => false,
        'data' => ['date' => '2026-09-04', 'serial' => 'PLIEGO000001', 'subtotal' => 375, 'vat' => 75, 'total' => 450],
    ];
    try {
        $items = [];
        foreach ([['Design audit', 125, '01:00'], ['Implementation review', 250, '02:00']] as [$title, $amount, $hours]) {
            $items[] = (new InvoiceItem)->title($title)->pricePerUnit(125)->quantity(1)
                ->subTotalPrice($amount)->hours($hours)->project('Website renewal', 'fixture-project')
                ->task(null)->totalDuration($amount === 125 ? 3600 : 7200);
        }
        $invoice = Invoice::make('Invoice')->template($template)->series('PLIEGO000001')
            ->serialNumberFormat('{SERIES}')->date(Carbon::parse('2026-09-04'))->payUntilDays(3)
            ->seller(new Party(['name' => 'Fixture Studio', 'email' => 'seller@example.invalid', 'address' => '10 Example Street']))
            ->buyer(new Party(['name' => 'Acme Fixture Ltd', 'email' => 'buyer@example.invalid', 'address' => "20 Sample Road\nExample City"]))
            ->currencySymbol('€')->currencyCode('EUR')->currencyFormat('{SYMBOL}{VALUE}')
            ->notes('September services')->addItems($items)->totalTaxes(75)
            ->setCustomData(['from' => '2026-09-01', 'to' => '2026-09-30', 'client_id' => 'fixture-client'])
            ->calculate();
        $html = $invoice->toHtml()->render();
        file_put_contents($directory.'/input.html', $html);
        preg_match_all('~(?:src|href)=["\'](https?://[^"\']+)["\']~', $html, $remote);
        $fixture = ['schema' => 'pliego.application-fixture.v1', 'id' => 'invobook-'.$template,
            'assets' => [], 'expected' => ['textContains' => ['PLIEGO000001', 'Acme Fixture Ltd',
                'Design audit', 'Implementation review', '€450.00']],
            'provenance' => $provenance, 'inputSha256' => hash('sha256', $html),
            'externalResources' => array_values(array_unique($remote[1])),
            'acceptance' => 'blocker-census; visual, font and pagination acceptance pending'];
        file_put_contents($directory.'/fixture.json', json_encode($fixture, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n");
        $inventory[] = ['template' => $template, 'status' => 'frozen', 'inputSha256' => $fixture['inputSha256'],
            'externalResources' => $fixture['externalResources']];
    } catch (Throwable $error) {
        $failure = ['template' => $template, 'status' => 'template_data_contract_failure',
            'class' => get_class($error), 'message' => $error->getMessage(), 'provenance' => $provenance];
        file_put_contents($directory.'/failure.json', json_encode($failure, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n");
        $inventory[] = $failure;
    }
}
file_put_contents($output.'/inventory.json', json_encode($inventory, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n");
echo json_encode(['output' => realpath($output), 'templates' => array_map(
    static fn (array $item): array => array_intersect_key($item, array_flip(['template', 'status', 'message', 'externalResources'])), $inventory
)], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n";
