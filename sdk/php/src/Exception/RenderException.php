<?php

declare(strict_types=1);

namespace Pliego\Php\Exception;

use RuntimeException;

class RenderException extends RuntimeException
{
    public readonly string $jobPath;

    /** @param array<string, mixed> $bridgeTimings */
    public function __construct(
        public readonly string $errorCode,
        public readonly int $exitCode,
        public readonly string $stderr,
        string $message,
        public readonly string $inputBundlePath,
        public readonly string $artifactsPath,
        public readonly array $bridgeTimings = [],
    ) {
        $this->jobPath = dirname($inputBundlePath);
        parent::__construct($message);
    }
}
