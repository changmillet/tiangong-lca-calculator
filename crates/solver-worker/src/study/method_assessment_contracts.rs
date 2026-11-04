//! Independent source-bound method assessment. No record grants authority or approval.
use super::{
    analysis_contracts::{Execution, RunInput},
    contracts::{CalculationRef, CoverageGap, DatasetRef, FunctionalUnit, MethodAxis},
    runtime_contracts::{ArtifactRef, PrivateArtifact},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const CONFIG_SCHEMA: &str = "lca.study-method-assessment-config.v1";
pub const INPUT_SCHEMA: &str = "lca.study-method-assessment-input.v1";
pub const CANDIDATE_INPUT_SCHEMA: &str = "lca.study-method-candidate-input.v1";
pub const FACTS_SCHEMA: &str = "lca.study-method-assessment-facts.v1";
pub const PROFILE: &str = "lca.study-method-pef-2021-ef31-cement-intermediate.v1";
pub const ASSESSOR: &str = "study.method-assessment.v1";
pub const MAX_EVIDENCE_REFS: usize = 128;
pub const MAX_SELECTED_FACTOR_RECORDS: usize = 8192;

macro_rules! record {
    ($name:ident {$($field:ident: $kind:ty),* $(,)?}) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $(pub $field: $kind),* }
    };
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProductRole {
    FinishedCementIntermediate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StudyPhase {
    Screening,
    FinalStudy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnitBasis {
    DeclaredMass,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CategoryRuleDeclaration {
    NoneEstablished,
    CommissionDeveloped {
        reference_id: String,
        version: String,
        evidence_id: Uuid,
    },
}
record!(ModuleRef {
    repository: String,
    source_commit: String,
    module_id: String,
    module_version: String,
    artifact_sha256: String,
    review_basis_sha256: String,
    source_bindings_sha256: String
});
record!(Declarations {
    product_role: ProductRole,
    study_phase: StudyPhase,
    reference_unit_basis: UnitBasis,
    category_rule: CategoryRuleDeclaration
});
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeclaredEvidenceKind {
    SourceRecord,
    ApplicabilityRecord,
    ProfessionalRecord,
}
record!(EvidenceReference {
    requirement_id: String,
    declared_kind: DeclaredEvidenceKind,
    artifact_ref: ArtifactRef
});
record!(Config {
    schema_version: String,
    baseline_run_id: String,
    module_ref: ModuleRef,
    declarations: Declarations,
    evidence_refs: Vec<EvidenceReference>
});
record!(AssessmentRecord {
    assessment_id: Uuid,
    study_id: Uuid,
    profile_id: String,
    config: Config,
    config_hash: String,
    execution: Execution
});
record!(CandidateInput {
    schema_version: String,
    candidate_id: Uuid,
    study_id: Uuid,
    config: Config,
    config_hash: String,
    baseline: RunInput,
    source_snapshot: Value,
    source_artifact_ref: ArtifactRef
});
record!(ServiceInput {
    schema_version: String,
    assessment: AssessmentRecord,
    baseline: RunInput,
    source_snapshot: Value,
    source_artifact: PrivateArtifact
});
record!(ModelRef {
    r#type: String,
    model_revision_id: String,
    content_hash: String,
    digest_scope: String
});
record!(Validity {
    status: String,
    checked_at: String,
    reason_codes: Vec<String>
});
record!(BaselineBinding {
    run_id: Uuid,
    study_revision_id: Uuid,
    scenario_revision_id: Uuid,
    model_preparation_id: Uuid,
    input_hash: String,
    effective_model_ref: ModelRef,
    prepared_artifact_ref: ArtifactRef,
    result_ref: ArtifactRef,
    source_snapshot_ref: ArtifactRef,
    source_origin: DatasetRef,
    calculation_contract_ref: CalculationRef,
    method_source_hash: String,
    validity: Validity
});
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineStatus {
    Verified,
    Violated,
    NotVerified,
    Unsupported,
    NotAssessed,
}
record!(MachineCheck {
    predicate_id: String,
    status: MachineStatus,
    reason_codes: Vec<String>
});
record!(MachineAssessment {
    status: MachineStatus,
    scope: String,
    checks: Vec<MachineCheck>
});
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfessionalStatus {
    Pending,
    ReferencesRecordedUnverified,
    NotRequiredForSourceMachinePredicate,
}
record!(ProfessionalAssessment {
    status: ProfessionalStatus,
    evidence_refs: Vec<EvidenceReference>,
    reason_codes: Vec<String>
});
record!(RequirementAssessment {
    requirement_id: String,
    source_applicability: String,
    source_check_kind: String,
    source_citations: Vec<String>,
    declaration_state: String,
    machine: MachineAssessment,
    professional: ProfessionalAssessment
});
record!(CategoryAssessment {
    method_ref: DatasetRef,
    name: String,
    role: String,
    parent_method_id: Option<String>,
    reference_quantity_id: String,
    unit: String,
    selected: bool,
    source_compatible: bool,
    coverage: String,
    canonical_characterised_value: Option<String>,
    missing_inventory_indices: Vec<usize>,
    normalisation_weighting_included: bool,
    reason_codes: Vec<String>
});
record!(SourceLocationEvidence {
    status: String,
    original_value: Option<String>,
    source_record_ordinal: Option<usize>,
    source_record_sha256: Option<String>
});
record!(SelectedFactorEvidence {
    method_ref: DatasetRef,
    inventory_index: usize,
    flow: DatasetRef,
    direction: String,
    frozen_unit: String,
    frozen_unit_support: Vec<DatasetRef>,
    canonical_effective_factor: Option<String>,
    raw_factor_value_bits: Option<String>,
    alternative_count: usize,
    location: SourceLocationEvidence,
    status: String
});
record!(SourceBridge {
    method_ref: DatasetRef,
    official_xml_sha256: String,
    local_xml_sha256: String,
    equivalence_kind: String,
    equivalence_evidence_sha256: Option<String>
});
record!(MethodSourceEvidence {
    static_source_snapshot: Value,
    normative_version: String,
    factor_package_version: String,
    interim_guidance_snapshot: Value,
    source_catalog_sha256: String,
    nonregional_policy: String,
    nonregional_defaults_sha256: String,
    official_factor_package_sha256: String,
    normalisation_weighting_source_sha256: String,
    frozen_c_matches_verified_bundle: bool,
    geographical_suitability: String,
    source_bridges: Vec<SourceBridge>,
    selected_factors: Vec<SelectedFactorEvidence>,
    limitations: Vec<String>
});
record!(NormalisationWeightingCategory {
    method_ref: DatasetRef,
    reference_quantity_id: String,
    characterised_unit: String,
    canonical_characterised_value: String,
    source_normalisation_factor: String,
    source_weight_fraction: String,
    source_unit_label: String,
    source_short_description: String,
    canonical_normalised_value: String,
    normalised_unit: String,
    canonical_weighted_value: String,
    weighted_unit: String
});
record!(CandidateNormalisationWeighting {
    status: String,
    scope: String,
    basis: String,
    weight_representation: String,
    point_definition: String,
    scaling_factor: String,
    categories: Vec<NormalisationWeightingCategory>,
    canonical_candidate_single_score: Option<String>,
    score_unit: String,
    reason_codes: Vec<String>
});
record!(FrozenEvidenceSummary {
    functional_unit: FunctionalUnit,
    declared_system_boundary: String,
    process_instances: usize,
    source_document_count: usize,
    method_axes: Vec<MethodAxis>,
    exchanges: usize,
    inventory_axes: usize,
    declared_cutoffs: usize,
    coverage_gaps: Vec<CoverageGap>,
    quality_findings: Vec<String>
});
record!(FactsBundle {
    schema_version: String,
    profile_id: String,
    assessor_version: String,
    identity_kind: String,
    study_id: Uuid,
    assessment_id: Uuid,
    config: Config,
    config_hash: String,
    module_ref: ModuleRef,
    module_source_status: String,
    baseline: BaselineBinding,
    frozen_evidence: FrozenEvidenceSummary,
    method_source_evidence: MethodSourceEvidence,
    categories: Vec<CategoryAssessment>,
    requirements: Vec<RequirementAssessment>,
    candidate_normalisation_weighting: CandidateNormalisationWeighting,
    fact_catalog: Vec<super::method_runtime_contracts::ReportFact>,
    outcome: String,
    limitations: Vec<String>,
    pef_conformity_qualified: bool,
    scientific_ranking_qualified: bool,
    business_pilot_qualified: bool
});
