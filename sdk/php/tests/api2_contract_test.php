<?php

declare(strict_types=1);

use Pliego\Php\Exception\InvocationException;
use Pliego\Php\RuntimeContract;

require dirname(__DIR__).'/vendor/autoload.php';

function api2Expect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function api2Rejected(callable $operation, string $message): void
{
    try {
        $operation();
    } catch (UnexpectedValueException) {
        return;
    }

    throw new RuntimeException("expected rejection: {$message}");
}

function api2RejectedWith(callable $operation, string $expected, string $message): void
{
    try {
        $operation();
    } catch (UnexpectedValueException $error) {
        api2Expect(
            str_contains($error->getMessage(), $expected),
            "{$message} rejection must contain {$expected}",
        );

        return;
    }

    throw new RuntimeException("expected rejection: {$message}");
}

/** @return array<string, mixed> */
function api2Protocol(int $requestVersion = 1, int $resultVersion = 1): array
{
    return [
        'api' => 2,
        'input_manifest' => ['schema' => 'pliego.input-manifest', 'version' => 1],
        'request' => ['schema' => 'pliego.render-request', 'version' => $requestVersion],
        'result' => ['schema' => 'pliego.render-result', 'version' => $resultVersion],
        'document_scene' => ['schema' => 'pliego.document-scene', 'version' => 1],
        'bundle_manifest' => ['schema' => 'pliego.bundle-manifest', 'version' => 1],
    ];
}

