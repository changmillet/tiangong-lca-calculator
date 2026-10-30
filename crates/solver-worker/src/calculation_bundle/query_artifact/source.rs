use super::super::{
    CALCULATION_BUNDLE_FORMAT, CalculationBundleArtifact, CalculationBundleArtifactRef,
    CalculationBundleManifest, bundle_content_hash, validate_sha256,
};
use super::{BuildLimits, MAX_AXIS_ENTRIES, MAX_IMPACTS, MAX_MANIFEST_BYTES, MAX_RECORD_BYTES};
use flate2::read::GzDecoder;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
};

pub(super) struct Source {
    _directory: tempfile::TempDir,
    pub manifest: CalculationBundleManifest,
    pub files: Vec<(CalculationBundleArtifact, PathBuf)>,
}

pub(super) fn safe_file(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !relative.is_empty() && !relative.contains(['\\', ':']),
        "invalid relative artifact path"
    );
    let relative = Path::new(relative);
    anyhow::ensure!(
        relative
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "invalid relative artifact path"
    );
    let mut path = root.canonicalize()?;
    for part in relative.components() {
        path.push(part);
        anyhow::ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "artifact symlinks are forbidden"
        );
    }
    anyhow::ensure!(path.is_file(), "artifact is not a regular file");
    Ok(path)
}

impl Source {
    pub fn load(
        root: &Path,
        expected: &CalculationBundleArtifactRef,
        limits: &BuildLimits,
    ) -> anyhow::Result<Self> {
        validate_sha256(&expected.manifest_sha256, "source manifest hash")?;
        validate_sha256(&expected.bundle_content_hash, "source content hash")?;
        anyhow::ensure!(
            expected.schema_version == CALCULATION_BUNDLE_FORMAT
                && expected.manifest_byte_size <= MAX_MANIFEST_BYTES,
            "invalid source manifest reference"
        );
        let path = safe_file(root, "calculation-bundle.json")?;
        anyhow::ensure!(
            std::fs::metadata(&path)?.len() == expected.manifest_byte_size
                && super::super::sha256_file(&path)? == expected.manifest_sha256,
            "source manifest integrity mismatch"
        );
        let manifest: CalculationBundleManifest = serde_json::from_reader(File::open(path)?)?;
        anyhow::ensure!(
            manifest.schema_version == CALCULATION_BUNDLE_FORMAT
                && manifest.calculation_id == expected.calculation_id
                && manifest.bundle_content_hash == expected.bundle_content_hash
                && bundle_content_hash(&manifest)? == expected.bundle_content_hash
                && manifest.artifacts.len() == expected.artifact_count,
            "source manifest binding mismatch"
        );
        anyhow::ensure!(
            manifest.scope.process_count == manifest.snapshot.process_count
                && (1..=MAX_AXIS_ENTRIES).contains(&manifest.scope.process_count)
                && manifest.snapshot.impact_count <= MAX_IMPACTS,
            "source axes exceed query admission"
        );
        let directory = tempfile::tempdir()?;
        let mut files = Vec::new();
        let mut paths = BTreeSet::new();
        let mut total = 0_u64;
        for (index, meta) in manifest.artifacts.iter().enumerate() {
            anyhow::ensure!(paths.insert(&meta.path), "duplicate source artifact path");
            if !matches!(
                meta.kind.as_str(),
                "process_axis"
                    | "method_axis"
                    | "biosphere_edges"
                    | "lci"
                    | "lcia"
                    | "source_closure"
            ) {
                continue;
            }
            validate_schema(meta)?;
            let path = safe_file(root, &meta.path)?;
            anyhow::ensure!(
                meta.byte_size <= limits.max_file_bytes
                    && std::fs::metadata(&path)?.len() == meta.byte_size
                    && super::super::sha256_file(&path)? == meta.sha256,
                "source shard integrity mismatch"
            );
            let plain_size = meta
                .uncompressed_byte_size
                .ok_or_else(|| anyhow::anyhow!("source shard has no uncompressed size"))?;
            total = total
                .checked_add(plain_size)
                .ok_or_else(|| anyhow::anyhow!("source size overflow"))?;
            anyhow::ensure!(
                plain_size <= limits.max_plain_file_bytes && total <= limits.max_total_plain_bytes,
                "source decompression budget exceeded"
            );
            let plain_hash = meta
                .uncompressed_sha256
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("source shard has no uncompressed hash"))?;
            validate_sha256(plain_hash, "source plain hash")?;
            let destination = directory.path().join(format!("{index}.ndjson"));
            decompress_verified(&path, &destination, plain_size, plain_hash)?;
            files.push((meta.clone(), destination));
        }
        let source = Self {
            _directory: directory,
            manifest,
            files,
        };
        for kind in ["process_axis", "biosphere_edges", "lci"] {
            source.ranges(kind)?;
        }
        if source.manifest.snapshot.impact_count > 0 {
            source.ranges("lcia")?;
        } else {
            anyhow::ensure!(
                source.of_kind("lcia").is_empty() && source.of_kind("method_axis").is_empty(),
                "unexpected LCIA data for absent method axis"
            );
        }
        Ok(source)
    }

    pub fn of_kind(&self, kind: &str) -> Vec<&(CalculationBundleArtifact, PathBuf)> {
        let mut result = self
            .files
            .iter()
            .filter(|(meta, _)| meta.kind == kind)
            .collect::<Vec<_>>();
        result.sort_unstable_by_key(|(meta, _)| meta.first_process_index);
        result
    }

    pub fn ranges(&self, kind: &str) -> anyhow::Result<()> {
        let mut next = 0;
        for (meta, _) in self.of_kind(kind) {
            let first = meta
                .first_process_index
                .ok_or_else(|| anyhow::anyhow!("missing source range"))?;
            let last = meta
                .last_process_index
                .ok_or_else(|| anyhow::anyhow!("missing source range"))?;
            anyhow::ensure!(
                first == next
                    && last >= first
                    && last < self.manifest.snapshot.process_count
                    && last - first < super::super::CALCULATION_BUNDLE_CHUNK_PROCESS_COUNT,
                "source chunk coverage mismatch"
            );
            next = last + 1;
        }
        anyhow::ensure!(
            next == self.manifest.snapshot.process_count,
            "incomplete source chunk coverage"
        );
        Ok(())
    }
}

