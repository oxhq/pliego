<?php

declare(strict_types=1);

namespace Pliego\Php\Exception;

use RuntimeException;
use Throwable;

/** The API 2 process or framing contract failed before a trustworthy result was available. */
final class TransportException extends RuntimeException
{
    public function __construct(
        string $message,
        public readonly ?string $jobPath = null,
        public readonly ?string $runtimeJobPath = null,
        public readonly ?int $exitCode = null,
        public readonly string $stdout = '',
        public readonly string $stderr = '',
        ?Throwable $previous = null,
    ) {
        parent::__construct($message, previous: $previous);
    }
}
