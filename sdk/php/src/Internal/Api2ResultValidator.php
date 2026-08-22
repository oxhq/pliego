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
        $expectedPageSize = $this->requestPageDimensions();
        $sceneResources = [];
        $pages = self::list($sceneDocument['pages'], 'document scene.pages', 1, 4_294_967_295);
        foreach ($pages as $index => $page) {
            $pagePath = "document scene.pages[{$index}]";
            $page = self::object($page, $pagePath);
            self::assertKeys(
                $page,
                ['number', 'style_source', 'size_app_units', 'margins_app_units', 'operations'],
                $pagePath,
            );
            self::assertLiteral($page['number'], $index + 1, "{$pagePath}.number");
            self::assertLiteral(
                $page['style_source'],
                'request-defaults',
                "{$pagePath}.style_source",
            );
            $size = self::object($page['size_app_units'], "{$pagePath}.size_app_units");
            self::assertKeys($size, ['width', 'height'], "{$pagePath}.size_app_units");
            if ($size !== $expectedPageSize) {
                throw new UnexpectedValueException("{$pagePath}.size_app_units contradicts the request");
            }
            if ($page['margins_app_units'] !== $this->request['page']['margins_app_units']) {
                throw new UnexpectedValueException("{$pagePath}.margins_app_units contradicts the request");
            }
            $this->validatePageBox(
                $size,
                self::object($page['margins_app_units'], "{$pagePath}.margins_app_units"),
                $pagePath,
            );
            $operations = self::list($page['operations'], "{$pagePath}.operations", 0, 4_294_967_295);
            foreach ($operations as $operationIndex => $operation) {
                $this->validateSceneOperation(
                    $operation,
                    "{$pagePath}.operations[{$operationIndex}]",
                    $sceneResources,
                );
            }
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
                $resource = 'sha256:'.substr($path, strlen('resources/'));
                if ($entry['sha256'] !== $resource) {
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
                if (isset($sceneResources[$resource]) && $entry['media_type'] !== $sceneResources[$resource]) {
                    throw new UnexpectedValueException(
                        "bundle resource {$resource} media type contradicts its document scene use",
                    );
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
            if (isset($resource)) {
                $this->validateResourceRepresentation(
                    $deliveryRoot.DIRECTORY_SEPARATOR.str_replace('/', DIRECTORY_SEPARATOR, $path),
                    $entry['media_type'],
                    $resource,
                );
                unset($resource);
            }
            $expectedFiles[] = $path;
            self::addParentDirectories($path, $expectedDirectories);
        }
        if ($manifestPdf !== $pdf || $manifestScene !== $scene) {
            throw new UnexpectedValueException('result PDF/scene descriptors do not equal bundle entries');
        }

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

    /** @return array{width: int, height: int} */
    private function requestPageDimensions(): array
    {
        $page = self::object($this->request['page'] ?? null, 'accepted request.page');
        self::assertKeys(
            $page,
            ['size', 'margins_app_units', 'geometry_authority'],
            'accepted request.page',
        );
        self::assertLiteral(
            $page['geometry_authority'],
            'request-only-v1',
            'accepted request.page.geometry_authority',
        );
        $size = self::object($page['size'], 'accepted request.page.size');
        if ($size === ['name' => 'A4']) {
            $dimensions = ['width' => 47_622, 'height' => 67_351];
        } else {
            self::assertKeys(
                $size,
                ['width_app_units', 'height_app_units'],
                'accepted request.page.size',
            );
            self::positiveInteger(
                $size['width_app_units'],
                'accepted request.page.size.width_app_units',
                2_147_483_647,
            );
            self::positiveInteger(
                $size['height_app_units'],
                'accepted request.page.size.height_app_units',
                2_147_483_647,
            );
            $dimensions = [
                'width' => $size['width_app_units'],
                'height' => $size['height_app_units'],
            ];
        }
        $this->validatePageBox(
            $dimensions,
            self::object($page['margins_app_units'], 'accepted request.page.margins_app_units'),
            'accepted request.page',
        );

        return $dimensions;
    }

    /**
     * @param array{width: mixed, height: mixed} $size
     * @param array<string, mixed> $margins
     */
    private function validatePageBox(array $size, array $margins, string $path): void
    {
        self::assertKeys($margins, ['top', 'right', 'bottom', 'left'], "{$path}.margins_app_units");
        self::positiveInteger($size['width'], "{$path}.size_app_units.width", 2_147_483_647);
        self::positiveInteger($size['height'], "{$path}.size_app_units.height", 2_147_483_647);
        foreach (['top', 'right', 'bottom', 'left'] as $side) {
            self::nonnegativeInteger(
                $margins[$side],
                "{$path}.margins_app_units.{$side}",
                2_147_483_647,
            );
        }
        if (
            $size['width'] - $margins['left'] - $margins['right'] <= 0
            || $size['height'] - $margins['top'] - $margins['bottom'] <= 0
        ) {
            throw new UnexpectedValueException("{$path} has no positive fixed-point content box");
        }
    }

    /** @param array<string, string> $resources */
    private function validateSceneOperation(mixed $operation, string $path, array &$resources): void
    {
        $operation = self::object($operation, $path);
        $type = $operation['type'] ?? null;
        if ($type === 'text') {
            self::assertKeys(
                $operation,
                ['type', 'text', 'font', 'font_size_app_units', 'color', 'glyphs'],
                $path,
            );
            if (
                !is_string($operation['text'])
                || $operation['text'] === ''
                || strlen($operation['text']) > 4_294_967_295
            ) {
                throw new UnexpectedValueException("{$path}.text must be a non-empty UTF-8 string");
            }
            $font = self::object($operation['font'], "{$path}.font");
            self::assertKeys(
                $font,
                ['resource', 'face_index', 'variations', 'synthetic_bold'],
                "{$path}.font",
            );
            $fontResource = self::contentAddress($font['resource'], "{$path}.font.resource");
            self::bindSceneResource($resources, $fontResource, 'application/octet-stream', $path);
            self::unsignedInteger($font['face_index'], "{$path}.font.face_index");
            if (!is_bool($font['synthetic_bold'])) {
                throw new UnexpectedValueException("{$path}.font.synthetic_bold must be boolean");
            }
            $variations = self::list($font['variations'], "{$path}.font.variations", 0, 4_294_967_295);
            $previousTag = null;
            foreach ($variations as $variationIndex => $variation) {
                $variationPath = "{$path}.font.variations[{$variationIndex}]";
                $variation = self::object($variation, $variationPath);
                self::assertKeys($variation, ['tag', 'value_f32_bits'], $variationPath);
                self::unsignedInteger($variation['tag'], "{$variationPath}.tag");
                if ($previousTag !== null && $previousTag >= $variation['tag']) {
                    throw new UnexpectedValueException("{$path}.font variation tags must be strictly ascending");
                }
                $previousTag = $variation['tag'];
                self::finiteF32Bits($variation['value_f32_bits'], "{$variationPath}.value_f32_bits");
            }
            self::positiveInteger(
                $operation['font_size_app_units'],
                "{$path}.font_size_app_units",
                2_147_483_647,
            );
            self::validateColor($operation['color'], "{$path}.color");
            $glyphs = self::list($operation['glyphs'], "{$path}.glyphs", 1, 4_294_967_295);
            foreach ($glyphs as $glyphIndex => $glyph) {
                $glyphPath = "{$path}.glyphs[{$glyphIndex}]";
                $glyph = self::object($glyph, $glyphPath);
                self::assertKeys($glyph, ['id', 'x', 'y', 'advance', 'text_range'], $glyphPath);
                self::unsignedInteger($glyph['id'], "{$glyphPath}.id");
                foreach (['x', 'y', 'advance'] as $member) {
                    self::signedInteger($glyph[$member], "{$glyphPath}.{$member}");
                }
                $range = self::object($glyph['text_range'], "{$glyphPath}.text_range");
                self::assertKeys($range, ['start', 'end'], "{$glyphPath}.text_range");
                self::unsignedInteger($range['start'], "{$glyphPath}.text_range.start");
                self::positiveInteger(
                    $range['end'],
                    "{$glyphPath}.text_range.end",
                    4_294_967_295,
                );
                self::validateUtf8Range(
                    $operation['text'],
                    $range['start'],
                    $range['end'],
                    "{$glyphPath}.text_range",
                );
            }

            return;
        }
        if ($type === 'path') {
            $hasFill = array_key_exists('fill', $operation);
            $hasStroke = array_key_exists('stroke', $operation);
            $keys = ['type', 'bounds', 'data'];
            if ($hasFill) {
                $keys[] = 'fill';
            }
            $keys[] = 'fill_rule';
            if ($hasStroke) {
                $keys[] = 'stroke';
            }
            self::assertKeys($operation, $keys, $path);
            if (!$hasFill && !$hasStroke) {
                throw new UnexpectedValueException("{$path} requires fill, stroke, or both");
            }
            self::validateRect($operation['bounds'], "{$path}.bounds");
            self::validatePathData($operation['data'], "{$path}.data");
            if ($hasFill) {
                self::validateColor($operation['fill'], "{$path}.fill");
            }
            if (!in_array($operation['fill_rule'], ['non-zero', 'even-odd'], true)) {
                throw new UnexpectedValueException("{$path}.fill_rule is unsupported");
            }
            if ($hasStroke) {
                $stroke = self::object($operation['stroke'], "{$path}.stroke");
                self::assertKeys($stroke, ['color', 'width_app_units'], "{$path}.stroke");
                self::validateColor($stroke['color'], "{$path}.stroke.color");
                self::positiveInteger(
                    $stroke['width_app_units'],
                    "{$path}.stroke.width_app_units",
                    2_147_483_647,
                );
            }

            return;
        }
        if ($type === 'image') {
            self::assertKeys($operation, ['type', 'bounds', 'resource', 'media_type'], $path);
            self::validateRect($operation['bounds'], "{$path}.bounds");
            $resource = self::contentAddress($operation['resource'], "{$path}.resource");
            if (!in_array($operation['media_type'], ['image/gif', 'image/jpeg', 'image/png', 'image/webp'], true)) {
                throw new UnexpectedValueException("{$path}.media_type is unsupported");
            }
            self::bindSceneResource($resources, $resource, $operation['media_type'], $path);

            return;
        }
        if ($type === 'link') {
            self::assertKeys($operation, ['type', 'bounds', 'target'], $path);
            self::validateRect($operation['bounds'], "{$path}.bounds");
            if (!is_string($operation['target']) || !self::canonicalLinkTarget($operation['target'])) {
                throw new UnexpectedValueException("{$path}.target is not canonical");
            }

            return;
        }

        throw new UnexpectedValueException("{$path}.type is unsupported");
    }

    /** @return array{x: int, y: int, width: int, height: int} */
    private static function validateRect(mixed $value, string $path): array
    {
        $rect = self::object($value, $path);
        self::assertKeys($rect, ['x', 'y', 'width', 'height'], $path);
        self::signedInteger($rect['x'], "{$path}.x");
        self::signedInteger($rect['y'], "{$path}.y");
        self::nonnegativeInteger($rect['width'], "{$path}.width", 2_147_483_647);
        self::nonnegativeInteger($rect['height'], "{$path}.height", 2_147_483_647);

        return $rect;
    }

    private static function validatePathData(mixed $value, string $path): void
    {
        if (!is_string($value) || strlen($value) < 5 || strlen($value) > 16_777_216) {
            throw new UnexpectedValueException("{$path} is outside the canonical path length range");
        }
        $tokens = explode(' ', $value);
        if ($tokens[0] !== 'M' || in_array('', $tokens, true)) {
            throw new UnexpectedValueException("{$path} must begin with absolute M and use one ASCII space");
        }
        $arity = ['M' => 2, 'L' => 2, 'Q' => 4, 'C' => 6, 'Z' => 0];
        for ($index = 0, $count = count($tokens); $index < $count;) {
            $command = $tokens[$index];
            if (!isset($arity[$command])) {
                throw new UnexpectedValueException("{$path} contains an unsupported path command");
            }
            $index++;
            $coordinates = $arity[$command];
            if ($index + $coordinates > $count) {
                throw new UnexpectedValueException("{$path} contains a truncated path command");
            }
            for ($offset = 0; $offset < $coordinates; $offset++, $index++) {
                $coordinate = $tokens[$index];
                if (preg_match('/^(?:0|-?[1-9][0-9]*)$/D', $coordinate) !== 1) {
                    throw new UnexpectedValueException("{$path} contains a noncanonical path coordinate");
                }
                $integer = (int) $coordinate;
                if ($integer < -2_147_483_648 || $integer > 2_147_483_647) {
                    throw new UnexpectedValueException("{$path} contains a coordinate outside signed i32");
                }
            }
        }
    }

    private static function validateColor(mixed $value, string $path): void
    {
        $color = self::object($value, $path);
        self::assertKeys($color, ['r', 'g', 'b', 'a'], $path);
        foreach (['r', 'g', 'b', 'a'] as $channel) {
            self::nonnegativeInteger($color[$channel], "{$path}.{$channel}", 255);
        }
    }

    private static function validateUtf8Range(string $text, int $start, int $end, string $path): void
    {
        if (
            $start >= $end
            || $end > strlen($text)
            || !self::isUtf8Boundary($text, $start)
            || !self::isUtf8Boundary($text, $end)
        ) {
            throw new UnexpectedValueException("{$path} is empty, outside the text, or not on UTF-8 boundaries");
        }
    }

    private static function isUtf8Boundary(string $text, int $offset): bool
    {
        return $offset === 0
            || $offset === strlen($text)
            || (ord($text[$offset]) & 0xc0) !== 0x80;
    }

    private static function finiteF32Bits(mixed $value, string $path): void
    {
        self::unsignedInteger($value, $path);
        if (
            !($value >= 0 && $value <= 2_139_095_039)
            && !($value >= 2_147_483_649 && $value <= 4_286_578_687)
        ) {
            throw new UnexpectedValueException("{$path} is not canonical finite binary32");
        }
    }

    private static function contentAddress(mixed $value, string $path): string
    {
        if (!is_string($value) || preg_match('/^sha256:[0-9a-f]{64}$/D', $value) !== 1) {
            throw new UnexpectedValueException("{$path} is not a content address");
        }

        return $value;
    }

    /** @param array<string, string> $resources */
    private static function bindSceneResource(
        array &$resources,
        string $resource,
        string $mediaType,
        string $path,
    ): void {
        if (isset($resources[$resource]) && $resources[$resource] !== $mediaType) {
            throw new UnexpectedValueException("{$path} gives {$resource} conflicting media identities");
        }
        $resources[$resource] = $mediaType;
    }

    private function validateResourceRepresentation(string $path, mixed $mediaType, string $resource): void
    {
        if (!is_string($mediaType) || !str_starts_with($mediaType, 'image/')) {
            return;
        }
        $prefix = file_get_contents($path, false, null, 0, 12);
        if (!is_string($prefix)) {
            throw new UnexpectedValueException("cannot read bundle resource {$resource}");
        }
        $matches = match ($mediaType) {
            'image/png' => str_starts_with($prefix, "\x89PNG\r\n\x1a\n"),
            'image/jpeg' => str_starts_with($prefix, "\xff\xd8\xff"),
            'image/gif' => str_starts_with($prefix, 'GIF87a') || str_starts_with($prefix, 'GIF89a'),
            'image/webp' => strlen($prefix) >= 12
                && str_starts_with($prefix, 'RIFF')
                && substr($prefix, 8, 4) === 'WEBP',
            default => true,
        };
        if (!$matches) {
            throw new UnexpectedValueException("bundle resource {$resource} bytes contradict {$mediaType}");
        }
    }

    private static function canonicalLinkTarget(string $target): bool
    {
        if (
            $target === ''
            || strlen($target) > 8_192
            || preg_match('/[^\x00-\x7f]/D', $target) === 1
            || preg_match('/[\x00-\x20\x7f]/D', $target) === 1
            || str_ends_with($target, '?')
            || str_ends_with($target, '#')
            || !self::canonicalPercentEncoding($target)
        ) {
            return false;
        }
        if (
            preg_match(
                '~\A(https?)://([^/?#]+)(/[^?#]*)(?:\?([^#]*))?(?:#(.*))?\z~',
                $target,
                $matches,
                PREG_UNMATCHED_AS_NULL,
            ) === 1
        ) {
            $scheme = $matches[1];
            $netloc = $matches[2];
            $path = $matches[3];
            $query = $matches[4];
            $fragment = $matches[5];
            try {
                $parts = parse_url($target);
            } catch (\ValueError) {
                return false;
            }
            if (
                !is_array($parts)
                || !isset($parts['host'])
                || !is_string($parts['host'])
                || $parts['host'] === ''
                || array_key_exists('user', $parts)
                || array_key_exists('pass', $parts)
                || $netloc !== strtolower($netloc)
                || !str_starts_with($path, '/')
                || !self::decodedPathHasNoDotSegments($path)
                || preg_match('/["<>`{}\\\\]/D', $path) === 1
                || ($query !== null && preg_match('/["<>\']/D', $query) === 1)
                || ($fragment !== null && preg_match('/["<>`]/D', $fragment) === 1)
            ) {
                return false;
            }
            $host = self::canonicalHttpHost($parts['host']);
            if ($host === null) {
                return false;
            }
            $canonicalNetloc = $host;
            if (isset($parts['port'])) {
                if (
                    !is_int($parts['port'])
                    || ($scheme === 'http' && $parts['port'] === 80)
                    || ($scheme === 'https' && $parts['port'] === 443)
                ) {
                    return false;
                }
                $canonicalNetloc .= ':'.$parts['port'];
            }

            return $netloc === $canonicalNetloc;
        }
        if (!str_starts_with($target, 'mailto:') || str_contains($target, '#')) {
            return false;
        }
        $address = substr($target, strlen('mailto:'));
        $query = null;
        if (($queryOffset = strpos($address, '?')) !== false) {
            $query = substr($address, $queryOffset + 1);
            $address = substr($address, 0, $queryOffset);
        }
        if (
            $address === ''
            || str_starts_with($address, '//')
            || ($query !== null && preg_match('/["<>]/D', $query) === 1)
        ) {
            return false;
        }
        $separator = strrpos($address, '@');
        if ($separator === false || $separator === 0 || $separator === strlen($address) - 1) {
            return false;
        }
        $domain = substr($address, $separator + 1);

        return $domain === strtolower($domain);
    }

    private static function canonicalHttpHost(string $host): ?string
    {
        if (str_starts_with($host, '[') || str_ends_with($host, ']')) {
            if (!str_starts_with($host, '[') || !str_ends_with($host, ']')) {
                return null;
            }
            $packed = @inet_pton(substr($host, 1, -1));
            if (!is_string($packed) || strlen($packed) !== 16) {
                return null;
            }
            $canonical = self::canonicalIpv6($packed);

            return $canonical !== null ? '['.$canonical.']' : null;
        }
        if (filter_var($host, FILTER_VALIDATE_IP, FILTER_FLAG_IPV4) !== false) {
            $packed = inet_pton($host);
            $canonical = is_string($packed) ? inet_ntop($packed) : false;

            return is_string($canonical) ? $canonical : null;
        }
        $candidate = rtrim($host, '.');
        $lastDot = strrpos($candidate, '.');
        $lastLabel = $lastDot === false ? $candidate : substr($candidate, $lastDot + 1);
        if (
            preg_match('/^(?:[0-9]+|0[xX][0-9A-Fa-f]*)$/D', $lastLabel) === 1
            || preg_match('/[\x00-\x20#\/:<>?@\[\]\\\\^|%]/D', $host) === 1
        ) {
            return null;
        }

        return $host;
    }

    private static function canonicalIpv6(string $packed): ?string
    {
        $segments = unpack('n8', $packed);
        if (!is_array($segments) || count($segments) !== 8) {
            return null;
        }
        $longestStart = -1;
        $longestLength = 0;
        $currentStart = -1;
        for ($index = 0; $index <= 8; $index++) {
            if ($index < 8 && $segments[$index + 1] === 0) {
                if ($currentStart < 0) {
                    $currentStart = $index;
                }
                continue;
            }
            if ($currentStart >= 0) {
                $length = $index - $currentStart;
                if ($length > $longestLength) {
                    $longestStart = $currentStart;
                    $longestLength = $length;
                }
                $currentStart = -1;
            }
        }
        if ($longestLength < 2) {
            $longestStart = -1;
        }

        $canonical = '';
        for ($index = 0; $index < 8; $index++) {
            if ($index === $longestStart) {
                $canonical .= $index === 0 ? '::' : ':';
                $index += $longestLength - 1;
                if ($index === 7) {
                    break;
                }
                continue;
            }
            $canonical .= dechex($segments[$index + 1]);
            if ($index < 7) {
                $canonical .= ':';
            }
        }

        return $canonical;
    }

    private static function canonicalPercentEncoding(string $value): bool
    {
        for ($index = 0, $length = strlen($value); $index < $length; $index++) {
            if ($value[$index] !== '%') {
                continue;
            }
            $pair = substr($value, $index + 1, 2);
            if (strlen($pair) !== 2 || preg_match('/^[0-9A-F]{2}$/D', $pair) !== 1) {
                return false;
            }
            $decoded = chr((int) hexdec($pair));
            if (preg_match('/^[A-Za-z0-9._~-]$/D', $decoded) === 1) {
                return false;
            }
            $index += 2;
        }

        return true;
    }

    private static function decodedPathHasNoDotSegments(string $path): bool
    {
        foreach (explode('/', rawurldecode($path)) as $segment) {
            if ($segment === '.' || $segment === '..') {
                return false;
            }
        }

        return true;
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

    private static function nonnegativeInteger(mixed $value, string $path, int $maximum): void
    {
        if (!is_int($value) || $value < 0 || $value > $maximum) {
            throw new UnexpectedValueException("{$path} is outside its integer range");
        }
    }

    private static function signedInteger(mixed $value, string $path): void
    {
        if (!is_int($value) || $value < -2_147_483_648 || $value > 2_147_483_647) {
            throw new UnexpectedValueException("{$path} is outside its integer range");
        }
    }

    private static function unsignedInteger(mixed $value, string $path): void
    {
        self::nonnegativeInteger($value, $path, 4_294_967_295);
    }

    private static function assertLiteral(mixed $actual, mixed $expected, string $path): void
    {
        if ($actual !== $expected) {
            throw new UnexpectedValueException("{$path} has an unsupported value or type");
        }
    }
}
