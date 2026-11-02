//! Study v1 compiler-owned formats. None of these records attests authorization.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SOURCE_SCHEMA: &str = "lca.study-source.v1";
pub const INPUT_SCHEMA: &str = "lca.study-prepared-input.v1";
pub const RESULT_SCHEMA: &str = "lca.study-result.v1";
pub const PROFILE: &str = "lca.study-explicit-v1";
pub const COMPILER: &str = "study.compiler.v1";
pub const MAX_FUNCTIONAL_UNIT_AMOUNT: f64 = 1e12;

macro_rules! record {
    ($name:ident {$($field:ident: $kind:ty),* $(,)?}) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $(pub $field: $kind),* }
    };
}
record!(DatasetRef {
    id: String,
    version: String
});
record!(Document {
    kind: String,
    identity: DatasetRef,
    state_code: i32,
    owner_actor_id: Option<String>,
    sha256: String,
    json: String
});
record!(Port {
    instance_id: String,
    exchange_id: String
});
record!(Cutoff {
    port: Port,
    reason: String
});
record!(AmountOverride {
    path: String,
    value: f64
});
record!(FunctionalUnit {
    amount: String,
    unit: String,
    reference_flow_id: String
});
record!(Assumption {
    id: String,
    statement: String,
    evidence_ref: String
});
record!(MethodRef {
    r#type: String,
    dataset_type: String,
    id: String,
    version: String
});
record!(Definition { goal: String, functional_unit: FunctionalUnit, system_boundary: String, method_refs: Vec<MethodRef>, assumptions: Vec<Assumption> });
record!(StudyBinding {
    study_revision_id: String,
    study_revision_content_hash: String,
    definition: Definition
});
record!(CalculationRef {
    kind: String,
    schema: String,
    sha256: String
});
record!(SourceInput {
    schema_version: String, origin: DatasetRef, actor_id: Option<String>, documents: Vec<Document>, method_refs: Vec<DatasetRef>,
    cutoffs: Vec<Cutoff>, overrides: Vec<AmountOverride>, study_binding: Option<StudyBinding>, quality_findings: Vec<String>
});
record!(Exchange {
    id: String, flow: DatasetRef, direction: String, amount: f64, kind: String, unit: String,
    unit_identity: Vec<DatasetRef>, allocations: Option<Value>
});
record!(Instance { id: String, process: DatasetRef, reference_exchange_id: String, exchanges: Vec<Exchange> });
record!(Connection {
    supplier: Port,
    consumer: Port
});
record!(SourceIdentity {
    kind: String,
    identity: DatasetRef,
    state_code: i32,
    sha256: String
});
record!(NormalizedModel {
    schema_version: String, origin: DatasetRef, reference: Port, instances: Vec<Instance>, connections: Vec<Connection>,
    cutoffs: Vec<Cutoff>, overrides: Vec<AmountOverride>, source_documents: Vec<SourceIdentity>, quality_findings: Vec<String>
});
record!(View {
    port: Port,
    flow: DatasetRef,
    unit: String,
    pivot: f64
});
record!(InventoryAxis {
    flow: DatasetRef,
    direction: String,
    unit: String
});
record!(MethodAxis {
    id: String,
    version: String,
    unit: String
});
record!(CoverageGap {
    method: DatasetRef,
    inventory_index: usize
});
record!(NumericInput { views: Vec<View>, inventory_axes: Vec<InventoryAxis>, method_axes: Vec<MethodAxis>, reference: View, a: Vec<Vec<f64>>, b: Vec<Vec<f64>>, c: Vec<Vec<f64>>, y: Vec<f64> });
record!(Bounds {
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
    max_functional_unit_amount: f64,
    max_owned_bytes: usize
});
record!(PreparedInput {
    schema_version: String, profile_id: String, compiler_version: String, calculation_contract_ref: CalculationRef,
    source_hash: String, model_hash: String, input_hash: Option<String>, study_binding: Option<StudyBinding>, model: NormalizedModel,
    numeric: NumericInput, coverage: Vec<CoverageGap>, method_source_hash: String, limits: Bounds
});
record!(StudyResult {
    schema_version: String, profile_id: String, compiler_version: String, calculation_contract_ref: CalculationRef,
    model_hash: String, input_hash: Option<String>, prepared_artifact_sha256: String, x: Vec<f64>, g: Vec<f64>, h: Vec<f64>,
    coverage: Vec<CoverageGap>, quality_findings: Vec<String>, max_residual: f64, elapsed_millis: u64,
    business_pilot_qualified: bool, complete_lcia_scores: bool
});
impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_source_bytes: 4 * 1024 * 1024,
            max_documents: 512,
            max_instances: 64,
            max_views: 128,
            max_exchanges: 4096,
            max_methods: 25,
            max_nonzeros: 32_768,
            max_dense_cells: 131_072,
            max_artifact_bytes: 4 * 1024 * 1024,
            max_elapsed_millis: 30_000,
            max_functional_unit_amount: MAX_FUNCTIONAL_UNIT_AMOUNT,
            max_owned_bytes: 256 * 1024 * 1024,
        }
    }
}

impl Bounds {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        let cap = Self::default();
        for (value, maximum) in [
            (self.max_source_bytes, cap.max_source_bytes),
            (self.max_documents, cap.max_documents),
            (self.max_instances, cap.max_instances),
            (self.max_views, cap.max_views),
            (self.max_exchanges, cap.max_exchanges),
            (self.max_methods, cap.max_methods),
            (self.max_nonzeros, cap.max_nonzeros),
            (self.max_dense_cells, cap.max_dense_cells),
            (self.max_artifact_bytes, cap.max_artifact_bytes),
            (self.max_owned_bytes, cap.max_owned_bytes),
        ] {
            anyhow::ensure!(
                value > 0 && value <= maximum,
                "study_unqualified_resource_bounds"
            );
        }
        anyhow::ensure!(
            self.max_elapsed_millis > 0
                && self.max_elapsed_millis <= cap.max_elapsed_millis
                && self.max_functional_unit_amount.is_finite()
                && self.max_functional_unit_amount > 0.0
                && self.max_functional_unit_amount <= cap.max_functional_unit_amount,
            "study_unqualified_resource_bounds"
        );
        Ok(())
    }
}
