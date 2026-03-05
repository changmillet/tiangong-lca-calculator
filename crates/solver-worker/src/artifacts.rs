use std::path::Path;

use hdf5::File;
use serde::Serialize;
use sha2::{Digest, Sha256};
use solver_core::{SolveBatchResult, SolveResult};
use tempfile::Builder;
use uuid::Uuid;

const SCHEMA_VERSION: u8 = 1;
const DATASET_SCHEMA_VERSION: &str = "schema_version";
const DATASET_FORMAT: &str = "format";
const DATASET_ENVELOPE_JSON: &str = "envelope_json";
const HDF5_DEFLATE_LEVEL: u8 = 4;
const HDF5_CHUNK_TARGET_BYTES: usize = 256 * 1024;

/// `HDF5` result artifact format persisted in object storage.
pub const ARTIFACT_FORMAT: &str = "hdf5:v1";
/// File extension for `HDF5` artifacts.
pub const ARTIFACT_EXTENSION: &str = "h5";
/// MIME type used for uploads.
pub const ARTIFACT_CONTENT_TYPE: &str = "application/x-hdf5";

#[derive(Debug, Clone, Serialize)]
struct ArtifactEnvelope<T>
where
    T: Serialize,
{
    version: u8,
    format: &'static str,
    snapshot_id: Uuid,
    job_id: Uuid,
    payload: T,
}

/// Encoded artifact bytes and checksums.
#[derive(Debug, Clone)]
pub struct EncodedArtifact {
    /// Artifact bytes in `HDF5` container format.
    pub bytes: Vec<u8>,
    /// `SHA-256` checksum in hex.
    pub sha256: String,
    /// Format identifier.
    pub format: &'static str,
    /// Content type for upload.
    pub content_type: &'static str,
    /// Recommended file extension.
    pub extension: &'static str,
}

/// Encodes one solve result as `HDF5`.
pub fn encode_solve_one_artifact(
    snapshot_id: Uuid,
    job_id: Uuid,
    result: &SolveResult,
) -> anyhow::Result<EncodedArtifact> {
    let envelope = ArtifactEnvelope {
        version: SCHEMA_VERSION,
        format: ARTIFACT_FORMAT,
        snapshot_id,
        job_id,
        payload: result,
    };
    encode(&envelope)
}

/// Encodes batch solve result as `HDF5`.
pub fn encode_solve_batch_artifact(
    snapshot_id: Uuid,
    job_id: Uuid,
    result: &SolveBatchResult,
) -> anyhow::Result<EncodedArtifact> {
    let envelope = ArtifactEnvelope {
        version: SCHEMA_VERSION,
        format: ARTIFACT_FORMAT,
        snapshot_id,
        job_id,
        payload: result,
    };
    encode(&envelope)
}

fn encode<T>(value: &T) -> anyhow::Result<EncodedArtifact>
where
    T: Serialize,
{
    let json = serde_json::to_vec(value)?;
    let temp = Builder::new()
        .prefix("lca-artifact-")
        .suffix(".h5")
        .tempfile()?;

    write_hdf5_file(temp.path(), json.as_slice())?;
    let bytes = std::fs::read(temp.path())?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = format!("{:x}", hasher.finalize());

    Ok(EncodedArtifact {
        bytes,
        sha256: digest,
        format: ARTIFACT_FORMAT,
        content_type: ARTIFACT_CONTENT_TYPE,
        extension: ARTIFACT_EXTENSION,
    })
}

