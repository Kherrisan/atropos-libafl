#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SPRING_ROOT="${ATROPOS_NYX_DATA_DIR:-${HOME:?HOME must be set}/.nyx}/spring"
if [[ ! -f "$SPRING_ROOT/bundle/guest-bundle.tar.gz" ]]; then
	printf 'Spring guest bundle is missing; run scripts/package-nyx-spring-guest.sh\n' >&2
	exit 1
fi

ATROPOS_NYX_DATA_DIR="$SPRING_ROOT" \
ATROPOS_NYX_GUEST_BUNDLE="$SPRING_ROOT/bundle/guest-bundle.tar.gz" \
	"$SCRIPT_DIR/create-nyx-vm.sh"

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
