#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
umask 077

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
COVERAGE_TOOLS_DIR="$REPO_ROOT/coverage-tools"

if [[ "${ATROPOS_NYX_BUILD_SHELL:-0}" != 1 ]]; then
	export ATROPOS_NYX_BUILD_SHELL=1
	exec "$SCRIPT_DIR/with-nyx-build-deps.sh" "$0" "$@"
fi

PHP_OUTPUT=""
PHP_SRC=""
PCOV_SRC=""
SRC=""
SKIP_WORDPRESS=0
while [[ $# -gt 0 ]]; do
	case "$1" in
	--php-output)
		PHP_OUTPUT="${2:?--php-output needs a directory}"
		shift 2
		;;
	--php-src)
		PHP_SRC="${2:?--php-src needs a directory}"
		shift 2
		;;
	--pcov-src)
		PCOV_SRC="${2:?--pcov-src needs a directory}"
		shift 2
		;;
	--src)
		SRC="${2:?--src needs a directory}"
		shift 2
		;;
	--skip-wordpress | --skip-app)
		SKIP_WORDPRESS=1
		shift
		;;
	*)
		printf 'unknown argument: %s\n' "$1" >&2
		exit 1
		;;
	esac
done
if [[ -z "$PHP_OUTPUT" || -z "$PHP_SRC" || -z "$PCOV_SRC" ]]; then
	printf 'usage: build-nyx-php.sh --php-output DIR --php-src DIR --pcov-src DIR [--src DIR]\n' >&2
	exit 1
fi
PHP_SOURCE="$PHP_SRC"
PCOV_SOURCE="$PCOV_SRC"
PHP_PREFIX="$PHP_OUTPUT/prefix"
ARTIFACT_DIR="$PHP_OUTPUT"

BUILD_TMPDIR="${ATROPOS_BUILD_TMPDIR:-${TMPDIR:-/tmp}}"
BUILD_ROOT="$(mktemp -d "$BUILD_TMPDIR/atropos-nyx-php.XXXXXX")"
JOBS="${JOBS:-$(getconf _NPROCESSORS_ONLN)}"
REUSE_PHP_BUILD=0
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

for dependency in autoconf make patch lddtree nim nimble curl git; do
	if ! command -v "$dependency" >/dev/null 2>&1; then
		printf 'Missing build command: %s\n' "$dependency" >&2
		exit 1
	fi
done
if [[ ! -f "$PHP_SOURCE/configure.ac" || ! -f "$PCOV_SOURCE/config.m4" ]]; then
	printf 'Missing patched PHP/PCOV sources under %s and %s\n' "$PHP_SOURCE" "$PCOV_SOURCE" >&2
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
	PHP_CLI="$PHP_PREFIX/bin/php"
	PCOV_SO="$(find "$PHP_PREFIX" -type f -name pcov.so -print -quit)"
	OPCACHE_SO="$(find "$PHP_PREFIX" -type f -name opcache.so -print -quit)"
	ATROPOS_SHM_SO="$ARTIFACT_DIR/atropos_shm.so"
	if [[ -x "$PHP_CGI" && -x "$PHP_CLI" && -n "$PCOV_SO" && -n "$OPCACHE_SO" ]]; then
		REUSE_PHP_BUILD=1
	else
		PHP_CGI="$ARTIFACT_DIR/target_executable"
		PHP_CLI="$ARTIFACT_DIR/php-cli"
		PCOV_SO="$ARTIFACT_DIR/pcov.so"
		OPCACHE_SO="$ARTIFACT_DIR/opcache.so"
		if [[ ! -x "$PHP_CGI" || ! -x "$PHP_CLI" || ! -f "$PCOV_SO" || ! -f "$OPCACHE_SO" || ! -f "$ATROPOS_SHM_SO" ]]; then
			printf 'ATROPOS_NYX_REUSE_PHP=1 requires the PHP/PCOV runtime under %s or %s\n' \
				"$PHP_PREFIX" "$ARTIFACT_DIR" >&2
			exit 1
		fi
		REUSE_RUNTIME_ARTIFACTS=1
	fi
fi

if [[ "$REUSE_RUNTIME_ARTIFACTS" != 1 ]]; then
	mkdir -p "$BUILD_ROOT/pcov-src"
	tar --exclude=.git -C "$PCOV_SOURCE" -cf - . | tar -C "$BUILD_ROOT/pcov-src" -xf -
	if [[ "$REUSE_PHP_BUILD" != 1 ]]; then
		mkdir -p "$BUILD_ROOT/php-src"
		tar --exclude=.git -C "$PHP_SOURCE" -cf - . | tar -C "$BUILD_ROOT/php-src" -xf -
	fi

	if [[ "$REUSE_PHP_BUILD" != 1 ]]; then
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
	fi

	export PATH="$PHP_PREFIX/bin:$PATH"
