<?php

declare(strict_types=1);

namespace Pliego\Php;

use InvalidArgumentException;
use JsonException;
use Pliego\Php\Exception\InvocationException;
use Pliego\Php\Exception\RenderFailedException;
use Pliego\Php\Exception\TransportException;
use Pliego\Php\Exception\UnsupportedContractException;
use Pliego\Php\Internal\Api2InputJob;
use Pliego\Php\Internal\Api2ResultValidator;
use Pliego\Php\Internal\CanonicalJson;
use RuntimeException;
use Throwable;
use UnexpectedValueException;

/** Public API 2 client for one deterministic render in one native process. */
final class DocumentEngine
{
    private const REQUEST_MAX_BYTES = 1_048_576;
    private const RESULT_MAX_BYTES = 16_777_216;
    private const STDERR_MAX_BYTES = 1_048_576;
    private const BRIDGE_TIMINGS_FILE = '.pliego-bridge-timings.json';

    /** @var non-empty-list<string> */
    private readonly array $command;
    private readonly string $workDirectory;
    private readonly int $timeoutSeconds;
    private readonly int $probeTimeoutSeconds;
    private ?int $runtimeResolutionNanoseconds;
    private ?RuntimeContract $runtimeContract = null;

    /**
     * @param non-empty-list<string> $command Production uses ["/path/to/pliego"].
     */
    public function __construct(
        array $command,
        string $workDirectory,
        int $timeoutSeconds = 65,
        int $probeTimeoutSeconds = 180,
        ?int $runtimeResolutionNanoseconds = null,
    ) {
        if ($command === [] || !array_is_list($command)) {
            throw new InvalidArgumentException('command must be a non-empty list');
        }
        foreach ($command as $part) {
            if (!is_string($part) || $part === '' || str_contains($part, "\0")) {
                throw new InvalidArgumentException('command must contain non-empty strings');
            }
        }
        if ($timeoutSeconds < 1 || $probeTimeoutSeconds < 1) {
            throw new InvalidArgumentException('process timeouts must be at least 1 second');
        }
        if ($runtimeResolutionNanoseconds !== null && $runtimeResolutionNanoseconds < 0) {
            throw new InvalidArgumentException('runtimeResolutionNanoseconds cannot be negative');
        }

        $this->command = $command;
        $this->workDirectory = $this->prepareWorkRoot($workDirectory);
        $this->timeoutSeconds = $timeoutSeconds;
        $this->probeTimeoutSeconds = $probeTimeoutSeconds;
        $this->runtimeResolutionNanoseconds = $runtimeResolutionNanoseconds;
    }

    /** @return array<string, mixed> */
    public static function requiredProtocol(): array
    {
        return [
            'api' => 2,
            'input_manifest' => ['schema' => 'pliego.input-manifest', 'version' => 1],
            'request' => ['schema' => 'pliego.render-request', 'version' => 1],
            'result' => ['schema' => 'pliego.render-result', 'version' => 1],
            'document_scene' => ['schema' => 'pliego.document-scene', 'version' => 2],
            'bundle_manifest' => ['schema' => 'pliego.bundle-manifest', 'version' => 1],
        ];
    }

    public function contract(): RuntimeContract
    {
        if ($this->runtimeContract !== null) {
            return $this->runtimeContract;
        }
        try {
            $contract = RuntimeContract::probe($this->command, $this->probeTimeoutSeconds);
        } catch (Throwable $error) {
            throw new TransportException(
                'cannot probe the Pliego API 2 executable: '.$error->getMessage(),
                previous: $error,
            );
        }
        $selection = $contract->select(self::requiredProtocol());
        if ($selection === null || $selection['contract']['profiles'] !== []) {
            throw new UnsupportedContractException(
                'Pliego does not advertise the exact API 2 tuple required by oxhq/pliego-php 0.3',
            );
        }

        return $this->runtimeContract = $contract;
    }

