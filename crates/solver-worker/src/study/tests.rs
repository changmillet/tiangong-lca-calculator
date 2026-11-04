#![allow(clippy::needless_pass_by_value, clippy::field_reassign_with_default)]
use super::compile::{Factor, VerifiedMethods};
use super::{contracts::*, *};
use serde_json::{Value, json};

fn document(kind: &str, id: &str, value: Value) -> Document {
    let json = value.to_string();
    Document {
        kind: kind.into(),
        state_code: 100,
        owner_actor_id: None,
        identity: DatasetRef {
            id: id.into(),
            version: "1".into(),
        },
        sha256: sha256(json.as_bytes()),
        json,
    }
}
fn reference(id: &str) -> Value {
    json!({"@refObjectId":id,"@version":"1"})
}
fn raw_exchange(id: &str, flow: &str, direction: &str, amount: f64) -> Value {
    json!({"@dataSetInternalID":id,"referenceToFlowDataSet":reference(flow),"exchangeDirection":direction,"resultingAmount":amount})
}
fn process(id: &str, exchanges: Vec<Value>) -> Document {
    document(
        "process",
        id,
        json!({"processDataSet":{"processInformation":{"dataSetInformation":{"common:UUID":id},"quantitativeReference":{"referenceToReferenceFlow":"p"}},"administrativeInformation":{"publicationAndOwnership":{"common:dataSetVersion":"1"}},"exchanges":{"exchange":exchanges}}}),
    )
}
fn model(instances: Value) -> Document {
    document(
        "lifecyclemodel",
        "model",
        json!({"lifeCycleModelDataSet":{"lifeCycleModelInformation":{"dataSetInformation":{"common:UUID":"model"},"quantitativeReference":{"referenceToReferenceProcess":"a"},"technology":{"processes":{"processInstance":instances}}},"administrativeInformation":{"publicationAndOwnership":{"common:dataSetVersion":"1"}}}}),
    )
}
fn instance(id: &str, process: &str, connections: Value) -> Value {
    json!({"@dataSetInternalID":id,"referenceToProcess":reference(process),"connections":{"outputExchange":connections}})
}
fn connection(flow: &str, target: &str) -> Value {
    json!({"@flowUUID":flow,"@version":"1","downstreamProcess":{"@id":target,"@flowUUID":flow,"@version":"1"}})
}
fn methods() -> VerifiedMethods {
    VerifiedMethods {
        axes: vec![MethodAxis {
            id: "method".into(),
            version: "1".into(),
            unit: "impact".into(),
        }],
        factors: vec![vec![Factor {
            flow: DatasetRef {
                id: "emission".into(),
                version: "1".into(),
            },
            direction: "Output".into(),
            value: 2.0,
        }]],
        source_hash: "synthetic-methods".into(),
    }
}
fn input(mut processes: Vec<Document>, model: Document, flows: &[&str]) -> SourceInput {
    processes.push(model);
    for id in flows {
        processes.push(document("flow",id,json!({"flowDataSet":{"flowInformation":{"dataSetInformation":{"common:UUID":id},"quantitativeReference":{"referenceToReferenceFlowProperty":"0"}},"administrativeInformation":{"publicationAndOwnership":{"common:dataSetVersion":"1"}},"flowProperties":{"flowProperty":{"@dataSetInternalID":"0","meanValue":"1","referenceToFlowPropertyDataSet":reference("mass")}},"modellingAndValidation":{"LCIMethod":{"typeOfDataSet":if *id=="emission"{"Elementary flow"}else{"Product flow"}}}}})));
    }
    processes.push(document("flowproperty","mass",json!({"flowPropertyDataSet":{"flowPropertiesInformation":{"dataSetInformation":{"common:UUID":"mass"},"quantitativeReference":{"referenceToReferenceUnitGroup":reference("kg")}},"administrativeInformation":{"publicationAndOwnership":{"common:dataSetVersion":"1"}}}})));
    processes.push(document("unitgroup","kg",json!({"unitGroupDataSet":{"unitGroupInformation":{"dataSetInformation":{"common:UUID":"kg"},"quantitativeReference":{"referenceToReferenceUnit":"0"}},"administrativeInformation":{"publicationAndOwnership":{"common:dataSetVersion":"1"}},"units":{"unit":{"@dataSetInternalID":"0","name":"kg","meanValue":"1"}}}})));
    SourceInput {
        schema_version: SOURCE_SCHEMA.into(),
        actor_id: None,
        origin: DatasetRef {
            id: "model".into(),
            version: "1".into(),
        },
        documents: processes,
        method_refs: vec![DatasetRef {
            id: "method".into(),
            version: "1".into(),
        }],
        cutoffs: vec![],
        overrides: vec![],
        study_binding: None,
        quality_findings: vec![],
    }
}
fn prepare_source(source: &SourceInput) -> PreparedInput {
    prepare(
        &serde_json::to_vec(source).unwrap(),
        &methods(),
        &Bounds::default(),
    )
    .unwrap()
}
fn single() -> SourceInput {
    input(
        vec![process(
            "process",
            vec![
                raw_exchange("p", "product", "Output", 2.0),
                raw_exchange("e", "emission", "Output", 10.0),
            ],
        )],
        model(json!([instance("a", "process", json!([]))])),
        &["product", "emission"],
    )
}
#[test]
fn source_to_core_and_frozen_replay() {
    let p = prepare_source(&single());
    let r = execute(&p).unwrap();
    assert_eq!(r.x, vec![1.0]);
    assert_eq!(r.g, vec![5.0]);
    assert_eq!(r.h, vec![10.0]);
    let bytes = artifact_bytes(&p).unwrap();
    let frozen = decode_prepared(&bytes, &sha256(&bytes), &Bounds::default()).unwrap();
    assert_eq!(execute(&frozen).unwrap().h, r.h);
    assert!(decode_prepared(&bytes, &"0".repeat(64), &Bounds::default()).is_err());
}
#[test]
fn cycle_is_simultaneous_and_same_source_instances_remain_distinct() {
    let pa = process(
        "pa",
        vec![
            raw_exchange("p", "product-a", "Output", 1.0),
            raw_exchange("i", "product-b", "Input", 0.2),
            raw_exchange("e", "emission", "Output", 2.0),
        ],
    );
    let pb = process(
        "pb",
        vec![
            raw_exchange("p", "product-b", "Output", 1.0),
            raw_exchange("i", "product-a", "Input", 0.1),
            raw_exchange("e", "emission", "Output", 3.0),
        ],
    );
    let s = input(
        vec![pa, pb],
        model(json!([
            instance("a", "pa", connection("product-a", "b")),
            instance("b", "pb", connection("product-b", "a"))
        ])),
        &["product-a", "product-b", "emission"],
    );
    let r = execute(&prepare_source(&s)).unwrap();
    assert!((r.x[0] - 50.0 / 49.0).abs() < 1e-12);
    assert!((r.x[1] - 10.0 / 49.0).abs() < 1e-12);
    assert!((r.g[0] - 130.0 / 49.0).abs() < 1e-12);
    let mut s = single();
    s.documents.retain(|d| d.kind != "lifecyclemodel");
    s.documents.push(model(json!([
        instance("a", "process", json!([])),
        instance("b", "process", json!([]))
    ])));
    let p = prepare_source(&s);
    assert_eq!(p.numeric.views.len(), 2);
    assert_eq!(execute(&p).unwrap().x, vec![1.0, 0.0]);
}
#[test]
fn self_reuse_and_input_reference() {
    let p = process(
        "process",
        vec![
            raw_exchange("p", "product", "Output", 1.0),
            raw_exchange("i", "product", "Input", 0.2),
            raw_exchange("e", "emission", "Output", 4.0),
        ],
    );
    let s = input(
        vec![p],
        model(json!([instance(
            "a",
            "process",
            connection("product", "a")
        )])),
        &["product", "emission"],
    );
    let r = execute(&prepare_source(&s)).unwrap();
    assert_eq!(r.x, vec![1.25]);
    assert_eq!(r.g, vec![5.0]);
    let mut s = single();
    let index = s
        .documents
        .iter()
        .position(|d| d.kind == "process")
        .unwrap();
    s.documents[index] = process(
        "process",
        vec![
            raw_exchange("p", "product", "Input", 2.0),
            raw_exchange("e", "emission", "Output", 10.0),
        ],
    );
    let p = prepare_source(&s);
    assert_eq!(p.numeric.reference.pivot, -1.0);
    assert_eq!(execute(&p).unwrap().g, vec![5.0]);
}
#[test]
fn bounds_hashes_overrides_and_strict_parsing() {
    let source = single();
    let bytes = serde_json::to_vec(&source).unwrap();
    let mut bound = Bounds::default();
    bound.max_source_bytes = 1;
    assert!(prepare(&bytes, &methods(), &bound).is_err());
    bound = Bounds::default();
    bound.max_documents = 1;
    assert!(prepare(&bytes, &methods(), &bound).is_err());
    bound = Bounds::default();
    bound.max_dense_cells = 1;
    assert!(prepare(&bytes, &methods(), &bound).is_err());
    let first = prepare_source(&source);
    let mut changed = source.clone();
    changed.documents.reverse();
    assert_eq!(prepare_source(&changed).model_hash, first.model_hash);
    changed.overrides.push(AmountOverride {
        path: "/instances/a/exchanges/e/amount".into(),
        value: 0.0,
    });
    let second = prepare_source(&changed);
    assert_ne!(second.model_hash, first.model_hash);
    assert_eq!(execute(&second).unwrap().h, vec![0.0]);
    changed.overrides[0].path = "/exchanges/e/amount".into();
    assert!(
        prepare(
            &serde_json::to_vec(&changed).unwrap(),
            &methods(),
            &Bounds::default()
        )
        .is_err()
    );
    let mut unknown = serde_json::to_value(source).unwrap();
    unknown["runtimeEnabled"] = json!(true);
    assert!(serde_json::from_value::<SourceInput>(unknown).is_err());
}
#[test]
fn missing_factor_is_not_known_zero() {
    let mut m = methods();
    m.factors[0].clear();
    let p = prepare(
        &serde_json::to_vec(&single()).unwrap(),
        &m,
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(p.coverage.len(), 1);
    assert!(!execute(&p).unwrap().complete_lcia_scores);
    m.factors[0].push(Factor {
        flow: DatasetRef {
            id: "emission".into(),
            version: "1".into(),
        },
        direction: "Output".into(),
        value: 0.0,
    });
    let p = prepare(
        &serde_json::to_vec(&single()).unwrap(),
        &m,
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(p.coverage.len(), 0);
    assert_eq!(execute(&p).unwrap().h, vec![0.0]);
}

fn allocation(p: f64, q: f64) -> Value {
    json!({"allocation":[{"@internalReferenceToCoProduct":"p","@allocatedFraction":p},{"@internalReferenceToCoProduct":"q","@allocatedFraction":q}]})
}
#[test]
fn product_views_select_target_and_never_reapply_allocation_to_pivot() {
    let mut burden = raw_exchange("e", "emission", "Output", 10.0);
    burden["allocations"] = allocation(60.0, 40.0);
    let pa = process(
        "pa",
        vec![
            raw_exchange("p", "product-p", "Output", 2.0),
            raw_exchange("q", "product-q", "Output", 1.0),
            burden,
        ],
    );
    let pb = process(
        "pb",
        vec![
            raw_exchange("p", "final", "Output", 1.0),
            raw_exchange("i", "product-q", "Input", 1.0),
        ],
    );
    let mut s = input(
        vec![pa, pb],
        model(json!([
            instance("a", "pa", connection("product-q", "b")),
            instance("b", "pb", json!([]))
        ])),
        &["product-p", "product-q", "final", "emission"],
    );
    let p = prepare_source(&s);
    let r = execute(&p).unwrap();
    assert!((r.g[0] - 3.0).abs() < 1e-12);
    let doc = s
        .documents
        .iter_mut()
        .find(|d| d.kind == "lifecyclemodel")
        .unwrap();
    let mut raw: Value = serde_json::from_str(&doc.json).unwrap();
    raw["lifeCycleModelDataSet"]["lifeCycleModelInformation"]["quantitativeReference"]["referenceToReferenceProcess"] =
        json!("b");
    *doc = document("lifecyclemodel", "model", raw);
    let p = prepare_source(&s);
    let r = execute(&p).unwrap();
    assert!((r.g[0] - 4.0).abs() < 1e-12);
    assert_eq!(p.numeric.views.len(), 3);
}
#[test]
fn zero_allocations_negative_inventory_and_invalid_sources() {
    let mut emission = raw_exchange("e", "emission", "Output", 999.0);
    emission["allocations"] = allocation(0.0, 100.0);
    let mut s = input(
        vec![process(
            "process",
            vec![
                raw_exchange("p", "product", "Output", 1.0),
                raw_exchange("q", "q", "Output", 1.0),
                emission,
            ],
        )],
        model(json!([instance("a", "process", json!([]))])),
        &["product", "q", "emission"],
    );
    s.cutoffs.push(Cutoff {
        port: Port {
            instance_id: "a".into(),
            exchange_id: "q".into(),
        },
        reason: "Boundary coproduct".into(),
    });
    assert_eq!(execute(&prepare_source(&s)).unwrap().g, vec![0.0]);
    let mut s = single();
    let i = s
        .documents
        .iter()
        .position(|d| d.kind == "process")
        .unwrap();
    s.documents[i] = process(
        "process",
        vec![
            raw_exchange("p", "product", "Output", 2.0),
            raw_exchange("e", "emission", "Output", -2.0),
        ],
    );
    assert_eq!(execute(&prepare_source(&s)).unwrap().g, vec![-1.0]);
    s.documents[i].sha256 = "0".repeat(64);
    assert!(
        prepare(
            &serde_json::to_vec(&s).unwrap(),
            &methods(),
            &Bounds::default()
        )
        .is_err()
    );
    for consumption in [1.0, 2.0] {
        let s = input(
            vec![process(
                "process",
                vec![
                    raw_exchange("p", "product", "Output", 1.0),
                    raw_exchange("i", "product", "Input", consumption),
                    raw_exchange("e", "emission", "Output", 1.0),
                ],
            )],
            model(json!([instance(
                "a",
                "process",
                connection("product", "a")
            )])),
            &["product", "emission"],
        );
        assert!(execute(&prepare_source(&s)).is_err());
    }
    let mut s = single();
    let i = s
        .documents
        .iter()
        .position(|d| d.kind == "process")
        .unwrap();
    s.documents[i] = process(
        "process",
        vec![
            raw_exchange("p", "product", "Output", 0.0),
            raw_exchange("e", "emission", "Output", 1.0),
        ],
    );
    assert!(
        prepare(
            &serde_json::to_vec(&s).unwrap(),
            &methods(),
            &Bounds::default()
        )
        .is_err()
    );
}
#[test]
fn qualification_upper_instance_envelope_uses_actual_core() {
    let mut burden = raw_exchange("e", "emission", "Output", 2.0);
    burden["allocations"] = allocation(50.0, 50.0);
    let mut consumption = raw_exchange("i", "q", "Input", 0.1);
    consumption["allocations"] = allocation(50.0, 50.0);
    let p = process(
        "process",
        vec![
            raw_exchange("p", "product", "Output", 1.0),
            raw_exchange("q", "q", "Output", 1.0),
            burden,
            consumption,
        ],
    );
    let ids = (0..64)
        .map(|i| {
            if i == 0 {
                "a".into()
            } else {
                format!("node{i}")
            }
        })
        .collect::<Vec<String>>();
    let instances = ids
        .iter()
        .enumerate()
        .map(|(index, id)| instance(id, "process", connection("q", &ids[(index + 1) % 64])))
        .collect::<Vec<_>>();
    let s = input(
        vec![p],
        model(json!(instances)),
        &["product", "q", "emission"],
    );
    let p = prepare_source(&s);
    assert_eq!(p.numeric.views.len(), 128);
    let result = execute(&p).unwrap();
    assert!(result.max_residual < 1e-12);
    assert!(result.elapsed_millis < 30_000);
    eprintln!(
        "study bound qualification: instances=64 views=128 artifactBytes={} nativeElapsedMillis={} residual={}",
        artifact_bytes(&p).unwrap().len(),
        result.elapsed_millis,
        result.max_residual
    );
}
#[test]
fn semantic_digest_domain_golden_and_reordering() {
    let v = json!({"b":[1,2],"a":"é"});
    assert_eq!(
        semantic_hash("study.identity-golden.v1", &v).unwrap(),
        "6523e3c81404d0bec7aa5cd50d15f436bec821c6f14fc48701480af41771b564"
    );
    assert_ne!(
        semantic_hash("study.other.v1", &v).unwrap(),
        semantic_hash("study.identity-golden.v1", &v).unwrap()
    );
    let p = prepare_source(&single());
    if let Ok(path) = std::env::var("STUDY_PREPARED_OUTPUT") {
        assert!(std::path::Path::new(&path).is_absolute());
        std::fs::write(path, artifact_bytes(&p).unwrap()).unwrap();
    }
}

#[test]
fn repeated_source_instances_have_independent_nonzero_demands() {
    let pa = process(
        "supplier",
        vec![
            raw_exchange("p", "product", "Output", 1.0),
            raw_exchange("e", "emission", "Output", 3.0),
        ],
    );
    let pr = process(
        "root",
        vec![
            raw_exchange("p", "final", "Output", 1.0),
            raw_exchange("i1", "product", "Input", 1.0),
            raw_exchange("i2", "product", "Input", 2.0),
        ],
    );
    let edge = |exchange: &str| json!({"@flowUUID":"product","@version":"1","downstreamProcess":{"@id":"r","@flowUUID":"product","@version":"1","@exchangeId":exchange}});
    let mut m = model(json!([
        instance("a", "supplier", edge("i1")),
        instance("b", "supplier", edge("i2")),
        instance("r", "root", json!([]))
    ]));
    let mut raw: Value = serde_json::from_str(&m.json).unwrap();
    raw["lifeCycleModelDataSet"]["lifeCycleModelInformation"]["quantitativeReference"]["referenceToReferenceProcess"] =
        json!("r");
    m = document("lifecyclemodel", "model", raw);
    let s = input(vec![pa, pr], m, &["product", "final", "emission"]);
    let r = execute(&prepare_source(&s)).unwrap();
    assert_eq!(r.x, vec![1.0, 2.0, 1.0]);
    assert_eq!(r.g, vec![9.0]);
    assert_eq!(r.h, vec![18.0]);
}
#[test]
fn exact_units_versions_direction_and_unknown_overrides_fail_closed() {
    let source = single();
    for direction in ["Input", "Output"] {
        let mut m = methods();
        m.factors[0][0].direction = direction.into();
        m.factors[0][0].flow.version = "other".into();
        let p = prepare(
            &serde_json::to_vec(&source).unwrap(),
            &m,
            &Bounds::default(),
        )
        .unwrap();
        assert_eq!(p.coverage.len(), 1);
    }
    let mut bad = source.clone();
    let doc = bad
        .documents
        .iter_mut()
        .find(|d| d.kind == "unitgroup")
        .unwrap();
    let mut raw: Value = serde_json::from_str(&doc.json).unwrap();
    raw["unitGroupDataSet"]["units"]["unit"]["meanValue"] = json!("1000");
    *doc = document("unitgroup", "kg", raw);
    assert!(
        prepare(
            &serde_json::to_vec(&bad).unwrap(),
            &methods(),
            &Bounds::default()
        )
        .is_err()
    );
    let mut bad = source.clone();
    bad.documents
        .iter_mut()
        .find(|d| d.kind == "flow")
        .unwrap()
        .identity
        .version = "other".into();
    assert!(
        prepare(
            &serde_json::to_vec(&bad).unwrap(),
            &methods(),
            &Bounds::default()
        )
        .is_err()
    );
    for path in [
        "/parameters/energy/value",
        "/providers/a",
        "/instances/a/exchanges/p/amount",
    ] {
        let mut bad = source.clone();
        bad.overrides.push(AmountOverride {
            path: path.into(),
            value: 1.0,
        });
        assert!(
            prepare(
                &serde_json::to_vec(&bad).unwrap(),
                &methods(),
                &Bounds::default()
            )
            .is_err()
        );
    }
    let mut expanded = Bounds::default();
    expanded.max_views = 129;
    assert!(prepare(&serde_json::to_vec(&source).unwrap(), &methods(), &expanded).is_err());
}

#[test]
fn resolver_state_assertions_are_enforced_but_not_authorization_receipts() {
    for state in [20, 101, 120, 199] {
        let mut s = single();
        s.documents[0].state_code = state;
        assert!(
            prepare(
                &serde_json::to_vec(&s).unwrap(),
                &methods(),
                &Bounds::default()
            )
            .is_err()
        );
    }
    let mut s = single();
    s.documents[0].state_code = 0;
    s.documents[0].owner_actor_id = Some("owner".into());
    assert!(
        prepare(
            &serde_json::to_vec(&s).unwrap(),
            &methods(),
            &Bounds::default()
        )
        .is_err()
    );
    s.actor_id = Some("other".into());
    assert!(
        prepare(
            &serde_json::to_vec(&s).unwrap(),
            &methods(),
            &Bounds::default()
        )
        .is_err()
    );
    s.actor_id = Some("owner".into());
    let p = prepare_source(&s);
    assert!(
        !String::from_utf8(artifact_bytes(&p).unwrap())
            .unwrap()
            .contains("ownerActorId")
    );
}
#[test]
fn scenario_input_identity_binds_definition_and_explicit_override_provenance() {
    let mut s = single();
    s.study_binding = Some(StudyBinding {
        study_revision_id: "revision".into(),
        study_revision_content_hash: "a".repeat(64),
        definition: Definition {
            goal: "goal".into(),
            functional_unit: FunctionalUnit {
                amount: "2".into(),
                unit: "kg".into(),
                reference_flow_id: "product".into(),
            },
            system_boundary: "declared".into(),
            method_refs: vec![MethodRef {
                r#type: "datasetRevision".into(),
                dataset_type: "lciamethod".into(),
                id: "method".into(),
                version: "1".into(),
            }],
            assumptions: vec![],
        },
    });
    let first = prepare_source(&s);
    assert_eq!(execute(&first).unwrap().g, vec![10.0]);
    assert!(first.input_hash.is_some());
    s.documents.reverse();
    let reordered = prepare_source(&s);
    assert_eq!(reordered.model_hash, first.model_hash);
    assert_eq!(reordered.input_hash, first.input_hash);
    s.study_binding.as_mut().unwrap().definition.goal = "new goal".into();
    let second = prepare_source(&s);
    assert_eq!(second.model_hash, first.model_hash);
    assert_ne!(second.input_hash, first.input_hash);
    s.overrides.push(AmountOverride {
        path: "/instances/a/exchanges/e/amount".into(),
        value: 10.0,
    });
    let third = prepare_source(&s);
    assert_ne!(third.model_hash, second.model_hash);
    assert_eq!(third.numeric.b, second.numeric.b);
}

#[test]
fn tiny_finite_inventory_is_preserved_under_large_characterization() {
    let mut s = single();
    let doc = s
        .documents
        .iter_mut()
        .find(|d| d.kind == "process")
        .unwrap();
    *doc = process(
        "process",
        vec![
            raw_exchange("p", "product", "Output", 1.0),
            raw_exchange("e", "emission", "Output", 1e-16),
        ],
    );
    let mut m = methods();
    m.factors[0][0].value = 1e16;
    let p = prepare(&serde_json::to_vec(&s).unwrap(), &m, &Bounds::default()).unwrap();
    let r = execute(&p).unwrap();
    assert_eq!(r.g, vec![1e-16]);
    assert!((r.h[0] - 1.0).abs() < 1e-12);
}

#[test]
fn aggregate_overflow_is_rejected_during_preparation() {
    let mut s = single();
    let doc = s
        .documents
        .iter_mut()
        .find(|d| d.kind == "process")
        .unwrap();
    *doc = process(
        "process",
        vec![
            raw_exchange("p", "product", "Output", 1.0),
            raw_exchange("e1", "emission", "Output", 1e308),
            raw_exchange("e2", "emission", "Output", 1e308),
        ],
    );
    let error = prepare(
        &serde_json::to_vec(&s).unwrap(),
        &methods(),
        &Bounds::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("aggregate_overflow"));
}
#[test]
fn allocation_marker_strings_and_empty_declarations_cannot_enable_product_view() {
    for declaration in [
        json!({"allocation":{},"note":"@internalReferenceToCoProduct"}),
        json!({"allocation":{}}),
    ] {
        let mut q = raw_exchange("q", "q", "Output", 1.0);
        q["allocations"] = declaration;
        let pa = process(
            "pa",
            vec![
                raw_exchange("p", "product", "Output", 1.0),
                q,
                raw_exchange("e", "emission", "Output", 10.0),
            ],
        );
        let pb = process(
            "pb",
            vec![
                raw_exchange("p", "final", "Output", 1.0),
                raw_exchange("i", "q", "Input", 1.0),
            ],
        );
        let s = input(
            vec![pa, pb],
            model(json!([
                instance("a", "pa", connection("q", "b")),
                instance("b", "pb", json!([]))
            ])),
            &["product", "q", "final", "emission"],
        );
        assert!(
            prepare(
                &serde_json::to_vec(&s).unwrap(),
                &methods(),
                &Bounds::default()
            )
            .is_err()
        );
    }
}
#[test]
fn legacy_core_constructor_keeps_its_threshold() {
    use solver_core::{
        ModelSparseData, NumericOptions, SolveOptions, SolverService, SparseTriplet,
    };
    let data = ModelSparseData {
        model_version: uuid::Uuid::new_v4(),
        process_count: 1,
        flow_count: 1,
        impact_count: 1,
        technosphere_entries: vec![],
        biosphere_entries: vec![SparseTriplet {
            row: 0,
            col: 0,
            value: 1e-16,
        }],
        characterization_factors: vec![SparseTriplet {
            row: 0,
            col: 0,
            value: 1e16,
        }],
    };
    let old = SolverService::new();
    old.prepare(&data, NumericOptions::default()).unwrap();
    assert_eq!(
        old.solve_one(
            data.model_version,
            NumericOptions::default(),
            &[1.0],
            SolveOptions::default()
        )
        .unwrap()
        .h
        .unwrap(),
        vec![0.0]
    );
    let study = SolverService::with_exact_zero_policy(1024 * 1024, 8.0);
    study.prepare(&data, NumericOptions::default()).unwrap();
    assert_eq!(
        study
            .solve_one(
                data.model_version,
                NumericOptions::default(),
                &[1.0],
                SolveOptions::default()
            )
            .unwrap()
            .h
            .unwrap(),
        vec![1.0]
    );
}

#[test]
fn one_method_category_cannot_select_multiple_versions() {
    let mut s = single();
    s.method_refs.push(DatasetRef {
        id: "method".into(),
        version: "2".into(),
    });
    let mut m = methods();
    m.axes.push(MethodAxis {
        id: "method".into(),
        version: "2".into(),
        unit: "impact".into(),
    });
    m.factors.push(m.factors[0].clone());
    assert!(
        prepare(&serde_json::to_vec(&s).unwrap(), &m, &Bounds::default())
            .unwrap_err()
            .to_string()
            .contains("duplicate_method")
    );
}

#[test]
fn independent_tiny_opposite_sign_link_is_representable() {
    let s = input(
        vec![
            process(
                "pa",
                vec![
                    raw_exchange("p", "product-a", "Output", 1e-200),
                    raw_exchange("e", "emission", "Output", 1.0),
                ],
            ),
            process(
                "pb",
                vec![
                    raw_exchange("p", "product-b", "Output", 1.0),
                    raw_exchange("i", "product-a", "Input", 1e-200),
                ],
            ),
        ],
        model(json!([
            instance("a", "pb", json!([])),
            instance("b", "pa", connection("product-a", "a"))
        ])),
        &["product-a", "product-b", "emission"],
    );
    let p = prepare(
        &serde_json::to_vec(&s).unwrap(),
        &methods(),
        &Bounds::default(),
    );
    assert!(p.is_ok(), "valid link rejected: {:?}", p.err());
    assert!((execute(&p.unwrap()).unwrap().g[0] - 1.0).abs() < 1e-12);
}
#[test]
fn independent_allocation_normalization_does_not_erase_representable_inventory() {
    let mut burden = raw_exchange("e", "emission", "Output", f64::from_bits(1));
    burden["allocations"] = allocation(50.0, 50.0);
    let mut s = input(
        vec![process(
            "pa",
            vec![
                raw_exchange("p", "product-p", "Output", 1e-308),
                raw_exchange("q", "product-q", "Output", 1e-308),
                burden,
            ],
        )],
        model(json!([instance("a", "pa", json!([]))])),
        &["product-p", "product-q", "emission"],
    );
    s.cutoffs.push(Cutoff {
        port: Port {
            instance_id: "a".into(),
            exchange_id: "q".into(),
        },
        reason: "explicit boundary q".into(),
    });
    let mut m = methods();
    m.factors[0][0].value = 1e16;
    let p = prepare(&serde_json::to_vec(&s).unwrap(), &m, &Bounds::default()).unwrap();
    let r = execute(&p).unwrap();
    let expected = (f64::from_bits(1) / 1e-308) * 0.5 * 1e16;
    eprintln!("B={:?} h={:?} expected={expected}", p.numeric.b, r.h);
    assert!((r.h[0] - expected).abs() < 1e-12);
}
#[test]
fn independent_repeated_allocation_metadata_exceeds_owned_envelope() {
    let mut burden = raw_exchange("e", "emission", "Output", 10.0);
    burden["allocations"] = allocation(50.0, 50.0);
    burden["allocations"]["note"] = json!("x".repeat(32_768));
    let ids = (0..64)
        .map(|i| if i == 0 { "a".into() } else { format!("n{i}") })
        .collect::<Vec<String>>();
    let mut s = input(
        vec![process(
            "pa",
            vec![
                raw_exchange("p", "product-p", "Output", 1.0),
                raw_exchange("q", "product-q", "Output", 1.0),
                burden,
            ],
        )],
        model(json!(
            ids.iter()
                .map(|id| instance(id, "pa", json!([])))
                .collect::<Vec<_>>()
        )),
        &["product-p", "product-q", "emission"],
    );
    for id in &ids {
        s.cutoffs.push(Cutoff {
            port: Port {
                instance_id: id.clone(),
                exchange_id: "q".into(),
            },
            reason: "explicit boundary q".into(),
        });
    }
    let mut bounds = Bounds::default();
    bounds.max_owned_bytes = 1024 * 1024;
    let bytes = serde_json::to_vec(&s).unwrap();
    let error = prepare(&bytes, &methods(), &bounds).unwrap_err();
    assert!(
        error.to_string().contains("source_expansion_memory_limit"),
        "unexpected preflight error: {error}"
    );
}

#[test]
fn normalization_preserves_finite_extremes_and_rejects_final_range_loss() {
    use super::compile::normalized_amount;
    let value = normalized_amount(1e308, 1e-308, 1e-308).unwrap();
    assert!((value / 1e308 - 1.0).abs() < 1e-15);
    assert_eq!(
        normalized_amount(f64::MIN_POSITIVE, f64::from_bits(1), f64::MIN_POSITIVE).unwrap(),
        f64::from_bits(1)
    );
    assert!(
        normalized_amount(f64::from_bits(1), 0.5, 1.0)
            .unwrap_err()
            .to_string()
            .contains("underflow")
    );
    assert!(
        normalized_amount(f64::MAX, 1.0, f64::MIN_POSITIVE)
            .unwrap_err()
            .to_string()
            .contains("overflow")
    );
    assert_eq!(normalized_amount(f64::from_bits(1), 0.0, 1.0).unwrap(), 0.0);
}

#[test]
fn scenario_reuses_frozen_factors_and_distinguishes_frozen_zero_from_gap() {
    let source = single();
    let original = prepare_source(&source);
    let frozen = VerifiedMethods::from_frozen(&original, &source.method_refs).unwrap();
    let mut next = source.clone();
    next.overrides.push(AmountOverride {
        path: "/instances/a/exchanges/e/amount".into(),
        value: 2.0,
    });
    let effective = prepare(
        &serde_json::to_vec(&next).unwrap(),
        &frozen,
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(execute(&effective).unwrap().h, vec![2.0]);
    assert_eq!(effective.method_source_hash, original.method_source_hash);
    let mut zero_methods = methods();
    zero_methods.factors[0][0].value = 0.0;
    let known_zero = prepare(
        &serde_json::to_vec(&source).unwrap(),
        &zero_methods,
        &Bounds::default(),
    )
    .unwrap();
    let frozen_zero = VerifiedMethods::from_frozen(&known_zero, &source.method_refs).unwrap();
    assert_eq!(
        prepare(
            &serde_json::to_vec(&next).unwrap(),
            &frozen_zero,
            &Bounds::default()
        )
        .unwrap()
        .coverage
        .len(),
        0
    );
    zero_methods.factors[0].clear();
    let gap = prepare(
        &serde_json::to_vec(&source).unwrap(),
        &zero_methods,
        &Bounds::default(),
    )
    .unwrap();
    let frozen_gap = VerifiedMethods::from_frozen(&gap, &source.method_refs).unwrap();
    assert_eq!(
        prepare(
            &serde_json::to_vec(&next).unwrap(),
            &frozen_gap,
            &Bounds::default()
        )
        .unwrap()
        .coverage
        .len(),
        1
    );
}

#[test]
fn decimal_normalized_inventory_keeps_identity_after_json_replay() {
    let mut source = single();
    let doc = source
        .documents
        .iter_mut()
        .find(|doc| doc.kind == "process")
        .unwrap();
    *doc = process(
        "process",
        vec![
            raw_exchange("p", "product", "Output", 1000.0),
            raw_exchange("e", "emission", "Output", 0.4662),
        ],
    );
    let prepared = prepare_source(&source);
    let bytes = artifact_bytes(&prepared).unwrap();
    let decoded: PreparedInput = super::json::from_slice(&bytes).unwrap();
    for (expected, actual) in prepared
        .numeric
        .b
        .iter()
        .flatten()
        .zip(decoded.numeric.b.iter().flatten())
    {
        assert_eq!(
            expected.to_bits(),
            actual.to_bits(),
            "expected={expected:?} actual={actual:?}"
        );
    }
    decode_prepared(&bytes, &sha256(&bytes), &Bounds::default()).unwrap();
}

#[test]
fn source_decimal_strings_keep_model_identity_after_json_replay() {
    for value in [
        "357.48", "1092.8", "1.602", "0.4662", "0.64", "1.31", "0.74",
    ] {
        let mut source = single();
        let doc = source
            .documents
            .iter_mut()
            .find(|doc| doc.kind == "process")
            .unwrap();
        let mut e = raw_exchange("e", "emission", "Output", 0.0);
        e["resultingAmount"] = json!(value);
        *doc = process(
            "process",
            vec![raw_exchange("p", "product", "Output", 1000.0), e],
        );
        let prepared = prepare_source(&source);
        let bytes = artifact_bytes(&prepared).unwrap();
        let decoded: PreparedInput = super::json::from_slice(&bytes).unwrap();
        for (expected, actual) in prepared.model.instances[0]
            .exchanges
            .iter()
            .zip(&decoded.model.instances[0].exchanges)
        {
            assert_eq!(
                expected.amount.to_bits(),
                actual.amount.to_bits(),
                "value={value}, expected={:?} actual={:?}",
                expected.amount,
                actual.amount
            );
        }
        for (expected, actual) in prepared
            .numeric
            .b
            .iter()
            .flatten()
            .zip(decoded.numeric.b.iter().flatten())
        {
            assert_eq!(
                expected.to_bits(),
                actual.to_bits(),
                "value={value}, expected={expected:?} actual={actual:?}"
            );
        }
        decode_prepared(&bytes, &sha256(&bytes), &Bounds::default()).unwrap();
    }
}

// Shared only by the analysis tests: actual compiler output, never a service receipt.
#[allow(clippy::too_many_lines)]
pub(super) fn analysis_prepared(
    case: &str,
    change: Option<f64>,
    factor: Option<f64>,
) -> PreparedInput {
    fn wire_versions(value: &mut Value) {
        match value {
            Value::Object(values) => {
                for (key, value) in values {
                    if (key == "@version" || key == "common:dataSetVersion") && value == "1" {
                        *value = json!("01.00.000");
                    } else {
                        wire_versions(value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    wire_versions(value);
                }
            }
            _ => {}
        }
    }
    let mut s = match case {
        "cycle" => input(
            vec![
                process(
                    "pa",
                    vec![
                        raw_exchange("p", "product-a", "Output", 1.0),
                        raw_exchange("i", "product-b", "Input", 0.2),
                        raw_exchange("e", "emission", "Output", 2.0),
                    ],
                ),
                process(
                    "pb",
                    vec![
                        raw_exchange("p", "product-b", "Output", 1.0),
                        raw_exchange("i", "product-a", "Input", 0.1),
                        raw_exchange("e", "emission", "Output", 3.0),
                    ],
                ),
            ],
            model(json!([
                instance("a", "pa", connection("product-a", "b")),
                instance("b", "pb", connection("product-b", "a"))
            ])),
            &["product-a", "product-b", "emission"],
        ),
        "allocation" => {
            let mut e = raw_exchange("e", "emission", "Output", 10.0);
            e["allocations"] = allocation(60.0, 40.0);
            input(
                vec![
                    process(
                        "pa",
                        vec![
                            raw_exchange("p", "product-p", "Output", 2.0),
                            raw_exchange("q", "product-q", "Output", 1.0),
                            e,
                        ],
                    ),
                    process(
                        "pb",
                        vec![
                            raw_exchange("p", "final", "Output", 1.0),
                            raw_exchange("i", "product-q", "Input", 1.0),
                        ],
                    ),
                ],
                model(json!([
                    instance("a", "pa", connection("product-q", "b")),
                    instance("b", "pb", json!([]))
                ])),
                &["product-p", "product-q", "final", "emission"],
            )
        }
        "repeated" => {
            let edge = |exchange: &str| json!({"@flowUUID":"product","@version":"1","downstreamProcess":{"@id":"r","@flowUUID":"product","@version":"1","@exchangeId":exchange}});
            let m = model(json!([
                instance("a", "supplier", edge("i1")),
                instance("b", "supplier", edge("i2")),
                instance("r", "root", json!([]))
            ]));
            let mut raw: Value = serde_json::from_str(&m.json).unwrap();
            raw["lifeCycleModelDataSet"]["lifeCycleModelInformation"]["quantitativeReference"]["referenceToReferenceProcess"] =
                json!("r");
            input(
                vec![
                    process(
                        "supplier",
                        vec![
                            raw_exchange("p", "product", "Output", 1.0),
                            raw_exchange("e", "emission", "Output", 3.0),
                        ],
                    ),
                    process(
                        "root",
                        vec![
                            raw_exchange("p", "final", "Output", 1.0),
                            raw_exchange("i1", "product", "Input", 1.0),
                            raw_exchange("i2", "product", "Input", 2.0),
                        ],
                    ),
                ],
                document("lifecyclemodel", "model", raw),
                &["product", "final", "emission"],
            )
        }
        "input" => input(
            vec![process(
                "process",
                vec![
                    raw_exchange("p", "product", "Input", 2.0),
                    raw_exchange("e", "emission", "Output", 10.0),
                ],
            )],
            model(json!([instance("a", "process", json!([]))])),
            &["product", "emission"],
        ),
        "envelope" => {
            let mut burden = raw_exchange("e", "emission", "Output", 2.0);
            burden["allocations"] = allocation(50.0, 50.0);
            let mut consumption = raw_exchange("i", "q", "Input", 0.1);
            consumption["allocations"] = allocation(50.0, 50.0);
            let p = process(
                "process",
                vec![
                    raw_exchange("p", "product", "Output", 1.0),
                    raw_exchange("q", "q", "Output", 1.0),
                    burden,
                    consumption,
                ],
            );
            let ids = (0..64)
                .map(|i| {
                    if i == 0 {
                        "a".to_owned()
                    } else {
                        format!("node{i}")
                    }
                })
                .collect::<Vec<_>>();
            let instances = ids
                .iter()
                .enumerate()
                .map(|(i, id)| instance(id, "process", connection("q", &ids[(i + 1) % 64])))
                .collect::<Vec<_>>();
            input(
                vec![p],
                model(json!(instances)),
                &["product", "q", "emission"],
            )
        }
        "partial" => input(
            vec![process(
                "process",
                vec![
                    raw_exchange("p", "product", "Output", 2.0),
                    raw_exchange("e", "emission", "Output", 10.0),
                    raw_exchange("e2", "emission", "Input", 4.0),
                ],
            )],
            model(json!([instance("a", "process", json!([]))])),
            &["product", "emission"],
        ),
        "tiny" => input(
            vec![process(
                "process",
                vec![
                    raw_exchange("p", "product", "Output", 1.0),
                    raw_exchange("e", "emission", "Output", f64::from_bits(1)),
                ],
            )],
            model(json!([instance("a", "process", json!([]))])),
            &["product", "emission"],
        ),
        _ => single(),
    };
    let mut m = methods();
    // Give exported W08 consumer fixtures real P1 version syntax before either
    // compiler or executor sees them. Never rewrite a produced artifact/hash.
    for document in &mut s.documents {
        document.identity.version = "01.00.000".into();
        let mut raw: Value = serde_json::from_str(&document.json).unwrap();
        wire_versions(&mut raw);
        document.json = serde_json::to_string(&raw).unwrap();
        document.sha256 = sha256(document.json.as_bytes());
    }
    s.origin.version = "01.00.000".into();
    for reference in &mut s.method_refs {
        reference.version = "01.00.000".into();
    }
    m.axes[0].version = "01.00.000".into();
    m.factors[0][0].flow.version = "01.00.000".into();
    m.source_hash = sha256(b"synthetic analysis method fixture; not a published dataset");
    if let Some(factor) = factor {
        m.factors[0][0].value = factor;
    } else {
        m.factors[0].clear();
    }
    let base = prepare(&serde_json::to_vec(&s).unwrap(), &m, &Bounds::default()).unwrap();
    s.study_binding = Some(StudyBinding {
        study_revision_id: "10000000-0000-4000-8000-000000000002".into(),
        study_revision_content_hash: "d".repeat(64),
        definition: Definition {
            goal: "Bounded numerical fixture".into(),
            functional_unit: FunctionalUnit {
                amount: "1".into(),
                unit: base.numeric.reference.unit.clone(),
                reference_flow_id: base.numeric.reference.flow.id.clone(),
            },
            system_boundary: "explicit".into(),
            method_refs: vec![MethodRef {
                r#type: "datasetRevision".into(),
                dataset_type: "lciamethod".into(),
                id: "method".into(),
                version: "01.00.000".into(),
            }],
            assumptions: vec![],
        },
    });
    if let Some(value) = change {
        s.overrides.push(AmountOverride {
            path: "/instances/a/exchanges/e/amount".into(),
            value,
        });
    }
    prepare(&serde_json::to_vec(&s).unwrap(), &m, &Bounds::default()).unwrap()
}

// Fresh synthetic source for the independent method-assessment verifier. This
// does not rewrite any produced fixture, Run artifact or existing hash domain.
pub(super) fn method_assessment_source_fixture() -> SourceInput {
    fn versions(value: &mut Value) {
        match value {
            Value::Object(values) => {
                for (key, value) in values {
                    if (key == "@version" || key == "common:dataSetVersion") && value == "1" {
                        *value = json!("01.00.000");
                    } else {
                        versions(value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    versions(value);
                }
            }
            _ => {}
        }
    }
    let mut source = single();
    for doc in &mut source.documents {
        doc.identity.version = "01.00.000".into();
        let mut raw: Value = serde_json::from_str(&doc.json).unwrap();
        versions(&mut raw);
        doc.json = serde_json::to_string(&raw).unwrap();
        doc.sha256 = sha256(doc.json.as_bytes());
    }
    source.origin.version = "01.00.000".into();
    source.method_refs = vec![DatasetRef {
        id: "6209b35f-9447-40b5-b68c-a1099e3674a0".into(),
        version: "01.00.000".into(),
    }];
    source
}
