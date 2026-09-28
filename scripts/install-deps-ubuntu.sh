#!/usr/bin/env bash
set -euo pipefail

if [[ "$(id -u)" -eq 0 ]]; then
	APT=(apt-get)
else
	APT=(sudo apt-get)
fi

"${APT[@]}" update
"${APT[@]}" install -y \
	build-essential autoconf bison re2c pkg-config git curl ca-certificates \
	libxml2-dev libsqlite3-dev libcurl4-openssl-dev libssl-dev libreadline-dev \
	libonig-dev libzip-dev zlib1g-dev libpng-dev libjpeg-dev libwebp-dev \
	libfreetype6-dev libicu-dev libxslt1-dev libbz2-dev libgmp-dev
