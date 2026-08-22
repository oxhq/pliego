<?php

declare(strict_types=1);

namespace Pliego\Php\Internal;

use JsonException;
use UnexpectedValueException;

/** @internal */
final class CanonicalJson
{
    private const ENCODE_FLAGS = JSON_UNESCAPED_SLASHES
        | JSON_UNESCAPED_UNICODE
        | JSON_UNESCAPED_LINE_TERMINATORS
        | JSON_THROW_ON_ERROR;

    /** @param array<string, mixed> $value */
    public static function encodeFrame(array $value, string $label): string
    {
        try {
            return json_encode($value, self::ENCODE_FLAGS)."\n";
        } catch (JsonException $error) {
            throw new UnexpectedValueException("cannot encode canonical {$label} JSON", previous: $error);
        }
    }

    /** @return array<string, mixed> */
    public static function decodeFrame(string $bytes, string $label): array
    {
        if (
            $bytes === ''
            || !str_ends_with($bytes, "\n")
            || str_contains(substr($bytes, 0, -1), "\n")
            || str_contains($bytes, "\r")
        ) {
            throw new UnexpectedValueException(
                "{$label} must contain one compact JSON object followed by one LF",
            );
        }

        $json = substr($bytes, 0, -1);
        try {
            $value = json_decode($json, true, flags: JSON_THROW_ON_ERROR);
        } catch (JsonException $error) {
            throw new UnexpectedValueException("{$label} contains invalid JSON", previous: $error);
        }
        if (!is_array($value) || array_is_list($value)) {
            throw new UnexpectedValueException("{$label} must contain a JSON object");
        }

        try {
            $canonical = json_encode($value, self::ENCODE_FLAGS);
        } catch (JsonException $error) {
            throw new UnexpectedValueException("{$label} cannot be canonically encoded", previous: $error);
        }
        if ($canonical !== $json) {
            throw new UnexpectedValueException(
                "{$label} must use exact typed member order and canonical compact JSON",
            );
        }

        return $value;
    }
}
