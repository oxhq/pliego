<?php

declare(strict_types=1);

use Pliego\Php\CliRenderer;
use Pliego\Php\Exception\EngineRenderException;
use Pliego\Php\Exception\InvalidRequestException;
use Pliego\Php\JobRetention;

require dirname(__DIR__).'/vendor/autoload.php';

function expect(bool $condition, string $message): void
{
    if (!$condition) {
        throw new RuntimeException($message);
    }
}

function treeBytes(string $directory): int
{
    $bytes = 0;
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($directory, FilesystemIterator::SKIP_DOTS),
    );
    foreach ($iterator as $entry) {
        if ($entry->isFile() && !$entry->isLink()) {
            $bytes += $entry->getSize();
        }
    }

    return $bytes;
}

function markAt(string $directory, string $status, int $timestamp): void
{
    JobRetention::mark($directory, $status);
    expect(
        touch($directory.DIRECTORY_SEPARATOR.JobRetention::STATUS_FILE, $timestamp),
        "cannot age {$directory}",
    );
}

$now = time();
$root = sys_get_temp_dir().'/pliego-retention-path-test-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($root, 0700);
file_put_contents("{$root}/unrelated.txt", "keep\n");

$cases = [
    ['expired success', str_repeat('a', 32), 'success', 200, true],
    ['expired failure', str_repeat('b', 32), 'failure', 200, true],
    ['recent success', str_repeat('c', 32), 'success', 10, false],
    ['recent failure', str_repeat('d', 32), 'failure', 10, false],
    ['active job', str_repeat('8', 32), 'running', 200, false],
    ['non-job name', 'not-a-job', 'success', 200, false],
    ['unmarked job', str_repeat('e', 32), null, 200, false],
    ['nested job', 'container/'.str_repeat('f', 32), 'failure', 200, false],
];
foreach ($cases as [, $relative, $status, $age]) {
    $directory = "{$root}/{$relative}";
    mkdir($directory, 0700, true);
    file_put_contents("{$directory}/payload.txt", "{$relative}\n");
    if (is_string($status)) {
        markAt($directory, $status, $now - $age);
    }
}

$outside = "{$root}-outside";
mkdir($outside, 0700);
file_put_contents("{$outside}/payload.txt", "outside\n");
markAt($outside, 'success', $now - 200);
$symlink = "{$root}/".str_repeat('9', 32);
$symlinkCreated = @symlink($outside, $symlink);
if ($symlinkCreated) {
    $cases[] = ['symlink escape', str_repeat('9', 32), 'success', 200, false];
}

$expectedBytes = 0;
foreach ($cases as [, $relative, , , $deleted]) {
    if ($deleted) {
        $expectedBytes += treeBytes("{$root}/{$relative}");
    }
}

$retention = new JobRetention();
$dryRun = $retention->prune($root, 100, 100, true);
expect($dryRun['jobs'] === 2, 'dry-run counts only eligible direct jobs');
expect($dryRun['success_jobs'] === 1, 'dry-run counts the eligible success');
expect($dryRun['failure_jobs'] === 1, 'dry-run counts the eligible failure');
expect($dryRun['bytes'] === $expectedBytes, 'dry-run reports eligible file bytes');
foreach ($cases as [$name, $relative]) {
    expect(file_exists("{$root}/{$relative}") || is_link("{$root}/{$relative}"), "dry-run retained {$name}");
}

$pruned = $retention->prune($root, 100, 100);
expect($pruned === $dryRun, 'live prune reports the dry-run counts and bytes');
foreach ($cases as [$name, $relative, , , $deleted]) {
    $exists = file_exists("{$root}/{$relative}") || is_link("{$root}/{$relative}");
    expect($exists !== $deleted, "path safety result for {$name}");
}
expect(is_dir($root), 'the configured root is never deleted');
expect(is_file("{$root}/unrelated.txt"), 'unrelated files are never deleted');
expect(is_dir($outside), 'a symlink target outside the root is never deleted');

foreach ([$root.'/missing', DIRECTORY_SEPARATOR === '\\' ? substr((string) realpath($root), 0, 3) : '/'] as $unsafe) {
    try {
        $retention->prune($unsafe, 100, 100);
        throw new RuntimeException("expected unsafe root rejection for {$unsafe}");
    } catch (InvalidArgumentException) {
    }
}
if ($symlinkCreated) {
    try {
        $retention->prune($symlink, 100, 100);
        throw new RuntimeException('expected symlink root rejection');
    } catch (InvalidArgumentException) {
    }
}

