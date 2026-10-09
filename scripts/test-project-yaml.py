#!/usr/bin/env python3
"""Checks for projects/*/project.yaml and the atropos.sh build skip."""

import os
import stat
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
LOAD = REPO / "scripts" / "load-project.py"
ATROPOS = REPO / "scripts" / "atropos.sh"
PHP_MARKER = "php-code-coverage-9.2.31+phpcov-8.2.1+fastcgi-v1"
AGENT_MARKER = "nyx-agent-fastcgi-v1"


def run(args, check=True, env=None):
    merged = os.environ.copy()
    if env:
        merged.update(env)
    result = subprocess.run(
        args,
        cwd=REPO,
        text=True,
        capture_output=True,
        check=False,
        env=merged,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            f"{args} exited {result.returncode}\n{result.stdout}\n{result.stderr}"
        )
    return result


def write_project(root, body):
    root.mkdir(parents=True)
    (root / "project.yaml").write_text(textwrap.dedent(body), encoding="utf-8")
    for name in ("build.sh", "setup.sh", "auth.py"):
        path = root / name
        path.write_text("#!/bin/sh\n", encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IEXEC)


class ProjectYamlTest(unittest.TestCase):
    def test_shipped_projects_check(self):
        for name in ("wordpress", "webgoat"):
            result = run(["python3", str(LOAD), "check", str(REPO / "projects" / name)])
            self.assertEqual(result.stdout, "")

    def test_missing_backend_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            write_project(
                root,
                """
                name: app
                language: php
                main_repo: https://example.invalid/app
                build_script: build.sh
                setup_script: setup.sh
                """,
            )
            result = run(["python3", str(LOAD), "check", str(root)], check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("backend", result.stderr)

    def test_invalid_backend_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            write_project(
                root,
                """
                name: app
                language: ruby
                main_repo: https://example.invalid/app
                backend: ruby
                build_script: build.sh
                setup_script: setup.sh
                """,
            )
            result = run(["python3", str(LOAD), "check", str(root)], check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid backend ruby", result.stderr)

    def test_seeds_directory_is_loaded(self):
        result = run(
            ["python3", str(LOAD), "shell", str(REPO / "projects" / "wordpress" / "project.yaml")]
        )
        script = result.stdout + "printf '%s\\n' \"$PROJECT_SEEDS\"\n"
        quoted = subprocess.run(
            ["bash", "-c", script], text=True, capture_output=True, check=True
        )
        self.assertEqual(
            quoted.stdout.strip(),
            str((REPO / "projects" / "wordpress" / "seeds").resolve()),
        )

    def test_seeds_path_cannot_escape_the_project(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            write_project(
                root,
                """
                name: app
                language: php
                main_repo: https://example.invalid/app
                backend: php
                build_script: build.sh
                setup_script: setup.sh
                seeds: ../../seeds
                """,
            )
            result = run(["python3", str(LOAD), "check", str(root)], check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("seeds escapes the project directory", result.stderr)

    def test_script_path_cannot_escape_the_project(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            write_project(
                root,
                """
                name: app
                language: php
                main_repo: https://example.invalid/app
                backend: php
                build_script: ../../etc/passwd
                setup_script: setup.sh
                """,
            )
            result = run(["python3", str(LOAD), "check", str(root)], check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("escapes the project directory", result.stderr)

    def test_shell_quoting_round_trip(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            write_project(
                root,
                """
                name: app
                language: php
                main_repo: https://example.invalid/o'hara
                backend: php
                build_script: build.sh
                setup_script: setup.sh
                """,
            )
            result = run(["python3", str(LOAD), "shell", str(root)])
            script = result.stdout + "printf '%s\\n' \"$PROJECT_MAIN_REPO\"\n"
            quoted = subprocess.run(
                ["bash", "-c", script],
                text=True,
                capture_output=True,
                check=True,
            )
            self.assertEqual(quoted.stdout.strip(), "https://example.invalid/o'hara")

    def test_hash_changes_when_source_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            source = Path(temporary) / "src"
            source.mkdir()
            (source / "index.php").write_text("<?php\n", encoding="utf-8")
            write_project(
                root,
                """
                name: app
                language: php
                main_repo: https://example.invalid/app
                backend: php
                build_script: build.sh
                setup_script: setup.sh
                """,
            )
            first = run(
                ["python3", str(LOAD), "hash", str(root), "--src", str(source)]
            ).stdout.strip()
            second = run(
                ["python3", str(LOAD), "hash", str(root), "--src", str(source)]
            ).stdout.strip()
            self.assertEqual(first, second)
            with (source / "index.php").open("a", encoding="utf-8") as handle:
                handle.write("echo 1;\n")
            changed = run(
                ["python3", str(LOAD), "hash", str(root), "--src", str(source)]
            ).stdout.strip()
            self.assertNotEqual(first, changed)

    def test_build_skips_when_artifacts_match_the_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "sample"
            source = Path(temporary) / "src"
            output = Path(temporary) / "nyx"
            php = Path(temporary) / "php"
            source.mkdir()
            (source / "index.php").write_text("<?php\n", encoding="utf-8")
            write_project(
                root,
                """
                name: sample
                language: php
                main_repo: https://example.invalid/sample
                backend: php
                build_script: build.sh
                setup_script: setup.sh
                auth_script: auth.py
                """,
            )
            (root / "build.sh").write_text(
                "#!/bin/sh\nmkdir -p \"$OUT\"\ntouch \"$OUT/ran\"\n",
                encoding="utf-8",
            )
            digest = run(
                ["python3", str(LOAD), "hash", str(root), "--src", str(source)]
            ).stdout.strip()
            php.mkdir()
            (php / "nyx-php-runtime.tar.gz").write_bytes(b"runtime")
            (php / "php-code-coverage-runtime").write_text(PHP_MARKER + "\n", encoding="utf-8")
            (php / "atropos-agent-phpcov-runtime").write_text(AGENT_MARKER + "\n", encoding="utf-8")
            agent = php / "atropos_agent"
            agent.write_text("#!/bin/sh\n", encoding="utf-8")
            agent.chmod(0o755)
            bundle = output / "bundle"
            bundle.mkdir(parents=True)
            (bundle / "guest-bundle.tar.gz").write_bytes(b"bundle")
            stamp_dir = output / "projects" / "sample"
            stamp_dir.mkdir(parents=True)
            (stamp_dir / "source.stamp").write_text(digest + "\n", encoding="utf-8")
            result = run(
                [
                    "bash",
                    str(ATROPOS),
                    "build",
                    "--project",
                    str(root / "project.yaml"),
                    "--php-output",
                    str(php),
                    "--php-src",
                    str(Path(temporary) / "missing-php"),
                    "--pcov-src",
                    str(Path(temporary) / "missing-pcov"),
                    str(source),
                ],
                env={"NYX_HOME": str(output)},
            )
            self.assertIn("skipping build for sample", result.stdout)
            self.assertFalse((stamp_dir / "out" / "ran").exists())

    def test_setup_requires_a_bundle(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "sample"
            output = Path(temporary) / "nyx"
            write_project(
                root,
                """
                name: sample
                language: php
                main_repo: https://example.invalid/sample
                backend: php
                build_script: build.sh
                setup_script: setup.sh
                """,
            )
            result = run(
                [
                    "bash",
                    str(ATROPOS),
                    "setup",
                    str(root),
                ],
                check=False,
                env={"NYX_HOME": str(output)},
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("guest bundle is missing", result.stderr)

    def test_project_directory_yaml_is_used_directly(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "sample"
            output = Path(temporary) / "nyx"
            write_project(
                root,
                """
                name: sample
                language: php
                main_repo: https://example.invalid/sample
                backend: php
                build_script: build.sh
                setup_script: setup.sh
                """,
            )
            result = run(
                [
                    "bash",
                    str(ATROPOS),
                    "setup",
                    "--project",
                    str(root),
                ],
                check=False,
                env={"NYX_HOME": str(output)},
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("guest bundle is missing", result.stderr)
            self.assertIn(str(root / "project.yaml"), result.stderr)

    def test_output_flag_belongs_to_fuzz(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "sample"
            nyx = Path(temporary) / "nyx"
            write_project(
                root,
                """
                name: sample
                language: php
                main_repo: https://example.invalid/sample
                backend: php
                build_script: build.sh
                setup_script: setup.sh
                """,
            )
            rejected = run(
                [
                    "bash",
                    str(ATROPOS),
                    "build",
                    "--output",
                    str(Path(temporary) / "run"),
                    "--project",
                    str(root / "project.yaml"),
                    str(root),
                ],
                check=False,
                env={"NYX_HOME": str(nyx)},
            )
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("--output is only valid for fuzz and run", rejected.stderr)
            accepted = run(
                [
                    "bash",
                    str(ATROPOS),
                    "fuzz",
                    "--output",
                    str(Path(temporary) / "run"),
                    "--project",
                    str(Path(temporary) / "missing.yaml"),
                ],
                check=False,
                env={"NYX_HOME": str(nyx)},
            )
            self.assertNotEqual(accepted.returncode, 0)
            self.assertNotIn("does not take extra arguments", accepted.stderr)
            self.assertIn("project.yaml", accepted.stderr)

    def test_webgoat_build_script_copies_a_local_jar(self):
        with tempfile.TemporaryDirectory() as temporary:
            jar = Path(temporary) / "upstream.jar"
            jar.write_bytes(b"jar-bytes")
            out = Path(temporary) / "out"
            env = os.environ.copy()
            env["OUT"] = str(out)
            env["WEBGOAT_URL"] = jar.as_uri()
            subprocess.run(
                ["bash", str(REPO / "projects" / "webgoat" / "build.sh")],
                cwd=REPO / "projects" / "webgoat",
                env=env,
                check=True,
                text=True,
                capture_output=True,
            )
            self.assertEqual((out / "webgoat.jar").read_bytes(), b"jar-bytes")
            self.assertEqual((out / "app-kind").read_text(encoding="utf-8").strip(), "webgoat")


if __name__ == "__main__":
    unittest.main()
