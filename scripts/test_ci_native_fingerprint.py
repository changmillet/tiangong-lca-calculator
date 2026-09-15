"""Exercise the native cache boundary without installing packages or using a cache."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("ci_native_fingerprint.py")
PACKAGES = "\n".join(
    f"{name}\t1.2-3ubuntu1\tamd64\tinstalled"
    for name in ("libsuitesparse-dev", "libopenblas-dev", "liblapack-dev", "pkg-config", "libc6")
) + "\n"
ALTERNATIVES = "cc auto /usr/bin/gcc\nlibblas.so-x86_64-linux-gnu auto /usr/lib/openblas/libblas.so\n"
ENV = {"ImageOS": "ubuntu24", "ImageVersion": "20260913.1.0"}


class NativeFingerprintTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("ci_native_fingerprint", SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.replies = [PACKAGES, ALTERNATIVES, "cc (Ubuntu) 14.2.0\nCopyright compiler\n", "1.8.1\n"]

    def fingerprint(self, replies=None, env=None):
        values = self.replies if replies is None else replies
        with patch.object(self.module.subprocess, "run", side_effect=[
            subprocess.CompletedProcess([], 0, reply, "") for reply in values
        ]) as run:
            result = self.module.fingerprint(ENV if env is None else env)
        return result, run

    def test_reordered_inventories_are_identical(self):
        expected, _ = self.fingerprint()
        values = list(self.replies)
        values[0] = "\n".join(reversed(PACKAGES.splitlines())) + "\n"
        values[1] = "\n".join(reversed(ALTERNATIVES.splitlines())) + "\n"
        actual, _ = self.fingerprint(values)
        self.assertEqual(expected, actual)
        self.assertRegex(actual, r"^[0-9a-f]{64}$")

    def test_every_native_input_changes_identity(self):
        expected, _ = self.fingerprint()
        for field in ENV:
            with self.subTest(field=field):
                actual, _ = self.fingerprint(env={**ENV, field: ENV[field] + "-changed"})
                self.assertNotEqual(expected, actual)
        mutations = [(0, "1.2-3ubuntu1", "1.2-3ubuntu2"),
                     (0, "amd64", "arm64"),
                     (0, "libc6\t", "libc7\t"),
                     (1, "/usr/bin/gcc", "/usr/bin/clang"),
                     (1, "auto", "manual"),
                     (2, "14.2.0", "14.2.1"), (3, "1.8.1", "1.8.2")]
        for index, old, new in mutations:
            with self.subTest(index=index, old=old):
                values = list(self.replies)
                values[index] = values[index].replace(old, new)
                actual, _ = self.fingerprint(values)
                self.assertNotEqual(expected, actual)

    def test_required_installed_packages_cannot_be_omitted_or_removed(self):
        for package in ("libsuitesparse-dev", "libopenblas-dev", "liblapack-dev", "pkg-config"):
            for mode in ("missing", "config-files"):
                with self.subTest(package=package, mode=mode):
                    values = list(self.replies)
                    values[0] = "\n".join(
                        row.replace("installed", mode) if row.startswith(package + "\t") else row
                        for row in PACKAGES.splitlines()
                        if mode != "missing" or not row.startswith(package + "\t")
                    )
                    with self.assertRaises(self.module.FingerprintError):
                        self.fingerprint(values)

    def test_missing_empty_or_malformed_metadata_fails(self):
        for field in ENV:
            for value in (None, "", " \n", "ubuntu\n24"):
                with self.subTest(field=field, value=value):
                    env = dict(ENV)
                    if value is None:
                        del env[field]
                    else:
                        env[field] = value
                    with self.assertRaises(self.module.FingerprintError):
                        self.fingerprint(env=env)
        mutations = [(i, "") for i in range(4)] + [
            (0, PACKAGES + "malformed\n"), (0, PACKAGES + PACKAGES.splitlines()[0] + "\n"),
            (0, PACKAGES.replace("amd64", "")), (0, PACKAGES.replace("installed", "broken")),
            (1, "cc invalid /usr/bin/gcc\n"), (1, "cc auto relative/path\n"),
            (1, ALTERNATIVES + "cc auto /usr/bin/clang\n"), (2, "not a compiler version\n"),
            (3, "not a version\n"),
        ]
        for index, value in mutations:
            with self.subTest(index=index, value=value):
                replies = list(self.replies)
                replies[index] = value
                with self.assertRaises(self.module.FingerprintError):
                    self.fingerprint(replies)

    def test_argv_locale_and_command_failures(self):
        _, run = self.fingerprint()
        calls = run.call_args_list
        self.assertEqual(calls[0].args[0], ["dpkg-query", "-W", "-f=${Package}\t${Version}\t${Architecture}\t${db:Status-Status}\n"])
        self.assertEqual([call.args[0] for call in calls[1:]], [
            ["update-alternatives", "--get-selections"], ["cc", "--version"], ["pkg-config", "--version"]])
        for call in calls:
            self.assertEqual(call.kwargs["env"]["LC_ALL"], "C")
            self.assertNotIn("shell", call.kwargs)
            self.assertGreater(call.kwargs["timeout"], 0)
        for index in range(4):
            for failure in (OSError("private path"), subprocess.TimeoutExpired("secret", 1),
                            subprocess.CompletedProcess([], 2, "private output", "private stderr")):
                with self.subTest(index=index, failure=type(failure).__name__):
                    responses = [subprocess.CompletedProcess([], 0, reply, "") for reply in self.replies[:index]] + [failure]
                    with patch.object(self.module.subprocess, "run", side_effect=responses):
                        with self.assertRaises(self.module.FingerprintError) as caught:
                            self.module.fingerprint(ENV)
                        self.assertNotIn("private", str(caught.exception))
                        self.assertNotIn("secret", str(caught.exception))

    def test_cli_only_publishes_hash_after_success(self):
        # Execute the real __main__/argparse boundary in a fresh Python process.
        # Mock command transport inside that process, so local qualification also
        # runs on Windows without relying on POSIX executable files or a shell.
        driver = """
import json, runpy, subprocess, sys
from unittest.mock import patch
script, encoded, *extra = sys.argv[1:]
responses = [subprocess.CompletedProcess([], *row) for row in json.loads(encoded)]
sys.argv = [script, *extra]
with patch.object(subprocess, 'run', side_effect=responses):
    runpy.run_path(script, run_name='__main__')
"""
        env = {**os.environ, **ENV}

        def execute(replies, *extra):
            return subprocess.run(
                [sys.executable, "-c", driver, str(SCRIPT), json.dumps(replies), *extra],
                env=env, capture_output=True, text=True,
            )

        replies = [[0, reply, ""] for reply in self.replies]
        result = execute(replies)
        self.assertEqual(result.returncode, 0, result.stderr)
        expected, _ = self.fingerprint()
        self.assertEqual(result.stdout, f"native_hash={expected}\n")
        for failure in ([2, "private output", "private error"], [0, "", ""]):
            with self.subTest(command_result=failure):
                result = execute(replies[:2] + [failure])
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertNotIn("private", result.stderr)
                self.assertNotIn("Traceback", result.stderr)
        result = execute(replies, "--unexpected")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
