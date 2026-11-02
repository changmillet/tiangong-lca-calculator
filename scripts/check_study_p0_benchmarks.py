#!/usr/bin/env python3
"""Check hand-derived P0 expectations without importing a product implementation.

This is a test oracle, not a model compiler or production solver. It verifies
supplied exact solutions against the stated equations; it never rewrites them.
"""

from __future__ import annotations

import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path


FIXTURES = Path(__file__).resolve().parents[1] / "crates/solver-worker/tests/fixtures/study_p0"
PROFILE_ID = "lca.study-deterministic-p0.v1"
REQUIRED_SYSTEMS = {
    "two-node-cycle", "self-reuse", "repeated-source-independent-instances",
    "negative-inventory-known-zero",
}
REQUIRED_ALLOCATIONS = {
    "coproduct-p", "coproduct-q", "input-treatment-reference", "explicit-and-sparse-zero",
}
REQUIRED_DIAGNOSTICS = {
    "flow-revision-mismatch", "unit-identity-mismatch", "missing-reference-unit",
    "missing-factor", "factor-revision-mismatch", "factor-direction-mismatch",
    "explicit-zero-factor",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def vector(values: list[str]) -> list[Fraction]:
    require(all(isinstance(value, str) for value in values), "exact numbers must be strings")
    return [Fraction(value) for value in values]


def matrix(values: list[list[str]], rows: int, columns: int) -> list[list[Fraction]]:
    require(len(values) == rows and all(len(row) == columns for row in values), "matrix dimension")
    return [vector(row) for row in values]


def multiply(rows: list[list[Fraction]], values: list[Fraction]) -> list[Fraction]:
    require(all(len(row) == len(values) for row in rows), "matrix/vector dimension")
    return [sum((a * b for a, b in zip(row, values)), Fraction()) for row in rows]


def require_unique_solution(rows: list[list[Fraction]]) -> None:
    """Exact elimination proves nonsingularity, without generating a solution."""
    work = [row[:] for row in rows]
    for column in range(len(work)):
        pivot = next((index for index in range(column, len(work)) if work[index][column]), None)
        require(pivot is not None, "expected numerical case must have a unique solution")
        work[column], work[pivot] = work[pivot], work[column]
        for index in range(column + 1, len(work)):
            scale = work[index][column] / work[column][column]
            work[index] = [a - scale * b for a, b in zip(work[index], work[column])]


def check_system(case: dict) -> None:
    n = len(case["activities"])
    instances = [item["instanceId"] for item in case["activities"]]
    require(n > 0 and len(set(instances)) == n, "independent instance axis required")
    require(all(item["sourceVersion"] for item in case["activities"]), "exact source revision required")
    a = matrix(case["A"], n, n)
    b = matrix(case["B"], len(case["B"]), n)
    c = matrix(case["C"], len(case["C"]), len(b))
    require(bool(b) and bool(c), "inventory and impact axes required")
    y = vector(case["y"])
    x, g, h = (vector(case["expected"][key]) for key in ("x", "g", "h"))
    require(len(y) == n and len(x) == n, "demand/activity dimension")
    m = [[Fraction(i == j) - a[i][j] for j in range(n)] for i in range(n)]
    require_unique_solution(m)
    require(multiply(m, x) == y, f"{case['id']}: (I-A)x != y")
    require(multiply(b, x) == g, f"{case['id']}: Bx != g")
    require(multiply(c, g) == h, f"{case['id']}: Cg != h")
    require(all(value >= 0 for value in x), "negative activity is outside successful P0 cases")
    require(bool(case["derivation"]), "manual derivation must be reviewable")


def selected_fraction(exchange: dict, reference: str) -> Fraction:
    declaration = exchange.get("allocations")
    if declaration is None:
        return Fraction(1)
    allocation = declaration["allocation"]
    items = allocation if isinstance(allocation, list) else [allocation]
    targets = [item["@internalReferenceToCoProduct"] for item in items]
    require(len(targets) == len(set(targets)), "duplicate allocation target")
    fractions = [Fraction(str(item["@allocatedFraction"])) / 100 for item in items]
    require(all(0 <= value <= 1 for value in fractions), "allocation fraction range")
    require(sum(fractions) == 1, "qualification vectors must close exactly")
    return sum((value for target, value in zip(targets, fractions) if target == reference), Fraction())


def check_allocation(case: dict) -> None:
    exchanges = case["exchanges"]
    ids = [item["@dataSetInternalID"] for item in exchanges]
    require(len(ids) == len(set(ids)) and ids.count(case["reference"]) == 1, "unique reference required")
    require(all(item["exchangeDirection"] in ("Input", "Output") for item in exchanges), "exchange direction")
    for item in exchanges:
        if "allocations" in item:
            allocation = item["allocations"]["allocation"]
            targets = allocation if isinstance(allocation, list) else [allocation]
            require(all(target["@internalReferenceToCoProduct"] in ids for target in targets), "unknown allocation target")
    fractions = [selected_fraction(item, case["reference"]) for item in exchanges]
    require(fractions == vector(case["expectedFractions"]), f"{case['id']}: wrong selected share")
    signed = [Fraction(item["resultingAmount"]) * (-1 if item["exchangeDirection"] == "Input" else 1) for item in exchanges]
    reference = signed[ids.index(case["reference"])]
    require(reference != 0, "zero quantitative reference")
    normalized = [value * share / abs(reference) for value, share in zip(signed, fractions)]
    require(normalized == vector(case["expectedNormalizedSigned"]), f"{case['id']}: normalization")
    require(reference / abs(reference) == Fraction(case["expectedPivot"]), "reference direction lost")


def diagnostic(case: dict) -> str:
    """Fixture vocabulary; these strings do not create new runtime error codes."""
    if "inventory" in case:
        inventory, factor = case["inventory"], case["factor"]
        if factor is None or any(inventory[key] != factor[key] for key in ("flow", "version", "direction")):
            return "incomplete_coverage_not_zero"
        return "characterized_zero" if Fraction(factor["value"]) == 0 else "characterized"
    left, right = case["left"], case["right"]
    unit_keys = ("flowPropertyId", "flowPropertyVersion", "unitGroupId", "unitGroupVersion", "referenceUnitId")
    if any(not isinstance(side.get("unit"), dict) or not all(side["unit"].get(key) for key in unit_keys) for side in (left, right)):
        return "reference_unit_missing"
    if left["id"] != right["id"] or left["version"] != right["version"]:
        return "flow_version_mismatch"
    return "unit_identity_mismatch" if left["unit"] != right["unit"] else "compatible"


def check_ids(cases: list[dict], required: set[str]) -> None:
    ids = [case["id"] for case in cases]
    require(len(ids) == len(set(ids)) and set(ids) == required, "missing, duplicate or unexpected qualification case")


def check_documents(profile: dict, fixture: dict, candidate: dict) -> None:
    require(profile["schema"] == "lca.study-p0-profile.v1", "profile schema")
    require(profile["profileId"] == fixture["profileId"] == PROFILE_ID, "profile identity")
    require(profile["runtimeEnabled"] is False, "P0 cannot enable a runtime")
    require(fixture["schema"] == "lca.study-p0-benchmarks.v1", "fixture schema")
    require(fixture["dataClassification"] == "synthetic-numerical-qualification-only", "synthetic scope")
    require(candidate["status"] == profile["businessPilot"]["status"] == "not_qualified", "candidate must not claim business acceptance")
    require(candidate["observedSupportClosure"]["declaredReferenceVersionMismatches"] > 0, "historical defect evidence")
    for key, required, check in (
        ("systems", REQUIRED_SYSTEMS, check_system),
        ("allocationCases", REQUIRED_ALLOCATIONS, check_allocation),
        ("diagnosticCases", REQUIRED_DIAGNOSTICS, lambda case: require(diagnostic(case) == case["expected"], f"{case['id']}: diagnostic")),
    ):
        check_ids(fixture[key], required)
        for case in fixture[key]:
            check(case)


def check_native(report: dict, profile_bytes: bytes, fixture_bytes: bytes, fixture: dict, profile: dict) -> None:
    require(report["schema"] == "lca.study-p0-native-result.v1", "native result schema")
    require(report["fixtureSha256"] == hashlib.sha256(fixture_bytes).hexdigest(), "native result fixture drift")
    require(report["profileSha256"] == hashlib.sha256(profile_bytes).hexdigest(), "native result profile drift")
    require(report["scope"] == "core-solve-and-allocation-primitives-not-model-compiler", "native proof scope")
    require(report["businessPilotQualified"] is False, "native result cannot qualify business pilot")
    reference = report["calculationContractRef"]
    require(reference["kind"] == "calculation_contract" and reference["schema"] == "lca.calculation-contract.v1", "calculation reference wrapper")
    require(reference["sha256"] == hashlib.sha256(report["descriptorBytes"].encode()).hexdigest(), "Worker descriptor bytes/hash mismatch")
    descriptor = json.loads(report["descriptorBytes"])
    require(descriptor["schema"] == reference["schema"] and descriptor["engine"] == "solver-worker", "Worker descriptor identity")
    require(descriptor["allocation_semantics"] == profile["semantics"]["allocation"], "allocation profile drift")
    require(descriptor["lcia_selection"] == profile["semantics"]["lciaSelection"], "LCIA profile drift")
    absolute, relative, residual = (Fraction(profile["tolerances"][key]) for key in ("absolute", "relative", "residual"))
    for group, expected_group, keys in (
        ("systems", "systems", ("x", "g", "h")),
        ("allocationCases", "allocationCases", ("fractions", "normalizedSigned")),
    ):
        actual = report[group]
        check_ids(actual, {item["id"] for item in fixture[expected_group]})
        expected_by_id = {item["id"]: item for item in fixture[expected_group]}
        for item in actual:
            case = expected_by_id[item["id"]]
            for key in keys:
                values = case["expected"][key] if group == "systems" else case[{"fractions": "expectedFractions", "normalizedSigned": "expectedNormalizedSigned"}[key]]
                expected = vector(values)
                numbers = [Fraction(str(value)) for value in item[key]]
                require(len(numbers) == len(expected), "native result vector dimension")
                require(all(abs(a - b) <= absolute + relative * abs(b) for a, b in zip(numbers, expected)), f"{item['id']}: native {key} mismatch")
            if group == "systems":
                x = [Fraction(str(value)) for value in item["x"]]
                a = [[Fraction(value) for value in row] for row in case["A"]]
                m = [[Fraction(i == j) - a[i][j] for j in range(len(a))] for i in range(len(a))]
                require(max(abs(v - y) for v, y in zip(multiply(m, x), vector(case["y"]))) <= residual, "native balance residual")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path, default=FIXTURES)
    parser.add_argument("--native-results", type=Path)
    args = parser.parse_args()
    try:
        profile_bytes = (args.fixtures / "profile.json").read_bytes()
        fixture_bytes = (args.fixtures / "benchmarks.json").read_bytes()
        profile, fixture = json.loads(profile_bytes), json.loads(fixture_bytes)
        candidate = json.loads((args.fixtures / "historical-cement-candidate.json").read_bytes())
        check_documents(profile, fixture, candidate)
        if args.native_results:
            check_native(json.loads(args.native_results.read_bytes()), profile_bytes, fixture_bytes, fixture, profile)
    except (OSError, ValueError, KeyError, TypeError, ZeroDivisionError) as error:
        parser.exit(1, f"study-p0: FAIL: {error}\n")
    print(json.dumps({"status": "passed", "profileId": PROFILE_ID, "systemCases": len(fixture["systems"]), "allocationCases": len(fixture["allocationCases"]), "diagnosticCases": len(fixture["diagnosticCases"]), "nativeResultChecked": args.native_results is not None, "runtimeEnabled": False, "businessPilotQualified": False}, sort_keys=True))


if __name__ == "__main__":
    main()
