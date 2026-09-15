//! Native diagnostics over a supplied finite universe. This is not an online census or a gate.

use std::cell::Cell;
use std::io::Read;

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use solver_worker::calculation_evidence::expected_scope_manifest;

use super::{
    AllocationMode, ArtifactPurpose, BTreeMap, BTreeSet, CompiledGraph,
    CompiledReleaseSourceDatasetType, CompiledSourceFlowType, FlowLinkIdentity,
    FlowReferenceRequests, FlowReleaseMetadata, FlowRow, HashMap, LifecycleModelLineageRow,
    MatrixReadinessPolicy, MethodRow, MethodSelection, NormalizationMode, Path, ProcessRow,
    ProviderRule, RequestRootProcess, ResolvedFlowMetadata, Sha256, SnapshotBuildConfig,
    SnapshotSelectionMode, SnapshotSourceReader, SourceDatasetReadIdentity, Uuid, Value,
    VersionedModelIdentity, assemble_sparse_payload_with_selection, build_provider_lineage_index,
    canonical_json_bytes, classify_source_flow_type, collect_process_flow_reference_requests,
    compile_scope_graph_from_source, flow_link_identity_from_parts, flow_space_for_source_type, fs,
    lifecycle_model_component_references, lifecycle_model_result_references,
    resolve_flow_release_metadata, resolve_process_selection_with_flow_versions, sha256_bytes,
    source_dataset_document_id, stable_json_sha256, valid_source_dataset_version,
    validate_flow_row_visibility, validate_process_row_visibility,
};
use sha2::Digest;

