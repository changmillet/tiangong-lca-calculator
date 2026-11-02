//! CPU/native work runs only inside the killable child, including static bundle loading.
use super::{
    VerifiedMethods, artifact_bytes,
    contracts::{AmountOverride, Bounds, PreparedInput, SOURCE_SCHEMA},
    decode_prepared, execute, prepare,
    runtime_contracts::{
        CHILD_SCHEMA, NativeAction, NativeRequest, SNAPSHOT_SCHEMA, SourceSnapshot,
    },
};
use crate::static_lcia_cache::{TrustedStaticCacheSource, load_verified_static_lcia_bundle};
use anyhow::{Context, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn read_bounded(path: &str, max: usize) -> anyhow::Result<Vec<u8>> {
    let path = Path::new(path);
    ensure!(path.is_absolute(), "study_child_path_invalid");
    ensure!(
        std::fs::metadata(path)?.len() <= u64::try_from(max)?,
        "study_child_input_limit"
    );
    Ok(std::fs::read(path)?)
}
fn snapshot(path: &str, limits: &Bounds) -> anyhow::Result<SourceSnapshot> {
    let packet: SourceSnapshot =
        super::json::from_slice(&read_bounded(path, limits.max_source_bytes)?)?;
    ensure!(
        packet.schema_version == SNAPSHOT_SCHEMA && packet.source.schema_version == SOURCE_SCHEMA,
        "study_source_snapshot_schema"
    );
    ensure!(
        packet.source.method_refs.is_empty()
            && packet.source.cutoffs.is_empty()
            && packet.source.overrides.is_empty()
            && packet.source.study_binding.is_none(),
        "study_source_snapshot_not_raw"
    );
    ensure!(
        packet.origin_ref["id"] == packet.source.origin.id
            && packet.origin_ref["version"] == packet.source.origin.version,
        "study_source_snapshot_origin"
    );
    Ok(packet)
}
/// This merges a child draft against the cumulative frozen model, retaining unchanged overrides.
fn merge_overrides(
    base: &[AmountOverride],
    changes: Vec<AmountOverride>,
) -> anyhow::Result<Vec<AmountOverride>> {
    let mut values = base
        .iter()
        .map(|v| (v.path.clone(), v.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut seen_paths = std::collections::BTreeSet::new();
    for change in changes {
        ensure!(
            seen_paths.insert(change.path.clone()),
            "study_duplicate_override"
        );
        values.insert(change.path.clone(), change);
    }
    Ok(values.into_values().collect())
}
fn validate_base_source(
    base: &PreparedInput,
    source: &super::contracts::SourceInput,
) -> anyhow::Result<()> {
    ensure!(
        base.model.origin == source.origin,
        "study_frozen_source_origin"
    );
    for doc in &source.documents {
        ensure!(
            base.model
                .source_documents
                .iter()
                .any(|d| d.kind == doc.kind
                    && d.identity == doc.identity
                    && d.sha256 == doc.sha256
                    && d.state_code == doc.state_code),
            "study_frozen_source_document"
        );
    }
    ensure!(
        base.model.source_documents.len() == source.documents.len(),
        "study_frozen_source_membership"
    );
    Ok(())
}
#[allow(clippy::too_many_lines)]
pub async fn execute_request(request: NativeRequest) -> anyhow::Result<()> {
    ensure!(request.schema_version == CHILD_SCHEMA, "study_child_schema");
    request.limits.validate()?;
    ensure!(
        Path::new(&request.output_path).is_absolute(),
        "study_child_path_invalid"
    );
    let output = match request.operation {
        NativeAction::Probe => {
            use solver_core::{
                ModelSparseData, NumericOptions, SolveOptions, SolverService, SparseTriplet,
            };
            let data = ModelSparseData {
                model_version: uuid::Uuid::new_v4(),
                process_count: 1,
                flow_count: 1,
                impact_count: 1,
                technosphere_entries: vec![SparseTriplet {
                    row: 0,
                    col: 0,
                    value: 0.2,
                }],
                biosphere_entries: vec![SparseTriplet {
                    row: 0,
                    col: 0,
                    value: 4.0,
                }],
                characterization_factors: vec![SparseTriplet {
                    row: 0,
                    col: 0,
                    value: 3.0,
                }],
            };
            let solver = SolverService::with_exact_zero_policy(request.limits.max_owned_bytes, 8.0);
            solver.prepare(&data, NumericOptions::default())?;
            let answer = solver.solve_one(
                data.model_version,
                NumericOptions::default(),
                &[1.0],
                SolveOptions::default(),
            )?;
            ensure!(
                answer.x == Some(vec![1.25])
                    && answer.g == Some(vec![5.0])
                    && answer.h == Some(vec![15.0]),
                "study_native_probe_numeric"
            );
            // Hold a real native process long enough for the parent to exercise its RSS guard.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            serde_json::to_vec(
                &serde_json::json!({"schemaVersion":"lca.study-native.probe.v1","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"abi":if cfg!(target_env="gnu"){"gnu"}else if cfg!(target_env="msvc"){"msvc"}else{"native"},"compilerVersion":super::contracts::COMPILER,"calculationContractSha256":crate::calculation_contract::fingerprint(),"x":[1.25],"g":[5.0],"h":[15.0]}),
            )?
        }
        NativeAction::Acquire { snapshot_path } => {
            let packet = snapshot(&snapshot_path, &request.limits)?;
            let source_bytes = serde_json::to_vec(&packet.source)?;
            ensure!(
                source_bytes.len().saturating_mul(8) <= request.limits.max_owned_bytes,
                "study_owned_memory_limit"
            );
            super::source::normalize(
                &packet.source,
                &request.limits,
                source_bytes.len().saturating_mul(8),
            )?;
            serde_json::to_vec(&packet)?
        }
        NativeAction::PrepareBase {
            snapshot_path,
            method_refs,
            cutoffs,
            static_cache_directory,
            static_cache_base_url,
        } => {
            let mut packet = snapshot(&snapshot_path, &request.limits)?;
            packet.source.method_refs = method_refs;
            packet.source.cutoffs = cutoffs;
            let configuration = TrustedStaticCacheSource::new(
                static_cache_directory.map(PathBuf::from),
                static_cache_base_url,
            )?;
            let bundle = load_verified_static_lcia_bundle(
                &configuration,
                &crate::calculation_evidence::method_factor_source_contract_fixture(),
            )
            .await?;
            let methods = VerifiedMethods::from_static(&bundle, &packet.source.method_refs)?;
            artifact_bytes(&prepare(
                &serde_json::to_vec(&packet.source)?,
                &methods,
                &request.limits,
            )?)?
        }
        NativeAction::PrepareScenario {
            snapshot_path,
            base_prepared_path,
            base_prepared_sha256,
            study_binding,
            overrides,
        } => {
            let mut packet = snapshot(&snapshot_path, &request.limits)?;
            let base = decode_prepared(
                &read_bounded(&base_prepared_path, request.limits.max_artifact_bytes)?,
                &base_prepared_sha256,
                &request.limits,
            )?;
            validate_base_source(&base, &packet.source)?;
            let refs = study_binding
                .definition
                .method_refs
                .iter()
                .map(|r| super::contracts::DatasetRef {
                    id: r.id.clone(),
                    version: r.version.clone(),
                })
                .collect::<Vec<_>>();
            let methods = VerifiedMethods::from_frozen(&base, &refs)?;
            packet.source.method_refs = refs;
            packet.source.cutoffs.clone_from(&base.model.cutoffs);
            packet.source.overrides = merge_overrides(&base.model.overrides, overrides)?;
            packet.source.study_binding = Some(study_binding);
            artifact_bytes(&prepare(
                &serde_json::to_vec(&packet.source)?,
                &methods,
                &request.limits,
            )?)?
        }
        NativeAction::Run {
            prepared_path,
            prepared_sha256,
        } => {
            let input = decode_prepared(
                &read_bounded(&prepared_path, request.limits.max_artifact_bytes)?,
                &prepared_sha256,
                &request.limits,
            )?;
            ensure!(
                input.input_hash.is_some(),
                "study_run_input_identity_missing"
            );
            serde_json::to_vec(&execute(&input)?)?
        }
    };
    ensure!(
        output.len() <= request.limits.max_artifact_bytes,
        "study_child_output_limit"
    );
    // Atomic publication into a parent-created private directory; the parent uploads only this file.
    let parent = Path::new(&request.output_path)
        .parent()
        .context("study_child_output_parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temp, &output)?;
    temp.as_file().sync_all()?;
    temp.persist(&request.output_path)?;
    Ok(())
}

/// Independently bind the private child request to the effective `SourceInput` bytes before launch.
/// This is transport/provenance assembly only; it does not compile matrices or read live factors.
pub(super) fn effective_source(
    operation: &NativeAction,
    limits: &Bounds,
) -> anyhow::Result<Option<super::contracts::SourceInput>> {
    match operation {
        NativeAction::PrepareBase {
            snapshot_path,
            method_refs,
            cutoffs,
            ..
        } => {
            let mut packet = snapshot(snapshot_path, limits)?;
            packet.source.method_refs.clone_from(method_refs);
            packet.source.cutoffs.clone_from(cutoffs);
            Ok(Some(packet.source))
        }
        NativeAction::PrepareScenario {
            snapshot_path,
            base_prepared_path,
            base_prepared_sha256,
            study_binding,
            overrides,
        } => {
            let mut packet = snapshot(snapshot_path, limits)?;
            let base = decode_prepared(
                &read_bounded(base_prepared_path, limits.max_artifact_bytes)?,
                base_prepared_sha256,
                limits,
            )?;
            validate_base_source(&base, &packet.source)?;
            packet.source.method_refs = study_binding
                .definition
                .method_refs
                .iter()
                .map(|r| super::contracts::DatasetRef {
                    id: r.id.clone(),
                    version: r.version.clone(),
                })
                .collect();
            packet.source.cutoffs = base.model.cutoffs;
            packet.source.overrides = merge_overrides(&base.model.overrides, overrides.clone())?;
            packet.source.study_binding = Some(study_binding.clone());
            Ok(Some(packet.source))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inherited_overrides_survive_child_draft() {
        let base = vec![
            AmountOverride {
                path: "/instances/a/exchanges/e/amount".into(),
                value: 2.0,
            },
            AmountOverride {
                path: "/instances/b/exchanges/e/amount".into(),
                value: 3.0,
            },
        ];
        let changed = merge_overrides(
            &base,
            vec![AmountOverride {
                path: base[0].path.clone(),
                value: 4.0,
            }],
        )
        .unwrap();
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[0].value, 4.0);
        assert_eq!(changed[1].value, 3.0);
    }
}

#[cfg(test)]
mod manual_component_fixture {
    #[tokio::test]
    #[ignore = "operator-owned private native request fixture; not source authorization evidence"]
    async fn execute_private_native_request() {
        let path =
            std::env::var("STUDY_NATIVE_TEST_REQUEST").expect("private request path required");
        let request: super::NativeRequest =
            crate::study::json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        if let Err(error) = super::execute_request(request).await {
            panic!("native component request failed: {error:#}");
        }
    }
}
