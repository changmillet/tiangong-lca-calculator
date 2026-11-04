//! Complete Worker-owned report scalar selectors; Monte Carlo samples remain full artifact evidence.
use super::super::analysis_advanced_contracts::{
    AmountRole, Endpoint, ExchangeQuantity, FactContext, FactsBundle, MethodQuantity, ReportFact,
    ReportUnitIdentity, ResolvedParameter, Statistic,
};
use anyhow::{Context, ensure};
use uuid::Uuid;
fn exchange(p: &ResolvedParameter) -> ExchangeQuantity {
    ExchangeQuantity {
        parameter_index: p.parameter_index,
        flow: p.flow.clone(),
        direction: p.direction.clone(),
        unit: p.unit.clone(),
        support: p.support.clone(),
    }
}
fn parameter_unit(p: &ResolvedParameter) -> ReportUnitIdentity {
    ReportUnitIdentity::ExchangeAmount {
        parameter_index: p.parameter_index,
        flow: p.flow.clone(),
        direction: p.direction.clone(),
        unit: p.unit.clone(),
        support: p.support.clone(),
    }
}
fn method_unit(method: &super::super::contracts::DatasetRef, unit: &str) -> ReportUnitIdentity {
    ReportUnitIdentity::MethodQuantity {
        method_ref: method.clone(),
        unit: unit.into(),
    }
}
#[allow(clippy::too_many_arguments)]
fn scalar(
    id: String,
    baseline: Uuid,
    context: FactContext,
    method: Option<super::super::contracts::DatasetRef>,
    value: Option<String>,
    unit: String,
    identity: ReportUnitIdentity,
    coverage: &str,
    relative: bool,
) -> ReportFact {
    let reason = if value.is_none() {
        Some(if relative && coverage != "unknown" {
            "zero_baseline_relative_undefined"
        } else {
            "no_characterized_terms"
        })
    } else {
        None
    };
    ReportFact {
        fact_id: id,
        baseline_run_id: baseline,
        context,
        method_ref: method,
        canonical_value: value,
        unit,
        unit_identity: identity,
        coverage: coverage.into(),
        reason_codes: reason.map_or_else(Vec::new, |r| vec![r.into()]),
    }
}
#[allow(clippy::too_many_lines)]
pub(super) fn build(bundle: &FactsBundle) -> anyhow::Result<Vec<ReportFact>> {
    let baseline = bundle.baseline.run_id;
    let mut out = vec![];
    for (index, (p, resolved)) in bundle
        .config
        .parameters()
        .iter()
        .zip(&bundle.resolved_parameters)
        .enumerate()
    {
        for (name, role, value) in [
            ("baseline", AmountRole::Baseline, &p.baseline_amount),
            ("low", AmountRole::Low, &p.low),
            ("high", AmountRole::High, &p.high),
        ] {
            out.push(scalar(
                format!("parameter:{index}:{name}"),
                baseline,
                FactContext::ParameterAmount {
                    parameter_index: index,
                    role,
                },
                None,
                Some(value.clone()),
                resolved.unit.clone(),
                parameter_unit(resolved),
                "complete",
                false,
            ));
        }
    }
    for (index, b) in bundle.baseline_evaluation.outputs.iter().enumerate() {
        out.push(scalar(
            format!("baseline:method:{index}"),
            baseline,
            FactContext::BaselineOutput {
                method_index: index,
            },
            Some(b.method_ref.clone()),
            b.canonical_value.clone(),
            b.unit.clone(),
            method_unit(&b.method_ref, &b.unit),
            &b.coverage,
            false,
        ));
    }
    match &bundle.config {
        super::super::analysis_advanced_contracts::Config::SensitivityOat {
            parameters, ..
        } => {
            let m = bundle.baseline_evaluation.outputs.len();
            ensure!(
                bundle.oat_effects.len() == parameters.len().saturating_mul(m),
                "study_advanced_catalog_shape"
            );
            for (p_index, resolved) in bundle.resolved_parameters.iter().enumerate() {
                for (m_index, b) in bundle.baseline_evaluation.outputs.iter().enumerate() {
                    let effect = &bundle.oat_effects[p_index * m + m_index];
                    for (name, endpoint, evaluation, absolute, relative) in [
                        (
                            "low",
                            Endpoint::Low,
                            &bundle.evaluations[2 * p_index],
                            &effect.low_absolute_delta,
                            &effect.low_relative_delta,
                        ),
                        (
                            "high",
                            Endpoint::High,
                            &bundle.evaluations[2 * p_index + 1],
                            &effect.high_absolute_delta,
                            &effect.high_relative_delta,
                        ),
                    ] {
                        let prefix = format!("oat:parameter:{p_index}:{name}");
                        let method = Some(b.method_ref.clone());
                        out.push(scalar(
                            format!("{prefix}:output:method:{m_index}"),
                            baseline,
                            FactContext::OatOutput {
                                parameter_index: p_index,
                                method_index: m_index,
                                endpoint,
                                evaluation_id: evaluation.evaluation_id.clone(),
                            },
                            method.clone(),
                            evaluation.outputs[m_index].canonical_value.clone(),
                            b.unit.clone(),
                            method_unit(&b.method_ref, &b.unit),
                            &b.coverage,
                            false,
                        ));
                        out.push(scalar(
                            format!("{prefix}:absoluteDelta:method:{m_index}"),
                            baseline,
                            FactContext::OatAbsoluteDelta {
                                parameter_index: p_index,
                                method_index: m_index,
                                endpoint,
                                evaluation_id: evaluation.evaluation_id.clone(),
                            },
                            method.clone(),
                            absolute.clone(),
                            b.unit.clone(),
                            method_unit(&b.method_ref, &b.unit),
                            &b.coverage,
                            false,
                        ));
                        out.push(scalar(
                            format!("{prefix}:relativeDelta:method:{m_index}"),
                            baseline,
                            FactContext::OatRelativeDelta {
                                parameter_index: p_index,
                                method_index: m_index,
                                endpoint,
                                evaluation_id: evaluation.evaluation_id.clone(),
                            },
                            method,
                            relative.clone(),
                            "1".into(),
                            ReportUnitIdentity::Dimensionless { unit: "1".into() },
                            &b.coverage,
                            true,
                        ));
                    }
                    out.push(scalar(
                        format!("oat:parameter:{p_index}:secant:method:{m_index}"),
                        baseline,
                        FactContext::OatSecant {
                            parameter_index: p_index,
                            method_index: m_index,
                            low_evaluation_id: effect.low_evaluation_id.clone(),
                            high_evaluation_id: effect.high_evaluation_id.clone(),
                        },
                        Some(b.method_ref.clone()),
                        effect.secant.clone(),
                        format!("({})/({})", b.unit, resolved.unit),
                        ReportUnitIdentity::Secant {
                            numerator: MethodQuantity {
                                method_ref: b.method_ref.clone(),
                                unit: b.unit.clone(),
                            },
                            denominator: exchange(resolved),
                        },
                        &b.coverage,
                        false,
                    ));
                }
            }
        }
        super::super::analysis_advanced_contracts::Config::Uncertainty { .. } => {
            for (index, s) in bundle.statistics.iter().enumerate() {
                for (name, kind, value) in [
                    ("mean", Statistic::Mean, &s.mean),
                    (
                        "sampleStandardDeviation",
                        Statistic::SampleStandardDeviation,
                        &s.sample_standard_deviation,
                    ),
                    ("meanMcse", Statistic::MeanMcse, &s.mean_mcse),
                    ("minimum", Statistic::Minimum, &s.minimum),
                    ("maximum", Statistic::Maximum, &s.maximum),
                ] {
                    out.push(scalar(
                        format!("uncertainty:{name}:method:{index}"),
                        baseline,
                        FactContext::UncertaintyStatistic {
                            method_index: index,
                            statistic: kind,
                        },
                        Some(s.method_ref.clone()),
                        value.clone(),
                        s.unit.clone(),
                        method_unit(&s.method_ref, &s.unit),
                        &s.coverage,
                        false,
                    ));
                }
                for q in &s.quantiles {
                    out.push(scalar(
                        format!("uncertainty:quantile:{}:method:{index}", q.probability),
                        baseline,
                        FactContext::UncertaintyQuantile {
                            method_index: index,
                            probability: q.probability.clone(),
                        },
                        Some(s.method_ref.clone()),
                        q.value.clone(),
                        s.unit.clone(),
                        method_unit(&s.method_ref, &s.unit),
                        &s.coverage,
                        false,
                    ));
                }
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for fact in &out {
        ensure!(
            seen.insert(&fact.fact_id) && fact.fact_id.len() <= 128,
            "study_advanced_catalog_identity"
        );
        if let Some(value) = &fact.canonical_value {
            super::number(value)?;
        }
        ensure!(
            fact.baseline_run_id.to_string() == bundle.config.baseline_run_id(),
            "study_advanced_catalog_baseline"
        );
    }
    let expected = bundle
        .config
        .parameters()
        .len()
        .checked_mul(3)
        .and_then(|n| n.checked_add(bundle.baseline_evaluation.outputs.len()))
        .context("study_advanced_catalog_limit")?;
    let extras = match bundle.config {
        super::super::analysis_advanced_contracts::Config::SensitivityOat { .. } => {
            bundle.oat_effects.len().checked_mul(7)
        }
        super::super::analysis_advanced_contracts::Config::Uncertainty { .. } => {
            bundle.statistics.len().checked_mul(8)
        }
    }
    .context("study_advanced_catalog_limit")?;
    ensure!(
        out.len()
            == expected
                .checked_add(extras)
                .context("study_advanced_catalog_limit")?,
        "study_advanced_catalog_shape"
    );
    Ok(out)
}

// Exact serialized fixed-field overhead plus twice each actual metadata record bounds
// every scalar variant, including escaped units/support references and secant display.
pub(super) fn preflight_bytes(
    config: &super::super::analysis_advanced_contracts::Config,
    prepared: &super::super::contracts::PreparedInput,
) -> anyhow::Result<usize> {
    use super::super::contracts::DatasetRef;
    let empty = DatasetRef {
        id: String::new(),
        version: String::new(),
    };
    let scaffold = ReportFact {
        fact_id: "x".repeat(128),
        baseline_run_id: Uuid::nil(),
        context: FactContext::OatSecant {
            parameter_index: usize::MAX,
            method_index: usize::MAX,
            low_evaluation_id: "x".repeat(128),
            high_evaluation_id: "x".repeat(128),
        },
        method_ref: Some(empty.clone()),
        canonical_value: Some("1".repeat(64)),
        unit: "()/()".into(),
        unit_identity: ReportUnitIdentity::Secant {
            numerator: MethodQuantity {
                method_ref: empty.clone(),
                unit: String::new(),
            },
            denominator: ExchangeQuantity {
                parameter_index: usize::MAX,
                flow: empty,
                direction: "Output".into(),
                unit: String::new(),
                support: vec![],
            },
        },
        coverage: "complete".into(),
        reason_codes: vec![
            "zero_baseline_relative_undefined".into(),
            "no_characterized_terms".into(),
        ],
    };
    let fixed = serde_json::to_vec(&scaffold)?.len().saturating_add(1);
    let mut total = 0usize;
    let mut exchange_sizes = vec![];
    for (index, p) in config.parameters().iter().enumerate() {
        let parts = p.path.split('/').collect::<Vec<_>>();
        let e = prepared
            .model
            .instances
            .iter()
            .find(|i| i.id == parts[2])
            .context("study_advanced_catalog_parameter")?
            .exchanges
            .iter()
            .find(|e| e.id == parts[4])
            .context("study_advanced_catalog_parameter")?;
        let size = serde_json::to_vec(&ExchangeQuantity {
            parameter_index: index,
            flow: e.flow.clone(),
            direction: e.direction.clone(),
            unit: e.unit.clone(),
            support: e.unit_identity.clone(),
        })?
        .len()
        .saturating_mul(2);
        exchange_sizes.push(size);
        total = total.saturating_add(fixed.saturating_add(size).saturating_mul(3));
    }
    for m in &prepared.numeric.method_axes {
        let size = serde_json::to_vec(&MethodQuantity {
            method_ref: DatasetRef {
                id: m.id.clone(),
                version: m.version.clone(),
            },
            unit: m.unit.clone(),
        })?
        .len()
        .saturating_mul(2);
        let one = fixed.saturating_add(size);
        total = total.saturating_add(one);
        match config {
            super::super::analysis_advanced_contracts::Config::SensitivityOat { .. } => {
                for e in &exchange_sizes {
                    total = total
                        .saturating_add(one.saturating_mul(7))
                        .saturating_add(*e);
                }
            }
            super::super::analysis_advanced_contracts::Config::Uncertainty { .. } => {
                total = total.saturating_add(one.saturating_mul(8));
            }
        }
    }
    Ok(total)
}