if ! command -v phpize >/dev/null 2>&1; then
	printf 'phpize was not installed under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi
cd "$BUILD_ROOT/pcov-src"
python3 - "$BUILD_ROOT/pcov-src/pcov.c" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
source = path.read_text()
anchor = "const zend_function_entry php_pcov_functions[] = {"
implementation = '''ZEND_BEGIN_ARG_INFO_EX(php_pcov_toggle_arginfo, 0, 0, 1)
\tZEND_ARG_INFO(0, enabled)
ZEND_END_ARG_INFO()

ZEND_BEGIN_ARG_INFO_EX(php_pcov_limit_arginfo, 0, 0, 1)
\tZEND_ARG_INFO(0, limit)
ZEND_END_ARG_INFO()

PHP_NAMED_FUNCTION(php_pcov_set_coverage_dump_enabled)
{
\tzend_bool enabled;
\tZEND_PARSE_PARAMETERS_START(1, 1)
\t\tZ_PARAM_BOOL(enabled)
\tZEND_PARSE_PARAMETERS_END();
\tcoverage_dump_enabled = enabled;
\tRETURN_TRUE;
}

PHP_NAMED_FUNCTION(php_pcov_set_execution_limit)
{
\tzend_long limit;
\tZEND_PARSE_PARAMETERS_START(1, 1)
\t\tZ_PARAM_LONG(limit)
\tZEND_PARSE_PARAMETERS_END();
\tif (limit < 0) {
\t\tlimit = 0;
\t}
\texecution_limit = (uint32_t)limit;
\texecuted_opcodes = 0;
\tRETURN_TRUE;
}

'''
if source.count(anchor) != 1:
    raise SystemExit("could not locate the PCOV function table for Nyx runtime toggles")
source = source.replace(anchor, implementation + anchor)
entry = '\tZEND_NS_FENTRY("pcov", set_coverage_dump_enabled, php_pcov_set_coverage_dump_enabled, php_pcov_toggle_arginfo, 0)\n\tZEND_NS_FENTRY("pcov", set_execution_limit, php_pcov_set_execution_limit, php_pcov_limit_arginfo, 0)\n'
source = source.replace(anchor + "\n", anchor + "\n" + entry, 1)
path.write_text(source)
PY
phpize
./configure --with-php-config="$PHP_PREFIX/bin/php-config"
if ! make -j"$JOBS" >"$BUILD_ROOT/pcov-build.log" 2>&1; then
	grep -n -B 3 -A 3 -E 'error:|undefined reference|^make: \*\*\*' "$BUILD_ROOT/pcov-build.log" | tail -n 100 >&2 || true
	tail -n 30 "$BUILD_ROOT/pcov-build.log" >&2
	exit 1
fi
make install

ATROPOS_EXT_SOURCE="$BUILD_ROOT/atropos-shm-ext"
mkdir -p "$ATROPOS_EXT_SOURCE"
cp -- "$REPO_ROOT/guest/php/ext/config.m4" "$REPO_ROOT/guest/php/ext/atropos_shm.c" \
	"$REPO_ROOT/guest/php/atropos_shared.h" "$ATROPOS_EXT_SOURCE/"
cd "$ATROPOS_EXT_SOURCE"
phpize
./configure --with-php-config="$PHP_PREFIX/bin/php-config"
if ! make -j"$JOBS" >"$BUILD_ROOT/atropos-shm-build.log" 2>&1; then
	grep -n -B 3 -A 3 -E 'error:|undefined reference|^make: \*\*\*' "$BUILD_ROOT/atropos-shm-build.log" | tail -n 100 >&2 || true
	tail -n 30 "$BUILD_ROOT/atropos-shm-build.log" >&2
	exit 1
fi
make install
fi

PHP_CLI="$PHP_PREFIX/bin/php"
ATROPOS_SHM_SO="$(find "$PHP_PREFIX" -type f -name atropos_shm.so -print -quit)"
if [[ ! -x "$PHP_CLI" ]]; then
	if [[ -x "$ARTIFACT_DIR/php-cli" ]]; then
		PHP_CLI="$ARTIFACT_DIR/php-cli"
	else
		printf 'PHP CLI is required to install PHP_CodeCoverage under %s\n' "$PHP_PREFIX" >&2
		exit 1
	fi
fi
if [[ -f "$ARTIFACT_DIR/atropos_shm.so" ]]; then
	ATROPOS_SHM_SO="$ARTIFACT_DIR/atropos_shm.so"
