use super::{
    contracts::{
        Bounds, COMPILER, CalculationRef, CoverageGap, DatasetRef, Exchange, INPUT_SCHEMA,
        Instance, InventoryAxis, MethodAxis, NormalizedModel, NumericInput, PROFILE, Port,
        PreparedInput, RESULT_SCHEMA, SourceInput, StudyResult, View,
    },
    identity::{artifact_bytes, semantic_hash, sha256},
    source,
};
use crate::{
    signed_flow::{SignedFlowDirection, normalize_reference_coefficient, signed_coefficient},
    static_lcia_cache::{StaticLciaDirection, VerifiedStaticLciaBundle},
    tidas_process_semantics::{TidasAllocationResolution, resolve_tidas_process_allocations},
};
use anyhow::{Context, ensure};
use serde_json::json;
use solver_core::{ModelSparseData, NumericOptions, SolveOptions, SolverService, SparseTriplet};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(super) struct Factor {
    pub(super) flow: DatasetRef,
    pub(super) direction: String,
    pub(super) value: f64,
}
/// Constructed from the existing verified pinned bundle; arbitrary request factors are never used.
pub struct VerifiedMethods {
    pub(super) axes: Vec<MethodAxis>,
    pub(super) factors: Vec<Vec<Factor>>,
    pub(super) source_hash: String,
}
impl VerifiedMethods {
    pub fn from_static(
        bundle: &VerifiedStaticLciaBundle,
        refs: &[DatasetRef],
    ) -> anyhow::Result<Self> {
        ensure!(
            !refs.is_empty() && refs.len() <= Bounds::default().max_methods,
            "study_method_limit"
        );
        let mut axes = Vec::new();
        let mut factors = Vec::new();
        let mut seen = BTreeSet::new();
        for r in refs {
            ensure!(seen.insert(r.id.clone()), "study_duplicate_method");
            let id = Uuid::parse_str(&r.id)?;
            let metadata = bundle
                .methods
                .iter()
                .find(|m| m.method_id == id && m.method_version == r.version)
                .context("study_exact_method_missing")?;
            axes.push(MethodAxis {
                id: r.id.clone(),
                version: r.version.clone(),
                unit: metadata.unit.clone(),
            });
            factors.push(
                crate::lcia_nonregional::prepare_static_method(bundle, id, &r.version)?
                    .into_iter()
                    .map(|f| Factor {
                        flow: DatasetRef {
                            id: f.flow_id.to_string(),
                            version: f.flow_version,
                        },
                        direction: match f.direction {
                            StaticLciaDirection::Input => "Input",
                            StaticLciaDirection::Output => "Output",
                        }
                        .into(),
                        value: f.value,
                    })
                    .collect(),
            );
        }
        Ok(Self {
            axes,
            factors,
            source_hash: semantic_hash("study.method-source.v1", &bundle.source_evidence)?,
        })
    }
}
fn direction(value: &str) -> SignedFlowDirection {
    if value == "Input" {
        SignedFlowDirection::Input
    } else {
        SignedFlowDirection::Output
    }
}
fn instance<'a>(model: &'a NormalizedModel, port: &Port) -> anyhow::Result<&'a Instance> {
    model
        .instances
        .iter()
        .find(|i| i.id == port.instance_id)
        .context("study_port_instance")
}
fn exchange<'a>(model: &'a NormalizedModel, port: &Port) -> anyhow::Result<&'a Exchange> {
    instance(model, port)?
        .exchanges
        .iter()
        .find(|e| e.id == port.exchange_id)
        .context("study_port_exchange")
}
fn key(port: &Port) -> (String, String) {
    (port.instance_id.clone(), port.exchange_id.clone())
}
fn view(model: &NormalizedModel, port: Port) -> anyhow::Result<View> {
    let e = exchange(model, &port)?;
    ensure!(e.kind != "Elementary flow", "study_reference_kind");
    let pivot =
        normalize_reference_coefficient(signed_coefficient(direction(&e.direction), e.amount)?)?;
    Ok(View {
        port,
        flow: e.flow.clone(),
        unit: e.unit.clone(),
        pivot: pivot.coefficient,
    })
}
fn is_legacy_share(declaration: Option<&serde_json::Value>) -> bool {
    let Some(allocation) = declaration.and_then(|d| d.get("allocation")) else {
        return false;
    };
    let entry = match allocation.as_array() {
        Some(entries) if entries.len() == 1 => &entries[0],
        Some(_) => return false,
        None => allocation,
    };
    entry.as_object().is_some_and(|e| {
        e.contains_key("@allocatedFraction") && !e.contains_key("@internalReferenceToCoProduct")
    })
}
fn fractions(instance: &Instance, pivot: &str) -> anyhow::Result<Vec<f64>> {
    let rows = instance
        .exchanges
        .iter()
        .map(|e| {
            let mut v = json!({"@dataSetInternalID":e.id,"exchangeDirection":e.direction});
            if let Some(a) = &e.allocations {
                v["allocations"] = a.clone();
            }
            v
        })
        .collect::<Vec<_>>();
    let borrowed = rows.iter().collect::<Vec<_>>();
    let resolved = resolve_tidas_process_allocations(&borrowed, pivot)?;
    for index in &resolved.allocation_target_indices {
        ensure!(
            instance.exchanges[*index].kind != "Elementary flow",
            "study_allocation_target_kind"
        );
    }
    let nonreference = pivot != instance.reference_exchange_id;
    let has_target = !resolved.allocation_target_indices.is_empty();
    if nonreference {
        let legacy_pivot = instance
            .exchanges
            .iter()
            .find(|e| e.id == pivot)
            .is_some_and(|e| e.direction == "Output" && is_legacy_share(e.allocations.as_ref()));
        ensure!(
            has_target || legacy_pivot,
            "study_missing_product_allocation"
        );
    }
    Ok(resolved
        .exchanges
        .into_iter()
        .enumerate()
        .map(|(index, r)| {
            if instance.exchanges[index].id == pivot {
                return 1.0;
            }
            match r {
                TidasAllocationResolution::Explicit { fraction }
                | TidasAllocationResolution::LegacyInferredReference { fraction } => fraction,
                TidasAllocationResolution::SparseZero => 0.0,
                TidasAllocationResolution::Undeclared
                | TidasAllocationResolution::LegacyEmptyUndeclared => {
                    if nonreference {
                        0.0
                    } else {
                        1.0
                    }
                }
            }
        })
        .collect())
}
fn calc_ref() -> CalculationRef {
    CalculationRef {
        kind: "calculation_contract".into(),
        schema: "lca.calculation-contract.v1".into(),
        sha256: crate::calculation_contract::fingerprint().into(),
    }
}
fn identities(input: &PreparedInput) -> anyhow::Result<(String, Option<String>)> {
    let model = semantic_hash(
        "study.model.v1",
        &json!({"profileId":input.profile_id,"compilerVersion":input.compiler_version,"calculationContractRef":input.calculation_contract_ref,"model":input.model,"numeric":input.numeric,"coverage":input.coverage,"methodSourceHash":input.method_source_hash}),
    )?;
    let hash=input.study_binding.as_ref().map(|b|semantic_hash("study.input.v1",&json!({"studyBinding":b,"modelHash":model,"profileId":input.profile_id,"compilerVersion":input.compiler_version,"calculationContractRef":input.calculation_contract_ref}))).transpose()?;
    Ok((model, hash))
}
pub(super) fn verify_identities(input: &PreparedInput) -> anyhow::Result<()> {
    let (model, hash) = identities(input)?;
    ensure!(
        model == input.model_hash && hash == input.input_hash,
        "study_semantic_hash_mismatch"
    );
    Ok(())
}
/// Compile only immutable source bytes and verified factors. Authorization and acquisition are W06.
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
pub fn prepare(
    bytes: &[u8],
    methods: &VerifiedMethods,
    bounds: &Bounds,
) -> anyhow::Result<PreparedInput> {
    let started = Instant::now();
    bounds.validate()?;
    ensure!(
        bytes.len() <= bounds.max_source_bytes,
        "study_source_bytes_limit"
    );
    let source: SourceInput = serde_json::from_slice(bytes)?;
    ensure!(
        !methods.axes.is_empty() && methods.axes.len() <= bounds.max_methods,
        "study_method_limit"
    );
    ensure!(
        methods
            .axes
            .iter()
            .map(|m| DatasetRef {
                id: m.id.clone(),
                version: m.version.clone()
            })
            .collect::<Vec<_>>()
            == source.method_refs,
        "study_method_binding"
    );
    ensure!(
        source
            .method_refs
            .iter()
            .map(|r| &r.id)
            .collect::<BTreeSet<_>>()
            .len()
            == source.method_refs.len(),
        "study_duplicate_method"
    );
    let model = source::normalize(&source, bounds)?;
    let mut ports = BTreeMap::new();
    for i in &model.instances {
        let p = Port {
            instance_id: i.id.clone(),
            exchange_id: i.reference_exchange_id.clone(),
        };
        ports.insert(key(&p), p);
    }
    for e in &model.connections {
        let supplier = exchange(&model, &e.supplier)?;
        let consumer = exchange(&model, &e.consumer)?;
        ensure!(
            supplier.flow == consumer.flow
                && supplier.unit_identity == consumer.unit_identity
                && supplier.unit == consumer.unit
                && supplier.kind != "Elementary flow"
                && consumer.kind != "Elementary flow",
            "study_connection_identity_mismatch"
        );
        ensure!(
            e.consumer.exchange_id != instance(&model, &e.consumer)?.reference_exchange_id,
            "study_consumed_reference_unsupported"
        );
        let s = signed_coefficient(direction(&supplier.direction), supplier.amount)?;
        let c = signed_coefficient(direction(&consumer.direction), consumer.amount)?;
        ensure!(
            s * c < 0.0 || c == 0.0,
            "study_connection_direction_mismatch"
        );
        ports.insert(key(&e.supplier), e.supplier.clone());
    }
    ensure!(ports.len() <= bounds.max_views, "study_view_limit");
    let views = ports
        .into_values()
        .map(|p| view(&model, p))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let n = views.len();
    let mut axis = BTreeMap::new();
    for i in &model.instances {
        for e in &i.exchanges {
            if e.kind == "Elementary flow" {
                axis.insert(
                    (
                        e.flow.id.clone(),
                        e.flow.version.clone(),
                        e.direction.clone(),
                    ),
                    InventoryAxis {
                        flow: e.flow.clone(),
                        direction: e.direction.clone(),
                        unit: e.unit.clone(),
                    },
                );
            }
        }
    }
    // Core requires at least one biosphere row, including a legitimate zero-inventory model.
    ensure!(!axis.is_empty(), "study_empty_inventory_unsupported");
    let inventory_axes = axis.into_values().collect::<Vec<_>>();
    let m = inventory_axes.len();
    let k = methods.axes.len();
    let cells = n
        .checked_mul(n)
        .and_then(|a| m.checked_mul(n).and_then(|b| a.checked_add(b)))
        .and_then(|a| k.checked_mul(m).and_then(|b| a.checked_add(b)))
        .context("study_dimension_overflow")?;
    ensure!(cells <= bounds.max_dense_cells, "study_matrix_memory_limit");
    ensure!(
        bytes
            .len()
            .saturating_mul(8)
            .saturating_add(cells.saturating_mul(64))
            <= bounds.max_owned_bytes,
        "study_owned_memory_limit"
    );
    let mut a = vec![vec![0.0; n]; n];
    let mut b = vec![vec![0.0; n]; m];
    let mut c = vec![vec![0.0; m]; k];
    let mut y = vec![0.0; n];
    for (column, v) in views.iter().enumerate() {
        let i = instance(&model, &v.port)?;
        let pivot = exchange(&model, &v.port)?;
        let scale = normalize_reference_coefficient(signed_coefficient(
            direction(&pivot.direction),
            pivot.amount,
        )?)?
        .scale;
        let shares = fractions(i, &v.port.exchange_id)?;
        for (e, share) in i.exchanges.iter().zip(shares) {
            if e.id == v.port.exchange_id {
                continue;
            }
            let value = e.amount * share * scale;
            ensure!(value.is_finite(), "study_nonfinite_coefficient");
            if e.kind == "Elementary flow" {
                let row = inventory_axes
                    .iter()
                    .position(|axis| axis.flow == e.flow && axis.direction == e.direction)
                    .unwrap();
                b[row][column] += value;
                continue;
            }
            let p = Port {
                instance_id: i.id.clone(),
                exchange_id: e.id.clone(),
            };
            if let Some(connection) = model.connections.iter().find(|c| c.consumer == p) {
                ensure!(
                    !model.cutoffs.iter().any(|c| c.port == p),
                    "study_connected_cutoff_conflict"
                );
                let row = views
                    .iter()
                    .position(|v| v.port == connection.supplier)
                    .unwrap();
                a[row][column] +=
                    -signed_coefficient(direction(&e.direction), value)? / views[row].pivot;
            } else if value != 0.0 {
                ensure!(
                    model.cutoffs.iter().any(|c| c.port == p)
                        || model.connections.iter().any(|c| c.supplier == p),
                    "study_undeclared_boundary:{}:{}",
                    i.id,
                    e.id
                );
            }
        }
    }
    let reference = view(&model, model.reference.clone())?;
    let reference_index = views
        .iter()
        .position(|v| v.port == model.reference)
        .unwrap();
    y[reference_index] = 1.0;
    if let Some(binding) = &source.study_binding {
        ensure!(
            !binding.study_revision_id.is_empty()
                && binding.study_revision_content_hash.len() == 64,
            "study_revision_binding"
        );
        let fu = &binding.definition.functional_unit;
        let amount = fu.amount.parse::<f64>()?;
        ensure!(
            amount.is_finite()
                && amount > 0.0
                && amount <= bounds.max_functional_unit_amount
                && fu.reference_flow_id == reference.flow.id
                && fu.unit == reference.unit,
            "study_functional_unit_mismatch"
        );
        ensure!(
            binding
                .definition
                .method_refs
                .iter()
                .map(|r| DatasetRef {
                    id: r.id.clone(),
                    version: r.version.clone()
                })
                .collect::<Vec<_>>()
                == source.method_refs
                && binding
                    .definition
                    .method_refs
                    .iter()
                    .all(|r| r.r#type == "datasetRevision" && r.dataset_type == "lciamethod"),
            "study_definition_method_binding"
        );
        y[reference_index] = amount;
    }
    let mut coverage = Vec::new();
    for (row, factors) in methods.factors.iter().enumerate() {
        for (column, axis) in inventory_axes.iter().enumerate() {
            let matches = factors
                .iter()
                .filter(|f| f.flow == axis.flow && f.direction == axis.direction)
                .collect::<Vec<_>>();
            ensure!(matches.len() <= 1, "study_factor_selection_not_unique");
            if let Some(f) = matches.first() {
                ensure!(f.value.is_finite(), "study_nonfinite_factor");
                c[row][column] = f.value;
            } else {
                coverage.push(CoverageGap {
                    method: source.method_refs[row].clone(),
                    inventory_index: column,
                });
            }
        }
    }
    for value in a.iter_mut().chain(&mut b).chain(&mut c).flatten() {
        ensure!(value.is_finite(), "study_aggregate_overflow");
        if *value == 0.0 {
            *value = 0.0;
        }
    }
    let nnz = a
        .iter()
        .chain(&b)
        .chain(&c)
        .flatten()
        .filter(|v| **v != 0.0)
        .count();
    ensure!(nnz <= bounds.max_nonzeros, "study_nnz_limit");
    let mut input = PreparedInput {
        schema_version: INPUT_SCHEMA.into(),
        profile_id: PROFILE.into(),
        compiler_version: COMPILER.into(),
        calculation_contract_ref: calc_ref(),
        source_hash: sha256(bytes),
        model_hash: String::new(),
        input_hash: None,
        study_binding: source.study_binding,
        model,
        numeric: NumericInput {
            views,
            inventory_axes,
            method_axes: methods.axes.clone(),
            reference,
            a,
            b,
            c,
            y,
        },
        coverage,
        method_source_hash: methods.source_hash.clone(),
        limits: bounds.clone(),
    };
    let (hash, input_hash) = identities(&input)?;
    input.model_hash = hash;
    input.input_hash = input_hash;
    artifact_bytes(&input)?;
    ensure!(
        started.elapsed().as_millis() <= u128::from(bounds.max_elapsed_millis),
        "study_compile_time_limit"
    );
    Ok(input)
}
fn triplets(matrix: &[Vec<f64>]) -> anyhow::Result<Vec<SparseTriplet>> {
    let mut result = Vec::new();
    for (row, values) in matrix.iter().enumerate() {
        for (col, value) in values.iter().enumerate() {
            ensure!(value.is_finite(), "study_nonfinite_matrix");
            if *value != 0.0 {
                result.push(SparseTriplet {
                    row: i32::try_from(row)?,
                    col: i32::try_from(col)?,
                    value: *value,
                });
            }
        }
    }
    Ok(result)
}
/// Local synchronous executor. W06 must isolate this call in a killable resource-limited process.
pub fn execute(input: &PreparedInput) -> anyhow::Result<StudyResult> {
    let started = Instant::now();
    validate_execution(input)?;
    let n = &input.numeric;
    let size = n.views.len();
    let rows = n.inventory_axes.len();
    let methods = n.method_axes.len();
    let data = ModelSparseData {
        model_version: Uuid::new_v4(),
        process_count: i32::try_from(size)?,
        flow_count: i32::try_from(rows)?,
        impact_count: i32::try_from(methods)?,
        technosphere_entries: triplets(&n.a)?,
        biosphere_entries: triplets(&n.b)?,
        characterization_factors: triplets(&n.c)?,
    };
    let service = SolverService::with_exact_zero_policy(input.limits.max_owned_bytes, 8.0);
    service.prepare(&data, NumericOptions::default())?;
    let result = service.solve_one(
        data.model_version,
        NumericOptions::default(),
        &n.y,
        SolveOptions::default(),
    )?;
    let x = result.x.context("study_missing_x")?;
    let g = result.g.context("study_missing_g")?;
    let h = result.h.context("study_missing_h")?;
    ensure!(
        x.iter().chain(&g).chain(&h).all(|v| v.is_finite()),
        "study_nonfinite_result"
    );
    let scale = x.iter().chain(&n.y).fold(1.0_f64, |a, b| a.max(b.abs()));
    ensure!(x.iter().all(|v| *v >= 0.0), "study_negative_activity");
    let mut max_residual = 0.0_f64;
    for (row, demand) in n.y.iter().enumerate() {
        let residual = x[row] - n.a[row].iter().zip(&x).map(|(a, x)| a * x).sum::<f64>() - demand;
        max_residual = max_residual.max(residual.abs());
    }
    ensure!(max_residual <= 1e-12 * scale, "study_balance_residual");
    let elapsed_millis = u64::try_from(started.elapsed().as_millis())?;
    ensure!(
        elapsed_millis <= input.limits.max_elapsed_millis,
        "study_execution_time_limit"
    );
    Ok(StudyResult {
        schema_version: RESULT_SCHEMA.into(),
        profile_id: PROFILE.into(),
        compiler_version: COMPILER.into(),
        calculation_contract_ref: calc_ref(),
        model_hash: input.model_hash.clone(),
        input_hash: input.input_hash.clone(),
        prepared_artifact_sha256: sha256(&artifact_bytes(input)?),
        x,
        g,
        h,
        coverage: input.coverage.clone(),
        quality_findings: input.model.quality_findings.clone(),
        max_residual,
        elapsed_millis,
        business_pilot_qualified: false,
        complete_lcia_scores: input.coverage.is_empty(),
    })
}

fn validate_execution(input: &PreparedInput) -> anyhow::Result<()> {
    input.limits.validate()?;
    artifact_bytes(input)?;
    verify_identities(input)?;
    ensure!(
        input.schema_version == INPUT_SCHEMA
            && input.profile_id == PROFILE
            && input.compiler_version == COMPILER
            && input.calculation_contract_ref == calc_ref(),
        "study_execution_contract"
    );
    let n = &input.numeric;
    let size = n.views.len();
    let rows = n.inventory_axes.len();
    let methods = n.method_axes.len();
    ensure!(
        size > 0
            && size <= input.limits.max_views
            && rows > 0
            && methods > 0
            && methods <= input.limits.max_methods,
        "study_execution_dimensions"
    );
    ensure!(
        n.a.len() == size
            && n.a.iter().all(|r| r.len() == size)
            && n.b.len() == rows
            && n.b.iter().all(|r| r.len() == size)
            && n.c.len() == methods
            && n.c.iter().all(|r| r.len() == rows)
            && n.y.len() == size,
        "study_matrix_dimensions"
    );
    let cells = size
        .checked_mul(size)
        .and_then(|v| rows.checked_mul(size).and_then(|b| v.checked_add(b)))
        .and_then(|v| methods.checked_mul(rows).and_then(|c| v.checked_add(c)))
        .context("study_dimension_overflow")?;
    ensure!(
        cells <= input.limits.max_dense_cells
            && cells.saturating_mul(64) <= input.limits.max_owned_bytes,
        "study_execution_memory_limit"
    );
    ensure!(
        n.a.iter()
            .chain(&n.b)
            .chain(&n.c)
            .flatten()
            .filter(|v| **v != 0.0)
            .count()
            <= input.limits.max_nonzeros,
        "study_nnz_limit"
    );
    ensure!(
        n.y.iter()
            .all(|v| v.is_finite() && *v >= 0.0 && *v <= input.limits.max_functional_unit_amount),
        "study_invalid_demand"
    );
    Ok(())
}
