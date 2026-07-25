<?php

declare(strict_types=1);

namespace Pliego\Php\Experimental;

use InvalidArgumentException;

/**
 * Experimental one-shot CLI options. This is not the future daemon protocol.
 */
final readonly class RenderOptions
{
    /**
     * @param list<string> $allowedHttpRoots An empty list explicitly denies network access.
     */
    public function __construct(
        public string $locale = 'en-US',
        public string $timezone = 'UTC',
        public string $pageSize = '612x792',
        public string $pageMargins = '36,36,36,36',
        public array $allowedHttpRoots = [],
    ) {
        foreach ([
            'locale' => $this->locale,
            'timezone' => $this->timezone,
            'pageSize' => $this->pageSize,
            'pageMargins' => $this->pageMargins,
        ] as $name => $value) {
            if ($value === '' || str_contains($value, "\0")) {
                throw new InvalidArgumentException("{$name} must be a non-empty string");
            }
        }

        foreach ($this->allowedHttpRoots as $root) {
            if (!is_string($root)) {
                throw new InvalidArgumentException(
                    'allowed HTTP roots must be absolute http(s) URLs',
                );
            }
            $scheme = parse_url($root, PHP_URL_SCHEME);
            if (!in_array($scheme, ['http', 'https'], true)) {
                throw new InvalidArgumentException(
                    'allowed HTTP roots must be absolute http(s) URLs',
                );
            }
        }
    }
}
