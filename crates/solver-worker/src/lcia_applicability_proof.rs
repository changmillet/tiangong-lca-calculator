//! Exact source binding for operator-pinned, reviewed nonspatial applicability claims.
//!
//! A trusted local configuration pins the proof file's raw SHA-256 independently of ordinary
//! compute payloads. This reader validates that engineering binding; it does not perform or grant
//! scientific review. The caller must supply hashes and the complete retained projection from the
//! actual bounded native source reader, and must separately reject lossy sources, named factor
//! regions, duplicate applicable records and unresolved applicability qualifiers.
//!
//! The canonical body is the complete native projection, hashed with sorted JSON object keys and
//! unchanged array order. The ordered-factor digest is supplied by that native reader and compared
//! exactly; this module never reconstructs factors or interprets geographic/model keywords.

use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::calculation_evidence::{canonical_json_sha256, sha256_bytes};

pub const PROOF_SET_SCHEMA_VERSION: &str = "lcia.applicability-proof-set.v1";
const LOCATION_INDEPENDENT: &str = "location_independent";
const MAX_PROOF_SET_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROOF_RECORDS: usize = 4096;
const MAX_BASIS_BINDINGS: usize = 256;

/// Complete native source bindings. A proof claim cannot supply or replace any of these facts.
#[derive(Debug, Clone, Copy)]
pub struct ProofSource<'a> {
    pub method_id: Uuid,
    pub method_version: &'a str,
    pub artifact_locator_id: Uuid,
    pub source_raw_sha256: &'a str,
    pub canonical_body_sha256: &'a str,
    pub ordered_factors_sha256: &'a str,
    pub projection_schema: &'a str,
    pub projection: &'a Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasisBinding {
    pub pointer: String,
    pub value_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReference {
    pub decision_id: String,
    pub sha256: String,
}

/// Persistable provenance only. Constructing or decoding a receipt cannot create a validated proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofReceipt {
    pub proof_set_sha256: String,
    pub proof_record_sha256: String,
    pub decision: String,
    pub method_id: Uuid,
    pub method_version: String,
    pub artifact_locator_id: Uuid,
    pub source_raw_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_xml_path: Option<PathBuf>,
    pub canonical_body_sha256: String,
    pub ordered_factors_sha256: String,
    pub projection_schema: String,
    pub basis: Vec<BasisBinding>,
    pub review_reference: ReviewReference,
}

/// Opaque validated binding; there is deliberately no deserialization or public constructor.
#[derive(Debug, Clone)]
pub struct ValidatedNonspatialProof {
    receipt: ProofReceipt,
}

