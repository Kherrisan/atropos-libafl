<?php

require_once '/tmp/atropos-coverage-auto-prepend.php';

if (function_exists('pcov\\set_coverage_dump_enabled')) {
    pcov\set_coverage_dump_enabled(true);
}
atropos_start_php_coverage();

function atropos_nyx_warmup_opcache(string $root): void
{
    if (!function_exists('pcov\\warmup_compile') || !is_dir($root)) {
        return;
    }
    pcov\warmup_begin();
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS)
    );
    foreach ($iterator as $file) {
        if (!$file->isFile()) {
            continue;
        }
        $path = $file->getPathname();
        if (substr($path, -4) !== '.php') {
            continue;
        }
        if (strpos($path, 'crash.php') !== false || strpos($path, 'logout') !== false) {
            continue;
        }
        @pcov\warmup_compile($path);
    }
    pcov\warmup_release();
}

function atropos_nyx_flush_permalinks_once(): void
{
    $script = '/tmp/atropos-flush-permalinks.php';
    if (!is_file($script)) {
        throw new RuntimeException('The permalink flush script is not installed');
    }
    $command = [
        PHP_BINARY,
        '-c', '/tmp/php.ini',
        '-d', 'auto_prepend_file=',
        '-d', 'auto_append_file=',
        '-d', 'pcov.enabled=0',
        $script,
    ];
    $environment = [];
    foreach ($_SERVER as $name => $value) {
        if (is_string($name) && is_string($value) && $name !== 'SHM_ID' && $name !== 'BITMAP_SIZE') {
            $environment[$name] = $value;
        }
    }
    $pipes = [];
    $process = proc_open(
        $command,
        [1 => ['pipe', 'w'], 2 => ['pipe', 'w']],
        $pipes,
        null,
        $environment
    );
    if (!is_resource($process)) {
        throw new RuntimeException('Could not start the permalink flush process');
    }
    $output = stream_get_contents($pipes[1]);
    $errors = stream_get_contents($pipes[2]);
    fclose($pipes[1]);
    fclose($pipes[2]);
    $status = proc_close($process);
    if ($status !== 0) {
        throw new RuntimeException(
            'Permalink flush failed with status ' . $status . ': ' . $output . $errors
        );
    }
}

function atropos_nyx_apply_request(string $payloadBytes): void
{
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

    $coverageDump = isset($payload['config']['COVERAGE_DUMP']);
    if (function_exists('pcov\\set_coverage_dump_enabled')) {
        pcov\set_coverage_dump_enabled($coverageDump);
    }
    if ($coverageDump && function_exists('atropos_start_php_coverage')) {
        atropos_start_php_coverage();
    }
    if (function_exists('pcov\\start')) {
        // Saving the bootstrap baseline calls CodeCoverage::stop(), which stops
        // PCOV too. Keep bitmap tracing enabled for every fuzz request.
        pcov\start();
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

if (!function_exists('atropos_request_wait')) {
    throw new RuntimeException('The atropos_shm PHP extension is not loaded');
}

atropos_nyx_warmup_opcache('/var/www/html');
atropos_nyx_flush_permalinks_once();

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
atropos_nyx_apply_request($payloadBytes);
