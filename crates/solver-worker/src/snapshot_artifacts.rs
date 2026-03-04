use std::path::Path;

use hdf5::File;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solver_core::ModelSparseData;
use tempfile::Builder;
use uuid::Uuid;

const SCHEMA_VERSION: u8 = 1;
const DATASET_SCHEMA_VERSION: &str = "schema_version";
const DATASET_FORMAT: &str = "format";
const DATASET_ENVELOPE_JSON: &str = "envelope_json";

/// Snapshot matrix artifact format identifier.
pub const SNAPSHOT_ARTIFACT_FORMAT: &str = "snapshot-hdf5:v1";
/// Snapshot artifact file extension.
pub const SNAPSHOT_ARTIFACT_EXTENSION: &str = "h5";
/// Snapshot artifact content type.
pub const SNAPSHOT_ARTIFACT_CONTENT_TYPE: &str = "application/x-hdf5";

/// Snapshot build options persisted in artifact metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotBuildConfig {
    /// `state_code` selection used in builder.
    pub process_states: String,
    /// Process cap (`0` means unlimited).
    pub process_limit: i32,
    /// Provider matching mode.
    pub provider_rule: String,
    /// Self-loop cutoff for technosphere diagonal filtering.
    pub self_loop_cutoff: f64,
    /// Near-singular epsilon.
    pub singular_eps: f64,
    /// Whether LCIA factors were enabled.
    pub has_lcia: bool,
    /// Optional LCIA method id.
    pub method_id: Option<Uuid>,
    /// Optional LCIA method version.
    pub method_version: Option<String>,
}

/// Matching coverage diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMatchingCoverage {
    pub input_edges_total: i64,
    pub matched_unique_provider: i64,
    pub matched_multi_provider: i64,
    pub unmatched_no_provider: i64,
    pub unique_provider_match_pct: f64,
    pub any_provider_match_pct: f64,
}

/// Singular risk diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotSingularRisk {
    pub risk_level: String,
    pub prefilter_diag_abs_ge_cutoff: i64,
    pub postfilter_a_diag_abs_ge_cutoff: i64,
    pub m_zero_diagonal_count: i64,
    pub m_min_abs_diagonal: f64,
}

/// Matrix scale diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMatrixScale {
    pub process_count: i64,
    pub flow_count: i64,
    pub impact_count: i64,
    pub a_nnz: i64,
    pub b_nnz: i64,
    pub c_nnz: i64,
    pub m_nnz_estimated: i64,
    pub m_sparsity_estimated: f64,
}

/// Snapshot coverage report persisted beside payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotCoverageReport {
    pub matching: SnapshotMatchingCoverage,
    pub singular_risk: SnapshotSingularRisk,
    pub matrix_scale: SnapshotMatrixScale,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SnapshotArtifactEnvelope {
    version: u8,
    format: String,
    snapshot_id: Uuid,
    config: SnapshotBuildConfig,
    coverage: SnapshotCoverageReport,
    payload: ModelSparseData,
}

/// Encoded snapshot artifact bytes and metadata.
#[derive(Debug, Clone)]
pub struct EncodedSnapshotArtifact {
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub byte_size: usize,
    pub format: &'static str,
    pub content_type: &'static str,
    pub extension: &'static str,
}

/// Decoded snapshot artifact payload.
#[derive(Debug, Clone)]
pub struct DecodedSnapshotArtifact {
    pub snapshot_id: Uuid,
    pub config: SnapshotBuildConfig,
    pub coverage: SnapshotCoverageReport,
    pub payload: ModelSparseData,
}

/// Encodes one snapshot matrix payload into `HDF5`.
pub fn encode_snapshot_artifact(
    snapshot_id: Uuid,
    config: SnapshotBuildConfig,
    coverage: SnapshotCoverageReport,
    payload: &ModelSparseData,
) -> anyhow::Result<EncodedSnapshotArtifact> {
    let envelope = SnapshotArtifactEnvelope {
        version: SCHEMA_VERSION,
        format: SNAPSHOT_ARTIFACT_FORMAT.to_owned(),
        snapshot_id,
        config,
        coverage,
        payload: payload.clone(),
    };

    let json = serde_json::to_vec(&envelope)?;
    let temp = Builder::new()
        .prefix("lca-snapshot-artifact-")
        .suffix(".h5")
        .tempfile()?;
    write_hdf5_file(temp.path(), json.as_slice())?;
    let bytes = std::fs::read(temp.path())?;

    let mut hasher = Sha256::new();
    hasher.update(bytes.as_slice());
    let sha256 = format!("{:x}", hasher.finalize());

    Ok(EncodedSnapshotArtifact {
        byte_size: bytes.len(),
        bytes,
        sha256,
        format: SNAPSHOT_ARTIFACT_FORMAT,
        content_type: SNAPSHOT_ARTIFACT_CONTENT_TYPE,
        extension: SNAPSHOT_ARTIFACT_EXTENSION,
    })
}

