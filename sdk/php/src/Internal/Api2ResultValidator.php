<?php

declare(strict_types=1);

namespace Pliego\Php\Internal;

use FilesystemIterator;
use UnexpectedValueException;

/** @internal */
final class Api2ResultValidator
{
    private const RESULT_KEYS = [
        'schema',
        'version',
        'api',
        'status',
        'request',
        'engine',
        'delivery',
        'conformance',
        'diagnostics',
        'error',
    ];
    private const DESCRIPTOR_KEYS = ['path', 'media_type', 'sha256', 'bytes'];
    private const ERROR_KINDS = [
        'resource',
        'readiness',
        'settlement',
        'capture',
        'artifact',
        'conformance',
        'internal',
    ];

    /**
     * @param array<string, mixed> $request
     * @param array<string, mixed> $engine
     */
    public function __construct(
        private readonly string $runtimeRoot,
        private readonly array $request,
        private readonly array $engine,
    ) {}

    /** @return array<string, mixed> */
    public function validate(string $stdout, int $exitCode): array
    {
        $result = CanonicalJson::decodeFrame($stdout, 'API 2 render result');
        self::assertKeys($result, self::RESULT_KEYS, 'render result');
        self::assertLiteral($result['schema'], 'pliego.render-result', 'render result.schema');
        self::assertLiteral($result['version'], 1, 'render result.version');
        self::assertLiteral($result['api'], 2, 'render result.api');
        if ($result['request'] !== $this->request) {
            throw new UnexpectedValueException('render result.request does not exactly echo the accepted request');
        }
        if ($result['engine'] !== $this->engine) {
            throw new UnexpectedValueException('render result.engine does not exactly match the probed executable');
        }

        $conformance = self::object($result['conformance'], 'render result.conformance');
        self::assertKeys(
            $conformance,
            ['requested', 'status', 'evidence'],
            'render result.conformance',
        );
        if ($conformance !== [
            'requested' => null,
            'status' => 'not-requested',
            'evidence' => null,
        ]) {
            throw new UnexpectedValueException(
                'profile-null render result requires not-requested conformance without evidence',
            );
        }

        $this->validateInputClosure();
        $this->validateDiagnostics($result['diagnostics'], $result['status']);
        if ($result['status'] === 'success') {
            if ($exitCode !== 0 || $result['error'] !== null) {
                throw new UnexpectedValueException('successful API 2 result requires exit 0 and error null');
            }
            $this->validateDelivery($result['delivery']);
        } elseif ($result['status'] === 'failed') {
            if ($exitCode !== 1 || $result['delivery'] !== null) {
                throw new UnexpectedValueException('failed API 2 result requires exit 1 and delivery null');
            }
            $error = self::object($result['error'], 'render result.error');
            self::assertKeys($error, ['kind'], 'render result.error');
            if (!is_string($error['kind']) || !in_array($error['kind'], self::ERROR_KINDS, true)) {
                throw new UnexpectedValueException('render result.error.kind is unsupported');
            }
            if (file_exists($this->runtimeRoot.DIRECTORY_SEPARATOR.'delivery')) {
                throw new UnexpectedValueException('failed API 2 result published a delivery directory');
            }
        } else {
            throw new UnexpectedValueException('render result.status is unsupported');
        }

        $this->validateTopLevel($result['status']);

        return $result;
    }