$lifecycleRoot = sys_get_temp_dir().'/pliego-retention-lifecycle-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($lifecycleRoot, 0700);
$asset = "{$lifecycleRoot}-asset.txt";
file_put_contents($asset, "asset\n");
$renderer = new CliRenderer([PHP_BINARY, __DIR__.'/fake_pliego.php']);
$render = static function (string $id, string $html, bool $shouldFail) use (
    $asset,
    $lifecycleRoot,
    $renderer,
): string {
    $job = "{$lifecycleRoot}/{$id}";
    mkdir($job, 0700);
    try {
        $result = $renderer->render(
            $html,
            "{$job}/input",
            "{$job}/document.pdf",
            "{$job}/artifacts",
            assets: ['assets/test.txt' => $asset],
        );
        expect(!$shouldFail, 'expected the lifecycle render to fail');
        expect($result->jobPath === $job, 'success exposes the retained job');
        expect($result->inputBundlePath === "{$job}/input", 'success exposes retained input');
        expect($result->artifactsPath === "{$job}/artifacts", 'success exposes retained artifacts');
        expect(trim((string) file_get_contents("{$job}/".JobRetention::STATUS_FILE)) === 'success', 'success is marked');
    } catch (EngineRenderException $error) {
        expect($shouldFail, 'unexpected lifecycle render failure');
        expect($error->jobPath === $job, 'engine failure exposes the retained job');
        expect($error->inputBundlePath === "{$job}/input", 'engine failure exposes retained input');
        expect($error->artifactsPath === "{$job}/artifacts", 'engine failure exposes retained artifacts');
        expect(trim((string) file_get_contents("{$job}/".JobRetention::STATUS_FILE)) === 'failure', 'failure is marked');
    }

    return $job;
};

$recentSuccess = $render(str_repeat('1', 32), '<p>recent success</p>', false);
$recentFailure = $render(str_repeat('2', 32), 'FAIL_ENGINE', true);
touch($recentFailure.'/'.JobRetention::STATUS_FILE, $now - 200);
$baselineBytes = treeBytes($lifecycleRoot);
$expiredSuccess = $render(str_repeat('3', 32), '<p>expired success</p>', false);
$expiredFailure = $render(str_repeat('4', 32), 'FAIL_ENGINE', true);
touch($expiredSuccess.'/'.JobRetention::STATUS_FILE, $now - 200);
touch($expiredFailure.'/'.JobRetention::STATUS_FILE, $now - 400);

$lifecycle = $retention->prune($lifecycleRoot, 100, 300);
expect($lifecycle['jobs'] === 2, 'four-job lifecycle prunes two expired jobs');
expect(!is_dir($expiredSuccess) && !is_dir($expiredFailure), 'expired success and failure are deleted');
expect(is_dir($recentSuccess) && is_dir($recentFailure), 'recent jobs are retained under separate policies');
expect(treeBytes($lifecycleRoot) === $baselineBytes, 'eligible disk usage returns to baseline');

$invalidJob = sys_get_temp_dir().'/pliego-retention-invalid-'.getmypid().'-'.bin2hex(random_bytes(4));
mkdir($invalidJob, 0700);
try {
    $renderer->render(
        '<p>invalid request</p>',
        "{$invalidJob}/input",
        "{$invalidJob}/document.pdf",
        "{$invalidJob}/artifacts",
    );
    throw new RuntimeException('expected invalid request failure');
} catch (InvalidRequestException $error) {
    expect($error->jobPath === $invalidJob, 'invalid request exposes the retained job');
    expect($error->inputBundlePath === "{$invalidJob}/input", 'invalid request exposes retained input');
    expect($error->artifactsPath === "{$invalidJob}/artifacts", 'invalid request exposes retained artifacts');
    expect(trim((string) file_get_contents("{$invalidJob}/".JobRetention::STATUS_FILE)) === 'failure', 'invalid request is marked failed');
}

$symlinkProof = $symlinkCreated ? 'tested' : 'skipped: platform denied symlink creation';
echo "Pliego retention path/lifecycle self-test passed ({$symlinkProof}); evidence: {$root}, {$lifecycleRoot}\n";
