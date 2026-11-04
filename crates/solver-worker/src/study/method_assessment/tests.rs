use super::*;
use crate::study::method_assessment_contracts::{
    AssessmentRecord, Declarations, ProductRole, UnitBasis,
};
use serde_json::json;
fn config() -> Config {
    Config {
        schema_version: CONFIG_SCHEMA.into(),
        baseline_run_id: "10000000-0000-4000-8000-000000000010".into(),
        module_ref: pinned_module_ref(),
        declarations: Declarations {
            product_role: ProductRole::FinishedCementIntermediate,
            study_phase: StudyPhase::FinalStudy,
            reference_unit_basis: UnitBasis::DeclaredMass,
            category_rule: CategoryRuleDeclaration::NoneEstablished,
        },
        evidence_refs: vec![],
    }
}
#[test]
fn exact_main_module_adoption_preserves_all_twenty_requirements_and_category_roles() {
    let m = module::verify(module::PACKAGE).unwrap();
    assert_eq!(m.requirements.len(), 20);
    assert_eq!(m.methods.iter().filter(|m| m.role == "primary").count(), 16);
    assert_eq!(
        m.methods.iter().filter(|m| m.role == "subcategory").count(),
        9
    );
    assert_eq!(m.normalisation.len(), 16);
    let ecotox = m
        .normalisation
        .iter()
        .find(|m| m.method_id == "05316e7a-b254-4bea-9cf0-6bf33eb5c630")
        .unwrap();
    assert_eq!(ecotox.source_unit_label, "CTUitaloe");
    assert_eq!(ecotox.unit_label, "CTUe");
    let mut replaced = module::PACKAGE.to_vec();
    replaced[0] = b' ';
    assert!(module::verify(&replaced).is_err());
}
#[test]
fn closed_config_rejects_caller_approval_and_substituted_module_or_run() {
    let c = config();
    validate_config(&c).unwrap();
    let mut value = serde_json::to_value(&c).unwrap();
    value["scientificApproval"] = json!(true);
    assert!(serde_json::from_value::<Config>(value).is_err());
    let mut c = c;
    c.module_ref.source_commit = "0".repeat(40);
    assert!(validate_config(&c).is_err());
    let mut c = config();
    c.baseline_run_id = c.baseline_run_id.replace('-', "");
    assert!(validate_config(&c).is_err());
}
#[test]
fn isolated_source_bound_normalisation_oracle_excludes_subcategories_and_never_divides_weights_twice()
 {
    // ISOLATED ARITHMETIC fixture, not a frozen Study or real/source-complete
    // inventory. H_i=NF_i gives sixteen unit ratios; source fractions sum to1.
    let m = module::verify(module::PACKAGE).unwrap();
    let categories = m
        .methods
        .iter()
        .map(|method| {
            let source = m.normalisation.iter().find(|n| n.method_id == method.id);
            CategoryAssessment {
                method_ref: DatasetRef {
                    id: method.id.clone(),
                    version: method.version.clone(),
                },
                name: method.name.clone(),
                role: method.role.clone(),
                parent_method_id: method.parent_id.clone(),
                reference_quantity_id: method.reference_quantity_id.clone(),
                unit: method.unit_label.clone(),
                selected: true,
                source_compatible: true,
                coverage: "complete".into(),
                canonical_characterised_value: Some(source.map_or_else(
                    || "1e100".into(),
                    |n| {
                        analysis::canonical_number(n.normalisation_factor.parse().unwrap()).unwrap()
                    },
                )),
                missing_inventory_indices: vec![],
                normalisation_weighting_included: method.role == "primary",
                reason_codes: vec![],
            }
        })
        .collect::<Vec<_>>();
    let n = normalisation::candidate(&m, &categories, true).unwrap();
    assert_eq!(n.categories.len(), 16);
    assert_eq!(n.canonical_candidate_single_score.as_deref(), Some("1"));
    assert!(
        n.categories
            .iter()
            .all(|r| r.canonical_normalised_value == "1")
    );
    assert!(
        normalisation::candidate(&m, &categories, false)
            .unwrap()
            .canonical_candidate_single_score
            .is_none()
    );
    let mut incomplete = categories;
    incomplete
        .iter_mut()
        .find(|c| c.role == "primary")
        .unwrap()
        .coverage = "partial".into();
    assert!(
        normalisation::candidate(&m, &incomplete, true)
            .unwrap()
            .canonical_candidate_single_score
            .is_none()
    );
}

