//! Read-only source-bound assessment. It never authorizes, rewrites or certifies a Study.
mod module;
mod normalisation;
mod source;
use super::{
    analysis, analysis_contracts as old,
    contracts::{Bounds, DatasetRef, PreparedInput, StudyResult},
    method_assessment_contracts::{
        ASSESSOR, AssessmentRecord, BaselineBinding, CANDIDATE_INPUT_SCHEMA, CONFIG_SCHEMA,
        CandidateInput, CandidateNormalisationWeighting, CategoryAssessment,
        CategoryRuleDeclaration, Config, FACTS_SCHEMA, FactsBundle, FrozenEvidenceSummary,
        INPUT_SCHEMA, MAX_EVIDENCE_REFS, MAX_SELECTED_FACTOR_RECORDS, MachineAssessment,
        MachineCheck, MachineStatus, MethodSourceEvidence, ModelRef, ModuleRef, PROFILE,
        ProfessionalAssessment, ProfessionalStatus, RequirementAssessment, ServiceInput,
        StudyPhase, Validity,
    },
    runtime_contracts::PrivateArtifact,
    semantic_hash,
};
use anyhow::{Context, ensure};
use serde_json::Value;
pub use source::TrustedMethods;
use std::collections::BTreeSet;
use uuid::Uuid;

