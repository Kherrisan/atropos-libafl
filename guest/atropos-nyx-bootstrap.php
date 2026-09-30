<?php

require_once '/tmp/atropos-coverage-auto-prepend.php';

if (function_exists('pcov\\set_coverage_dump_enabled')) {
    pcov\set_coverage_dump_enabled(true);
}
atropos_start_php_coverage();

function atropos_nyx_bootstrap_request(): void
{
    if (!function_exists('atropos_request_wait')) {
        throw new RuntimeException('The atropos_shm PHP extension is not loaded');
    }

    if (empty($GLOBALS['atropos_nyx_bootstrap_coverage_saved'])) {
        atropos_finish_php_coverage('baseline');
        $GLOBALS['atropos_nyx_bootstrap_coverage_saved'] = true;
    }
    if (function_exists('pcov\\set_coverage_dump_enabled')) {
        pcov\set_coverage_dump_enabled(false);
    }

    $payloadBytes = atropos_request_wait();
    if (!is_string($payloadBytes)) {
        throw new RuntimeException('No request payload was delivered over shared memory');
    }

    $payload = json_decode($payloadBytes, true);
    if (!is_array($payload) || !isset($payload['requests'][0]) || !is_array($payload['requests'][0])) {
        throw new RuntimeException('The Nyx payload must contain one HTTP request');
    }

    $request = $payload['requests'][0];
    $server = [
        'GATEWAY_INTERFACE' => 'CGI/1.1',
        'SERVER_SOFTWARE' => 'Apache/2.4.58',
        'SERVER_PROTOCOL' => 'HTTP/1.1',
        'SERVER_NAME' => 'localhost',
        'SERVER_ADDR' => '127.0.0.1',
        'SERVER_PORT' => '8000',
        'HTTP_HOST' => 'localhost:8000',
        'REQUEST_METHOD' => 'GET',
        'REQUEST_URI' => '/',
        'QUERY_STRING' => '',
        'SCRIPT_FILENAME' => '/var/www/html/index.php',
        'SCRIPT_NAME' => '/index.php',
        'PHP_SELF' => '/index.php',
        'CONTENT_TYPE' => 'application/json',
        'CONTENT_LENGTH' => '0',
        'REDIRECT_STATUS' => '1',
    ];
    foreach ($request as $key => $value) {
        if (is_string($key) && is_scalar($value)) {
            $server[$key] = (string) $value;
        }
    }

    $_SERVER = array_merge($_SERVER, $server);
    $_GET = [];
    parse_str((string) ($server['QUERY_STRING'] ?? ''), $_GET);

    $body = (string) ($request['POST_DATA'] ?? '');
    $_POST = [];
    if (stripos((string) ($server['CONTENT_TYPE'] ?? ''), 'application/x-www-form-urlencoded') === 0) {
        parse_str($body, $_POST);
    }
    $_COOKIE = [];
    $cookieHeader = (string) ($request['HTTP_COOKIE'] ?? '');
    foreach (explode(';', $cookieHeader) as $cookie) {
        $parts = explode('=', trim($cookie), 2);
        if (count($parts) === 2 && $parts[0] !== '') {
            $_COOKIE[urldecode($parts[0])] = urldecode($parts[1]);
        }
    }
    $_FILES = [];
    $_REQUEST = array_merge($_COOKIE, $_POST, $_GET);
    $GLOBALS['HTTP_RAW_POST_DATA'] = $body;
    $GLOBALS['atropos_nyx_request_payload'] = $payload;

    if (function_exists('wp_fix_server_vars')) {
        wp_fix_server_vars();
    }

    $coverageDump = isset($payload['config']['COVERAGE_DUMP']);
    if (function_exists('pcov\\set_coverage_dump_enabled')) {
        pcov\set_coverage_dump_enabled($coverageDump);
    }
    if ($coverageDump && function_exists('atropos_start_php_coverage')) {
        atropos_start_php_coverage();
    }
    if (function_exists('pcov\\set_execution_limit')) {
        $limit = isset($payload['config']['EXEC_LIMIT']) ? (int) $payload['config']['EXEC_LIMIT'] : 0;
        pcov\set_execution_limit($limit);
    }
}

function atropos_nyx_finalize_request(): void
{
    if (function_exists('atropos_finish_php_coverage')) {
        atropos_finish_php_coverage();
    }

    while (ob_get_level() > 1) {
        @ob_end_flush();
    }
    $response = ob_get_contents();
    if (!is_string($response)) {
        $response = '';
    }
    $status = http_response_code();
    if (!is_int($status) || $status < 100 || $status > 599) {
        $status = 200;
    }

    if (function_exists('atropos_request_complete')) {
        atropos_request_complete($response, $status);
    }
    while (ob_get_level() > 0) {
        @ob_end_clean();
    }
}

ob_start();
register_shutdown_function('atropos_nyx_finalize_request');

if (function_exists('opcache_compile_file') && function_exists('opcache_get_status') && opcache_get_status(false)) {
    $iterator = new RecursiveIteratorIterator(new RecursiveDirectoryIterator('/var/www/html', FilesystemIterator::SKIP_DOTS));
    foreach ($iterator as $file) {
        $path = $file->getPathname();
        if (!$file->isFile() || strtolower($file->getExtension()) !== 'php' || substr($path, -9) === '/crash.php') {
            continue;
        }
        @opcache_compile_file($path);
    }
}
