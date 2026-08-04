<?php

declare(strict_types=1);

namespace Pliego\Laravel;

use Closure;
use FilesystemIterator;
use Illuminate\Http\Client\Factory as HttpFactory;
use JsonException;
use PharData;
use RecursiveDirectoryIterator;
use RecursiveIteratorIterator;
use RuntimeException;
use Throwable;

final class ManagedRuntime
{
    private const RELEASE_ROOT = 'https://github.com/oxhq/pliego/releases/download';

    /** @var array{schema: int, version: string, api: int, assets: array<string, array{bytes: int, sha256: string, files: list<string>}>} */
    private array $manifest;

    /** @var Closure(string): string */
    private Closure $versionProbe;

    public function __construct(
        private readonly string $installRoot,
        string $manifestPath,
        private readonly HttpFactory $http,
        private readonly ?string $binaryOverride = null,
        ?Closure $versionProbe = null,
    ) {
        if ($installRoot === '' || str_contains($installRoot, "\0")) {
            throw new RuntimeException('Pliego runtime directory is invalid');
        }
        if ($binaryOverride !== null && ($binaryOverride === '' || str_contains($binaryOverride, "\0"))) {
            throw new RuntimeException('PLIEGO_BINARY is invalid');
        }

        $this->manifest = $this->readManifest($manifestPath);
        $this->versionProbe = $versionProbe ?? Closure::fromCallable([$this, 'probeVersion']);
    }

    public function version(): string
    {
        return $this->manifest['version'];
    }

    public static function platformKey(string $osFamily = PHP_OS_FAMILY, ?string $machine = null): string
    {
        $architecture = match (strtolower(trim($machine ?? php_uname('m')))) {
            'amd64', 'x86_64' => 'x86_64',
            'aarch64', 'arm64' => 'aarch64',
            default => null,
        };

        $platform = match ([$osFamily, $architecture]) {
            ['Linux', 'x86_64'] => 'linux-x86_64',
            ['Windows', 'x86_64'] => 'windows-x86_64',
            ['Darwin', 'x86_64'] => 'macos-x86_64',
            ['Darwin', 'aarch64'] => 'macos-aarch64',
            default => null,
        };
        if ($platform === null) {
            throw new RuntimeException("Pliego has no runtime for {$osFamily} ".($machine ?? php_uname('m')));
        }

        return $platform;
    }

    public function binary(?string $platform = null): string
    {
        if ($this->binaryOverride !== null) {
            return $this->binaryOverride;
        }

        $platform ??= self::platformKey();
        $path = $this->runtimeDirectory($this->installRoot, $platform)
            .DIRECTORY_SEPARATOR.$this->executable($platform);
        if (!is_file($path) || is_link($path)) {
            throw new RuntimeException('Pliego is not installed. Run `php artisan pliego:install`.');
        }

        return $path;
    }

    public function install(?string $platform = null): string
    {
        if ($this->binaryOverride !== null) {
            throw new RuntimeException('Unset PLIEGO_BINARY before installing the managed Pliego runtime.');
        }

        $platform ??= self::platformKey();
        $asset = $this->asset($platform);
        $root = $this->ensureDirectory($this->installRoot);
        $lock = fopen($root.DIRECTORY_SEPARATOR.'.install.lock', 'c+b');
        if (!is_resource($lock) || !flock($lock, LOCK_EX)) {
            if (is_resource($lock)) {
                fclose($lock);
            }
            throw new RuntimeException("Cannot lock Pliego runtime directory {$root}");
        }

        try {
            $final = $this->runtimeDirectory($root, $platform);
            if (is_dir($final) && !is_link($final)) {
                $this->assertInstalled($final, $platform, $asset['files']);

                return $final.DIRECTORY_SEPARATOR.$this->executable($platform);
            }
            if (file_exists($final) || is_link($final)) {
                throw new RuntimeException("Unsafe Pliego runtime path {$final}");
            }

            $stage = $root.DIRECTORY_SEPARATOR.'.install-'.bin2hex(random_bytes(8));
            if (!mkdir($stage, 0700)) {
                throw new RuntimeException("Cannot create Pliego staging directory {$stage}");
            }

            try {
                $archive = $stage.DIRECTORY_SEPARATOR.$asset['archive'];
                $this->download($asset['url'], $archive);
                $this->verifyArchive($archive, $asset['bytes'], $asset['sha256']);
                $candidate = $this->extract($archive, $stage, $asset['bundle'], $asset['files']);
                $this->assertInstalled($candidate, $platform, $asset['files']);

                $versionDirectory = dirname($final);
                $this->ensureDirectory($versionDirectory);
                if (!rename($candidate, $final)) {
                    throw new RuntimeException("Cannot publish Pliego runtime to {$final}");
                }
            } finally {
                $this->removeTree($stage);
            }

            return $final.DIRECTORY_SEPARATOR.$this->executable($platform);
        } finally {
            flock($lock, LOCK_UN);
            fclose($lock);
        }
    }

