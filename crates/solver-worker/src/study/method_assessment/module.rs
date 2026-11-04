//! Consumer-pinned portable package. Source review metadata is never study approval.
use super::super::{method_assessment_contracts::ModuleRef, sha256};
use anyhow::{Context, ensure};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use std::collections::BTreeSet;

pub const PACKAGE_SHA256: &str = "b19ba5754b0bc20c44c8c65966bafafd5984cd74636f2c805c0fa4373f5554ad";
pub const PCR_MAIN: &str = "51c0842c67b14ba886e2edee2846e26eefac030f";
pub const REVIEW_BASIS: &str = "989391423c7f31bdf77e18ea4438fab1a27ff6708c0f44ecad8ab8813763a165";
pub const BINDINGS_SHA256: &str =
    "3fd9b1dc68228db3e146b6d23daac853b27b9f504f9445a0cc5a971d53257ce2";
pub const MODULE_ID: &str = "method.pef-2021-ef31";
pub const MODULE_VERSION: &str = "0.1.0";
pub const PACKAGE: &[u8] = include_bytes!("../method_assets/method.pef-2021-ef31.0.1.0.json");
pub const ADOPTION: &[u8] = include_bytes!("../method_assets/adoption.json");
pub const BRIDGE: &[u8] = include_bytes!("../method_assets/source-bridge.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub applicability: String,
    pub check_kind: String,
    pub sources: Vec<String>,
    pub canonical_markdown: String,
    pub translation_markdown: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub id: String,
    pub version: String,
    pub name: String,
    pub role: String,
    pub parent_id: Option<String>,
    pub source_member: String,
    pub source_sha256: String,
    pub source_bytes: u64,
    pub filename_id: String,
    pub reference_quantity_id: String,
    pub unit_label: String,
    pub source_unit_label: String,
    pub intervention_locations: Vec<String>,
    pub impact_locations: Vec<String>,
    pub factor_count: usize,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NwRecord {
    pub method_id: String,
    pub method_version: String,
    pub reference_quantity_id: String,
    pub unit_label: String,
    pub normalisation_factor: String,
    pub weight_fraction: String,
    pub source_unit_label: String,
    pub source_short_description: String,
}
#[derive(Debug, Clone)]
pub struct Module {
    pub reference: ModuleRef,
    pub requirements: Vec<Requirement>,
    pub methods: Vec<Method>,
    pub normalisation: Vec<NwRecord>,
    pub normalisation_source_sha256: String,
    pub package_source_sha256: String,
    pub normative_version: String,
    pub factor_package_version: String,
    pub interim_guidance: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    schema_version: u8,
    kind: String,
    payload_sha256: String,
    payload: Box<RawValue>,
}

#[must_use]
pub fn reference() -> ModuleRef {
    ModuleRef {
        repository: "tiangong-lca/pcr".into(),
        source_commit: PCR_MAIN.into(),
        module_id: MODULE_ID.into(),
        module_version: MODULE_VERSION.into(),
        artifact_sha256: PACKAGE_SHA256.into(),
        review_basis_sha256: REVIEW_BASIS.into(),
        source_bindings_sha256: BINDINGS_SHA256.into(),
    }
}
fn text(value: &Value, key: &str) -> anyhow::Result<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .context("study_method_package_text")
}
/// PCR owns ordered two-space JSON + newline, not Study's sorted compact hash.
fn payload_bytes(raw: &str) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(raw.len());
    for (index, line) in raw.split('\n').enumerate() {
        let line = if index == 0 {
            line
        } else {
            line.strip_prefix("  ")
                .context("study_method_package_payload_indent")?
        };
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
    Ok(output)
}