fn pipeline_fixture() -> (ServiceInput, Vec<u8>, Vec<u8>, Vec<u8>, TrustedMethods) {
    use crate::study::{
        artifact_bytes,
        contracts::{Definition, FunctionalUnit, MethodRef, StudyBinding},
        execute,
        runtime_contracts::{ArtifactRef, PrivateArtifact, SourceSnapshot},
        sha256,
    };
    let methods = TrustedMethods::synthetic_fixture().unwrap();
    let mut source = crate::study::tests::method_assessment_source_fixture();
    let snapshot_source = source.clone();
    source.study_binding = Some(StudyBinding {
        study_revision_id: "10000000-0000-4000-8000-000000000002".into(),
        study_revision_content_hash: "d".repeat(64),
        definition: Definition {
            goal: "SYNTHETIC frozen-byte predicate fixture, not an industry case".into(),
            functional_unit: FunctionalUnit {
                amount: "1".into(),
                unit: "kg".into(),
                reference_flow_id: "product".into(),
            },
            system_boundary: "Declared synthetic source only".into(),
            method_refs: source
                .method_refs
                .iter()
                .map(|m| MethodRef {
                    r#type: "datasetRevision".into(),
                    dataset_type: "lciamethod".into(),
                    id: m.id.clone(),
                    version: m.version.clone(),
                })
                .collect(),
            assumptions: vec![],
        },
    });
    let prepared = methods.reprepare(&source, &Bounds::default()).unwrap();
    let mut result = execute(&prepared).unwrap();
    // Synthetic service fixtures freeze timing metadata; no capacity claim follows.
    result.elapsed_millis = 0;
    let pb = artifact_bytes(&prepared).unwrap();
    let rb = serde_json::to_vec(&result).unwrap();
    let seams: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-analysis/service-seams.json"
    ))
    .unwrap();
    let mut row: old::RunInput =
        serde_json::from_value(seams["readInputResponse"]["data"]["runs"][0].clone()).unwrap();
    row.prepared_artifact.artifact_ref.content_hash = sha256(&pb);
    row.prepared_artifact.artifact_ref.byte_size = u64::try_from(pb.len()).unwrap();
    row.result_artifact.artifact_ref.content_hash = sha256(&rb);
    row.result_artifact.artifact_ref.byte_size = u64::try_from(rb.len()).unwrap();
    row.run["inputHash"] = json!(prepared.input_hash);
    row.run["effectiveModelRef"]["contentHash"] = json!(prepared.model_hash);
    row.run["calculationContractRef"] =
        serde_json::to_value(&prepared.calculation_contract_ref).unwrap();
    row.run["resultRef"] = serde_json::to_value(&row.result_artifact.artifact_ref).unwrap();
    row.revision["contentHash"] = json!("d".repeat(64));
    row.revision["definition"] =
        serde_json::to_value(&source.study_binding.as_ref().unwrap().definition).unwrap();
    row.scenario["effectiveModelRef"] = row.run["effectiveModelRef"].clone();
    row.scenario["overrides"] = json!([]);
    row.preparation["modelRef"] = row.run["effectiveModelRef"].clone();
    row.preparation["inputHash"] = row.run["inputHash"].clone();
    row.preparation["inputArtifactRef"] =
        serde_json::to_value(&row.prepared_artifact.artifact_ref).unwrap();
    row.preparation["calculationContractRef"] = row.run["calculationContractRef"].clone();
    row.preparation["methodRefs"] = row.revision["definition"]["methodRefs"].clone();
    let packet = SourceSnapshot {
        schema_version: crate::study::runtime_contracts::SNAPSHOT_SCHEMA.into(),
        origin_ref: json!({"type":"datasetRevision","datasetType":"lifecyclemodel","id":"model","version":"01.00.000"}),
        license_ref: "synthetic-not-a-license".into(),
        issuer_id: "synthetic-fixture-not-an-authorized-source-issuer".into(),
        observed_at: "2026-11-04T00:00:00Z".into(),
        source_authorization: json!({"synthetic":true,"doesNotGrantAuthority":true}),
        source: snapshot_source,
    };
    let sb = serde_json::to_vec(&packet).unwrap();
    let cap=PrivateArtifact{study_id:Uuid::parse_str("10000000-0000-4000-8000-000000000001").unwrap(),artifact_ref:ArtifactRef{r#type:"modelSource".into(),id:Uuid::parse_str("10000000-0000-4000-8000-000000000080").unwrap(),content_hash:sha256(&sb),digest_scope:"artifact_bytes_sha256".into(),byte_size:u64::try_from(sb.len()).unwrap(),media_type:"application/json".into(),schema_version:crate::study::runtime_contracts::SNAPSHOT_SCHEMA.into()},storage_bucket:"study-artifacts".into(),storage_object_path:"study/10000000-0000-4000-8000-000000000001/10000000-0000-4000-8000-000000000080/source.json".into()};
    let c = config();
    let input = ServiceInput {
        schema_version: INPUT_SCHEMA.into(),
        assessment: AssessmentRecord {
            assessment_id: Uuid::parse_str("10000000-0000-4000-8000-000000000104").unwrap(),
            study_id: cap.study_id,
            profile_id: PROFILE.into(),
            config_hash: config_hash(&c).unwrap(),
            config: c,
            execution: old::Execution {
                job_id: Uuid::parse_str("10000000-0000-4000-8000-000000000105").unwrap(),
                status: "running".into(),
            },
        },
        baseline: row,
        source_snapshot: json!({"snapshotId":"10000000-0000-4000-8000-000000000081","studyId":cap.study_id,"originRef":packet.origin_ref,"sourceArtifactRef":cap.artifact_ref,"digestScope":"artifact_bytes_sha256","licenseRef":packet.license_ref,"issuerId":packet.issuer_id,"observedAt":packet.observed_at}),
        source_artifact: cap,
    };
    (input, pb, rb, sb, methods)
}
#[test]
fn synthetic_pipeline_verifies_only_frozen_predicates_and_never_professional_or_official_source_approval()
 {
    let (input, pb, rb, sb, methods) = pipeline_fixture();
    let original = (pb.clone(), rb.clone(), sb.clone());
    let facts = assess(&input, &pb, &rb, &sb, &methods, &Bounds::default()).unwrap();
    assert_eq!(facts.requirements.len(), 20);
    assert!(
        !facts.pef_conformity_qualified
            && !facts.business_pilot_qualified
            && !facts.scientific_ranking_qualified
    );
    assert!(
        !facts
            .method_source_evidence
            .frozen_c_matches_verified_bundle
    );
    assert_eq!(
        facts.method_source_evidence.geographical_suitability,
        "not_verified"
    );
    assert!(
        facts
            .candidate_normalisation_weighting
            .canonical_candidate_single_score
            .is_none()
    );
    assert!(
        facts
            .requirements
            .iter()
            .all(|r| r.professional.status != ProfessionalStatus::ReferencesRecordedUnverified)
    );
    let wire = serde_json::to_vec(&facts).unwrap();
    verify_output(&facts, &wire, 4 * 1024 * 1024).unwrap();
    let text = String::from_utf8(wire).unwrap();
    for forbidden in [
        "storageBucket",
        "storageObjectPath",
        "sourceAuthorization",
        "ownerActorId",
        "doesNotGrantAuthority",
    ] {
        assert!(!text.contains(forbidden));
    }
    assert_eq!((pb, rb, sb), original);
}
#[test]
fn snapshot_substitution_issuer_document_and_result_forgery_are_rejected() {
    let (input, pb, rb, sb, methods) = pipeline_fixture();
    let mut altered = sb.clone();
    altered.push(b' ');
    assert!(assess(&input, &pb, &rb, &altered, &methods, &Bounds::default()).is_err());
    let mut changed = input.clone();
    changed.source_snapshot["issuerId"] = json!("caller-issuer");
    assert!(assess(&changed, &pb, &rb, &sb, &methods, &Bounds::default()).is_err());
    let mut result: Value = serde_json::from_slice(&rb).unwrap();
    result["h"][0] = json!(999.0);
    let bytes = serde_json::to_vec(&result).unwrap();
    let mut changed = input;
    changed.baseline.result_artifact.artifact_ref.content_hash = crate::study::sha256(&bytes);
    changed.baseline.result_artifact.artifact_ref.byte_size = u64::try_from(bytes.len()).unwrap();
    changed.baseline.run["resultRef"] =
        serde_json::to_value(&changed.baseline.result_artifact.artifact_ref).unwrap();
    assert!(assess(&changed, &pb, &bytes, &sb, &methods, &Bounds::default()).is_err());
}
#[test]
fn caller_reference_presence_cannot_approve_judgment_or_output() {
    use crate::study::method_assessment_contracts::{DeclaredEvidenceKind, EvidenceReference};
    let (mut input, pb, rb, sb, methods) = pipeline_fixture();
    input
        .assessment
        .config
        .evidence_refs
        .push(EvidenceReference {
            requirement_id: "PEF-18".into(),
            declared_kind: DeclaredEvidenceKind::ProfessionalRecord,
            artifact_ref: crate::study::runtime_contracts::ArtifactRef {
                r#type: "methodEvidence".into(),
                id: Uuid::parse_str("10000000-0000-4000-8000-000000000180").unwrap(),
                content_hash: "a".repeat(64),
                digest_scope: "artifact_bytes_sha256".into(),
                byte_size: 1,
                media_type: "application/json".into(),
                schema_version: "lca.study-method-evidence.v1".into(),
            },
        });
    input.assessment.config_hash = config_hash(&input.assessment.config).unwrap();
    let facts = assess(&input, &pb, &rb, &sb, &methods, &Bounds::default()).unwrap();
    assert_eq!(
        facts.requirements[17].professional.status,
        ProfessionalStatus::ReferencesRecordedUnverified
    );
    assert!(!facts.pef_conformity_qualified);
    let mut forged = serde_json::to_value(&facts).unwrap();
    forged["pefConformityQualified"] = json!(true);
    assert!(
        verify_output(
            &facts,
            &serde_json::to_vec(&forged).unwrap(),
            4 * 1024 * 1024
        )
        .is_err()
    );
}

#[test]
fn source_ordered_mixed_arithmetic_matches_independent_decimal_fixture() {
    // Deliberately not a Study: source NF_i * distinct exact binary ratios,
    // signed/zero values and enormous decompositions catch scaling/mapping bugs.
    let m = module::verify(module::PACKAGE).unwrap();
    let ratios = [
        0.0, 0.125, -0.5, 1.0, 2.5, -4.0, 8.0, 0.25, 1.5, 3.0, -2.0, 0.0625, 4.5, 0.75, -0.125,
        16.0,
    ];
    let categories = m
        .methods
        .iter()
        .map(|method| {
            let n = m
                .normalisation
                .iter()
                .position(|n| n.method_id == method.id);
            CategoryAssessment {
                method_ref: DatasetRef {
                    id: method.id.clone(),
                    version: method.version.clone(),
                },
                name: method.name.clone(),
                role: method.role.clone(),
                parent_method_id: method.parent_id.clone(),
                reference_quantity_id: method.reference_quantity_id.clone(),
                unit: method.unit_label.clone(),
                selected: true,
                source_compatible: true,
                coverage: "complete".into(),
                canonical_characterised_value: Some(
                    analysis::canonical_number(n.map_or(1e100, |i| {
                        m.normalisation[i]
                            .normalisation_factor
                            .parse::<f64>()
                            .unwrap()
                            * ratios[i]
                    }))
                    .unwrap(),
                ),
                missing_inventory_indices: vec![],
                normalisation_weighting_included: method.role == "primary",
                reason_codes: vec![],
            }
        })
        .collect::<Vec<_>>();
    let result = normalisation::candidate(&m, &categories, true).unwrap();
    let actual = json!({"classification":"isolated_arithmetic_not_a_study_or_conformance_case","moduleArtifactSha256":module::PACKAGE_SHA256,"inputCategories":categories,"normalisationWeighting":result});
    let bytes = serde_json::to_vec_pretty(&actual).unwrap();
    if let Some(path) = std::env::var_os("STUDY_METHOD_ARITHMETIC_OUTPUT") {
        std::fs::write(path, bytes).unwrap();
    } else {
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/study-method-assessment/arithmetic.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }
    for mutation in [
        "missing",
        "version",
        "unit",
        "reference",
        "source",
        "unknown",
    ] {
        let mut changed = categories.clone();
        let c = changed.iter_mut().find(|c| c.role == "primary").unwrap();
        match mutation {
            "missing" => c.selected = false,
            "version" => c.method_ref.version = "99.99.999".into(),
            "unit" => c.unit = "arbitrary".into(),
            "reference" => c.reference_quantity_id = "wrong".into(),
            "source" => c.source_compatible = false,
            _ => c.coverage = "unknown".into(),
        }
        let refused = normalisation::candidate(&m, &changed, true).unwrap();
        assert_eq!(refused.status, "unavailable", "{mutation}");
        assert!(refused.canonical_candidate_single_score.is_none());
    }
}

#[test]
fn cutoff_branch_and_declared_rule_never_turn_presence_into_method_approval() {
    let (mut input, pb, rb, sb, methods) = pipeline_fixture();
    input.assessment.config.declarations.study_phase = StudyPhase::Screening;
    input.assessment.config.declarations.category_rule =
        CategoryRuleDeclaration::CommissionDeveloped {
            reference_id: "caller-supplied-unverified-pefcr".into(),
            version: "1".into(),
            evidence_id: Uuid::parse_str("10000000-0000-4000-8000-000000000181").unwrap(),
        };
    input.assessment.config_hash = config_hash(&input.assessment.config).unwrap();
    let facts = assess(&input, &pb, &rb, &sb, &methods, &Bounds::default()).unwrap();
    assert_eq!(
        facts.requirements[7].machine.status,
        MachineStatus::Unsupported
    );
    assert_eq!(
        facts.requirements[7].professional.status,
        ProfessionalStatus::Pending
    );
    assert!(!facts.pef_conformity_qualified);
}

#[test]
fn offline_candidate_has_no_job_lease_or_source_locator_and_rejects_nil_identity() {
    use crate::study::method_assessment_contracts::{CANDIDATE_INPUT_SCHEMA, CandidateInput};
    let (input, pb, rb, sb, methods) = pipeline_fixture();
    let candidate = CandidateInput {
        schema_version: CANDIDATE_INPUT_SCHEMA.into(),
        candidate_id: input.assessment.assessment_id,
        study_id: input.assessment.study_id,
        config: input.assessment.config,
        config_hash: input.assessment.config_hash,
        baseline: input.baseline,
        source_snapshot: input.source_snapshot,
        source_artifact_ref: input.source_artifact.artifact_ref,
    };
    let facts = assess_candidate(&candidate, &pb, &rb, &sb, &methods, &Bounds::default()).unwrap();
    assert_eq!(
        facts.identity_kind,
        "offline_candidate_not_registered_or_authorized"
    );
    for field in [
        "jobId",
        "leaseToken",
        "execution",
        "sourceArtifact",
        "callerApproved",
    ] {
        let mut value = serde_json::to_value(&candidate).unwrap();
        value[field] = json!("caller-invented");
        assert!(
            serde_json::from_value::<CandidateInput>(value).is_err(),
            "{field}"
        );
    }
    let mut nil = candidate.clone();
    nil.candidate_id = Uuid::nil();
    assert!(validate_candidate(&nil).is_err());
    let mut nil = candidate.clone();
    nil.study_id = Uuid::nil();
    assert!(validate_candidate(&nil).is_err());
    let mut nil = candidate;
    nil.source_artifact_ref.id = Uuid::nil();
    assert!(validate_candidate(&nil).is_err());
}

#[test]
fn config_identity_matches_independent_cross_language_golden() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/study-method-assessment/config-identity.json"
    ))
    .unwrap();
    let parsed: Config = serde_json::from_value(fixture["config"].clone()).unwrap();
    assert_eq!(config_hash(&parsed).unwrap(), fixture["sha256"]);
    assert_eq!(
        serde_json::to_string(&serde_json::to_value(&parsed).unwrap()).unwrap(),
        fixture["canonicalUtf8"]
    );
    assert_eq!(fixture["domain"], "study.method-assessment-config.v1");
}

