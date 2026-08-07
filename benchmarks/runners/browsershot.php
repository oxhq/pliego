#!/usr/bin/env php
<?php

/**
 * Browsershot (Chrome headless via Puppeteer) competitor runner — B2 stub.
 *
 * Same NDJSON output contract as pliego.php. The B2 milestone will need two
 * modes: cold (Node + Chrome per render) and warm (persistent Chrome). Until
 * then this exits with a clear message.
 */

declare(strict_types=1);

const USAGE = <<<EOT
Usage: php browsershot.php --input <file.html> --output <file.pdf>
  [--mode cold|warm] [--samples N] [--warmup N] [--page-count N] [--text-contains a,b,c]
EOT;

$options = getopt('', ['input:', 'output:', 'mode:', 'samples:', 'warmup:', 'page-count:', 'text-contains:']);
if ($options === false) {
    fwrite(STDERR, USAGE . "\n");
    exit(2);
}

fwrite(STDERR, "browsershot.php: not implemented — B2 competitor milestone\n");
fwrite(STDOUT, json_encode([
    'ok' => false,
    'exit_code' => 2,
    'wall_ms' => 0.0,
    'user_ms' => null,
    'sys_ms' => null,
    'peak_rss_kib' => null,
    'reason' => 'browsershot runner is a B2 stub',
]) . "\n");
exit(2);
