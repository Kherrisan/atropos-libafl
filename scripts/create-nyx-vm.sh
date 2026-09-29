#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

if [[ "${ATROPOS_NYX_BUILD_SHELL:-0}" != 1 ]]; then
	export ATROPOS_NYX_BUILD_SHELL=1
	exec "$SCRIPT_DIR/with-nyx-build-deps.sh" "$0" "$@"
fi

DATA_DIR="${ATROPOS_NYX_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/atropos-libafl/nyx}"
VM_DIR="${ATROPOS_NYX_VM_DIR:-$DATA_DIR/vm}"
BASE_IMAGE="$VM_DIR/noble-server-cloudimg-amd64.img"
VM_IMAGE="${ATROPOS_NYX_VM_IMAGE:-$VM_DIR/atropos-nyx.qcow2}"
PAYLOAD_ISO="$VM_DIR/atropos-payload.iso"
SEED_ISO="$VM_DIR/cloud-init-seed.iso"
PAYLOAD_DIR="$VM_DIR/payload"
BUNDLE="${ATROPOS_NYX_GUEST_BUNDLE:-$DATA_DIR/bundle/guest-bundle.tar.gz}"
QEMU_NYX="${ATROPOS_NYX_QEMU:-$REPO_ROOT/target/nyx/QEMU-Nyx/x86_64-softmmu/qemu-system-x86_64}"
QEMU_TCG="${ATROPOS_NYX_TCG_QEMU:-$(command -v qemu-system-x86_64 || true)}"
DISK_SIZE_GB="${ATROPOS_NYX_DISK_GB:-32}"
MEMORY_MB="${ATROPOS_NYX_MEMORY_MB:-8192}"
CPU_COUNT="${ATROPOS_NYX_BOOT_CPUS:-1}"
PREIMAGE="${ATROPOS_NYX_PRESNAPSHOT:-$VM_DIR/presnapshot}"

for command in curl sha256sum awk qemu-img cloud-localds genisoimage timeout; do
	if ! command -v "$command" >/dev/null 2>&1; then
		printf 'Missing build tool %s; run through scripts/with-nyx-build-deps.sh\n' "$command" >&2
		exit 1
	fi
done
if [[ ! -x "$QEMU_NYX" ]]; then
	printf 'Nyx QEMU is missing at %s; run scripts/build-nyx-fuzzer.sh first\n' "$QEMU_NYX" >&2
	exit 1
fi
if [[ ! -x "$QEMU_TCG" ]]; then
	printf 'Standard QEMU is missing; install qemu-system-x86 or set ATROPOS_NYX_TCG_QEMU to its executable path\n' >&2
	exit 1
fi
if [[ ! -f "$BUNDLE" ]]; then
	printf 'Nyx guest bundle is missing at %s; build PHP/PCOV and run scripts/package-nyx-guest.sh first\n' "$BUNDLE" >&2
	exit 1
fi
if [[ ! "$DISK_SIZE_GB" =~ ^[0-9]+$ || "$DISK_SIZE_GB" -lt 24 ]]; then
	printf 'ATROPOS_NYX_DISK_GB must be an integer of at least 24\n' >&2
	exit 1
fi

mkdir -p "$VM_DIR"
chmod 700 "$DATA_DIR" "$VM_DIR"

if [[ ! -f "$BASE_IMAGE" ]]; then
	IMAGE_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"
	CHECKSUM_URL="https://cloud-images.ubuntu.com/noble/current/SHA256SUMS"
	curl --fail --location --retry 3 --continue-at - "$IMAGE_URL" --output "$BASE_IMAGE.part"
	curl --fail --location --retry 3 "$CHECKSUM_URL" --output "$VM_DIR/SHA256SUMS"
	EXPECTED_SHA="$(awk '$2 ~ /^\*?noble-server-cloudimg-amd64\.img$/ { print $1; exit }' "$VM_DIR/SHA256SUMS")"
	if [[ ! "$EXPECTED_SHA" =~ ^[[:xdigit:]]{64}$ ]]; then
		rm -f -- "$BASE_IMAGE.part"
		printf 'Official Ubuntu checksum file did not contain the expected image entry.\n' >&2
		exit 1
	fi
	printf '%s  %s\n' "$EXPECTED_SHA" "$BASE_IMAGE.part" | sha256sum --check --status || {
		rm -f -- "$BASE_IMAGE.part"
		printf 'Ubuntu cloud image checksum verification failed.\n' >&2
		exit 1
	}
	mv -- "$BASE_IMAGE.part" "$BASE_IMAGE"
	chmod 600 "$BASE_IMAGE"
