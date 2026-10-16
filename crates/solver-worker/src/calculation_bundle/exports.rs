//! Optional exports consume a completed, hash-bound bundle; they never invoke the solver.
use super::{
    CALCULATION_BUNDLE_FORMAT, CalculationBundleArtifactRef, CalculationBundleManifest,
    CalculationDownloadArtifactRef, LCI_EXPORT_HEADERS, LocalCalculationBundleArtifact,
    LocalCalculationDownloadArtifact, ObjectStoreClient, OwnedInventoryRecord, ReleaseImpact,
    ReleaseProcessRecord, artifacts_of_kind, bundle_content_hash, canonical_json_bytes, lci_row,
    local_download, require_record_count, validate_sha256, visit_gzip_ndjson, write_csv_row,
    write_lci_parquet, write_lcia_csv_zip, write_lcia_xlsx,
};
use crate::{resource::CancellationToken, storage::ObjectTransferOptions};
use flate2::{Compression, GzBuilder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const JOB_KIND: &str = "lca.calculation_export";
pub const REQUEST_SCHEMA: &str = "lca.calculation_export.request.v1";
pub const RESULT_SCHEMA: &str = "lca.calculation_export.result.v1";
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub const CSV_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const CSV_MAX_ROWS: u64 = 250_000;
const MAX_MANIFEST_BYTES: u64 = 5 * 1024 * 1024;
const MAX_PARTS: usize = 10_000;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    #[default]
    Parquet,
    Csv,
    LciaXlsx,
    LciaCsv,
    Audit,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportRequest {
    pub package_id: Uuid,
    pub bundle: CalculationBundleArtifactRef,
    #[serde(default)]
    pub format: ExportFormat,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub schema_version: &'static str,
    pub worker_job_id: Uuid,
    pub package_id: Uuid,
    pub bundle_content_hash: String,
    pub source_manifest_sha256: String,
    pub format: ExportFormat,
    pub files: Vec<CalculationDownloadArtifactRef>,
    pub manifest: CalculationDownloadArtifactRef,
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub async fn execute(
    store: &ObjectStoreClient,
    job_id: Uuid,
    request: ExportRequest,
) -> anyhow::Result<ExportResult> {
    let cancellation = CancellationToken::default();
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let PreparedSources {
        directory,
        manifest_path,
        artifacts,
        process_count,
    } = load_sources(store, &request).await?;
    let format = request.format;
    let count = process_count;
    // CPU/file conversion runs off the async executor so queue leases continue renewing.
    let (directory, generated) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let files = generate(
            directory.path(),
            &manifest_path,
            &artifacts,
            count,
            format,
            &cancellation,
        )?;
        Ok((directory, files))
    })
    .await??;
    anyhow::ensure!(
        generated.len() <= MAX_PARTS,
        "export part count exceeds limit"
    );
    let export_prefix = format!(
        "calculation-exports/{}/{job_id}",
        request.bundle.bundle_content_hash
    );
    let mut files = Vec::new();
    for file in generated {
        anyhow::ensure!(
            file.metadata.byte_size
                <= if format == ExportFormat::Csv {
                    CSV_MAX_BYTES
                } else {
                    MAX_FILE_BYTES
                },
            "export file exceeds limit"
        );
        let key =
            store.prefixed_object_key(&format!("{export_prefix}/{}", file.metadata.file_name))?;
        let artifact_url = upload_or_reuse(store, &request, &key, &file).await?;
        files.push(CalculationDownloadArtifactRef {
            metadata: file.metadata,
            artifact_url,
        });
    }
    let export_manifest_path = directory.path().join("export-manifest.json");
    let export_manifest = json!({
        "schemaVersion": RESULT_SCHEMA, "workerJobId": job_id, "packageId": request.package_id,
        "bundleContentHash": request.bundle.bundle_content_hash,
        "sourceManifestSha256": request.bundle.manifest_sha256, "format": format,
        "limits": { "csvMaxBytes": CSV_MAX_BYTES, "csvMaxRows": CSV_MAX_ROWS, "maxFileBytes": MAX_FILE_BYTES },
        "recordCount": files.iter().filter(|f| f.metadata.role != "calculation_evidence_bundle").map(|f| f.metadata.record_count).sum::<u64>(),
        "files": files.iter().map(|f| &f.metadata).collect::<Vec<_>>(),
    });
    let bytes = canonical_json_bytes(&export_manifest)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "export manifest exceeds limit"
    );
    std::fs::write(&export_manifest_path, bytes)?;
    let metadata = local_download(
        "export_manifest",
        "advanced_data",
        "export-manifest.json",
        "application/json",
        files.len() as u64,
        export_manifest_path,
    )?;
    let key = store.prefixed_object_key(&format!("{export_prefix}/export-manifest.json"))?;
    let uploaded = store
        .upload_object_key_file(
            &key,
            "application/json",
            &metadata.local_path,
            metadata.metadata.byte_size,
        )
        .await?;
    Ok(ExportResult {
        schema_version: RESULT_SCHEMA,
        worker_job_id: job_id,
        package_id: request.package_id,
        bundle_content_hash: request.bundle.bundle_content_hash,
        source_manifest_sha256: request.bundle.manifest_sha256,
        format,
        files,
        manifest: CalculationDownloadArtifactRef {
            metadata: metadata.metadata,
            artifact_url: uploaded.object_url,
        },
    })
}

