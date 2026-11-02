//! Executable component evidence, not a source authorization or publication receipt.
use super::{
    contracts::{Bounds, COMPILER},
    runtime_contracts::{CHILD_SCHEMA, NativeAction, NativeRequest},
    runtime_process::{NativeFailure, run_native},
};
use crate::resource::CancellationToken;
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path, time::Duration};
use tokio::time::Instant;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreflightReceipt {
    pub schema_version: String,
    pub os: String,
    pub arch: String,
    pub abi: String,
    pub native_binary_sha256: String,
    pub calculation_contract_sha256: String,
    pub compiler_version: String,
    pub peak_rss_bytes: u64,
    pub timeout_reaped: bool,
    pub cancellation_reaped: bool,
    pub memory_limit_reaped: bool,
}
pub fn binary_hash(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    ensure!(
        std::fs::metadata(path)?.len() <= 512 * 1024 * 1024,
        "study_native_binary_limit"
    );
    let mut reader = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 65_536].into_boxed_slice();
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn request(directory: &Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let path = directory.join(format!("{name}.request.json"));
    let request = NativeRequest {
        schema_version: CHILD_SCHEMA.into(),
        limits: Bounds::default(),
        output_path: directory
            .join(format!("{name}.output.json"))
            .to_str()
            .context("study_path_encoding")?
            .into(),
        operation: NativeAction::Probe,
    };
    std::fs::write(&path, serde_json::to_vec(&request)?)?;
    Ok(path)
}
fn failure(error: &anyhow::Error, code: &str) -> anyhow::Result<()> {
    let value = error
        .downcast_ref::<NativeFailure>()
        .context("study_native_probe_failure_kind")?;
    ensure!(
        value.code == code && value.reaped,
        "study_native_probe_reaping"
    );
    Ok(())
}
async fn expect_guard(
    binary: &Path,
    path: &Path,
    cancel: &CancellationToken,
    deadline: Instant,
    rss: u64,
    code: &str,
) -> anyhow::Result<()> {
    let error = run_native(binary, path, cancel, deadline, rss)
        .await
        .err()
        .context("study_native_probe_guard_ineffective")?;
    failure(&error, code)
}
fn host_abi() -> &'static str {
    if cfg!(target_env = "gnu") {
        "gnu"
    } else if cfg!(target_env = "msvc") {
        "msvc"
    } else {
        "native"
    }
}
/// Every advertised native server tuple must produce this evidence on that actual host.
/// This does not attest outer cgroup/JobObject containment, which deployment qualification owns.
pub async fn probe(
    binary: &Path,
    directory: &Path,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> anyhow::Result<PreflightReceipt> {
    ensure!(
        binary.is_absolute() && directory.is_absolute(),
        "study_native_probe_paths"
    );
    let work = tempfile::Builder::new()
        .prefix("study-component-probe-")
        .tempdir_in(directory)?;
    let binary_sha256 = binary_hash(binary)?;
    let request_file = request(work.path(), "success")?;
    let success = run_native(
        binary,
        &request_file,
        cancellation,
        deadline,
        512 * 1024 * 1024,
    )
    .await?;
    ensure!(
        success.reaped && success.peak_rss_bytes > 0,
        "study_native_probe_rss_missing"
    );
    let output: serde_json::Value =
        super::json::from_slice(&std::fs::read(work.path().join("success.output.json"))?)?;
    ensure!(
        output["schemaVersion"] == "lca.study-native.probe.v1"
            && output["os"] == std::env::consts::OS
            && output["arch"] == std::env::consts::ARCH
            && output["abi"] == host_abi()
            && output["compilerVersion"] == COMPILER
            && output["calculationContractSha256"] == crate::calculation_contract::fingerprint()
            && output["x"] == serde_json::json!([1.25])
            && output["g"] == serde_json::json!([5.0])
            && output["h"] == serde_json::json!([15.0]),
        "study_native_probe_component_mismatch"
    );
    expect_guard(
        binary,
        &request(work.path(), "memory")?,
        cancellation,
        deadline,
        1,
        "study_native_memory_limit",
    )
    .await?;
    expect_guard(
        binary,
        &request(work.path(), "timeout")?,
        cancellation,
        deadline.min(Instant::now() + Duration::from_millis(100)),
        512 * 1024 * 1024,
        "study_job_timeout",
    )
    .await?;
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let trigger = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        signal.cancel();
    });
    let outcome = expect_guard(
        binary,
        &request(work.path(), "cancel")?,
        &cancel,
        deadline,
        512 * 1024 * 1024,
        "study_job_cancelled",
    )
    .await;
    trigger.await?;
    outcome?;
    cancellation.check("study_component_preflight")?;
    ensure!(
        binary_hash(binary)? == binary_sha256,
        "study_native_component_changed"
    );
    Ok(PreflightReceipt {
        schema_version: "lca.study-native.preflight.v1".into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        abi: host_abi().into(),
        native_binary_sha256: binary_sha256,
        calculation_contract_sha256: crate::calculation_contract::fingerprint().into(),
        compiler_version: COMPILER.into(),
        peak_rss_bytes: success.peak_rss_bytes,
        timeout_reaped: true,
        cancellation_reaped: true,
        memory_limit_reaped: true,
    })
}
