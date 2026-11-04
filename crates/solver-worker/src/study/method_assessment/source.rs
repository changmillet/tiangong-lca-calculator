//! A sealed, actual-release loader and source comparison. No caller trust boolean.
use super::{
    super::{
        contracts::{Bounds, DatasetRef, PreparedInput, SourceInput, StudyResult},
        method_assessment_contracts::{
            SelectedFactorEvidence, SourceBridge, SourceLocationEvidence,
        },
        runtime_contracts::{ArtifactRef, SourceSnapshot},
        semantic_hash, sha256,
    },
    module::Module,
};
use crate::{
    lcia_nonregional,
    static_lcia_cache::{
        TrustedStaticCacheSource, VerifiedStaticLciaBundle, load_verified_static_lcia_bundle,
    },
};
use anyhow::{Context, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const BRIDGE_SHA256: &str = "eb0ff425a1ecc2dae1e8e6c1660fb672957400bd4c6f266e2453e0cc34fc0b51";
#[derive(Debug, Deserialize)]
struct BridgeFile {
    source_catalog_sha256: String,
    static_bundle_manifest_sha256: String,
    static_factor_manifest_sha256: String,
    nonregional_defaults_sha256: String,
    methods: Vec<BridgeMethod>,
}
#[derive(Debug, Deserialize)]
struct BridgeMethod {
    method_id: String,
    method_version: String,
    official_xml_sha256: String,
    local_xml_sha256: String,
    equivalence_kind: String,
    equivalence_evidence_sha256: Option<String>,
}
#[derive(Debug, Deserialize)]
struct Defaults {
    methods: Vec<DefaultMethod>,
}
#[derive(Debug, Deserialize)]
struct DefaultMethod {
    method_id: String,
    method_version: String,
    alternatives: Vec<DefaultAlternative>,
}
#[derive(Debug, Deserialize)]
struct DefaultAlternative {
    flow_id: String,
    flow_version: String,
    direction: String,
    value_bits: String,
    alternative_count: usize,
    source_record_index: usize,
    source_record_sha256: String,
    source_location: Option<String>,
}

/// Construction requires actual pinned manifest/list/factor bytes. This cannot
/// be deserialized or constructed from request-supplied bundle metadata.
pub struct TrustedMethods {
    bundle: VerifiedStaticLciaBundle,
    bridge: BridgeFile,
    defaults: Defaults,
    pinned_release: bool,
    #[cfg(test)]
    synthetic_methods: Option<super::super::VerifiedMethods>,
}
impl TrustedMethods {
    pub async fn load(source: &TrustedStaticCacheSource) -> anyhow::Result<Self> {
        let bundle = load_verified_static_lcia_bundle(
            source,
            &crate::calculation_evidence::method_factor_source_contract_fixture(),
        )
        .await?;
        Self::from_loaded(bundle, true)
    }
    fn from_loaded(bundle: VerifiedStaticLciaBundle, pinned_release: bool) -> anyhow::Result<Self> {
        ensure!(
            sha256(super::module::BRIDGE) == BRIDGE_SHA256,
            "study_method_source_bridge_pin"
        );
        let bridge: BridgeFile = serde_json::from_slice(super::module::BRIDGE)?;
        ensure!(
            bundle.source_evidence.bundle_manifest_sha256 == bridge.static_bundle_manifest_sha256
                && bundle.source_evidence.factor_manifest_sha256
                    == bridge.static_factor_manifest_sha256
                && sha256(lcia_nonregional::DEFAULTS) == bridge.nonregional_defaults_sha256,
            "study_method_static_source_pin"
        );
        let defaults = serde_json::from_slice(lcia_nonregional::DEFAULTS)?;
        Ok(Self {
            bundle,
            bridge,
            defaults,
            pinned_release,
            #[cfg(test)]
            synthetic_methods: None,
        })
    }
    pub(super) fn snapshot(&self) -> anyhow::Result<Value> {
        Ok(serde_json::to_value(&self.bundle.source_evidence)?)
    }
    pub(super) fn source_catalog_sha256(&self) -> &str {
        &self.bridge.source_catalog_sha256
    }
    pub(super) fn defaults_sha256(&self) -> &str {
        &self.bridge.nonregional_defaults_sha256
    }
    pub(super) const fn is_pinned_release(&self) -> bool {
        self.pinned_release
    }
    pub(super) fn bridges(&self, module: &Module) -> anyhow::Result<Vec<SourceBridge>> {
        ensure!(
            self.bridge.methods.len() == 25,
            "study_method_source_bridge_membership"
        );
        module
            .methods
            .iter()
            .map(|m| {
                let b = self
                    .bridge
                    .methods
                    .iter()
                    .find(|b| b.method_id == m.id && b.method_version == m.version)
                    .context("study_method_source_bridge_method")?;
                ensure!(
                    b.official_xml_sha256 == m.source_sha256,
                    "study_method_official_source_substitution"
                );
                ensure!(
                    b.official_xml_sha256 == b.local_xml_sha256
                        || (m.id == "b5c602c6-def3-11e6-bf01-fe55135034f3"
                            && b.equivalence_kind == "reviewed_whitespace_only_xml_structure"
                            && b.equivalence_evidence_sha256.is_some()),
                    "study_method_unreviewed_source_equivalence"
                );
                Ok(SourceBridge {
                    method_ref: DatasetRef {
                        id: m.id.clone(),
                        version: m.version.clone(),
                    },
                    official_xml_sha256: b.official_xml_sha256.clone(),
                    local_xml_sha256: b.local_xml_sha256.clone(),
                    equivalence_kind: b.equivalence_kind.clone(),
                    equivalence_evidence_sha256: b.equivalence_evidence_sha256.clone(),
                })
            })
            .collect()
    }
    #[cfg(test)]
    pub(super) fn synthetic_fixture() -> anyhow::Result<Self> {
        use super::super::{VerifiedMethods, compile::Factor, contracts::MethodAxis};
        use crate::calculation_evidence::LcaMethodFactorSourceSnapshot;
        let snapshot = LcaMethodFactorSourceSnapshot {
            schema_version: "synthetic.method-source.v1".into(),
            source_kind: "synthetic_software_fixture_not_official".into(),
            bundle_manifest_path: "synthetic".into(),
            bundle_manifest_sha256: "0".repeat(64),
            bundle_version: "synthetic".into(),
            source_snapshot_sha256: "0".repeat(64),
            method_manifest_sha256: "0".repeat(64),
            factor_manifest_sha256: "0".repeat(64),
            method_identity_manifest_sha256: "0".repeat(64),
            method_count: 1,
        };
        let methods = VerifiedMethods {
            axes: vec![MethodAxis {
                id: "6209b35f-9447-40b5-b68c-a1099e3674a0".into(),
                version: "01.00.000".into(),
                unit: "kg CO2 Equivalents".into(),
            }],
            factors: vec![vec![Factor {
                flow: DatasetRef {
                    id: "emission".into(),
                    version: "01.00.000".into(),
                },
                direction: "Output".into(),
                value: 1.0,
            }]],
            source_hash: semantic_hash("study.method-source.v1", &snapshot)?,
        };
        Ok(Self {
            bundle: VerifiedStaticLciaBundle {
                source_evidence: snapshot,
                methods: vec![],
                factors_by_method: BTreeMap::new(),
            },
            bridge: serde_json::from_slice(super::module::BRIDGE)?,
            defaults: serde_json::from_slice(lcia_nonregional::DEFAULTS)?,
            pinned_release: false,
            synthetic_methods: Some(methods),
        })
    }
    pub(super) fn reprepare(
        &self,
        source: &SourceInput,
        bounds: &Bounds,
    ) -> anyhow::Result<PreparedInput> {
        #[cfg(test)]
        if let Some(methods) = &self.synthetic_methods {
            return super::super::prepare(&serde_json::to_vec(source)?, methods, bounds);
        }
        let methods =
            super::super::VerifiedMethods::from_static(&self.bundle, &source.method_refs)?;
        super::super::prepare(&serde_json::to_vec(source)?, &methods, bounds)
    }
    pub(super) fn source_hash(&self) -> anyhow::Result<String> {
        semantic_hash("study.method-source.v1", &self.bundle.source_evidence)
    }
    pub(super) fn method_compatible(&self, id: &str, version: &str, unit: &str) -> bool {
        self.pinned_release
            && self.bundle.methods.iter().any(|m| {
                m.method_id.to_string() == id && m.method_version == version && m.unit == unit
            })
    }
    #[allow(clippy::too_many_lines)]
    pub(super) fn selected_factors(
        &self,
        prepared: &PreparedInput,
    ) -> anyhow::Result<Vec<SelectedFactorEvidence>> {
        let mut output = Vec::new();
        ensure!(
            prepared
                .numeric
                .method_axes
                .len()
                .saturating_mul(prepared.numeric.inventory_axes.len())
                <= super::super::method_assessment_contracts::MAX_SELECTED_FACTOR_RECORDS,
            "study_method_selected_factor_limit"
        );
        for (row, method) in prepared.numeric.method_axes.iter().enumerate() {
            let id = Uuid::parse_str(&method.id)?;
            let records = if self
                .bundle
                .methods
                .iter()
                .any(|m| m.method_id == id && m.method_version == method.version)
            {
                self.bundle.factors_by_method.get(&id)
            } else {
                None
            };
            let defaults = self
                .defaults
                .methods
                .iter()
                .find(|m| m.method_id == method.id && m.method_version == method.version);
            let selected =
                lcia_nonregional::prepare_static_method(&self.bundle, id, &method.version).ok();
            for (index, axis) in prepared.numeric.inventory_axes.iter().enumerate() {
                let direction = if axis.direction == "Input" {
                    crate::static_lcia_cache::StaticLciaDirection::Input
                } else {
                    crate::static_lcia_cache::StaticLciaDirection::Output
                };
                let f = selected.as_ref().and_then(|v| {
                    v.iter().find(|f| {
                        f.flow_id.to_string() == axis.flow.id
                            && f.flow_version == axis.flow.version
                            && f.direction == direction
                    })
                });
                let raw_count = records.map_or(0, |v| {
                    v.iter()
                        .filter(|f| {
                            f.flow_id.to_string() == axis.flow.id
                                && f.flow_version == axis.flow.version
                                && f.direction == direction
                        })
                        .count()
                });
                let alternative = defaults.and_then(|m| {
                    m.alternatives.iter().find(|a| {
                        a.flow_id == axis.flow.id
                            && a.flow_version == axis.flow.version
                            && a.direction.eq_ignore_ascii_case(&axis.direction)
                    })
                });
                let gap = prepared.coverage.iter().any(|g| {
                    g.method.id == method.id
                        && g.method.version == method.version
                        && g.inventory_index == index
                });
                let location = if let Some(a) = alternative {
                    ensure!(
                        f.is_some_and(|f| format!("{:016x}", f.value.to_bits()) == a.value_bits)
                            && raw_count == a.alternative_count,
                        "study_method_selection_provenance"
                    );
                    SourceLocationEvidence {
                        status: "source_bound_ambiguous_default".into(),
                        original_value: a.source_location.clone(),
                        source_record_ordinal: Some(a.source_record_index),
                        source_record_sha256: Some(a.source_record_sha256.clone()),
                    }
                } else {
                    SourceLocationEvidence {
                        status: if raw_count == 1 {
                            "unique_factor_raw_context_not_retained"
                        } else {
                            "no_selected_factor"
                        }
                        .into(),
                        original_value: None,
                        source_record_ordinal: None,
                        source_record_sha256: None,
                    }
                };
                let matches = f.is_some_and(|f| {
                    let expected = if f.value == 0.0 { 0.0 } else { f.value };
                    !gap && prepared.numeric.c[row][index].to_bits() == expected.to_bits()
                });
                output.push(SelectedFactorEvidence {
                    method_ref: DatasetRef {
                        id: method.id.clone(),
                        version: method.version.clone(),
                    },
                    inventory_index: index,
                    flow: axis.flow.clone(),
                    direction: axis.direction.clone(),
                    frozen_unit: axis.unit.clone(),
                    frozen_unit_support: prepared
                        .model
                        .instances
                        .iter()
                        .flat_map(|i| &i.exchanges)
                        .find(|e| {
                            e.flow == axis.flow
                                && e.direction == axis.direction
                                && e.unit == axis.unit
                        })
                        .map_or_else(Vec::new, |e| e.unit_identity.clone()),
                    canonical_effective_factor: if matches {
                        Some(super::super::analysis::canonical_number(
                            prepared.numeric.c[row][index],
                        )?)
                    } else {
                        None
                    },
                    raw_factor_value_bits: f.map(|f| format!("{:016x}", f.value.to_bits())),
                    alternative_count: raw_count,
                    location,
                    status: if matches {
                        "source_factor_matches_frozen_c"
                    } else if f.is_none() && gap {
                        "missing_factor_not_zero"
                    } else {
                        "factor_or_coverage_mismatch"
                    }
                    .into(),
                });
            }
        }
        Ok(output)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotMetadata {
    snapshot_id: Uuid,
    study_id: Uuid,
    origin_ref: Value,
    source_artifact_ref: ArtifactRef,
    digest_scope: String,
    license_ref: String,
    issuer_id: String,
    observed_at: String,
}

pub(super) fn validate_metadata_binding(
    input: &super::super::method_assessment_contracts::ServiceInput,
) -> anyhow::Result<()> {
    let meta: SnapshotMetadata = serde_json::from_value(input.source_snapshot.clone())?;
    let origin = meta
        .origin_ref
        .as_object()
        .context("study_method_snapshot_origin")?;
    ensure!(
        !meta.snapshot_id.is_nil()
            && meta.study_id == input.assessment.study_id
            && meta.digest_scope == "artifact_bytes_sha256"
            && serde_json::to_value(meta.source_artifact_ref)?
                == serde_json::to_value(&input.source_artifact.artifact_ref)?
            && origin.len() == 4
            && meta.origin_ref["type"] == "datasetRevision"
            && meta.origin_ref["datasetType"] == "lifecyclemodel"
            && origin
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|v| super::bounded(v, 128))
            && origin
                .get("version")
                .and_then(Value::as_str)
                .is_some_and(|v| super::bounded(v, 64))
            && super::bounded(&meta.license_ref, 2048)
            && super::bounded(&meta.issuer_id, 256)
            && chrono::DateTime::parse_from_rfc3339(&meta.observed_at).is_ok(),
        "study_method_snapshot_metadata_binding"
    );
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct SourceProof {
    pub source: SourceInput,
    pub source_ref: ArtifactRef,
    pub source_origin: DatasetRef,
    pub recompiled_matches: bool,
    pub method_source_matches: bool,
}
fn same(a: &Value, b: &Value) -> anyhow::Result<()> {
    ensure!(a == b, "study_method_source_binding");
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(super) fn verify_snapshot(
    input: &super::super::method_assessment_contracts::ServiceInput,
    bytes: &[u8],
    prepared: &PreparedInput,
    result: &StudyResult,
    methods: &TrustedMethods,
    bounds: &Bounds,
) -> anyhow::Result<SourceProof> {
    let cap = &input.source_artifact;
    ensure!(
        bytes.len() <= bounds.max_source_bytes
            && bytes.len() == usize::try_from(cap.artifact_ref.byte_size)?
            && sha256(bytes) == cap.artifact_ref.content_hash,
        "study_method_snapshot_integrity"
    );
    ensure!(
        cap.study_id == input.assessment.study_id
            && cap.artifact_ref.r#type == "modelSource"
            && cap.artifact_ref.schema_version == super::super::runtime_contracts::SNAPSHOT_SCHEMA
            && cap.artifact_ref.digest_scope == "artifact_bytes_sha256"
            && cap.artifact_ref.media_type == "application/json",
        "study_method_snapshot_domain"
    );
    let packet: SourceSnapshot = super::super::json::from_slice(bytes)?;
    ensure!(
        packet.schema_version == super::super::runtime_contracts::SNAPSHOT_SCHEMA,
        "study_method_snapshot_schema"
    );
    let meta: SnapshotMetadata = serde_json::from_value(input.source_snapshot.clone())?;
    ensure!(!meta.snapshot_id.is_nil(), "study_method_snapshot_identity");
    same(
        &serde_json::to_value(&meta.source_artifact_ref)?,
        &serde_json::to_value(&cap.artifact_ref)?,
    )?;
    ensure!(
        meta.study_id == input.assessment.study_id && meta.digest_scope == "artifact_bytes_sha256",
        "study_method_snapshot_study"
    );
    same(&meta.origin_ref, &packet.origin_ref)?;
    ensure!(
        meta.issuer_id == packet.issuer_id && meta.license_ref == packet.license_ref,
        "study_method_snapshot_issuer_license"
    );
    ensure!(
        chrono::DateTime::parse_from_rfc3339(&meta.observed_at)?
            == chrono::DateTime::parse_from_rfc3339(&packet.observed_at)?,
        "study_method_snapshot_time"
    );
    ensure!(
        packet.source.origin == prepared.model.origin
            && packet.origin_ref["type"] == "datasetRevision"
            && packet.origin_ref["datasetType"] == "lifecyclemodel"
            && packet.origin_ref["id"] == packet.source.origin.id
            && packet.origin_ref["version"] == packet.source.origin.version,
        "study_method_snapshot_origin"
    );
    let expected = prepared
        .model
        .source_documents
        .iter()
        .map(|d| {
            (
                (
                    d.kind.clone(),
                    d.identity.id.clone(),
                    d.identity.version.clone(),
                ),
                (d.sha256.clone(), d.state_code),
            )
        })
        .collect::<BTreeMap<_, _>>();
    ensure!(
        expected.len() == prepared.model.source_documents.len(),
        "study_method_duplicate_frozen_source"
    );
    let mut seen = BTreeSet::new();
    for doc in &packet.source.documents {
        let key = (
            doc.kind.clone(),
            doc.identity.id.clone(),
            doc.identity.version.clone(),
        );
        ensure!(
            seen.insert(key.clone())
                && expected.get(&key) == Some(&(doc.sha256.clone(), doc.state_code))
                && sha256(doc.json.as_bytes()) == doc.sha256,
            "study_method_snapshot_document_membership"
        );
    }
    ensure!(
        seen.len() == expected.len(),
        "study_method_snapshot_document_membership"
    );
    let mut source = packet.source;
    let binding = prepared
        .study_binding
        .as_ref()
        .context("study_method_study_binding")?;
    source.method_refs = binding
        .definition
        .method_refs
        .iter()
        .map(|m| DatasetRef {
            id: m.id.clone(),
            version: m.version.clone(),
        })
        .collect();
    source.cutoffs.clone_from(&prepared.model.cutoffs);
    source.overrides.clone_from(&prepared.model.overrides);
    source.study_binding = Some(binding.clone());
    ensure!(
        sha256(&serde_json::to_vec(&source)?) == prepared.source_hash,
        "study_method_effective_source_hash"
    );
    let method_source_matches =
        methods.is_pinned_release() && methods.source_hash()? == prepared.method_source_hash;
    // Resource/parse failures remain failures, never a misleading scientific
    // mismatch or partial successful assessment.
    let recompiled = methods.reprepare(&source, bounds)?;
    let recompiled_matches = recompiled.model_hash == prepared.model_hash
        && recompiled.input_hash == prepared.input_hash
        && recompiled.numeric == prepared.numeric
        && recompiled.coverage == prepared.coverage
        && recompiled.method_source_hash == prepared.method_source_hash;
    ensure!(
        result.prepared_artifact_sha256
            == input.baseline.prepared_artifact.artifact_ref.content_hash,
        "study_method_result_source_binding"
    );
    Ok(SourceProof {
        source_origin: source.origin.clone(),
        source,
        source_ref: cap.artifact_ref.clone(),
        recompiled_matches,
        method_source_matches,
    })
}