async fn upload_or_reuse(
    store: &ObjectStoreClient,
    request: &ExportRequest,
    key: &str,
    file: &LocalCalculationDownloadArtifact,
) -> anyhow::Result<String> {
    if request.format == ExportFormat::Audit {
        let source_path = file
            .metadata
            .source_path
            .as_deref()
            .unwrap_or("calculation-bundle.json");
        let base = request
            .bundle
            .manifest_url
            .rsplit_once('/')
            .ok_or_else(|| anyhow::anyhow!("invalid source manifest URL"))?
            .0;
        Ok(format!("{base}/{source_path}"))
    } else {
        Ok(store
            .upload_object_key_file(
                key,
                &file.metadata.media_type,
                &file.local_path,
                file.metadata.byte_size,
            )
            .await?
            .object_url)
    }
}

struct PreparedSources {
    directory: tempfile::TempDir,
    manifest_path: PathBuf,
    artifacts: Vec<LocalCalculationBundleArtifact>,
    process_count: usize,
}

async fn load_sources(
    store: &ObjectStoreClient,
    request: &ExportRequest,
) -> anyhow::Result<PreparedSources> {
    validate_sha256(&request.bundle.bundle_content_hash, "bundleContentHash")?;
    validate_sha256(&request.bundle.manifest_sha256, "manifestSha256")?;
    anyhow::ensure!(
        request.bundle.manifest_byte_size <= MAX_MANIFEST_BYTES,
        "export source manifest exceeds limit"
    );
    let directory = tempfile::Builder::new()
        .prefix("tiangong-calculation-export-")
        .tempdir()?;
    let prefix = format!(
        "calculation-bundles/{}/{}",
        request.bundle.calculation_id, request.bundle.bundle_content_hash
    );
    let manifest_path = directory.path().join("calculation-bundle.json");
    let downloaded = store
        .download_object_key_to_file(
            &store.prefixed_object_key(&format!("{prefix}/calculation-bundle.json"))?,
            &manifest_path,
            ObjectTransferOptions::new(MAX_MANIFEST_BYTES)
                .with_expected_sha256(&request.bundle.manifest_sha256),
        )
        .await?;
    anyhow::ensure!(
        downloaded.byte_size == request.bundle.manifest_byte_size,
        "export source manifest size mismatch"
    );
    let manifest: CalculationBundleManifest = serde_json::from_reader(File::open(&manifest_path)?)?;
    anyhow::ensure!(
        manifest.schema_version == CALCULATION_BUNDLE_FORMAT
            && manifest.calculation_id == request.bundle.calculation_id
            && manifest.bundle_content_hash == request.bundle.bundle_content_hash
            && bundle_content_hash(&manifest)? == manifest.bundle_content_hash
            && manifest.artifacts.len() == request.bundle.artifact_count,
        "export source bundle binding mismatch"
    );
    let mut artifacts = Vec::new();
    let mut paths = BTreeSet::new();
    for metadata in &manifest.artifacts {
        // Never accept absolute paths, traversal, duplicate files or arbitrary object URLs.
        let path = Path::new(&metadata.path);
        anyhow::ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
                && paths.insert(metadata.path.clone()),
            "invalid export source path"
        );
        let needed = request.format == ExportFormat::Audit
            || metadata.kind == "process_axis"
            || matches!(request.format, ExportFormat::Parquet | ExportFormat::Csv)
                && metadata.kind == "lci"
            || matches!(
                request.format,
                ExportFormat::LciaCsv | ExportFormat::LciaXlsx
            ) && matches!(metadata.kind.as_str(), "method_axis" | "lcia");
        if !needed {
            continue;
        }
        anyhow::ensure!(
            metadata.byte_size <= MAX_FILE_BYTES,
            "export source shard exceeds limit"
        );
        let local_path = directory.path().join(path);
        std::fs::create_dir_all(
            local_path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("source path has no parent"))?,
        )?;
        let observed = store
            .download_object_key_to_file(
                &store.prefixed_object_key(&format!("{prefix}/{}", metadata.path))?,
                &local_path,
                ObjectTransferOptions::new(metadata.byte_size)
                    .with_expected_sha256(&metadata.sha256),
            )
            .await?;
        anyhow::ensure!(
            observed.byte_size == metadata.byte_size,
            "export source shard size mismatch"
        );
        artifacts.push(LocalCalculationBundleArtifact {
            metadata: metadata.clone(),
            local_path,
        });
    }
    Ok(PreparedSources {
        directory,
        manifest_path,
        artifacts,
        process_count: manifest.snapshot.process_count,
    })
}

