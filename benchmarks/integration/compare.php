<?php

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

declare(strict_types=1);

// One cold process and one retained result. This is a prepared-HTML comparison,
// not the application's original URL/authentication/queue workflow.

function requiredFile(string $path): string
{
    $resolved = realpath($path);
    if ($resolved === false || !is_file($resolved)) {
        throw new RuntimeException("Required file unavailable: {$path}");
    }
    return $resolved;
}

function fixtureFile(string $root, string $relative): string
{
    if (preg_match('~^(?:assets/)[A-Za-z0-9_./ -]+$~D', $relative) !== 1
        || in_array('..', explode('/', $relative), true)) {
        throw new RuntimeException("Non-portable fixture asset path: {$relative}");
    }
    $resolved = requiredFile($root.DIRECTORY_SEPARATOR.$relative);
    if (!str_starts_with($resolved, $root.DIRECTORY_SEPARATOR)) {
        throw new RuntimeException("Asset escapes fixture: {$relative}");
    }
    return $resolved;
}

function storeAndVerify(string $source, string $destination): array
{
    $read = fopen($source, 'rb');
    if ($read === false) {
        throw new RuntimeException('Cannot open rendered PDF');
    }
    $write = false;
    try {
        $write = fopen($destination, 'xb');
        if ($write === false || stream_copy_to_stream($read, $write) === false || !fflush($write)) {
            throw new RuntimeException('PDF stream storage failed');
        }
    } finally {
        fclose($read);
        if (is_resource($write)) {
            fclose($write);
        }
    }
    $sourceHash = hash_file('sha256', $source);
    $storedHash = hash_file('sha256', $destination);
    if (!is_string($sourceHash) || $sourceHash !== $storedHash || filesize($source) !== filesize($destination)) {
        throw new RuntimeException('Stored PDF readback differs from rendered PDF');
    }
    return ['path' => $destination, 'sha256' => $storedHash, 'bytes' => filesize($destination), 'readbackVerified' => true];
}

$started = hrtime(true);
$phase = 'setup';
$record = ['schema' => 'pliego.application-render.v1', 'status' => 'setup_failure',
    'track' => 'prepared-html-cold-process-local', 'phpVersion' => PHP_VERSION];
