<?php

declare(strict_types=1);

// Pure guards only: no app dependency loading, browser, renderer, or measurement.
define('PLIEGO_REAL_DOCUMENT_ADAPTER_LIBRARY', true);
$adapter = $argv[1] ?? '';
if (!in_array($adapter, ['invobook-browsershot', 'aureus-dompdf'], true)) {
    throw new RuntimeException('Select one exact application adapter.');
}
require dirname(__DIR__).'/adapters/'.$adapter.'/adapter.php';

$temporary = sys_get_temp_dir().'/pliego-app-adapter-test-'.bin2hex(random_bytes(8));
rd_require(mkdir($temporary, 0700), 'Cannot create test directory.');
try {
    rd_write_report($temporary.'/complete.json', ['pass' => true]);
    rd_require(json_decode(file_get_contents($temporary.'/complete.json'), true) === ['pass' => true], 'Evidence readback differs.');
    foreach (['short', 'false', 'existing', 'missing-parent'] as $case) {
        try {
            $path = $temporary.'/'.($case === 'existing' ? 'complete' : $case).'.json';
            if ($case === 'missing-parent') {
                $path = $temporary.'/missing/report.json';
            }
            $writer = match ($case) {
                'short' => static fn ($stream, string $bytes): int|false => fwrite($stream, substr($bytes, 0, 1)),
                'false' => static fn ($stream, string $bytes): false => false,
                default => null,
            };
            rd_write_report($path, ['pass' => true], $writer);
            throw new LogicException('Report failure was accepted: '.$case);
        } catch (RuntimeException) {
            // No success is returned for a missing, partial, or colliding report.
        }
    }
    file_put_contents($temporary.'/runtime.json', json_encode([
        'schema' => 'pliego.real-document-runtime.v1', 'app_root' => $temporary,
        'resolved_app_root' => '/unverified-root',
    ], JSON_THROW_ON_ERROR));
    try {
        rd_runtime($temporary, str_repeat('0', 64));
        throw new LogicException('Config overrode a verified derived path.');
    } catch (RuntimeException $error) {
        rd_require($error->getMessage() === 'Unknown app runtime config fields.', 'Derived-path guard ran too late.');
    }
    file_put_contents($temporary.'/DejaVuSans.ttf', 'not the frozen font');
    try {
        rd_font_closure($temporary, 2);
        throw new LogicException('Changed source font was accepted.');
    } catch (RuntimeException $error) {
        rd_require(str_contains($error->getMessage(), 'Staged font differs'), 'Wrong staged-font rejection.');
    }
} finally {
    @unlink($temporary.'/DejaVuSans.ttf');
    foreach (glob($temporary.'/*.json') as $path) {
        unlink($path);
    }
    rmdir($temporary);
}

rd_require(parse_render_options(['--output', 'a.pdf', '--artifacts', 'a', '--page-size', '1x2au', '--page-margins', '0,0,0,0au'])['--page-size'] === '1x2au', 'Adapter argument transport lost exact Au.');
rd_require(is_bare_input_name('document.html') && !is_bare_input_name('../document.html'), 'Bare input guard changed.');
if ($adapter === 'invobook-browsershot') {
    $previous = getenv('TMPDIR');
    $previousEnv = $_ENV;
    $previousServer = $_SERVER;
    try {
        putenv('TMPDIR=adapter-parent');
        $_ENV['TMPDIR'] = 'startup-env';
        $_SERVER['TMPDIR'] = 'startup-server';
        invobook_with_node_temp('child-temp', static function (): void {
            rd_require(getenv('TMPDIR') === 'child-temp', 'Child temp was not bound.');
            rd_require($_ENV['TMPDIR'] === 'child-temp' && $_SERVER['TMPDIR'] === 'child-temp', 'PHP superglobal precedence can override child temp.');
        });
        rd_require(getenv('TMPDIR') === 'adapter-parent', 'Parent temp was not restored.');
        rd_require($_ENV['TMPDIR'] === 'startup-env' && $_SERVER['TMPDIR'] === 'startup-server', 'Superglobal values were not restored.');
        try {
            invobook_with_node_temp('child-temp', static function (): void {
                throw new RuntimeException('expected child failure');
            });
            throw new LogicException('Child failure was swallowed.');
        } catch (RuntimeException $error) {
            rd_require($error->getMessage() === 'expected child failure', 'Wrong child error.');
        }
        rd_require(getenv('TMPDIR') === 'adapter-parent', 'Failed child changed parent temp.');
        rd_require($_ENV['TMPDIR'] === 'startup-env' && $_SERVER['TMPDIR'] === 'startup-server', 'Failed child changed superglobal temp.');
        putenv('TMPDIR');
        unset($_ENV['TMPDIR'], $_SERVER['TMPDIR']);
        invobook_with_node_temp('child-temp', static function (): void {});
        rd_require(getenv('TMPDIR') === false, 'Originally unset temp was not restored.');
        rd_require(!array_key_exists('TMPDIR', $_ENV) && !array_key_exists('TMPDIR', $_SERVER), 'Originally missing superglobal entries were created.');
        invobook_with_node_temp(null, static function (): void {
            rd_require(getenv('TMPDIR') === false, 'Unbound smoke path changed temp.');
        });
        if (($argv[2] ?? '') === '--actual-symfony') {
            invobook_runtime();
            putenv('TMPDIR=parent-getenv');
            $_ENV['TMPDIR'] = 'parent-env';
            $_SERVER['TMPDIR'] = 'parent-server';
            invobook_with_node_temp('expected-child-temp', static function (): void {
                $process = new Symfony\Component\Process\Process([PHP_BINARY, '-r', 'echo getenv("TMPDIR");']);
                $process->setTimeout(5)->mustRun();
                rd_require($process->getOutput() === 'expected-child-temp', 'Actual locked Symfony child did not receive the scoped environment.');
            });
            rd_require(getenv('TMPDIR') === 'parent-getenv' && $_ENV['TMPDIR'] === 'parent-env' && $_SERVER['TMPDIR'] === 'parent-server', 'Actual child changed parent environment.');
        }
    } finally {
        putenv($previous === false ? 'TMPDIR' : 'TMPDIR='.$previous);
        $_ENV = $previousEnv;
        $_SERVER = $previousServer;
    }
    rd_require(strlen(INVOBOOK_HTML) === 64 && strlen(INVOBOOK_LOCK) === 64, 'Invoice pins missing.');
} else {
    rd_require(count(AUREUS_DOCUMENTS) === 2, 'Aureus corpus inventory changed.');
    $profiles = array_values(AUREUS_DOCUMENTS);
    rd_require($profiles[0]['orientation'] === 'landscape' && $profiles[0]['faces'] === 4 && $profiles[0]['margins'] === '2268,2268,5669,2268au', 'Ledger original action mapping changed.');
    rd_require($profiles[1]['orientation'] === 'portrait' && $profiles[1]['faces'] === 2 && $profiles[1]['margins'] === '2721,2721,2721,2721au', 'Work order original UA margin mapping changed.');
}
echo $adapter." pure adapter guards passed\n";