pub(super) fn load_processes(
    artifacts: &[LocalCalculationBundleArtifact],
    count: usize,
) -> anyhow::Result<Vec<ReleaseProcessRecord>> {
    let mut processes = Vec::new();
    for artifact in artifacts_of_kind(artifacts, "process_axis") {
        let observed =
            visit_gzip_ndjson::<ReleaseProcessRecord, _>(&artifact.local_path, |record| {
                processes.push(record);
                Ok(())
            })?;
        require_record_count(artifact, observed)?;
    }
    processes.sort_unstable_by_key(|p| p.process_index);
    anyhow::ensure!(
        processes.len() == count
            && processes
                .iter()
                .enumerate()
                .all(|(i, p)| p.process_index == i),
        "export process axis mismatch"
    );
    Ok(processes)
}

pub(super) fn load_impacts(
    artifacts: &[LocalCalculationBundleArtifact],
) -> anyhow::Result<Vec<ReleaseImpact>> {
    let mut impacts = Vec::new();
    for artifact in artifacts_of_kind(artifacts, "method_axis") {
        let observed = visit_gzip_ndjson::<ReleaseImpact, _>(&artifact.local_path, |record| {
            impacts.push(record);
            Ok(())
        })?;
        require_record_count(artifact, observed)?;
    }
    anyhow::ensure!(
        !impacts.is_empty(),
        "legacy bundle has no frozen method axis; use its existing LCIA downloads"
    );
    Ok(impacts)
}

