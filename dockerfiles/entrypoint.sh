#!/usr/bin/env bash
# Start one Atropos Nyx worker.
# The host kernel must already have enable_vmware_backdoor=Y. This process
# does not reload KVM, because that would stop every other instance.
set -euo pipefail

if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
	printf 'error: /dev/kvm is not readable and writable.\n' >&2
	printf 'Start the container with --device /dev/kvm. The host KVM module must already be loaded with enable_vmware_backdoor=Y.\n' >&2
	exit 1
fi

if [[ -f /root/.nix-profile/etc/profile.d/nix.sh ]]; then
	# shellcheck disable=SC1091
	. /root/.nix-profile/etc/profile.d/nix.sh
fi
if [[ -f /root/.cargo/env ]]; then
	# shellcheck disable=SC1091
	. /root/.cargo/env
fi

export ATROPOS_NYX_DATA_DIR="${ATROPOS_NYX_DATA_DIR:-/var/lib/atropos/nyx}"
export ATROPOS_LEGACY_ROOT="${ATROPOS_LEGACY_ROOT:-/opt/atropos-legacy}"
export ATROPOS_WORDPRESS_ROOT="${ATROPOS_WORDPRESS_ROOT:-/opt/wordpress}"
export ATROPOS_NYX_CPU="${ATROPOS_NYX_CPU:-0}"
export ATROPOS_NYX_VM_DIR="${ATROPOS_NYX_VM_DIR:-$ATROPOS_NYX_DATA_DIR/vm}"
export ATROPOS_NYX_VM_IMAGE="${ATROPOS_NYX_VM_IMAGE:-$ATROPOS_NYX_VM_DIR/atropos-nyx.qcow2}"
export ATROPOS_NYX_PRESNAPSHOT="${ATROPOS_NYX_PRESNAPSHOT:-$ATROPOS_NYX_VM_DIR/presnapshot}"
export ATROPOS_NYX_WORKDIR="${ATROPOS_NYX_WORKDIR:-$ATROPOS_NYX_DATA_DIR/workdir-$ATROPOS_NYX_CPU}"
export ATROPOS_NYX_SHARE="${ATROPOS_NYX_SHARE:-$ATROPOS_NYX_DATA_DIR/share-$ATROPOS_NYX_CPU}"

REPO_ROOT=/opt/atropos-libafl
cd "$REPO_ROOT"
mkdir -p "$ATROPOS_NYX_DATA_DIR"
chmod 700 "$ATROPOS_NYX_DATA_DIR"

snapshot_ready() {
	[[ -f "$ATROPOS_NYX_VM_IMAGE" && -d "$ATROPOS_NYX_PRESNAPSHOT" ]] &&
		find "$ATROPOS_NYX_PRESNAPSHOT" -mindepth 1 -print -quit | grep -q .
}

start_mariadb() {
	if mariadb-admin --no-defaults --protocol=TCP --host=127.0.0.1 --port=33060 ping --silent >/dev/null 2>&1; then
		return 0
	fi
	# Bring up the database created by scripts/setup-wordpress.sh. systemd is
	# not running in this container, so start mariadbd directly.
	bash "$REPO_ROOT/scripts/setup-wordpress.sh"
}

exec 9>"$ATROPOS_NYX_DATA_DIR/.provision.lock"
flock 9
if ! snapshot_ready; then
	printf 'Nyx guest image or pre-snapshot is missing; provisioning into %s\n' "$ATROPOS_NYX_DATA_DIR"
	start_mariadb
	bash "$REPO_ROOT/scripts/package-nyx-guest.sh"
	bash "$REPO_ROOT/scripts/create-nyx-vm.sh"
fi
if ! snapshot_ready; then
	printf 'error: provisioning finished but %s or %s is still missing\n' \
		"$ATROPOS_NYX_VM_IMAGE" "$ATROPOS_NYX_PRESNAPSHOT" >&2
	exit 1
fi
flock -u 9

bash "$REPO_ROOT/scripts/prepare-nyx-share.sh"
exec bash "$REPO_ROOT/scripts/run-fuzzer.sh" \
	--nyx-share "$ATROPOS_NYX_SHARE" \
	--nyx-workdir "$ATROPOS_NYX_WORKDIR" \
	"$@"
