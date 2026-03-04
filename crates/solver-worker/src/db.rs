use serde_json::Value;
use solver_core::{
    ModelSparseData, NumericOptions, PrepareResult, SolveBatchResult, SolveOptions, SolveResult,
    SolverService, SparseTriplet,
};
use sqlx::{PgPool, Row};
use tracing::{instrument, warn};
use uuid::Uuid;

use crate::{
    artifacts::{EncodedArtifact, encode_solve_batch_artifact, encode_solve_one_artifact},
    config::AppConfig,
    snapshot_artifacts::decode_snapshot_artifact,
    storage::ObjectStoreClient,
    types::{JobPayload, SolveOptionsPayload},
};

/// Queue message from pgmq.read.
#[derive(Debug, Clone)]
pub struct QueueMessage {
    /// pgmq message id.
    pub msg_id: i64,
    /// Raw payload.
    pub payload: Value,
}

/// App state shared by worker and HTTP server.
#[derive(Debug)]
pub struct AppState {
    /// DB pool.
    pub pool: PgPool,
    /// Core solver service.
    pub solver: SolverService,
    /// Optional object storage for large result artifacts.
    pub object_store: Option<ObjectStoreClient>,
    /// Threshold for keeping result payload inline in DB.
    pub result_inline_max_bytes: usize,
}

impl AppState {
    /// Creates app state with DB pool and optional object storage.
    pub async fn new(config: &AppConfig) -> anyhow::Result<Self> {
        let pool = PgPool::connect(config.resolved_database_url()?).await?;

        let s3_any_set = config.s3_endpoint.is_some()
            || config.s3_region.is_some()
            || config.s3_bucket.is_some()
            || config.s3_access_key_id.is_some()
            || config.s3_secret_access_key.is_some()
            || config.s3_session_token.is_some();

        let object_store = if s3_any_set {
            let endpoint = config.s3_endpoint.as_deref().ok_or_else(|| {
                anyhow::anyhow!("incomplete S3 config: missing S3_ENDPOINT for object storage")
            })?;
            let region = config.s3_region.as_deref().ok_or_else(|| {
                anyhow::anyhow!("incomplete S3 config: missing S3_REGION for object storage")
            })?;
            let bucket = config.s3_bucket.as_deref().ok_or_else(|| {
                anyhow::anyhow!("incomplete S3 config: missing S3_BUCKET for object storage")
            })?;
            let access_key_id = config.s3_access_key_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!("incomplete S3 config: missing S3_ACCESS_KEY_ID for object storage")
            })?;
            let secret_access_key = config.s3_secret_access_key.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "incomplete S3 config: missing S3_SECRET_ACCESS_KEY for object storage"
                )
            })?;

            Some(ObjectStoreClient::new(
                endpoint,
                region,
                bucket,
                &config.s3_prefix,
                access_key_id,
                secret_access_key,
                config.s3_session_token.clone(),
            )?)
        } else {
            None
        };

        Ok(Self {
            pool,
            solver: SolverService::new(),
            object_store,
            result_inline_max_bytes: config.result_inline_max_bytes,
        })
    }
}

/// Reads one message from pgmq queue.
#[instrument(skip(pool))]
pub async fn read_one_queue_message(
    pool: &PgPool,
    queue_name: &str,
    vt_seconds: i32,
) -> anyhow::Result<Option<QueueMessage>> {
    let row = sqlx::query(
        r"
        SELECT msg_id, message
        FROM pgmq.read($1, $2, $3)
        LIMIT 1
        ",
    )
    .bind(queue_name)
    .bind(vt_seconds)
    .bind(1_i32)
    .fetch_optional(pool)
    .await?;

    row.map(|r| {
        Ok(QueueMessage {
            msg_id: r.try_get::<i64, _>("msg_id")?,
            payload: r.try_get::<Value, _>("message")?,
        })
    })
    .transpose()
}

