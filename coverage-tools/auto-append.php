<?php

$coverage = $GLOBALS['atropos_php_code_coverage'] ?? null;
if (!($coverage instanceof SebastianBergmann\CodeCoverage\CodeCoverage)) {
    return;
}

$reportDirectory = '/tmp/atropos-php-coverage';
if (!is_dir($reportDirectory) && !mkdir($reportDirectory, 0700, true) && !is_dir($reportDirectory)) {
    return;
}

try {
    $coverage->stop();

    (new SebastianBergmann\CodeCoverage\Report\Cobertura())->process(
        $coverage,
        $reportDirectory . '/current.cobertura.xml'
    );
    (new SebastianBergmann\CodeCoverage\Report\PHP())->process(
        $coverage,
        $reportDirectory . '/current.cov'
    );
} catch (Throwable $error) {
    @file_put_contents($reportDirectory . '/error.log', (string) $error);
}
