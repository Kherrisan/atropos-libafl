#!/usr/bin/env bash
# One entry point for the Nyx PHP and Spring guest pipeline.
# Paths are flags. The implementation scripts are not invoked with those paths
# in the environment.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

usage() {
	cat <<EOF
usage: scripts/atropos.sh <command> [options] [-- fuzzer-args]

commands:
  build-fuzzer
  build-php
  setup-wordpress
  package-guest
  create-vm
  run
  build-spring
  package-spring
  create-spring-vm

options:
  --src DIR             WordPress source tree (default: ../wordpress)
  --php-output DIR      PHP guest artifacts and install prefix (default: <fuzzer-output>/guest)
  --fuzzer-output DIR   Nyx images, bundle, share, and workdir (default: ~/.nyx)
  --target php|spring   guest selected by run (default: php)
  --cpu-set LIST        CPU list for run (default: the built-in set)
EOF
}

if [[ $# -lt 1 || "$1" == "--help" || "$1" == "-h" ]]; then
	usage
	exit 0
fi
command="$1"
shift

SRC=""
SRC_SET=0
PHP_OUTPUT=""
FUZZER_OUTPUT=""
TARGET="php"
CPU_SET=""
extra=()
while [[ $# -gt 0 ]]; do
	case "$1" in
	--src)
		SRC="${2:?--src needs a directory}"
		SRC_SET=1
		shift 2
		;;
	--php-output)
		PHP_OUTPUT="${2:?--php-output needs a directory}"
		shift 2
		;;
	--fuzzer-output)
		FUZZER_OUTPUT="${2:?--fuzzer-output needs a directory}"
		shift 2
		;;
	--target)
		TARGET="${2:?--target needs php or spring}"
		shift 2
		;;
	--cpu-set)
		CPU_SET="${2:?--cpu-set needs a CPU list}"
		shift 2
		;;
	--help | -h)
		usage
		exit 0
		;;
	--)
		shift
		extra+=("$@")
		break
		;;
	*)
		extra+=("$1")
		shift
		;;
	esac
done

FUZZER_OUTPUT="${FUZZER_OUTPUT:-${HOME:?HOME must be set}/.nyx}"
PHP_OUTPUT="${PHP_OUTPUT:-$FUZZER_OUTPUT/guest}"
SRC="${SRC:-$REPO_ROOT/../wordpress}"

case "$command" in
build-fuzzer)
	if ((${#extra[@]} > 0)); then
		printf 'build-fuzzer does not take extra arguments\n' >&2
		exit 1
	fi
	exec "$SCRIPT_DIR/build-nyx-fuzzer.sh"
	;;
build-php)
	args=(--php-output "$PHP_OUTPUT")
	if [[ -f "$SRC/index.php" ]]; then
		args+=(--src "$SRC")
	elif [[ "$SRC_SET" == 1 ]]; then
		printf 'WordPress source not found under %s\n' "$SRC" >&2
		exit 1
	else
		args+=(--skip-wordpress)
	fi
	exec "$SCRIPT_DIR/build-nyx-php.sh" "${args[@]}"
	;;
setup-wordpress)
	exec "$SCRIPT_DIR/setup-wordpress.sh" --src "$SRC" --php-output "$PHP_OUTPUT"
	;;
package-guest)
	exec "$SCRIPT_DIR/package-nyx-guest.sh" \
		--src "$SRC" \
		--php-output "$PHP_OUTPUT" \
		--fuzzer-output "$FUZZER_OUTPUT"
	;;
create-vm)
	exec "$SCRIPT_DIR/create-nyx-vm.sh" --fuzzer-output "$FUZZER_OUTPUT"
	;;
run)
	args=(--fuzzer-output "$FUZZER_OUTPUT" --target "$TARGET")
	if [[ -n "$CPU_SET" ]]; then
		args+=(--cpu-set "$CPU_SET")
	fi
	exec "$SCRIPT_DIR/run-fuzzer.sh" "${args[@]}" "${extra[@]}"
	;;
build-spring)
	exec "$SCRIPT_DIR/build-nyx-spring.sh" --fuzzer-output "$FUZZER_OUTPUT"
	;;
package-spring)
	exec "$SCRIPT_DIR/package-nyx-spring-guest.sh" --fuzzer-output "$FUZZER_OUTPUT"
	;;
create-spring-vm)
	exec "$SCRIPT_DIR/create-nyx-spring-vm.sh" --fuzzer-output "$FUZZER_OUTPUT"
	;;
*)
	printf 'unknown command: %s\n' "$command" >&2
	usage >&2
	exit 1
	;;
esac
