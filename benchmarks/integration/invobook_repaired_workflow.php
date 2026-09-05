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

function workflowExactSet(array $actual, array $expected, string $message): void
{
    sort($actual, SORT_STRING);
    sort($expected, SORT_STRING);
    if ($actual !== $expected) {
        throw new RuntimeException($message);
    }
}

/** Pure guard, also exercised with deliberately invalid snapshots without editing an app. */
function workflowValidateModernizedSources(array $manifest, array $snapshot): void
{
    if ($snapshot['commit'] !== $manifest['applicationCommit']) {
        throw new RuntimeException('Modernized application source commit mismatch');
    }
    workflowExactSet($snapshot['modified'], array_keys($manifest['changedFiles']), 'Modernized application changed-file allowlist mismatch');
    workflowExactSet($snapshot['untracked'], $manifest['untracked'], 'Modernized application untracked-file allowlist mismatch');
    workflowExactSet(array_keys($snapshot['hashes']), array_keys($manifest['changedFiles']), 'Modernized application hash inventory mismatch');
    foreach ($manifest['changedFiles'] as $relative => $hash) {
        if ($snapshot['hashes'][$relative] !== $hash) {
            throw new RuntimeException('Modernized application source hash mismatch: '.$relative);
        }
    }
}

function workflowGitPaths(string $root, array $arguments): array
{
    return array_values(array_filter(explode("\0", workflowGit($root, $arguments)), static fn (string $path): bool => $path !== ''));
}

