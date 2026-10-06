#!/usr/bin/env bash
# Release disk blocks for all-zero 4 KiB pages in a Nyx fast_snapshot.mem_dump.
# The file keeps its logical size, so Nyx still accepts it. Reads of punched
# pages return zeros.
set -euo pipefail

if [[ $# -ne 1 ]]; then
	printf 'usage: %s FAST_SNAPSHOT_MEM_DUMP\n' "$0" >&2
	exit 1
fi

DUMP="$1"
if [[ ! -f "$DUMP" ]]; then
	printf 'Nyx memory dump is missing: %s\n' "$DUMP" >&2
	exit 1
fi

python3 - "$DUMP" <<'PY'
import ctypes
import os
import sys

path = sys.argv[1]
page = 4096
punch_mode = 0x01 | 0x02  # FALLOC_FL_KEEP_SIZE | FALLOC_FL_PUNCH_HOLE
zero_page = b"\x00" * page

libc = ctypes.CDLL(None, use_errno=True)
libc.fallocate.argtypes = [
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int64,
    ctypes.c_int64,
]
libc.fallocate.restype = ctypes.c_int


def allocated_bytes(file_path):
    return os.stat(file_path).st_blocks * 512


def punch(fd, offset, length):
    if length == 0:
        return
    result = libc.fallocate(fd, punch_mode, ctypes.c_int64(offset), ctypes.c_int64(length))
    if result == 0:
        return
    error = ctypes.get_errno()
    raise OSError(error, f"fallocate punch at {offset} length {length}: {os.strerror(error)}")


before = allocated_bytes(path)
logical = os.path.getsize(path)
punched = 0
pages = 0
run_start = None
offset = 0

fd = os.open(path, os.O_RDWR)
try:
    with open(fd, "rb", closefd=False, buffering=1024 * 1024) as source:
        while True:
            chunk = source.read(page * 256)
            if not chunk:
                break
            if len(chunk) % page != 0:
                raise SystemExit(f"{path} length is not a multiple of {page}")
            for index in range(0, len(chunk), page):
                pages += 1
                if chunk[index : index + page] == zero_page:
                    if run_start is None:
                        run_start = offset
                elif run_start is not None:
                    punch(fd, run_start, offset - run_start)
                    punched += offset - run_start
                    run_start = None
                offset += page
        if run_start is not None:
            punch(fd, run_start, offset - run_start)
            punched += offset - run_start
finally:
    os.close(fd)

after = allocated_bytes(path)
if os.path.getsize(path) != logical:
    raise SystemExit(f"{path} logical size changed")
print(
    f"punched {punched // page} of {pages} pages in {path}: "
    f"allocated {before // (1024 * 1024)} MiB -> {after // (1024 * 1024)} MiB "
    f"(logical {logical // (1024 * 1024)} MiB)"
)
PY
