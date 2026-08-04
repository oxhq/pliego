<?php

declare(strict_types=1);

namespace Pliego\Php;

use InvalidArgumentException;

/**
 * Options for one-shot CLI rendering, not a persistent daemon protocol.
 */
final readonly class RenderOptions
{
    public string $locale;
    public string $timezone;
    public string $pageSize;
    public string $pageMargins;

    /** @var list<string> */
    public array $allowedHttpRoots;

    /**
     * @param list<string> $allowedHttpRoots An empty list explicitly denies network access.
     */
    public function __construct(
        string $locale = 'en-US',
        string $timezone = 'UTC',
        string $pageSize = '816x1056',
        string $pageMargins = '48,48,48,48',
        array $allowedHttpRoots = [],
    ) {
        foreach ([
            'locale' => $locale,
            'timezone' => $timezone,
            'pageSize' => $pageSize,
            'pageMargins' => $pageMargins,
        ] as $name => $value) {
            if ($value === '' || str_contains($value, "\0")) {
                throw new InvalidArgumentException("{$name} must be a non-empty string");
            }
        }

        $normalizedRoots = [];
        foreach ($allowedHttpRoots as $root) {
            $parts = is_string($root) ? parse_url($root) : false;
            if (
                $parts === false
                || !isset($parts['scheme'], $parts['host'])
                || !in_array(strtolower($parts['scheme']), ['http', 'https'], true)
                || $parts['host'] === ''
                || isset($parts['user'])
                || isset($parts['pass'])
                || isset($parts['query'])
                || isset($parts['fragment'])
                || str_contains($root, "\0")
                || filter_var($root, FILTER_VALIDATE_URL) === false
            ) {
                throw new InvalidArgumentException(
                    'allowed HTTP roots must be absolute http(s) URLs without credentials, query, or fragment',
                );
            }
            $path = $parts['path'] ?? '';
            if (!str_ends_with($path, '/')) {
                $root .= '/';
            }
            $normalizedRoots[] = $root;
        }
        sort($normalizedRoots, SORT_STRING);

        $this->locale = $locale;
        $this->timezone = $timezone;
        $this->pageSize = $pageSize;
        $this->pageMargins = $pageMargins;
        $this->allowedHttpRoots = array_values(array_unique($normalizedRoots));
    }
}
