#!/usr/bin/env python3
"""Author an additive LCIA applicability companion from fixed, read-only sources.

This is source inspection, not a calculation or an original reference-cache writer.
An existing output directory is refused. The old release files are read and hashed,
never rewritten. Policy designations are explicit reviewable output, and require the
same scientific candidate acceptance as the runtime which consumes them.
"""

from __future__ import annotations

import argparse
from collections import defaultdict, deque
from decimal import Decimal
import gzip
import hashlib
import json
import math
from pathlib import Path
import re
import struct
import subprocess
import xml.etree.ElementTree as ET

SOURCE_COMMIT = "367642d7994e0e75188e7e241fb2a4b85ffff62f"
BUNDLE_SHA256 = "e9b4e7f9a5125bb921efbffba9a4b50711f9ea982e22b500f35211884a0479c5"
FACTOR_SHA256 = "40ffd33323c9882dbd0b0d9c99982bad1752e311062231bcf1f490ee96f92e96"
POLICY_VERSION = "lcia.applicability.source-bound.v3"
SCHEMA_VERSION = "lcia.supported-source-policy.v2"
METHOD_NS = "http://lca.jrc.it/ILCD/LCIAMethod"
COMMON_NS = "http://lca.jrc.it/ILCD/Common"
NS = {"m": METHOD_NS, "c": COMMON_NS}
ILCD_DOC = "https://eplca.pages.code.europa.eu/ilcd_docs/impactmethod/CharacterisationFactorType.html"
JRC_REPORT = "https://eplca.jrc.ec.europa.eu/permalink/TR_SupportingCF_FINAL.pdf"

GEOGRAPHY_DOC = "https://eplca.pages.code.europa.eu/ilcd_docs/impactmethod/GeographyType.html"
# This review recipe enumerates exact retained source identities. Runtime readers
# consume the pinned declarations, never infer geographical scope from names,
# absent locations, numerical uniqueness, or region/country containment.
GLOBAL_GENERIC_SOURCE_IDS = {
    "01500b74-7ffb-463e-9bd4-72f17c2263ff", "05316e7a-b254-4bea-9cf0-6bf33eb5c630",
    "14af9ca7-aa1d-4832-b1d9-ab05a06dcb12", "2299222a-bbd8-474f-9d4f-4dd1f18aea7c",
    "503699e0-eca9-4089-8bf8-e0f49c93e578", "6209b35f-9447-40b5-b68c-a1099e3674a0",
    "706261af-a357-4cc0-a50a-f3033fcbd556", "7cfdcfcf-b222-4b26-888a-a55f9fbf7ac8",
    "7fce5b3a-66b8-4ce1-91e8-a925aee1f186", "8c3141e9-1f15-43b5-bff2-182e49893a46",
    "9d1d43a2-e1aa-4c16-acd4-3dd8a6a2fb85", "b2ad6110-c78d-11e6-9d9d-cec0c932ce01",
    "b2ad6494-c78d-11e6-9d9d-cec0c932ce01", "b2ad66ce-c78d-11e6-9d9d-cec0c932ce01",
    "b2ad6890-c78d-11e6-9d9d-cec0c932ce01", "b5c602c6-def3-11e6-bf01-fe55135034f3",
    "b5c629d6-def3-11e6-bf01-fe55135034f3", "b5c632be-def3-11e6-bf01-fe55135034f3",
    "dacd48b5-4da5-49aa-aff4-cd5f5495c037", "fd530f00-9325-424a-92ef-aaac67922fd9",
}
RESTRICTED_GENERIC_SOURCE_TOKENS = {
    "b53ec18f-7377-4ad3-86eb-cc3f4f276b2b": "RER",
    "b5c610fe-def3-11e6-bf01-fe55135034f3": "RER",
    "b5c611c6-def3-11e6-bf01-fe55135034f3": "RER",
    "b5c614d2-def3-11e6-bf01-fe55135034f3": "RER",
    "b5c619fa-def3-11e6-bf01-fe55135034f3": "RER",
}


