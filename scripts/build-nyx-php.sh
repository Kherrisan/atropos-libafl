#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
umask 077

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
LEGACY_ROOT="${ATROPOS_LEGACY_ROOT:-$(cd -- "$REPO_ROOT/../atropos-legacy" && pwd)}"
PHP_SOURCE="$LEGACY_ROOT/php-7.4-patched"
PCOV_SOURCE="$LEGACY_ROOT/pcov-patched"
PHP_PREFIX="${ATROPOS_NYX_PHP_PREFIX:-$HOME/.local/opt/atropos-libafl-nyx-php}"
ARTIFACT_DIR="${ATROPOS_NYX_GUEST_ARTIFACTS:-${XDG_DATA_HOME:-$HOME/.local/share}/atropos-libafl/nyx/guest}"
COVERAGE_TOOLS_DIR="$REPO_ROOT/coverage-tools"

if [[ "${ATROPOS_NYX_BUILD_SHELL:-0}" != 1 ]]; then
	export ATROPOS_NYX_BUILD_SHELL=1
	exec "$SCRIPT_DIR/with-nyx-build-deps.sh" "$0" "$@"
fi

BUILD_TMPDIR="${ATROPOS_BUILD_TMPDIR:-${TMPDIR:-/tmp}}"
BUILD_ROOT="$(mktemp -d "$BUILD_TMPDIR/atropos-nyx-php.XXXXXX")"
JOBS="${JOBS:-$(getconf _NPROCESSORS_ONLN)}"
REUSE_RUNTIME_ARTIFACTS=0

cleanup() {
	local status=$?
	if ((status == 0)) && [[ "${ATROPOS_KEEP_NYX_PHP_BUILD:-0}" != 1 ]]; then
		rm -rf -- "$BUILD_ROOT"
	else
		printf 'Preserving Nyx PHP build files at %s\n' "$BUILD_ROOT" >&2
	fi
}
trap cleanup EXIT

for dependency in autoconf make patch lddtree nim nimble curl; do
	if ! command -v "$dependency" >/dev/null 2>&1; then
		printf 'Missing build command: %s\n' "$dependency" >&2
		exit 1
	fi
done
if [[ ! -f "$PHP_SOURCE/configure.ac" || ! -f "$PCOV_SOURCE/config.m4" ]]; then
	printf 'Missing patched PHP/PCOV sources under %s\n' "$LEGACY_ROOT" >&2
	exit 1
fi
if [[ ! -f "$COVERAGE_TOOLS_DIR/composer.json" || ! -f "$COVERAGE_TOOLS_DIR/composer.lock" ]]; then
	printf 'Missing pinned PHP coverage tools under %s\n' "$COVERAGE_TOOLS_DIR" >&2
	exit 1
fi

mkdir -p "$PHP_PREFIX" "$ARTIFACT_DIR/lib"
chmod 700 "$ARTIFACT_DIR"
rm -f -- "$ARTIFACT_DIR/php-code-coverage-runtime"
rm -f -- "$ARTIFACT_DIR/atropos-agent-phpcov-runtime"
if [[ "${ATROPOS_NYX_REUSE_PHP:-0}" == 1 ]]; then
	PHP_CGI="$PHP_PREFIX/bin/php-cgi"
	PCOV_SO="$(find "$PHP_PREFIX" -type f -name pcov.so -print -quit)"
	OPCACHE_SO="$(find "$PHP_PREFIX" -type f -name opcache.so -print -quit)"
	if [[ ! -x "$PHP_CGI" || -z "$PCOV_SO" ]]; then
		PHP_CGI="$ARTIFACT_DIR/target_executable"
		PCOV_SO="$ARTIFACT_DIR/pcov.so"
		OPCACHE_SO="$ARTIFACT_DIR/opcache.so"
		if [[ ! -x "$PHP_CGI" || ! -f "$PCOV_SO" || ! -f "$OPCACHE_SO" ]]; then
			printf 'ATROPOS_NYX_REUSE_PHP=1 requires the PHP/PCOV runtime under %s or %s\n' \
				"$PHP_PREFIX" "$ARTIFACT_DIR" >&2
			exit 1
		fi
		REUSE_RUNTIME_ARTIFACTS=1
	fi
