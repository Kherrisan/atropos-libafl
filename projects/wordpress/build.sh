#!/usr/bin/env bash
# Host-side project build. WordPress is not compiled; the tree is packaged as-is.
set -euo pipefail
mkdir -p "${OUT:?}"
printf 'wordpress\n' >"$OUT/app-kind"