    private function validateInputClosure(): void
    {
        $descriptor = $this->request['input']['manifest'] ?? null;
        $descriptor = self::object($descriptor, 'request.input.manifest');
        $this->validateDescriptor(
            $descriptor,
            $this->runtimeRoot,
            'input-manifest.json',
            'application/vnd.pliego.input-manifest+json',
            'request.input.manifest',
        );
        $manifestPath = $this->runtimeRoot.DIRECTORY_SEPARATOR.'input-manifest.json';
        $manifestBytes = file_get_contents($manifestPath);
        if (!is_string($manifestBytes)) {
            throw new UnexpectedValueException('cannot read retained API 2 input manifest');
        }
        $manifest = CanonicalJson::decodeFrame($manifestBytes, 'API 2 input manifest');
        self::assertKeys($manifest, ['schema', 'version', 'url_root', 'entries'], 'input manifest');
        self::assertLiteral($manifest['schema'], 'pliego.input-manifest', 'input manifest.schema');
        self::assertLiteral($manifest['version'], 1, 'input manifest.version');
        self::assertLiteral($manifest['url_root'], 'pliego-input:///', 'input manifest.url_root');
        $entries = self::list($manifest['entries'], 'input manifest.entries', 1, 16_384);

        $expectedFiles = [];
        $expectedDirectories = [];
        $previous = null;
        $entrypointFound = false;
        foreach ($entries as $index => $entry) {
            $entry = self::object($entry, "input manifest.entries[{$index}]");
            self::assertKeys($entry, self::DESCRIPTOR_KEYS, "input manifest.entries[{$index}]");
            $path = self::portablePath($entry['path'], "input manifest.entries[{$index}].path");
            if ($previous !== null && strcmp($previous, $path) >= 0) {
                throw new UnexpectedValueException('input manifest entries are not uniquely ASCII-ordered');
            }
            $previous = $path;
            $this->validateDescriptor(
                $entry,
                $this->runtimeRoot.DIRECTORY_SEPARATOR.'input',
                $path,
                null,
                "input manifest.entries[{$index}]",
                allowEmpty: true,
            );
            $expectedFiles[] = $path;
            self::addParentDirectories($path, $expectedDirectories);
            if ($path === ($this->request['input']['entrypoint'] ?? null)) {
                if ($entry['media_type'] !== 'text/html;charset=utf-8') {
                    throw new UnexpectedValueException('API 2 entrypoint requires text/html;charset=utf-8');
                }
                $entrypointFound = true;
            }
        }
        if (!$entrypointFound) {
            throw new UnexpectedValueException('API 2 entrypoint is absent from the input manifest');
        }

        [$actualFiles, $actualDirectories] = $this->collectTree(
            $this->runtimeRoot.DIRECTORY_SEPARATOR.'input',
        );
        sort($expectedFiles, SORT_STRING);
        $expectedDirectories = array_keys($expectedDirectories);
        sort($expectedDirectories, SORT_STRING);
        if ($actualFiles !== $expectedFiles || $actualDirectories !== $expectedDirectories) {
            throw new UnexpectedValueException('retained API 2 input tree does not exactly match its manifest');
        }
    }

    private function validateDiagnostics(mixed $diagnostics, mixed $status): void
    {
        $diagnostics = self::object($diagnostics, 'render result.diagnostics');
        self::assertKeys($diagnostics, ['retained', 'artifacts'], 'render result.diagnostics');
        if (!is_bool($diagnostics['retained'])) {
            throw new UnexpectedValueException('render result.diagnostics.retained must be boolean');
        }
        $artifacts = self::list(
            $diagnostics['artifacts'],
            'render result.diagnostics.artifacts',
            0,
            1_000_000,
        );
        $expectedRetained = match ($this->request['diagnostics']['retention'] ?? null) {
            'always' => true,
            'none' => false,
            'on-failure' => $status === 'failed',
            default => throw new UnexpectedValueException('accepted request has invalid diagnostics policy'),
        };
        if ($diagnostics['retained'] !== $expectedRetained) {
            throw new UnexpectedValueException('render result diagnostics retention contradicts the request');
        }

        $expectedFiles = [];
        $expectedDirectories = [];
        $previous = null;
        foreach ($artifacts as $index => $artifact) {
            $artifact = self::object($artifact, "render result.diagnostics.artifacts[{$index}]");
            self::assertKeys($artifact, self::DESCRIPTOR_KEYS, "render result.diagnostics.artifacts[{$index}]");
            if (
                !is_string($artifact['path'])
                || preg_match(
                    '/^diagnostics\/[A-Za-z0-9](?:[A-Za-z0-9._\/-]*[A-Za-z0-9_-])?$/D',
                    $artifact['path'],
                ) !== 1
            ) {
                throw new UnexpectedValueException('diagnostic artifact path is not portable');
            }
            $path = $artifact['path'];
            self::portablePath(substr($path, strlen('diagnostics/')), 'diagnostic artifact relative path');
            if ($previous !== null && strcmp($previous, $path) >= 0) {
                throw new UnexpectedValueException('diagnostic artifacts are not uniquely ASCII-ordered');
            }
            $previous = $path;
            $this->validateDescriptor(
                $artifact,
                $this->runtimeRoot,
                $path,
                null,
                "render result.diagnostics.artifacts[{$index}]",
            );
            $relative = substr($path, strlen('diagnostics/'));
            $expectedFiles[] = $relative;
            self::addParentDirectories($relative, $expectedDirectories);
        }

        $diagnosticsRoot = $this->runtimeRoot.DIRECTORY_SEPARATOR.'diagnostics';
        if (!$diagnostics['retained']) {
            if ($artifacts !== [] || file_exists($diagnosticsRoot)) {
                throw new UnexpectedValueException('unretained diagnostics must have no artifacts or directory');
            }

            return;
        }
        if (!is_dir($diagnosticsRoot) || is_link($diagnosticsRoot)) {
            throw new UnexpectedValueException('retained diagnostics directory is missing or unsafe');
        }
        [$actualFiles, $actualDirectories] = $this->collectTree($diagnosticsRoot);
        sort($expectedFiles, SORT_STRING);
        $expectedDirectories = array_keys($expectedDirectories);
        sort($expectedDirectories, SORT_STRING);
        if ($actualFiles !== $expectedFiles || $actualDirectories !== $expectedDirectories) {
            throw new UnexpectedValueException('diagnostics tree contains missing or uninventoried entries');
        }
    }

