"""Network-free destination checks for the manual LCIA validation helper."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("validate_lcia_targets.sh")
PROCESS_ID = "11111111-1111-4111-8111-111111111111"
TEST_KEY = "synthetic-key-must-not-appear-in-diagnostics"


class ValidationTargetTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="worker-validation-target-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.expected = self.root / "expected.tsv"
        self.expected.write_text(f"{PROCESS_ID}\t12.5\n")
        self.calls = self.root / "calls.jsonl"
        binary_dir = self.root / "bin"
        binary_dir.mkdir()
        curl = binary_dir / "curl"
        curl.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "args = sys.argv[1:]\n"
            "url = next(x for x in args if x.startswith(('http://', 'https://')))\n"
            "body = json.loads(args[args.index('--data') + 1])\n"
            "with open(os.environ['VALIDATION_TEST_CALLS'], 'a') as stream:\n"
            "    stream.write(json.dumps({'url': url, 'scope': body['scope']}) + '\\n')\n"
            f"print(json.dumps({{'data': {{'values': {{'{PROCESS_ID}': 12.5}}}}}}))\n"
        )
        curl.chmod(0o755)
        self.env = dict(os.environ)
        self.env.pop("LCIA_VALIDATION_BASE_URL", None)
        self.env.update(
            PATH=f"{binary_dir}{os.pathsep}{os.environ['PATH']}",
            USER_API_KEY=TEST_KEY,
            VALIDATION_TEST_CALLS=str(self.calls),
        )

    def invoke(self, *args, env_target=None):
        if env_target is not None:
            self.env["LCIA_VALIDATION_BASE_URL"] = env_target
        return subprocess.run(
            [
                "bash",
                str(SCRIPT),
                "--snapshot-id",
                "22222222-2222-4222-8222-222222222222",
                "--expected",
                str(self.expected),
                *args,
            ],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def recorded_calls(self):
        if not self.calls.exists():
            return []
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def assert_secret_safe(self, result):
        self.assertNotIn(TEST_KEY, result.stdout + result.stderr)

    def test_missing_destination_fails_before_http(self):
        result = self.invoke()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("missing --base-url or LCIA_VALIDATION_BASE_URL", result.stderr)
        self.assertEqual(self.recorded_calls(), [])
        self.assert_secret_safe(result)

    def test_explicit_loopback_destination_preserves_result_contract(self):
        result = self.invoke("--base-url", "http://127.0.0.1:54321/functions/v1/")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.recorded_calls(),
            [{"url": "http://127.0.0.1:54321/functions/v1/lca_query_results", "scope": "prod"}],
        )
        self.assertIn("validation passed", result.stdout)
        self.assert_secret_safe(result)

    def test_documented_environment_destination_is_explicit(self):
        result = self.invoke(env_target="https://custom.example.invalid/functions/v1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.recorded_calls()[0]["url"],
            "https://custom.example.invalid/functions/v1/lca_query_results",
        )
        self.assert_secret_safe(result)

    def test_flag_destination_overrides_environment(self):
        result = self.invoke(
            "--base-url",
            "http://localhost:8000/functions/v1",
            env_target="https://unused.example.invalid/functions/v1",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.recorded_calls()[0]["url"],
            "http://localhost:8000/functions/v1/lca_query_results",
        )

    def test_blank_flag_does_not_fall_back_to_environment(self):
        result = self.invoke("--base-url", "", env_target="https://unused.example.invalid")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.recorded_calls(), [])

    def test_malformed_destinations_fail_without_echoing_values(self):
        for target in ["file:///tmp/results", "https://", "https://host.invalid/a b", "https://user:private@host.invalid"]:
            with self.subTest(target=target):
                result = self.invoke("--base-url", target)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertEqual(self.recorded_calls(), [])
                self.assertNotIn(target, result.stdout + result.stderr)
                self.assert_secret_safe(result)

    def test_missing_flag_value_is_a_usage_error(self):
        result = self.invoke("--base-url")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("missing value for --base-url", result.stderr)
        self.assertEqual(self.recorded_calls(), [])


if __name__ == "__main__":
    unittest.main()
