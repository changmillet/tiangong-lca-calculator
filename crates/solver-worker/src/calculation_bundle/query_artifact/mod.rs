//! Immutable numeric query projections of completed Calculation Bundles.
//! This module never invokes a solver or reads mutable authoring data.
mod builder;
mod reader;
mod source;
pub mod transport;
pub use builder::{BuiltQueryArtifact, build};
pub use reader::{InventoryValue, LciPage, QueryReader};

use super::{CalculationBundleArtifactRef, ReleaseImpact, ReleaseProcessRecord};
use crate::compiled_graph::CompiledExchangeDirection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

pub const FORMAT: &str = "tiangong.query-artifact.v1";
pub const MANIFEST_NAME: &str = "manifest.json";
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const MAX_INDEX_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_AXIS_ENTRIES: usize = 2_000_000;
pub const MAX_IMPACTS: usize = 4096;
pub const MAX_ROW_VALUES: usize = 2_000_000;
pub const MAX_PAGE_SIZE: usize = 1000;

/// Explicit admission limits, independent of solver admission.
#[derive(Debug, Clone)]
pub struct BuildLimits {
    pub max_file_bytes: u64,
    pub max_plain_file_bytes: u64,
    pub max_total_plain_bytes: u64,
    pub max_output_bytes: u64,
}
impl Default for BuildLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 * 1024 * 1024 * 1024,
            max_plain_file_bytes: 8 * 1024 * 1024 * 1024,
            max_total_plain_bytes: 64 * 1024 * 1024 * 1024,
            max_output_bytes: 32 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryFile {
    pub name: String,
    pub sha256: String,
    pub byte_size: u64,
    pub plain_byte_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryManifest {
    pub schema_version: String,
    pub artifact_id: Uuid,
    pub source: CalculationBundleArtifactRef,
    pub process_count: usize,
    pub impact_count: usize,
    pub inventory_count: usize,
    pub lci_nonzero_count: u64,
    pub files: Vec<QueryFile>,
}

/// Receipt is bound to an immutable manifest, never an arbitrary download URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryArtifactRef {
    pub schema_version: String,
    pub artifact_id: Uuid,
    pub manifest_sha256: String,
    pub manifest_byte_size: u64,
}
impl QueryArtifactRef {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.schema_version == FORMAT, "unsupported query format");
        super::validate_sha256(&self.manifest_sha256, "query manifest hash")?;
        anyhow::ensure!(
            self.manifest_byte_size > 0 && self.manifest_byte_size <= MAX_MANIFEST_BYTES,
            "query manifest size exceeds limit"
        );
        Ok(())
    }
    #[must_use]
    pub fn relative_prefix(&self) -> String {
        format!(
            "query-artifacts/{}/{}",
            self.artifact_id, self.manifest_sha256
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessEntry {
    pub ordinal: usize,
    pub id: Uuid,
    pub version: String,
    pub quantitative_reference: Value,
}
impl TryFrom<ReleaseProcessRecord> for ProcessEntry {
    type Error = anyhow::Error;
    fn try_from(record: ReleaseProcessRecord) -> anyhow::Result<Self> {
        super::validate_version(&record.root_process.version, "source version")?;
        super::validate_version(
            &record.quantitative_reference.flow.version,
            "reference flow version",
        )?;
        super::ensure_finite_nonzero(
            record.quantitative_reference.mean_amount,
            "reference amount",
        )?;
        super::require_nonempty(
            &record.quantitative_reference.reference_unit,
            "reference unit",
        )?;
        Ok(Self {
            ordinal: record.process_index,
            id: record.root_process.id,
            version: record.root_process.version,
            quantitative_reference: serde_json::to_value(record.quantitative_reference)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactEntry {
    pub ordinal: usize,
    pub id: Uuid,
    pub version: String,
    pub key: String,
    pub name: String,
    pub unit: String,
}
impl TryFrom<ReleaseImpact> for ImpactEntry {
    type Error = anyhow::Error;
    fn try_from(record: ReleaseImpact) -> anyhow::Result<Self> {
        super::validate_version(&record.version, "impact version")?;
        super::require_nonempty(&record.name, "impact name")?;
        super::require_nonempty(&record.unit, "impact unit")?;
        Ok(Self {
            ordinal: record.index,
            id: record.id,
            version: record.version,
            key: record.key,
            name: record.name,
            unit: record.unit,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryKey {
    pub flow_id: Uuid,
    pub flow_version: String,
    pub direction: CompiledExchangeDirection,
    pub unit: String,
    pub location: Option<String>,
}
impl InventoryKey {
    fn validate(&self) -> anyhow::Result<()> {
        super::validate_version(&self.flow_version, "inventory flow version")?;
        super::require_nonempty(&self.unit, "inventory unit")?;
        Ok(())
    }
}
impl From<&super::OwnedInventoryRecord> for InventoryKey {
    fn from(record: &super::OwnedInventoryRecord) -> Self {
        Self {
            flow_id: record.flow.id,
            flow_version: record.flow.version.clone(),
            direction: record.direction,
            unit: record.unit.clone(),
            location: record.location.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryEntry {
    pub ordinal: usize,
    pub key: InventoryKey,
    /// Frozen TIDAS name object (including localized fields); never fetched live.
    pub name: Value,
    pub classification: Value,
}

fn verified_file(root: &Path, file: &QueryFile) -> anyhow::Result<std::path::PathBuf> {
    let path = source::safe_file(root, &file.name)?;
    anyhow::ensure!(
        std::fs::metadata(&path)?.len() == file.byte_size,
        "query file size mismatch"
    );
    super::validate_sha256(&file.sha256, "query file hash")?;
    anyhow::ensure!(
        super::sha256_file(&path)? == file.sha256,
        "query file hash mismatch"
    );
    Ok(path)
}

#[cfg(test)]
pub(crate) mod tests;
