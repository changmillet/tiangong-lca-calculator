use super::*;

const VERSION: &str = "01.00.000";
const WORKER_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn record(kind: CompiledReleaseSourceDatasetType, number: u128, mut document: Value) -> Document {
    let (root, information) = kind.document_identity_keys();
    document[root][information]["dataSetInformation"]["common:UUID"] = json!(id(number));
    document[root]["administrativeInformation"]["publicationAndOwnership"]["common:dataSetVersion"] =
        json!(VERSION);
    Document {
        identity: Identity {
            dataset_type: kind,
            id: id(number),
            version: VERSION.to_owned(),
        },
        document_sha256: stable_json_sha256(&document).unwrap(),
        document,
        user_id: Some(id(999)),
        state_code: 0,
        model_id: None,
        model_version: None,
        team_id: None,
        review_id: None,
    }
}

fn flow(number: u128, property: u128, kind: &str) -> Document {
    record(
        CompiledReleaseSourceDatasetType::Flow,
        number,
        json!({"flowDataSet": {
            "flowInformation": {"quantitativeReference": {"referenceToReferenceFlowProperty": "7"}},
            "modellingAndValidation": {"LCIMethod": {"typeOfDataSet": kind}},
            "flowProperties": {"flowProperty": [{"@dataSetInternalID": "7", "meanValue": 1,
                "referenceToFlowPropertyDataSet": {"@type": "flow property data set", "@refObjectId": id(property), "@version": VERSION}}]}
        }}),
    )
}

fn property(number: u128, unit_group: u128) -> Document {
    record(
        CompiledReleaseSourceDatasetType::FlowProperty,
        number,
        json!({"flowPropertyDataSet": {
            "flowPropertiesInformation": {"quantitativeReference": {"referenceToReferenceUnitGroup": {
                "@type": "unit group data set", "@refObjectId": id(unit_group), "@version": VERSION}}}
        }}),
    )
}

fn units(number: u128, name: &str) -> Document {
    record(
        CompiledReleaseSourceDatasetType::UnitGroup,
        number,
        json!({"unitGroupDataSet": {
            "unitGroupInformation": {"quantitativeReference": {"referenceToReferenceUnit": "0"}},
            "units": {"unit": [{"@dataSetInternalID": "0", "name": name, "meanValue": 1}]}
        }}),
    )
}

fn process(number: u128, output: u128, input: Option<u128>) -> Document {
    let mut exchanges = vec![
        json!({"@dataSetInternalID": "1", "exchangeDirection": "Output", "meanAmount": 1,
        "referenceToFlowDataSet": {"@type": "flow data set", "@refObjectId": id(output), "@version": VERSION}}),
    ];
    if let Some(flow) = input {
        exchanges.push(json!({"@dataSetInternalID": "2", "exchangeDirection": "Input", "meanAmount": 2,
        "referenceToFlowDataSet": {"@type": "flow data set", "@refObjectId": id(flow), "@version": VERSION}}));
    }
    record(
        CompiledReleaseSourceDatasetType::Process,
        number,
        json!({"processDataSet": {
            "processInformation": {"quantitativeReference": {"referenceToReferenceFlow": "1"}},
            "exchanges": {"exchange": exchanges}
        }}),
    )
}

fn fixture() -> Value {
    // Two peer producers and two consumers, one disconnected from the requested root.
    let documents = vec![
        process(1, 101, None),
        process(2, 101, None),
        process(3, 102, Some(101)),
        process(4, 103, Some(101)),
        flow(101, 201, "Product flow"),
        flow(102, 201, "Product flow"),
        flow(103, 201, "Product flow"),
        property(201, 301),
        units(301, "kg"),
        property(202, 302),
        units(302, "m3"),
    ];
    let baseline = Baseline {
        actor_user_id: id(999),
        scope_manifest: expected_scope_manifest(id(999)),
        effective_processes: (1..=4)
            .map(|n| RequestRootProcess::new(id(n), VERSION))
            .collect(),
        request_roots: vec![RequestRootProcess::new(id(3), VERSION)],
        omitted_flow_resolutions: Vec::new(),
        documents,
        models: Vec::new(),
        native_policy: native_policy(),
    };
    let mut value = json!({"schema_version": INPUT_SCHEMA, "mode": "matrix_only", "expected_worker_sha256": WORKER_HASH,
        "baseline_sha256": "", "baseline": baseline, "overlays": []});
    rehash(&mut value);
    value
}