    /**
     * @param array<array-key, string|InputAsset> $assets
     * @param array{total_started_ns?: int, laravel_setup_ns?: int, view_render_ns?: int} $bridgeContext
     */
    public function render(
        string $html,
        ?RenderOptions $options = null,
        array $assets = [],
        array $bridgeContext = [],
    ): RenderResult {
        $rendererStartedAt = hrtime(true);
        $context = $this->normalizeBridgeContext($bridgeContext, $rendererStartedAt);
        $phases = ['contract_probe' => 0, 'input_staging' => 0, 'process' => 0, 'validation' => 0, 'cleanup' => 0];
        $options ??= new RenderOptions();
        if ($options->allowedHttpRoots !== []) {
            throw new InvalidArgumentException(
                'API 2 denies live network access; prefetch the resource and supply it through assets instead',
            );
        }
        if ($options->hostWallMilliseconds >= $this->timeoutSeconds * 1_000) {
            throw new InvalidArgumentException(
                'hostWallMilliseconds must be lower than the DocumentEngine process timeout',
            );
        }

        $probeStartedAt = hrtime(true);
        $contract = $this->contract();
        $phases['contract_probe'] = hrtime(true) - $probeStartedAt;
        $jobPath = $this->allocateOuterJob();
        JobRetention::mark($jobPath, 'running');
        $runtimeJobPath = $jobPath.DIRECTORY_SEPARATOR.'runtime';

        try {
            $stagingStartedAt = hrtime(true);
            $inputJob = Api2InputJob::stage($jobPath, $html, $assets);
            $request = $this->buildRequest($inputJob, $options);
            $requestBytes = CanonicalJson::encodeFrame($request, 'render request');
            if (strlen($requestBytes) > self::REQUEST_MAX_BYTES) {
                throw new InvalidArgumentException('canonical API 2 render request exceeds 1 MiB');
            }
            $phases['input_staging'] = hrtime(true) - $stagingStartedAt;

            $processStartedAt = hrtime(true);
            try {
                $process = $this->execute($inputJob->runtimeRoot, $requestBytes);
            } catch (TransportException $error) {
                throw new TransportException(
                    $error->getMessage(),
                    $jobPath,
                    $runtimeJobPath,
                    $error->exitCode,
                    $error->stdout,
                    $error->stderr,
                    $error,
                );
            }
            $phases['process'] = hrtime(true) - $processStartedAt;
            if ($process['timed_out']) {
                throw new TransportException(
                    "Pliego render-api2 exceeded {$this->timeoutSeconds} seconds",
                    $jobPath,
                    $runtimeJobPath,
                    $process['exit_code'],
                    $process['stdout'],
                    $process['stderr'],
                );
            }
            if ($process['stdout_overflow'] || $process['stderr_overflow']) {
                throw new TransportException(
                    'Pliego render-api2 exceeded the SDK output transport limit',
                    $jobPath,
                    $runtimeJobPath,
                    $process['exit_code'],
                    $process['stdout'],
                    $process['stderr'],
                );
            }
            if ($process['exit_code'] === 64) {
                try {
                    throw InvocationException::fromProcessResult(
                        64,
                        $process['stdout'],
                        $process['stderr'],
                        $jobPath,
                        $runtimeJobPath,
                    );
                } catch (InvocationException $error) {
                    throw $error;
                } catch (Throwable $error) {
                    throw new TransportException(
                        'Pliego returned a malformed API 2 invocation error',
                        $jobPath,
                        $runtimeJobPath,
                        64,
                        $process['stdout'],
                        $process['stderr'],
                        $error,
                    );
                }
            }
            if (!in_array($process['exit_code'], [0, 1], true) || $process['stderr'] !== '') {
                throw new TransportException(
                    'Pliego returned an unsupported API 2 exit/stderr combination',
                    $jobPath,
                    $runtimeJobPath,
                    $process['exit_code'],
                    $process['stdout'],
                    $process['stderr'],
                );
            }

            $validationStartedAt = hrtime(true);
            try {
                $result = (new Api2ResultValidator(
                    $inputJob->runtimeRoot,
                    $request,
                    $contract->engine(),
                ))->validate($process['stdout'], $process['exit_code']);
            } catch (UnexpectedValueException $error) {
                throw new TransportException(
                    'Pliego returned an invalid API 2 result: '.$error->getMessage(),
                    $jobPath,
                    $runtimeJobPath,
                    $process['exit_code'],
                    $process['stdout'],
                    $process['stderr'],
                    $error,
                );
            }
            $phases['validation'] = hrtime(true) - $validationStartedAt;

            if ($result['status'] === 'failed') {
                JobRetention::mark($jobPath, 'failure');
                throw new RenderFailedException(
                    $result['error']['kind'],
                    $result,
                    $jobPath,
                    $runtimeJobPath,
                    $runtimeJobPath.DIRECTORY_SEPARATOR.'diagnostics',
                );
            }

            $cleanupStartedAt = hrtime(true);
            JobRetention::mark($jobPath, 'success');
            $phases['cleanup'] = hrtime(true) - $cleanupStartedAt;
            $bridgeTimings = $this->persistBridgeTimings(
                $jobPath,
                $this->finishBridgeTimings($context, $rendererStartedAt, $phases),
            );
            $deliveryPath = $runtimeJobPath.DIRECTORY_SEPARATOR.'delivery';
            $diagnosticsPath = $runtimeJobPath.DIRECTORY_SEPARATOR.'diagnostics';

            return new RenderResult(
                pdfPath: $deliveryPath.DIRECTORY_SEPARATOR.'document.pdf',
                artifactsPath: $diagnosticsPath,
                inputBundlePath: $inputJob->inputPath,
                metadata: $result,
                bridgeTimings: $bridgeTimings,
                jobPath: $jobPath,
                runtimeJobPath: $runtimeJobPath,
                inputPath: $inputJob->inputPath,
                deliveryPath: $deliveryPath,
                diagnosticsPath: $diagnosticsPath,
                scenePath: $deliveryPath.DIRECTORY_SEPARATOR.'scene.json',
                bundlePath: $deliveryPath.DIRECTORY_SEPARATOR.'bundle.json',
            );
        } catch (Throwable $error) {
            $this->markFailureIfRunning($jobPath);
            throw $error;
        } finally {
            if ($this->runtimeResolutionNanoseconds !== null) {
                $this->runtimeResolutionNanoseconds = 0;
            }
        }
    }

