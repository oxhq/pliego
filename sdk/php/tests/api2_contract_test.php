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
        'document_scene' => ['schema' => 'pliego.document-scene', 'version' => 2],
        'bundle_manifest' => ['schema' => 'pliego.bundle-manifest', 'version' => 1],
    ];
}

/** @param array<string, mixed> $document */
function api2ProbeFrame(array $document): string
{
    return json_encode($document, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n";
}

$command = [PHP_BINARY, __DIR__.'/fake_pliego.php'];
$probeParameters = (new ReflectionMethod(RuntimeContract::class, 'probe'))->getParameters();
api2Expect(
    count($probeParameters) === 2
        && $probeParameters[1]->isDefaultValueAvailable()
        && $probeParameters[1]->getDefaultValue() === 180,
    'the public probe default retains the bounded debug-binary identity budget',
);
$validateCommand = new ReflectionMethod(RuntimeContract::class, 'validateCommand');
$batchTargets = [
    'C:\\tools\\pliego.cmd',
    'C:\\tools\\PLIEGO.BAT',
    'C:\\tools\\pliego.cmd ',
    'C:\\tools\\pliego.bat.',
];
foreach ($batchTargets as $batchTarget) {
    if (PHP_OS_FAMILY !== 'Windows') {
        $validateCommand->invoke(null, [$batchTarget], 1);

        continue;
    }

    try {
        RuntimeContract::probe([$batchTarget], 1);
        throw new RuntimeException("expected Windows batch target rejection: {$batchTarget}");
    } catch (InvalidArgumentException $error) {
        api2Expect(
            $error->getMessage()
                === 'command must use a native executable on Windows; .bat and .cmd targets are not supported',
            'Windows batch target rejection is explicit and stable',
        );
    }
}

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

putenv('PLIEGO_API2_FAKE_MODE=slow-probe');
$slowFoundation = RuntimeContract::probe($command);
api2Expect(
    !$slowFoundation->api2Available(),
    'the bounded probe budget permits exact executable identity work',
);
try {
    RuntimeContract::probe($command, timeoutSeconds: 1);
    throw new RuntimeException('expected the explicit probe deadline');
} catch (RuntimeException $error) {
    api2Expect(
        str_contains($error->getMessage(), 'contract-probe exceeded 1 seconds'),
        'an explicit probe deadline remains bounded and actionable',
    );
}

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
putenv('PLIEGO_API2_FAKE_MODE=exit-64');
api2RejectedWith(
    fn () => RuntimeContract::probe($command),
    'received exit 64; stderr="invalid probe invocation\\n" (25 bytes)',
    'nonzero probe diagnostics are safely exposed',
);
putenv('PLIEGO_API2_FAKE_MODE=stderr');
api2RejectedWith(
    fn () => RuntimeContract::probe($command),
    'leave stderr empty; stderr="unexpected diagnostic\\n" (22 bytes)',
    'dirty successful-probe stderr is safely exposed',
);
putenv('PLIEGO_API2_FAKE_MODE=adversarial-stderr');
try {
    RuntimeContract::probe($command);
    throw new RuntimeException('expected adversarial probe stderr rejection');
} catch (UnexpectedValueException $error) {
    $message = $error->getMessage();
    api2Expect(str_contains($message, 'received exit 65'), 'adversarial stderr retains the exit code');
    api2Expect(
        str_contains($message, 'stderr="first\\r\\nsecond\\xFF'),
        'multiline and invalid UTF-8 stderr bytes are escaped',
    );
    api2Expect(
        str_contains($message, '314 bytes total; preview truncated to 256 bytes'),
        'oversized stderr reports its bounded preview and total byte count',
    );
    api2Expect(!str_contains($message, "\r") && !str_contains($message, "\n"), 'stderr cannot inject lines');
    api2Expect(preg_match('//u', $message) === 1, 'escaped stderr leaves a valid UTF-8 exception message');
    api2Expect(strlen($message) < 1_100, 'escaped stderr exception text remains bounded');
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
$legacyScene = $valid;
$legacyScene['contracts'][0]['document_scene']['version'] = 1;
$mutations['legacy document scene tuple'] = $legacyScene;
$badProfile = $valid;
$badProfile['contracts'][0]['profiles'][0]['schema'] = 'pliego.profile.PDF-UA';
$mutations['profile reference'] = $badProfile;
$badInvocationOrder = $valid;
$badInvocationOrder['invocation'] = array_reverse($badInvocationOrder['invocation'], true);
$mutations['invocation member order'] = $badInvocationOrder;
$badFrameLimit = $valid;
$badFrameLimit['invocation']['request_max_bytes'] = 1_048_575;
$mutations['request frame limit'] = $badFrameLimit;
$badJobTransport = $valid;
$badJobTransport['invocation']['job_root_transport'] = 'argument-v1';
$mutations['job root transport'] = $badJobTransport;
$badManifestLimit = $valid;
$badManifestLimit['invocation']['input_manifest_max_bytes']--;
$mutations['input manifest limit'] = $badManifestLimit;
$badContentLimit = $valid;
$badContentLimit['invocation']['input_content_max_bytes']--;
$mutations['input content limit'] = $badContentLimit;
$badTransportExit = $valid;
$badTransportExit['invocation']['transport_error_exit_code'] = 75;
$mutations['transport error exit code'] = $badTransportExit;
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
    [64, '', "escape sequence \x1B[31m\n"],
    [64, '', "delete byte \x7F\n"],
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
    api2Expect($realRuntime->api2Available(), 'real executable advertises API 2');
    api2Expect(
        $realRuntime->select(api2Protocol()) !== null,
        'real executable advertises the exact SDK API 2 tuple',
    );
    api2Expect(
        $realRuntime->invocation() === [
            'request_transport' => 'stdin-single-json',
            'request_max_bytes' => 1_048_576,
            'job_root_transport' => 'cwd-v1',
            'input_manifest_max_bytes' => 16_777_216,
            'input_content_max_bytes' => 67_108_864,
            'result_transport' => 'stdout-single-json',
            'invocation_error_transport' => 'stderr-utf8-line',
            'transport_error_transport' => 'stderr-utf8-line',
            'success_exit_code' => 0,
            'failed_exit_code' => 1,
            'invocation_error_exit_code' => 64,
            'transport_error_exit_code' => 74,
        ],
        'real executable foundation advertises the exact API 2 invocation transport',
    );
}

echo 'Pliego PHP API 2 contract self-test passed'
    .($binary === null ? '' : ' with real executable probe')."\n";
