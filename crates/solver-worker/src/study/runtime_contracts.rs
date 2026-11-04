//! Service-issued acquisition format and private native-child protocol. These are not public API.
use super::contracts::{AmountOverride, Bounds, Cutoff, DatasetRef, SourceInput, StudyBinding};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SNAPSHOT_SCHEMA: &str = "lca.study-source-snapshot.v1";
pub const CHILD_SCHEMA: &str = "lca.study-native.request.v1";
pub const JOB_RECEIPT_SCHEMA: &str = "lca.study-job-receipt.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSnapshot {
    pub schema_version: String,
    pub origin_ref: Value,
    pub license_ref: String,
    pub issuer_id: String,
    pub observed_at: String,
    pub source_authorization: Value,
    pub source: SourceInput,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum NativeAction {
    Probe,
    Acquire {
        snapshot_path: String,
    },
    PrepareBase {
        snapshot_path: String,
        method_refs: Vec<DatasetRef>,
        cutoffs: Vec<Cutoff>,
        static_cache_directory: Option<String>,
        static_cache_base_url: Option<String>,
    },
    PrepareScenario {
        snapshot_path: String,
        base_prepared_path: String,
        base_prepared_sha256: String,
        study_binding: StudyBinding,
        overrides: Vec<AmountOverride>,
    },
    Run {
        prepared_path: String,
        prepared_sha256: String,
    },
    Analyze {
        analysis_path: String,
    },
    AnalyzeAdvanced {
        analysis_path: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRequest {
    pub schema_version: String,
    pub limits: Bounds,
    pub output_path: String,
    pub operation: NativeAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactRef {
    pub r#type: String,
    pub id: uuid::Uuid,
    pub content_hash: String,
    pub digest_scope: String,
    pub byte_size: u64,
    pub media_type: String,
    pub schema_version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateArtifact {
    pub study_id: uuid::Uuid,
    pub artifact_ref: ArtifactRef,
    pub storage_bucket: String,
    pub storage_object_path: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparationSource {
    pub schema_version: String,
    pub source_snapshot: Value,
    pub source_artifact: PrivateArtifact,
    pub base_prepared_artifact: Option<PrivateArtifact>,
    pub base_prepared_preparation_id: Option<uuid::Uuid>,
    pub study_binding: Option<StudyBinding>,
    pub overrides: Vec<AmountOverride>,
    pub cutoffs: Vec<Cutoff>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeErrorKind {
    SourceInvalid,
    BoundaryUndeclared,
    NormalizationUnderflow,
    NormalizationOverflow,
    NumericInvalid,
    ResourceLimit,
    ModelNotSolvable,
    ExecutionFailed,
}
impl NativeErrorKind {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceInvalid => "study_source_invalid",
            Self::BoundaryUndeclared => "study_boundary_undeclared",
            Self::NormalizationUnderflow => "study_normalization_underflow",
            Self::NormalizationOverflow => "study_normalization_overflow",
            Self::NumericInvalid => "study_numeric_invalid",
            Self::ResourceLimit => "study_resource_limit",
            Self::ModelNotSolvable => "study_model_not_solvable",
            Self::ExecutionFailed => "study_native_execution_failed",
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeErrorEnvelope {
    pub schema_version: String,
    pub kind: NativeErrorKind,
}
