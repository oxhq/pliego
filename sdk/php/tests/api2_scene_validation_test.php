<?php

declare(strict_types=1);

use Pliego\Php\Internal\Api2ResultValidator;
use Pliego\Php\Internal\CanonicalJson;

require dirname(__DIR__).'/vendor/autoload.php';

function api2SceneExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

/** @param array<string, mixed> $value */
function api2SceneJson(array $value): string
{
    return json_encode(
        $value,
        JSON_UNESCAPED_SLASHES
            | JSON_UNESCAPED_UNICODE
            | JSON_UNESCAPED_LINE_TERMINATORS
            | JSON_THROW_ON_ERROR,
    )."\n";
}

/** @return array{path: string, media_type: string, sha256: string, bytes: int} */
function api2SceneDescriptor(string $root, string $path, string $mediaType): array
{
    $file = $root.DIRECTORY_SEPARATOR.str_replace('/', DIRECTORY_SEPARATOR, $path);
    $bytes = filesize($file);
    $hash = hash_file('sha256', $file);
    if (!is_int($bytes) || !is_string($hash)) {
        throw new RuntimeException("cannot describe {$file}");
    }

    return [
        'path' => $path,
        'media_type' => $mediaType,
        'sha256' => 'sha256:'.$hash,
        'bytes' => $bytes,
    ];
}

/**
 * @param array<string, mixed>|null $page
 * @return array{
 *   scene: array<string, mixed>,
 *   resources: array<string, array{bytes: string, media_type: string}>,
 *   page: array<string, mixed>
 * }
 */
function api2SceneFixture(?array $page = null): array
{
    $page ??= [
        'size' => ['name' => 'A4'],
        'margins_app_units' => ['top' => 2_880, 'right' => 2_880, 'bottom' => 2_880, 'left' => 2_880],
        'geometry_authority' => 'request-only-v1',
    ];
    $size = isset($page['size']['name'])
        ? ['width' => 47_622, 'height' => 67_351]
        : [
            'width' => $page['size']['width_app_units'],
            'height' => $page['size']['height_app_units'],
        ];
    $fontBytes = "OTTO\0strict-scene-font";
    $imageBytes = "\x89PNG\r\n\x1a\nstrict-image";
    $font = 'sha256:'.hash('sha256', $fontBytes);
    $image = 'sha256:'.hash('sha256', $imageBytes);

    return [
        'scene' => [
            'schema' => 'pliego.document-scene',
            'version' => 2,
            'app_units_per_css_px' => 60,
            'request_page' => $page,
            'semantic_layer' => null,
            'pages' => [[
                'number' => 1,
                'style_source' => 'request-defaults',
                'size_app_units' => $size,
                'margins_app_units' => $page['margins_app_units'],
                'operations' => [[
                    'type' => 'text',
                    'text' => 'Aé',
                    'font' => [
                        'resource' => $font,
                        'face_index' => 4_294_967_295,
                        'variations' => [
                            ['tag' => 100, 'value_f32_bits' => 0],
                            ['tag' => 200, 'value_f32_bits' => 1_065_353_216],
                        ],
                        'synthetic_bold' => true,
                    ],
                    'font_size_app_units' => 720,
                    'color' => ['r' => 1, 'g' => 2, 'b' => 3, 'a' => 255],
                    'glyphs' => [
                        [
                            'id' => 4_294_967_295,
                            'x' => -2_147_483_648,
                            'y' => 2_147_483_647,
                            'advance' => -1,
                            'text_range' => ['start' => 0, 'end' => 1],
                        ],
                        [
                            'id' => 43,
                            'x' => 3_240,
                            'y' => 4_320,
                            'advance' => 360,
                            'text_range' => ['start' => 1, 'end' => 3],
                        ],
                    ],
                ], [
                    'type' => 'path',
                    'bounds' => ['x' => -2_147_483_648, 'y' => 2_147_483_647, 'width' => 0, 'height' => 0],
                    'data' => 'M -2147483648 2147483647 Q -10 0 10 20 C 1 2 3 4 5 6 Z',
                    'fill' => ['r' => 64, 'g' => 64, 'b' => 64, 'a' => 255],
                    'fill_rule' => 'non-zero',
                    'stroke' => [
                        'color' => ['r' => 5, 'g' => 6, 'b' => 7, 'a' => 8],
                        'width_app_units' => 1,
                    ],
                ], [
                    'type' => 'image',
                    'bounds' => ['x' => -1, 'y' => -2, 'width' => 0, 'height' => 0],
                    'resource' => $image,
                    'media_type' => 'image/png',
                ], [
                    'type' => 'link',
                    'bounds' => ['x' => 2_880, 'y' => 12_000, 'width' => 7_200, 'height' => 1_080],
                    'target' => 'https://example.test/invoices/42?view=full#details',
                ]],
            ]],
        ],
        'resources' => [
            $font => ['bytes' => $fontBytes, 'media_type' => 'application/octet-stream'],
            $image => ['bytes' => $imageBytes, 'media_type' => 'image/png'],
        ],
        'page' => $page,
    ];
}