fn rehash(value: &mut Value) {
    for doc in value["baseline"]["documents"].as_array_mut().unwrap() {
        doc["document_sha256"] = json!(stable_json_sha256(&doc["document"]).unwrap());
    }
    for model in value["baseline"]["models"].as_array_mut().unwrap() {
        model["document_sha256"] = json!(stable_json_sha256(&model["document"]).unwrap());
    }
    value["baseline_sha256"] = json!(stable_json_sha256(&value["baseline"]).unwrap());
}

fn add_overlay(value: &mut Value, number: u128, change: impl FnOnce(&mut Value)) {
    let original = value["baseline"]["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["identity"]["id"] == json!(id(number)))
        .unwrap()
        .clone();
    let mut candidate = original["document"].clone();
    change(&mut candidate);
    value["overlays"].as_array_mut().unwrap().push(
        json!({"identity": original["identity"], "before_sha256": original["document_sha256"],
        "candidate_sha256": stable_json_sha256(&candidate).unwrap(), "document": candidate}),
    );
}

async fn run_value(value: Value) -> anyhow::Result<Value> {
    let hash = stable_json_sha256(&value)?;
    evaluate(value, &hash, WORKER_HASH).await
}

#[tokio::test]
async fn shared_provider_change_recompiles_all_consumers_and_is_reproducible() {
    let mut input = fixture();
    add_overlay(&mut input, 1, |body| {
        body["processDataSet"]["exchanges"]["exchange"][0]["meanAmount"] = json!(-1);
    });
    let report = run_value(input.clone()).await.unwrap();
    assert_eq!(report, run_value(input).await.unwrap());
    let affected = report["affected_processes"].as_array().unwrap();
    assert!(affected.iter().any(|p| p["process_id"] == json!(id(3))));
    assert!(affected.iter().any(|p| p["process_id"] == json!(id(4))));
    assert_eq!(
        report["before"]["provider_decisions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_ne!(
        report["before"]["provider_decisions"],
        report["after"]["provider_decisions"]
    );
    assert_eq!(report["operation_counts"]["factorizations"], 0);
    assert_eq!(report["operation_counts"]["unit_solves"], 0);
    assert_eq!(report["operation_counts"]["database_queries"], 0);
    assert_eq!(report["scope"]["online_census_verified"], false);
    assert_eq!(report["scope"]["save_admitted"], false);
    assert_eq!(
        report["before"]["readiness"]["compute_stability"]["factorization_checked"],
        false
    );
    // The native fallback is evidence, never an invented annual production observation.
    assert!(
        report["before"]["provider_decisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["volume_fallback_to_one_count"].as_i64().unwrap() > 0)
    );
}

#[tokio::test]
async fn full_flow_body_changes_unit_chain_and_native_source_evidence() {
    let mut input = fixture();
    add_overlay(&mut input, 101, |body| {
        body["flowDataSet"]["flowProperties"]["flowProperty"][0]["referenceToFlowPropertyDataSet"]
            ["@refObjectId"] = json!(id(202));
    });
    let report = run_value(input).await.unwrap();
    let units = |phase: &str| {
        report[phase]["release_evidence"]["inventory_exchanges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["flow_id"] == json!(id(101)))
            .map(|e| e["unit"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(units("before"), BTreeSet::from(["kg"]));
    assert_eq!(units("after"), BTreeSet::from(["m3"]));
    assert_eq!(report["affected_processes"].as_array().unwrap().len(), 4);
    let after_sources = report["after"]["release_evidence"]["source_datasets"]
        .as_array()
        .unwrap();
    let flow = after_sources
        .iter()
        .find(|d| d["dataset_id"] == json!(id(101)))
        .unwrap();
    assert_eq!(
        flow["document_sha256"],
        report["overlays"][0]["candidate_sha256"]
    );
}

#[tokio::test]
async fn unchanged_overlay_has_no_impact_and_does_not_solve_singular_finite_matrix() {
    let mut input = fixture();
    // The only two Processes consume exactly each other's reference output:
    // A = [[0, 1], [1, 0]], so det(I - A) = 0.
    let mut first = process(1, 101, Some(102));
    let mut second = process(2, 102, Some(101));
    for record in [&mut first, &mut second] {
        record.document["processDataSet"]["exchanges"]["exchange"][1]["meanAmount"] = json!(1);
    }
    input["baseline"]["documents"] = json!([
        first,
        second,
        flow(101, 201, "Product flow"),
        flow(102, 201, "Product flow"),
        property(201, 301),
        units(301, "kg")
    ]);
    input["baseline"]["effective_processes"] = json!([
        RequestRootProcess::new(id(1), VERSION),
        RequestRootProcess::new(id(2), VERSION)
    ]);
    input["baseline"]["request_roots"] = json!([RequestRootProcess::new(id(1), VERSION)]);
    rehash(&mut input);
    add_overlay(&mut input, 1, |_| {});
    let report = run_value(input).await.unwrap();
    assert_eq!(report["affected_processes"], json!([]));
    assert_eq!(report["before"]["matrix"], report["after"]["matrix"]);
    assert_eq!(
        report["after"]["matrix"]["technosphere_entries"],
        json!([
            {"row": 0, "col": 1, "value": 1.0}, {"row": 1, "col": 0, "value": 1.0}
        ])
    );
    assert_eq!(
        report["after"]["readiness"]["compute_stability"]["sampled_unit_solves"],
        0
    );
}

#[tokio::test]
async fn changed_provider_includes_transitive_downstream_processes() {
    let mut input = fixture();
    input["baseline"]["documents"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!(process(5, 104, Some(102))),
            json!(flow(104, 201, "Product flow")),
        ]);
    input["baseline"]["effective_processes"]
        .as_array_mut()
        .unwrap()
        .push(json!(RequestRootProcess::new(id(5), VERSION)));
    rehash(&mut input);
    add_overlay(&mut input, 1, |body| {
        body["processDataSet"]["exchanges"]["exchange"][0]["meanAmount"] = json!(-1);
    });
    let report = run_value(input).await.unwrap();
    assert!(
        report["affected_processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["process_id"] == json!(id(5)))
    );
}

#[tokio::test]
async fn missing_support_and_missing_census_member_fail_without_a_database() {
    for omitted in [101, 201, 301, 4] {
        let mut input = fixture();
        input["baseline"]["documents"]
            .as_array_mut()
            .unwrap()
            .retain(|d| d["identity"]["id"] != json!(id(omitted)));
        rehash(&mut input);
        assert!(run_value(input).await.is_err(), "missing {omitted}");
    }
}

#[tokio::test]
async fn guards_reject_drift_scope_expansion_duplicate_and_forbidden_overlays() {
    let mut cases = Vec::new();
    let mut value = fixture();
    value["baseline"]["documents"][0]["document"]["processDataSet"]["exchanges"]["exchange"][0]["meanAmount"] =
        json!(3);
    cases.push(value);
    let mut value = fixture();
    value["expected_worker_sha256"] = json!("wrong");
    cases.push(value);
    let mut value = fixture();
    value["mode"] = json!("solve");
    cases.push(value);
    let mut value = fixture();
    value["baseline"]["native_policy"]["provider_rule"] = json!("split_equal");
    rehash(&mut value);
    cases.push(value);
    let mut value = fixture();
    value["baseline"]["documents"][0]["state_code"] = json!(101);
    rehash(&mut value);
    cases.push(value);
    let mut value = fixture();
    let duplicate = value["baseline"]["documents"][0].clone();
    value["baseline"]["documents"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    rehash(&mut value);
    cases.push(value);
    let mut value = fixture();
    add_overlay(&mut value, 1, |_| {});
    value["overlays"][0]["before_sha256"] = json!("wrong");
    cases.push(value);
    let mut value = fixture();
    add_overlay(&mut value, 1, |body| {
        body["processDataSet"]["administrativeInformation"]["publicationAndOwnership"]["common:dataSetVersion"] =
            json!("01.00.001");
    });
    cases.push(value);
    let mut value = fixture();
    add_overlay(&mut value, 101, |body| {
        body["flowDataSet"]["modellingAndValidation"]["LCIMethod"]["typeOfDataSet"] =
            json!("Elementary flow");
    });
    cases.push(value);
    let mut value = fixture();
    add_overlay(&mut value, 201, |_| {});
    cases.push(value);
    let mut value = fixture();
    add_overlay(&mut value, 1, |_| {});
    let duplicate = value["overlays"][0].clone();
    value["overlays"].as_array_mut().unwrap().push(duplicate);
    cases.push(value);
    let mut value = fixture();
    value["baseline"]["documents"][0]["state_code"] = json!(100);
    rehash(&mut value);
    add_overlay(&mut value, 1, |_| {});
    cases.push(value);
    for (i, value) in cases.into_iter().enumerate() {
        assert!(run_value(value).await.is_err(), "guard case {i}");
    }
}

#[tokio::test]
async fn explicit_flow_versions_coexist_but_unbound_omitted_references_reject() {
    let mut input = fixture();
    let mut flow = input["baseline"]["documents"][4].clone();
    flow["identity"]["version"] = json!("01.00.001");
    flow["document"]["flowDataSet"]["administrativeInformation"]["publicationAndOwnership"]["common:dataSetVersion"] =
        json!("01.00.001");
    input["baseline"]["documents"]
        .as_array_mut()
        .unwrap()
        .push(flow);
    input["baseline"]["documents"][3]["document"]["processDataSet"]["exchanges"]["exchange"][1]["referenceToFlowDataSet"]
        ["@version"] = json!("01.00.001");
    rehash(&mut input);
    let report = run_value(input.clone()).await.unwrap();
    assert_eq!(
        report["before"]["flow_axis"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["flow_id"] == json!(id(101)))
            .count(),
        2
    );
    let mut overlaid = input.clone();
    add_overlay(&mut overlaid, 101, |body| {
        body["flowDataSet"]["flowProperties"]["flowProperty"][0]["referenceToFlowPropertyDataSet"]
            ["@refObjectId"] = json!(id(202));
    });
    let changed = run_value(overlaid).await.unwrap();
    assert!(
        !changed["affected_processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["process_id"] == json!(id(4)))
    );
    input["baseline"]["documents"][3]["document"]["processDataSet"]["exchanges"]["exchange"][1]["referenceToFlowDataSet"].as_object_mut().unwrap().remove("@version");
    rehash(&mut input);
    assert!(run_value(input.clone()).await.is_err());
    input["baseline"]["omitted_flow_resolutions"] =
        json!([{"id": id(101), "version": "01.00.001"}]);
    rehash(&mut input);
    assert!(run_value(input).await.is_ok());
}

#[tokio::test]
async fn process_axis_is_exact_and_unselected_historical_revision_is_not_compiled() {
    let mut input = fixture();
    let mut historical = json!(process(1, 9999, None));
    historical["identity"]["version"] = json!("00.00.001");
    historical["document"]["processDataSet"]["administrativeInformation"]["publicationAndOwnership"]
        ["common:dataSetVersion"] = json!("00.00.001");
    input["baseline"]["documents"]
        .as_array_mut()
        .unwrap()
        .push(historical);
    rehash(&mut input);
    let report = run_value(input.clone()).await.unwrap();
    assert_eq!(
        report["before"]["effective_processes"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(
        !report["before"]["release_evidence"]["source_datasets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["dataset_version"] == "00.00.001")
    );
    input["baseline"]["effective_processes"]
        .as_array_mut()
        .unwrap()
        .push(json!(RequestRootProcess::new(id(1), "00.00.001")));
    rehash(&mut input);
    assert!(
        run_value(input)
            .await
            .unwrap_err()
            .to_string()
            .contains("alternative versions")
    );
}

#[tokio::test]
async fn file_entrypoint_binds_raw_input_and_binary_and_never_overwrites_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let input_path = directory.path().join("request.json");
    let output_path = directory.path().join("report.json");
    let mut input = fixture();
    input["expected_worker_sha256"] =
        json!(file_sha256(&std::env::current_exe().unwrap()).unwrap());
    let bytes = serde_json::to_vec_pretty(&input).unwrap();
    fs::write(&input_path, &bytes).unwrap();
    assert!(run(&input_path, WORKER_HASH, &output_path).await.is_err());
    assert!(!output_path.exists());
    run(&input_path, &sha256_bytes(&bytes), &output_path)
        .await
        .unwrap();
    let original = fs::read(&output_path).unwrap();
    let mut report: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(report["input_sha256"], sha256_bytes(&bytes));
    let report_hash = report
        .as_object_mut()
        .unwrap()
        .remove("report_sha256")
        .unwrap();
    assert_eq!(report_hash, stable_json_sha256(&report).unwrap());
    assert!(
        run(&input_path, &sha256_bytes(&bytes), &output_path)
            .await
            .is_err()
    );
    assert_eq!(fs::read(output_path).unwrap(), original);
}

#[tokio::test]
async fn model_lineage_uses_exact_root_and_missing_or_changed_model_evidence_rejects() {
    let mut input = fixture();
    for i in [0, 1] {
        input["baseline"]["documents"][i]["model_id"] = json!(id(500));
        input["baseline"]["documents"][i]["model_version"] = json!(VERSION);
    }
    let model = json!({"lifeCycleModelDataSet": {"lifeCycleModelInformation": {
        "dataSetInformation": {"common:UUID": id(500), "referenceToResultingProcess": {"@refObjectId": id(1), "@version": VERSION}},
        "technology": {"processes": {"processInstance": [{"referenceToProcess": {"@refObjectId": id(2), "@version": VERSION}}]}}
    }, "administrativeInformation": {"publicationAndOwnership": {"common:dataSetVersion": VERSION}}}});
    input["baseline"]["models"] = json!([{"id": id(500), "version": VERSION, "document_sha256": stable_json_sha256(&model).unwrap(), "document": model, "state_code": 100, "user_id": null}]);
    rehash(&mut input);
    let unresolved = run_value(input.clone()).await.unwrap();
    assert!(
        unresolved["before"]["provider_decisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["failure_reason"] == "lineage_overlap_requires_binding")
    );
    input["baseline"]["request_roots"]
        .as_array_mut()
        .unwrap()
        .push(json!({"process_id": id(1), "process_version": VERSION}));
    rehash(&mut input);
    let selected = run_value(input.clone()).await.unwrap();
    assert!(
        selected["before"]["provider_decisions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["failure_reason"].is_null())
    );
    let mut missing = input.clone();
    missing["baseline"]["models"] = json!([]);
    rehash(&mut missing);
    assert!(run_value(missing).await.is_err());
    input["baseline"]["models"][0]["document"]["lifeCycleModelDataSet"]["lifeCycleModelInformation"]
        ["dataSetInformation"]["referenceToResultingProcess"]["@version"] = json!("01.00.001");
    assert!(run_value(input).await.is_err());
}

#[tokio::test]
async fn unused_overlay_cannot_disappear_from_final_evidence() {
    let mut input = fixture();
    let unused = flow(104, 201, "Product flow");
    input["baseline"]["documents"]
        .as_array_mut()
        .unwrap()
        .push(json!(unused));
    rehash(&mut input);
    add_overlay(&mut input, 104, |_| {});
    let error = run_value(input).await.unwrap_err().to_string();
    assert!(error.contains("absent from native source evidence"));
}
