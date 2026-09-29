#!/usr/bin/env bash
set -euo pipefail
umask 077

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
if [[ "${ATROPOS_NYX_BUILD_SHELL:-0}" != 1 ]]; then
	export ATROPOS_NYX_BUILD_SHELL=1
	exec "$SCRIPT_DIR/with-nyx-build-deps.sh" "$0" "$@"
fi

LEGACY_ROOT="${ATROPOS_LEGACY_ROOT:-$(cd -- "$REPO_ROOT/../atropos-legacy" && pwd)}"
ARTIFACT_DIR="${ATROPOS_NYX_GUEST_ARTIFACTS:-${XDG_DATA_HOME:-$HOME/.local/share}/atropos-libafl/nyx/guest}"
WP_ROOT="${ATROPOS_WORDPRESS_ROOT:-$REPO_ROOT/../wordpress}"
WP_ROOT="$(realpath -- "$WP_ROOT")"
SECRET_FILE="${ATROPOS_WORDPRESS_SECRET_FILE:-$HOME/.config/atropos-libafl/wordpress-db.env}"
DATA_DIR="${ATROPOS_NYX_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/atropos-libafl/nyx}"
BUNDLE_DIR="$DATA_DIR/bundle"

for path in "$ARTIFACT_DIR/nyx-php-runtime.tar.gz" "$ARTIFACT_DIR/php-code-coverage-runtime" \
	"$ARTIFACT_DIR/atropos-agent-phpcov-runtime" "$ARTIFACT_DIR/atropos_agent" \
	"$WP_ROOT/index.php" "$WP_ROOT/wp-config.php" "$SECRET_FILE" \
	"$LEGACY_ROOT/fuzzer/nyx.h" "$SCRIPT_DIR/nyx-guest-launch.sh"; do
	if [[ ! -e "$path" ]]; then
		printf 'Required Nyx guest input is missing: %s\n' "$path" >&2
		exit 1
	fi
done
if ! command -v mariadb-dump >/dev/null 2>&1; then
	printf 'mariadb-dump is unavailable; run this script through scripts/with-nyx-build-deps.sh\n' >&2
	exit 1
fi
if ! systemctl --user is-active --quiet atropos-libafl-mariadb.service; then
	printf 'The local Atropos MariaDB service is not active; run scripts/setup-wordpress.sh first\n' >&2
	exit 1
fi

set -a
# This file is generated locally by setup-wordpress.sh and contains validated hex credentials.
# shellcheck disable=SC1090
. "$SECRET_FILE"
set +a
if [[ ! "$MARIADB_DATABASE" =~ ^[A-Za-z0-9_]+$ || ! "$MARIADB_USER" =~ ^[A-Za-z0-9_]+$ ||
	! "$MARIADB_PASSWORD" =~ ^[[:xdigit:]]{64}$ ]]; then
	printf 'Unexpected database values in %s\n' "$SECRET_FILE" >&2
	exit 1
fi

mkdir -p "$BUNDLE_DIR"
chmod 700 "$DATA_DIR" "$BUNDLE_DIR"
rm -f -- "$BUNDLE_DIR/wordpress-db.sql" "$BUNDLE_DIR/wordpress-db.env" "$BUNDLE_DIR/guest-bundle.tar.gz"
MYSQL_PWD="$MARIADB_PASSWORD" mariadb-dump \
	--protocol=TCP --host=127.0.0.1 --port=33060 \
	--user="$MARIADB_USER" --single-transaction --skip-lock-tables \
	--databases "$MARIADB_DATABASE" > "$BUNDLE_DIR/wordpress-db.sql"
chmod 600 "$BUNDLE_DIR/wordpress-db.sql"

cat >"$BUNDLE_DIR/wordpress-db.env" <<EOF
MARIADB_DATABASE=$MARIADB_DATABASE
MARIADB_USER=$MARIADB_USER
MARIADB_PASSWORD=$MARIADB_PASSWORD
EOF
chmod 600 "$BUNDLE_DIR/wordpress-db.env"

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
"${HOST_CC[@]}" -static -O2 -I"$LEGACY_ROOT/fuzzer" \
	-o "$BUNDLE_DIR/atropos-nyx-preimage" "$BUNDLE_DIR/nyx-preimage.c"
