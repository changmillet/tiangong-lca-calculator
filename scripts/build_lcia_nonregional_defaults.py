#!/usr/bin/env python3
"""Prepare nonregional defaults; original source/cache assets are read-only.

Reuses PR #340's exact source identities. This is a calculation profile, not
geographic/scientific admission: native absent/blank locations are its defaults.
All other records remain regional alternatives. No operator proof is required.
"""
import argparse
from collections import Counter, defaultdict
from decimal import Decimal
import gzip
import hashlib
import json
from pathlib import Path
import re
import struct
import xml.etree.ElementTree as ET

NS = {"m": "http://lca.jrc.it/ILCD/LCIAMethod"}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def build(data_root, cache_dir):
    sources = json.loads(Path(__file__).with_name("lcia_nonregional_sources.json").read_text())
    manifest = (cache_dir / "cache_manifest.json").read_bytes()
    assert digest(manifest) == sources["static_bundle_manifest_sha256"], "cache manifest drift"
    cache_manifest = json.loads(manifest)
    factor_file = cache_manifest["files"]["factors"]
    raw_cache = (cache_dir / Path(factor_file["path"]).name).read_bytes()
    assert digest(raw_cache) == factor_file["sha256"], "factor asset drift"
    cache = json.loads(gzip.decompress(raw_cache), parse_float=Decimal)
    cached = defaultdict(Counter)
    for record in cache.values():
        items = record["factor"]
        for item in items if isinstance(items, list) else [items]:
            cached[item["key"]][(record["@refObjectId"], record["@version"],
                                 record["exchangeDirection"].upper(), Decimal(str(item["value"])))] += 1
    result = {"schema": "lcia.nonregional-defaults.v1", **sources, "methods": []}
    for method in sources["methods"]:
        raw = (data_root / method["source_path"]).read_bytes()
        assert digest(raw) == method["source_xml_sha256"], "XML source drift"
        factors = ET.fromstring(raw).findall("m:characterisationFactors/m:factor", NS)
        raw_factors = re.findall(rb"<factor(?:\s[^>]*)?>.*?</factor\s*>", raw, re.DOTALL)
        assert len(factors) == len(raw_factors) == method["factor_count"]
        groups = defaultdict(list)
        numeric = Counter()
        for ordinal, factor in enumerate(factors):
            ref = factor.find("m:referenceToFlowDataSet", NS)
            key = (ref.attrib["refObjectId"], ref.attrib["version"],
                   factor.findtext("m:exchangeDirection", namespaces=NS).upper())
            value = Decimal(factor.findtext("m:meanValue", namespaces=NS))
            location = factor.findtext("m:location", namespaces=NS)
            groups[key].append((ordinal, value, location))
            numeric[(*key, value)] += 1
        assert numeric == cached[method["method_id"]], "source/cache numeric multiset drift"
        alternatives = []
        for key, records in sorted(groups.items()):
            defaults = [r for r in records if r[2] is None or not r[2].strip()]
            assert len(defaults) == 1, f"source does not define one nonregional default: {key}"
            if len(records) == 1:
                continue
            ordinal, value, location = defaults[0]
            alternatives.append({"flow_id": key[0], "flow_version": key[1], "direction": key[2],
                                 "value_bits": struct.pack(">d", float(value)).hex(),
                                 "source_record_index": ordinal,
                                 "source_record_sha256": digest(raw_factors[ordinal]),
                                 "source_location": location, "alternative_count": len(records)})
        result["methods"].append({**method, "default_count": len(groups), "alternatives": alternatives})
    return (json.dumps(result, ensure_ascii=False, indent=2) + "\n").encode()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-root", type=Path, required=True)
    parser.add_argument("--cache-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    output = build(args.data_root, args.cache_dir)
    if args.verify:
        assert args.output.read_bytes() == output, "prepared defaults drift"
    else:
        with args.output.open("xb") as target:
            target.write(output)
    print(json.dumps({"sha256": digest(output), "bytes": len(output)}))
