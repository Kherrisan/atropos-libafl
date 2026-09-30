#!/usr/bin/env bash
set -euo pipefail

if ! /usr/local/bin/atropos-nyx-preimage --check; then
	exit 0
fi

RUNTIME_DIR=/usr/local/lib/atropos-nyx-php
if [[ ! -x "$RUNTIME_DIR/target_executable" || ! -x "$RUNTIME_DIR/php-cli" || \
	! -f "$RUNTIME_DIR/atropos_shm.so" || ! -f "$RUNTIME_DIR/atropos-nyx-bootstrap.php" ]]; then
	RUNTIME_ARCHIVE=/usr/local/lib/atropos-nyx-runtime.tar.gz
	if [[ ! -f "$RUNTIME_ARCHIVE" ]]; then
		echo "Nyx PHP runtime is missing from $RUNTIME_DIR" >&2
		exit 1
	fi
	install -d -o root -g root -m 0755 "$RUNTIME_DIR"
	tar -xzf "$RUNTIME_ARCHIVE" -C "$RUNTIME_DIR"
	rm -f "$RUNTIME_ARCHIVE"
fi

# /tmp is cleaned during guest boot; refresh its legacy paths with new mtimes.
umask 022
cp -r "$RUNTIME_DIR"/. /tmp/
cp -r "$RUNTIME_DIR"/lib/. /tmp/
chmod 0755 /tmp/target_executable /tmp/php-cli /tmp/pcov.so
chmod 0644 /tmp/php.ini
chmod 0644 /tmp/atropos_shm.so /tmp/atropos-nyx-bootstrap.php
if [[ -f /tmp/opcache.so ]]; then
	chmod 0644 /tmp/opcache.so
fi

exec env IN_NYX=1 PHP_TARGET=/tmp/target_executable LD_LIBRARY_PATH=/tmp/ \
	/usr/local/bin/atropos_agent