else
	mkdir -p "$BUILD_ROOT/php-src" "$BUILD_ROOT/pcov-src"
	tar --exclude=.git -C "$PHP_SOURCE" -cf - . | tar -C "$BUILD_ROOT/php-src" -xf -
	tar --exclude=.git -C "$PCOV_SOURCE" -cf - . | tar -C "$BUILD_ROOT/pcov-src" -xf -

	BZIP2_OPTION=--with-bz2
	OPENSSL_OPTION=--with-openssl
	if [[ -n "${ATROPOS_NIX_BZIP2_DEV:-}" && -n "${ATROPOS_NIX_BZIP2_LIB:-}" ]]; then
		BZIP2_ROOT="$PHP_PREFIX/.build-deps/bzip2"
		mkdir -p "$BZIP2_ROOT"
		ln -sfn "$ATROPOS_NIX_BZIP2_DEV/include" "$BZIP2_ROOT/include"
		ln -sfn "$ATROPOS_NIX_BZIP2_LIB/lib" "$BZIP2_ROOT/lib"
		BZIP2_OPTION="--with-bz2=$BZIP2_ROOT"
	fi
	if [[ -n "${ATROPOS_NIX_OPENSSL_DEV:-}" && -n "${ATROPOS_NIX_OPENSSL_LIB:-}" ]]; then
		OPENSSL_ROOT="$PHP_PREFIX/.build-deps/openssl"
		mkdir -p "$OPENSSL_ROOT"
		ln -sfn "$ATROPOS_NIX_OPENSSL_DEV/include" "$OPENSSL_ROOT/include"
		ln -sfn "$ATROPOS_NIX_OPENSSL_LIB/lib" "$OPENSSL_ROOT/lib"
		if [[ -d "$ATROPOS_NIX_OPENSSL_DEV/lib/pkgconfig" ]]; then
			export PKG_CONFIG_PATH="$ATROPOS_NIX_OPENSSL_DEV/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
		fi
		OPENSSL_OPTION="--with-openssl=$OPENSSL_ROOT"
	fi

	OPENSSL_COMPILER_FLAGS=()
	if [[ -d "$PHP_PREFIX/.build-deps/openssl/include" && -d "$PHP_PREFIX/.build-deps/openssl/lib" ]]; then
		OPENSSL_COMPILER_FLAGS+=("-I$PHP_PREFIX/.build-deps/openssl/include")
		OPENSSL_COMPILER_FLAGS+=("-L$PHP_PREFIX/.build-deps/openssl/lib")
fi
export CC="${ATROPOS_HOST_CC:-/usr/bin/gcc -B/usr/bin/ -fno-lto} ${OPENSSL_COMPILER_FLAGS[*]}"
export CXX="${ATROPOS_HOST_CXX:-/usr/bin/g++ -B/usr/bin/ -fno-lto} ${OPENSSL_COMPILER_FLAGS[*]}"
NYX_DEPENDENCY_CPPFLAGS=()
if [[ -d "$PHP_PREFIX/.build-deps/openssl/include" ]]; then
	NYX_DEPENDENCY_CPPFLAGS+=("-I$PHP_PREFIX/.build-deps/openssl/include")
fi
if [[ -d "$PHP_PREFIX/.build-deps/bzip2/include" ]]; then
	NYX_DEPENDENCY_CPPFLAGS+=("-I$PHP_PREFIX/.build-deps/bzip2/include")
fi
NIX_CPPFLAGS=()
if [[ -n "${NIX_CFLAGS_COMPILE:-}" ]]; then
	read -r -a NIX_CPPFLAGS <<< "$NIX_CFLAGS_COMPILE"
