#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${ATROPOS_NYX_DATA_DIR:-${HOME:?HOME must be set}/.nyx}"
SHARE_DIR="${ATROPOS_NYX_SHARE:-$DATA_DIR/share}"
WORKDIR="${ATROPOS_NYX_WORKDIR:-$DATA_DIR/workdir}"
VM_DIR="${ATROPOS_NYX_VM_DIR:-$DATA_DIR/vm}"
QEMU_NYX="${ATROPOS_NYX_QEMU:-$REPO_ROOT/target/nyx/QEMU-Nyx/x86_64-softmmu/qemu-system-x86_64}"
VM_IMAGE="${ATROPOS_NYX_VM_IMAGE:-$VM_DIR/atropos-nyx.qcow2}"
PRESNAPSHOT="${ATROPOS_NYX_PRESNAPSHOT:-$VM_DIR/presnapshot}"

for path in "$QEMU_NYX" "$VM_IMAGE" "$PRESNAPSHOT"; do
	if [[ ! -e "$path" ]]; then
		printf 'Nyx share input is missing: %s\n' "$path" >&2
		exit 1
	fi
done
if [[ ! -x "$QEMU_NYX" || ! -f "$VM_IMAGE" || ! -d "$PRESNAPSHOT" ]] ||
	! find "$PRESNAPSHOT" -mindepth 1 -print -quit | rg -q .; then
	printf 'Nyx QEMU, VM image, or pre-snapshot is incomplete; run scripts/create-nyx-vm.sh\n' >&2
	exit 1
fi
if [[ "$SHARE_DIR$WORKDIR$VM_IMAGE$PRESNAPSHOT$QEMU_NYX" == *,* ]]; then
	printf 'Nyx paths cannot contain commas because QEMU parses its Nyx device options as comma-separated values.\n' >&2
	exit 1
fi

mkdir -p "$SHARE_DIR" "$WORKDIR"
chmod 700 "$DATA_DIR" "$SHARE_DIR" "$WORKDIR"
export SHARE_DIR WORKDIR QEMU_NYX VM_IMAGE PRESNAPSHOT
python3 <<'PY'
import os
from pathlib import Path

def ron(value: str) -> str:
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'

share = Path(os.environ['SHARE_DIR'])
default = f'''#![enable(implicit_some)]
(
  runner: QemuSnapshot((
    qemu_binary: {ron(os.environ['QEMU_NYX'])},
    hda: {ron(os.environ['VM_IMAGE'])},
    presnapshot: {ron(os.environ['PRESNAPSHOT'])},
    snapshot_path: DefaultPath,
    debug: false,
  )),
  fuzz: (
    workdir_path: {ron(os.environ['WORKDIR'])},
    bitmap_size: 524288,
    mem_limit: 8192,
    time_limit: (secs: 2, nanos: 0),
    threads: 1,
    thread_id: 0,
    cpu_pin_start_at: 0,
    snapshot_placement: none,
    seed_path: "",
    dict: [],
  ),
)
'''
config = '''#![enable(implicit_some)]
(
  include_default_config_path: "default_config.ron",
  runner: QemuSnapshot(()),
  fuzz: (
    workdir_path: "workdir",
    seed_path: "",
  ),
)
'''
(share / 'default_config.ron').write_text(default)
(share / 'config.ron').write_text(config)
for path in (share / 'default_config.ron', share / 'config.ron'):
    path.chmod(0o600)
PY

printf 'Nyx share configured at %s\n' "$SHARE_DIR"
printf 'VM image: %s\nPre-snapshot: %s\n' "$VM_IMAGE" "$PRESNAPSHOT"