fi
if [[ ! -f "$ATROPOS_SHM_SO" ]]; then
	printf 'Atropos shared-memory PHP extension is missing under %s\n' "$PHP_PREFIX" >&2
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
	"$PHP_CLI" -n "$COMPOSER_PHAR" install --working-dir="$COVERAGE_TOOLS_DIR" --no-dev \
		--prefer-dist --no-interaction --classmap-authoritative
cp -- "$COVERAGE_TOOLS_DIR/auto-prepend.php" "$ARTIFACT_DIR/atropos-coverage-auto-prepend.php"
cp -- "$COVERAGE_TOOLS_DIR/auto-append.php" "$ARTIFACT_DIR/atropos-coverage-auto-append.php"
cp -- "$REPO_ROOT/guest/php/atropos-nyx-bootstrap.php" "$ARTIFACT_DIR/atropos-nyx-bootstrap.php"
cp -- "$REPO_ROOT/guest/php/atropos-flush-permalinks.php" "$ARTIFACT_DIR/atropos-flush-permalinks.php"

export CC="${ATROPOS_NYX_NIM_CC:-/usr/bin/gcc}"
export CXX="${ATROPOS_NYX_NIM_CXX:-/usr/bin/g++}"

if [[ "$REUSE_RUNTIME_ARTIFACTS" != 1 ]]; then
	PHP_CGI="$PHP_PREFIX/bin/php-cgi"
	PHP_CLI="$PHP_PREFIX/bin/php"
	if [[ ! -x "$PHP_CGI" ]]; then
		PHP_CGI="$BUILD_ROOT/php-src/sapi/cgi/php-cgi"
	fi
	PCOV_SO="$(find "$PHP_PREFIX" -type f -name pcov.so -print -quit)"
	OPCACHE_SO="$(find "$PHP_PREFIX" -type f -name opcache.so -print -quit)"
	ATROPOS_SHM_SO="$(find "$PHP_PREFIX" -type f -name atropos_shm.so -print -quit)"
fi
if [[ ! -x "$PHP_CGI" ]]; then
	printf 'PHP CGI SAPI was not built under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi
if [[ ! -x "$PHP_CLI" ]]; then
	printf 'PHP CLI SAPI was not built under %s\n' "$PHP_PREFIX" >&2
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
if [[ -z "$ATROPOS_SHM_SO" || ! -f "$ATROPOS_SHM_SO" ]]; then
	printf 'Atropos shared-memory PHP extension was not installed under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi

if [[ "$PHP_CGI" != "$ARTIFACT_DIR/target_executable" ]]; then
	cp --remove-destination -- "$PHP_CGI" "$ARTIFACT_DIR/target_executable"
fi
if [[ "$PHP_CLI" != "$ARTIFACT_DIR/php-cli" ]]; then
	cp --remove-destination -- "$PHP_CLI" "$ARTIFACT_DIR/php-cli"
fi
if [[ "$PCOV_SO" != "$ARTIFACT_DIR/pcov.so" ]]; then
	cp --remove-destination -- "$PCOV_SO" "$ARTIFACT_DIR/pcov.so"
fi
if [[ "$OPCACHE_SO" != "$ARTIFACT_DIR/opcache.so" ]]; then
	cp --remove-destination -- "$OPCACHE_SO" "$ARTIFACT_DIR/opcache.so"
fi
if [[ "$ATROPOS_SHM_SO" != "$ARTIFACT_DIR/atropos_shm.so" ]]; then
	cp --remove-destination -- "$ATROPOS_SHM_SO" "$ARTIFACT_DIR/atropos_shm.so"
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
auto_prepend_file=/tmp/atropos-nyx-bootstrap.php
opcache.enable=1
opcache.enable_cli=1
opcache.memory_consumption=256
opcache.interned_strings_buffer=16
opcache.max_accelerated_files=50000
opcache.validate_timestamps=0
opcache.file_update_protection=0
EOF

for executable in "$PHP_CGI" "$PHP_CLI" "$PCOV_SO" "$OPCACHE_SO" "$ATROPOS_SHM_SO"; do
	while IFS= read -r dependency; do
		[[ -f "$dependency" ]] || continue
		cp --remove-destination -L -- "$dependency" "$ARTIFACT_DIR/lib/$(basename -- "$dependency")"
	done < <(lddtree -l "$executable")
done

