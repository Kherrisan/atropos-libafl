#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

if [[ -f "$HOME/.cargo/env" ]]; then
	# shellcheck disable=SC1091
	. "$HOME/.cargo/env"
fi

cd "$REPO_ROOT"
exec "$SCRIPT_DIR/with-nyx-build-deps.sh" \
	cargo build --profile nyx --bin atropos-libafl "$@"
