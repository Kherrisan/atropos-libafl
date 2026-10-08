#!/usr/bin/env python3
"""Read projects/<name>/project.yaml. The subset used here is flat scalars."""

import hashlib
import os
import sys


REQUIRED = ("name", "language", "main_repo", "backend", "build_script", "setup_script")


def parse_yaml(text):
    data = {}
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line or ":" not in line:
            continue
        key, value = line.split(":", 1)
        data[key.strip()] = value.strip().strip("\"'")
    return data


def locate(project):
    if os.path.isdir(project):
        directory = os.path.realpath(project)
        path = os.path.join(directory, "project.yaml")
    else:
        path = os.path.realpath(project)
        directory = os.path.dirname(path)
    if not os.path.isfile(path):
        raise SystemExit(f"project.yaml is missing: {path}")
    return directory, path


def load(project_dir):
    root, path = locate(project_dir)
    with open(path, encoding="utf-8") as handle:
        data = parse_yaml(handle.read())
    missing = [key for key in REQUIRED if not data.get(key)]
    if missing:
        raise SystemExit("project.yaml is missing fields: " + ", ".join(missing))
    if data["backend"] not in ("php", "spring"):
        raise SystemExit(f"invalid backend {data['backend']}; use php or spring")
    for field, kind in (
        ("build_script", "file"),
        ("setup_script", "file"),
        ("auth_script", "file"),
        ("seeds", "dir"),
    ):
        if field not in data or not data[field]:
            continue
        target = os.path.realpath(os.path.join(root, data[field]))
        if target != root and not target.startswith(root + os.sep):
            raise SystemExit(f"{field} escapes the project directory: {data[field]}")
        if kind == "file" and not os.path.isfile(target):
            raise SystemExit(f"{field} does not exist: {target}")
        if kind == "dir" and not os.path.isdir(target):
            raise SystemExit(f"{field} is not a directory: {target}")
        data[field + "_path"] = target
    data["dir"] = root
    data["yaml"] = path
    return data


def fingerprint(project, src):
    digest = hashlib.sha256()
    paths = [project.get("yaml") or os.path.join(project["dir"], "project.yaml")]
    for field in ("build_script_path", "setup_script_path", "auth_script_path"):
        if field in project:
            paths.append(project[field])
    if src and os.path.isdir(src):
        for dirpath, dirnames, filenames in os.walk(src):
            dirnames[:] = [name for name in dirnames if name != ".git"]
            for name in filenames:
                paths.append(os.path.join(dirpath, name))
    for path in sorted(paths):
        stat = os.stat(path)
        digest.update(path.encode())
        digest.update(str(stat.st_size).encode())
        digest.update(str(stat.st_mtime_ns).encode())
    return digest.hexdigest()


def shell_quote(value):
    return "'" + value.replace("'", "'\\''") + "'"


def main():
    if len(sys.argv) < 3:
        raise SystemExit("usage: load-project.py shell|check|hash PROJECT_YAML_OR_DIR [--src DIR]")
    action, project_dir = sys.argv[1], sys.argv[2]
    src = ""
    if "--src" in sys.argv:
        src = sys.argv[sys.argv.index("--src") + 1]
    project = load(project_dir)
    if action == "check":
        return
    if action == "hash":
        print(fingerprint(project, src))
        return
    if action != "shell":
        raise SystemExit(f"unknown action {action}")
    fields = {
        "PROJECT_NAME": project["name"],
        "PROJECT_LANGUAGE": project["language"],
        "PROJECT_MAIN_REPO": project["main_repo"],
        "PROJECT_BACKEND": project["backend"],
        "PROJECT_DIR": project["dir"],
        "PROJECT_BUILD_SCRIPT": project.get("build_script_path", ""),
        "PROJECT_SETUP_SCRIPT": project.get("setup_script_path", ""),
        "PROJECT_AUTH_SCRIPT": project.get("auth_script_path", ""),
        "PROJECT_SEEDS": project.get("seeds_path", ""),
    }
    for key, value in fields.items():
        print(f"{key}={shell_quote(value)}")


if __name__ == "__main__":
    main()