    /** @return array<string, mixed> */
    private function buildRequest(Api2InputJob $inputJob, RenderOptions $options): array
    {
        if (!in_array($options->locale, ['en-US', 'es-MX'], true)) {
            throw new InvalidArgumentException('API 2 locale must be en-US or es-MX');
        }
        $timezone = $options->timezone === 'PST8PDT' ? 'America/Tijuana' : $options->timezone;
        if (!in_array($timezone, ['UTC', 'America/Tijuana'], true)) {
            throw new InvalidArgumentException('API 2 timezone must be UTC or America/Tijuana');
        }

        return [
            'schema' => 'pliego.render-request',
            'version' => 1,
            'api' => 2,
            'profile' => null,
            'input' => [
                'entrypoint' => 'document.html',
                'manifest' => $inputJob->manifestDescriptor,
            ],
            'environment' => [
                'locale' => $options->locale,
                'timezone' => $timezone,
            ],
            'page' => [
                'size' => $this->pageSize($options->pageSize),
                'margins_app_units' => $this->pageMargins($options->pageMargins),
                'geometry_authority' => 'request-only-v1',
            ],
            'resources' => [
                'network' => 'deny',
                'host_fonts' => 'deny',
            ],
            'time' => [
                'policy_version' => 1,
                'epoch_unix_ms' => $options->epochUnixMilliseconds,
                'initial_offset_ns' => 0,
            ],
            'settlement' => [
                'policy_version' => 1,
                'infinite_source_policy' => 'fail',
                'empty_checkpoints' => 2,
                'limits' => [
                    'virtual_span_ms' => $options->virtualSpanMilliseconds,
                    'ordinary_tasks' => $options->ordinaryTasks,
                    'microtasks' => $options->microtasks,
                    'rendering_opportunities' => $options->renderingOpportunities,
                    'mutations' => $options->mutations,
                    'host_wall_ms' => $options->hostWallMilliseconds,
                ],
            ],
            'diagnostics' => [
                'retention' => $options->diagnosticsRetention,
            ],
        ];
    }

    /** @return array{name: string}|array{width_app_units: int, height_app_units: int} */
    private function pageSize(string $pageSize): array
    {
        if ($pageSize === 'A4') {
            return ['name' => 'A4'];
        }
        if (preg_match('/^([1-9][0-9]*)x([1-9][0-9]*)$/D', $pageSize, $match) !== 1) {
            throw new InvalidArgumentException('API 2 pageSize must be A4 or integer WIDTHxHEIGHT CSS pixels');
        }

        return [
            'width_app_units' => $this->cssPixelsToAppUnits($match[1], 'page width'),
            'height_app_units' => $this->cssPixelsToAppUnits($match[2], 'page height'),
        ];
    }