    /**
     * @return array{archive: string, bundle: string, bytes: int, sha256: string, files: list<string>, url: string}
     */
    private function asset(string $platform): array
    {
        $entry = $this->manifest['assets'][$platform] ?? null;
        if (!is_array($entry)) {
            throw new RuntimeException("Pliego runtime manifest has no {$platform} asset");
        }

        $bundle = "pliego-{$this->manifest['version']}-{$platform}";
        $archive = $bundle.($platform === 'windows-x86_64' ? '.zip' : '.tar.gz');

        return [
            'archive' => $archive,
            'bundle' => $bundle,
            'bytes' => $entry['bytes'],
            'sha256' => $entry['sha256'],
            'files' => $entry['files'],
            'url' => self::RELEASE_ROOT.'/v'.$this->manifest['version'].'/'.$archive,
        ];
    }

    private function download(string $url, string $destination): void
    {
        $this->http
            ->connectTimeout(15)
            ->timeout(600)
            ->maxRedirects(5)
            ->sink($destination)
            ->get($url)
            ->throw();
    }

    private function verifyArchive(string $archive, int $bytes, string $sha256): void
    {
        $actualBytes = filesize($archive);
        if ($actualBytes !== $bytes) {
            throw new RuntimeException("Pliego archive size mismatch: expected {$bytes}, received ".($actualBytes === false ? 'unknown' : $actualBytes));
        }

        $actualHash = hash_file('sha256', $archive);
        if (!is_string($actualHash) || !hash_equals($sha256, $actualHash)) {
            throw new RuntimeException('Pliego archive SHA-256 mismatch');
        }
    }

    /** @param list<string> $files */
    private function extract(string $archive, string $stage, string $bundle, array $files): string
    {
        if (!class_exists(PharData::class)) {
            throw new RuntimeException('The PHP Phar extension is required by `pliego:install`');
        }

        $entries = [];
        try {
            $package = new PharData($archive);
            foreach ($files as $file) {
                $entry = "{$bundle}/{$file}";
                if (!isset($package[$entry]) || !$package[$entry]->isFile() || $package[$entry]->isLink()) {
                    throw new RuntimeException("Pliego archive is missing safe file {$entry}");
                }
                $entries[] = $entry;
            }
            if (!$package->extractTo($stage, $entries, false)) {
                throw new RuntimeException('Cannot extract the Pliego archive');
            }
        } catch (RuntimeException $error) {
            throw $error;
        } catch (Throwable $error) {
            throw new RuntimeException('Cannot read the Pliego archive: '.$error->getMessage(), 0, $error);
        }

        $candidate = $stage.DIRECTORY_SEPARATOR.$bundle;
        $resolvedCandidate = realpath($candidate);
        if ($resolvedCandidate === false || !is_dir($resolvedCandidate) || is_link($candidate)) {
            throw new RuntimeException('Pliego archive did not contain its expected root');
        }
        foreach ($files as $file) {
            $path = $candidate.DIRECTORY_SEPARATOR.str_replace('/', DIRECTORY_SEPARATOR, $file);
            $resolved = realpath($path);
            if ($resolved === false || !is_file($resolved) || is_link($path) || !$this->within($resolved, $resolvedCandidate)) {
                throw new RuntimeException("Pliego archive published an unsafe file {$file}");
            }
        }

        return $candidate;
    }

    /** @param list<string> $files */
    private function assertInstalled(string $directory, string $platform, array $files): void
    {
        foreach ($files as $file) {
            $path = $directory.DIRECTORY_SEPARATOR.str_replace('/', DIRECTORY_SEPARATOR, $file);
            if (!is_file($path) || is_link($path)) {
                throw new RuntimeException("Installed Pliego runtime is missing {$file}");
            }
        }

        $binary = $directory.DIRECTORY_SEPARATOR.$this->executable($platform);
        if ($platform !== 'windows-x86_64' && !chmod($binary, 0755)) {
            throw new RuntimeException("Cannot make Pliego executable {$binary}");
        }
        $lines = preg_split('/\R/', trim(($this->versionProbe)($binary))) ?: [];
        if (
            ($lines[0] ?? null) !== 'pliego '.$this->manifest['version']
            || !in_array('pliego-api '.$this->manifest['api'], $lines, true)
        ) {
            throw new RuntimeException('Installed Pliego runtime reported an incompatible version');
        }
    }

    private function probeVersion(string $binary): string
    {
        $pipes = [];
        $process = proc_open(
            [$binary, '--version'],
            [
                0 => ['pipe', 'r'],
                1 => ['pipe', 'w'],
                2 => ['pipe', 'w'],
            ],
            $pipes,
        );
        if (!is_resource($process)) {
            throw new RuntimeException("Cannot start installed Pliego runtime {$binary}");
        }

        fclose($pipes[0]);
        $stdout = stream_get_contents($pipes[1]);
        $stderr = stream_get_contents($pipes[2]);
        fclose($pipes[1]);
        fclose($pipes[2]);
        $exitCode = proc_close($process);
        if ($exitCode !== 0 || $stdout === false) {
            throw new RuntimeException('Installed Pliego runtime failed `--version`: '.trim($stderr === false ? '' : $stderr));
        }

        return $stdout;
    }

