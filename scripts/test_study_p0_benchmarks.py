"""Adversarial checks for the offline oracle and native evidence reader."""

import copy
from fractions import Fraction
import hashlib
import json
import unittest

import check_study_p0_benchmarks as checker


class StudyP0BenchmarksTest(unittest.TestCase):
    def setUp(self):
        self.profile_bytes = (checker.FIXTURES / "profile.json").read_bytes()
        self.fixture_bytes = (checker.FIXTURES / "benchmarks.json").read_bytes()
        self.profile = json.loads(self.profile_bytes)
        self.fixture = json.loads(self.fixture_bytes)
        self.candidate = json.loads((checker.FIXTURES / "historical-cement-candidate.json").read_bytes())

    def check(self):
        checker.check_documents(self.profile, self.fixture, self.candidate)

    def test_reviewed_exact_equations_and_semantic_cases(self):
        self.check()

    def test_wrong_activity_inventory_or_impact_is_rejected(self):
        for key in ("x", "g", "h"):
            with self.subTest(key=key):
                case = copy.deepcopy(self.fixture["systems"][0])
                case["expected"][key][0] = "0"
                with self.assertRaisesRegex(ValueError, "!="):
                    checker.check_system(case)

    def test_residual_is_not_sufficient_without_unique_solution(self):
        case = copy.deepcopy(self.fixture["systems"][1])
        case["A"] = [["1"]]
        case["y"] = ["0"]
        with self.assertRaisesRegex(ValueError, "unique solution"):
            checker.check_system(case)

    def test_instance_axis_cannot_merge_same_source(self):
        case = copy.deepcopy(self.fixture["systems"][2])
        case["activities"][1]["instanceId"] = "left"
        with self.assertRaisesRegex(ValueError, "instance axis"):
            checker.check_system(case)

    def test_dropped_case_cannot_shrink_qualification(self):
        self.fixture["allocationCases"].pop()
        with self.assertRaisesRegex(ValueError, "qualification case"):
            self.check()

    def test_zero_share_cannot_default_to_one(self):
        case = copy.deepcopy(self.fixture["allocationCases"][-1])
        case["exchanges"][-1].pop("allocations")
        with self.assertRaisesRegex(ValueError, "wrong selected share"):
            checker.check_allocation(case)

    def test_input_reference_direction_cannot_flip(self):
        case = copy.deepcopy(self.fixture["allocationCases"][2])
        case["exchanges"][0]["exchangeDirection"] = "Output"
        with self.assertRaisesRegex(ValueError, "normalization"):
            checker.check_allocation(case)

    def test_missing_factor_cannot_be_labeled_zero(self):
        self.fixture["diagnosticCases"][3]["expected"] = "characterized_zero"
        with self.assertRaisesRegex(ValueError, "diagnostic"):
            self.check()

    def test_unit_label_cannot_replace_versioned_unit_evidence(self):
        case = copy.deepcopy(self.fixture["diagnosticCases"][1])
        case["left"]["unit"] = "kg"
        self.assertEqual(checker.diagnostic(case), "reference_unit_missing")

    def test_cannot_promote_candidate_or_enable_runtime(self):
        self.profile["runtimeEnabled"] = True
        with self.assertRaisesRegex(ValueError, "enable"):
            self.check()
        self.profile["runtimeEnabled"] = False
        self.candidate["status"] = "qualified"
        with self.assertRaisesRegex(ValueError, "business acceptance"):
            self.check()

    def native_reader_fixture(self):
        # This synthetic envelope tests the reader only, never native execution.
        descriptor = json.dumps({
            "schema": "lca.calculation-contract.v1", "engine": "solver-worker",
            "allocation_semantics": self.profile["semantics"]["allocation"],
            "lcia_selection": self.profile["semantics"]["lciaSelection"],
        })
        return {
            "schema": "lca.study-p0-native-result.v1",
            "fixtureSha256": hashlib.sha256(self.fixture_bytes).hexdigest(),
            "profileSha256": hashlib.sha256(self.profile_bytes).hexdigest(),
            "scope": "core-solve-and-allocation-primitives-not-model-compiler",
            "businessPilotQualified": False,
            "descriptorBytes": descriptor,
            "calculationContractRef": {
                "kind": "calculation_contract", "schema": "lca.calculation-contract.v1",
                "sha256": hashlib.sha256(descriptor.encode()).hexdigest(),
            },
            "systems": [{"id": case["id"], **{key: [float(Fraction(value)) for value in values] for key, values in case["expected"].items()}} for case in self.fixture["systems"]],
            "allocationCases": [{"id": case["id"], "fractions": [float(Fraction(value)) for value in case["expectedFractions"]], "normalizedSigned": [float(Fraction(value)) for value in case["expectedNormalizedSigned"]]} for case in self.fixture["allocationCases"]],
        }

    def check_native(self, report):
        checker.check_native(report, self.profile_bytes, self.fixture_bytes, self.fixture, self.profile)

    def test_native_reader_accepts_matching_numbers(self):
        self.check_native(self.native_reader_fixture())

    def test_native_reader_rejects_hash_drift_or_changed_result(self):
        for mutate in (
            lambda report: report.update(fixtureSha256="0" * 64),
            lambda report: report.update(profileSha256="0" * 64),
            lambda report: report.update(descriptorBytes=report["descriptorBytes"] + " "),
            lambda report: report["systems"][0]["h"].__setitem__(0, 0),
            lambda report: report["allocationCases"][-1]["fractions"].__setitem__(-1, 1),
            lambda report: report.update(businessPilotQualified=True),
        ):
            report = self.native_reader_fixture()
            mutate(report)
            with self.assertRaises(ValueError):
                self.check_native(report)

    def test_native_reader_rejects_missing_case_and_nonfinite_values(self):
        report = self.native_reader_fixture()
        report["systems"].pop()
        with self.assertRaises(ValueError):
            self.check_native(report)
        report = self.native_reader_fixture()
        report["systems"][0]["x"][0] = float("nan")
        with self.assertRaises(ValueError):
            self.check_native(report)


if __name__ == "__main__":
    unittest.main()
