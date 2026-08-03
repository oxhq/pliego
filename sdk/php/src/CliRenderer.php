<?php

declare(strict_types=1);

namespace Pliego\Php\Experimental;

use InvalidArgumentException;
use JsonException;
use Pliego\Php\Experimental\Exception\EngineRenderException;
use Pliego\Php\Experimental\Exception\InvalidRequestException;
use RuntimeException;

/**
 * Experimental one-render-per-process bridge. It deliberately does not model
 * the planned daemon protocol.
 */
final readonly class CliRenderer
{
    /**
     * @param non-empty-list<string> $command Production uses ["/path/to/pliego"].
     */
    public function __construct(private array $command, private int $timeoutSeconds = 60)
    {
        if ($command === []) {
            throw new InvalidArgumentException('command must contain non-empty strings');
        }
        foreach ($command as $part) {
            if (!is_string($part) || $part === '' || str_contains($part, "\0")) {
                throw new InvalidArgumentException('command must contain non-empty strings');
            }
        }
        if ($timeoutSeconds < 1) {
            throw new InvalidArgumentException('timeoutSeconds must be at least 1');
        }
    }

    /**
     * @param array<string, string> $assets Bundle-relative path => source file.
     */
    public function render(
        string $html,
        string $inputBundle,
        string $output,
        string $artifacts,
        ?RenderOptions $options = null,
        array $assets = [],
    ): RenderResult {
        $options ??= new RenderOptions();
        foreach ([$inputBundle, $output, $artifacts] as $path) {
            if (!$this->isAbsolutePath($path) || str_contains($path, "\0")) {
                throw new InvalidArgumentException("render paths must be absolute: {$path}");
            }
        }

        $this->writeInputBundle($inputBundle, $html, $options, $assets);
        $jobPath = dirname($inputBundle);
        JobRetention::mark($jobPath, 'running');
        $arguments = [
            ...$this->command,
            'render',
            'document.html',
            '--output',
            $output,
            '--artifacts',
            $artifacts,
            '--locale',
            $options->locale,
            '--timezone',
            $options->timezone,
            '--page-size',
            $options->pageSize,
            '--page-margins',
            $options->pageMargins,
        ];
        foreach ($options->allowedHttpRoots as $root) {
            $arguments[] = '--allow-http-root';
            $arguments[] = $root;
        }

        $stdoutFile = tmpfile();
        $stderrFile = tmpfile();
        if (!is_resource($stdoutFile) || !is_resource($stderrFile)) {
            if (is_resource($stdoutFile)) {
                fclose($stdoutFile);
            }
            if (is_resource($stderrFile)) {
                fclose($stderrFile);
            }
            JobRetention::mark($jobPath, 'failure');
            throw new RuntimeException('cannot create Pliego process output streams');
        }

        $pipes = [];
        $process = proc_open(
            $arguments,
            [
                0 => ['pipe', 'r'],
                1 => $stdoutFile,
                2 => $stderrFile,
            ],
            $pipes,
            $inputBundle,
        );
        if (!is_resource($process)) {
            fclose($stdoutFile);
            fclose($stderrFile);
            JobRetention::mark($jobPath, 'failure');
            throw new RuntimeException('cannot start the Pliego process');
        }

        fclose($pipes[0]);
        $deadline = hrtime(true) + ($this->timeoutSeconds * 1_000_000_000);
        $timedOut = false;
        $status = proc_get_status($process);
        while ($status['running']) {
            if (hrtime(true) >= $deadline) {
                $timedOut = true;
                proc_terminate($process, 9);
                break;
            }
            usleep(10_000);
            $status = proc_get_status($process);
        }
        $exitCode = $status['exitcode'];
        $closedExitCode = proc_close($process);
        if ($exitCode < 0) {
            $exitCode = $closedExitCode;
        }
        rewind($stdoutFile);
        rewind($stderrFile);
        $stdout = stream_get_contents($stdoutFile);
        $stderr = stream_get_contents($stderrFile);
        fclose($stdoutFile);
        fclose($stderrFile);

        if ($timedOut) {
            if (is_file($output)) {
                @unlink($output);
            }
            JobRetention::mark($jobPath, 'failure');
            throw new EngineRenderException(
                'RENDER_TIMEOUT',
                $exitCode,
                $stderr === false ? '' : $stderr,
                "Pliego render exceeded {$this->timeoutSeconds} seconds",
                $inputBundle,
                $artifacts,
            );
        }

        $metadata = $this->lastJsonObject($stdout === false ? '' : $stdout);

        if ($exitCode !== 0 || ($metadata['status'] ?? null) !== 'rendered') {
            $code = is_string($metadata['error']['code'] ?? null)
                ? $metadata['error']['code']
                : 'PLIEGO_PROCESS_FAILED';
            $message = is_string($metadata['error']['message'] ?? null)
                ? $metadata['error']['message']
                : 'Pliego did not return a rendered result';
            $exception = $exitCode === 2 || $code === 'INVALID_REQUEST'
                ? InvalidRequestException::class
                : EngineRenderException::class;
            JobRetention::mark($jobPath, 'failure');
            throw new $exception(
                $code,
                $exitCode,
                $stderr === false ? '' : $stderr,
                $message,
                $inputBundle,
                $artifacts,
            );
        }
        if (!is_file($output)) {
            JobRetention::mark($jobPath, 'failure');
            throw new EngineRenderException(
                'OUTPUT_MISSING',
                $exitCode,
                $stderr === false ? '' : $stderr,
                "Pliego reported success without publishing {$output}",
                $inputBundle,
                $artifacts,
            );
        }

        JobRetention::mark($jobPath, 'success');

        return new RenderResult($output, $artifacts, $inputBundle, $metadata);
    }

    /**
     * @param array<string, string> $assets
     */
    private function writeInputBundle(
        string $directory,
        string $html,
        RenderOptions $options,
        array $assets,
    ): void {
        if (!@mkdir($directory, 0700, false)) {
            throw new RuntimeException("cannot create exclusive input bundle {$directory}");
        }
        if (file_put_contents("{$directory}/document.html", $html, LOCK_EX) === false) {
            throw new RuntimeException("cannot write {$directory}/document.html");
        }

        $manifestAssets = [];
        $bundlePaths = ['document.html' => true, 'input-bundle.json' => true];
        ksort($assets, SORT_STRING);
        foreach ($assets as $relative => $source) {
            if (!is_string($relative) || !is_string($source)) {
                throw new InvalidArgumentException('bundle asset paths and sources must be strings');
            }
            $relative = str_replace('\\', '/', $relative);
            $parts = explode('/', $relative);
            if (
                $relative === ''
                || str_contains($relative, "\0")
                || str_contains($relative, ':')
                || str_starts_with($relative, '/')
                || preg_match('/^[A-Za-z]:/', $relative) === 1
                || array_intersect($parts, ['', '.', '..']) !== []
                || array_filter($parts, static fn (string $part): bool => $part !== rtrim($part, ". ")) !== []
                || array_filter(
                    $parts,
                    static fn (string $part): bool => preg_match(
                        '/^(?:CON|PRN|AUX|NUL|CLOCK\\$|COM[1-9]|LPT[1-9])(?:\\.|$)/i',
                        $part,
                    ) === 1,
                )
            ) {
                throw new InvalidArgumentException("unsafe bundle asset path: {$relative}");
            }
            $portablePath = strtolower($relative);
            if (isset($bundlePaths[$portablePath])) {
                throw new InvalidArgumentException("reserved or duplicate bundle asset path: {$relative}");
            }
            $bundlePaths[$portablePath] = true;
            if (!is_file($source)) {
                throw new InvalidArgumentException("bundle asset is not a file: {$source}");
            }

            $destination = "{$directory}/{$relative}";
            $parent = dirname($destination);
            if (!is_dir($parent) && !mkdir($parent, 0700, true)) {
                throw new RuntimeException("cannot create bundle directory {$parent}");
            }
            if (file_exists($destination)) {
                throw new InvalidArgumentException("bundle asset destination already exists: {$relative}");
            }
            if (!copy($source, $destination)) {
                throw new RuntimeException("cannot copy bundle asset {$source}");
            }
            if (!is_file($destination)) {
                throw new RuntimeException("bundle asset did not create a regular file: {$relative}");
            }
            $manifestAssets[$relative] = [
                'bytes' => filesize($destination),
                'sha256' => 'sha256:'.hash_file('sha256', $destination),
            ];
        }

        $manifest = [
            'schema' => 'pliego.php-input-bundle',
            'version' => 1,
            'document' => 'document.html',
            'document_sha256' => 'sha256:'.hash('sha256', $html),
            'assets' => $manifestAssets,
            'environment' => [
                'locale' => $options->locale,
                'timezone' => $options->timezone,
                'page_size' => $options->pageSize,
                'page_margins' => $options->pageMargins,
                'network' => $options->allowedHttpRoots === []
                    ? ['policy' => 'deny']
                    : ['policy' => 'allow-roots', 'roots' => $options->allowedHttpRoots],
            ],
        ];
        try {
            $json = json_encode(
                $manifest,
                JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES,
            )."\n";
        } catch (JsonException $error) {
            throw new RuntimeException('cannot encode input bundle manifest', previous: $error);
        }
        if (file_put_contents("{$directory}/input-bundle.json", $json, LOCK_EX) === false) {
            throw new RuntimeException("cannot write {$directory}/input-bundle.json");
        }
    }

    /**
     * @return array<string, mixed>
     */
    private function lastJsonObject(string $stdout): array
    {
        $lines = array_reverse(preg_split('/\R/', trim($stdout)) ?: []);
        foreach ($lines as $line) {
            try {
                $value = json_decode($line, true, flags: JSON_THROW_ON_ERROR);
                if (is_array($value)) {
                    return $value;
                }
            } catch (JsonException) {
                continue;
            }
        }

        return [];
    }

    private function isAbsolutePath(string $path): bool
    {
        return str_starts_with($path, '/')
            || str_starts_with($path, '\\\\')
            || preg_match('/^[A-Za-z]:[\\\\\/]/', $path) === 1;
    }
}
