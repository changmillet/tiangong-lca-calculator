"""Negative checks for the bounded real-data projection, with no private packet."""

import copy
import json
import unittest

import check_study_p0_cement as checker


class StudyP0CementTest(unittest.TestCase):
    def setUp(self):
        self.fixture = json.loads(checker.FIXTURE.read_bytes())

    def test_bounded_real_inventory_and_independent_subtotals(self):
        checker.check_cement(self.fixture)

    def test_requested_historical_version_cannot_alias_current_data(self):
        self.fixture["source"]["process"]["requestedVersion"] = "00.01.005"
        with self.assertRaisesRegex(ValueError, "no fallback"):
            checker.check_cement(self.fixture)

    def test_source_comment_cannot_silently_change_numerical_reference(self):
        self.fixture["scope"]["referenceProductAmount"] = "1"
        with self.assertRaisesRegex(ValueError, "1000 kg"):
            checker.check_cement(self.fixture)

    def test_public_state_does_not_qualify_business_or_science(self):
        self.fixture["scope"]["scientificReviewComplete"] = True
        with self.assertRaisesRegex(ValueError, "qualification"):
            checker.check_cement(self.fixture)

    def test_incomplete_method_cannot_hide_uncharacterized_flows(self):
        self.fixture["methods"][0]["uncharacterizedExchangeIds"] = []
        with self.assertRaisesRegex(ValueError, "unknown coverage"):
            checker.check_cement(self.fixture)

    def test_regional_alternatives_cannot_be_added(self):
        self.fixture["methods"][1]["selectedFactors"][0]["value"] = "48.47"
        with self.assertRaisesRegex(ValueError, "must not be summed"):
            checker.check_cement(self.fixture)

    def test_reference_unit_label_cannot_override_source_evidence(self):
        self.fixture["exchanges"][3]["unit"] = "kWh"
        with self.assertRaisesRegex(ValueError, "unit mismatch"):
            checker.check_cement(self.fixture)

    def test_numeric_support_version_drift_or_missing_document_fails(self):
        for remove in (False, True):
            with self.subTest(remove=remove):
                fixture = copy.deepcopy(self.fixture)
                if remove:
                    fixture["numericSupport"]["unitGroups"].pop()
                else:
                    fixture["numericSupport"]["flowProperties"][0]["version"] = "00.00.000"
                with self.assertRaises((ValueError, KeyError)):
                    checker.check_cement(fixture)

    def test_private_account_metadata_is_rejected(self):
        self.fixture["source"]["process"]["user_id"] = "must-not-be-published"
        with self.assertRaisesRegex(ValueError, "account or credential"):
            checker.check_cement(self.fixture)

    def test_source_quality_findings_cannot_disappear(self):
        self.fixture["qualityFindings"] = []
        with self.assertRaisesRegex(ValueError, "quality findings"):
            checker.check_cement(self.fixture)


if __name__ == "__main__":
    unittest.main()
