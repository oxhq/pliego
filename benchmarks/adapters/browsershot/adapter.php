#!/usr/bin/php
<?php

/** Browsershot adapter for pliego.benchmark-adapter.v1. */

declare(strict_types=1);

const CONTRACT = 'pliego.benchmark-adapter.v1';
const TARGET = 'browsershot-5.4.0-puppeteer-25.8.0';
const PACKAGE = 'spatie/browsershot';
const PACKAGE_VERSION = '5.4.0';
const PUPPETEER_VERSION = '25.8.0';
const BLOCKED_NETWORK_URL_SUBSTRINGS = ['http://', 'https://'];
const PRIVATE_BROWSER_PROFILE = 'chrome-profile';
const PRIVATE_RUNTIME_DIRECTORIES = [
    'HOME' => 'home',
    'XDG_CACHE_HOME' => 'xdg-cache',
    'XDG_CONFIG_HOME' => 'xdg-config',
    'XDG_DATA_HOME' => 'xdg-data',
    'XDG_RUNTIME_DIR' => 'xdg-runtime',
    'XDG_STATE_HOME' => 'xdg-state',
];

function abort_adapter(string $message, int $code = 2): never
{
    fwrite(STDERR, "browsershot-adapter: {$message}\n");
    exit($code);
}

function required_file(string $path): string
{
    $resolved = realpath($path);
    if ($resolved === false || !is_file($resolved)) {
        abort_adapter("required file is unavailable: {$path}");
    }
    return $resolved;
}

function tree_sha256(string $path): string
{
    $root = realpath($path);
    if ($root === false || !is_dir($root)) {
        abort_adapter("dependency tree is unavailable: {$path}");
    }
    $entries = [];
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS)
    );
    foreach ($iterator as $entry) {
        $entries[] = $entry->getPathname();
    }
    sort($entries, SORT_STRING);
    $digest = hash_init('sha256');
    foreach ($entries as $entry) {
        $relative = str_replace('\\', '/', substr($entry, strlen($root) + 1));
        if (is_link($entry)) {
            hash_update($digest, "L\0{$relative}\0" . (string) readlink($entry) . "\0");
        } elseif (is_file($entry)) {
            $hash = hash_file('sha256', $entry);
            if (!is_string($hash)) {
                abort_adapter("cannot hash dependency file: {$entry}");
            }
            hash_update($digest, "F\0{$relative}\0" . hex2bin($hash));
        }
    }
    return hash_final($digest);
}

function is_bare_input_name(string $value): bool
{
    return $value !== '' && $value !== '.' && $value !== '..'
        && !str_contains($value, '/') && !str_contains($value, '\\')
        && preg_match('/^[A-Za-z]:/', $value) !== 1;
}

/** @return array<string, string> */
function parse_render_options(array $arguments): array
{
    $parsed = [];
    for ($index = 0; $index < count($arguments); $index += 2) {
        $name = $arguments[$index] ?? '';
        $value = $arguments[$index + 1] ?? null;
        if (!in_array($name, ['--output', '--artifacts', '--page-size', '--page-margins'], true)
            || !is_string($value) || $value === '') {
            abort_adapter("invalid render option near " . ($name ?: '(empty)'));
        }
        if (array_key_exists($name, $parsed)) {
            abort_adapter("duplicate render option: {$name}");
        }
        $parsed[$name] = $value;
    }
    foreach (['--output', '--artifacts', '--page-size', '--page-margins'] as $required) {
        if (!isset($parsed[$required])) {
            abort_adapter("missing render option: {$required}");
        }
    }
    return $parsed;
}

/** @return array{0: float, 1: float} */
function page_size(string $value): array
{
    if (preg_match('/^([0-9]+(?:\.[0-9]+)?)x([0-9]+(?:\.[0-9]+)?)$/D', $value, $matches) !== 1) {
        abort_adapter('--page-size must be WIDTHxHEIGHT in positive CSS pixels');
    }
    $width = (float) $matches[1];
    $height = (float) $matches[2];
    if ($width <= 0 || $height <= 0) {
        abort_adapter('--page-size values must be positive');
    }
    return [$width, $height];
}

/** @return array{0: float, 1: float, 2: float, 3: float} */
function page_margins(string $value): array
{
    if (preg_match(
        '/^([0-9]+(?:\.[0-9]+)?),([0-9]+(?:\.[0-9]+)?),([0-9]+(?:\.[0-9]+)?),([0-9]+(?:\.[0-9]+)?)$/D',
        $value,
        $matches
    ) !== 1) {
        abort_adapter('--page-margins must be TOP,RIGHT,BOTTOM,LEFT in nonnegative CSS pixels');
    }
    return [(float) $matches[1], (float) $matches[2], (float) $matches[3], (float) $matches[4]];
}