/**
 * @param array<string, mixed> $scene
 * @param array<string, array{bytes: string, media_type: string}> $resources
 * @param array<string, mixed> $page
 * @return array{validator: Api2ResultValidator, stdout: string, root: string}
 */
function api2SceneTransaction(array $scene, array $resources, array $page): array
{
    $root = sys_get_temp_dir().'/pliego-php-scene-'.getmypid().'-'.bin2hex(random_bytes(6));
    if (!mkdir($root, 0700) || !mkdir($root.DIRECTORY_SEPARATOR.'input', 0700)) {
        throw new RuntimeException("cannot create {$root}");
    }
    file_put_contents($root.DIRECTORY_SEPARATOR.'input'.DIRECTORY_SEPARATOR.'document.html', '<p>scene</p>');
    $inputEntry = api2SceneDescriptor(
        $root.DIRECTORY_SEPARATOR.'input',
        'document.html',
        'text/html;charset=utf-8',
    );
    file_put_contents($root.DIRECTORY_SEPARATOR.'input-manifest.json', api2SceneJson([
        'schema' => 'pliego.input-manifest',
        'version' => 1,
        'url_root' => 'pliego-input:///',
        'entries' => [$inputEntry],
    ]));
    $manifest = api2SceneDescriptor(
        $root,
        'input-manifest.json',
        'application/vnd.pliego.input-manifest+json',
    );
    $request = [
        'schema' => 'pliego.render-request',
        'version' => 1,
        'api' => 2,
        'profile' => null,
        'input' => ['entrypoint' => 'document.html', 'manifest' => $manifest],
        'environment' => ['locale' => 'en-US', 'timezone' => 'UTC'],
        'page' => $page,
        'resources' => ['network' => 'deny', 'host_fonts' => 'deny'],
        'time' => [
            'policy_version' => 1,
            'epoch_unix_ms' => 946_684_800_000,
            'initial_offset_ns' => 0,
        ],
        'settlement' => [
            'policy_version' => 1,
            'infinite_source_policy' => 'fail',
            'empty_checkpoints' => 2,
            'limits' => [
                'virtual_span_ms' => 86_400_000,
                'ordinary_tasks' => 100_000,
                'microtasks' => 1_000_000,
                'rendering_opportunities' => 10_000,
                'mutations' => 1_000_000,
                'host_wall_ms' => 60_000,
            ],
        ],
        'diagnostics' => ['retention' => 'none'],
    ];
    $engine = [
        'name' => 'pliego',
        'version' => '0.3.3',
        'api' => 2,
        'source_commit' => str_repeat('1', 40),
        'runtime' => [
            'mode' => 'one-shot',
            'target' => 'x86_64-unknown-linux-gnu',
            'binary_sha256' => 'sha256:'.str_repeat('2', 64),
            'servo_base' => str_repeat('3', 40),
        ],
    ];

    $deliveryRoot = $root.DIRECTORY_SEPARATOR.'delivery';
    mkdir($deliveryRoot, 0700);
    file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'document.pdf', "%PDF-1.7\n% scene fixture\n");
    file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'scene.json', api2SceneJson($scene));
    if ($resources !== []) {
        mkdir($deliveryRoot.DIRECTORY_SEPARATOR.'resources', 0700);
    }
    $entries = [
        api2SceneDescriptor($deliveryRoot, 'document.pdf', 'application/pdf'),
        api2SceneDescriptor(
            $deliveryRoot,
            'scene.json',
            'application/vnd.pliego.document-scene+json',
        ),
    ];
    foreach ($resources as $address => $resource) {
        $name = substr($address, strlen('sha256:'));
        file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'resources'.DIRECTORY_SEPARATOR.$name, $resource['bytes']);
        $entries[] = api2SceneDescriptor($deliveryRoot, 'resources/'.$name, $resource['media_type']);
    }
    usort(
        $entries,
        static fn (array $left, array $right): int => strcmp($left['path'], $right['path']),
    );
    file_put_contents($deliveryRoot.DIRECTORY_SEPARATOR.'bundle.json', api2SceneJson([
        'schema' => 'pliego.bundle-manifest',
        'version' => 1,
        'entries' => $entries,
    ]));
    $pdf = api2SceneDescriptor($deliveryRoot, 'document.pdf', 'application/pdf');
    $sceneDescriptor = api2SceneDescriptor(
        $deliveryRoot,
        'scene.json',
        'application/vnd.pliego.document-scene+json',
    );
    $bundle = api2SceneDescriptor(
        $deliveryRoot,
        'bundle.json',
        'application/vnd.pliego.bundle-manifest+json',
    );
    $result = [
        'schema' => 'pliego.render-result',
        'version' => 1,
        'api' => 2,
        'status' => 'success',
        'request' => $request,
        'engine' => $engine,
        'delivery' => ['pdf' => $pdf, 'scene' => $sceneDescriptor, 'bundle' => $bundle],
        'conformance' => ['requested' => null, 'status' => 'not-requested', 'evidence' => null],
        'diagnostics' => ['retained' => false, 'artifacts' => []],
        'error' => null,
    ];

    return [
        'validator' => new Api2ResultValidator($root, $request, $engine),
        'stdout' => api2SceneJson($result),
        'root' => $root,
    ];
}

