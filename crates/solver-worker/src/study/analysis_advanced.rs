//! Bounded independent OAT and fixed-budget uncertainty through frozen-source recompilation.
use super::{
    VerifiedMethods,
    analysis::{self, canonical_number},
    analysis_advanced_contracts::{
        BatchLimits, CONFIG_SCHEMA, Config, Counts, Design, Evaluation, EvaluationFailure,
        ExecutionLimits, FACTS_SCHEMA, FactsBundle, GENERATOR, INPUT_SCHEMA, NativeInput,
        NativeLimits, OatEffect, OutputValue, PRODUCER, Parameter, Quantile, ResolvedParameter,
        ServiceInput, Statistics,
    },
    analysis_contracts as old, artifact_bytes,
    contracts::{AmountOverride, Bounds, DatasetRef, PreparedInput, StudyResult},
    execute, prepare,
    runtime_contracts::{SNAPSHOT_SCHEMA, SourceSnapshot},
    semantic_hash, sha256,
};
use anyhow::{Context, ensure};
use std::{collections::BTreeSet, time::Instant};
mod catalog;
mod rng;
use rng::Drbg;

pub fn number(text: &str) -> anyhow::Result<f64> {
    ensure!(text.len() <= 64, "study_advanced_number");
    let value = text.parse::<f64>().context("study_advanced_number")?;
    ensure!(
        canonical_number(value)? == text,
        "study_advanced_canonical_number"
    );
    Ok(value)
}
pub fn validate_config(config: &Config) -> anyhow::Result<()> {
    let schema = match config {
        Config::SensitivityOat { schema_version, .. }
        | Config::Uncertainty { schema_version, .. } => schema_version,
    };
    ensure!(schema == CONFIG_SCHEMA, "study_advanced_config_schema");
    ensure!(
        serde_json::to_vec(config)?.len() <= 262_144,
        "study_advanced_config_byte_limit"
    );
    let id = config.baseline_run_id().parse::<uuid::Uuid>()?;
    ensure!(
        id.to_string() == config.baseline_run_id(),
        "study_advanced_baseline_uuid"
    );
    ensure!(!config.parameters().is_empty(), "study_advanced_parameters");
    let mut paths = BTreeSet::new();
    for p in config.parameters() {
        ensure!(
            paths.insert(&p.path) && !p.unit.is_empty() && p.unit.len() <= 256,
            "study_advanced_duplicate_or_unit"
        );
        let parts = p.path.split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() == 6
                && parts[0].is_empty()
                && parts[1] == "instances"
                && parts[3] == "exchanges"
                && parts[5] == "amount"
                && !parts[2].is_empty()
                && !parts[4].is_empty()
                && p.path.len() <= 256,
            "study_advanced_path"
        );
        let (low, base, high) = (
            number(&p.low)?,
            number(&p.baseline_amount)?,
            number(&p.high)?,
        );
        ensure!(
            low < high && low <= base && base <= high && (high - low).is_finite(),
            "study_advanced_bounds"
        );
    }
    if let Config::Uncertainty {
        design,
        sample_count,
        seed_hex,
        generator,
        ..
    } = config
    {
        ensure!(
            *sample_count >= 2
                && u32::try_from(*sample_count).is_ok()
                && *generator == GENERATOR
                && seed_hex.len() == 64
                && seed_hex
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "study_advanced_sampling_config"
        );
        match design {
            Design::IndependentUniform {
                independence_declared,
            } => ensure!(*independence_declared, "study_advanced_independence"),
            Design::JointEmpirical { rows } => {
                ensure!(!rows.is_empty(), "study_advanced_empirical_rows");
                for row in rows {
                    ensure!(
                        row.len() == config.parameters().len(),
                        "study_advanced_empirical_width"
                    );
                    for (v, p) in row.iter().zip(config.parameters()) {
                        let v = number(v)?;
                        ensure!(
                            v >= number(&p.low)? && v <= number(&p.high)?,
                            "study_advanced_empirical_bounds"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
pub fn config_hash(config: &Config) -> anyhow::Result<String> {
    validate_config(config)?;
    semantic_hash("study.analysis-config.v2", config)
}
fn old_input(input: &ServiceInput) -> anyhow::Result<old::ServiceInput> {
    let config = old::AnalysisConfig {
        schema_version: old::CONFIG_SCHEMA.into(),
        kind: old::AnalysisKind::Contribution,
        run_ids: vec![input.analysis.config.baseline_run_id().parse()?],
    };
    Ok(old::ServiceInput {
        schema_version: old::INPUT_SCHEMA.into(),
        analysis: old::AnalysisRecord {
            analysis_id: input.analysis.analysis_id,
            study_id: input.analysis.study_id,
            profile_id: old::PROFILE.into(),
            config_hash: analysis::config_hash(&config)?,
            config,
            execution: input.analysis.execution.clone(),
        },
        runs: vec![input.baseline.clone()],
    })
}
pub fn validate_input(input: &ServiceInput) -> anyhow::Result<()> {
    ensure!(
        input.schema_version == INPUT_SCHEMA
            && input.analysis.profile_id == input.analysis.config.profile()
            && input.analysis.config_hash == config_hash(&input.analysis.config)?,
        "study_advanced_input"
    );
    analysis::validate_input(&old_input(input)?)?;
    ensure!(
        input.source_snapshot.digest_scope == "artifact_bytes_sha256"
            && input.source_snapshot.origin_ref.r#type == "datasetRevision"
            && input.source_snapshot.origin_ref.dataset_type == "lifecyclemodel",
        "study_advanced_snapshot_metadata"
    );
    let artifact = &input.source_artifact;
    super::runtime::validate_location(artifact, input.analysis.study_id, "modelSource")?;
    ensure!(
        artifact.study_id == input.analysis.study_id
            && artifact.artifact_ref.r#type == "modelSource"
            && artifact.artifact_ref.schema_version == SNAPSHOT_SCHEMA
            && artifact.artifact_ref.digest_scope == "artifact_bytes_sha256"
            && artifact.artifact_ref.media_type == "application/json"
            && artifact.artifact_ref.byte_size > 0
            && input.source_snapshot.study_id == input.analysis.study_id
            && serde_json::to_value(&input.source_snapshot.source_artifact_ref)?
                == serde_json::to_value(&artifact.artifact_ref)?,
        "study_advanced_snapshot_binding"
    );
    Ok(())
}
fn check_clock(start: Instant, bounds: &Bounds) -> anyhow::Result<()> {
    ensure!(
        start.elapsed().as_millis() < u128::from(bounds.max_elapsed_millis),
        "study_advanced_batch_deadline"
    );
    Ok(())
}
fn check_parameter(p: &Parameter, base: &PreparedInput) -> anyhow::Result<()> {
    let parts = p.path.split('/').collect::<Vec<_>>();
    let instance = base
        .model
        .instances
        .iter()
        .find(|i| i.id == parts[2])
        .context("study_advanced_instance")?;
    ensure!(
        instance.reference_exchange_id != parts[4],
        "study_reference_override_unsupported"
    );
    let e = instance
        .exchanges
        .iter()
        .find(|e| e.id == parts[4])
        .context("study_advanced_exchange")?;
    ensure!(
        e.unit == p.unit && e.amount.to_bits() == number(&p.baseline_amount)?.to_bits(),
        "study_advanced_parameter_baseline"
    );
    Ok(())
}
fn effective_source(
    packet: &SourceSnapshot,
    base: &PreparedInput,
    changes: Vec<AmountOverride>,
) -> anyhow::Result<super::contracts::SourceInput> {
    let mut source = packet.source.clone();
    super::runtime_child::validate_base_source(base, &source)?;
    source.method_refs = base
        .numeric
        .method_axes
        .iter()
        .map(|m| DatasetRef {
            id: m.id.clone(),
            version: m.version.clone(),
        })
        .collect();
    source.cutoffs.clone_from(&base.model.cutoffs);
    source.overrides = super::runtime_child::merge_overrides(&base.model.overrides, changes)?;
    source.study_binding.clone_from(&base.study_binding);
    Ok(source)
}
fn values(p: &PreparedInput, r: &StudyResult) -> anyhow::Result<Vec<OutputValue>> {
    p.numeric
        .method_axes
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let method = DatasetRef {
                id: m.id.clone(),
                version: m.version.clone(),
            };
            let matched = p.numeric.inventory_axes.len()
                - p.coverage.iter().filter(|g| g.method == method).count();
            let coverage = if matched == 0 {
                "unknown"
            } else if matched < p.numeric.inventory_axes.len() {
                "partial"
            } else {
                "complete"
            };
            Ok(OutputValue {
                method_ref: method,
                unit: m.unit.clone(),
                canonical_value: if matched == 0 {
                    None
                } else {
                    Some(canonical_number(r.h[index])?)
                },
                coverage: coverage.into(),
            })
        })
        .collect()
}
fn evaluation(
    id: String,
    kind: &str,
    parameter_index: Option<usize>,
    row: Option<usize>,
    amounts: Vec<String>,
    p: &PreparedInput,
    r: &StudyResult,
) -> anyhow::Result<Evaluation> {
    Ok(Evaluation {
        evaluation_id: id,
        kind: kind.into(),
        parameter_index,
        empirical_row_index: row,
        amounts,
        input_hash: p.input_hash.clone().context("study_advanced_input_hash")?,
        model_hash: p.model_hash.clone(),
        prepared_sha256: sha256(&artifact_bytes(p)?),
        result_sha256: sha256(&serde_json::to_vec(r)?),
        outputs: values(p, r)?,
        max_residual: canonical_number(r.max_residual)?,
    })
}
fn checked_product(a: f64, b: f64) -> anyhow::Result<f64> {
    let out = a * b;
    ensure!(
        out.is_finite() && (out != 0.0 || a == 0.0 || b == 0.0),
        "study_advanced_arithmetic_range"
    );
    Ok(out)
}
fn checked_quotient(a: f64, b: f64) -> anyhow::Result<f64> {
    let out = a / b;
    ensure!(
        out.is_finite() && (out != 0.0 || a == 0.0),
        "study_advanced_arithmetic_range"
    );
    Ok(out)
}
fn delta(a: Option<&String>, b: Option<&String>) -> anyhow::Result<Option<String>> {
    match (a, b) {
        (Some(a), Some(b)) => Ok(Some(canonical_number(number(a)? - number(b)?)?)),
        _ => Ok(None),
    }
}
fn relative(delta: Option<&String>, base: Option<&String>) -> anyhow::Result<Option<String>> {
    match (delta, base) {
        (Some(d), Some(b)) if number(b)? != 0.0 => Ok(Some(canonical_number(checked_quotient(
            number(d)?,
            number(b)?.abs(),
        )?)?)),
        _ => Ok(None),
    }
}
fn effects(
    config: &Config,
    baseline: &Evaluation,
    evaluations: &[Evaluation],
) -> anyhow::Result<Vec<OatEffect>> {
    let mut effects = vec![];
    for (i, p) in config.parameters().iter().enumerate() {
        let (lo, hi) = (&evaluations[2 * i], &evaluations[2 * i + 1]);
        for (m, b) in baseline.outputs.iter().enumerate() {
            let l = delta(
                lo.outputs[m].canonical_value.as_ref(),
                b.canonical_value.as_ref(),
            )?;
            let h = delta(
                hi.outputs[m].canonical_value.as_ref(),
                b.canonical_value.as_ref(),
            )?;
            let secant = delta(
                hi.outputs[m].canonical_value.as_ref(),
                lo.outputs[m].canonical_value.as_ref(),
            )?
            .map(|d| {
                canonical_number(checked_quotient(
                    number(&d)?,
                    number(&p.high)? - number(&p.low)?,
                )?)
            })
            .transpose()?;
            let mut reason_codes = vec![];
            match &b.canonical_value {
                None => reason_codes.push("no_characterized_terms".into()),
                Some(v) if number(v)? == 0.0 => {
                    reason_codes.push("zero_baseline_relative_undefined".into());
                }
                Some(_) => {}
            }
            effects.push(OatEffect {
                parameter_index: i,
                method_ref: b.method_ref.clone(),
                output_unit: b.unit.clone(),
                input_unit: p.unit.clone(),
                coverage: b.coverage.clone(),
                low_evaluation_id: lo.evaluation_id.clone(),
                high_evaluation_id: hi.evaluation_id.clone(),
                low_relative_delta: relative(l.as_ref(), b.canonical_value.as_ref())?,
                high_relative_delta: relative(h.as_ref(), b.canonical_value.as_ref())?,
                low_absolute_delta: l,
                high_absolute_delta: h,
                secant,
                reason_codes,
            });
        }
    }
    Ok(effects)
}
fn execution_limits(bounds: &Bounds, batch: &BatchLimits) -> anyhow::Result<ExecutionLimits> {
    Ok(ExecutionLimits {
        batch: batch.clone(),
        native: NativeLimits {
            max_source_bytes: bounds.max_source_bytes,
            max_documents: bounds.max_documents,
            max_instances: bounds.max_instances,
            max_views: bounds.max_views,
            max_exchanges: bounds.max_exchanges,
            max_methods: bounds.max_methods,
            max_nonzeros: bounds.max_nonzeros,
            max_dense_cells: bounds.max_dense_cells,
            max_artifact_bytes: bounds.max_artifact_bytes,
            max_elapsed_millis: bounds.max_elapsed_millis,
            max_owned_bytes: bounds.max_owned_bytes,
            max_functional_unit_amount: canonical_number(bounds.max_functional_unit_amount)?,
        },
    })
}
fn count_float(n: usize) -> anyhow::Result<f64> {
    Ok(f64::from(
        u32::try_from(n).context("study_advanced_count_limit")?,
    ))
}
/// Hyndman-Fan type 7, zero-based h=(n-1)p; n=1 returns the sole observation.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn quantile(sorted: &[f64], p: f64) -> anyhow::Result<f64> {
    ensure!(
        !sorted.is_empty() && (0.0..=1.0).contains(&p),
        "study_advanced_quantile"
    );
    let h = count_float(sorted.len() - 1)? * p;
    let j = h.floor() as usize;
    let w = h - count_float(j)?;
    let result = if w == 0.0 {
        sorted[j]
    } else {
        checked_product(sorted[j], 1.0 - w)? + checked_product(sorted[j + 1], w)?
    };
    ensure!(result.is_finite(), "study_advanced_stat_nonfinite");
    Ok(result)
}
fn statistics(
    baseline: &Evaluation,
    evaluations: &[Evaluation],
) -> anyhow::Result<Vec<Statistics>> {
    baseline
        .outputs
        .iter()
        .enumerate()
        .map(|(m, b)| {
            let mut xs = Vec::with_capacity(evaluations.len());
            let (mut mean, mut m2) = (0.0, 0.0);
            for e in evaluations {
                if let Some(s) = &e.outputs[m].canonical_value {
                    let x = number(s)?;
                    xs.push(x);
                    let d = x - mean;
                    mean += checked_quotient(d, count_float(xs.len())?)?;
                    m2 += checked_product(d, x - mean)?;
                    ensure!(
                        mean.is_finite() && m2.is_finite() && m2 >= 0.0,
                        "study_advanced_stat_nonfinite"
                    );
                }
            }
            ensure!(
                xs.is_empty() || xs.len() == evaluations.len(),
                "study_advanced_conditional_subset"
            );
            xs.sort_by(f64::total_cmp);
            let known = !xs.is_empty();
            let sd = if known {
                Some(canonical_number(
                    checked_quotient(m2, count_float(xs.len() - 1)?)?.sqrt(),
                )?)
            } else {
                None
            };
            Ok(Statistics {
                method_ref: b.method_ref.clone(),
                unit: b.unit.clone(),
                coverage: b.coverage.clone(),
                mean: known.then(|| canonical_number(mean)).transpose()?,
                mean_mcse: sd
                    .as_ref()
                    .map(|s| {
                        canonical_number(checked_quotient(
                            number(s)?,
                            count_float(xs.len())?.sqrt(),
                        )?)
                    })
                    .transpose()?,
                sample_standard_deviation: sd,
                minimum: xs.first().map(|v| canonical_number(*v)).transpose()?,
                maximum: xs.last().map(|v| canonical_number(*v)).transpose()?,
                quantiles: [("0.025", 0.025), ("0.5", 0.5), ("0.975", 0.975)]
                    .into_iter()
                    .map(|(p, n)| {
                        Ok(Quantile {
                            probability: p.into(),
                            value: known
                                .then(|| quantile(&xs, n).and_then(canonical_number))
                                .transpose()?,
                        })
                    })
                    .collect::<anyhow::Result<_>>()?,
            })
        })
        .collect()
}
// Conservative serialized evidence preflight using actual identities/units and the codec's
// 64-byte value cap. Actual incremental and final serialization are checked independently.
fn preflight_evidence(
    input: &ServiceInput,
    base: &PreparedInput,
    baseline: &Evaluation,
    n: usize,
    owned: usize,
    bounds: &Bounds,
) -> anyhow::Result<()> {
    let mut sample = baseline.clone();
    sample.evaluation_id = format!("evaluation-{}", usize::MAX);
    sample.kind = "sample".into();
    sample.parameter_index = Some(usize::MAX);
    sample.empirical_row_index = Some(usize::MAX);
    sample.max_residual = "1".repeat(64);
    sample.amounts = vec!["1".repeat(64); input.analysis.config.parameters().len()];
    for output in &mut sample.outputs {
        output.canonical_value = Some("1".repeat(64));
    }
    let evaluations = serde_json::to_vec(&sample)?
        .len()
        .saturating_add(1)
        .saturating_mul(n);
    let mut summary = 0usize;
    for parameter in input.analysis.config.parameters() {
        for method in &base.numeric.method_axes {
            summary = summary
                .saturating_add(2048)
                .saturating_add(serde_json::to_vec(method)?.len().saturating_mul(2))
                .saturating_add(parameter.unit.len().saturating_mul(6));
        }
    }
    let fixed = serde_json::to_vec(input)?
        .len()
        .saturating_add(serde_json::to_vec(baseline)?.len())
        .saturating_add(serde_json::to_vec(&base.model)?.len())
        .saturating_add(4096);
    let estimated = evaluations
        .saturating_add(summary)
        .saturating_add(fixed)
        .saturating_add(catalog::preflight_bytes(&input.analysis.config, base)?);
    ensure!(
        estimated <= bounds.max_artifact_bytes,
        "study_advanced_artifact_preflight_limit"
    );
    ensure!(
        owned
            .saturating_mul(12)
            .saturating_add(estimated.saturating_mul(8))
            <= bounds.max_owned_bytes,
        "study_advanced_memory_limit"
    );
    Ok(())
}
/// All-or-error: invalid draws, deadline and byte/memory limits produce no partial successful facts.
#[allow(clippy::too_many_lines)]
pub fn analyze(
    input: &ServiceInput,
    prepared_bytes: &[u8],
    result_bytes: &[u8],
    snapshot_bytes: &[u8],
    bounds: &Bounds,
    batch: &BatchLimits,
) -> anyhow::Result<FactsBundle> {
    analyze_at(
        input,
        prepared_bytes,
        result_bytes,
        snapshot_bytes,
        bounds,
        batch,
        Instant::now(),
    )
}
#[allow(clippy::too_many_lines)]
fn analyze_at(
    input: &ServiceInput,
    prepared_bytes: &[u8],
    result_bytes: &[u8],
    snapshot_bytes: &[u8],
    bounds: &Bounds,
    batch: &BatchLimits,
    started: Instant,
) -> anyhow::Result<FactsBundle> {
    bounds.validate()?;
    validate_input(input)?;
    let config = &input.analysis.config;
    let n = match config {
        Config::SensitivityOat { parameters, .. } => parameters
            .len()
            .checked_mul(2)
            .context("study_advanced_evaluation_limit")?,
        Config::Uncertainty { sample_count, .. } => *sample_count,
    };
    ensure!(
        config.parameters().len() <= batch.max_parameters
            && n.checked_add(1).is_some_and(|v| v <= batch.max_evaluations),
        "study_advanced_evaluation_limit"
    );
    let owned = prepared_bytes
        .len()
        .saturating_add(result_bytes.len())
        .saturating_add(snapshot_bytes.len())
        .saturating_add(serde_json::to_vec(input)?.len());
    ensure!(
        owned.saturating_mul(12) <= bounds.max_owned_bytes,
        "study_advanced_memory_limit"
    );
    ensure!(
        snapshot_bytes.len() == usize::try_from(input.source_artifact.artifact_ref.byte_size)?
            && sha256(snapshot_bytes) == input.source_artifact.artifact_ref.content_hash,
        "study_advanced_snapshot_integrity"
    );
    let packet: SourceSnapshot = super::json::from_slice(snapshot_bytes)?;
    ensure!(
        packet.schema_version == SNAPSHOT_SCHEMA
            && packet.source.schema_version == super::contracts::SOURCE_SCHEMA
            && packet.source.method_refs.is_empty()
            && packet.source.cutoffs.is_empty()
            && packet.source.overrides.is_empty()
            && packet.source.study_binding.is_none()
            && packet.origin_ref["id"] == packet.source.origin.id
            && packet.origin_ref["version"] == packet.source.origin.version,
        "study_advanced_snapshot_raw"
    );
    ensure!(
        packet.origin_ref == serde_json::to_value(&input.source_snapshot.origin_ref)?
            && packet.license_ref == input.source_snapshot.license_ref
            && packet.issuer_id == input.source_snapshot.issuer_id
            && chrono::DateTime::parse_from_rfc3339(&packet.observed_at)?
                == chrono::DateTime::parse_from_rfc3339(&input.source_snapshot.observed_at)?,
        "study_advanced_snapshot_metadata"
    );
    let verified =
        analysis::verify_run(input.baseline.clone(), prepared_bytes, result_bytes, bounds)?;
    let base = &verified.prepared;
    for p in config.parameters() {
        check_parameter(p, base)?;
    }
    let methods = VerifiedMethods::from_frozen(
        base,
        &base
            .numeric
            .method_axes
            .iter()
            .map(|m| DatasetRef {
                id: m.id.clone(),
                version: m.version.clone(),
            })
            .collect::<Vec<_>>(),
    )?;
    let source = effective_source(&packet, base, vec![])?;
    check_clock(started, bounds)?;
    let recompiled = prepare(&serde_json::to_vec(&source)?, &methods, bounds)?;
    drop(source);
    ensure!(
        recompiled.model_hash == base.model_hash
            && recompiled.input_hash == base.input_hash
            && recompiled.numeric == base.numeric
            && recompiled.coverage == base.coverage,
        "study_advanced_baseline_recompile"
    );
    let baseline_result = execute(&recompiled)?;
    // Compare all vectors with the existing verified frozen-result tolerance.
    for (a, b) in baseline_result
        .x
        .iter()
        .chain(&baseline_result.g)
        .chain(&baseline_result.h)
        .zip(
            verified
                .result
                .x
                .iter()
                .chain(&verified.result.g)
                .chain(&verified.result.h),
        )
    {
        ensure!(
            a.to_bits() == b.to_bits() || (a - b).abs() <= 1e-12 * a.abs().max(b.abs()),
            "study_advanced_baseline_replay"
        );
    }
    check_clock(started, bounds)?;
    let baseline_amounts = config
        .parameters()
        .iter()
        .map(|p| p.baseline_amount.clone())
        .collect::<Vec<_>>();
    let baseline_evaluation = evaluation(
        "baseline".into(),
        "baseline",
        None,
        None,
        baseline_amounts.clone(),
        base,
        &verified.result,
    )?;
    preflight_evidence(input, base, &baseline_evaluation, n, owned, bounds)?;
    // The replay working model is released before sequential samples are compiled.
    drop(recompiled);
    let mut rng = if let Config::Uncertainty { seed_hex, .. } = config {
        Some(Drbg::instantiate(
            &hex::decode(seed_hex)?,
            &[],
            GENERATOR.as_bytes(),
        ))
    } else {
        None
    };
    let mut evaluations = Vec::new();
    let mut evidence_bytes = serde_json::to_vec(&baseline_evaluation)?.len();
    for index in 0..n {
        check_clock(started, bounds)?;
        let (amounts, parameter, row, kind) = match config {
            Config::SensitivityOat { parameters, .. } => {
                let p = index / 2;
                let mut amounts = baseline_amounts.clone();
                amounts[p] = if index % 2 == 0 {
                    parameters[p].low.clone()
                } else {
                    parameters[p].high.clone()
                };
                (
                    amounts,
                    Some(p),
                    None,
                    if index % 2 == 0 { "low" } else { "high" },
                )
            }
            Config::Uncertainty { design, .. } => {
                let rng = rng.as_mut().expect("uncertainty generator");
                match design {
                    Design::IndependentUniform { .. } => {
                        let amounts = config
                            .parameters()
                            .iter()
                            .map(|p| {
                                let low = number(&p.low)?;
                                let high = number(&p.high)?;
                                let u = rng.uniform();
                                let value = low + checked_product(high - low, u)?;
                                ensure!(
                                    value.is_finite() && value >= low && value <= high,
                                    "study_advanced_uniform_range"
                                );
                                canonical_number(value)
                            })
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        (amounts, None, None, "sample")
                    }
                    Design::JointEmpirical { rows } => {
                        let row = rng.row(rows.len())?;
                        (rows[row].clone(), None, Some(row), "sample")
                    }
                }
            }
        };
        let changes = config
            .parameters()
            .iter()
            .zip(&amounts)
            .map(|(p, v)| {
                Ok(AmountOverride {
                    path: p.path.clone(),
                    value: number(v)?,
                })
            })
            .collect::<anyhow::Result<_>>()?;
        let effective = effective_source(&packet, base, changes)?;
        let prepared =
            prepare(&serde_json::to_vec(&effective)?, &methods, bounds).map_err(|_| {
                EvaluationFailure {
                    requested: n,
                    attempted: index + 1,
                    valid: index,
                    invalid: 1,
                    stop_reason: "invalid_evaluation".into(),
                    evaluation_id: format!("evaluation-{index}"),
                }
            })?;
        let result = execute(&prepared).map_err(|_| EvaluationFailure {
            requested: n,
            attempted: index + 1,
            valid: index,
            invalid: 1,
            stop_reason: "invalid_evaluation".into(),
            evaluation_id: format!("evaluation-{index}"),
        })?;
        check_clock(started, bounds)?;
        ensure!(
            prepared.numeric.method_axes == base.numeric.method_axes
                && prepared.numeric.inventory_axes == base.numeric.inventory_axes
                && prepared.coverage == base.coverage
                && prepared.method_source_hash == base.method_source_hash,
            "study_advanced_axes_changed"
        );
        let e = evaluation(
            format!("evaluation-{index}"),
            kind,
            parameter,
            row,
            amounts,
            &prepared,
            &result,
        )?;
        evidence_bytes = evidence_bytes
            .checked_add(serde_json::to_vec(&e)?.len())
            .context("study_advanced_artifact_limit")?;
        ensure!(
            evidence_bytes <= bounds.max_artifact_bytes
                && owned
                    .saturating_mul(12)
                    .saturating_add(evidence_bytes.saturating_mul(8))
                    <= bounds.max_owned_bytes,
            "study_advanced_evidence_limit"
        );
        evaluations.push(e);
    }
    let (oat_effects, stats) = match config {
        Config::SensitivityOat { .. } => {
            (effects(config, &baseline_evaluation, &evaluations)?, vec![])
        }
        Config::Uncertainty { .. } => (vec![], statistics(&baseline_evaluation, &evaluations)?),
    };
    let resolved_parameters = config
        .parameters()
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let parts = p.path.split('/').collect::<Vec<_>>();
            let e = base
                .model
                .instances
                .iter()
                .find(|i| i.id == parts[2])
                .expect("validated instance")
                .exchanges
                .iter()
                .find(|e| e.id == parts[4])
                .expect("validated exchange");
            ResolvedParameter {
                parameter_index: index,
                path: p.path.clone(),
                flow: e.flow.clone(),
                direction: e.direction.clone(),
                unit: e.unit.clone(),
                support: e.unit_identity.clone(),
                baseline_amount: p.baseline_amount.clone(),
            }
        })
        .collect();
    let mut out = FactsBundle {
        schema_version: FACTS_SCHEMA.into(),
        profile_id: config.profile().into(),
        producer_version: PRODUCER.into(),
        study_id: input.analysis.study_id,
        analysis_id: input.analysis.analysis_id,
        config: config.clone(),
        config_hash: input.analysis.config_hash.clone(),
        execution_limits: execution_limits(bounds, batch)?,
        baseline: analysis::binding(&verified)?,
        source_snapshot_ref: input.source_artifact.artifact_ref.clone(),
        resolved_parameters,
        coverage_gaps: base.coverage.clone(),
        quality_findings: base.model.quality_findings.clone(),
        baseline_evaluation,
        evaluations,
        oat_effects,
        statistics: stats,
        fact_catalog: vec![],
        counts: Counts {
            requested: n,
            attempted: n,
            valid: n,
            invalid: 0,
            stop_reason: "fixed_budget_complete".into(),
        },
        limitations: vec![
            "caller_declared_ranges_not_source_uncertainty".into(),
            "no_scientific_ranking_or_recommendation".into(),
            if matches!(config, Config::SensitivityOat { .. }) {
                "range_dependent_secants_not_derivatives_or_interactions"
            } else {
                "fixed_budget_not_convergence_or_tail_precision"
            }
            .into(),
        ],
        scientific_ranking_qualified: false,
        business_pilot_qualified: false,
    };
    out.fact_catalog = catalog::build(&out)?;
    ensure!(
        serde_json::to_vec(&out)?.len() <= bounds.max_artifact_bytes,
        "study_advanced_artifact_limit"
    );
    check_clock(started, bounds)?;
    Ok(out)
}
pub fn execute_native(path: &str, bounds: &Bounds) -> anyhow::Result<Vec<u8>> {
    fn read(path: &str, max: usize) -> anyhow::Result<Vec<u8>> {
        ensure!(
            std::path::Path::new(path).is_absolute()
                && std::fs::metadata(path)?.len() <= u64::try_from(max)?,
            "study_advanced_input_limit"
        );
        Ok(std::fs::read(path)?)
    }
    let started = Instant::now();
    let request: NativeInput = super::json::from_slice(&read(path, bounds.max_artifact_bytes)?)?;
    ensure!(
        request.schema_version == "lca.study-analysis-native.v2",
        "study_advanced_native_schema"
    );
    let prepared = read(&request.prepared_path, bounds.max_artifact_bytes)?;
    let result = read(&request.result_path, bounds.max_artifact_bytes)?;
    let source = read(&request.snapshot_path, bounds.max_source_bytes)?;
    let bundle = analyze_at(
        &request.input,
        &prepared,
        &result,
        &source,
        bounds,
        &request.batch_limits,
        started,
    )?;
    validate_output(
        &bundle,
        &request.input,
        &prepared,
        bounds,
        &request.batch_limits,
    )?;
    let bytes = serde_json::to_vec(&bundle)?;
    check_clock(started, bounds)?;
    Ok(bytes)
}
#[cfg(test)]
mod tests;

/// Parent-side purpose/codec checks before registering an immutable native facts artifact.
#[allow(clippy::too_many_lines)]
pub fn validate_output(
    bundle: &FactsBundle,
    input: &ServiceInput,
    prepared_bytes: &[u8],
    bounds: &Bounds,
    batch: &BatchLimits,
) -> anyhow::Result<()> {
    let prepared = super::decode_prepared(
        prepared_bytes,
        &input.baseline.prepared_artifact.artifact_ref.content_hash,
        bounds,
    )?;
    validate_input(input)?;
    ensure!(
        serde_json::to_value(&bundle.execution_limits)?
            == serde_json::to_value(execution_limits(bounds, batch)?)?,
        "study_advanced_output_limits"
    );
    ensure!(
        bundle.schema_version == FACTS_SCHEMA
            && bundle.profile_id == input.analysis.profile_id
            && bundle.producer_version == PRODUCER
            && bundle.study_id == input.analysis.study_id
            && bundle.analysis_id == input.analysis.analysis_id
            && bundle.config_hash == input.analysis.config_hash
            && serde_json::to_value(&bundle.config)?
                == serde_json::to_value(&input.analysis.config)?
            && !bundle.scientific_ranking_qualified
            && !bundle.business_pilot_qualified,
        "study_advanced_output_binding"
    );
    let run = &input.baseline.run;
    ensure!(
        bundle.baseline.run_id.to_string() == input.analysis.config.baseline_run_id()
            && run["studyRevisionId"] == bundle.baseline.study_revision_id.to_string()
            && run["scenarioRevisionId"] == bundle.baseline.scenario_revision_id.to_string()
            && run["modelPreparationId"] == bundle.baseline.model_preparation_id.to_string()
            && run["inputHash"] == bundle.baseline.input_hash
            && run["effectiveModelRef"] == bundle.baseline.effective_model_ref
            && run["calculationContractRef"]
                == serde_json::to_value(&bundle.baseline.calculation_contract_ref)?
            && serde_json::to_value(&bundle.baseline.prepared_artifact_ref)?
                == serde_json::to_value(&input.baseline.prepared_artifact.artifact_ref)?
            && serde_json::to_value(&bundle.baseline.result_ref)?
                == serde_json::to_value(&input.baseline.result_artifact.artifact_ref)?
            && serde_json::to_value(&bundle.source_snapshot_ref)?
                == serde_json::to_value(&input.source_artifact.artifact_ref)?,
        "study_advanced_output_baseline"
    );
    let config = &input.analysis.config;
    let n = match config {
        Config::SensitivityOat { parameters, .. } => parameters
            .len()
            .checked_mul(2)
            .context("study_advanced_evaluation_limit")?,
        Config::Uncertainty { sample_count, .. } => *sample_count,
    };
    ensure!(
        bundle.counts.requested == n
            && bundle.counts.attempted == n
            && bundle.counts.valid == n
            && bundle.counts.invalid == 0
            && bundle.counts.stop_reason == "fixed_budget_complete"
            && bundle.evaluations.len() == n
            && bundle.resolved_parameters.len() == config.parameters().len(),
        "study_advanced_output_counts"
    );
    ensure!(
        bundle.baseline.method_source_hash == prepared.method_source_hash
            && bundle.coverage_gaps == prepared.coverage
            && bundle.quality_findings == prepared.model.quality_findings,
        "study_advanced_output_frozen_evidence"
    );
    let base = &bundle.baseline_evaluation;
    ensure!(
        base.outputs.len() == prepared.numeric.method_axes.len(),
        "study_advanced_output_methods"
    );
    for (actual, method) in base.outputs.iter().zip(&prepared.numeric.method_axes) {
        let method_ref = DatasetRef {
            id: method.id.clone(),
            version: method.version.clone(),
        };
        let matched = prepared.numeric.inventory_axes.len()
            - prepared
                .coverage
                .iter()
                .filter(|g| g.method == method_ref)
                .count();
        let coverage = if matched == 0 {
            "unknown"
        } else if matched < prepared.numeric.inventory_axes.len() {
            "partial"
        } else {
            "complete"
        };
        ensure!(
            actual.method_ref == method_ref
                && actual.unit == method.unit
                && actual.coverage == coverage,
            "study_advanced_output_unit_coverage"
        );
    }
    ensure!(
        base.evaluation_id == "baseline"
            && base.kind == "baseline"
            && base.parameter_index.is_none()
            && base.empirical_row_index.is_none()
            && base.input_hash == bundle.baseline.input_hash
            && run["effectiveModelRef"]["contentHash"] == base.model_hash
            && base.prepared_sha256 == input.baseline.prepared_artifact.artifact_ref.content_hash
            && base.result_sha256 == input.baseline.result_artifact.artifact_ref.content_hash
            && base.amounts
                == config
                    .parameters()
                    .iter()
                    .map(|p| p.baseline_amount.clone())
                    .collect::<Vec<_>>(),
        "study_advanced_output_baseline_evaluation"
    );
    let methods = input.baseline.revision["definition"]["methodRefs"]
        .as_array()
        .context("study_advanced_output_methods")?;
    ensure!(
        methods.len() == base.outputs.len(),
        "study_advanced_output_methods"
    );
    for (index, (p, resolved)) in config
        .parameters()
        .iter()
        .zip(&bundle.resolved_parameters)
        .enumerate()
    {
        check_parameter(p, &prepared)?;
        let parts = p.path.split('/').collect::<Vec<_>>();
        let exchange = prepared
            .model
            .instances
            .iter()
            .find(|i| i.id == parts[2])
            .context("study_advanced_output_parameter")?
            .exchanges
            .iter()
            .find(|e| e.id == parts[4])
            .context("study_advanced_output_parameter")?;
        ensure!(
            resolved.parameter_index == index
                && resolved.path == p.path
                && resolved.unit == exchange.unit
                && resolved.baseline_amount == p.baseline_amount
                && resolved.flow == exchange.flow
                && resolved.direction == exchange.direction
                && resolved.support == exchange.unit_identity,
            "study_advanced_output_parameter"
        );
    }
    let mut replay_rng = if let Config::Uncertainty { seed_hex, .. } = config {
        Some(Drbg::instantiate(
            &hex::decode(seed_hex)?,
            &[],
            GENERATOR.as_bytes(),
        ))
    } else {
        None
    };
    for (index, e) in std::iter::once(base).chain(&bundle.evaluations).enumerate() {
        number(&e.max_residual)?;
        ensure!(
            e.amounts.len() == config.parameters().len() && e.outputs.len() == methods.len(),
            "study_advanced_output_shape"
        );
        for (o, m) in e.outputs.iter().zip(methods) {
            ensure!(
                m["id"] == o.method_ref.id
                    && m["version"] == o.method_ref.version
                    && matches!(o.coverage.as_str(), "complete" | "partial" | "unknown")
                    && (o.coverage == "unknown") == o.canonical_value.is_none(),
                "study_advanced_output_method"
            );
            if let Some(v) = &o.canonical_value {
                number(v)?;
            }
        }
        if index == 0 {
            continue;
        }
        let index = index - 1;
        ensure!(
            e.evaluation_id == format!("evaluation-{index}"),
            "study_advanced_output_order"
        );
        for ((v, p), b) in e.outputs.iter().zip(&base.outputs).zip(methods) {
            ensure!(
                v.unit == p.unit && v.coverage == p.coverage && b["id"] == v.method_ref.id,
                "study_advanced_output_unit_coverage"
            );
        }
        for (v, p) in e.amounts.iter().zip(config.parameters()) {
            let v = number(v)?;
            ensure!(
                v >= number(&p.low)? && v <= number(&p.high)?,
                "study_advanced_output_amount"
            );
        }
        match config {
            Config::SensitivityOat { parameters, .. } => {
                let p = index / 2;
                let mut expected = base.amounts.clone();
                expected[p] = if index % 2 == 0 {
                    parameters[p].low.clone()
                } else {
                    parameters[p].high.clone()
                };
                ensure!(
                    e.amounts == expected
                        && e.parameter_index == Some(p)
                        && e.empirical_row_index.is_none()
                        && e.kind == if index % 2 == 0 { "low" } else { "high" },
                    "study_advanced_output_oat"
                );
            }
            Config::Uncertainty { design, .. } => {
                ensure!(
                    e.kind == "sample" && e.parameter_index.is_none(),
                    "study_advanced_output_sample"
                );
                let rng = replay_rng.as_mut().expect("uncertainty replay generator");
                match design {
                    Design::IndependentUniform { .. } => {
                        ensure!(
                            e.empirical_row_index.is_none(),
                            "study_advanced_output_sample"
                        );
                        let expected = config
                            .parameters()
                            .iter()
                            .map(|p| {
                                let low = number(&p.low)?;
                                let high = number(&p.high)?;
                                canonical_number(low + checked_product(high - low, rng.uniform())?)
                            })
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        ensure!(e.amounts == expected, "study_advanced_output_rng");
                    }
                    Design::JointEmpirical { rows } => {
                        let row = rng.row(rows.len())?;
                        ensure!(
                            e.empirical_row_index == Some(row) && rows.get(row) == Some(&e.amounts),
                            "study_advanced_output_rng"
                        );
                    }
                }
            }
        }
    }
    let (expected_effects, expected_stats) = match config {
        Config::SensitivityOat { .. } => (effects(config, base, &bundle.evaluations)?, vec![]),
        Config::Uncertainty { .. } => (vec![], statistics(base, &bundle.evaluations)?),
    };
    ensure!(
        serde_json::to_value(&bundle.oat_effects)? == serde_json::to_value(&expected_effects)?
            && serde_json::to_value(&bundle.statistics)? == serde_json::to_value(&expected_stats)?,
        "study_advanced_output_summary"
    );
    ensure!(
        serde_json::to_value(&bundle.fact_catalog)?
            == serde_json::to_value(catalog::build(bundle)?)?,
        "study_advanced_output_catalog"
    );
    Ok(())
}
