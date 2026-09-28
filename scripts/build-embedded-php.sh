#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
LEGACY_ROOT="${ATROPOS_LEGACY_ROOT:-$(cd -- "$REPO_ROOT/../atropos-legacy" && pwd)}"
PHP_SOURCE="$LEGACY_ROOT/php-7.4-patched"
PCOV_SOURCE="$LEGACY_ROOT/pcov-patched"
PHP_PREFIX="${ATROPOS_PHP_PREFIX:-$HOME/.local/opt/atropos-embed}"
WP_ROOT="${ATROPOS_WORDPRESS_ROOT:-$REPO_ROOT/../wordpress}"
BUILD_TMPDIR="${ATROPOS_BUILD_TMPDIR:-${TMPDIR:-/tmp}}"
BUILD_ROOT="$(mktemp -d "$BUILD_TMPDIR/atropos-php-build.XXXXXX")"
JOBS="${JOBS:-$(getconf _NPROCESSORS_ONLN)}"

cleanup() {
	local status=$?
	if ((status == 0)) && [[ "${ATROPOS_KEEP_PHP_BUILD:-0}" != 1 ]]; then
		rm -rf -- "$BUILD_ROOT"
	else
		printf 'Preserving PHP build files at %s\n' "$BUILD_ROOT" >&2
	fi
}
trap cleanup EXIT

if [[ ! -f "$PHP_SOURCE/configure.ac" || ! -f "$PCOV_SOURCE/config.m4" ]]; then
	printf 'Missing patched PHP/PCOV sources under %s\n' "$LEGACY_ROOT" >&2
	exit 1
fi

mkdir -p "$BUILD_ROOT/php-src" "$BUILD_ROOT/pcov-src" "$PHP_PREFIX"
tar --exclude=.git -C "$PHP_SOURCE" -cf - . | tar -C "$BUILD_ROOT/php-src" -xf -
tar --exclude=.git -C "$PCOV_SOURCE" -cf - . | tar -C "$BUILD_ROOT/pcov-src" -xf -
patch --batch --directory="$BUILD_ROOT/pcov-src" --strip=1 \
	--input="$REPO_ROOT/patches/pcov-libafl-host.patch"

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
	OPENSSL_OPTION="--with-openssl=$OPENSSL_ROOT"
fi

# Use the host compiler/libc headers while injecting the Nix-provided dependency
# search paths. This avoids mixing the host's multiarch headers with Nix glibc.
export CC="${ATROPOS_HOST_CC:-/usr/bin/gcc -B/usr/bin/ -fno-lto}"
export CXX="${ATROPOS_HOST_CXX:-/usr/bin/g++ -B/usr/bin/ -fno-lto}"
export CPPFLAGS="${CPPFLAGS:-} ${NIX_CFLAGS_COMPILE:-}"
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
	--enable-embed=shared \
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
cd "$BUILD_ROOT/pcov-src"
phpize
./configure --with-php-config="$PHP_PREFIX/bin/php-config"
if ! make -j"$JOBS" >"$BUILD_ROOT/pcov-build.log" 2>&1; then
	grep -n -B 3 -A 3 -E 'error:|undefined reference|^make: \*\*\*' "$BUILD_ROOT/pcov-build.log" | tail -n 100 >&2 || true
	tail -n 30 "$BUILD_ROOT/pcov-build.log" >&2
	exit 1
fi
make install

WP_ROOT="$(realpath -m -- "$WP_ROOT")"
cat >> "$PHP_PREFIX/lib/php.ini" <<EOF

; Atropos coverage instrumentation
extension=pcov.so
pcov.enabled=1
pcov.directory=$WP_ROOT
EOF

printf 'Embedded PHP and patched PCOV installed under %s\n' "$PHP_PREFIX"
printf 'Use this build setting: PHP_CONFIG=%s/bin/php-config\n' "$PHP_PREFIX"
printf 'Use this runtime setting: ATROPOS_PHP_PREFIX=%s\n' "$PHP_PREFIX"