/// Archives processed message.
#[instrument(skip(pool))]
pub async fn archive_queue_message(
    pool: &PgPool,
    queue_name: &str,
    msg_id: i64,
) -> anyhow::Result<()> {
    let _ = sqlx::query("SELECT pgmq.archive($1, $2)")
        .bind(queue_name)
        .bind(msg_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Updates `lca_jobs` status and diagnostics.
#[instrument(skip(pool, diagnostics))]
pub async fn update_job_status(
    pool: &PgPool,
    job_id: Uuid,
    status: &str,
    diagnostics: Value,
) -> anyhow::Result<()> {
    let _ = sqlx::query(
        r"
        UPDATE lca_jobs
        SET status = $2,
            diagnostics = $3::jsonb,
            updated_at = NOW(),
            started_at = CASE WHEN $2 = 'running' AND started_at IS NULL THEN NOW() ELSE started_at END,
            finished_at = CASE WHEN $2 IN ('completed','failed') THEN NOW() ELSE finished_at END
        WHERE id = $1
        ",
    )
    .bind(job_id)
    .bind(status)
    .bind(diagnostics)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Default)]
struct ResultInsert {
    payload: Option<Value>,
    diagnostics: Value,
    artifact_url: Option<String>,
    artifact_sha256: Option<String>,
    artifact_byte_size: Option<i64>,
    artifact_format: Option<String>,
}

/// Inserts one `lca_results` row.
#[instrument(skip(pool, data))]
async fn insert_result(
    pool: &PgPool,
    job_id: Uuid,
    snapshot_id: Uuid,
    data: ResultInsert,
) -> anyhow::Result<()> {
    let _ = sqlx::query(
        r"
        INSERT INTO lca_results (
            job_id,
            snapshot_id,
            payload,
            diagnostics,
            artifact_url,
            artifact_sha256,
            artifact_byte_size,
            artifact_format,
            created_at
        )
        VALUES ($1, $2, $3::jsonb, $4::jsonb, $5, $6, $7, $8, NOW())
        ",
    )
    .bind(job_id)
    .bind(snapshot_id)
    .bind(data.payload)
    .bind(data.diagnostics)
    .bind(data.artifact_url)
    .bind(data.artifact_sha256)
    .bind(data.artifact_byte_size)
    .bind(data.artifact_format)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct SnapshotArtifactMeta {
    artifact_url: String,
    artifact_format: String,
}

/// Loads sparse snapshot data from snapshot artifact first, then falls back to `lca_*` tables.
#[instrument(skip(state))]
pub async fn fetch_snapshot_sparse_data(
    state: &AppState,
    snapshot_id: Uuid,
) -> anyhow::Result<ModelSparseData> {
    if let Some(meta) = fetch_snapshot_artifact_meta(&state.pool, snapshot_id).await? {
        match fetch_snapshot_payload_from_artifact(state, snapshot_id, &meta).await {
            Ok(payload) => return Ok(payload),
            Err(err) => {
                warn!(
                    snapshot_id = %snapshot_id,
                    artifact_format = %meta.artifact_format,
                    error = %err,
                    "failed to load snapshot artifact, falling back to table-backed sparse data"
                );
            }
        }
    }

    match fetch_snapshot_sparse_data_from_tables(&state.pool, snapshot_id).await {
        Ok(data) => Ok(data),
        Err(err) => {
            if let Some(sqlx_err) = err.downcast_ref::<sqlx::Error>()
                && is_undefined_table(sqlx_err)
            {
                return Err(anyhow::anyhow!(
                    "snapshot {snapshot_id} has no readable artifact and legacy lca_* matrix tables are missing"
                ));
            }
            Err(err)
        }
    }
}

#[instrument(skip(pool))]
async fn fetch_snapshot_sparse_data_from_tables(
    pool: &PgPool,
    snapshot_id: Uuid,
) -> anyhow::Result<ModelSparseData> {
    let process_count = fetch_process_count(pool, snapshot_id).await?;
    let flow_count = fetch_flow_count(pool, snapshot_id).await?;
    let impact_count = fetch_impact_count(pool, snapshot_id).await?;

    let technosphere_entries = fetch_triplets(
        pool,
        snapshot_id,
        r#"
        SELECT "row" AS row_idx, "col" AS col_idx, value
        FROM lca_technosphere_entries
        WHERE snapshot_id = $1
        "#,
    )
    .await?;

    let biosphere_entries = fetch_triplets(
        pool,
        snapshot_id,
        r#"
        SELECT "row" AS row_idx, "col" AS col_idx, value
        FROM lca_biosphere_entries
        WHERE snapshot_id = $1
        "#,
    )
    .await?;

    let characterization_factors = fetch_triplets(
        pool,
        snapshot_id,
        r#"
        SELECT "row" AS row_idx, "col" AS col_idx, value
        FROM lca_characterization_factors
        WHERE snapshot_id = $1
        "#,
    )
    .await?;

    Ok(ModelSparseData {
        model_version: snapshot_id,
        process_count,
        flow_count,
        impact_count,
        technosphere_entries,
        biosphere_entries,
        characterization_factors,
    })
}

async fn fetch_snapshot_artifact_meta(
    pool: &PgPool,
    snapshot_id: Uuid,
) -> anyhow::Result<Option<SnapshotArtifactMeta>> {
    let row = match sqlx::query(
        r"
        SELECT artifact_url, artifact_format
        FROM lca_snapshot_artifacts
        WHERE snapshot_id = $1
          AND status = 'ready'
        ORDER BY created_at DESC
        LIMIT 1
        ",
    )
    .bind(snapshot_id)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        Err(err) if is_undefined_table(&err) => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    row.map(|r| {
        Ok(SnapshotArtifactMeta {
            artifact_url: r.try_get::<String, _>("artifact_url")?,
            artifact_format: r.try_get::<String, _>("artifact_format")?,
        })
    })
    .transpose()
}

async fn fetch_snapshot_payload_from_artifact(
    state: &AppState,
    snapshot_id: Uuid,
    meta: &SnapshotArtifactMeta,
) -> anyhow::Result<ModelSparseData> {
    let bytes = if let Some(store) = &state.object_store {
        store.download_object_url(&meta.artifact_url).await?
    } else {
        let response = reqwest::get(&meta.artifact_url).await?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "artifact download failed status={} url={}",
                response.status(),
                meta.artifact_url
            ));
        }
        response.bytes().await?.to_vec()
    };

    let decoded = decode_snapshot_artifact(bytes.as_slice())?;
    if decoded.snapshot_id != snapshot_id {
        return Err(anyhow::anyhow!(
            "artifact snapshot mismatch: expected={} got={}",
            snapshot_id,
            decoded.snapshot_id
        ));
    }

    Ok(decoded.payload)
}

