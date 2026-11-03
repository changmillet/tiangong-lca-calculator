//! Independent W14 profiles. These private formats do not confer source authorization.
use super::{
    analysis_contracts::{Execution, InputBinding, RunInput},
    contracts::{CoverageGap, DatasetRef},
    runtime_contracts::{ArtifactRef, PrivateArtifact},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
pub const CONFIG_SCHEMA: &str = "lca.study-analysis-config.v2";
pub const INPUT_SCHEMA: &str = "lca.study-analysis-input.v2";
pub const FACTS_SCHEMA: &str = "lca.study-analysis-facts.v2";
pub const OAT_PROFILE: &str = "lca.study-sensitivity-oat.v1";
pub const UNCERTAINTY_PROFILE: &str = "lca.study-uncertainty-bounded.v1";
pub const PRODUCER: &str = "study.analysis.advanced.v1";
pub const GENERATOR: &str = "study.uncertainty.hmac-drbg-sha256.v1";
macro_rules! record { ($n:ident {$($f:ident:$t:ty),*$(,)?}) => { #[derive(Debug,Clone,Serialize,Deserialize)] #[serde(rename_all="camelCase",deny_unknown_fields)] pub struct $n { $(pub $f:$t),* } }; }
record!(Parameter {
    path: String,
    unit: String,
    baseline_amount: String,
    low: String,
    high: String
});
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Design {
    IndependentUniform { independence_declared: bool },
    JointEmpirical { rows: Vec<Vec<String>> },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Config {
    SensitivityOat {
        schema_version: String,
        baseline_run_id: String,
        parameters: Vec<Parameter>,
    },
    Uncertainty {
        schema_version: String,
        baseline_run_id: String,
        parameters: Vec<Parameter>,
        design: Design,
        sample_count: usize,
        seed_hex: String,
        generator: String,
    },
}
impl Config {
    #[must_use]
    pub fn parameters(&self) -> &[Parameter] {
        match self {
            Self::SensitivityOat { parameters, .. } | Self::Uncertainty { parameters, .. } => {
                parameters
            }
        }
    }
    #[must_use]
    pub fn baseline_run_id(&self) -> &str {
        match self {
            Self::SensitivityOat {
                baseline_run_id, ..
            }
            | Self::Uncertainty {
                baseline_run_id, ..
            } => baseline_run_id,
        }
    }
    #[must_use]
    pub const fn profile(&self) -> &'static str {
        match self {
            Self::SensitivityOat { .. } => OAT_PROFILE,
            Self::Uncertainty { .. } => UNCERTAINTY_PROFILE,
        }
    }
}
record!(AnalysisRecord {
    analysis_id: Uuid,
    study_id: Uuid,
    profile_id: String,
    config: Config,
    config_hash: String,
    execution: Execution
});
record!(ServiceInput {
    schema_version: String,
    analysis: AnalysisRecord,
    baseline: RunInput,
    source_snapshot: Value,
    source_artifact: PrivateArtifact
});
// Explicit service-selected experimental limits; no default constitutes hosted capacity qualification.
record!(BatchLimits {
    max_parameters: usize,
    max_evaluations: usize
});
record!(NativeLimits {
    max_source_bytes: usize,
    max_documents: usize,
    max_instances: usize,
    max_views: usize,
    max_exchanges: usize,
    max_methods: usize,
    max_nonzeros: usize,
    max_dense_cells: usize,
    max_artifact_bytes: usize,
    max_elapsed_millis: u64,
    max_owned_bytes: usize,
    max_functional_unit_amount: String
});
record!(ExecutionLimits {
    batch: BatchLimits,
    native: NativeLimits
});
record!(NativeInput {
    schema_version: String,
    input: ServiceInput,
    snapshot_path: String,
    prepared_path: String,
    result_path: String,
    batch_limits: BatchLimits
});
record!(ResolvedParameter { parameter_index:usize, path:String, flow:DatasetRef, direction:String, unit:String, support:Vec<DatasetRef>, baseline_amount:String });
record!(OutputValue { method_ref:DatasetRef, unit:String, canonical_value:Option<String>, coverage:String });
record!(Evaluation { evaluation_id:String, kind:String, parameter_index:Option<usize>, empirical_row_index:Option<usize>, amounts:Vec<String>, input_hash:String, model_hash:String, prepared_sha256:String, result_sha256:String, outputs:Vec<OutputValue>, max_residual:String });
record!(OatEffect { parameter_index:usize, method_ref:DatasetRef, output_unit:String, input_unit:String, coverage:String, low_evaluation_id:String, high_evaluation_id:String, low_absolute_delta:Option<String>, high_absolute_delta:Option<String>, low_relative_delta:Option<String>, high_relative_delta:Option<String>, secant:Option<String>, reason_codes:Vec<String> });
record!(Statistics { method_ref:DatasetRef, unit:String, coverage:String, mean:Option<String>, sample_standard_deviation:Option<String>, mean_mcse:Option<String>, minimum:Option<String>, maximum:Option<String>, quantiles:Vec<Quantile> });
record!(Quantile { probability:String, value:Option<String> });
record!(Counts {
    requested: usize,
    attempted: usize,
    valid: usize,
    invalid: usize,
    stop_reason: String
});
record!(FactsBundle { schema_version:String, profile_id:String, producer_version:String, study_id:Uuid, analysis_id:Uuid, config:Config, config_hash:String, execution_limits:ExecutionLimits, baseline:InputBinding, source_snapshot_ref:ArtifactRef, resolved_parameters:Vec<ResolvedParameter>, coverage_gaps:Vec<CoverageGap>, quality_findings:Vec<String>, baseline_evaluation:Evaluation, evaluations:Vec<Evaluation>, oat_effects:Vec<OatEffect>, statistics:Vec<Statistics>, counts:Counts, limitations:Vec<String>, scientific_ranking_qualified:bool, business_pilot_qualified:bool });
/// Sanitized all-or-error diagnostic; never a successful conditional sample subset.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[error("study_advanced_invalid_evaluation")]
pub struct EvaluationFailure {
    pub requested: usize,
    pub attempted: usize,
    pub valid: usize,
    pub invalid: usize,
    pub stop_reason: String,
    pub evaluation_id: String,
}
