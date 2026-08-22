<?php

declare(strict_types=1);

namespace Pliego\Php\Internal;

use InvalidArgumentException;
use Pliego\Php\InputAsset;
use RuntimeException;
use Throwable;

/** @internal */
final readonly class Api2InputJob
{
    private const MAX_ENTRIES = 16_384;
    private const MAX_NODES = 16_384;
    private const MAX_MANIFEST_BYTES = 16_777_216;
    private const MAX_CONTENT_BYTES = 67_108_864;
    private const WINDOWS_TOOL_TIMEOUT_NANOSECONDS = 15_000_000_000;
    private const WINDOWS_TOOL_OUTPUT_MAX_BYTES = 65_536;
    private const WINDOWS_DEFAULT_DACL_TRUSTEES = [
        'S-1-5-18',
        'S-1-5-32-544',
        'S-1-5-11',
        'S-1-5-32-545',
        'S-1-1-0',
        'S-1-3-0',
        'S-1-3-1',
    ];

    /**
     * @param array<string, mixed> $manifestDescriptor
     * @param list<array{path: string, media_type: string, sha256: string, bytes: int}> $entries
     */
    private function __construct(
        public string $runtimeRoot,
        public string $inputPath,
        public string $manifestPath,
        public array $manifestDescriptor,
        public array $entries,
    ) {}

    /**
     * @param array<array-key, string|InputAsset> $assets
     */
    public static function stage(string $outerJobPath, string $html, array $assets): self
    {
        if (preg_match('//u', $html) !== 1) {
            throw new InvalidArgumentException('HTML must be valid UTF-8 for text/html;charset=utf-8');
        }

        $runtimeRoot = $outerJobPath.DIRECTORY_SEPARATOR.'runtime';
        $inputPath = $runtimeRoot.DIRECTORY_SEPARATOR.'input';
        self::createPrivateRuntimeRoot($runtimeRoot);
        if (!@mkdir($inputPath, 0700)) {
            @rmdir($runtimeRoot);
            throw new RuntimeException("cannot create API 2 input directory {$inputPath}");
        }

        $specifications = [[
            'path' => 'document.html',
            'source' => null,
            'media_type' => 'text/html;charset=utf-8',
        ]];
        foreach ($assets as $path => $asset) {
            if ($asset instanceof InputAsset) {
                if (is_string($path) && $path !== $asset->path) {
                    throw new InvalidArgumentException(
                        'an associative InputAsset key must equal the asset path',
                    );
                }
                $specifications[] = [
                    'path' => $asset->path,
                    'source' => $asset->sourcePath,
                    'media_type' => $asset->mediaType ?? self::inferMediaType($asset->path),
                ];

                continue;
            }
            if (!is_string($path) || !is_string($asset)) {
                throw new InvalidArgumentException(
                    'assets must map portable paths to source files or contain InputAsset values',
                );
            }
            $specifications[] = [
                'path' => $path,
                'source' => $asset,
                'media_type' => self::inferMediaType($path),
            ];
        }

        if (count($specifications) > self::MAX_ENTRIES) {
            throw new InvalidArgumentException('API 2 accepts at most 16384 input entries');
        }
        usort(
            $specifications,
            static fn (array $left, array $right): int => strcmp($left['path'], $right['path']),
        );
        self::validateSpecifications($specifications);

        $entries = [];
        $totalBytes = 0;
        foreach ($specifications as $specification) {
            $relative = $specification['path'];
            $destination = $inputPath.DIRECTORY_SEPARATOR.str_replace('/', DIRECTORY_SEPARATOR, $relative);
            $parent = dirname($destination);
            if (!is_dir($parent) && !@mkdir($parent, 0700, true)) {
                throw new RuntimeException("cannot create API 2 input directory {$parent}");
            }

            if ($specification['source'] === null) {
                self::writeExclusive($destination, $html);
            } else {
                self::copyExclusive($specification['source'], $destination);
            }

            $bytes = filesize($destination);
            $sha256 = hash_file('sha256', $destination);
            if (!is_int($bytes) || !is_string($sha256)) {
                throw new RuntimeException("cannot inspect staged API 2 input {$relative}");
            }
            if ($bytes > self::MAX_CONTENT_BYTES) {
                throw new InvalidArgumentException("API 2 input exceeds 64 MiB: {$relative}");
            }
            $totalBytes += $bytes;
            if ($totalBytes > self::MAX_CONTENT_BYTES) {
                throw new InvalidArgumentException('API 2 input content exceeds 64 MiB in total');
            }

            $entries[] = [
                'path' => $relative,
                'media_type' => $specification['media_type'],
                'sha256' => 'sha256:'.$sha256,
                'bytes' => $bytes,
            ];
        }

        $manifest = [
            'schema' => 'pliego.input-manifest',
            'version' => 1,
            'url_root' => 'pliego-input:///',
            'entries' => $entries,
        ];
        $manifestBytes = CanonicalJson::encodeFrame($manifest, 'input manifest');
        if (strlen($manifestBytes) > self::MAX_MANIFEST_BYTES) {
            throw new InvalidArgumentException('canonical API 2 input manifest exceeds 16 MiB');
        }
        $manifestPath = $runtimeRoot.DIRECTORY_SEPARATOR.'input-manifest.json';
        self::writeExclusive($manifestPath, $manifestBytes);

        return new self(
            $runtimeRoot,
            $inputPath,
            $manifestPath,
            [
                'path' => 'input-manifest.json',
                'media_type' => 'application/vnd.pliego.input-manifest+json',
                'sha256' => 'sha256:'.hash('sha256', $manifestBytes),
                'bytes' => strlen($manifestBytes),
            ],
            $entries,
        );
    }

    private static function createPrivateRuntimeRoot(string $path): void
    {
        if (!@mkdir($path, 0700)) {
            throw new RuntimeException("cannot create exclusive API 2 job root {$path}");
        }

        try {
            if (PHP_OS_FAMILY !== 'Windows') {
                if (!@chmod($path, 0700)) {
                    throw new RuntimeException('cannot set Unix mode 0700');
                }
                $permissions = @fileperms($path);
                if (!is_int($permissions) || ($permissions & 0777) !== 0700) {
                    throw new RuntimeException('Unix job root does not have exact mode 0700');
                }

                return;
            }

            self::hardenWindowsPrivateDirectory($path);
        } catch (Throwable $error) {
            @rmdir($path);
            throw new RuntimeException(
                "cannot create exclusive API 2 job root {$path}: {$error->getMessage()}",
                0,
                $error,
            );
        }
    }

    private static function hardenWindowsPrivateDirectory(string $path): void
    {
        $whoami = self::resolveWindowsSystemTool('whoami.exe');
        $icacls = self::resolveWindowsSystemTool('icacls.exe');
        $sid = self::windowsUserSid(self::executeWindowsTool(
            [$whoami, '/user', '/fo', 'csv', '/nh'],
            'current-user lookup',
        ));

        self::executeWindowsTool(
            [$icacls, $path, '/setowner', "*{$sid}", '/q'],
            'owner assignment',
        );
        self::executeWindowsTool(
            [$icacls, $path, '/inheritance:r', '/q'],
            'DACL inheritance removal',
        );
        self::executeWindowsTool(
            [
                $icacls,
                $path,
                '/remove',
                ...array_map(
                    static fn (string $trustee): string => "*{$trustee}",
                    self::WINDOWS_DEFAULT_DACL_TRUSTEES,
                ),
                '/q',
            ],
            'default DACL removal',
        );
        self::executeWindowsTool(
            [$icacls, $path, '/grant:r', "*{$sid}:(OI)(CI)F", '/q'],
            'owner-only DACL assignment',
        );

        $proofPath = @tempnam(dirname($path), '.pliego-api2-acl-');
        if (!is_string($proofPath)) {
            throw new RuntimeException('cannot allocate Windows DACL proof');
        }
        try {
            self::executeWindowsTool(
                [$icacls, $path, '/save', $proofPath, '/q'],
                'DACL verification',
            );
            $proof = @file_get_contents($proofPath);
            if (!is_string($proof)) {
                throw new RuntimeException('cannot read Windows DACL proof');
            }
            self::validateWindowsOwnerOnlyDacl($proof, $sid);
        } finally {
            @unlink($proofPath);
        }

        $entries = @scandir($path);
        if (!is_array($entries) || array_values(array_diff($entries, ['.', '..'])) !== []) {
            throw new RuntimeException('Windows job root changed before input staging');
        }
    }

    private static function resolveWindowsSystemTool(string $name, ?string $systemRoot = null): string
    {
        if (!in_array($name, ['whoami.exe', 'icacls.exe'], true)) {
            throw new RuntimeException("unsupported Windows ACL tool {$name}");
        }
        $root = $systemRoot ?? getenv('SystemRoot');
        if (
            !is_string($root)
            || str_contains($root, "\0")
            || preg_match('/^[A-Za-z]:[\\\\\/]/D', $root) !== 1
        ) {
            throw new RuntimeException('SystemRoot does not identify an absolute Windows directory');
        }
        $candidate = rtrim($root, "\\/").DIRECTORY_SEPARATOR.'System32'.DIRECTORY_SEPARATOR.$name;
        $resolved = realpath($candidate);
        if (
            !is_string($resolved)
            || !is_file($resolved)
            || strtolower(basename($resolved)) !== strtolower($name)
            || strtolower(pathinfo($resolved, PATHINFO_EXTENSION)) !== 'exe'
        ) {
            throw new RuntimeException("required Windows ACL tool is unavailable: {$name}");
        }

        return $resolved;
    }

    private static function windowsUserSid(string $payload): string
    {
        $count = preg_match_all(
            '/(?<![A-Za-z0-9-])(S-1-(?:[0-9]+-)+[0-9]+)(?![A-Za-z0-9-])/',
            $payload,
            $matches,
        );
        if (!is_int($count)) {
            throw new RuntimeException('cannot parse whoami.exe current-user SID');
        }
        $sids = array_values(array_unique($matches[1] ?? []));
        if (count($sids) !== 1 || !is_string($sids[0])) {
            throw new RuntimeException('whoami.exe did not return exactly one current-user SID');
        }

        return $sids[0];
    }

    private static function validateWindowsOwnerOnlyDacl(string $payload, string $sid): void
    {
        $descriptor = self::windowsAclDescriptor($payload);
        $matched = preg_match(
            '/\AD:P(?P<control>(?:(?:AI|AR))*)\(A;(?P<flags>[A-Z]*);FA;;;'
                .preg_quote($sid, '/').'\)\z/D',
            $descriptor,
            $matches,
        );
        $flags = is_string($matches['flags'] ?? null) ? $matches['flags'] : '';
        $flagPairs = strlen($flags) % 2 === 0 ? str_split($flags, 2) : [];
        sort($flagPairs, SORT_STRING);
        if ($matched !== 1 || $flagPairs !== ['CI', 'OI']) {
            throw new RuntimeException(
                'Windows job root does not have one protected owner-only full-access DACL',
            );
        }
    }

    private static function windowsAclDescriptor(string $payload): string
    {
        $length = strlen($payload);
        if ($length === 0 || $length % 2 !== 0 || $length > self::WINDOWS_TOOL_OUTPUT_MAX_BYTES) {
            throw new RuntimeException('icacls.exe returned an invalid or oversized ACL proof');
        }

        $decoded = '';
        for ($offset = 0; $offset < $length; $offset += 2) {
            $low = ord($payload[$offset]);
            $high = ord($payload[$offset + 1]);
            $decoded .= $high === 0 && ($low === 9 || $low === 10 || $low === 13 || ($low >= 32 && $low <= 126))
                ? chr($low)
                : '?';
        }
        $descriptors = array_values(array_filter(
            array_map('trim', preg_split('/\r\n|\r|\n/', $decoded) ?: []),
            static fn (string $line): bool => str_starts_with($line, 'D:'),
        ));
        if (count($descriptors) !== 1) {
            throw new RuntimeException('icacls.exe ACL proof did not contain exactly one DACL');
        }

        return $descriptors[0];
    }

    /**
     * @param non-empty-list<string> $arguments
     */
    private static function executeWindowsTool(array $arguments, string $operation): string
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
            throw new RuntimeException("Windows API 2 job-root {$operation} cannot allocate transport streams");
        }

        $pipes = [];
        $process = @proc_open(
            $arguments,
            [0 => ['pipe', 'r'], 1 => $stdoutFile, 2 => $stderrFile],
            $pipes,
            null,
            null,
            ['bypass_shell' => true, 'suppress_errors' => true],
        );
        if (!is_resource($process)) {
            fclose($stdoutFile);
            fclose($stderrFile);
            throw new RuntimeException("Windows API 2 job-root {$operation} could not start");
        }
        fclose($pipes[0]);

        $deadline = hrtime(true) + self::WINDOWS_TOOL_TIMEOUT_NANOSECONDS;
        $status = proc_get_status($process);
        while ($status['running']) {
            if (hrtime(true) >= $deadline) {
                proc_terminate($process, 9);
                proc_close($process);
                fclose($stdoutFile);
                fclose($stderrFile);
                throw new RuntimeException("Windows API 2 job-root {$operation} exceeded 15 seconds");
            }
            usleep(10_000);
            $status = proc_get_status($process);
        }
        $exitCode = $status['exitcode'];
        $closedExitCode = proc_close($process);
        if ($exitCode < 0) {
            $exitCode = $closedExitCode;
        }

        $stdout = self::readBoundedWindowsToolStream($stdoutFile, $operation);
        $stderr = self::readBoundedWindowsToolStream($stderrFile, $operation);
        fclose($stdoutFile);
        fclose($stderrFile);
        if ($exitCode !== 0) {
            $diagnostic = $stderr !== '' ? $stderr : $stdout;
            throw new RuntimeException(
                "Windows API 2 job-root {$operation} failed with exit {$exitCode}: "
                    .self::formatWindowsToolDiagnostic($diagnostic),
            );
        }

        return $stdout;
    }

    private static function readBoundedWindowsToolStream(mixed $stream, string $operation): string
    {
        $stats = fstat($stream);
        $size = is_array($stats) && is_int($stats['size'] ?? null) ? $stats['size'] : 0;
        rewind($stream);
        $bytes = stream_get_contents($stream, self::WINDOWS_TOOL_OUTPUT_MAX_BYTES + 1);
        if (
            !is_string($bytes)
            || $size > self::WINDOWS_TOOL_OUTPUT_MAX_BYTES
            || strlen($bytes) > self::WINDOWS_TOOL_OUTPUT_MAX_BYTES
        ) {
            throw new RuntimeException("Windows API 2 job-root {$operation} returned oversized output");
        }

        return $bytes;
    }

    private static function formatWindowsToolDiagnostic(string $bytes): string
    {
        if ($bytes === '') {
            return '<empty output>';
        }
        $preview = substr($bytes, 0, 512);
        $escaped = '';
        for ($index = 0, $length = strlen($preview); $index < $length; $index++) {
            $byte = ord($preview[$index]);
            $escaped .= match ($byte) {
                0x09 => '\\t',
                0x0a => '\\n',
                0x0d => '\\r',
                0x5c => '\\\\',
                default => $byte >= 0x20 && $byte <= 0x7e
                    ? $preview[$index]
                    : sprintf('\\x%02X', $byte),
            };
        }

        return strlen($bytes) > 512 ? "{$escaped} (truncated)" : $escaped;
    }

    /**
     * @param list<array{path: mixed, source: mixed, media_type: mixed}> $specifications
     */
    private static function validateSpecifications(array $specifications): void
    {
        $portablePaths = [];
        $filePaths = [];
        $portableFilePaths = [];
        $nodes = [];
        foreach ($specifications as $specification) {
            $path = $specification['path'];
            $mediaType = $specification['media_type'];
            if (!is_string($path)) {
                throw new InvalidArgumentException('API 2 input paths must be strings');
            }
            self::validatePortablePath($path);
            if (
                !is_string($mediaType)
                || strlen($mediaType) < 3
                || strlen($mediaType) > 255
                || preg_match(
                    '/^[a-z0-9][a-z0-9!#$&^_.+-]*\/[a-z0-9][a-z0-9!#$&^_.+-]*(?:;charset=utf-8)?$/D',
                    $mediaType,
                ) !== 1
            ) {
                throw new InvalidArgumentException("invalid canonical media type for API 2 input {$path}");
            }

            $portable = strtolower($path);
            if (isset($portablePaths[$portable])) {
                throw new InvalidArgumentException("duplicate or case-colliding API 2 input path: {$path}");
            }
            $portablePaths[$portable] = true;
            $filePaths[$path] = true;
            $portableFilePaths[$portable] = true;
        }

        foreach (array_keys($filePaths) as $path) {
            $segments = explode('/', $path);
            $prefix = '';
            foreach ($segments as $index => $segment) {
                $prefix = $prefix === '' ? $segment : $prefix.'/'.$segment;
                if ($index < count($segments) - 1 && isset($portableFilePaths[strtolower($prefix)])) {
                    throw new InvalidArgumentException("API 2 input path is also a directory prefix: {$prefix}");
                }
                $nodes[strtolower($prefix)] = true;
            }
        }
        if (count($nodes) > self::MAX_NODES) {
            throw new InvalidArgumentException('API 2 input files and implied directories exceed 16384 nodes');
        }
    }

    private static function validatePortablePath(string $path): void
    {
        if (
            strlen($path) < 1
            || strlen($path) > 240
            || preg_match('/^[A-Za-z0-9](?:[A-Za-z0-9._\/-]*[A-Za-z0-9_-])?$/D', $path) !== 1
        ) {
            throw new InvalidArgumentException("unsafe API 2 input path: {$path}");
        }
        $segments = explode('/', $path);
        if (count($segments) > 32) {
            throw new InvalidArgumentException("API 2 input path has more than 32 segments: {$path}");
        }
        foreach ($segments as $segment) {
            if (
                $segment === ''
                || $segment === '.'
                || $segment === '..'
                || strlen($segment) > 100
                || $segment !== rtrim($segment, '. ')
                || preg_match(
                    '/^(?:CON|PRN|AUX|NUL|CLOCK\$|COM[1-9]|LPT[1-9])(?:\.|$)/iD',
                    $segment,
                ) === 1
            ) {
                throw new InvalidArgumentException("unsafe API 2 input path segment in {$path}");
            }
        }
    }

    private static function copyExclusive(string $source, string $destination): void
    {
        if ($source === '' || str_contains($source, "\0") || is_link($source) || !is_file($source)) {
            throw new InvalidArgumentException("API 2 asset source is not a regular file: {$source}");
        }
        $input = @fopen($source, 'rb');
        $output = @fopen($destination, 'xb');
        if (!is_resource($input) || !is_resource($output)) {
            if (is_resource($input)) {
                fclose($input);
            }
            if (is_resource($output)) {
                fclose($output);
            }
            throw new RuntimeException("cannot stage API 2 asset {$source}");
        }
        try {
            $copied = stream_copy_to_stream($input, $output, self::MAX_CONTENT_BYTES + 1);
            if (!is_int($copied) || $copied > self::MAX_CONTENT_BYTES || !fflush($output)) {
                throw new InvalidArgumentException("API 2 asset exceeds or cannot satisfy the 64 MiB limit: {$source}");
            }
        } finally {
            fclose($input);
            fclose($output);
        }
    }

    private static function writeExclusive(string $path, string $bytes): void
    {
        $stream = @fopen($path, 'xb');
        if (!is_resource($stream)) {
            throw new RuntimeException("cannot create API 2 input file {$path}");
        }
        try {
            $remaining = $bytes;
            while ($remaining !== '') {
                $written = fwrite($stream, $remaining);
                if (!is_int($written) || $written < 1) {
                    throw new RuntimeException("cannot write API 2 input file {$path}");
                }
                $remaining = substr($remaining, $written);
            }
            if (!fflush($stream)) {
                throw new RuntimeException("cannot flush API 2 input file {$path}");
            }
        } finally {
            fclose($stream);
        }
    }

    private static function inferMediaType(string $path): string
    {
        $extension = strtolower(pathinfo($path, PATHINFO_EXTENSION));

        return match ($extension) {
            'css' => 'text/css;charset=utf-8',
            'html', 'htm' => 'text/html;charset=utf-8',
            'js', 'mjs' => 'text/javascript;charset=utf-8',
            'json' => 'application/json',
            'svg' => 'image/svg+xml',
            'gif' => 'image/gif',
            'jpg', 'jpeg' => 'image/jpeg',
            'png' => 'image/png',
            'webp' => 'image/webp',
            'woff' => 'font/woff',
            'woff2' => 'font/woff2',
            'ttf' => 'font/ttf',
            'otf' => 'font/otf',
            default => 'application/octet-stream',
        };
    }
}