fn is_undefined_table(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.code().as_deref() == Some("42P01"),
        _ => false,
    }
}

/// Executes one queue payload end-to-end.
#[instrument(skip(state))]
#[allow(clippy::too_many_lines)]
pub async fn handle_job_payload(state: &AppState, payload: JobPayload) -> anyhow::Result<()> {
    match payload {
        JobPayload::PrepareFactorization {
            job_id,
            snapshot_id,
            print_level,
        } => {
            update_job_status(
                &state.pool,
                job_id,
                "running",
                serde_json::json!({"phase": "prepare"}),
            )
            .await?;

            let data = fetch_snapshot_sparse_data(state, snapshot_id).await?;
            let prepared = state.solver.prepare(
                &data,
                NumericOptions {
                    print_level: print_level.unwrap_or(0.0),
                },
            )?;

            update_job_status(
                &state.pool,
                job_id,
                "ready",
                serde_json::to_value(prepared)?,
            )
            .await?;
        }
        JobPayload::SolveOne {
            job_id,
            snapshot_id,
            rhs,
            solve,
            print_level,
        } => {
            update_job_status(
                &state.pool,
                job_id,
                "running",
                serde_json::json!({"phase": "solve_one"}),
            )
            .await?;

            let level = print_level.unwrap_or(0.0);
            ensure_prepared(state, snapshot_id, level).await?;
            let solved = state.solver.solve_one(
                snapshot_id,
                NumericOptions { print_level: level },
                &rhs,
                to_core_solve_options(solve),
            )?;

            let result_diag = persist_solve_one_result(state, job_id, snapshot_id, &solved).await?;
            update_job_status(
                &state.pool,
                job_id,
                "completed",
                serde_json::json!({"result": "stored", "storage": result_diag}),
            )
            .await?;
        }
        JobPayload::SolveBatch {
            job_id,
            snapshot_id,
            rhs_batch,
            solve,
            print_level,
        } => {
            update_job_status(
                &state.pool,
                job_id,
                "running",
                serde_json::json!({"phase": "solve_batch"}),
            )
            .await?;

            let level = print_level.unwrap_or(0.0);
            ensure_prepared(state, snapshot_id, level).await?;
            let solved = state.solver.solve_batch(
                snapshot_id,
                NumericOptions { print_level: level },
                &rhs_batch,
                to_core_solve_options(solve),
            )?;

            let result_diag =
                persist_solve_batch_result(state, job_id, snapshot_id, &solved).await?;
            update_job_status(
                &state.pool,
                job_id,
                "completed",
                serde_json::json!({"result": "stored", "storage": result_diag}),
            )
            .await?;
        }
        JobPayload::InvalidateFactorization {
            job_id,
            snapshot_id,
        } => {
            let invalidated = state.solver.invalidate(snapshot_id);
            update_job_status(
                &state.pool,
                job_id,
                "completed",
                serde_json::json!({"invalidated": invalidated}),
            )
            .await?;
        }
        JobPayload::RebuildFactorization {
            job_id,
            snapshot_id,
            print_level,
        } => {
            let _ = state.solver.invalidate(snapshot_id);
            let data = fetch_snapshot_sparse_data(state, snapshot_id).await?;
            let prepared: PrepareResult = state.solver.prepare(
                &data,
                NumericOptions {
                    print_level: print_level.unwrap_or(0.0),
                },
            )?;
            update_job_status(
                &state.pool,
                job_id,
                "ready",
                serde_json::to_value(prepared)?,
            )
            .await?;
        }
    }

    Ok(())
}

