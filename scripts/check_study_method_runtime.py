#!/usr/bin/env python3
"""Independent identity/catalog checks over synthetic transport, never live authority."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / 'crates/solver-worker/tests/fixtures/study-method-assessment/service-seams.json'
PACKAGE = ROOT / 'crates/solver-worker/src/study/method_assets/method.pef-2021-ef31.0.1.0.json'


def require(condition, code):
    if not condition:
        raise ValueError(code)


def verify(value):
    require(value['classification'] == 'synthetic_transport_not_authorization_or_scientific_qualification', 'fixture_scope')
    context, record, completion, facts = [value[k] for k in ['inputContext', 'record', 'completion', 'facts']]
    input_record = context['input']['assessment']
    require(context['schemaVersion'] == 'lca.study-method-job-context.v1' and value['jobKind'] == 'study.method_assessment', 'method_family')
    require(value['payload'] == {'schemaVersion': 'study.method_assessment.request.v1', 'studyId': record['studyId'], 'assessmentId': record['assessmentId']}, 'payload_identity')
    require(record['assessmentId'] == input_record['assessmentId'] == facts['assessmentId'] and record['studyId'] == input_record['studyId'] == facts['studyId'], 'record_identity')
    require(record['config'] == input_record['config'] == facts['config'] and record['configHash'] == input_record['configHash'] == facts['configHash'] == completion['configHash'], 'config_identity')
    require(record['execution']['jobId'] == input_record['execution']['jobId'] and record['factsRef'] == completion['factsRef'], 'job_artifact_identity')
    raw = (FIXTURE.parent / value['factsFile']).read_bytes()
    require(json.loads(raw) == facts and hashlib.sha256(raw).hexdigest() == completion['factsRef']['contentHash'] and len(raw) == completion['factsRef']['byteSize'], 'exact_facts_bytes')
    require(completion['factsRef']['type'] == 'methodAssessmentFacts' and completion['factsRef']['schemaVersion'] == 'lca.study-method-assessment-facts.v1', 'facts_family')
    require(completion['selectedFactorRecordCount'] == len(facts['methodSourceEvidence']['selectedFactors']) <= context['budget']['maxSelectedFactorRecords'] <= 8192, 'admitted_factor_budget')
    require(context['budget']['maxFactsBytes'] == 4194304 and not facts['pefConformityQualified'] and not facts['scientificRankingQualified'] and not facts['businessPilotQualified'], 'qualification_not_inferred')
    require(len(facts['requirements']) == 20 and {r['requirementId'] for r in facts['requirements']} == {f'PEF-{i:02}' for i in range(1, 21)}, 'all_requirements')
    package = json.loads(PACKAGE.read_bytes())['payload']['source_bindings']
    categories = {c['methodRef']['id']: c for c in facts['categories']}
    require(len(categories) == 25, 'all_categories')
    catalog = facts['factCatalog']
    require(len(catalog) == len({f['factId'] for f in catalog}) == 58, 'catalog_inventory')
    by_id = {f['factId']: f for f in catalog}
    for category in categories.values():
        fact = by_id['category:' + category['methodRef']['id'] + ':characterised']
        require(fact['assessmentId'] == record['assessmentId'] and fact['baselineRunId'] == facts['baseline']['runId'], 'fact_identity')
        require(fact['canonicalValue'] == category['canonicalCharacterisedValue'] and fact['methodRef'] == category['methodRef'] and fact['unit'] == category['unit'], 'characterised_value_unit')
        require(fact['context'] == {'kind': 'characterisedCategory', 'methodRef': category['methodRef'], 'categoryRole': category['role']}, 'category_role')
        require(fact['coverage'] == ('unknown' if category['coverage'] == 'not_selected' else category['coverage']) and fact['reasonCodes'] == category['reasonCodes'], 'category_coverage_reasons')
    nw = facts['candidateNormalisationWeighting']
    available = nw['status'] == 'candidate_arithmetic_available'
    available_values = {r['methodRef']['id']: r for r in nw['categories']}
    require(len(available_values) == (16 if available else 0), 'normalisation_eligibility')
    for source in package['normalisation_weighting']:
        method = {'id': source['method_id'], 'version': source['method_version']}
        for name, unit, field in [('normalised', '1', 'canonicalNormalisedValue'), ('weighted', 'Pt', 'canonicalWeightedValue')]:
            fact = by_id[name + ':' + method['id']]
            require(fact['methodRef'] == method and fact['unit'] == unit and fact['unitIdentity']['moduleArtifactSha256'] == facts['moduleRef']['artifactSha256'], 'nw_source_identity')
            require(fact['unitIdentity']['sourceNormalisationFactor'] == source['normalisation_factor'], 'nf_not_replaced')
            if name == 'weighted':
                require(fact['unitIdentity']['sourceWeightFraction'] == source['weight_fraction'] and fact['unitIdentity']['scalingFactor'] == '1', 'weight_once_and_points')
            require(fact['canonicalValue'] == (available_values[method['id']][field] if available else None), 'nw_catalog_value')
            require(fact['coverage'] == ('complete' if available else 'unknown') and fact['reasonCodes'] == nw['reasonCodes'], 'nw_unavailable_not_zero')
    score = by_id['candidate:singleScore']
    require(score['canonicalValue'] == nw['canonicalCandidateSingleScore'] and score['methodRef'] is None and score['unit'] == 'Pt', 'score_identity')
    require([m['id'] for m in score['unitIdentity']['primaryMethods']] == [r['method_id'] for r in package['normalisation_weighting']], 'primary_only_source_order')
    return {'classification': value['classification'], 'requirements': 20, 'categoryFacts': 25, 'normalisedFacts': 16, 'weightedFacts': 16, 'scoreFacts': 1, 'qualified': False}


if __name__ == '__main__':
    print(json.dumps(verify(json.loads(FIXTURE.read_bytes())), sort_keys=True))
