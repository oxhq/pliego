<?php

declare(strict_types=1);

namespace Pliego\Php;

use InvalidArgumentException;

/** Options for one deterministic, one-shot document render. */
final readonly class RenderOptions
{
    public string $locale;
    public string $timezone;
    public string $pageSize;
    public string $pageMargins;

    /** @var list<string> */
    public array $allowedHttpRoots;

    public int $epochUnixMilliseconds;
    public int $virtualSpanMilliseconds;
    public int $ordinaryTasks;
    public int $microtasks;
    public int $renderingOpportunities;
    public int $mutations;
    public int $hostWallMilliseconds;
    public string $diagnosticsRetention;

    /**
     * @param list<string> $allowedHttpRoots API 1 compatibility only. API 2 always denies live network access.
     */
    public function __construct(
        string $locale = 'en-US',
        string $timezone = 'UTC',
        string $pageSize = '816x1056',
        string $pageMargins = '48,48,48,48',
        array $allowedHttpRoots = [],
        int $epochUnixMilliseconds = 946_684_800_000,
        int $virtualSpanMilliseconds = 86_400_000,
        int $ordinaryTasks = 100_000,
        int $microtasks = 1_000_000,
        int $renderingOpportunities = 10_000,
        int $mutations = 1_000_000,
        int $hostWallMilliseconds = 60_000,
        string $diagnosticsRetention = 'always',
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

        if (
            $epochUnixMilliseconds < -8_640_000_000_000_000
            || $epochUnixMilliseconds > 8_640_000_000_000_000
        ) {
            throw new InvalidArgumentException('epochUnixMilliseconds is outside the API 2 range');
        }
        if ($virtualSpanMilliseconds < 1 || $virtualSpanMilliseconds > 9_007_199_254_740_991) {
            throw new InvalidArgumentException('virtualSpanMilliseconds is outside the API 2 range');
        }
        foreach ([
            'ordinaryTasks' => $ordinaryTasks,
            'microtasks' => $microtasks,
            'renderingOpportunities' => $renderingOpportunities,
            'mutations' => $mutations,
            'hostWallMilliseconds' => $hostWallMilliseconds,
        ] as $name => $value) {
            if ($value < 1 || $value > 4_294_967_295) {
                throw new InvalidArgumentException("{$name} is outside the API 2 range");
            }
        }
        if (!in_array($diagnosticsRetention, ['none', 'on-failure', 'always'], true)) {
            throw new InvalidArgumentException(
                'diagnosticsRetention must be none, on-failure, or always',
            );
        }

        $this->locale = $locale;
        $this->timezone = $timezone;
        $this->pageSize = $pageSize;
        $this->pageMargins = $pageMargins;
        $this->allowedHttpRoots = array_values(array_unique($normalizedRoots));
        $this->epochUnixMilliseconds = $epochUnixMilliseconds;
        $this->virtualSpanMilliseconds = $virtualSpanMilliseconds;
        $this->ordinaryTasks = $ordinaryTasks;
        $this->microtasks = $microtasks;
        $this->renderingOpportunities = $renderingOpportunities;
        $this->mutations = $mutations;
        $this->hostWallMilliseconds = $hostWallMilliseconds;
        $this->diagnosticsRetention = $diagnosticsRetention;
    }
}