fn generate(
    directory: &Path,
    manifest_path: &Path,
    artifacts: &[LocalCalculationBundleArtifact],
    process_count: usize,
    format: ExportFormat,
    cancellation: &CancellationToken,
) -> anyhow::Result<Vec<LocalCalculationDownloadArtifact>> {
    cancellation.check("calculation_export")?;
    let processes = load_processes(artifacts, process_count)?;
    let lci = artifacts_of_kind(artifacts, "lci");
    match format {
        ExportFormat::Csv => write_bounded_lci_csv(
            directory,
            &lci,
            &processes,
            CSV_MAX_BYTES,
            CSV_MAX_ROWS,
            cancellation,
        ),
        ExportFormat::Parquet => {
            let mut files = Vec::new();
            for (index, artifact) in lci.iter().enumerate() {
                cancellation.check("calculation_export_parquet_shard")?;
                let name = format!("lci-inventory-{index:06}.parquet");
                let path = directory.join(&name);
                let count = write_lci_parquet(&path, &[*artifact], &processes)?;
                files.push(local_download(
                    "lci_inventory_parquet",
                    "advanced_data",
                    &name,
                    "application/vnd.apache.parquet",
                    count,
                    path,
                )?);
            }
            Ok(files)
        }
        ExportFormat::LciaXlsx | ExportFormat::LciaCsv => {
            let impacts = load_impacts(artifacts)?;
            let lcia = artifacts_of_kind(artifacts, "lcia");
            let (role, name, media) = if format == ExportFormat::LciaXlsx {
                (
                    "lcia_results_xlsx",
                    "lcia-results.xlsx",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                )
            } else {
                (
                    "lcia_results_csv_zip",
                    "lcia-results.csv.zip",
                    "application/zip",
                )
            };
            let path = directory.join(name);
            let count = if format == ExportFormat::LciaXlsx {
                write_lcia_xlsx(&path, &lcia, &processes, &impacts)?
            } else {
                write_lcia_csv_zip(&path, &lcia, &processes, &impacts)?
            };
            Ok(vec![local_download(
                role, "results", name, media, count, path,
            )?])
        }
        ExportFormat::Audit => {
            // Each canonical shard is already independently compressed and hash-bound.
            // Return separate files plus the source manifest; never re-aggregate a giant ZIP.
            let mut files = vec![local_download(
                "calculation_evidence_bundle",
                "audit_evidence",
                "calculation-bundle.json",
                "application/json",
                1,
                manifest_path.to_owned(),
            )?];
            for (i, artifact) in artifacts.iter().enumerate() {
                let name = format!("evidence-{i:06}.ndjson.gz");
                files.push(local_download(
                    "calculation_evidence_bundle",
                    "audit_evidence",
                    &name,
                    "application/gzip",
                    artifact.metadata.record_count,
                    artifact.local_path.clone(),
                )?);
                files.last_mut().expect("audit file").metadata.source_path =
                    Some(artifact.metadata.path.clone());
            }
            Ok(files)
        }
    }
}

