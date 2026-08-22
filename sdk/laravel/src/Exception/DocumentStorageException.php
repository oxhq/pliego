<?php

declare(strict_types=1);

namespace Pliego\Laravel\Exception;

use Pliego\Php\RenderResult;
use RuntimeException;
use Throwable;

final class DocumentStorageException extends RuntimeException
{
    public function __construct(
        public readonly string $disk,
        public readonly string $path,
        public readonly RenderResult $renderResult,
        Throwable $previous,
    ) {
        parent::__construct(
            "Cannot store rendered PDF at {$disk}:{$path}: {$previous->getMessage()}",
            0,
            $previous,
        );
    }
}
