//! Synthetic operator pins test engineering binding; they grant no real scientific authority.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const BINARY: &str = env!("CARGO_BIN_EXE_snapshot_builder");
const METHOD_VERSION: &str = "01.00.000";
const FLOW_VERSION: &str = "03.00.004";

fn hash(path: &Path) -> String {
    let mut input = fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            return hex::encode(hasher.finalize());
        }
        hasher.update(&buffer[..count]);
    }
}

fn binary_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| hash(Path::new(BINARY)))
}

fn factor(location: &str, qualifier: &str) -> String {
    format!(
        r#"<factor><referenceToFlowDataSet refObjectId="{}" version="{FLOW_VERSION}" uri="../flows/not-followed.xml"/><exchangeDirection>Output</exchangeDirection>{location}<meanValue>4.0</meanValue><generalComment/><dataDerivationTypeStatus>Calculated</dataDerivationTypeStatus><relativeStandardDeviation95In>5</relativeStandardDeviation95In>{qualifier}</factor>"#,
        Uuid::from_u128(34000)
    )
}

fn xml(factors: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><LCIAMethodDataSet xmlns="http://lca.jrc.it/ILCD/LCIAMethod" xmlns:c="http://lca.jrc.it/ILCD/Common" xmlns:s="urn:synthetic"><LCIAMethodInformation><dataSetInformation><c:UUID>{}</c:UUID><c:name xml:lang="en">Synthetic method only</c:name></dataSetInformation></LCIAMethodInformation><administrativeInformation><c:commissionerAndGoal><c:intendedApplications xml:lang="en">Fictitious location independent model for synthetic engineering tests only</c:intendedApplications></c:commissionerAndGoal><publicationAndOwnership><c:dataSetVersion>{METHOD_VERSION}</c:dataSetVersion></publicationAndOwnership></administrativeInformation><characterisationFactors>{factors}</characterisationFactors></LCIAMethodDataSet>"#,
        Uuid::from_u128(33999)
    )
}

fn observation(location: Value) -> Value {
    let mut observation = json!({"flow_id":Uuid::from_u128(34000),"flow_version":FLOW_VERSION,
        "direction":"Output","direction_label":"Output",
        "exchange_id":"synthetic-process:exchange","amount":1});
    observation["location"] = location;
    observation
}

struct Fixture {
    directory: TempDir,
    source_path: PathBuf,
    inspection: Value,
}

impl Fixture {
    fn new(source: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("synthetic-method.xml");
        fs::write(&source_path, source).unwrap();
        let mut fixture = Self {
            directory,
            source_path,
            inspection: Value::Null,
        };
        let (output, path) = fixture.run(&fixture.request("inspect", vec![]), None, "inspect");
        assert_exit(&output, 0);
        let report = read_report(&path);
        assert_eq!(report["selection_qualified"], false);
        assert_eq!(report["operation"], "inspect");
        assert_eq!(report["selection"]["status"], "not_requested");
        fixture.inspection = report["source_inspections"][0].clone();
        fixture
    }

    fn request(&self, operation: &str, observations: Vec<Value>) -> Value {
        json!({"schema_version":"lcia.applicability.diagnostic-request.v1","operation":operation,
            "source":{"kind":"ilcd_xml","methods":[{"path":self.source_path,"sha256":hash(&self.source_path)}]},
            "observations":Value::Array(observations)})
    }

