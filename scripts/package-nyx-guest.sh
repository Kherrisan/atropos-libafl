#!/usr/bin/env bash
set -euo pipefail
umask 077

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
if [[ "${ATROPOS_NYX_BUILD_SHELL:-0}" != 1 ]]; then
	export ATROPOS_NYX_BUILD_SHELL=1
	exec "$SCRIPT_DIR/with-nyx-build-deps.sh" "$0" "$@"
fi

DATA_DIR=""
PHP_OUTPUT=""
SRC=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--fuzzer-output)
		DATA_DIR="${2:?--fuzzer-output needs a directory}"
		shift 2
		;;
	--php-output)
		PHP_OUTPUT="${2:?--php-output needs a directory}"
		shift 2
		;;
	--src)
		SRC="${2:?--src needs a directory}"
		shift 2
		;;
	--app)
		APP="${2:?--app needs wordpress or generic}"
		shift 2
		;;
	--db-env)
		DB_ENV="${2:?--db-env needs a file}"
		shift 2
		;;
	*)
		printf 'unknown argument: %s\n' "$1" >&2
		exit 1
		;;
	esac
done
APP="${APP:-wordpress}"
if [[ "$APP" != wordpress && "$APP" != generic ]]; then
	printf 'unknown app %s; use wordpress or generic\n' "$APP" >&2
	exit 1
fi
if [[ -z "$DATA_DIR" || -z "$PHP_OUTPUT" || -z "$SRC" ]]; then
	printf 'usage: package-nyx-guest.sh --src DIR --php-output DIR --fuzzer-output DIR [--app wordpress|generic] [--db-env FILE]\n' >&2
	exit 1
fi
ARTIFACT_DIR="$PHP_OUTPUT"
APP_ROOT="$(realpath -- "$SRC")"
BUNDLE_DIR="$DATA_DIR/bundle"

for path in "$ARTIFACT_DIR/nyx-php-runtime.tar.gz" "$ARTIFACT_DIR/php-code-coverage-runtime" \
	"$ARTIFACT_DIR/atropos-agent-phpcov-runtime" "$ARTIFACT_DIR/atropos_agent" \
	"$ARTIFACT_DIR/php-cli" "$ARTIFACT_DIR/atropos_shm.so" "$ARTIFACT_DIR/atropos-nyx-bootstrap.php" \
	"$ARTIFACT_DIR/atropos-flush-permalinks.php" \
	"$APP_ROOT/index.php" \
	"$REPO_ROOT/guest/common/nyx.h" "$SCRIPT_DIR/nyx-guest-launch.sh"; do
	if [[ ! -e "$path" ]]; then
		printf 'Required Nyx guest input is missing: %s\n' "$path" >&2
		exit 1
	fi
	done
if [[ "$(cat "$ARTIFACT_DIR/atropos-agent-phpcov-runtime")" != nyx-agent-fastcgi-v1 ]]; then
	printf 'The Nyx guest agent still uses the shared-memory request channel; rerun scripts/build-nyx-php.sh.\n' >&2
	exit 1
fi
if [[ "$(cat "$ARTIFACT_DIR/php-code-coverage-runtime")" != php-code-coverage-9.2.31+phpcov-8.2.1+fastcgi-v1 ]]; then
	printf 'The PHP runtime still uses the shared-memory request channel; rerun scripts/build-nyx-php.sh.\n' >&2
	exit 1
fi
if [[ "$APP" == wordpress && -f "$APP_ROOT/wp-config.php" ]]; then
python3 - "$APP_ROOT/wp-config.php" "$APP_ROOT/wp-content" <<'PY'
from pathlib import Path
import re
import sys

config = Path(sys.argv[1]).read_text()
content = Path(sys.argv[2])
request_inputs = re.compile(r"\$_(?:SERVER|GET|POST|REQUEST|COOKIE)\b")
if request_inputs.search(config):
    raise SystemExit("wp-config.php reads per-request superglobals before the Nyx checkpoint")