/** The caller cannot choose a manifest or supply permissive hashes. */
function workflowModernizedBaseline(string $appPath): array
{
    $manifestPath = __DIR__.'/invobook_modernized_baseline.json';
    $manifest = json_decode((string) file_get_contents(workflowFile($manifestPath)), true, flags: JSON_THROW_ON_ERROR);
    if ($manifest['schema'] !== 'pliego.invobook-modernized-baseline.v1' || $manifest['name'] !== 'modernized-laravel12') {
        throw new RuntimeException('Unexpected modernized baseline manifest');
    }
    $hashes = [];
    foreach ($manifest['changedFiles'] as $relative => $_hash) {
        $hashes[$relative] = hash_file('sha256', workflowFile($appPath.'/'.$relative));
    }
    workflowValidateModernizedSources($manifest, [
        'commit' => trim(workflowGit($appPath, ['rev-parse', 'HEAD'])),
        'modified' => workflowGitPaths($appPath, ['diff', '--no-ext-diff', 'HEAD', '--name-only', '-z']),
        'untracked' => workflowGitPaths($appPath, ['ls-files', '--others', '--exclude-standard', '-z']),
        'hashes' => $hashes,
    ]);
    $fork = $manifest['fork'];
    $forkPath = $appPath.'/'.$fork['path'];
    if (trim(workflowGit($forkPath, ['rev-parse', 'HEAD'])) !== $fork['sourceCommit']) {
        throw new RuntimeException('GlowChart source commit mismatch');
    }
    workflowExactSet(workflowGitPaths($forkPath, ['diff', '--no-ext-diff', 'HEAD', '--name-only', '-z']),
        array_keys($fork['changedFiles']), 'GlowChart changed-file allowlist mismatch');
    workflowExactSet(workflowGitPaths($forkPath, ['ls-files', '--others', '--exclude-standard', '-z']),
        array_keys($fork['untrackedFiles']), 'GlowChart untracked-file allowlist mismatch');
    foreach ($fork['changedFiles'] + $fork['untrackedFiles'] as $relative => $hash) {
        if (hash_file('sha256', workflowFile($forkPath.'/'.$relative)) !== $hash) {
            throw new RuntimeException('GlowChart overlay hash mismatch: '.$relative);
        }
    }
    $forkOriginal = json_decode(workflowGit($forkPath, ['show', 'HEAD:composer.json']), true, flags: JSON_THROW_ON_ERROR);
    $forkOriginal['require']['php'] = '^8.2';
    $forkOriginal['require']['illuminate/contracts'] = '^10.0|^11.0|^12.0';
    $forkOriginal['require']['flowframe/laravel-trend'] = '^0.5';
    if (json_decode((string) file_get_contents($forkPath.'/composer.json'), true, flags: JSON_THROW_ON_ERROR) !== $forkOriginal) {
        throw new RuntimeException('GlowChart differs from the three approved manifest constraints');
    }
    $forkSources = [];
    $mirrorSources = [];
    $paths = [...workflowGitPaths($forkPath, ['ls-files', '-z']), ...array_keys($fork['untrackedFiles'])];
    foreach ($paths as $relative) {
        $source = workflowFile($forkPath.'/'.$relative);
        $hash = hash_file('sha256', $source);
        $forkSources[$relative] = $hash;
        $hashes[$fork['path'].'/'.$relative] = $hash;
        if (str_starts_with($relative, 'src/') || str_starts_with($relative, 'resources/')
            || in_array($relative, ['composer.json', 'LICENSE.md', 'PLIEGO-COMPATIBILITY.md'], true)) {
            $mirror = 'vendor/'.$fork['applicationPackage'].'/'.$relative;
            if (is_link($appPath.'/'.$mirror) || !workflowSameBytes($source, workflowFile($appPath.'/'.$mirror))) {
                throw new RuntimeException('Installed GlowChart mirror differs from source: '.$relative);
            }
            $mirrorSources[$relative] = $hash;
            $hashes[$mirror] = $hash;
        }
    }
    $mirrorRoot = $appPath.'/vendor/'.$fork['applicationPackage'];
    if (is_link($mirrorRoot)) {
        throw new RuntimeException('Installed GlowChart must be a mirrored directory');
    }
    $mirrorPaths = ['composer.json', 'LICENSE.md', 'PLIEGO-COMPATIBILITY.md'];
    foreach (['src', 'resources'] as $directory) {
        $iterator = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($mirrorRoot.'/'.$directory, FilesystemIterator::SKIP_DOTS));
        foreach ($iterator as $entry) {
            if ($entry->isLink() || !$entry->isFile()) {
                throw new RuntimeException('Installed GlowChart runtime contains a non-regular file');
            }
            $mirrorPaths[] = substr(str_replace('\\', '/', $entry->getPathname()), strlen(str_replace('\\', '/', $mirrorRoot)) + 1);
        }
    }
    workflowExactSet($mirrorPaths, array_keys($mirrorSources), 'Installed GlowChart runtime inventory mismatch');
    $packages = [];
    foreach ($manifest['installedPackages'] as $package => $version) {
        if (ltrim((string) Composer\InstalledVersions::getPrettyVersion($package), 'v') !== $version) {
            throw new RuntimeException('Modernized installed package version mismatch: '.$package);
        }
        $packages[$package] = ['version' => $version, 'reference' => Composer\InstalledVersions::getReference($package)];
    }
    if ($packages[$fork['applicationPackage']]['reference'] !== $fork['pathReference']) {
        throw new RuntimeException('Installed GlowChart path reference mismatch');
    }
    foreach (['vendor/composer/installed.json', 'vendor/composer/installed.php'] as $relative) {
        $hashes[$relative] = hash_file('sha256', workflowFile($appPath.'/'.$relative));
    }
    return ['name' => $manifest['name'], 'manifestSha256' => hash_file('sha256', $manifestPath),
        'applicationCommit' => $manifest['applicationCommit'], 'approvedRootSources' => $manifest['changedFiles'],
        'forkCommit' => $fork['sourceCommit'], 'forkSources' => $forkSources, 'installedForkSources' => $mirrorSources,
        'installedPackages' => $packages, 'sourceHashes' => $hashes,
        'boundary' => 'Exact shared modernization; path reference is not full-source integrity; no security or PDF acceptance claim'];
}

if (defined('PLIEGO_INVOBOOK_WORKFLOW_LIBRARY') && PLIEGO_INVOBOOK_WORKFLOW_LIBRARY === true) {
    return;
}

$options = getopt('', ['app:', 'output:', 'template:', 'provider:', 'baseline:', 'sdk:', 'binary:', 'chrome:', 'node:', 'node-modules:', 'simple-pdf-media:', 'simple-pdf-repair:']);
$phase = 'setup';
$output = null;
$report = ['schema' => 'pliego.invobook-repaired-workflow.v1', 'status' => 'setup_failure',
    'track' => 'shared-currency-repair-html-delivery', 'phpVersion' => PHP_VERSION, 'performanceQualified' => false];
