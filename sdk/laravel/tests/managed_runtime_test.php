<?php

declare(strict_types=1);

use Illuminate\Http\Client\Factory as HttpFactory;
use Pliego\Laravel\ManagedRuntime;

$autoload = getenv('PLIEGO_TEST_AUTOLOAD');
require is_string($autoload) && $autoload !== ''
    ? $autoload
    : dirname(__DIR__).'/vendor/autoload.php';

function runtimeExpect(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

$bundled = json_decode(
    (string) file_get_contents(dirname(__DIR__).'/resources/runtimes.json'),
    true,
    flags: JSON_THROW_ON_ERROR,
);
runtimeExpect(
    ($bundled['version'] ?? null) === '0.3.3'
        && ($bundled['api'] ?? null) === 2
        && ($bundled['release_ready'] ?? null) === false,
    'bundled v0.3 runtime manifest must remain pending until native promotion',
);

function removeRuntimeFixture(string $path): void
{
    if (! is_dir($path)) {
        return;
    }
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($path, FilesystemIterator::SKIP_DOTS),
        RecursiveIteratorIterator::CHILD_FIRST,
    );
    foreach ($iterator as $entry) {
        $entry->isDir() && ! $entry->isLink()
            ? rmdir($entry->getPathname())
            : unlink($entry->getPathname());
    }
    rmdir($path);
}

$platforms = [
    ['Linux', 'x86_64', 'linux-x86_64'],
    ['Linux', 'AMD64', 'linux-x86_64'],
    ['Windows', 'amd64', 'windows-x86_64'],
    ['Darwin', 'x86_64', 'macos-x86_64'],
    ['Darwin', 'arm64', 'macos-aarch64'],
    ['Darwin', 'aarch64', 'macos-aarch64'],
];
foreach ($platforms as [$os, $machine, $expected]) {
    runtimeExpect(ManagedRuntime::platformKey($os, $machine) === $expected, "platform mapping failed for {$os} {$machine}");
}
try {
    ManagedRuntime::platformKey('Linux', 'aarch64');
    throw new RuntimeException('unsupported Linux architecture was accepted');
} catch (RuntimeException $error) {
    runtimeExpect(str_contains($error->getMessage(), 'no runtime'), 'unsupported platform error is not actionable');
}

$fixture = sys_get_temp_dir().DIRECTORY_SEPARATOR.'pliego-runtime-test-'.bin2hex(random_bytes(8));
runtimeExpect(mkdir($fixture, 0700), 'cannot create managed-runtime fixture');
register_shutdown_function(static fn () => removeRuntimeFixture($fixture));

$version = '9.8.7-test.1';
$bundle = "pliego-{$version}-linux-x86_64";
$nestedLicense = 'licenses/krilla-0.8.2/LICENSE_MIT';
$files = ['pliego', 'LICENSE', 'INSTALL.txt', 'VERSION.txt', $nestedLicense];
$tarPath = $fixture.DIRECTORY_SEPARATOR.'runtime.tar';
$archive = new PharData($tarPath);
foreach ($files as $file) {
    $archive->addFromString("{$bundle}/{$file}", $file === 'pliego' ? 'fake executable' : $file);
}
$archive->addFromString("{$bundle}/unexpected.txt", 'must not be extracted');
$compressed = $archive->compress(Phar::GZ);
unset($compressed, $archive);
$archivePath = $tarPath.'.gz';

$manifest = [
    'schema' => 1,
    'version' => $version,
    'api' => 2,
    'release_ready' => true,
    'assets' => [
        'linux-x86_64' => [
            'bytes' => filesize($archivePath),
            'sha256' => hash_file('sha256', $archivePath),
            'files' => $files,
        ],
        'windows-x86_64' => [
            'bytes' => 1,
            'sha256' => str_repeat('1', 64),
            'files' => ['pliego.exe', 'libEGL.dll', 'libGLESv2.dll', 'LICENSE', 'INSTALL.txt', 'VERSION.txt'],
        ],
        'macos-x86_64' => [
            'bytes' => 1,
            'sha256' => str_repeat('2', 64),
            'files' => $files,
        ],
        'macos-aarch64' => [
            'bytes' => 1,
            'sha256' => str_repeat('3', 64),
            'files' => $files,
        ],
    ],
];
$manifestPath = $fixture.DIRECTORY_SEPARATOR.'runtimes.json';
file_put_contents($manifestPath, json_encode($manifest, JSON_PRETTY_PRINT | JSON_THROW_ON_ERROR));
$url = "https://github.com/oxhq/pliego/releases/download/v{$version}/{$bundle}.tar.gz";
$body = file_get_contents($archivePath);
runtimeExpect(is_string($body), 'cannot read runtime fixture archive');
$http = new HttpFactory;
$http->fake([$url => HttpFactory::response($body)]);
$probe = static fn (string $binary): string => is_file($binary)
    ? "pliego {$version}\npliego-api 2\n"
    : throw new RuntimeException('version probe did not receive the installed binary');
