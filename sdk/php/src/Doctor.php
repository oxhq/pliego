<?php

declare(strict_types=1);

namespace Pliego\Php\Experimental;

use RuntimeException;
use Throwable;

final readonly class Doctor
{
    private const SUPPORTED_API_VERSION = '1';

    /**
     * @param non-empty-list<string> $command
     */
    public function __construct(private array $command, private int $timeoutSeconds = 60)
    {
        if ($command === [] || $timeoutSeconds < 1) {
            throw new RuntimeException('doctor command and timeout must be configured');
        }
    }

    /**
     * @return array{binary: string, version: string, api_version: int, platform: string, work_root: string, smoke_pdf: string}
     */
    public function run(string $workDirectory): array
    {
        $command = $this->resolvedCommand();
        [$exitCode, $stdout, $stderr] = $this->execute([...$command, '--version']);
        if ($exitCode !== 0) {
            $detail = trim($stderr) ?: "exit code {$exitCode}";
            throw new RuntimeException(
                "Pliego cannot run on ".PHP_OS_FAMILY.": {$detail}. Install Pliego or set PLIEGO_BINARY to a compatible executable.",
            );
        }
        if (preg_match('/^pliego ([^\s]+)\r?$/m', $stdout, $match) !== 1) {
            throw new RuntimeException(
                'PLIEGO_BINARY does not expose the expected `pliego <version>` output; install a compatible Pliego CLI.',
            );
        }
        if (preg_match('/^pliego-api ([0-9]+)\r?$/m', $stdout, $apiMatch) !== 1) {
            throw new RuntimeException(
                'PLIEGO_BINARY does not expose `pliego-api <version>`; this SDK requires Pliego API v1.',
            );
        }
        if ($apiMatch[1] !== self::SUPPORTED_API_VERSION) {
            throw new RuntimeException(
                "PLIEGO_BINARY exposes unsupported Pliego API v{$apiMatch[1]}; this SDK requires v1.",
            );
        }

        $root = $this->prepareWorkRoot($workDirectory);
        $job = $root.DIRECTORY_SEPARATOR.bin2hex(random_bytes(16));
        if (!@mkdir($job, 0700)) {
            throw new RuntimeException(
                "Pliego work root is not writable: {$root}. Grant PHP write access or set PLIEGO_WORK_DIR.",
            );
        }
        $style = $job.DIRECTORY_SEPARATOR.'doctor.css';
        if (file_put_contents($style, "body { font: 12px/1.4 serif; }\n", LOCK_EX) === false) {
            throw new RuntimeException("cannot write the offline doctor asset in {$job}");
        }

        try {
            $result = (new CliRenderer($command, $this->timeoutSeconds))->render(
                '<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="assets/doctor.css"><p>Pliego doctor</p>',
                $job.DIRECTORY_SEPARATOR.'input',
                $job.DIRECTORY_SEPARATOR.'doctor.pdf',
                $job.DIRECTORY_SEPARATOR.'artifacts',
                new RenderOptions(allowedHttpRoots: []),
                ['assets/doctor.css' => $style],
            );
        } catch (Throwable $error) {
            throw new RuntimeException(
                "Pliego offline smoke failed; evidence retained at {$job}: {$error->getMessage()}",
                previous: $error,
            );
        }
        if (!str_starts_with($result->bytes(), '%PDF-')) {
            throw new RuntimeException("Pliego offline smoke returned a non-PDF file at {$result->pdfPath}");
        }

        return [
            'binary' => $command[0],
            'version' => $match[1],
            'api_version' => (int) $apiMatch[1],
            'platform' => PHP_OS_FAMILY.' '.(PHP_INT_SIZE * 8).'-bit',
            'work_root' => $root,
            'smoke_pdf' => $result->pdfPath,
        ];
    }

    /**
     * @return non-empty-list<string>
     */
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

    private function prepareWorkRoot(string $workDirectory): string
    {
        if (
            $workDirectory === ''
            || str_contains($workDirectory, "\0")
            || (!$this->isAbsolutePath($workDirectory))
            || is_link($workDirectory)
            || is_link(rtrim($workDirectory, '/\\'))
        ) {
            throw new RuntimeException(
                'PLIEGO_WORK_DIR must be an absolute, non-symlink directory outside the filesystem root.',
            );
        }
        if (!is_dir($workDirectory) && !@mkdir($workDirectory, 0700, true)) {
            throw new RuntimeException(
                "Pliego work root cannot be created: {$workDirectory}. Grant PHP write access or set PLIEGO_WORK_DIR.",
            );
        }

        $resolved = realpath($workDirectory);
        if (
            $resolved === false
            || !is_dir($resolved)
            || $resolved === DIRECTORY_SEPARATOR
            || preg_match('/^[A-Za-z]:[\\\\\/]?$/', $resolved) === 1
            || $this->samePath(dirname($resolved), $resolved)
        ) {
            throw new RuntimeException(
                'PLIEGO_WORK_DIR resolves to an unsafe filesystem root; configure a dedicated directory.',
            );
        }
        if (!is_writable($resolved)) {
            throw new RuntimeException(
                "Pliego work root is not writable: {$resolved}. Grant PHP write access or set PLIEGO_WORK_DIR.",
            );
        }

        return $resolved;
    }

    /**
     * @param non-empty-list<string> $arguments
     * @return array{int, string, string}
     */
    private function execute(array $arguments): array
    {
        $stdoutFile = tmpfile();
        $stderrFile = tmpfile();
        if (!is_resource($stdoutFile) || !is_resource($stderrFile)) {
            if (is_resource($stdoutFile)) {
                fclose($stdoutFile);
            }
            if (is_resource($stderrFile)) {
                fclose($stderrFile);
            }
            throw new RuntimeException('cannot create Pliego doctor output streams');
        }
        $pipes = [];
        $process = @proc_open(
            $arguments,
            [0 => ['pipe', 'r'], 1 => $stdoutFile, 2 => $stderrFile],
            $pipes,
        );
        if (!is_resource($process)) {
            fclose($stdoutFile);
            fclose($stderrFile);
            throw new RuntimeException(
                "Pliego process {$arguments[0]} could not start on ".PHP_OS_FAMILY.'; install it or set PLIEGO_BINARY.',
            );
        }
        fclose($pipes[0]);

        $deadline = hrtime(true) + ($this->timeoutSeconds * 1_000_000_000);
        $status = proc_get_status($process);
        while ($status['running']) {
            if (hrtime(true) >= $deadline) {
                proc_terminate($process, 9);
                proc_close($process);
                fclose($stdoutFile);
                fclose($stderrFile);
                throw new RuntimeException("Pliego --version exceeded {$this->timeoutSeconds} seconds");
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

        return [$exitCode, $stdout === false ? '' : $stdout, $stderr === false ? '' : $stderr];
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
