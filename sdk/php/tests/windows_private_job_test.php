<?php

declare(strict_types=1);

use Pliego\Php\Internal\Api2InputJob;

require dirname(__DIR__).'/vendor/autoload.php';

function privateJobExpect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function privateJobUtf16Le(string $ascii): string
{
    $encoded = "\xFF\xFE";
    for ($index = 0, $length = strlen($ascii); $index < $length; $index++) {
        $encoded .= $ascii[$index]."\0";
    }

    return $encoded;
}

function privateJobExpectFailure(callable $operation, string $diagnostic): void
{
    try {
        $operation();
    } catch (RuntimeException $error) {
        privateJobExpect(
            str_contains($error->getMessage(), $diagnostic),
            "private job-root rejection is not actionable: {$error->getMessage()}",
        );

        return;
    }

    throw new RuntimeException("expected private job-root rejection: {$diagnostic}");
}

$sid = 'S-1-5-21-4080267330-3575100508-2019971957-1001';
$parseSid = new ReflectionMethod(Api2InputJob::class, 'windowsUserSid');
privateJobExpect(
    $parseSid->invoke(null, "\"H4X\\user\",\"{$sid}\"\r\n") === $sid,
    'whoami SID parsing lost the current user',
);
privateJobExpectFailure(
    static fn (): mixed => $parseSid->invoke(null, 'no SID'),
    'exactly one current-user SID',
);
privateJobExpectFailure(
    static fn (): mixed => $parseSid->invoke(null, "{$sid}\r\nS-1-5-18\r\n"),
    'exactly one current-user SID',
);
$tokenHasSid = new ReflectionMethod(Api2InputJob::class, 'windowsTokenHasSid');
privateJobExpect(
    $tokenHasSid->invoke(null, "Local account,S-1-5-113\r\n", 'S-1-5-113') === true,
    'local-account token SID was not recognized',
);
privateJobExpect(
    $tokenHasSid->invoke(null, "Domain Users,S-1-5-21-1-2-3-513\r\n", 'S-1-5-113') === false,
    'domain-only token was misclassified as a local account',
);

$validateDacl = new ReflectionMethod(Api2InputJob::class, 'validateWindowsOwnerOnlyDacl');
$validateDacl->invoke(
    null,
    privateJobUtf16Le("fixture\r\nD:PAI(A;OICI;FA;;;{$sid})\r\n"),
    $sid,
);
$validateDacl->invoke(
    null,
    privateJobUtf16Le("fixture\r\nD:PAI(A;OICI;FA;;;SY)\r\n"),
    'S-1-5-18',
);
$administratorSid = 'S-1-5-21-51256336-3298027356-2228789493-500';
$validateDacl->invoke(
    null,
    privateJobUtf16Le("fixture\r\nD:PAI(A;OICI;FA;;;LA)\r\n"),
    $administratorSid,
    true,
);
foreach ([
    "D:AI(A;OICI;FA;;;{$sid})",
    "D:PAI(A;OICI;FA;;;{$sid})(A;OICI;FA;;;S-1-5-18)",
    "D:PAI(A;OI;FA;;;{$sid})",
    'D:PAI(A;OICI;FA;;;S-1-5-18)',
    'D:PAI(A;OICI;FA;;;LA)',
] as $descriptor) {
    privateJobExpectFailure(
        static fn (): mixed => $validateDacl->invoke(
            null,
            privateJobUtf16Le("fixture\r\n{$descriptor}\r\n"),
            $sid,
        ),
        'protected owner-only full-access DACL',
    );
}
privateJobExpectFailure(
    static fn (): mixed => $validateDacl->invoke(
        null,
        privateJobUtf16Le("fixture\r\nD:PAI(A;OICI;FA;;;LA)\r\n"),
        $administratorSid,
    ),
    'protected owner-only full-access DACL',
);
privateJobExpectFailure(
    static fn (): mixed => $validateDacl->invoke(null, "\xFF", $sid),
    'invalid or oversized ACL proof',
);

$resolveTool = new ReflectionMethod(Api2InputJob::class, 'resolveWindowsSystemTool');
privateJobExpectFailure(
    static fn (): mixed => $resolveTool->invoke(null, 'cmd.exe'),
    'unsupported Windows ACL tool',
);
privateJobExpectFailure(
    static fn (): mixed => $resolveTool->invoke(null, 'whoami.exe', 'Z:\\pliego-missing-system-root'),
    'required Windows ACL tool is unavailable',
);

$outer = sys_get_temp_dir().DIRECTORY_SEPARATOR.'pliego-private-root-'.getmypid().'-'.bin2hex(random_bytes(4));
$runtimeRoot = $outer.DIRECTORY_SEPARATOR.'private & shell (literal)';
privateJobExpect(@mkdir($outer, 0700), 'cannot allocate private job-root test directory');
$createRoot = new ReflectionMethod(Api2InputJob::class, 'createPrivateRuntimeRoot');
try {
    $createRoot->invoke(null, $runtimeRoot);
    privateJobExpect(is_dir($runtimeRoot), 'private job root was not created');
    privateJobExpect(
        array_values(array_diff(scandir($runtimeRoot) ?: [], ['.', '..'])) === [],
        'private job-root setup introduced entries',
    );
    if (PHP_OS_FAMILY !== 'Windows') {
        $permissions = fileperms($runtimeRoot);
        privateJobExpect(
            is_int($permissions) && ($permissions & 0777) === 0700,
            'Unix private job root does not have exact mode 0700',
        );
    }
} finally {
    @rmdir($runtimeRoot);
    @rmdir($outer);
}

echo "Pliego PHP private API 2 job-root self-test passed\n";
