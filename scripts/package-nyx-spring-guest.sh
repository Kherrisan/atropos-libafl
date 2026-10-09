#!/usr/bin/env bash
set -euo pipefail
umask 077

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
if [[ $# -gt 0 ]]; then
	printf 'unknown argument: %s\n' "$1" >&2
	exit 1
fi
: "${NYX_HOME:=${HOME:?HOME must be set}/.nyx}"
export NYX_HOME
DATA_DIR="$NYX_HOME/spring"
GUEST_DIR="$DATA_DIR/guest"
BUNDLE_DIR="$DATA_DIR/bundle"

for path in "$GUEST_DIR/spring-runtime" "$GUEST_DIR/webgoat.jar" "$GUEST_DIR/springfuzz-agent.jar" "$GUEST_DIR/springfuzz-hooks.jar" \
	"$GUEST_DIR/atropos_spring_agent" "$GUEST_DIR/lib/libatropos_nyx_bitmap.so" "$GUEST_DIR/jre/bin/java" \
	"$GUEST_DIR/openapi.json" "$REPO_ROOT/guest/common/nyx.h"; do
	if [[ ! -e "$path" ]]; then
		printf 'Required Spring guest input is missing: %s\n' "$path" >&2
		exit 1
	fi
done
if [[ "$(cat "$GUEST_DIR/spring-runtime")" != spring-nyx-webgoat-2023.8 ]]; then
	printf 'Spring guest artifacts are stale; rerun scripts/build-nyx-spring.sh\n' >&2
	exit 1
fi

mkdir -p "$BUNDLE_DIR"
chmod 700 "$DATA_DIR" "$BUNDLE_DIR"
rm -rf "$BUNDLE_DIR/runtime" "$BUNDLE_DIR/install-guest.sh" "$BUNDLE_DIR/atropos-nyx-preimage"
mkdir -p "$BUNDLE_DIR/runtime"
cp -a "$GUEST_DIR/webgoat.jar" "$GUEST_DIR/springfuzz-agent.jar" "$GUEST_DIR/springfuzz-hooks.jar" "$GUEST_DIR/extra" \
	"$GUEST_DIR/lib" "$GUEST_DIR/jre" "$GUEST_DIR/openapi.json" "$BUNDLE_DIR/runtime/"
if [[ -f "$GUEST_DIR/seeds/sql-injection.json" ]]; then
	mkdir -p "$BUNDLE_DIR/runtime/seeds"
	cp -- "$GUEST_DIR/seeds/sql-injection.json" "$BUNDLE_DIR/runtime/seeds/"
fi
cp -- "$GUEST_DIR/atropos_spring_agent" "$BUNDLE_DIR/atropos_spring_agent"

cat >"$BUNDLE_DIR/nyx-preimage.c" <<'C'
#define NO_PT_NYX
#include "nyx.h"
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
	if (argc == 2 && strcmp(argv[1], "--check") == 0) {
		return is_nyx_vcpu() ? 0 : 1;
	}
	if (!is_nyx_vcpu()) {
		return 0;
	}
	if (system("systemctl disable atropos-nyx-preimage.service") != 0) {
		return 1;
	}
	kAFL_hypercall(HYPERCALL_KAFL_LOCK, 0);
	return 0;
}
C
gcc -static -O2 -I"$REPO_ROOT/guest/common" -o "$BUNDLE_DIR/atropos-nyx-preimage" "$BUNDLE_DIR/nyx-preimage.c"
rm -f -- "$BUNDLE_DIR/nyx-preimage.c"
chmod 0755 "$BUNDLE_DIR/atropos-nyx-preimage" "$BUNDLE_DIR/atropos_spring_agent"

cat >"$BUNDLE_DIR/atropos-nyx-launch" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export LD_LIBRARY_PATH=/usr/local/lib/atropos-spring/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
exec /usr/local/bin/atropos_spring_agent
EOF
chmod 0755 "$BUNDLE_DIR/atropos-nyx-launch"

cat >"$BUNDLE_DIR/install-guest.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
umask 022
cd /root/atropos-nyx
install -d -o root -g root -m 0755 /usr/local/lib/atropos-spring /var/lib/webgoat-home
cp -a runtime/webgoat.jar runtime/springfuzz-agent.jar runtime/springfuzz-hooks.jar runtime/jre runtime/lib runtime/extra \
	/usr/local/lib/atropos-spring/
if [[ -f runtime/openapi.json ]]; then
	cp -- runtime/openapi.json /usr/local/lib/atropos-spring/openapi.json
fi
cp -- atropos_spring_agent /usr/local/bin/atropos_spring_agent
cp -- atropos-nyx-preimage /usr/local/bin/atropos-nyx-preimage
cp -- atropos-nyx-launch /usr/local/bin/atropos-nyx-launch
install -d -o root -g root -m 0755 /usr/local/lib/atropos
if [[ -f auth.py ]]; then
	# The agent runs this after the JVM is listening.
	cp -- auth.py /usr/local/lib/atropos/auth.py
	chmod 0755 /usr/local/lib/atropos/auth.py
	if ! command -v python3 >/dev/null 2>&1; then
		export DEBIAN_FRONTEND=noninteractive
		apt-get update
		apt-get install -y python3
	fi
fi
if [[ -f setup.sh ]]; then
	export APP_ROOT=/root/atropos-nyx/runtime
	export OUT=/root/atropos-nyx/out
	bash setup.sh
fi
chmod 0755 /usr/local/bin/atropos_spring_agent /usr/local/bin/atropos-nyx-preimage \
	/usr/local/bin/atropos-nyx-launch /usr/local/lib/atropos-spring/jre/bin/java
cat >/etc/systemd/system/atropos-nyx-agent.service <<'UNIT'
[Unit]
Description=Atropos Nyx Spring guest agent
After=atropos-nyx-preimage.service
Requires=atropos-nyx-preimage.service

[Service]
Type=simple
WorkingDirectory=/
ExecStart=/usr/local/bin/atropos-nyx-launch
Restart=no

[Install]
WantedBy=multi-user.target
UNIT
cat >/etc/systemd/system/atropos-nyx-preimage.service <<'UNIT'
[Unit]
Description=Create the Nyx pre-snapshot and continue guest boot
Before=atropos-nyx-agent.service

[Service]
Type=oneshot
ExecStart=/usr/local/bin/atropos-nyx-preimage
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable atropos-nyx-agent.service
systemctl enable atropos-nyx-preimage.service
rm -rf /root/atropos-nyx
EOF
chmod 0700 "$BUNDLE_DIR/install-guest.sh"
if [[ -n "${SETUP_SCRIPT:-}" ]]; then
	cp -- "$SETUP_SCRIPT" "$BUNDLE_DIR/setup.sh"
	chmod 0755 "$BUNDLE_DIR/setup.sh"
fi
if [[ -n "${AUTH_SCRIPT:-}" ]]; then
	cp -- "$AUTH_SCRIPT" "$BUNDLE_DIR/auth.py"
	chmod 0755 "$BUNDLE_DIR/auth.py"
fi
if [[ -n "${PROJECT_OUT:-}" && -d "$PROJECT_OUT" ]]; then
	rm -rf -- "$BUNDLE_DIR/out"
	mkdir -p "$BUNDLE_DIR/out"
	cp -a "$PROJECT_OUT"/. "$BUNDLE_DIR/out/"
fi
bundle_files=(runtime atropos_spring_agent atropos-nyx-preimage atropos-nyx-launch install-guest.sh)
if [[ -f "$BUNDLE_DIR/setup.sh" ]]; then
	bundle_files+=(setup.sh)
fi
if [[ -f "$BUNDLE_DIR/auth.py" ]]; then
	bundle_files+=(auth.py)
fi
if [[ -d "$BUNDLE_DIR/out" ]]; then
	bundle_files+=(out)
fi
tar -C "$BUNDLE_DIR" -czf "$BUNDLE_DIR/guest-bundle.tar.gz" "${bundle_files[@]}"
printf 'Spring guest bundle: %s\n' "$BUNDLE_DIR/guest-bundle.tar.gz"
