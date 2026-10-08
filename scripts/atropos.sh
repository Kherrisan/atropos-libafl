#!/usr/bin/env bash
# One entry point for the Nyx PHP and Spring guest pipeline.
# Paths are flags. The implementation scripts are not invoked with those paths
# in the environment.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

usage() {
	cat <<EOF
usage: scripts/atropos.sh <command> [options] [source-dir] [-- fuzzer-args]

commands:
  build --project YAML SOURCE   build the backend, run build_script, and package
  setup --project YAML SOURCE   create the guest VM; the installer runs setup_script
  fuzz --project YAML SOURCE    run the fuzzer for the project's backend
  build-fuzzer
  build-php
  setup-wordpress
  package-guest
  create-vm
  run
  build-spring
  package-spring
  create-spring-vm

The last argument of build, setup, and fuzz is the application source directory.
--project is the project.yaml file. When SOURCE already contains project.yaml,
that file is used and --project can be omitted. A --project directory uses
the project.yaml inside it. fuzz passes the yaml seeds directory as --seed-dir.

options:
  --project FILE        path to project.yaml
  --src DIR             application source tree for the PHP commands (default: ../wordpress)
  --app NAME            PHP app adapter: wordpress or generic (default: wordpress)
  --db-env FILE         Database credential file for package-guest
  --php-output DIR      PHP guest artifacts and install prefix (default: <fuzzer-output>/guest)
  --php-src DIR         PHP 7.4 source tree (default: guest/php/php-7.4-patched)
  --pcov-src DIR        PCOV source tree (default: guest/php/pcov-patched)
  --springfuzz-src DIR  SpringFuzz source tree (default: SpringFuzz submodule)
  --fuzzer-output DIR   Nyx images, bundle, share, and workdir (default: ~/.nyx)
  --target php|spring   guest selected by run (default: php; fuzz uses project.yaml)
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
PROJECT=""
APP="wordpress"
DB_ENV=""
PHP_OUTPUT=""
PHP_SRC=""
PCOV_SRC=""
SPRINGFUZZ_SRC=""
FUZZER_OUTPUT=""
TARGET="php"
CPU_SET=""
leading=()
positionals=()
fuzzer_args=()
while [[ $# -gt 0 ]]; do
	case "$1" in
	--src)
		SRC="${2:?--src needs a directory}"
		SRC_SET=1
		shift 2
		;;
	--project)
		PROJECT="${2:?--project needs a project.yaml file}"
		shift 2
		;;
	--app)
		APP="${2:?--app needs wordpress or generic}"
		shift 2
		;;
	--db-env)
		DB_ENV="${2:?--db-env needs a file}"
		shift 2
		;;
	--php-output)
		PHP_OUTPUT="${2:?--php-output needs a directory}"
		shift 2
		;;
	--php-src)
		PHP_SRC="${2:?--php-src needs a directory}"
		shift 2
		;;
	--pcov-src)
		PCOV_SRC="${2:?--pcov-src needs a directory}"
		shift 2
		;;
	--springfuzz-src)
		SPRINGFUZZ_SRC="${2:?--springfuzz-src needs a directory}"
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
		fuzzer_args=("$@")
		break
		;;
	*)
		if [[ "$1" != -* ]]; then
			positionals+=("$1")
		fi
		leading+=("$1")
		shift
		;;
	esac
done

FUZZER_OUTPUT="${FUZZER_OUTPUT:-${HOME:?HOME must be set}/.nyx}"
PHP_OUTPUT="${PHP_OUTPUT:-$FUZZER_OUTPUT/guest}"
PHP_SRC="${PHP_SRC:-$REPO_ROOT/guest/php/php-7.4-patched}"
PCOV_SRC="${PCOV_SRC:-$REPO_ROOT/guest/php/pcov-patched}"
SPRINGFUZZ_SRC="${SPRINGFUZZ_SRC:-$REPO_ROOT/SpringFuzz}"
SRC="${SRC:-$REPO_ROOT/../wordpress}"

