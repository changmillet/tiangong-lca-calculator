//! Distinct method-job and evidence records. Recording never verifies a professional claim.
use super::{
    analysis_contracts::Execution,
    contracts::DatasetRef,
    method_assessment_contracts::{Config, DeclaredEvidenceKind, ServiceInput},
    runtime_contracts::ArtifactRef,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const JOB_KIND: &str = "study.method_assessment";
pub const JOB_SCHEMA: &str = "study.method_assessment.request.v1";
pub const CONTEXT_SCHEMA: &str = "lca.study-method-job-context.v1";
pub const NATIVE_SCHEMA: &str = "lca.study-method-native.v1";
pub const EVIDENCE_SCHEMA: &str = "lca.study-method-evidence.v1";
pub const EVIDENCE_RECORD_SCHEMA: &str = "lca.study-method-evidence-record.v1";
pub const MAX_FACTS_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EVIDENCE_BYTES: usize = 65_536;
pub const READ_INPUT_RPC: &str = "svc_study_method_assessment_read_input_v1";
pub const COMPLETE_RPC: &str = "svc_study_method_assessment_complete_v1";
pub const FAIL_RPC: &str = "svc_study_method_assessment_fail_v1";
pub const RECONCILE_RPC: &str = "svc_study_method_assessment_reconcile_v1";

macro_rules! record {
    ($name:ident {$($field:ident: $kind:ty),* $(,)?}) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $(pub $field: $kind),* }
    };
}
record!(JobRequest {
    schema_version: String,
    study_id: Uuid,
    assessment_id: Uuid
});
record!(Budget {
    max_selected_factor_records: usize,
    max_facts_bytes: usize
});
record!(JobContext {
    schema_version: String,
    input: ServiceInput,
    budget: Budget
});
record!(NativeInput {
    schema_version: String,
    context: JobContext,
    snapshot_path: String,
    prepared_path: String,
    result_path: String,
    static_cache_directory: Option<String>,
    static_cache_base_url: Option<String>
});
record!(PublicAssessmentRecord {
    assessment_id: Uuid,
    study_id: Uuid,
    profile_id: String,
    config: Config,
    config_hash: String,
    execution: Execution,
    facts_ref: Option<ArtifactRef>
});
record!(Completion {
    profile_id: String,
    config_hash: String,
    selected_factor_record_count: usize,
    facts_ref: ArtifactRef,
    storage_bucket: String,
    storage_object_path: String
});
record!(FailureRequest {
    resource_type: String,
    resource_id: Uuid,
    job_id: Uuid,
    lease_token: Uuid,
    error_code: String,
    idempotency_key: String
});
record!(ReconcileRequest {
    resource_type: String,
    resource_id: Uuid,
    job_id: Uuid,
    lease_token: Uuid,
    idempotency_key: String,
    artifact_id: Option<Uuid>
});
record!(ReconciledResource {
    r#type: String,
    value: PublicAssessmentRecord
});
record!(ReconcileResult {
    state: String,
    resource: Option<ReconciledResource>,
    job_status: String
});
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CitationKind {
    NormativeSource,
    DatasetRevision,
    Document,
    ProfessionalRecord,
    OtherDeclaredSource,
}
record!(DeclaredCitation {
    kind: CitationKind,
    identity: String,
    version: String,
    content_hash: Option<String>,
    locator: String
});
record!(EvidenceBody {
    schema_version: String,
    requirement_id: String,
    declared_kind: DeclaredEvidenceKind,
    title: String,
    statement: String,
    citations: Vec<DeclaredCitation>
});
record!(EvidenceRecord {
    schema_version: String,
    evidence_id: Uuid,
    study_id: Uuid,
    requirement_id: String,
    declared_kind: DeclaredEvidenceKind,
    artifact_ref: ArtifactRef,
    recorded_at: String,
    status: String
});

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FactContext {
    CharacterisedCategory {
        method_ref: DatasetRef,
        category_role: String,
    },
    NormalisedCategory {
        method_ref: DatasetRef,
    },
    WeightedCategory {
        method_ref: DatasetRef,
    },
    CandidateSingleScore {
        primary_methods: Vec<DatasetRef>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum UnitIdentity {
    Characterised {
        method_ref: DatasetRef,
        reference_quantity_id: String,
        unit: String,
    },
    Normalised {
        method_ref: DatasetRef,
        reference_quantity_id: String,
        unit: String,
        source_normalisation_factor: String,
        module_artifact_sha256: String,
    },
    Weighted {
        method_ref: DatasetRef,
        reference_quantity_id: String,
        unit: String,
        source_normalisation_factor: String,
        source_weight_fraction: String,
        point_definition: String,
        scaling_factor: String,
        module_artifact_sha256: String,
    },
    CandidateScore {
        primary_methods: Vec<DatasetRef>,
        unit: String,
        point_definition: String,
        scaling_factor: String,
        module_artifact_sha256: String,
    },
}
record!(ReportFact {
    fact_id: String,
    assessment_id: Uuid,
    baseline_run_id: Uuid,
    context: FactContext,
    method_ref: Option<DatasetRef>,
    canonical_value: Option<String>,
    unit: String,
    unit_identity: UnitIdentity,
    coverage: String,
    reason_codes: Vec<String>
});