fi

if [[ ! -f "$VM_IMAGE" ]]; then
	qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMAGE" "$VM_IMAGE" "${DISK_SIZE_GB}G"
	chmod 600 "$VM_IMAGE"
fi

mkdir -p "$PAYLOAD_DIR"
cp -- "$BUNDLE" "$PAYLOAD_DIR/guest-bundle.tar.gz"
chmod 600 "$PAYLOAD_DIR/guest-bundle.tar.gz"
genisoimage -quiet -output "$PAYLOAD_ISO" -volid ATROPOSPAYLOAD -joliet -rock "$PAYLOAD_DIR"
chmod 600 "$PAYLOAD_ISO"

cat >"$VM_DIR/user-data" <<'USERDATA'
#cloud-config
write_files:
  - path: /root/bootstrap-nyx-guest.sh
    owner: root:root
    permissions: '0700'
    content: |
      #!/usr/bin/env bash
      set -euxo pipefail
      export DEBIAN_FRONTEND=noninteractive
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
      if ! dpkg-query -W -f='${Status}' mariadb-server 2>/dev/null | grep -qx 'install ok installed'; then
        apt-get update
        apt-get install -y mariadb-server
      fi
      rm -f /usr/sbin/policy-rc.d
      mkdir -p /mnt/atropos-payload
      mount -L ATROPOSPAYLOAD /mnt/atropos-payload
      mkdir -p /root/atropos-nyx
      tar -xzf /mnt/atropos-payload/guest-bundle.tar.gz -C /root/atropos-nyx
      chmod 0700 /root/atropos-nyx/install-guest.sh
      if ! /root/atropos-nyx/install-guest.sh; then
        systemctl status mariadb.service --no-pager --full || true
        journalctl -b -u mariadb.service --no-pager --full || true
        cat /var/log/mysql/error.log || true
        exit 1
      fi
      sync
      echo ATROPOS_NYX_GUEST_READY
      shutdown -h now
runcmd:
  - [bash, /root/bootstrap-nyx-guest.sh]
USERDATA
printf 'instance-id: atropos-nyx-local-%s\nlocal-hostname: atropos-nyx\n' "$(date +%s)" >"$VM_DIR/meta-data"
cloud-localds "$SEED_ISO" "$VM_DIR/user-data" "$VM_DIR/meta-data"
chmod 600 "$SEED_ISO" "$VM_DIR/user-data" "$VM_DIR/meta-data"

if [[ "${ATROPOS_NYX_SKIP_CLOUD_INIT:-0}" != 1 ]]; then
	if [[ ! -e "$VM_DIR/cloud-init-complete" ]]; then
		printf 'Provisioning Ubuntu guest; cloud-init installs MariaDB and imports the local WordPress snapshot.\n'
		set +e
		timeout --signal=TERM 30m "$QEMU_TCG" \
			-accel tcg -machine pc -cpu qemu64 -smp "$CPU_COUNT" -m "$MEMORY_MB" \
			-boot order=c \
			-drive "file=$VM_IMAGE,format=qcow2,if=virtio" \
			-drive "file=$PAYLOAD_ISO,format=raw,media=cdrom,readonly=on" \
			-drive "file=$SEED_ISO,format=raw,media=cdrom,readonly=on" \
			-netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
			-display none -serial stdio -monitor none -no-reboot 2>&1 | tee "$VM_DIR/cloud-init.log"
		QEMU_STATUS="${PIPESTATUS[0]}"
		set -e
		if [[ "$QEMU_STATUS" -ne 0 ]] || ! rg -Fq 'ATROPOS_NYX_GUEST_READY' "$VM_DIR/cloud-init.log"; then
			printf 'Guest cloud-init did not shut down cleanly (QEMU status %s). Inspect %s and rerun after fixing the guest boot.\n' \
				"$QEMU_STATUS" "$VM_DIR/cloud-init.log" >&2
			exit 1
		fi
		touch "$VM_DIR/cloud-init-complete"
		chmod 600 "$VM_DIR/cloud-init-complete"
	else
		printf 'Cloud-init is already marked complete at %s\n' "$VM_DIR/cloud-init-complete"
	fi
