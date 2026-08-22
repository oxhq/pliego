<?php

declare(strict_types=1);

namespace Pliego\Php;

use RuntimeException;
use Throwable;

/** Verifies the installed API 2 tuple with a real offline transaction. */
final readonly class Doctor
{
    /** @param non-empty-list<string> $command */
    public function __construct(private array $command, private int $timeoutSeconds = 60)
    {
        if ($command === [] || $timeoutSeconds < 1) {
            throw new RuntimeException('doctor command and timeout must be configured');
        }
    }

    /**
     * @return array{
     *   binary: string,
     *   version: string,
     *   api_version: int,
     *   platform: string,
     *   work_root: string,
     *   smoke_pdf: string,
     *   smoke_scene: string,
     *   smoke_bundle: string,
     *   delivery_identity: string
     * }
     */
    public function run(string $workDirectory): array
    {
        $command = $this->resolvedCommand();
        try {
            $engine = new DocumentEngine(
                $command,
                $workDirectory,
                timeoutSeconds: $this->timeoutSeconds,
                probeTimeoutSeconds: max(180, $this->timeoutSeconds),
            );
            $contract = $engine->contract();
        } catch (Throwable $error) {
            throw new RuntimeException(
                'PLIEGO_BINARY does not expose the compatible API 2 contract: '.$error->getMessage(),
                previous: $error,
            );
        }

        $font = dirname(__DIR__).'/resources/HasubiMono-Regular.woff2';
        if (!is_file($font)) {
            throw new RuntimeException('Pliego doctor bundled font is missing; reinstall oxhq/pliego-php.');
        }
        $sourceDirectory = rtrim($workDirectory, '/\\').DIRECTORY_SEPARATOR.'.doctor-'.bin2hex(random_bytes(8));
        if (!@mkdir($sourceDirectory, 0700)) {
            throw new RuntimeException("cannot create offline doctor staging directory in {$workDirectory}");
        }
        $style = $sourceDirectory.DIRECTORY_SEPARATOR.'doctor.css';
        if (file_put_contents(
            $style,
            "@font-face { font-family: \"Pliego Doctor\"; src: url(\"doctor.woff2\") format(\"woff2\"); }\n"
                ."body { font: 12px/1.4 \"Pliego Doctor\"; }\n",
            LOCK_EX,
        ) === false) {
            @rmdir($sourceDirectory);
            throw new RuntimeException("cannot write the offline doctor asset in {$sourceDirectory}");
        }

        try {
            $result = $engine->render(
                '<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="assets/doctor.css"><p>Pliego doctor</p>',
                new RenderOptions(
                    allowedHttpRoots: [],
                    hostWallMilliseconds: max(
                        1,
                        min(60_000, ($this->timeoutSeconds * 1_000) - 1_000),
                    ),
                ),
                [
                    new InputAsset('assets/doctor.css', $style, 'text/css;charset=utf-8'),
                    new InputAsset('assets/doctor.woff2', $font, 'font/woff2'),
                ],
            );
        } catch (Throwable $error) {
            throw new RuntimeException(
                "Pliego API 2 offline smoke failed: {$error->getMessage()}",
                previous: $error,
            );
        } finally {
            @unlink($style);
            @rmdir($sourceDirectory);
        }

        if (!str_starts_with($result->bytes(), '%PDF-')) {
            throw new RuntimeException("Pliego API 2 offline smoke returned a non-PDF file at {$result->pdfPath}");
        }
        $scene = $result->metadata['delivery']['scene'] ?? null;
        $bundle = $result->metadata['delivery']['bundle'] ?? null;
        if (
            !is_array($scene)
            || !is_array($bundle)
            || $result->deliveryIdentity === null
            || $result->deliveryIdentity !== ($bundle['sha256'] ?? null)
            || !is_file($result->scenePath)
            || !is_file($result->bundlePath)
        ) {
            throw new RuntimeException("Pliego API 2 offline smoke evidence is incomplete at {$result->jobPath}");
        }

        $identity = $contract->engine();

        return [
            'binary' => $command[0],
            'version' => $identity['version'],
            'api_version' => $identity['api'],
            'platform' => PHP_OS_FAMILY.' '.(PHP_INT_SIZE * 8).'-bit',
            'work_root' => dirname($result->jobPath),
            'smoke_pdf' => $result->pdfPath,
            'smoke_scene' => $result->scenePath,
            'smoke_bundle' => $result->bundlePath,
            'delivery_identity' => $result->deliveryIdentity,
        ];
    }

    /** @return non-empty-list<string> */
    private function resolvedCommand(): array
    {
        foreach ($this->command as $part) {
            if (!is_string($part) || $part === '' || str_contains($part, "\0")) {
                throw new RuntimeException('PLIEGO_BINARY command contains an invalid argument');
            }
        }

        $candidate = $this->command[0];
        $configuredPath = str_contains($candidate, '/') || str_contains($candidate, '\\');
        if ($configuredPath && !is_file($candidate)) {
            throw new RuntimeException(
                "Pliego binary not found: {$candidate}. Install Pliego and set PLIEGO_BINARY to its executable path.",
            );
        }
        if ($configuredPath && !is_executable($candidate)) {
            throw new RuntimeException(
                "Pliego binary is not executable: {$candidate}. Fix its execute permission or PLIEGO_BINARY.",
            );
        }
        if ($configuredPath) {
            $candidate = realpath($candidate);
            if ($candidate === false) {
                throw new RuntimeException("cannot resolve Pliego binary {$this->command[0]}");
            }
        }

        return [$candidate, ...array_slice($this->command, 1)];
    }
}