if re.search(r"define\s*\(\s*['\"]MULTISITE['\"]\s*,\s*(?:true|1)\b", config, re.I):
    raise SystemExit("the Nyx pre-plugin checkpoint currently supports single-site WordPress only")
dropins = ["advanced-cache.php", "db.php", "object-cache.php", "maintenance.php", "sunrise.php"]
present = [name for name in dropins if (content / name).exists()]
if present:
    raise SystemExit("request-sensitive pre-checkpoint drop-ins are unsupported: " + ", ".join(present))
PY
fi

mkdir -p "$BUNDLE_DIR"
chmod 700 "$BUNDLE_DIR"

if [[ -n "${DB_ENV:-}" ]]; then
if ! command -v mariadb-dump >/dev/null 2>&1; then
	printf 'mariadb-dump is unavailable; run this script through scripts/with-nyx-build-deps.sh\n' >&2
	exit 1
fi
if ! mariadb-admin --no-defaults --protocol=TCP --host=127.0.0.1 --port=33060 ping --silent >/dev/null 2>&1; then
	printf 'MariaDB is not active on 127.0.0.1:33060. Start it or omit --db-env to initialize the database in the guest.\n' >&2
	exit 1
fi
set -a
# shellcheck disable=SC1090
. "$DB_ENV"
set +a
if [[ ! "$MARIADB_DATABASE" =~ ^[A-Za-z0-9_]+$ || ! "$MARIADB_USER" =~ ^[A-Za-z0-9_]+$ ||
	! "$MARIADB_PASSWORD" =~ ^[[:xdigit:]]{64}$ ]]; then
	printf 'Unexpected database values in %s\n' "$DB_ENV" >&2
	exit 1
fi

mkdir -p "$BUNDLE_DIR"
chmod 700 "$DATA_DIR" "$BUNDLE_DIR"
rm -f -- "$BUNDLE_DIR/app-db.sql" "$BUNDLE_DIR/app-db.env" "$BUNDLE_DIR/guest-bundle.tar.gz"
MYSQL_PWD="$MARIADB_PASSWORD" mariadb-dump \
	--protocol=TCP --host=127.0.0.1 --port=33060 \
	--user="$MARIADB_USER" --single-transaction --skip-lock-tables \
	--databases "$MARIADB_DATABASE" > "$BUNDLE_DIR/app-db.sql"
chmod 600 "$BUNDLE_DIR/app-db.sql"

cat >"$BUNDLE_DIR/app-db.env" <<EOF
MARIADB_DATABASE=$MARIADB_DATABASE
MARIADB_USER=$MARIADB_USER
MARIADB_PASSWORD=$MARIADB_PASSWORD
EOF
chmod 600 "$BUNDLE_DIR/app-db.env"
else
	rm -f -- "$BUNDLE_DIR/app-db.sql" "$BUNDLE_DIR/app-db.env" "$BUNDLE_DIR/guest-bundle.tar.gz"
fi

cp -- "$ARTIFACT_DIR/nyx-php-runtime.tar.gz" "$BUNDLE_DIR/"
cp -- "$ARTIFACT_DIR/atropos_agent" "$BUNDLE_DIR/"
cp -- "$SCRIPT_DIR/nyx-guest-launch.sh" "$BUNDLE_DIR/atropos-nyx-launch"
chmod 0755 "$BUNDLE_DIR/atropos-nyx-launch"
cat >"$BUNDLE_DIR/nyx-preimage.c" <<'C'
#define NO_PT_NYX
#include "nyx.h"
#include <stdlib.h>

