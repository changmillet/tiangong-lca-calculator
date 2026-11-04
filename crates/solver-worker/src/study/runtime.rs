//! Existing `worker_jobs` adapter. Domain callbacks atomically own successful terminal completion.
use super::{
    contracts::{Bounds, COMPILER, DatasetRef, INPUT_SCHEMA, PROFILE, RESULT_SCHEMA},
    decode_prepared,
    runtime_contracts::{
        ArtifactRef, CHILD_SCHEMA, NativeAction, NativeRequest, PreparationSource, PrivateArtifact,
        SNAPSHOT_SCHEMA, SourceSnapshot,
    },
    runtime_process::run_native,
    sha256,
};
use crate::{
    db::AppState,
    db_pool::sql_string_literal,
    pgbouncer_sqlx::{self as sqlx, Row},
    resource::CancellationToken,
    storage::{ObjectStoreClient, ObjectTransferOptions},
    worker_jobs::{
        WorkerJob, heartbeat_worker_job, lease_heartbeat_period, renew_worker_job_lease,
    },
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, timeout};
use uuid::Uuid;
const BUCKET: &str = "study-artifacts";
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const JOB_SECONDS: u64 = 120;
const RSS_BYTES: u64 = 512 * 1024 * 1024;
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobRequest {
    schema_version: String,
    study_id: Uuid,
    #[serde(default)]
    acquisition_id: Option<Uuid>,
    #[serde(default)]
    preparation_id: Option<Uuid>,
    #[serde(default)]
    run_id: Option<Uuid>,
    #[serde(default)]
    analysis_id: Option<Uuid>,
}
#[derive(Debug, thiserror::Error)]
#[error("study_completion_outcome_unknown")]
struct CompletionUnknown;
#[derive(Debug, thiserror::Error)]
#[error("study_domain_rpc_rejected")]
struct DomainRejected;
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct NativeRejected(&'static str);
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrphanReceipt {
    schema_version: String,
    worker_job_id: Uuid,
    study_id: Uuid,
    artifact_ref: ArtifactRef,
    storage_bucket: String,
    storage_object_path: String,
    reason: String,
    attempt_count: i64,
    lease_token: Uuid,
    resource_type: String,
    resource_id: Uuid,
    idempotency_key: String,
    storage_authority_hash: String,
}

#[must_use]
pub fn handles(kind: &str) -> bool {
    matches!(
        kind,
        "study.source_acquire"
            | "study.model_prepare"
            | "study.run"
            | "study.analysis"
            | "study.analysis_advanced"
    )
}
fn resource_type(kind: &str) -> anyhow::Result<&'static str> {
    match kind {
        "study.source_acquire" => Ok("modelSourceAcquisition"),
        "study.model_prepare" => Ok("modelPreparation"),
        "study.run" => Ok("run"),
        "study.analysis" | "study.analysis_advanced" => Ok("analysis"),
        _ => anyhow::bail!("study_job_kind"),
    }
}
fn request(job: &WorkerJob) -> anyhow::Result<(JobRequest, Uuid)> {
    ensure!(
        handles(&job.job_kind)
            && job.worker_queue == "solver"
            && job.payload_schema_version == format!("{}.request.v1", job.job_kind),
        "study_job_schema"
    );
    let value: JobRequest = serde_json::from_value(job.payload.clone())?;
    ensure!(
        value.schema_version == job.payload_schema_version && job.requested_by.is_some(),
        "study_job_actor_or_schema"
    );
    let ids = [
        value.acquisition_id,
        value.preparation_id,
        value.run_id,
        value.analysis_id,
    ];
    ensure!(
        ids.iter().filter(|v| v.is_some()).count() == 1,
        "study_job_subject"
    );
    let subject = match job.job_kind.as_str() {
        "study.source_acquire" => value.acquisition_id,
        "study.model_prepare" => value.preparation_id,
        "study.run" => value.run_id,
        _ => value.analysis_id,
    }
    .context("study_job_subject")?;
    Ok((value, subject))
}
fn input_rpc(kind: &str) -> anyhow::Result<&'static str> {
    match kind {
        "study.source_acquire" => Ok("svc_study_source_acquisition_read_input_v1"),
        "study.model_prepare" => Ok("svc_study_model_preparation_read_input_v1"),
        "study.run" => Ok("svc_study_run_read_input_v1"),
        "study.analysis" => Ok("svc_study_analysis_read_input_v1"),
        "study.analysis_advanced" => Ok("svc_study_analysis_advanced_read_input_v1"),
        _ => anyhow::bail!("study_job_kind"),
    }
}
fn completion_rpc(kind: &str) -> anyhow::Result<&'static str> {
    match kind {
        "study.source_acquire" => Ok("svc_study_source_record_v1"),
        "study.model_prepare" => Ok("svc_study_model_preparation_complete_v1"),
        "study.run" => Ok("svc_study_run_complete_v1"),
        "study.analysis" => Ok("svc_study_analysis_complete_v1"),
        "study.analysis_advanced" => Ok("svc_study_analysis_advanced_complete_v1"),
        _ => anyhow::bail!("study_job_kind"),
    }
}
fn check_live(cancel: &CancellationToken, deadline: Instant) -> anyhow::Result<()> {
    cancel.check("study_runtime")?;
    ensure!(Instant::now() < deadline, "study_job_timeout");
    Ok(())
}
async fn bounded<T>(
    cancel: &CancellationToken,
    deadline: Instant,
    work: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    check_live(cancel, deadline)?;
    tokio::pin!(work);
    loop {
        tokio::select! {result=&mut work=>{check_live(cancel,deadline)?;return result;},()=tokio::time::sleep(Duration::from_millis(50))=>check_live(cancel,deadline)?}
    }
}
async fn rpc(
    pool: &sqlx::PgPool,
    name: &str,
    subject: Uuid,
    job: &WorkerJob,
    completion: Option<&Value>,
) -> anyhow::Result<Value> {
    let args = format!(
        "'{}'::uuid,'{}'::uuid,'{}'::uuid",
        subject, job.id, job.lease_token
    );
    let tail = completion.map_or_else(String::new, |value| {
        format!(
            ",{}::jsonb,{}",
            sql_string_literal(&value.to_string()),
            sql_string_literal(&format!("study:{}:{}", job.id, job.lease_token))
        )
    });
    let query = format!(
        "WITH _service_role AS (SELECT set_config('request.jwt.claim.role','service_role',true)) SELECT api.{name}({args}{tail})::text AS result FROM _service_role"
    );
    let raw = sqlx::raw_query(&query)
        .fetch_one(pool)
        .await?
        .try_get::<String, _>("result")?;
    let response: Value = super::json::from_str(&raw)?;
    if response["ok"].as_bool() != Some(true) {
        return Err(DomainRejected.into());
    }
    Ok(response["data"].clone())
}
#[allow(clippy::too_many_arguments)]
async fn task_rpc(
    pool: &sqlx::PgPool,
    name: &str,
    kind: &str,
    subject: Uuid,
    job: Uuid,
    token: Uuid,
    key: &str,
    tail: &str,
) -> anyhow::Result<Value> {
    let query = format!(
        "WITH _service_role AS (SELECT set_config('request.jwt.claim.role','service_role',true)) SELECT api.{name}({},'{}'::uuid,'{}'::uuid,'{}'::uuid,{}{tail})::text AS result FROM _service_role",
        sql_string_literal(kind),
        subject,
        job,
        token,
        sql_string_literal(key)
    );
    let raw = sqlx::raw_query(&query)
        .fetch_one(pool)
        .await?
        .try_get::<String, _>("result")?;
    let response: Value = super::json::from_str(&raw)?;
    if response["ok"].as_bool() != Some(true) {
        return Err(DomainRejected.into());
    }
    Ok(response["data"].clone())
}
pub(super) fn validate_location(
    location: &PrivateArtifact,
    study: Uuid,
    kind: &str,
) -> anyhow::Result<()> {
    ensure!(
        location.study_id == study
            && location.storage_bucket == BUCKET
            && location.artifact_ref.r#type == kind
            && location.artifact_ref.digest_scope == "artifact_bytes_sha256"
            && location.artifact_ref.byte_size > 0
            && location.artifact_ref.byte_size <= MAX_BYTES,
        "study_artifact_binding"
    );
    let prefix = format!("study/{study}/{}/", location.artifact_ref.id);
    let suffix = location
        .storage_object_path
        .strip_prefix(&prefix)
        .context("study_artifact_scope")?;
    ensure!(
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
            && !suffix.contains("..")
            && Path::new(suffix).extension().is_some_and(|v| v == "json"),
        "study_artifact_scope"
    );
    ensure!(
        location.artifact_ref.media_type == "application/json",
        "study_artifact_media_type"
    );
    Ok(())
}
async fn download(
    store: &ObjectStoreClient,
    location: &PrivateArtifact,
    study: Uuid,
    kind: &str,
    path: &Path,
    cancel: &CancellationToken,
    deadline: Instant,
) -> anyhow::Result<()> {
    validate_location(location, study, kind)?;
    let result = bounded(
        cancel,
        deadline,
        store.download_object_key_to_file(
            &location.storage_object_path,
            path,
            ObjectTransferOptions::new(MAX_BYTES)
                .with_expected_sha256(&location.artifact_ref.content_hash)
                .with_cancellation(cancel.clone())
                .with_request_timeout(Duration::from_secs(20)),
        ),
    )
    .await?;
    ensure!(
        result.byte_size == location.artifact_ref.byte_size,
        "study_artifact_size_mismatch"
    );
    Ok(())
}
fn safe_error(error: &anyhow::Error) -> &'static str {
    if let Some(rejected) = error.downcast_ref::<NativeRejected>() {
        return rejected.0;
    }
    if let Some(native) = error.downcast_ref::<super::runtime_process::NativeFailure>() {
        return native.code;
    }
    if error.is::<CompletionUnknown>() {
        return "study_completion_outcome_unknown";
    }
    if let Some(resource) = error.downcast_ref::<crate::resource::ResourceError>() {
        return resource.error_code();
    }
    for code in [
        "study_job_timeout",
        "study_job_lease_lost",
        "study_runtime_unavailable",
        "study_runtime_configuration",
        "study_artifact_scope",
        "study_source_scope_document",
        "study_calculation_binding",
    ] {
        if error.to_string() == code {
            return code;
        }
    }
    "study_runtime_failed"
}
fn failure_code(error: &anyhow::Error) -> &'static str {
    let label = safe_error(error);
    let bare = label.strip_prefix("study_").unwrap_or(label);
    match bare {
        "source_invalid" => "source_invalid",
        "boundary_undeclared" => "boundary_undeclared",
        "normalization_underflow" => "normalization_underflow",
        "normalization_overflow" => "normalization_overflow",
        "numeric_invalid" => "numeric_invalid",
        "resource_limit" | "artifact_limit_exceeded" | "resource_admission_rejected" => {
            "resource_limit"
        }
        "model_not_solvable" => "model_not_solvable",
        "native_execution_failed" => "native_execution_failed",
        "job_timeout" => "job_timeout",
        "job_lease_lost" | "operation_cancelled" => "job_lease_lost",
        "runtime_unavailable" => "runtime_unavailable",
        _ => "runtime_failed",
    }
}
fn verify_packet(packet: &SourceSnapshot, job: &WorkerJob, origin: &Value) -> anyhow::Result<()> {
    ensure!(
        packet.schema_version == SNAPSHOT_SCHEMA
            && packet.origin_ref == *origin
            && packet.issuer_id == "database.study-source.v1"
            && !packet.license_ref.is_empty(),
        "study_source_issuer_binding"
    );
    let actor = job.requested_by.context("study_job_actor")?.to_string();
    ensure!(
        packet.source.actor_id.as_ref() == Some(&actor)
            && packet.source_authorization["requestedBy"] == actor
            && packet.source_authorization["originRef"] == *origin
            && packet.source_authorization["schemaVersion"] == "lca.study-source-authorization.v1"
            && packet.source_authorization["sourceEncoding"] == "postgresql.json-text.utf8.v1",
        "study_source_scope_binding"
    );
    let scope = packet.source_authorization["sourceScope"]
        .as_array()
        .context("study_source_scope")?;
    ensure!(
        scope.len() == packet.source.documents.len()
            && scope.len() <= Bounds::default().max_documents,
        "study_source_scope_membership"
    );
    for doc in &packet.source.documents {
        let matches = scope
            .iter()
            .filter(|v| {
                v["kind"] == doc.kind
                    && v["id"] == doc.identity.id
                    && v["version"] == doc.identity.version
                    && v["stateCode"] == doc.state_code
                    && v["sha256"] == doc.sha256
                    && v["ownerActorId"]
                        == serde_json::to_value(&doc.owner_actor_id).unwrap_or(Value::Null)
            })
            .count();
        ensure!(
            matches == 1 && sha256(doc.json.as_bytes()) == doc.sha256,
            "study_source_scope_document"
        );
    }
    // observedAt is authoritative Database time, not a host-wall-clock freshness comparison.
    chrono::DateTime::parse_from_rfc3339(&packet.observed_at)?;
    Ok(())
}

