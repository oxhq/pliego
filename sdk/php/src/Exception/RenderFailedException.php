<?php

declare(strict_types=1);

namespace Pliego\Php\Exception;

use RuntimeException;

/** A normalized API 2 request was accepted and produced a validated failed result. */
final class RenderFailedException extends RuntimeException
{
    public readonly int $exitCode;

    /** @param array<string, mixed> $result */
    public function __construct(
        public readonly string $kind,
        public readonly array $result,
        public readonly string $jobPath,
        public readonly string $runtimeJobPath,
        public readonly string $diagnosticsPath,
    ) {
        $this->exitCode = 1;
        parent::__construct("Pliego render failed: {$kind}; evidence retained at {$jobPath}");
    }
}
