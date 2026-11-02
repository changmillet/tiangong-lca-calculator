//! Reader for Release-owned final Result artifacts. No calculation/source lookup.
use super::{
    ImpactEntry, MANIFEST_NAME, MAX_AXIS_ENTRIES, MAX_IMPACTS, MAX_INDEX_BYTES, MAX_PAGE_SIZE,
    MAX_ROW_VALUES, QueryArtifactRef, QueryFile, source, verified_file,
};
use crate::compiled_graph::CompiledExchangeDirection;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};
use uuid::Uuid;

pub const FORMAT: &str = "tiangong.query-artifact.v2";
pub const PROJECTION: &str = "tiangong.final-result-projection.v1";
pub const HASH_DOMAIN: &str = "tiangong.final-result-content.jcs.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalSource {
    pub schema_version: String,
    pub input_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalManifest {
    pub schema_version: String,
    pub artifact_id: Uuid,
    pub source: FinalSource,
    pub result_count: usize,
    pub impact_count: usize,
    pub inventory_count: usize,
    pub lci_nonzero_count: u64,
    pub files: Vec<QueryFile>,
}
impl FinalManifest {
    pub fn validate(&self, reference: &QueryArtifactRef) -> anyhow::Result<()> {
        reference.validate()?;
        anyhow::ensure!(
            self.schema_version == FORMAT
                && reference.schema_version == FORMAT
                && self.artifact_id == reference.artifact_id
                && self.source.schema_version == PROJECTION
                && (1..=MAX_AXIS_ENTRIES).contains(&self.result_count)
                && self.inventory_count <= MAX_AXIS_ENTRIES
                && self.impact_count <= MAX_IMPACTS
                && self.lci_nonzero_count <= (self.result_count as u64) * (MAX_ROW_VALUES as u64),
            "invalid final Result manifest"
        );
        super::super::validate_sha256(&self.source.input_hash, "final input hash")?;
        let mut required = BTreeSet::from(["results.json.gz", "inventory-items.json.gz", "lci.h5"]);
        if self.impact_count > 0 {
            required.extend(["impacts.json.gz", "lcia.h5"]);
        }
        for file in &self.files {
            anyhow::ensure!(
                required.remove(file.name.as_str()) && file.byte_size > 0,
                "invalid final Result file set"
            );
            super::super::validate_sha256(&file.sha256, "final file hash")?;
            if file.name.ends_with(".json.gz") {
                anyhow::ensure!(
                    file.plain_byte_size
                        .is_some_and(|size| size > 0 && size <= MAX_INDEX_BYTES)
                        && file.byte_size <= MAX_INDEX_BYTES,
                    "final index exceeds budget"
                );
            } else {
                anyhow::ensure!(file.plain_byte_size.is_none(), "unexpected HDF5 plain size");
            }
        }
        anyhow::ensure!(required.is_empty(), "incomplete final Result file set");
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResultIdentity {
    pub uuid: Uuid,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalResultEntry {
    pub ordinal: usize,
    pub uuid: Uuid,
    pub version: String,
    pub content_hash: String,
    pub hash_domain: String,
    pub canonical_content_hash: String,
    pub quantitative_reference: FinalReference,
    pub relation_kind: String,
    pub sources: Vec<ResultIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalUnit {
    pub flow_property_id: Uuid,
    pub flow_property_version: String,
    pub unit_group_id: Uuid,
    pub unit_group_version: String,
    pub internal_id: String,
    pub name: String,
}
impl FinalUnit {
    fn validate(&self) -> anyhow::Result<()> {
        super::super::validate_version(&self.flow_property_version, "unit property version")?;
        super::super::validate_version(&self.unit_group_version, "unit group version")?;
        anyhow::ensure!(
            !self.internal_id.is_empty() && !self.name.trim().is_empty(),
            "invalid final unit"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalInventoryEntry {
    pub ordinal: usize,
    pub flow_id: Uuid,
    pub flow_version: String,
    pub flow_name: String,
    pub flow_classification: Value,
    pub direction: CompiledExchangeDirection,
    pub location: Option<String>,
    pub function_type: Option<String>,
    pub unit: FinalUnit,
}
impl FinalInventoryEntry {
    fn key(&self) -> anyhow::Result<String> {
        // Match Release's JSON string key, including semantic unit identity and
        // functionType. Compare UTF-16 below, as JavaScript does, not UTF-8 bytes.
        Ok(serde_json::to_string(&serde_json::json!([
            self.flow_id,
            self.flow_version,
            self.direction,
            self.location,
            self.function_type,
            self.unit.flow_property_id,
            self.unit.flow_property_version,
            self.unit.unit_group_id,
            self.unit.unit_group_version,
            self.unit.internal_id,
            self.unit.name
        ]))?)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalReference {
    pub flow_id: Uuid,
    pub flow_version: String,
    pub flow_name: String,
    pub flow_classification: Value,
    pub direction: CompiledExchangeDirection,
    pub location: Option<String>,
    pub function_type: Option<String>,
    pub unit: FinalUnit,
    pub value: f64,
    pub amount_field: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalInventoryValue {
    pub item: FinalInventoryEntry,
    pub amount: f64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalLciPage {
    pub items: Vec<FinalInventoryValue>,
    pub next_offset: Option<usize>,
    pub nonzero_count: u64,
}

pub struct FinalQueryReader {
    pub manifest: FinalManifest,
    results: Vec<FinalResultEntry>,
    impacts: Vec<ImpactEntry>,
    inventory: Vec<FinalInventoryEntry>,
    lookup: BTreeMap<(Uuid, String), usize>,
    lcia: Option<hdf5::Dataset>,
    indices: hdf5::Dataset,
    values: hdf5::Dataset,
    offsets: Vec<u64>,
}
impl FinalQueryReader {
    pub fn open(root: &Path, reference: &QueryArtifactRef) -> anyhow::Result<Self> {
        reference.validate()?;
        let path = source::safe_file(root, MANIFEST_NAME)?;
        anyhow::ensure!(
            std::fs::metadata(&path)?.len() == reference.manifest_byte_size
                && super::super::sha256_file(&path)? == reference.manifest_sha256,
            "final manifest integrity mismatch"
        );
        let manifest: FinalManifest = serde_json::from_reader(File::open(path)?)?;
        manifest.validate(reference)?;
        for file in &manifest.files {
            verified_file(root, file)?;
        }
        let results: Vec<FinalResultEntry> = read_index(root, &manifest, "results.json.gz")?;
        let inventory: Vec<FinalInventoryEntry> =
            read_index(root, &manifest, "inventory-items.json.gz")?;
        let impacts: Vec<ImpactEntry> = if manifest.impact_count == 0 {
            vec![]
        } else {
            read_index(root, &manifest, "impacts.json.gz")?
        };
        let lookup = validate_axes(&manifest, &results, &impacts, &inventory)?;
        let lcia = if impacts.is_empty() {
            None
        } else {
            let ds = hdf5::File::open(root.join("lcia.h5"))?.dataset("values")?;
            anyhow::ensure!(
                ds.shape() == [results.len(), impacts.len()] && ds.dtype()?.is::<f64>(),
                "final LCIA shape/type mismatch"
            );
            Some(ds)
        };
        let lci = hdf5::File::open(root.join("lci.h5"))?;
        let indptr = lci.dataset("indptr")?;
        let indices = lci.dataset("indices")?;
        let values = lci.dataset("values")?;
        let nonzeros = usize::try_from(manifest.lci_nonzero_count)?;
        anyhow::ensure!(
            indptr.dtype()?.is::<u64>()
                && indptr.shape() == [results.len() + 1]
                && indices.dtype()?.is::<u64>()
                && values.dtype()?.is::<f64>()
                && indices.shape() == [nonzeros]
                && values.shape() == [nonzeros],
            "final LCI shape/type mismatch"
        );
        let offsets = indptr.read_raw::<u64>()?;
        anyhow::ensure!(
            offsets.first() == Some(&0)
                && offsets.last() == Some(&manifest.lci_nonzero_count)
                && offsets
                    .windows(2)
                    .all(|pair| pair[1] >= pair[0] && pair[1] - pair[0] <= MAX_ROW_VALUES as u64),
            "invalid final CSR offsets"
        );
        Ok(Self {
            manifest,
            results,
            impacts,
            inventory,
            lookup,
            lcia,
            indices,
            values,
            offsets,
        })
    }
    pub fn result(&self, id: Uuid, version: &str) -> anyhow::Result<&FinalResultEntry> {
        let ordinal = self
            .lookup
            .get(&(id, version.to_owned()))
            .ok_or_else(|| anyhow::anyhow!("Result version absent from query artifact"))?;
        Ok(&self.results[*ordinal])
    }
    pub fn lcia(&self, id: Uuid, version: &str) -> anyhow::Result<Vec<(ImpactEntry, f64)>> {
        let ordinal = self.result(id, version)?.ordinal;
        let ds = self
            .lcia
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("LCIA not provided"))?;
        let values = ds.read_slice_1d::<f64, _>((ordinal, ..))?;
        anyhow::ensure!(
            values.iter().all(|value| value.is_finite()),
            "invalid final LCIA value"
        );
        let metadata_bytes = self
            .impacts
            .iter()
            .try_fold(0_usize, |sum, impact| -> anyhow::Result<usize> {
                Ok(sum + serde_json::to_vec(impact)?.len())
            })?;
        anyhow::ensure!(
            metadata_bytes <= 1024 * 1024,
            "final LCIA metadata exceeds budget"
        );
        Ok(self
            .impacts
            .iter()
            .cloned()
            .zip(values.iter().copied())
            .collect())
    }
    pub fn lci(
        &self,
        id: Uuid,
        version: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<FinalLciPage> {
        anyhow::ensure!(
            (1..=MAX_PAGE_SIZE).contains(&limit),
            "final LCI page size exceeds limit"
        );
        let ordinal = self.result(id, version)?.ordinal;
        let start = usize::try_from(self.offsets[ordinal])?;
        let end = usize::try_from(self.offsets[ordinal + 1])?;
        anyhow::ensure!(offset <= end - start, "final LCI offset outside row");
        let begin = start + offset;
        let stop = begin.saturating_add(limit).min(end);
        // Include the preceding column so an ordering violation at a page
        // boundary is rejected even without a prior full-file validation pass.
        let check_start = if begin > start { begin - 1 } else { begin };
        let columns = self.indices.read_slice_1d::<u64, _>(check_start..stop)?;
        anyhow::ensure!(
            columns
                .iter()
                .zip(columns.iter().skip(1))
                .all(|(a, b)| a < b),
            "invalid final CSR column order"
        );
        let numbers = self.values.read_slice_1d::<f64, _>(begin..stop)?;
        let mut items = Vec::with_capacity(stop - begin);
        let mut bytes = 0;
        for (column, amount) in columns.iter().skip(begin - check_start).zip(numbers.iter()) {
            anyhow::ensure!(
                amount.is_finite() && *amount != 0.0,
                "invalid final LCI value"
            );
            let item = self
                .inventory
                .get(usize::try_from(*column)?)
                .ok_or_else(|| anyhow::anyhow!("final inventory ordinal outside axis"))?;
            bytes += serde_json::to_vec(item)?.len();
            anyhow::ensure!(bytes <= 1024 * 1024, "final LCI metadata exceeds budget");
            items.push(FinalInventoryValue {
                item: item.clone(),
                amount: *amount,
            });
        }
        Ok(FinalLciPage {
            items,
            next_offset: (stop < end).then_some(stop - start),
            nonzero_count: (end - start) as u64,
        })
    }
    pub fn validate_values(&self) -> anyhow::Result<()> {
        for row in &self.results {
            if self.lcia.is_some() {
                self.lcia(row.uuid, &row.version)?;
            }
            let mut offset = 0;
            loop {
                let page = self.lci(row.uuid, &row.version, offset, MAX_PAGE_SIZE)?;
                if let Some(next) = page.next_offset {
                    offset = next;
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
}
fn validate_axes(
    manifest: &FinalManifest,
    results: &[FinalResultEntry],
    impacts: &[ImpactEntry],
    inventory: &[FinalInventoryEntry],
) -> anyhow::Result<BTreeMap<(Uuid, String), usize>> {
    anyhow::ensure!(
        results.len() == manifest.result_count
            && inventory.len() == manifest.inventory_count
            && impacts.len() == manifest.impact_count,
        "final axis length mismatch"
    );
    let mut lookup = BTreeMap::new();
    for (ordinal, row) in results.iter().enumerate() {
        super::super::validate_version(&row.version, "Result version")?;
        super::super::validate_sha256(&row.content_hash, "Result content hash")?;
        super::super::validate_sha256(&row.canonical_content_hash, "Result canonical hash")?;
        super::super::validate_version(
            &row.quantitative_reference.flow_version,
            "reference flow version",
        )?;
        row.quantitative_reference.unit.validate()?;
        anyhow::ensure!(
            row.ordinal == ordinal
                && row.hash_domain == HASH_DOMAIN
                && lookup
                    .insert((row.uuid, row.version.clone()), ordinal)
                    .is_none()
                && row.quantitative_reference.value.is_finite()
                && row.quantitative_reference.value != 0.0
                && row.quantitative_reference.amount_field == "resultingAmount"
                && !row.quantitative_reference.flow_name.trim().is_empty()
                && !row.sources.is_empty()
                && matches!(row.relation_kind.as_str(), "direct" | "derived")
                && (row.relation_kind != "direct" || row.sources.len() == 1),
            "invalid final Result axis"
        );
        let mut sources = BTreeSet::new();
        for source in &row.sources {
            super::super::validate_version(&source.version, "lineage source version")?;
            anyhow::ensure!(
                sources.insert((source.uuid, &source.version)),
                "duplicate final source identity"
            );
        }
    }
    let mut previous: Option<String> = None;
    for (ordinal, item) in inventory.iter().enumerate() {
        super::super::validate_version(&item.flow_version, "inventory flow version")?;
        item.unit.validate()?;
        let key = item.key()?;
        anyhow::ensure!(
            item.ordinal == ordinal
                && !item.flow_name.trim().is_empty()
                && previous
                    .as_ref()
                    .is_none_or(|old| old.encode_utf16().cmp(key.encode_utf16()).is_lt()),
            "invalid final inventory axis"
        );
        previous = Some(key);
    }
    let mut impact_ids = BTreeSet::new();
    for (ordinal, impact) in impacts.iter().enumerate() {
        super::super::validate_version(&impact.version, "impact version")?;
        anyhow::ensure!(
            impact.ordinal == ordinal
                && impact_ids.insert((impact.id, &impact.version))
                && impact.key == format!("lciamethod:{}@{}", impact.id, impact.version)
                && !impact.name.trim().is_empty()
                && !impact.unit.trim().is_empty(),
            "invalid final impact axis"
        );
    }
    Ok(lookup)
}
fn read_index<T: DeserializeOwned>(
    root: &Path,
    manifest: &FinalManifest,
    name: &str,
) -> anyhow::Result<T> {
    let file = manifest
        .files
        .iter()
        .find(|file| file.name == name)
        .ok_or_else(|| anyhow::anyhow!("missing final index"))?;
    let expected = file
        .plain_byte_size
        .ok_or_else(|| anyhow::anyhow!("missing final index size"))?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(File::open(root.join(name))?)
        .take(expected + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 == expected,
        "final index plain size mismatch"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
#[path = "final_result_tests.rs"]
mod tests;