fn service_context(input: ServiceInput) -> super::super::method_runtime_contracts::JobContext {
    use super::super::method_runtime_contracts::{
        Budget, CONTEXT_SCHEMA, JobContext, MAX_FACTS_BYTES,
    };
    JobContext {
        schema_version: CONTEXT_SCHEMA.into(),
        input,
        budget: Budget {
            max_selected_factor_records: MAX_SELECTED_FACTOR_RECORDS,
            max_facts_bytes: MAX_FACTS_BYTES,
        },
    }
}

#[test]
fn registered_method_output_has_complete_stable_catalog_and_exact_frozen_purpose() {
    let (input, pb, rb, sb, methods) = pipeline_fixture();
    let context = service_context(input);
    let facts = assess_context(&context, &pb, &rb, &sb, &methods, &Bounds::default()).unwrap();
    validate_registered_output(&facts, &context, &pb, &rb, &Bounds::default()).unwrap();
    assert_eq!(facts.fact_catalog.len(), 58);
    assert_eq!(
        facts
            .fact_catalog
            .iter()
            .map(|f| &f.fact_id)
            .collect::<BTreeSet<_>>()
            .len(),
        58
    );
    assert_eq!(
        facts
            .fact_catalog
            .iter()
            .filter(|f| f.fact_id.starts_with("normalised:"))
            .count(),
        16
    );
    assert!(
        facts
            .fact_catalog
            .iter()
            .filter(|f| !f.fact_id.starts_with("category:"))
            .all(|f| f.canonical_value.is_none()
                && f.coverage == "unknown"
                && !f.reason_codes.is_empty())
    );
    let mut forged = facts.clone();
    forged.fact_catalog[0].canonical_value = Some("999".into());
    assert!(validate_registered_output(&forged, &context, &pb, &rb, &Bounds::default()).is_err());
    forged = facts.clone();
    forged.assessment_id = Uuid::new_v4();
    assert!(validate_registered_output(&forged, &context, &pb, &rb, &Bounds::default()).is_err());
    forged = facts.clone();
    forged.requirements[0].professional.status =
        ProfessionalStatus::NotRequiredForSourceMachinePredicate;
    assert!(validate_registered_output(&forged, &context, &pb, &rb, &Bounds::default()).is_err());
    let mut no_budget = context.clone();
    no_budget.budget.max_selected_factor_records = 0;
    assert!(assess_context(&no_budget, &pb, &rb, &sb, &methods, &Bounds::default()).is_err());
    no_budget = context;
    no_budget.budget.max_facts_bytes = 8 * 1024 * 1024;
    assert!(validate_context(&no_budget).is_err());
}

