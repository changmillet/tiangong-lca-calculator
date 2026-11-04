#!/usr/bin/env python3
"""Deterministic authored method DTO schemas. No approval or execution authority."""
import argparse
import copy
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'docs/agents/contracts'
FACTS = OUT / 'study-method-assessment-facts.v1.schema.json'


def obj(fields):
    return {'type': 'object', 'additionalProperties': False, 'required': list(fields), 'properties': fields}


def text(maximum, minimum=1):
    return {'type': 'string', 'minLength': minimum, 'maxLength': maximum, 'pattern': r'^[^\u0000-\u001f\u007f-\u009f<>]+$', 'allOf': [{'pattern': r'[^\u0020\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]'}]}


def enum(*values):
    return {'enum': list(values)}


def const(value):
    return {'const': value}


def arr(item, minimum, maximum):
    return {'type': 'array', 'items': item, 'minItems': minimum, 'maxItems': maximum}


def ref(name):
    return {'$ref': '#/$defs/' + name}


def nullable(value):
    return {'anyOf': [value, {'type': 'null'}]}


def tagged(fields):
    return {'oneOf': [obj({'kind': const(kind), **body}) for kind, body in fields.items()]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    outputs = {}
    document = json.loads(FACTS.read_bytes())
    defs = document['$defs']
    uuid = {'type': 'string', 'pattern': '^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$'}
    sha = {'type': 'string', 'pattern': '^[a-f0-9]{64}$'}
    integer = lambda lo, hi: {'type': 'integer', 'minimum': lo, 'maximum': hi}
    statuses = enum('queued', 'running', 'waiting', 'completed', 'blocked', 'stale', 'failed', 'cancelled')
    kind = enum('sourceRecord', 'applicabilityRecord', 'professionalRecord')
    requirement = {'type': 'string', 'pattern': '^PEF-(?:0[1-9]|1[0-9]|20)$'}
    artifact = copy.deepcopy(defs['artifactRef'])
    artifact['properties']['type'] = const('methodAssessmentFacts')
    artifact['properties']['schemaVersion'] = const('lca.study-method-assessment-facts.v1')
    artifact['properties']['byteSize'] = integer(1, 4 * 1024 * 1024)
    artifact['properties']['mediaType'] = const('application/json')
    defs['methodFactsRef'] = artifact
    defs['publicAssessmentRecord'] = obj({'assessmentId': uuid, 'studyId': uuid, 'profileId': const('lca.study-method-pef-2021-ef31-cement-intermediate.v1'), 'config': ref('config'), 'configHash': sha, 'execution': obj({'jobId': uuid, 'status': statuses}), 'factsRef': nullable(ref('methodFactsRef'))})
    defs['jobRequest'] = obj({'schemaVersion': const('study.method_assessment.request.v1'), 'studyId': uuid, 'assessmentId': uuid})
    defs['methodBudget'] = obj({'maxSelectedFactorRecords': integer(1, 8192), 'maxFactsBytes': const(4 * 1024 * 1024)})
    defs['jobContext'] = obj({'schemaVersion': const('lca.study-method-job-context.v1'), 'input': ref('serviceInput'), 'budget': ref('methodBudget')})
    defs['methodCompletion'] = obj({'profileId': const('lca.study-method-pef-2021-ef31-cement-intermediate.v1'), 'configHash': sha, 'selectedFactorRecordCount': integer(0, 8192), 'factsRef': ref('methodFactsRef'), 'storageBucket': const('study-artifacts'), 'storageObjectPath': {'type': 'string', 'minLength': 1, 'maxLength': 512, 'pattern': '^study/[a-f0-9-]{36}/[a-f0-9-]{36}/[A-Za-z0-9_.-]+\\.json$'}})
    defs['failureRequest'] = obj({'resourceType': const('methodAssessment'), 'resourceId': uuid, 'jobId': uuid, 'leaseToken': uuid, 'errorCode': enum('source_invalid', 'boundary_undeclared', 'normalization_underflow', 'normalization_overflow', 'numeric_invalid', 'resource_limit', 'model_not_solvable', 'native_execution_failed', 'job_timeout', 'job_lease_lost', 'runtime_unavailable', 'runtime_failed'), 'idempotencyKey': {'type': 'string', 'minLength': 1, 'maxLength': 128, 'pattern': '^[A-Za-z0-9][A-Za-z0-9._:-]*$'}})
    defs['reconcileRequest'] = obj({'resourceType': const('methodAssessment'), 'resourceId': uuid, 'jobId': uuid, 'leaseToken': uuid, 'idempotencyKey': {'type': 'string', 'minLength': 1, 'maxLength': 128, 'pattern': '^[A-Za-z0-9][A-Za-z0-9._:-]*$'}, 'artifactId': nullable(uuid)})
    defs['reconcileResult'] = obj({'state': enum('committed', 'referenced', 'unreferenced', 'pending'), 'resource': nullable(obj({'type': const('methodAssessment'), 'value': ref('publicAssessmentRecord')})), 'jobStatus': statuses})
    defs['declaredCitation'] = obj({'kind': enum('normativeSource', 'datasetRevision', 'document', 'professionalRecord', 'otherDeclaredSource'), 'identity': text(256), 'version': text(128), 'contentHash': nullable(sha), 'locator': text(512)})
    defs['evidenceBody'] = obj({'schemaVersion': const('lca.study-method-evidence.v1'), 'requirementId': requirement, 'declaredKind': kind, 'title': text(256), 'statement': text(8192), 'citations': arr(ref('declaredCitation'), 0, 32)})
    evidence_ref = copy.deepcopy(defs['artifactRef'])
    evidence_ref['properties']['type'] = const('methodEvidence')
    evidence_ref['properties']['schemaVersion'] = const('lca.study-method-evidence.v1')
    evidence_ref['properties']['byteSize'] = integer(1, 65_536)
    evidence_ref['properties']['mediaType'] = const('application/json')
    defs['evidenceRecord'] = obj({'schemaVersion': const('lca.study-method-evidence-record.v1'), 'evidenceId': uuid, 'studyId': uuid, 'requirementId': requirement, 'declaredKind': kind, 'artifactRef': evidence_ref, 'recordedAt': {'type': 'string', 'format': 'date-time'}, 'status': const('recorded_unverified')})
    methods = arr(ref('datasetRef'), 16, 16)
    mr = {'methodRef': ref('datasetRef')}
    defs['methodFactContext'] = tagged({'characterisedCategory': {**mr, 'categoryRole': enum('primary', 'subcategory')}, 'normalisedCategory': mr, 'weightedCategory': mr, 'candidateSingleScore': {'primaryMethods': methods}})
    quantity = {**mr, 'referenceQuantityId': uuid}
    basis = {'moduleArtifactSha256': sha}
    point = {'pointDefinition': text(2048), 'scalingFactor': const('1')}
    defs['methodUnitIdentity'] = tagged({'characterised': {**quantity, 'unit': text(128)}, 'normalised': {**quantity, 'unit': const('1'), 'sourceNormalisationFactor': text(128), **basis}, 'weighted': {**quantity, 'unit': const('Pt'), 'sourceNormalisationFactor': text(128), 'sourceWeightFraction': text(128), **point, **basis}, 'candidateScore': {'primaryMethods': methods, 'unit': const('Pt'), **point, **basis}})
    number = {'type': 'string', 'minLength': 1, 'maxLength': 64, 'pattern': r'^-?(?:0|[1-9][0-9]*)(?:\.[0-9]*[1-9])?(?:e-?[1-9][0-9]{0,2})?$'}
    defs['methodReportFact'] = obj({'factId': {'type': 'string', 'minLength': 1, 'maxLength': 128, 'pattern': '^[A-Za-z0-9][A-Za-z0-9._:-]*$'}, 'assessmentId': uuid, 'baselineRunId': uuid, 'context': ref('methodFactContext'), 'methodRef': nullable(ref('datasetRef')), 'canonicalValue': nullable(number), 'unit': text(128), 'unitIdentity': ref('methodUnitIdentity'), 'coverage': enum('complete', 'partial', 'unknown'), 'reasonCodes': arr(text(128), 0, 32)})
    facts = defs['factsBundle']
    if 'factCatalog' not in facts['required']:
        facts['required'].insert(facts['required'].index('outcome'), 'factCatalog')
    facts['properties']['factCatalog'] = arr(ref('methodReportFact'), 58, 58)
    outputs[FACTS] = json.dumps(document, ensure_ascii=False, indent=2) + '\n'
    roots = {'study-method-assessment-record.v1': 'publicAssessmentRecord', 'study-method-job-request.v1': 'jobRequest', 'study-method-job-context.v1': 'jobContext', 'study-method-completion.v1': 'methodCompletion', 'study-method-failure-request.v1': 'failureRequest', 'study-method-reconcile-request.v1': 'reconcileRequest', 'study-method-reconcile-result.v1': 'reconcileResult', 'study-method-evidence.v1': 'evidenceBody', 'study-method-evidence-record.v1': 'evidenceRecord', 'study-method-fact-catalog-entry.v1': 'methodReportFact'}
    def dependencies(name, result):
        if name in result:
            return
        result.add(name)
        def visit(value):
            if isinstance(value, dict):
                for key, item in value.items():
                    if key == '$ref' and item.startswith('#/$defs/'):
                        dependencies(item.split('/')[-1], result)
                    else:
                        visit(item)
            elif isinstance(value, list):
                for item in value:
                    visit(item)
        visit(defs[name])
    for name, root in roots.items():
        names = set()
        dependencies(root, names)
        schema = {'$schema': document['$schema'], '$id': 'lca.' + name, '$comment': 'Worker-owned closed method interface. Schema checks never establish professional approval, current actor/source/lease authority or byte identity.', '$ref': '#/$defs/' + root, '$defs': {key: defs[key] for key in sorted(names)}}
        outputs[OUT / (name + '.schema.json')] = json.dumps(schema, ensure_ascii=False, indent=2) + '\n'
    for path, value in outputs.items():
        if args.check:
            if not path.is_file() or path.read_bytes() != value.encode():
                raise ValueError('stale_method_schema:' + path.name)
        else:
            path.write_text(value)
    print(json.dumps({'factCatalogCount': 58, 'standaloneSchemas': len(roots), 'generatedFrom': 'authored Worker method DTO definitions and retained facts definitions'}, sort_keys=True))


if __name__ == '__main__':
    main()