if [[ "${ATROPOS_NYX_SKIP_AGENT:-0}" != 1 ]]; then
	AGENT_SOURCE="$BUILD_ROOT/agent-src"
	mkdir -p "$AGENT_SOURCE"
	NIMBLE_DIR="$BUILD_ROOT/nimble"
	mkdir -p "$NIMBLE_DIR"
	env NIMBLE_DIR="$NIMBLE_DIR" nimble install -y \
		https://github.com/egueler/fastcgi.nim-patched.git
	FASTCGI_CLIENT="$(find "$NIMBLE_DIR" -type f -path '*/fastcgi/client.nim' -print -quit)"
	if [[ -z "$FASTCGI_CLIENT" ]]; then
		printf 'nimble did not install fastcgi/client from the patched FastCGI package\n' >&2
		exit 1
	fi
	FASTCGI_PATH="$(dirname -- "$(dirname -- "$FASTCGI_CLIENT")")"
	cp -- "$REPO_ROOT/guest/php/atropos_agent.nim" \
		"$REPO_ROOT/guest/common/nyx_dump_file.c" \
		"$REPO_ROOT/guest/common/nyx.c" "$REPO_ROOT/guest/common/nyx.h" "$AGENT_SOURCE/"
	python3 - "$AGENT_SOURCE/nyx.c" <<'PYTHON'
from pathlib import Path
import sys

path = Path(sys.argv[1])
source = path.read_text()
anchor = "        kAFL_hypercall(HYPERCALL_KAFL_GET_PAYLOAD, (uintptr_t)payload_buffer);\n"
if source.count(anchor) != 1:
    raise SystemExit("could not locate the Nyx payload initialization anchor")
length_function = """uint32_t nyx_get_payload_len() {
    return payload_buffer->size - sizeof(payload_buffer->size);
}"""
if source.count(length_function) != 1:
    raise SystemExit("could not locate the legacy Nyx payload-length helper")
source = source.replace(
    length_function,
    """uint32_t nyx_get_payload_len() {
    /* libnyx stores the input byte count here; it is not the struct size. */
    return payload_buffer->size;
}""",
)
path.write_text(source.replace(anchor, anchor + "        done = true;\n"))
PYTHON
	(cd "$AGENT_SOURCE" && env -u LD_LIBRARY_PATH NIMBLE_DIR="$NIMBLE_DIR" nim c \
		--passC:-B/usr/bin/ --passL:-B/usr/bin/ \
		--path:"$FASTCGI_PATH" \
		--nimcache:"$BUILD_ROOT/nimcache" \
		--d:release --opt:speed \
		--out:"$ARTIFACT_DIR/atropos_agent" \
		atropos_agent.nim)
	printf 'nyx-agent-fastcgi-v1\n' >"$ARTIFACT_DIR/atropos-agent-phpcov-runtime"
	while IFS= read -r dependency; do
		[[ -f "$dependency" ]] || continue
		cp --remove-destination -L -- "$dependency" "$ARTIFACT_DIR/lib/$(basename -- "$dependency")"
	done < <(lddtree -l "$ARTIFACT_DIR/atropos_agent")
fi

if [[ "$SKIP_WORDPRESS" != 1 ]]; then
	if [[ -z "$SRC" ]]; then
		printf 'build-nyx-php.sh needs --src or --skip-app\n' >&2
		exit 1
	fi
	WP_ROOT="$(realpath -- "$SRC")"
	if [[ ! -f "$WP_ROOT/index.php" ]]; then
		printf 'PHP application not found under %s; pass --src\n' "$WP_ROOT" >&2
		exit 1
	fi
	mkdir -p "$ARTIFACT_DIR/webapp"
	chmod 700 "$ARTIFACT_DIR/webapp"
	tar --exclude=.git -C "$WP_ROOT" -cf - . | tar -C "$ARTIFACT_DIR/webapp" -xf -
	if [[ -f "$ARTIFACT_DIR/webapp/wp-config.php" ]]; then
		python3 - "$ARTIFACT_DIR/webapp/wp-config.php" <<'PY'
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
chmod 0755 "$ARTIFACT_DIR/php-cli"
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

if ! env LD_LIBRARY_PATH="$ARTIFACT_DIR/lib" "$ARTIFACT_DIR/php-cli" \
	-n -d "extension=$ARTIFACT_DIR/pcov.so" \
	-r 'exit(function_exists("pcov\\set_coverage_dump_enabled") && function_exists("pcov\\set_execution_limit") ? 0 : 1);'; then
	printf 'PHP CLI must load the Nyx PCOV toggle functions.\n' >&2
	exit 1
fi

tar -C "$ARTIFACT_DIR" -czf "$ARTIFACT_DIR/nyx-php-runtime.tar.gz" \
	target_executable php-cli php.ini pcov.so opcache.so atropos_shm.so lib php-code-coverage \
	atropos-coverage-auto-prepend.php atropos-coverage-auto-append.php atropos-nyx-bootstrap.php \
	atropos-flush-permalinks.php
printf 'php-code-coverage-9.2.31+phpcov-8.2.1+fastcgi-v1\n' >"$ARTIFACT_DIR/php-code-coverage-runtime"
printf 'Nyx PHP/PCOV runtime: %s\n' "$ARTIFACT_DIR"
printf 'Host PHP prefix: %s\n' "$PHP_PREFIX"