fi
FILTERED_NIX_CPPFLAGS=()
for ((i = 0; i < ${#NIX_CPPFLAGS[@]}; i++)); do
	flag="${NIX_CPPFLAGS[i]}"
	case "$flag" in
		-I | -isystem | -idirafter)
			if ((i + 1 < ${#NIX_CPPFLAGS[@]})) && [[ "${NIX_CPPFLAGS[i + 1]}" == *-openssl-3* ]]; then
				i=$((i + 1))
				continue
			fi
			FILTERED_NIX_CPPFLAGS+=("$flag")
			if ((i + 1 < ${#NIX_CPPFLAGS[@]})); then
				i=$((i + 1))
				FILTERED_NIX_CPPFLAGS+=("${NIX_CPPFLAGS[i]}")
			fi
			continue
			;;
		*-openssl-3*) continue ;;
	esac
	FILTERED_NIX_CPPFLAGS+=("$flag")
done
export CPPFLAGS="${NYX_DEPENDENCY_CPPFLAGS[*]} ${CPPFLAGS:-} ${FILTERED_NIX_CPPFLAGS[*]}"
NIX_LDFLAGS_FOR_GCC=()
if [[ -n "${NIX_LDFLAGS:-}" ]]; then
	read -r -a NIX_LINK_ARGS <<< "$NIX_LDFLAGS"
	for ((i = 0; i < ${#NIX_LINK_ARGS[@]}; i++)); do
		if [[ "${NIX_LINK_ARGS[i]}" == "-rpath" ]]; then
			i=$((i + 1))
			if ((i >= ${#NIX_LINK_ARGS[@]})); then
				printf 'NIX_LDFLAGS contains -rpath without a path\n' >&2
				exit 1
			fi
			NIX_LDFLAGS_FOR_GCC+=("-Wl,-rpath,${NIX_LINK_ARGS[i]}")
		else
			NIX_LDFLAGS_FOR_GCC+=("${NIX_LINK_ARGS[i]}")
		fi
	done
fi
export LDFLAGS="${LDFLAGS:-} ${NIX_LDFLAGS_FOR_GCC[*]}"

cd "$BUILD_ROOT/php-src"
./buildconf --force
./configure \
	--prefix="$PHP_PREFIX" \
	--with-config-file-path="$PHP_PREFIX/lib" \
	--enable-opcache \
	--enable-cgi \
	--enable-cli \
	--enable-dom \
	--enable-xml \
	--enable-xmlwriter \
	--enable-mbstring \
	--enable-bcmath \
	--enable-intl \
	--enable-sockets \
	--with-curl \
	"$OPENSSL_OPTION" \
	--with-zlib \
	--with-mysqli=mysqlnd \
	--with-pdo-mysql=mysqlnd \
	--with-zip \
	--enable-gd \
	--with-jpeg \
	--with-webp \
	--with-freetype \
	--with-xsl \
	"$BZIP2_OPTION"
if ! make -j"$JOBS" >"$BUILD_ROOT/php-build.log" 2>&1; then
	grep -n -B 3 -A 3 -E 'error:|undefined reference|^make: \*\*\*' "$BUILD_ROOT/php-build.log" | tail -n 100 >&2 || true
	tail -n 30 "$BUILD_ROOT/php-build.log" >&2
	exit 1
fi
make install
mkdir -p "$PHP_PREFIX/lib"
cp php.ini-development "$PHP_PREFIX/lib/php.ini"

export PATH="$PHP_PREFIX/bin:$PATH"
if ! command -v phpize >/dev/null 2>&1; then
	printf 'phpize was not installed under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi
cd "$BUILD_ROOT/pcov-src"
phpize
./configure --with-php-config="$PHP_PREFIX/bin/php-config"
if ! make -j"$JOBS" >"$BUILD_ROOT/pcov-build.log" 2>&1; then
	grep -n -B 3 -A 3 -E 'error:|undefined reference|^make: \*\*\*' "$BUILD_ROOT/pcov-build.log" | tail -n 100 >&2 || true
	tail -n 30 "$BUILD_ROOT/pcov-build.log" >&2
	exit 1
fi
make install
fi

PHP_CLI="$PHP_PREFIX/bin/php"
if [[ ! -x "$PHP_CLI" ]]; then
	printf 'PHP CLI is required to install PHP_CodeCoverage under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi
export PATH="$PHP_PREFIX/bin:$PATH"
COVERAGE_VENDOR_DIR="$ARTIFACT_DIR/php-code-coverage/vendor"
mkdir -p "$COVERAGE_VENDOR_DIR"
COMPOSER_PHAR="${ATROPOS_COMPOSER_PHAR:-$BUILD_ROOT/composer.phar}"
if [[ ! -f "$COMPOSER_PHAR" ]]; then
	curl --fail --location --silent --show-error --retry 3 \
		--output "$COMPOSER_PHAR" \
		https://getcomposer.org/download/2.2.24/composer.phar
fi
COMPOSER_HOME="$BUILD_ROOT/composer-home" \
COMPOSER_CACHE_DIR="$PHP_PREFIX/.composer/cache" \
COMPOSER_VENDOR_DIR="$COVERAGE_VENDOR_DIR" \
COMPOSER_MEMORY_LIMIT=-1 \
	"$PHP_CLI" "$COMPOSER_PHAR" install --working-dir="$COVERAGE_TOOLS_DIR" --no-dev \
		--prefer-dist --no-interaction --classmap-authoritative
cp -- "$COVERAGE_TOOLS_DIR/auto-prepend.php" "$ARTIFACT_DIR/atropos-coverage-auto-prepend.php"
cp -- "$COVERAGE_TOOLS_DIR/auto-append.php" "$ARTIFACT_DIR/atropos-coverage-auto-append.php"

export CC="${ATROPOS_NYX_NIM_CC:-/usr/bin/gcc}"
export CXX="${ATROPOS_NYX_NIM_CXX:-/usr/bin/g++}"

if [[ "$REUSE_RUNTIME_ARTIFACTS" != 1 ]]; then
	PHP_CGI="$PHP_PREFIX/bin/php-cgi"
	if [[ ! -x "$PHP_CGI" ]]; then
		PHP_CGI="$BUILD_ROOT/php-src/sapi/cgi/php-cgi"
	fi
	PCOV_SO="$(find "$PHP_PREFIX" -type f -name pcov.so -print -quit)"
	OPCACHE_SO="$(find "$PHP_PREFIX" -type f -name opcache.so -print -quit)"
fi
if [[ ! -x "$PHP_CGI" ]]; then
	printf 'PHP CGI SAPI was not built under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi
if [[ -z "$PCOV_SO" ]]; then
	printf 'PCOV extension was not installed under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi
if [[ -z "$OPCACHE_SO" || ! -f "$OPCACHE_SO" ]]; then
	printf 'OPcache extension was not installed under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi

if [[ "$PHP_CGI" != "$ARTIFACT_DIR/target_executable" ]]; then
	cp --remove-destination -- "$PHP_CGI" "$ARTIFACT_DIR/target_executable"
fi
if [[ "$PCOV_SO" != "$ARTIFACT_DIR/pcov.so" ]]; then
	cp --remove-destination -- "$PCOV_SO" "$ARTIFACT_DIR/pcov.so"
fi
if [[ "$OPCACHE_SO" != "$ARTIFACT_DIR/opcache.so" ]]; then
	cp --remove-destination -- "$OPCACHE_SO" "$ARTIFACT_DIR/opcache.so"
fi
cat >"$ARTIFACT_DIR/php.ini" <<'EOF'
display_errors=Off
log_errors=Off
expose_php=Off
date.timezone=UTC
extension_dir=/tmp
zend_extension=/tmp/opcache.so
extension=pcov.so
pcov.enabled=1
pcov.directory=/var/www/html
memory_limit=512M
auto_prepend_file=/tmp/atropos-coverage-auto-prepend.php
auto_append_file=/tmp/atropos-coverage-auto-append.php
opcache.enable=1
opcache.memory_consumption=256
opcache.interned_strings_buffer=16
opcache.max_accelerated_files=50000
opcache.validate_timestamps=0
opcache.file_update_protection=0
EOF

for executable in "$PHP_CGI" "$PCOV_SO" "$OPCACHE_SO"; do
	while IFS= read -r dependency; do
		[[ -f "$dependency" ]] || continue
		cp --remove-destination -L -- "$dependency" "$ARTIFACT_DIR/lib/$(basename -- "$dependency")"
	done < <(lddtree -l "$executable")
done

if [[ "${ATROPOS_NYX_SKIP_AGENT:-0}" != 1 ]]; then
	NIMBLE_DIR="${ATROPOS_NYX_NIMBLE_DIR:-$BUILD_ROOT/nimble}"
	mkdir -p "$NIMBLE_DIR"
	if ! find "$NIMBLE_DIR/pkgs" -path '*/fastcgi/client.nim' -print -quit 2>/dev/null | rg -q .; then
		nimble --nimbleDir:"$NIMBLE_DIR" install -y \
			https://github.com/egueler/fastcgi.nim-patched.git
	fi
	FASTCGI_MODULE="$(find "$NIMBLE_DIR/pkgs" -path '*/fastcgi/client.nim' -print -quit)"
	if [[ -z "$FASTCGI_MODULE" ]]; then
		printf 'The patched FastCGI Nim package did not install under %s\n' "$NIMBLE_DIR" >&2
		exit 1
	fi
	FASTCGI_PATH="${FASTCGI_MODULE%/fastcgi/client.nim}"
	AGENT_SOURCE="$BUILD_ROOT/agent-src"
	mkdir -p "$AGENT_SOURCE"
	for source in atropos_agent.nim nyx.c nyx.h; do
		cp -- "$LEGACY_ROOT/fuzzer/$source" "$AGENT_SOURCE/$source"
	done
	python3 - "$AGENT_SOURCE/nyx.c" "$AGENT_SOURCE/atropos_agent.nim" <<'PY'
from pathlib import Path
import sys

nyx_c, agent_nim = map(Path, sys.argv[1:])
c_source = nyx_c.read_text()
c_anchor = "        kAFL_hypercall(HYPERCALL_KAFL_GET_PAYLOAD, (uintptr_t)payload_buffer);\n"
if c_source.count(c_anchor) != 1:
    raise SystemExit("could not locate the Nyx agent configuration anchor")
c_source = c_source.replace(c_anchor, c_anchor + "        done = true;\n")
coverage_dump_anchor = "void nyx_hprintf(char* buf) {"
coverage_dump_function = '''void nyx_coverage_dump(char* buffer, uint32_t len, uint32_t pinned_core, uint8_t kind) {
    static bool initialized[2] = {false, false};
    static char filenames[2][64];
    static kafl_dump_file_t objects[2] = {{0}};

    if (kind > 1 || len == 0) {
        return;
    }
    if (!initialized[kind]) {
        snprintf(filenames[kind], sizeof(filenames[kind]),
                 kind == 0 ? "coverage_cobertura_%u" : "coverage_php_%u",
                 pinned_core);
        objects[kind].file_name_str_ptr = (uintptr_t)filenames[kind];
        objects[kind].append = 0;
        objects[kind].bytes = 0;
        kAFL_hypercall(HYPERCALL_KAFL_DUMP_FILE, (uintptr_t)&objects[kind]);
        initialized[kind] = true;
    }
    objects[kind].append = 1;
    objects[kind].bytes = len;
    objects[kind].data_ptr = (uintptr_t)buffer;
    kAFL_hypercall(HYPERCALL_KAFL_DUMP_FILE, (uintptr_t)&objects[kind]);
}

'''
if c_source.count(coverage_dump_anchor) != 1:
    raise SystemExit("could not locate Nyx dump helper insertion point")
c_source = c_source.replace(coverage_dump_anchor, coverage_dump_function + coverage_dump_anchor)
nyx_c.write_text(c_source)

nim_source = agent_nim.read_text()
payload_anchor = '    let payload: string = fmt"{nyx_get_payload()}"\n'
if nim_source.count(payload_anchor) != 1:
    raise SystemExit("could not locate the Nyx payload read")
nim_source = nim_source.replace(
    payload_anchor,
    '    var payload: string = fmt"{nyx_get_payload()}"\n',
)
nim_anchor = "    let jsonNode = parseJson(payload)\n"
nim_replacement = '''    var jsonNode: JsonNode
    try:
        jsonNode = parseJson(payload)
    except CatchableError:
        # libafl_nyx starts the guest with its `not_init` placeholder buffer.
        # Release initialization, then acquire the root snapshot expected by
        # LibAFL before parsing the first real input.
        nyx_exit()
        nyx_create_snapshot()
        nyx_hprintf("LibAFL Nyx bootstrap ready\\n")
        payload = fmt"{nyx_get_payload()}"
        try:
            jsonNode = parseJson(payload)
        except CatchableError:
            nyx_exit()
            quit(0)
'''
if nim_source.count(nim_anchor) != 1:
    raise SystemExit("could not locate the guest input parser")
nim_source = nim_source.replace(nim_anchor, nim_replacement)

connect_anchor = "        cl.connect()\n"
connect_replacement = '''        var connected = false
        for attempt in 0 .. 50:
            try:
                cl.connect()
                connected = true
                break
            except CatchableError:
                sleep(100)
        if not connected:
            nyx_hprintf("FastCGI socket connection failed\\n")
            nyx_exit()
            quit(0)
'''
if nim_source.count(connect_anchor) != 1:
    raise SystemExit("could not locate the FastCGI connect call")
nim_source = nim_source.replace(connect_anchor, connect_replacement)
preload_connect_anchor = "    cl2.connect()\n"
preload_connect_replacement = '''    var preloadConnected = false
    for attempt in 0 .. 50:
        try:
            cl2.connect()
            preloadConnected = true
            break
        except CatchableError:
            sleep(100)
    if not preloadConnected:
        nyx_hprintf("FastCGI socket unavailable during WordPress preload\\n")
        break
'''
if nim_source.count(preload_connect_anchor) != 1:
    raise SystemExit("could not locate the WordPress preload FastCGI connection")
nim_source = nim_source.replace(preload_connect_anchor, preload_connect_replacement)
cgi_command_anchor = "/tmp/target_executable -b /tmp/php.sock -c /tmp/php.ini &"
if nim_source.count(cgi_command_anchor) != 1:
    raise SystemExit("could not locate the PHP-CGI launch command")
nim_source = nim_source.replace(
    cgi_command_anchor,
    "/tmp/target_executable -b /tmp/php.sock -c /tmp/php.ini >/tmp/php-cgi.log 2>&1 &",
)
startup_markers = [
    (
        "nyx_init()\n",
        'nyx_init()\n'
        'nyx_hprintf("agent Nyx initialization complete\\n")\n',
    ),
    (
        'discard execCmd("chown -R mysql:mysql /var/lib/mysql /var/run/mysqld; service mysql restart") #mysqld --innodb-thread-sleep-delay=0 & ")\n',
        'nyx_hprintf("MariaDB already initialized in the Nyx preimage\\n")\n',
    ),
    (
        "start_php_interpreter(true) # without bug oracles for preloading\n",
        '''start_php_interpreter(true) # without bug oracles for preloading
nyx_hprintf("PHP-CGI launch returned\\n")
if not fileExists("/tmp/php.sock"):
    nyx_hprintf("PHP-CGI socket missing after startup\\n")
    if fileExists("/tmp/php-cgi.log"):
        let phpCgiLog = readFile("/tmp/php-cgi.log")
        if phpCgiLog.len > 0:
            nyx_hprintf(phpCgiLog.cstring)
''',
    ),
    (
        "sleep(2000)\n\n# enable bug oracle reporting after cache is loaded",
        'sleep(2000)\n'
        'nyx_hprintf("WordPress preload complete\\n")\n\n'
        '# enable bug oracle reporting after cache is loaded',
    ),
]
for startup_anchor, startup_replacement in startup_markers:
    if nim_source.count(startup_anchor) != 1:
        raise SystemExit(f"could not locate guest startup marker anchor: {startup_anchor[:48]!r}")
    nim_source = nim_source.replace(startup_anchor, startup_replacement)
import_anchor = "proc nyx_init(): void {.importc.}\n"
if nim_source.count(import_anchor) != 1:
    raise SystemExit("could not locate the Nyx agent imports")
nim_source = nim_source.replace(
    import_anchor,
    "proc nyx_hprintf(msg: cstring): void {.importc.}\n"
    "proc nyx_coverage_dump(buffer: cstring, len: uint32, pinned_core: uint32, kind: uint8): void {.importc.}\n"
    + import_anchor,
)

coverage_state_anchor = "    var html_dump_mode = false\n"
if nim_source.count(coverage_state_anchor) != 1:
    raise SystemExit("could not locate coverage mode variables")
nim_source = nim_source.replace(
    coverage_state_anchor,
    coverage_state_anchor + "    var coverage_dump_mode = false\n    var coverage_core = 0\n",
)
coverage_marker_old = '            writeFile("/tmp/coverage_dump_enabled", config[key].getStr()&chr(0x00))\n'
coverage_marker_new = (
    '            coverage_dump_mode = true\n'
    '            coverage_core = parseInt(config[key].getStr())\n'
    '            writeFile("/tmp/atropos-php-coverage-enabled", config[key].getStr()&chr(0x00))\n'
)
if nim_source.count(coverage_marker_old) != 1:
    raise SystemExit("could not locate the guest PHP coverage marker")
nim_source = nim_source.replace(coverage_marker_old, coverage_marker_new)

coverage_export_anchor = "    report_crashes_if_necessary(crash_log)\n\nnyx_exit()"
coverage_export = '''    if coverage_dump_mode:
        let reportDirectory = "/tmp/atropos-php-coverage"
        let coberturaPath = reportDirectory & "/current.cobertura.xml"
        let serializedPath = reportDirectory & "/current.cov"
        if fileExists(coberturaPath):
            let cobertura = readFile(coberturaPath)
            nyx_coverage_dump(cobertura.cstring, uint32(cobertura.len), uint32(coverage_core), 0'u8)
        else:
            nyx_hprintf("PHP_CodeCoverage did not create current.cobertura.xml\\n")
            if fileExists(reportDirectory & "/error.log"):
                let coverageError = readFile(reportDirectory & "/error.log")
                nyx_hprintf(coverageError.cstring)
        if fileExists(serializedPath):
            let serialized = readFile(serializedPath)
            nyx_coverage_dump(serialized.cstring, uint32(serialized.len), uint32(coverage_core), 1'u8)
        else:
            nyx_hprintf("PHP_CodeCoverage did not create current.cov\\n")
        if fileExists("/tmp/atropos-php-coverage-enabled"):
            removeFile("/tmp/atropos-php-coverage-enabled")
    report_crashes_if_necessary(crash_log)

nyx_exit()'''
if nim_source.count(coverage_export_anchor) != 1:
    raise SystemExit("could not locate the end of the guest request loop")
nim_source = nim_source.replace(coverage_export_anchor, coverage_export)
agent_nim.write_text(nim_source)
PY
	(cd "$AGENT_SOURCE" && env -u LD_LIBRARY_PATH nim c \
		--passC:-B/usr/bin/ --passL:-B/usr/bin/ \
		--nimblePath:"$NIMBLE_DIR/pkgs" \
		--path:"$FASTCGI_PATH" \
		--nimcache:"$BUILD_ROOT/nimcache" \
		--d:release --opt:speed \
		--out:"$ARTIFACT_DIR/atropos_agent" \
		atropos_agent.nim)
	printf 'nyx-agent-phpcov-v1\n' >"$ARTIFACT_DIR/atropos-agent-phpcov-runtime"
	while IFS= read -r dependency; do
		[[ -f "$dependency" ]] || continue
		cp -L -- "$dependency" "$ARTIFACT_DIR/lib/$(basename -- "$dependency")"
	done < <(lddtree -l "$ARTIFACT_DIR/atropos_agent")
fi

if [[ "${ATROPOS_NYX_SKIP_WORDPRESS:-0}" != 1 ]]; then
	WP_ROOT="${ATROPOS_WORDPRESS_ROOT:-$REPO_ROOT/../wordpress}"
	WP_ROOT="$(realpath -- "$WP_ROOT")"
	if [[ ! -f "$WP_ROOT/index.php" ]]; then
		printf 'WordPress not found under %s; set ATROPOS_WORDPRESS_ROOT\n' "$WP_ROOT" >&2
		exit 1
	fi
	mkdir -p "$ARTIFACT_DIR/wordpress"
	chmod 700 "$ARTIFACT_DIR/wordpress"
	tar --exclude=.git -C "$WP_ROOT" -cf - . | tar -C "$ARTIFACT_DIR/wordpress" -xf -
	if [[ -f "$ARTIFACT_DIR/wordpress/wp-config.php" ]]; then
		python3 - "$ARTIFACT_DIR/wordpress/wp-config.php" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
data = path.read_text()
data = data.replace("127.0.0.1:33060", "127.0.0.1")
path.write_text(data)
PY
	fi
fi

chmod 0755 "$ARTIFACT_DIR/target_executable"
[[ ! -e "$ARTIFACT_DIR/atropos_agent" ]] || chmod 0755 "$ARTIFACT_DIR/atropos_agent"
env LD_LIBRARY_PATH="$ARTIFACT_DIR/lib" "$ARTIFACT_DIR/target_executable" -n -v
if ! env LD_LIBRARY_PATH="$ARTIFACT_DIR/lib" "$ARTIFACT_DIR/target_executable" \
	-n -d "extension=$ARTIFACT_DIR/pcov.so" -m | grep -qx pcov; then
	printf 'PCOV failed to load into the built PHP CGI runtime.\n' >&2
	exit 1
fi
if ! env LD_LIBRARY_PATH="$ARTIFACT_DIR/lib" "$ARTIFACT_DIR/target_executable" \
	-n -d "zend_extension=$ARTIFACT_DIR/opcache.so" -d opcache.enable=1 -m | grep -qx 'Zend OPcache'; then
	printf 'OPcache failed to load into the built PHP CGI runtime.\n' >&2
	exit 1
fi
for module in dom libxml xmlwriter; do
	if ! env LD_LIBRARY_PATH="$ARTIFACT_DIR/lib" "$ARTIFACT_DIR/target_executable" -n -m | grep -ixq "$module"; then
		printf 'PHP coverage reporting requires the %s extension.\n' "$module" >&2
		exit 1
	fi
done
tar -C "$ARTIFACT_DIR" -czf "$ARTIFACT_DIR/nyx-php-runtime.tar.gz" \
	target_executable php.ini pcov.so opcache.so lib php-code-coverage \
	atropos-coverage-auto-prepend.php atropos-coverage-auto-append.php
printf 'php-code-coverage-9.2.31+phpcov-8.2.1\n' >"$ARTIFACT_DIR/php-code-coverage-runtime"
printf 'Nyx PHP/PCOV runtime: %s\n' "$ARTIFACT_DIR"
printf 'Host PHP prefix: %s\n' "$PHP_PREFIX"
