<?php

declare(strict_types=1);

namespace Pliego\Php;

use RuntimeException;

final readonly class RenderResult
{
    public string $jobPath;

    /**
     * @param array<string, mixed> $metadata
     */
    public function __construct(
        public string $pdfPath,
        public string $artifactsPath,
        public string $inputBundlePath,
        public array $metadata,
    ) {
        $this->jobPath = dirname($inputBundlePath);
    }

    public function bytes(): string
    {
        $bytes = file_get_contents($this->pdfPath);
        if ($bytes === false) {
            throw new RuntimeException("cannot read rendered PDF {$this->pdfPath}");
        }

        return $bytes;
    }
}
