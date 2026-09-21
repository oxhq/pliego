<?php

declare(strict_types=1);

namespace Pliego\Php;

use InvalidArgumentException;

/** One host file to stage into the canonical API 2 input closure. */
final readonly class InputAsset
{
    public function __construct(
        public string $path,
        public string $sourcePath,
        public ?string $mediaType = null,
    ) {
        if ($path === '' || $sourcePath === '' || str_contains($path.$sourcePath, "\0")) {
            throw new InvalidArgumentException('input asset path and sourcePath must be non-empty strings');
        }
        if ($mediaType !== null && ($mediaType === '' || str_contains($mediaType, "\0"))) {
            throw new InvalidArgumentException('input asset mediaType must be null or a non-empty string');
        }
    }
}
