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
}
