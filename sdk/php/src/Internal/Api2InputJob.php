<?php

declare(strict_types=1);

namespace Pliego\Php\Internal;

use InvalidArgumentException;
use Pliego\Php\InputAsset;
use RuntimeException;

/** @internal */
final readonly class Api2InputJob
{
    private const MAX_ENTRIES = 16_384;
    private const MAX_NODES = 16_384;
    private const MAX_MANIFEST_BYTES = 16_777_216;
    private const MAX_CONTENT_BYTES = 67_108_864;

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
        if (!@mkdir($runtimeRoot, 0700) || !@mkdir($inputPath, 0700)) {
            throw new RuntimeException("cannot create exclusive API 2 job root {$runtimeRoot}");
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
