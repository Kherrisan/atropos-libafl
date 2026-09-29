<?php

if (!is_file('/tmp/atropos-php-coverage-enabled')) {
    return;
}

$reportDirectory = '/tmp/atropos-php-coverage';
if (!is_dir($reportDirectory)) {
    @mkdir($reportDirectory, 0700, true);
}
@unlink($reportDirectory . '/current.cobertura.xml');
@unlink($reportDirectory . '/current.cov');
@unlink($reportDirectory . '/error.log');

try {
    require_once '/tmp/php-code-coverage/vendor/autoload.php';

    $filter = new SebastianBergmann\CodeCoverage\Filter();
    $coverage = new SebastianBergmann\CodeCoverage\CodeCoverage(
        (new SebastianBergmann\CodeCoverage\Driver\Selector())->forLineCoverage($filter),
        $filter
    );
    $coverage->excludeUncoveredFiles();
    $coverage->start('Atropos Nyx request ' . uniqid('', true));

    $GLOBALS['atropos_php_code_coverage'] = $coverage;
} catch (Throwable $error) {
    @file_put_contents($reportDirectory . '/error.log', (string) $error);
}
