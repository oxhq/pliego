<?php

declare(strict_types=1);

// Diagnostic variants only; the source invoice and comparison corpus are never modified.
$options = getopt('', ['sdk-autoload:', 'binary:', 'source:', 'font:', 'output:', 'case:']);
foreach (['sdk-autoload', 'binary', 'source', 'font', 'output', 'case'] as $name) {
    if (!is_string($options[$name] ?? null) || $options[$name] === '') {
        throw new InvalidArgumentException('Missing --'.$name);
    }
}
foreach (['sdk-autoload', 'binary', 'source', 'font'] as $name) {
    $options[$name] = realpath($options[$name]) ?: throw new InvalidArgumentException('Missing file --'.$name);
    if (!is_file($options[$name])) {
        throw new InvalidArgumentException('--'.$name.' must be a file');
    }
}
$fontCss = '@font-face{font-family:Diagnostic;src:url("assets/font.woff2")}body{font-family:Diagnostic}';
$table = '<table><tr><td>Invoice</td><td>450.00</td></tr></table>';
$collapseCss = 'table{border-collapse:collapse}td{border:1px solid #222}';
$cases = [
    'paragraph' => '<p>Invoice 450.00</p>',
    'table' => $table,
    'collapse-screen' => '<style media="screen">'.$collapseCss.'</style>'.$table,
    'collapse-print' => '<style media="print">'.$collapseCss.'</style>'.$table,
    'collapse-all' => '<style>'.$collapseCss.'</style>'.$table,
    'collapse-borderless' => '<style>table{border-collapse:collapse}td{border:none}</style>'.$table,
    'collapse-horizontal' => '<style>table{border-collapse:collapse}td{border:0;border-bottom:1px solid #222}th{border:0;border-bottom:2px solid #222}</style><table><thead><tr><th>Invoice</th><th>450.00</th></tr></thead><tbody><tr><td>Item</td><td>450.00</td></tr></tbody></table>',
    'collapse-2px' => '<style>table{border-collapse:collapse}td{border:2px solid #222}</style>'.$table,
    'collapse-fixed' => '<style>table{border-collapse:collapse;table-layout:fixed;width:240px}td{border:1px solid #222}</style>'.$table,
    'collapse-black' => '<style>table{border-collapse:collapse}td{border:1px solid black}</style>'.$table,
    'collapse-header' => '<style>'.$collapseCss.'</style><table><thead><tr><td>Invoice</td><td>450.00</td></tr></thead><tbody><tr><td>Item</td><td>450.00</td></tr></tbody></table>',
    'paragraph-no-font' => '<p>Invoice 450.00</p>',
];
$case = $options['case'];
if (!isset($cases[$case])) {
    throw new InvalidArgumentException('Unknown case: '.implode(', ', array_keys($cases)));
}
if (file_exists($options['output']) || is_link($options['output'])) {
    throw new InvalidArgumentException('Output must be a new directory');
}
$parent = realpath(dirname($options['output'])) ?: throw new InvalidArgumentException('Output parent must exist');
$output = $parent.DIRECTORY_SEPARATOR.basename($options['output']);
if (!mkdir($output, 0700)) {
    throw new RuntimeException('Cannot create diagnostic output directory');
}
$write = static function (string $name, mixed $value) use ($output): void {
    $bytes = json_encode($value, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n";
    if (file_put_contents($output.'/'.$name, $bytes, LOCK_EX) !== strlen($bytes)) {
        throw new RuntimeException('Cannot write diagnostic artifact: '.$name);
    }
};
$suppliedFont = $case !== 'paragraph-no-font';
$html = '<!doctype html><html lang="en"><meta charset="utf-8"><title>Invoice diagnostic</title>'
    .($suppliedFont ? '<style>'.$fontCss.'</style>' : '').'<body>'.$cases[$case].'</body></html>';
if (file_put_contents($output.'/input.html', $html, LOCK_EX) !== strlen($html)) {
    throw new RuntimeException('Cannot write diagnostic HTML');
}
require $options['sdk-autoload'];
$record = [
    'schema' => 'pliego.invobook-diagnostic.v1', 'case' => $case,
    'track' => 'synthetic-minimizer-not-compatibility-evidence',
    'sourceSha256' => hash_file('sha256', $options['source']),
    'inputSha256' => hash('sha256', $html), 'phpVersion' => PHP_VERSION,
    'binarySha256' => hash_file('sha256', $options['binary']),
    'suppliedFontSha256' => $suppliedFont ? hash_file('sha256', $options['font']) : null,
    'outerTimeoutSeconds' => 30, 'hostWallMilliseconds' => 20000,
];
$engine = new Pliego\Php\DocumentEngine([$options['binary']], $output.'/jobs', 30, 30);
$assets = $suppliedFont ? [new Pliego\Php\InputAsset('assets/font.woff2', $options['font'], 'font/woff2')] : [];
$started = hrtime(true);
try {
    $result = $engine->render($html, new Pliego\Php\RenderOptions(
        pageSize: 'A4', pageMargins: '0,0,0,0', hostWallMilliseconds: 20000,
    ), $assets);
    $record['status'] = 'success';
    $metadata = $result->metadata;
    $record['pdf'] = ['path' => $result->pdfPath, 'bytes' => filesize($result->pdfPath), 'sha256' => hash_file('sha256', $result->pdfPath)];
    $record['jobPath'] = $result->jobPath;
} catch (Pliego\Php\Exception\RenderFailedException $error) {
    $record['status'] = 'render_failure';
    $record['kind'] = $error->kind;
    $record['jobPath'] = $error->jobPath;
    $metadata = $error->result;
    $failure = $error->diagnosticsPath.'/failure.json';
    if (is_file($failure)) {
        $record['failure'] = json_decode(file_get_contents($failure), true, flags: JSON_THROW_ON_ERROR);
        $write('failure.json', $record['failure']);
    }
} catch (Throwable $error) {
    $record['status'] = 'invocation_or_transport_failure';
    $record['error'] = ['class' => get_class($error), 'message' => $error->getMessage()];
}
$record['wallMs'] = (hrtime(true) - $started) / 1e6;
if (isset($metadata)) {
    $write('request.json', $metadata['request']);
    $write('result.json', $metadata);
    $record['engine'] = $metadata['engine'];
}
$record['sourceUnchanged'] = hash_file('sha256', $options['source']) === $record['sourceSha256'];
$write('diagnostic.json', $record);
fwrite(STDOUT, json_encode($record, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)."\n");
exit($record['sourceUnchanged'] && $record['status'] !== 'invocation_or_transport_failure' ? 0 : 1);
