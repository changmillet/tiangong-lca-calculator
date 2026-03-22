use std::sync::Arc;

use clap::Parser;
use serde_json::json;
use solver_worker::{
    config::AppConfig,
    db::{AppState, archive_queue_message, read_one_queue_message},
    package_db::{
        extract_package_job_id, extract_package_job_id_from_raw_payload,
        handle_package_job_payload, is_retryable_package_job_error,
        mark_package_request_cache_failed, reschedule_retryable_package_job,
        update_package_job_status,
    },
    package_execution::clear_runtime_export_traversal_cache,
    package_types::{PACKAGE_QUEUE_NAME, PackageJobPayload},
};
use tokio::time::sleep;
use tracing::{error, info, instrument, warn};

#[derive(Debug, Clone, Parser)]
#[command(name = "package-worker")]
struct PackageWorkerCli {
    #[command(flatten)]
    app: AppConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = PackageWorkerCli::parse();
    let state = Arc::new(AppState::new(&cli.app).await?);
    let queue_name = resolve_queue_name(&cli.app.pgmq_queue);

    if cli.app.pgmq_queue == "lca_jobs" {
        info!(
            queue = %queue_name,
            "using package queue default instead of solver-worker queue default"
        );
    }

    run_package_worker_loop(
        state,
        queue_name,
        cli.app.worker_vt_seconds,
        cli.app.poll_interval(),
    )
    .await
}

#[instrument(skip(state))]
#[allow(clippy::too_many_lines)]
async fn run_package_worker_loop(
    state: Arc<AppState>,
    queue_name: String,
    vt_seconds: i32,
    poll_interval: std::time::Duration,
) -> anyhow::Result<()> {
    loop {
        match read_one_queue_message(&state.pool, &queue_name, vt_seconds).await {
            Ok(Some(message)) => {
                let parsed = serde_json::from_value::<PackageJobPayload>(message.payload.clone());
                match parsed {
                    Ok(payload) => {
                        if let Err(err) = handle_package_job_payload(&state, payload.clone()).await
                        {
                            error!(error = %err, "package job execution failed");
                            let job_id = extract_package_job_id(&payload);
                            let err_message = err.to_string();
                            let mut rescheduled = false;
                            clear_runtime_export_traversal_cache(job_id);
                            if is_retryable_package_job_error(&err) {
                                match reschedule_retryable_package_job(
                                    &state.pool,
                                    &payload,
                                    &err_message,
                                )
                                .await
                                {
                                    Ok(true) => {
                                        rescheduled = true;
                                    }
                                    Ok(false) => {
                                        warn!(
                                            job_id = %job_id,
                                            error = %err_message,
                                            "package job retry budget exhausted"
                                        );
                                    }
                                    Err(retry_err) => {
                                        warn!(
                                            job_id = %job_id,
                                            error = %retry_err,
                                            original_error = %err_message,
                                            "failed to reschedule retryable package job"
                                        );
                                    }
                                }
                            }
                            if !rescheduled {
                                let _ = update_package_job_status(
                                    &state.pool,
                                    job_id,
                                    "failed",
                                    json!({"error": err_message}),
                                )
                                .await;
                                let _ = mark_package_request_cache_failed(
                                    &state.pool,
                                    job_id,
                                    "job_execution_failed",
                                    &err_message,
                                )
                                .await;
                            }
                        } else {
                            info!("package job completed");
                        }
                    }
                    Err(err) => {
                        warn!(error = %err, "invalid package job payload");
                        if let Some(job_id) =
                            extract_package_job_id_from_raw_payload(&message.payload)
                        {
                            let err_message = format!("invalid package job payload: {err}");
                            let _ = update_package_job_status(
                                &state.pool,
                                job_id,
                                "failed",
                                json!({"error": err_message}),
                            )
                            .await;
                            let _ = mark_package_request_cache_failed(
                                &state.pool,
                                job_id,
                                "invalid_job_payload",
                                &err_message,
                            )
                            .await;
                        }
                    }
                }

                if let Err(err) =
                    archive_queue_message(&state.pool, &queue_name, message.msg_id).await
                {
                    error!(error = %err, msg_id = message.msg_id, "failed to archive queue message");
                }
            }
            Ok(None) => {
                sleep(poll_interval).await;
            }
            Err(err) => {
                error!(error = %err, "package queue read error");
                sleep(poll_interval).await;
            }
        }
    }
}

fn resolve_queue_name(requested: &str) -> String {
    if requested == "lca_jobs" {
        PACKAGE_QUEUE_NAME.to_owned()
    } else {
        requested.to_owned()
    }
}
