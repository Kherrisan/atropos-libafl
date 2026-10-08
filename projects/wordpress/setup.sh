#!/usr/bin/env bash
# Guest-side setup. The installer has already placed the PHP runtime and, when
# this app needs one, MariaDB. Run from the extracted bundle directory.
set -euo pipefail
APP_ROOT="${APP_ROOT:?}"

run_php() {
	/usr/local/lib/atropos-nyx-php/lib/ld-linux-x86-64.so.2 \
		--library-path /usr/local/lib/atropos-nyx-php/lib \
		/usr/local/lib/atropos-nyx-php/php-cli -d auto_prepend_file= -d auto_append_file= \
		-d pcov.enabled=0 "$@"
}

install -d -o www-data -g www-data /var/www/html
find /var/www/html -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
cp -a "$APP_ROOT"/. /var/www/html/
chown -R www-data:www-data /var/www/html
if [[ ! -x /usr/local/lib/atropos-nyx-php/php-cli ]]; then
	printf 'PHP runtime is not installed yet\n' >&2
	exit 1
fi
if [[ ! -f app-db.sql ]]; then
	run_php /usr/local/lib/atropos-nyx-php/atropos-wp-install.php
	run_php /usr/local/lib/atropos-nyx-php/atropos-flush-permalinks.php
	if command -v mariadb-admin >/dev/null 2>&1; then
		mariadb-admin --no-defaults --protocol=socket flush-tables
	fi
	sync
fi