    fn proof(&self) -> Value {
        let source = &self.inspection;
        let basis = source["metadata_index"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["xml_path"]
                    .as_str()
                    .unwrap()
                    .ends_with("}intendedApplications")
            })
            .expect("actual retained synthetic basis");
        assert!(
            basis["text"]
                .as_str()
                .unwrap()
                .contains("synthetic engineering tests only")
        );
        json!({"schema_version":"lcia.applicability-proof-set.v1","records":[{
            "method_id":source["method_id"],"method_version":source["method_version"],
            "artifact_locator_id":source["artifact_locator_id"],"decision":"location_independent",
            "source_xml_path":self.source_path,"source_raw_sha256":source["raw_sha256"],
            "canonical_body_sha256":source["canonical_body_sha256"],
            "ordered_factors_sha256":source["ordered_factors_sha256"],"projection_schema":source["projection_schema"],
            "basis":[{"pointer":basis["pointer"],"value_sha256":basis["sha256"]}],
            "review_reference":{"decision_id":"synthetic-review-only-no-real-authorization",
                "sha256":hex::encode(Sha256::digest(b"synthetic immutable test review"))}
        }]})
    }

    fn write_proof(&self, value: &Value) -> PathBuf {
        let path = self.directory.path().join("synthetic-proof.json");
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn run(&self, request: &Value, proof: Option<(&Path, &str)>, label: &str) -> (Output, PathBuf) {
        let request_path = self.directory.path().join(format!("{label}-request.json"));
        let output_path = self.directory.path().join(format!("{label}-report.json"));
        fs::write(&request_path, serde_json::to_vec(request).unwrap()).unwrap();
        let mut command = Command::new(BINARY);
        command.current_dir(self.directory.path());
        for (key, _) in std::env::vars_os() {
            let text = key.to_string_lossy();
            if text == "DATABASE_URL"
                || text == "CONN"
                || text.starts_with("LCIA_STATIC_CACHE")
                || text.starts_with("LCIA_APPLICABILITY")
            {
                command.env_remove(key);
            }
        }
        command
            .arg("--diagnose-lcia-request")
            .arg(request_path.as_os_str())
            .arg("--diagnose-lcia-request-sha256")
            .arg(hash(&request_path))
            .arg("--diagnose-lcia-binary-sha256")
            .arg(binary_hash())
            .arg("--diagnose-lcia-output")
            .arg(&output_path);
        if let Some((path, pin)) = proof {
            command
                .arg("--lcia-applicability-proof-set")
                .arg(path)
                .arg("--lcia-applicability-proof-set-sha256")
                .arg(pin);
        }
        (command.output().expect("actual public binary"), output_path)
    }
}

fn assert_exit(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_report(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("published structured report")).unwrap()
}

#[test]
fn native_inspection_retains_basis_hashes_without_exporting_factor_records() {
    let source = xml(&factor("<location/>", ""));
    let fixture = Fixture::new(&source);
    let inspected = &fixture.inspection;
    assert_eq!(inspected["method_id"], Uuid::from_u128(33999).to_string());
    assert_eq!(inspected["method_version"], METHOD_VERSION);
    assert_eq!(inspected["raw_sha256"], hash(&fixture.source_path));
    assert_eq!(inspected["factor_count"], 1);
    assert_eq!(inspected["factor_records_exported"], false);
    assert_eq!(inspected["scientific_applicability_inferred"], false);
    for key in ["projection", "ordered_factors", "method_document"] {
        assert!(
            inspected.get(key).is_none(),
            "inspection must not export {key}"
        );
    }
    for entry in inspected["metadata_index"].as_array().unwrap() {
        assert!(!entry["xml_path"].as_str().unwrap().contains("}factor/"));
    }
    assert_eq!(fs::read_to_string(&fixture.source_path).unwrap(), source);
}

#[test]
fn blank_and_absent_factors_require_reviewed_proof_even_for_br() {
    for (index, location) in ["<location/>", ""].iter().enumerate() {
        let fixture = Fixture::new(&xml(&factor(location, "")));
        let request = fixture.request(
            "select",
            vec![observation(json!({"kind":"code","value":"BR"}))],
        );
        let (output, path) = fixture.run(&request, None, &format!("no-proof-{index}"));
        assert_exit(&output, 2);
        let report = read_report(&path);
        assert_eq!(report["selection_qualified"], false);
        assert_eq!(
            report["selection"]["issue"]["details"]["reason"],
            "lcia_factor_location_semantics_unresolved"
        );
    }
}

