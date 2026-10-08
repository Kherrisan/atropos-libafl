#!/usr/bin/env bash
# Guest-side setup. The Spring installer copies the instrumented runtime,
# including webgoat.jar, before this script. Extra jars from OUT are added
# beside that runtime.
set -euo pipefail
install -d -o root -g root -m 0755 /var/lib/webgoat-home /usr/local/lib/atropos-spring
if [[ -d "${OUT:-}" ]]; then
	find "$OUT" -maxdepth 1 -type f -name '*.jar' \
		! -name 'webgoat.jar' ! -name 'webgoat-*.jar' \
		-exec cp -a {} /usr/local/lib/atropos-spring/ \;
fi