    /** @return array{top: int, right: int, bottom: int, left: int} */
    private function pageMargins(string $pageMargins): array
    {
        if (preg_match('/^([0-9]+),([0-9]+),([0-9]+),([0-9]+)$/D', $pageMargins, $match) !== 1) {
            throw new InvalidArgumentException(
                'API 2 pageMargins must be four comma-separated nonnegative integer CSS pixels',
            );
        }

        return [
            'top' => $this->cssPixelsToAppUnits($match[1], 'top margin', allowZero: true),
            'right' => $this->cssPixelsToAppUnits($match[2], 'right margin', allowZero: true),
            'bottom' => $this->cssPixelsToAppUnits($match[3], 'bottom margin', allowZero: true),
            'left' => $this->cssPixelsToAppUnits($match[4], 'left margin', allowZero: true),
        ];
    }

    private function cssPixelsToAppUnits(string $value, string $label, bool $allowZero = false): int
    {
        $normalized = ltrim($value, '0');
        $normalized = $normalized === '' ? '0' : $normalized;
        if (strlen($normalized) > 10 || (int) $normalized > intdiv(2_147_483_647, 60)) {
            throw new InvalidArgumentException("API 2 {$label} exceeds signed app-unit range");
        }
        $integer = (int) $normalized;
        if (!$allowZero && $integer === 0) {
            throw new InvalidArgumentException("API 2 {$label} must be positive");
        }

        return $integer * 60;
    }

    /**
     * @return array{
     *   exit_code: int,
     *   stdout: string,
     *   stderr: string,
     *   timed_out: bool,
     *   stdout_overflow: bool,
     *   stderr_overflow: bool
     * }
     */
    private function execute(string $runtimeRoot, string $request): array
    {
        $stdin = tmpfile();
        $stdout = tmpfile();
        $stderr = tmpfile();
        if (!is_resource($stdin) || !is_resource($stdout) || !is_resource($stderr)) {
            foreach ([$stdin, $stdout, $stderr] as $stream) {
                if (is_resource($stream)) {
                    fclose($stream);
                }
            }
            throw new TransportException('cannot create API 2 process transport streams');
        }
        $this->writeStream($stdin, $request);
        rewind($stdin);

        $pipes = [];
        $process = @proc_open(
            [...$this->command, 'render-api2'],
            [0 => $stdin, 1 => $stdout, 2 => $stderr],
            $pipes,
            $runtimeRoot,
        );
        fclose($stdin);
        if (!is_resource($process)) {
            fclose($stdout);
            fclose($stderr);
            throw new TransportException('cannot start the Pliego render-api2 process');
        }

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

        [$stdoutBytes, $stdoutOverflow] = $this->readBoundedStream($stdout, self::RESULT_MAX_BYTES);
        [$stderrBytes, $stderrOverflow] = $this->readBoundedStream($stderr, self::STDERR_MAX_BYTES);
        fclose($stdout);
        fclose($stderr);

        return [
            'exit_code' => $exitCode,
            'stdout' => $stdoutBytes,
            'stderr' => $stderrBytes,
            'timed_out' => $timedOut,
            'stdout_overflow' => $stdoutOverflow,
            'stderr_overflow' => $stderrOverflow,
        ];
    }

    /** @return array{string, bool} */
    private function readBoundedStream(mixed $stream, int $maximum): array
    {
        $stats = fstat($stream);
        $size = is_array($stats) && is_int($stats['size'] ?? null) ? $stats['size'] : 0;
        rewind($stream);
        $bytes = stream_get_contents($stream, $maximum + 1);
        if (!is_string($bytes)) {
            throw new TransportException('cannot read API 2 process transport stream');
        }

        return [substr($bytes, 0, $maximum), $size > $maximum || strlen($bytes) > $maximum];
    }

    private function writeStream(mixed $stream, string $bytes): void
    {
        $remaining = $bytes;
        while ($remaining !== '') {
            $written = fwrite($stream, $remaining);
            if (!is_int($written) || $written < 1) {
                throw new TransportException('cannot write the API 2 request to stdin');
            }
            $remaining = substr($remaining, $written);
        }
        if (!fflush($stream)) {
            throw new TransportException('cannot flush the API 2 request transport');
        }
    }

    private function allocateOuterJob(): string
    {
        for ($attempt = 0; $attempt < 10; $attempt++) {
            $job = $this->workDirectory.DIRECTORY_SEPARATOR.bin2hex(random_bytes(16));
            if (@mkdir($job, 0700)) {
                $resolved = realpath($job);
                if (is_string($resolved)) {
                    return $resolved;
                }
            }
        }

        throw new RuntimeException("cannot create an exclusive Pliego job in {$this->workDirectory}");
    }