rm -f -- "$BUNDLE_DIR/nyx-preimage.c"
chmod 0755 "$BUNDLE_DIR/atropos-nyx-preimage"
rm -rf -- "$BUNDLE_DIR/wordpress"
mkdir -m 700 "$BUNDLE_DIR/wordpress"
tar --exclude=.git -C "$WP_ROOT" -cf - . | tar -C "$BUNDLE_DIR/wordpress" -xf -
if [[ -f "$BUNDLE_DIR/wordpress/wp-config.php" ]]; then
	python3 - "$BUNDLE_DIR/wordpress/wp-config.php" <<'PY'
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
# shellcheck disable=SC1091
. ./wordpress-db.env

if [[ ! "$MARIADB_DATABASE" =~ ^[A-Za-z0-9_]+$ || ! "$MARIADB_USER" =~ ^[A-Za-z0-9_]+$ ||
	! "$MARIADB_PASSWORD" =~ ^[[:xdigit:]]{64}$ ]]; then
	echo 'Invalid local WordPress database configuration' >&2
	exit 1
fi

mkdir -p /var/www/html
cp -a wordpress/. /var/www/html/
chown -R www-data:www-data /var/www/html
install -d -o root -g root -m 0700 /usr/local/lib/atropos-nyx-db
install -o root -g root -m 0600 wordpress-db.env /usr/local/lib/atropos-nyx-db/wordpress-db.env
install -o root -g root -m 0600 wordpress-db.sql /usr/local/lib/atropos-nyx-db/wordpress-db.sql
cat >/etc/mysql/mariadb.conf.d/90-atropos-nyx.cnf <<'MYSQL'
[mysqld]
innodb_use_native_aio=0
innodb_flush_method=fsync
MYSQL
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
. /usr/local/lib/atropos-nyx-db/wordpress-db.env
if [[ ! "$MARIADB_DATABASE" =~ ^[A-Za-z0-9_]+$ || ! "$MARIADB_USER" =~ ^[A-Za-z0-9_]+$ ||
	! "$MARIADB_PASSWORD" =~ ^[[:xdigit:]]{64}$ ]]; then
	echo 'Invalid local WordPress database configuration' >&2
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
mariadb --protocol=socket -uroot < /usr/local/lib/atropos-nyx-db/wordpress-db.sql
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
Description=Import the Atropos WordPress database snapshot
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
install -d -o root -g root -m 0755 /usr/local/lib/atropos-nyx-php
tar -xzf nyx-php-runtime.tar.gz -C /usr/local/lib/atropos-nyx-php
cp -- atropos_agent /usr/local/bin/atropos_agent
cp -- atropos-nyx-preimage /usr/local/bin/atropos-nyx-preimage
cp -- atropos-nyx-launch /usr/local/bin/atropos-nyx-launch
chmod 0755 /usr/local/bin/atropos_agent \
	/usr/local/bin/atropos-nyx-preimage /usr/local/bin/atropos-nyx-launch \
	/usr/local/lib/atropos-nyx-php/target_executable \
	/usr/local/lib/atropos-nyx-php/pcov.so
chmod 0644 /usr/local/lib/atropos-nyx-php/php.ini
if [[ -f /usr/local/lib/atropos-nyx-php/opcache.so ]]; then
	chmod 0644 /usr/local/lib/atropos-nyx-php/opcache.so
fi

cat >/etc/systemd/system/atropos-nyx-agent.service <<'UNIT'
[Unit]
Description=Atropos Nyx guest agent
After=atropos-nyx-preimage.service atropos-nyx-db-import.service network.target
Requires=atropos-nyx-preimage.service atropos-nyx-db-import.service

[Service]
Type=simple
WorkingDirectory=/
ExecStart=/usr/local/bin/atropos-nyx-launch
Restart=on-failure
RestartSec=1

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
systemctl enable atropos-nyx-agent.service
systemctl enable atropos-nyx-preimage.service
systemctl enable atropos-nyx-db-prepare.service
systemctl enable atropos-nyx-db-import.service
systemctl enable mariadb.service
rm -rf /root/atropos-nyx
EOF
chmod 0700 "$BUNDLE_DIR/install-guest.sh"

tar -C "$BUNDLE_DIR" -czf "$BUNDLE_DIR/guest-bundle.tar.gz" \
	nyx-php-runtime.tar.gz atropos_agent atropos-nyx-preimage atropos-nyx-launch \
	wordpress wordpress-db.sql wordpress-db.env install-guest.sh
chmod 600 "$BUNDLE_DIR/guest-bundle.tar.gz"
printf 'Nyx guest bundle created at %s\n' "$BUNDLE_DIR/guest-bundle.tar.gz"
printf 'The bundle contains a local copy of the WordPress database and credentials.\n'