/// Ensures factorization exists in cache.
pub async fn ensure_prepared(
    state: &AppState,
    snapshot_id: Uuid,
    print_level: f64,
) -> anyhow::Result<()> {
    if state
        .solver
        .factorization_status(snapshot_id, NumericOptions { print_level })
        .is_none()
    {
        let data = fetch_snapshot_sparse_data(state, snapshot_id).await?;
        let _ = state
            .solver
            .prepare(&data, NumericOptions { print_level })?;
    }
    Ok(())
}

async fn persist_solve_one_result(
    state: &AppState,
    job_id: Uuid,
    snapshot_id: Uuid,
    solved: &SolveResult,
) -> anyhow::Result<Value> {
    let payload_json = serde_json::to_value(solved)?;
    let encoded = encode_solve_one_artifact(snapshot_id, job_id, solved)?;

    persist_result_payload(
        state,
        job_id,
        snapshot_id,
        "solve_one",
        payload_json,
        encoded,
    )
    .await
}

async fn persist_solve_batch_result(
    state: &AppState,
    job_id: Uuid,
    snapshot_id: Uuid,
    solved: &SolveBatchResult,
) -> anyhow::Result<Value> {
    let payload_json = serde_json::to_value(solved)?;
    let encoded = encode_solve_batch_artifact(snapshot_id, job_id, solved)?;

    persist_result_payload(
        state,
        job_id,
        snapshot_id,
        "solve_batch",
        payload_json,
        encoded,
    )
    .await
}

