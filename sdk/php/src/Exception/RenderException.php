<?php

declare(strict_types=1);

namespace Pliego\Php\Experimental\Exception;

use RuntimeException;

class RenderException extends RuntimeException
{
    public function __construct(
        public readonly string $errorCode,
        public readonly int $exitCode,
        public readonly string $stderr,
        string $message,
    ) {
        parent::__construct($message);
    }
}
