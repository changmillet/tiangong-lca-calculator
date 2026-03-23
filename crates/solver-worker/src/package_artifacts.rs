use std::{fs::File, io::Read, path::Path};

use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    artifacts::EncodedArtifact,
    package_types::{
        PACKAGE_EXPORT_REPORT_ARTIFACT_FORMAT, PACKAGE_IMPORT_REPORT_ARTIFACT_FORMAT,
        PACKAGE_REPORT_CONTENT_TYPE, PACKAGE_ZIP_ARTIFACT_FORMAT, PACKAGE_ZIP_CONTENT_TYPE,
    },
};

const PACKAGE_SCHEMA_VERSION: u8 = 1;
/// File extension for package ZIP artifacts.
pub const PACKAGE_ZIP_EXTENSION: &str = "zip";
/// File extension for package JSON report artifacts.
pub const PACKAGE_REPORT_EXTENSION: &str = "json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageArtifactUploadMeta {
    pub sha256: String,
    pub format: &'static str,
    pub content_type: &'static str,
    pub extension: &'static str,
    pub byte_size: u64,
}

#[derive(Debug, Clone, Serialize)]
struct PackageReportEnvelope<T>
where
    T: Serialize,
{
    version: u8,
    format: &'static str,
    job_id: Uuid,
    payload: T,
}

/// Encodes an export summary/report artifact as stable JSON bytes.
pub fn encode_export_report_artifact<T: Serialize>(
    job_id: Uuid,
    report: &T,
) -> anyhow::Result<EncodedArtifact> {
    encode_report_artifact(job_id, report, PACKAGE_EXPORT_REPORT_ARTIFACT_FORMAT)
}

/// Encodes an import summary/conflict report artifact as stable JSON bytes.
pub fn encode_import_report_artifact<T: Serialize>(
    job_id: Uuid,
    report: &T,
) -> anyhow::Result<EncodedArtifact> {
    encode_report_artifact(job_id, report, PACKAGE_IMPORT_REPORT_ARTIFACT_FORMAT)
}

/// Wraps generated ZIP bytes with the storage metadata expected by the package pipeline.
#[must_use]
pub fn encode_package_zip_artifact(bytes: Vec<u8>) -> EncodedArtifact {
    EncodedArtifact {
        sha256: checksum_sha256(bytes.as_slice()),
        bytes,
        format: PACKAGE_ZIP_ARTIFACT_FORMAT,
        content_type: PACKAGE_ZIP_CONTENT_TYPE,
        extension: PACKAGE_ZIP_EXTENSION,
    }
}

pub fn package_artifact_meta_from_encoded(
    encoded: &EncodedArtifact,
) -> anyhow::Result<PackageArtifactUploadMeta> {
    Ok(PackageArtifactUploadMeta {
        sha256: encoded.sha256.clone(),
        format: encoded.format,
        content_type: encoded.content_type,
        extension: encoded.extension,
        byte_size: u64::try_from(encoded.bytes.len())
            .map_err(|_| anyhow::anyhow!("artifact size exceeds u64"))?,
    })
}

pub fn prepare_package_zip_artifact_from_path(
    path: &Path,
) -> anyhow::Result<PackageArtifactUploadMeta> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut byte_size = 0_u64;

    loop {
        let read_len = file.read(buffer.as_mut_slice())?;
        if read_len == 0 {
            break;
        }
        hasher.update(&buffer[..read_len]);
        byte_size = byte_size
            .checked_add(u64::try_from(read_len).map_err(|_| anyhow::anyhow!("read overflow"))?)
            .ok_or_else(|| anyhow::anyhow!("artifact size exceeds u64"))?;
    }

    Ok(PackageArtifactUploadMeta {
        sha256: format!("{:x}", hasher.finalize()),
        format: PACKAGE_ZIP_ARTIFACT_FORMAT,
        content_type: PACKAGE_ZIP_CONTENT_TYPE,
        extension: PACKAGE_ZIP_EXTENSION,
        byte_size,
    })
}

fn encode_report_artifact<T: Serialize>(
    job_id: Uuid,
    report: &T,
    format: &'static str,
) -> anyhow::Result<EncodedArtifact> {
    let envelope = PackageReportEnvelope {
        version: PACKAGE_SCHEMA_VERSION,
        format,
        job_id,
        payload: report,
    };
    let bytes = serde_json::to_vec(&envelope)?;

    Ok(EncodedArtifact {
        sha256: checksum_sha256(bytes.as_slice()),
        bytes,
        format,
        content_type: PACKAGE_REPORT_CONTENT_TYPE,
        extension: PACKAGE_REPORT_EXTENSION,
    })
}

