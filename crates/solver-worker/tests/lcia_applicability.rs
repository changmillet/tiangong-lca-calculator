// These assertions compare parsed source values without arithmetic or rounding.
#![allow(clippy::float_cmp)]

use serde_json::{Value, json};
use solver_worker::calculation_evidence::{LciaExchangeDirection, LciaExchangeObservation};
use solver_worker::lcia_applicability::{DecisionStatus, FactorLocation, MethodFactors};
use uuid::Uuid;

const FLOW_VERSION: &str = "03.00.004";

fn flow_id() -> Uuid {
    Uuid::from_u128(1)
}

fn factor(location: &str, value: Value) -> Value {
    let mut record = json!({
        "referenceToFlowDataSet": {"@refObjectId": flow_id(), "@version": FLOW_VERSION},
        "exchangeDirection": "Output",
        "location": location
    });
    record["meanValue"] = value;
    record
}

fn document(factors: Vec<Value>) -> Value {
    json!({"LCIAMethodDataSet": {"characterisationFactors": {"factor": Value::Array(factors)}}})
}

fn load(document: &Value) -> anyhow::Result<MethodFactors> {
    MethodFactors::from_method_document(
        Uuid::from_u128(2),
        "01.04.000",
        Uuid::from_u128(3),
        document,
    )
}

fn observation(location: FactorLocation) -> LciaExchangeObservation {
    LciaExchangeObservation {
        flow_id: flow_id(),
        flow_version: FLOW_VERSION.to_owned(),
        location,
        direction: Some(LciaExchangeDirection::Output),
        direction_label: "Output".to_owned(),
        exchange_id: "synthetic-process:exchange-1".to_owned(),
        amount: Some(1.0),
    }
}

#[test]
fn exact_locations_select_one_factor_and_glo_is_only_a_literal_token() {
    let methods = load(&document(vec![
        factor("A", json!(4.0)),
        factor("B", json!(11.491)),
        factor("GLO", json!(3.02)),
    ]))
    .expect("synthetic complete method");

    for (location, expected) in [("A", 4.0), ("B", 11.491), ("GLO", 3.02)] {
        let decision = methods.select(&observation(FactorLocation::Code(location.to_owned())));
        assert_eq!(decision.status, DecisionStatus::Selected);
        assert_eq!(
            decision.selected.as_ref().expect("selected factor").value,
            expected
        );
        assert_eq!(decision.rejected.len(), 2);
        assert!(
            decision
                .rejected
                .iter()
                .all(|item| item.reason == "location_mismatch")
        );
    }

    let unknown_country = methods.select(&observation(FactorLocation::Code("C".to_owned())));
    assert_ne!(unknown_country.status, DecisionStatus::Selected);
    assert!(
        unknown_country.selected.is_none(),
        "GLO must not become a fallback"
    );
}

#[test]
fn zero_and_negative_factors_remain_selected_scientific_values() {
    let methods = load(&document(vec![
        factor("A", json!(0.0)),
        factor("B", json!(-2.5)),
    ]))
    .expect("synthetic complete method");
    for (location, expected) in [("A", 0.0), ("B", -2.5)] {
        let decision = methods.select(&observation(FactorLocation::Code(location.to_owned())));
        assert_eq!(decision.status, DecisionStatus::Selected);
        assert_eq!(
            decision
                .selected
                .expect("zero and negative factors have receipts")
                .value,
            expected
        );
    }
}

#[test]
fn equal_regional_values_are_distinct_records_and_exact_duplicates_are_ambiguous() {
    let methods = load(&document(vec![
        factor("A", json!(7)),
        factor("B", json!(7)),
    ]))
    .expect("synthetic complete method");
    assert_eq!(methods.records.len(), 2);
    let a = methods.select(&observation(FactorLocation::Code("A".to_owned())));
    let b = methods.select(&observation(FactorLocation::Code("B".to_owned())));
    assert_eq!(a.status, DecisionStatus::Selected);
    assert_eq!(b.status, DecisionStatus::Selected);
    assert_ne!(
        a.selected.as_ref().unwrap().record_sha256,
        b.selected.as_ref().unwrap().record_sha256
    );
    assert_eq!(a.rejected.len(), 1);
    assert_eq!(b.rejected.len(), 1);

    let duplicate = factor("A", json!(7));
    let methods = load(&document(vec![duplicate.clone(), duplicate])).expect("retain duplicates");
    let decision = methods.select(&observation(FactorLocation::Code("A".to_owned())));
    assert_eq!(decision.status, DecisionStatus::Unsupported);
    assert_eq!(decision.reason, "lcia_factor_applicability_ambiguous");
    assert!(decision.selected.is_none());
    assert_eq!(decision.rejected.len(), 2);
}

#[test]
fn factor_versions_and_directions_cannot_cross_match() {
    let methods = load(&document(vec![factor("A", json!(4))])).expect("synthetic complete method");
    let mut exchange = observation(FactorLocation::Code("A".to_owned()));
    exchange.flow_version = "03.00.003".to_owned();
    let decision = methods.select(&exchange);
    assert_eq!(decision.status, DecisionStatus::Unsupported);
    assert_eq!(decision.reason, "lcia_factor_flow_version_mismatch");
    assert!(decision.selected.is_none());

    exchange.flow_version = FLOW_VERSION.to_owned();
    exchange.direction = Some(LciaExchangeDirection::Input);
    let decision = methods.select(&exchange);
    assert_ne!(decision.status, DecisionStatus::Selected);
    assert!(decision.selected.is_none());
    assert_eq!(decision.rejected[0].reason, "direction_mismatch");
}