#[test]
fn reviewed_synthetic_nonspatial_source_selects_one_original_factor_across_locations() {
    for (index, (location, expected_kind)) in [("<location/>", "blank"), ("", "missing")]
        .iter()
        .enumerate()
    {
        let fixture = Fixture::new(&xml(&factor(location, "")));
        let original_source = fs::read(&fixture.source_path).unwrap();
        let proof_path = fixture.write_proof(&fixture.proof());
        let original_proof = fs::read(&proof_path).unwrap();
        let pin = hash(&proof_path);
        let request = fixture.request(
            "select",
            vec![
                observation(json!({"kind":"missing"})),
                observation(json!({"kind":"unknown"})),
                observation(json!({"kind":"code","value":"BR"})),
            ],
        );
        let (output, path) = fixture.run(
            &request,
            Some((&proof_path, &pin)),
            &format!("reviewed-{index}"),
        );
        assert_exit(&output, 0);
        let report = read_report(&path);
        assert_eq!(report["selection_qualified"], true);
        assert_eq!(report["proof_set_sha256"], pin);
        assert_eq!(report["matrix_created"], false);
        assert_eq!(report["calculation_performed"], false);
        let decisions = report["selection"]["selected"].as_array().unwrap();
        assert_eq!(decisions.len(), 3);
        let mut selected_hashes = std::collections::BTreeSet::new();
        let mut exchange_locations = std::collections::BTreeSet::new();
        for entry in decisions {
            let decision = &entry["decision"];
            assert_eq!(decision["reason"], "reviewed_location_independent_source");
            assert_eq!(decision["selected"]["value"], json!(4.0));
            assert_eq!(decision["selected"]["location"]["kind"], *expected_kind);
            assert_eq!(decision["selected"]["qualifiers"]["generalComment"], "");
            assert_eq!(
                decision["selected"]["qualifiers"]["relativeStandardDeviation95In"],
                "5"
            );
            assert_eq!(
                decision["source"]["nonspatial_proof"]["proof_set_sha256"],
                pin
            );
            assert_eq!(
                decision["source"]["native_source"]["raw_sha256"],
                fixture.inspection["raw_sha256"]
            );
            assert_eq!(
                decision["source"]["native_source"]["ordered_factors_sha256"],
                fixture.inspection["ordered_factors_sha256"]
            );
            selected_hashes.insert(decision["selected"]["record_sha256"].as_str().unwrap());
            exchange_locations.insert(serde_json::to_string(&decision["location"]).unwrap());
        }
        assert_eq!(
            selected_hashes.len(),
            1,
            "multiple exchange locations reuse one exact CF, never sum"
        );
        assert_eq!(
            exchange_locations.len(),
            3,
            "original exchange evidence states survive"
        );
        assert_eq!(fs::read(&fixture.source_path).unwrap(), original_source);
        assert_eq!(fs::read(&proof_path).unwrap(), original_proof);
    }
}

#[test]
fn reviewed_proof_cannot_override_comments_unknown_conditions_named_regions_or_duplicates() {
    let empty = factor("<location/>", "");
    for (index, factors) in [
        empty.replace(
            "<generalComment/>",
            "<generalComment>Only under a specific condition</generalComment>",
        ),
        factor(
            "<location/>",
            "<s:condition>Only applies in a special context</s:condition>",
        ),
        factor("<location>DE</location>", ""),
        format!("{empty}{empty}"),
    ]
    .iter()
    .enumerate()
    {
        let fixture = Fixture::new(&xml(factors));
        let proof = fixture.write_proof(&fixture.proof());
        let pin = hash(&proof);
        let request = fixture.request(
            "select",
            vec![observation(json!({"kind":"code","value":"BR"}))],
        );
        let (output, path) = fixture.run(
            &request,
            Some((&proof, &pin)),
            &format!("source-conflict-{index}"),
        );
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("lcia_nonspatial_source_conflict")
        );
        assert!(
            !path.exists(),
            "conflicting source cannot publish a qualified report"
        );
    }
}

