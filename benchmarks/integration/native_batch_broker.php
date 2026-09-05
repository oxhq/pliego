<?php

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

declare(strict_types=1);

// Root-only preparation/measurement adapter. The existing runner owns staging,
// capture, UID separation and the one exclusive cgroup sampler invocation.
define('PLIEGO_BENCHMARK_RUNNER_LIBRARY_ONLY', true);
require dirname(__DIR__) . '/runners/pliego.php';

function batch_require(bool $condition, string $message): void
{
    if (! $condition) {
        throw new RuntimeException($message);
    }
}

try {
    batch_require(PHP_OS_FAMILY === 'Linux' && posix_geteuid() === 0, 'requires Linux root broker');
    $raw = stream_get_contents(STDIN, 65_537);
    batch_require(is_string($raw) && strlen($raw) <= 65_536, 'oversized broker input');
    $input = json_decode($raw, true, flags: JSON_THROW_ON_ERROR);
    $mode = $argv[1] ?? '';
    batch_require(is_array($input), 'invalid broker input');
    if ($mode === 'prepare') {
        batch_require(count($input) === 2 && isset($input['state'], $input['binary']), 'invalid preparation fields');
        $state = $input['state'];
        $account = posix_getpwnam('pliego-benchmark-engine');
        batch_require(is_array($account) && $account['uid'] > 0, 'missing engine account');
        $state['engineUid'] = $account['uid'];
        $state['engineGid'] = $account['gid'];
        assert_fixture_identity($state);
        $workers = [];
        for ($index = 0; $index < 2; $index++) {
            $job = stage_api2_job($state);
            prepare_engine_directory($job['temporary'], $account['uid'], $account['gid']);
            $workers[] = [
                'index' => $index,
                'job' => $job['root'],
                'temporary' => $job['temporary'],
                'request' => $job['request'],
                'request_sha256' => hash('sha256', $job['request']),
            ];
        }
        $outer = benchmark_engine_temporary_path('pliego-bench-two-');
        prepare_engine_directory($outer, $account['uid'], $account['gid']);
        foreach (['artifacts', 'temporary'] as $name) {
            prepare_engine_directory($outer . '/' . $name, $account['uid'], $account['gid']);
        }
        $result = [
            'plan' => ['schema' => 'pliego.native-two-job-batch.v1', 'binary' => $input['binary'], 'workers' => $workers],
            'cwd' => $outer,
            'artifacts' => $outer . '/artifacts',
            'temporary' => $outer . '/temporary',
        ];
    } elseif ($mode === 'measure') {
        batch_require(array_keys($input) === ['prepared'], 'invalid measurement fields');
        $prepared = $input['prepared'];
        $launcher = realpath(__DIR__ . '/native_batch_launcher.py');
        batch_require(is_string($launcher), 'missing fixed launcher');
        $request = api2_request_file(canonical_json_frame($prepared['plan'], 'two-job batch plan'));
        try {
            $result = run_engine(
                [$launcher, 'render', 'paired-minimal-static', '--artifacts', $prepared['artifacts']],
                $prepared['cwd'],
                true,
                $request,
                $prepared['temporary'],
                65_000.0,
            );
        } finally {
            rrmdir(dirname($request));
        }
    } else {
        throw new RuntimeException('expected prepare or measure');
    }
    echo json_encode($result, JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES), "\n";
} catch (Throwable $failure) {
    fwrite(STDERR, 'native_batch_broker: ' . $failure->getMessage() . "\n");
    exit(2);
}