    private function validateDelivery(mixed $delivery): void
    {
        $delivery = self::object($delivery, 'render result.delivery');
        self::assertKeys($delivery, ['pdf', 'scene', 'bundle'], 'render result.delivery');
        $pdf = self::object($delivery['pdf'], 'render result.delivery.pdf');
        $scene = self::object($delivery['scene'], 'render result.delivery.scene');
        $bundle = self::object($delivery['bundle'], 'render result.delivery.bundle');
        $deliveryRoot = $this->runtimeRoot.DIRECTORY_SEPARATOR.'delivery';
        $this->validateDescriptor(
            $pdf,
            $deliveryRoot,
            'document.pdf',
            'application/pdf',
            'render result.delivery.pdf',
        );
        $this->validateDescriptor(
            $scene,
            $deliveryRoot,
            'scene.json',
            'application/vnd.pliego.document-scene+json',
            'render result.delivery.scene',
        );
        $this->validateDescriptor(
            $bundle,
            $deliveryRoot,
            'bundle.json',
            'application/vnd.pliego.bundle-manifest+json',
            'render result.delivery.bundle',
        );

        $pdfBytes = file_get_contents($deliveryRoot.DIRECTORY_SEPARATOR.'document.pdf', false, null, 0, 5);
        if ($pdfBytes !== '%PDF-') {
            throw new UnexpectedValueException('API 2 delivery document is not a PDF');
        }

        $sceneBytes = file_get_contents($deliveryRoot.DIRECTORY_SEPARATOR.'scene.json');
        if (!is_string($sceneBytes)) {
            throw new UnexpectedValueException('cannot read delivered API 2 scene');
        }
        $sceneDocument = CanonicalJson::decodeFrame($sceneBytes, 'API 2 document scene');
        self::assertKeys(
            $sceneDocument,
            ['schema', 'version', 'app_units_per_css_px', 'request_page', 'semantic_layer', 'pages'],
            'document scene',
        );
        self::assertLiteral($sceneDocument['schema'], 'pliego.document-scene', 'document scene.schema');
        self::assertLiteral($sceneDocument['version'], 2, 'document scene.version');
        self::assertLiteral($sceneDocument['app_units_per_css_px'], 60, 'document scene.app_units_per_css_px');
        if ($sceneDocument['request_page'] !== $this->request['page']) {
            throw new UnexpectedValueException('document scene does not echo request page authority');
        }
        if ($sceneDocument['semantic_layer'] !== null) {
            throw new UnexpectedValueException('profile-null document scene requires semantic_layer null');
        }
        $pages = self::list($sceneDocument['pages'], 'document scene.pages', 1, 1_000_000);
        foreach ($pages as $index => $page) {
            $page = self::object($page, "document scene.pages[{$index}]");
            self::assertKeys(
                $page,
                ['number', 'style_source', 'size_app_units', 'margins_app_units', 'operations'],
                "document scene.pages[{$index}]",
            );
            self::positiveInteger($page['number'], "document scene.pages[{$index}].number", 4_294_967_295);
            self::assertLiteral(
                $page['style_source'],
                'request-defaults',
                "document scene.pages[{$index}].style_source",
            );
            $size = self::object($page['size_app_units'], "document scene.pages[{$index}].size_app_units");
            self::assertKeys($size, ['width', 'height'], "document scene.pages[{$index}].size_app_units");
            self::positiveInteger($size['width'], 'document scene page width', 2_147_483_647);
            self::positiveInteger($size['height'], 'document scene page height', 2_147_483_647);
            if ($page['margins_app_units'] !== $this->request['page']['margins_app_units']) {
                throw new UnexpectedValueException('document scene page margins contradict the request');
            }
            self::list($page['operations'], "document scene.pages[{$index}].operations", 0, 1_000_000);
        }

        $bundleBytes = file_get_contents($deliveryRoot.DIRECTORY_SEPARATOR.'bundle.json');
        if (!is_string($bundleBytes)) {
            throw new UnexpectedValueException('cannot read delivered API 2 bundle manifest');
        }
        $bundleDocument = CanonicalJson::decodeFrame($bundleBytes, 'API 2 bundle manifest');
        self::assertKeys($bundleDocument, ['schema', 'version', 'entries'], 'bundle manifest');
        self::assertLiteral($bundleDocument['schema'], 'pliego.bundle-manifest', 'bundle manifest.schema');
        self::assertLiteral($bundleDocument['version'], 1, 'bundle manifest.version');
        $entries = self::list($bundleDocument['entries'], 'bundle manifest.entries', 2, 1_000_002);

        $expectedFiles = ['bundle.json'];
        $expectedDirectories = [];
        $resourcePaths = [];
        $manifestPdf = null;
        $manifestScene = null;
        $previous = null;
        foreach ($entries as $index => $entry) {
            $entry = self::object($entry, "bundle manifest.entries[{$index}]");
            self::assertKeys($entry, self::DESCRIPTOR_KEYS, "bundle manifest.entries[{$index}]");
            if (!is_string($entry['path'])) {
                throw new UnexpectedValueException('bundle manifest entry path must be a string');
            }
            $path = $entry['path'];
            if ($previous !== null && strcmp($previous, $path) >= 0) {
                throw new UnexpectedValueException('bundle manifest entries are not uniquely ASCII-ordered');
            }
            $previous = $path;
            if ($path === 'document.pdf') {
                if ($manifestPdf !== null) {
                    throw new UnexpectedValueException('bundle manifest contains duplicate document.pdf');
                }
                $manifestPdf = $entry;
                $expectedMediaType = 'application/pdf';
            } elseif ($path === 'scene.json') {
                if ($manifestScene !== null) {
                    throw new UnexpectedValueException('bundle manifest contains duplicate scene.json');
                }
                $manifestScene = $entry;
                $expectedMediaType = 'application/vnd.pliego.document-scene+json';
            } elseif (preg_match('/^resources\/[0-9a-f]{64}$/D', $path) === 1) {
                $expectedMediaType = null;
                if ($entry['sha256'] !== 'sha256:'.substr($path, strlen('resources/'))) {
                    throw new UnexpectedValueException('bundle resource path does not match its content address');
                }
                if (
                    !is_string($entry['media_type'])
                    || preg_match(
                        '/^[a-z0-9][a-z0-9!#$&^_.+-]*\/[a-z0-9][a-z0-9!#$&^_.+-]*(?:;charset=utf-8)?$/D',
                        $entry['media_type'],
                    ) !== 1
                ) {
                    throw new UnexpectedValueException('bundle resource media type is not canonical');
                }
                $resourcePaths[] = $path;
            } else {
                throw new UnexpectedValueException("unsupported bundle manifest path: {$path}");
            }
            $this->validateDescriptor(
                $entry,
                $deliveryRoot,
                $path,
                $expectedMediaType,
                "bundle manifest.entries[{$index}]",
            );
            $expectedFiles[] = $path;
            self::addParentDirectories($path, $expectedDirectories);
        }
        if ($manifestPdf !== $pdf || $manifestScene !== $scene) {
            throw new UnexpectedValueException('result PDF/scene descriptors do not equal bundle entries');
        }

        $sceneResources = [];
        self::collectResourceReferences($sceneDocument, $sceneResources);
        $expectedResourcePaths = array_map(
            static fn (string $address): string => 'resources/'.substr($address, strlen('sha256:')),
            array_keys($sceneResources),
        );
        sort($expectedResourcePaths, SORT_STRING);
        sort($resourcePaths, SORT_STRING);
        if ($expectedResourcePaths !== $resourcePaths) {
            throw new UnexpectedValueException('bundle resources do not exactly close document scene references');
        }

        [$actualFiles, $actualDirectories] = $this->collectTree($deliveryRoot);
        sort($expectedFiles, SORT_STRING);
        $expectedDirectories = array_keys($expectedDirectories);
        sort($expectedDirectories, SORT_STRING);
        if ($actualFiles !== $expectedFiles || $actualDirectories !== $expectedDirectories) {
            throw new UnexpectedValueException('delivery tree contains missing or unmanifested entries');
        }
    }