/// Decodes snapshot artifact bytes into sparse payload.
pub fn decode_snapshot_artifact(bytes: &[u8]) -> anyhow::Result<DecodedSnapshotArtifact> {
    let temp = Builder::new()
        .prefix("lca-snapshot-artifact-read-")
        .suffix(".h5")
        .tempfile()?;
    std::fs::write(temp.path(), bytes)?;

    let file = File::open(temp.path())?;
    let format_bytes = file
        .dataset(DATASET_FORMAT)?
        .read_1d::<u8>()?
        .into_raw_vec();
    let format = String::from_utf8(format_bytes)?;
    if format != SNAPSHOT_ARTIFACT_FORMAT {
        return Err(anyhow::anyhow!(
            "unsupported snapshot artifact format: {format}"
        ));
    }

    let envelope_bytes = file
        .dataset(DATASET_ENVELOPE_JSON)?
        .read_1d::<u8>()?
        .into_raw_vec();
    let envelope: SnapshotArtifactEnvelope = serde_json::from_slice(&envelope_bytes)?;
    if envelope.payload.model_version != envelope.snapshot_id {
        return Err(anyhow::anyhow!(
            "snapshot payload model_version mismatch: payload={} envelope={}",
            envelope.payload.model_version,
            envelope.snapshot_id
        ));
    }

    Ok(DecodedSnapshotArtifact {
        snapshot_id: envelope.snapshot_id,
        config: envelope.config,
        coverage: envelope.coverage,
        payload: envelope.payload,
    })
}

fn write_hdf5_file(path: &Path, envelope_json: &[u8]) -> anyhow::Result<()> {
    let file = File::create(path)?;
    file.new_dataset_builder()
        .with_data(&[SCHEMA_VERSION])
        .create(DATASET_SCHEMA_VERSION)?;
    file.new_dataset_builder()
        .with_data(SNAPSHOT_ARTIFACT_FORMAT.as_bytes())
        .create(DATASET_FORMAT)?;
    file.new_dataset_builder()
        .with_data(envelope_json)
        .create(DATASET_ENVELOPE_JSON)?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use solver_core::{ModelSparseData, SparseTriplet};

    use super::{
        SNAPSHOT_ARTIFACT_FORMAT, SnapshotBuildConfig, SnapshotCoverageReport,
        SnapshotMatchingCoverage, SnapshotMatrixScale, SnapshotSingularRisk,
        decode_snapshot_artifact, encode_snapshot_artifact,
    };

    #[test]
    fn encode_decode_snapshot_artifact_roundtrip() {
        let snapshot_id = uuid::Uuid::new_v4();
        let config = SnapshotBuildConfig {
            process_states: "100".to_owned(),
            process_limit: 0,
            provider_rule: "strict_unique_provider".to_owned(),
            self_loop_cutoff: 0.999_999,
            singular_eps: 1e-12,
            has_lcia: true,
            method_id: Some(uuid::Uuid::new_v4()),
            method_version: Some("01.00.000".to_owned()),
        };
        let coverage = SnapshotCoverageReport {
            matching: SnapshotMatchingCoverage {
                input_edges_total: 10,
                matched_unique_provider: 7,
                matched_multi_provider: 2,
                unmatched_no_provider: 1,
                unique_provider_match_pct: 70.0,
                any_provider_match_pct: 90.0,
            },
            singular_risk: SnapshotSingularRisk {
                risk_level: "low".to_owned(),
                prefilter_diag_abs_ge_cutoff: 0,
                postfilter_a_diag_abs_ge_cutoff: 0,
                m_zero_diagonal_count: 0,
                m_min_abs_diagonal: 1.0,
            },
            matrix_scale: SnapshotMatrixScale {
                process_count: 2,
                flow_count: 2,
                impact_count: 1,
                a_nnz: 2,
                b_nnz: 2,
                c_nnz: 1,
                m_nnz_estimated: 4,
                m_sparsity_estimated: 0.0,
            },
        };
        let payload = ModelSparseData {
            model_version: snapshot_id,
            process_count: 2,
            flow_count: 2,
            impact_count: 1,
            technosphere_entries: vec![
                SparseTriplet {
                    row: 0,
                    col: 1,
                    value: 0.1,
                },
                SparseTriplet {
                    row: 1,
                    col: 0,
                    value: 0.2,
                },
            ],
            biosphere_entries: vec![
                SparseTriplet {
                    row: 0,
                    col: 0,
                    value: 1.0,
                },
                SparseTriplet {
                    row: 1,
                    col: 1,
                    value: -2.0,
                },
            ],
            characterization_factors: vec![SparseTriplet {
                row: 0,
                col: 1,
                value: 3.5,
            }],
        };

        let encoded =
            encode_snapshot_artifact(snapshot_id, config.clone(), coverage.clone(), &payload)
                .expect("encode");
        assert_eq!(encoded.format, SNAPSHOT_ARTIFACT_FORMAT);
        assert_eq!(encoded.byte_size, encoded.bytes.len());

        let decoded = decode_snapshot_artifact(encoded.bytes.as_slice()).expect("decode");
        assert_eq!(decoded.snapshot_id, snapshot_id);
        assert_eq!(decoded.config, config);
        assert_eq!(decoded.coverage, coverage);
        assert_eq!(decoded.payload, payload);
    }
}
