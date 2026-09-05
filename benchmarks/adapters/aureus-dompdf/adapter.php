#!/usr/bin/php
<?php

declare(strict_types=1);

use Illuminate\Foundation\Application;

define('PLIEGO_BENCHMARK_ADAPTER_LIBRARY', true);
require __DIR__.'/../dompdf/adapter.php';
require __DIR__.'/../real_document_runtime.php';

const AUREUS_LOCK = '82133507ad710cc2748d95cba0ea3dfe5d375728c0d8d3587303e91d027c5fae';
const AUREUS_CONFIG = '4d22df6fb0728d4a4020c8fe435263cb2e2bb81c7a675ea4f30e2a78537df10b';
const AUREUS_DOCUMENTS = [
    'ccb11d4c713a66d71fe7798e03bfbeb3b7fe90b74229790c7e9c1a512a7aa105' => [
        'family' => 'aureus-ledger-300-repaired', 'orientation' => 'landscape',
        'page' => '67351x47622au', 'margins' => '2268,2268,5669,2268au', 'faces' => 4,
    ],
    'a08f2e7250be77df689e62768b5929bed7a41ed9d4ae1783a0e297fc709f2c3c' => [
        'family' => 'aureus-manufacturing-008-font-closed', 'orientation' => 'portrait',
        'page' => '47622x67351au', 'margins' => '2721,2721,2721,2721au', 'faces' => 2,
    ],
];

/** @return array{0: array, 1: array, 2: Application} */
function aureus_runtime(): array
{
    $runtime = rd_runtime(__DIR__, AUREUS_LOCK);
    rd_package('dompdf/dompdf', '3.1.6');
    rd_package('barryvdh/laravel-dompdf', '3.1.2');
    $app = $runtime['resolved_app_root'];
    rd_require(! is_file($app.'/config/dompdf.php'), 'Unexpected application dompdf override.');
    $path = required_file($app.'/vendor/barryvdh/laravel-dompdf/config/dompdf.php');
    rd_require(hash_file('sha256', $path) === AUREUS_CONFIG, 'Original vendor configuration changed.');
    // Base container only resolves storage_path/base_path in the original config.
    // Never load .env, kernel, service providers, database or application routes.
    $container = new Application($app);
    $config = require $path;
    rd_require(! $container->bound('db'), 'Unexpected database binding.');

    return [$runtime, $config, $container];
}

function aureus_render(array $arguments): void
{
    rd_require(PHP_OS_FAMILY === 'Linux', 'Measured application adapters require the Linux cgroup/durability recipe.');
    [$input, $options, $artifacts] = rd_render_paths($arguments);
    $html = (string) file_get_contents($input);
    $profile = AUREUS_DOCUMENTS[hash('sha256', $html)] ?? null;
    rd_require(is_array($profile), 'Input is not one of the two frozen Aureus documents.');
    $inputFonts = rd_font_closure(dirname($input).'/resources', $profile['faces']);
    rd_require($options['--page-size'] === $profile['page'] && $options['--page-margins'] === $profile['margins'], 'Requested geometry differs from the frozen original action.');
    [$runtime, $config, $container] = aureus_runtime();
    $effective = $config['options'];
    foreach (['font-cache', 'temp'] as $name) {
        rd_require(mkdir($artifacts.'/'.$name, 0700), 'Fresh renderer state is required.');
    }
    foreach (['font_dir' => 'font-cache', 'font_cache' => 'font-cache', 'temp_dir' => 'temp'] as $key => $name) {
        $effective[$key] = $artifacts.'/'.$name;
    }
    $effective['chroot'] = dirname($input);
    $dompdf = new Dompdf\Dompdf($effective);
    rd_require($dompdf->getOptions()->getDpi() === 96 && ! $dompdf->getOptions()->getIsRemoteEnabled(), 'Original DPI/offline settings changed.');
    $dompdf->setProtocol('file://');
    $dompdf->setBaseHost('');
    $dompdf->setBasePath(str_replace('\\', '/', dirname($input)).'/');
    $dompdf->setPaper('a4', $profile['orientation']);
    rd_require(str_replace(['€', '£'], ['&euro;', '&pound;'], $html) === $html, 'Original wrapper conversion would change frozen engine bytes.');
    $dompdf->loadHtml($html, 'UTF-8');
    $dompdf->render();
    $fonts = [
        'normal' => 'DejaVuSans.ttf', 'bold' => 'DejaVuSans-Bold.ttf',
        'italic' => 'DejaVuSans-Oblique.ttf', 'bold_italic' => 'DejaVuSans-BoldOblique.ttf',
    ];
    $selected = [];
    foreach (array_slice($fonts, 0, $profile['faces']) as $style => $filename) {
        $source = required_file(dirname($input).'/resources/'.$filename);
        $registered = required_file($dompdf->getFontMetrics()->getFont('dejavu sans', $style).'.ttf');
        rd_require(str_starts_with($registered, $artifacts.DIRECTORY_SEPARATOR.'font-cache'.DIRECTORY_SEPARATOR), 'Font came from fallback instead of the supplied resource.');
        rd_require(hash_file('sha256', $registered) === hash_file('sha256', $source), 'Registered font differs from the supplied bytes.');
        $selected[] = ['style' => $style, 'source' => $filename, 'sha256' => hash_file('sha256', $registered)];
    }
    global $_dompdf_warnings;
    rd_require(($_dompdf_warnings ?? []) === [] && ! $container->bound('db'), 'Renderer warning or unexpected database binding.');
    $report = [
        'family' => $profile['family'], 'input_sha256' => hash('sha256', $html),
        'source_config_sha256' => AUREUS_CONFIG, 'original_options' => $config['options'],
        'effective_options' => $effective, 'input_fonts' => $inputFonts, 'selected_fonts' => $selected, 'warnings' => [],
        'page_count' => $dompdf->getCanvas()->get_page_count(), 'database_booted' => false,
    ];
    rd_write_report($artifacts.'/renderer.json', $report);
    sync_artifact_tree($artifacts);
    publish_pdf($options['--output'], $dompdf->output());
}

if (defined('PLIEGO_REAL_DOCUMENT_ADAPTER_LIBRARY') && PLIEGO_REAL_DOCUMENT_ADAPTER_LIBRARY === true) {
    return;
}

try {
    $mode = $argv[1] ?? '';
    if ($mode === 'identity') {
        [$runtime] = aureus_runtime();
        $identity = rd_identity($runtime, __FILE__, __DIR__.'/../dompdf/adapter.php', 'aureus-dompdf-3.1.6', [
            'dompdf/dompdf' => '3.1.6', 'barryvdh/laravel-dompdf' => '3.1.2',
        ]);
        $identity['source_config_sha256'] = AUREUS_CONFIG;
        echo json_encode($identity, JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES).PHP_EOL;
    } elseif ($mode === 'render') {
        aureus_render(array_slice($argv, 2));
    } else {
        throw new RuntimeException('Expected identity or render.');
    }
} catch (Throwable $error) {
    fwrite(STDERR, 'aureus-dompdf: '.$error->getMessage().PHP_EOL);
    exit(1);
}
