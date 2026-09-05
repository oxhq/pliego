#!/usr/bin/php
<?php

declare(strict_types=1);

use Spatie\Browsershot\Browsershot;

define('PLIEGO_BENCHMARK_ADAPTER_LIBRARY', true);
require __DIR__.'/../browsershot/adapter.php';
require __DIR__.'/../real_document_runtime.php';

const INVOBOOK_LOCK = '24f563534a57775144db27b746011117b8db209aa39dee35f5c0ba01ed96ca74';
const INVOBOOK_HTML = 'afd286bca202309923fd66bee1f71e732bdd340d1b05a2307094feb535fa7195';

function invobook_runtime(): array
{
    $runtime = rd_runtime(__DIR__, INVOBOOK_LOCK);
    rd_package('spatie/browsershot', '5.0.5');
    foreach (['node_path', 'chrome_path', 'node_modules'] as $key) {
        rd_require(is_string($runtime[$key] ?? null), 'Missing runtime binding: '.$key);
        $resolved = realpath($runtime[$key]);
        rd_require(is_string($resolved), 'Unavailable runtime binding: '.$key);
        $runtime[$key] = $resolved;
    }
    $modules = $runtime['node_modules'];
    $manifest = json_decode((string) file_get_contents(required_file($modules.'/puppeteer/package.json')), true, flags: JSON_THROW_ON_ERROR);
    rd_require(($manifest['version'] ?? null) === '25.8.0', 'Unexpected harness Puppeteer version.');
    rd_require(is_executable($runtime['node_path']) && is_executable($runtime['chrome_path']), 'Browser runtimes must be executable.');
    // Browsershot5.0.5 assembles these two POSIX shell assignments itself.
    // Limit the copied hosted closure to simple paths; do not patch the library.
    if (PHP_OS_FAMILY !== 'Windows') {
        foreach ([$modules, $runtime['node_path']] as $path) {
            rd_require(preg_match('~^/[A-Za-z0-9_./-]+$~D', $path) === 1, 'Hosted Node/module paths require a shell-safe fixed runtime location.');
        }
    }

    return $runtime;
}

/** Scope the inherited Node TMPDIR without requiring the newer setNodeEnv API. */
function invobook_with_node_temp(?string $directory, callable $render): void
{
    $previous = getenv('TMPDIR');
    $hadEnv = array_key_exists('TMPDIR', $_ENV);
    $previousEnv = $_ENV['TMPDIR'] ?? null;
    $hadServer = array_key_exists('TMPDIR', $_SERVER);
    $previousServer = $_SERVER['TMPDIR'] ?? null;
    if ($directory !== null) {
        rd_require(putenv('TMPDIR='.$directory), 'Cannot bind Node shared-memory TMPDIR.');
        // Symfony Process gives populated PHP superglobals precedence over
        // getenv(). Keep all three views coherent for this one child launch.
        $_ENV['TMPDIR'] = $directory;
        $_SERVER['TMPDIR'] = $directory;
    }
    try {
        $render();
    } finally {
        if ($directory !== null) {
            rd_require(putenv($previous === false ? 'TMPDIR' : 'TMPDIR='.$previous), 'Cannot restore adapter TMPDIR.');
            if ($hadEnv) {
                $_ENV['TMPDIR'] = $previousEnv;
            } else {
                unset($_ENV['TMPDIR']);
            }
            if ($hadServer) {
                $_SERVER['TMPDIR'] = $previousServer;
            } else {
                unset($_SERVER['TMPDIR']);
            }
        }
    }
}

