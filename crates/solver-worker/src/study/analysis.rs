//! Analysis of issued frozen inputs/results. This does not query data or rerun a solver.
use super::{
    analysis_contracts::{
        ANALYZER, AnalysisConfig, AnalysisKind, CONFIG_SCHEMA, CanonicalOverride, Check,
        Comparability, FACTS_SCHEMA, Fact, FactsBundle, Hotspots, INPUT_SCHEMA, InputBinding,
        MAX_FACTS, NativeInput, PROFILE, RunCoverage, RunInput, ScenarioEvidence, ServiceInput,
        Subject, UnitIdentity,
    },
    contracts::{Bounds, COMPILER, DatasetRef, PreparedInput, StudyResult},
    decode_prepared, semantic_hash,
};
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Instant};
use uuid::Uuid;

pub struct VerifiedRun {
    pub(super) input: RunInput,
    pub(super) prepared: PreparedInput,
    pub(super) result: StudyResult,
}

pub fn config_hash(config: &AnalysisConfig) -> anyhow::Result<String> {
    validate_config(config)?;
    semantic_hash("study.analysis-config.v1", config)
}
pub fn validate_config(config: &AnalysisConfig) -> anyhow::Result<()> {
    ensure!(
        config.schema_version == CONFIG_SCHEMA
            && config.run_ids.len()
                == match config.kind {
                    AnalysisKind::Contribution => 1,
                    AnalysisKind::Comparison => 2,
                },
        "study_analysis_config"
    );
    ensure!(
        config.run_ids.iter().collect::<BTreeSet<_>>().len() == config.run_ids.len(),
        "study_analysis_duplicate_runs"
    );
    Ok(())
}
/// New facts spelling only. Never used to serialize or hash a P1 model/input/result.
pub fn canonical_number(value: f64) -> anyhow::Result<String> {
    ensure!(value.is_finite(), "study_analysis_nonfinite");
    if value == 0.0 {
        return Ok("0".into());
    }
    let token = serde_json::to_string(&value)?;
    let (mantissa, exponent) = token
        .split_once('e')
        .map_or((token.as_str(), None), |(m, e)| (m, Some(e)));
    let mantissa = if mantissa.contains('.') {
        mantissa.trim_end_matches('0').trim_end_matches('.')
    } else {
        mantissa
    };
    let output = if let Some(exponent) = exponent {
        let exponent: i32 = exponent.parse()?;
        if exponent == 0 {
            mantissa.to_owned()
        } else {
            format!("{mantissa}e{exponent}")
        }
    } else {
        mantissa.to_owned()
    };
    ensure!(
        output.len() <= 64 && output.parse::<f64>()?.to_bits() == value.to_bits(),
        "study_analysis_number_spelling"
    );
    Ok(output)
}
fn id(value: &Value, key: &str) -> anyhow::Result<Uuid> {
    value[key]
        .as_str()
        .context("study_analysis_identity")?
        .parse()
        .context("study_analysis_identity")
}
fn same(a: &Value, b: &Value) -> anyhow::Result<()> {
    ensure!(a == b, "study_analysis_binding");
    Ok(())
}
fn close(actual: f64, expected: f64) -> anyhow::Result<()> {
    ensure!(
        actual.is_finite() && expected.is_finite(),
        "study_analysis_nonfinite"
    );
    ensure!(
        actual.to_bits() == expected.to_bits()
            || (actual - expected).abs() <= 1e-12 * actual.abs().max(expected.abs()),
        "study_analysis_result_numeric"
    );
    Ok(())
}
fn product(a: f64, b: f64) -> anyhow::Result<f64> {
    let value = a * b;
    ensure!(value.is_finite(), "study_analysis_nonfinite");
    ensure!(
        value != 0.0 || a == 0.0 || b == 0.0,
        "study_analysis_arithmetic_underflow"
    );
    Ok(value)
}
fn contribution(b: &[Vec<f64>], c: &[f64], x: f64, view: usize) -> anyhow::Result<f64> {
    // Preserve B*x before characterization, matching P1 inventory order. Computing
    // x*sum(B*C) can erase a representable result when B*C underflows first.
    b.iter().zip(c).try_fold(0.0, |sum, (row, c)| {
        let value = sum + product(product(row[view], x)?, *c)?;
        ensure!(value.is_finite(), "study_analysis_nonfinite");
        Ok(value)
    })
}
pub fn validate_input(input: &ServiceInput) -> anyhow::Result<()> {
    ensure!(
        input.schema_version == INPUT_SCHEMA
            && input.analysis.profile_id == PROFILE
            && input.analysis.execution.status == "running"
            && input.analysis.config_hash == config_hash(&input.analysis.config)?
            && input.runs.len() == input.analysis.config.run_ids.len(),
        "study_analysis_input_binding"
    );
    for (row, expected) in input.runs.iter().zip(&input.analysis.config.run_ids) {
        ensure!(
            id(&row.run, "runId")? == *expected
                && id(&row.run, "studyId")? == input.analysis.study_id,
            "study_analysis_run_binding"
        );
        for doc in [&row.revision, &row.scenario, &row.preparation] {
            ensure!(
                id(doc, "studyId")? == input.analysis.study_id,
                "study_analysis_cross_study"
            );
        }
        ensure!(
            row.run["execution"]["status"] == "completed"
                && row.preparation["execution"]["status"] == "completed"
                && row.revision["status"] == "frozen"
                && row.revision["validation"] == "passed"
                && row.scenario["status"] == "frozen",
            "study_analysis_unfrozen_run"
        );
        for (artifact, kind, schema) in [
            (
                &row.prepared_artifact,
                "modelInput",
                super::contracts::INPUT_SCHEMA,
            ),
            (
                &row.result_artifact,
                "runResult",
                super::contracts::RESULT_SCHEMA,
            ),
        ] {
            ensure!(
                artifact.study_id == input.analysis.study_id
                    && artifact.artifact_ref.r#type == kind
                    && artifact.artifact_ref.schema_version == schema
                    && artifact.artifact_ref.digest_scope == "artifact_bytes_sha256"
                    && artifact.artifact_ref.media_type == "application/json"
                    && artifact.artifact_ref.byte_size > 0
                    && artifact.artifact_ref.byte_size <= 4 * 1024 * 1024,
                "study_analysis_artifact_binding"
            );
        }
        same(
            &row.run["resultRef"],
            &serde_json::to_value(&row.result_artifact.artifact_ref)?,
        )?;
        same(
            &row.preparation["inputArtifactRef"],
            &serde_json::to_value(&row.prepared_artifact.artifact_ref)?,
        )?;
        same(
            &row.run["effectiveModelRef"],
            &row.scenario["effectiveModelRef"],
        )?;
        same(&row.run["effectiveModelRef"], &row.preparation["modelRef"])?;
        ensure!(
            row.run["effectiveModelRef"]["type"] == "modelRevision"
                && row.run["effectiveModelRef"]["digestScope"] == "worker_prepared_model"
                && row.run["engineId"] == "solver-worker",
            "study_analysis_model_domain"
        );
        ensure!(
            id(&row.run, "studyRevisionId")? == id(&row.revision, "revisionId")?
                && id(&row.scenario, "studyRevisionId")? == id(&row.revision, "revisionId")?
                && id(&row.run, "scenarioRevisionId")? == id(&row.scenario, "scenarioRevisionId")?
                && id(&row.run, "modelPreparationId")? == id(&row.preparation, "preparationId")?
                && id(&row.scenario, "modelPreparationId")?
                    == id(&row.preparation, "preparationId")?,
            "study_analysis_domain_binding"
        );
        ensure!(
            row.preparation["source"]["type"] == "scenarioRevision"
                && id(&row.preparation["source"], "scenarioRevisionId")?
                    == id(&row.scenario, "scenarioRevisionId")?
                && row.preparation["source"]["contentHash"] == row.scenario["contentHash"]
                && row.preparation["source"]["expectedRevision"]
                    .as_u64()
                    .and_then(|n| n.checked_add(1))
                    == row.scenario["revision"].as_u64(),
            "study_analysis_scenario_preparation_binding"
        );
    }
    Ok(())
}
#[allow(clippy::too_many_lines)]
pub fn verify_run(
    input: RunInput,
    prepared_bytes: &[u8],
    result_bytes: &[u8],
    bounds: &Bounds,
) -> anyhow::Result<VerifiedRun> {
    ensure!(
        prepared_bytes.len() == usize::try_from(input.prepared_artifact.artifact_ref.byte_size)?
            && result_bytes.len() == usize::try_from(input.result_artifact.artifact_ref.byte_size)?
            && super::sha256(result_bytes) == input.result_artifact.artifact_ref.content_hash,
        "study_analysis_artifact_integrity"
    );
    let prepared = decode_prepared(
        prepared_bytes,
        &input.prepared_artifact.artifact_ref.content_hash,
        bounds,
    )?;
    super::compile::validate_execution(&prepared)?;
    let result: StudyResult = super::json::from_slice(result_bytes)?;
    let binding = prepared
        .study_binding
        .as_ref()
        .context("study_analysis_study_binding")?;
    same(
        &input.revision["definition"],
        &serde_json::to_value(&binding.definition)?,
    )?;
    ensure!(
        input.revision["revisionId"] == binding.study_revision_id
            && input.revision["contentHash"] == binding.study_revision_content_hash
            && input.run["inputHash"] == serde_json::to_value(&prepared.input_hash)?
            && input.preparation["inputHash"] == input.run["inputHash"]
            && input.run["effectiveModelRef"]["contentHash"] == prepared.model_hash,
        "study_analysis_input_identity"
    );
    let calculation = serde_json::to_value(&prepared.calculation_contract_ref)?;
    same(&input.run["calculationContractRef"], &calculation)?;
    same(&input.preparation["calculationContractRef"], &calculation)?;
    ensure!(
        input.run["profileId"] == prepared.profile_id
            && input.preparation["profileId"] == prepared.profile_id
            && input.preparation["methodRefs"]
                == serde_json::to_value(&binding.definition.method_refs)?,
        "study_analysis_profile_binding"
    );
    ensure!(
        result.schema_version == super::contracts::RESULT_SCHEMA
            && result.compiler_version == COMPILER
            && result.profile_id == prepared.profile_id
            && result.calculation_contract_ref == prepared.calculation_contract_ref
            && result.model_hash == prepared.model_hash
            && result.input_hash == prepared.input_hash
            && result.prepared_artifact_sha256 == input.prepared_artifact.artifact_ref.content_hash
            && result.coverage == prepared.coverage
            && result.quality_findings == prepared.model.quality_findings
            && !result.business_pilot_qualified
            && result.complete_lcia_scores == prepared.coverage.is_empty(),
        "study_analysis_result_binding"
    );
    let n = &prepared.numeric;
    ensure!(
        n.reference.port == prepared.model.reference
            && n.reference.flow.id == binding.definition.functional_unit.reference_flow_id
            && n.reference.unit == binding.definition.functional_unit.unit,
        "study_analysis_functional_unit"
    );
    ensure!(
        n.method_axes.len() == binding.definition.method_refs.len()
            && n.method_axes
                .iter()
                .zip(&binding.definition.method_refs)
                .all(|(axis, reference)| axis.id == reference.id
                    && axis.version == reference.version
                    && reference.r#type == "datasetRevision"
                    && reference.dataset_type == "lciamethod"),
        "study_analysis_method_axis"
    );
    let reference_index = n
        .views
        .iter()
        .position(|v| v == &n.reference)
        .context("study_analysis_reference_view")?;
    let demand: f64 = binding.definition.functional_unit.amount.parse()?;
    for (index, value) in n.y.iter().enumerate() {
        ensure!(
            if index == reference_index {
                value.to_bits() == demand.to_bits()
            } else {
                *value == 0.0
            },
            "study_analysis_reference_demand"
        );
    }
    ensure!(
        result.x.len() == n.views.len()
            && result.g.len() == n.inventory_axes.len()
            && result.h.len() == n.method_axes.len()
            && result.x.iter().all(|v| v.is_finite() && *v >= 0.0),
        "study_analysis_result_axes"
    );
    let scale = result
        .x
        .iter()
        .chain(&n.y)
        .fold(1.0_f64, |a, b| a.max(b.abs()));
    for (i, row) in n.a.iter().enumerate() {
        let residual =
            result.x[i] - row.iter().zip(&result.x).map(|(a, x)| a * x).sum::<f64>() - n.y[i];
        ensure!(
            residual.is_finite() && residual.abs() <= 1e-12 * scale,
            "study_analysis_result_residual"
        );
    }
    for (row, g) in n.b.iter().zip(&result.g) {
        close(*g, row.iter().zip(&result.x).map(|(b, x)| b * x).sum())?;
    }
    for (row, h) in n.c.iter().zip(&result.h) {
        close(*h, row.iter().zip(&result.g).map(|(c, g)| c * g).sum())?;
    }
    for gap in &prepared.coverage {
        ensure!(
            gap.inventory_index < n.inventory_axes.len()
                && n.method_axes
                    .iter()
                    .any(|m| m.id == gap.method.id && m.version == gap.method.version),
            "study_analysis_coverage_axis"
        );
        let method = n
            .method_axes
            .iter()
            .position(|m| m.id == gap.method.id && m.version == gap.method.version)
            .context("study_analysis_coverage_axis")?;
        ensure!(
            n.c[method][gap.inventory_index] == 0.0,
            "study_analysis_gap_has_factor"
        );
    }
    for axis in &n.inventory_axes {
        ensure!(
            prepared
                .model
                .instances
                .iter()
                .flat_map(|i| &i.exchanges)
                .any(|e| e.kind == "Elementary flow"
                    && e.flow == axis.flow
                    && e.direction == axis.direction
                    && e.unit == axis.unit),
            "study_analysis_inventory_unit"
        );
    }
    for view in &n.views {
        ensure!(
            prepared
                .model
                .instances
                .iter()
                .find(|i| i.id == view.port.instance_id)
                .and_then(|i| i.exchanges.iter().find(|e| e.id == view.port.exchange_id))
                .is_some_and(|e| e.flow == view.flow && e.unit == view.unit),
            "study_analysis_view_unit"
        );
    }
    Ok(VerifiedRun {
        input,
        prepared,
        result,
    })
}
pub(super) fn binding(run: &VerifiedRun) -> anyhow::Result<InputBinding> {
    let row = &run.input;
    Ok(InputBinding {
        run_id: id(&row.run, "runId")?,
        study_revision_id: id(&row.run, "studyRevisionId")?,
        scenario_revision_id: id(&row.run, "scenarioRevisionId")?,
        model_preparation_id: id(&row.run, "modelPreparationId")?,
        input_hash: run
            .prepared
            .input_hash
            .clone()
            .context("study_analysis_input_identity")?,
        effective_model_ref: row.run["effectiveModelRef"].clone(),
        prepared_artifact_ref: row.prepared_artifact.artifact_ref.clone(),
        result_ref: row.result_artifact.artifact_ref.clone(),
        calculation_contract_ref: run.prepared.calculation_contract_ref.clone(),
        method_source_hash: run.prepared.method_source_hash.clone(),
        validity: row.run["validity"].clone(),
    })
}
fn method(run: &VerifiedRun, index: usize) -> DatasetRef {
    let m = &run.prepared.numeric.method_axes[index];
    DatasetRef {
        id: m.id.clone(),
        version: m.version.clone(),
    }
}
fn coverage(run: &VerifiedRun, index: usize) -> &'static str {
    let method = method(run, index);
    let gaps = run
        .prepared
        .coverage
        .iter()
        .filter(|g| g.method == method)
        .map(|g| g.inventory_index)
        .collect::<BTreeSet<_>>()
        .len();
    if gaps == 0 {
        "complete"
    } else if gaps == run.prepared.numeric.inventory_axes.len() {
        "unknown"
    } else {
        "partial"
    }
}
fn comparability(runs: &[VerifiedRun]) -> Comparability {
    let mut result = Comparability {
        status: "not_assessed".into(),
        reason_codes: vec![],
        scope: "frozen_numerical_inputs_only".into(),
        ranking_allowed: false,
        checks: vec![],
    };
    if runs.len() == 1 {
        result.reason_codes.push("comparison_not_requested".into());
        return result;
    }
    let a = &runs[0].prepared;
    let b = &runs[1].prepared;
    let da = &a.study_binding.as_ref().unwrap().definition;
    let db = &b.study_binding.as_ref().unwrap().definition;
    for (rule, passed) in [
        ("functional_unit", da.functional_unit == db.functional_unit),
        ("system_boundary", da.system_boundary == db.system_boundary),
        (
            "method_axes",
            a.numeric.method_axes == b.numeric.method_axes,
        ),
        (
            "calculation_profile",
            a.profile_id == b.profile_id
                && a.compiler_version == b.compiler_version
                && a.calculation_contract_ref == b.calculation_contract_ref,
        ),
        (
            "method_source",
            a.method_source_hash == b.method_source_hash,
        ),
        ("declared_assumptions", da.assumptions == db.assumptions),
        ("declared_cutoffs", a.model.cutoffs == b.model.cutoffs),
    ] {
        result.checks.push(Check {
            rule_id: rule.into(),
            status: if passed { "passed" } else { "failed" }.into(),
        });
        if !passed {
            result.reason_codes.push(format!("{rule}_mismatch"));
        }
    }
    if !result.reason_codes.is_empty() {
        result.status = "not_comparable".into();
        return result;
    }
    for (rule, passed) in [
        (
            "complete_characterization",
            runs.iter().all(|r| r.prepared.coverage.is_empty()),
        ),
        (
            "source_quality",
            runs.iter()
                .all(|r| r.prepared.model.quality_findings.is_empty()),
        ),
        (
            "current_run_validity",
            runs.iter()
                .all(|r| r.input.run["validity"]["status"] == "current"),
        ),
        (
            "source_identity",
            a.model.source_documents == b.model.source_documents,
        ),
    ] {
        result.checks.push(Check {
            rule_id: rule.into(),
            status: if passed { "passed" } else { "unknown" }.into(),
        });
        if !passed {
            result.reason_codes.push(format!("{rule}_not_qualified"));
        }
    }
    result.status = if result.reason_codes.is_empty() {
        "comparable"
    } else {
        "not_assessed"
    }
    .into();
    result
}
fn unit(flow: Option<DatasetRef>, method: Option<DatasetRef>) -> UnitIdentity {
    UnitIdentity {
        kind: if method.is_some() {
            "lcia_method_quantity"
        } else if flow.is_some() {
            "flow_reference_unit"
        } else {
            "dimensionless"
        }
        .into(),
        flow,
        method,
        support: vec![],
        view: None,
    }
}
#[allow(clippy::too_many_arguments)]
fn fact(
    id: String,
    kind: &str,
    run_ids: Vec<Uuid>,
    method_ref: Option<DatasetRef>,
    value: Option<f64>,
    unit_text: String,
    unit_identity: UnitIdentity,
    coverage: &str,
    scope: &str,
    subject: Subject,
) -> anyhow::Result<Fact> {
    let run_label = id
        .strip_prefix('r')
        .and_then(|v| v.split(':').next())
        .and_then(|v| v.parse::<usize>().ok())
        .map_or_else(|| "Comparison".to_owned(), |v| format!("Run {}", v + 1));
    let method_label = method_ref
        .as_ref()
        .map(|m| format!("LCIA {}@{}", m.id, m.version));
    let flow_label = subject
        .inventory
        .as_ref()
        .map(|i| format!("{}@{} ({})", i.flow.id, i.flow.version, i.direction))
        .or_else(|| {
            unit_identity
                .flow
                .as_ref()
                .map(|f| format!("{}@{}", f.id, f.version))
        });
    let view_label = subject
        .view
        .as_ref()
        .map(|v| format!("instance {} / exchange {}", v.instance_id, v.exchange_id));
    let mut parts = vec![format!("{run_label} {kind}")];
    parts.extend(method_label);
    parts.extend(view_label);
    parts.extend(flow_label);
    if kind == "comparisonDelta" {
        parts.push("alternative minus baseline".into());
    }
    if kind == "relativeDelta" {
        parts.push("signed delta / absolute baseline".into());
    }
    Ok(Fact {
        fact_id: id,
        label: parts.join(" — "),
        kind: kind.into(),
        run_ids,
        method_ref,
        canonical_value: value.map(canonical_number).transpose()?,
        unit: unit_text,
        unit_identity,
        coverage: coverage.into(),
        scope: scope.into(),
        reason_codes: if coverage == "partial" {
            vec!["partial_characterization".into()]
        } else if value.is_none() {
            vec![
                if kind == "inventoryContribution" {
                    "missing_characterization_factor"
                } else {
                    "value_not_defined"
                }
                .into(),
            ]
        } else {
            vec![]
        },
        subject,
    })
}
fn no_subject() -> Subject {
    Subject {
        view: None,
        inventory: None,
    }
}
#[allow(clippy::too_many_lines)]
pub fn analyze(input: &ServiceInput, runs: &[VerifiedRun]) -> anyhow::Result<FactsBundle> {
    validate_input(input)?;
    ensure!(runs.len() == input.runs.len(), "study_analysis_runs");
    let started = Instant::now();
    let mut facts = vec![];
    let mut hotspots = vec![];
    let estimate = runs.iter().try_fold(0usize, |total, r| {
        let n = &r.prepared.numeric;
        total
            .checked_add(
                n.views.len()
                    + n.inventory_axes.len()
                    + n.method_axes
                        .len()
                        .saturating_mul(1 + n.views.len() + n.inventory_axes.len()),
            )
            .context("study_analysis_fact_limit")
    })?;
    ensure!(
        estimate.saturating_add(2 * runs[0].prepared.numeric.method_axes.len()) <= MAX_FACTS,
        "study_analysis_fact_limit"
    );
    let comp = comparability(runs);
    for (ri, run) in runs.iter().enumerate() {
        same(
            &serde_json::to_value(&run.input)?,
            &serde_json::to_value(&input.runs[ri])?,
        )?;
        ensure!(
            id(&run.input.run, "runId")? == input.analysis.config.run_ids[ri],
            "study_analysis_run_order"
        );
        let run_id = input.analysis.config.run_ids[ri];
        let n = &run.prepared.numeric;
        for (i, v) in n.views.iter().enumerate() {
            let mut u = unit(Some(v.flow.clone()), None);
            u.kind = "model_view_reference_unit".into();
            u.view = Some(v.port.clone());
            if let Some(exchange) = run
                .prepared
                .model
                .instances
                .iter()
                .find(|i| i.id == v.port.instance_id)
                .and_then(|i| i.exchanges.iter().find(|e| e.id == v.port.exchange_id))
            {
                u.support.clone_from(&exchange.unit_identity);
            }
            facts.push(fact(
                format!("r{ri}:x:{i}"),
                "activity",
                vec![run_id],
                None,
                Some(run.result.x[i]),
                v.unit.clone(),
                u,
                "complete",
                "declared_model_activity",
                Subject {
                    view: Some(v.port.clone()),
                    inventory: None,
                },
            )?);
        }
        for (i, axis) in n.inventory_axes.iter().enumerate() {
            let mut u = unit(Some(axis.flow.clone()), None);
            if let Some(e) = run
                .prepared
                .model
                .instances
                .iter()
                .flat_map(|i| &i.exchanges)
                .find(|e| e.flow == axis.flow && e.direction == axis.direction)
            {
                u.support.clone_from(&e.unit_identity);
            }
            facts.push(fact(
                format!("r{ri}:g:{i}"),
                "inventory",
                vec![run_id],
                None,
                Some(run.result.g[i]),
                axis.unit.clone(),
                u,
                "complete",
                "declared_inventory",
                Subject {
                    view: None,
                    inventory: Some(axis.clone()),
                },
            )?);
        }
        for (mi, m) in n.method_axes.iter().enumerate() {
            ensure!(
                started.elapsed().as_millis() <= u128::from(run.prepared.limits.max_elapsed_millis),
                "study_analysis_time_limit"
            );
            let mr = method(run, mi);
            let cov = coverage(run, mi);
            let known = cov != "unknown";
            facts.push(fact(
                format!("r{ri}:h:{mi}"),
                "impact",
                vec![run_id],
                Some(mr.clone()),
                known.then_some(run.result.h[mi]),
                m.unit.clone(),
                unit(None, Some(mr.clone())),
                cov,
                "characterized_subtotal",
                no_subject(),
            )?);
            let mut sum = 0.0;
            let mut magnitude = 0.0;
            let mut ordered = vec![];
            for (vi, v) in n.views.iter().enumerate() {
                let contribution = contribution(&n.b, &n.c[mi], run.result.x[vi], vi)?;
                sum += contribution;
                magnitude += contribution.abs();
                if known {
                    ordered.push((format!("r{ri}:v:{mi}:{vi}"), contribution.abs()));
                }
                let mut u = unit(None, Some(mr.clone()));
                u.view = Some(v.port.clone());
                facts.push(fact(
                    format!("r{ri}:v:{mi}:{vi}"),
                    "viewContribution",
                    vec![run_id],
                    Some(mr.clone()),
                    known.then_some(contribution),
                    m.unit.clone(),
                    u,
                    cov,
                    "characterized_subtotal",
                    Subject {
                        view: Some(v.port.clone()),
                        inventory: None,
                    },
                )?);
            }
            ensure!(
                sum.is_finite()
                    && magnitude.is_finite()
                    && (sum - run.result.h[mi]).abs()
                        <= 1e-12 * magnitude.max(run.result.h[mi].abs()),
                "study_analysis_contribution_balance"
            );
            ordered.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            hotspots.push(Hotspots {
                run_id,
                method_ref: mr.clone(),
                coverage: cov.into(),
                ordering: "absolute_characterized_contribution_desc_then_fact_id".into(),
                fact_ids: ordered.into_iter().map(|(id, _)| id).collect(),
            });
            for (ii, axis) in n.inventory_axes.iter().enumerate() {
                let missing = run
                    .prepared
                    .coverage
                    .iter()
                    .any(|g| g.method == mr && g.inventory_index == ii);
                facts.push(fact(
                    format!("r{ri}:i:{mi}:{ii}"),
                    "inventoryContribution",
                    vec![run_id],
                    Some(mr.clone()),
                    if missing {
                        None
                    } else {
                        Some(product(n.c[mi][ii], run.result.g[ii])?)
                    },
                    m.unit.clone(),
                    unit(None, Some(mr.clone())),
                    if missing { "unknown" } else { cov },
                    "characterized_inventory_term",
                    Subject {
                        view: None,
                        inventory: Some(axis.clone()),
                    },
                )?);
            }
        }
    }
    if runs.len() == 2 {
        let a = &runs[0];
        let b = &runs[1];
        for (mi, m) in a.prepared.numeric.method_axes.iter().enumerate() {
            let mr = method(a, mi);
            let usable = comp.status != "not_comparable"
                && coverage(a, mi) != "unknown"
                && coverage(b, mi) != "unknown";
            let cov = if !usable {
                "unknown"
            } else if coverage(a, mi) == "complete" && coverage(b, mi) == "complete" {
                "complete"
            } else {
                "partial"
            };
            let delta = usable.then(|| b.result.h[mi] - a.result.h[mi]);
            facts.push(fact(
                format!("comparison:delta:{mi}"),
                "comparisonDelta",
                input.analysis.config.run_ids.clone(),
                Some(mr.clone()),
                delta,
                m.unit.clone(),
                unit(None, Some(mr.clone())),
                cov,
                "numeric_delta_of_characterized_subtotals",
                no_subject(),
            )?);
            let relative = delta
                .filter(|_| a.result.h[mi] != 0.0)
                .map(|v| v / a.result.h[mi].abs());
            facts.push(fact(
                format!("comparison:relative:{mi}"),
                "relativeDelta",
                input.analysis.config.run_ids.clone(),
                Some(mr),
                relative,
                "1".into(),
                unit(None, None),
                if relative.is_some() { cov } else { "unknown" },
                "delta_divided_by_absolute_baseline",
                no_subject(),
            )?);
        }
    }
    let bundle = FactsBundle {
        schema_version: FACTS_SCHEMA.into(),
        profile_id: PROFILE.into(),
        analyzer_version: ANALYZER.into(),
        study_id: input.analysis.study_id,
        analysis_id: input.analysis.analysis_id,
        config: input.analysis.config.clone(),
        config_hash: input.analysis.config_hash.clone(),
        inputs: runs.iter().map(binding).collect::<anyhow::Result<_>>()?,
        comparability: comp,
        facts,
        coverage: runs
            .iter()
            .map(|r| {
                Ok(RunCoverage {
                    run_id: id(&r.input.run, "runId")?,
                    gaps: r.prepared.coverage.clone(),
                    quality_findings: r.prepared.model.quality_findings.clone(),
                    complete_lcia_scores: r.result.complete_lcia_scores,
                })
            })
            .collect::<anyhow::Result<_>>()?,
        scenario_evidence: runs
            .iter()
            .map(|r| {
                Ok(ScenarioEvidence {
                    run_id: id(&r.input.run, "runId")?,
                    scenario_revision_id: id(&r.input.run, "scenarioRevisionId")?,
                    content_hash: r.input.scenario["contentHash"]
                        .as_str()
                        .context("study_analysis_scenario_hash")?
                        .into(),
                    overrides: r.input.scenario["overrides"]
                        .as_array()
                        .context("study_analysis_scenario_overrides")?
                        .iter()
                        .map(|v| {
                            Ok(CanonicalOverride {
                                path: v["path"]
                                    .as_str()
                                    .context("study_analysis_override_path")?
                                    .into(),
                                value: canonical_number(
                                    v["value"]
                                        .as_f64()
                                        .context("study_analysis_override_value")?,
                                )?,
                            })
                        })
                        .collect::<anyhow::Result<_>>()?,
                })
            })
            .collect::<anyhow::Result<_>>()?,
        hotspots,
        business_pilot_qualified: false,
        scientific_ranking_qualified: false,
    };
    ensure!(
        serde_json::to_vec(&bundle)?.len() <= Bounds::default().max_artifact_bytes,
        "study_analysis_artifact_limit"
    );
    Ok(bundle)
}
pub fn validate_output(output: &FactsBundle, input: &ServiceInput) -> anyhow::Result<()> {
    ensure!(
        output.schema_version == FACTS_SCHEMA
            && output.profile_id == PROFILE
            && output.analyzer_version == ANALYZER
            && output.study_id == input.analysis.study_id
            && output.analysis_id == input.analysis.analysis_id
            && output.config_hash == input.analysis.config_hash
            && serde_json::to_value(&output.config)?
                == serde_json::to_value(&input.analysis.config)?
            && output.inputs.len() == input.runs.len()
            && !output.business_pilot_qualified
            && !output.scientific_ranking_qualified
            && !output.comparability.ranking_allowed
            && !output.facts.is_empty()
            && output.facts.len() <= MAX_FACTS,
        "study_analysis_output_binding"
    );
    for (binding, run) in output.inputs.iter().zip(&input.runs) {
        ensure!(
            binding.run_id == id(&run.run, "runId")?
                && binding.study_revision_id == id(&run.run, "studyRevisionId")?
                && binding.scenario_revision_id == id(&run.run, "scenarioRevisionId")?
                && serde_json::to_value(&binding.prepared_artifact_ref)?
                    == serde_json::to_value(&run.prepared_artifact.artifact_ref)?
                && serde_json::to_value(&binding.result_ref)? == run.run["resultRef"]
                && binding.input_hash
                    == run.run["inputHash"]
                        .as_str()
                        .context("study_analysis_input_hash")?,
            "study_analysis_output_run_binding"
        );
        ensure!(
            binding.model_preparation_id == id(&run.run, "modelPreparationId")?
                && binding.effective_model_ref == run.run["effectiveModelRef"]
                && serde_json::to_value(&binding.calculation_contract_ref)?
                    == run.run["calculationContractRef"]
                && binding.validity == run.run["validity"],
            "study_analysis_output_identity_binding"
        );
    }
    ensure!(
        output
            .facts
            .iter()
            .map(|f| &f.fact_id)
            .collect::<BTreeSet<_>>()
            .len()
            == output.facts.len()
            && output.coverage.iter().map(|c| c.run_id).collect::<Vec<_>>()
                == input.analysis.config.run_ids,
        "study_analysis_output_fact_identity"
    );
    for fact in &output.facts {
        validate_fact_codec(fact)?;
        ensure!(
            (fact.canonical_value.is_none()) == (fact.coverage == "unknown")
                && ["complete", "partial", "unknown"].contains(&fact.coverage.as_str())
                && fact.label.len() <= 1024
                && !fact.label.is_empty()
                && fact.unit.len() <= 128
                && !fact.unit.is_empty()
                && !fact.run_ids.is_empty()
                && fact.run_ids.len() <= 2
                && fact
                    .run_ids
                    .iter()
                    .all(|id| input.analysis.config.run_ids.contains(id)),
            "study_analysis_output_fact_shape"
        );
    }
    Ok(())
}
pub(super) fn validate_fact_codec(fact: &Fact) -> anyhow::Result<()> {
    ensure!(
        (fact.canonical_value.is_none()) == (fact.coverage == "unknown")
            && ["complete", "partial", "unknown"].contains(&fact.coverage.as_str()),
        "study_analysis_output_value_coverage"
    );
    ensure!(
        fact.canonical_value.as_ref().is_none_or(|s| s
            .parse::<f64>()
            .ok()
            .and_then(|v| canonical_number(v).ok())
            .as_ref()
            == Some(s)),
        "study_analysis_output_number"
    );
    Ok(())
}
#[must_use]
pub fn summary(bundle: &FactsBundle) -> Value {
    json!({"status":bundle.comparability.status,"reasonCodes":bundle.comparability.reason_codes})
}

