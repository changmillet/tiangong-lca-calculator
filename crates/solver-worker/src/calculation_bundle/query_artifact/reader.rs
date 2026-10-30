use super::{
    FORMAT, ImpactEntry, InventoryEntry, MANIFEST_NAME, MAX_AXIS_ENTRIES, MAX_IMPACTS,
    MAX_INDEX_BYTES, MAX_PAGE_SIZE, MAX_ROW_VALUES, ProcessEntry, QueryArtifactRef, QueryManifest,
    source, verified_file,
};
use flate2::read::GzDecoder;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};
use uuid::Uuid;

pub struct QueryReader {
    pub manifest: QueryManifest,
    processes: Vec<ProcessEntry>,
    impacts: Vec<ImpactEntry>,
    inventory: Vec<InventoryEntry>,
    process_lookup: BTreeMap<(Uuid, String), usize>,
    lcia: Option<hdf5::Dataset>,
    indices: hdf5::Dataset,
    values: hdf5::Dataset,
    offsets: Vec<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryValue {
    pub item: InventoryEntry,
    pub amount: f64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LciPage {
    pub items: Vec<InventoryValue>,
    pub next_offset: Option<usize>,
    pub nonzero_count: u64,
}

impl QueryReader {
    pub fn open(root: &Path, reference: &QueryArtifactRef) -> anyhow::Result<Self> {
        reference.validate()?;
        let path = source::safe_file(root, MANIFEST_NAME)?;
        anyhow::ensure!(
            std::fs::metadata(&path)?.len() == reference.manifest_byte_size
                && super::super::sha256_file(&path)? == reference.manifest_sha256,
            "query manifest integrity mismatch"
        );
        let manifest: QueryManifest = serde_json::from_reader(File::open(path)?)?;
        validate_manifest(&manifest, reference)?;
        for file in &manifest.files {
            verified_file(root, file)?;
        }
        let processes: Vec<ProcessEntry> = read_index(root, &manifest, "processes.json.gz")?;
        let impacts: Vec<ImpactEntry> = if manifest.impact_count == 0 {
            Vec::new()
        } else {
            read_index(root, &manifest, "impacts.json.gz")?
        };
        let inventory: Vec<InventoryEntry> =
            read_index(root, &manifest, "inventory-items.json.gz")?;
        let process_lookup = validate_axes(&manifest, &processes, &impacts, &inventory)?;
        let lcia = if manifest.impact_count == 0 {
            None
        } else {
            let ds = hdf5::File::open(root.join("lcia.h5"))?.dataset("values")?;
            anyhow::ensure!(
                ds.shape() == [manifest.process_count, manifest.impact_count]
                    && ds.dtype()?.is::<f64>(),
                "LCIA shape/type mismatch"
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
                && indptr.shape() == [manifest.process_count + 1]
                && indices.dtype()?.is::<u64>()
                && values.dtype()?.is::<f64>()
                && indices.shape() == [nonzeros]
                && values.shape() == [nonzeros],
            "LCI shape/type mismatch"
        );
        let offsets = indptr.read_raw::<u64>()?;
        anyhow::ensure!(
            offsets.first() == Some(&0)
                && offsets.last() == Some(&manifest.lci_nonzero_count)
                && offsets
                    .windows(2)
                    .all(|pair| pair[1] >= pair[0] && pair[1] - pair[0] <= MAX_ROW_VALUES as u64),
            "invalid CSR row offsets"
        );
        Ok(Self {
            manifest,
            processes,
            impacts,
            inventory,
            process_lookup,
            lcia,
            indices,
            values,
            offsets,
        })
    }

    pub fn process(&self, id: Uuid, version: &str) -> anyhow::Result<&ProcessEntry> {
        let row = self
            .process_lookup
            .get(&(id, version.to_owned()))
            .ok_or_else(|| anyhow::anyhow!("source version absent from query artifact"))?;
        Ok(&self.processes[*row])
    }

    /// Callers authorize the exact publication membership before invoking this reader.
    pub fn lcia(&self, id: Uuid, version: &str) -> anyhow::Result<Vec<(ImpactEntry, f64)>> {
        let row = self.process(id, version)?.ordinal;
        let matrix = self
            .lcia
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("LCIA not calculated"))?;
        let values = matrix.read_slice_1d::<f64, _>((row, ..))?;
        anyhow::ensure!(
            values.iter().all(|value| value.is_finite()),
            "invalid LCIA value"
        );
        let bytes = self
            .impacts
            .iter()
            .try_fold(0_usize, |sum, impact| -> anyhow::Result<usize> {
                Ok(sum + serde_json::to_vec(impact)?.len())
            })?;
        anyhow::ensure!(bytes <= 1024 * 1024, "LCIA response metadata exceeds limit");
        Ok(self
            .impacts
            .iter()
            .cloned()
            .zip(values.iter().copied())
            .collect())
    }

    /// Stable CSR order. Offset is internal and must be bound into an authorized cursor.
    pub fn lci(
        &self,
        id: Uuid,
        version: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<LciPage> {
        anyhow::ensure!(
            (1..=MAX_PAGE_SIZE).contains(&limit),
            "LCI page size exceeds limit"
        );
        let row = self.process(id, version)?.ordinal;
        let start = usize::try_from(self.offsets[row])?;
        let end = usize::try_from(self.offsets[row + 1])?;
        anyhow::ensure!(offset <= end - start, "LCI offset outside row");
        let begin = start + offset;
        let stop = begin.saturating_add(limit).min(end);
        let columns = self.indices.read_slice_1d::<u64, _>(begin..stop)?;
        let numbers = self.values.read_slice_1d::<f64, _>(begin..stop)?;
        anyhow::ensure!(
            columns
                .iter()
                .zip(columns.iter().skip(1))
                .all(|(a, b)| a < b),
            "invalid CSR column order"
        );
        let mut items = Vec::with_capacity(stop - begin);
        let mut metadata_bytes = 0;
        for (column, amount) in columns.iter().zip(&numbers) {
            anyhow::ensure!(amount.is_finite() && *amount != 0.0, "invalid LCI value");
            let item = self
                .inventory
                .get(usize::try_from(*column)?)
                .ok_or_else(|| anyhow::anyhow!("CSR column outside axis"))?;
            metadata_bytes += serde_json::to_vec(item)?.len();
            anyhow::ensure!(
                metadata_bytes <= 1024 * 1024,
                "LCI response metadata exceeds limit"
            );
            items.push(InventoryValue {
                item: item.clone(),
                amount: *amount,
            });
        }
        Ok(LciPage {
            items,
            next_offset: (stop < end).then_some(stop - start),
            nonzero_count: (end - start) as u64,
        })
    }

    /// Bounded full readback used once by the builder, not on every cache hit.
    pub fn validate_values(&self) -> anyhow::Result<()> {
        for process in &self.processes {
            if self.lcia.is_some() {
                self.lcia(process.id, &process.version)?;
            }
            let mut offset = 0;
            let mut previous = None;
            loop {
                let page = self.lci(process.id, &process.version, offset, MAX_PAGE_SIZE)?;
                for item in &page.items {
                    anyhow::ensure!(
                        previous.is_none_or(|ordinal| ordinal < item.item.ordinal),
                        "invalid cross-page CSR order"
                    );
                    previous = Some(item.item.ordinal);
                }
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

pub(super) fn validate_manifest(
    manifest: &QueryManifest,
    reference: &QueryArtifactRef,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        manifest.schema_version == FORMAT
            && manifest.artifact_id == reference.artifact_id
            && (1..=MAX_AXIS_ENTRIES).contains(&manifest.process_count)
            && manifest.impact_count <= MAX_IMPACTS
            && manifest.inventory_count <= MAX_AXIS_ENTRIES,
        "query manifest identity or axis limit mismatch"
    );
    let mut required = BTreeSet::from(["processes.json.gz", "inventory-items.json.gz", "lci.h5"]);
    if manifest.impact_count > 0 {
        required.extend(["impacts.json.gz", "lcia.h5"]);
    }
    for file in &manifest.files {
        anyhow::ensure!(
            required.remove(file.name.as_str()) && file.byte_size > 0,
            "unexpected, duplicate or empty query file"
        );
        super::super::validate_sha256(&file.sha256, "query file hash")?;
        if file.name.ends_with(".json.gz") {
            anyhow::ensure!(
                file.plain_byte_size
                    .is_some_and(|bytes| bytes <= MAX_INDEX_BYTES)
                    && file.byte_size <= MAX_INDEX_BYTES,
                "index exceeds budget"
            );
        } else {
            anyhow::ensure!(file.plain_byte_size.is_none(), "unexpected HDF5 plain size");
        }
    }
    anyhow::ensure!(required.is_empty(), "query file set incomplete");
    Ok(())
}

fn read_index<T: DeserializeOwned>(
    root: &Path,
    manifest: &QueryManifest,
    name: &str,
) -> anyhow::Result<T> {
    let descriptor = manifest
        .files
        .iter()
        .find(|file| file.name == name)
        .ok_or_else(|| anyhow::anyhow!("missing index"))?;
    let expected = descriptor
        .plain_byte_size
        .ok_or_else(|| anyhow::anyhow!("missing index plain size"))?;
    let mut bytes = Vec::new();
    GzDecoder::new(File::open(root.join(name))?)
        .take(expected + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 == expected, "index plain size mismatch");
    Ok(serde_json::from_slice(&bytes)?)
}

type ProcessLookup = BTreeMap<(Uuid, String), usize>;
fn validate_axes(
    manifest: &QueryManifest,
    processes: &[ProcessEntry],
    impacts: &[ImpactEntry],
    inventory: &[InventoryEntry],
) -> anyhow::Result<ProcessLookup> {
    anyhow::ensure!(
        processes.len() == manifest.process_count
            && impacts.len() == manifest.impact_count
            && inventory.len() == manifest.inventory_count,
        "query axis length mismatch"
    );
    let mut lookup = BTreeMap::new();
    for (ordinal, process) in processes.iter().enumerate() {
        super::super::validate_version(&process.version, "process version")?;
        anyhow::ensure!(
            process.ordinal == ordinal
                && lookup
                    .insert((process.id, process.version.clone()), ordinal)
                    .is_none(),
            "query process axis mismatch"
        );
    }
    let mut impact_ids = BTreeSet::new();
    for (ordinal, impact) in impacts.iter().enumerate() {
        super::super::validate_version(&impact.version, "impact version")?;
        anyhow::ensure!(
            impact.ordinal == ordinal && impact_ids.insert((impact.id, &impact.version)),
            "query impact axis mismatch"
        );
    }
    let mut previous = None;
    for (ordinal, item) in inventory.iter().enumerate() {
        item.key.validate()?;
        anyhow::ensure!(
            item.ordinal == ordinal && previous.is_none_or(|key| key < &item.key),
            "query inventory axis mismatch"
        );
        previous = Some(&item.key);
    }
    Ok(lookup)
}