$runtimeRoot = $fixture.DIRECTORY_SEPARATOR.'installed';
$runtime = new ManagedRuntime($runtimeRoot, $manifestPath, $http, versionProbe: $probe);

$binary = $runtime->install('linux-x86_64');
runtimeExpect(is_file($binary), 'managed binary was not installed');
runtimeExpect(file_get_contents($binary) === 'fake executable', 'managed binary contents changed');
runtimeExpect(is_file(dirname($binary).DIRECTORY_SEPARATOR.$nestedLicense), 'nested license was not installed');
runtimeExpect(! file_exists(dirname($binary).DIRECTORY_SEPARATOR.'unexpected.txt'), 'unlisted archive file was extracted');
runtimeExpect($runtime->binary('linux-x86_64') === $binary, 'managed binary was not resolved');
runtimeExpect($runtime->install('linux-x86_64') === $binary, 'managed install is not idempotent');
runtimeExpect($http->recorded()->count() === 1, 'idempotent install downloaded the archive again');

$pendingManifest = $manifest;
$pendingManifest['release_ready'] = false;
$pendingManifestPath = $fixture.DIRECTORY_SEPARATOR.'pending-runtimes.json';
file_put_contents($pendingManifestPath, json_encode($pendingManifest, JSON_THROW_ON_ERROR));
try {
    (new ManagedRuntime(
        $fixture.DIRECTORY_SEPARATOR.'pending-install',
        $pendingManifestPath,
        new HttpFactory,
        versionProbe: $probe,
    ))->install('linux-x86_64');
    throw new RuntimeException('unfinalized managed runtime metadata was installed');
} catch (RuntimeException $error) {
    runtimeExpect(str_contains($error->getMessage(), 'not finalized'), 'pending runtime error is not actionable');
}

$override = new ManagedRuntime(
    $fixture.DIRECTORY_SEPARATOR.'override-root',
    $pendingManifestPath,
    new HttpFactory,
    'custom-pliego',
    $probe,
);
runtimeExpect($override->binary('linux-x86_64') === 'custom-pliego', 'PLIEGO_BINARY override lost precedence');
try {
    $override->install('linux-x86_64');
    throw new RuntimeException('managed runtime installed while PLIEGO_BINARY was set');
} catch (RuntimeException $error) {
    runtimeExpect(str_contains($error->getMessage(), 'Unset PLIEGO_BINARY'), 'override install error is not actionable');
}

$badManifest = $manifest;
$badManifest['assets']['linux-x86_64']['sha256'] = str_repeat('0', 64);
$badManifestPath = $fixture.DIRECTORY_SEPARATOR.'bad-runtimes.json';
file_put_contents($badManifestPath, json_encode($badManifest, JSON_THROW_ON_ERROR));
$badHttp = new HttpFactory;
$badHttp->fake([$url => HttpFactory::response($body)]);
try {
    (new ManagedRuntime(
        $fixture.DIRECTORY_SEPARATOR.'bad-install',
        $badManifestPath,
        $badHttp,
        versionProbe: $probe,
    ))->install('linux-x86_64');
    throw new RuntimeException('tampered runtime archive was installed');
} catch (RuntimeException $error) {
    runtimeExpect(str_contains($error->getMessage(), 'SHA-256 mismatch'), 'tampered archive error lost its cause');
}
runtimeExpect(
    ! is_dir($fixture.DIRECTORY_SEPARATOR."bad-install/{$version}/linux-x86_64"),
    'tampered runtime reached the final directory',
);

$unsafeManifest = $manifest;
$unsafeManifest['assets']['linux-x86_64']['files'][] = '../escape';
$unsafeManifestPath = $fixture.DIRECTORY_SEPARATOR.'unsafe-runtimes.json';
file_put_contents($unsafeManifestPath, json_encode($unsafeManifest, JSON_THROW_ON_ERROR));
try {
    new ManagedRuntime($fixture.DIRECTORY_SEPARATOR.'unsafe-install', $unsafeManifestPath, new HttpFactory, versionProbe: $probe);
    throw new RuntimeException('unsafe runtime manifest path was accepted');
} catch (RuntimeException $error) {
    runtimeExpect(str_contains($error->getMessage(), 'unsafe'), 'unsafe path error lost its cause');
}
runtimeExpect(! file_exists($fixture.DIRECTORY_SEPARATOR.'escape'), 'unsafe manifest escaped its install root');

echo "managed runtime: ok\n";