function invobook_render(array $arguments): void
{
    rd_require(PHP_OS_FAMILY === 'Linux', 'Measured application adapters require the Linux cgroup/durability recipe.');
    [$input, $options, $artifacts] = rd_render_paths($arguments);
    rd_require(hash_file('sha256', $input) === INVOBOOK_HTML, 'Input differs from the frozen repaired invoice.');
    $inputFonts = rd_font_closure(dirname($input).'/fonts', 2);
    rd_require($options['--page-size'] === '47622x67351au' && $options['--page-margins'] === '0,0,0,0au', 'Invoice requires original A4 portrait with zero margins.');
    $runtime = invobook_runtime();
    $privateRoot = private_runtime_root();
    $sharedMemory = browser_shared_memory_directory($privateRoot);
    $profile = $privateRoot === null ? null : $privateRoot.DIRECTORY_SEPARATOR.PRIVATE_BROWSER_PROFILE;
    $parent = realpath(dirname($options['--output']));
    rd_require(is_string($parent), 'Output parent is unavailable.');
    $temporary = $parent.'/.invoice-'.bin2hex(random_bytes(8)).'.pdf';
    $report = [];
    run_browser_with_finalizer(
        static function (callable $markDescendantsPossible) use ($input, $inputFonts, $artifacts, $runtime, $privateRoot, $sharedMemory, $profile, $temporary, &$report): void {
            rd_require(create_private_browser_profile($privateRoot) === $profile, 'Private profile identity changed.');
            rd_require(($privateRoot === null) === ($sharedMemory === null), 'Incomplete private runtime binding.');
            $shot = Browsershot::htmlFromFilePath(str_replace('\\', '/', $input))
                ->setNodeBinary($runtime['node_path'])->setChromePath($runtime['chrome_path'])
                ->setNodeModulePath($runtime['node_modules'])->setCustomTempPath($artifacts)
                ->format('A4')->margins(0, 0, 0, 0)->showBackground()->timeout(65)
                ->blockUrls(['http://', 'https://', 'ws://', 'wss://', 'ftp://'])->disableRedirects()
                ->addChromiumArguments(chromium_arguments());
            if ($profile !== null) {
                $shot->setUserDataDir($profile);
            }
            $markDescendantsPossible();
            invobook_with_node_temp($sharedMemory, static fn () => $shot->savePdf($temporary));
            $browser = $shot->getOutput();
            rd_require($browser !== null, 'Missing Chromium request/error evidence.');
            $requests = $browser->getRequestsList() ?? [];
            $external = array_values(array_filter($requests, static fn (array $request): bool => ! in_array(
                strtolower((string) parse_url($request['url'], PHP_URL_SCHEME)), ['file', 'data', 'about'], true,
            )));
            $report = [
                'input_sha256' => INVOBOOK_HTML, 'input_fonts' => $inputFonts, 'requests' => $requests, 'external_requests' => $external,
                'failed_requests' => $browser->getFailedRequests() ?? [], 'page_errors' => $browser->getPageErrors() ?? [],
                'node_tmpdir' => $sharedMemory, 'profile' => $profile,
                'node_environment_binding' => 'scoped inherited TMPDIR; Browsershot5.0.5 has no setNodeEnv',
            ];
            rd_write_report($artifacts.'/renderer.json', $report);
            rd_require($external === [] && $report['failed_requests'] === [] && $report['page_errors'] === [], 'Browser resource or script failure.');
        },
        $privateRoot === null ? null : static fn () => clear_synced_runtime_root($privateRoot),
    );
    $pdf = (string) file_get_contents($temporary);
    rd_require(str_starts_with($pdf, '%PDF-') && str_contains(substr($pdf, -4096), '%%EOF'), 'Invalid PDF envelope.');
    sync_path($temporary, false);
    sync_tree($artifacts);
    commit_pdf_output($temporary, $options['--output']);
}

if (defined('PLIEGO_REAL_DOCUMENT_ADAPTER_LIBRARY') && PLIEGO_REAL_DOCUMENT_ADAPTER_LIBRARY === true) {
    return;
}

try {
    $mode = $argv[1] ?? '';
    if ($mode === 'identity') {
        $runtime = invobook_runtime();
        $identity = rd_identity($runtime, __FILE__, __DIR__.'/../browsershot/adapter.php', 'invobook-browsershot-5.0.5-puppeteer-25.8.0', ['spatie/browsershot' => '5.0.5']);
        foreach (['node', 'chrome'] as $name) {
            $path = $runtime[$name.'_path'];
            // chrome.exe --version can open an existing user's browser session
            // on Windows instead of returning a version. Hosted measurement is
            // Linux-only; keep that local identity field explicitly unavailable.
            $version = $name === 'chrome' && PHP_OS_FAMILY === 'Windows' ? null : command_version($path);
            $identity += [$name.'_path' => $path, $name.'_sha256' => hash_file('sha256', $path), $name.'_version' => $version];
        }
        $identity += [
            'node_modules_path' => $runtime['node_modules'], 'node_modules_sha256' => tree_sha256($runtime['node_modules']),
            'package_lock_sha256' => hash_file('sha256', required_file(dirname($runtime['node_modules']).'/package-lock.json')),
            'puppeteer_version' => '25.8.0', 'puppeteer_provenance' => 'harness dependency; not locked by upstream Invobook',
        ];
        echo json_encode($identity, JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES).PHP_EOL;
    } elseif ($mode === 'render') {
        invobook_render(array_slice($argv, 2));
    } else {
        throw new RuntimeException('Expected identity or render.');
    }
} catch (Throwable $error) {
    fwrite(STDERR, 'invobook-browsershot: '.$error->getMessage().PHP_EOL);
    exit(1);
}
