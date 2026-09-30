<?php

function atropos_start_php_coverage(): void
{
    if (isset($GLOBALS['atropos_php_code_coverage'])) {
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
}

function atropos_finish_php_coverage(string $reportPrefix = 'current'): void
{
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
            $reportDirectory . '/' . $reportPrefix . '.cobertura.xml'
        );
        (new SebastianBergmann\CodeCoverage\Report\PHP())->process(
            $coverage,
            $reportDirectory . '/' . $reportPrefix . '.cov'
        );
    } catch (Throwable $error) {
        @file_put_contents($reportDirectory . '/error.log', (string) $error);
    } finally {
        unset($GLOBALS['atropos_php_code_coverage']);
    }
}

if (is_file('/tmp/atropos-php-coverage-enabled')) {
    atropos_start_php_coverage();
}