impl ValidatedNonspatialProof {
    #[must_use]
    pub fn receipt(&self) -> ProofReceipt {
        self.receipt.clone()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofRecord {
    method_id: Uuid,
    method_version: String,
    artifact_locator_id: Uuid,
    decision: String,
    source_raw_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_xml_path: Option<PathBuf>,
    canonical_body_sha256: String,
    ordered_factors_sha256: String,
    projection_schema: String,
    basis: Vec<BasisBinding>,
    review_reference: ReviewReference,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofSetDocument {
    schema_version: String,
    records: Vec<ProofRecord>,
}

/// Proof records become trusted only through an explicit pin of the complete local file bytes.
#[derive(Debug)]
pub struct TrustedProofSet {
    raw_sha256: String,
    records: Vec<ProofRecord>,
}

impl TrustedProofSet {
    pub fn load(path: &Path, expected_raw_sha256: &str) -> anyhow::Result<Self> {
        validate_sha256(expected_raw_sha256)?;
        anyhow::ensure!(path.is_absolute(), "lcia_proof_path_must_be_absolute");
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(metadata.is_file(), "lcia_proof_source_must_be_regular_file");
        anyhow::ensure!(
            metadata.len() <= MAX_PROOF_SET_BYTES,
            "lcia_proof_set_size_limit_exceeded"
        );
        let mut bytes = Vec::new();
        file.take(MAX_PROOF_SET_BYTES + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= MAX_PROOF_SET_BYTES,
            "lcia_proof_set_size_limit_exceeded"
        );
        let raw_sha256 = sha256_bytes(&bytes);
        anyhow::ensure!(
            raw_sha256 == expected_raw_sha256,
            "lcia_proof_set_pin_mismatch"
        );
        // Deserialize directly into strict structs: duplicate object fields must not be silently
        // overwritten by an intermediate JSON Value.
        let document: ProofSetDocument = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            document.schema_version == PROOF_SET_SCHEMA_VERSION,
            "lcia_proof_set_schema_unsupported"
        );
        anyhow::ensure!(
            document.records.len() <= MAX_PROOF_RECORDS,
            "lcia_proof_set_record_limit_exceeded"
        );
        let mut methods = BTreeSet::new();
        let mut locators = BTreeSet::new();
        for record in &document.records {
            anyhow::ensure!(
                methods.insert(record.method_id),
                "lcia_proof_duplicate_method"
            );
            anyhow::ensure!(
                locators.insert(record.artifact_locator_id),
                "lcia_proof_duplicate_artifact_locator"
            );
            validate_record(record)?;
        }
        Ok(Self {
            raw_sha256,
            records: document.records,
        })
    }

    #[must_use]
    pub fn raw_sha256(&self) -> &str {
        &self.raw_sha256
    }

    /// Returns an operator-bound source locator without opening it. Production must independently
    /// read/hash the XML and compare its native projection with the already-selected method.
    pub fn source_for_method(
        &self,
        method_id: Uuid,
        method_version: &str,
        artifact_locator_id: Uuid,
    ) -> anyhow::Result<Option<(PathBuf, String)>> {
        let Some(record) = self.matching_record(method_id, method_version, artifact_locator_id)?
        else {
            return Ok(None);
        };
        let path = record
            .source_xml_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("lcia_proof_source_xml_path_missing"))?;
        Ok(Some((path.clone(), record.source_raw_sha256.clone())))
    }

    fn matching_record(
        &self,
        method_id: Uuid,
        method_version: &str,
        artifact_locator_id: Uuid,
    ) -> anyhow::Result<Option<&ProofRecord>> {
        let Some(record) = self
            .records
            .iter()
            .find(|record| record.method_id == method_id)
        else {
            anyhow::ensure!(
                !self
                    .records
                    .iter()
                    .any(|record| record.artifact_locator_id == artifact_locator_id),
                "lcia_proof_method_identity_mismatch"
            );
            return Ok(None);
        };
        anyhow::ensure!(
            record.method_version == method_version,
            "lcia_proof_method_version_mismatch"
        );
        anyhow::ensure!(
            record.artifact_locator_id == artifact_locator_id,
            "lcia_proof_artifact_locator_mismatch"
        );
        Ok(Some(record))
    }

    pub fn bind_location_independent(
        &self,
        input: ProofSource<'_>,
    ) -> anyhow::Result<Option<ValidatedNonspatialProof>> {
        let Some(record) = self.matching_record(
            input.method_id,
            input.method_version,
            input.artifact_locator_id,
        )?
        else {
            return Ok(None);
        };
        validate_sha256(input.source_raw_sha256)?;
        validate_sha256(input.canonical_body_sha256)?;
        validate_sha256(input.ordered_factors_sha256)?;
        anyhow::ensure!(
            record.source_raw_sha256 == input.source_raw_sha256,
            "lcia_proof_source_raw_mismatch"
        );
        anyhow::ensure!(
            record.canonical_body_sha256 == input.canonical_body_sha256,
            "lcia_proof_source_body_mismatch"
        );
        anyhow::ensure!(
            record.ordered_factors_sha256 == input.ordered_factors_sha256,
            "lcia_proof_ordered_factors_mismatch"
        );
        anyhow::ensure!(
            record.projection_schema == input.projection_schema,
            "lcia_proof_projection_schema_mismatch"
        );
        anyhow::ensure!(
            canonical_json_sha256(input.projection)? == input.canonical_body_sha256,
            "lcia_proof_projection_body_mismatch"
        );
        for binding in &record.basis {
            let value = input.projection.pointer(&binding.pointer).ok_or_else(|| {
                anyhow::anyhow!("lcia_proof_basis_pointer_missing: {}", binding.pointer)
            })?;
            anyhow::ensure!(
                canonical_json_sha256(value)? == binding.value_sha256,
                "lcia_proof_basis_value_mismatch: {}",
                binding.pointer
            );
        }
        Ok(Some(ValidatedNonspatialProof {
            receipt: ProofReceipt {
                proof_set_sha256: self.raw_sha256.clone(),
                proof_record_sha256: canonical_json_sha256(&serde_json::to_value(record)?)?,
                decision: record.decision.clone(),
                method_id: record.method_id,
                method_version: record.method_version.clone(),
                artifact_locator_id: record.artifact_locator_id,
                source_raw_sha256: record.source_raw_sha256.clone(),
                source_xml_path: record.source_xml_path.clone(),
                canonical_body_sha256: record.canonical_body_sha256.clone(),
                ordered_factors_sha256: record.ordered_factors_sha256.clone(),
                projection_schema: record.projection_schema.clone(),
                basis: record.basis.clone(),
                review_reference: record.review_reference.clone(),
            },
        }))
    }
}

fn validate_sha256(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "lcia_proof_sha256_must_be_lowercase_hex"
    );
    Ok(())
}

