//! Offline qualification of the existing core and public allocation primitives.
//! This deliberately does not implement a product-system compiler.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solver_core::{ModelSparseData, NumericOptions, SolveOptions, SolverService, SparseTriplet};
use solver_worker::signed_flow::{
    SignedFlowDirection, normalize_reference_coefficient, signed_coefficient,
};
use solver_worker::tidas_process_semantics::{
    TIDAS_ALLOCATION_SEMANTICS_VERSION, TidasAllocationResolution,
    resolve_tidas_process_allocations,
};
use uuid::Uuid;

const FIXTURE: &[u8] = include_bytes!("fixtures/study_p0/benchmarks.json");
const PROFILE: &[u8] = include_bytes!("fixtures/study_p0/profile.json");

fn number(value: &Value) -> f64 {
    let text = value.as_str().expect("exact fixture numbers are strings");
    let result = text.split_once('/').map_or_else(
        || text.parse::<f64>().unwrap(),
        |(numerator, denominator)| {
            numerator.parse::<f64>().unwrap() / denominator.parse::<f64>().unwrap()
        },
    );
    assert!(result.is_finite());
    result
}

fn numbers(value: &Value) -> Vec<f64> {
    value.as_array().unwrap().iter().map(number).collect()
}

fn triplets(value: &Value) -> Vec<SparseTriplet> {
    value
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .flat_map(|(row, values)| {
            values
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .filter_map(move |(col, value)| {
                    let value = number(value);
                    (value != 0.0).then(|| SparseTriplet {
                        row: i32::try_from(row).unwrap(),
                        col: i32::try_from(col).unwrap(),
                        value,
                    })
                })
        })
        .collect()
}

fn assert_vector(actual: &[f64], expected: &Value, tolerance: &Value) {
    let expected = numbers(expected);
    assert_eq!(actual.len(), expected.len());
    let absolute = number(&tolerance["absolute"]);
    let relative = number(&tolerance["relative"]);
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(actual.is_finite());
        assert!(
            (actual - expected).abs() <= absolute + relative * expected.abs(),
            "actual={actual}, expected={expected}"
        );
    }
}

fn run_system(case: &Value, tolerance: &Value) -> Value {
    let count = case["activities"].as_array().unwrap().len();
    let data = ModelSparseData {
        model_version: Uuid::new_v4(),
        process_count: i32::try_from(count).unwrap(),
        flow_count: i32::try_from(case["B"].as_array().unwrap().len()).unwrap(),
        impact_count: i32::try_from(case["C"].as_array().unwrap().len()).unwrap(),
        technosphere_entries: triplets(&case["A"]),
        biosphere_entries: triplets(&case["B"]),
        characterization_factors: triplets(&case["C"]),
    };
    let service = SolverService::new();
    service.prepare(&data, NumericOptions::default()).unwrap();
    let solved = service
        .solve_one(
            data.model_version,
            NumericOptions::default(),
            &numbers(&case["y"]),
            SolveOptions::default(),
        )
        .unwrap();
    let x = solved.x.unwrap();
    let g = solved.g.unwrap();
    let h = solved.h.unwrap();
    assert_vector(&x, &case["expected"]["x"], tolerance);
    assert_vector(&g, &case["expected"]["g"], tolerance);
    assert_vector(&h, &case["expected"]["h"], tolerance);
    json!({"id": case["id"], "x": x, "g": g, "h": h})
}

fn direction(exchange: &Value) -> SignedFlowDirection {
    match exchange["exchangeDirection"].as_str().unwrap() {
        "Input" => SignedFlowDirection::Input,
        "Output" => SignedFlowDirection::Output,
        value => panic!("unsupported fixture direction {value}"),
    }
}

fn run_allocation(case: &Value, tolerance: &Value) -> Value {
    let exchanges = case["exchanges"].as_array().unwrap();
    let borrowed = exchanges.iter().collect::<Vec<_>>();
    let reference = case["reference"].as_str().unwrap();
    let before = exchanges.clone();
    let allocation = resolve_tidas_process_allocations(&borrowed, reference).unwrap();
    assert_eq!(&before, exchanges, "source inventory must not mutate");
    let fractions = allocation
        .exchanges
        .iter()
        .map(|resolution| match resolution {
            TidasAllocationResolution::Explicit { fraction }
            | TidasAllocationResolution::LegacyInferredReference { fraction } => *fraction,
            TidasAllocationResolution::SparseZero => 0.0,
            TidasAllocationResolution::Undeclared
            | TidasAllocationResolution::LegacyEmptyUndeclared => 1.0,
        })
        .collect::<Vec<_>>();
    let reference_exchange = exchanges
        .iter()
        .find(|exchange| exchange["@dataSetInternalID"] == reference)
        .unwrap();
    let raw_pivot = signed_coefficient(
        direction(reference_exchange),
        number(&reference_exchange["resultingAmount"]),
    )
    .unwrap();
    let pivot = normalize_reference_coefficient(raw_pivot).unwrap();
    assert!((pivot.coefficient - number(&case["expectedPivot"])).abs() <= f64::EPSILON);
    let normalized = exchanges
        .iter()
        .zip(&fractions)
        .map(|(exchange, fraction)| {
            signed_coefficient(direction(exchange), number(&exchange["resultingAmount"])).unwrap()
                * fraction
                * pivot.scale
        })
        .collect::<Vec<_>>();
    assert_vector(&fractions, &case["expectedFractions"], tolerance);
    assert_vector(&normalized, &case["expectedNormalizedSigned"], tolerance);
    json!({"id": case["id"], "fractions": fractions, "normalizedSigned": normalized})
}

#[test]
fn study_p0_existing_core_and_allocation_match_independent_expectations() {
    let fixture: Value = serde_json::from_slice(FIXTURE).unwrap();
    let profile: Value = serde_json::from_slice(PROFILE).unwrap();
    assert_eq!(profile["runtimeEnabled"], false);
    assert_eq!(profile["profileId"], fixture["profileId"]);
    assert_eq!(
        profile["semantics"]["allocation"],
        TIDAS_ALLOCATION_SEMANTICS_VERSION
    );
    let descriptor = solver_worker::calculation_contract::descriptor();
    let descriptor_value: Value = serde_json::from_str(&descriptor).unwrap();
    assert_eq!(
        descriptor_value["lcia_selection"],
        profile["semantics"]["lciaSelection"]
    );
    let tolerance = &profile["tolerances"];
    let systems = fixture["systems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| run_system(case, tolerance))
        .collect::<Vec<_>>();
    let allocations = fixture["allocationCases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| run_allocation(case, tolerance))
        .collect::<Vec<_>>();
    let report = json!({
        "schema": "lca.study-p0-native-result.v1",
        "profileSha256": format!("{:x}", Sha256::digest(PROFILE)),
        "fixtureSha256": format!("{:x}", Sha256::digest(FIXTURE)),
        "scope": "core-solve-and-allocation-primitives-not-model-compiler",
        "businessPilotQualified": false,
        "calculationContractRef": {
            "kind": "calculation_contract",
            "schema": "lca.calculation-contract.v1",
            "sha256": solver_worker::calculation_contract::fingerprint()
        },
        "descriptorBytes": descriptor,
        "systems": systems,
        "allocationCases": allocations
    });
    if let Some(path) = std::env::var_os("STUDY_P0_NATIVE_OUTPUT") {
        assert!(
            std::path::Path::new(&path).is_absolute(),
            "STUDY_P0_NATIVE_OUTPUT must be absolute; Cargo tests run from the crate directory"
        );
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