/** @return array{files: list<string>, directories: list<string>} */
function durability_sync_plan(string $path): array
{
    $root = realpath($path);
    if ($root === false || !is_dir($root) || is_link($path)) {
        throw new RuntimeException("durability root is unavailable or unsafe: {$path}");
    }
    $files = [];
    $directories = [$root];
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS),
        RecursiveIteratorIterator::CHILD_FIRST
    );
    foreach ($iterator as $entry) {
        $entryPath = $entry->getPathname();
        if ($entry->isLink()) {
            throw new RuntimeException("durability tree contains a symbolic link: {$entryPath}");
        }
        if ($entry->isFile()) {
            $files[] = $entryPath;
        } elseif ($entry->isDir()) {
            $directories[] = $entryPath;
        } else {
            throw new RuntimeException("durability tree contains a special file: {$entryPath}");
        }
    }
    sort($files, SORT_STRING);
    usort($directories, static function (string $left, string $right): int {
        $depth = substr_count($right, DIRECTORY_SEPARATOR) <=> substr_count($left, DIRECTORY_SEPARATOR);
        return $depth !== 0 ? $depth : strcmp($left, $right);
    });
    return ['files' => $files, 'directories' => $directories];
}

function sync_path(string $path, bool $directory): void
{
    if (!function_exists('fsync')) {
        throw new RuntimeException('PHP fsync support is required for benchmark durability');
    }
    $stream = @fopen($path, $directory ? 'rb' : 'r+b');
    if ($stream === false || !fsync($stream)) {
        if (is_resource($stream)) {
            fclose($stream);
        }
        throw new RuntimeException("cannot durably flush benchmark path: {$path}");
    }
    fclose($stream);
}

function sync_tree(string $path, ?callable $sync = null): void
{
    $plan = durability_sync_plan($path);
    $sync ??= static function (string $entry, bool $directory): void {
        sync_path($entry, $directory);
    };
    foreach ($plan['files'] as $file) {
        $sync($file, false);
    }
    foreach ($plan['directories'] as $directory) {
        $sync($directory, true);
    }
}

function remove_synced_tree(string $path, ?callable $sync = null): void
{
    $plan = durability_sync_plan($path);
    $sync ??= static function (string $entry, bool $directory): void {
        sync_path($entry, $directory);
    };
    foreach ($plan['files'] as $file) {
        if (!unlink($file)) {
            throw new RuntimeException("cannot remove synced benchmark file: {$file}");
        }
    }
    foreach ($plan['directories'] as $directory) {
        $sync($directory, true);
        if (!rmdir($directory)) {
            throw new RuntimeException("cannot remove synced benchmark directory: {$directory}");
        }
    }
}

function clear_synced_runtime_root(string $path, ?callable $sync = null): void
{
    $root = realpath($path);
    if ($root === false || !is_dir($root) || is_link($path)) {
        throw new RuntimeException("private runtime root is unavailable or unsafe: {$path}");
    }
    $rootMetadata = lstat($root);
    if ($rootMetadata === false) {
        throw new RuntimeException("cannot identify private runtime root: {$root}");
    }
    $preserved = [
        $root => [(int) $rootMetadata['dev'], (int) $rootMetadata['ino']],
    ];
    $expectedRootEntries = [];
    foreach (PRIVATE_RUNTIME_DIRECTORIES as $_variable => $relative) {
        $expected = $root . DIRECTORY_SEPARATOR . $relative;
        $resolved = realpath($expected);
        if ($resolved === false || $resolved !== $expected || !is_dir($resolved) || is_link($expected)) {
            throw new RuntimeException("private runtime directory is unavailable or unsafe: {$expected}");
        }
        $metadata = lstat($resolved);
        if ($metadata === false) {
            throw new RuntimeException("cannot identify private runtime directory: {$resolved}");
        }
        $preserved[$resolved] = [(int) $metadata['dev'], (int) $metadata['ino']];
        $expectedRootEntries[] = $relative;
    }
    $plan = durability_sync_plan($root);
    $sync ??= static function (string $entry, bool $directory): void {
        sync_path($entry, $directory);
    };
    foreach ($plan['files'] as $file) {
        $sync($file, false);
        if (!unlink($file)) {
            throw new RuntimeException("cannot remove synced private runtime file: {$file}");
        }
    }
    foreach ($plan['directories'] as $directory) {
        $sync($directory, true);
        if (!isset($preserved[$directory]) && !rmdir($directory)) {
            throw new RuntimeException("cannot remove synced private runtime directory: {$directory}");
        }
    }
    sort($expectedRootEntries, SORT_STRING);
    foreach ($preserved as $directory => $identity) {
        clearstatcache(true, $directory);
        $metadata = lstat($directory);
        if ($metadata === false || !is_dir($directory) || is_link($directory)
            || [(int) $metadata['dev'], (int) $metadata['ino']] !== $identity) {
            throw new RuntimeException("private runtime directory identity changed during teardown: {$directory}");
        }
        $expectedEntries = array_fill_keys($directory === $root ? $expectedRootEntries : [], true);
        $seen = [];
        $entries = new FilesystemIterator($directory, FilesystemIterator::SKIP_DOTS);
        foreach ($entries as $entry) {
            $name = $entry->getFilename();
            if (!isset($expectedEntries[$name]) || isset($seen[$name])) {
                throw new RuntimeException("private runtime directory is not empty after teardown: {$directory}");
            }
            $seen[$name] = true;
        }
        if (count($seen) !== count($expectedEntries)) {
            throw new RuntimeException("private runtime directory lost a bound entry during teardown: {$directory}");
        }
    }
}

