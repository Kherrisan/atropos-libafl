#!/usr/bin/env bash
# Host-side project build. Fetch the WebGoat release; the Spring backend
# instruments this jar while it builds the guest agent.
set -euo pipefail
mkdir -p "${OUT:?}"
url="${WEBGOAT_URL:-https://github.com/WebGoat/WebGoat/releases/download/v2023.8/webgoat-2023.8.jar}"
if [[ ! -f "$OUT/webgoat.jar" ]]; then
	curl --fail --location --retry 3 --output "$OUT/webgoat.jar.part" "$url"
	mv -- "$OUT/webgoat.jar.part" "$OUT/webgoat.jar"
fi
printf 'webgoat\n' >"$OUT/app-kind"
