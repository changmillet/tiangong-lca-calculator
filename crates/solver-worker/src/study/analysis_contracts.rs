//! Worker-owned deterministic analysis formats; none grants source or Study access.
use super::{
    contracts::{CalculationRef, CoverageGap, DatasetRef, InventoryAxis, Port},
    runtime_contracts::{ArtifactRef, PrivateArtifact},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const CONFIG_SCHEMA: &str = "lca.study-analysis-config.v1";
pub const INPUT_SCHEMA: &str = "lca.study-analysis-input.v1";
pub const FACTS_SCHEMA: &str = "lca.study-analysis-facts.v1";
pub const PROFILE: &str = "lca.study-analysis-explicit-v1";
pub const ANALYZER: &str = "study.analysis.v1";
pub const MAX_FACTS: usize = 8192;

macro_rules! record {
    ($name:ident {$($field:ident: $kind:ty),* $(,)?}) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $(pub $field: $kind),* }
    };
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisKind {
    Contribution,
    Comparison,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisConfig {
    pub schema_version: String,
    pub kind: AnalysisKind,
    #[serde(deserialize_with = "canonical_run_ids")]
    pub run_ids: Vec<Uuid>,
}
fn canonical_run_ids<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<Uuid>, D::Error> {
    let values = Vec::<String>::deserialize(decoder)?;
    values
        .into_iter()
        .map(|text| {
            let id = text.parse::<Uuid>().map_err(serde::de::Error::custom)?;
            if id.to_string() != text {
                return Err(serde::de::Error::custom(
                    "canonical lowercase UUID required",
                ));
            }
            Ok(id)
        })
        .collect()
}
record!(Execution {
    job_id: Uuid,
    status: String
});
record!(AnalysisRecord {
    analysis_id: Uuid,
    study_id: Uuid,
    profile_id: String,
    config: AnalysisConfig,
    config_hash: String,
    execution: Execution
});
// The four nested records are unchanged Edge-owned frozen P1 projections. No raw source data.
record!(RunInput {
    run: Value,
    revision: Value,
    scenario: Value,
    preparation: Value,
    prepared_artifact: PrivateArtifact,
    result_artifact: PrivateArtifact
});
record!(ServiceInput { schema_version: String, analysis: AnalysisRecord, runs: Vec<RunInput> });
record!(NativeFiles {
    run_id: Uuid,
    prepared_path: String,
    result_path: String
});
record!(NativeInput { schema_version: String, input: ServiceInput, files: Vec<NativeFiles> });
record!(InputBinding {
    run_id: Uuid,
    study_revision_id: Uuid,
    scenario_revision_id: Uuid,
    model_preparation_id: Uuid,
    input_hash: String,
    effective_model_ref: Value,
    prepared_artifact_ref: ArtifactRef,
    result_ref: ArtifactRef,
    calculation_contract_ref: CalculationRef,
    method_source_hash: String,
    validity: Value
});
record!(Check {
    rule_id: String,
    status: String
});
record!(Comparability { status: String, reason_codes: Vec<String>, scope: String, ranking_allowed: bool, checks: Vec<Check> });
record!(UnitIdentity { kind: String, flow: Option<DatasetRef>, method: Option<DatasetRef>, support: Vec<DatasetRef>, view: Option<Port> });
record!(Subject { view: Option<Port>, inventory: Option<InventoryAxis> });
record!(Fact { fact_id: String, kind: String, label: String, run_ids: Vec<Uuid>, method_ref: Option<DatasetRef>, canonical_value: Option<String>, unit: String, unit_identity: UnitIdentity, coverage: String, scope: String, reason_codes: Vec<String>, subject: Subject });
record!(RunCoverage { run_id: Uuid, gaps: Vec<CoverageGap>, quality_findings: Vec<String>, complete_lcia_scores: bool });
record!(ScenarioEvidence {
    run_id: Uuid,
    scenario_revision_id: Uuid,
    content_hash: String,
    overrides: Vec<CanonicalOverride>
});
record!(CanonicalOverride {
    path: String,
    value: String
});
record!(Hotspots { run_id: Uuid, method_ref: DatasetRef, coverage: String, ordering: String, fact_ids: Vec<String> });
record!(FactsBundle { schema_version: String, profile_id: String, analyzer_version: String, study_id: Uuid, analysis_id: Uuid, config: AnalysisConfig, config_hash: String, inputs: Vec<InputBinding>, comparability: Comparability, facts: Vec<Fact>, coverage: Vec<RunCoverage>, scenario_evidence: Vec<ScenarioEvidence>, hotspots: Vec<Hotspots>, business_pilot_qualified: bool, scientific_ranking_qualified: bool });
