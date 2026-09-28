#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
WP_ROOT="$(cd -- "${ATROPOS_WORDPRESS_ROOT:-$REPO_ROOT/../wordpress}" && pwd)"
PHP_PREFIX="${ATROPOS_PHP_PREFIX:-${ATROPOS_NYX_PHP_PREFIX:-$HOME/.local/opt/atropos-libafl-nyx-php}}"
CONFIG_DIR="$HOME/.config/atropos-libafl"
SECRET_FILE="$CONFIG_DIR/wordpress-db.env"
DB_DIR="${ATROPOS_MARIADB_DATA_DIR:-$HOME/.local/share/atropos-libafl/mariadb}"
DB_SOCKET="$DB_DIR/mariadb.sock"
DB_PID="$DB_DIR/mariadb.pid"
DB_LOG="$DB_DIR/mariadb.log"
DB_SERVICE_NAME="atropos-libafl-mariadb.service"
DB_SERVICE_FILE="$HOME/.config/systemd/user/$DB_SERVICE_NAME"

if [[ ! -x "$PHP_PREFIX/bin/php" ]]; then
	printf 'PHP CLI not found at %s/bin/php; run scripts/build-nyx-php.sh first\n' "$PHP_PREFIX" >&2
	exit 1
fi
if [[ ! -f "$WP_ROOT/wp-settings.php" ]]; then
	printf 'WordPress source not found under %s; set ATROPOS_WORDPRESS_ROOT\n' "$WP_ROOT" >&2
	exit 1
fi

mkdir -p "$CONFIG_DIR"
chmod 700 "$CONFIG_DIR"
if [[ ! -f "$SECRET_FILE" ]]; then
	cat > "$SECRET_FILE" <<EOF
MARIADB_ROOT_PASSWORD=$(openssl rand -hex 32)
MARIADB_DATABASE=wordpress
MARIADB_USER=atropos
MARIADB_PASSWORD=$(openssl rand -hex 32)
ATROPOS_WP_ADMIN_PASSWORD=$(openssl rand -hex 24)
EOF
	chmod 600 "$SECRET_FILE"
fi
set -a
# The local secrets file is generated here with hex-only values.
# shellcheck disable=SC1090
. "$SECRET_FILE"
set +a

if [[ ! "$MARIADB_DATABASE" =~ ^[A-Za-z0-9_]+$ || ! "$MARIADB_USER" =~ ^[A-Za-z0-9_]+$ ||
	! "$MARIADB_ROOT_PASSWORD" =~ ^[[:xdigit:]]{64}$ || ! "$MARIADB_PASSWORD" =~ ^[[:xdigit:]]{64}$ ||
	! "$ATROPOS_WP_ADMIN_PASSWORD" =~ ^[[:xdigit:]]{48}$ ]]; then
	printf 'Unexpected value in %s; expected the generated local hex credentials.\n' "$SECRET_FILE" >&2
	exit 1
fi

ATROPOS_NIXPKGS_PATH="$(nix --extra-experimental-features 'nix-command flakes' eval --impure --raw \
	--expr 'builtins.fetchTarball "https://channels.nixos.org/nixos-22.11/nixexprs.tar.xz"')"
export NIX_PATH="nixpkgs=$ATROPOS_NIXPKGS_PATH${NIX_PATH:+:$NIX_PATH}"
MARIADB_PREFIX="$(nix --extra-experimental-features 'nix-command flakes' eval --impure --raw \
	--expr 'let pkgs = import <nixpkgs> {}; in pkgs.mariadb.outPath')"
MARIADB_BIN="$MARIADB_PREFIX/bin"

mkdir -p "$DB_DIR"
chmod 700 "$DB_DIR"
MARIADB_BASEDIR="$MARIADB_PREFIX"
if [[ ! -d "$DB_DIR/mysql" ]]; then
	if ! "$MARIADB_BIN/mariadb-install-db" --no-defaults --basedir="$MARIADB_BASEDIR" \
		--datadir="$DB_DIR" --user="$(id -un)" \
		--auth-root-authentication-method=normal --skip-test-db >"$DB_DIR/install.log" 2>&1; then
		tail -n 50 "$DB_DIR/install.log" >&2
		exit 1
	fi
fi

mkdir -p "$(dirname -- "$DB_SERVICE_FILE")"
cat > "$DB_SERVICE_FILE" <<EOF
[Unit]
Description=Atropos local MariaDB database
After=network.target

[Service]
Type=simple
ExecStart=$MARIADB_BIN/mariadbd --no-defaults --basedir=$MARIADB_BASEDIR --datadir=$DB_DIR --user=$(id -un) --socket=$DB_SOCKET --pid-file=$DB_PID --bind-address=127.0.0.1 --port=33060 --log-error=$DB_LOG
Restart=on-failure
RestartSec=2
UMask=0077

[Install]
WantedBy=default.target
EOF
chmod 600 "$DB_SERVICE_FILE"
if ! systemctl --user show-environment >/dev/null 2>&1; then
	printf 'A running systemd user manager is required to keep MariaDB available after setup.\n' >&2
	exit 1
fi
systemctl --user daemon-reload
if ! systemctl --user is-active --quiet "$DB_SERVICE_NAME"; then
	rm -f -- "$DB_SOCKET" "$DB_PID"
	systemctl --user enable --now "$DB_SERVICE_NAME"
