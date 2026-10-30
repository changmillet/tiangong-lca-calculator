//! S3 locators are derived from trusted receipts and the configured bucket/prefix.
use super::super::{
    CALCULATION_BUNDLE_FORMAT, CalculationBundleArtifactRef, CalculationBundleManifest,
    bundle_content_hash,
};
use super::{
    BuildLimits, BuiltQueryArtifact, MANIFEST_NAME, MAX_MANIFEST_BYTES, QueryArtifactRef,
    QueryManifest, reader::validate_manifest,
};
use crate::storage::{ObjectStoreClient, ObjectTransferOptions};
use std::{
    collections::BTreeSet,
    fs::File,
    path::{Component, Path},
};

/// Configuration is intentionally independent of the solver/database configuration.
pub fn store_from_env() -> anyhow::Result<ObjectStoreClient> {
    let required =
        |name: &str| std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"));
    ObjectStoreClient::new(
        &required("S3_ENDPOINT")?,
        &required("S3_REGION")?,
        &required("S3_BUCKET")?,
        &std::env::var("S3_PREFIX").unwrap_or_default(),
        &required("S3_ACCESS_KEY_ID")?,
        &required("S3_SECRET_ACCESS_KEY")?,
        std::env::var("S3_SESSION_TOKEN").ok(),
    )
}

pub async fn download_bundle(
    store: &ObjectStoreClient,
    reference: &CalculationBundleArtifactRef,
    limits: &BuildLimits,
) -> anyhow::Result<tempfile::TempDir> {
    super::super::validate_sha256(&reference.manifest_sha256, "source manifest hash")?;
    super::super::validate_sha256(&reference.bundle_content_hash, "source content hash")?;
    anyhow::ensure!(
        reference.manifest_byte_size <= MAX_MANIFEST_BYTES,
        "source manifest exceeds budget"
    );
    let directory = tempfile::tempdir()?;
    let prefix = format!(
        "calculation-bundles/{}/{}",
        reference.calculation_id, reference.bundle_content_hash
    );
    download(
        store,
        &prefix,
        "calculation-bundle.json",
        reference.manifest_byte_size,
        &reference.manifest_sha256,
        directory.path(),
    )
    .await?;
    let manifest: CalculationBundleManifest = serde_json::from_reader(File::open(
        directory.path().join("calculation-bundle.json"),
    )?)?;
    anyhow::ensure!(
        reference.schema_version == CALCULATION_BUNDLE_FORMAT
            && manifest.schema_version == CALCULATION_BUNDLE_FORMAT
            && manifest.calculation_id == reference.calculation_id
            && manifest.bundle_content_hash == reference.bundle_content_hash
            && bundle_content_hash(&manifest)? == reference.bundle_content_hash
            && manifest.artifacts.len() == reference.artifact_count,
        "source bundle binding mismatch"
    );
    let mut paths = BTreeSet::new();
    let mut total = 0_u64;
    for file in &manifest.artifacts {
        anyhow::ensure!(paths.insert(&file.path), "duplicate source file");
        if !matches!(
            file.kind.as_str(),
            "process_axis" | "method_axis" | "biosphere_edges" | "source_closure" | "lci" | "lcia"
        ) {
            continue;
        }
        total = total
            .checked_add(file.byte_size)
            .ok_or_else(|| anyhow::anyhow!("source size overflow"))?;
        anyhow::ensure!(
            file.byte_size <= limits.max_file_bytes && total <= limits.max_total_plain_bytes,
            "source download exceeds budget"
        );
        download(
            store,
            &prefix,
            &file.path,
            file.byte_size,
            &file.sha256,
            directory.path(),
        )
        .await?;
    }
    Ok(directory)
}

pub async fn upload(
    store: &ObjectStoreClient,
    artifact: &BuiltQueryArtifact,
) -> anyhow::Result<QueryArtifactRef> {
    upload_directory(store, artifact.path(), &artifact.reference).await
}

/// Retry publication from a previously persisted, verified local artifact set.
pub async fn upload_directory(
    store: &ObjectStoreClient,
    root: &Path,
    reference: &QueryArtifactRef,
) -> anyhow::Result<QueryArtifactRef> {
    let reader = super::QueryReader::open(root, reference)?;
    reader.validate_values()?;
    let prefix = reference.relative_prefix();
    for file in &reader.manifest.files {
        store
            .upload_object_key_file(
                &store.prefixed_object_key(&format!("{prefix}/{}", file.name))?,
                if matches!(file.name.as_str(), "lci.h5" | "lcia.h5") {
                    "application/x-hdf5"
                } else {
                    "application/gzip"
                },
                &root.join(&file.name),
                file.byte_size,
            )
            .await?;
    }
    store
        .upload_object_key_file(
            &store.prefixed_object_key(&format!("{prefix}/{MANIFEST_NAME}"))?,
            "application/json",
            &root.join(MANIFEST_NAME),
            reference.manifest_byte_size,
        )
        .await?;
    Ok(reference.clone())
}

pub async fn download_manifest(
    store: &ObjectStoreClient,
    reference: &QueryArtifactRef,
    root: &Path,
) -> anyhow::Result<QueryManifest> {
    reference.validate()?;
    download(
        store,
        &reference.relative_prefix(),
        MANIFEST_NAME,
        reference.manifest_byte_size,
        &reference.manifest_sha256,
        root,
    )
    .await?;
    let manifest = serde_json::from_reader(File::open(root.join(MANIFEST_NAME))?)?;
    validate_manifest(&manifest, reference)?;
    Ok(manifest)
}

pub async fn download_files(
    store: &ObjectStoreClient,
    reference: &QueryArtifactRef,
    manifest: &QueryManifest,
    root: &Path,
) -> anyhow::Result<()> {
    for file in &manifest.files {
        download(
            store,
            &reference.relative_prefix(),
            &file.name,
            file.byte_size,
            &file.sha256,
            root,
        )
        .await?;
    }
    Ok(())
}

async fn download(
    store: &ObjectStoreClient,
    prefix: &str,
    name: &str,
    bytes: u64,
    hash: &str,
    root: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && !name.contains(['\\', ':'])
            && Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "invalid artifact path"
    );
    super::super::validate_sha256(hash, "artifact hash")?;
    let destination = root.join(name);
    std::fs::create_dir_all(
        destination
            .parent()
            .ok_or_else(|| anyhow::anyhow!("missing artifact parent"))?,
    )?;
    let observed = store
        .download_object_key_to_file(
            &store.prefixed_object_key(&format!("{prefix}/{name}"))?,
            &destination,
            ObjectTransferOptions::new(bytes).with_expected_sha256(hash),
        )
        .await?;
    anyhow::ensure!(
        observed.byte_size == bytes,
        "downloaded artifact size mismatch"
    );
    Ok(())
}
