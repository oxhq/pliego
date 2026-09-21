<?php

declare(strict_types=1);

namespace Pliego\Php;

use RuntimeException;

final readonly class RenderResult
{
    public string $jobPath;
    public string $runtimeJobPath;
    public string $inputPath;
    public string $deliveryPath;
    public string $diagnosticsPath;
    public string $scenePath;
    public string $bundlePath;
    public ?string $deliveryIdentity;

    /**
     * @param array<string, mixed> $metadata
     * @param array<string, mixed> $bridgeTimings
     */
    public function __construct(
        public string $pdfPath,
        public string $artifactsPath,
        public string $inputBundlePath,
        public array $metadata,
        public array $bridgeTimings = [],
        ?string $jobPath = null,
        ?string $runtimeJobPath = null,
        ?string $inputPath = null,
        ?string $deliveryPath = null,
        ?string $diagnosticsPath = null,
        ?string $scenePath = null,
        ?string $bundlePath = null,
    ) {
        $this->jobPath = $jobPath ?? dirname($inputBundlePath);
        $this->runtimeJobPath = $runtimeJobPath ?? $this->jobPath;
        $this->inputPath = $inputPath ?? $inputBundlePath;
        $this->deliveryPath = $deliveryPath ?? dirname($pdfPath);
        $this->diagnosticsPath = $diagnosticsPath ?? $artifactsPath;
        $this->scenePath = $scenePath ?? $this->deliveryPath.DIRECTORY_SEPARATOR.'scene.json';
        $this->bundlePath = $bundlePath ?? $this->deliveryPath.DIRECTORY_SEPARATOR.'bundle.json';
        $identity = $metadata['delivery']['bundle']['sha256'] ?? null;
        $this->deliveryIdentity = is_string($identity) ? $identity : null;
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
