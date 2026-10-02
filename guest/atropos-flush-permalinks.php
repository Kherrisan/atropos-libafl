<?php

/*
 * Run once, in a separate PHP process, before the Nyx snapshot. The fuzzer
 * worker itself has not loaded WordPress yet. This process commits pretty
 * permalinks into MariaDB so the restored worker sees them on every request.
 */

$_SERVER = array_merge($_SERVER, [
    'HTTP_HOST' => 'localhost:8000',
    'SERVER_NAME' => 'localhost',
    'SERVER_ADDR' => '127.0.0.1',
    'SERVER_PORT' => '8000',
    'REQUEST_METHOD' => 'GET',
    'REQUEST_URI' => '/',
    'QUERY_STRING' => '',
    'SCRIPT_FILENAME' => '/var/www/html/index.php',
    'SCRIPT_NAME' => '/index.php',
    'PHP_SELF' => '/index.php',
]);

define('WP_USE_THEMES', false);
require '/var/www/html/wp-load.php';

global $wp_rewrite;
if (!($wp_rewrite instanceof WP_Rewrite)) {
    fwrite(STDERR, "permalink flush could not load WP_Rewrite\n");
    exit(1);
}
// update_option() alone leaves the structure captured at init empty, and
// rewrite_rules() returns nothing until that in-memory property is replaced.
$wp_rewrite->set_permalink_structure('/%postname%/');
$wp_rewrite->flush_rules(false);

$structure = get_option('permalink_structure');
$rules = get_option('rewrite_rules');
if ($structure !== '/%postname%/' || !is_array($rules) || !isset($rules['^wp-json/?$'])) {
    $count = is_array($rules) ? count($rules) : -1;
    fwrite(STDERR, "permalink flush did not install wp-json rewrite rules structure={$structure} rules={$count}\n");
    exit(1);
}
