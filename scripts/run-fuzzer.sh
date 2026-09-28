#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
export ATROPOS_OUTPUT_DIR="${ATROPOS_OUTPUT_DIR:-$REPO_ROOT}"

if [[ ! -f "${ATROPOS_NYX_SHARE:-$HOME/.local/share/atropos-libafl/nyx/share}/config.ron" ]]; then
	printf 'Nyx config is missing; build the guest image and run scripts/prepare-nyx-share.sh first\n' >&2
	exit 1
fi

cd "$REPO_ROOT"
exec "$REPO_ROOT/target/nyx/atropos-libafl" "$@"
