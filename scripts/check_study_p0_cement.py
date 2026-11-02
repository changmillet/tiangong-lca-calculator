#!/usr/bin/env python3
"""Verify a minimal real inventory projection and its bounded direct subtotals.

Optional source checks read an explicitly supplied private acquisition packet and
public static assets. They never print or copy raw payloads/account metadata.
No API, database, credentials, product solver or provider inference is used.
"""

from __future__ import annotations

import argparse
from collections import Counter
from fractions import Fraction
import gzip
import hashlib
import json
from pathlib import Path
import re
import struct
import xml.etree.ElementTree as ET

from check_study_p0_benchmarks import FIXTURES, require


REPO = Path(__file__).resolve().parents[1]
FIXTURE = FIXTURES / "real-cement-gate-to-gate.json"
DEFAULTS = REPO / "crates/solver-worker/src/lcia_nonregional_defaults.json"
METHODS = {
    "6209b35f-9447-40b5-b68c-a1099e3674a0": "01.00.000",
    "b5c611c6-def3-11e6-bf01-fe55135034f3": "01.04.000",
}
NS = {"m": "http://lca.jrc.it/ILCD/LCIAMethod"}


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical(document: dict) -> bytes:
    return json.dumps(document, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def many(value):
    return value if isinstance(value, list) else [value]


def identity(value: dict) -> tuple:
    return value["id"], value["version"]


def raw_ref(value: dict) -> dict:
    return {"id": value["@refObjectId"], "version": value["@version"]}


def no_account_fields(value) -> None:
    if isinstance(value, dict):
        forbidden = {"user_id", "requested_user_id", "access_token", "refresh_token", "authorization", "session", "email", "identity_receipt"}
        require(not forbidden.intersection(value), "account or credential metadata must not enter the projection")
        for item in value.values():
            no_account_fields(item)
    elif isinstance(value, list):
        for item in value:
            no_account_fields(item)


def check_cement(fixture: dict) -> None:
    no_account_fields(fixture)
    require(fixture["schema"] == "lca.study-p0-real-inventory.v1", "real fixture schema")
    require(fixture["status"] == "bounded_numerical_example_not_business_qualified", "bounded example status")
    require(fixture["runtimeEnabled"] is False, "example cannot enable runtime")
    require(fixture["scope"]["fullLcaQualified"] is False and fixture["scope"]["scientificReviewComplete"] is False, "no business/scientific qualification")
    process = fixture["source"]["process"]
    require(process["id"] == "dcffcc2b-d964-46d2-94ed-0c7b3c5b0c75", "source Process identity")
    require(process["requestedVersion"] == process["resolvedVersion"] == "01.01.000", "exact source version; no fallback")
    require(fixture["source"]["datasetCopyrightDeclaration"] is False, "retain dataset declaration")
    require(fixture["source"]["datasetLicenseDeclaration"] == "Free of charge for all users and uses", "retain dataset license declaration")
    flows = {identity(row): row for row in fixture["flows"]}
    properties = {identity(row): row for row in fixture["numericSupport"]["flowProperties"]}
    units = {identity(row): row for row in fixture["numericSupport"]["unitGroups"]}
    require(len(flows) == len(fixture["flows"]) == 9 and len(properties) == len(units) == 2, "exact numeric support membership")
    require(fixture["numericSupport"]["fullSourceReferenceClosureVerified"] is False, "numeric closure is not full administrative/source closure")
    for row in [process, *flows.values(), *properties.values(), *units.values()]:
        require(row["stateCode"] == 100, "public numerical source state")
        require(re.fullmatch(r"[0-9a-f]{64}", row["documentSha256"]) is not None, "source document digest")
    for flow in flows.values():
        prop = properties[identity(flow["referenceFlowProperty"])]
        require(prop["referenceUnitGroup"] == flow["referenceUnitGroup"], "property/unit-group reference mismatch")
        unit = units[identity(prop["referenceUnitGroup"])]
        require(flow["referenceUnit"] == unit["referenceUnit"], "reference unit identity")
        require(Fraction(flow["referencePropertyFactor"]) == Fraction(unit["referenceUnit"]["factor"]) == 1, "no implicit unit conversion in this example")
    exchanges = {row["id"]: row for row in fixture["exchanges"]}
    require(set(exchanges) == {str(index) for index in range(9)}, "nine source exchanges required")
    reference = exchanges[fixture["scope"]["referenceExchangeId"]]
    require(reference["id"] == "8" and Fraction(reference["amount"]) == Fraction(fixture["scope"]["referenceProductAmount"]) == 1000, "declared reference remains 1000 kg")
    require(reference["unit"] == fixture["scope"]["referenceUnit"] == "kg", "reference mass unit")
    emissions = {}
    for key, row in exchanges.items():
        flow = flows[identity(row["flow"])]
        require(row["unit"] == flow["referenceUnit"]["name"], "exchange/reference unit mismatch")
        if key in {"0", "1", "2", "3"}:
            require(row["direction"] == "Input" and row["boundaryRole"] == "upstreamInputExcluded" and flow["type"] == "Product flow", "upstream product inputs remain outside boundary")
        elif key in {"4", "5", "6", "7"}:
            require(row["direction"] == "Output" and row["boundaryRole"] == "directEmissionIncluded" and flow["type"] == "Elementary flow", "direct emission boundary")
            emissions[key] = row
        else:
            require(row["direction"] == "Output" and row["boundaryRole"] == "referenceProduct", "reference product")
    require(exchanges["3"]["unit"] == "MJ", "electricity reference is MJ, not guessed kWh")
    defaults_bytes = DEFAULTS.read_bytes()
    require(fixture["methodEvidence"]["nonregionalDefaultsSha256"] == sha(defaults_bytes), "prepared defaults drift")
    defaults = json.loads(defaults_bytes)
    for key, source_key in (("staticBundleManifestSha256", "static_bundle_manifest_sha256"), ("staticFactorManifestSha256", "static_factor_manifest_sha256")):
        require(fixture["methodEvidence"][key] == defaults[source_key], "method source binding")
    require({row["id"]: row["version"] for row in fixture["methods"]} == METHODS, "exact selected methods")
    for method in fixture["methods"]:
        prepared = next(row for row in defaults["methods"] if row["method_id"] == method["id"])
        require(method["sourceXmlSha256"] == prepared["source_xml_sha256"], "method XML identity")
        require(method["coverageStatus"] == "incomplete_coverage_not_zero", "unknown factors cannot be reported as zeros")
        selected = method["selectedFactors"]
        known = {factor["exchangeId"] for factor in selected}
        require(len(known) == len(selected) and known < set(emissions), "bounded partial characterization")
        require(set(method["uncharacterizedExchangeIds"]) == set(emissions) - known, "explicit unknown coverage")
        subtotal = Fraction()
        for factor in selected:
            exchange = emissions[factor["exchangeId"]]
            require(factor["flow"] == exchange["flow"] and factor["direction"] == exchange["direction"], "exact factor flow version and direction")
            require(factor["sourceLocationKind"] in ("absent", "blank"), "one source nonregional default")
            if factor["matchingAlternativeCount"] > 1:
                alternative = next(row for row in prepared["alternatives"] if row["flow_id"] == factor["flow"]["id"] and row["flow_version"] == factor["flow"]["version"] and row["direction"] == factor["direction"].upper())
                require(alternative["value_bits"] == struct.pack(">d", float(Fraction(factor["value"]))).hex(), "regional alternatives must not be summed")
                require(alternative["source_record_sha256"] == factor["sourceRecordSha256"] and alternative["source_record_index"] == factor["sourceRecordIndex"] and alternative["alternative_count"] == factor["matchingAlternativeCount"], "default source-record binding")
            contribution = Fraction(exchange["amount"]) * Fraction(factor["value"])
            require(contribution == Fraction(factor["expectedContribution"]), "independent contribution equation")
            subtotal += contribution
        require(subtotal == Fraction(method["expectedCharacterizedSubtotal"]), "independent subtotal equation")
    findings = {finding["code"] for finding in fixture["qualityFindings"]}
    require({"REFERENCE_COMMENT_AMOUNT_CONFLICT", "PUBLIC_STATE_AND_SOURCE_WORKFLOW_DIFFER", "PARTICLE_PROXY_AND_DERIVATION_STATUS", "NUMERIC_REFERENCE_UNITS_ONLY", "METHOD_COVERAGE_INCOMPLETE"} <= findings, "source quality findings must remain explicit")


def verify_source_packet(fixture: dict, packet: Path) -> None:
    wrapper = json.loads((packet / "cement-process-exact.json").read_bytes())
    source = fixture["source"]["process"]
    require(wrapper["process_id"] == source["id"] and wrapper["requested_version"] == source["requestedVersion"] and wrapper["resolved_version"] == source["resolvedVersion"] and wrapper["state_code"] == 100, "exact public Process read")
    require(sha(canonical(wrapper["process"])) == source["documentSha256"], "Process source payload hash")
    process = wrapper["process"]["processDataSet"]
    require(str(process["processInformation"]["quantitativeReference"]["referenceToReferenceFlow"]) == fixture["scope"]["referenceExchangeId"], "source quantitative reference")
    require(process["processInformation"]["time"]["common:referenceYear"] == fixture["source"]["referenceYear"] and process["processInformation"]["geography"]["locationOfOperationSupplyOrProduction"]["@location"] == fixture["source"]["geography"], "source time/geography projection")
    citation = process["modellingAndValidation"]["dataSourcesTreatmentAndRepresentativeness"]["referenceToDataSource"]
    require(raw_ref(citation) == fixture["source"]["citationReference"], "source citation reference projection")
    publication = process["administrativeInformation"]["publicationAndOwnership"]
    require(publication["common:licenseType"] == fixture["source"]["datasetLicenseDeclaration"] and publication["common:copyright"] == "false" and publication["common:workflowAndPublicationStatus"] == fixture["source"]["sourceWorkflowLabel"], "source declarations")
    original_exchanges = {row["@dataSetInternalID"]: row for row in process["exchanges"]["exchange"]}
    require(set(original_exchanges) == {row["id"] for row in fixture["exchanges"]}, "source exchange membership")
    for row in fixture["exchanges"]:
        original = original_exchanges[row["id"]]
        require(original["resultingAmount"] == row["amount"] and original["exchangeDirection"] == row["direction"] and raw_ref(original["referenceToFlowDataSet"]) == row["flow"] and original.get("dataDerivationTypeStatus") == row["sourceDerivationStatus"], "source numeric exchange projection")
    for flow in fixture["flows"]:
        raw = json.loads((packet / f"flow-{flow['id']}-{flow['version']}.json").read_bytes())
        require(raw["flow_id"] == flow["id"] and raw["requested_version"] == raw["resolved_version"] == flow["version"] and raw["state_code"] == 100, "exact public Flow read")
        require(sha(canonical(raw["flow"])) == flow["documentSha256"], "Flow source payload hash")
        document = raw["flow"]["flowDataSet"]
        require(document["modellingAndValidation"]["LCIMethod"]["typeOfDataSet"] == flow["type"], "source Flow kind projection")
        reference = str(document["flowInformation"]["quantitativeReference"]["referenceToReferenceFlowProperty"])
        prop = next(row for row in many(document["flowProperties"]["flowProperty"]) if str(row["@dataSetInternalID"]) == reference)
        require(raw_ref(prop["referenceToFlowPropertyDataSet"]) == flow["referenceFlowProperty"] and prop["meanValue"] == flow["referencePropertyFactor"], "Flow numeric reference projection")
    for category, projected_key in (("flowproperties", "flowProperties"), ("unitgroups", "unitGroups")):
        rows = [json.loads(line) for line in (packet / "support-public" / f"{category}.jsonl").read_text().splitlines()]
        for projected in fixture["numericSupport"][projected_key]:
            matches = [row for row in rows if (row["id"], row["version"]) == identity(projected)]
            require(len(matches) == 1 and matches[0]["state_code"] == 100, "exact public numeric support read")
            require(sha(canonical(matches[0]["json"])) == projected["documentSha256"], "support source payload hash")
            if category == "flowproperties":
                reference = matches[0]["json"]["flowPropertyDataSet"]["flowPropertiesInformation"]["quantitativeReference"]["referenceToReferenceUnitGroup"]
                require(raw_ref(reference) == projected["referenceUnitGroup"], "source property/unit-group projection")
            else:
                document = matches[0]["json"]["unitGroupDataSet"]
                reference = str(document["unitGroupInformation"]["quantitativeReference"]["referenceToReferenceUnit"])
                unit = next(row for row in many(document["units"]["unit"]) if str(row["@dataSetInternalID"]) == reference)
                require({"id": reference, "name": unit["name"], "factor": unit["meanValue"]} == projected["referenceUnit"], "source reference-unit projection")


def verify_static_sources(fixture: dict, cache: Path, data: Path) -> None:
    raw_manifest = (cache / "cache_manifest.json").read_bytes()
    require(sha(raw_manifest) == fixture["methodEvidence"]["staticBundleManifestSha256"], "pinned static manifest")
    manifest = json.loads(raw_manifest)
    raw_list = (cache / manifest["files"]["list"]["path"]).read_bytes()
    raw_factors = (cache / manifest["files"]["factors"]["path"]).read_bytes()
    require(sha(raw_list) == manifest["files"]["list"]["sha256"] == fixture["methodEvidence"]["staticListSha256"], "static method list hash")
    require(sha(raw_factors) == manifest["files"]["factors"]["sha256"] == fixture["methodEvidence"]["staticFactorAssetSha256"], "static factor asset hash")
    expanded = gzip.decompress(raw_factors)
    require(sha(expanded) == manifest["files"]["factors"]["decompressed_sha256"], "decompressed factor hash")
    cached = json.loads(expanded)
    listing = json.loads(raw_list)
    for method in fixture["methods"]:
        raw = (data / method["sourcePath"]).read_bytes()
        require(sha(raw) == method["sourceXmlSha256"], "original method XML hash")
        metadata = next(row for row in listing["files"] if row["id"] == method["id"])
        require(metadata["version"] == method["version"] and metadata["referenceQuantity"]["common:shortDescription"]["#text"] == method["unit"], "exact method version/result unit")
        records = ET.fromstring(raw).findall("m:characterisationFactors/m:factor", NS)
        raw_records = re.findall(rb"<factor(?:\s[^>]*)?>.*?</factor\s*>", raw, re.DOTALL)
        for exchange in (row for row in fixture["exchanges"] if row["boundaryRole"] == "directEmissionIncluded"):
            matches = []
            for index, record in enumerate(records):
                reference = record.find("m:referenceToFlowDataSet", NS)
                if raw_ref({"@refObjectId": reference.attrib["refObjectId"], "@version": reference.attrib["version"]}) == exchange["flow"] and record.findtext("m:exchangeDirection", namespaces=NS).upper() == exchange["direction"].upper():
                    matches.append((index, record))
            selected = [row for row in method["selectedFactors"] if row["exchangeId"] == exchange["id"]]
            if not matches:
                require(not selected and exchange["id"] in method["uncharacterizedExchangeIds"], "missing source factor remains unknown")
                cache_row = cached.get(exchange["flow"]["id"] + ":" + exchange["direction"].upper())
                require(cache_row is None or not any(row["key"] == method["id"] for row in many(cache_row["factor"])), "cache/source missing-factor disagreement")
                continue
            choices = [(index, row) for index, row in matches if not (row.findtext("m:location", namespaces=NS) or "").strip()]
            require(len(choices) == len(selected) == 1, "unique nonregional source default")
            index, record = choices[0]
            require(index == selected[0]["sourceRecordIndex"] and sha(raw_records[index]) == selected[0]["sourceRecordSha256"], "selected source record hash")
            require(Fraction(record.findtext("m:meanValue", namespaces=NS)) == Fraction(selected[0]["value"]) and len(matches) == selected[0]["matchingAlternativeCount"], "selected source default value/count")
            numeric = Counter(Fraction(row.findtext("m:meanValue", namespaces=NS)) for _, row in matches)
            cached_row = cached[exchange["flow"]["id"] + ":" + exchange["direction"].upper()]
            require(cached_row["@version"] == exchange["flow"]["version"], "cache exact Flow version")
            cache_numeric = Counter(Fraction(str(row["value"])) for row in many(cached_row["factor"]) if row["key"] == method["id"])
            require(numeric == cache_numeric, "source/cache alternatives multiset")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, default=FIXTURE)
    parser.add_argument("--source-packet", type=Path)
    parser.add_argument("--static-cache", type=Path)
    parser.add_argument("--data-root", type=Path)
    args = parser.parse_args()
    try:
        fixture = json.loads(args.fixture.read_bytes())
        check_cement(fixture)
        if args.source_packet:
            verify_source_packet(fixture, args.source_packet)
        require(bool(args.static_cache) == bool(args.data_root), "static-cache and data-root must be supplied together")
        if args.static_cache:
            verify_static_sources(fixture, args.static_cache, args.data_root)
    except (OSError, ValueError, KeyError, TypeError, ZeroDivisionError, StopIteration, ET.ParseError) as error:
        parser.exit(1, f"study-p0-cement: FAIL: {type(error).__name__}: {error}\n")
    print(json.dumps({"status": "passed", "scope": "bounded-real-inventory-and-characterized-subtotals", "processVersion": "01.01.000", "flows": 9, "numericSupportDocuments": 4, "sourcePacketVerified": args.source_packet is not None, "staticSourcesVerified": args.static_cache is not None, "businessPilotQualified": False, "completeLciaScores": False}, sort_keys=True))


if __name__ == "__main__":
    main()