fn read_native_error(path: &Path) -> Option<super::runtime_contracts::NativeErrorKind> {
    if std::fs::metadata(path).ok()?.len() > 1024 {
        return None;
    }
    let envelope: super::runtime_contracts::NativeErrorEnvelope =
        super::json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (envelope.schema_version == "lca.study-native.failure.v1").then_some(envelope.kind)
}
struct ExecutionConfig {
    binary: PathBuf,
    orphan_dir: PathBuf,
    temp_root: PathBuf,
}

// These are operator-selected, independently qualified limits, never payload authority.
// Missing advanced admission does not change any deterministic Study handler.
fn advanced_batch_limits(
    enabled: Option<&str>,
    parameters: Option<&str>,
    evaluations: Option<&str>,
) -> anyhow::Result<super::analysis_advanced_contracts::BatchLimits> {
    fn count(text: Option<&str>) -> anyhow::Result<usize> {
        let text = text.context("study_runtime_configuration")?;
        ensure!(
            !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
            "study_runtime_configuration"
        );
        let value: u32 = text.parse().context("study_runtime_configuration")?;
        ensure!(value > 0, "study_runtime_configuration");
        Ok(usize::try_from(value)?)
    }
    ensure!(enabled == Some("true"), "study_runtime_unavailable");
    let max_parameters = count(parameters)?;
    let max_evaluations = count(evaluations)?;
    ensure!(max_evaluations >= 3, "study_runtime_configuration");
    Ok(super::analysis_advanced_contracts::BatchLimits {
        max_parameters,
        max_evaluations,
    })
}