/** @param null|callable(string): mixed $environment */
function private_runtime_root(?callable $environment = null): ?string
{
    $environment ??= static fn (string $name): string|false => getenv($name);
    $values = ['TMPDIR' => $environment('TMPDIR')];
    $configuredXdg = 0;
    foreach (PRIVATE_RUNTIME_DIRECTORIES as $variable => $_relative) {
        $value = $environment($variable);
        $values[$variable] = $value;
        if (str_starts_with($variable, 'XDG_') && is_string($value) && $value !== '') {
            $configuredXdg++;
        }
    }
    if ($configuredXdg === 0) {
        // Unmeasured smoke invocations do not receive the sampler's private
        // runtime map. Publishable measurement always configures every entry.
        return null;
    }
    $requiredXdg = count(PRIVATE_RUNTIME_DIRECTORIES) - 1;
    if ($configuredXdg !== $requiredXdg) {
        throw new RuntimeException('controlled browser runtime requires the complete XDG directory map');
    }
    foreach ($values as $variable => $value) {
        if (!is_string($value) || $value === '') {
            throw new RuntimeException("controlled browser runtime omitted {$variable}");
        }
    }
    $rootValue = $values['TMPDIR'];
    $root = realpath($rootValue);
    if ($root === false || $root !== $rootValue || !is_dir($root) || is_link($rootValue)) {
        throw new RuntimeException('TMPDIR must identify the canonical private browser runtime root');
    }
    foreach (PRIVATE_RUNTIME_DIRECTORIES as $variable => $relative) {
        $value = $values[$variable];
        $resolved = realpath($value);
        $expected = $root . DIRECTORY_SEPARATOR . $relative;
        if ($resolved === false || $resolved !== $value || $resolved !== $expected
            || !is_dir($resolved) || is_link($value)) {
            throw new RuntimeException("{$variable} escaped the private browser runtime root");
        }
    }
    return $root;
}

function create_private_browser_profile(?string $runtimeRoot): ?string
{
    if ($runtimeRoot === null) {
        return null;
    }
    $profile = $runtimeRoot . DIRECTORY_SEPARATOR . PRIVATE_BROWSER_PROFILE;
    if (file_exists($profile) || is_link($profile) || !mkdir($profile, 0700)) {
        throw new RuntimeException('cannot create a fresh private browser profile');
    }
    $resolved = realpath($profile);
    if ($resolved === false || $resolved !== $profile || !is_dir($resolved) || is_link($profile)) {
        throw new RuntimeException('private browser profile identity is unsafe');
    }
    return $profile;
}

function run_browser_with_finalizer(callable $browser, ?callable $finalize): void
{
    $browserFailure = null;
    try {
        $browser();
    } catch (Throwable $error) {
        $browserFailure = $error;
    }
    $finalizerFailure = null;
    if ($finalize !== null) {
        try {
            $finalize();
        } catch (Throwable $error) {
            $finalizerFailure = $error;
        }
    }
    if ($browserFailure !== null) {
        $suffix = $finalizerFailure === null ? '' : '; profile teardown failed: ' . $finalizerFailure->getMessage();
        throw new RuntimeException('Chromium render failed: ' . $browserFailure->getMessage() . $suffix, 0, $browserFailure);
    }
    if ($finalizerFailure !== null) {
        throw new RuntimeException(
            'private browser profile teardown failed: ' . $finalizerFailure->getMessage(),
            0,
            $finalizerFailure
        );
    }
}

function commit_pdf_output(string $temporary, string $output, ?callable $sync = null): void
{
    $parent = realpath(dirname($output));
    if ($parent === false || !is_dir($parent)) {
        throw new RuntimeException('output parent is unavailable during publication');
    }
    if (!rename($temporary, $output)) {
        throw new RuntimeException('cannot atomically publish PDF output');
    }
    $sync ??= static function (string $path, bool $directory): void {
        sync_path($path, $directory);
    };
    try {
        $sync($parent, true);
    } catch (Throwable $error) {
        $removed = @unlink($output);
        try {
            $sync($parent, true);
        } catch (Throwable) {
            // The requested output is already absent; retain the original durability failure.
        }
        if (!$removed) {
            throw new RuntimeException(
                "cannot durably publish or roll back requested PDF output: {$output}",
                0,
                $error
            );
        }
        throw new RuntimeException("cannot durably publish requested PDF output: {$output}", 0, $error);
    }
}

