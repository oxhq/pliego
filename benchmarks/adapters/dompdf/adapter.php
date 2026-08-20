#!/usr/bin/php
<?php

/**
 * dompdf adapter for pliego.benchmark-adapter.v1.
 *
 * One invocation renders one document. Dependency installation and the PDF
 * oracle stay outside this adapter and outside engine timing.
 */

declare(strict_types=1);

const CONTRACT = 'pliego.benchmark-adapter.v1';
const TARGET = 'dompdf-3.1.6';
const PACKAGE = 'dompdf/dompdf';
const PACKAGE_VERSION = '3.1.6';

function abort_adapter(string $message, int $code = 2): never
{
    fwrite(STDERR, "dompdf-adapter: {$message}\n");
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
            hash_update($digest, "F\0{$relative}\0" . hex2bin((string) $hash));
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
    return [$width * 0.75, $height * 0.75];
}

function publish_pdf(string $output, string $pdf): void
{
    if (!str_starts_with($pdf, '%PDF-') || !str_contains(substr($pdf, -4096), '%%EOF')) {
        abort_adapter('dompdf returned an invalid PDF envelope', 1);
    }
    if (file_exists($output)) {
        abort_adapter("refusing to replace existing output: {$output}");
    }
    $parent = realpath(dirname($output));
    if ($parent === false || !is_dir($parent)) {
        abort_adapter("output parent is unavailable: " . dirname($output));
    }
    $temporary = $parent . DIRECTORY_SEPARATOR . '.' . basename($output) . '.tmp-' . bin2hex(random_bytes(8));
    $stream = fopen($temporary, 'xb');
    if ($stream === false) {
        abort_adapter("cannot create temporary output: {$temporary}");
    }
    $offset = 0;
    while ($offset < strlen($pdf)) {
        $written = fwrite($stream, substr($pdf, $offset));
        if ($written === false || $written === 0) {
            fclose($stream);
            @unlink($temporary);
            abort_adapter('cannot write complete PDF output');
        }
        $offset += $written;
    }
    if (!fflush($stream) || (function_exists('fsync') && !fsync($stream))) {
        fclose($stream);
        @unlink($temporary);
        abort_adapter('cannot flush PDF output');
    }
    fclose($stream);
    if (!rename($temporary, $output)) {
        @unlink($temporary);
        abort_adapter('cannot atomically publish PDF output');
    }
}

function load_dependencies(): void
{
    foreach (['dom', 'mbstring'] as $extension) {
        if (!extension_loaded($extension)) {
            abort_adapter("required PHP extension is unavailable: {$extension}");
        }
    }
    require required_file(__DIR__ . '/vendor/autoload.php');
    $installed = Composer\InstalledVersions::getPrettyVersion(PACKAGE);
    if (ltrim((string) $installed, 'v') !== PACKAGE_VERSION) {
        abort_adapter('installed dompdf version does not match the pinned adapter');
    }
}

function identity(): void
{
    load_dependencies();
    $php = required_file(PHP_BINARY);
    $lock = required_file(__DIR__ . '/composer.lock');
    $adapter = required_file(__FILE__);
    echo json_encode([
        'contract' => CONTRACT,
        'target' => TARGET,
        'package' => PACKAGE,
        'package_version' => PACKAGE_VERSION,
        'adapter_path' => $adapter,
        'adapter_sha256' => hash_file('sha256', $adapter),
        'composer_lock_sha256' => hash_file('sha256', $lock),
        'composer_vendor_path' => (string) realpath(__DIR__ . '/vendor'),
        'composer_vendor_sha256' => tree_sha256(__DIR__ . '/vendor'),
        'php_path' => $php,
        'php_sha256' => hash_file('sha256', $php),
        'php_version' => PHP_VERSION,
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
    if ($options['--page-margins'] !== '0,0,0,0') {
        abort_adapter('contract v1 supports only zero page margins');
    }
    [$widthPoints, $heightPoints] = page_size($options['--page-size']);
    load_dependencies();

    $dompdfOptions = new Dompdf\Options();
    $dompdfOptions->setChroot([$cwd]);
    $dompdfOptions->setIsRemoteEnabled(false);
    $dompdfOptions->setAllowedRemoteHosts([]);
    $dompdfOptions->setTempDir($artifacts);
    $dompdfOptions->setFontDir($artifacts);
    $dompdfOptions->setFontCache($artifacts);
    $dompdf = new Dompdf\Dompdf($dompdfOptions);
    $dompdf->setProtocol('file://');
    $dompdf->setBaseHost('');
    $dompdf->setBasePath($cwd . DIRECTORY_SEPARATOR);
    $dompdf->loadHtmlFile($input);
    $dompdf->setPaper([0.0, 0.0, $widthPoints, $heightPoints]);
    $dompdf->render();
    publish_pdf($options['--output'], $dompdf->output());
}

$mode = $argv[1] ?? '';
if ($mode === 'identity') {
    identity();
    exit(0);
}
if ($mode === 'self-test') {
    [$width, $height] = page_size('793.7008x1122.52');
    if (abs($width - 595.2756) > 0.0001 || abs($height - 841.89) > 0.0001) {
        abort_adapter('page conversion self-test failed', 1);
    }
    if (!is_bare_input_name('input.html') || is_bare_input_name('../input.html')
        || is_bare_input_name('..\\input.html') || is_bare_input_name('C:\\input.html')) {
        abort_adapter('bare input self-test failed', 1);
    }
    echo "dompdf adapter self-test passed\n";
    exit(0);
}
if ($mode !== 'render') {
    abort_adapter('expected identity, render, or self-test');
}
render(array_slice($argv, 2));