fn write_bounded_lci_csv(
    directory: &Path,
    artifacts: &[&LocalCalculationBundleArtifact],
    processes: &[ReleaseProcessRecord],
    max_bytes: u64,
    max_rows: u64,
    cancellation: &CancellationToken,
) -> anyhow::Result<Vec<LocalCalculationDownloadArtifact>> {
    let mut header = Vec::new();
    write_csv_row(&mut header, LCI_EXPORT_HEADERS)?;
    anyhow::ensure!(
        max_bytes > header.len() as u64 && max_rows > 0,
        "invalid CSV limits"
    );
    let mut files = Vec::new();
    let mut part: Option<(GzEncoder<File>, PathBuf, String, u64, u64)> = None;
    for artifact in artifacts {
        let observed =
            visit_gzip_ndjson::<OwnedInventoryRecord, _>(&artifact.local_path, |record| {
                cancellation.check("calculation_export_csv_row")?;
                let mut row = Vec::new();
                write_csv_row(&mut row, lci_row(&record, processes)?)?;
                let row_bytes = row.len() as u64;
                anyhow::ensure!(
                    header.len() as u64 + row_bytes <= max_bytes,
                    "CSV row exceeds byte limit"
                );
                if part.as_ref().is_some_and(|(_, _, _, bytes, rows)| {
                    bytes + row_bytes > max_bytes || *rows >= max_rows
                }) {
                    finish_csv_part(&mut files, part.take().expect("existing CSV part"))?;
                }
                if part.is_none() {
                    anyhow::ensure!(files.len() < MAX_PARTS, "CSV part count exceeds limit");
                    let name = format!("lci-inventory-{:06}.csv.gz", files.len());
                    let path = directory.join(&name);
                    let mut file = GzBuilder::new()
                        .mtime(0)
                        .write(File::create(&path)?, Compression::new(6));
                    file.write_all(&header)?;
                    part = Some((file, path, name, header.len() as u64, 0));
                }
                let (file, _, _, bytes, rows) = part.as_mut().expect("initialized CSV part");
                file.write_all(&row)?;
                *bytes += row_bytes;
                *rows += 1;
                Ok(())
            })?;
        require_record_count(artifact, observed)?;
    }
    if let Some(part) = part {
        finish_csv_part(&mut files, part)?;
    }
    if files.is_empty() {
        let path = directory.join("lci-inventory-000000.csv.gz");
        let mut file = GzBuilder::new()
            .mtime(0)
            .write(File::create(&path)?, Compression::new(6));
        file.write_all(&header)?;
        file.finish()?;
        files.push(local_download(
            "lci_inventory_csv",
            "advanced_data",
            "lci-inventory-000000.csv.gz",
            "application/gzip",
            0,
            path,
        )?);
    }
    Ok(files)
}