fn validate_token(value: &str, max_bytes: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value == value.trim()
            && value.len() <= max_bytes
            && !value.chars().any(char::is_control),
        "lcia_proof_text_invalid"
    );
    Ok(())
}

fn validate_pointer(pointer: &str) -> anyhow::Result<()> {
    validate_token(pointer, 4096)?;
    anyhow::ensure!(pointer.starts_with('/'), "lcia_proof_basis_pointer_invalid");
    let mut bytes = pointer.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'~' {
            anyhow::ensure!(
                matches!(bytes.next(), Some(b'0' | b'1')),
                "lcia_proof_basis_pointer_invalid"
            );
        }
    }
    Ok(())
}

fn validate_record(record: &ProofRecord) -> anyhow::Result<()> {
    anyhow::ensure!(
        record.decision == LOCATION_INDEPENDENT,
        "lcia_proof_decision_unsupported"
    );
    crate::calculation_bundle::validate_version(&record.method_version, "proof.method_version")?;
    validate_sha256(&record.source_raw_sha256)?;
    if let Some(path) = &record.source_xml_path {
        let text = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("lcia_proof_source_xml_path_invalid"))?;
        anyhow::ensure!(
            path.is_absolute() && !text.contains("://") && !text.chars().any(char::is_control),
            "lcia_proof_source_xml_path_must_be_absolute_local"
        );
    }
    validate_sha256(&record.canonical_body_sha256)?;
    validate_sha256(&record.ordered_factors_sha256)?;
    validate_token(&record.projection_schema, 256)?;
    validate_token(&record.review_reference.decision_id, 1024)?;
    validate_sha256(&record.review_reference.sha256)?;
    anyhow::ensure!(
        !record.basis.is_empty() && record.basis.len() <= MAX_BASIS_BINDINGS,
        "lcia_proof_basis_missing_or_oversized"
    );
    let mut pointers = BTreeSet::new();
    for binding in &record.basis {
        validate_pointer(&binding.pointer)?;
        validate_sha256(&binding.value_sha256)?;
        anyhow::ensure!(
            pointers.insert(&binding.pointer),
            "lcia_proof_duplicate_basis_pointer"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn projection() -> Value {
        json!({"schema":"synthetic.native-projection.v1", "metadata": {
            "identity": {"method_id":Uuid::from_u128(339),"version":"01.00.000"},
            "geography": {"model_description":"Reviewed synthetic model's stated scope"},
            "intended_application":"Synthetic complete source reviewed independently",
            "unknown_metadata":{"retained":"yes"}, "comments":["Original source comment"]},
            "factors":[{"flow_id":Uuid::from_u128(340),"version":"01.00.000",
                "direction":"Output","location":"","mean_value":"4",
                "derivation":{"source":"retained reference"}}]})
    }

    fn proof_document(projection: &Value) -> Value {
        json!({"schema_version":PROOF_SET_SCHEMA_VERSION,"records":[{
            "method_id":Uuid::from_u128(339),"method_version":"01.00.000",
            "artifact_locator_id":Uuid::from_u128(341),"decision":"location_independent",
            "source_raw_sha256":sha256_bytes(b"synthetic original XML bytes"),
            "canonical_body_sha256":canonical_json_sha256(projection).unwrap(),
            "ordered_factors_sha256":canonical_json_sha256(&projection["factors"]).unwrap(),
            "projection_schema":"synthetic.native-projection.v1",
            "basis":[{"pointer":"/metadata/geography/model_description",
                "value_sha256":canonical_json_sha256(&projection["metadata"]["geography"]["model_description"]).unwrap()}],
            "review_reference":{"decision_id":"synthetic-review-339","sha256":sha256_bytes(b"synthetic immutable review")}
        }]})
    }

    fn write_document(document: &Value) -> (tempfile::NamedTempFile, String) {
        let bytes = serde_json::to_vec(document).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &bytes).unwrap();
        (file, sha256_bytes(&bytes))
    }

    fn bind(
        set: &TrustedProofSet,
        projection: &Value,
        record: &Value,
    ) -> anyhow::Result<Option<ValidatedNonspatialProof>> {
        set.bind_location_independent(ProofSource {
            method_id: Uuid::from_u128(339),
            method_version: record["method_version"].as_str().unwrap(),
            artifact_locator_id: Uuid::parse_str(record["artifact_locator_id"].as_str().unwrap())
                .unwrap(),
            source_raw_sha256: record["source_raw_sha256"].as_str().unwrap(),
            canonical_body_sha256: record["canonical_body_sha256"].as_str().unwrap(),
            ordered_factors_sha256: record["ordered_factors_sha256"].as_str().unwrap(),
            projection_schema: record["projection_schema"].as_str().unwrap(),
            projection,
        })
    }

    #[test]
    fn exact_local_pin_and_retained_basis_produce_an_opaque_receipt() {
        let projection = projection();
        let document = proof_document(&projection);
        let (file, pin) = write_document(&document);
        let original = std::fs::read(file.path()).unwrap();
        let set = TrustedProofSet::load(file.path(), &pin).unwrap();
        let proof = bind(&set, &projection, &document["records"][0])
            .unwrap()
            .unwrap();
        let receipt = proof.receipt();
        assert_eq!(receipt.proof_set_sha256, pin);
        assert_eq!(
            receipt.proof_record_sha256,
            canonical_json_sha256(&document["records"][0]).unwrap()
        );
        assert_eq!(receipt.decision, LOCATION_INDEPENDENT);
        assert_eq!(
            receipt.basis[0].pointer,
            "/metadata/geography/model_description"
        );
        assert_eq!(receipt.review_reference.decision_id, "synthetic-review-339");
        assert_eq!(
            serde_json::to_value(&receipt).unwrap()["canonical_body_sha256"],
            document["records"][0]["canonical_body_sha256"]
        );
        assert_eq!(std::fs::read(file.path()).unwrap(), original);
    }

    #[test]
    fn production_source_lookup_returns_only_the_exact_pinned_reference() {
        let projection = projection();
        let directory = tempfile::tempdir().unwrap();
        let xml_path = directory.path().join("unopened-primary.xml");
        let mut document = proof_document(&projection);
        document["records"][0]["source_xml_path"] = json!(xml_path);
        let (file, pin) = write_document(&document);
        let set = TrustedProofSet::load(file.path(), &pin).unwrap();
        assert_eq!(set.raw_sha256(), pin);
        let (path, raw_hash) = set
            .source_for_method(Uuid::from_u128(339), "01.00.000", Uuid::from_u128(341))
            .unwrap()
            .unwrap();
        assert_eq!(path, xml_path);
        assert_eq!(
            raw_hash,
            document["records"][0]["source_raw_sha256"]
                .as_str()
                .unwrap()
        );
        assert!(
            !xml_path.exists(),
            "proof lookup must not open or materialize the XML"
        );
        let receipt = bind(&set, &projection, &document["records"][0])
            .unwrap()
            .unwrap()
            .receipt();
        assert_eq!(receipt.source_xml_path.as_ref(), Some(&xml_path));
        assert_eq!(
            receipt.proof_record_sha256,
            canonical_json_sha256(&document["records"][0]).unwrap()
        );
        for (method, version, locator, reason) in [
            (339, "01.00.001", 341, "method_version_mismatch"),
            (339, "01.00.000", 342, "artifact_locator_mismatch"),
            (343, "01.00.000", 341, "method_identity_mismatch"),
        ] {
            let error = set
                .source_for_method(Uuid::from_u128(method), version, Uuid::from_u128(locator))
                .unwrap_err();
            assert!(error.to_string().contains(reason), "{error}");
        }
        assert!(
            set.source_for_method(Uuid::from_u128(343), "01.00.000", Uuid::from_u128(344))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn diagnostic_binding_does_not_require_a_production_source_locator() {
        let projection = projection();
        let document = proof_document(&projection);
        let (file, pin) = write_document(&document);
        let set = TrustedProofSet::load(file.path(), &pin).unwrap();
        let error = set
            .source_for_method(Uuid::from_u128(339), "01.00.000", Uuid::from_u128(341))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("lcia_proof_source_xml_path_missing")
        );
        assert!(
            bind(&set, &projection, &document["records"][0])
                .unwrap()
                .is_some()
        );
        let proof = bind(&set, &projection, &document["records"][0])
            .unwrap()
            .unwrap();
        let mut historical_receipt = serde_json::to_value(proof.receipt()).unwrap();
        historical_receipt
            .as_object_mut()
            .unwrap()
            .remove("source_xml_path");
        let restored: ProofReceipt = serde_json::from_value(historical_receipt).unwrap();
        assert!(restored.source_xml_path.is_none());
    }

    #[test]
    fn proof_source_locators_must_be_absolute_local_paths() {
        for path in [
            "relative/source.xml",
            "https://example.invalid/source.xml",
            "file:///source.xml",
            "/tmp/https://example.invalid/source.xml",
            "/tmp/source\n.xml",
        ] {
            let mut document = proof_document(&projection());
            document["records"][0]["source_xml_path"] = json!(path);
            let (file, pin) = write_document(&document);
            assert!(
                TrustedProofSet::load(file.path(), &pin)
                    .unwrap_err()
                    .to_string()
                    .contains("source_xml_path_must_be_absolute_local")
            );
        }
    }

    #[test]
    fn a_pin_authenticates_review_bytes_without_inventing_scientific_review() {
        let mut document = proof_document(&projection());
        let (file, pin) = write_document(&document);
        assert!(TrustedProofSet::load(file.path(), "").is_err());
        assert!(TrustedProofSet::load(file.path(), &"0".repeat(64)).is_err());
        document["records"][0]["review_reference"]["decision_id"] = json!("different review");
        std::fs::write(file.path(), serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            TrustedProofSet::load(file.path(), &pin)
                .unwrap_err()
                .to_string()
                .contains("pin_mismatch")
        );
        // A newly operator-pinned record is an engineering assertion. No remote review lookup,
        // keyword, method name or numeric similarity silently authenticates it here.
    }

    #[test]
    fn all_source_identity_and_projection_bindings_are_exact() {
        let projection = projection();
        let document = proof_document(&projection);
        let (file, pin) = write_document(&document);
        let set = TrustedProofSet::load(file.path(), &pin).unwrap();
        for (field, changed, reason) in [
            (
                "method_version",
                json!("01.00.001"),
                "method_version_mismatch",
            ),
            (
                "artifact_locator_id",
                json!(Uuid::from_u128(342)),
                "artifact_locator_mismatch",
            ),
            (
                "source_raw_sha256",
                json!("a".repeat(64)),
                "source_raw_mismatch",
            ),
            (
                "canonical_body_sha256",
                json!("b".repeat(64)),
                "source_body_mismatch",
            ),
            (
                "ordered_factors_sha256",
                json!("c".repeat(64)),
                "ordered_factors_mismatch",
            ),
            (
                "projection_schema",
                json!("different.native-projection.v1"),
                "projection_schema_mismatch",
            ),
        ] {
            let mut source_record = document["records"][0].clone();
            source_record[field] = changed;
            let error = bind(&set, &projection, &source_record).unwrap_err();
            assert!(error.to_string().contains(reason), "{field}: {error}");
        }
        let mut changed_projection = projection.clone();
        changed_projection["metadata"]["unknown_metadata"]["retained"] = json!("changed");
        assert!(
            bind(&set, &changed_projection, &document["records"][0])
                .unwrap_err()
                .to_string()
                .contains("projection_body_mismatch")
        );
    }

    #[test]
    fn unrelated_methods_have_no_proof_but_locator_alias_drift_is_rejected() {
        let projection = projection();
        let document = proof_document(&projection);
        let (file, pin) = write_document(&document);
        let set = TrustedProofSet::load(file.path(), &pin).unwrap();
        let make_source = |locator| ProofSource {
            method_id: Uuid::from_u128(343),
            method_version: "01.00.000",
            artifact_locator_id: locator,
            source_raw_sha256: "",
            canonical_body_sha256: "",
            ordered_factors_sha256: "",
            projection_schema: "synthetic.native-projection.v1",
            projection: &projection,
        };
        assert!(
            set.bind_location_independent(make_source(Uuid::from_u128(344)))
                .unwrap()
                .is_none()
        );
        assert!(
            set.bind_location_independent(make_source(Uuid::from_u128(341)))
                .unwrap_err()
                .to_string()
                .contains("method_identity_mismatch")
        );
    }

    #[test]
    fn basis_must_address_retained_exact_values() {
        let projection = projection();
        for (pointer, digest, reason) in [
            ("/metadata/absent", "a".repeat(64), "basis_pointer_missing"),
            (
                "/metadata/geography/model_description",
                "b".repeat(64),
                "basis_value_mismatch",
            ),
        ] {
            let mut document = proof_document(&projection);
            document["records"][0]["basis"][0] = json!({"pointer":pointer,"value_sha256":digest});
            let (file, pin) = write_document(&document);
            let set = TrustedProofSet::load(file.path(), &pin).unwrap();
            assert!(
                bind(&set, &projection, &document["records"][0])
                    .unwrap_err()
                    .to_string()
                    .contains(reason)
            );
        }
    }

    #[test]
    fn malformed_or_duplicate_records_cannot_be_trusted_even_with_a_matching_pin() {
        let original = proof_document(&projection());
        let mut cases = Vec::new();
        for (pointer, value) in [
            ("/schema_version", json!("unknown")),
            ("/records/0/decision", json!("regional_transfer")),
            ("/records/0/method_version", json!("1.0")),
            ("/records/0/review_reference/decision_id", json!("")),
            ("/records/0/review_reference/sha256", json!("not-a-hash")),
            ("/records/0/basis", json!([])),
            ("/records/0/basis/0/pointer", json!("/bad~2escape")),
        ] {
            let mut document = original.clone();
            *document.pointer_mut(pointer).unwrap() = value;
            cases.push(document);
        }
        let mut unknown = original.clone();
        unknown["records"][0]["unrecognized_assertion"] = json!(true);
        cases.push(unknown);
        let mut duplicate = original.clone();
        duplicate["records"]
            .as_array_mut()
            .unwrap()
            .push(original["records"][0].clone());
        cases.push(duplicate);
        let mut duplicate_version = original.clone();
        let mut stale = original["records"][0].clone();
        stale["method_version"] = json!("01.00.001");
        duplicate_version["records"]
            .as_array_mut()
            .unwrap()
            .push(stale);
        cases.push(duplicate_version);
        let mut duplicate_basis = original.clone();
        duplicate_basis["records"][0]["basis"]
            .as_array_mut()
            .unwrap()
            .push(original["records"][0]["basis"][0].clone());
        cases.push(duplicate_basis);
        for document in cases {
            let (file, pin) = write_document(&document);
            assert!(
                TrustedProofSet::load(file.path(), &pin).is_err(),
                "{document}"
            );
        }
        let duplicate_field = b"{\"schema_version\":\"lcia.applicability-proof-set.v1\",\"schema_version\":\"lcia.applicability-proof-set.v1\",\"records\":[]}";
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), duplicate_field).unwrap();
        assert!(TrustedProofSet::load(file.path(), &sha256_bytes(duplicate_field)).is_err());
    }

    #[test]
    fn oversized_local_proof_is_rejected_before_parsing() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_PROOF_SET_BYTES + 1).unwrap();
        assert!(
            TrustedProofSet::load(file.path(), &"0".repeat(64))
                .unwrap_err()
                .to_string()
                .contains("size_limit_exceeded")
        );
    }
}