const INPUT_SCHEMA: &str = "worker.provider-impact.input.v1";
const REPORT_SCHEMA: &str = "worker.provider-impact.report.v1";
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 10_000;
const MAX_REPORT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    dataset_type: CompiledReleaseSourceDatasetType,
    id: Uuid,
    version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    identity: Identity,
    document_sha256: String,
    document: Value,
    user_id: Option<Uuid>,
    state_code: i32,
    model_id: Option<Uuid>,
    model_version: Option<String>,
    team_id: Option<Uuid>,
    review_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    id: Uuid,
    version: String,
    document_sha256: String,
    document: Value,
    user_id: Option<Uuid>,
    state_code: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlowResolution {
    id: Uuid,
    version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    actor_user_id: Uuid,
    scope_manifest: Value,
    /// Already selected exact revisions. The diagnostic does not assert online selection truth.
    effective_processes: Vec<RequestRootProcess>,
    request_roots: Vec<RequestRootProcess>,
    /// Explicit frozen answers for references whose Flow version was omitted.
    omitted_flow_resolutions: Vec<FlowResolution>,
    documents: Vec<Document>,
    models: Vec<Model>,
    native_policy: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overlay {
    identity: Identity,
    before_sha256: String,
    candidate_sha256: String,
    document: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema_version: String,
    mode: String,
    expected_worker_sha256: String,
    baseline_sha256: String,
    baseline: Baseline,
    overlays: Vec<Overlay>,
}

#[derive(Clone)]
pub(super) struct FrozenSources {
    documents: BTreeMap<Identity, Document>,
    omitted_flows: BTreeMap<Uuid, String>,
    reads: Cell<u64>,
}

impl FrozenSources {
    fn document(
        &self,
        kind: CompiledReleaseSourceDatasetType,
        id: Uuid,
        version: &str,
    ) -> anyhow::Result<&Document> {
        self.reads.set(self.reads.get() + 1);
        self.documents
            .get(&Identity {
                dataset_type: kind,
                id,
                version: version.to_owned(),
            })
            .with_context(|| {
                format!(
                    "frozen dependency unavailable: {}:{id}@{version}",
                    kind.as_str()
                )
            })
    }

    pub(super) fn flow_meta(
        &self,
        requests: &FlowReferenceRequests,
    ) -> anyhow::Result<ResolvedFlowMetadata> {
        let mut result = ResolvedFlowMetadata::default();
        let mut identities = requests.exact.clone();
        for id in &requests.omitted {
            let version = self.omitted_flows.get(id).with_context(|| {
                format!("omitted Flow reference has no frozen resolution: {id}")
            })?;
            identities.insert(flow_link_identity_from_parts(*id, version));
            result.omitted_version_by_id.insert(*id, version.clone());
        }
        for identity in identities {
            let record = self.document(
                CompiledReleaseSourceDatasetType::Flow,
                identity.flow_id,
                &identity.flow_version,
            )?;
            result.by_identity.insert(identity, record.flow_row());
        }
        Ok(result)
    }

    pub(super) fn flow_release_metadata(
        &self,
        flows: &HashMap<FlowLinkIdentity, FlowRow>,
    ) -> anyhow::Result<HashMap<FlowLinkIdentity, FlowReleaseMetadata>> {
        let values = |kind| {
            self.documents
                .values()
                .filter(|record| record.identity.dataset_type == kind)
                .map(|record| {
                    (
                        (record.identity.id, record.identity.version.clone()),
                        record.document.clone(),
                    )
                })
                .collect()
        };
        let result = resolve_flow_release_metadata(
            flows,
            &values(CompiledReleaseSourceDatasetType::FlowProperty),
            &values(CompiledReleaseSourceDatasetType::UnitGroup),
        );
        ensure!(
            result.values().all(|meta| meta
                .reference_unit
                .as_deref()
                .is_some_and(|unit| !unit.trim().is_empty())),
            "frozen Flow reference property/unit chain is incomplete"
        );
        Ok(result)
    }

    pub(super) fn dataset_rows(
        &self,
        kind: CompiledReleaseSourceDatasetType,
        ids: &[Uuid],
    ) -> Vec<MethodRow> {
        self.reads.set(self.reads.get() + 1);
        self.documents
            .values()
            .filter(|record| {
                record.identity.dataset_type == kind && ids.contains(&record.identity.id)
            })
            .map(Document::method_row)
            .collect()
    }

    pub(super) fn dataset_metadata(
        &self,
        kind: CompiledReleaseSourceDatasetType,
        ids: &[Uuid],
    ) -> anyhow::Result<Vec<SourceDatasetReadIdentity>> {
        self.dataset_rows(kind, ids)
            .into_iter()
            .map(|row| {
                Ok(SourceDatasetReadIdentity {
                    estimated_bytes: serde_json::to_vec(&row.json)?.len(),
                    id: row.id,
                    version: row.version,
                })
            })
            .collect()
    }

    pub(super) fn exact_dataset_rows(
        &self,
        kind: CompiledReleaseSourceDatasetType,
        identities: &[SourceDatasetReadIdentity],
    ) -> Vec<MethodRow> {
        self.reads.set(self.reads.get() + 1);
        identities
            .iter()
            .filter_map(|identity| {
                self.documents.get(&Identity {
                    dataset_type: kind,
                    id: identity.id,
                    version: identity.version.clone(),
                })
            })
            .map(Document::method_row)
            .collect()
    }
}

impl Document {
    fn process_row(&self) -> ProcessRow {
        ProcessRow {
            id: self.identity.id,
            version: self.identity.version.clone(),
            model_id: self.model_id,
            model_version: self.model_version.clone(),
            user_id: self.user_id,
            state_code: self.state_code,
            team_id: self.team_id,
            review_id: self.review_id,
            modified_at: None,
            json: self.document.clone(),
        }
    }

    fn flow_row(&self) -> FlowRow {
        FlowRow {
            id: self.identity.id,
            version: self.identity.version.clone(),
            user_id: self.user_id,
            state_code: self.state_code,
            team_id: self.team_id,
            review_id: self.review_id,
            json: self.document.clone(),
        }
    }

    fn method_row(&self) -> MethodRow {
        MethodRow {
            id: self.identity.id,
            version: self.identity.version.clone(),
            json: self.document.clone(),
        }
    }

    fn validate(&self, actor: Uuid) -> anyhow::Result<()> {
        let kind = self.identity.dataset_type;
        ensure!(
            valid_source_dataset_version(&self.identity.version),
            "invalid frozen dataset version"
        );
        ensure!(
            source_dataset_document_id(kind, &self.document)? == self.identity.id,
            "frozen document UUID drift"
        );
        let (root, _) = kind.document_identity_keys();
        ensure!(
            document_version(&self.document, root) == Some(self.identity.version.as_str()),
            "frozen document version drift"
        );
        ensure!(
            stable_json_sha256(&self.document)? == self.document_sha256,
            "frozen document hash drift"
        );
        match kind {
            CompiledReleaseSourceDatasetType::Process => {
                validate_process_row_visibility(&self.process_row(), actor, false, false)?;
            }
            CompiledReleaseSourceDatasetType::Flow => {
                validate_flow_row_visibility(&self.flow_row(), actor, false, false)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn document_version<'a>(document: &'a Value, root: &str) -> Option<&'a str> {
    document
        .get(root)?
        .get("administrativeInformation")?
        .get("publicationAndOwnership")?
        .get("common:dataSetVersion")?
        .as_str()
}

fn native_policy() -> Value {
    json!({
        "provider_rule": "split_by_process_volume",
        "provider_candidate_eligibility_mode": "opposite_sign_reference_port",
        "provider_lineage_policy": "version-exact-lineage-gate-v1",
        "allocation_semantics_version": solver_worker::tidas_process_semantics::TIDAS_PROCESS_SEMANTICS_VERSION,
        "link_semantics_version": solver_worker::tidas_process_semantics::SIGNED_FLOW_LINK_SEMANTICS_VERSION,
        "flow_identity_policy": "exact-flow-version-reference-unit-v2",
        "source_closure_policy": "path-aware-bounded-frontier-v2",
        "source_reference_policy": solver_worker::source_reference_policy::SOURCE_REFERENCE_POLICY_VERSION,
        "reference_normalization_mode": "strict", "allocation_fraction_mode": "strict",
        "technosphere_boundary_policy": "closed", "self_loop_cutoff": 0.999_999, "singular_eps": 1e-12,
        "has_lcia": false, "method_ids": []
    })
}

fn validate_input(value: Value, worker_sha256: &str) -> anyhow::Result<Input> {
    let baseline_hash =
        stable_json_sha256(value.get("baseline").context("missing frozen baseline")?)?;
    let input: Input = serde_json::from_value(value)?;
    ensure!(
        input.schema_version == INPUT_SCHEMA && input.mode == "matrix_only",
        "unsupported provider-impact contract or mode"
    );
    ensure!(
        input.expected_worker_sha256 == worker_sha256,
        "Worker binary identity drift"
    );
    ensure!(
        input.baseline_sha256 == baseline_hash,
        "frozen baseline hash drift"
    );
    ensure!(
        input.baseline.native_policy == native_policy(),
        "unsupported native policy; no selection or weighting overrides"
    );
    ensure!(
        input.baseline.scope_manifest == expected_scope_manifest(input.baseline.actor_user_id),
        "frozen visibility predicate differs from state100 plus owner0"
    );
    ensure!(
        !input.baseline.documents.is_empty()
            && input.baseline.documents.len() + input.baseline.models.len() <= MAX_DOCUMENTS
            && input.overlays.len() <= MAX_DOCUMENTS,
        "frozen input document count is empty or exceeds the bound"
    );
    Ok(input)
}

fn prepare_sources(baseline: &Baseline) -> anyhow::Result<FrozenSources> {
    let mut sources = FrozenSources {
        documents: BTreeMap::new(),
        omitted_flows: BTreeMap::new(),
        reads: Cell::new(0),
    };
    for document in &baseline.documents {
        document.validate(baseline.actor_user_id)?;
        ensure!(
            sources
                .documents
                .insert(document.identity.clone(), document.clone())
                .is_none(),
            "duplicate frozen typed identity"
        );
    }
    for flow in &baseline.omitted_flow_resolutions {
        sources.document(
            CompiledReleaseSourceDatasetType::Flow,
            flow.id,
            &flow.version,
        )?;
        ensure!(
            sources
                .omitted_flows
                .insert(flow.id, flow.version.clone())
                .is_none(),
            "duplicate omitted Flow resolution"
        );
    }
    let mut ids = BTreeSet::new();
    for process in &baseline.effective_processes {
        sources.document(
            CompiledReleaseSourceDatasetType::Process,
            process.process_id,
            &process.process_version,
        )?;
        ensure!(
            ids.insert(process.process_id),
            "effective Process axis has duplicate or alternative versions"
        );
    }
    ensure!(!ids.is_empty(), "effective Process axis is empty");
    let mut roots = BTreeSet::new();
    for root in &baseline.request_roots {
        ensure!(
            baseline.effective_processes.contains(root),
            "request root is outside the effective frozen axis"
        );
        ensure!(roots.insert(root.clone()), "duplicate request root");
    }
    Ok(sources)
}

fn apply_overlays(input: &Input, before: &FrozenSources) -> anyhow::Result<FrozenSources> {
    let mut after = before.clone();
    let mut changed = BTreeSet::new();
    for overlay in &input.overlays {
        ensure!(
            changed.insert(overlay.identity.clone()),
            "duplicate candidate overlay"
        );
        let record = after
            .documents
            .get_mut(&overlay.identity)
            .context("candidate outside frozen universe")?;
        ensure!(
            record.document_sha256 == overlay.before_sha256,
            "candidate before hash drift"
        );
        ensure!(
            stable_json_sha256(&overlay.document)? == overlay.candidate_sha256,
            "candidate body hash drift"
        );
        ensure!(
            record.state_code == 0 && record.user_id == Some(input.baseline.actor_user_id),
            "candidate must be an existing actor-owned state0 draft"
        );
        match overlay.identity.dataset_type {
            CompiledReleaseSourceDatasetType::Process => {
                ensure!(
                    input
                        .baseline
                        .effective_processes
                        .iter()
                        .any(|p| p.process_id == overlay.identity.id
                            && p.process_version == overlay.identity.version),
                    "candidate Process is outside the effective axis"
                );
            }
            CompiledReleaseSourceDatasetType::Flow => {
                let kind = classify_source_flow_type(&record.document);
                ensure!(
                    matches!(
                        kind,
                        CompiledSourceFlowType::Product | CompiledSourceFlowType::Waste
                    ) && classify_source_flow_type(&overlay.document) == kind,
                    "Elementary/other Flow or Flow-type changes are forbidden"
                );
            }
            _ => anyhow::bail!("only Process and Product/Waste Flow body overlays are supported"),
        }
        record.document.clone_from(&overlay.document);
        record.document_sha256.clone_from(&overlay.candidate_sha256);
        record.validate(input.baseline.actor_user_id)?;
    }
    after.reads.set(0);
    Ok(after)
}

fn lineage_rows(
    baseline: &Baseline,
    processes: &[ProcessRow],
    sources: &FrozenSources,
) -> anyhow::Result<Vec<LifecycleModelLineageRow>> {
    let needed = processes
        .iter()
        .filter_map(|p| {
            p.model_id.map(|id| {
                (
                    id,
                    p.model_version.clone().unwrap_or_else(|| p.version.clone()),
                )
            })
        })
        .collect::<BTreeSet<_>>();
    let mut found = BTreeSet::new();
    let mut rows = Vec::new();
    for model in &baseline.models {
        ensure!(
            needed.contains(&(model.id, model.version.clone())),
            "unrequested Lifecycle Model outside frozen lineage set"
        );
        ensure!(
            found.insert((model.id, model.version.clone())),
            "duplicate exact Lifecycle Model"
        );
        ensure!(
            model.state_code == 100
                || (model.state_code == 0 && model.user_id == Some(baseline.actor_user_id)),
            "Lifecycle Model outside state100/owner0"
        );
        ensure!(valid_source_dataset_version(&model.version)
            && document_version(&model.document, "lifeCycleModelDataSet") == Some(model.version.as_str())
            && model.document.pointer("/lifeCycleModelDataSet/lifeCycleModelInformation/dataSetInformation/common:UUID").and_then(Value::as_str) == Some(model.id.to_string().as_str()), "Lifecycle Model identity drift");
        ensure!(
            stable_json_sha256(&model.document)? == model.document_sha256,
            "Lifecycle Model body hash drift"
        );
        for process in lifecycle_model_result_references(&model.document)
            .into_iter()
            .chain(lifecycle_model_component_references(&model.document))
        {
            sources.document(
                CompiledReleaseSourceDatasetType::Process,
                process.process_id,
                &process.process_version,
            )?;
        }
        rows.push(LifecycleModelLineageRow {
            identity: VersionedModelIdentity::new(model.id, model.version.clone()),
            modified_at: None,
            json: model.document.clone(),
        });
    }
    ensure!(found == needed, "incomplete frozen Lifecycle Model lineage");
    rows.sort_by(|a, b| a.identity.cmp(&b.identity));
    Ok(rows)
}

fn build_config(baseline: &Baseline) -> anyhow::Result<SnapshotBuildConfig> {
    let mut config = native_policy();
    config
        .as_object_mut()
        .expect("native policy object")
        .extend(
            json!({
                "process_states": "100", "include_user_id": baseline.actor_user_id,
                "data_scope": super::PUBLIC_PLUS_OWNER_DRAFT_SCOPE,
                "scope_manifest_sha256": stable_json_sha256(&baseline.scope_manifest)?,
                "selection_mode": SnapshotSelectionMode::FilteredLibrary,
                "request_roots": baseline.request_roots, "process_limit": 0,
                "biosphere_sign_mode": "gross", "artifact_purpose": "provider_impact_finite_input",
                "method_id": null, "method_version": null
            })
            .as_object()
            .expect("config object")
            .clone(),
        );
    Ok(serde_json::from_value(config)?)
}

struct Phase {
    graph: CompiledGraph,
    report: Value,
}

async fn evaluate_phase(baseline: &Baseline, sources: &FrozenSources) -> anyhow::Result<Phase> {
    let mut processes = baseline
        .effective_processes
        .iter()
        .map(|p| {
            sources
                .document(
                    CompiledReleaseSourceDatasetType::Process,
                    p.process_id,
                    &p.process_version,
                )
                .map(Document::process_row)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    processes.sort_by(|a, b| a.id.cmp(&b.id).then(a.version.cmp(&b.version)));
    let models = lineage_rows(baseline, &processes, sources)?;
    let lineage = build_provider_lineage_index(&processes, &models, &baseline.request_roots);
    let flow_meta = sources.flow_meta(&collect_process_flow_reference_requests(&processes))?;
    let spaces = flow_meta
        .by_identity
        .iter()
        .map(|(identity, flow)| {
            (
                identity.clone(),
                flow_space_for_source_type(classify_source_flow_type(&flow.json)),
            )
        })
        .collect();
    let closure = resolve_process_selection_with_flow_versions(
        processes.clone(),
        false,
        &[100],
        Some(baseline.actor_user_id),
        &baseline.request_roots,
        ProviderRule::SplitByProcessVolume,
        0,
        Some(&spaces),
        Some(&flow_meta),
        &lineage,
    )?;
    // Compile every effective Process, including disconnected consumers, in the SAME fixed root context.
    let compiled = compile_scope_graph_from_source(
        SnapshotSourceReader::Frozen(sources),
        processes,
        Some(baseline.actor_user_id),
        None,
        ProviderRule::SplitByProcessVolume,
        NormalizationMode::Strict,
        AllocationMode::Strict,
        &[],
        &lineage,
        ArtifactPurpose::CalculationBundle,
        false,
    )
    .await?;
    let method = MethodSelection {
        has_lcia: false,
        method_id: None,
        method_version: None,
        method_count: 0,
        factor_count: 0,
        source_evidence: None,
        rows: Vec::new(),
        static_bundle: None,
    };
    let config = build_config(baseline)?;
    let mut built = assemble_sparse_payload_with_selection(
        Uuid::nil(),
        &method,
        &config,
        &compiled.graph,
        0.999_999,
        1e-12,
        false,
        &[],
        &compiled.lcia_exchange_observations,
        false,
        &compiled.active_lcia_factors,
        MatrixReadinessPolicy {
            run_factorization: false,
            sample_solve_unit_limit: 0,
            require_lcia_factors: false,
            ..MatrixReadinessPolicy::default()
        },
    )?;
    ensure!(
        !built
            .readiness
            .metrics
            .compute_stability
            .factorization_checked
            && built
                .readiness
                .metrics
                .compute_stability
                .sampled_unit_solves
                == 0,
        "matrix-only numerical operation invariant failed"
    );
    // Native matrix assembly uses hash maps; canonicalize only the report's triplet order.
    for entries in [
        &mut built.data.technosphere_entries,
        &mut built.data.biosphere_entries,
        &mut built.data.characterization_factors,
    ] {
        entries.sort_by(|a, b| {
            a.row
                .cmp(&b.row)
                .then(a.col.cmp(&b.col))
                .then(a.value.total_cmp(&b.value))
        });
    }
    let evidence = built
        .compiled_graph
        .release_evidence
        .as_ref()
        .context("native source evidence absent")?;
    for source in &evidence.source_datasets {
        let record = sources.document(
            source.dataset_type,
            source.dataset_id,
            &source.dataset_version,
        )?;
        ensure!(
            source.document_sha256 == record.document_sha256 && source.document == record.document,
            "native source evidence diverges from input body"
        );
    }
    let report = json!({
        "effective_processes": built.compiled_graph.processes, "flow_axis": built.compiled_graph.flows,
        "request_root_closure": closure.scope_summary,
        "provider_decisions": built.compiled_graph.provider_decisions,
        "reference_ports": built.compiled_graph.reference_ports,
        "balance_resolutions": built.compiled_graph.balance_resolutions,
        "unresolved_balances": built.compiled_graph.unresolved_balances,
        "release_evidence": evidence,
        "matrix": built.data, "coverage": built.coverage,
        "readiness": { "provider_closure": built.readiness.metrics.provider_closure,
            "graph_readiness": built.readiness.metrics.graph_readiness,
            "compute_stability": built.readiness.metrics.compute_stability,
            "findings": built.readiness.findings, "blockers": built.readiness.blockers },
        "operation_counts": {"matrix_assemblies": 1, "factorizations": usize::from(built.readiness.metrics.compute_stability.factorization_checked),
            "unit_solves": built.readiness.metrics.compute_stability.sampled_unit_solves, "frozen_read_requests": sources.reads.get()}
    });
    Ok(Phase {
        graph: built.compiled_graph,
        report,
    })
}

fn consumer_projection(graph: &CompiledGraph, index: i32) -> Value {
    json!({
        "decisions": graph.provider_decisions.iter().filter(|d| d.consumer_idx == index).collect::<Vec<_>>(),
        "balances": graph.balance_resolutions.iter().filter(|d| d.dependent_process_idx == index).collect::<Vec<_>>(),
        "unresolved": graph.unresolved_balances.iter().filter(|d| d.dependent_process_idx == index).collect::<Vec<_>>()
    })
}

fn affected_processes(
    before: &CompiledGraph,
    after: &CompiledGraph,
    overlays: &[Overlay],
) -> Vec<Value> {
    let changed_flows = overlays
        .iter()
        .filter(|o| {
            o.identity.dataset_type == CompiledReleaseSourceDatasetType::Flow
                && o.before_sha256 != o.candidate_sha256
        })
        .map(|o| (o.identity.id, o.identity.version.clone()))
        .collect::<BTreeSet<_>>();
    let mut affected = BTreeSet::new();
    for process in &before.processes {
        if consumer_projection(before, process.process_idx)
            != consumer_projection(after, process.process_idx)
            || overlays.iter().any(|o| {
                o.identity.dataset_type == CompiledReleaseSourceDatasetType::Process
                    && o.identity.id == process.process_id
                    && o.before_sha256 != o.candidate_sha256
            })
        {
            affected.insert(process.process_idx);
        }
    }
    for graph in [before, after] {
        if let Some(evidence) = &graph.release_evidence {
            for exchange in &evidence.inventory_exchanges {
                if changed_flows.contains(&(exchange.flow_id, exchange.flow_version.clone())) {
                    affected.insert(exchange.process_idx);
                }
            }
        }
    }
    // Union before/after routing preserves downstream impact when a chosen edge disappears.
    loop {
        let old_count = affected.len();
        for graph in [before, after] {
            for edge in &graph.technosphere_edges {
                if affected.contains(&edge.provider_idx) {
                    affected.insert(edge.consumer_idx);
                }
            }
        }
        if old_count == affected.len() {
            break;
        }
    }
    before
        .processes
        .iter()
        .filter(|p| affected.contains(&p.process_idx))
        .map(|p| json!({"process_id": p.process_id, "process_version": p.process_version}))
        .collect()
}

async fn evaluate(value: Value, input_sha256: &str, worker_sha256: &str) -> anyhow::Result<Value> {
    let input = validate_input(value, worker_sha256)?;
    let before_sources = prepare_sources(&input.baseline)?;
    let after_sources = apply_overlays(&input, &before_sources)?;
    let before = evaluate_phase(&input.baseline, &before_sources).await?;
    let after = evaluate_phase(&input.baseline, &after_sources).await?;
    for overlay in &input.overlays {
        for (phase, expected) in [
            (&before, &overlay.before_sha256),
            (&after, &overlay.candidate_sha256),
        ] {
            let consumed = phase
                .graph
                .release_evidence
                .as_ref()
                .context("missing source evidence")?
                .source_datasets
                .iter()
                .any(|source| {
                    source.dataset_type == overlay.identity.dataset_type
                        && source.dataset_id == overlay.identity.id
                        && source.dataset_version == overlay.identity.version
                        && &source.document_sha256 == expected
                });
            ensure!(
                consumed,
                "candidate body is absent from native source evidence"
            );
        }
    }
    let affected = affected_processes(&before.graph, &after.graph, &input.overlays);
    let mut report = json!({
        "schema_version": REPORT_SCHEMA, "mode": "matrix_only", "status": "diagnostic_complete",
        "scope": {"kind": "supplied_finite_universe", "online_census_verified": false, "online_effective_selection_verified": false,
            "scientific_qualified": false, "save_admitted": false, "publication_qualified": false,
            "numerical_stability": "not_evaluated", "lcia": "not_evaluated"},
        "runtime": {"binary": "snapshot_builder", "package_version": env!("CARGO_PKG_VERSION"), "worker_binary_sha256": worker_sha256},
        "input_sha256": input_sha256, "baseline_sha256": input.baseline_sha256,
        "native_policy": input.baseline.native_policy,
        "selection_context": {"actor_user_id": input.baseline.actor_user_id,
            "scope_manifest": input.baseline.scope_manifest, "effective_processes": input.baseline.effective_processes,
            "request_roots": input.baseline.request_roots, "omitted_flow_resolutions": input.baseline.omitted_flow_resolutions},
        "input_membership": input.baseline.documents.iter().map(|r| json!({"identity": r.identity, "document_sha256": r.document_sha256})).collect::<Vec<_>>(),
        "lineage_evidence": input.baseline.models,
        "overlays": input.overlays.iter().map(|o| json!({"identity": o.identity, "before_sha256": o.before_sha256, "candidate_sha256": o.candidate_sha256})).collect::<Vec<_>>(),
        "affected_processes": affected,
        "before": before.report, "after": after.report,
        "operation_counts": {"matrix_assemblies": 2, "factorizations": 0, "unit_solves": 0, "database_queries": 0, "queue_writes": 0, "object_store_operations": 0}
    });
    let hash = stable_json_sha256(&report)?;
    report["report_sha256"] = Value::String(hash);
    Ok(report)
}

fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex::encode(digest.finalize()))
}

pub(super) async fn run(input: &Path, expected_sha256: &str, output: &Path) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    fs::File::open(input)?
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= MAX_INPUT_BYTES,
        "provider-impact input exceeds 64 MiB"
    );
    let input_sha256 = sha256_bytes(&bytes);
    ensure!(
        input_sha256 == expected_sha256,
        "provider-impact input file hash drift"
    );
    let worker_sha256 = file_sha256(&std::env::current_exe()?)?;
    let report = evaluate(
        serde_json::from_slice(&bytes)?,
        &input_sha256,
        &worker_sha256,
    )
    .await?;
    let bytes = canonical_json_bytes(&report)?;
    ensure!(
        bytes.len() <= MAX_REPORT_BYTES,
        "provider-impact report exceeds 128 MiB"
    );
    let parent = output
        .parent()
        .context("report requires an output directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temp, &bytes)?;
    temp.persist_noclobber(output).map_err(|e| e.error)?;
    println!(
        "[provider_impact] status=diagnostic_complete scope=supplied_finite_universe online_census_verified=false report_sha256={}",
        report["report_sha256"].as_str().unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
#[path = "provider_impact_tests.rs"]
mod tests;
