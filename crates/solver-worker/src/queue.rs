use std::sync::Arc;

use tokio::time::sleep;
use tracing::{error, info, instrument, warn};

use crate::{
    db::{
        AppState, archive_queue_message, handle_job_payload, mark_result_cache_failed,
        read_one_queue_message, update_job_status,
    },
    types::JobPayload,
};

/// Runs pgmq polling loop.
#[instrument(skip(state))]
pub async fn run_worker_loop(
    state: Arc<AppState>,
    queue_name: String,
    vt_seconds: i32,
    poll_interval: std::time::Duration,
) -> anyhow::Result<()> {
    loop {
        match read_one_queue_message(&state.pool, &queue_name, vt_seconds).await {
            Ok(Some(message)) => {
                let parsed = serde_json::from_value::<JobPayload>(message.payload.clone());
                match parsed {
                    Ok(payload) => {
                        if let Err(err) = handle_job_payload(&state, payload.clone()).await {
                            error!(error = %err, "job execution failed");
                            let job_id = extract_job_id(&payload);
                            let err_message = err.to_string();
                            let _ = update_job_status(
                                &state.pool,
                                job_id,
                                "failed",
                                serde_json::json!({"error": err_message}),
                            )
                            .await;
                            let _ = mark_result_cache_failed(
                                &state.pool,
                                job_id,
                                "job_execution_failed",
                                &err_message,
                            )
                            .await;
                        } else {
                            info!("job completed");
                        }
                    }
                    Err(err) => {
                        warn!(error = %err, "invalid job payload");
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
                error!(error = %err, "queue read error");
                sleep(poll_interval).await;
            }
        }
    }
}

fn extract_job_id(payload: &JobPayload) -> uuid::Uuid {
    match payload {
        JobPayload::PrepareFactorization { job_id, .. }
        | JobPayload::SolveOne { job_id, .. }
        | JobPayload::SolveBatch { job_id, .. }
        | JobPayload::InvalidateFactorization { job_id, .. }
        | JobPayload::RebuildFactorization { job_id, .. } => *job_id,
    }
}
