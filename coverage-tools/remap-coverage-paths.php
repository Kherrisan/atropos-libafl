<?php declare(strict_types=1);

use SebastianBergmann\CodeCoverage\CodeCoverage;
use SebastianBergmann\CodeCoverage\Report\PHP as PhpReport;

if ($argc !== 5) {
    fwrite(STDERR, "Usage: remap-coverage-paths.php <autoload.php> <coverage-dir> <guest-root> <host-root>\n");
    exit(2);
}

require $argv[1];

$coverageDirectory = rtrim($argv[2], DIRECTORY_SEPARATOR);
$guestRoot = rtrim($argv[3], DIRECTORY_SEPARATOR);
$hostRoot = rtrim(realpath($argv[4]) ?: $argv[4], DIRECTORY_SEPARATOR);
$files = glob($coverageDirectory . DIRECTORY_SEPARATOR . '*.cov') ?: [];
$reports = 0;
$mappedPaths = 0;

foreach ($files as $file) {
    $coverage = include $file;

    if (!$coverage instanceof CodeCoverage) {
        throw new RuntimeException(sprintf('Coverage file did not return a CodeCoverage object: %s', $file));
    }

    $data = $coverage->getData(true);
    foreach ($data->coveredFiles() as $guestPath) {
        if ($guestPath !== $guestRoot && strpos($guestPath, $guestRoot . DIRECTORY_SEPARATOR) !== 0) {
            continue;
        }

        $relativePath = ltrim(substr($guestPath, strlen($guestRoot)), DIRECTORY_SEPARATOR);
        $hostPath = $relativePath === ''
            ? $hostRoot
            : $hostRoot . DIRECTORY_SEPARATOR . $relativePath;
        $hostPath = realpath($hostPath);

        if ($hostPath === false || !is_file($hostPath)) {
            continue;
        }

        $data->renameFile($guestPath, $hostPath);
        $mappedPaths++;
    }

    $coverage->setData($data);
    $temporary = $file . '.remapped';
    (new PhpReport())->process($coverage, $temporary);

    if (!rename($temporary, $file)) {
        throw new RuntimeException(sprintf('Could not replace remapped coverage file: %s', $file));
    }

    $reports++;
}

if ($reports === 0 || $mappedPaths === 0) {
    throw new RuntimeException(sprintf(
        'No coverage paths under %s matched source files beneath %s (%d report files)',
        $guestRoot,
        $hostRoot,
        $reports
    ));
}

printf("Remapped %d source path(s) across %d testcase report(s).\n", $mappedPaths, $reports);
