import copy
import json
import unittest
from check_study_method_runtime import FIXTURE, verify


class MethodRuntimeOracle(unittest.TestCase):
    def setUp(self):
        self.value = json.loads(FIXTURE.read_bytes())

    def reject(self, change):
        value = copy.deepcopy(self.value)
        change(value)
        with self.assertRaises((ValueError, KeyError)):
            verify(value)

    def test_exact_synthetic_fixture(self):
        self.assertEqual(verify(self.value)['categoryFacts'], 25)

    def test_wrong_family_job_or_config(self):
        self.reject(lambda v: v.__setitem__('jobKind', 'study.analysis_advanced'))
        self.reject(lambda v: v['payload'].__setitem__('assessmentId', 'other'))
        self.reject(lambda v: v['record'].__setitem__('configHash', '0' * 64))

    def test_missing_requirement_or_scalar(self):
        self.reject(lambda v: v['facts']['requirements'].pop())
        self.reject(lambda v: v['facts']['factCatalog'].pop())

    def test_forced_qualification_or_zero_replaces_unavailable(self):
        self.reject(lambda v: v['facts'].__setitem__('pefConformityQualified', True))
        self.reject(lambda v: next(f for f in v['facts']['factCatalog'] if f['factId'].startswith('normalised:')).__setitem__('canonicalValue', '0'))

    def test_swapped_provenance_and_raw_factor_budget(self):
        self.reject(lambda v: v['facts']['factCatalog'][0].__setitem__('unit', 'wrong'))
        self.reject(lambda v: v['completion'].__setitem__('selectedFactorRecordCount', 9000))

    def test_nf_weight_and_decomposition_source_substitution(self):
        self.reject(lambda v: next(f for f in v['facts']['factCatalog'] if f['factId'].startswith('normalised:'))['unitIdentity'].__setitem__('sourceNormalisationFactor', '1'))
        self.reject(lambda v: next(f for f in v['facts']['factCatalog'] if f['factId'].startswith('weighted:'))['unitIdentity'].__setitem__('scalingFactor', '100'))
        self.reject(lambda v: v['facts']['factCatalog'][-1]['unitIdentity']['primaryMethods'].reverse())


if __name__ == '__main__':
    unittest.main()