def generic_geographic_admission(method_id: str, metadata: dict[str, object]) -> dict[str, object]:
    geography = metadata["information"]["geography"]
    geographic_hash = sha256(canonical(geography))
    intervention = geography["interventionLocation"]
    if method_id in GLOBAL_GENERIC_SOURCE_IDS:
        if intervention != "GLO":
            raise ValueError("reviewed global source intervention changed")
        return {"kind": "global", "source_geography_sha256": geographic_hash}
    token = RESTRICTED_GENERIC_SOURCE_TOKENS.get(method_id)
    if token is None or intervention != token:
        raise ValueError("source has no reviewed exact geographic admission")
    return {"kind": "exact_source_tokens", "tokens": [token], "source_geography_sha256": geographic_hash}

# These entries describe the exact retained sources, rather than a rule that would
# authorize arbitrary missing locations. Named regional alternatives remain first.
REGIONAL_DEFAULT_BASIS = {
    "b5c611c6-def3-11e6-bf01-fe55135034f3": {
        "id": "jrc-supporting-cf-2018-section-4.7",
        "uri": JRC_REPORT,
        "locator": "section 4.7, Table 9, PDF page 23",
        "scope": "European generic factors alongside country factors; no global-coverage claim",
    },
    "b5c614d2-def3-11e6-bf01-fe55135034f3": {
        "id": "jrc-supporting-cf-2018-section-4.8",
        "uri": JRC_REPORT,
        "locator": "section 4.8, PDF page 24",
        "scope": "Site-independent nitrogen-compound factors alongside country factors; European model scope retained",
    },
    "b5c619fa-def3-11e6-bf01-fe55135034f3": {
        "id": "native-marine-averaged-factor-model",
        "uri": "https://github.com/tiangong-lca/data",
        "locator": "LCIAMethodInformation/dataSetInformation/common:generalComment and modellingAndValidation/useAdviceForDataSet",
        "scope": "European averaged characterization factors from country-dependent assessments",
    },
    "b5c610fe-def3-11e6-bf01-fe55135034f3": {
        "id": "native-lotos-euros-european-average",
        "uri": "https://github.com/tiangong-lca/data",
        "locator": "LCIAMethodInformation/impactModel/modelDescription and modellingAndValidation/useAdviceForDataSet",
        "scope": "LOTOS-EUROS model averages over 14000 grid cells to define European factors",
    },
}


