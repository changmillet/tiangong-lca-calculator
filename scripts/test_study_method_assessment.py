"""Adversarial oracle checks: exact source provenance, scale and role exclusions."""
import copy
import json
import unittest
from check_study_method_assessment import FIXTURE, verify


class MethodArithmeticOracle(unittest.TestCase):
    def setUp(self):
        self.original = json.loads(FIXTURE.read_bytes())

    def reject(self, change):
        value = copy.deepcopy(self.original)
        change(value)
        with self.assertRaises((ValueError, KeyError)):
            verify(value)

    def test_exact_native_fixture(self):
        self.assertEqual(verify(self.original)['primaryCategories'], 16)

    def test_percent_division_and_implicit_point_scaling_fail(self):
        self.reject(lambda v: v['normalisationWeighting'].__setitem__('canonicalCandidateSingleScore', str(float(v['normalisationWeighting']['canonicalCandidateSingleScore']) / 100)))
        self.reject(lambda v: v['normalisationWeighting'].__setitem__('scalingFactor', '1000'))

    def test_swapped_method_order_fails(self):
        def change(v):
            rows = v['normalisationWeighting']['categories']
            rows[0], rows[1] = rows[1], rows[0]
        self.reject(change)

    def test_source_factor_unit_or_reference_substitution_fails(self):
        for key in ['sourceNormalisationFactor', 'sourceWeightFraction', 'sourceUnitLabel', 'characterisedUnit', 'referenceQuantityId']:
            with self.subTest(key=key):
                self.reject(lambda v: v['normalisationWeighting']['categories'][0].__setitem__(key, 'substituted'))

    def test_missing_factor_cannot_be_complete(self):
        self.reject(lambda v: next(c for c in v['inputCategories'] if c['role'] == 'primary').__setitem__('coverage', 'unknown'))

    def test_decomposition_addition_or_role_change_fails(self):
        self.reject(lambda v: v['normalisationWeighting']['categories'].append(v['normalisationWeighting']['categories'][0]))
        self.reject(lambda v: next(c for c in v['inputCategories'] if c['role'] == 'primary').__setitem__('role', 'subcategory'))

    def test_nonfinite_and_wrong_arithmetic_fail(self):
        self.reject(lambda v: v['normalisationWeighting']['categories'][1].__setitem__('canonicalNormalisedValue', 'NaN'))
        self.reject(lambda v: v['normalisationWeighting']['categories'][1].__setitem__('canonicalWeightedValue', '1'))

    def test_module_or_fixture_scope_substitution_fails(self):
        self.reject(lambda v: v.__setitem__('moduleArtifactSha256', '0' * 64))
        self.reject(lambda v: v.__setitem__('classification', 'pef_certified'))


if __name__ == '__main__':
    unittest.main()