fn advanced_job_binding(
    input: &super::analysis_advanced_contracts::ServiceInput,
    request: &JobRequest,
    subject: Uuid,
    job: &WorkerJob,
    batch: &super::analysis_advanced_contracts::BatchLimits,
) -> anyhow::Result<()> {
    super::analysis_advanced::validate_input(input)?;
    ensure!(
        input.analysis.study_id == request.study_id
            && input.analysis.analysis_id == subject
            && input.analysis.execution.job_id == job.id,
        "study_analysis_job_binding"
    );
    let evaluations = match &input.analysis.config {
        super::analysis_advanced_contracts::Config::SensitivityOat { parameters, .. } => {
            parameters.len().checked_mul(2)
        }
        super::analysis_advanced_contracts::Config::Uncertainty { sample_count, .. } => {
            Some(*sample_count)
        }
    };
    ensure!(
        input.analysis.config.parameters().len() <= batch.max_parameters
            && evaluations
                .and_then(|n| n.checked_add(1))
                .is_some_and(|n| n <= batch.max_evaluations),
        "study_advanced_evaluation_limit"
    );
    Ok(())
}
impl ExecutionConfig {
    fn environment() -> anyhow::Result<Self> {
        ensure!(
            std::env::var("STUDY_RUNTIME_ENABLED").as_deref() == Ok("true"),
            "study_runtime_unavailable"
        );
        let binary = PathBuf::from(std::env::var("STUDY_NATIVE_BIN")?);
        let orphan_dir = PathBuf::from(std::env::var("STUDY_ORPHAN_RECEIPT_DIR")?);
        let temp_root = PathBuf::from(std::env::var("STUDY_TEMP_ROOT")?);
        ensure!(
            binary.is_absolute()
                && binary.is_file()
                && orphan_dir.is_absolute()
                && temp_root.is_absolute(),
            "study_runtime_configuration"
        );
        if !orphan_dir.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&orphan_dir)?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(&orphan_dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::metadata(&orphan_dir)?
                    .permissions()
                    .mode()
                    .trailing_zeros()
                    >= 6,
                "study_orphan_journal_not_private"
            );
        }
        if !temp_root.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&temp_root)?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(&temp_root)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::metadata(&temp_root)?
                    .permissions()
                    .mode()
                    .trailing_zeros()
                    >= 6,
                "study_temp_root_not_private"
            );
        }
        Ok(Self {
            binary,
            orphan_dir,
            temp_root,
        })
    }
}
async fn ensure_preflight(
    config: &ExecutionConfig,
    deadline: Instant,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    static CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<String>>> =
        std::sync::OnceLock::new();
    let fingerprint = super::runtime_preflight::binary_hash(&config.binary)?;
    let mut cached = CACHE
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    if cached.as_ref() == Some(&fingerprint) {
        return Ok(());
    }
    let receipt =
        super::runtime_preflight::probe(&config.binary, &config.temp_root, deadline, cancel)
            .await?;
    ensure!(
        receipt.native_binary_sha256 == fingerprint,
        "study_native_component_changed"
    );
    write_json(&config.temp_root.join("native-preflight.json"), &receipt)?;
    *cached = Some(fingerprint);
    Ok(())
}
fn record_binding(
    record: &Value,
    study: Uuid,
    job: Uuid,
    id_key: &str,
    subject: Uuid,
) -> anyhow::Result<()> {
    ensure!(
        record["studyId"] == study.to_string()
            && record[id_key] == subject.to_string()
            && record["execution"]["jobId"] == job.to_string(),
        "study_domain_job_binding"
    );
    Ok(())
}
fn contract_binding(record: &Value) -> anyhow::Result<()> {
    ensure!(
        record["profileId"] == PROFILE
            && record["calculationContractRef"]
                == serde_json::to_value(super::contracts::CalculationRef {
                    kind: "calculation_contract".into(),
                    schema: "lca.calculation-contract.v1".into(),
                    sha256: crate::calculation_contract::fingerprint().into()
                })?,
        "study_calculation_binding"
    );
    Ok(())
}
fn text_path(path: &Path) -> anyhow::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("study_path_encoding")
}
fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= usize::try_from(MAX_BYTES)?,
        "study_artifact_limit"
    );
    std::fs::write(path, bytes)?;
    Ok(())
}
fn receipt_path(config: &ExecutionConfig, job: &WorkerJob, reference: &ArtifactRef) -> PathBuf {
    config
        .orphan_dir
        .join(format!("{}-{}.json", job.id, reference.id))
}
fn keep_orphan(
    config: &ExecutionConfig,
    job: &WorkerJob,
    study: Uuid,
    reference: &ArtifactRef,
    path: &str,
    reason: &str,
    authority: &str,
) -> anyhow::Result<()> {
    let (_, subject) = request(job)?;
    let resource_type = resource_type(&job.job_kind)?;
    let receipt = OrphanReceipt {
        schema_version: "lca.study-orphan-artifact.v1".into(),
        worker_job_id: job.id,
        study_id: study,
        artifact_ref: reference.clone(),
        storage_bucket: BUCKET.into(),
        storage_object_path: path.into(),
        reason: reason.into(),
        attempt_count: job.attempt_count,
        lease_token: job.lease_token,
        resource_type: resource_type.into(),
        resource_id: subject,
        idempotency_key: format!("study:{}:{}", job.id, job.lease_token),
        storage_authority_hash: authority.into(),
    };
    if !receipt_path(config, job, reference).exists() {
        ensure!(
            std::fs::read_dir(&config.orphan_dir)?.take(4097).count() < 4096,
            "study_orphan_journal_limit"
        );
    }
    let mut file = tempfile::NamedTempFile::new_in(&config.orphan_dir)?;
    file.write_all(&serde_json::to_vec(&receipt)?)?;
    file.as_file().sync_all()?;
    file.persist(receipt_path(config, job, reference))?;
    Ok(())
}
async fn complete(
    pool: &sqlx::PgPool,
    name: &str,
    subject: Uuid,
    job: &WorkerJob,
    body: &Value,
    deadline: Instant,
) -> anyhow::Result<()> {
    complete_with_retry(deadline, || rpc(pool, name, subject, job, Some(body))).await
}
async fn complete_with_retry<F, Fut>(deadline: Instant, mut invoke: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    let mut attempted = false;
    let mut ambiguous = false;
    for attempt in 0..3 {
        let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|v| !v.is_zero())
        else {
            if !attempted {
                anyhow::bail!("study_job_timeout");
            }
            return Err(CompletionUnknown.into());
        };
        attempted = true;
        match timeout(remaining.min(Duration::from_secs(10)), invoke()).await {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(error)) if error.is::<DomainRejected>() => {
                // A later authorization/lease rejection cannot disprove an
                // earlier committed callback whose acknowledgement was lost.
                if ambiguous {
                    return Err(CompletionUnknown.into());
                }
                return Err(error);
            }
            _ => ambiguous = true,
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Err(CompletionUnknown.into())
}
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn pipeline(
    state: &AppState,
    job: &WorkerJob,
    request: &JobRequest,
    subject: Uuid,
    config: &ExecutionConfig,
    cancel: &CancellationToken,
    deadline: Instant,
    publishing: &AtomicBool,
    lease_seconds: i32,
) -> anyhow::Result<()> {
    let advanced_batch = if job.job_kind == "study.analysis_advanced" {
        Some(advanced_batch_limits(
            std::env::var("STUDY_ADVANCED_ENABLED").ok().as_deref(),
            std::env::var("STUDY_ADVANCED_MAX_PARAMETERS")
                .ok()
                .as_deref(),
            std::env::var("STUDY_ADVANCED_MAX_EVALUATIONS")
                .ok()
                .as_deref(),
        )?)
    } else {
        None
    };
    ensure_preflight(config, deadline, cancel).await?;
    let store = state.object_store.study_artifacts();
    let work = tempfile::Builder::new()
        .prefix(&format!("study-{}-attempt{}-", job.id, job.attempt_count))
        .tempdir_in(&config.temp_root)?;
    write_json(
        &work.path().join("ownership.json"),
        &json!({"schemaVersion":"lca.study-local-resource.v1","workerJobId":job.id,"studyId":request.study_id,"attemptCount":job.attempt_count,"workerPid":std::process::id()}),
    )?;
    let input_file = work.path().join("source.json");
    let output = work.path().join("output.json");
    let completion_rpc = completion_rpc(&job.job_kind)?;
    let (operation, mut completion, artifact_kind, artifact_schema) = match job.job_kind.as_str() {
        "study.source_acquire" => {
            let data = bounded(
                cancel,
                deadline,
                rpc(&state.pool, input_rpc(&job.job_kind)?, subject, job, None),
            )
            .await?;
            record_binding(
                &data["acquisition"],
                request.study_id,
                job.id,
                "acquisitionId",
                subject,
            )?;
            let packet: SourceSnapshot = serde_json::from_value(data["sourcePacket"].clone())?;
            verify_packet(&packet, job, &data["acquisition"]["originRef"])?;
            write_json(&input_file, &packet)?;
            let body = json!({"snapshotId":Uuid::new_v4(),"originRef":packet.origin_ref,"licenseRef":packet.license_ref,"issuerId":packet.issuer_id,"observedAt":packet.observed_at,"sourceAuthorization":packet.source_authorization});
            (
                NativeAction::Acquire {
                    snapshot_path: text_path(&input_file)?,
                },
                body,
                "modelSource",
                SNAPSHOT_SCHEMA,
            )
        }
        "study.model_prepare" => {
            let data = bounded(
                cancel,
                deadline,
                rpc(&state.pool, input_rpc(&job.job_kind)?, subject, job, None),
            )
            .await?;
            record_binding(
                &data["preparation"],
                request.study_id,
                job.id,
                "preparationId",
                subject,
            )?;
            contract_binding(&data["preparation"])?;
            let document: PreparationSource =
                serde_json::from_value(data["sourceDocument"].clone())?;
            ensure!(
                document.schema_version == "lca.study-preparation-input.v1"
                    && document.source_snapshot["studyId"] == request.study_id.to_string()
                    && document.source_snapshot["sourceArtifactRef"]
                        == serde_json::to_value(&document.source_artifact.artifact_ref)?,
                "study_preparation_source_binding"
            );
            download(
                &store,
                &document.source_artifact,
                request.study_id,
                "modelSource",
                &input_file,
                cancel,
                deadline,
            )
            .await?;
            ensure!(
                document.source_artifact.artifact_ref.schema_version == SNAPSHOT_SCHEMA,
                "study_source_snapshot_schema"
            );
            let operation = if let Some(base) = document.base_prepared_artifact {
                ensure!(
                    document.base_prepared_preparation_id.is_some()
                        && data["preparation"]["source"]["type"] == "scenarioRevision",
                    "study_preparation_base_binding"
                );
                let base_file = work.path().join("base.json");
                download(
                    &store,
                    &base,
                    request.study_id,
                    "modelInput",
                    &base_file,
                    cancel,
                    deadline,
                )
                .await?;
                ensure!(
                    base.artifact_ref.schema_version == INPUT_SCHEMA,
                    "study_prepared_schema"
                );
                let frozen = decode_prepared(
                    &std::fs::read(&base_file)?,
                    &base.artifact_ref.content_hash,
                    &Bounds::default(),
                )?;
                let mut captured_cutoffs = document.cutoffs.clone();
                captured_cutoffs
                    .sort_by_key(|c| (c.port.instance_id.clone(), c.port.exchange_id.clone()));
                ensure!(
                    captured_cutoffs == frozen.model.cutoffs,
                    "study_inherited_cutoff_binding"
                );
                let binding = document.study_binding.context("study_binding_missing")?;
                NativeAction::PrepareScenario {
                    snapshot_path: text_path(&input_file)?,
                    base_prepared_path: text_path(&base_file)?,
                    base_prepared_sha256: base.artifact_ref.content_hash,
                    study_binding: binding,
                    overrides: document.overrides,
                }
            } else {
                ensure!(
                    document.base_prepared_preparation_id.is_none()
                        && document.study_binding.is_none()
                        && document.overrides.is_empty()
                        && data["preparation"]["source"]["type"] == "modelSourceSnapshot",
                    "study_preparation_base_binding"
                );
                let refs: Vec<super::contracts::MethodRef> =
                    serde_json::from_value(data["methodRefs"].clone())?;
                ensure!(
                    refs.iter()
                        .all(|r| r.r#type == "datasetRevision" && r.dataset_type == "lciamethod"),
                    "study_method_binding"
                );
                NativeAction::PrepareBase {
                    snapshot_path: text_path(&input_file)?,
                    method_refs: refs
                        .into_iter()
                        .map(|r| DatasetRef {
                            id: r.id,
                            version: r.version,
                        })
                        .collect(),
                    cutoffs: document.cutoffs,
                    static_cache_directory: std::env::var("LCIA_STATIC_CACHE_DIR").ok(),
                    static_cache_base_url: std::env::var("LCIA_STATIC_CACHE_BASE_URL").ok(),
                }
            };
            (
                operation,
                json!({"profileId":data["preparation"]["profileId"],"calculationContractRef":data["preparation"]["calculationContractRef"]}),
                "modelInput",
                INPUT_SCHEMA,
            )
        }
        "study.analysis_advanced" => {
            let data = bounded(
                cancel,
                deadline,
                rpc(&state.pool, input_rpc(&job.job_kind)?, subject, job, None),
            )
            .await?;
            let input: super::analysis_advanced_contracts::ServiceInput =
                serde_json::from_value(data)?;
            let batch = advanced_batch.context("study_runtime_unavailable")?;
            advanced_job_binding(&input, request, subject, job, &batch)?;
            let source = work.path().join("advanced-source.json");
            let prepared = work.path().join("advanced-prepared.json");
            let result = work.path().join("advanced-result.json");
            for (artifact, kind, path) in [
                (&input.source_artifact, "modelSource", &source),
                (&input.baseline.prepared_artifact, "modelInput", &prepared),
                (&input.baseline.result_artifact, "runResult", &result),
            ] {
                download(
                    &store,
                    artifact,
                    request.study_id,
                    kind,
                    path,
                    cancel,
                    deadline,
                )
                .await?;
            }
            let body = json!({"profileId":input.analysis.profile_id,"configHash":input.analysis.config_hash});
            write_json(
                &input_file,
                &super::analysis_advanced_contracts::NativeInput {
                    schema_version: "lca.study-analysis-native.v2".into(),
                    input,
                    snapshot_path: text_path(&source)?,
                    prepared_path: text_path(&prepared)?,
                    result_path: text_path(&result)?,
                    batch_limits: batch,
                },
            )?;
            (
                NativeAction::AnalyzeAdvanced {
                    analysis_path: text_path(&input_file)?,
                },
                body,
                "analysisFacts",
                super::analysis_advanced_contracts::FACTS_SCHEMA,
            )
        }
        "study.analysis" => {
            let data = bounded(
                cancel,
                deadline,
                rpc(&state.pool, input_rpc(&job.job_kind)?, subject, job, None),
            )
            .await?;
            let input: super::analysis_contracts::ServiceInput = serde_json::from_value(data)?;
            super::analysis::validate_input(&input)?;
            ensure!(
                input.analysis.study_id == request.study_id
                    && input.analysis.analysis_id == subject
                    && input.analysis.execution.job_id == job.id,
                "study_analysis_job_binding"
            );
            let mut files = vec![];
            for (index, run) in input.runs.iter().enumerate() {
                let prepared = work.path().join(format!("analysis-{index}-prepared.json"));
                let result = work.path().join(format!("analysis-{index}-result.json"));
                download(
                    &store,
                    &run.prepared_artifact,
                    request.study_id,
                    "modelInput",
                    &prepared,
                    cancel,
                    deadline,
                )
                .await?;
                download(
                    &store,
                    &run.result_artifact,
                    request.study_id,
                    "runResult",
                    &result,
                    cancel,
                    deadline,
                )
                .await?;
                files.push(super::analysis_contracts::NativeFiles {
                    run_id: input.analysis.config.run_ids[index],
                    prepared_path: text_path(&prepared)?,
                    result_path: text_path(&result)?,
                });
            }
            let body = json!({"profileId":input.analysis.profile_id,"configHash":input.analysis.config_hash});
            write_json(
                &input_file,
                &super::analysis_contracts::NativeInput {
                    schema_version: "lca.study-analysis-native.v1".into(),
                    input,
                    files,
                },
            )?;
            (
                NativeAction::Analyze {
                    analysis_path: text_path(&input_file)?,
                },
                body,
                "analysisFacts",
                super::analysis_contracts::FACTS_SCHEMA,
            )
        }
        _ => {
            let data = bounded(
                cancel,
                deadline,
                rpc(&state.pool, input_rpc(&job.job_kind)?, subject, job, None),
            )
            .await?;
            record_binding(&data["run"], request.study_id, job.id, "runId", subject)?;
            contract_binding(&data["run"])?;
            let artifact: PrivateArtifact =
                serde_json::from_value(data["preparedArtifact"].clone())?;
            ensure!(
                data["preparation"]["inputArtifactRef"]
                    == serde_json::to_value(&artifact.artifact_ref)?
                    && data["preparation"]["preparationId"] == data["run"]["modelPreparationId"],
                "study_run_preparation_binding"
            );
            download(
                &store,
                &artifact,
                request.study_id,
                "modelInput",
                &input_file,
                cancel,
                deadline,
            )
            .await?;
            let prepared = decode_prepared(
                &std::fs::read(&input_file)?,
                &artifact.artifact_ref.content_hash,
                &Bounds::default(),
            )?;
            ensure!(
                prepared
                    .input_hash
                    .as_ref()
                    .is_some_and(|hash| data["run"]["inputHash"] == *hash)
                    && data["run"]["effectiveModelRef"]["contentHash"] == prepared.model_hash
                    && data["run"]["effectiveModelRef"] == data["scenario"]["effectiveModelRef"],
                "study_run_input_binding"
            );
            let binding = prepared
                .study_binding
                .as_ref()
                .context("study_binding_missing")?;
            ensure!(
                data["revision"]["revisionId"] == binding.study_revision_id
                    && data["revision"]["contentHash"] == binding.study_revision_content_hash
                    && data["revision"]["definition"] == serde_json::to_value(&binding.definition)?
                    && data["scenario"]["studyRevisionId"] == binding.study_revision_id
                    && data["scenario"]["modelPreparationId"]
                        == data["preparation"]["preparationId"]
                    && data["revision"]["status"] == "frozen"
                    && data["scenario"]["status"] == "frozen",
                "study_run_frozen_revision_binding"
            );
            (
                NativeAction::Run {
                    prepared_path: text_path(&input_file)?,
                    prepared_sha256: artifact.artifact_ref.content_hash,
                },
                json!({"inputHash":data["run"]["inputHash"],"calculationContractRef":data["run"]["calculationContractRef"]}),
                "runResult",
                RESULT_SCHEMA,
            )
        }
    };
    check_live(cancel, deadline)?;
    let native = NativeRequest {
        schema_version: CHILD_SCHEMA.into(),
        limits: Bounds::default(),
        output_path: text_path(&output)?,
        operation,
    };
    let expected_run = if let NativeAction::Run {
        prepared_path,
        prepared_sha256,
    } = &native.operation
    {
        Some(decode_prepared(
            &std::fs::read(prepared_path)?,
            prepared_sha256,
            &native.limits,
        )?)
    } else {
        None
    };
    let expected_source =
        super::runtime_child::effective_source(&native.operation, &native.limits)?;
    let expected_source_hash = expected_source
        .as_ref()
        .map(|source| serde_json::to_vec(source).map(|bytes| sha256(&bytes)))
        .transpose()?;
    let request_file = work.path().join("request.json");
    write_json(&request_file, &native)?;
    bounded(
        cancel,
        deadline,
        heartbeat_worker_job(
            &state.queue_pool,
            job.id,
            job.lease_token,
            "study_native",
            0.35,
            None,
            lease_seconds,
        ),
    )
    .await?;
    let child_started = Instant::now();
    let proof = match run_native(&config.binary, &request_file, cancel, deadline, RSS_BYTES).await {
        Ok(proof) => proof,
        Err(error) => {
            if error.to_string() == "study_native_execution_failed"
                && let Some(kind) = read_native_error(&output)
            {
                return Err(NativeRejected(kind.code()).into());
            }
            return Err(error);
        }
    };
    ensure!(proof.reaped, "study_child_not_reaped");
    bounded(cancel,deadline,heartbeat_worker_job(&state.queue_pool,job.id,job.lease_token,"study_artifact",0.8,Some(json!({"nativeElapsedMillis":u64::try_from(child_started.elapsed().as_millis())?,"peakRssBytes":proof.peak_rss_bytes,"childReaped":proof.reaped})),lease_seconds)).await?;
    check_live(cancel, deadline)?;
    ensure!(
        std::fs::metadata(&output)?.len() <= MAX_BYTES,
        "study_artifact_limit"
    );
    let bytes = std::fs::read(&output)?;
    let reference = ArtifactRef {
        r#type: artifact_kind.into(),
        id: Uuid::new_v4(),
        content_hash: sha256(&bytes),
        digest_scope: "artifact_bytes_sha256".into(),
        byte_size: u64::try_from(bytes.len())?,
        media_type: "application/json".into(),
        schema_version: artifact_schema.into(),
    };
    if artifact_kind == "modelInput" {
        let prepared = decode_prepared(&bytes, &reference.content_hash, &Bounds::default())?;
        ensure!(
            prepared.profile_id == PROFILE && prepared.compiler_version == COMPILER,
            "study_prepared_profile"
        );
        let expected = expected_source.as_ref().context("study_prepared_purpose")?;
        ensure!(
            expected_source_hash.as_ref() == Some(&prepared.source_hash)
                && prepared.study_binding == expected.study_binding,
            "study_child_source_binding"
        );
        ensure!(
            prepared.input_hash.is_some() == expected.study_binding.is_some(),
            "study_child_input_identity_binding"
        );
        completion["modelRef"] = json!({"type":"modelRevision","modelRevisionId":Uuid::new_v4(),"contentHash":prepared.model_hash,"digestScope":"worker_prepared_model"});
        completion["inputHash"] = serde_json::to_value(prepared.input_hash)?;
    }
    if artifact_kind == "modelSource" {
        ensure!(
            bytes == std::fs::read(&input_file)?,
            "study_child_snapshot_changed"
        );
    }
    if artifact_kind == "runResult" {
        let result: super::contracts::StudyResult = super::json::from_slice(&bytes)?;
        ensure!(
            result.schema_version == RESULT_SCHEMA
                && result.profile_id == PROFILE
                && result.compiler_version == COMPILER
                && serde_json::to_value(&result.calculation_contract_ref)?
                    == completion["calculationContractRef"]
                && serde_json::to_value(&result.input_hash)? == completion["inputHash"],
            "study_child_result_binding"
        );
        let frozen = expected_run
            .as_ref()
            .context("study_child_result_purpose")?;
        ensure!(
            result.model_hash == frozen.model_hash
                && result.x.len() == frozen.numeric.views.len()
                && result.g.len() == frozen.numeric.inventory_axes.len()
                && result.h.len() == frozen.numeric.method_axes.len()
                && result.coverage == frozen.coverage
                && result.quality_findings == frozen.model.quality_findings
                && !result.business_pilot_qualified
                && result.complete_lcia_scores == frozen.coverage.is_empty(),
            "study_child_result_shape_binding"
        );
        if let NativeAction::Run {
            prepared_sha256, ..
        } = &native.operation
        {
            ensure!(
                result.prepared_artifact_sha256 == *prepared_sha256,
                "study_child_result_artifact_binding"
            );
        }
    }
    if artifact_schema == super::analysis_advanced_contracts::FACTS_SCHEMA {
        let output: super::analysis_advanced_contracts::FactsBundle =
            super::json::from_slice(&bytes)?;
        let expected: super::analysis_advanced_contracts::NativeInput =
            super::json::from_slice(&std::fs::read(&input_file)?)?;
        super::analysis_advanced::validate_output(
            &output,
            &expected.input,
            &std::fs::read(&expected.prepared_path)?,
            &native.limits,
            &expected.batch_limits,
        )?;
    } else if artifact_kind == "analysisFacts" {
        let output: super::analysis_contracts::FactsBundle = super::json::from_slice(&bytes)?;
        let expected: super::analysis_contracts::NativeInput =
            super::json::from_slice(&std::fs::read(&input_file)?)?;
        super::analysis::validate_output(&output, &expected.input)?;
        completion["comparability"] = super::analysis::summary(&output);
    }
    let key = match artifact_kind {
        "modelSource" => "sourceArtifactRef",
        "modelInput" => "inputArtifactRef",
        "analysisFacts" => "factsRef",
        _ => "resultRef",
    };
    completion[key] = serde_json::to_value(&reference)?;
    let object_path = format!(
        "study/{}/{}/job_{}_attempt_{}.json",
        request.study_id, reference.id, job.id, job.attempt_count
    );
    completion["storageBucket"] = json!(BUCKET);
    completion["storageObjectPath"] = json!(object_path);
    let authority = store.authority_fingerprint();
    // Register exact upload ownership before issuing a potentially ambiguous remote PUT.
    keep_orphan(
        config,
        job,
        request.study_id,
        &reference,
        &object_path,
        "upload_pending",
        &authority,
    )?;
    let uploaded = bounded(
        cancel,
        deadline,
        store.upload_object_key_file_bounded(
            &object_path,
            "application/json",
            &output,
            ObjectTransferOptions::new(MAX_BYTES)
                .with_expected_sha256(&reference.content_hash)
                .with_cancellation(cancel.clone())
                .with_request_timeout(Duration::from_secs(20)),
        ),
    )
    .await;
    if let Err(error) = uploaded {
        let _ = keep_orphan(
            config,
            job,
            request.study_id,
            &reference,
            &object_path,
            "upload_outcome_unknown",
            &authority,
        );
        return Err(error);
    }
    let finalize = async {
        let verify = work.path().join("readback.json");
        let location = PrivateArtifact {
            study_id: request.study_id,
            artifact_ref: reference.clone(),
            storage_bucket: BUCKET.into(),
            storage_object_path: object_path.clone(),
        };
        download(
            &store,
            &location,
            request.study_id,
            artifact_kind,
            &verify,
            cancel,
            deadline,
        )
        .await?;
        bounded(
            cancel,
            deadline,
            heartbeat_worker_job(
                &state.queue_pool,
                job.id,
                job.lease_token,
                "study_publish",
                0.95,
                None,
                lease_seconds,
            ),
        )
        .await?;
        check_live(cancel, deadline)?;
        publishing.store(true, Ordering::Release);
        complete(
            &state.pool,
            completion_rpc,
            subject,
            job,
            &completion,
            deadline,
        )
        .await
    }
    .await;
    if let Err(error) = finalize {
        if error.is::<CompletionUnknown>() {
            let _ = keep_orphan(
                config,
                job,
                request.study_id,
                &reference,
                &object_path,
                "completion_outcome_unknown",
                &authority,
            );
        } else if timeout(
            Duration::from_secs(10),
            store.delete_object_key(&object_path),
        )
        .await
        .map_or(true, |v| v.is_err())
        {
            let _ = keep_orphan(
                config,
                job,
                request.study_id,
                &reference,
                &object_path,
                "cleanup_requires_reconciliation",
                &authority,
            );
        } else {
            let _ = keep_orphan(
                config,
                job,
                request.study_id,
                &reference,
                &object_path,
                "deleted_verified",
                &authority,
            );
        }
        return Err(error);
    }
    let _ = std::fs::remove_file(receipt_path(config, job, &reference));
    Ok(())
}

/// Dispatch from the existing solver `worker_jobs` loop; never create a competing queue lifecycle.
pub async fn handle(state: &AppState, job: &WorkerJob, lease_seconds: i32) {
    let deadline = Instant::now() + Duration::from_secs(JOB_SECONDS);
    let result=async {
        let (request,subject)=request(job)?;let config=ExecutionConfig::environment()?;
        let cancel=CancellationToken::default();
        bounded(&cancel,deadline,heartbeat_worker_job(&state.queue_pool,job.id,job.lease_token,"study_start",0.01,None,lease_seconds)).await?;
        let publishing=Arc::new(AtomicBool::new(false));
        let monitor_pool=state.queue_pool.clone();let monitor_cancel=cancel.clone();let monitor_publishing=publishing.clone();
        let id=job.id;let token=job.lease_token;
        let monitor=tokio::spawn(async move {
            let period=lease_heartbeat_period(lease_seconds).min(Duration::from_secs(2));let mut tick=tokio::time::interval(period);tick.tick().await;
            loop {
                tokio::select!{_=tick.tick()=>{},()=tokio::time::sleep(Duration::from_millis(50))=>{if monitor_publishing.load(Ordering::Acquire){return Ok::<(),anyhow::Error>(());}continue;}}
                if monitor_publishing.load(Ordering::Acquire){return Ok(());}
                if Instant::now()>=deadline {monitor_cancel.cancel();anyhow::bail!("study_job_timeout");}
                let renewal=timeout(period,renew_worker_job_lease(&monitor_pool,id,token,lease_seconds)).await;
                if !matches!(renewal,Ok(Ok(()))) {
                    if monitor_publishing.load(Ordering::Acquire){return Ok(());}
                    monitor_cancel.cancel();anyhow::bail!("study_job_lease_lost");
                }
            }
        });
        // A failed monitor sets cancellation; pipeline is then JOINED so native kill/wait and
        // upload disposition run to completion. Dropping its future would only best-effort reap.
        let outcome=pipeline(state,job,&request,subject,&config,&cancel,deadline,&publishing,lease_seconds).await;
        publishing.store(true,Ordering::Release);
        let _=monitor.await;
        outcome
    }.await;
    if let Err(error) = result {
        let code = safe_error(&error);
        if error.is::<CompletionUnknown>() {
            tracing::warn!(worker_job_id=%job.id,code,"Study completion acknowledgement requires authoritative recovery");
            return;
        }
        if let Ok((_, subject)) = request(job) {
            if let Ok(kind) = resource_type(&job.job_kind) {
                let key = format!("study-fail:{}:{}", job.id, job.lease_token);
                // The domain owns failure terminal projection as well; no generic Study result write.
                let tail = format!(",{}", sql_string_literal(&key));
                // Failure acknowledgement shares the original attempt budget. An expired
                // budget dispatches nothing; a dispatched lost reply remains recoverable.
                let outcome = complete_with_retry(deadline, || {
                    task_rpc(
                        &state.pool,
                        "svc_study_task_fail_v1",
                        kind,
                        subject,
                        job.id,
                        job.lease_token,
                        failure_code(&error),
                        &tail,
                    )
                })
                .await;
                if outcome.is_err() {
                    tracing::warn!(worker_job_id=%job.id,code,"Study terminal state requires authoritative recovery");
                }
            }
        } else {
            tracing::warn!(worker_job_id=%job.id,code,"Study job payload requires authoritative recovery");
        }
    }
}

/// Reconcile a private upload receipt only against canonical DB job/attachment facts and the
/// original storage authority. Absence alone never proves an interrupted PUT cannot arrive late.
pub async fn reconcile(state: &AppState, path: &Path) -> anyhow::Result<&'static str> {
    ensure!(
        std::fs::metadata(path)?.len() <= 8192,
        "study_orphan_receipt_limit"
    );
    let receipt: OrphanReceipt = super::json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        receipt.schema_version == "lca.study-orphan-artifact.v1",
        "study_orphan_receipt_schema"
    );
    let store = state.object_store.study_artifacts();
    ensure!(
        store.authority_fingerprint() == receipt.storage_authority_hash,
        "study_orphan_storage_authority_changed"
    );
    let location = PrivateArtifact {
        study_id: receipt.study_id,
        artifact_ref: receipt.artifact_ref.clone(),
        storage_bucket: receipt.storage_bucket.clone(),
        storage_object_path: receipt.storage_object_path.clone(),
    };
    validate_location(&location, receipt.study_id, &receipt.artifact_ref.r#type)?;
    ensure!(
        receipt.storage_object_path.ends_with(&format!(
            "/job_{}_attempt_{}.json",
            receipt.worker_job_id, receipt.attempt_count
        )),
        "study_orphan_job_path_binding"
    );
    let tail = format!(",'{}'::uuid", receipt.artifact_ref.id);
    let decision = task_rpc(
        &state.pool,
        "svc_study_task_reconcile_v1",
        &receipt.resource_type,
        receipt.resource_id,
        receipt.worker_job_id,
        receipt.lease_token,
        &receipt.idempotency_key,
        &tail,
    )
    .await?;
    match decision["state"].as_str() {
        Some("committed" | "referenced") => {
            std::fs::remove_file(path)?;
            return Ok("attached");
        }
        Some("unreferenced") => {}
        Some("pending") => return Ok("waiting_for_authoritative_outcome"),
        _ => anyhow::bail!("study_orphan_reconcile_state"),
    }
    let directory = tempfile::tempdir()?;
    let copy = directory.path().join("orphan.json");
    match timeout(
        Duration::from_secs(20),
        store.download_object_key_to_file(
            &receipt.storage_object_path,
            &copy,
            ObjectTransferOptions::new(MAX_BYTES)
                .with_expected_sha256(&receipt.artifact_ref.content_hash),
        ),
    )
    .await?
    {
        Ok(value) => ensure!(
            value.byte_size == receipt.artifact_ref.byte_size,
            "study_orphan_byte_size"
        ),
        Err(error)
            if error
                .downcast_ref::<crate::storage::ObjectDownloadError>()
                .is_some_and(|value| value.status == reqwest::StatusCode::NOT_FOUND) =>
        {
            return Ok("absent_receipt_retained");
        }
        Err(error) => return Err(error),
    }
    timeout(
        Duration::from_secs(20),
        store.delete_object_key(&receipt.storage_object_path),
    )
    .await??;
    let mut audit = receipt;
    audit.reason = "deleted_verified".into();
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("study_orphan_parent")?)?;
    file.write_all(&serde_json::to_vec(&audit)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok("deleted")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job() -> WorkerJob {
        let study = Uuid::new_v4();
        WorkerJob {
            id: Uuid::new_v4(),
            job_kind: "study.run".into(),
            worker_queue: "solver".into(),
            payload_schema_version: "study.run.request.v1".into(),
            payload: json!({"schemaVersion":"study.run.request.v1","studyId":study,"runId":Uuid::new_v4()}),
            requested_by: Some(Uuid::new_v4()),
            lease_token: Uuid::new_v4(),
            attempt_count: 1,
        }
    }
    #[test]
    fn dispatch_matches_observed_database_service_seams_and_ordered_arguments() {
        let producer: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/study-runtime/database-service-seams.json"
        ))
        .unwrap();
        assert_eq!(
            producer["schemaVersion"],
            "lca.study-database-service-seams.v1"
        );
        let functions = producer["functions"].as_array().unwrap();
        for (kind, subject) in [
            ("study.source_acquire", "p_acquisition_id"),
            ("study.model_prepare", "p_preparation_id"),
            ("study.run", "p_run_id"),
        ] {
            for (name, completion) in [
                (input_rpc(kind).unwrap(), false),
                (completion_rpc(kind).unwrap(), true),
            ] {
                let matches: Vec<_> = functions.iter().filter(|row| row["name"] == name).collect();
                assert_eq!(matches.len(), 1, "exact producer function for {kind}");
                let function = matches[0];
                assert_eq!(function["schema"], "api");
                assert_eq!(function["result"], "jsonb");
                assert_eq!(function["serviceExecute"], true);
                assert_eq!(function["actorExecute"], false);
                let mut names = vec![subject, "p_job_id", "p_lease_token"];
                let mut types = vec!["uuid", "uuid", "uuid"];
                if completion {
                    names.extend(["p_completion", "p_idempotency_key"]);
                    types.extend(["jsonb", "text"]);
                }
                assert_eq!(function["argumentNames"], json!(names));
                assert_eq!(
                    function["identityArguments"],
                    names
                        .iter()
                        .zip(types)
                        .map(|(name, typ)| format!("{name} {typ}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        assert!(input_rpc("study.unknown").is_err());
        assert!(completion_rpc("study.unknown").is_err());
    }
    #[test]
    fn request_is_ids_only_and_public_errors_do_not_echo_payload() {
        let mut value = job();
        assert!(request(&value).is_ok());
        value.payload["authorization"] = json!("private-token-must-not-escape");
        let error = request(&value).unwrap_err();
        assert_eq!(safe_error(&error), "study_runtime_failed");
        assert!(!safe_error(&error).contains("token"));
        value = job();
        value.payload["preparationId"] = json!(Uuid::new_v4());
        assert!(request(&value).is_err());
        value = job();
        value.requested_by = None;
        assert!(request(&value).is_err());
    }
    #[test]
    fn analysis_dispatch_uses_exact_declared_seams_and_only_one_subject() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/study-analysis/service-seams.json"
        ))
        .unwrap();
        assert_eq!(resource_type("study.analysis").unwrap(), "analysis");
        for (name, complete) in [
            (input_rpc("study.analysis").unwrap(), false),
            (completion_rpc("study.analysis").unwrap(), true),
        ] {
            let function = fixture["functions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .unwrap();
            let mut arguments = vec!["p_analysis_id", "p_job_id", "p_lease_token"];
            if complete {
                arguments.extend(["p_completion", "p_idempotency_key"]);
            }
            assert_eq!(function["argumentNames"], json!(arguments));
            assert_eq!(function["serviceExecute"], true);
            assert_eq!(function["actorExecute"], false);
        }
        let mut value = job();
        value.job_kind = "study.analysis".into();
        value.payload_schema_version = "study.analysis.request.v1".into();
        let subject = Uuid::new_v4();
        value.payload = json!({"schemaVersion":"study.analysis.request.v1","studyId":Uuid::new_v4(),"analysisId":subject});
        assert_eq!(request(&value).unwrap().1, subject);
        value.payload["runId"] = json!(Uuid::new_v4());
        assert!(request(&value).is_err());
    }
    #[test]
    fn advanced_admission_and_job_bindings_fail_closed_without_changing_old_jobs() {
        for (enabled, parameters, evaluations) in [
            (None, Some("8"), Some("129")),
            (Some("false"), Some("8"), Some("129")),
            (Some("TRUE"), Some("8"), Some("129")),
            (Some("true"), None, Some("129")),
            (Some("true"), Some("0"), Some("129")),
            (Some("true"), Some("8"), Some("2")),
            (Some("true"), Some("+8"), Some("129")),
            (Some("true"), Some("8"), Some("4294967296")),
        ] {
            assert!(advanced_batch_limits(enabled, parameters, evaluations).is_err());
        }
        let batch = advanced_batch_limits(Some("true"), Some("8"), Some("129")).unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/study-analysis/advanced/service-seams.json"
        ))
        .unwrap();
        let input: super::super::analysis_advanced_contracts::ServiceInput =
            serde_json::from_value(fixture["readInputResponse"]["data"].clone()).unwrap();
        let mut value = job();
        value.id = input.analysis.execution.job_id;
        value.job_kind = "study.analysis_advanced".into();
        value.payload_schema_version = "study.analysis_advanced.request.v1".into();
        value.payload = json!({"schemaVersion":value.payload_schema_version,"studyId":input.analysis.study_id,"analysisId":input.analysis.analysis_id});
        let (request, subject) = request(&value).unwrap();
        advanced_job_binding(&input, &request, subject, &value, &batch).unwrap();
        assert_eq!(resource_type(&value.job_kind).unwrap(), "analysis");
        assert!(handles(&value.job_kind));
        for name in [
            input_rpc(&value.job_kind).unwrap(),
            completion_rpc(&value.job_kind).unwrap(),
        ] {
            let row = fixture["functions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .unwrap();
            assert_eq!(row["serviceExecute"], true);
            assert_eq!(row["actorExecute"], false);
            let mut args = vec!["p_analysis_id", "p_job_id", "p_lease_token"];
            if name.contains("complete") {
                args.extend(["p_completion", "p_idempotency_key"]);
            }
            assert_eq!(row["argumentNames"], json!(args));
        }
        value.id = Uuid::new_v4();
        assert!(advanced_job_binding(&input, &request, subject, &value, &batch).is_err());
        value.id = input.analysis.execution.job_id;
        assert!(advanced_job_binding(&input, &request, Uuid::new_v4(), &value, &batch).is_err());
        let tiny = super::super::analysis_advanced_contracts::BatchLimits {
            max_parameters: 0,
            max_evaluations: 2,
        };
        assert!(advanced_job_binding(&input, &request, subject, &value, &tiny).is_err());
        let mut wrong_request = request;
        wrong_request.study_id = Uuid::new_v4();
        assert!(advanced_job_binding(&input, &wrong_request, subject, &value, &batch).is_err());
    }

    #[test]
    fn private_storage_location_is_bound_to_study_and_registered_artifact() {
        let study = Uuid::new_v4();
        let id = Uuid::new_v4();
        let mut value = PrivateArtifact {
            study_id: study,
            storage_bucket: BUCKET.into(),
            storage_object_path: format!("study/{study}/{id}/input.json"),
            artifact_ref: ArtifactRef {
                r#type: "modelInput".into(),
                id,
                content_hash: "a".repeat(64),
                digest_scope: "artifact_bytes_sha256".into(),
                byte_size: 1,
                media_type: "application/json".into(),
                schema_version: INPUT_SCHEMA.into(),
            },
        };
        assert!(validate_location(&value, study, "modelInput").is_ok());
        value.storage_object_path.push_str("/../foreign.json");
        assert!(validate_location(&value, study, "modelInput").is_err());
        value.storage_object_path = format!("study/{study}/{id}/input.json");
        assert!(validate_location(&value, Uuid::new_v4(), "modelInput").is_err());
        value.storage_bucket = "foreign".into();
        assert!(validate_location(&value, study, "modelInput").is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn readiness_wait_is_bounded_and_expired_or_cancelled_work_is_not_polled() {
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        for (token, deadline) in [
            (cancelled, Instant::now() + Duration::from_secs(1)),
            (CancellationToken::default(), Instant::now()),
        ] {
            let polled = AtomicBool::new(false);
            assert!(
                bounded(&token, deadline, async {
                    polled.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .is_err()
            );
            assert!(!polled.load(Ordering::SeqCst));
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let owned = Dropped(dropped.clone());
        let start = Instant::now();
        let error = bounded(
            &CancellationToken::default(),
            start + Duration::from_millis(100),
            async move {
                let _owned = owned;
                std::future::pending::<anyhow::Result<()>>().await
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "study_job_timeout");
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(Instant::now() - start, Duration::from_millis(100));
    }
    #[test]
    fn observed_database_time_is_structural_not_host_clock_eligibility() {
        let job = job();
        let actor = job.requested_by.unwrap().to_string();
        let origin = json!({"type":"datasetRevision","datasetType":"lifecyclemodel","id":"source","version":"1"});
        let mut packet = SourceSnapshot {
            schema_version: SNAPSHOT_SCHEMA.into(),
            origin_ref: origin.clone(),
            license_ref: "policy".into(),
            issuer_id: "database.study-source.v1".into(),
            observed_at: "2000-01-01T00:00:00Z".into(),
            source_authorization: json!({"schemaVersion":"lca.study-source-authorization.v1","requestedBy":actor,"originRef":origin,"sourceEncoding":"postgresql.json-text.utf8.v1","sourceScope":[]}),
            source: super::super::contracts::SourceInput {
                schema_version: super::super::contracts::SOURCE_SCHEMA.into(),
                origin: DatasetRef {
                    id: "source".into(),
                    version: "1".into(),
                },
                actor_id: Some(actor),
                documents: vec![],
                method_refs: vec![],
                cutoffs: vec![],
                overrides: vec![],
                study_binding: None,
                quality_findings: vec![],
            },
        };
        // No claim of a usable numerical model: this only tests the source-time ownership check.
        assert!(verify_packet(&packet, &job, &origin).is_ok());
        packet.source.actor_id = Some(Uuid::new_v4().to_string());
        assert!(verify_packet(&packet, &job, &origin).is_err());
    }
}

#[cfg(test)]
mod callback_race_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    #[tokio::test]
    async fn expired_local_budget_never_dispatches_callback() {
        let calls = AtomicUsize::new(0);
        let outcome = complete_with_retry(Instant::now(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(json!({})) }
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            outcome
                .unwrap_err()
                .to_string()
                .contains("study_job_timeout")
        );
    }
    #[tokio::test]
    async fn dispatched_timeout_can_commit_and_stays_unknown_not_remote_abort() {
        let committed = Arc::new(AtomicBool::new(false));
        let marker = committed.clone();
        let server = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            marker.store(true, Ordering::SeqCst);
        });
        let calls = AtomicUsize::new(0);
        let error = complete_with_retry(Instant::now() + Duration::from_millis(60), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(json!({}))
            }
        })
        .await
        .unwrap_err();
        server.await.unwrap();
        assert!(committed.load(Ordering::SeqCst));
        assert!(error.is::<CompletionUnknown>());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn lost_ack_then_authorization_denial_preserves_unknown_completion() {
        let committed = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let error = complete_with_retry(Instant::now() + Duration::from_secs(2), || {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                committed.store(true, Ordering::SeqCst);
            }
            async move {
                if call == 0 {
                    anyhow::bail!("simulated_committed_reply_lost");
                }
                Err(DomainRejected.into())
            }
        })
        .await
        .unwrap_err();
        assert!(committed.load(Ordering::SeqCst));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Both publication cleanup and top-level failure handling retain an
        // unknown completion: no object deletion or contradictory failure RPC.
        assert!(error.is::<CompletionUnknown>());
        assert!(!error.is::<DomainRejected>());
    }

    #[tokio::test]
    async fn identical_receipt_retry_recovers_ack_and_rejection_is_not_unknown() {
        let calls = AtomicUsize::new(0);
        complete_with_retry(Instant::now() + Duration::from_secs(2), || {
            let count = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if count == 0 {
                    anyhow::bail!("simulated_lost_ack");
                }
                Ok(json!({"state":"committed"}))
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let calls = AtomicUsize::new(0);
        let error = complete_with_retry(Instant::now() + Duration::from_secs(2), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(DomainRejected.into()) }
        })
        .await
        .unwrap_err();
        assert!(error.is::<DomainRejected>());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