fn finish_csv_part(
    files: &mut Vec<LocalCalculationDownloadArtifact>,
    (file, path, name, _, rows): (GzEncoder<File>, PathBuf, String, u64, u64),
) -> anyhow::Result<()> {
    file.finish()?;
    files.push(local_download(
        "lci_inventory_csv",
        "advanced_data",
        &name,
        "application/gzip",
        rows,
        path,
    )?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        BuiltCalculationBundle, CompiledExchangeDirection, DeterministicGzipNdjsonWriter,
        GlobalReference, sha256_file,
    };
    use super::*;
    use solver_core::{FactorizationState, SolveResult};

    fn fixture() -> BuiltCalculationBundle {
        let mut writer = super::super::tests::fixture_writer();
        writer
            .write_result_chunk(
                0,
                &[SolveResult {
                    x: Some(vec![2.0]),
                    g: None,
                    h: Some(vec![1.0; 25]),
                    factorization_state: FactorizationState::Ready,
                }],
            )
            .unwrap();
        writer.finish().unwrap()
    }

    #[test]
    fn export_defaults_to_parquet_and_computation_has_no_downloads() {
        assert_eq!(ExportFormat::default(), ExportFormat::Parquet);
        let built = fixture();
        assert!(built.downloads.is_empty());
        for format in [
            ExportFormat::Parquet,
            ExportFormat::Csv,
            ExportFormat::LciaCsv,
            ExportFormat::LciaXlsx,
            ExportFormat::Audit,
        ] {
            let out = tempfile::tempdir().unwrap();
            let files = generate(
                out.path(),
                &built.manifest_path,
                &built.artifacts,
                1,
                format,
                &CancellationToken::default(),
            )
            .unwrap();
            assert!(!files.is_empty());
            assert!(
                files
                    .iter()
                    .all(|f| f.metadata.sha256 == sha256_file(&f.local_path).unwrap())
            );
            if format == ExportFormat::Csv || format == ExportFormat::Parquet {
                assert_eq!(
                    files.iter().map(|f| f.metadata.record_count).sum::<u64>(),
                    1
                );
            }
            if format == ExportFormat::Audit {
                assert_eq!(files.len(), built.artifacts.len() + 1);
                for file in files.iter().skip(1) {
                    let source = built
                        .artifacts
                        .iter()
                        .find(|a| Some(&a.metadata.path) == file.metadata.source_path.as_ref())
                        .unwrap();
                    assert_eq!(file.metadata.sha256, source.metadata.sha256);
                }
            }
        }
    }

    /// Private frozen full-library qualification; no network or fixture data is checked in.
    #[test]
    #[ignore = "requires independently downloaded original full-library export manifests"]
    fn original_full_library_parquet_and_csv_are_row_exact() {
        use arrow_array::{Float64Array, StringArray, UInt64Array};
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        use std::io::{BufReader, Read};
        let root = PathBuf::from(std::env::var("CALCULATION_EXPORT_QUALIFICATION_DIR").unwrap());
        let parquet: serde_json::Value =
            serde_json::from_reader(File::open(root.join("parquet/export-manifest.json")).unwrap())
                .unwrap();
        let csv: serde_json::Value =
            serde_json::from_reader(File::open(root.join("csv/export-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(parquet["bundleContentHash"], csv["bundleContentHash"]);
        assert_eq!(parquet["sourceManifestSha256"], csv["sourceManifestSha256"]);
        let mut parts = csv["files"].as_array().unwrap().iter();
        let mut current: Option<BufReader<flate2::read::GzDecoder<File>>> = None;
        let mut remaining = 0_u64;
        let mut header = Vec::new();
        write_csv_row(&mut header, LCI_EXPORT_HEADERS).unwrap();
        let mut total = 0_u64;
        for file in parquet["files"].as_array().unwrap() {
            let reader = ParquetRecordBatchReaderBuilder::try_new(
                File::open(
                    root.join("parquet")
                        .join(file["fileName"].as_str().unwrap()),
                )
                .unwrap(),
            )
            .unwrap()
            .with_batch_size(4096)
            .build()
            .unwrap();
            let mut file_rows = 0_u64;
            for batch in reader {
                let batch = batch.unwrap();
                for row in 0..batch.num_rows() {
                    if remaining == 0 {
                        if let Some(mut previous) = current.take() {
                            assert_eq!(previous.read(&mut [0]).unwrap(), 0);
                        }
                        let part = parts.next().expect("CSV covers every Parquet row");
                        remaining = part["recordCount"].as_u64().unwrap();
                        let mut stream = BufReader::new(flate2::read::GzDecoder::new(
                            File::open(root.join("csv").join(part["fileName"].as_str().unwrap()))
                                .unwrap(),
                        ));
                        let mut actual_header = vec![0; header.len()];
                        stream.read_exact(&mut actual_header).unwrap();
                        assert_eq!(actual_header, header);
                        current = Some(stream);
                    }
                    let mut values = vec![
                        batch
                            .column(0)
                            .as_any()
                            .downcast_ref::<UInt64Array>()
                            .unwrap()
                            .value(row)
                            .to_string(),
                    ];
                    for index in 1..8 {
                        values.push(
                            batch
                                .column(index)
                                .as_any()
                                .downcast_ref::<StringArray>()
                                .unwrap()
                                .value(row)
                                .to_owned(),
                        );
                    }
                    values.push(
                        batch
                            .column(8)
                            .as_any()
                            .downcast_ref::<Float64Array>()
                            .unwrap()
                            .value(row)
                            .to_string(),
                    );
                    let mut expected = Vec::new();
                    write_csv_row(&mut expected, values).unwrap();
                    let mut actual = vec![0; expected.len()];
                    current.as_mut().unwrap().read_exact(&mut actual).unwrap();
                    assert_eq!(actual, expected, "CSV/Parquet mismatch at row {total}");
                    remaining -= 1;
                    file_rows += 1;
                    total += 1;
                }
            }
            assert_eq!(file_rows, file["recordCount"].as_u64().unwrap());
        }
        assert_eq!(remaining, 0);
        assert!(parts.next().is_none());
        assert_eq!(current.unwrap().read(&mut [0]).unwrap(), 0);
        assert_eq!(total, 31_192_161);
        println!("Original full-library CSV/Parquet row parity: {total} exact rows");
    }

    #[test]
    fn empty_csv_has_one_header_and_cancelled_exports_stop_before_conversion() {
        let output = tempfile::tempdir().unwrap();
        let files = write_bounded_lci_csv(
            output.path(),
            &[],
            &[],
            CSV_MAX_BYTES,
            CSV_MAX_ROWS,
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].metadata.record_count, 0);
        let mut expected = Vec::new();
        write_csv_row(&mut expected, LCI_EXPORT_HEADERS).unwrap();
        let mut actual = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(File::open(&files[0].local_path).unwrap()),
            &mut actual,
        )
        .unwrap();
        assert_eq!(actual, expected);
        let cancellation = CancellationToken::default();
        {
            let _guard = CancelOnDrop(cancellation.clone());
        }
        assert!(
            generate(
                output.path(),
                &output.path().join("missing.json"),
                &[],
                0,
                ExportFormat::Parquet,
                &cancellation
            )
            .is_err()
        );
    }

    #[test]
    fn csv_splits_on_actual_escaped_bytes_and_rows_without_loss() {
        let built = fixture();
        let processes = load_processes(&built.artifacts, 1).unwrap();
        let output = tempfile::tempdir().unwrap();
        let path = output.path().join("source.gz");
        let mut writer = DeterministicGzipNdjsonWriter::create(&path).unwrap();
        let record = OwnedInventoryRecord {
            process_index: 0,
            flow: GlobalReference {
                id: Uuid::nil(),
                version: "01.01.000".to_owned(),
            },
            direction: CompiledExchangeDirection::Output,
            unit: "kg,\"quoted\"\nunit".to_owned(),
            location: Some("CN".to_owned()),
            mean_amount: -0.125,
        };
        for _ in 0..5 {
            writer.write(&record).unwrap();
        }
        let finished = writer.finish().unwrap();
        let mut metadata = built
            .artifacts
            .iter()
            .find(|a| a.metadata.kind == "lci")
            .unwrap()
            .metadata
            .clone();
        metadata.record_count = 5;
        let artifact = LocalCalculationBundleArtifact {
            metadata,
            local_path: path,
        };
        let mut header = Vec::new();
        write_csv_row(&mut header, LCI_EXPORT_HEADERS).unwrap();
        let mut row = Vec::new();
        write_csv_row(&mut row, lci_row(&record, &processes).unwrap()).unwrap();
        for (max_bytes, max_rows) in [
            (CSV_MAX_BYTES, 2),
            ((header.len() + 2 * row.len()) as u64, 100),
        ] {
            let files = write_bounded_lci_csv(
                output.path(),
                &[&artifact],
                &processes,
                max_bytes,
                max_rows,
                &CancellationToken::default(),
            )
            .unwrap();
            assert_eq!(
                files
                    .iter()
                    .map(|f| f.metadata.record_count)
                    .collect::<Vec<_>>(),
                [2, 2, 1]
            );
            let mut joined = Vec::new();
            for file in files {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(
                    &mut flate2::read::GzDecoder::new(File::open(file.local_path).unwrap()),
                    &mut bytes,
                )
                .unwrap();
                assert!(bytes.len() as u64 <= max_bytes);
                assert!(file.metadata.record_count <= max_rows);
                assert!(bytes.starts_with(&header));
                joined.extend_from_slice(&bytes[header.len()..]);
            }
            assert_eq!(joined, row.repeat(5));
        }
        assert!(
            write_bounded_lci_csv(
                output.path(),
                &[&artifact],
                &processes,
                (header.len() + row.len() - 1) as u64,
                100,
                &CancellationToken::default(),
            )
            .is_err()
        );
        let _ = finished;
    }
}
