<?php

declare(strict_types=1);

namespace Pliego\Php\Exception;

use RuntimeException;
use UnexpectedValueException;

/**
 * An API 2 request was rejected before a normalized render request was accepted.
 */
final class InvocationException extends RuntimeException
{
    private function __construct(
        public readonly int $exitCode,
        public readonly string $stdout,
        public readonly string $stderr,
        public readonly ?string $jobPath,
        public readonly ?string $runtimeJobPath,
        string $message,
    ) {
        parent::__construct($message);
    }

    public static function fromProcessResult(
        int $exitCode,
        string $stdout,
        string $stderr,
        ?string $jobPath = null,
        ?string $runtimeJobPath = null,
    ): self
    {
        $diagnostic = str_ends_with($stderr, "\n") ? substr($stderr, 0, -1) : '';
        if (
            $exitCode !== 64
            || $stdout !== ''
            || $diagnostic === ''
            || preg_match('/[\x00-\x1F\x7F]/', $diagnostic) === 1
            || preg_match('//u', $diagnostic) !== 1
        ) {
            throw new UnexpectedValueException(
                'API 2 invocation errors require exit 64, empty stdout, and one newline-terminated UTF-8 stderr line'
                    .' without control characters',
            );
        }

        return new self(
            $exitCode,
            $stdout,
            $stderr,
            $jobPath,
            $runtimeJobPath,
            $diagnostic,
        );
    }
}
