//! Exact method-owned scalar selectors; professional states are never numerical scores.
use super::{super::method_assessment_contracts::FactsBundle, module::Module};
use crate::study::{
    contracts::DatasetRef,
    method_runtime_contracts::{FactContext, ReportFact, UnitIdentity},
};
use anyhow::{Context, ensure};

// Keep the three explicit selector families together for source/quantity review.
#[allow(clippy::too_many_lines)]
pub(super) fn build(bundle: &FactsBundle, module: &Module) -> anyhow::Result<Vec<ReportFact>> {
    let mut facts = Vec::with_capacity(58);
    for category in &bundle.categories {
        let method = category.method_ref.clone();
        facts.push(ReportFact {
            fact_id: format!("category:{}:characterised", method.id),
            assessment_id: bundle.assessment_id,
            baseline_run_id: bundle.baseline.run_id,
            context: FactContext::CharacterisedCategory {
                method_ref: method.clone(),
                category_role: category.role.clone(),
            },
            method_ref: Some(method.clone()),
            canonical_value: category.canonical_characterised_value.clone(),
            unit: category.unit.clone(),
            unit_identity: UnitIdentity::Characterised {
                method_ref: method,
                reference_quantity_id: category.reference_quantity_id.clone(),
                unit: category.unit.clone(),
            },
            coverage: if category.coverage == "not_selected" {
                "unknown".into()
            } else {
                category.coverage.clone()
            },
            reason_codes: category.reason_codes.clone(),
        });
    }
    let nw = &bundle.candidate_normalisation_weighting;
    let available = nw.status == "candidate_arithmetic_available";
    ensure!(
        (available && nw.categories.len() == 16) || (!available && nw.categories.is_empty()),
        "study_method_catalog_nw_shape"
    );
    for source in &module.normalisation {
        let method = DatasetRef {
            id: source.method_id.clone(),
            version: source.method_version.clone(),
        };
        let value = nw.categories.iter().find(|c| c.method_ref == method);
        ensure!(
            value.is_some() == available,
            "study_method_catalog_nw_mapping"
        );
        for normalised in [true, false] {
            let unit = if normalised { "1" } else { "Pt" };
            let canonical_value = value.map(|v| {
                if normalised {
                    v.canonical_normalised_value.clone()
                } else {
                    v.canonical_weighted_value.clone()
                }
            });
            let context = if normalised {
                FactContext::NormalisedCategory {
                    method_ref: method.clone(),
                }
            } else {
                FactContext::WeightedCategory {
                    method_ref: method.clone(),
                }
            };
            let unit_identity = if normalised {
                UnitIdentity::Normalised {
                    method_ref: method.clone(),
                    reference_quantity_id: source.reference_quantity_id.clone(),
                    unit: unit.into(),
                    source_normalisation_factor: source.normalisation_factor.clone(),
                    module_artifact_sha256: module.reference.artifact_sha256.clone(),
                }
            } else {
                UnitIdentity::Weighted {
                    method_ref: method.clone(),
                    reference_quantity_id: source.reference_quantity_id.clone(),
                    unit: unit.into(),
                    source_normalisation_factor: source.normalisation_factor.clone(),
                    source_weight_fraction: source.weight_fraction.clone(),
                    point_definition: nw.point_definition.clone(),
                    scaling_factor: nw.scaling_factor.clone(),
                    module_artifact_sha256: module.reference.artifact_sha256.clone(),
                }
            };
            facts.push(ReportFact {
                fact_id: format!(
                    "{}:{}",
                    if normalised { "normalised" } else { "weighted" },
                    method.id
                ),
                assessment_id: bundle.assessment_id,
                baseline_run_id: bundle.baseline.run_id,
                context,
                method_ref: Some(method.clone()),
                canonical_value,
                unit: unit.into(),
                unit_identity,
                coverage: if available {
                    "complete".into()
                } else {
                    "unknown".into()
                },
                reason_codes: nw.reason_codes.clone(),
            });
        }
    }
    let methods = module
        .normalisation
        .iter()
        .map(|v| DatasetRef {
            id: v.method_id.clone(),
            version: v.method_version.clone(),
        })
        .collect::<Vec<_>>();
    facts.push(ReportFact {
        fact_id: "candidate:singleScore".into(),
        assessment_id: bundle.assessment_id,
        baseline_run_id: bundle.baseline.run_id,
        context: FactContext::CandidateSingleScore {
            primary_methods: methods.clone(),
        },
        method_ref: None,
        canonical_value: nw.canonical_candidate_single_score.clone(),
        unit: nw.score_unit.clone(),
        unit_identity: UnitIdentity::CandidateScore {
            primary_methods: methods,
            unit: nw.score_unit.clone(),
            point_definition: nw.point_definition.clone(),
            scaling_factor: nw.scaling_factor.clone(),
            module_artifact_sha256: module.reference.artifact_sha256.clone(),
        },
        coverage: if available {
            "complete".into()
        } else {
            "unknown".into()
        },
        reason_codes: nw.reason_codes.clone(),
    });
    ensure!(facts.len() == 58, "study_method_catalog_inventory");
    for fact in &facts {
        ensure!(
            fact.fact_id.len() <= 128
                && fact.reason_codes.len() <= 32
                && (fact.coverage != "unknown" || fact.canonical_value.is_none()),
            "study_method_catalog_shape"
        );
        if let Some(value) = &fact.canonical_value {
            let number: f64 = value.parse().context("study_method_catalog_number")?;
            ensure!(
                super::super::analysis::canonical_number(number)? == *value,
                "study_method_catalog_number"
            );
        }
    }
    Ok(facts)
}
