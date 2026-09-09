<?php

declare(strict_types=1);
use Composer\InstalledVersions;

const RD_DEJAVU_FONTS = [
    'DejaVuSans.ttf' => '7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954',
    'DejaVuSans-Bold.ttf' => 'e6476c1b80502924294eed40894c5b18e06c181444ca953e5334262df9c27724',
    'DejaVuSans-Oblique.ttf' => '4af75fa16ee6d3ad43e1ecec41862c24954af26a55c6bb1ebb27bd486a50f5f4',
    'DejaVuSans-BoldOblique.ttf' => 'eb436dca0c2594b73d8b603b892e374fdfd8d885d25ffb4f18df4c4c0b49e50f',
];

/** Small shared closure checks for the two application-derived adapters. */
function rd_require(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

function rd_font_closure(string $directory, int $faces): array
{
    rd_require(in_array($faces, [2, 4], true), 'Unsupported frozen font inventory.');
    $result = [];
    foreach (array_slice(RD_DEJAVU_FONTS, 0, $faces) as $name => $hash) {
        $path = required_file($directory.'/'.$name);
        rd_require(!is_link($directory.'/'.$name) && hash_file('sha256', $path) === $hash, 'Staged font differs from the frozen original: '.$name);
        $result[] = ['file' => $name, 'sha256' => $hash, 'bytes' => filesize($path)];
    }
    return $result;
}

/** A report failure must prevent PDF publication, including a partial write. */
function rd_write_report(string $path, array $report, ?callable $write = null): void
{
    $bytes = json_encode($report, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT).PHP_EOL;
    $stream = @fopen($path, 'xb');
    rd_require(is_resource($stream), 'Cannot create fresh renderer evidence.');
    $write ??= static fn ($stream, string $bytes): int|false => fwrite($stream, $bytes);
    try {
        rd_require($write($stream, $bytes) === strlen($bytes), 'Incomplete renderer evidence write.');
        rd_require(fflush($stream), 'Cannot flush renderer evidence.');
    } finally {
        fclose($stream);
    }
}

/** @return array<string, mixed> */
function rd_runtime(string $directory, string $lockHash): array
{
    $configPath = required_file($directory.'/runtime.json');
    $config = json_decode((string) file_get_contents($configPath), true, flags: JSON_THROW_ON_ERROR);
    rd_require(is_array($config) && ($config['schema'] ?? null) === 'pliego.real-document-runtime.v1', 'Invalid app runtime config.');
    rd_require(array_diff(array_keys($config), ['schema', 'app_root', 'node_path', 'chrome_path', 'node_modules']) === [], 'Unknown app runtime config fields.');
    rd_require(is_string($config['app_root'] ?? null), 'Explicit app_root is required.');
    $app = realpath($config['app_root']);
    rd_require(is_string($app) && is_dir($app), 'Application root is unavailable.');
    $lock = required_file($app.'/composer.lock');
    rd_require(hash_file('sha256', $lock) === $lockHash, 'Application lock differs from the reviewed frozen dependency repair.');
    $autoload = required_file($app.'/vendor/autoload.php');
    require_once $autoload;

    return $config + ['config_path' => $configPath, 'lock_path' => $lock, 'resolved_app_root' => $app];
}

function rd_package(string $package, string $version): void
{
    rd_require(
        ltrim((string) InstalledVersions::getPrettyVersion($package), 'v') === $version,
        'Unexpected installed package version: '.$package,
    );
}

/** @return array<string, mixed> */
function rd_identity(array $runtime, string $adapter, string $helper, string $target, array $packages): array
{
    $app = $runtime['resolved_app_root'];
    $packageIdentities = [];
    foreach ($packages as $package => $version) {
        rd_package($package, $version);
        $packageIdentities[$package] = [
            'version' => $version,
            'reference' => InstalledVersions::getReference($package),
        ];
    }

    return [
        'contract' => 'pliego.benchmark-adapter.v1', 'target' => $target,
        'packages' => $packageIdentities,
        'adapter_path' => realpath($adapter), 'adapter_sha256' => hash_file('sha256', $adapter),
        'helper_path' => realpath($helper), 'helper_sha256' => hash_file('sha256', $helper),
        'runtime_helper_sha256' => hash_file('sha256', __FILE__),
        'runtime_config_path' => $runtime['config_path'],
        'runtime_config_sha256' => hash_file('sha256', $runtime['config_path']),
        'composer_lock_sha256' => hash_file('sha256', $runtime['lock_path']),
        'composer_vendor_path' => realpath($app.'/vendor'),
        'composer_vendor_sha256' => tree_sha256($app.'/vendor'),
        'php_path' => realpath(PHP_BINARY), 'php_sha256' => hash_file('sha256', PHP_BINARY),
        'php_version' => PHP_VERSION,
    ];
}

/** @return array{0: string, 1: array<string, string>, 2: string} */
function rd_render_paths(array $arguments): array
{
    $input = array_shift($arguments);
    rd_require(is_string($input) && is_bare_input_name($input), 'Render requires a bare input filename.');
    $options = parse_render_options($arguments);
    $cwd = realpath(getcwd() ?: '');
    $path = $cwd === false ? false : realpath($cwd.DIRECTORY_SEPARATOR.$input);
    rd_require(is_string($path) && is_file($path) && dirname($path) === $cwd, 'Input must be directly inside cwd.');
    $artifacts = realpath($options['--artifacts']);
    rd_require(is_string($artifacts) && is_dir($artifacts) && ! is_link($options['--artifacts']), 'Artifact directory is unavailable.');
    rd_require(! file_exists($options['--output']) && ! is_link($options['--output']), 'Output must be fresh.');

    return [$path, $options, $artifacts];
}
