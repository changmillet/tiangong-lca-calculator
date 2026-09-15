//! Public offline command qualification with a synthetic singular finite universe.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solver_worker::calculation_evidence::{canonical_json_sha256, sha256_bytes};

fn binary_hash() -> String {
    let mut file = fs::File::open(env!("CARGO_BIN_EXE_snapshot_builder")).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    hex::encode(hash.finalize())
}

fn command(input: &Path, input_hash: &str, output: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_snapshot_builder"))
        .env_clear()
        // Invalid values would fail immediately if the offline path initialized live clients.
        .env("DATABASE_URL", "not-a-database-url")
        .env("CONN", "not-a-database-url")
        .env("S3_ENDPOINT", "not-an-object-store-url")
        .args(["--provider-impact-input"])
        .arg(input)
        .args([
            "--provider-impact-input-sha256",
            input_hash,
            "--provider-impact-out",
        ])
        .arg(output)
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn public_command_is_offline_deterministic_input_bound_and_non_overwriting() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("request.json");
    let first_path = temp.path().join("first.json");
    let second_path = temp.path().join("second.json");
    let mut input: Value = serde_json::from_str(include_str!(
        "fixtures/provider_impact_v1/singular-template.json"
    ))
    .unwrap();
    // The committed template's baseline/body hashes are independent golden values.
    input["expected_worker_sha256"] = json!(binary_hash());
    let bytes = serde_json::to_vec(&input).unwrap();
    fs::write(&input_path, &bytes).unwrap();
    let input_hash = sha256_bytes(&bytes);
    let first = command(&input_path, &input_hash, &first_path, &[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let original = fs::read(&first_path).unwrap();
    let mut report: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(report["scope"]["online_census_verified"], false);
    assert_eq!(report["input_sha256"], input_hash);
    assert_eq!(report["operation_counts"]["database_queries"], 0);
    assert_eq!(report["operation_counts"]["factorizations"], 0);
    assert_eq!(report["operation_counts"]["unit_solves"], 0);
    assert_eq!(
        report["before"]["matrix"]["technosphere_entries"],
        json!([
            {"row": 0, "col": 1, "value": 1.0}, {"row": 1, "col": 0, "value": 1.0}
        ])
    );
    let hash = report
        .as_object_mut()
        .unwrap()
        .remove("report_sha256")
        .unwrap();
    assert_eq!(hash, canonical_json_sha256(&report).unwrap());
    assert!(
        command(&input_path, &input_hash, &second_path, &[])
            .status
            .success()
    );
    assert_eq!(original, fs::read(&second_path).unwrap());
    assert!(
        !command(&input_path, &input_hash, &first_path, &[])
            .status
            .success()
    );
    assert_eq!(original, fs::read(&first_path).unwrap());
    let absent = temp.path().join("must-not-exist.json");
    let conflict = command(&input_path, &input_hash, &absent, &["--process-limit", "1"]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("unrelated CLI option"));
    assert!(!command(&input_path, "wrong", &absent, &[]).status.success());
    assert!(!absent.exists());
}