    private function validateTopLevel(mixed $status): void
    {
        $expected = ['input', 'input-manifest.json'];
        if ($status === 'success') {
            $expected[] = 'delivery';
        }
        if (file_exists($this->runtimeRoot.DIRECTORY_SEPARATOR.'diagnostics')) {
            $expected[] = 'diagnostics';
        }
        sort($expected, SORT_STRING);
        $actual = array_values(array_filter(
            scandir($this->runtimeRoot) ?: [],
            static fn (string $entry): bool => $entry !== '.' && $entry !== '..',
        ));
        sort($actual, SORT_STRING);
        if ($actual !== $expected) {
            throw new UnexpectedValueException('API 2 runtime job root contains unsupported top-level entries');
        }
    }

    /** @param array<string, mixed> $descriptor */
    private function validateDescriptor(
        array $descriptor,
        string $root,
        string $expectedPath,
        ?string $expectedMediaType,
        string $label,
        bool $allowEmpty = false,
    ): void {
        self::assertKeys($descriptor, self::DESCRIPTOR_KEYS, $label);
        self::assertLiteral($descriptor['path'], $expectedPath, "{$label}.path");
        if (
            !is_string($descriptor['media_type'])
            || strlen($descriptor['media_type']) < 3
            || strlen($descriptor['media_type']) > 255
        ) {
            throw new UnexpectedValueException("{$label}.media_type is invalid");
        }
        if ($expectedMediaType !== null) {
            self::assertLiteral($descriptor['media_type'], $expectedMediaType, "{$label}.media_type");
        }
        if (!is_string($descriptor['sha256']) || preg_match('/^sha256:[0-9a-f]{64}$/D', $descriptor['sha256']) !== 1) {
            throw new UnexpectedValueException("{$label}.sha256 is invalid");
        }
        $minimum = $allowEmpty ? 0 : 1;
        if (
            !is_int($descriptor['bytes'])
            || $descriptor['bytes'] < $minimum
            || $descriptor['bytes'] > 9_007_199_254_740_991
        ) {
            throw new UnexpectedValueException("{$label}.bytes is invalid");
        }

        $path = $root.DIRECTORY_SEPARATOR.str_replace('/', DIRECTORY_SEPARATOR, $expectedPath);
        if (is_link($path) || !is_file($path)) {
            throw new UnexpectedValueException("{$label} does not resolve to a regular retained file");
        }
        $bytes = filesize($path);
        $hash = hash_file('sha256', $path);
        if ($bytes !== $descriptor['bytes'] || !is_string($hash) || 'sha256:'.$hash !== $descriptor['sha256']) {
            throw new UnexpectedValueException("{$label} does not match retained bytes");
        }
    }

