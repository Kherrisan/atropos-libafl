#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${ATROPOS_NYX_DATA_DIR:-${HOME:?HOME must be set}/.nyx}/spring"
GUEST_DIR="$DATA_DIR/guest"
SOCKET="${ATROPOS_SPRING_SOCK:-/tmp/spring.sock}"
HOME_DIR="${ATROPOS_WEBGOAT_HOME:-/tmp/atropos-webgoat-home}"
BITMAP_SIZE=524288
LOG="${ATROPOS_WEBGOAT_LOG:-/tmp/webgoat-spring-test.log}"

if [[ ! -f "$GUEST_DIR/spring-runtime" || "$(cat "$GUEST_DIR/spring-runtime")" != spring-nyx-webgoat-2023.8 ]]; then
	printf 'Spring guest artifacts are missing; run scripts/build-nyx-spring.sh\n' >&2
	exit 1
fi

http() {
	python3 - "$SOCKET" "$1" <<'PY'
import socket
import sys
path, request = sys.argv[1], sys.argv[2]
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(path)
client.sendall(request.encode())
chunks = []
while True:
    data = client.recv(65536)
    if not data:
        break
    chunks.append(data)
    if sum(len(part) for part in chunks) > 8 * 1024 * 1024:
        break
sys.stdout.buffer.write(b"".join(chunks))
PY
}

cleanup() {
	if [[ -n "${JAVA_PID:-}" ]]; then
		kill "$JAVA_PID" >/dev/null 2>&1 || true
		wait "$JAVA_PID" >/dev/null 2>&1 || true
	fi
	rm -f -- "$SOCKET" "${SOCKET}.webwolf" /tmp/spring_fuzz_enabled /tmp/bug_triggered
}
trap cleanup EXIT
rm -f -- "$SOCKET" "${SOCKET}.webwolf" /tmp/spring_fuzz_enabled /tmp/bug_triggered
mkdir -p "$HOME_DIR"

SHM_ID="$("$GUEST_DIR/shm-tool" create "$BITMAP_SIZE")"
export SHM_ID BITMAP_SIZE
export ATROPOS_SPRING_SOCK="$SOCKET"
export LD_LIBRARY_PATH="$GUEST_DIR/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

"$GUEST_DIR/jre/bin/java" \
	-Djava.library.path="$GUEST_DIR/lib" \
	-Duser.home="$HOME_DIR" \
	-Xmx2g \
	-javaagent:"$GUEST_DIR/springfuzz-agent.jar=$GUEST_DIR/springfuzz-hooks.jar" \
	-jar "$GUEST_DIR/webgoat.jar" >"$LOG" 2>&1 &
JAVA_PID=$!

for _ in $(seq 1 180); do
	if [[ -S "$SOCKET" ]]; then
		break
	fi
	if ! kill -0 "$JAVA_PID" >/dev/null 2>&1; then
		printf 'WebGoat exited before listening. See %s\n' "$LOG" >&2
		exit 1
	fi
	sleep 1
done
if [[ ! -S "$SOCKET" ]]; then
	printf 'WebGoat did not create %s. See %s\n' "$SOCKET" "$LOG" >&2
	exit 1
fi

docs="$(http $'GET /WebGoat/v3/api-docs HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n')"
printf '%s' "$docs" >"$GUEST_DIR/openapi.http"
python3 - "$GUEST_DIR/openapi.http" "$GUEST_DIR/openapi.json" <<'PY'
import pathlib
import sys
raw = pathlib.Path(sys.argv[1]).read_bytes()
marker = b"\r\n\r\n"
split = raw.find(marker)
body = raw[split + len(marker):] if split >= 0 else raw
pathlib.Path(sys.argv[2]).write_bytes(body)
text = body.decode("utf-8", "replace")
if "SqlInjection" not in text and "/SqlInjection" not in text:
    raise SystemExit("OpenAPI document does not mention SqlInjection")
PY

register_body='username=atropos&password=atropos&matchingPassword=atropos&agree=agree'
register="$(python3 -c 'import sys; body=sys.argv[1]; print(f"POST /WebGoat/register.mvc HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {len(body)}\r\n\r\n{body}", end="")' "$register_body")"
register_response="$(http "$register")"
cookie="$(printf '%s' "$register_response" | python3 -c '
import sys
cookie = ""
for line in sys.stdin.read().split("\r\n"):
    if line.lower().startswith("set-cookie:") and "jsessionid=" in line.lower():
        cookie = line.split(":", 1)[1].split(";", 1)[0].strip()
print(cookie)
')"
if [[ -z "$cookie" ]]; then
	login_body='username=atropos&password=atropos'
	login="$(python3 -c 'import sys; body=sys.argv[1]; print(f"POST /WebGoat/login HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {len(body)}\r\n\r\n{body}", end="")' "$login_body")"
	login_response="$(http "$login")"
	cookie="$(printf '%s' "$login_response" | python3 -c '
import sys
cookie = ""
for line in sys.stdin.read().split("\r\n"):
    if line.lower().startswith("set-cookie:") and "jsessionid=" in line.lower():
        cookie = line.split(":", 1)[1].split(";", 1)[0].strip()
print(cookie)
')"
fi
if [[ -z "$cookie" ]]; then
	printf 'WebGoat registration did not return a session. See %s\n' "$LOG" >&2
	printf '%s\n' "$register_response" | head -c 2000 >&2
	exit 1
fi
session_value="${cookie#*=}"
python3 - "$GUEST_DIR/seeds/sql-injection.json" "$session_value" <<'PY'
import json
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps({
    "method": "POST",
    "path": "/WebGoat/SqlInjection/attack5",
    "query": {"query": "John"},
    "cookies": [["JSESSIONID", sys.argv[2]]],
    "body": None,
    "pin_route": True,
}, indent=2) + "\n")
PY

touch /tmp/spring_fuzz_enabled
printf -v attack 'POST /WebGoat/SqlInjection/attack5?query=%%27 HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\nCookie: %s\r\n\r\n' "$cookie"
http "$attack" >/tmp/webgoat-sql-response.txt || true
if ! grep -q sqlInjection /tmp/bug_triggered; then
	printf 'SQL oracle did not record sqlInjection. Response follows.\n' >&2
	head -c 2000 /tmp/webgoat-sql-response.txt >&2 || true
	exit 1
fi
nonzero="$("$GUEST_DIR/shm-tool" nonzero "$SHM_ID" "$BITMAP_SIZE")"
if [[ "$nonzero" == 0 ]]; then
	printf 'Nyx bitmap stayed empty after the SQL request\n' >&2
	exit 1
fi
printf 'Spring socket test passed: openapi, sqlInjection, bitmap bytes %s\n' "$nonzero"