    private function prepareWorkRoot(string $workDirectory): string
    {
        if (
            $workDirectory === ''
            || str_contains($workDirectory, "\0")
            || !$this->isAbsolutePath($workDirectory)
            || is_link($workDirectory)
            || is_link(rtrim($workDirectory, '/\\'))
        ) {
            throw new InvalidArgumentException(
                'workDirectory must be an absolute, non-symlink directory outside the filesystem root',
            );
        }
        if (!is_dir($workDirectory) && !@mkdir($workDirectory, 0700, true)) {
            throw new RuntimeException("cannot create Pliego work directory {$workDirectory}");
        }
        $resolved = realpath($workDirectory);
        if (
            $resolved === false
            || !is_dir($resolved)
            || $resolved === DIRECTORY_SEPARATOR
            || preg_match('/^[A-Za-z]:[\\\\\/]?$/', $resolved) === 1
            || $this->samePath(dirname($resolved), $resolved)
            || !is_writable($resolved)
        ) {
            throw new InvalidArgumentException('workDirectory resolves to an unsafe or unwritable filesystem root');
        }

        return $resolved;
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
     * @param array<string, int> $phases
     * @return array<string, mixed>
     */
    private function finishBridgeTimings(array $context, int $rendererStartedAt, array $phases): array
    {
        $total = hrtime(true) - $context['total_started_ns'];
        $outside = $rendererStartedAt - $context['total_started_ns'];
        $attributed = array_sum($phases) + $outside;

        return [
            'schema' => 'pliego.php-bridge-timings',
            'version' => 2,
            'measurement_boundary' => 'api2-render-invocation-before-timing-diagnostic',
            'total_ms' => $this->milliseconds($total),
            'setup_ms' => [
                'runtime_resolution' => $this->milliseconds($this->runtimeResolutionNanoseconds),
                'runtime_install' => null,
            ],
            'phases_ms' => [
                'laravel_setup' => $this->milliseconds($context['laravel_setup_ns']),
                'view_render' => $this->milliseconds($context['view_render_ns']),
                'contract_probe' => $this->milliseconds($phases['contract_probe']),
                'input_staging' => $this->milliseconds($phases['input_staging']),
                'process' => $this->milliseconds($phases['process']),
                'validation' => $this->milliseconds($phases['validation']),
                'cleanup' => $this->milliseconds($phases['cleanup']),
                'unattributed' => $this->milliseconds(max(0, $total - $attributed)),
            ],
            'diagnostics' => ['retained' => true],
        ];
    }

    /** @param array<string, mixed> $timings @return array<string, mixed> */
    private function persistBridgeTimings(string $jobPath, array $timings): array
    {
        try {
            $json = json_encode(
                $timings,
                JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_PRESERVE_ZERO_FRACTION,
            )."\n";
        } catch (JsonException) {
            $timings['diagnostics']['retained'] = false;

            return $timings;
        }
        $written = file_put_contents(
            $jobPath.DIRECTORY_SEPARATOR.self::BRIDGE_TIMINGS_FILE,
            $json,
            LOCK_EX,
        );
        if ($written !== strlen($json)) {
            $timings['diagnostics']['retained'] = false;
        }

        return $timings;
    }

    private function markFailureIfRunning(string $jobPath): void
    {
        $status = @file_get_contents($jobPath.DIRECTORY_SEPARATOR.JobRetention::STATUS_FILE);
        if ($status === "running\n") {
            try {
                JobRetention::mark($jobPath, 'failure');
            } catch (Throwable) {
                // Preserve the original API 2 failure; the retained running marker remains evidence.
            }
        }
    }

    private function milliseconds(?int $nanoseconds): ?float
    {
        return $nanoseconds === null ? null : round($nanoseconds / 1_000_000, 3);
    }

    private function isAbsolutePath(string $path): bool
    {
        return str_starts_with($path, '/')
            || str_starts_with($path, '\\\\')
            || preg_match('/^[A-Za-z]:[\\\\\/]/', $path) === 1;
    }

    private function samePath(string $left, string $right): bool
    {
        return DIRECTORY_SEPARATOR === '\\' ? strcasecmp($left, $right) === 0 : $left === $right;
    }
}
