//! Method work, including pinned source loading, stays inside the supervised native child.
use crate::{
    static_lcia_cache::TrustedStaticCacheSource,
    study::{
        contracts::Bounds,
        method_runtime_contracts::{NATIVE_SCHEMA, NativeInput},
    },
};
use anyhow::{Context, ensure};
use std::{
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};

fn read(path: &str, maximum: usize) -> anyhow::Result<Vec<u8>> {
    let path = Path::new(path);
    ensure!(path.is_absolute(), "study_method_native_path");
    let before = std::fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink(),
        "study_method_native_regular_file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= u64::try_from(maximum)?,
        "study_method_native_byte_limit"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            before.dev() == metadata.dev() && before.ino() == metadata.ino(),
            "study_method_native_file_changed"
        );
    }
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(maximum)?
            .checked_add(1)
            .context("study_method_native_byte_limit")?,
    )
    .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= maximum, "study_method_native_byte_limit");
    Ok(bytes)
}

pub async fn execute_native(path: &str, bounds: &Bounds) -> anyhow::Result<Vec<u8>> {
    bounds.validate()?;
    let request: NativeInput =
        crate::study::json::from_slice(&read(path, bounds.max_artifact_bytes)?)?;
    ensure!(
        request.schema_version == NATIVE_SCHEMA,
        "study_method_native_schema"
    );
    super::validate_context(&request.context)?;
    let prepared = read(&request.prepared_path, bounds.max_artifact_bytes)?;
    let result = read(&request.result_path, bounds.max_artifact_bytes)?;
    let source = read(&request.snapshot_path, bounds.max_source_bytes)?;
    // Enforce the selected-factor ceiling before the full source bundle or re-preparation expands.
    let verified = crate::study::analysis::verify_run(
        request.context.input.baseline.clone(),
        &prepared,
        &result,
        bounds,
    )?;
    ensure!(
        verified
            .prepared()
            .numeric
            .method_axes
            .len()
            .checked_mul(verified.prepared().numeric.inventory_axes.len())
            .is_some_and(|n| n <= request.context.budget.max_selected_factor_records),
        "study_method_selected_factor_limit"
    );
    drop(verified);
    let configuration = TrustedStaticCacheSource::new(
        request.static_cache_directory.map(PathBuf::from),
        request.static_cache_base_url,
    )?;
    let methods = super::TrustedMethods::load(&configuration).await?;
    let facts = super::assess_context(
        &request.context,
        &prepared,
        &result,
        &source,
        &methods,
        bounds,
    )?;
    super::validate_registered_output(&facts, &request.context, &prepared, &result, bounds)?;
    let bytes = serde_json::to_vec(&facts)?;
    super::verify_output(&facts, &bytes, request.context.budget.max_facts_bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn method_private_native_read_rejects_oversize_and_nonregular_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.json");
        std::fs::write(&path, b"12345").unwrap();
        assert!(super::read(path.to_str().unwrap(), 4).is_err());
        assert!(super::read(dir.path().to_str().unwrap(), 100).is_err());
        assert!(super::read("source.json", 100).is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("link.json");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(super::read(link.to_str().unwrap(), 100).is_err());
        }
    }
}