    /**
     * @return array{schema: int, version: string, api: int, assets: array<string, array{bytes: int, sha256: string, files: list<string>}>}
     */
    private function readManifest(string $path): array
    {
        try {
            $contents = file_get_contents($path);
            $manifest = is_string($contents) ? json_decode($contents, true, flags: JSON_THROW_ON_ERROR) : null;
        } catch (JsonException $error) {
            throw new RuntimeException('Pliego runtime manifest is invalid JSON', 0, $error);
        }
        if (!is_array($manifest)) {
            throw new RuntimeException("Cannot read Pliego runtime manifest {$path}");
        }

        $version = $manifest['version'] ?? null;
        $api = $manifest['api'] ?? null;
        $assets = $manifest['assets'] ?? null;
        $platforms = ['linux-x86_64', 'windows-x86_64', 'macos-x86_64', 'macos-aarch64'];
        if (
            ($manifest['schema'] ?? null) !== 1
            || !is_string($version)
            || preg_match('/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/D', $version) !== 1
            || !is_int($api)
            || $api < 1
            || !is_array($assets)
            || array_keys($assets) !== $platforms
        ) {
            throw new RuntimeException('Pliego runtime manifest contract is invalid');
        }
        foreach ($platforms as $platform) {
            $asset = $assets[$platform];
            if (
                !is_array($asset)
                || !is_int($asset['bytes'] ?? null)
                || $asset['bytes'] < 1
                || !is_string($asset['sha256'] ?? null)
                || preg_match('/^[0-9a-f]{64}$/D', $asset['sha256']) !== 1
                || !is_array($asset['files'] ?? null)
                || $asset['files'] === []
            ) {
                throw new RuntimeException("Pliego runtime manifest asset {$platform} is invalid");
            }
            foreach ($asset['files'] as $file) {
                if (!is_string($file) || !$this->safeRelativePath($file)) {
                    throw new RuntimeException("Pliego runtime manifest file for {$platform} is unsafe");
                }
            }
            if (!in_array($this->executable($platform), $asset['files'], true)) {
                throw new RuntimeException("Pliego runtime manifest asset {$platform} has no executable");
            }
        }

        /** @var array{schema: int, version: string, api: int, assets: array<string, array{bytes: int, sha256: string, files: list<string>}>} $manifest */
        return $manifest;
    }

    private function safeRelativePath(string $path): bool
    {
        if ($path === '' || str_contains($path, "\0") || str_contains($path, '\\') || str_starts_with($path, '/')) {
            return false;
        }
        foreach (explode('/', $path) as $segment) {
            if ($segment === '' || $segment === '.' || $segment === '..' || str_contains($segment, ':')) {
                return false;
            }
        }

        return true;
    }

    private function executable(string $platform): string
    {
        return $platform === 'windows-x86_64' ? 'pliego.exe' : 'pliego';
    }

    private function runtimeDirectory(string $root, string $platform): string
    {
        return rtrim($root, '/\\').DIRECTORY_SEPARATOR.$this->manifest['version'].DIRECTORY_SEPARATOR.$platform;
    }

    private function ensureDirectory(string $path): string
    {
        if (is_link($path)) {
            throw new RuntimeException("Pliego runtime directory cannot be a symlink: {$path}");
        }
        if (!is_dir($path) && !mkdir($path, 0700, true) && !is_dir($path)) {
            throw new RuntimeException("Cannot create Pliego runtime directory {$path}");
        }
        $resolved = realpath($path);
        if ($resolved === false || !is_dir($resolved) || is_link($path)) {
            throw new RuntimeException("Cannot resolve Pliego runtime directory {$path}");
        }

        return $resolved;
    }

    private function within(string $path, string $root): bool
    {
        $prefix = rtrim($root, '/\\').DIRECTORY_SEPARATOR;

        return DIRECTORY_SEPARATOR === '\\'
            ? strncasecmp($path, $prefix, strlen($prefix)) === 0
            : str_starts_with($path, $prefix);
    }

    private function removeTree(string $path): void
    {
        if (!file_exists($path) && !is_link($path)) {
            return;
        }
        if (is_file($path) || is_link($path)) {
            @unlink($path);

            return;
        }

        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($path, FilesystemIterator::SKIP_DOTS),
            RecursiveIteratorIterator::CHILD_FIRST,
        );
        foreach ($iterator as $entry) {
            $entry->isDir() && !$entry->isLink()
                ? @rmdir($entry->getPathname())
                : @unlink($entry->getPathname());
        }
        @rmdir($path);
    }
}
