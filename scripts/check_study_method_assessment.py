#!/usr/bin/env python3
"""Independent Decimal oracle for isolated source-bound arithmetic, not PEF approval."""
import argparse
import hashlib
import json
from decimal import Decimal, localcontext
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / 'crates/solver-worker/src/study/method_assets/method.pef-2021-ef31.0.1.0.json'
FIXTURE = ROOT / 'crates/solver-worker/tests/fixtures/study-method-assessment/arithmetic.json'
PIN = 'b19ba5754b0bc20c44c8c65966bafafd5984cd74636f2c805c0fa4373f5554ad'


def require(condition, code):
    if not condition:
        raise ValueError(code)


def near(actual, expected, code):
    parsed = Decimal(actual)
    require(parsed.is_finite(), code + '_finite')
    # This independently computed decimal target permits binary64 rounding only.
    require(abs(parsed - expected) <= max(abs(expected), Decimal('1e-300')) * Decimal('4e-15'), code)


def verify(value):
    raw = PACKAGE.read_bytes()
    require(hashlib.sha256(raw).hexdigest() == PIN, 'module_artifact_pin')
    binding = json.loads(raw)['payload']['source_bindings']
    methods = binding['methods']
    source = binding['normalisation_weighting']
    primary = {m['id']: m for m in methods if m['role'] == 'primary'}
    require(len(primary) == len(source) == 16 and len(methods) == 25, 'source_category_roles')
    require(value['classification'] == 'isolated_arithmetic_not_a_study_or_conformance_case', 'fixture_scope')
    require(value['moduleArtifactSha256'] == PIN, 'fixture_module_pin')
    inputs = value['inputCategories']
    require(len(inputs) == 25 and len({v['methodRef']['id'] for v in inputs}) == 25, 'input_membership')
    by_id = {v['methodRef']['id']: v for v in inputs}
    result = value['normalisationWeighting']
    require(result['status'] == 'candidate_arithmetic_available' and len(result['categories']) == 16, 'nw_available')
    require(result['scalingFactor'] == '1' and result['scoreUnit'] == 'Pt', 'point_scale')
    require(result['weightRepresentation'] == 'source_fraction_no_additional_division_by_100', 'weight_scale')
    with localcontext() as ctx:
        ctx.prec = 80
        total = Decimal(0)
        fractions = Decimal(0)
        for i, n in enumerate(source):
            category = by_id[n['method_id']]
            record = result['categories'][i]
            require(n['method_id'] in primary, 'primary_only')
            require(record['methodRef'] == category['methodRef'] == {'id': n['method_id'], 'version': n['method_version']}, 'source_ordered_method_mapping')
            require(category['role'] == 'primary' and category['coverage'] == 'complete' and category['selected'] and category['sourceCompatible'], 'numeric_eligibility')
            for k, expected in [('referenceQuantityId', n['reference_quantity_id']), ('characterisedUnit', n['unit_label']), ('sourceNormalisationFactor', n['normalisation_factor']), ('sourceWeightFraction', n['weight_fraction']), ('sourceUnitLabel', n['source_unit_label']), ('sourceShortDescription', n['source_short_description'])]:
                require(record[k] == expected, 'source_field_' + k)
            require(record['normalisedUnit'] == '1' and record['weightedUnit'] == 'Pt', 'category_units')
            h = Decimal(category['canonicalCharacterisedValue'])
            nf, fraction = Decimal(n['normalisation_factor']), Decimal(n['weight_fraction'])
            require(nf > 0 and 0 < fraction <= 1, 'source_positive_nf_fraction')
            normalized = h / nf
            weighted = normalized * fraction
            require(record['canonicalCharacterisedValue'] == category['canonicalCharacterisedValue'], 'characterised_identity')
            near(record['canonicalNormalisedValue'], normalized, 'normalised_decimal')
            near(record['canonicalWeightedValue'], weighted, 'weighted_decimal')
            total += weighted
            fractions += fraction
        require(fractions == 1, 'source_fraction_sum')
        near(result['canonicalCandidateSingleScore'], total, 'primary_only_total_decimal')
    return {'classification': value['classification'], 'sourceArtifactSha256': PIN, 'primaryCategories': 16, 'excludedDecompositions': 9, 'decimalPrecision': 80, 'candidateScore': result['canonicalCandidateSingleScore'], 'pefConformityQualified': False, 'realStudyQualified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native-results', type=Path, default=FIXTURE)
    args = parser.parse_args()
    print(json.dumps(verify(json.loads(args.native_results.read_bytes())), sort_keys=True))


if __name__ == '__main__':
    main()