fn checksum_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{
        PACKAGE_REPORT_EXTENSION, PACKAGE_ZIP_EXTENSION, encode_export_report_artifact,
        encode_import_report_artifact, encode_package_zip_artifact,
        package_artifact_meta_from_encoded, prepare_package_zip_artifact_from_path,
    };
    use crate::package_types::{
        PACKAGE_EXPORT_REPORT_ARTIFACT_FORMAT, PACKAGE_IMPORT_REPORT_ARTIFACT_FORMAT,
        PACKAGE_REPORT_CONTENT_TYPE, PACKAGE_ZIP_ARTIFACT_FORMAT, PACKAGE_ZIP_CONTENT_TYPE,
    };

    #[test]
    fn encode_export_report_artifact_wraps_payload() {
        let job_id = Uuid::nil();
        let report = json!({
            "scope": "current_user",
            "root_count": 2
        });

        let encoded = encode_export_report_artifact(job_id, &report).expect("encode export report");
        let parsed: Value = serde_json::from_slice(encoded.bytes.as_slice()).expect("parse json");

        assert_eq!(encoded.format, PACKAGE_EXPORT_REPORT_ARTIFACT_FORMAT);
        assert_eq!(encoded.content_type, PACKAGE_REPORT_CONTENT_TYPE);
        assert_eq!(encoded.extension, PACKAGE_REPORT_EXTENSION);
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["format"], PACKAGE_EXPORT_REPORT_ARTIFACT_FORMAT);
        assert_eq!(parsed["job_id"], job_id.to_string());
        assert_eq!(parsed["payload"]["root_count"], 2);
    }

    #[test]
    fn encode_import_report_artifact_wraps_payload() {
        let job_id = Uuid::nil();
        let report = json!({
            "status": "rejected",
            "duplicates": 3
        });

        let encoded = encode_import_report_artifact(job_id, &report).expect("encode import report");
        let parsed: Value = serde_json::from_slice(encoded.bytes.as_slice()).expect("parse json");

        assert_eq!(encoded.format, PACKAGE_IMPORT_REPORT_ARTIFACT_FORMAT);
        assert_eq!(encoded.content_type, PACKAGE_REPORT_CONTENT_TYPE);
        assert_eq!(parsed["payload"]["status"], "rejected");
        assert_eq!(parsed["payload"]["duplicates"], 3);
    }

    #[test]
    fn encode_report_artifacts_are_deterministic() {
        let job_id = Uuid::nil();
        let report = json!({
            "scope": "selected_roots",
            "items": ["processes", "flows"]
        });

        let first = encode_export_report_artifact(job_id, &report).expect("encode first report");
        let second = encode_export_report_artifact(job_id, &report).expect("encode second report");

        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.bytes, second.bytes);
    }

    #[test]
    fn encode_package_zip_artifact_sets_zip_metadata() {
        let encoded = encode_package_zip_artifact(b"zip-bytes".to_vec());

        assert_eq!(encoded.format, PACKAGE_ZIP_ARTIFACT_FORMAT);
        assert_eq!(encoded.content_type, PACKAGE_ZIP_CONTENT_TYPE);
        assert_eq!(encoded.extension, PACKAGE_ZIP_EXTENSION);
        assert!(!encoded.sha256.is_empty());
    }

    #[test]
    fn package_artifact_meta_from_encoded_tracks_byte_size() {
        let encoded = encode_package_zip_artifact(b"zip-bytes".to_vec());
        let meta = package_artifact_meta_from_encoded(&encoded).expect("build artifact meta");

        assert_eq!(meta.byte_size, 9);
        assert_eq!(meta.format, PACKAGE_ZIP_ARTIFACT_FORMAT);
        assert_eq!(meta.content_type, PACKAGE_ZIP_CONTENT_TYPE);
    }

    #[test]
    fn prepare_package_zip_artifact_from_path_reads_sha_and_size() {
        let temp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(temp.path(), b"zip-bytes").expect("write zip bytes");

        let meta =
            prepare_package_zip_artifact_from_path(temp.path()).expect("read package zip meta");

        assert_eq!(meta.byte_size, 9);
        assert_eq!(meta.format, PACKAGE_ZIP_ARTIFACT_FORMAT);
        assert_eq!(meta.extension, PACKAGE_ZIP_EXTENSION);
        assert!(!meta.sha256.is_empty());
    }
}