int main(int argc, char **argv) {
	if (argc == 2 && strcmp(argv[1], "--check") == 0) {
		return is_nyx_vcpu() ? 0 : 1;
	}
	if (!is_nyx_vcpu()) {
		return 0;
	}
	if (system("systemctl disable atropos-nyx-preimage.service") != 0) {
		return 1;
	}
	kAFL_hypercall(HYPERCALL_KAFL_LOCK, 0);
	return 0;
}
C
HOST_CC=(/usr/bin/gcc -B/usr/bin/ -fno-lto)
if [[ -n "${ATROPOS_HOST_CC:-}" ]]; then
	read -r -a HOST_CC <<< "$ATROPOS_HOST_CC"
fi
"${HOST_CC[@]}" -static -O2 -I"$REPO_ROOT/guest/common" \
	-o "$BUNDLE_DIR/atropos-nyx-preimage" "$BUNDLE_DIR/nyx-preimage.c"
rm -f -- "$BUNDLE_DIR/nyx-preimage.c"
chmod 0755 "$BUNDLE_DIR/atropos-nyx-preimage"
printf '%s\n' "$APP" >"$BUNDLE_DIR/app-id"
if [[ -n "${SETUP_SCRIPT:-}" ]]; then
	cp -- "$SETUP_SCRIPT" "$BUNDLE_DIR/setup.sh"
	chmod 0755 "$BUNDLE_DIR/setup.sh"
fi
if [[ -n "${AUTH_SCRIPT:-}" ]]; then
	cp -- "$AUTH_SCRIPT" "$BUNDLE_DIR/auth.py"
	chmod 0755 "$BUNDLE_DIR/auth.py"
fi
if [[ -n "${PROJECT_OUT:-}" && -d "$PROJECT_OUT" ]]; then
	rm -rf -- "$BUNDLE_DIR/out"
	mkdir -p "$BUNDLE_DIR/out"
	cp -a "$PROJECT_OUT"/. "$BUNDLE_DIR/out/"
fi
chmod 644 "$BUNDLE_DIR/app-id"
rm -rf -- "$BUNDLE_DIR/webapp"
mkdir -m 700 "$BUNDLE_DIR/webapp"
tar --exclude=.git -C "$APP_ROOT" -cf - . | tar -C "$BUNDLE_DIR/webapp" -xf -
if [[ -f "$BUNDLE_DIR/webapp/wp-config.php" ]]; then
	python3 - "$BUNDLE_DIR/webapp/wp-config.php" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
data = path.read_text()
data = data.replace("127.0.0.1:33060", "127.0.0.1")
path.write_text(data)
PY
fi
cat >"$BUNDLE_DIR/install-guest.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
umask 077
cd /root/atropos-nyx
APP_ID="$(tr -d '[:space:]' < app-id)"
NEED_DB=0
if [[ "$APP_ID" == wordpress || ( -f app-db.sql && -f app-db.env ) ]]; then
	NEED_DB=1
fi
if [[ "$NEED_DB" == 1 ]]; then
mkdir -p /etc/mysql/mariadb.conf.d
cat >/etc/mysql/mariadb.conf.d/90-atropos-nyx.cnf <<'MYSQL'
[mysqld]
innodb_use_native_aio=0
innodb_flush_method=fsync
MYSQL
cat >/usr/sbin/policy-rc.d <<'POLICY'
#!/bin/sh
exit 101
POLICY
chmod 0755 /usr/sbin/policy-rc.d
export DEBIAN_FRONTEND=noninteractive
if ! dpkg-query -W -f='${Status}' mariadb-server 2>/dev/null | grep -qx 'install ok installed'; then
	apt-get update
	apt-get install -y mariadb-server
fi
rm -f /usr/sbin/policy-rc.d
if [[ ! -e /var/lib/mysql/.atropos-nyx-db-initialized ]]; then
	find /var/lib/mysql -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
	install -d -o mysql -g mysql -m 0750 /var/lib/mysql
	mariadb-install-db --user=mysql --datadir=/var/lib/mysql \
		--auth-root-authentication-method=socket --skip-test-db
	touch /var/lib/mysql/.atropos-nyx-db-initialized
	chown mysql:mysql /var/lib/mysql/.atropos-nyx-db-initialized