try {
    $options = getopt('', ['provider:', 'fixture:', 'output:', 'app-autoload:', 'sdk-autoload:', 'binary:', 'chrome:', 'node:', 'node-modules:']);
    foreach (['provider', 'fixture', 'output'] as $name) {
        if (!isset($options[$name]) || !is_string($options[$name]) || $options[$name] === '') {
            throw new RuntimeException("Missing --{$name}");
        }
    }
    $provider = $record['provider'] = $options['provider'];
    if (!in_array($provider, ['pliego', 'browsershot'], true)) {
        throw new RuntimeException('Provider must be pliego or browsershot');
    }
    $root = realpath($options['fixture']);
    if ($root === false || !is_dir($root)) {
        throw new RuntimeException('Fixture directory unavailable');
    }
    $inputPath = requiredFile($root.DIRECTORY_SEPARATOR.'input.html');
    $fixture = json_decode(file_get_contents(requiredFile($root.DIRECTORY_SEPARATOR.'fixture.json')), true, flags: JSON_THROW_ON_ERROR);
    if (($fixture['schema'] ?? null) !== 'pliego.application-fixture.v1' || !is_string($fixture['id'] ?? null)
        || !is_array($fixture['assets'] ?? null)) {
        throw new RuntimeException('Invalid application fixture metadata');
    }
    $record['fixture'] = $fixture['id'];
    $record['inputSha256'] = hash_file('sha256', $inputPath);
    $record['page'] = ['size' => 'A4', 'marginsMm' => [0, 0, 0, 0], 'backgrounds' => true];
    $assetFiles = [];
    foreach ($fixture['assets'] as $asset) {
        if (!is_string($asset['path'] ?? null) || !is_string($asset['mediaType'] ?? null)) {
            throw new RuntimeException('Invalid fixture asset entry');
        }
        $assetFiles[] = [$asset, fixtureFile($root, $asset['path'])];
    }
    $output = $options['output'];
    if (!is_dir($output) && !mkdir($output, 0700, true)) {
        throw new RuntimeException('Cannot create output directory');
    }
    $output = realpath($output);
    if ($output === $root || str_starts_with($output, $root.DIRECTORY_SEPARATOR)) {
        throw new RuntimeException('Output must be outside the immutable fixture');
    }
    if (file_exists($output.DIRECTORY_SEPARATOR.'storage.pdf') || file_exists($output.DIRECTORY_SEPARATOR.'rendered.pdf')) {
        throw new RuntimeException('Output already contains a PDF; use a fresh attempt directory');
    }
    if ($provider === 'pliego') {
        require requiredFile($options['sdk-autoload'] ?? '');
        $binary = requiredFile($options['binary'] ?? '');
        $assets = array_map(static fn (array $entry) => new Pliego\Php\InputAsset(
            $entry[0]['path'], $entry[1], $entry[0]['mediaType'],
        ), $assetFiles);
        $engine = new Pliego\Php\DocumentEngine([$binary], $output.DIRECTORY_SEPARATOR.'jobs', 65, 65);
        $record['networkPolicy'] = 'API 2 closed input manifest; live network denied';
        $phase = 'render';
        $renderStarted = hrtime(true);
        $rendered = $engine->render(file_get_contents($inputPath), new Pliego\Php\RenderOptions(
            pageSize: 'A4', pageMargins: '0,0,0,0', diagnosticsRetention: 'always',
        ), $assets);
        $record['renderWallMs'] = (hrtime(true) - $renderStarted) / 1e6;
        $record['metadata'] = $rendered->metadata;
        $record['bridgeTimings'] = $rendered->bridgeTimings;
        $record['jobPath'] = $rendered->jobPath;
        $source = $rendered->pdfPath;
    } else {
        require requiredFile($options['app-autoload'] ?? '');
        foreach (['chrome', 'node'] as $name) {
            $options[$name] = requiredFile($options[$name] ?? '');
        }
        $modules = realpath($options['node-modules'] ?? '');
        if ($modules === false || !is_dir($modules)) {
            throw new RuntimeException('Node modules directory unavailable');
        }
        requiredFile($modules.DIRECTORY_SEPARATOR.'puppeteer'.DIRECTORY_SEPARATOR.'package.json');
        // Browsershot 5.0.5's Windows command bypasses setNodeModulePath().
        // Symfony inherits NODE_PATH only when present in both getenv() and
        // $_SERVER. This standalone PHP process does not alter its caller env.
        if (!putenv('NODE_PATH='.$modules)) {
            throw new RuntimeException('Cannot configure the Node module environment');
        }
        $_SERVER['NODE_PATH'] = $modules;
        // The orchestrator verifies actual Node loading once, outside samples.
        $record['puppeteerVersion'] = json_decode(file_get_contents(
            $modules.DIRECTORY_SEPARATOR.'puppeteer'.DIRECTORY_SEPARATOR.'package.json',
        ), true, flags: JSON_THROW_ON_ERROR)['version'];
        $record['browsershotVersion'] = Composer\InstalledVersions::getPrettyVersion('spatie/browsershot');
        $record['networkPolicy'] = 'Browsershot blocks http/https/ws/wss/ftp document requests; not an OS network sandbox';
        $source = $output.DIRECTORY_SEPARATOR.'rendered.pdf';
        // The app-locked 5.0.5 rejects file: in url(); its explicit local-file
        // API preserves the original HTML bytes and the relative resource base.
        $shot = Spatie\Browsershot\Browsershot::htmlFromFilePath(str_replace('\\', '/', $inputPath))
            ->setChromePath($options['chrome'])->setNodeBinary($options['node'])
            ->setNodeModulePath($modules)->setCustomTempPath($output)
            ->setUserDataDir($output.DIRECTORY_SEPARATOR.'chrome-profile')
            ->format('A4')->margins(0, 0, 0, 0)->showBackground()->timeout(65)
            ->blockUrls(['http://', 'https://', 'ws://', 'wss://', 'ftp://'])
            ->disableRedirects()->addChromiumArguments(['allow-file-access-from-files', 'disable-background-networking', 'no-first-run']);
        $phase = 'render';
        $renderStarted = hrtime(true);
        $shot->savePdf($source);
        $record['renderWallMs'] = (hrtime(true) - $renderStarted) / 1e6;
        $browserOutput = $shot->getOutput();
        $record['browser'] = [
            'requests' => $browserOutput?->getRequestsList() ?? [],
            'failedRequests' => $browserOutput?->getFailedRequests() ?? [],
            'pageErrors' => $browserOutput?->getPageErrors() ?? [],
            'consoleMessages' => $browserOutput?->getConsoleMessages() ?? [],
        ];
        $record['pdfPath'] = $source;
        $blockedRequests = array_values(array_filter($record['browser']['requests'], static fn (array $request): bool =>
            in_array(strtolower((string) parse_url($request['url'], PHP_URL_SCHEME)), ['http', 'https', 'ws', 'wss', 'ftp'], true)
        ));
        if ($blockedRequests !== []) {
            $record['failureReason'] = 'blocked_network';
            $record['blockedRequests'] = $blockedRequests;
            throw new RuntimeException('Document requested external resources blocked by the offline comparison policy');
        }
        if ($record['browser']['failedRequests'] !== [] || $record['browser']['pageErrors'] !== []) {
            $record['failureReason'] = 'browser_resource_or_script_error';
            throw new RuntimeException('Browser reported resource or script errors; retain PDF without accepting it');
        }
    }
    $record['pdfPath'] = $source;
    $phase = 'storage';
    $storageStarted = hrtime(true);
    $record['storage'] = storeAndVerify($source, $output.DIRECTORY_SEPARATOR.'storage.pdf');
    $record['storageWallMs'] = (hrtime(true) - $storageStarted) / 1e6;
    $record['status'] = 'render_success'; // The Python oracle still has to approve the PDF.
} catch (Throwable $error) {
    $record['status'] = match ($phase) {
        'setup' => 'setup_failure', 'storage' => 'storage_failure', default => 'render_failure',
    };
    $record['error'] = ['class' => get_class($error), 'message' => $error->getMessage()];
    if ($error instanceof Pliego\Php\Exception\RenderFailedException) {
        // Current API 2 kinds do not distinguish unsupported content. Preserve
        // capture/resource/etc. without guessing from prose diagnostics.
        $record['error']['kind'] = $error->kind;
        $record['metadata'] = $error->result;
        $record['jobPath'] = $error->jobPath;
        $record['diagnosticsPath'] = $error->diagnosticsPath;
    } elseif ($error instanceof Pliego\Php\Exception\TransportException) {
        $record['status'] = 'transport_failure';
        $record['error'] += ['exitCode' => $error->exitCode, 'stdout' => $error->stdout, 'stderr' => $error->stderr];
        $record['jobPath'] = $error->jobPath;
    }
}
$record['adapterWallMs'] = (hrtime(true) - $started) / 1e6;
echo json_encode($record, JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE)."\n";
exit($record['status'] === 'render_success' ? 0 : 1);
