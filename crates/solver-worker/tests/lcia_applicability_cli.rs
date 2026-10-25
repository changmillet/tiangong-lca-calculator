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
const FLOW_VERSION: &str = "03.00.004";

fn sha256(path: &Path) -> String {
    let mut file = fs::File::open(path).expect("open bound fixture");
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).expect("read bound fixture");
        if count == 0 {
            return hex::encode(hash.finalize());
        }
        hash.update(&buffer[..count]);
    }
}

fn binary_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| sha256(Path::new(BINARY)))
}

fn observation(location: &str) -> Value {
    json!({
        "flow_id": Uuid::from_u128(3391), "flow_version": FLOW_VERSION,
        "location": {"kind": "code", "value": location},
        "direction": "Output", "direction_label": "Output",
        "exchange_id": format!("synthetic-process:exchange-{location}"), "amount": 1
    })
}

fn request() -> Value {
    let method_id = Uuid::from_u128(3390);
    json!({
        "schema_version": "lcia.applicability.diagnostic-request.v1",
        "source": {"kind": "method_documents", "methods": [{
            "artifact_locator_id": method_id, "method_version": "01.04.000",
            "document": {"LCIAMethodDataSet": {
                "LCIAMethodInformation": {"dataSetInformation": {"common:UUID": method_id}},
                "characterisationFactors": {"factor": [
                    {"referenceToFlowDataSet": {"@refObjectId": Uuid::from_u128(3391), "@version": FLOW_VERSION},
                        "exchangeDirection": "Output", "location": "A", "meanValue": "4.0"},
                    {"referenceToFlowDataSet": {"@refObjectId": Uuid::from_u128(3391), "@version": FLOW_VERSION},
                        "exchangeDirection": "Output", "location": "B", "meanValue": "11.491"}
                ]}
            }}
        }]},
        "observations": [observation("A")]
    })
}

struct Fixture {
    _directory: TempDir,
    request_path: PathBuf,
    output_path: PathBuf,
    artifact_path: PathBuf,
}

impl Fixture {
    fn new(mut request: Value) -> Self {
        let directory = tempfile::tempdir().expect("isolated diagnostic directory");
        let artifact_path = directory.path().join("original-artifact.bin");
        fs::write(&artifact_path, b"synthetic immutable artifact bytes\n").expect("write artifact");
        request["artifacts"] = json!([{"path": artifact_path, "sha256": sha256(&artifact_path)}]);
        let request_path = directory.path().join("request.json");
        fs::write(&request_path, serde_json::to_vec(&request).unwrap()).expect("write request");
        Self {
            request_path,
            output_path: directory.path().join("report.json"),
            artifact_path,
            _directory: directory,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BINARY);
        command.current_dir(self.request_path.parent().unwrap());
        for (key, _) in std::env::vars_os() {
            let text = key.to_string_lossy();
            if text == "DATABASE_URL" || text == "CONN" || text.starts_with("LCIA_STATIC_CACHE") {
                command.env_remove(key);
            }
        }
        command.args([
            "--diagnose-lcia-request",
            self.request_path.to_str().unwrap(),
            "--diagnose-lcia-request-sha256",
            &sha256(&self.request_path),
            "--diagnose-lcia-binary-sha256",
            binary_hash(),
            "--diagnose-lcia-output",
            self.output_path.to_str().unwrap(),
        ]);
        command
    }

    fn report(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.output_path).expect("structured report exists"))
            .expect("valid diagnostic JSON")
    }
}