fi
systemctl start mariadb.service
if [[ -f app-db.sql && -f app-db.env ]]; then
# shellcheck disable=SC1091
. ./app-db.env
if [[ ! "$MARIADB_DATABASE" =~ ^[A-Za-z0-9_]+$ || ! "$MARIADB_USER" =~ ^[A-Za-z0-9_]+$ ||
	! "$MARIADB_PASSWORD" =~ ^[[:xdigit:]]{64}$ ]]; then
	echo 'Invalid application database configuration' >&2
	exit 1
fi
install -d -o root -g root -m 0700 /usr/local/lib/atropos-nyx-db
install -o root -g root -m 0600 app-db.env /usr/local/lib/atropos-nyx-db/app-db.env
install -o root -g root -m 0600 app-db.sql /usr/local/lib/atropos-nyx-db/app-db.sql
fi
cat >/usr/local/sbin/atropos-nyx-db-prepare <<'DBPREP'
#!/usr/bin/env bash
set -euo pipefail
if [[ -e /var/lib/mysql/.atropos-nyx-db-initialized ]]; then
	exit 0
fi
find /var/lib/mysql -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
install -d -o mysql -g mysql -m 0750 /var/lib/mysql
mariadb-install-db --user=mysql --datadir=/var/lib/mysql \
	--auth-root-authentication-method=socket --skip-test-db
touch /var/lib/mysql/.atropos-nyx-db-initialized
DBPREP
cat >/usr/local/sbin/atropos-nyx-db-import <<'DBIMPORT'
#!/usr/bin/env bash
set -euo pipefail
umask 077
if [[ -e /var/lib/mysql/.atropos-nyx-db-imported ]]; then
	exit 0
fi
# shellcheck disable=SC1091
. /usr/local/lib/atropos-nyx-db/app-db.env
if [[ ! "$MARIADB_DATABASE" =~ ^[A-Za-z0-9_]+$ || ! "$MARIADB_USER" =~ ^[A-Za-z0-9_]+$ ||
	! "$MARIADB_PASSWORD" =~ ^[[:xdigit:]]{64}$ ]]; then
	echo 'Invalid application database configuration' >&2
	exit 1