    /** @return array{list<string>, list<string>} */
    private function collectTree(string $root): array
    {
        if (is_link($root) || !is_dir($root)) {
            throw new UnexpectedValueException("API 2 artifact root is missing or unsafe: {$root}");
        }
        $files = [];
        $directories = [];
        $walk = function (string $directory, string $prefix) use (&$walk, &$files, &$directories): void {
            $iterator = new FilesystemIterator($directory, FilesystemIterator::SKIP_DOTS);
            foreach ($iterator as $entry) {
                $name = $entry->getFilename();
                $relative = $prefix === '' ? $name : $prefix.'/'.$name;
                if ($entry->isLink()) {
                    throw new UnexpectedValueException("API 2 artifact tree contains a link: {$relative}");
                }
                if ($entry->isDir()) {
                    $directories[] = $relative;
                    $walk($entry->getPathname(), $relative);
                } elseif ($entry->isFile()) {
                    $files[] = $relative;
                } else {
                    throw new UnexpectedValueException("API 2 artifact tree contains a special node: {$relative}");
                }
            }
        };
        $walk($root, '');
        sort($files, SORT_STRING);
        sort($directories, SORT_STRING);

        return [$files, $directories];
    }

    /** @param array<string, true> $directories */
    private static function addParentDirectories(string $path, array &$directories): void
    {
        $segments = explode('/', $path);
        array_pop($segments);
        $prefix = '';
        foreach ($segments as $segment) {
            $prefix = $prefix === '' ? $segment : $prefix.'/'.$segment;
            $directories[$prefix] = true;
        }
    }

