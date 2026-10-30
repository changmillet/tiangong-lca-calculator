use super::super::{
    CalculationBundleArtifactRef, LciaRecord, OwnedInventoryRecord, ReleaseImpact,
    ReleaseProcessRecord, sha256_file,
};
use super::{
    BuildLimits, FORMAT, ImpactEntry, InventoryEntry, InventoryKey, MANIFEST_NAME,
    MAX_AXIS_ENTRIES, MAX_INDEX_BYTES, MAX_ROW_VALUES, ProcessEntry, QueryArtifactRef, QueryFile,
    QueryManifest,
    source::{Source, in_range, visit},
};
use flate2::{Compression, GzBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::Path,
};
use uuid::Uuid;

pub struct BuiltQueryArtifact {
    directory: tempfile::TempDir,
    pub manifest: QueryManifest,
    pub reference: QueryArtifactRef,
}
impl BuiltQueryArtifact {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    /// Create a new destination exclusively; manifest is always copied last.
    pub fn persist(&self, destination: &Path) -> anyhow::Result<()> {
        std::fs::create_dir(destination)?;
        let result = (|| {
            for file in &self.manifest.files {
                std::fs::copy(self.path().join(&file.name), destination.join(&file.name))?;
            }
            serde_json::to_writer(
                File::create(destination.join("artifact-ref.json"))?,
                &self.reference,
            )?;
            std::fs::copy(
                self.path().join(MANIFEST_NAME),
                destination.join(MANIFEST_NAME),
            )?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(destination);
        }
        result
    }
}

/// Build a separate query projection from a local canonical bundle and its trusted receipt.
/// Input files are hash verified and decompressed under explicit disk/record limits.
pub fn build(
    root: &Path,
    expected: &CalculationBundleArtifactRef,
    artifact_id: Uuid,
    limits: &BuildLimits,
) -> anyhow::Result<BuiltQueryArtifact> {
    let source = Source::load(root, expected, limits)?;
    let processes = processes(&source)?;
    let impacts = impacts(&source)?;
    let inventory = inventory(&source)?;
    anyhow::ensure!(
        (processes.len() as u64)
            .checked_mul(impacts.len() as u64)
            .and_then(|count| count.checked_mul(8))
            .is_some_and(|bytes| bytes <= limits.max_output_bytes),
        "dense LCIA exceeds output budget"
    );
    let directory = tempfile::tempdir()?;
    let mut files = vec![
        write_index(directory.path(), "processes.json.gz", &processes)?,
        write_index(directory.path(), "inventory-items.json.gz", &inventory)?,
    ];
    if !impacts.is_empty() {
        files.push(write_index(directory.path(), "impacts.json.gz", &impacts)?);
        write_lcia(directory.path(), &source, &impacts)?;
        files.push(describe(directory.path(), "lcia.h5", None)?);
    }
    let nonzeros = write_lci(directory.path(), &source, &inventory, limits)?;
    files.push(describe(directory.path(), "lci.h5", None)?);
    files.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    let total = files.iter().try_fold(0_u64, |sum, file| {
        sum.checked_add(file.byte_size)
            .ok_or_else(|| anyhow::anyhow!("query output size overflow"))
    })?;
    anyhow::ensure!(
        total <= limits.max_output_bytes,
        "query output exceeds budget"
    );
    let manifest = QueryManifest {
        schema_version: FORMAT.to_owned(),
        artifact_id,
        source: expected.clone(),
        process_count: processes.len(),
        impact_count: impacts.len(),
        inventory_count: inventory.len(),
        lci_nonzero_count: nonzeros,
        files,
    };
    let manifest_path = directory.path().join(MANIFEST_NAME);
    serde_json::to_writer(File::create(&manifest_path)?, &manifest)?;
    let reference = QueryArtifactRef {
        schema_version: FORMAT.to_owned(),
        artifact_id,
        manifest_sha256: sha256_file(&manifest_path)?,
        manifest_byte_size: std::fs::metadata(&manifest_path)?.len(),
    };
    reference.validate()?;
    // Read-back validates the entire structure; no publication receipt escapes on failure.
    super::QueryReader::open(directory.path(), &reference)?.validate_values()?;
    Ok(BuiltQueryArtifact {
        directory,
        manifest,
        reference,
    })
}

fn charge<T: Serialize>(value: &T, bytes: &mut usize) -> anyhow::Result<()> {
    *bytes = bytes
        .checked_add(serde_json::to_vec(value)?.len())
        .ok_or_else(|| anyhow::anyhow!("axis size overflow"))?;
    anyhow::ensure!(
        *bytes as u64 <= MAX_INDEX_BYTES,
        "axis metadata exceeds budget"
    );
    Ok(())
}

fn processes(source: &Source) -> anyhow::Result<Vec<ProcessEntry>> {
    let mut result = Vec::new();
    let mut identities = BTreeSet::new();
    let mut bytes = 0;
    for file in source.of_kind("process_axis") {
        visit::<ReleaseProcessRecord>(file, |record| {
            in_range(&file.0, record.process_index)?;
            let entry = ProcessEntry::try_from(record)?;
            anyhow::ensure!(
                entry.ordinal == result.len()
                    && identities.insert((entry.id, entry.version.clone())),
                "process axis order or identity mismatch"
            );
            charge(&entry, &mut bytes)?;
            result.push(entry);
            Ok(())
        })?;
    }
    anyhow::ensure!(
        result.len() == source.manifest.snapshot.process_count,
        "process axis count mismatch"
    );
    Ok(result)
}

fn impacts(source: &Source) -> anyhow::Result<Vec<ImpactEntry>> {
    let mut result = Vec::new();
    let mut ids = BTreeSet::new();
    let mut bytes = 0;
    for file in source.of_kind("method_axis") {
        visit::<ReleaseImpact>(file, |record| {
            let entry = ImpactEntry::try_from(record)?;
            anyhow::ensure!(
                entry.ordinal == result.len()
                    && result.len() < super::MAX_IMPACTS
                    && ids.insert((entry.id, entry.version.clone())),
                "impact axis mismatch"
            );
            charge(&entry, &mut bytes)?;
            result.push(entry);
            Ok(())
        })?;
    }
    anyhow::ensure!(
        result.len() == source.manifest.snapshot.impact_count,
        "missing frozen impact axis"
    );
    Ok(result)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrozenSource {
    dataset_type: String,
    uuid: Uuid,
    version: String,
    sha256: String,
    document: Value,
}

fn inventory(source: &Source) -> anyhow::Result<Vec<InventoryEntry>> {
    let mut keys = BTreeSet::new();
    let mut bytes = 0;
    for file in source.of_kind("biosphere_edges") {
        visit::<OwnedInventoryRecord>(file, |record| {
            in_range(&file.0, record.process_index)?;
            let key = InventoryKey::from(&record);
            key.validate()?;
            anyhow::ensure!(record.mean_amount.is_finite(), "nonfinite biosphere value");
            if !keys.contains(&key) {
                charge(&key, &mut bytes)?;
                keys.insert(key);
            }
            anyhow::ensure!(
                keys.len() <= MAX_AXIS_ENTRIES,
                "inventory axis exceeds budget"
            );
            Ok(())
        })?;
    }
    let identities = keys
        .iter()
        .map(|key| (key.flow_id, key.flow_version.clone()))
        .collect::<BTreeSet<_>>();
    let mut names = BTreeMap::new();
    for file in source.of_kind("source_closure") {
        visit::<FrozenSource>(file, |record| {
            let id = (record.uuid, record.version);
            if record.dataset_type != "flow" || !identities.contains(&id) {
                return Ok(());
            }
            anyhow::ensure!(
                super::super::canonical_sha256(&record.document)? == record.sha256,
                "frozen flow content hash mismatch"
            );
            let info = record
                .document
                .pointer("/flowDataSet/flowInformation/dataSetInformation")
                .ok_or_else(|| anyhow::anyhow!("frozen flow has no dataset information"))?;
            let name = info.get("name").cloned().unwrap_or(Value::Null);
            let classification = info
                .get("classificationInformation")
                .cloned()
                .unwrap_or(Value::Null);
            charge(&(&name, &classification), &mut bytes)?;
            anyhow::ensure!(
                names.insert(id, (name, classification)).is_none(),
                "duplicate frozen flow identity"
            );
            Ok(())
        })?;
    }
    keys.into_iter()
        .enumerate()
        .map(|(ordinal, key)| {
            let (name, classification) = names
                .get(&(key.flow_id, key.flow_version.clone()))
                .ok_or_else(|| anyhow::anyhow!("inventory flow has no frozen source metadata"))?;
            Ok(InventoryEntry {
                ordinal,
                key,
                name: name.clone(),
                classification: classification.clone(),
            })
        })
        .collect()
}

fn write_lcia(root: &Path, source: &Source, impacts: &[ImpactEntry]) -> anyhow::Result<()> {
    let file = hdf5::File::create(root.join("lcia.h5"))?;
    let matrix = file
        .new_dataset::<f64>()
        .shape((source.manifest.snapshot.process_count, impacts.len()))
        .chunk((1, impacts.len()))
        .deflate(3)
        .obj_track_times(false)
        .create("values")?;
    let mut row = Vec::with_capacity(impacts.len());
    let mut next_process = 0;
    for shard in source.of_kind("lcia") {
        visit::<LciaRecord>(shard, |record| {
            in_range(&shard.0, record.process_index)?;
            let impact = &impacts[row.len()];
            anyhow::ensure!(
                record.process_index == next_process
                    && record.method.id == impact.id
                    && record.method.version == impact.version
                    && record.mean_amount.is_finite(),
                "LCIA row/method order or value mismatch"
            );
            row.push(record.mean_amount);
            if row.len() == impacts.len() {
                matrix.write_slice(row.as_slice(), (next_process, ..))?;
                next_process += 1;
                row.clear();
            }
            Ok(())
        })?;
        anyhow::ensure!(
            row.is_empty() && next_process.checked_sub(1) == shard.0.last_process_index,
            "LCIA chunk is incomplete"
        );
    }
    anyhow::ensure!(
        next_process == source.manifest.snapshot.process_count,
        "LCIA rows missing"
    );
    file.flush()?;
    Ok(())
}

fn write_lci(
    root: &Path,
    source: &Source,
    inventory: &[InventoryEntry],
    limits: &BuildLimits,
) -> anyhow::Result<u64> {
    let file = hdf5::File::create(root.join("lci.h5"))?;
    let indices = file
        .new_dataset::<u64>()
        .shape(0..)
        .chunk(8192)
        .deflate(3)
        .obj_track_times(false)
        .create("indices")?;
    let values = file
        .new_dataset::<f64>()
        .shape(0..)
        .chunk(8192)
        .deflate(3)
        .obj_track_times(false)
        .create("values")?;
    let lookup = inventory
        .iter()
        .map(|entry| (&entry.key, entry.ordinal))
        .collect::<BTreeMap<_, _>>();
    let mut offsets = vec![0_u64];
    let mut columns = Vec::new();
    let mut numbers = Vec::new();
    let mut current = 0;
    let mut written = 0;
    for shard in source.of_kind("lci") {
        visit::<OwnedInventoryRecord>(shard, |record| {
            in_range(&shard.0, record.process_index)?;
            anyhow::ensure!(
                record.process_index >= current
                    && record.mean_amount.is_finite()
                    && record.mean_amount != 0.0,
                "LCI process order or finite-nonzero policy mismatch"
            );
            while current < record.process_index {
                flush_row(
                    &indices,
                    &values,
                    &mut columns,
                    &mut numbers,
                    &mut written,
                    limits,
                )?;
                offsets.push(written as u64);
                current += 1;
            }
            let key = InventoryKey::from(&record);
            let col = *lookup
                .get(&key)
                .ok_or_else(|| anyhow::anyhow!("LCI column absent from frozen biosphere axis"))?
                as u64;
            anyhow::ensure!(
                columns.last().is_none_or(|previous| *previous < col)
                    && columns.len() < MAX_ROW_VALUES,
                "duplicate/unsorted LCI column or row budget exceeded"
            );
            columns.push(col);
            numbers.push(record.mean_amount);
            Ok(())
        })?;
        // Declared, verified complete chunk coverage makes absent rows valid zero rows.
        let end = shard.0.last_process_index.expect("validated range") + 1;
        while current < end {
            flush_row(
                &indices,
                &values,
                &mut columns,
                &mut numbers,
                &mut written,
                limits,
            )?;
            offsets.push(written as u64);
            current += 1;
        }
    }
    anyhow::ensure!(
        offsets.len() == source.manifest.snapshot.process_count + 1,
        "LCI row coverage mismatch"
    );
    file.new_dataset_builder()
        .with_data(offsets.as_slice())
        .obj_track_times(false)
        .create("indptr")?;
    file.flush()?;
    Ok(written as u64)
}

fn flush_row(
    indices: &hdf5::Dataset,
    values: &hdf5::Dataset,
    columns: &mut Vec<u64>,
    numbers: &mut Vec<f64>,
    written: &mut usize,
    limits: &BuildLimits,
) -> anyhow::Result<()> {
    if !columns.is_empty() {
        let end = written
            .checked_add(columns.len())
            .ok_or_else(|| anyhow::anyhow!("LCI size overflow"))?;
        anyhow::ensure!(
            (end as u64)
                .checked_mul(16)
                .is_some_and(|bytes| bytes <= limits.max_output_bytes),
            "LCI output exceeds budget"
        );
        indices.resize(end)?;
        values.resize(end)?;
        indices.write_slice(columns.as_slice(), *written..end)?;
        values.write_slice(numbers.as_slice(), *written..end)?;
        *written = end;
        columns.clear();
        numbers.clear();
    }
    Ok(())
}

fn write_index<T: Serialize>(root: &Path, name: &str, value: &T) -> anyhow::Result<QueryFile> {
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_INDEX_BYTES,
        "serialized index exceeds budget"
    );
    let mut gzip = GzBuilder::new()
        .mtime(0)
        .write(File::create(root.join(name))?, Compression::new(6));
    std::io::Write::write_all(&mut gzip, &bytes)?;
    gzip.finish()?;
    describe(root, name, Some(bytes.len() as u64))
}

fn describe(root: &Path, name: &str, plain_byte_size: Option<u64>) -> anyhow::Result<QueryFile> {
    let path = root.join(name);
    Ok(QueryFile {
        name: name.to_owned(),
        sha256: sha256_file(&path)?,
        byte_size: std::fs::metadata(path)?.len(),
        plain_byte_size,
    })
}
