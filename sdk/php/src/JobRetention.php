<?php

declare(strict_types=1);

namespace Pliego\Php;

use FilesystemIterator;
use InvalidArgumentException;
use RecursiveDirectoryIterator;
use RecursiveIteratorIterator;
use RuntimeException;

final class JobRetention
{
    public const STATUS_FILE = '.pliego-status';

    public static function mark(string $jobPath, string $status): void
    {
        if (!in_array($status, ['running', 'success', 'failure'], true)) {
            throw new InvalidArgumentException('job status must be running, success, or failure');
        }
        if (!is_dir($jobPath) || is_link($jobPath)) {
            throw new RuntimeException("cannot mark unsafe Pliego job {$jobPath}");
        }
        if (file_put_contents($jobPath.DIRECTORY_SEPARATOR.self::STATUS_FILE, "{$status}\n", LOCK_EX) === false) {
            throw new RuntimeException("cannot mark Pliego job {$jobPath}");
        }
    }

    /**
     * @return array{jobs: int, success_jobs: int, failure_jobs: int, bytes: int}
     */
    public function prune(
        string $root,
        int $successRetentionSeconds,
        int $failureRetentionSeconds,
        bool $dryRun = false,
    ): array {
        if ($successRetentionSeconds < 0 || $failureRetentionSeconds < 0) {
            throw new InvalidArgumentException('retention seconds must be zero or greater');
        }
        if ($root === '' || str_contains($root, "\0") || is_link($root) || is_link(rtrim($root, '/\\'))) {
            throw new InvalidArgumentException('Pliego work root must be a real directory, not a symlink');
        }
        $resolvedRoot = realpath($root);
        if (
            $resolvedRoot === false
            || !is_dir($resolvedRoot)
            || $resolvedRoot === DIRECTORY_SEPARATOR
            || preg_match('/^[A-Za-z]:[\\\\\/]?$/', $resolvedRoot) === 1
            || $this->samePath(dirname($resolvedRoot), $resolvedRoot)
        ) {
            throw new InvalidArgumentException('Pliego work root is unresolved or unsafe');
        }

        $successJobs = 0;
        $failureJobs = 0;
        $bytes = 0;
        $now = time();
        foreach (new FilesystemIterator($resolvedRoot, FilesystemIterator::SKIP_DOTS) as $entry) {
            if (
                preg_match('/^[0-9a-f]{32}$/D', $entry->getFilename()) !== 1
                || $entry->isLink()
                || !$entry->isDir()
            ) {
                continue;
            }
            $job = $entry->getRealPath();
            if (
                $job === false
                || $this->samePath($job, $resolvedRoot)
                || !$this->samePath(dirname($job), $resolvedRoot)
            ) {
                continue;
            }

            $statusPath = $job.DIRECTORY_SEPARATOR.self::STATUS_FILE;
            $statusBytes = @filesize($statusPath);
            if (is_link($statusPath) || !is_file($statusPath) || $statusBytes === false || $statusBytes > 16) {
                continue;
            }
            $statusContents = @file_get_contents($statusPath);
            if ($statusContents === false) {
                continue;
            }
            $status = trim($statusContents);
            if (!in_array($status, ['success', 'failure'], true)) {
                continue;
            }
            $modified = @filemtime($statusPath);
            $retention = $status === 'success' ? $successRetentionSeconds : $failureRetentionSeconds;
            if ($modified === false || $modified > $now - $retention) {
                continue;
            }

            $contents = $this->inspect($job);
            if ($contents === null) {
                continue;
            }
            [$jobBytes, $paths] = $contents;
            $bytes += $jobBytes;
            $status === 'success' ? $successJobs++ : $failureJobs++;
            if (!$dryRun) {
                foreach ($paths as [$path, $directory]) {
                    if (!($directory ? @rmdir($path) : @unlink($path))) {
                        throw new RuntimeException("cannot prune Pliego path {$path}");
                    }
                }
                if (!@rmdir($job)) {
                    throw new RuntimeException("cannot prune Pliego job {$job}");
                }
            }
        }

        return [
            'jobs' => $successJobs + $failureJobs,
            'success_jobs' => $successJobs,
            'failure_jobs' => $failureJobs,
            'bytes' => $bytes,
        ];
    }

    /**
     * @return array{int, list<array{string, bool}>}|null
     */
    private function inspect(string $job): ?array
    {
        $bytes = 0;
        $paths = [];
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($job, FilesystemIterator::SKIP_DOTS),
            RecursiveIteratorIterator::CHILD_FIRST,
        );
        foreach ($iterator as $entry) {
            $resolved = $entry->getRealPath();
            if (
                $entry->isLink()
                || $resolved === false
                || !$this->within($resolved, $job)
                || (!$entry->isFile() && !$entry->isDir())
            ) {
                return null;
            }
            $directory = $entry->isDir();
            if (!$directory) {
                $bytes += $entry->getSize();
            }
            $paths[] = [$entry->getPathname(), $directory];
        }

        return [$bytes, $paths];
    }

    private function samePath(string $left, string $right): bool
    {
        return DIRECTORY_SEPARATOR === '\\' ? strcasecmp($left, $right) === 0 : $left === $right;
    }

    private function within(string $path, string $root): bool
    {
        $prefix = rtrim($root, '/\\').DIRECTORY_SEPARATOR;

        return DIRECTORY_SEPARATOR === '\\'
            ? strncasecmp($path, $prefix, strlen($prefix)) === 0
            : str_starts_with($path, $prefix);
    }
}
