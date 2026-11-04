//! Source-ordered primary-only arithmetic; not an applicability or approval engine.
use super::super::{
    analysis::canonical_number,
    contracts::DatasetRef,
    method_assessment_contracts::{
        CandidateNormalisationWeighting, CategoryAssessment, NormalisationWeightingCategory,
    },
};
use super::module::Module;
use anyhow::{Context, ensure};

fn ratio(numerator: f64, denominator: f64) -> anyhow::Result<f64> {
    ensure!(
        numerator.is_finite() && denominator.is_finite() && denominator > 0.0,
        "study_method_normalisation_factor"
    );
    let result = numerator / denominator;
    ensure!(
        result.is_finite() && (result != 0.0 || numerator == 0.0),
        "study_method_normalisation_range"
    );
    Ok(result)
}
fn weighted(value: f64, fraction: f64) -> anyhow::Result<f64> {
    ensure!(
        fraction.is_finite() && fraction > 0.0 && fraction <= 1.0,
        "study_method_weight_fraction"
    );
    let result = value * fraction;
    ensure!(
        result.is_finite() && (result != 0.0 || value == 0.0),
        "study_method_weighted_range"
    );
    Ok(result)
}
fn sum(values: impl Iterator<Item = f64>) -> anyhow::Result<f64> {
    let mut total = 0.0_f64;
    let mut correction = 0.0_f64;
    for value in values {
        let next = total + value;
        correction += if total.abs() >= value.abs() {
            (total - next) + value
        } else {
            (value - next) + total
        };
        ensure!(
            next.is_finite() && correction.is_finite(),
            "study_method_aggregation_range"
        );
        total = next;
    }
    let result = total + correction;
    ensure!(result.is_finite(), "study_method_aggregation_range");
    Ok(result)
}
pub(super) fn unavailable(reason_codes: Vec<String>) -> CandidateNormalisationWeighting {
    CandidateNormalisationWeighting {status:"unavailable".into(),scope:"candidate_arithmetic_only_not_pef_qualification".into(),basis:"source_global_per_capita_reference_impacts".into(),weight_representation:"source_fraction_no_additional_division_by_100".into(),point_definition:"1 Pt = sum of characterised_i / source_NF_i times source_weight_fraction_i over exactly sixteen primary categories".into(),scaling_factor:"1".into(),categories:vec![],canonical_candidate_single_score:None,score_unit:"Pt".into(),reason_codes}
}
/// Production callers supply only source-verified frozen categories. The helper
/// is private: actor requests cannot submit arbitrary category totals.
pub(super) fn candidate(
    module: &Module,
    categories: &[CategoryAssessment],
    source_matches: bool,
) -> anyhow::Result<CandidateNormalisationWeighting> {
    let mut reasons = Vec::new();
    if !source_matches {
        reasons.push("frozen_method_source_not_verified".into());
    }
    for source in &module.normalisation {
        let found = categories.iter().find(|c| {
            c.method_ref.id == source.method_id
                && c.method_ref.version == source.method_version
                && c.role == "primary"
        });
        if !found.is_some_and(|c| {
            c.selected
                && c.source_compatible
                && c.coverage == "complete"
                && c.canonical_characterised_value.is_some()
                && c.unit == source.unit_label
                && c.reference_quantity_id == source.reference_quantity_id
        }) {
            reasons.push(format!(
                "primary_category_not_complete:{}",
                source.method_id
            ));
        }
    }
    if !reasons.is_empty() {
        return Ok(unavailable(reasons));
    }
    let mut result = unavailable(vec![]);
    let mut weighted_values = Vec::with_capacity(16);
    for source in &module.normalisation {
        let category = categories
            .iter()
            .find(|c| {
                c.method_ref.id == source.method_id
                    && c.method_ref.version == source.method_version
                    && c.role == "primary"
            })
            .context("study_method_primary_mapping")?;
        let characterised = category
            .canonical_characterised_value
            .as_ref()
            .context("study_method_primary_value")?;
        let value = characterised.parse::<f64>()?;
        ensure!(
            canonical_number(value)? == *characterised,
            "study_method_characterised_number"
        );
        let factor = source.normalisation_factor.parse::<f64>()?;
        let fraction = source.weight_fraction.parse::<f64>()?;
        let normalised = ratio(value, factor)?;
        let weight = weighted(normalised, fraction)?;
        weighted_values.push(weight);
        result.categories.push(NormalisationWeightingCategory {
            method_ref: DatasetRef {
                id: source.method_id.clone(),
                version: source.method_version.clone(),
            },
            reference_quantity_id: source.reference_quantity_id.clone(),
            characterised_unit: source.unit_label.clone(),
            canonical_characterised_value: characterised.clone(),
            source_normalisation_factor: source.normalisation_factor.clone(),
            source_weight_fraction: source.weight_fraction.clone(),
            source_unit_label: source.source_unit_label.clone(),
            source_short_description: source.source_short_description.clone(),
            canonical_normalised_value: canonical_number(normalised)?,
            normalised_unit: "1".into(),
            canonical_weighted_value: canonical_number(weight)?,
            weighted_unit: "Pt".into(),
        });
    }
    result.status = "candidate_arithmetic_available".into();
    result.canonical_candidate_single_score =
        Some(canonical_number(sum(weighted_values.into_iter())?)?);
    Ok(result)
}
