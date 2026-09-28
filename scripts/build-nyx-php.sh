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

if [[ "${ATROPOS_NYX_BUILD_SHELL:-0}" != 1 ]]; then
	export ATROPOS_NYX_BUILD_SHELL=1
	exec "$SCRIPT_DIR/with-nyx-build-deps.sh" "$0" "$@"
fi

BUILD_TMPDIR="${ATROPOS_BUILD_TMPDIR:-${TMPDIR:-/tmp}}"
BUILD_ROOT="$(mktemp -d "$BUILD_TMPDIR/atropos-nyx-php.XXXXXX")"
JOBS="${JOBS:-$(getconf _NPROCESSORS_ONLN)}"

cleanup() {
	local status=$?
	if ((status == 0)) && [[ "${ATROPOS_KEEP_NYX_PHP_BUILD:-0}" != 1 ]]; then
		rm -rf -- "$BUILD_ROOT"
	else
		printf 'Preserving Nyx PHP build files at %s\n' "$BUILD_ROOT" >&2
	fi
}
trap cleanup EXIT

for dependency in autoconf make lddtree nim nimble; do
	if ! command -v "$dependency" >/dev/null 2>&1; then
		printf 'Missing build command: %s\n' "$dependency" >&2
		exit 1
	fi
done
if [[ ! -f "$PHP_SOURCE/configure.ac" || ! -f "$PCOV_SOURCE/config.m4" ]]; then
	printf 'Missing patched PHP/PCOV sources under %s\n' "$LEGACY_ROOT" >&2
	exit 1
fi

mkdir -p "$PHP_PREFIX" "$ARTIFACT_DIR/lib"
chmod 700 "$ARTIFACT_DIR"
if [[ "${ATROPOS_NYX_REUSE_PHP:-0}" == 1 ]]; then
	PHP_CGI="$PHP_PREFIX/bin/php-cgi"
	PCOV_SO="$(find "$PHP_PREFIX" -type f -name pcov.so -print -quit)"
	if [[ ! -x "$PHP_CGI" || -z "$PCOV_SO" ]]; then
		printf 'ATROPOS_NYX_REUSE_PHP=1 requires php-cgi and pcov.so under %s\n' "$PHP_PREFIX" >&2
		exit 1
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
	--enable-cgi \
	--enable-cli \
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

export CC="${ATROPOS_NYX_NIM_CC:-/usr/bin/gcc}"
export CXX="${ATROPOS_NYX_NIM_CXX:-/usr/bin/g++}"

PHP_CGI="$PHP_PREFIX/bin/php-cgi"
if [[ ! -x "$PHP_CGI" ]]; then
	PHP_CGI="$BUILD_ROOT/php-src/sapi/cgi/php-cgi"
fi
if [[ ! -x "$PHP_CGI" ]]; then
	printf 'PHP CGI SAPI was not built under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi
PCOV_SO="$(find "$PHP_PREFIX" -type f -name pcov.so -print -quit)"
if [[ -z "$PCOV_SO" ]]; then
	printf 'PCOV extension was not installed under %s\n' "$PHP_PREFIX" >&2
	exit 1
fi

cp --remove-destination -- "$PHP_CGI" "$ARTIFACT_DIR/target_executable"
cp --remove-destination -- "$PCOV_SO" "$ARTIFACT_DIR/pcov.so"
cat >"$ARTIFACT_DIR/php.ini" <<'EOF'
display_errors=Off
log_errors=Off
expose_php=Off
date.timezone=UTC
extension_dir=/tmp
extension=pcov.so
pcov.enabled=1
pcov.directory=/var/www/html
EOF

for executable in "$PHP_CGI" "$PCOV_SO"; do
	while IFS= read -r dependency; do
		[[ -f "$dependency" ]] || continue
		cp --remove-destination -L -- "$dependency" "$ARTIFACT_DIR/lib/$(basename -- "$dependency")"
	done < <(lddtree -l "$executable")
done

if [[ "${ATROPOS_NYX_SKIP_AGENT:-0}" != 1 ]]; then
	NIMBLE_DIR="$BUILD_ROOT/nimble"
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
	(cd "$LEGACY_ROOT/fuzzer" && env -u LD_LIBRARY_PATH nim c \
		--passC:-B/usr/bin/ --passL:-B/usr/bin/ \
		--nimblePath:"$NIMBLE_DIR/pkgs" \
		--path:"$FASTCGI_PATH" \
		--nimcache:"$BUILD_ROOT/nimcache" \
		--d:release --opt:speed \
		--out:"$ARTIFACT_DIR/atropos_agent" \
		atropos_agent.nim)
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
tar -C "$ARTIFACT_DIR" -czf "$ARTIFACT_DIR/nyx-php-runtime.tar.gz" \
	target_executable php.ini pcov.so lib
printf 'Nyx PHP/PCOV runtime: %s\n' "$ARTIFACT_DIR"
printf 'Host PHP prefix: %s\n' "$PHP_PREFIX"