fi

ready=0
for _ in $(seq 1 60); do
	if "$MARIADB_BIN/mariadb-admin" --no-defaults --socket="$DB_SOCKET" -uroot ping --silent >/dev/null 2>&1; then
		ready=1
		break
	fi
	sleep 1
done
if [[ "$ready" -ne 1 ]]; then
	printf 'MariaDB did not become ready; inspect %s\n' "$DB_LOG" >&2
	tail -n 50 "$DB_LOG" >&2 || true
	exit 1
fi

ROOT_AUTH_ARGS=()
if ! "$MARIADB_BIN/mariadb" --no-defaults --protocol=socket --socket="$DB_SOCKET" -uroot -e 'SELECT 1' >/dev/null 2>&1; then
	ROOT_AUTH_ARGS+=("--password=$MARIADB_ROOT_PASSWORD")
fi
"$MARIADB_BIN/mariadb" --no-defaults --protocol=socket --socket="$DB_SOCKET" -uroot "${ROOT_AUTH_ARGS[@]}" <<SQL
ALTER USER 'root'@'localhost' IDENTIFIED BY '$MARIADB_ROOT_PASSWORD';
CREATE DATABASE IF NOT EXISTS \`$MARIADB_DATABASE\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE USER IF NOT EXISTS '$MARIADB_USER'@'localhost' IDENTIFIED BY '$MARIADB_PASSWORD';
CREATE USER IF NOT EXISTS '$MARIADB_USER'@'127.0.0.1' IDENTIFIED BY '$MARIADB_PASSWORD';
ALTER USER '$MARIADB_USER'@'localhost' IDENTIFIED BY '$MARIADB_PASSWORD';
ALTER USER '$MARIADB_USER'@'127.0.0.1' IDENTIFIED BY '$MARIADB_PASSWORD';
GRANT ALL PRIVILEGES ON \`$MARIADB_DATABASE\`.* TO '$MARIADB_USER'@'localhost';
GRANT ALL PRIVILEGES ON \`$MARIADB_DATABASE\`.* TO '$MARIADB_USER'@'127.0.0.1';
SQL

if [[ ! -f "$WP_ROOT/wp-config.php" ]]; then
	cat > "$WP_ROOT/wp-config.php" <<EOF
<?php
define( 'DB_NAME', '$MARIADB_DATABASE' );
define( 'DB_USER', '$MARIADB_USER' );
define( 'DB_PASSWORD', '$MARIADB_PASSWORD' );
define( 'DB_HOST', '127.0.0.1:33060' );
define( 'DB_CHARSET', 'utf8mb4' );
define( 'DB_COLLATE', '' );
define( 'AUTH_KEY', '$(openssl rand -hex 32)' );
define( 'SECURE_AUTH_KEY', '$(openssl rand -hex 32)' );
define( 'LOGGED_IN_KEY', '$(openssl rand -hex 32)' );
define( 'NONCE_KEY', '$(openssl rand -hex 32)' );
define( 'AUTH_SALT', '$(openssl rand -hex 32)' );
define( 'SECURE_AUTH_SALT', '$(openssl rand -hex 32)' );
define( 'LOGGED_IN_SALT', '$(openssl rand -hex 32)' );
define( 'NONCE_SALT', '$(openssl rand -hex 32)' );
\$table_prefix = 'wp_';
define( 'WP_HOME', 'http://127.0.0.1' );
define( 'WP_SITEURL', 'http://127.0.0.1' );
define( 'WP_DEBUG', true );
define( 'WP_DEBUG_DISPLAY', false );
if ( ! defined( 'ABSPATH' ) ) {
	define( 'ABSPATH', __DIR__ . '/' );
}
require_once ABSPATH . 'wp-settings.php';
EOF
	chmod 600 "$WP_ROOT/wp-config.php"

	if git -C "$WP_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
		exclude_file="$(git -C "$WP_ROOT" rev-parse --git-path info/exclude)"
		if ! rg -Fxq 'wp-config.php' "$exclude_file" 2>/dev/null; then
			printf '\n/wp-config.php\n' >> "$exclude_file"
		fi
	fi
fi

export ATROPOS_WORDPRESS_ROOT="$WP_ROOT"
ATROPOS_PHP_PREFIX="$PHP_PREFIX" ATROPOS_WORDPRESS_ROOT="$WP_ROOT" \
ATROPOS_WP_ADMIN_PASSWORD="$ATROPOS_WP_ADMIN_PASSWORD" \
	"$PHP_PREFIX/bin/php" -r '
define("WP_INSTALLING", true);
require getenv("ATROPOS_WORDPRESS_ROOT") . "/wp-load.php";
require ABSPATH . "wp-admin/includes/upgrade.php";
if (function_exists("is_blog_installed") && is_blog_installed()) { exit(0); }
wp_install("Atropos local fuzz target", "admin", "admin@atropos.invalid", false, "", getenv("ATROPOS_WP_ADMIN_PASSWORD"));
' >/dev/null

printf 'WordPress database is ready at 127.0.0.1:33060.\n'
printf 'MariaDB data: %s\n' "$DB_DIR"
printf 'MariaDB user service: systemctl --user status %s\n' "$DB_SERVICE_NAME"
printf 'The local-only admin password is stored in %s\n' "$SECRET_FILE"