/// Private native-file adapter. All paths are parent-created, never actor request fields.
pub fn execute_native(path: &str, limits: &Bounds) -> anyhow::Result<Vec<u8>> {
    fn read(path: &str, max: usize) -> anyhow::Result<Vec<u8>> {
        ensure!(
            std::path::Path::new(path).is_absolute()
                && std::fs::metadata(path)?.len() <= u64::try_from(max)?,
            "study_analysis_input_limit"
        );
        Ok(std::fs::read(path)?)
    }
    let request: NativeInput = super::json::from_slice(&read(path, limits.max_artifact_bytes)?)?;
    ensure!(
        request.schema_version == "lca.study-analysis-native.v1"
            && request.files.len() == request.input.runs.len(),
        "study_analysis_native_schema"
    );
    validate_input(&request.input)?;
    let mut owned = 0usize;
    let mut runs = vec![];
    for (files, input) in request.files.iter().zip(&request.input.runs) {
        ensure!(
            files.run_id == id(&input.run, "runId")?,
            "study_analysis_native_run_order"
        );
        let prepared = read(&files.prepared_path, limits.max_artifact_bytes)?;
        let result = read(&files.result_path, limits.max_artifact_bytes)?;
        owned = owned
            .checked_add(prepared.len())
            .and_then(|v| v.checked_add(result.len()))
            .context("study_analysis_memory_limit")?;
        ensure!(
            owned.saturating_mul(8) <= limits.max_owned_bytes,
            "study_analysis_memory_limit"
        );
        runs.push(verify_run(input.clone(), &prepared, &result, limits)?);
    }
    let bundle = analyze(&request.input, &runs)?;
    validate_output(&bundle, &request.input)?;
    Ok(serde_json::to_vec(&bundle)?)
}

#[cfg(test)]
pub(super) mod tests;