function api2SceneRemoveTree(string $root): void
{
    $realRoot = realpath($root);
    $realTemp = realpath(sys_get_temp_dir());
    if (!is_string($realRoot) || !is_string($realTemp) || !str_starts_with($realRoot, $realTemp.DIRECTORY_SEPARATOR)) {
        throw new RuntimeException("refusing to remove unexpected test root {$root}");
    }
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($realRoot, FilesystemIterator::SKIP_DOTS),
        RecursiveIteratorIterator::CHILD_FIRST,
    );
    foreach ($iterator as $entry) {
        $entry->isDir() ? rmdir($entry->getPathname()) : unlink($entry->getPathname());
    }
    rmdir($realRoot);
}

/** @param Closure(array<string, mixed>&, array<string, array{bytes: string, media_type: string}>&): void $mutate */
function api2SceneExpectRejected(string $name, Closure $mutate, string $message): void
{
    $fixture = api2SceneFixture();
    $mutate($fixture['scene'], $fixture['resources']);
    $transaction = api2SceneTransaction($fixture['scene'], $fixture['resources'], $fixture['page']);
    try {
        $transaction['validator']->validate($transaction['stdout'], 0);
        throw new RuntimeException("expected scene rejection for {$name}");
    } catch (UnexpectedValueException $error) {
        api2SceneExpect(
            str_contains($error->getMessage(), $message),
            "{$name} produced an unexpected rejection: {$error->getMessage()}",
        );
    } finally {
        api2SceneRemoveTree($transaction['root']);
    }
}