fi

if [[ "$(cat /sys/module/kvm/parameters/enable_vmware_backdoor 2>/dev/null || true)" != Y ]]; then
	cat >&2 <<EOF
The VM image is provisioned, but Nyx pre-snapshot creation needs KVM VMware backdoor support.
Current kernel parameter: $(cat /sys/module/kvm/parameters/enable_vmware_backdoor 2>/dev/null || echo unavailable)
Run scripts/enable-kvm-nyx.sh, then rerun scripts/create-nyx-vm.sh.
EOF
	exit 1
fi
if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
	printf 'The current user cannot access /dev/kvm. Log out and back in after joining the kvm group.\n' >&2
	exit 1
fi

if [[ -f "$VM_DIR/preimage-complete" ]]; then
	if [[ ! -d "$PREIMAGE" ]] || ! find "$PREIMAGE" -mindepth 1 -print -quit | rg -q .; then
		printf 'Pre-snapshot completion marker exists but the directory is incomplete: %s\n' "$PREIMAGE" >&2
		exit 1
	fi
elif [[ -d "$PREIMAGE" ]] && find "$PREIMAGE" -mindepth 1 -print -quit | rg -q .; then
	printf 'A partial pre-snapshot already exists at %s. Inspect it and remove it manually before retrying.\n' "$PREIMAGE" >&2
	exit 1
else
	mkdir -p "$PREIMAGE"
	chmod 700 "$PREIMAGE"
	PREIMAGE_LOG="$VM_DIR/preimage-serial.log"
	printf 'Booting the Nyx guest once to create its pre-snapshot.\n'
	set +e
		timeout --signal=TERM 5m env NYX_DISABLE_DIRTY_RING=y "$QEMU_NYX" \
		-enable-kvm -machine kAFL64-v1 -cpu kAFL64-Hypervisor-v2 \
		-smp 1 -m "$MEMORY_MB" -drive "file=$VM_IMAGE,format=qcow2,index=0,media=disk" \
		-k de -net none -display none -serial "file:$PREIMAGE_LOG" -monitor none \
		-fast_vm_reload "pre_path=$PREIMAGE,load=off"
	QEMU_STATUS=$?
	set -e
	if [[ "$QEMU_STATUS" -ne 0 ]] || ! find "$PREIMAGE" -mindepth 1 -print -quit | rg -q .; then
		tail -n 80 "$PREIMAGE_LOG" 2>/dev/null || true
		printf 'Nyx pre-snapshot creation failed (QEMU status %s). See %s\n' "$QEMU_STATUS" "$PREIMAGE_LOG" >&2
		exit 1
	fi
	touch "$VM_DIR/preimage-complete"
	chmod 600 "$VM_DIR/preimage-complete"
fi

ATROPOS_NYX_VM_DIR="$VM_DIR" ATROPOS_NYX_QEMU="$QEMU_NYX" \
	ATROPOS_NYX_VM_IMAGE="$VM_IMAGE" ATROPOS_NYX_PRESNAPSHOT="$PREIMAGE" \
	ATROPOS_NYX_DATA_DIR="$DATA_DIR" "$SCRIPT_DIR/prepare-nyx-share.sh"
printf '\nNyx VM and share configuration are ready. Run scripts/run-fuzzer.sh to start fuzzing.\n'
