//! Service DTO checks and independent parent purpose checks; no service grants originate here.
use super::{
    super::{
        contracts::{Bounds, DatasetRef},
        method_assessment_contracts::{FactsBundle, MAX_SELECTED_FACTOR_RECORDS},
        method_runtime_contracts::{
            CONTEXT_SCHEMA, EVIDENCE_SCHEMA, EvidenceBody, JobContext, MAX_EVIDENCE_BYTES,
            MAX_FACTS_BYTES,
        },
    },
    module,
};
use anyhow::ensure;
use std::collections::BTreeSet;

pub fn validate_evidence_body(value: &EvidenceBody) -> anyhow::Result<()> {
    ensure!(
        value.schema_version == EVIDENCE_SCHEMA
            && (1..=20).any(|i| value.requirement_id == format!("PEF-{i:02}"))
            && super::bounded(&value.title, 256)
            && super::bounded(&value.statement, 8192)
            && !value.title.contains(['<', '>'])
            && !value.statement.contains(['<', '>'])
            && value.citations.len() <= 32
            && serde_json::to_vec(value)?.len() <= MAX_EVIDENCE_BYTES,
        "study_method_evidence_body"
    );
    let mut seen = BTreeSet::new();
    for citation in &value.citations {
        ensure!(
            super::bounded(&citation.identity, 256)
                && super::bounded(&citation.version, 128)
                && super::bounded(&citation.locator, 512)
                && !citation.identity.contains(['<', '>'])
                && !citation.version.contains(['<', '>'])
                && !citation.locator.contains(['<', '>'])
                && citation
                    .content_hash
                    .as_ref()
                    .is_none_or(|h| super::hash(h))
                && seen.insert(serde_json::to_string(citation)?),
            "study_method_evidence_citation"
        );
    }
    Ok(())
}

pub fn validate_context(context: &JobContext) -> anyhow::Result<()> {
    ensure!(
        context.schema_version == CONTEXT_SCHEMA
            && (1..=MAX_SELECTED_FACTOR_RECORDS)
                .contains(&context.budget.max_selected_factor_records)
            && context.budget.max_facts_bytes == MAX_FACTS_BYTES,
        "study_method_job_budget"
    );
    super::validate_input(&context.input)?;
    super::source::validate_metadata_binding(&context.input)
}

/// This rechecks the output's frozen identities, values, statuses and scalar catalog without
/// loading a method cache or re-preparing source outside the supervised native process.
#[allow(clippy::too_many_lines)]
pub fn validate_registered_output(
    bundle: &FactsBundle,
    context: &JobContext,
    prepared_bytes: &[u8],
    result_bytes: &[u8],
    bounds: &Bounds,
) -> anyhow::Result<()> {
    validate_context(context)?;
    let input = &context.input;
    let module = module::verify(module::PACKAGE)?;
    let verified = super::super::analysis::verify_run(
        input.baseline.clone(),
        prepared_bytes,
        result_bytes,
        bounds,
    )?;
    let p = verified.prepared();
    let result = verified.result();
    let source = DatasetRef {
        id: input.source_snapshot["originRef"]["id"]
            .as_str()
            .unwrap_or_default()
            .into(),
        version: input.source_snapshot["originRef"]["version"]
            .as_str()
            .unwrap_or_default()
            .into(),
    };
    let binding = super::binding(input, p, &input.source_artifact.artifact_ref, &source)?;
    let selected_count = p
        .numeric
        .method_axes
        .len()
        .checked_mul(p.numeric.inventory_axes.len())
        .ok_or_else(|| anyhow::anyhow!("study_method_selected_factor_limit"))?;
    ensure!(
        bundle.schema_version == super::FACTS_SCHEMA
            && bundle.profile_id == super::PROFILE
            && bundle.assessor_version == super::ASSESSOR
            && bundle.identity_kind == "private_job_metadata_not_an_authorization_receipt"
            && bundle.assessment_id == input.assessment.assessment_id
            && bundle.study_id == input.assessment.study_id
            && bundle.config_hash == input.assessment.config_hash
            && serde_json::to_value(&bundle.config)?
                == serde_json::to_value(&input.assessment.config)?
            && serde_json::to_value(&bundle.module_ref)?
                == serde_json::to_value(&module.reference)?
            && serde_json::to_value(&bundle.baseline)? == serde_json::to_value(binding)?
            && bundle.categories.len() == 25
            && bundle.requirements.len() == 20
            && selected_count <= context.budget.max_selected_factor_records
            && bundle.method_source_evidence.selected_factors.len() == selected_count
            && !bundle.pef_conformity_qualified
            && !bundle.scientific_ranking_qualified
            && !bundle.business_pilot_qualified
            && serde_json::to_vec(bundle)?.len() <= context.budget.max_facts_bytes,
        "study_method_output_binding"
    );
    for (category, method) in bundle.categories.iter().zip(&module.methods) {
        let position = p
            .numeric
            .method_axes
            .iter()
            .position(|a| a.id == method.id && a.version == method.version);
        let missing = p
            .coverage
            .iter()
            .filter(|g| g.method.id == method.id && g.method.version == method.version)
            .map(|g| g.inventory_index)
            .collect::<Vec<_>>();
        let coverage = if position.is_none() {
            "not_selected"
        } else if p.numeric.inventory_axes.is_empty()
            || missing.len() == p.numeric.inventory_axes.len()
        {
            "unknown"
        } else if missing.is_empty() {
            "complete"
        } else {
            "partial"
        };
        let value = if coverage == "unknown" {
            None
        } else {
            position
                .map(|i| super::super::analysis::canonical_number(result.h[i]))
                .transpose()?
        };
        ensure!(
            category.method_ref.id == method.id
                && category.method_ref.version == method.version
                && category.role == method.role
                && category.parent_method_id == method.parent_id
                && category.reference_quantity_id == method.reference_quantity_id
                && category.unit == method.unit_label
                && category.selected == position.is_some()
                && category.coverage == coverage
                && category.missing_inventory_indices == missing
                && category.canonical_characterised_value == value,
            "study_method_output_category"
        );
    }
    let source_matches = bundle
        .method_source_evidence
        .frozen_c_matches_verified_bundle;
    let nw = super::normalisation::candidate(&module, &bundle.categories, source_matches)?;
    ensure!(
        serde_json::to_value(&nw)?
            == serde_json::to_value(&bundle.candidate_normalisation_weighting)?
            && serde_json::to_value(super::requirements(
                &module,
                &input.assessment.config,
                p,
                source_matches,
                &bundle.categories,
                &nw
            ))? == serde_json::to_value(&bundle.requirements)?
            && serde_json::to_value(super::catalog::build(bundle, &module)?)?
                == serde_json::to_value(&bundle.fact_catalog)?,
        "study_method_output_catalog_or_requirements"
    );
    Ok(())
}