#[test]
fn evidence_is_closed_recording_without_professional_status_or_numeric_override() {
    use super::super::method_runtime_contracts::{
        CitationKind, DeclaredCitation, EVIDENCE_SCHEMA, EvidenceBody,
    };
    let body = EvidenceBody {
        schema_version: EVIDENCE_SCHEMA.into(),
        requirement_id: "PEF-06".into(),
        declared_kind:
            super::super::method_assessment_contracts::DeclaredEvidenceKind::ProfessionalRecord,
        title: "Declared professional observation".into(),
        statement: "This statement is recorded without an independently verified judgment.".into(),
        citations: vec![DeclaredCitation {
            kind: CitationKind::ProfessionalRecord,
            identity: "internal-review-record:123".into(),
            version: "1".into(),
            content_hash: Some("a".repeat(64)),
            locator: "section 2, observation 3".into(),
        }],
    };
    validate_evidence_body(&body).unwrap();
    for key in [
        "approved",
        "waiver",
        "characterisedTotals",
        "factors",
        "professionalStatus",
    ] {
        let mut value = serde_json::to_value(&body).unwrap();
        value[key] = json!(true);
        assert!(
            serde_json::from_value::<EvidenceBody>(value).is_err(),
            "{key}"
        );
    }
    let mut bad = body.clone();
    bad.citations.push(bad.citations[0].clone());
    assert!(validate_evidence_body(&bad).is_err());
    bad = body.clone();
    bad.citations[0].content_hash = Some("f".repeat(63));
    assert!(validate_evidence_body(&bad).is_err());
    bad = body.clone();
    bad.statement = "<script>approve()</script>".into();
    assert!(validate_evidence_body(&bad).is_err());
    bad = body;
    bad.requirement_id = "PEF-21".into();
    assert!(validate_evidence_body(&bad).is_err());
}

