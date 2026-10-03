#!/usr/bin/env python3
"""Apply rules/wordpress-batch-params.yml to one PHP file.

Usage: build-batch-dictionary.py PHP_FILE

Semgrep OSS redacts matched source in JSON, so values are read back from the
PHP file by the reported byte offsets. The dictionary is written to
dict/<php-stem>.dict.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
RULES = REPO / "rules" / "wordpress-batch-params.yml"

# Schema machinery, not request fields. type/format/context stay: menu items
# have a type field, posts have format, and context is a query parameter.
SKIP = {
    "description",
    "enum",
    "default",
    "items",
    "properties",
    "additionalproperties",
    "additionalProperties",
    "arg_options",
    "sanitize_callback",
    "validate_callback",
    "readonly",
    "required",
    "minimum",
    "maximum",
    "exclusiveminimum",
    "exclusivemaximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minlength",
    "maxlength",
    "minLength",
    "maxLength",
    "minitems",
    "maxitems",
    "minItems",
    "maxItems",
    "uniqueitems",
    "uniqueItems",
    "pattern",
    "oneof",
    "anyof",
    "allof",
    "oneOf",
    "anyOf",
    "allOf",
    "methods",
    "callback",
    "permission_callback",
    "args",
    "allow_batch",
    "schema",
    "show_in_index",
    "namespace",
    "rest_base",
    "targetschema",
    "targetSchema",
    "links",
    "rel",
    "href",
    "string",
    "object",
    "integer",
    "boolean",
    "array",
    "number",
    "null",
    "date-time",
    "uri",
}

TOKEN = re.compile(r"[A-Za-z0-9_./:@+-]+")
QUOTED = re.compile(r"""(['"])([^'"\\]*)\1""")
KEY_BEFORE = re.compile(
    r"""(?:\[['\"]([A-Za-z0-9_-]+)['\"]\]\s*=\s*array\s*\(?|['\"]([A-Za-z0-9_-]+)['\"]\s*=>\s*array\s*\(?)\s*$"""
)
BARE_DEFAULT = re.compile(
    r"""(?:\[['\"]default['\"]\]\s*=|['\"]default['\"]\s*=>)\s*(true|false|-?\d+)\b"""
)
ENUM_APPEND = re.compile(
    r"""\[['\"]([A-Za-z0-9_-]+)['\"]\]\[['\"]enum['\"]\]\[\]\s*=\s*(['"])([^'"]+)\2"""
)


def valid_token(value: str) -> bool:
    if not value or value in SKIP or len(value) > 80:
        return False
    if any(ch.isspace() for ch in value):
        return False
    return TOKEN.fullmatch(value) is not None


def param_name(text: str, offset: int) -> str | None:
    pos = offset
    for _ in range(6):
        window = text[max(0, pos - 300) : pos]
        match = None
        for candidate in KEY_BEFORE.finditer(window):
            match = candidate
        if match is None:
            return None
        key = match.group(1) or match.group(2)
        if key not in SKIP:
            return key
        pos = max(0, pos - 300) + match.start()
    return None


def keys_and_values(snippet: str) -> tuple[list[str], list[str]]:
    keys: list[str] = []
    values: list[str] = []
    for match in QUOTED.finditer(snippet):
        raw = match.group(2)
        if re.match(r"\s*=>", snippet[match.end() : match.end() + 8]):
            if valid_token(raw):
                keys.append(raw)
            continue
        if valid_token(raw):
            values.append(raw)
    for match in re.finditer(r"""\[['\"]([A-Za-z0-9_-]+)['\"]\]""", snippet):
        if valid_token(match.group(1)):
            keys.append(match.group(1))
    for match in BARE_DEFAULT.finditer(snippet):
        if valid_token(match.group(1)):
            values.append(match.group(1))
    for match in ENUM_APPEND.finditer(snippet):
        if valid_token(match.group(1)):
            keys.append(match.group(1))
        if valid_token(match.group(3)):
            values.append(match.group(3))
    return keys, values


def afl_name(token: str, used: set[str]) -> str:
    """AFL++ labels are alphanumeric or underscore and must be unique."""
    base = re.sub(r"[^A-Za-z0-9_]", "_", token)
    if not base:
        base = "t"
    name = base
    suffix = 2
    while name in used:
        name = f"{base}_{suffix}"
        suffix += 1
    used.add(name)
    return name


def afl_escape(token: str) -> str:
    """Escape a token the way AFL++ load_extras_file decodes it."""
    parts: list[str] = []
    for byte in token.encode("utf-8"):
        if byte in (ord("\\"), ord('"')):
            parts.append("\\" + chr(byte))
        elif 0x20 <= byte <= 0x7E:
            parts.append(chr(byte))
        else:
            parts.append(f"\\x{byte:02x}")
    return "".join(parts)


def afl_line(token: str, used_names: set[str]) -> str:
    return f'{afl_name(token, used_names)}="{afl_escape(token)}"'


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: build-batch-dictionary.py PHP_FILE")
    source = Path(sys.argv[1])
    if not source.is_file():
        sys.exit(f"PHP file not found: {source}")

    report = Path(f"/tmp/wordpress-batch-semgrep-{source.stem}.json")
    command = [
        "semgrep",
        "--metrics=off",
        "--quiet",
        "--disable-version-check",
        "--novcs",
        "--timeout",
        "0",
        "--config",
        str(RULES),
        "--json",
        "-o",
        str(report),
        str(source),
    ]
    completed = subprocess.run(command, text=True)
    if completed.returncode not in (0, 1):
        sys.exit(f"semgrep failed with status {completed.returncode}")
    payload = json.loads(report.read_text())
    errors = payload.get("errors") or []
    if errors:
        print(f"semgrep reported {len(errors)} error(s)", file=sys.stderr)
        for error in errors[:8]:
            print(error, file=sys.stderr)

    text = source.read_text(encoding="utf-8", errors="replace")
    results = payload.get("results") or []
    results.sort(key=lambda item: item["start"]["offset"])

    seen: set[str] = set()
    used_names: set[str] = set()
    rows: list[str] = []

    def add(token: str) -> None:
        if token in seen or not valid_token(token):
            return
        seen.add(token)
        rows.append(afl_line(token, used_names))

    for result in results:
        start = result["start"]["offset"]
        end = result["end"]["offset"]
        if not (0 <= start <= end <= len(text)):
            continue
        snippet = text[start:end]
        name = param_name(text, start)
        keys, values = keys_and_values(snippet)
        if name:
            add(name)
        for key in keys:
            add(key)
        for value in values:
            add(value)

    dict_path = REPO / "dict" / f"{source.stem}.dict"
    dict_path.parent.mkdir(parents=True, exist_ok=True)
    header = [
        "# AFL++ dictionary. Entries are name=\"value\"; the name is ignored.",
        "# Extracted with rules/wordpress-batch-params.yml",
        f"# Source: {source}",
    ]
    dict_path.write_text("\n".join(header + rows) + "\n", encoding="utf-8")
    print(f"matches={len(results)} tokens={len(rows)} dict={dict_path}")


if __name__ == "__main__":
    main()
