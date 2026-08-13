<?php

declare(strict_types=1);

namespace Pliego\Php;

use InvalidArgumentException;
use JsonException;
use RuntimeException;
use UnexpectedValueException;

/**
 * Strict decoder and exact-tuple selector for `pliego --contract-probe`.
 *
 * This class negotiates capability only. It does not make API 2 rendering
 * available and it does not alter the API 1 CliRenderer path.
 */
final readonly class RuntimeContract
{
    /**
     * The probe hashes the exact executable before responding. Keep this
     * host-side budget independent from render and doctor process deadlines.
     */
    private const DEFAULT_PROBE_TIMEOUT_SECONDS = 180;

    /** Keep child diagnostics useful without reflecting raw or unbounded bytes. */
    private const STDERR_DIAGNOSTIC_MAX_BYTES = 256;

    private const TOP_LEVEL_KEYS = ['schema', 'version', 'engine', 'contracts', 'invocation'];
    private const ENGINE_KEYS = ['name', 'version', 'api', 'source_commit', 'runtime'];
    private const RUNTIME_KEYS = ['mode', 'target', 'binary_sha256', 'servo_base'];
    private const CONTRACT_KEYS = [
        'api',
        'input_manifest',
        'request',
        'result',
        'document_scene',
        'bundle_manifest',
        'profiles',
    ];
    private const PROTOCOL_KEYS = [
        'api',
        'input_manifest',
        'request',
        'result',
        'document_scene',
        'bundle_manifest',
    ];
    private const SCHEMA_REFERENCE_KEYS = ['schema', 'version'];
    private const INVOCATION_KEYS = [
        'request_transport',
        'request_max_bytes',
        'result_transport',
        'invocation_error_transport',
        'success_exit_code',
        'failed_exit_code',
        'invocation_error_exit_code',
    ];

    /** @param array<string, mixed> $document */
    private function __construct(private array $document) {}

    /**
     * @param non-empty-list<string> $command
     */
    public static function probe(
        array $command,
        int $timeoutSeconds = self::DEFAULT_PROBE_TIMEOUT_SECONDS,
    ): self
    {
        self::validateCommand($command, $timeoutSeconds);
        [$exitCode, $stdout, $stderr] = self::execute(
            [...$command, '--contract-probe'],
            $timeoutSeconds,
        );

        return self::fromProbeResult($exitCode, $stdout, $stderr);
    }

    public static function fromProbeResult(int $exitCode, string $stdout, string $stderr): self
    {
        if ($exitCode !== 0) {
            throw new UnexpectedValueException(
                "Pliego contract probe must exit 0; received exit {$exitCode}; "
                    .self::formatStderrDiagnostic($stderr),
            );
        }
        if ($stderr !== '') {
            throw new UnexpectedValueException(
                'Pliego contract probe must leave stderr empty; '
                    .self::formatStderrDiagnostic($stderr),
            );
        }
        if (
            $stdout === ''
            || !str_ends_with($stdout, "\n")
            || str_contains(substr($stdout, 0, -1), "\n")
            || str_contains($stdout, "\r")
        ) {
            throw new UnexpectedValueException(
                'Pliego contract probe must write one compact JSON object followed by one LF',
            );
        }

        $json = substr($stdout, 0, -1);
        try {
            $document = json_decode($json, true, flags: JSON_THROW_ON_ERROR);
        } catch (JsonException $error) {
            throw new UnexpectedValueException(
                'Pliego contract probe returned invalid JSON',
                previous: $error,
            );
        }
        if (!is_array($document) || array_is_list($document)) {
            throw new UnexpectedValueException('Pliego contract probe must return a JSON object');
        }

        try {
            $canonical = json_encode($document, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        } catch (JsonException $error) {
            throw new UnexpectedValueException(
                'Pliego contract probe returned JSON that cannot be encoded',
                previous: $error,
            );
        }
        if ($canonical !== $json) {
            throw new UnexpectedValueException(
                'Pliego contract probe JSON must use exact typed member order and compact encoding',
            );
        }

        self::validateDocument($document);

        return new self($document);
    }

    /** @return array<string, mixed> */
    public function toArray(): array
    {
        return $this->document;
    }

    /** @return array<string, mixed> */
    public function engine(): array
    {
        /** @var array<string, mixed> */
        return $this->document['engine'];
    }

    /** @return list<array<string, mixed>> */
    public function contracts(): array
    {
        /** @var list<array<string, mixed>> */
        return $this->document['contracts'];
    }

    /** @return array<string, mixed> */
    public function invocation(): array
    {
        /** @var array<string, mixed> */
        return $this->document['invocation'];
    }

    public function api2Available(): bool
    {
        return $this->document['contracts'] !== [];
    }

    /**
     * Selects one complete advertised protocol tuple and, only when explicitly
     * requested, one profile advertised inside that same tuple.
     *
     * A null result means the exact pair is unavailable. A null requested
     * profile remains null even if the tuple advertises profiles.
     *
     * @param array<string, mixed> $protocol
     * @param null|array<string, mixed> $profile
     * @return null|array{contract: array<string, mixed>, profile: null|array<string, mixed>}
     */
    public function select(array $protocol, ?array $profile = null): ?array
    {
        self::validateProtocolReference($protocol, 'requested protocol');
        if ($profile !== null) {
            self::validateProfileReference($profile, 'requested profile');
        }

        foreach ($this->contracts() as $contract) {
            $advertisedProtocol = array_intersect_key($contract, array_flip(self::PROTOCOL_KEYS));
            if ($advertisedProtocol !== $protocol) {
                continue;
            }
            if ($profile !== null && !self::containsExactProfile($contract['profiles'], $profile)) {
                return null;
            }

            return ['contract' => $contract, 'profile' => $profile];
        }

        return null;
    }

    /** @param array<string, mixed> $document */
    private static function validateDocument(array $document): void
    {
        self::assertKeys($document, self::TOP_LEVEL_KEYS, 'runtime contract');
        self::assertLiteral($document['schema'], 'pliego.runtime-contract', 'runtime contract.schema');
        self::assertLiteral($document['version'], 1, 'runtime contract.version');
        self::validateEngine($document['engine']);

        $contracts = self::assertList($document['contracts'], 'runtime contract.contracts', 32);
        $seenContracts = [];
        $seenProtocols = [];
        $previousContract = null;
        foreach ($contracts as $index => $contract) {
            self::validateContract($contract, "runtime contract.contracts[{$index}]");
            /** @var array<string, mixed> $contract */
            $serializedContract = json_encode(
                $contract,
                JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR,
            );
            if (isset($seenContracts[$serializedContract])) {
                throw new UnexpectedValueException('runtime contract contains a duplicate contract tuple');
            }
            if ($previousContract !== null && strcmp($previousContract, $serializedContract) > 0) {
                throw new UnexpectedValueException(
                    'runtime contract tuples must be canonically ordered by compact JSON bytes',
                );
            }
            $seenContracts[$serializedContract] = true;
            $previousContract = $serializedContract;

            $protocolIdentity = json_encode(
                array_intersect_key($contract, array_flip(self::PROTOCOL_KEYS)),
                JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR,
            );
            if (isset($seenProtocols[$protocolIdentity])) {
                throw new UnexpectedValueException('runtime contract contains a duplicate protocol tuple');
            }
            $seenProtocols[$protocolIdentity] = true;
        }

        self::validateInvocation($document['invocation']);
    }

    private static function validateEngine(mixed $engine): void
    {
        $engine = self::assertObject($engine, 'runtime contract.engine');
        self::assertKeys($engine, self::ENGINE_KEYS, 'runtime contract.engine');
        self::assertLiteral($engine['name'], 'pliego', 'runtime contract.engine.name');
        self::assertPattern(
            $engine['version'],
            '/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/D',
            'runtime contract.engine.version',
        );
        self::assertLiteral($engine['api'], 2, 'runtime contract.engine.api');
        self::assertPattern(
            $engine['source_commit'],
            '/^[0-9a-f]{40}$/D',
            'runtime contract.engine.source_commit',
        );

        $runtime = self::assertObject($engine['runtime'], 'runtime contract.engine.runtime');
        self::assertKeys($runtime, self::RUNTIME_KEYS, 'runtime contract.engine.runtime');
        self::assertLiteral($runtime['mode'], 'one-shot', 'runtime contract.engine.runtime.mode');
        self::assertPattern(
            $runtime['target'],
            '/^[a-z0-9]+(?:_[a-z0-9]+)*-[a-z0-9]+(?:_[a-z0-9]+)*-[a-z0-9]+(?:_[a-z0-9]+)*(?:-[a-z0-9]+(?:_[a-z0-9]+)*)?$/D',
            'runtime contract.engine.runtime.target',
            5,
            63,
        );
        self::assertPattern(
            $runtime['binary_sha256'],
            '/^sha256:[0-9a-f]{64}$/D',
            'runtime contract.engine.runtime.binary_sha256',
        );
        self::assertPattern(
            $runtime['servo_base'],
            '/^[0-9a-f]{40}$/D',
            'runtime contract.engine.runtime.servo_base',
        );
    }

    private static function validateContract(mixed $contract, string $path): void
    {
        $contract = self::assertObject($contract, $path);
        self::assertKeys($contract, self::CONTRACT_KEYS, $path);
        self::assertLiteral($contract['api'], 2, "{$path}.api");
        self::validateExactSchemaReference(
            $contract['input_manifest'],
            'pliego.input-manifest',
            "{$path}.input_manifest",
        );
        self::validateExactSchemaReference(
            $contract['request'],
            'pliego.render-request',
            "{$path}.request",
        );
        self::validateExactSchemaReference(
            $contract['result'],
            'pliego.render-result',
            "{$path}.result",
        );
        self::validateExactSchemaReference(
            $contract['document_scene'],
            'pliego.document-scene',
            "{$path}.document_scene",
        );
        self::validateExactSchemaReference(
            $contract['bundle_manifest'],
            'pliego.bundle-manifest',
            "{$path}.bundle_manifest",
        );

        $profiles = self::assertList($contract['profiles'], "{$path}.profiles", 32);
        $seen = [];
        $previousProfile = null;
        foreach ($profiles as $index => $profile) {
            self::validateProfileReference($profile, "{$path}.profiles[{$index}]");
            /** @var array<string, mixed> $profile */
            $identity = $profile['schema'].'@'.$profile['version'];
            if (isset($seen[$identity])) {
                throw new UnexpectedValueException("{$path}.profiles contains a duplicate profile");
            }
            if (
                $previousProfile !== null
                && (
                    strcmp($previousProfile['schema'], $profile['schema']) > 0
                    || (
                        $previousProfile['schema'] === $profile['schema']
                        && $previousProfile['version'] > $profile['version']
                    )
                )
            ) {
                throw new UnexpectedValueException(
                    "{$path}.profiles must be canonically ordered by schema and version",
                );
            }
            $seen[$identity] = true;
            $previousProfile = $profile;
        }
    }

    private static function validateInvocation(mixed $invocation): void
    {
        $invocation = self::assertObject($invocation, 'runtime contract.invocation');
        self::assertKeys($invocation, self::INVOCATION_KEYS, 'runtime contract.invocation');
        foreach ([
            'request_transport' => 'stdin-single-json',
            'request_max_bytes' => 1_048_576,
            'result_transport' => 'stdout-single-json',
            'invocation_error_transport' => 'stderr-utf8-line',
            'success_exit_code' => 0,
            'failed_exit_code' => 1,
            'invocation_error_exit_code' => 64,
        ] as $key => $expected) {
            self::assertLiteral($invocation[$key], $expected, "runtime contract.invocation.{$key}");
        }
    }

    /** @param array<string, mixed> $protocol */
    private static function validateProtocolReference(array $protocol, string $path): void
    {
        self::assertKeys($protocol, self::PROTOCOL_KEYS, $path);
        self::assertLiteral($protocol['api'], 2, "{$path}.api");
        foreach ([
            'input_manifest' => 'pliego.input-manifest',
            'request' => 'pliego.render-request',
            'result' => 'pliego.render-result',
            'document_scene' => 'pliego.document-scene',
            'bundle_manifest' => 'pliego.bundle-manifest',
        ] as $member => $schema) {
            self::validateSchemaReference($protocol[$member], $schema, "{$path}.{$member}");
        }
    }

    private static function validateExactSchemaReference(mixed $reference, string $schema, string $path): void
    {
        self::validateSchemaReference($reference, $schema, $path);
        /** @var array<string, mixed> $reference */
        self::assertLiteral($reference['version'], 1, "{$path}.version");
    }

    private static function validateSchemaReference(mixed $reference, string $schema, string $path): void
    {
        $reference = self::assertObject($reference, $path);
        self::assertKeys($reference, self::SCHEMA_REFERENCE_KEYS, $path);
        self::assertLiteral($reference['schema'], $schema, "{$path}.schema");
        self::assertPositiveU32($reference['version'], "{$path}.version");
    }

    private static function validateProfileReference(mixed $profile, string $path): void
    {
        $profile = self::assertObject($profile, $path);
        self::assertKeys($profile, self::SCHEMA_REFERENCE_KEYS, $path);
        self::assertPattern(
            $profile['schema'],
            '/^pliego\.profile\.[a-z0-9][a-z0-9.-]{0,127}$/D',
            "{$path}.schema",
        );
        self::assertPositiveU32($profile['version'], "{$path}.version");
    }

    /**
     * @param list<mixed> $profiles
     * @param array<string, mixed> $requested
     */
    private static function containsExactProfile(array $profiles, array $requested): bool
    {
        foreach ($profiles as $profile) {
            if ($profile === $requested) {
                return true;
            }
        }

        return false;
    }

    /**
     * @param array<string, mixed> $object
     * @param list<string> $expected
     */
    private static function assertKeys(array $object, array $expected, string $path): void
    {
        if (array_keys($object) !== $expected) {
            throw new UnexpectedValueException("{$path} has unsupported or out-of-order members");
        }
    }

    /** @return array<string, mixed> */
    private static function assertObject(mixed $value, string $path): array
    {
        if (!is_array($value) || array_is_list($value)) {
            throw new UnexpectedValueException("{$path} must be an object");
        }

        return $value;
    }

    /** @return list<mixed> */
    private static function assertList(mixed $value, string $path, int $maximum): array
    {
        if (!is_array($value) || !array_is_list($value) || count($value) > $maximum) {
            throw new UnexpectedValueException("{$path} must be a list of at most {$maximum} items");
        }

        return $value;
    }

    private static function assertLiteral(mixed $actual, string|int $expected, string $path): void
    {
        if ($actual !== $expected) {
            throw new UnexpectedValueException("{$path} has an unsupported value or type");
        }
    }

    private static function assertPattern(
        mixed $value,
        string $pattern,
        string $path,
        ?int $minimumLength = null,
        ?int $maximumLength = null,
    ): void {
        if (
            !is_string($value)
            || ($minimumLength !== null && strlen($value) < $minimumLength)
            || ($maximumLength !== null && strlen($value) > $maximumLength)
            || preg_match($pattern, $value) !== 1
        ) {
            throw new UnexpectedValueException("{$path} has an unsupported value or type");
        }
    }

    private static function assertPositiveU32(mixed $value, string $path): void
    {
        if (!is_int($value) || $value < 1 || $value > 4_294_967_295) {
            throw new UnexpectedValueException("{$path} must be an unsigned 32-bit version integer");
        }
    }

    private static function formatStderrDiagnostic(string $stderr): string
    {
        $totalBytes = strlen($stderr);
        if ($totalBytes === 0) {
            return 'stderr=<empty>';
        }

        $preview = substr($stderr, 0, self::STDERR_DIAGNOSTIC_MAX_BYTES);
        $escaped = '';
        for ($index = 0, $length = strlen($preview); $index < $length; $index++) {
            $byte = ord($preview[$index]);
            $escaped .= match ($byte) {
                0x09 => '\\t',
                0x0a => '\\n',
                0x0d => '\\r',
                0x22 => '\\"',
                0x5c => '\\\\',
                default => $byte >= 0x20 && $byte <= 0x7e
                    ? $preview[$index]
                    : sprintf('\\x%02X', $byte),
            };
        }

        $extent = $totalBytes > self::STDERR_DIAGNOSTIC_MAX_BYTES
            ? "{$totalBytes} bytes total; preview truncated to ".self::STDERR_DIAGNOSTIC_MAX_BYTES.' bytes'
            : "{$totalBytes} bytes";

        return "stderr=\"{$escaped}\" ({$extent})";
    }

    /**
     * @param list<string> $command
     */
    private static function validateCommand(array $command, int $timeoutSeconds): void
    {
        if ($command === [] || !array_is_list($command)) {
            throw new InvalidArgumentException('command must be a non-empty list');
        }
        foreach ($command as $part) {
            if (!is_string($part) || $part === '' || str_contains($part, "\0")) {
                throw new InvalidArgumentException('command must contain non-empty strings');
            }
        }
        // Windows dispatches batch targets through cmd.exe even for array-form process commands.
        if (
            PHP_OS_FAMILY === 'Windows'
            && preg_match('/\.(?:bat|cmd)\z/i', rtrim($command[0], " .\t")) === 1
        ) {
            throw new InvalidArgumentException(
                'command must use a native executable on Windows; .bat and .cmd targets are not supported',
            );
        }
        if ($timeoutSeconds < 1) {
            throw new InvalidArgumentException('timeoutSeconds must be at least 1');
        }
    }

    /**
     * @param non-empty-list<string> $arguments
     * @return array{int, string, string}
     */
    private static function execute(array $arguments, int $timeoutSeconds): array
    {
        $stdoutFile = tmpfile();
        $stderrFile = tmpfile();
        if (!is_resource($stdoutFile) || !is_resource($stderrFile)) {
            if (is_resource($stdoutFile)) {
                fclose($stdoutFile);
            }
            if (is_resource($stderrFile)) {
                fclose($stderrFile);
            }
            throw new RuntimeException('cannot create Pliego contract-probe output streams');
        }

        $pipes = [];
        $process = @proc_open(
            $arguments,
            [0 => ['pipe', 'r'], 1 => $stdoutFile, 2 => $stderrFile],
            $pipes,
        );
        if (!is_resource($process)) {
            fclose($stdoutFile);
            fclose($stderrFile);
            throw new RuntimeException('cannot start the Pliego contract-probe process');
        }
        fclose($pipes[0]);

        $deadline = hrtime(true) + ($timeoutSeconds * 1_000_000_000);
        $status = proc_get_status($process);
        while ($status['running']) {
            if (hrtime(true) >= $deadline) {
                proc_terminate($process, 9);
                proc_close($process);
                fclose($stdoutFile);
                fclose($stderrFile);
                throw new RuntimeException(
                    "Pliego --contract-probe exceeded {$timeoutSeconds} seconds",
                );
            }
            usleep(10_000);
            $status = proc_get_status($process);
        }
        $exitCode = $status['exitcode'];
        $closedExitCode = proc_close($process);
        if ($exitCode < 0) {
            $exitCode = $closedExitCode;
        }

        rewind($stdoutFile);
        rewind($stderrFile);
        $stdout = stream_get_contents($stdoutFile);
        $stderr = stream_get_contents($stderrFile);
        fclose($stdoutFile);
        fclose($stderrFile);

        return [$exitCode, $stdout === false ? '' : $stdout, $stderr === false ? '' : $stderr];
    }
}
