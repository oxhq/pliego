<?php

declare(strict_types=1);

$releaseManifest = getenv('PLIEGO_RELEASE_RUNTIME_MANIFEST');
if (! is_string($releaseManifest) || $releaseManifest === '') {
    throw new RuntimeException('PLIEGO_RELEASE_RUNTIME_MANIFEST must name the promoted runtimes.json');
}

$bundledManifest = dirname(__DIR__).'/resources/runtimes.json';
$releaseBytes = file_get_contents($releaseManifest);
$bundledBytes = file_get_contents($bundledManifest);
if (! is_string($releaseBytes) || ! is_string($bundledBytes) || $releaseBytes !== $bundledBytes) {
    throw new RuntimeException('Bundled runtimes.json is not byte-identical to the promoted release asset');
}

$manifest = json_decode($bundledBytes, true, flags: JSON_THROW_ON_ERROR);
$composer = json_decode(
    (string) file_get_contents(dirname(__DIR__).'/composer.json'),
    true,
    flags: JSON_THROW_ON_ERROR,
);
$bridgeVersion = $composer['require']['oxhq/pliego-php'] ?? null;
if (
    ! is_array($manifest)
    || ($manifest['release_ready'] ?? null) !== true
    || ($manifest['api'] ?? null) !== 2
    || ! is_string($bridgeVersion)
    || ($manifest['version'] ?? null) !== $bridgeVersion
    || trim((string) file_get_contents(dirname(__DIR__).'/VERSION')) !== $bridgeVersion
) {
    throw new RuntimeException('Bundled runtimes.json is not finalized for the pinned Pliego PHP/native version');
}

echo "release runtime manifest: ok\n";
