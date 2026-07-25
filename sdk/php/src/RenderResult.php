<?php

declare(strict_types=1);

namespace Pliego\Php\Experimental;

use RuntimeException;

final readonly class RenderResult
{
    /**
     * @param array<string, mixed> $metadata
     */
    public function __construct(
        public string $pdfPath,
        public string $artifactsPath,
        public string $inputBundlePath,
        public array $metadata,
    ) {}

    public function bytes(): string
    {
        $bytes = file_get_contents($this->pdfPath);
        if ($bytes === false) {
            throw new RuntimeException("cannot read rendered PDF {$this->pdfPath}");
        }

        return $bytes;
    }
}