$lineSeparator = "\xE2\x80\xA8";
$rawUtf8Value = "café 東京{$lineSeparator}fin";
$rawUtf8Frame = '{"value":"'.$rawUtf8Value.'"}'."\n";
$decodedRawUtf8 = CanonicalJson::decodeFrame($rawUtf8Frame, 'raw UTF-8 fixture');
api2SceneExpect($decodedRawUtf8['value'] === $rawUtf8Value, 'raw UTF-8 and U+2028 survive decoding');
api2SceneExpect(
    CanonicalJson::encodeFrame(['value' => $rawUtf8Value], 'raw UTF-8 fixture') === $rawUtf8Frame,
    'canonical encoding matches native raw UTF-8 and U+2028 bytes',
);
api2SceneExpect(
    api2SceneJson(['value' => $rawUtf8Value]) === $rawUtf8Frame,
    'scene fixture helper preserves native raw UTF-8 and U+2028 bytes',
);
foreach ([
    'escaped Unicode' => '{"value":"caf\u00e9"}'."\n",
    'escaped U+2028' => '{"value":"before\u2028after"}'."\n",
    'escaped slash' => '{"value":"https:\/\/example.test\/"}'."\n",
] as $name => $frame) {
    try {
        CanonicalJson::decodeFrame($frame, $name);
        throw new RuntimeException("expected noncanonical {$name} rejection");
    } catch (UnexpectedValueException $error) {
        api2SceneExpect(
            str_contains($error->getMessage(), 'canonical compact JSON'),
            "{$name} produced an unexpected rejection: {$error->getMessage()}",
        );
    }
}

$valid = api2SceneFixture();
$transaction = api2SceneTransaction($valid['scene'], $valid['resources'], $valid['page']);
try {
    $validated = $transaction['validator']->validate($transaction['stdout'], 0);
    api2SceneExpect($validated['status'] === 'success', 'strict all-operation scene is accepted');
} finally {
    api2SceneRemoveTree($transaction['root']);
}

$explicitPage = [
    'size' => ['width_app_units' => 48_960, 'height_app_units' => 63_360],
    'margins_app_units' => ['top' => 60, 'right' => 120, 'bottom' => 180, 'left' => 240],
    'geometry_authority' => 'request-only-v1',
];
$valid = api2SceneFixture($explicitPage);
$transaction = api2SceneTransaction($valid['scene'], $valid['resources'], $valid['page']);
try {
    $transaction['validator']->validate($transaction['stdout'], 0);
} finally {
    api2SceneRemoveTree($transaction['root']);
}

$valid = api2SceneFixture();
unset($valid['scene']['pages'][0]['operations'][1]['fill']);
$transaction = api2SceneTransaction($valid['scene'], $valid['resources'], $valid['page']);
try {
    $transaction['validator']->validate($transaction['stdout'], 0);
} finally {
    api2SceneRemoveTree($transaction['root']);
}

$valid = api2SceneFixture();
$link = $valid['scene']['pages'][0]['operations'][3];
foreach ([
    'https://example.test:8443/path',
    'https://example.test/a%2Fb',
    'https://example.test/%7B%7D/',
    'https://example.test/^|',
    'https://example.test/path?{query}',
    'https://example.test/path?`query',
    'https://example.test/path#{fragment}',
    'https://127.0.0.1/',
    'https://exa{mple.test/',
    'https://[::1]/',
    'https://[abcd::1]/',
    'https://[::ffff:c000:280]/',
    'https://[::]/',
    'https://[1:0:2:3:4:5:6:7]/',
    'https://[1::2:0:0:3:4]/',
    'https://[1:0:0:2::3]/',
    'https://[1:2:3:4:5:6::]/',
    'mailto:user@example.test',
    "mailto:user@example.test?subject='",
    'mailto:user@example.test?subject={',
    'mailto:user@example.test?subject=`',
] as $target) {
    $operation = $link;
    $operation['target'] = $target;
    $valid['scene']['pages'][0]['operations'][] = $operation;
}
$transaction = api2SceneTransaction($valid['scene'], $valid['resources'], $valid['page']);
try {
    $transaction['validator']->validate($transaction['stdout'], 0);
} finally {
    api2SceneRemoveTree($transaction['root']);
}