    /** @param array<string, true> $resources */
    private static function collectResourceReferences(mixed $value, array &$resources): void
    {
        if (!is_array($value)) {
            return;
        }
        foreach ($value as $key => $member) {
            if (
                $key === 'resource'
                && is_string($member)
                && preg_match('/^sha256:[0-9a-f]{64}$/D', $member) === 1
            ) {
                $resources[$member] = true;
            }
            self::collectResourceReferences($member, $resources);
        }
    }

    /** @param array<string, mixed> $object @param list<string> $keys */
    private static function assertKeys(array $object, array $keys, string $path): void
    {
        if (array_keys($object) !== $keys) {
            throw new UnexpectedValueException("{$path} has unsupported or out-of-order members");
        }
    }

    /** @return array<string, mixed> */
    private static function object(mixed $value, string $path): array
    {
        if (!is_array($value) || array_is_list($value)) {
            throw new UnexpectedValueException("{$path} must be an object");
        }

        return $value;
    }

    /** @return list<mixed> */
    private static function list(mixed $value, string $path, int $minimum, int $maximum): array
    {
        if (
            !is_array($value)
            || !array_is_list($value)
            || count($value) < $minimum
            || count($value) > $maximum
        ) {
            throw new UnexpectedValueException("{$path} must contain {$minimum}..{$maximum} items");
        }

        return $value;
    }

    private static function portablePath(mixed $value, string $path): string
    {
        if (
            !is_string($value)
            || strlen($value) < 1
            || strlen($value) > 240
            || preg_match('/^[A-Za-z0-9](?:[A-Za-z0-9._\/-]*[A-Za-z0-9_-])?$/D', $value) !== 1
        ) {
            throw new UnexpectedValueException("{$path} is not a portable path");
        }
        $segments = explode('/', $value);
        if (count($segments) > 32) {
            throw new UnexpectedValueException("{$path} has more than 32 segments");
        }
        foreach ($segments as $segment) {
            if (
                $segment === ''
                || $segment === '.'
                || $segment === '..'
                || strlen($segment) > 100
                || $segment !== rtrim($segment, '. ')
                || preg_match(
                    '/^(?:CON|PRN|AUX|NUL|CLOCK\$|COM[1-9]|LPT[1-9])(?:\.|$)/iD',
                    $segment,
                ) === 1
            ) {
                throw new UnexpectedValueException("{$path} has an unsafe segment");
            }
        }

        return $value;
    }

    private static function positiveInteger(mixed $value, string $path, int $maximum): void
    {
        if (!is_int($value) || $value < 1 || $value > $maximum) {
            throw new UnexpectedValueException("{$path} is outside its integer range");
        }
    }

    private static function assertLiteral(mixed $actual, mixed $expected, string $path): void
    {
        if ($actual !== $expected) {
            throw new UnexpectedValueException("{$path} has an unsupported value or type");
        }
    }
}
