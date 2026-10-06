<?php

require_once '/tmp/atropos-coverage-auto-prepend.php';

$atroposCoverageDump = is_file('/tmp/coverage_dump_enabled');
if (function_exists('pcov\\set_coverage_dump_enabled')) {
    pcov\set_coverage_dump_enabled($atroposCoverageDump);
}
if ($atroposCoverageDump && function_exists('atropos_start_php_coverage')) {
    atropos_start_php_coverage();
}
if (function_exists('pcov\\start')) {
    pcov\start();
}
if (function_exists('pcov\\set_execution_limit')) {
    $atroposExecutionLimit = 0;
    if (is_file('/tmp/execution_limit')) {
        $atroposExecutionLimit = (int) file_get_contents('/tmp/execution_limit');
    }
    pcov\set_execution_limit($atroposExecutionLimit);
}

register_shutdown_function(static function () use ($atroposCoverageDump): void {
    if ($atroposCoverageDump && function_exists('atropos_finish_php_coverage')) {
        atropos_finish_php_coverage();
    }
});