/** @param array<string, mixed> $document */
function api2ProbeFrame(array $document): string
{
    return json_encode($document, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n";
}

$command = [PHP_BINARY, __DIR__.'/fake_pliego.php'];

putenv('PLIEGO_API2_FAKE_MODE=empty');
$foundation = RuntimeContract::probe($command);
api2Expect($foundation->engine()['api'] === 2, 'probe engine identity retains API 2');
api2Expect($foundation->contracts() === [], 'foundation truthfully advertises no API 2 tuples');
api2Expect(!$foundation->api2Available(), 'empty contracts means API 2 is unavailable');
api2Expect(
    $foundation->select(api2Protocol()) === null,
    'engine API and command presence do not imply an available tuple',
);
api2Expect(
    $foundation->invocation()['request_max_bytes'] === 1_048_576,
    'probe fixes the inclusive request frame limit',
);

putenv('PLIEGO_API2_FAKE_MODE=available');
$available = RuntimeContract::probe($command);
api2Expect($available->api2Available(), 'one complete tuple makes API 2 negotiable');
$selection = $available->select(api2Protocol());
api2Expect($selection !== null, 'the exact whole API 2 tuple is selected');
api2Expect($selection['profile'] === null, 'a profile is never inferred');
api2Expect(
    $available->select(api2Protocol(requestVersion: 2)) === null,
    'independently recognized schema members are not cross-paired',
);
api2Rejected(
    fn () => $available->select([
        'api' => 2,
        'request' => ['schema' => 'pliego.render-request', 'version' => 1],
    ]),
    'partial tuple selection',
);

putenv('PLIEGO_API2_FAKE_MODE=profile');
$profileRuntime = RuntimeContract::probe($command);
$implicitProfile = $profileRuntime->select(api2Protocol());
api2Expect($implicitProfile !== null && $implicitProfile['profile'] === null, 'profile remains opt-in');
$profile = ['schema' => 'pliego.profile.test', 'version' => 1];
$profileSelection = $profileRuntime->select(api2Protocol(), $profile);
api2Expect($profileSelection !== null && $profileSelection['profile'] === $profile, 'exact profile is selected');
api2Expect(
    $profileRuntime->select(
        api2Protocol(),
        ['schema' => 'pliego.profile.unadvertised', 'version' => 1],
    ) === null,
    'an unadvertised profile is not inferred from the protocol tuple',
);

foreach (['out-of-order', 'unknown-member', 'stderr', 'second-frame', 'exit-64'] as $mode) {
    putenv("PLIEGO_API2_FAKE_MODE={$mode}");
    api2Rejected(fn () => RuntimeContract::probe($command), "invalid probe mode {$mode}");
}

$valid = $profileRuntime->toArray();
$wrongType = $valid;
$wrongType['version'] = '1';
api2Rejected(
    fn () => RuntimeContract::fromProbeResult(0, api2ProbeFrame($wrongType), ''),
    'schema version with the wrong JSON type',
);
$duplicateTuple = $valid;
$duplicateTuple['contracts'][] = $duplicateTuple['contracts'][0];
api2Rejected(
    fn () => RuntimeContract::fromProbeResult(0, api2ProbeFrame($duplicateTuple), ''),
    'duplicate complete tuple',
);
$duplicateProfile = $valid;
$duplicateProfile['contracts'][0]['profiles'][] = $duplicateProfile['contracts'][0]['profiles'][0];
api2Rejected(
    fn () => RuntimeContract::fromProbeResult(0, api2ProbeFrame($duplicateProfile), ''),
    'duplicate profile within one tuple',
);
$reversedProfiles = $valid;
$reversedProfiles['contracts'][0]['profiles'] = [
    ['schema' => 'pliego.profile.z', 'version' => 1],
    ['schema' => 'pliego.profile.a', 'version' => 1],
];
api2RejectedWith(
    fn () => RuntimeContract::fromProbeResult(0, api2ProbeFrame($reversedProfiles), ''),
    'profiles must be canonically ordered',
    'reversed profile references',
);
$orderedProfiles = $valid;
$orderedProfiles['contracts'][0]['profiles'] = [
    ['schema' => 'pliego.profile.a', 'version' => 1],
    ['schema' => 'pliego.profile.z', 'version' => 1],
];
api2Expect(
    RuntimeContract::fromProbeResult(0, api2ProbeFrame($orderedProfiles), '')->api2Available(),
    'ascending profile references remain valid',
);
$reversedProfileVersions = $valid;
$reversedProfileVersions['contracts'][0]['profiles'] = [
    ['schema' => 'pliego.profile.same', 'version' => 2],
    ['schema' => 'pliego.profile.same', 'version' => 1],
];
api2RejectedWith(
    fn () => RuntimeContract::fromProbeResult(0, api2ProbeFrame($reversedProfileVersions), ''),
    'profiles must be canonically ordered',
    'reversed versions of one profile schema',
);
$reversedContracts = $valid;
$firstContract = $reversedContracts['contracts'][0];
$firstContract['profiles'] = [['schema' => 'pliego.profile.z', 'version' => 1]];
$secondContract = $reversedContracts['contracts'][0];
$secondContract['profiles'] = [['schema' => 'pliego.profile.a', 'version' => 1]];
$reversedContracts['contracts'] = [$firstContract, $secondContract];
api2RejectedWith(
    fn () => RuntimeContract::fromProbeResult(0, api2ProbeFrame($reversedContracts), ''),
    'contract tuples must be canonically ordered',
    'reversed complete contract tuples',
);

$mutations = [];
$badEngineOrder = $valid;
$badEngineOrder['engine'] = array_reverse($badEngineOrder['engine'], true);
$mutations['engine member order'] = $badEngineOrder;
$badTarget = $valid;
$badTarget['engine']['runtime']['target'] = 'x86_64-UNKNOWN-linux-gnu';
$mutations['canonical target'] = $badTarget;
$badTupleOrder = $valid;
$badTupleOrder['contracts'][0] = array_reverse($badTupleOrder['contracts'][0], true);
$mutations['tuple member order'] = $badTupleOrder;
$badSchemaType = $valid;
$badSchemaType['contracts'][0]['request']['version'] = '1';
$mutations['nested schema version type'] = $badSchemaType;
$badProfile = $valid;
$badProfile['contracts'][0]['profiles'][0]['schema'] = 'pliego.profile.PDF-UA';
$mutations['profile reference'] = $badProfile;
$badInvocationOrder = $valid;
$badInvocationOrder['invocation'] = array_reverse($badInvocationOrder['invocation'], true);
$mutations['invocation member order'] = $badInvocationOrder;
$badFrameLimit = $valid;
$badFrameLimit['invocation']['request_max_bytes'] = 1_048_575;
$mutations['request frame limit'] = $badFrameLimit;
foreach ($mutations as $message => $mutation) {
    api2Rejected(
        fn () => RuntimeContract::fromProbeResult(0, api2ProbeFrame($mutation), ''),
        $message,
    );
}

$compact = substr(api2ProbeFrame($valid), 0, -1);
foreach ([
    $compact,
    $compact."\r\n",
    ' '.$compact."\n",
    $compact."\n{}\n",
] as $badFrame) {
    api2Rejected(
        fn () => RuntimeContract::fromProbeResult(0, $badFrame, ''),
        'noncanonical probe frame',
    );
}
$duplicateName = preg_replace(
    '/^\{"schema":/',
    '{"schema":"ignored","schema":',
    $compact,
    limit: 1,
);
api2Expect(is_string($duplicateName), 'duplicate-name adversarial frame is constructed');
api2Rejected(
    fn () => RuntimeContract::fromProbeResult(0, $duplicateName."\n", ''),
    'duplicate JSON object name',
);
foreach (['-0', '1.0', '1e0'] as $noncanonicalVersion) {
    $noncanonicalNumber = preg_replace(
        '/^\{"schema":"pliego\.runtime-contract","version":1,/',
        '{"schema":"pliego.runtime-contract","version":'.$noncanonicalVersion.',',
        $compact,
        limit: 1,
    );
    api2Expect(is_string($noncanonicalNumber), 'noncanonical-number adversarial frame is constructed');
    api2Rejected(
        fn () => RuntimeContract::fromProbeResult(0, $noncanonicalNumber."\n", ''),
        "noncanonical numeric spelling {$noncanonicalVersion}",
    );
}

$invocation = InvocationException::fromProcessResult(64, '', "invalid request frame\n");
api2Expect($invocation->exitCode === 64, 'invocation exception retains exit 64');
api2Expect($invocation->stdout === '', 'invocation exception retains empty stdout');
api2Expect($invocation->stderr === "invalid request frame\n", 'invocation exception retains diagnostic bytes');
api2Expect($invocation->getMessage() === 'invalid request frame', 'diagnostic line becomes the message');
foreach ([
    [1, '', "render failed\n"],
    [64, '{}\n', "invalid request\n"],
    [64, '', 'missing newline'],
    [64, '', "two\nlines\n"],
    [64, '', "carriage return\r\n"],
    [64, '', "invalid utf8 \xFF\n"],
] as [$exitCode, $stdout, $stderr]) {
    api2Rejected(
        fn () => InvocationException::fromProcessResult($exitCode, $stdout, $stderr),
        'noncanonical invocation error transport',
    );
}

putenv('PLIEGO_API2_FAKE_MODE');

$binary = $argv[1] ?? null;
if ($binary !== null) {
    api2Expect($binary !== '' && is_file($binary), 'optional Pliego binary path must name a file');
    $realRuntime = RuntimeContract::probe([$binary]);
    api2Expect($realRuntime->contracts() === [], 'real executable foundation advertises no API 2 tuples');
    api2Expect(!$realRuntime->api2Available(), 'real executable foundation keeps API 2 unavailable');
    api2Expect(
        $realRuntime->select(api2Protocol()) === null,
        'real executable foundation does not infer the accepted API 2 tuple',
    );
    api2Expect(
        $realRuntime->invocation() === [
            'request_transport' => 'stdin-single-json',
            'request_max_bytes' => 1_048_576,
            'result_transport' => 'stdout-single-json',
            'invocation_error_transport' => 'stderr-utf8-line',
            'success_exit_code' => 0,
            'failed_exit_code' => 1,
            'invocation_error_exit_code' => 64,
        ],
        'real executable foundation advertises the exact API 2 invocation transport',
    );
}

echo 'Pliego PHP API 2 contract foundation self-test passed'
    .($binary === null ? '' : ' with real executable probe')."\n";