fn write_hdf5_file(path: &Path, envelope_json: &[u8]) -> anyhow::Result<()> {
    let file = File::create(path)?;

    file.new_dataset_builder()
        .with_data(&[SCHEMA_VERSION])
        .create(DATASET_SCHEMA_VERSION)?;
    file.new_dataset_builder()
        .with_data(ARTIFACT_FORMAT.as_bytes())
        .create(DATASET_FORMAT)?;
    if !hdf5::filters::deflate_available() {
        return Err(anyhow::anyhow!(
            "HDF5 deflate filter is unavailable; zlib-enabled HDF5 is required"
        ));
    }
    let chunk_len = envelope_json.len().clamp(1, HDF5_CHUNK_TARGET_BYTES);
    file.new_dataset_builder()
        .chunk((chunk_len,))
        .deflate(HDF5_DEFLATE_LEVEL)
        .with_data(envelope_json)
        .create(DATASET_ENVELOPE_JSON)?;

    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use hdf5::File;
    use hdf5::filters::Filter;
    use serde::Deserialize;
    use solver_core::{FactorizationState, SolveBatchResult, SolveResult};
    use tempfile::Builder;
    use uuid::Uuid;

    use super::{
        ARTIFACT_CONTENT_TYPE, ARTIFACT_EXTENSION, ARTIFACT_FORMAT, DATASET_ENVELOPE_JSON,
        DATASET_FORMAT, DATASET_SCHEMA_VERSION, HDF5_DEFLATE_LEVEL, encode_solve_batch_artifact,
        encode_solve_one_artifact,
    };

    #[derive(Debug, Deserialize)]
    struct EnvelopeOne {
        version: u8,
        format: String,
        snapshot_id: Uuid,
        job_id: Uuid,
        payload: SolveResult,
    }

    #[derive(Debug, Deserialize)]
    struct EnvelopeBatch {
        version: u8,
        format: String,
        snapshot_id: Uuid,
        job_id: Uuid,
        payload: SolveBatchResult,
    }

    #[test]
    fn encode_solve_one_artifact_metadata_and_payload_roundtrip() {
        let solved = SolveResult {
            x: Some(vec![1.0, 2.0]),
            g: Some(vec![3.0]),
            h: None,
            factorization_state: FactorizationState::Ready,
        };

        let encoded =
            encode_solve_one_artifact(Uuid::nil(), Uuid::nil(), &solved).expect("encode artifact");

        assert_eq!(encoded.format, ARTIFACT_FORMAT);
        assert_eq!(encoded.content_type, ARTIFACT_CONTENT_TYPE);
        assert_eq!(encoded.extension, ARTIFACT_EXTENSION);
        assert_eq!(encoded.sha256.len(), 64);
        assert!(encoded.sha256.chars().all(|ch| ch.is_ascii_hexdigit()));

        let file = write_and_open_hdf5(&encoded.bytes);
        let format_bytes = file
            .dataset(DATASET_FORMAT)
            .expect("format dataset")
            .read_1d::<u8>()
            .expect("read format bytes")
            .to_vec();
        assert_eq!(
            String::from_utf8(format_bytes).expect("utf8 format"),
            ARTIFACT_FORMAT
        );

        let schema_version = file
            .dataset(DATASET_SCHEMA_VERSION)
            .expect("schema_version dataset")
            .read_1d::<u8>()
            .expect("read schema version")
            .to_vec();
        assert_eq!(schema_version, vec![1_u8]);

        let envelope_bytes = file
            .dataset(DATASET_ENVELOPE_JSON)
            .expect("envelope_json dataset")
            .read_1d::<u8>()
            .expect("read envelope_json")
            .to_vec();
        let envelope: EnvelopeOne =
            serde_json::from_slice(&envelope_bytes).expect("parse envelope");
        let envelope_ds = file
            .dataset(DATASET_ENVELOPE_JSON)
            .expect("envelope_json dataset");
        assert!(envelope_ds.is_chunked());
        let filters = envelope_ds.filters();
        assert!(filters.iter().any(
            |filter| matches!(filter, Filter::Deflate(level) if *level == HDF5_DEFLATE_LEVEL)
        ));

        assert_eq!(envelope.version, 1);
        assert_eq!(envelope.format, ARTIFACT_FORMAT);
        assert_eq!(envelope.snapshot_id, Uuid::nil());
        assert_eq!(envelope.job_id, Uuid::nil());
        assert!((envelope.payload.x.as_ref().expect("x exists")[0] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn encode_solve_batch_artifact_metadata_and_payload_roundtrip() {
        let solved = SolveBatchResult {
            items: vec![SolveResult {
                x: Some(vec![1.0]),
                g: None,
                h: Some(vec![2.0]),
                factorization_state: FactorizationState::Ready,
            }],
        };

        let encoded =
            encode_solve_batch_artifact(Uuid::nil(), Uuid::nil(), &solved).expect("encode batch");

        assert_eq!(encoded.format, ARTIFACT_FORMAT);
        assert_eq!(encoded.content_type, ARTIFACT_CONTENT_TYPE);
        assert_eq!(encoded.extension, ARTIFACT_EXTENSION);
        assert_eq!(encoded.sha256.len(), 64);

        let file = write_and_open_hdf5(&encoded.bytes);
        let envelope_bytes = file
            .dataset(DATASET_ENVELOPE_JSON)
            .expect("envelope_json dataset")
            .read_1d::<u8>()
            .expect("read envelope_json")
            .to_vec();
        let envelope: EnvelopeBatch =
            serde_json::from_slice(&envelope_bytes).expect("parse envelope");

        assert_eq!(envelope.version, 1);
        assert_eq!(envelope.format, ARTIFACT_FORMAT);
        assert_eq!(envelope.snapshot_id, Uuid::nil());
        assert_eq!(envelope.job_id, Uuid::nil());
        assert!(
            (envelope.payload.items[0].h.as_ref().expect("h exists")[0] - 2.0).abs() < f64::EPSILON
        );
    }

    fn write_and_open_hdf5(bytes: &[u8]) -> File {
        let temp = Builder::new()
            .prefix("lca-artifact-test-")
            .suffix(".h5")
            .tempfile()
            .expect("create tempfile");
        std::fs::write(temp.path(), bytes).expect("write hdf5 bytes");
        File::open(temp.path()).expect("open hdf5 file")
    }
}