#[test]
fn omitted_blank_and_unknown_exchange_locations_are_distinct_and_unsupported() {
    let methods =
        load(&document(vec![factor("GLO", json!(3.02))])).expect("synthetic complete method");
    let states = [
        FactorLocation::Missing,
        FactorLocation::Blank,
        FactorLocation::Unknown,
    ];
    let mut serialized = std::collections::BTreeSet::new();
    for location in states {
        let decision = methods.select(&observation(location.clone()));
        assert_eq!(decision.status, DecisionStatus::Unsupported);
        assert_eq!(decision.location, location);
        assert!(decision.selected.is_none());
        serialized.insert(serde_json::to_string(&decision.location).unwrap());
    }
    assert_eq!(serialized.len(), 3);

    let legacy_exchange = json!({"flow_id": flow_id(), "flow_version": FLOW_VERSION,
        "direction": "Output", "direction_label": "Output", "exchange_id": "old", "amount": 1});
    let legacy: LciaExchangeObservation =
        serde_json::from_value(legacy_exchange).expect("old evidence readable");
    assert_eq!(legacy.location, FactorLocation::Unknown);
    assert_eq!(methods.select(&legacy).status, DecisionStatus::Unsupported);
}

#[test]
fn omitted_and_blank_source_locations_do_not_declare_defaults() {
    for location in [None, Some("")] {
        let mut record = factor("A", json!(3.02));
        match location {
            None => {
                record.as_object_mut().unwrap().remove("location");
            }
            Some(value) => record["location"] = json!(value),
        }
        let methods = load(&document(vec![record])).expect("preserve unresolved source location");
        assert_eq!(
            methods.records[0].location,
            if location.is_some() {
                FactorLocation::Blank
            } else {
                FactorLocation::Missing
            }
        );
        let decision = methods.select(&observation(FactorLocation::Code("A".to_owned())));
        assert_eq!(decision.status, DecisionStatus::Unsupported);
        assert!(decision.selected.is_none());
    }
}

#[test]
fn comments_and_unknown_qualifiers_cannot_silently_authorize_a_factor() {
    for (key, value) in [
        (
            "generalComment",
            json!("Only applies under a source condition"),
        ),
        (
            "syntheticApplicabilityCondition",
            json!({"temperature": "specific"}),
        ),
    ] {
        let mut record = factor("A", json!(4));
        record[key] = value.clone();
        let methods = load(&document(vec![record])).expect("retain qualifier evidence");
        let decision = methods.select(&observation(FactorLocation::Code("A".to_owned())));
        assert_eq!(decision.status, DecisionStatus::Unsupported);
        assert_eq!(decision.reason, "lcia_factor_qualifier_unsupported");
        assert!(decision.selected.is_none());
        assert_eq!(decision.rejected[0].factor.qualifiers[key], value);
    }
}

#[test]
fn malformed_and_missing_factor_fields_fail_without_becoming_zero_or_absent_factors() {
    let mutations = [
        (
            "/referenceToFlowDataSet/@refObjectId",
            json!("invalid-uuid"),
        ),
        ("/referenceToFlowDataSet/@refObjectId", Value::Null),
        ("/referenceToFlowDataSet/@version", json!("")),
        ("/referenceToFlowDataSet/@version", json!("unknown")),
        ("/referenceToFlowDataSet/@version", json!("not-a-version")),
        ("/referenceToFlowDataSet/@version", Value::Null),
        ("/exchangeDirection", json!("sideways")),
        ("/exchangeDirection", Value::Null),
        ("/meanValue", json!("NaN")),
        ("/meanValue", json!("inf")),
        ("/meanValue", json!("not-a-number")),
        ("/meanValue", Value::Null),
        ("/location", json!(["A", "B"])),
        ("/location", json!({"#text": "A"})),
        ("/location", json!(123)),
        ("/location", Value::Null),
    ];
    for (path, value) in mutations {
        let mut record = factor("A", json!(4));
        *record.pointer_mut(path).expect("fixture path") = value;
        assert!(
            load(&document(vec![record])).is_err(),
            "must reject malformed field {path}"
        );
    }
    for field in ["@refObjectId", "@version"] {
        let mut record = factor("A", json!(4));
        record["referenceToFlowDataSet"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            load(&document(vec![record])).is_err(),
            "must reject missing {field}"
        );
    }
    for field in ["exchangeDirection", "meanValue"] {
        let mut record = factor("A", json!(4));
        record.as_object_mut().unwrap().remove(field);
        assert!(
            load(&document(vec![record])).is_err(),
            "must reject missing {field}"
        );
    }
}

#[test]
fn source_and_record_receipts_change_when_applicability_changes() {
    let a = load(&document(vec![factor("A", json!(4))])).expect("first source");
    let b = load(&document(vec![factor("B", json!(4))])).expect("second source");
    assert_ne!(a.source.source_sha256, b.source.source_sha256);
    assert_ne!(a.records[0].record_sha256, b.records[0].record_sha256);
    let decision = a.select(&observation(FactorLocation::Code("A".to_owned())));
    assert_eq!(decision.source, a.source);
    assert_eq!(
        decision.selected.unwrap().record_sha256,
        a.records[0].record_sha256
    );
}
