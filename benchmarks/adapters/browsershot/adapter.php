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
            'disable-domain-reliability',
            'disable-sync',
            'metrics-recording-only',
            'no-first-run',
        ]);
    $shot->savePdf($temporary);
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
    if (!rename($temporary, $output)) {
        abort_adapter('cannot atomically publish PDF output');
    }
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
    echo "Browsershot adapter self-test passed\n";
    exit(0);
}
if ($mode !== 'render') {
    abort_adapter('expected identity, render, or self-test');
}
render(array_slice($argv, 2));