function runtime_path(string $variable): string
{
    $value = getenv($variable);
    if (!is_string($value) || $value === '') {
        abort_adapter("{$variable} is required");
    }
    $resolved = required_file($value);
    if (!is_executable($resolved)) {
        abort_adapter("{$variable} must be executable: {$resolved}");
    }
    return $resolved;
}

function command_version(string $executable): string
{
    $process = proc_open(
        [$executable, '--version'],
        [0 => ['file', PHP_OS_FAMILY === 'Windows' ? 'NUL' : '/dev/null', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']],
        $pipes
    );
    if (!is_resource($process)) {
        abort_adapter("cannot execute runtime: {$executable}");
    }
    $output = trim((string) stream_get_contents($pipes[1]) . (string) stream_get_contents($pipes[2]));
    fclose($pipes[1]);
    fclose($pipes[2]);
    $code = proc_close($process);
    if ($code !== 0 || $output === '') {
        abort_adapter("cannot identify runtime {$executable}");
    }
    return strtok($output, "\r\n") ?: $output;
}

/** @return array{node: string, chrome: string} */
function load_dependencies(): array
{
    foreach (['fileinfo', 'json'] as $extension) {
        if (!extension_loaded($extension)) {
            abort_adapter("required PHP extension is unavailable: {$extension}");
        }
    }
    require required_file(__DIR__ . '/vendor/autoload.php');
    $installed = Composer\InstalledVersions::getPrettyVersion(PACKAGE);
    if (ltrim((string) $installed, 'v') !== PACKAGE_VERSION) {
        abort_adapter('installed Browsershot version does not match the pinned adapter');
    }
    if (!method_exists(Spatie\Browsershot\Browsershot::class, 'setUserDataDir')) {
        abort_adapter('installed Browsershot does not expose controlled profile binding');
    }
    $puppeteerPath = required_file(__DIR__ . '/node_modules/puppeteer/package.json');
    $puppeteer = json_decode((string) file_get_contents($puppeteerPath), true);
    if (!is_array($puppeteer) || ($puppeteer['version'] ?? null) !== PUPPETEER_VERSION) {
        abort_adapter('installed Puppeteer version does not match package-lock.json');
    }
    return [
        'node' => runtime_path('BROWSERSHOT_NODE_BINARY'),
        'chrome' => runtime_path('BROWSERSHOT_CHROME_PATH'),
    ];
}

function identity(): void
{
    $runtime = load_dependencies();
    $php = required_file(PHP_BINARY);
    $adapter = required_file(__FILE__);
    echo json_encode([
        'contract' => CONTRACT,
        'target' => TARGET,
        'package' => PACKAGE,
        'package_version' => PACKAGE_VERSION,
        'puppeteer_version' => PUPPETEER_VERSION,
        'adapter_path' => $adapter,
        'adapter_sha256' => hash_file('sha256', $adapter),
        'composer_lock_sha256' => hash_file('sha256', required_file(__DIR__ . '/composer.lock')),
        'package_lock_sha256' => hash_file('sha256', required_file(__DIR__ . '/package-lock.json')),
        'composer_vendor_path' => (string) realpath(__DIR__ . '/vendor'),
        'composer_vendor_sha256' => tree_sha256(__DIR__ . '/vendor'),
        'node_modules_path' => (string) realpath(__DIR__ . '/node_modules'),
        'node_modules_sha256' => tree_sha256(__DIR__ . '/node_modules'),
        'php_path' => $php,
        'php_sha256' => hash_file('sha256', $php),
        'php_version' => PHP_VERSION,
        'node_path' => $runtime['node'],
        'node_sha256' => hash_file('sha256', $runtime['node']),
        'node_version' => command_version($runtime['node']),
        'chrome_path' => $runtime['chrome'],
        'chrome_sha256' => hash_file('sha256', $runtime['chrome']),
        'chrome_version' => command_version($runtime['chrome']),
    ], JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR) . "\n";
}

function render(array $arguments): void
{
    $input = array_shift($arguments);
    if (!is_string($input) || !is_bare_input_name($input)) {
        abort_adapter('render requires one bare input file name');
    }
    $options = parse_render_options($arguments);
    $cwd = realpath(getcwd() ?: '');
    $inputPath = $cwd !== false ? realpath($cwd . DIRECTORY_SEPARATOR . $input) : false;
    if ($cwd === false || $inputPath === false || dirname($inputPath) !== $cwd || !is_file($inputPath)) {
        abort_adapter('input must resolve to a regular file directly inside cwd');
    }
    $artifacts = realpath($options['--artifacts']);
    if ($artifacts === false || !is_dir($artifacts)) {
        abort_adapter('artifacts directory must already exist');
    }
    $output = $options['--output'];
    if (file_exists($output)) {
        abort_adapter("refusing to replace existing output: {$output}");
    }
    $parent = realpath(dirname($output));
    if ($parent === false || !is_dir($parent)) {
        abort_adapter('output parent is unavailable');
    }
    [$width, $height] = page_size($options['--page-size']);
    [$top, $right, $bottom, $left] = page_margins($options['--page-margins']);
    $runtime = load_dependencies();
    $privateRuntimeRoot = private_runtime_root();
    $privateBrowserProfile = create_private_browser_profile($privateRuntimeRoot);
    $temporary = $parent . DIRECTORY_SEPARATOR . '.' . basename($output) . '.tmp-' . bin2hex(random_bytes(8));

    $shot = Spatie\Browsershot\Browsershot::htmlFromFilePath($inputPath)
        ->setNodeBinary($runtime['node'])
        ->setChromePath($runtime['chrome'])
        ->setNodeModulePath(__DIR__ . '/node_modules')
        ->setCustomTempPath($artifacts)
        ->paperSize($width / 96.0, $height / 96.0, 'in')
        ->margins($top / 96.0, $right / 96.0, $bottom / 96.0, $left / 96.0, 'in')
        ->showBackground()
        // Browsershot treats these values as substrings, not glob patterns.
        ->blockUrls(BLOCKED_NETWORK_URL_SUBSTRINGS)
        ->disableRedirects()
        ->addChromiumArguments([
            'allow-file-access-from-files',
            'disable-background-networking',
            'disable-component-update',
            // Chromium's benchmark-only Crashpad metrics mmap can outlive the
            // deleted Puppeteer profile as a dirty page after browser exit.
            'disable-crashpad-metrics',
            'disable-domain-reliability',
            'disable-sync',
            'metrics-recording-only',
            // The sampler is the benchmark sandbox: fixed UID, no capabilities,
            // no_new_privs, private network namespace, and a sealed filesystem
            // closure. Chrome's copied setuid helper cannot elevate inside it.
            'no-sandbox',
            'no-first-run',
        ]);
    if ($privateBrowserProfile !== null) {
        // Puppeteer otherwise creates and recursively deletes an implicit
        // profile before the adapter can flush its file-backed pages.
        $shot->setUserDataDir($privateBrowserProfile);
    }
    if (($privateRuntimeRoot === null) !== ($privateBrowserProfile === null)) {
        throw new RuntimeException('controlled browser profile binding is incomplete');
    }
    $profileFinalizer = null;
    if ($privateRuntimeRoot !== null && $privateBrowserProfile !== null) {
        $profileFinalizer = static function () use ($privateRuntimeRoot, $privateBrowserProfile): void {
            // savePdf waits for Node and Chrome to exit. Flush all retained
            // state before removing every per-invocation cache/profile entry.
            // The bound top-level runtime directories retain their identities.
            // Teardown remains inside the measured cgroup and also runs after a
            // browser failure so diagnostics are not masked by dirty pages.
            clear_synced_runtime_root($privateRuntimeRoot);
            if (file_exists($privateBrowserProfile) || is_link($privateBrowserProfile)) {
                throw new RuntimeException('private browser profile survived runtime teardown');
            }
        };
    }
    run_browser_with_finalizer(
        static function () use ($shot, $temporary): void {
            $shot->savePdf($temporary);
        },
        $profileFinalizer
    );
    $pdf = (string) file_get_contents($temporary);
    if (!str_starts_with($pdf, '%PDF-') || !str_contains(substr($pdf, -4096), '%%EOF')) {
        abort_adapter('Chromium returned an invalid PDF envelope', 1);
    }
    $stream = fopen($temporary, 'r+b');
    if ($stream === false || !fflush($stream) || (function_exists('fsync') && !fsync($stream))) {
        if (is_resource($stream)) {
            fclose($stream);
        }
        abort_adapter('cannot flush Chromium PDF output');
    }
    fclose($stream);
    sync_tree($artifacts);
    commit_pdf_output($temporary, $output);
}

$mode = $argv[1] ?? '';
if ($mode === 'identity') {
    identity();
    exit(0);
}
if ($mode === 'self-test') {
    [$width, $height] = page_size('793.7008x1122.52');
    $margins = page_margins('0,0,0,0');
    if ($width !== 793.7008 || $height !== 1122.52 || $margins !== [0.0, 0.0, 0.0, 0.0]) {
        abort_adapter('geometry parser self-test failed', 1);
    }
    if (!is_bare_input_name('input.html') || is_bare_input_name('../input.html')
        || is_bare_input_name('..\\input.html') || is_bare_input_name('C:\\input.html')) {
        abort_adapter('bare input self-test failed', 1);
    }
    if (BLOCKED_NETWORK_URL_SUBSTRINGS !== ['http://', 'https://']) {
        abort_adapter('network block self-test failed', 1);
    }
    $finalizerEvents = [];
    try {
        run_browser_with_finalizer(
            static function () use (&$finalizerEvents): void {
                $finalizerEvents[] = 'browser';
                throw new RuntimeException('injected browser failure');
            },
            static function () use (&$finalizerEvents): void {
                $finalizerEvents[] = 'finalizer';
            }
        );
        abort_adapter('browser failure bypassed the profile finalizer', 1);
    } catch (RuntimeException $error) {
        if ($error->getMessage() !== 'Chromium render failed: injected browser failure'
            || $finalizerEvents !== ['browser', 'finalizer']
            || $error->getPrevious()?->getMessage() !== 'injected browser failure') {
            abort_adapter('browser failure finalization was not retained', 1);
        }
    }
    $syncRoot = sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'pliego-browsershot-sync-' . bin2hex(random_bytes(8));
    $syncNested = $syncRoot . DIRECTORY_SEPARATOR . 'nested';
    if (!mkdir($syncNested, 0700, true) || file_put_contents($syncNested . DIRECTORY_SEPARATOR . 'cache.bin', 'cache') === false) {
        abort_adapter('artifact sync-plan self-test setup failed', 1);
    }
    $runtimeRoot = $syncRoot . DIRECTORY_SEPARATOR . 'runtime';
    $runtimeEnvironment = [];
    $runtimeCache = null;
    $runtimeProfile = null;
    $runtimeProfileNested = null;
    $runtimeProfileState = null;
    try {
        $plan = durability_sync_plan($syncRoot);
        if ($plan['files'] !== [$syncNested . DIRECTORY_SEPARATOR . 'cache.bin']
            || $plan['directories'] !== [$syncNested, $syncRoot]) {
            abort_adapter('artifact sync-plan ordering self-test failed', 1);
        }
        if (PHP_OS_FAMILY !== 'Windows') {
            sync_tree($syncRoot);
        }
        if (!mkdir($runtimeRoot, 0700)) {
            abort_adapter('private runtime self-test setup failed', 1);
        }
        $runtimeEnvironment = ['TMPDIR' => $runtimeRoot];
        foreach (PRIVATE_RUNTIME_DIRECTORIES as $variable => $relative) {
            $path = $runtimeRoot . DIRECTORY_SEPARATOR . $relative;
            if (!mkdir($path, 0700)) {
                abort_adapter("private runtime {$variable} self-test setup failed", 1);
            }
            $runtimeEnvironment[$variable] = $path;
        }
        $runtimeCache = $runtimeEnvironment['XDG_CACHE_HOME'] . DIRECTORY_SEPARATOR . 'cache.bin';
        if (file_put_contents($runtimeCache, 'runtime-cache') === false) {
            abort_adapter('private runtime cache self-test setup failed', 1);
        }
        $runtimeGetter = static fn (string $name): string|false => $runtimeEnvironment[$name] ?? false;
        if (private_runtime_root($runtimeGetter) !== $runtimeRoot
            || private_runtime_root(static fn (string $_name): false => false) !== null
            || create_private_browser_profile(null) !== null) {
            abort_adapter('private runtime binding self-test failed', 1);
        }
        $runtimeProfile = create_private_browser_profile($runtimeRoot);
        $runtimeProfileNested = $runtimeProfile . DIRECTORY_SEPARATOR . 'Default';
        $runtimeProfileState = $runtimeProfileNested . DIRECTORY_SEPARATOR . 'Preferences';
        if (!mkdir($runtimeProfileNested, 0700)
            || file_put_contents($runtimeProfileState, 'profile-state') === false) {
            abort_adapter('private browser profile self-test setup failed', 1);
        }
        try {
            create_private_browser_profile($runtimeRoot);
            abort_adapter('private browser profile reused an existing directory', 1);
        } catch (RuntimeException $error) {
            if ($error->getMessage() !== 'cannot create a fresh private browser profile') {
                abort_adapter('private browser profile reuse failure was not typed', 1);
            }
        }
        $partialEnvironment = ['XDG_CACHE_HOME' => $runtimeEnvironment['XDG_CACHE_HOME']];
        try {
            private_runtime_root(
                static fn (string $name): string|false => $partialEnvironment[$name] ?? false
            );
            abort_adapter('private runtime binding accepted a partial XDG map', 1);
        } catch (RuntimeException $error) {
            if ($error->getMessage() !== 'controlled browser runtime requires the complete XDG directory map') {
                abort_adapter('private runtime partial-map failure was not typed', 1);
            }
        }
        $escapedEnvironment = $runtimeEnvironment;
        $escapedEnvironment['XDG_CACHE_HOME'] = $runtimeEnvironment['XDG_DATA_HOME'];
        try {
            private_runtime_root(
                static fn (string $name): string|false => $escapedEnvironment[$name] ?? false
            );
            abort_adapter('private runtime binding accepted an escaped XDG path', 1);
        } catch (RuntimeException $error) {
            if ($error->getMessage() !== 'XDG_CACHE_HOME escaped the private browser runtime root') {
                abort_adapter('private runtime escape failure was not typed', 1);
            }
        }
        $syncEvents = [];
        sync_tree(
            $runtimeRoot,
            static function (string $path, bool $directory) use (&$syncEvents): void {
                $syncEvents[] = [$path, $directory];
            }
        );
        $directorySeen = false;
        foreach ($syncEvents as [$path, $directory]) {
            if (!$directory && $directorySeen) {
                abort_adapter("private runtime file was synced after a directory: {$path}", 1);
            }
            $directorySeen = $directorySeen || $directory;
        }
        $syncedFiles = array_values(array_filter(
            $syncEvents,
            static fn (array $event): bool => $event[1] === false
        ));
        $expectedFiles = [[$runtimeProfileState, false], [$runtimeCache, false]];
        usort($expectedFiles, static fn (array $left, array $right): int => strcmp($left[0], $right[0]));
        if ($syncEvents === [] || $syncedFiles !== $expectedFiles
            || $syncEvents[count($syncEvents) - 1] !== [$runtimeRoot, true]) {
            abort_adapter('private runtime sync ordering self-test failed', 1);
        }
        if (PHP_OS_FAMILY !== 'Windows') {
            sync_tree($runtimeRoot);
        }
        $removalPlan = durability_sync_plan($runtimeProfile);
        $removalSyncEvents = [];
        remove_synced_tree(
            $runtimeProfile,
            static function (string $path, bool $directory) use (&$removalSyncEvents): void {
                $removalSyncEvents[] = [$path, $directory];
            }
        );
        $expectedRemovalSyncEvents = array_map(
            static fn (string $directory): array => [$directory, true],
            $removalPlan['directories']
        );
        if ($removalSyncEvents !== $expectedRemovalSyncEvents) {
            abort_adapter('private browser profile deletion was not synced deepest-first', 1);
        }
        if (file_exists($runtimeProfile)) {
            abort_adapter('private browser profile removal self-test failed', 1);
        }
        $runtimeProfile = null;
        $runtimeProfileNested = null;
        $runtimeProfileState = null;
        $transientDirectory = $runtimeRoot . DIRECTORY_SEPARATOR . 'transient' . DIRECTORY_SEPARATOR . 'nested';
        $transientFile = $transientDirectory . DIRECTORY_SEPARATOR . 'state.bin';
        if (!mkdir($transientDirectory, 0700, true)
            || file_put_contents($transientFile, 'transient-state') === false) {
            abort_adapter('private runtime teardown self-test setup failed', 1);
        }
        $clearEvents = [];
        clear_synced_runtime_root(
            $runtimeRoot,
            static function (string $path, bool $directory) use (&$clearEvents): void {
                $clearEvents[] = [$path, $directory];
                if (PHP_OS_FAMILY !== 'Windows') {
                    sync_path($path, $directory);
                }
            }
        );
        $directorySeen = false;
        foreach ($clearEvents as [$path, $directory]) {
            if (!$directory && $directorySeen) {
                abort_adapter("private runtime teardown synced a file after a directory: {$path}", 1);
            }
            $directorySeen = $directorySeen || $directory;
        }
        foreach (PRIVATE_RUNTIME_DIRECTORIES as $variable => $_relative) {
            $entries = scandir($runtimeEnvironment[$variable]);
            if ($entries !== ['.', '..']) {
                abort_adapter("private runtime teardown did not empty {$variable}", 1);
            }
        }
        if (file_exists($runtimeCache) || file_exists($transientDirectory)
            || $clearEvents === [] || $clearEvents[count($clearEvents) - 1] !== [$runtimeRoot, true]) {
            abort_adapter('private runtime teardown self-test failed', 1);
        }
        $runtimeCache = null;
        $lateRuntimeFile = $runtimeEnvironment['XDG_CACHE_HOME'] . DIRECTORY_SEPARATOR . 'late-state.bin';
        try {
            clear_synced_runtime_root(
                $runtimeRoot,
                static function (string $path, bool $directory) use ($runtimeRoot, $lateRuntimeFile): void {
                    if ($directory && $path === $runtimeRoot
                        && file_put_contents($lateRuntimeFile, 'late-state') === false) {
                        throw new RuntimeException('cannot inject late private runtime state');
                    }
                }
            );
            abort_adapter('private runtime teardown accepted a late cache entry', 1);
        } catch (RuntimeException $error) {
            if (!str_contains($error->getMessage(), 'is not empty after teardown')) {
                abort_adapter('private runtime teardown did not type a late cache entry', 1);
            }
        } finally {
            @unlink($lateRuntimeFile);
        }
        $blockedProfile = $runtimeRoot . DIRECTORY_SEPARATOR . PRIVATE_BROWSER_PROFILE;
        if (file_put_contents($blockedProfile, 'not-a-directory') === false) {
            abort_adapter('private browser profile collision self-test setup failed', 1);
        }
        try {
            create_private_browser_profile($runtimeRoot);
            abort_adapter('private browser profile replaced a non-directory', 1);
        } catch (RuntimeException $error) {
            if ($error->getMessage() !== 'cannot create a fresh private browser profile') {
                abort_adapter('private browser profile collision failure was not typed', 1);
            }
        } finally {
            @unlink($blockedProfile);
        }
        if (PHP_OS_FAMILY !== 'Windows') {
            sync_path($runtimeRoot, true);
        }
        $temporaryOutput = $syncRoot . DIRECTORY_SEPARATOR . 'temporary.pdf';
        $requestedOutput = $syncRoot . DIRECTORY_SEPARATOR . 'requested.pdf';
        if (file_put_contents($temporaryOutput, '%PDF-self-test') === false) {
            abort_adapter('publication rollback self-test setup failed', 1);
        }
        try {
            commit_pdf_output(
                $temporaryOutput,
                $requestedOutput,
                static function (string $_path, bool $_directory): void {
                    throw new RuntimeException('injected directory fsync failure');
                }
            );
            abort_adapter('publication rollback self-test accepted a durability failure', 1);
        } catch (RuntimeException $error) {
            if (!str_contains($error->getMessage(), 'cannot durably publish requested PDF output')
                || file_exists($requestedOutput)) {
                abort_adapter('publication rollback self-test left requested output behind', 1);
            }
        }
        if (PHP_OS_FAMILY !== 'Windows' && function_exists('symlink')) {
            $link = $syncRoot . DIRECTORY_SEPARATOR . 'unsafe-link';
            if (!symlink($syncNested . DIRECTORY_SEPARATOR . 'cache.bin', $link)) {
                abort_adapter('artifact sync-plan symlink self-test setup failed', 1);
            }
            try {
                durability_sync_plan($syncRoot);
                abort_adapter('artifact sync-plan followed a symbolic link', 1);
            } catch (RuntimeException) {
                // Expected: benchmark artifact durability never follows links.
            }
            unlink($link);
        }
        if (PHP_OS_FAMILY !== 'Windows' && function_exists('posix_mkfifo')) {
            $fifo = $syncRoot . DIRECTORY_SEPARATOR . 'unsafe-fifo';
            if (!posix_mkfifo($fifo, 0600)) {
                abort_adapter('artifact sync-plan FIFO self-test setup failed', 1);
            }
            try {
                durability_sync_plan($syncRoot);
                abort_adapter('artifact sync-plan accepted a special file', 1);
            } catch (RuntimeException) {
                // Expected: benchmark artifact durability opens only regular files and directories.
            }
            unlink($fifo);
        }
    } finally {
        if (is_string($runtimeProfileState)) {
            @unlink($runtimeProfileState);
        }
        if (is_string($runtimeProfileNested)) {
            @rmdir($runtimeProfileNested);
        }
        if (is_string($runtimeProfile)) {
            @rmdir($runtimeProfile);
        }
        if (is_string($runtimeCache)) {
            @unlink($runtimeCache);
        }
        foreach (array_reverse(PRIVATE_RUNTIME_DIRECTORIES) as $variable => $_relative) {
            if (isset($runtimeEnvironment[$variable])) {
                @rmdir($runtimeEnvironment[$variable]);
            }
        }
        @rmdir($runtimeRoot);
        @unlink($syncNested . DIRECTORY_SEPARATOR . 'cache.bin');
        @unlink($syncRoot . DIRECTORY_SEPARATOR . 'temporary.pdf');
        @unlink($syncRoot . DIRECTORY_SEPARATOR . 'requested.pdf');
        @rmdir($syncNested);
        @rmdir($syncRoot);
    }
    echo "Browsershot adapter self-test passed\n";
    exit(0);
}
if ($mode !== 'render') {
    abort_adapter('expected identity, render, or self-test');
}
try {
    render(array_slice($argv, 2));
} catch (RuntimeException $error) {
    abort_adapter($error->getMessage(), 1);
}