$cases = [
    'page-number' => [
        static function (array &$scene): void { $scene['pages'][0]['number'] = 2; },
        '.number has an unsupported value',
    ],
    'page-size' => [
        static function (array &$scene): void { $scene['pages'][0]['size_app_units']['width']++; },
        'size_app_units contradicts the request',
    ],
    'unknown-operation-member' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][0]['debug'] = true; },
        'unsupported or out-of-order members',
    ],
    'unknown-operation' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][0] = ['type' => 'video']; },
        '.type is unsupported',
    ],
    'empty-text' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][0]['text'] = ''; },
        'non-empty UTF-8 string',
    ],
    'font-face-range' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][0]['font']['face_index'] = -1; },
        'face_index is outside its integer range',
    ],
    'font-variation-order' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][0]['font']['variations'][1]['tag'] = 100;
        },
        'variation tags must be strictly ascending',
    ],
    'font-variation-negative-zero' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][0]['font']['variations'][0]['value_f32_bits'] = 2_147_483_648;
        },
        'not canonical finite binary32',
    ],
    'font-synthetic-type' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][0]['font']['synthetic_bold'] = 1;
        },
        'synthetic_bold must be boolean',
    ],
    'glyph-u32-overflow' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][0]['glyphs'][0]['id'] = 4_294_967_296;
        },
        '.id is outside its integer range',
    ],
    'glyph-i32-overflow' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][0]['glyphs'][0]['x'] = -2_147_483_649;
        },
        '.x is outside its integer range',
    ],
    'glyph-utf8-boundary' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][0]['glyphs'][1]['text_range'] = ['start' => 2, 'end' => 3];
        },
        'not on UTF-8 boundaries',
    ],
    'color-range' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][0]['color']['r'] = 256; },
        '.r is outside its integer range',
    ],
    'path-leading-zero' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][1]['data'] = 'M 01 0'; },
        'noncanonical path coordinate',
    ],
    'path-coordinate-i32-overflow' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][1]['data'] = 'M 2147483648 0';
        },
        'coordinate outside signed i32',
    ],
    'path-truncated-curve' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][1]['data'] = 'M 0 0 Q 1 2 3'; },
        'truncated path command',
    ],
    'path-no-paint' => [
        static function (array &$scene): void {
            unset($scene['pages'][0]['operations'][1]['fill']);
            unset($scene['pages'][0]['operations'][1]['stroke']);
        },
        'requires fill, stroke, or both',
    ],
    'path-zero-stroke-width' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][1]['stroke']['width_app_units'] = 0;
        },
        'width_app_units is outside its integer range',
    ],
    'rectangle-coordinate-i32-overflow' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][2]['bounds']['x'] = -2_147_483_649;
        },
        '.x is outside its integer range',
    ],
    'rectangle-negative-width' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][2]['bounds']['width'] = -1; },
        '.width is outside its integer range',
    ],
    'image-media-type' => [
        static function (array &$scene): void { $scene['pages'][0]['operations'][2]['media_type'] = 'image/svg+xml'; },
        '.media_type is unsupported',
    ],
    'link-uppercase-host' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://EXAMPLE.test/invoices/42';
        },
        '.target is not canonical',
    ],
    'link-default-port' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test:443/invoices/42';
        },
        '.target is not canonical',
    ],
    'link-empty-port' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test:/';
        },
        '.target is not canonical',
    ],
    'link-zero-padded-port' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test:08443/';
        },
        '.target is not canonical',
    ],
    'link-out-of-range-port' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test:65536/';
        },
        '.target is not canonical',
    ],
    'link-raw-path-braces' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/{}/';
        },
        '.target is not canonical',
    ],
    'link-raw-path-backslash' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/\\foo';
        },
        '.target is not canonical',
    ],
    'link-authority-backslash' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test\\evil.test/path';
        },
        '.target is not canonical',
    ],
    'link-short-ipv4' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://127.1/';
        },
        '.target is not canonical',
    ],
    'link-hex-ipv4' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://0x7f000001/';
        },
        '.target is not canonical',
    ],
    'link-octal-ipv4' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://0177.0.0.1/';
        },
        '.target is not canonical',
    ],
    'link-bare-hex-prefix-host' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.0x/';
        },
        '.target is not canonical',
    ],
    'link-expanded-ipv6' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://[0:0:0:0:0:0:0:1]/';
        },
        '.target is not canonical',
    ],
    'link-uppercase-ipv6' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://[ABCD::1]/';
        },
        '.target is not canonical',
    ],
    'link-invalid-ipv6' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://[gggg::1]/';
        },
        '.target is not canonical',
    ],
    'link-dotted-ipv4-mapped-ipv6' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://[::ffff:192.0.2.128]/';
        },
        '.target is not canonical',
    ],
    'link-second-tied-ipv6-run' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://[1:0:0:2::3:4]/';
        },
        '.target is not canonical',
    ],
    'link-shorter-ipv6-run' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://[1::2:0:0:0:3]/';
        },
        '.target is not canonical',
    ],
    'link-compressed-single-ipv6-zero' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://[1::2:3:4:5:6:7]/';
        },
        '.target is not canonical',
    ],
    'link-raw-path-backtick' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/`';
        },
        '.target is not canonical',
    ],
    'link-raw-path-quote' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/"';
        },
        '.target is not canonical',
    ],
    'link-raw-path-angle' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/<>/';
        },
        '.target is not canonical',
    ],
    'link-raw-query-apostrophe' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = "https://example.test/path?'x";
        },
        '.target is not canonical',
    ],
    'link-raw-query-quote' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/path?"x';
        },
        '.target is not canonical',
    ],
    'link-raw-query-angle' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/path?<x>';
        },
        '.target is not canonical',
    ],
    'link-raw-fragment-backtick' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/path#`x';
        },
        '.target is not canonical',
    ],
    'link-raw-fragment-quote' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/path#"x';
        },
        '.target is not canonical',
    ],
    'link-raw-fragment-angle' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/path#<x>';
        },
        '.target is not canonical',
    ],
    'link-dot-segment' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/a/../b';
        },
        '.target is not canonical',
    ],
    'link-mailto-query-quote' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'mailto:user@example.test?subject="';
        },
        '.target is not canonical',
    ],
    'link-mailto-query-left-angle' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'mailto:user@example.test?subject=<';
        },
        '.target is not canonical',
    ],
    'link-mailto-query-right-angle' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'mailto:user@example.test?subject=>';
        },
        '.target is not canonical',
    ],
    'link-encoded-unreserved' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][3]['target'] = 'https://example.test/%41';
        },
        '.target is not canonical',
    ],
    'missing-resource' => [
        static function (array &$scene, array &$resources): void {
            unset($resources[$scene['pages'][0]['operations'][2]['resource']]);
        },
        'do not exactly close document scene references',
    ],
    'extra-resource' => [
        static function (array &$scene, array &$resources): void {
            $bytes = 'unreferenced';
            $resources['sha256:'.hash('sha256', $bytes)] = [
                'bytes' => $bytes,
                'media_type' => 'application/octet-stream',
            ];
        },
        'do not exactly close document scene references',
    ],
    'resource-media-identity' => [
        static function (array &$scene, array &$resources): void {
            $resources[$scene['pages'][0]['operations'][2]['resource']]['media_type'] = 'image/jpeg';
        },
        'media type contradicts its document scene use',
    ],
    'resource-image-signature' => [
        static function (array &$scene, array &$resources): void {
            $old = $scene['pages'][0]['operations'][2]['resource'];
            unset($resources[$old]);
            $bytes = 'not a PNG';
            $replacement = 'sha256:'.hash('sha256', $bytes);
            $scene['pages'][0]['operations'][2]['resource'] = $replacement;
            $resources[$replacement] = ['bytes' => $bytes, 'media_type' => 'image/png'];
        },
        'bytes contradict image/png',
    ],
    'resource-conflicting-use' => [
        static function (array &$scene): void {
            $scene['pages'][0]['operations'][2]['resource'] =
                $scene['pages'][0]['operations'][0]['font']['resource'];
        },
        'conflicting media identities',
    ],
];

foreach ($cases as $name => [$mutate, $message]) {
    api2SceneExpectRejected($name, $mutate, $message);
}

echo 'Pliego PHP strict API 2 DocumentScene validation self-test passed ('.count($cases)." adversarial cases)\n";
