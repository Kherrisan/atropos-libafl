#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

if [[ -f "$HOME/.cargo/env" ]]; then
	# shellcheck disable=SC1091
	. "$HOME/.cargo/env"
fi

if [[ "$#" -eq 0 ]]; then
	printf 'Usage: %s command [args...]\n' "$0" >&2
	exit 2
fi
if ! command -v nix-shell >/dev/null 2>&1; then
	printf 'Nix is required for the user-local Nyx build environment.\n' >&2
	exit 1
fi

ATROPOS_NIXPKGS_PATH="$(nix --extra-experimental-features 'nix-command flakes' eval --impure --raw \
	--expr 'builtins.fetchTarball "https://channels.nixos.org/nixos-22.11/nixexprs.tar.xz"')"
export NIX_PATH="nixpkgs=$ATROPOS_NIXPKGS_PATH${NIX_PATH:+:$NIX_PATH}"
export LC_ALL=C
printf -v command_line '%q ' "$@"
exec nix-shell --extra-experimental-features 'nix-command flakes' \
	"$REPO_ROOT/nyx-shell.nix" --run "$command_line"