fi
mariadb --protocol=socket -uroot <<SQL
CREATE DATABASE IF NOT EXISTS \`$MARIADB_DATABASE\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE USER IF NOT EXISTS '$MARIADB_USER'@'localhost' IDENTIFIED BY '$MARIADB_PASSWORD';
CREATE USER IF NOT EXISTS '$MARIADB_USER'@'127.0.0.1' IDENTIFIED BY '$MARIADB_PASSWORD';
ALTER USER '$MARIADB_USER'@'localhost' IDENTIFIED BY '$MARIADB_PASSWORD';
ALTER USER '$MARIADB_USER'@'127.0.0.1' IDENTIFIED BY '$MARIADB_PASSWORD';
GRANT ALL PRIVILEGES ON \`$MARIADB_DATABASE\`.* TO '$MARIADB_USER'@'localhost';
GRANT ALL PRIVILEGES ON \`$MARIADB_DATABASE\`.* TO '$MARIADB_USER'@'127.0.0.1';
SQL
mariadb --protocol=socket -uroot < /usr/local/lib/atropos-nyx-db/app-db.sql
touch /var/lib/mysql/.atropos-nyx-db-imported
rm -rf /usr/local/lib/atropos-nyx-db
DBIMPORT
chmod 0755 /usr/local/sbin/atropos-nyx-db-prepare /usr/local/sbin/atropos-nyx-db-import
cat >/etc/systemd/system/atropos-nyx-db-prepare.service <<'UNIT'
[Unit]
Description=Initialize the Atropos Nyx MariaDB data directory
After=local-fs.target
Before=mariadb.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/atropos-nyx-db-prepare
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
mkdir -p /etc/systemd/system/mariadb.service.d
cat >/etc/systemd/system/mariadb.service.d/atropos-nyx.conf <<'UNIT'
[Unit]
Requires=atropos-nyx-db-prepare.service
After=atropos-nyx-db-prepare.service
UNIT
cat >/etc/systemd/system/atropos-nyx-db-import.service <<'UNIT'
[Unit]
Description=Import the Atropos application database snapshot
Requires=mariadb.service
After=mariadb.service
Before=atropos-nyx-preimage.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/atropos-nyx-db-import
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
fi
install -d -o root -g root -m 0755 /usr/local/lib/atropos-nyx-php
tar -xzf nyx-php-runtime.tar.gz -C /usr/local/lib/atropos-nyx-php
cp -- atropos_agent /usr/local/bin/atropos_agent
cp -- atropos-nyx-preimage /usr/local/bin/atropos-nyx-preimage
cp -- atropos-nyx-launch /usr/local/bin/atropos-nyx-launch
chmod 0755 /usr/local/bin/atropos_agent \
	/usr/local/bin/atropos-nyx-preimage /usr/local/bin/atropos-nyx-launch \
	/usr/local/lib/atropos-nyx-php/target_executable \
	/usr/local/lib/atropos-nyx-php/php-cli \
	/usr/local/lib/atropos-nyx-php/pcov.so
chmod 0644 /usr/local/lib/atropos-nyx-php/php.ini
chmod 0644 /usr/local/lib/atropos-nyx-php/atropos_shm.so \
	/usr/local/lib/atropos-nyx-php/atropos-nyx-bootstrap.php \
	/usr/local/lib/atropos-nyx-php/atropos-flush-permalinks.php
if [[ -f /usr/local/lib/atropos-nyx-php/opcache.so ]]; then
	chmod 0644 /usr/local/lib/atropos-nyx-php/opcache.so
fi
cp -- app-id /usr/local/lib/atropos-nyx-php/atropos-app-id
chmod 0644 /usr/local/lib/atropos-nyx-php/atropos-app-id
install -d -o root -g root -m 0755 /usr/local/lib/atropos
if [[ -f auth.py ]]; then
	# The agent runs this after php-cgi is listening.
	cp -- auth.py /usr/local/lib/atropos/auth.py
	chmod 0755 /usr/local/lib/atropos/auth.py
	if ! command -v python3 >/dev/null 2>&1; then
		export DEBIAN_FRONTEND=noninteractive
		apt-get update
		apt-get install -y python3
	fi
fi
mkdir -p /var/www/html
find /var/www/html -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
cp -a webapp/. /var/www/html/
chown -R www-data:www-data /var/www/html
if [[ "$APP_ID" == wordpress && ! -f app-db.sql ]]; then
	cat >/usr/local/lib/atropos-nyx-php/atropos-wp-install.php <<'PHP'
<?php
$config = '/var/www/html/wp-config.php';
if (!is_file($config)) {
    $password = bin2hex(random_bytes(32));
    $salts = '';
    foreach (['AUTH_KEY', 'SECURE_AUTH_KEY', 'LOGGED_IN_KEY', 'NONCE_KEY', 'AUTH_SALT', 'SECURE_AUTH_SALT', 'LOGGED_IN_SALT', 'NONCE_SALT'] as $name) {
        $salts .= "define( '$name', '" . bin2hex(random_bytes(32)) . "' );\n";
    }
    file_put_contents($config, "<?php\n"
        . "define( 'DB_NAME', 'wordpress' );\n"
        . "define( 'DB_USER', 'atropos' );\n"
        . "define( 'DB_PASSWORD', '$password' );\n"
        . "define( 'DB_HOST', '127.0.0.1' );\n"
        . "define( 'DB_CHARSET', 'utf8mb4' );\n"
        . "define( 'DB_COLLATE', '' );\n"
        . $salts
        . "\$table_prefix = 'wp_';\n"
        . "define( 'WP_HOME', 'http://127.0.0.1' );\n"
        . "define( 'WP_SITEURL', 'http://127.0.0.1' );\n"
        . "define( 'WP_DEBUG', true );\n"
        . "define( 'WP_DEBUG_DISPLAY', false );\n"
        . "if ( ! defined( 'ABSPATH' ) ) { define( 'ABSPATH', __DIR__ . '/' ); }\n"
        . "require_once ABSPATH . 'wp-settings.php';\n");
    chmod($config, 0600);
    chown($config, 'www-data');
}
$source = file_get_contents($config);
$values = [];
foreach (['DB_NAME', 'DB_USER', 'DB_PASSWORD'] as $name) {
    if (!preg_match("/define\\s*\\(\\s*['\\\"]{$name}['\\\"]\\s*,\\s*['\\\"]([^'\\\"]*)['\\\"]/", $source, $match)) {
        fwrite(STDERR, "wp-config.php is missing {$name}\n");
        exit(1);
    }
    $values[$name] = $match[1];
}
$sql = "CREATE DATABASE IF NOT EXISTS `{$values['DB_NAME']}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
    . "CREATE USER IF NOT EXISTS '{$values['DB_USER']}'@'localhost' IDENTIFIED BY '{$values['DB_PASSWORD']}';"
    . "CREATE USER IF NOT EXISTS '{$values['DB_USER']}'@'127.0.0.1' IDENTIFIED BY '{$values['DB_PASSWORD']}';"
    . "ALTER USER '{$values['DB_USER']}'@'localhost' IDENTIFIED BY '{$values['DB_PASSWORD']}';"
    . "ALTER USER '{$values['DB_USER']}'@'127.0.0.1' IDENTIFIED BY '{$values['DB_PASSWORD']}';"
    . "GRANT ALL PRIVILEGES ON `{$values['DB_NAME']}`.* TO '{$values['DB_USER']}'@'localhost';"
    . "GRANT ALL PRIVILEGES ON `{$values['DB_NAME']}`.* TO '{$values['DB_USER']}'@'127.0.0.1';";