#[test]
fn proof_pin_and_every_bound_source_dimension_must_match_inspected_source() {
    let fixture = Fixture::new(&xml(&factor("<location/>", "")));
    let request = fixture.request("select", vec![observation(json!({"kind":"unknown"}))]);
    let cases = [
        (
            "source_raw_sha256",
            json!("0".repeat(64)),
            "source_raw_mismatch",
        ),
        (
            "canonical_body_sha256",
            json!("0".repeat(64)),
            "source_body_mismatch",
        ),
        (
            "ordered_factors_sha256",
            json!("0".repeat(64)),
            "ordered_factors_mismatch",
        ),
        (
            "method_version",
            json!("01.00.001"),
            "method_version_mismatch",
        ),
        (
            "artifact_locator_id",
            json!(Uuid::from_u128(33998)),
            "artifact_locator_mismatch",
        ),
        (
            "method_id",
            json!(Uuid::from_u128(33998)),
            "method_identity_mismatch",
        ),
        (
            "projection_schema",
            json!("synthetic-wrong-schema"),
            "projection_schema_mismatch",
        ),
    ];
    for (index, (field, value, reason)) in cases.iter().enumerate() {
        let mut proof = fixture.proof();
        proof["records"][0][field] = value.clone();
        let path = fixture.write_proof(&proof);
        let pin = hash(&path);
        let (output, report_path) = fixture.run(
            &request,
            Some((&path, &pin)),
            &format!("wrong-binding-{index}"),
        );
        assert!(!output.status.success(), "must reject {field}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!report_path.exists());
    }
    for (index, (field, value, reason)) in [
        (
            "pointer",
            json!("/missing-source-basis"),
            "basis_pointer_missing",
        ),
        (
            "value_sha256",
            json!("0".repeat(64)),
            "basis_value_mismatch",
        ),
    ]
    .iter()
    .enumerate()
    {
        let mut proof = fixture.proof();
        proof["records"][0]["basis"][0][field] = value.clone();
        let path = fixture.write_proof(&proof);
        let pin = hash(&path);
        let (output, report_path) = fixture.run(
            &request,
            Some((&path, &pin)),
            &format!("wrong-basis-{index}"),
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(reason));
        assert!(!report_path.exists());
    }
    let proof = fixture.write_proof(&fixture.proof());
    let (output, path) = fixture.run(&request, Some((&proof, &"0".repeat(64))), "wrong-proof-pin");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("proof_set_pin_mismatch"));
    assert!(!path.exists());
}

#[test]
fn native_source_hash_alias_and_inventory_version_cannot_silently_change() {
    let fixture = Fixture::new(&xml(&factor("<location/>", "")));
    let proof = fixture.write_proof(&fixture.proof());
    let pin = hash(&proof);
    for (label, field, value, reason) in [
        (
            "source-hash",
            "sha256",
            json!("0".repeat(64)),
            "source_raw_hash_mismatch",
        ),
        (
            "source-alias",
            "artifact_locator_id",
            json!(Uuid::from_u128(12345)),
            "identity alias is not reviewed",
        ),
    ] {
        let mut request = fixture.request("select", vec![observation(json!({"kind":"unknown"}))]);
        request["source"]["methods"][0][field] = value;
        let (output, path) = fixture.run(&request, Some((&proof, &pin)), label);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(reason));
        assert!(!path.exists());
    }
    let mut request = fixture.request("select", vec![observation(json!({"kind":"unknown"}))]);
    request["observations"][0]["flow_version"] = json!("03.00.003");
    let (output, path) = fixture.run(&request, Some((&proof, &pin)), "inventory-version");
    assert_exit(&output, 2);
    let report = read_report(&path);
    assert_eq!(report["selection_qualified"], false);
    assert_eq!(
        report["selection"]["issue"]["details"]["reason"],
        "lcia_factor_flow_version_mismatch"
    );
}

#[test]
fn nonspatial_review_does_not_remove_the_existing_single_direction_constraint() {
    let fixture = Fixture::new(&xml(&factor("<location/>", "")));
    let proof = fixture.write_proof(&fixture.proof());
    let pin = hash(&proof);
    let output_exchange = observation(json!({"kind":"unknown"}));
    let mut input_exchange = output_exchange.clone();
    input_exchange["direction"] = json!("Input");
    input_exchange["direction_label"] = json!("Input");
    let request = fixture.request("select", vec![output_exchange, input_exchange]);
    let (output, path) = fixture.run(&request, Some((&proof, &pin)), "mixed-direction");
    assert_exit(&output, 2);
    let report = read_report(&path);
    assert_eq!(report["selection_qualified"], false);
    assert!(
        report["selection"]["selected"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        report["selection"]["unsupported_direction_group_count"]
            .as_u64()
            .unwrap()
            > 0
    );
}