fn execute(command: &mut Command) -> Output {
    command
        .output()
        .expect("run actual public snapshot_builder")
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn actual_binary_selects_one_regional_factor_and_binds_exact_inputs() {
    let fixture = Fixture::new(request());
    let request_before = fs::read(&fixture.request_path).unwrap();
    let artifact_before = fs::read(&fixture.artifact_path).unwrap();
    let output = execute(&mut fixture.command());
    assert_exit(&output, 0);
    let report = fixture.report();
    assert_eq!(report["schema_version"], "lcia.applicability.diagnostic.v1");
    assert_eq!(report["selection_qualified"], true);
    assert_eq!(report["matrix_created"], false);
    assert_eq!(report["calculation_performed"], false);
    assert_eq!(report["historical_axis_qualification"], "unknown");
    assert_eq!(report["binary"]["sha256"], binary_hash());
    assert_eq!(report["request"]["sha256"], sha256(&fixture.request_path));
    assert_eq!(
        report["artifacts"][0]["sha256"],
        sha256(&fixture.artifact_path)
    );
    let selected = report["selection"]["selected"].as_array().unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["flow_version"], FLOW_VERSION);
    assert_eq!(selected[0]["decision"]["selected"]["value"], json!(4.0));
    assert_eq!(
        selected[0]["decision"]["selected"]["location"],
        json!({"kind": "code", "value": "A"})
    );
    assert_eq!(
        selected[0]["decision"]["rejected"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fs::read(&fixture.request_path).unwrap(), request_before);
    assert_eq!(fs::read(&fixture.artifact_path).unwrap(), artifact_before);
    let receipt: Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one JSON receipt");
    assert_eq!(receipt["selection_qualified"], true);
}

#[test]
fn actual_binary_refuses_mixed_locations_in_the_existing_flow_version_axis() {
    let mut input = request();
    input["observations"] = json!([observation("A"), observation("B")]);
    let fixture = Fixture::new(input);
    let output = execute(&mut fixture.command());
    assert_exit(&output, 2);
    let report = fixture.report();
    assert_eq!(report["selection_qualified"], false);
    assert_eq!(report["selection"]["status"], "incomplete");
    assert_eq!(
        report["selection"]["lcia_availability"]["categories"][0]["reasons"][0],
        "lcia_mixed_location_axis_unsupported"
    );
    assert_eq!(report["matrix_created"], false);
}

#[test]
fn actual_binary_refuses_wrong_flow_version_and_unknown_old_location() {
    for missing_location in [false, true] {
        let mut input = request();
        let expected_reason = if missing_location {
            input["observations"][0]
                .as_object_mut()
                .unwrap()
                .remove("location");
            "lcia_exchange_location_semantics_unresolved"
        } else {
            input["observations"][0]["flow_version"] = json!("03.00.003");
            "lcia_factor_flow_version_mismatch"
        };
        let fixture = Fixture::new(input);
        let output = execute(&mut fixture.command());
        assert_exit(&output, 2);
        let report = fixture.report();
        assert_eq!(report["selection_qualified"], false);
        assert_eq!(report["selection"]["status"], "incomplete");
        assert_eq!(
            report["selection"]["lcia_availability"]["completeness"],
            "unavailable"
        );
        assert_eq!(
            report["selection"]["unavailable"][0]["decision"]["reason"],
            expected_reason
        );
    }
}

#[test]
fn wrong_request_binary_or_artifact_hash_refuses_before_creating_output() {
    for corrupted in ["request", "binary", "artifact"] {
        let fixture = Fixture::new(request());
        let mut command = fixture.command();
        match corrupted {
            // Bind the original bytes, then change the file itself.
            "request" => fs::write(&fixture.request_path, b"{}\n").unwrap(),
            "artifact" => fs::write(&fixture.artifact_path, b"changed bytes\n").unwrap(),
            "binary" => {
                command = Command::new(BINARY);
                command.current_dir(fixture.request_path.parent().unwrap());
                for (key, _) in std::env::vars_os() {
                    let text = key.to_string_lossy();
                    if text == "DATABASE_URL"
                        || text == "CONN"
                        || text.starts_with("LCIA_STATIC_CACHE")
                    {
                        command.env_remove(key);
                    }
                }
                command
                    .arg("--diagnose-lcia-request")
                    .arg(&fixture.request_path)
                    .arg("--diagnose-lcia-request-sha256")
                    .arg(sha256(&fixture.request_path))
                    .arg("--diagnose-lcia-binary-sha256")
                    .arg("0".repeat(64))
                    .arg("--diagnose-lcia-output")
                    .arg(&fixture.output_path);
            }
            _ => unreachable!(),
        }
        let output = execute(&mut command);
        assert!(
            !output.status.success(),
            "must refuse {corrupted} hash drift"
        );
        assert!(
            !fixture.output_path.exists(),
            "must not publish an unbound report"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("SHA256 mismatch"));
    }
}

#[test]
fn existing_output_is_never_clobbered() {
    let fixture = Fixture::new(request());
    let original = b"existing evidence must remain unchanged\n";
    fs::write(&fixture.output_path, original).unwrap();
    let output = execute(&mut fixture.command());
    assert!(!output.status.success());
    assert_eq!(fs::read(&fixture.output_path).unwrap(), original);
}

#[test]
fn diagnostic_cli_rejects_connection_and_other_mode_flags() {
    for (flag, argument) in [
        ("--database-url", "postgres://invalid.invalid/never-connect"),
        ("--conn", "postgres://invalid.invalid/never-connect"),
        ("--lcia-static-cache-dir", "/nonexistent-must-not-read"),
        ("--snapshot-id", "00000000-0000-0000-0000-000000000001"),
    ] {
        let fixture = Fixture::new(request());
        let mut command = fixture.command();
        command.args([flag, argument]);
        let output = execute(&mut command);
        assert_exit(&output, 2);
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
        assert!(!fixture.output_path.exists());
    }
}

#[test]
fn duplicate_nested_source_keys_cannot_hide_a_factor_condition() {
    let fixture = Fixture::new(request());
    let original = fs::read_to_string(&fixture.request_path).unwrap();
    let duplicate = original.replacen(
        "\"meanValue\":\"4.0\"",
        "\"meanValue\":\"4.0\",\"generalComment\":\"urban only\",\"generalComment\":\"\"",
        1,
    );
    assert_ne!(original, duplicate);
    fs::write(&fixture.request_path, duplicate).unwrap();
    let output = execute(&mut fixture.command());
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate_json_source_field"));
    assert!(!fixture.output_path.exists());
}
