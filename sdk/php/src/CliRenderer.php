<?php

declare(strict_types=1);

namespace Pliego\Php;

use InvalidArgumentException;
use JsonException;
use Pliego\Php\Exception\EngineRenderException;
use Pliego\Php\Exception\InvalidRequestException;
use Pliego\Php\Exception\RenderException;
use RuntimeException;

/**
 * One-render-per-process bridge. It deliberately does not model a persistent
 * daemon protocol.
 */
final class CliRenderer
{
    private const BRIDGE_TIMINGS_FILE = '.pliego-bridge-timings.json';

    /** @var non-empty-list<string> */
    private readonly array $command;

    private readonly int $timeoutSeconds;
    private ?int $runtimeResolutionNanoseconds;

    /**
     * @param non-empty-list<string> $command Production uses ["/path/to/pliego"].
     */
    public function __construct(
        array $command,
        int $timeoutSeconds = 60,
        ?int $runtimeResolutionNanoseconds = null,
    ) {
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
        if ($runtimeResolutionNanoseconds !== null && $runtimeResolutionNanoseconds < 0) {
            throw new InvalidArgumentException('runtimeResolutionNanoseconds cannot be negative');
        }

        $this->command = $command;
        $this->timeoutSeconds = $timeoutSeconds;
        $this->runtimeResolutionNanoseconds = $runtimeResolutionNanoseconds;
    }

    /**
     * @param array<string, string> $assets Bundle-relative path => source file.
     * @param array{total_started_ns?: int, laravel_setup_ns?: int, view_render_ns?: int} $bridgeContext
     */
    public function render(
        string $html,
        string $inputBundle,
        string $output,
        string $artifacts,
        ?RenderOptions $options = null,
        array $assets = [],
        array $bridgeContext = [],
    ): RenderResult {
        $rendererStartedAt = hrtime(true);
        $context = $this->normalizeBridgeContext($bridgeContext, $rendererStartedAt);
        $phaseNanoseconds = [
            'bundle_staging' => 0,
            'asset_manifest_hash' => 0,
            'process_launch' => 0,
            'stdin_stdout' => 0,
            'native_wait' => 0,
            'result_parse' => 0,
            'cleanup' => 0,
        ];
        $options ??= new RenderOptions();
        foreach ([$inputBundle, $output, $artifacts] as $path) {
            if (!$this->isAbsolutePath($path) || str_contains($path, "\0")) {
                throw new InvalidArgumentException("render paths must be absolute: {$path}");
            }
        }
        if (file_exists($output) || is_link($output)) {
            throw new InvalidArgumentException("render output already exists: {$output}");
        }
        $outputDirectory = dirname($output);
        if (!is_dir($outputDirectory)) {
            throw new InvalidArgumentException("render output directory does not exist: {$outputDirectory}");
        }

        $runtimeResolutionNanoseconds = $this->runtimeResolutionNanoseconds;
        $bundleStartedAt = hrtime(true);
        $this->writeInputBundle(
            $inputBundle,
            $html,
            $options,
            $assets,
            $phaseNanoseconds['asset_manifest_hash'],
        );
        $phaseNanoseconds['bundle_staging'] = hrtime(true)
            - $bundleStartedAt
            - $phaseNanoseconds['asset_manifest_hash'];
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

        $ioStartedAt = hrtime(true);
        $stdoutFile = tmpfile();
        $stderrFile = tmpfile();
        $phaseNanoseconds['stdin_stdout'] += hrtime(true) - $ioStartedAt;
        if (!is_resource($stdoutFile) || !is_resource($stderrFile)) {
            if (is_resource($stdoutFile)) {
                fclose($stdoutFile);
            }
            if (is_resource($stderrFile)) {
                fclose($stderrFile);
            }
            throw $this->failure(
                EngineRenderException::class,
                'PLIEGO_PROCESS_FAILED',
                -1,
                '',
                'cannot create Pliego process output streams',
                $inputBundle,
                $artifacts,
                $jobPath,
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                [],
            );
        }

        $pipes = [];
        $launchStartedAt = hrtime(true);
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
        $phaseNanoseconds['process_launch'] = hrtime(true) - $launchStartedAt;
        if (!is_resource($process)) {
            foreach ($pipes as $pipe) {
                if (is_resource($pipe)) {
                    fclose($pipe);
                }
            }
            fclose($stdoutFile);
            fclose($stderrFile);
            throw $this->failure(
                EngineRenderException::class,
                'PLIEGO_PROCESS_FAILED',
                -1,
                '',
                'cannot start the Pliego process',
                $inputBundle,
                $artifacts,
                $jobPath,
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                [],
            );
        }

        $ioStartedAt = hrtime(true);
        fclose($pipes[0]);
        $phaseNanoseconds['stdin_stdout'] += hrtime(true) - $ioStartedAt;
        $waitStartedAt = hrtime(true);
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
        $phaseNanoseconds['native_wait'] = hrtime(true) - $waitStartedAt;
        $ioStartedAt = hrtime(true);
        rewind($stdoutFile);
        rewind($stderrFile);
        $stdout = stream_get_contents($stdoutFile);
        $stderr = stream_get_contents($stderrFile);
        fclose($stdoutFile);
        fclose($stderrFile);
        $phaseNanoseconds['stdin_stdout'] += hrtime(true) - $ioStartedAt;

        $parseStartedAt = hrtime(true);
        $metadata = $this->lastJsonObject($stdout === false ? '' : $stdout);
        $phaseNanoseconds['result_parse'] = hrtime(true) - $parseStartedAt;

        if ($timedOut) {
            throw $this->failure(
                EngineRenderException::class,
                'RENDER_TIMEOUT',
                $exitCode,
                $stderr === false ? '' : $stderr,
                "Pliego render exceeded {$this->timeoutSeconds} seconds",
                $inputBundle,
                $artifacts,
                $jobPath,
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                $metadata,
            );
        }

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
            throw $this->failure(
                $exception,
                $code,
                $exitCode,
                $stderr === false ? '' : $stderr,
                $message,
                $inputBundle,
                $artifacts,
                $jobPath,
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                $metadata,
            );
        }
        if (($metadata['scene']['capture_status'] ?? null) !== 'complete') {
            $code = is_string($metadata['scene']['capture_code'] ?? null)
                ? $metadata['scene']['capture_code']
                : 'SCENE_CAPTURE_INCOMPLETE';
            throw $this->failure(
                EngineRenderException::class,
                $code,
                $exitCode,
                $stderr === false ? '' : $stderr,
                'Pliego did not capture the complete document paint',
                $inputBundle,
                $artifacts,
                $jobPath,
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                $metadata,
            );
        }
        if (!is_file($output)) {
            throw $this->failure(
                EngineRenderException::class,
                'OUTPUT_MISSING',
                $exitCode,
                $stderr === false ? '' : $stderr,
                "Pliego reported success without publishing {$output}",
                $inputBundle,
                $artifacts,
                $jobPath,
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                $metadata,
            );
        }

