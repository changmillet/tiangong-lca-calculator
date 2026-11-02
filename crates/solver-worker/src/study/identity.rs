use super::contracts::{Bounds, COMPILER, INPUT_SCHEMA, PROFILE, PreparedInput};
use anyhow::ensure;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// JSON objects sort lexicographically; arrays preserve owner-defined order. Finite f64 use
/// `serde_json`'s pinned shortest-roundtrip encoding. Domain + NUL precedes UTF-8 JSON, no newline.
pub fn semantic_hash(domain: &str, value: &impl Serialize) -> anyhow::Result<String> {
    let value = serde_json::to_value(value)?;
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend(serde_json::to_vec(&value)?);
    Ok(sha256(&bytes))
}
pub fn artifact_bytes(input: &PreparedInput) -> anyhow::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(input)?;
    ensure!(
        bytes.len() <= input.limits.max_artifact_bytes,
        "study_artifact_limit"
    );
    Ok(bytes)
}
pub fn decode_prepared(
    bytes: &[u8],
    expected_sha256: &str,
    bounds: &Bounds,
) -> anyhow::Result<PreparedInput> {
    ensure!(
        bytes.len() <= bounds.max_artifact_bytes,
        "study_artifact_limit"
    );
    ensure!(
        sha256(bytes) == expected_sha256,
        "study_artifact_hash_mismatch"
    );
    let value: PreparedInput = serde_json::from_slice(bytes)?;
    ensure!(
        value.schema_version == INPUT_SCHEMA
            && value.compiler_version == COMPILER
            && value.profile_id == PROFILE,
        "study_artifact_schema"
    );
    ensure!(
        value.calculation_contract_ref.sha256 == crate::calculation_contract::fingerprint(),
        "study_calculation_contract_mismatch"
    );
    super::compile::verify_identities(&value)?;
    Ok(value)
}