async fn persist_result_payload(
    state: &AppState,
    job_id: Uuid,
    snapshot_id: Uuid,
    suffix: &str,
    payload_json: Value,
    encoded: EncodedArtifact,
) -> anyhow::Result<Value> {
    let encoded_len = encoded.bytes.len();
    let artifact_len = i64::try_from(encoded_len).ok();

    if encoded_len > state.result_inline_max_bytes
        && let Some(object_store) = &state.object_store
    {
        match object_store
            .upload_result(
                snapshot_id,
                job_id,
                suffix,
                encoded.extension,
                encoded.content_type,
                encoded.bytes,
            )
            .await
        {
            Ok(url) => {
                let diagnostics = serde_json::json!({
                    "storage": "object_storage",
                    "artifact_format": encoded.format,
                    "artifact_sha256": encoded.sha256,
                    "artifact_bytes": encoded_len,
                    "artifact_url": url,
                });

                insert_result(
                    &state.pool,
                    job_id,
                    snapshot_id,
                    ResultInsert {
                        payload: None,
                        diagnostics: diagnostics.clone(),
                        artifact_url: Some(url),
                        artifact_sha256: Some(encoded.sha256),
                        artifact_byte_size: artifact_len,
                        artifact_format: Some(encoded.format.to_owned()),
                    },
                )
                .await?;

                return Ok(diagnostics);
            }
            Err(err) => {
                warn!(error = %err, "artifact upload failed; falling back to inline payload storage");
            }
        }
    }

    let diagnostics = serde_json::json!({
        "storage": "inline_json",
        "artifact_format": encoded.format,
        "encoded_bytes": encoded_len,
    });

    insert_result(
        &state.pool,
        job_id,
        snapshot_id,
        ResultInsert {
            payload: Some(payload_json),
            diagnostics: diagnostics.clone(),
            artifact_url: None,
            artifact_sha256: None,
            artifact_byte_size: None,
            artifact_format: Some(encoded.format.to_owned()),
        },
    )
    .await?;

    Ok(diagnostics)
}

fn to_core_solve_options(solve: SolveOptionsPayload) -> SolveOptions {
    SolveOptions {
        return_x: solve.return_x,
        return_g: solve.return_g,
        return_h: solve.return_h,
    }
}

async fn fetch_process_count(pool: &PgPool, snapshot_id: Uuid) -> anyhow::Result<i32> {
    let count: i64 = sqlx::query_scalar(
        r"
        SELECT COUNT(*)::bigint
        FROM lca_process_index
        WHERE snapshot_id = $1
        ",
    )
    .bind(snapshot_id)
    .fetch_one(pool)
    .await?;

    i32::try_from(count).map_err(|_| anyhow::anyhow!("process count overflow: {count}"))
}

async fn fetch_flow_count(pool: &PgPool, snapshot_id: Uuid) -> anyhow::Result<i32> {
    let count: i64 = sqlx::query_scalar(
        r"
        SELECT COUNT(*)::bigint
        FROM lca_flow_index
        WHERE snapshot_id = $1
        ",
    )
    .bind(snapshot_id)
    .fetch_one(pool)
    .await?;

    i32::try_from(count).map_err(|_| anyhow::anyhow!("flow count overflow: {count}"))
}

async fn fetch_impact_count(pool: &PgPool, snapshot_id: Uuid) -> anyhow::Result<i32> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COALESCE(MAX("row"), -1)::bigint + 1
        FROM lca_characterization_factors
        WHERE snapshot_id = $1
        "#,
    )
    .bind(snapshot_id)
    .fetch_one(pool)
    .await?;

    i32::try_from(count).map_err(|_| anyhow::anyhow!("impact count overflow: {count}"))
}

async fn fetch_triplets(
    pool: &PgPool,
    snapshot_id: Uuid,
    sql: &str,
) -> anyhow::Result<Vec<SparseTriplet>> {
    let rows = sqlx::query(sql).bind(snapshot_id).fetch_all(pool).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(SparseTriplet {
            row: row.try_get::<i32, _>("row_idx")?,
            col: row.try_get::<i32, _>("col_idx")?,
            value: row.try_get::<f64, _>("value")?,
        });
    }

    Ok(out)
}

#[allow(dead_code)]
fn _assert_result_types(_a: SolveResult, _b: SolveBatchResult) {}
