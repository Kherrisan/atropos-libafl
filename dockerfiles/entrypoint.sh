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

export NYX_HOME="${NYX_HOME:-${ATROPOS_NYX_DATA_DIR:-/var/lib/atropos/nyx}}"
export ATROPOS_NYX_CPU="${ATROPOS_NYX_CPU:-0}"

REPO_ROOT=/opt/atropos-libafl
cd "$REPO_ROOT"
mkdir -p "$NYX_HOME"
chmod 700 "$NYX_HOME"

PROJECT="${ATROPOS_PROJECT:-wordpress}"
case "$PROJECT" in
wordpress)
	PROJECT_YAML="${ATROPOS_PROJECT_YAML:-$REPO_ROOT/projects/wordpress/project.yaml}"
	PROJECT_SOURCE="${ATROPOS_SOURCE:-/opt/wordpress}"
	;;
webgoat)
	PROJECT_YAML="${ATROPOS_PROJECT_YAML:-$REPO_ROOT/projects/webgoat/project.yaml}"
	PROJECT_SOURCE="${ATROPOS_SOURCE:-}"
	;;
*)
	PROJECT_YAML="${ATROPOS_PROJECT_YAML:?set ATROPOS_PROJECT_YAML for project $PROJECT}"
	PROJECT_SOURCE="${ATROPOS_SOURCE:-}"
	;;
esac

# shellcheck disable=SC1090
eval "$(python3 "$REPO_ROOT/scripts/load-project.py" shell "$PROJECT_YAML")"
if [[ "$PROJECT_BACKEND" == spring ]]; then
	RUNTIME_ROOT="$NYX_HOME/spring"
else
	RUNTIME_ROOT="$NYX_HOME"
fi
export ATROPOS_NYX_VM_DIR="${ATROPOS_NYX_VM_DIR:-$RUNTIME_ROOT/vm}"
export ATROPOS_NYX_VM_IMAGE="${ATROPOS_NYX_VM_IMAGE:-$ATROPOS_NYX_VM_DIR/atropos-nyx.qcow2}"
export ATROPOS_NYX_PRESNAPSHOT="${ATROPOS_NYX_PRESNAPSHOT:-$ATROPOS_NYX_VM_DIR/presnapshot}"
export ATROPOS_NYX_SHARE="${ATROPOS_NYX_SHARE:-$RUNTIME_ROOT/share}"
export ATROPOS_NYX_WORKDIR="${ATROPOS_NYX_WORKDIR:-$RUNTIME_ROOT/workdir-$ATROPOS_NYX_CPU}"

project_args=(--project "$PROJECT_YAML")
if [[ "$PROJECT_BACKEND" != spring ]]; then
	project_args+=(--php-output "$NYX_HOME/guest")
fi
if [[ -n "$PROJECT_SOURCE" ]]; then
	project_args+=("$PROJECT_SOURCE")
fi

snapshot_ready() {
	[[ -f "$ATROPOS_NYX_VM_IMAGE" && -d "$ATROPOS_NYX_PRESNAPSHOT" ]] &&
		find "$ATROPOS_NYX_PRESNAPSHOT" -mindepth 1 -print -quit | grep -q .
}

exec 9>"$NYX_HOME/.provision.lock"
flock 9
if [[ "$PROJECT_BACKEND" == spring ]]; then
	bundle="$NYX_HOME/spring/bundle/guest-bundle.tar.gz"
else
	bundle="$NYX_HOME/bundle/guest-bundle.tar.gz"
fi
if [[ ! -f "$bundle" ]]; then
	printf 'Guest bundle is missing; building %s into %s\n' "$PROJECT_NAME" "$NYX_HOME"
	bash "$REPO_ROOT/scripts/atropos.sh" build "${project_args[@]}"
fi
if ! snapshot_ready; then
	printf 'Nyx guest image or pre-snapshot is missing; provisioning %s into %s\n' \
		"$PROJECT_NAME" "$NYX_HOME"
	bash "$REPO_ROOT/scripts/atropos.sh" setup "${project_args[@]}"
fi
if ! snapshot_ready; then
	printf 'error: provisioning finished but %s or %s is still missing\n' \
		"$ATROPOS_NYX_VM_IMAGE" "$ATROPOS_NYX_PRESNAPSHOT" >&2
	exit 1
fi
flock -u 9

fuzz_args=(--project "$PROJECT_YAML")
if [[ -n "${ATROPOS_OUTPUT:-}" ]]; then
	fuzz_args+=(--output "$ATROPOS_OUTPUT")
fi
if [[ -n "${ATROPOS_NYX_CPUSET:-}" ]]; then
	fuzz_args+=(--cpu-set "$ATROPOS_NYX_CPUSET")
fi
if [[ -n "$PROJECT_SOURCE" ]]; then
	fuzz_args+=("$PROJECT_SOURCE")
fi
exec bash "$REPO_ROOT/scripts/atropos.sh" fuzz "${fuzz_args[@]}" \
	-- \
	--nyx-share "$ATROPOS_NYX_SHARE" \
	--nyx-workdir "$ATROPOS_NYX_WORKDIR" \
	"$@"
