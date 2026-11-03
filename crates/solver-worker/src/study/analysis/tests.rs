use super::*;
use crate::study::analysis_contracts::NativeFiles;
use crate::study::{artifact_bytes, execute, tests::analysis_prepared};

pub(in crate::study) fn service(prepared: Vec<PreparedInput>) -> (ServiceInput, Vec<VerifiedRun>) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-analysis/service-seams.json"
    ))
    .unwrap();
    let mut input: ServiceInput =
        serde_json::from_value(fixture["readInputResponse"]["data"].clone()).unwrap();
    input.runs.truncate(prepared.len());
    input.analysis.config.run_ids.truncate(prepared.len());
    input.analysis.config.kind = if prepared.len() == 1 {
        AnalysisKind::Contribution
    } else {
        AnalysisKind::Comparison
    };
    input.analysis.config_hash = config_hash(&input.analysis.config).unwrap();
    let mut verified = vec![];
    for (row, p) in input.runs.iter_mut().zip(prepared) {
        let pb = artifact_bytes(&p).unwrap();
        let result = execute(&p).unwrap();
        let rb = serde_json::to_vec(&result).unwrap();
        row.prepared_artifact.artifact_ref.content_hash = super::super::sha256(&pb);
        row.prepared_artifact.artifact_ref.byte_size = u64::try_from(pb.len()).unwrap();
        row.result_artifact.artifact_ref.content_hash = super::super::sha256(&rb);
        row.result_artifact.artifact_ref.byte_size = u64::try_from(rb.len()).unwrap();
        row.run["inputHash"] = json!(p.input_hash);
        row.run["effectiveModelRef"]["contentHash"] = json!(p.model_hash);
        row.run["calculationContractRef"] =
            serde_json::to_value(&p.calculation_contract_ref).unwrap();
        row.run["resultRef"] = serde_json::to_value(&row.result_artifact.artifact_ref).unwrap();
        row.revision["contentHash"] = json!(
            p.study_binding
                .as_ref()
                .unwrap()
                .study_revision_content_hash
        );
        row.revision["definition"] =
            serde_json::to_value(&p.study_binding.as_ref().unwrap().definition).unwrap();
        row.scenario["effectiveModelRef"] = row.run["effectiveModelRef"].clone();
        row.scenario["overrides"] = serde_json::to_value(&p.model.overrides).unwrap();
        row.preparation["modelRef"] = row.run["effectiveModelRef"].clone();
        row.preparation["inputHash"] = row.run["inputHash"].clone();
        row.preparation["inputArtifactRef"] =
            serde_json::to_value(&row.prepared_artifact.artifact_ref).unwrap();
        row.preparation["calculationContractRef"] = row.run["calculationContractRef"].clone();
        row.preparation["methodRefs"] = row.revision["definition"]["methodRefs"].clone();
        verified.push(verify_run(row.clone(), &pb, &rb, &Bounds::default()).unwrap());
    }
    (input, verified)
}
fn number(bundle: &FactsBundle, id: &str) -> Option<f64> {
    bundle
        .facts
        .iter()
        .find(|f| f.fact_id == id)
        .unwrap()
        .canonical_value
        .as_ref()
        .map(|s| s.parse().unwrap())
}
#[test]
fn canonical_numeric_and_config_golden_are_portable_without_changing_p1() {
    for (value, expected) in [
        (0.0, "0"),
        (-0.0, "0"),
        (1.0, "1"),
        (-1.25, "-1.25"),
        (1e20, "1e20"),
        (1e-20, "1e-20"),
        (f64::from_bits(1), "5e-324"),
    ] {
        assert_eq!(canonical_number(value).unwrap(), expected);
    }
    assert!(canonical_number(f64::NAN).is_err());
    assert!(canonical_number(f64::INFINITY).is_err());
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-analysis/service-seams.json"
    ))
    .unwrap();
    let config: AnalysisConfig =
        serde_json::from_value(fixture["readInputResponse"]["data"]["analysis"]["config"].clone())
            .unwrap();
    assert_eq!(
        config_hash(&config).unwrap(),
        fixture["readInputResponse"]["data"]["analysis"]["configHash"]
    );
}
#[test]
fn owner_schema_equivalence_vectors_match_producer_codec() {
    let configs: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-analysis/config-vectors.json"
    ))
    .unwrap();
    for case in configs["cases"].as_array().unwrap() {
        let parsed = serde_json::from_value::<AnalysisConfig>(case["value"].clone());
        let valid = parsed.as_ref().is_ok_and(|c| validate_config(c).is_ok());
        assert_eq!(valid, case["valid"].as_bool().unwrap(), "{}", case["name"]);
        if valid {
            assert_eq!(config_hash(&parsed.unwrap()).unwrap(), case["configHash"]);
        }
    }
    let facts: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-analysis/fact-codec-vectors.json"
    ))
    .unwrap();
    for case in facts["cases"].as_array().unwrap() {
        let parsed = serde_json::from_value::<Fact>(case["value"].clone());
        let valid = parsed
            .as_ref()
            .is_ok_and(|f| validate_fact_codec(f).is_ok());
        assert_eq!(valid, case["valid"].as_bool().unwrap(), "{}", case["name"]);
    }
}
#[test]
fn independent_product_order_preserves_extreme_representable_contributions() {
    // Exact arithmetic: 10^-250 * 10^250 * 10^-100 = 10^-100.
    // The tempting (B*C)*x order computes 10^-350 first and erases it.
    let value = contribution(&[vec![1e-250]], &[1e-100], 1e250, 0).unwrap();
    close(value, 1e-100).unwrap();
    assert!(product(f64::from_bits(1), 0.5).is_err());
}
#[test]
fn independent_cycle_allocation_repeated_and_treatment_contributions() {
    for (case, expected) in [
        ("single", 10.0),
        ("cycle", 260.0 / 49.0),
        ("allocation", 6.0),
        ("repeated", 18.0),
        ("input", 10.0),
    ] {
        let (input, runs) = service(vec![analysis_prepared(case, None, Some(2.0))]);
        let bundle = analyze(&input, &runs).unwrap();
        close(number(&bundle, "r0:h:0").unwrap(), expected).unwrap();
        let contributions: Vec<_> = bundle
            .facts
            .iter()
            .filter(|f| f.kind == "viewContribution")
            .collect();
        close(
            contributions
                .iter()
                .map(|f| f.canonical_value.as_ref().unwrap().parse::<f64>().unwrap())
                .sum(),
            expected,
        )
        .unwrap();
        assert_eq!(contributions.len(), runs[0].prepared.numeric.views.len());
        assert!(!bundle.scientific_ranking_qualified);
        if case == "repeated" {
            assert_eq!(
                contributions
                    .iter()
                    .filter(|f| f.canonical_value.as_deref() != Some("0"))
                    .count(),
                2
            );
        }
    }
}
#[test]
fn independent_comparison_zero_negative_and_tiny_values() {
    let (input, runs) = service(vec![
        analysis_prepared("single", None, Some(2.0)),
        analysis_prepared("single", Some(5.0), Some(2.0)),
    ]);
    let result = analyze(&input, &runs).unwrap();
    assert_eq!(number(&result, "comparison:delta:0"), Some(-5.0));
    assert_eq!(number(&result, "comparison:relative:0"), Some(-0.5));
    assert_eq!(result.comparability.status, "comparable");
    assert!(!result.comparability.ranking_allowed);
    let (input, runs) = service(vec![
        analysis_prepared("single", None, Some(0.0)),
        analysis_prepared("single", Some(-2.0), Some(0.0)),
    ]);
    let result = analyze(&input, &runs).unwrap();
    assert_eq!(number(&result, "comparison:delta:0"), Some(0.0));
    assert_eq!(number(&result, "comparison:relative:0"), None);
    let (input, runs) = service(vec![
        analysis_prepared("single", Some(-2.0), Some(2.0)),
        analysis_prepared("single", Some(-4.0), Some(2.0)),
    ]);
    let result = analyze(&input, &runs).unwrap();
    assert_eq!(number(&result, "comparison:delta:0"), Some(-2.0));
    assert_eq!(number(&result, "comparison:relative:0"), Some(-1.0));
    let (input, runs) = service(vec![analysis_prepared("tiny", None, Some(1.0))]);
    let result = analyze(&input, &runs).unwrap();
    assert_eq!(
        result
            .facts
            .iter()
            .find(|f| f.fact_id == "r0:h:0")
            .unwrap()
            .canonical_value
            .as_deref(),
        Some("5e-324")
    );
}
#[test]
fn missing_factor_is_unknown_but_known_zero_remains_zero() {
    let (input, runs) = service(vec![analysis_prepared("single", None, None)]);
    let result = analyze(&input, &runs).unwrap();
    assert_eq!(number(&result, "r0:h:0"), None);
    assert_eq!(number(&result, "r0:i:0:0"), None);
    assert!(!result.coverage[0].complete_lcia_scores);
    let (input, runs) = service(vec![analysis_prepared("single", None, Some(0.0))]);
    let result = analyze(&input, &runs).unwrap();
    assert_eq!(number(&result, "r0:h:0"), Some(0.0));
    assert!(result.coverage[0].complete_lcia_scores);
}
#[test]
fn partial_comparison_retains_unknown_rows_and_never_upgrades_known_terms_to_complete_category() {
    let (input, runs) = service(vec![
        analysis_prepared("partial", None, Some(2.0)),
        analysis_prepared("partial", Some(5.0), Some(2.0)),
    ]);
    let bundle = analyze(&input, &runs).unwrap();
    assert_eq!(number(&bundle, "comparison:delta:0"), Some(-5.0));
    assert_eq!(bundle.comparability.status, "not_assessed");
    assert!(!bundle.comparability.ranking_allowed);
    assert!(
        bundle
            .facts
            .iter()
            .any(|f| f.kind == "inventoryContribution"
                && f.canonical_value.is_none()
                && f.coverage == "unknown")
    );
    assert!(
        bundle
            .facts
            .iter()
            .filter(|f| f.kind == "inventoryContribution" && f.canonical_value.is_some())
            .all(|f| f.coverage == "partial")
    );
    assert!(
        bundle
            .coverage
            .iter()
            .all(|r| r.gaps.len() == 1 && !r.complete_lcia_scores)
    );
}
#[test]
fn frozen_bindings_hashes_and_numeric_corruption_are_rejected() {
    let (input, runs) = service(vec![analysis_prepared("single", None, Some(2.0))]);
    let pb = artifact_bytes(&runs[0].prepared).unwrap();
    let rb = serde_json::to_vec(&runs[0].result).unwrap();
    for mutate in [0, 1, 2, 3] {
        let mut row = input.runs[0].clone();
        match mutate {
            0 => row.run["inputHash"] = json!("0".repeat(64)),
            1 => row.revision["definition"]["functionalUnit"]["unit"] = json!("MJ"),
            2 => row.run["effectiveModelRef"]["contentHash"] = json!("0".repeat(64)),
            _ => row.prepared_artifact.artifact_ref.content_hash = "0".repeat(64),
        }
        assert!(verify_run(row, &pb, &rb, &Bounds::default()).is_err());
    }
    let mut result = runs[0].result.clone();
    result.h[0] = 0.0;
    let bad = serde_json::to_vec(&result).unwrap();
    let mut row = input.runs[0].clone();
    row.result_artifact.artifact_ref.content_hash = super::super::sha256(&bad);
    row.result_artifact.artifact_ref.byte_size = u64::try_from(bad.len()).unwrap();
    assert!(verify_run(row, &pb, &bad, &Bounds::default()).is_err());
    let mut bad = input.clone();
    bad.runs[0].run["studyId"] = json!(Uuid::new_v4());
    assert!(validate_input(&bad).is_err());
    let mut bad = input.clone();
    bad.analysis
        .config
        .run_ids
        .push(bad.analysis.config.run_ids[0]);
    assert!(validate_input(&bad).is_err());
}
#[test]
fn source_quality_partial_and_incompatible_units_do_not_become_rankings() {
    let (input, mut runs) = service(vec![
        analysis_prepared("single", None, Some(2.0)),
        analysis_prepared("single", Some(5.0), Some(2.0)),
    ]);
    // Comparability consumes frozen metadata; this isolated test changes only the
    // already verified in-memory proof to exercise its rejection branch.
    runs[1].prepared.numeric.method_axes[0].unit = "incompatible".into();
    let result = analyze(&input, &runs).unwrap();
    assert_eq!(result.comparability.status, "not_comparable");
    assert_eq!(number(&result, "comparison:delta:0"), None);
}
#[test]
fn native_file_adapter_emits_no_private_locators_and_checks_integrity() {
    let (input, runs) = service(vec![analysis_prepared("single", None, Some(2.0))]);
    let dir = tempfile::tempdir().unwrap();
    let prepared = dir.path().join("prepared.json");
    let result = dir.path().join("result.json");
    std::fs::write(&prepared, artifact_bytes(&runs[0].prepared).unwrap()).unwrap();
    std::fs::write(&result, serde_json::to_vec(&runs[0].result).unwrap()).unwrap();
    let request = NativeInput {
        schema_version: "lca.study-analysis-native.v1".into(),
        input: input.clone(),
        files: vec![NativeFiles {
            run_id: input.analysis.config.run_ids[0],
            prepared_path: prepared.to_str().unwrap().into(),
            result_path: result.to_str().unwrap().into(),
        }],
    };
    let file = dir.path().join("analysis.json");
    std::fs::write(&file, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = execute_native(file.to_str().unwrap(), &Bounds::default()).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(
        !text.contains("storageBucket")
            && !text.contains("storageObjectPath")
            && !text.contains("ownerActorId")
    );
    let parsed: FactsBundle = super::super::json::from_str(&text).unwrap();
    validate_output(&parsed, &input).unwrap();
    if let Some(path) = std::env::var_os("STUDY_ANALYSIS_FIXTURE_DIR") {
        let target = std::path::PathBuf::from(path);
        assert!(target.is_absolute());
        std::fs::create_dir_all(&target).unwrap();
        let prepared_path = target.join("prepared.json");
        let result_path = target.join("result.json");
        std::fs::copy(&prepared, &prepared_path).unwrap();
        std::fs::copy(&result, &result_path).unwrap();
        std::fs::write(
            target.join("service-input.json"),
            serde_json::to_vec_pretty(&input).unwrap(),
        )
        .unwrap();
        let native = NativeInput {
            schema_version: "lca.study-analysis-native.v1".into(),
            input: input.clone(),
            files: vec![NativeFiles {
                run_id: input.analysis.config.run_ids[0],
                prepared_path: prepared_path.to_str().unwrap().into(),
                result_path: result_path.to_str().unwrap().into(),
            }],
        };
        let native_path = target.join("analysis-native-input.json");
        std::fs::write(&native_path, serde_json::to_vec(&native).unwrap()).unwrap();
        let request = crate::study::runtime_contracts::NativeRequest {
            schema_version: crate::study::runtime_contracts::CHILD_SCHEMA.into(),
            limits: Bounds::default(),
            output_path: target.join("facts.json").to_str().unwrap().into(),
            operation: crate::study::runtime_contracts::NativeAction::Analyze {
                analysis_path: native_path.to_str().unwrap().into(),
            },
        };
        std::fs::write(
            target.join("native-request.json"),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        std::fs::write(target.join("facts.json"), text.as_bytes()).unwrap();
        std::fs::write(target.join("facts-ref.json"),serde_json::to_vec_pretty(&json!({"type":"analysisFacts","id":"10000000-0000-4000-8000-000000000006","contentHash":super::super::sha256(text.as_bytes()),"digestScope":"artifact_bytes_sha256","byteSize":text.len(),"mediaType":"application/json","schemaVersion":FACTS_SCHEMA})).unwrap()).unwrap();
        std::fs::write(target.join("CLASSIFICATION.txt"),"SYNTHETIC numerical native fixture. No API authorization, database issuance, public source, scientific, timing or business qualification. Service input contains example private artifact wrappers; facts contain no locators.\n").unwrap();
    }
    std::fs::write(result, b"{}").unwrap();
    assert!(execute_native(file.to_str().unwrap(), &Bounds::default()).is_err());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly selected built native binary; isolated component proof only"]
#[allow(clippy::too_many_lines)]
async fn analysis_native_component_and_running_cancellation() {
    use crate::{
        resource::CancellationToken,
        study::{
            runtime_contracts::{CHILD_SCHEMA, NativeAction, NativeRequest},
            runtime_process::{NativeFailure, run_native},
        },
    };
    use std::time::Duration;
    let binary = std::path::PathBuf::from(
        std::env::var_os("STUDY_NATIVE_BINARY").expect("absolute native binary required"),
    );
    assert!(binary.is_absolute());
    let (input, runs) = service(vec![
        analysis_prepared("envelope", None, Some(2.0)),
        analysis_prepared("envelope", Some(1.0), Some(2.0)),
    ]);
    assert_eq!(runs[0].prepared.numeric.views.len(), 128);
    let dir = tempfile::tempdir().unwrap();
    let mut files = vec![];
    for (index, run) in runs.iter().enumerate() {
        let prepared = dir.path().join(format!("{index}-prepared.json"));
        let result = dir.path().join(format!("{index}-result.json"));
        std::fs::write(&prepared, artifact_bytes(&run.prepared).unwrap()).unwrap();
        std::fs::write(&result, serde_json::to_vec(&run.result).unwrap()).unwrap();
        files.push(NativeFiles {
            run_id: input.analysis.config.run_ids[index],
            prepared_path: prepared.to_str().unwrap().into(),
            result_path: result.to_str().unwrap().into(),
        });
    }
    let native = NativeInput {
        schema_version: "lca.study-analysis-native.v1".into(),
        input: input.clone(),
        files,
    };
    let payload = dir.path().join("analysis-input.json");
    std::fs::write(&payload, serde_json::to_vec(&native).unwrap()).unwrap();
    let output = dir.path().join("facts.json");
    let request = NativeRequest {
        schema_version: CHILD_SCHEMA.into(),
        limits: Bounds::default(),
        output_path: output.to_str().unwrap().into(),
        operation: NativeAction::Analyze {
            analysis_path: payload.to_str().unwrap().into(),
        },
    };
    let request_path = dir.path().join("native-request.json");
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let cancel = CancellationToken::default();
    let started = tokio::time::Instant::now();
    let proof = run_native(
        &binary,
        &request_path,
        &cancel,
        started + Duration::from_secs(30),
        512 * 1024 * 1024,
    )
    .await
    .unwrap();
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap();
    assert!(proof.reaped);
    let bytes = std::fs::read(&output).unwrap();
    let bundle: FactsBundle = crate::study::json::from_slice(&bytes).unwrap();
    validate_output(&bundle, &input).unwrap();
    assert_eq!(bundle.inputs.len(), 2);
    assert!(bundle.facts.len() > 500);
    // A private FIFO holds this actual analysis child in its input phase. It is
    // never an actor-provided path and is removed with the task temp directory.
    let fifo = dir.path().join("blocked-prepared.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut blocked = native.clone();
    blocked.files[0].prepared_path = fifo.to_str().unwrap().into();
    std::fs::write(&payload, serde_json::to_vec(&blocked).unwrap()).unwrap();
    let cancelled = CancellationToken::default();
    let signal = cancelled.clone();
    let started_cancel = tokio::time::Instant::now();
    let (result, ()) = tokio::join!(
        run_native(
            &binary,
            &request_path,
            &cancelled,
            started_cancel + Duration::from_secs(5),
            512 * 1024 * 1024
        ),
        async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            signal.cancel();
        }
    );
    let failure = result.unwrap_err();
    let failure = failure.downcast_ref::<NativeFailure>().unwrap();
    assert_eq!(failure.code, "study_job_cancelled");
    assert!(failure.reaped);
    assert!(started_cancel.elapsed() < Duration::from_secs(5));
    assert_eq!(
        std::fs::read(&output).unwrap(),
        bytes,
        "cancelled native attempt cannot replace the prior artifact"
    );
    let receipt = json!({"schemaVersion":"lca.study-analysis-component-proof.v1","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"nativeBinarySha256":crate::study::runtime_preflight::binary_hash(&binary).unwrap(),"runs":2,"instancesPerRun":64,"viewsPerRun":128,"facts":bundle.facts.len(),"artifactBytes":bytes.len(),"elapsedMillis":elapsed,"sampledPeakRssBytes":proof.peak_rss_bytes,"successReaped":proof.reaped,"cancelledReaped":failure.reaped,"cancellationMillis":u64::try_from(started_cancel.elapsed().as_millis()).unwrap(),"scope":"synthetic native component; not API/DB/queue/source/production/scientific qualification"});
    if let Some(path) = std::env::var_os("STUDY_ANALYSIS_QUALIFICATION_RECEIPT") {
        assert!(std::path::Path::new(&path).is_absolute());
        std::fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }
    eprintln!("{receipt}");
}