def canonical(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()


def sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def read_json(path: Path) -> object:
    return json.loads(path.read_bytes())


def split_name(name: str) -> tuple[str, str]:
    if name.startswith("{"):
        return tuple(name[1:].split("}", 1))  # type: ignore[return-value]
    return "", name


def adapter_key(name: str) -> str:
    namespace, local = split_name(name)
    if namespace == COMMON_NS:
        return f"common:{local}"
    if namespace in ("", METHOD_NS):
        return local
    return f"{{{namespace}}}{local}"


def adapter(element: ET.Element) -> object:
    fields: dict[str, object] = {}
    for name, value in element.attrib.items():
        namespace, local = split_name(name)
        if namespace == "http://www.w3.org/XML/1998/namespace":
            key = f"@xml:{local}"
        elif namespace == "http://www.w3.org/2001/XMLSchema-instance":
            key = f"@xsi:{local}"
        elif namespace:
            raise ValueError(f"unclassified source attribute namespace {namespace}")
        else:
            key = f"@{local}"
        fields[key] = value
    grouped: dict[str, list[object]] = defaultdict(list)
    for child in element:
        grouped[adapter_key(child.tag)].append(adapter(child))
    if not fields and not grouped:
        return element.text or ""
    for key, values in grouped.items():
        fields[key] = values[0] if len(values) == 1 else values
    if not grouped and element.text:
        fields["#text"] = element.text
    return fields


def parse_source(raw: bytes) -> ET.Element:
    # The native reader preserves CR text. XML standard parsers normalize literal
    # CRLF, so retain CR with an equivalent numeric reference in text segments for
    # this adapter only. Original raw source and factor byte hashes use raw bytes.
    if b"<![CDATA[" in raw or b"<!DOCTYPE" in raw or b"<!ENTITY" in raw:
        raise ValueError("source authoring requires reviewed simple ILCD XML")
    chunks = re.split(rb"(<[^>]*>)", raw)
    retained = []
    inside = False
    for index, chunk in enumerate(chunks):
        if index % 2:
            if re.match(rb"<LCIAMethodDataSet(?:\s|>)", chunk):
                inside = True
            elif chunk.startswith(b"</LCIAMethodDataSet"):
                inside = False
            retained.append(chunk)
        else:
            retained.append(chunk.replace(b"\r", b"&#13;") if inside else chunk)
    return ET.fromstring(b"".join(retained))


def numeric_line(flow: str, version: str, direction: str, value: str) -> bytes:
    parsed = float(value)
    if not math.isfinite(parsed):
        raise ValueError("nonfinite source factor")
    bits = struct.unpack(">Q", struct.pack(">d", parsed))[0]
    return f"{flow}|{version}|{direction.upper()}|{bits:016x}\n".encode()


def dictionary_id(items: list[object], positions: dict[bytes, int], value: object) -> int:
    key = canonical(value)
    if key not in positions:
        positions[key] = len(items)
        items.append(value)
    return positions[key]


def source_method(source_root: Path, entry: dict[str, object], index: dict[tuple[object, ...], deque[tuple[str, int]]]) -> tuple[dict[str, object], bytes]:
    method_id = str(entry["method_id"])
    locator = str(entry["artifact_locator_id"])
    relative = f"tiangong_lca_data/lciamethods/{locator}.xml"
    raw = (source_root / relative).read_bytes()
    committed = subprocess.check_output(["git", "-C", str(source_root), "show", f"{SOURCE_COMMIT}:{relative}"])
    if raw != committed:
        raise ValueError(f"source differs from fixed commit: {relative}")
    root = parse_source(raw)
    if root.findtext("m:LCIAMethodInformation/m:dataSetInformation/c:UUID", namespaces=NS) != method_id:
        raise ValueError(f"source scientific identity differs: {relative}")
    if root.findtext("m:administrativeInformation/m:publicationAndOwnership/c:dataSetVersion", namespaces=NS) != entry["method_version"]:
        raise ValueError(f"source method version differs: {relative}")
    factors = root.findall("m:characterisationFactors/m:factor", NS)
    raw_factors = re.findall(rb"<factor(?:\s[^>]*)?>.*?</factor\s*>", raw, re.DOTALL)
    if len(factors) != len(raw_factors) or len(factors) != entry["factor_entry_count"]:
        raise ValueError(f"source factor count differs: {relative}")
    document = {"LCIAMethodDataSet": adapter(root)}
    document["LCIAMethodDataSet"]["characterisationFactors"]["factor"] = [adapter(factor) for factor in factors]  # type: ignore[index]
    metadata = {
        "information": adapter(root.find("m:LCIAMethodInformation", NS)),  # type: ignore[arg-type]
        "modelling_and_validation": adapter(root.find("m:modellingAndValidation", NS)),  # type: ignore[arg-type]
    }
    locations: list[object] = []
    qualifiers: list[object] = []
    location_positions: dict[bytes, int] = {}
    qualifier_positions: dict[bytes, int] = {}
    bindings = []
    families: dict[tuple[str, str, str], list[tuple[int, bool]]] = defaultdict(list)
    numeric = hashlib.sha256(b"lcia.source-numeric-binding.v1\n")
    reviewed_comments = []
    for native_index, (factor, raw_factor) in enumerate(zip(factors, raw_factors, strict=True)):
        reference = factor.find("m:referenceToFlowDataSet", NS)
        assert reference is not None
        flow = reference.attrib["refObjectId"]
        version = reference.attrib["version"]
        direction = factor.findtext("m:exchangeDirection", namespaces=NS)
        value = factor.findtext("m:meanValue", namespaces=NS)
        assert direction is not None and value is not None
        numeric.update(numeric_line(flow, version, direction, value))
        key = (method_id, flow, version, direction.upper(), Decimal(value))
        if not index[key]:
            raise ValueError(f"source/cache exact tuple mismatch: {relative} factor {native_index}")
        static_key, static_factor_index = index[key].popleft()
        source_location = factor.find("m:location", NS)
        location = {"kind": "missing"} if source_location is None else {"kind": "blank"} if not (source_location.text or "").strip() else {"kind": "code", "value": source_location.text}
        family = (flow, version, direction.upper())
        families[family].append((native_index, location["kind"] == "code"))
        projected = adapter(factor)
        assert isinstance(projected, dict)
        qualifier = {key: value for key, value in projected.items() if key not in {"referenceToFlowDataSet", "exchangeDirection", "location", "meanValue"}}
        unknown = set(qualifier) - {"generalComment", "dataDerivationTypeStatus"}
        if unknown:
            raise ValueError(f"source has unreviewed qualifiers {unknown}: {relative}")
        comment = factor.findtext("m:generalComment", "", NS).strip()
        if comment:
            description = reference.findtext("c:shortDescription", "", NS)
            if method_id != "b5c602c6-def3-11e6-bf01-fe55135034f3" or comment not in description:
                raise ValueError(f"source comment is not a reviewed PM Flow-archetype label: {relative} factor {native_index}")
            reviewed_comments.append(native_index)
        bindings.append([static_key, static_factor_index, sha256(raw_factor), dictionary_id(locations, location_positions, location), dictionary_id(qualifiers, qualifier_positions, qualifier)])
    generic_records = []
    unresolved_regional_families = 0
    for family in families.values():
        unnamed = [position for position, named in family if not named]
        named = any(named for _, named in family)
        if len(unnamed) != 1:
            raise ValueError(f"source does not meet reviewed exact family cardinality: {relative}")
        if not named or method_id in REGIONAL_DEFAULT_BASIS:
            generic_records.extend(unnamed)
        else:
            unresolved_regional_families += 1
    method = {
        "method_id": method_id,
        "method_version": entry["method_version"],
        "artifact_locator_id": locator,
        "source_path": relative,
        "source_xml_sha256": sha256(raw),
        "method_document_sha256": sha256(canonical(document)),
        "numeric_binding_sha256": numeric.hexdigest(),
        "metadata_sha256": sha256(canonical(metadata)),
        "source_scope": metadata,
        "generic_geographic_admission": generic_geographic_admission(method_id, metadata),
        "basis": [
            {"id": "source-intervention-geographic-admission", "uri": GEOGRAPHY_DOC,
             "locator": "LCIAMethodInformation/geography/interventionLocation and geographicalRepresentativenessDescription",
             "scope": "Generic records require their source-qualified global or exact declared intervention context; no geographic inference or proxy adoption"},
            {"id": "ilcd-optional-factor-location", "uri": ILCD_DOC, "locator": "location [0..1], used only by methods using location information", "scope": "Source-specific nonregional families, preserving the method model and geography"},
            *([REGIONAL_DEFAULT_BASIS[method_id]] if method_id in REGIONAL_DEFAULT_BASIS else []),
        ],
        "regional_default_unresolved_family_count": unresolved_regional_families,
        "locations": locations,
        "qualifiers": qualifiers,
        "bindings": bindings,
        "generic_records": sorted(generic_records),
        "reviewed_comment_records": reviewed_comments,
    }
    encoded = canonical(method) + b"\n"
    compressed = gzip.compress(encoded, compresslevel=9, mtime=0)
    metadata_entry = {key: method[key] for key in ["method_id", "method_version", "artifact_locator_id", "source_path", "source_xml_sha256", "method_document_sha256", "numeric_binding_sha256", "metadata_sha256", "generic_geographic_admission", "basis", "regional_default_unresolved_family_count"]}
    metadata_entry.update({"asset": f"{locator}.json.gz", "asset_sha256": sha256(compressed), "decompressed_sha256": sha256(encoded), "decompressed_byte_size": len(encoded), "factor_count": len(bindings), "generic_record_count": len(generic_records), "reviewed_comment_count": len(reviewed_comments)})
    return metadata_entry, compressed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--cache-root", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    args = parser.parse_args()
    source_root = args.source_root.resolve(strict=True)
    cache_root = args.cache_root.resolve(strict=True)
    output = args.output_directory.resolve()
    if output.exists() or output.is_relative_to(source_root) or output.is_relative_to(cache_root):
        raise ValueError("output must be a new directory outside the protected source/cache trees")
    manifest_bytes = (cache_root / "cache_manifest.json").read_bytes()
    if sha256(manifest_bytes) != BUNDLE_SHA256:
        raise ValueError("source cache manifest is not the reviewed 1.2.4 bytes")
    manifest = json.loads(manifest_bytes)
    if manifest["factor_manifest_sha256"] != FACTOR_SHA256:
        raise ValueError("source factor manifest differs")
    factor_bytes = (cache_root / "flow_factors.json.gz").read_bytes()
    if sha256(factor_bytes) != manifest["files"]["factors"]["sha256"]:
        raise ValueError("compressed cache source hash differs")
    decompressed = gzip.decompress(factor_bytes)
    if sha256(decompressed) != manifest["files"]["factors"]["decompressed_sha256"]:
        raise ValueError("decompressed cache source hash differs")
    index: dict[tuple[object, ...], deque[tuple[str, int]]] = defaultdict(deque)
    for key, record in json.loads(decompressed).items():
        factors = record["factor"]
        for position, factor in enumerate(factors if isinstance(factors, list) else [factors]):
            identity = (factor["key"], record["@refObjectId"], record["@version"], record["exchangeDirection"].upper(), Decimal(factor["value"]))
            index[identity].append((key, position))
    entries = []
    assets = []
    for entry in manifest["methods"]:
        metadata, compressed = source_method(source_root, entry, index)
        entries.append(metadata)
        assets.append((metadata["asset"], compressed))
    if any(index.values()):
        raise ValueError("cache has factors absent from exact original source multiset")
    companion_manifest = {"schema_version": SCHEMA_VERSION, "policy_version": POLICY_VERSION, "source_repository": "tiangong-lca/data", "source_commit": SOURCE_COMMIT, "static_bundle_manifest_sha256": BUNDLE_SHA256, "static_factor_manifest_sha256": FACTOR_SHA256, "record_hash_kind": "sha256-original-factor-xml-bytes.v1", "numeric_binding": "lcia.source-numeric-binding.v1: UUID|exactVersion|UPPERCASEdirection|f64BitsHexLowercase16, LF records in native order", "methods": entries}
    encoded_manifest = json.dumps(companion_manifest, ensure_ascii=False, indent=2) + "\n"
    output.mkdir(parents=True, exist_ok=False)
    for name, compressed in assets:
        (output / str(name)).write_bytes(compressed)
    (output / "manifest.json").write_text(encoded_manifest)
    print(json.dumps({"method_count": len(entries), "factor_count": sum(entry["factor_count"] for entry in entries), "compressed_byte_size": sum(len(asset) for _, asset in assets), "manifest_sha256": sha256(encoded_manifest.encode()), "output_directory": str(output)}))


if __name__ == "__main__":
    main()