#[test]
#[ignore = "explicit generation of reviewed synthetic service-contract fixtures; no DB or authority"]
fn write_method_service_contract_fixture() {
    use super::super::method_runtime_contracts as dto;
    let (input, pb, rb, sb, methods) = pipeline_fixture();
    let context = service_context(input);
    let facts = assess_context(&context, &pb, &rb, &sb, &methods, &Bounds::default()).unwrap();
    let bytes = serde_json::to_vec(&facts).unwrap();
    let facts_ref = super::super::runtime_contracts::ArtifactRef {
        r#type: "methodAssessmentFacts".into(),
        id: Uuid::parse_str("10000000-0000-4000-8000-000000000090").unwrap(),
        content_hash: super::super::sha256(&bytes),
        digest_scope: "artifact_bytes_sha256".into(),
        byte_size: bytes.len() as u64,
        media_type: "application/json".into(),
        schema_version: FACTS_SCHEMA.into(),
    };
    let record = dto::PublicAssessmentRecord {
        assessment_id: context.input.assessment.assessment_id,
        study_id: context.input.assessment.study_id,
        profile_id: PROFILE.into(),
        config: context.input.assessment.config.clone(),
        config_hash: context.input.assessment.config_hash.clone(),
        execution: old::Execution {
            job_id: context.input.assessment.execution.job_id,
            status: "completed".into(),
        },
        facts_ref: Some(facts_ref.clone()),
    };
    let completion = dto::Completion { profile_id: PROFILE.into(), config_hash: context.input.assessment.config_hash.clone(), selected_factor_record_count: facts.method_source_evidence.selected_factors.len(), facts_ref, storage_bucket: "study-artifacts".into(), storage_object_path: "study/10000000-0000-4000-8000-000000000001/10000000-0000-4000-8000-000000000090/method-facts.json".into() };
    let fixture = json!({"factsFile":"facts.synthetic.json","schemaVersion":"lca.study-method-service-fixture.v1","classification":"synthetic_transport_not_authorization_or_scientific_qualification","jobKind":dto::JOB_KIND,"payload":{"schemaVersion":dto::JOB_SCHEMA,"studyId":context.input.assessment.study_id,"assessmentId":context.input.assessment.assessment_id},"inputContext":context,"record":record,"completion":completion,"facts":facts,"serviceFunctions":[{"name":dto::READ_INPUT_RPC,"arguments":["p_assessment_id","p_job_id","p_lease_token"]},{"name":dto::COMPLETE_RPC,"arguments":["p_assessment_id","p_job_id","p_lease_token","p_completion","p_idempotency_key"]},{"name":dto::FAIL_RPC,"arguments":["p_resource_type","p_resource_id","p_job_id","p_lease_token","p_error_code","p_idempotency_key"]},{"name":dto::RECONCILE_RPC,"arguments":["p_resource_type","p_resource_id","p_job_id","p_lease_token","p_idempotency_key","p_artifact_id"]}]});
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/study-method-assessment/service-seams.json");
    let mut output_bytes = serde_json::to_vec_pretty(&fixture).unwrap();
    output_bytes.push(b'\n');
    std::fs::write(output.with_file_name("facts.synthetic.json"), bytes).unwrap();
    std::fs::write(output, output_bytes).unwrap();
}
