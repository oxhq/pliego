#!/usr/bin/env php
<?php

/**
 * dompdf competitor runner — B2 stub.
 *
 * Same NDJSON output contract as pliego.php (one JSON object per sample on
 * stdout: wall_ms, user_ms, sys_ms, peak_rss_kib, exit_code, ok, output,
 * correctness). Implemented during the B2 competitor-comparison milestone;
 * until then it exits with a clear message.
 */

declare(strict_types=1);

const USAGE = <<<EOT
Usage: php dompdf.php --input <file.html> --output <file.pdf>
  [--samples N] [--warmup N] [--page-count N] [--text-contains a,b,c]
EOT;

$options = getopt('', ['input:', 'output:', 'samples:', 'warmup:', 'page-count:', 'text-contains:']);
if ($options === false) {
    fwrite(STDERR, USAGE . "\n");
    exit(2);
}

fwrite(STDERR, "dompdf.php: not implemented — B2 competitor milestone\n");
fwrite(STDOUT, json_encode([
    'ok' => false,
    'exit_code' => 2,
    'wall_ms' => 0.0,
    'user_ms' => null,
    'sys_ms' => null,
    'peak_rss_kib' => null,
    'reason' => 'dompdf runner is a B2 stub',
]) . "\n");
exit(2);
