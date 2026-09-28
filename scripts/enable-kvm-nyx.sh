#!/usr/bin/env bash
set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
	exec sudo -E "$0" "$@"
fi

config_file=/etc/modprobe.d/atropos-nyx.conf
cat >"$config_file" <<'EOF'
options kvm enable_vmware_backdoor=Y
EOF

accelerator_module=""
if [[ -d /sys/module/kvm_intel ]]; then
	accelerator_module=kvm_intel
elif [[ -d /sys/module/kvm_amd ]]; then
	accelerator_module=kvm_amd
else
	printf 'Neither kvm_intel nor kvm_amd is currently loaded.\n' >&2
	exit 1
fi

modprobe -r "$accelerator_module"
modprobe -r kvm
modprobe kvm enable_vmware_backdoor=Y
modprobe "$accelerator_module"

if [[ -n "${SUDO_USER:-}" ]] && ! id -nG "$SUDO_USER" | tr ' ' '\n' | grep -qx kvm; then
	usermod -aG kvm "$SUDO_USER"
	printf 'Added %s to the kvm group; log out and back in for group membership to refresh.\n' "$SUDO_USER"
fi

printf 'enable_vmware_backdoor=%s\n' "$(cat /sys/module/kvm/parameters/enable_vmware_backdoor)"
printf 'config=%s\n' "$config_file"
test -r /dev/kvm && test -w /dev/kvm
printf '/dev/kvm is accessible to this process.\n'