reject_unknown_project_args() {
	local allow_fuzzer="${1:-0}"
	if ((${#positionals[@]} > 1)); then
		printf 'pass the source directory as the last argument\n' >&2
		exit 1
	fi
	if ((${#leading[@]} != ${#positionals[@]})); then
		printf '%s does not take extra arguments before the source directory\n' "$command" >&2
		exit 1
	fi
	if [[ "$allow_fuzzer" != 1 && ${#fuzzer_args[@]} -gt 0 ]]; then
		printf '%s does not take fuzzer arguments\n' "$command" >&2
		exit 1
	fi
}

load_project() {
	local code="" yaml=""
	if ((${#positionals[@]} == 1)); then
		code="${positionals[0]}"
	fi
	if [[ -n "$PROJECT" ]]; then
		if [[ -f "$PROJECT" ]]; then
			yaml="$PROJECT"
		elif [[ -d "$PROJECT" && -f "$PROJECT/project.yaml" ]]; then
			yaml="$PROJECT/project.yaml"
		else
			printf -- '--project must be a project.yaml file: %s\n' "$PROJECT" >&2
			exit 1
		fi
	elif [[ -n "$code" && -f "$code/project.yaml" ]]; then
		yaml="$code/project.yaml"
	else
		printf 'pass --project YAML and the source directory\n' >&2
		exit 1
	fi
	PROJECT_DIR="$(cd -- "$(dirname -- "$yaml")" && pwd)"
	PROJECT_FILE="$PROJECT_DIR/$(basename -- "$yaml")"
	if [[ -n "$code" ]]; then
		if [[ ! -d "$code" ]]; then
			printf 'source directory not found: %s\n' "$code" >&2
			exit 1
		fi
		SRC="$(cd -- "$code" && pwd)"
	else
		SRC="$PROJECT_DIR"
	fi
	# shellcheck disable=SC1090
	eval "$(python3 "$SCRIPT_DIR/load-project.py" shell "$PROJECT_FILE")"
}

project_work_dir() {
	printf '%s\n' "$FUZZER_OUTPUT/projects/$PROJECT_NAME"
}

php_backend_ready() {
	[[ -f "$PHP_OUTPUT/nyx-php-runtime.tar.gz" && -x "$PHP_OUTPUT/atropos_agent" ]] || return 1
	[[ "$(cat "$PHP_OUTPUT/php-code-coverage-runtime" 2>/dev/null || true)" == php-code-coverage-9.2.31+phpcov-8.2.1+fastcgi-v1 ]] || return 1
	[[ "$(cat "$PHP_OUTPUT/atropos-agent-phpcov-runtime" 2>/dev/null || true)" == nyx-agent-fastcgi-v1 ]]
}

spring_backend_ready() {
	local guest="$FUZZER_OUTPUT/spring/guest"
	[[ -f "$guest/webgoat.jar" && -x "$guest/atropos_spring_agent" ]] || return 1
	[[ "$(cat "$guest/spring-runtime" 2>/dev/null || true)" == spring-nyx-webgoat-2023.8 ]]
}

package_ready() {
	if [[ "$PROJECT_BACKEND" == spring ]]; then
		[[ -f "$FUZZER_OUTPUT/spring/bundle/guest-bundle.tar.gz" ]]
	else
		[[ -f "$FUZZER_OUTPUT/bundle/guest-bundle.tar.gz" ]]
	fi
}

project_package() {
	local work="$1"
	export SETUP_SCRIPT="$PROJECT_SETUP_SCRIPT"
	export AUTH_SCRIPT="${PROJECT_AUTH_SCRIPT:-}"
	export PROJECT_OUT="$work/out"
	mkdir -p "$PROJECT_OUT"
	if [[ "$PROJECT_BACKEND" == spring ]]; then
		"$SCRIPT_DIR/package-nyx-spring-guest.sh" --fuzzer-output "$FUZZER_OUTPUT"
		return
	fi
	local app="generic"
	if [[ "$PROJECT_NAME" == wordpress ]]; then
		app="wordpress"
	fi
	local args=(
		--src "$SRC"
		--php-output "$PHP_OUTPUT"
		--fuzzer-output "$FUZZER_OUTPUT"
		--app "$app"
	)
	if [[ -n "$DB_ENV" ]]; then
		args+=(--db-env "$DB_ENV")
	fi
	"$SCRIPT_DIR/package-nyx-guest.sh" "${args[@]}"
}

project_build() {
	reject_unknown_project_args 0
	load_project
	local hash work stamp
	hash="$(python3 "$SCRIPT_DIR/load-project.py" hash "$PROJECT_DIR" --src "$SRC")"
	work="$(project_work_dir)"
	stamp="$work/source.stamp"
	local backend_ok=1
	local package_ok=1
	if [[ "$PROJECT_BACKEND" == spring ]]; then
		spring_backend_ready || backend_ok=0
	else
		php_backend_ready || backend_ok=0
	fi
	package_ready || package_ok=0
	if [[ "$backend_ok" == 1 && "$package_ok" == 1 && -f "$stamp" && "$(cat "$stamp")" == "$hash" ]]; then
		printf 'skipping build for %s; source is unchanged and artifacts exist\n' "$PROJECT_NAME"
		return 0
	fi
	mkdir -p "$work/out"
	if [[ "$PROJECT_BACKEND" == spring ]]; then
		# The Spring backend instruments the jar this script downloads, so the
		# project build runs before that instrumentation step.
		(
			cd -- "$SRC"
			OUT="$work/out" bash "$PROJECT_BUILD_SCRIPT"
		)
		if [[ ! -f "$work/out/webgoat.jar" ]]; then
			printf 'Spring build_script did not create %s\n' "$work/out/webgoat.jar" >&2
			exit 1
		fi
		"$SCRIPT_DIR/build-nyx-spring.sh" \
			--fuzzer-output "$FUZZER_OUTPUT" \
			--springfuzz-src "$SPRINGFUZZ_SRC" \
			--webgoat-jar "$work/out/webgoat.jar"
	else
		"$SCRIPT_DIR/build-nyx-php.sh" \
			--php-output "$PHP_OUTPUT" \
			--php-src "$PHP_SRC" \
			--pcov-src "$PCOV_SRC" \
			--skip-app
		(
			cd -- "$SRC"
			OUT="$work/out" bash "$PROJECT_BUILD_SCRIPT"
		)
	fi
	project_package "$work"
	printf '%s\n' "$hash" >"$stamp"
}

project_setup() {
	reject_unknown_project_args 0
	load_project
	if ! package_ready; then
		printf 'guest bundle is missing; run scripts/atropos.sh build --project %s %s\n' "$PROJECT_FILE" "$SRC" >&2
		exit 1
	fi
	if [[ "$PROJECT_BACKEND" == spring ]]; then
		"$SCRIPT_DIR/create-nyx-spring-vm.sh" --fuzzer-output "$FUZZER_OUTPUT"
	else
		"$SCRIPT_DIR/create-nyx-vm.sh" --fuzzer-output "$FUZZER_OUTPUT"
	fi
}

project_fuzz() {
	reject_unknown_project_args 1
	load_project
	TARGET="$PROJECT_BACKEND"
	local args=(--fuzzer-output "$FUZZER_OUTPUT" --target "$TARGET")
	if [[ -n "$CPU_SET" ]]; then
		args+=(--cpu-set "$CPU_SET")
	fi
	local has_seed=0
	local arg
	for arg in "${fuzzer_args[@]}"; do
		case "$arg" in
		--seed-dir | --seed-dir=*)
			has_seed=1
			;;
		esac
	done
	if [[ "$has_seed" == 0 && -n "${PROJECT_SEEDS:-}" ]]; then
		fuzzer_args+=(--seed-dir "$PROJECT_SEEDS")
	fi
	exec "$SCRIPT_DIR/run-fuzzer.sh" "${args[@]}" "${fuzzer_args[@]}"
}

case "$command" in
build)
	project_build
	;;
setup)
	project_setup
	;;
fuzz)
	project_fuzz
	;;
build-fuzzer)
	if ((${#leading[@]} > 0 || ${#fuzzer_args[@]} > 0)); then
		printf 'build-fuzzer does not take extra arguments\n' >&2
		exit 1
	fi
	exec "$SCRIPT_DIR/build-nyx-fuzzer.sh"
	;;
build-php)
	args=(--php-output "$PHP_OUTPUT" --php-src "$PHP_SRC" --pcov-src "$PCOV_SRC")
	if [[ -f "$SRC/index.php" ]]; then
		args+=(--src "$SRC")
	elif [[ "$SRC_SET" == 1 ]]; then
		printf 'PHP application source not found under %s\n' "$SRC" >&2
		exit 1
	else
		args+=(--skip-app)
	fi
	exec "$SCRIPT_DIR/build-nyx-php.sh" "${args[@]}"
	;;
setup-wordpress)
	exec "$SCRIPT_DIR/setup-wordpress.sh" --src "$SRC" --php-output "$PHP_OUTPUT"
	;;
package-guest)
	args=(
		--src "$SRC"
		--php-output "$PHP_OUTPUT"
		--fuzzer-output "$FUZZER_OUTPUT"
		--app "$APP"
	)
	if [[ -n "$DB_ENV" ]]; then
		args+=(--db-env "$DB_ENV")
	fi
	exec "$SCRIPT_DIR/package-nyx-guest.sh" "${args[@]}"
	;;
create-vm)
	exec "$SCRIPT_DIR/create-nyx-vm.sh" --fuzzer-output "$FUZZER_OUTPUT"
	;;
run)
	args=(--fuzzer-output "$FUZZER_OUTPUT" --target "$TARGET")
	if [[ -n "$CPU_SET" ]]; then
		args+=(--cpu-set "$CPU_SET")
	fi
	exec "$SCRIPT_DIR/run-fuzzer.sh" "${args[@]}" "${leading[@]}" "${fuzzer_args[@]}"
	;;
build-spring)
	exec "$SCRIPT_DIR/build-nyx-spring.sh" --fuzzer-output "$FUZZER_OUTPUT" --springfuzz-src "$SPRINGFUZZ_SRC"
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
