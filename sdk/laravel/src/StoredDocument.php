<?php

declare(strict_types=1);

namespace Pliego\Laravel;

use Pliego\Php\RenderResult;

final readonly class StoredDocument
{
    public function __construct(
        public string $disk,
        public string $path,
        public RenderResult $renderResult,
    ) {}
}