/// Independent code pins are the trust anchor; package-declared review is only data.
#[allow(clippy::too_many_lines)]
pub fn verify(bytes: &[u8]) -> anyhow::Result<Module> {
    ensure!(sha256(bytes) == PACKAGE_SHA256, "study_method_package_pin");
    let envelope: RawPackage = serde_json::from_slice(bytes)?;
    ensure!(
        envelope.schema_version == 1
            && envelope.kind == "method_module_package"
            && sha256(&payload_bytes(envelope.payload.get())?) == envelope.payload_sha256,
        "study_method_package_integrity"
    );
    let payload: Value = serde_json::from_str(envelope.payload.get())?;
    ensure!(
        payload["schema_version"] == 1
            && payload["kind"] == "method_module_projection"
            && payload["module_id"] == MODULE_ID
            && payload["module_version"] == MODULE_VERSION
            && payload["review_basis_sha256"] == REVIEW_BASIS
            && payload["manifest"]["status"] == "candidate"
            && payload["manifest"]["scientific_approval"] == false
            && payload["manifest"]["consumer_policy_included"] == false,
        "study_method_package_identity"
    );
    let files = payload["authored_files"]
        .as_object()
        .context("study_method_package_sources")?;
    ensure!(files.len() == 5, "study_method_package_sources");
    for name in [
        "manifest.yaml",
        "module.en-US.md",
        "module.zh-CN.md",
        "source-bindings.json",
        "review.json",
    ] {
        let raw = files[name]
            .as_str()
            .context("study_method_package_source")?;
        ensure!(
            payload["source_sha256"][name] == sha256(raw.as_bytes()),
            "study_method_package_source_hash"
        );
    }
    let bindings_raw = files["source-bindings.json"]
        .as_str()
        .context("study_method_bindings")?;
    ensure!(
        sha256(bindings_raw.as_bytes()) == BINDINGS_SHA256,
        "study_method_bindings_pin"
    );
    let bindings: Value = serde_json::from_str(bindings_raw)?;
    ensure!(
        bindings == payload["source_bindings"],
        "study_method_binding_projection"
    );
    let adoption: Value = serde_json::from_slice(ADOPTION)?;
    ensure!(
        adoption["repository"] == "tiangong-lca/pcr"
            && adoption["sourceCommit"] == PCR_MAIN
            && adoption["artifactSha256"] == PACKAGE_SHA256
            && adoption["reviewBasisSha256"] == REVIEW_BASIS
            && adoption["moduleId"] == MODULE_ID
            && adoption["moduleVersion"] == MODULE_VERSION
            && adoption["scientificApproval"] == false,
        "study_method_adoption_pin"
    );
    let requirements: Vec<Requirement> = serde_json::from_value(payload["requirements"].clone())?;
    ensure!(
        requirements.len() == 20
            && requirements.iter().enumerate().all(|(i, r)| {
                r.id == format!("PEF-{:02}", i + 1)
                    && !r.sources.is_empty()
                    && !r.canonical_markdown.is_empty()
                    && !r.translation_markdown.is_empty()
                    && matches!(r.check_kind.as_str(), "mixed" | "machine" | "professional")
            }),
        "study_method_requirement_inventory"
    );
    let methods: Vec<Method> = serde_json::from_value(bindings["methods"].clone())?;
    ensure!(
        methods.iter().all(|m| {
            !m.name.is_empty()
                && std::path::Path::new(&m.source_member)
                    .extension()
                    .is_some_and(|extension| extension == "xml")
                && m.source_bytes > 0
                && m.factor_count > 0
                && uuid::Uuid::parse_str(&m.filename_id).is_ok()
                && !m.unit_label.is_empty()
                && !m.source_unit_label.is_empty()
                && m.intervention_locations
                    .iter()
                    .chain(&m.impact_locations)
                    .all(|v| v.len() <= 256)
        }),
        "study_method_source_metadata"
    );
    let ids = methods.iter().map(|m| &m.id).collect::<BTreeSet<_>>();
    ensure!(
        methods.len() == 25 && ids.len() == 25,
        "study_method_category_inventory"
    );
    let primary = methods
        .iter()
        .filter(|m| m.role == "primary")
        .collect::<Vec<_>>();
    ensure!(
        primary.len() == 16
            && primary.iter().all(|m| m.parent_id.is_none())
            && methods.iter().filter(|m| m.role == "subcategory").count() == 9
            && methods
                .iter()
                .filter(|m| m.role == "subcategory")
                .all(|m| primary.iter().any(|p| Some(&p.id) == m.parent_id.as_ref())),
        "study_method_category_roles"
    );
    let normalisation: Vec<NwRecord> =
        serde_json::from_value(bindings["normalisation_weighting"].clone())?;
    ensure!(
        normalisation.len() == 16
            && normalisation
                .iter()
                .map(|n| &n.method_id)
                .collect::<BTreeSet<_>>()
                .len()
                == 16
            && normalisation
                .iter()
                .all(|n| primary.iter().any(|m| m.id == n.method_id
                    && m.version == n.method_version
                    && m.reference_quantity_id == n.reference_quantity_id
                    && m.unit_label == n.unit_label)),
        "study_method_normalisation_mapping"
    );
    let package_source = bindings["sources"]
        .as_array()
        .context("study_method_sources")?
        .iter()
        .find(|s| s["id"] == "jrc-ef31-package")
        .context("study_method_factor_package")?;
    Ok(Module {
        reference: reference(),
        requirements,
        methods,
        normalisation,
        normalisation_source_sha256: text(&bindings["normalisation_weighting_source"], "sha256")?,
        package_source_sha256: text(package_source, "sha256")?,
        normative_version: text(&bindings, "normative_version")?,
        factor_package_version: text(&bindings, "factor_package_version")?,
        interim_guidance: bindings["interim_guidance"].clone(),
    })
}
