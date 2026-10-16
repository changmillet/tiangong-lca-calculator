"""Small real-library qualification with synthetic inputs and no service access."""

import importlib
import io
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import h5py
import numpy as np


def setUpModule():
    directory = tempfile.TemporaryDirectory(prefix="bw25-smoke-")
    previous = os.environ.get("BRIGHTWAY2_DIR")
    os.environ["BRIGHTWAY2_DIR"] = directory.name

    def cleanup():
        if previous is None:
            os.environ.pop("BRIGHTWAY2_DIR", None)
        else:
            os.environ["BRIGHTWAY2_DIR"] = previous
        directory.cleanup()

    unittest.addModuleCleanup(cleanup)


class DependencySmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cli = importlib.import_module("bw25_validator.cli")

    def setUp(self):
        for target in ("requests.sessions.Session.request", "psycopg.connect", "boto3.client"):
            guard = patch(target, side_effect=AssertionError("Offline smoke must not access services"))
            guard.start()
            self.addCleanup(guard.stop)
        self.m, self.b, self.c = self.cli.build_matrices(
            {
                "process_count": 2,
                "flow_count": 1,
                "impact_count": 1,
                "technosphere_entries": [{"row": 0, "col": 1, "value": 0.25}],
                "biosphere_entries": [
                    {"row": 0, "col": 0, "value": 2.0},
                    {"row": 0, "col": 1, "value": 3.0},
                ],
                "characterization_factors": [{"row": 0, "col": 0, "value": 4.0}],
            }
        )

    def validate(self, result):
        return self.cli.validate_solve_one(
            m=self.m, b=self.b, c=self.c, n=2,
            result_payload=result, job_payload={"rhs": [0.0, 1.0]}, atol=1e-10, rtol=1e-10,
        )

    def test_real_brightway_solve_and_impact_match_independent_values(self):
        # M = [[1, -.25], [0, 1]], so x=(.25, 1), Bx=3.5 and CBx=14.
        report = self.validate({"x": [0.25, 1.0], "g": [3.5], "h": [14.0]})
        for name in ("x_metrics", "g_metrics", "h_metrics"):
            self.assertTrue(report[name]["pass_threshold"], name)
        self.assertAlmostEqual(report["bw_residual_rel"], 0.0, places=12)
        self.assertAlmostEqual(report["lcia_check"]["bw_score"], 14.0, places=10)

    def test_incorrect_stored_numerical_result_is_rejected(self):
        report = self.validate({"x": [0.5, 1.0], "g": [4.0], "h": [16.0]})
        for name in ("x_metrics", "g_metrics", "h_metrics"):
            self.assertFalse(report[name]["pass_threshold"], name)

    def test_platform_sparse_backend_solves_known_system(self):
        if platform.system() == "Linux" and platform.machine() == "x86_64":
            from pypardiso import spsolve
        else:
            from scipy.sparse.linalg import spsolve
        result = spsolve(self.m, np.array([0.0, 1.0]))
        np.testing.assert_allclose(result, [0.25, 1.0], rtol=1e-10, atol=1e-10)

    def test_rhs_dimension_mismatch_stops_before_solving(self):
        with patch.object(self.cli, "run_brightway_lca", side_effect=AssertionError("No solve")):
            with self.assertRaisesRegex(SystemExit, "rhs length mismatch"):
                self.cli.validate_solve_one(
                    m=self.m, b=self.b, c=self.c, n=2,
                    result_payload={}, job_payload={"rhs": [1.0]}, atol=1e-10, rtol=1e-10,
                )

    def test_compressed_hdf5_envelope_round_trip(self):
        envelope = {"payload": {"rhs": [0.0, 1.0], "label": "synthetic 气候"}}
        with io.BytesIO() as stream:
            with h5py.File(stream, "w") as file:
                for name, data in (
                    ("format", b"hdf5:v1"),
                    ("envelope_json", json.dumps(envelope, ensure_ascii=False).encode("utf-8")),
                ):
                    file.create_dataset(name, data=np.frombuffer(data, dtype=np.uint8), compression="gzip")
            raw = stream.getvalue()
        self.assertEqual(self.cli.decode_hdf5_envelope(raw), {"format": "hdf5:v1", "envelope": envelope})

    def test_packaged_cli_modules_show_help_without_service_configuration(self):
        console = Path(sys.executable).parent / (
            "bw25-validate.exe" if os.name == "nt" else "bw25-validate"
        )
        self.assertTrue(console.is_file(), console)
        commands = [[str(console), "--help"]] + [
            [sys.executable, "-m", f"bw25_validator.{module}", "--help"]
            for module in ("cli", "expected_cli", "export_matrices_cli")
        ]
        for command in commands:
            with self.subTest(command=command):
                process = subprocess.run(
                    command,
                    cwd=Path(os.environ["BRIGHTWAY2_DIR"]),
                    text=True, capture_output=True, check=False, timeout=30,
                )
                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertIn("usage:", process.stdout)


if __name__ == "__main__":
    unittest.main()