$socket = is_file('/run/mysqld/mysqld.sock') ? '/run/mysqld/mysqld.sock' : '/var/run/mysqld/mysqld.sock';
$mysqli = new mysqli('localhost', 'root', '', '', 0, $socket);
if ($mysqli->connect_error) {
    fwrite(STDERR, $mysqli->connect_error . "\n");
    exit(1);
}
if (!$mysqli->multi_query($sql)) {
    fwrite(STDERR, $mysqli->error . "\n");
    exit(1);
}
while ($mysqli->more_results()) {
    $mysqli->next_result();
}
define('WP_INSTALLING', true);
require '/var/www/html/wp-load.php';
require ABSPATH . 'wp-admin/includes/upgrade.php';
if (!function_exists('is_blog_installed') || !is_blog_installed()) {
    wp_install('Atropos local fuzz target', 'admin', 'admin@atropos.invalid', false, '', $values['DB_PASSWORD']);
}
file_put_contents('/var/lib/mysql/.atropos-nyx-db-imported', '');
PHP
	chown mysql:mysql /var/lib/mysql/.atropos-nyx-db-initialized 2>/dev/null || true
fi
run_guest_php() {
	/usr/local/lib/atropos-nyx-php/lib/ld-linux-x86-64.so.2 \
		--library-path /usr/local/lib/atropos-nyx-php/lib \
		/usr/local/lib/atropos-nyx-php/php-cli -d auto_prepend_file= -d auto_append_file= \
		-d pcov.enabled=0 "$@"
}
if [[ -f setup.sh ]]; then
	export APP_ROOT=/root/atropos-nyx/webapp
	export OUT=/root/atropos-nyx/out
	bash setup.sh
	chown mysql:mysql /var/lib/mysql/.atropos-nyx-db-imported 2>/dev/null || true
