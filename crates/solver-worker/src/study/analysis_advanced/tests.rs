#![allow(clippy::ref_option)]
use super::*;
use crate::study::{
    analysis::tests::service, analysis_advanced_contracts::AnalysisRecord, runtime_contracts::*,
    tests::analysis_fixture,
};
use serde_json::json;
fn fixture(
    case: &str,
    change: Option<f64>,
    factor: Option<f64>,
) -> (ServiceInput, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (mut source, methods) = analysis_fixture(case, change, factor);
    let prepared = prepare(
        &serde_json::to_vec(&source).unwrap(),
        &methods,
        &Bounds::default(),
    )
    .unwrap();
    let pb = artifact_bytes(&prepared).unwrap();
    let (old, verified) = service(vec![prepared.clone()]);
    let rb = serde_json::to_vec(&verified[0].result).unwrap();
    source.method_refs.clear();
    source.cutoffs.clear();
    source.overrides.clear();
    source.study_binding = None;
    let packet = SourceSnapshot {
        schema_version: SNAPSHOT_SCHEMA.into(),
        origin_ref: json!({"type":"datasetRevision","datasetType":"lifecyclemodel","id":source.origin.id,"version":source.origin.version}),
        license_ref: "synthetic-not-a-license".into(),
        issuer_id: "synthetic-owner-fixture".into(),
        observed_at: "2026-11-04T00:00:00Z".into(),
        source_authorization: json!({"fixtureOnly":true}),
        source,
    };
    let sb = serde_json::to_vec(&packet).unwrap();
    let mut source_artifact = old.runs[0].prepared_artifact.clone();
    source_artifact.artifact_ref.r#type = "modelSource".into();
    source_artifact.artifact_ref.schema_version = SNAPSHOT_SCHEMA.into();
    source_artifact.artifact_ref.id = "10000000-0000-4000-8000-000000000080".parse().unwrap();
    source_artifact.storage_object_path = format!(
        "study/{}/{}/source.json",
        old.analysis.study_id, source_artifact.artifact_ref.id
    );
    source_artifact.artifact_ref.content_hash = sha256(&sb);
    source_artifact.artifact_ref.byte_size = sb.len() as u64;
    let amount = prepared
        .model
        .instances
        .iter()
        .find(|i| i.id == "a")
        .unwrap()
        .exchanges
        .iter()
        .find(|e| e.id == "e")
        .unwrap()
        .amount;
    let config = Config::SensitivityOat {
        schema_version: CONFIG_SCHEMA.into(),
        baseline_run_id: old.analysis.config.run_ids[0].to_string(),
        parameters: vec![Parameter {
            path: "/instances/a/exchanges/e/amount".into(),
            unit: "kg".into(),
            baseline_amount: canonical_number(amount).unwrap(),
            low: canonical_number(amount - 1.0).unwrap(),
            high: canonical_number(amount + 1.0).unwrap(),
        }],
    };
    let input = ServiceInput {
        schema_version: INPUT_SCHEMA.into(),
        analysis: AnalysisRecord {
            analysis_id: old.analysis.analysis_id,
            study_id: old.analysis.study_id,
            profile_id: config.profile().into(),
            config_hash: config_hash(&config).unwrap(),
            execution: old.analysis.execution,
            config,
        },
        baseline: old.runs[0].clone(),
        source_snapshot: serde_json::from_value(json!({"snapshotId":"10000000-0000-4000-8000-000000000081","studyId":old.analysis.study_id,"originRef":packet.origin_ref,"sourceArtifactRef":source_artifact.artifact_ref,"digestScope":"artifact_bytes_sha256","licenseRef":packet.license_ref,"issuerId":packet.issuer_id,"observedAt":packet.observed_at})).unwrap(),
        source_artifact,
    };
    (input, pb, rb, sb)
}
fn run(f: &(ServiceInput, Vec<u8>, Vec<u8>, Vec<u8>)) -> anyhow::Result<FactsBundle> {
    analyze(
        &f.0,
        &f.1,
        &f.2,
        &f.3,
        &Bounds::default(),
        &BatchLimits {
            max_parameters: 4096,
            max_evaluations: 20000,
        },
    )
}
fn uncertainty(input: &mut ServiceInput, design: Design, n: usize) {
    let old = &input.analysis.config;
    input.analysis.config = Config::Uncertainty {
        schema_version: CONFIG_SCHEMA.into(),
        baseline_run_id: old.baseline_run_id().into(),
        parameters: old.parameters().to_vec(),
        design,
        sample_count: n,
        seed_hex: "00".repeat(32),
        generator: GENERATOR.into(),
    };
    rehash(input);
}
fn rehash(input: &mut ServiceInput) {
    input.analysis.profile_id = input.analysis.config.profile().into();
    input.analysis.config_hash = config_hash(&input.analysis.config).unwrap();
}
fn value(v: &Option<String>) -> f64 {
    number(v.as_ref().unwrap()).unwrap()
}
#[test]
fn published_nist_hmac_drbg_sha256_no_reseed_vector() {
    let entropy =
        hex::decode("ca851911349384bffe89de1cbdc46e6831e44d34a4fb935ee285dd14b71a7488").unwrap();
    let nonce = hex::decode("659ba96c601dc69fc902940805ec0ca8").unwrap();
    let mut rng = Drbg::instantiate(&entropy, &nonce, &[]);
    rng.generate(128);
    assert_eq!(
        hex::encode(rng.generate(128)),
        "e528e9abf2dece54d47c7e75e5fe302149f817ea9fb4bee6f4199697d04d5b89d54fbb978a15b5c443c9ec21036d2460b6f73ebad0dc2aba6e624abf07745bc107694bb7547bb0995f70de25d6b29e2d3011bb19d27676c07162c8b5ccde0668961df86803482cb37ed6d5c0bb8d50cf1f50d476aa0458bdaba806f48be9dcb8"
    );
}
#[test]
fn oat_independent_analytic_baseline_signed_zero_allocation_cycle_and_partial() {
    for (case, change, factor, slope) in [
        ("single", None, Some(2.0), 1.0),
        ("single", Some(0.0), Some(2.0), 1.0),
        ("single", None, Some(-2.0), -1.0),
        ("allocation", None, Some(2.0), 0.6),
        ("cycle", None, Some(2.0), 100.0 / 49.0),
        ("partial", None, Some(2.0), 1.0),
    ] {
        let f = fixture(case, change, factor);
        let before = (f.1.clone(), f.2.clone(), f.3.clone());
        let out = run(&f).unwrap();
        let effect = &out.oat_effects[0];
        assert!((value(&effect.secant) - slope).abs() < 1e-12, "{case}");
        assert!((value(&effect.low_absolute_delta) + slope).abs() < 1e-12);
        assert!((value(&effect.high_absolute_delta) - slope).abs() < 1e-12);
        if change == Some(0.0) {
            assert!(effect.high_relative_delta.is_none());
            assert!(
                effect
                    .reason_codes
                    .contains(&"zero_baseline_relative_undefined".into())
            );
        }
        if case == "partial" {
            assert_eq!(effect.coverage, "partial");
        }
        assert_eq!(before, (f.1, f.2, f.3));
        assert_eq!(out.counts.valid, 2);
        assert_eq!(out.evaluations[0].evaluation_id, "evaluation-0");
        assert_eq!(out.resolved_parameters[0].support.len(), 2);
    }
    let f = fixture("single", None, None);
    let out = run(&f).unwrap();
    assert!(out.oat_effects[0].secant.is_none());
    assert_eq!(out.baseline_evaluation.outputs[0].coverage, "unknown");
}
#[test]
fn frozen_cumulative_other_overrides_survive() {
    let f = fixture("cycle", Some(8.0), Some(2.0));
    let out = run(&f).unwrap();
    assert_eq!(out.baseline_evaluation.amounts, vec!["8"]);
    assert!((value(&out.oat_effects[0].secant) - 100.0 / 49.0).abs() < 1e-12);
}
#[test]
fn uncertain_uniform_moments_replay_and_empirical_dependence() {
    let mut f = fixture("single", None, Some(2.0));
    uncertainty(
        &mut f.0,
        Design::IndependentUniform {
            independence_declared: true,
        },
        1024,
    );
    let out = run(&f).unwrap();
    let again = run(&f).unwrap();
    assert_eq!(
        out.evaluations
            .iter()
            .map(|e| (&e.amounts, &e.input_hash, &e.model_hash))
            .collect::<Vec<_>>(),
        again
            .evaluations
            .iter()
            .map(|e| (&e.amounts, &e.input_hash, &e.model_hash))
            .collect::<Vec<_>>()
    );
    let stats = &out.statistics[0];
    assert!((value(&stats.mean) - 10.0).abs() < 0.08);
    assert!((value(&stats.sample_standard_deviation) - (1.0_f64 / 3.0).sqrt()).abs() < 0.04);
    assert!(
        (value(&stats.mean_mcse) - value(&stats.sample_standard_deviation) / 32.0).abs() < 1e-15
    );
    // Two perfectly dependent raw amounts imply output 2*a + 2*b*10/49 under this cyclic control.
    let mut f = fixture("cycle", None, Some(2.0));
    if let Config::SensitivityOat { parameters, .. } = &mut f.0.analysis.config {
        parameters.push(Parameter {
            path: "/instances/b/exchanges/e/amount".into(),
            unit: "kg".into(),
            baseline_amount: "3".into(),
            low: "2".into(),
            high: "4".into(),
        });
    }
    uncertainty(
        &mut f.0,
        Design::JointEmpirical {
            rows: vec![vec!["1".into(), "2".into()], vec!["3".into(), "4".into()]],
        },
        64,
    );
    let out = run(&f).unwrap();
    for e in out.evaluations {
        let row = e.empirical_row_index.unwrap();
        assert_eq!(
            e.amounts,
            if row == 0 {
                vec!["1", "2"]
            } else {
                vec!["3", "4"]
            }
        );
        let a = number(&e.amounts[0]).unwrap();
        let b = number(&e.amounts[1]).unwrap();
        assert!(
            (value(&e.outputs[0].canonical_value) - (100.0 * a + 20.0 * b) / 49.0).abs() < 1e-12
        );
    }
}
#[test]
fn statistics_small_n_and_type7_reference() {
    assert_eq!(quantile(&[2.0], 0.025).unwrap(), 2.0);
    assert_eq!(quantile(&[0.0, 10.0], 0.25).unwrap(), 2.5);
    assert_eq!(quantile(&[-10.0, -2.0, 0.0, 20.0], 0.5).unwrap(), -1.0);
    assert!(quantile(&[], 0.5).is_err());
    let mut f = fixture("single", None, Some(2.0));
    uncertainty(
        &mut f.0,
        Design::JointEmpirical {
            rows: vec![vec!["10".into()]],
        },
        2,
    );
    let out = run(&f).unwrap();
    let s = &out.statistics[0];
    assert_eq!(s.mean.as_deref(), Some("10"));
    assert_eq!(s.sample_standard_deviation.as_deref(), Some("0"));
    assert_eq!(s.mean_mcse.as_deref(), Some("0"));
}
#[test]
fn invalid_configs_bounds_units_sources_identity_counts_and_budget_fail_closed() {
    for mutate in [0, 1, 2, 3, 4, 5, 6] {
        let mut f = fixture("single", None, Some(2.0));
        match mutate {
            0 => {
                f.0.analysis.config.parameters_mut_for_test()[0].path =
                    "/instances/a/exchanges/p/amount".into();
            }
            1 => f.0.analysis.config.parameters_mut_for_test()[0].unit = "tonnes".into(),
            2 => f.0.analysis.config.parameters_mut_for_test()[0].baseline_amount = "9".into(),
            3 => f.1[10] ^= 1,
            4 => f.3[10] ^= 1,
            5 => f.0.analysis.config_hash = "0".repeat(64),
            _ => f.0.source_snapshot.source_artifact_ref.content_hash = "0".repeat(64),
        }
        if mutate < 3 {
            rehash(&mut f.0);
        }
        assert!(run(&f).is_err(), "mutation{mutate}");
    }
    let f = fixture("single", None, Some(2.0));
    let bounds = Bounds {
        max_elapsed_millis: 1,
        ..Bounds::default()
    };
    assert!(
        analyze(
            &f.0,
            &f.1,
            &f.2,
            &f.3,
            &bounds,
            &BatchLimits {
                max_parameters: 1,
                max_evaluations: 2
            }
        )
        .is_err()
    );
    let mut raw = serde_json::to_value(&f.0.analysis.config).unwrap();
    raw["parameters"][0]["low"] = json!("9.0");
    assert!(validate_config(&serde_json::from_value(raw).unwrap()).is_err());
    let mut f = fixture("single", None, Some(2.0));
    uncertainty(
        &mut f.0,
        Design::IndependentUniform {
            independence_declared: true,
        },
        2,
    );
    if let Config::Uncertainty { design, .. } = &mut f.0.analysis.config {
        *design = Design::IndependentUniform {
            independence_declared: false,
        };
    }
    assert!(validate_config(&f.0.analysis.config).is_err());
}
impl Config {
    fn parameters_mut_for_test(&mut self) -> &mut Vec<Parameter> {
        match self {
            Self::SensitivityOat { parameters, .. } | Self::Uncertainty { parameters, .. } => {
                parameters
            }
        }
    }
}
#[test]
fn profile_rng_and_config_independent_python_golden_and_unbiased_rejection() {
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-analysis/advanced-rng-vectors.json"
    ))
    .unwrap();
    let mut rng = Drbg::instantiate(&[0; 32], &[], GENERATOR.as_bytes());
    for expected in vector["wordsHex"].as_array().unwrap() {
        assert_eq!(hex::encode(rng.generate(8)), expected.as_str().unwrap());
    }
    let mut words = [0_u64, 4].into_iter();
    let mut count = 0;
    assert_eq!(
        rng::select_row(3, || {
            count += 1;
            words.next().unwrap()
        })
        .unwrap(),
        1
    );
    assert_eq!(count, 2);
    let config: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-analysis/advanced-config-vectors.json"
    ))
    .unwrap();
    let parsed: Config = serde_json::from_value(config["config"].clone()).unwrap();
    assert_eq!(config_hash(&parsed).unwrap(), config["configHash"]);
}
#[test]
fn invalid_singular_endpoint_and_whole_row_fail_without_redraw() {
    for empirical in [false, true] {
        let mut f = fixture("cycle", None, Some(2.0));
        let p = &mut f.0.analysis.config.parameters_mut_for_test()[0];
        p.path = "/instances/a/exchanges/i/amount".into();
        p.baseline_amount = "0.2".into();
        p.low = "0.1".into();
        p.high = "10".into();
        rehash(&mut f.0);
        if empirical {
            uncertainty(
                &mut f.0,
                Design::JointEmpirical {
                    rows: vec![vec!["10".into()]],
                },
                8,
            );
        }
        let error = run(&f).unwrap_err();
        let failure = error.downcast_ref::<EvaluationFailure>().unwrap();
        assert_eq!(failure.invalid, 1);
        assert_eq!(failure.attempted, if empirical { 1 } else { 2 });
        assert_eq!(failure.valid, usize::from(!empirical));
    }
}
#[test]
fn actual_native_input_fixture_export_is_opt_in_and_integrity_checked() {
    let f = fixture("single", None, Some(2.0));
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("source.json");
    let prepared = dir.path().join("prepared.json");
    let result = dir.path().join("result.json");
    for (p, b) in [(&snapshot, &f.3), (&prepared, &f.1), (&result, &f.2)] {
        std::fs::write(p, b).unwrap();
    }
    let native = NativeInput {
        schema_version: "lca.study-analysis-native.v2".into(),
        input: f.0.clone(),
        snapshot_path: snapshot.to_str().unwrap().into(),
        prepared_path: prepared.to_str().unwrap().into(),
        result_path: result.to_str().unwrap().into(),
        batch_limits: BatchLimits {
            max_parameters: 8,
            max_evaluations: 1025,
        },
    };
    let path = dir.path().join("input.json");
    std::fs::write(&path, serde_json::to_vec(&native).unwrap()).unwrap();
    let bytes = execute_native(path.to_str().unwrap(), &Bounds::default()).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("storageBucket"));
    assert!(!text.contains("sourceAuthorization"));
    assert!(!text.contains("source.json"));
    if let Some(target) = std::env::var_os("STUDY_ADVANCED_FIXTURE_DIR") {
        let target = std::path::PathBuf::from(target);
        assert!(target.is_absolute());
        std::fs::create_dir_all(&target).unwrap();
        for (name, b) in [
            ("source.json", f.3),
            ("prepared.json", f.1),
            ("result.json", f.2),
            ("input.json", serde_json::to_vec_pretty(&f.0).unwrap()),
            ("facts.json", bytes),
        ] {
            std::fs::write(target.join(name), b).unwrap();
        }
        let mut u = fixture("cycle", None, Some(2.0));
        uncertainty(
            &mut u.0,
            Design::JointEmpirical {
                rows: vec![vec!["1".into()], vec!["3".into()]],
            },
            16,
        );
        let facts = run(&u).unwrap();
        std::fs::write(
            target.join("input-uncertainty.json"),
            serde_json::to_vec_pretty(&u.0).unwrap(),
        )
        .unwrap();
        std::fs::write(
            target.join("facts-uncertainty.json"),
            serde_json::to_vec_pretty(&facts).unwrap(),
        )
        .unwrap();
        std::fs::write(
            target.join("catalog-empirical.json"),
            serde_json::to_vec_pretty(&facts.fact_catalog).unwrap(),
        )
        .unwrap();
        let mut u = fixture("single", None, Some(2.0));
        uncertainty(
            &mut u.0,
            Design::IndependentUniform {
                independence_declared: true,
            },
            4,
        );
        let uniform = run(&u).unwrap();
        std::fs::write(
            target.join("input-uniform.json"),
            serde_json::to_vec_pretty(&u.0).unwrap(),
        )
        .unwrap();
        std::fs::write(
            target.join("facts-uniform.json"),
            serde_json::to_vec_pretty(&uniform).unwrap(),
        )
        .unwrap();
        std::fs::write(
            target.join("catalog-uniform.json"),
            serde_json::to_vec_pretty(&uniform.fact_catalog).unwrap(),
        )
        .unwrap();
        let oat: FactsBundle =
            super::super::json::from_slice(&std::fs::read(target.join("facts.json")).unwrap())
                .unwrap();
        std::fs::write(
            target.join("catalog-oat.json"),
            serde_json::to_vec_pretty(&oat.fact_catalog).unwrap(),
        )
        .unwrap();
        std::fs::write(target.join("CLASSIFICATION.txt"),"Synthetic numerical component. Not API, DB, source authorization, scientific or capacity qualification. Private input/source are not report content.\n").unwrap();
    }
    std::fs::write(&prepared, b"{}").unwrap();
    assert!(execute_native(path.to_str().unwrap(), &Bounds::default()).is_err());
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly selected native binary; synthetic local component measurement only"]
#[allow(clippy::too_many_lines)]
async fn advanced_native_capacity_and_running_cancellation() {
    use crate::{
        resource::CancellationToken,
        study::runtime_process::{NativeFailure, run_native},
    };
    use std::{path::PathBuf, time::Duration};
    let binary =
        PathBuf::from(std::env::var_os("STUDY_NATIVE_BINARY").expect("selected native binary"));
    assert!(binary.is_absolute());
    let mut measurements = vec![];
    for (case, uncertain, n, parameters, factor) in [
        ("single", false, 0, 1, 2.0),
        ("cycle", false, 0, 2, 2.0),
        ("allocation", false, 0, 1, 2.0),
        ("single", true, 1024, 1, -2.0),
        ("cycle", true, 1024, 1, 2.0),
        ("envelope", false, 0, 8, 2.0),
        ("envelope", true, 64, 8, 2.0),
        ("envelope", true, 128, 8, 2.0),
        ("dense-envelope", false, 0, 8, 2.0),
        ("dense-envelope", true, 16, 8, 2.0),
    ] {
        let mut f = fixture(case, None, Some(factor));
        let prepared_shape: PreparedInput = super::super::json::from_slice(&f.1).unwrap();
        if case == "dense-envelope" {
            assert_eq!(
                prepared_shape
                    .model
                    .instances
                    .iter()
                    .map(|i| i.exchanges.len())
                    .sum::<usize>(),
                4096
            );
            assert_eq!(prepared_shape.numeric.inventory_axes.len(), 61);
            assert_eq!(prepared_shape.numeric.method_axes.len(), 25);
            assert_eq!(prepared_shape.numeric.views.len(), 128);
            assert_eq!(prepared_shape.coverage.len(), 0);
        }
        if parameters > 1 {
            let base: PreparedInput = super::super::json::from_slice(&f.1).unwrap();
            let mut ps = vec![];
            for instance in &base.model.instances {
                if let Some(e) = instance.exchanges.iter().find(|e| e.id == "e") {
                    ps.push(Parameter {
                        path: format!("/instances/{}/exchanges/e/amount", instance.id),
                        unit: e.unit.clone(),
                        baseline_amount: canonical_number(e.amount).unwrap(),
                        low: canonical_number(e.amount - 1.0).unwrap(),
                        high: canonical_number(e.amount + 1.0).unwrap(),
                    });
                    if ps.len() == parameters {
                        break;
                    }
                }
            }
            *f.0.analysis.config.parameters_mut_for_test() = ps;
            rehash(&mut f.0);
        }
        if uncertain {
            uncertainty(
                &mut f.0,
                Design::IndependentUniform {
                    independence_declared: true,
                },
                n,
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("source.json");
        let prepared = dir.path().join("prepared.json");
        let result = dir.path().join("result.json");
        for (p, b) in [(&snapshot, &f.3), (&prepared, &f.1), (&result, &f.2)] {
            std::fs::write(p, b).unwrap();
        }
        let native = NativeInput {
            schema_version: "lca.study-analysis-native.v2".into(),
            input: f.0.clone(),
            snapshot_path: snapshot.to_str().unwrap().into(),
            prepared_path: prepared.to_str().unwrap().into(),
            result_path: result.to_str().unwrap().into(),
            batch_limits: BatchLimits {
                max_parameters: 8,
                max_evaluations: 1025,
            },
        };
        let payload = dir.path().join("input.json");
        std::fs::write(&payload, serde_json::to_vec(&native).unwrap()).unwrap();
        let output = dir.path().join("facts.json");
        let request = NativeRequest {
            schema_version: CHILD_SCHEMA.into(),
            limits: Bounds::default(),
            output_path: output.to_str().unwrap().into(),
            operation: NativeAction::AnalyzeAdvanced {
                analysis_path: payload.to_str().unwrap().into(),
            },
        };
        let request_path = dir.path().join("request.json");
        std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
        let started = tokio::time::Instant::now();
        let proof = run_native(
            &binary,
            &request_path,
            &CancellationToken::default(),
            started + Duration::from_secs(30),
            512 * 1024 * 1024,
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();
        let bytes = std::fs::read(&output).unwrap();
        let facts: FactsBundle = super::super::json::from_slice(&bytes).unwrap();
        assert_eq!(facts.config_hash, f.0.analysis.config_hash);
        if case == "dense-envelope" {
            // One unit of reference activity requires a 0.05 supplier ring.
            // Sum(x) = 1/(1-0.05); each of 61 flows has allocated burden 1,
            // and method 0 has factor 2: h = 2440/19, independently of A/B/C.
            let expected_baseline = 2440.0 / 19.0;
            assert!(
                (value(&facts.baseline_evaluation.outputs[0].canonical_value) - expected_baseline)
                    .abs()
                    < 1e-10
            );
            // Every independently constructed method is an exact signed scalar
            // multiple of method 0, across every baseline and sampled input.
            for evaluation in std::iter::once(&facts.baseline_evaluation).chain(&facts.evaluations)
            {
                assert_eq!(evaluation.outputs.len(), 25);
                let baseline = value(&evaluation.outputs[0].canonical_value);
                for (index, output) in evaluation.outputs.iter().enumerate() {
                    let scalar = f64::from(u32::try_from(index + 1).unwrap())
                        * if index % 2 == 0 { 1.0 } else { -1.0 };
                    let expected = baseline * scalar;
                    assert!(
                        (value(&output.canonical_value) - expected).abs()
                            <= 1e-10 * expected.abs().max(1.0)
                    );
                    assert_eq!(output.coverage, "complete");
                }
            }
            assert!(!facts.business_pilot_qualified && !facts.scientific_ranking_qualified);
        }
        assert!(proof.reaped);
        assert_eq!(std::fs::read(&prepared).unwrap(), f.1);
        assert_eq!(std::fs::read(&result).unwrap(), f.2);
        measurements.push(json!({"case":case,"kind":if uncertain{"uncertainty"}else{"sensitivityOat"},"parameters":parameters,"evaluationsIncludingBaseline":facts.evaluations.len()+1,"artifactBytes":bytes.len(),"elapsedMillis":u64::try_from(elapsed.as_millis()).unwrap(),"sampledPeakRssBytes":proof.peak_rss_bytes,"reaped":proof.reaped,"sourceBytes":f.3.len(),"preparedBytes":f.1.len(),"views":prepared_shape.numeric.views.len(),"inventoryAxes":prepared_shape.numeric.inventory_axes.len(),"methods":prepared_shape.numeric.method_axes.len(),"exchanges":prepared_shape.model.instances.iter().map(|i|i.exchanges.len()).sum::<usize>()}));
        if case == "envelope" && uncertain {
            let fifo = dir.path().join("blocked.fifo");
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(&fifo)
                    .status()
                    .unwrap()
                    .success()
            );
            let mut blocked = native;
            blocked.prepared_path = fifo.to_str().unwrap().into();
            std::fs::write(&payload, serde_json::to_vec(&blocked).unwrap()).unwrap();
            let cancel = CancellationToken::default();
            let signal = cancel.clone();
            let started = tokio::time::Instant::now();
            let (out, ()) = tokio::join!(
                run_native(
                    &binary,
                    &request_path,
                    &cancel,
                    started + Duration::from_secs(5),
                    512 * 1024 * 1024
                ),
                async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    signal.cancel();
                }
            );
            let err = out.unwrap_err();
            let failure = err.downcast_ref::<NativeFailure>().unwrap();
            assert_eq!(failure.code, "study_job_cancelled");
            assert!(failure.reaped);
            assert_eq!(std::fs::read(&output).unwrap(), bytes);
            measurements.push(json!({"case":"private-input-phase-cancellation","elapsedMillis":u64::try_from(started.elapsed().as_millis()).unwrap(),"reaped":failure.reaped,"sampledPeakRssBytes":failure.peak_rss_bytes}));
        }
    }
    let receipt = json!({"schemaVersion":"lca.study-advanced-component-proof.v1","scope":"synthetic local native component; no hosted capacity or cross-platform qualification","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"nativeBinarySha256":crate::study::runtime_preflight::binary_hash(&binary).unwrap(),"measurements":measurements});
    if let Some(path) = std::env::var_os("STUDY_ADVANCED_QUALIFICATION_RECEIPT") {
        let path = PathBuf::from(path);
        assert!(path.is_absolute());
        std::fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }
    eprintln!("{receipt}");
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "explicit native binary; actual 30-second aggregate deadline experiment"]
async fn advanced_native_aggregate_deadline_stops_near_limit_1024() {
    use crate::{resource::CancellationToken, study::runtime_process::run_native};
    use std::{path::PathBuf, time::Duration};
    let binary =
        PathBuf::from(std::env::var_os("STUDY_NATIVE_BINARY").expect("selected native binary"));
    let mut f = fixture("envelope", None, Some(2.0));
    uncertainty(
        &mut f.0,
        Design::IndependentUniform {
            independence_declared: true,
        },
        1024,
    );
    let dir = tempfile::tempdir().unwrap();
    let mut paths = vec![];
    for (name, bytes) in [
        ("source.json", f.3),
        ("prepared.json", f.1),
        ("result.json", f.2),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        paths.push(path);
    }
    let native = NativeInput {
        schema_version: "lca.study-analysis-native.v2".into(),
        input: f.0,
        snapshot_path: paths[0].to_str().unwrap().into(),
        prepared_path: paths[1].to_str().unwrap().into(),
        result_path: paths[2].to_str().unwrap().into(),
        batch_limits: BatchLimits {
            max_parameters: 8,
            max_evaluations: 1025,
        },
    };
    let payload = dir.path().join("input.json");
    std::fs::write(&payload, serde_json::to_vec(&native).unwrap()).unwrap();
    let output = dir.path().join("output.json");
    let request = NativeRequest {
        schema_version: CHILD_SCHEMA.into(),
        limits: Bounds::default(),
        output_path: output.to_str().unwrap().into(),
        operation: NativeAction::AnalyzeAdvanced {
            analysis_path: payload.to_str().unwrap().into(),
        },
    };
    let request_path = dir.path().join("request.json");
    std::fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let started = tokio::time::Instant::now();
    let result = run_native(
        &binary,
        &request_path,
        &CancellationToken::default(),
        started + Duration::from_secs(35),
        512 * 1024 * 1024,
    )
    .await;
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(35));
    let failure: NativeErrorEnvelope =
        super::super::json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(failure.kind.code(), "study_resource_limit");
    let receipt = json!({"scope":"actual native aggregate deadline on synthetic128view64instance model; no hosted capacity claim","elapsedMillis":u64::try_from(started.elapsed().as_millis()).unwrap(),"sampleCount":1024,"stopCode":failure.kind.code(),"nativeBinarySha256":crate::study::runtime_preflight::binary_hash(&binary).unwrap()});
    if let Some(path) = std::env::var_os("STUDY_ADVANCED_DEADLINE_RECEIPT") {
        std::fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }
    eprintln!("{receipt}");
}

#[test]
fn parent_output_validation_rejects_misbound_settings_samples_statistics_and_baseline() {
    let f = fixture("single", None, Some(2.0));
    let out = run(&f).unwrap();
    validate_output(
        &out,
        &f.0,
        &f.1,
        &Bounds::default(),
        &BatchLimits {
            max_parameters: 4096,
            max_evaluations: 20000,
        },
    )
    .unwrap();
    for kind in 0..5 {
        let mut changed = out.clone();
        match kind {
            0 => changed.baseline_evaluation.result_sha256 = "0".repeat(64),
            1 => changed.evaluations[0].amounts[0] = "9.5".into(),
            2 => changed.oat_effects[0].secant = Some("99".into()),
            3 => changed.counts.valid = 1,
            _ => changed.evaluations[0].outputs[0].unit = "wrong".into(),
        }
        assert!(
            validate_output(
                &changed,
                &f.0,
                &f.1,
                &Bounds::default(),
                &BatchLimits {
                    max_parameters: 4096,
                    max_evaluations: 20000
                }
            )
            .is_err()
        );
    }
}

#[test]
fn parent_validation_rejects_consistently_forged_frozen_metadata_and_rng() {
    let batch = BatchLimits {
        max_parameters: 4096,
        max_evaluations: 20000,
    };
    let f = fixture("partial", None, Some(2.0));
    let out = run(&f).unwrap();
    for mutation in 0..6 {
        let mut b = out.clone();
        match mutation {
            0 => {
                for e in std::iter::once(&mut b.baseline_evaluation).chain(&mut b.evaluations) {
                    for o in &mut e.outputs {
                        o.unit = "forged".into();
                    }
                }
                for e in &mut b.oat_effects {
                    e.output_unit = "forged".into();
                }
            }
            1 => b.baseline.method_source_hash = "0".repeat(64),
            2 => b.coverage_gaps.clear(),
            3 => b.quality_findings.push("forged".into()),
            4 => b.resolved_parameters[0].support.clear(),
            _ => b.execution_limits.native.max_elapsed_millis = 1,
        }
        assert!(validate_output(&b, &f.0, &f.1, &Bounds::default(), &batch).is_err());
    }
    let mut f = fixture("single", None, Some(2.0));
    uncertainty(
        &mut f.0,
        Design::IndependentUniform {
            independence_declared: true,
        },
        2,
    );
    let mut out = run(&f).unwrap();
    out.evaluations[0].amounts[0] = "10".into();
    assert_eq!(
        validate_output(&out, &f.0, &f.1, &Bounds::default(), &batch)
            .unwrap_err()
            .to_string(),
        "study_advanced_output_rng"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn report_catalog_counts_context_units_and_complete_entries_are_producer_owned() {
    use crate::study::analysis_advanced_contracts::{FactContext, ReportUnitIdentity};
    let batch = BatchLimits {
        max_parameters: 4096,
        max_evaluations: 20000,
    };
    for (case, change, factor) in [
        ("single", None, Some(2.0)),
        ("single", Some(0.0), Some(2.0)),
        ("partial", None, Some(0.0)),
        ("single", None, None),
    ] {
        let f = fixture(case, change, factor);
        let out = run(&f).unwrap();
        assert_eq!(out.fact_catalog.len(), 11);
        let prepared: PreparedInput = super::super::json::from_slice(&f.1).unwrap();
        assert!(
            catalog::preflight_bytes(&f.0.analysis.config, &prepared).unwrap()
                >= serde_json::to_vec(&out.fact_catalog).unwrap().len()
        );
        let p = &out.fact_catalog[0];
        assert_eq!(p.fact_id, "parameter:0:baseline");
        assert_eq!(p.baseline_run_id, out.baseline.run_id);
        assert!(
            matches!(&p.unit_identity,ReportUnitIdentity::ExchangeAmount{support,..} if support==&out.resolved_parameters[0].support)
        );
        let relative = out
            .fact_catalog
            .iter()
            .find(|f| f.fact_id == "oat:parameter:0:low:relativeDelta:method:0")
            .unwrap();
        assert_eq!(relative.unit, "1");
        assert!(
            matches!(&relative.unit_identity,ReportUnitIdentity::Dimensionless{unit} if unit=="1")
        );
        let secant = out
            .fact_catalog
            .iter()
            .find(|f| f.fact_id == "oat:parameter:0:secant:method:0")
            .unwrap();
        assert_eq!(secant.unit, "(impact)/(kg)");
        assert!(
            matches!(&secant.unit_identity,ReportUnitIdentity::Secant{numerator,denominator} if numerator.method_ref==out.baseline_evaluation.outputs[0].method_ref&&denominator.support==out.resolved_parameters[0].support)
        );
        if change == Some(0.0) || factor == Some(0.0) {
            assert!(relative.canonical_value.is_none());
            assert_eq!(
                relative.reason_codes,
                vec!["zero_baseline_relative_undefined"]
            );
        }
        if factor.is_none() {
            for f in out.fact_catalog.iter().filter(|f| f.method_ref.is_some()) {
                assert!(f.canonical_value.is_none());
                assert_eq!(f.coverage, "unknown");
                assert_eq!(f.reason_codes, vec!["no_characterized_terms"]);
            }
        }
        for mutation in 0..6 {
            let mut changed = out.clone();
            match mutation {
                0 => {
                    changed.fact_catalog.pop();
                }
                1 => changed.fact_catalog.push(changed.fact_catalog[0].clone()),
                2 => changed.fact_catalog[0].canonical_value = Some("999".into()),
                3 => changed.fact_catalog[4].unit = "kg".into(),
                4 => {
                    changed.fact_catalog[4].context =
                        FactContext::BaselineOutput { method_index: 0 }
                }
                _ => changed.fact_catalog.swap(0, 1),
            }
            assert_eq!(
                validate_output(&changed, &f.0, &f.1, &Bounds::default(), &batch)
                    .unwrap_err()
                    .to_string(),
                "study_advanced_output_catalog"
            );
        }
    }
    for empirical in [false, true] {
        let mut f = fixture("single", None, Some(2.0));
        uncertainty(
            &mut f.0,
            if empirical {
                Design::JointEmpirical {
                    rows: vec![vec!["9".into()], vec!["11".into()]],
                }
            } else {
                Design::IndependentUniform {
                    independence_declared: true,
                }
            },
            4,
        );
        let out = run(&f).unwrap();
        assert_eq!(out.fact_catalog.len(), 12);
        assert_eq!(
            out.fact_catalog
                .iter()
                .filter(|f| matches!(f.context, FactContext::UncertaintyStatistic { .. }))
                .count(),
            5
        );
        assert_eq!(
            out.fact_catalog
                .iter()
                .filter(|f| matches!(f.context, FactContext::UncertaintyQuantile { .. }))
                .count(),
            3
        );
        assert!(
            !out.fact_catalog
                .iter()
                .any(|f| f.fact_id.contains("evaluation-"))
        );
    }
}

#[test]
fn captured_source_metadata_is_closed_and_preserves_timestamp_instant() {
    let mut f = fixture("single", None, Some(2.0));
    f.0.source_snapshot.observed_at = "2026-11-04T08:00:00+08:00".into();
    assert!(run(&f).is_ok());
    f.0.source_snapshot.observed_at = "2026-11-04T08:00:01+08:00".into();
    assert_eq!(
        run(&f).unwrap_err().to_string(),
        "study_advanced_snapshot_metadata"
    );
    let mut wire = serde_json::to_value(&f.0).unwrap();
    wire["sourceSnapshot"]["authorized"] = json!(true);
    assert!(serde_json::from_value::<ServiceInput>(wire).is_err());
}
