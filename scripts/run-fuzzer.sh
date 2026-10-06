#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
ORIGINAL_CWD="$(pwd)"
NYX_DATA_DIR=""
TARGET="php"
CPU_SET=""
args=()
while [[ $# -gt 0 ]]; do
	case "$1" in
	--fuzzer-output)
		NYX_DATA_DIR="${2:?--fuzzer-output needs a directory}"
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
	*)
		args+=("$1")
		shift
		;;
	esac
done
if [[ -z "$NYX_DATA_DIR" ]]; then
	printf 'usage: run-fuzzer.sh --fuzzer-output DIR [--target php|spring] [fuzzer flags]\n' >&2
	exit 1
fi
if [[ "$TARGET" == spring ]]; then
	NYX_SHARE="$NYX_DATA_DIR/spring/share"
else
	NYX_SHARE="$NYX_DATA_DIR/phase-run/share-oracle"
fi

abs_one() {
	local path="$1"
	if [[ "$path" = /* ]]; then
		printf '%s\n' "$path"
	else
		printf '%s\n' "$ORIGINAL_CWD/$path"
	fi
}

abs_list() {
	local raw="$1"
	local part abs out=""
	local -a parts
	IFS=',' read -ra parts <<< "$raw"
	for part in "${parts[@]}"; do
		part="${part#"${part%%[![:space:]]*}"}"
		part="${part%"${part##*[![:space:]]}"}"
		if [[ -z "$part" ]]; then
			continue
		fi
		abs="$(abs_one "$part")"
		if [[ -n "$out" ]]; then
			out+=",$abs"
		else
			out="$abs"
		fi
	done
	printf '%s\n' "$out"
}

rewrite_path_arg() {
	local flag="$1"
	local i=0
	while ((i < ${#args[@]})); do
		case "${args[$i]}" in
		"$flag")
			i=$((i + 1))
			if ((i >= ${#args[@]})); then
				printf 'missing value for %s\n' "$flag" >&2
				exit 1
			fi
			args[$i]="$(abs_one "${args[$i]}")"
			;;
		"$flag"=*)
			args[$i]="$flag=$(abs_one "${args[$i]#"$flag"=}")"
			;;
		esac
		i=$((i + 1))
	done
}

rewrite_path_list_arg() {
	local flag="$1"
	local i=0
	while ((i < ${#args[@]})); do
		case "${args[$i]}" in
		"$flag")
			i=$((i + 1))
			if ((i >= ${#args[@]})); then
				printf 'missing value for %s\n' "$flag" >&2
				exit 1
			fi
			args[$i]="$(abs_list "${args[$i]}")"
			;;
		"$flag"=*)
			args[$i]="$flag=$(abs_list "${args[$i]#"$flag"=}")"
			;;
		esac
		i=$((i + 1))
	done
}

rewrite_path_arg --nyx-share
rewrite_path_arg --nyx-workdir
rewrite_path_arg --seed-dir
rewrite_path_arg --corpus-dir
rewrite_path_arg --objectives-dir
rewrite_path_list_arg --mutation-dict
rewrite_path_list_arg --bug-trigger
rewrite_path_list_arg --openapi

share_set=0
i=0
while ((i < ${#args[@]})); do
	case "${args[$i]}" in
	--nyx-share)
		i=$((i + 1))
		NYX_SHARE="${args[$i]}"
		share_set=1
		;;
	--nyx-share=*)
		NYX_SHARE="${args[$i]#--nyx-share=}"
		share_set=1
		;;
	esac
	i=$((i + 1))
done
if ((share_set == 0)); then
	args+=(--nyx-share "$NYX_SHARE")
fi

if [[ ! -f "$NYX_SHARE/config.ron" ]]; then
	printf 'Nyx config is missing; build the guest image and run scripts/prepare-nyx-share.sh first\n' >&2
	exit 1
fi

OUTPUT_DIR="$REPO_ROOT/output"
mkdir -p "$OUTPUT_DIR"
run_stamp="$(date +%Y%m%d-%H%M)"
run_id=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
	run_id="${run_stamp}-$(uuidgen | tr '[:upper:]' '[:lower:]' | tr -d '-' | cut -c1-4)"
	if [[ ! -e "$OUTPUT_DIR/$run_id" ]]; then
		break
	fi
	run_id=""
done
if [[ -z "$run_id" ]]; then
	printf 'could not allocate a run directory under %s\n' "$OUTPUT_DIR" >&2
	exit 1
fi

RUN_DIR="$OUTPUT_DIR/$run_id"
mkdir -p "$RUN_DIR"
LOG_FILE="$RUN_DIR/fuzzer.log"
touch "$LOG_FILE"
# Keep the live terminal stream and store the same stdout/stderr in this run.
exec > >(tee -a "$LOG_FILE") 2>&1
printf 'fuzzer run directory: %s\n' "$RUN_DIR"
printf 'fuzzer log: %s\n' "$LOG_FILE"
cd "$RUN_DIR"
# These cores were running at 2100 MHz. QEMU otherwise inherits 0-63 and can
# stay on an 800 MHz core for a whole run.
CPUSET="${CPU_SET:-2-5,7,9,12-14,16-20,22-28,32,35,37-39,41-48,51-60,63}"
printf 'fuzzer cpu set: %s\n' "$CPUSET"
taskset -c "$CPUSET" "$REPO_ROOT/target/nyx/atropos-libafl" "${args[@]}"