elif [[ "$APP_ID" == wordpress && ! -f app-db.sql ]]; then
	run_guest_php /usr/local/lib/atropos-nyx-php/atropos-wp-install.php
	run_guest_php /usr/local/lib/atropos-nyx-php/atropos-flush-permalinks.php
	chown mysql:mysql /var/lib/mysql/.atropos-nyx-db-imported 2>/dev/null || true
fi
if [[ -f app-db.sql ]]; then
	/usr/local/sbin/atropos-nyx-db-import
fi

if [[ "$NEED_DB" == 1 ]]; then
cat >/etc/systemd/system/atropos-nyx-agent.service <<'UNIT'
[Unit]
Description=Atropos Nyx guest agent
After=atropos-nyx-preimage.service atropos-nyx-db-import.service network.target
Requires=atropos-nyx-preimage.service atropos-nyx-db-import.service

[Service]
Type=simple
WorkingDirectory=/
ExecStart=/usr/local/bin/atropos-nyx-launch
Restart=no

[Install]
WantedBy=multi-user.target
UNIT
cat >/etc/systemd/system/atropos-nyx-preimage.service <<'UNIT'
[Unit]
Description=Create the Nyx pre-snapshot and continue guest boot
After=atropos-nyx-db-import.service
Requires=atropos-nyx-db-import.service
Before=atropos-nyx-agent.service

[Service]
Type=oneshot
ExecStart=/usr/local/bin/atropos-nyx-preimage
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
else
cat >/etc/systemd/system/atropos-nyx-agent.service <<'UNIT'
[Unit]
Description=Atropos Nyx guest agent
After=atropos-nyx-preimage.service network.target
Requires=atropos-nyx-preimage.service

[Service]
Type=simple
WorkingDirectory=/
ExecStart=/usr/local/bin/atropos-nyx-launch
Restart=no

[Install]
WantedBy=multi-user.target
UNIT
cat >/etc/systemd/system/atropos-nyx-preimage.service <<'UNIT'
[Unit]
Description=Create the Nyx pre-snapshot and continue guest boot
Before=atropos-nyx-agent.service

[Service]
Type=oneshot
ExecStart=/usr/local/bin/atropos-nyx-preimage
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
fi
rm -f /etc/systemd/system/atropos-nyx-agent.service.d/diagnostic.conf
rmdir --ignore-fail-on-non-empty /etc/systemd/system/atropos-nyx-agent.service.d 2>/dev/null || true
systemctl daemon-reload
systemctl enable atropos-nyx-agent.service
systemctl enable atropos-nyx-preimage.service
if [[ "$NEED_DB" == 1 ]]; then
	systemctl enable atropos-nyx-db-prepare.service
	systemctl enable atropos-nyx-db-import.service
	systemctl enable mariadb.service
fi
rm -rf /root/atropos-nyx
EOF
chmod 0700 "$BUNDLE_DIR/install-guest.sh"

bundle_files=(nyx-php-runtime.tar.gz atropos_agent atropos-nyx-preimage atropos-nyx-launch webapp app-id install-guest.sh)
if [[ -f "$BUNDLE_DIR/setup.sh" ]]; then
	bundle_files+=(setup.sh)
fi
if [[ -f "$BUNDLE_DIR/auth.py" ]]; then
	bundle_files+=(auth.py)
fi
if [[ -d "$BUNDLE_DIR/out" ]]; then
	bundle_files+=(out)
fi
if [[ -f "$BUNDLE_DIR/app-db.sql" ]]; then
	bundle_files+=(app-db.sql app-db.env)
fi
tar -C "$BUNDLE_DIR" -czf "$BUNDLE_DIR/guest-bundle.tar.gz" "${bundle_files[@]}"
chmod 600 "$BUNDLE_DIR/guest-bundle.tar.gz"
printf 'Nyx guest bundle created at %s\n' "$BUNDLE_DIR/guest-bundle.tar.gz"
if [[ -f "$BUNDLE_DIR/app-db.sql" ]]; then
	printf 'The bundle contains a local copy of the application database and credentials.\n'
fi