fn validate_schema(meta: &CalculationBundleArtifact) -> anyhow::Result<()> {
    let suffix = match meta.kind.as_str() {
        "process_axis" if meta.schema_version == "tiangong.calculation-bundle.process-axis.v2" => {
            "process-axis.v2"
        }
        "process_axis" => "process-axis.v1",
        "method_axis" => "method-axis.v1",
        "biosphere_edges" => "biosphere-edges.v1",
        "lci" => "lci.v1",
        "lcia" => "lcia.v1",
        "source_closure" => {
            anyhow::ensure!(
                meta.schema_version == "tiangong.source-closure.bundle.v1"
                    && meta.compression == "gzip",
                "unsupported source closure schema"
            );
            return Ok(());
        }
        _ => anyhow::bail!("unsupported source kind"),
    };
    anyhow::ensure!(
        meta.schema_version == format!("tiangong.calculation-bundle.{suffix}")
            && meta.compression == "gzip",
        "unsupported source shard schema"
    );
    Ok(())
}

pub(super) fn visit<T: DeserializeOwned>(
    file: &(CalculationBundleArtifact, PathBuf),
    mut visitor: impl FnMut(T) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::new(File::open(&file.1)?);
    let mut line = Vec::new();
    let mut count = 0_u64;
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        anyhow::ensure!(
            read as u64 <= MAX_RECORD_BYTES && line.last() == Some(&b'\n'),
            "source record exceeds limit or is incomplete"
        );
        count += 1;
        anyhow::ensure!(count <= file.0.record_count, "source record count overflow");
        visitor(serde_json::from_slice(&line)?)?;
    }
    anyhow::ensure!(count == file.0.record_count, "source record count mismatch");
    Ok(())
}

pub(super) fn in_range(meta: &CalculationBundleArtifact, process: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        meta.first_process_index
            .is_some_and(|first| process >= first)
            && meta.last_process_index.is_some_and(|last| process <= last),
        "record outside source chunk range"
    );
    Ok(())
}

fn decompress_verified(
    path: &Path,
    destination: &Path,
    plain_size: u64,
    plain_hash: &str,
) -> anyhow::Result<()> {
    let mut reader = GzDecoder::new(File::open(path)?);
    let mut writer = File::create(destination)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    let mut digest = Sha256::new();
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        anyhow::ensure!(
            size <= plain_size,
            "source decompression exceeds declared size"
        );
        digest.update(&buffer[..count]);
        writer.write_all(&buffer[..count])?;
    }
    anyhow::ensure!(
        size == plain_size && hex::encode(digest.finalize()) == plain_hash,
        "source plain integrity mismatch"
    );
    Ok(())
}