$transaction = false;
$storagePath = null;
$sourceHashes = [];
$deliveryAssets = [];
$modernization = null;
try {
    foreach (['app', 'output', 'template', 'provider'] as $key) {
        if (!isset($options[$key]) || !is_string($options[$key]) || $options[$key] === '') {
            throw new RuntimeException('Missing --'.$key);
        }
    }
    $baseline = $report['baseline'] = $options['baseline'] ?? 'historical';
    if (!in_array($baseline, ['historical', 'modernized-laravel12'], true)) {
        throw new RuntimeException('Unsupported baseline');
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
    $repairFull = isset($options['simple-pdf-repair']);
    if ($repairFull && ($template !== 'simple' || $options['simple-pdf-repair'] !== 'quantity-fonts')) {
        throw new RuntimeException('--simple-pdf-repair quantity-fonts is allowed only for the simple template');
    }
    $repairMedia = isset($options['simple-pdf-media']) || $repairFull;
    if (isset($options['simple-pdf-media']) && ($template !== 'simple' || $options['simple-pdf-media'] !== 'all')) {
        throw new RuntimeException('--simple-pdf-media all is allowed only for the simple template');
    }
    if ($repairMedia) {
        $report['track'] = 'shared-currency-and-simple-media-repair-html-delivery';
    }
    if ($repairFull) {
        $report['track'] = 'shared-currency-media-quantity-font-repair-html-delivery';
    }
    if ($baseline === 'modernized-laravel12') {
        $report['track'] = 'modernized-laravel12-'.$report['track'];
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
    if ($baseline === 'historical') {
        $modified = trim(workflowGit($appPath, ['diff', 'HEAD', '--name-only']));
        if ($modified !== 'app/Actions/CreateInvoice.php') {
            throw new RuntimeException('Only the recorded CreateInvoice currency repair is allowed');
        }
    } else {
        $modernization = workflowModernizedBaseline($appPath);
        $sourceHashes = $modernization['sourceHashes'];
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
    foreach (['app/Actions/CreateInvoice.php', 'app/Actions/GenerateInvoicePdf.php', 'composer.json', 'composer.lock', 'package.json', 'package-lock.json',
        'resources/views/vendor/invoices/templates/'.$template.'.blade.php', 'resources/views/components/layouts/invoice.blade.php'] as $relative) {
        $sourceHashes[$relative] = hash_file('sha256', workflowFile($appPath.'/'.$relative));
    }
    $builtAssets = [];
    foreach ([$appPath.'/public/build/manifest.json', ...glob($appPath.'/public/build/assets/*')] as $file) {
        if (is_file($file)) {
            $relative = substr(str_replace('\\', '/', $file), strlen(str_replace('\\', '/', $appPath)) + 1);
            $builtAssets[$relative] = $sourceHashes[$relative] = hash_file('sha256', $file);
        }
    }
    $report['provenance'] = ['applicationCommit' => $pin, 'applicationPath' => $appPath,
        'runnerSha256' => hash_file('sha256', __FILE__), 'builtAssetSha256' => $builtAssets,
        'originalCreateInvoiceSha256' => hash('sha256', $original), 'repairPatchSha256' => hash_file('sha256', $output.'/application.patch'),
        'applicationSources' => $sourceHashes, 'templateChanged' => false, 'generateActionChanged' => false];
    if ($modernization !== null) {
        $report['provenance']['modernization'] = $modernization;
    }
    if ($repairFull) {
        $relative = 'app/Actions/GenerateInvoicePdf.php';
        $originalAction = (string) file_get_contents(workflowFile($appPath.'/'.$relative));
        if (str_replace("\r\n", "\n", $originalAction) !== workflowGit($appPath, ['show', 'HEAD:'.$relative])) {
            throw new RuntimeException('GenerateInvoicePdf differs from the pinned source');
        }
        $newline = str_contains($originalAction, "\r\n") ? "\r\n" : "\n";
        $before = '                    ->pricePerUnit($item->rate_in_cents / 100)';
        $addition = '                    ->quantity($item->total_duration / 3600)';
        $repairedAction = str_replace($before, $before.$newline.$addition, $originalAction, $count);
        if ($count !== 1 || class_exists(App\Actions\GenerateInvoicePdf::class, false)) {
            throw new RuntimeException('Quantity repair requires one exact source site and an unloaded action class');
        }
        $line = substr_count(substr($originalAction, 0, strpos($originalAction, $before)), "\n") + 1;
        $patch = "--- a/{$relative}\n+++ b/{$relative}\n@@ -{$line} +{$line},2 @@\n {$before}\n+{$addition}\n";
        mkdir($output.'/action-override', 0700);
        workflowWrite($output.'/action.original.php', $originalAction);
        workflowWrite($output.'/action-override/GenerateInvoicePdf.php', $repairedAction);
        workflowWrite($output.'/action-repair.patch', $patch);
        require $output.'/action-override/GenerateInvoicePdf.php';
        if (realpath((new ReflectionClass(App\Actions\GenerateInvoicePdf::class))->getFileName()) !== realpath($output.'/action-override/GenerateInvoicePdf.php')) {
            throw new RuntimeException('Quantity-repaired action did not load from its retained source');
        }
        $report['provenance']['generateActionChanged'] = true;
        $report['provenance']['actionRepair'] = ['name' => 'hour-duration-quantity', 'sourcePath' => $relative,
            'originalSha256' => hash('sha256', $originalAction), 'repairedSha256' => hash('sha256', $repairedAction),
            'patchSha256' => hash('sha256', $patch), 'replacementCount' => $count,
            'addition' => trim($addition), 'applicationActionUnchanged' => true];
        mkdir($output.'/fonts', 0700);
        $fontSources = [
            'DejaVuSans.ttf' => '7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954',
            'DejaVuSans-Bold.ttf' => 'e6476c1b80502924294eed40894c5b18e06c181444ca953e5334262df9c27724',
        ];
        $fontEvidence = [];
        foreach ($fontSources as $name => $expectedSha256) {
            $relative = 'vendor/dompdf/dompdf/lib/fonts/'.$name;
            $source = workflowFile($appPath.'/'.$relative);
            if (hash_file('sha256', $source) !== $expectedSha256) {
                throw new RuntimeException('Original bundled font hash mismatch: '.$name);
            }
            $font = FontLib\Font::load($source);
            $font->parse();
            $license = $font->getNameTableString(13);
            if ($font->getFontName() !== 'DejaVu Sans' || $font->getFontVersion() !== 'Version 2.37' || !is_string($license) || $license === '') {
                throw new RuntimeException('Bundled font identity or embedded license is missing');
            }
            workflowWrite($output.'/fonts/'.$name, (string) file_get_contents($source));
            workflowWrite($output.'/fonts/'.$name.'.LICENSE.txt', $font->getFontCopyright()."\n".$license."\n");
            $deliveryAssets['fonts/'.$name] = $output.'/fonts/'.$name;
            $sourceHashes[$relative] = $expectedSha256;
            $fontEvidence[] = ['sourcePath' => $relative, 'path' => 'fonts/'.$name, 'sha256' => $expectedSha256,
                'family' => $font->getFontName(), 'postscriptName' => $font->getFontPostscriptName(),
                'version' => $font->getFontVersion(), 'licenseSource' => 'original font name table records 0 and 13',
                'licenseSha256' => hash_file('sha256', $output.'/fonts/'.$name.'.LICENSE.txt')];
        }
        $report['provenance']['fontRepair'] = ['name' => 'original-bundled-dejavu-resource-closure',
            'package' => 'dompdf/dompdf', 'version' => Composer\InstalledVersions::getPrettyVersion('dompdf/dompdf'),
            'reference' => Composer\InstalledVersions::getReference('dompdf/dompdf'), 'fonts' => $fontEvidence];
        $report['provenance']['applicationSources'] = $sourceHashes;
        workflowWrite($output.'/font-provenance.json', json_encode($report['provenance']['fontRepair'], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n");
    }
    if ($repairMedia) {
        $relative = 'resources/views/vendor/invoices/templates/simple.blade.php';
        $originalTemplate = (string) file_get_contents(workflowFile($appPath.'/'.$relative));
        if (str_replace("\r\n", "\n", $originalTemplate) !== workflowGit($appPath, ['show', 'HEAD:'.$relative])) {
            throw new RuntimeException('Simple template differs from the pinned source');
        }
        $mediaBefore = '<style type="text/css" media="screen">';
        $mediaAfter = '<style type="text/css" media="all">';
        $fontRules = $repairFull ? [
            '            @font-face { font-family: "DejaVu Sans"; font-style: normal; font-weight: 400; src: url("fonts/DejaVuSans.ttf") format("truetype"); }',
            '            @font-face { font-family: "DejaVu Sans"; font-style: normal; font-weight: 700; src: url("fonts/DejaVuSans-Bold.ttf") format("truetype"); }',
        ] : [];
        $newline = str_contains($originalTemplate, "\r\n") ? "\r\n" : "\n";
        $styleAfter = $mediaAfter.($repairFull ? $newline.implode($newline, $fontRules) : '');
        $repairedTemplate = str_replace($mediaBefore, $styleAfter, $originalTemplate, $count);
        if ($count !== 1) {
            throw new RuntimeException('Simple media repair requires exactly one recorded stylesheet tag');
        }
        mkdir($output.'/template-override/templates', 0700, true);
        workflowWrite($output.'/template.original.blade.php', $originalTemplate);
        workflowWrite($output.'/template-override/templates/simple.blade.php', $repairedTemplate);
        $hunk = $repairFull ? '@@ -7 +7,3 @@' : '@@ -7 +7 @@';
        $patch = "--- a/{$relative}\n+++ b/{$relative}\n{$hunk}\n"
            ."-        {$mediaBefore}\n+        {$mediaAfter}\n"
            .($repairFull ? '+'.implode("\n+", $fontRules)."\n" : '');
        workflowWrite($output.'/template-repair.patch', $patch);
        // The pinned application has no standalone LICENSE file; preserve its
        // README attribution and MIT declaration rather than inventing one.
        workflowWrite($output.'/application.README.md', workflowGit($appPath, ['show', 'HEAD:README.md']));
        $report['provenance']['templateChanged'] = true;
        $report['provenance']['templateRepair'] = [
            'name' => $repairFull ? 'simple-pdf-css-media-all-and-original-dejavu' : 'simple-pdf-css-media-all',
            'scope' => 'same output-owned Blade override for both providers',
            'sourcePath' => $relative, 'originalSha256' => hash('sha256', $originalTemplate),
            'repairedSha256' => hash('sha256', $repairedTemplate), 'patchSha256' => hash('sha256', $patch),
            'replacementCount' => $count, 'before' => $mediaBefore, 'after' => $mediaAfter,
            'applicationTemplateUnchanged' => true,
        ];
        if ($repairFull) {
            $report['provenance']['templateRepair']['fontFaceRules'] = $fontRules;
        }
    }
    $report['proofBoundary'] = [
        'businessAction' => ($repairFull ? 'exact quantity-setter repair in' : 'unchanged').' App\\Actions\\GenerateInvoicePdf in HTML mode; CreateInvoice currency line repaired for both providers',
        'deliveryAdaptation' => 'returned Blade view delivered in-process; original URL, browser authentication and Livewire preview are not exercised',
        'storage' => 'isolated Laravel local disk; database commit only after stored bytes match render bytes',
        'sdk' => $baseline === 'historical'
            ? 'PHP SDK development source; candidate Laravel SDK targets Illuminate 12/13 while this app locks Laravel 11.31.0; no constraint bypass'
            : 'PHP SDK development source in the exact shared Laravel 12.69.1 app; no packaged Laravel SDK install or constraint bypass claim',
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
    if ($repairMedia) {
        // Only this one view is shadowed; the real business action and every
        // other invoice view continue resolving from the installed application.
        $app['view']->getFinder()->prependNamespace('invoices', $output.'/template-override');
    }
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
    if ($repairMedia && realpath($view->getPath()) !== realpath($output.'/template-override/templates/simple.blade.php')) {
        throw new RuntimeException('Invoice action did not resolve the allowlisted simple Blade override');
    }
    $html = $view->render();
    workflowWrite($output.'/input.html', $html);
    $report['inputSha256'] = hash('sha256', $html);
    $report['expectedPdfFacts'] = ['Fixture Seller', 'Fixture Buyer', 'Fixture consultation', 'Fixture implementation', '€450.00'];
    if ($repairFull) {
        $invoice = $view->getData()['invoice'];
        $facts = $invoice->items->map(static fn ($item) => ['title' => $item->title, 'quantity' => (float) $item->quantity,
            'unitPrice' => (float) $item->price_per_unit, 'subtotal' => (float) $item->sub_total_price])->values()->all();
        $expectedFacts = [
            ['title' => 'Fixture consultation', 'quantity' => 1.0, 'unitPrice' => 125.0, 'subtotal' => 125.0],
            ['title' => 'Fixture implementation', 'quantity' => 2.0, 'unitPrice' => 125.0, 'subtotal' => 250.0],
        ];
        if ($facts !== $expectedFacts) {
            throw new RuntimeException('Quantity repair did not preserve the exact hour/rate/subtotal fixture equations');
        }
        $report['invoiceItemFacts'] = $facts;
        $report['expectedPdfFacts'][] = 'Fixture consultation 1 €125.00 €125.00';
        $report['expectedPdfFacts'][] = 'Fixture implementation 2 €125.00 €250.00';
        $report['expectedFontFamilies'] = ['DejaVuSans', 'DejaVuSans-Bold'];
    }
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
            $binary = workflowFile($options['binary'] ?? '');
            $engine = new Pliego\Php\DocumentEngine([$binary], $output.'/jobs', 65, 65);
            $report['providerIdentity'] = ['binaryPath' => $binary, 'binarySha256' => hash_file('sha256', $binary),
                'runtimeContract' => $engine->contract()->toArray(),
                'documentEngineSha256' => hash_file('sha256', workflowFile($sdk.'/php/src/DocumentEngine.php'))];
            $rendered = $engine->render($html,
                new Pliego\Php\RenderOptions(pageSize: 'A4', pageMargins: '0,0,0,0', diagnosticsRetention: 'always'), $deliveryAssets);
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
            $chrome = workflowFile($options['chrome'] ?? '');
            $node = workflowFile($options['node'] ?? '');
            $puppeteer = json_decode((string) file_get_contents($modules.'/puppeteer/package.json'), true, flags: JSON_THROW_ON_ERROR);
            $report['providerIdentity'] = [
                'browsershotVersion' => Composer\InstalledVersions::getPrettyVersion('spatie/browsershot'),
                'chromePath' => $chrome, 'chromeSha256' => hash_file('sha256', $chrome),
                'nodePath' => $node, 'nodeSha256' => hash_file('sha256', $node),
                'puppeteerVersion' => $puppeteer['version'],
                'puppeteerManifestSha256' => hash_file('sha256', $modules.'/puppeteer/package.json'),
            ];
            $shot = Spatie\Browsershot\Browsershot::htmlFromFilePath(str_replace('\\', '/', $output.'/input.html'))
                ->setChromePath($chrome)->setNodeBinary($node)
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
    if ($modernization !== null) {
        try {
            $report['modernizedBaselineUnchangedDuringRun'] = workflowModernizedBaseline($appPath) === $modernization;
        } catch (Throwable $error) {
            $report['modernizedBaselineUnchangedDuringRun'] = false;
            $report['baselineRecheckError'] = ['class' => get_class($error), 'message' => $error->getMessage()];
        }
        $report['sourceFilesUnchangedDuringRun'] = $report['sourceFilesUnchangedDuringRun'] && $report['modernizedBaselineUnchangedDuringRun'];
    }
    $json = json_encode($report, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE | JSON_THROW_ON_ERROR)."\n";
    if ($output !== null && is_dir($output)) {
        workflowWrite($output.'/report.json', $json);
    }
    fwrite(STDOUT, $json);
}

exit(in_array($report['status'], ['html_action_passed', 'delivered_pending_pdf_acceptance'], true) && $report['sourceFilesUnchangedDuringRun'] ? 0 : 1);
