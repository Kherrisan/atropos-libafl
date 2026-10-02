#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
WORDPRESS_ROOT="${ATROPOS_WORDPRESS_ROOT:-$REPO_ROOT/../wordpress}"
export ATROPOS_OUTPUT_DIR="${ATROPOS_OUTPUT_DIR:-$WORDPRESS_ROOT/atropos-output}"
NYX_DATA_DIR="${ATROPOS_NYX_DATA_DIR:-${HOME:?HOME must be set}/.nyx}"
NYX_SHARE="${ATROPOS_NYX_SHARE:-$NYX_DATA_DIR/share}"

if [[ ! -f "$NYX_SHARE/config.ron" ]]; then
	printf 'Nyx config is missing; build the guest image and run scripts/prepare-nyx-share.sh first\n' >&2
	exit 1
fi

cd "$REPO_ROOT"
exec "$REPO_ROOT/target/nyx/atropos-libafl" "$@"