#[must_use]
pub fn pinned_module_ref() -> ModuleRef {
    module::reference()
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
pub fn validate_config(config: &Config) -> anyhow::Result<()> {
    ensure!(
        config.schema_version == CONFIG_SCHEMA
            && serde_json::to_value(&config.module_ref)?
                == serde_json::to_value(module::reference())?,
        "study_method_config_module"
    );
    let id = Uuid::parse_str(&config.baseline_run_id)?;
    ensure!(
        !id.is_nil() && id.to_string() == config.baseline_run_id,
        "study_method_baseline_id"
    );
    ensure!(
        serde_json::to_vec(config)?.len() <= 262_144
            && config.evidence_refs.len() <= MAX_EVIDENCE_REFS,
        "study_method_config_limit"
    );
    if let CategoryRuleDeclaration::CommissionDeveloped {
        reference_id,
        version,
        evidence_id,
    } = &config.declarations.category_rule
    {
        ensure!(
            bounded(reference_id, 256) && bounded(version, 128) && !evidence_id.is_nil(),
            "study_method_category_rule_declaration"
        );
    }
    let mut seen = BTreeSet::new();
    for evidence in &config.evidence_refs {
        let r = &evidence.artifact_ref;
        ensure!(
            (1..=20).any(|i| evidence.requirement_id == format!("PEF-{i:02}"))
                && seen.insert((evidence.requirement_id.clone(), r.id))
                && !r.id.is_nil()
                && r.r#type == "methodEvidence"
                && r.schema_version == "lca.study-method-evidence.v1"
                && r.digest_scope == "artifact_bytes_sha256"
                && r.media_type == "application/json"
                && r.byte_size > 0
                && r.byte_size <= 4 * 1024 * 1024
                && hash(&r.content_hash),
            "study_method_evidence_reference"
        );
    }
    Ok(())
}
pub fn config_hash(config: &Config) -> anyhow::Result<String> {
    validate_config(config)?;
    semantic_hash("study.method-assessment-config.v1", config)
}
fn legacy(input: &ServiceInput) -> anyhow::Result<old::ServiceInput> {
    let cfg = old::AnalysisConfig {
        schema_version: old::CONFIG_SCHEMA.into(),
        kind: old::AnalysisKind::Contribution,
        run_ids: vec![Uuid::parse_str(&input.assessment.config.baseline_run_id)?],
    };
    Ok(old::ServiceInput {
        schema_version: old::INPUT_SCHEMA.into(),
        analysis: old::AnalysisRecord {
            analysis_id: input.assessment.assessment_id,
            study_id: input.assessment.study_id,
            profile_id: old::PROFILE.into(),
            config_hash: analysis::config_hash(&cfg)?,
            config: cfg,
            execution: old::Execution {
                job_id: input.assessment.execution.job_id,
                status: "running".into(),
            },
        },
        runs: vec![input.baseline.clone()],
    })
}
fn private_cap(cap: &PrivateArtifact, study: Uuid, kind: &str) -> anyhow::Result<()> {
    let r = &cap.artifact_ref;
    let prefix = format!("study/{study}/{}/", r.id);
    let suffix = cap
        .storage_object_path
        .strip_prefix(&prefix)
        .context("study_method_private_path")?;
    ensure!(
        !study.is_nil()
            && !r.id.is_nil()
            && cap.study_id == study
            && cap.storage_bucket == "study-artifacts"
            && r.r#type == kind
            && r.byte_size > 0
            && r.byte_size <= 4 * 1024 * 1024
            && r.digest_scope == "artifact_bytes_sha256"
            && r.media_type == "application/json"
            && hash(&r.content_hash)
            && std::path::Path::new(suffix)
                .extension()
                .is_some_and(|extension| extension == "json")
            && suffix.len() <= 256
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'),
        "study_method_private_capability"
    );
    Ok(())
}
fn validate_common(input: &ServiceInput, private_source_capability: bool) -> anyhow::Result<()> {
    ensure!(
        input.schema_version == INPUT_SCHEMA
            && input.assessment.profile_id == PROFILE
            && input.assessment.config_hash == config_hash(&input.assessment.config)?
            && !input.assessment.assessment_id.is_nil()
            && !input.assessment.study_id.is_nil(),
        "study_method_input_binding"
    );
    analysis::validate_input(&legacy(input)?)?;
    if private_source_capability {
        private_cap(
            &input.source_artifact,
            input.assessment.study_id,
            "modelSource",
        )?;
    } else {
        let r = &input.source_artifact.artifact_ref;
        ensure!(
            !r.id.is_nil()
                && r.r#type == "modelSource"
                && r.schema_version == super::runtime_contracts::SNAPSHOT_SCHEMA
                && r.media_type == "application/json"
                && r.digest_scope == "artifact_bytes_sha256"
                && r.byte_size > 0
                && r.byte_size <= 4 * 1024 * 1024
                && hash(&r.content_hash),
            "study_method_offline_source_reference"
        );
    }

    private_cap(
        &input.baseline.prepared_artifact,
        input.assessment.study_id,
        "modelInput",
    )?;
    private_cap(
        &input.baseline.result_artifact,
        input.assessment.study_id,
        "runResult",
    )?;
    Ok(())
}
pub fn validate_input(input: &ServiceInput) -> anyhow::Result<()> {
    ensure!(
        input.assessment.execution.status == "running"
            && !input.assessment.execution.job_id.is_nil(),
        "study_method_private_job_metadata"
    );
    validate_common(input, true)
}
fn candidate_context(input: &CandidateInput) -> anyhow::Result<ServiceInput> {
    ensure!(
        input.schema_version == CANDIDATE_INPUT_SCHEMA && !input.candidate_id.is_nil(),
        "study_method_offline_candidate_identity"
    );
    let context = ServiceInput {
        schema_version: INPUT_SCHEMA.into(),
        assessment: AssessmentRecord {
            assessment_id: input.candidate_id,
            study_id: input.study_id,
            profile_id: PROFILE.into(),
            config: input.config.clone(),
            config_hash: input.config_hash.clone(),
            execution: old::Execution {
                job_id: Uuid::nil(),
                status: "offline_candidate_not_registered".into(),
            },
        },
        baseline: input.baseline.clone(),
        source_snapshot: input.source_snapshot.clone(),
        // Offline bytes have an exact reference but no newly issued source
        // capability/locator. These empty internal locator fields are never used.
        source_artifact: PrivateArtifact {
            study_id: input.study_id,
            artifact_ref: input.source_artifact_ref.clone(),
            storage_bucket: String::new(),
            storage_object_path: String::new(),
        },
    };
    validate_common(&context, false)?;
    Ok(context)
}
pub fn validate_candidate(input: &CandidateInput) -> anyhow::Result<()> {
    candidate_context(input)?;
    Ok(())
}

fn id(value: &Value, key: &str) -> anyhow::Result<Uuid> {
    Ok(Uuid::parse_str(
        value[key]
            .as_str()
            .context("study_method_baseline_identity")?,
    )?)
}
fn binding(
    input: &ServiceInput,
    p: &PreparedInput,
    proof: &source::SourceProof,
) -> anyhow::Result<BaselineBinding> {
    let model: ModelRef = serde_json::from_value(input.baseline.run["effectiveModelRef"].clone())?;
    let validity: Validity = serde_json::from_value(input.baseline.run["validity"].clone())?;
    ensure!(
        bounded(&validity.status, 64)
            && validity.reason_codes.len() <= 32
            && validity.reason_codes.iter().all(|r| bounded(r, 128))
            && chrono::DateTime::parse_from_rfc3339(&validity.checked_at).is_ok(),
        "study_method_validity_annotation"
    );
    Ok(BaselineBinding {
        run_id: id(&input.baseline.run, "runId")?,
        study_revision_id: id(&input.baseline.revision, "revisionId")?,
        scenario_revision_id: id(&input.baseline.scenario, "scenarioRevisionId")?,
        model_preparation_id: id(&input.baseline.preparation, "preparationId")?,
        input_hash: p
            .input_hash
            .clone()
            .context("study_method_input_identity")?,
        effective_model_ref: model,
        prepared_artifact_ref: input.baseline.prepared_artifact.artifact_ref.clone(),
        result_ref: input.baseline.result_artifact.artifact_ref.clone(),
        source_snapshot_ref: proof.source_ref.clone(),
        source_origin: proof.source_origin.clone(),
        calculation_contract_ref: p.calculation_contract_ref.clone(),
        method_source_hash: p.method_source_hash.clone(),
        validity,
    })
}
fn categories(
    module: &module::Module,
    p: &PreparedInput,
    r: &StudyResult,
    methods: &TrustedMethods,
    source_matches: bool,
) -> anyhow::Result<Vec<CategoryAssessment>> {
    module
        .methods
        .iter()
        .map(|m| {
            let position = p
                .numeric
                .method_axes
                .iter()
                .position(|axis| axis.id == m.id && axis.version == m.version);
            let mut reasons = Vec::new();
            let gaps = position.map_or_else(Vec::new, |_| {
                p.coverage
                    .iter()
                    .filter(|g| g.method.id == m.id && g.method.version == m.version)
                    .map(|g| g.inventory_index)
                    .collect::<Vec<_>>()
            });
            let compatible = position.is_some_and(|i| {
                source_matches
                    && methods.method_compatible(&m.id, &m.version, &p.numeric.method_axes[i].unit)
                    && p.numeric.method_axes[i].unit == m.unit_label
            });
            let coverage = if position.is_none() {
                reasons.push("required_exact_method_not_selected".into());
                "not_selected"
            } else if p.numeric.inventory_axes.is_empty() {
                reasons.push("empty_inventory_not_completeness_evidence".into());
                "unknown"
            } else if gaps.is_empty() {
                "complete"
            } else if gaps.len() == p.numeric.inventory_axes.len() {
                reasons.push("all_factors_unknown_not_zero".into());
                "unknown"
            } else {
                reasons.push("unmatched_factors_remain_unknown".into());
                "partial"
            };
            if position.is_some() && !compatible {
                reasons.push("method_source_or_unit_not_verified".into());
            }
            let value = if coverage == "unknown" {
                None
            } else {
                position
                    .map(|i| analysis::canonical_number(r.h[i]))
                    .transpose()?
            };
            Ok(CategoryAssessment {
                method_ref: DatasetRef {
                    id: m.id.clone(),
                    version: m.version.clone(),
                },
                name: m.name.clone(),
                role: m.role.clone(),
                parent_method_id: m.parent_id.clone(),
                reference_quantity_id: m.reference_quantity_id.clone(),
                unit: m.unit_label.clone(),
                selected: position.is_some(),
                source_compatible: compatible,
                coverage: coverage.into(),
                canonical_characterised_value: value,
                missing_inventory_indices: gaps,
                normalisation_weighting_included: m.role == "primary",
                reason_codes: reasons,
            })
        })
        .collect()
}
fn check(id: &str, status: MachineStatus, reason: &str) -> MachineCheck {
    MachineCheck {
        predicate_id: id.into(),
        status,
        reason_codes: if reason.is_empty() {
            vec![]
        } else {
            vec![reason.into()]
        },
    }
}
#[allow(clippy::too_many_lines)]
fn requirements(
    module: &module::Module,
    config: &Config,
    p: &PreparedInput,
    source_matches: bool,
    categories: &[CategoryAssessment],
    nw: &CandidateNormalisationWeighting,
) -> Vec<RequirementAssessment> {
    use MachineStatus::{NotAssessed, NotVerified, Unsupported, Verified, Violated};
    let selected_primaries = categories
        .iter()
        .filter(|c| c.role == "primary" && c.selected)
        .count();
    let product = p
        .model
        .instances
        .iter()
        .find(|i| i.id == p.model.reference.instance_id)
        .and_then(|i| {
            i.exchanges
                .iter()
                .find(|e| e.id == p.model.reference.exchange_id)
        })
        .is_some_and(|e| e.kind == "Product flow" && e.direction == "Output" && e.unit == "kg");
    let no_cutoffs = p.model.cutoffs.is_empty();
    let with_pefcr = matches!(
        config.declarations.category_rule,
        CategoryRuleDeclaration::CommissionDeveloped { .. }
    );
    module.requirements.iter().map(|requirement| {
        let (status,checks)=match requirement.id.as_str() {
            "PEF-01" => (NotVerified,vec![check("frozen_goal_definition_binding",Verified,""),check("goal_and_category_rule_fitness",NotVerified,"structured_declaration_is_not_applicability_proof")]),
            "PEF-02" => (if product {NotVerified}else{Unsupported},vec![check("frozen_functional_unit_reference_binding",Verified,""),check("supported_declared_kg_output_product",if product {Verified}else{Unsupported},if product {""}else{"profile_supports_declared_kg_product_only_not_a_universal_pef_rule"}),check("function_performance_cement_identity",NotVerified,"mass_and_caller_product_role_do_not_prove_function")]),
            "PEF-03" => (NotVerified,vec![check("frozen_graph_and_cutoffs_bound",Verified,""),check("life_cycle_stage_applicability_and_completeness",NotVerified,"no_freeform_boundary_inference")]),
            "PEF-04" => (NotVerified,vec![check("frozen_inventory_and_unknown_coverage_preserved",Verified,""),check("scientific_inventory_completeness",NotVerified,"empty_or_zero_fields_do_not_prove_completeness")]),
            "PEF-05" => {
                let s=if no_cutoffs {NotVerified}else if config.declarations.study_phase==StudyPhase::Screening {Violated}else{Unsupported};
                (s,vec![check("foreground_cutoff_observation",if no_cutoffs {Verified}else{s},if no_cutoffs {"no_declared_foreground_cutoffs_background_cutoffs_unknown"}else if s==Violated {"source_screening_rule_allows_no_cutoffs"}else{"ordinary_significance_and_data_gap_allowances_not_implemented_or_waived"})])
            },
            "PEF-06" => (NotVerified,vec![check("frozen_process_source_identities",Verified,""),check("primary_data_dnm_and_dqr_quality",NotVerified,"no_self_assigned_scores_or_presence_based_quality")]),
            "PEF-07" => (if with_pefcr {NotAssessed}else{NotVerified},vec![check("conditional_without_commission_pefcr_branch",NotVerified,"actual_search_availability_non_ef_use_and_exception_conditions_unverified")]),
            "PEF-08" => (if with_pefcr {Unsupported}else{NotAssessed},vec![check("conditional_commission_pefcr_branch",if with_pefcr {Unsupported}else{NotAssessed},"common_module_is_not_an_applicable_cement_pefcr_or_verifier_validation")]),
            "PEF-09" => (NotVerified,vec![check("exact_original_corrigendum_interim_factor_source_pins",Verified,""),check("conditional_interim_currentness",NotVerified,"2028_is_not_fixed_expiry_no_latest_database_or_guidance_proof")]),
            "PEF-10" => (NotVerified,vec![check("frozen_allocation_numeric_structure",Verified,""),check("allocation_hierarchy_causal_appropriateness",NotVerified,"arithmetic_closure_is_not_allocation_judgment")]),
            "PEF-11" => (NotVerified,vec![check("frozen_upstream_model_observation",Verified,""),check("energy_transport_capital_and_other_suitability",NotVerified,"source_identity_is_not_geographical_or_scenario_suitability")]),
            "PEF-12" => (Unsupported,vec![check("circularity_and_lifetime_applicability",Unsupported,"cff_parameters_substitution_and_lifetime_not_assessed")]),
            "PEF-13" => (NotVerified,vec![check("adopted_module_climate_role_definitions",Verified,""),check("carbon_balance_removals_and_offsets",NotVerified,"factor_match_does_not_prove_carbon_stock_flow_boundaries")]),
            "PEF-14" => (if selected_primaries==16 {Verified}else{Violated},vec![check("exact_sixteen_primary_category_selection",if selected_primaries==16 {Verified}else{Violated},if selected_primaries==16 {""}else{"partial_selected_methods_cannot_satisfy_full_pef_selection"}),check("decompositions_excluded_from_normalisation_total",Verified,"")]),
            "PEF-15" => (if source_matches {NotVerified}else{Violated},vec![check("actual_verified_source_c_and_coverage_comparison",if source_matches {Verified}else{Violated},if source_matches {""}else{"frozen_source_not_reproduced_by_actual_pinned_bundle"}),check("geography_compartment_and_unit_representativeness",NotVerified,"cache_equality_blank_location_and_known_ids_are_not_suitability")]),
            "PEF-16" => (if nw.status=="candidate_arithmetic_available" {Verified}else{NotVerified},vec![check("source_nf_primary_only_division",if nw.status=="candidate_arithmetic_available" {Verified}else{NotVerified},if nw.status=="candidate_arithmetic_available" {""}else{"complete_source_compatible_sixteen_values_unavailable"})]),
            "PEF-17" => (if nw.status=="candidate_arithmetic_available" {Verified}else{NotVerified},vec![check("source_fraction_weight_once_and_explicit_point_scale",if nw.status=="candidate_arithmetic_available" {Verified}else{NotVerified},if nw.status=="candidate_arithmetic_available" {"candidate_arithmetic_not_qualification"}else{"no_arbitrary_or_partial_category_total"})]),
            "PEF-18" => (NotAssessed,vec![check("professional_robustness_and_prescribed_hotspot_procedures",NotAssessed,"no_arbitrary_top_n_surrogate")]),
            "PEF-19" => (NotVerified,vec![check("report_independent_verification_and_communication",NotVerified,"source_hash_ci_ai_review_and_module_review_are_not_study_approval")]),
            _ => (NotVerified,vec![check("independently_pinned_module_and_factor_identity",Verified,""),check("source_rights_and_redistribution",NotVerified,"derived_facts_and_reports_license_do_not_grant_factor_or_background_rights")]),
        };
        let refs=config.evidence_refs.iter().filter(|r|r.requirement_id==requirement.id).cloned().collect::<Vec<_>>();
        let professional=if requirement.check_kind=="machine" {ProfessionalStatus::NotRequiredForSourceMachinePredicate}else if refs.is_empty(){ProfessionalStatus::Pending}else{ProfessionalStatus::ReferencesRecordedUnverified};
        RequirementAssessment{requirement_id:requirement.id.clone(),source_applicability:requirement.applicability.clone(),source_check_kind:requirement.check_kind.clone(),source_citations:requirement.sources.clone(),declaration_state:if requirement.id=="PEF-07" || requirement.id=="PEF-08" {if with_pefcr {"declared_with_commission_pefcr_applicability_unverified"}else{"declared_none_established_applicability_unverified"}}else{"scope_declaration_not_professional_proof"}.into(),machine:MachineAssessment{status,scope:"only_explicitly_named_source_bound_predicates_not_full_method_conformance".into(),checks},professional:ProfessionalAssessment{status:professional,evidence_refs:refs,reason_codes:if requirement.check_kind=="machine" {vec![]}else{vec!["professional_judgment_not_verified_by_software_or_reference_presence".into()]}}}
    }).collect()
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn assess_common(
    input: &ServiceInput,
    prepared_bytes: &[u8],
    result_bytes: &[u8],
    snapshot_bytes: &[u8],
    methods: &TrustedMethods,
    bounds: &Bounds,
    identity_kind: &str,
) -> anyhow::Result<FactsBundle> {
    bounds.validate()?;
    let module = module::verify(module::PACKAGE)?;
    let verified =
        analysis::verify_run(input.baseline.clone(), prepared_bytes, result_bytes, bounds)?;
    let p = verified.prepared();
    ensure!(
        p.numeric
            .method_axes
            .len()
            .saturating_mul(p.numeric.inventory_axes.len())
            <= MAX_SELECTED_FACTOR_RECORDS,
        "study_method_selected_factor_limit"
    );
    let r = verified.result();
    let proof = source::verify_snapshot(input, snapshot_bytes, p, r, methods, bounds)?;
    let source_matches =
        proof.method_source_matches && proof.recompiled_matches && methods.is_pinned_release();
    let mut categories = categories(&module, p, r, methods, source_matches)?;
    let nw = normalisation::candidate(&module, &categories, source_matches)?;
    for category in &mut categories {
        category.normalisation_weighting_included =
            nw.status == "candidate_arithmetic_available" && category.role == "primary";
    }
    let requirements = requirements(
        &module,
        &input.assessment.config,
        p,
        source_matches,
        &categories,
        &nw,
    );
    let selected = methods.selected_factors(p)?;
    let mut limitations=vec!["Initial method assessment is source/byte consistency and explicitly bounded predicates, not PEF conformity, scientific, business, ranking, EPD or Commission approval.".into(),"The user has no complete real positive industry case; synthetic arithmetic or conformance fixtures cannot replace real-study/professional acceptance.".into(),"P1 missing factors remain gaps; separately reviewed non-applicability is required for ordinary sparse sixteen-category numerical adoption.".into(),"Structured declarations and provided evidence references remain unverified; no freeform goal/boundary inference or file-presence approval.".into(),"Current interim guidance applicability cannot be established from frozen artifacts or a fixed 2028 date.".into()];
    if !p.model.cutoffs.is_empty() {
        limitations.push("Existing declared upstream cutoffs are retained; ordinary significance/data-gap exceptions are not implemented or silently waived.".into());
    }
    let definition = &p
        .study_binding
        .as_ref()
        .context("study_method_definition")?
        .definition;
    let facts = FactsBundle {
        schema_version: FACTS_SCHEMA.into(),
        profile_id: PROFILE.into(),
        assessor_version: ASSESSOR.into(),
        identity_kind: identity_kind.into(),
        study_id: input.assessment.study_id,
        assessment_id: input.assessment.assessment_id,
        config: input.assessment.config.clone(),
        config_hash: input.assessment.config_hash.clone(),
        module_ref: module.reference.clone(),
        module_source_status: "candidate_source_module_exact_main_adoption_not_study_approval".into(),
        baseline: binding(input,p,&proof)?,
        frozen_evidence: FrozenEvidenceSummary {
            functional_unit: definition.functional_unit.clone(),
            declared_system_boundary: definition.system_boundary.clone(),
            process_instances: p.model.instances.len(),
            source_document_count: proof.source.documents.len(),
            method_axes: p.numeric.method_axes.clone(),
            exchanges: p.model.instances.iter().map(|i|i.exchanges.len()).sum(),
            inventory_axes: p.numeric.inventory_axes.len(),
            declared_cutoffs: p.model.cutoffs.len(),
            coverage_gaps: p.coverage.clone(),
            quality_findings: p.model.quality_findings.clone(),
        },
        method_source_evidence: MethodSourceEvidence {
            static_source_snapshot: methods.snapshot()?,
            normative_version: module.normative_version.clone(),
            factor_package_version: module.factor_package_version.clone(),
            interim_guidance_snapshot: module.interim_guidance.clone(),
            source_catalog_sha256: methods.source_catalog_sha256().into(),
            nonregional_policy: crate::lcia_nonregional::POLICY.into(),
            nonregional_defaults_sha256: methods.defaults_sha256().into(),
            official_factor_package_sha256: module.package_source_sha256.clone(),
            normalisation_weighting_source_sha256: module.normalisation_source_sha256.clone(),
            frozen_c_matches_verified_bundle: source_matches,
            geographical_suitability: "not_verified".into(),
            source_bridges: methods.bridges(&module)?,
            selected_factors: selected,
            limitations: vec![
                "Original raw source hashes stay distinct; the PM bridge is reviewed structural equivalence, not source substitution or rights approval.".into(),
                "Unique-factor raw ordinal/location/compartment context is not retained by the existing cache; unavailable metadata is not fabricated.".into(),
                "Source defaults/equivalence never establish study geographic or technological suitability.".into(),
            ],
        },
        categories,
        requirements,
        candidate_normalisation_weighting: nw,
        outcome: "not_qualified_professional_and_actual_study_evidence_pending".into(),
        limitations,
        pef_conformity_qualified: false,
        scientific_ranking_qualified: false,
        business_pilot_qualified: false,
    };
    ensure!(
        serde_json::to_vec(&facts)?.len() <= bounds.max_artifact_bytes,
        "study_method_facts_byte_limit"
    );
    Ok(facts)
}

pub fn assess(
    input: &ServiceInput,
    prepared: &[u8],
    result: &[u8],
    snapshot: &[u8],
    methods: &TrustedMethods,
    bounds: &Bounds,
) -> anyhow::Result<FactsBundle> {
    validate_input(input)?;
    assess_common(
        input,
        prepared,
        result,
        snapshot,
        methods,
        bounds,
        "private_job_metadata_not_an_authorization_receipt",
    )
}
pub fn assess_candidate(
    input: &CandidateInput,
    prepared: &[u8],
    result: &[u8],
    snapshot: &[u8],
    methods: &TrustedMethods,
    bounds: &Bounds,
) -> anyhow::Result<FactsBundle> {
    let context = candidate_context(input)?;
    assess_common(
        &context,
        prepared,
        result,
        snapshot,
        methods,
        bounds,
        "offline_candidate_not_registered_or_authorized",
    )
}

pub fn verify_output(expected: &FactsBundle, bytes: &[u8], max_bytes: usize) -> anyhow::Result<()> {
    ensure!(bytes.len() <= max_bytes, "study_method_facts_byte_limit");
    let actual: FactsBundle = super::json::from_slice(bytes)?;
    ensure!(
        !actual.pef_conformity_qualified
            && !actual.scientific_ranking_qualified
            && !actual.business_pilot_qualified
            && serde_json::to_value(actual)? == serde_json::to_value(expected)?,
        "study_method_facts_binding"
    );
    Ok(())
}
#[cfg(test)]
mod tests;
