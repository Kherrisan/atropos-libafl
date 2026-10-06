#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
FUZZER_OUTPUT=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--fuzzer-output)
		FUZZER_OUTPUT="${2:?--fuzzer-output needs a directory}"
		shift 2
		;;
	*)
		printf 'unknown argument: %s\n' "$1" >&2
		exit 1
		;;
	esac
done
if [[ -z "$FUZZER_OUTPUT" ]]; then
	printf 'usage: create-nyx-spring-vm.sh --fuzzer-output DIR\n' >&2
	exit 1
fi
SPRING_ROOT="$FUZZER_OUTPUT/spring"
if [[ ! -f "$SPRING_ROOT/bundle/guest-bundle.tar.gz" ]]; then
	printf 'Spring guest bundle is missing; run scripts/package-nyx-spring-guest.sh\n' >&2
	exit 1
fi

"$SCRIPT_DIR/create-nyx-vm.sh" --fuzzer-output "$SPRING_ROOT"

serial="$SPRING_ROOT/vm/preimage-serial.log"
seed_dir="$SPRING_ROOT/guest/seeds"
if [[ -f "$serial" ]] && grep -q 'ATROPOS_SPRING_SESSION ' "$serial"; then
	cookie="$(grep -o 'ATROPOS_SPRING_SESSION JSESSIONID=[^[:space:]]*' "$serial" | tail -1 | awk '{print $2}')"
	value="${cookie#JSESSIONID=}"
	mkdir -p "$seed_dir"
	python3 - "$seed_dir/sql-injection.json" "$value" <<'PY'
import json
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
path.write_text(json.dumps({
    "method": "POST",
    "path": "/WebGoat/SqlInjection/attack5",
    "query": {"query": "John"},
    "cookies": [["JSESSIONID", sys.argv[2]]],
    "body": None,
    "pin_route": True,
}, indent=2) + "\n")
PY
	printf 'Spring seed written to %s\n' "$seed_dir/sql-injection.json"
fi