        $cleanupStartedAt = hrtime(true);
        JobRetention::mark($jobPath, 'success');
        $phaseNanoseconds['cleanup'] = hrtime(true) - $cleanupStartedAt;
        $bridgeTimings = $this->persistBridgeTimings(
            $jobPath,
            $this->finishBridgeTimings(
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                $metadata,
            ),
        );
        if ($runtimeResolutionNanoseconds !== null) {
            $this->runtimeResolutionNanoseconds = 0;
        }

        return new RenderResult($output, $artifacts, $inputBundle, $metadata, $bridgeTimings);
    }

    /**
     * @param array<string, string> $assets
     */
    private function writeInputBundle(
        string $directory,
        string $html,
        RenderOptions $options,
        array $assets,
        int &$assetManifestHashNanoseconds,
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
            $manifestStartedAt = hrtime(true);
            $manifestAssets[$relative] = [
                'bytes' => filesize($destination),
                'sha256' => 'sha256:'.hash_file('sha256', $destination),
            ];
            $assetManifestHashNanoseconds += (int) (hrtime(true) - $manifestStartedAt);
        }

        $manifestStartedAt = hrtime(true);
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
        $assetManifestHashNanoseconds += (int) (hrtime(true) - $manifestStartedAt);
    }

    /**
     * @param class-string<RenderException> $exception
     * @param array{total_started_ns: int, laravel_setup_ns: int|null, view_render_ns: int|null} $context
     * @param array<string, int> $phaseNanoseconds
     * @param array<string, mixed> $metadata
     */
    private function failure(
        string $exception,
        string $code,
        int $exitCode,
        string $stderr,
        string $message,
        string $inputBundle,
        string $artifacts,
        string $jobPath,
        array $context,
        ?int $runtimeResolutionNanoseconds,
        array $phaseNanoseconds,
        array $metadata,
    ): RenderException {
        $cleanupStartedAt = hrtime(true);
        JobRetention::mark($jobPath, 'failure');
        $phaseNanoseconds['cleanup'] = hrtime(true) - $cleanupStartedAt;
        $bridgeTimings = $this->persistBridgeTimings(
            $jobPath,
            $this->finishBridgeTimings(
                $context,
                $runtimeResolutionNanoseconds,
                $phaseNanoseconds,
                $metadata,
            ),
        );
        if ($runtimeResolutionNanoseconds !== null) {
            $this->runtimeResolutionNanoseconds = 0;
        }

        return new $exception(
            $code,
            $exitCode,
            $stderr,
            $message,
            $inputBundle,
            $artifacts,
            $bridgeTimings,
        );
    }

    /**
     * @param array{total_started_ns?: int, laravel_setup_ns?: int, view_render_ns?: int} $context
     * @return array{total_started_ns: int, laravel_setup_ns: int|null, view_render_ns: int|null}
     */
    private function normalizeBridgeContext(array $context, int $rendererStartedAt): array
    {
        foreach ($context as $name => $value) {
            if (
                !in_array($name, ['total_started_ns', 'laravel_setup_ns', 'view_render_ns'], true)
                || !is_int($value)
                || $value < 0
            ) {
                throw new InvalidArgumentException('bridge timing context is invalid');
            }
        }
        $totalStartedAt = $context['total_started_ns'] ?? $rendererStartedAt;
        if ($totalStartedAt > $rendererStartedAt) {
            throw new InvalidArgumentException('bridge timing start cannot be in the future');
        }
        if (
            ($context['laravel_setup_ns'] ?? 0) + ($context['view_render_ns'] ?? 0)
            > $rendererStartedAt - $totalStartedAt
        ) {
            throw new InvalidArgumentException('bridge timing phases exceed elapsed time');
        }

        return [
            'total_started_ns' => $totalStartedAt,
            'laravel_setup_ns' => $context['laravel_setup_ns'] ?? null,
            'view_render_ns' => $context['view_render_ns'] ?? null,
        ];
    }

    /**
     * @param array{total_started_ns: int, laravel_setup_ns: int|null, view_render_ns: int|null} $context
     * @param array<string, int> $phaseNanoseconds
     * @param array<string, mixed> $metadata
     * @return array<string, mixed>
     */
    private function finishBridgeTimings(
        array $context,
        ?int $runtimeResolutionNanoseconds,
        array $phaseNanoseconds,
        array $metadata,
    ): array {
        $totalNanoseconds = hrtime(true) - $context['total_started_ns'];
        $rawTotalMilliseconds = $totalNanoseconds / 1_000_000;
        $totalMilliseconds = $this->milliseconds($totalNanoseconds);
        $attributedNanoseconds = array_sum($phaseNanoseconds)
            + ($context['laravel_setup_ns'] ?? 0)
            + ($context['view_render_ns'] ?? 0);
        $nativeEngineTimings = $metadata['engine_timings'] ?? null;
        $nativeEngineUnavailable = null;
        if (!array_key_exists('engine_timings', $metadata)) {
            $nativeEngineMilliseconds = null;
            $nativeEngineUnavailable = 'engine-total-not-reported';
        } elseif (
            !is_array($nativeEngineTimings)
            || ($nativeEngineTimings['schema'] ?? null) !== 'pliego.engine-timings'
            || ($nativeEngineTimings['version'] ?? null) !== 1
            || ($nativeEngineTimings['unit'] ?? null) !== 'milliseconds'
            || ($nativeEngineTimings['measurement_boundary'] ?? null) !== 'before_timings_artifact_write'
            || !array_key_exists('total_ms', $nativeEngineTimings)
        ) {
            $nativeEngineMilliseconds = null;
            $nativeEngineUnavailable = 'engine-timing-contract-invalid';
        } else {
            $nativeEngineMilliseconds = $nativeEngineTimings['total_ms'];
        }
        if ($nativeEngineUnavailable === null) {
            if (
                (!is_int($nativeEngineMilliseconds) && !is_float($nativeEngineMilliseconds))
                || !is_finite((float) $nativeEngineMilliseconds)
                || $nativeEngineMilliseconds < 0
            ) {
                $nativeEngineMilliseconds = null;
                $nativeEngineUnavailable = 'engine-total-invalid';
            } elseif ($nativeEngineMilliseconds > $rawTotalMilliseconds) {
                $nativeEngineMilliseconds = null;
                $nativeEngineUnavailable = 'engine-total-exceeds-render-boundary';
            } else {
                $nativeEngineMilliseconds = (float) $nativeEngineMilliseconds;
            }
        }

        $phases = [
            'laravel_setup' => $this->milliseconds($context['laravel_setup_ns']),
            'view_render' => $this->milliseconds($context['view_render_ns']),
            'bundle_staging' => $this->milliseconds($phaseNanoseconds['bundle_staging']),
            'asset_manifest_hash' => $this->milliseconds($phaseNanoseconds['asset_manifest_hash']),
            'process_launch' => $this->milliseconds($phaseNanoseconds['process_launch']),
            'stdin_stdout' => $this->milliseconds($phaseNanoseconds['stdin_stdout']),
            'native_wait' => $this->milliseconds($phaseNanoseconds['native_wait']),
            'result_parse' => $this->milliseconds($phaseNanoseconds['result_parse']),
            'publication_copy' => null,
            'cleanup' => $this->milliseconds($phaseNanoseconds['cleanup']),
            'unattributed' => $this->milliseconds(max(0, $totalNanoseconds - $attributedNanoseconds)),
        ];
        $unavailable = [
            'runtime_install' => 'explicit-artisan-command',
            'publication_copy' => 'native-engine-publishes-directly',
        ];
        foreach ([
            'laravel_setup' => $context['laravel_setup_ns'],
            'view_render' => $context['view_render_ns'],
        ] as $phase => $value) {
            if ($value === null) {
                $unavailable[$phase] = 'outside-this-render-path';
            }
        }
        if ($runtimeResolutionNanoseconds === null) {
            $unavailable['runtime_resolution'] = 'outside-this-render-path';
        }
        if ($nativeEngineUnavailable !== null) {
            $unavailable['native_engine'] = $nativeEngineUnavailable;
            $unavailable['bridge_overhead'] = 'native-engine-total-unavailable';
        }

        return [
            'schema' => 'pliego.php-bridge-timings',
            'version' => 1,
            'measurement_boundary' => 'render-invocation-before-timing-diagnostics',
            'total_ms' => $totalMilliseconds,
            'native_engine_ms' => $nativeEngineMilliseconds,
            'bridge_overhead_ms' => $nativeEngineMilliseconds === null
                ? null
                : round($rawTotalMilliseconds - $nativeEngineMilliseconds, 3),
            'setup_ms' => [
                'runtime_resolution' => $this->milliseconds($runtimeResolutionNanoseconds),
                'runtime_install' => null,
            ],
            'phases_ms' => $phases,
            'unavailable' => $unavailable,
            'notes' => [
                'measurement_boundary' => 'total_ms covers this render invocation and stops before its timing diagnostic is encoded and written.',
                'setup_ms' => 'One-time setup observations are reported separately and are never added to total_ms.',
                'native_engine_ms' => 'Engine-reported time is contained within total_ms and may overlap process_launch and native_wait.',
                'cleanup' => 'The bridge closes process resources and marks the retained job; scheduled pruning removes it later.',
            ],
        ];
    }

    /**
     * @param array<string, mixed> $timings
     * @return array<string, mixed>
     */
    private function persistBridgeTimings(string $jobPath, array $timings): array
    {
        $timings['diagnostics'] = ['retained' => true];
        try {
            $json = json_encode(
                $timings,
                JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_PRESERVE_ZERO_FRACTION,
            )."\n";
        } catch (JsonException) {
            $timings['diagnostics']['retained'] = false;

            return $timings;
        }
        if (file_put_contents(
            $jobPath.DIRECTORY_SEPARATOR.self::BRIDGE_TIMINGS_FILE,
            $json,
            LOCK_EX,
        ) === false) {
            $timings['diagnostics']['retained'] = false;
        }

        return $timings;
    }

    private function milliseconds(?int $nanoseconds): ?float
    {
        return $nanoseconds === null ? null : round($nanoseconds / 1_000_000, 3);
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
