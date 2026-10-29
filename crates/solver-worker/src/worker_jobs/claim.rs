use std::time::Duration;

use crate::pgbouncer_sqlx::{
    self as sqlx, PgConnection, PgPool, Postgres, Row, pool::PoolConnection,
};
use anyhow::Context;
use serde_json::Value;
use tokio::time::{sleep, timeout};
use tracing::warn;
use uuid::Uuid;

use super::{WorkerJob, ensure_ok};
use crate::db_pool::sql_string_literal;

#[derive(Clone, Copy)]
struct ClaimDeadlines {
    acquire: Duration,
    response: Duration,
    retry: Duration,
}

const DEADLINES: ClaimDeadlines = ClaimDeadlines {
    acquire: Duration::from_secs(30),
    response: Duration::from_secs(30),
    retry: Duration::from_secs(1),
};

// An interrupted simple-protocol query must never return its socket to the pool.
// Detach releases pool capacity; dropping the underlying connection closes the
// socket immediately, including when the caller cancels this future.
struct ClaimConnection(Option<PoolConnection<Postgres>>);

impl ClaimConnection {
    async fn acquire(pool: &PgPool, deadlines: ClaimDeadlines) -> anyhow::Result<Self> {
        let connection = timeout(deadlines.acquire, pool.acquire())
            .await
            .context("worker_claim_acquire_timeout")?
            .context("worker_claim_acquire_failed")?;
        Ok(Self(Some(connection)))
    }

    fn connection(&mut self) -> &mut PgConnection {
        self.0.as_mut().expect("claim connection already released")
    }

    fn release(mut self) {
        // Only after consuming ReadyForQuery for a successful COMMIT.
        drop(self.0.take());
    }
}

impl Drop for ClaimConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.0.take() {
            drop(connection.detach());
        }
    }
}

pub async fn claim_worker_jobs(
    pool: &PgPool,
    worker_queue: &str,
    worker_id: &str,
    limit: i32,
    lease_seconds: i32,
) -> anyhow::Result<Vec<WorkerJob>> {
    claim(
        pool,
        worker_queue,
        worker_id,
        limit,
        lease_seconds,
        DEADLINES,
    )
    .await
}

async fn claim(
    pool: &PgPool,
    worker_queue: &str,
    worker_id: &str,
    limit: i32,
    lease_seconds: i32,
    deadlines: ClaimDeadlines,
) -> anyhow::Result<Vec<WorkerJob>> {
    let mut connection = ClaimConnection::acquire(pool, deadlines).await?;
    // Do not send COMMIT until the complete claim response has arrived and all
    // leases have been decoded. Lost SELECT responses then roll back, rather
    // than consuming attempts for jobs whose identities the caller never saw.
    let sql = format!(
        "{} WITH _service_role AS (SELECT set_config('request.jwt.claim.role', \
         'service_role', true)) SELECT private.worker_claim_jobs({}, {}, {}, {}) \
         AS result FROM _service_role",
        transaction_settings(),
        sql_string_literal(worker_queue),
        sql_string_literal(worker_id),
        limit.clamp(1, 50),
        lease_seconds.clamp(1, 86_400),
    );
    // Executor with &str has no arguments/prepared statement (SQLx 0.8.6);
    // its boxed future also preserves Send across all spawned consumers.
    let rows = timeout(
        deadlines.response,
        sqlx::Executor::fetch_all(connection.connection(), sql.as_str()),
    )
    .await
    .context("worker_claim_response_timeout")?
    .context("worker_claim_response_failed")?;
    let result = rows
        .first()
        .context("worker_claim_jobs returned no response")?
        .try_get::<Value, _>("result")?;
    ensure_ok(&result, "worker_claim_jobs")?;
    let jobs = result
        .get("data")
        .and_then(Value::as_array)
        .context("worker_claim_jobs returned invalid data")?
        .iter()
        .map(WorkerJob::from_json)
        .collect::<anyhow::Result<Vec<_>>>()?;

    match timeout(
        deadlines.response,
        sqlx::Executor::execute(connection.connection(), "COMMIT"),
    )
    .await
    {
        Ok(Ok(_)) => {
            connection.release();
            Ok(jobs)
        }
        outcome => {
            warn!(?outcome, phase = "commit", "worker_claim_commit_uncertain");
            drop(connection);
            reconcile_until_settled(pool, jobs, deadlines).await
        }
    }
}

fn transaction_settings() -> &'static str {
    // LOCAL settings do not leak to other users of a transaction pooler. The
    // server deadline also releases locks if a broken transport delays closure.
    "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '25s'; \
     SET LOCAL idle_in_transaction_session_timeout = '30s'; \
     SET LOCAL \"request.jwt.claim.role\" = 'service_role';"
}

async fn reconcile_until_settled(
    pool: &PgPool,
    jobs: Vec<WorkerJob>,
    deadlines: ClaimDeadlines,
) -> anyhow::Result<Vec<WorkerJob>> {
    if jobs.is_empty() {
        return Ok(jobs);
    }
    // Keep the exact IDs/tokens across reconnects. A fresh claim is forbidden
    // until the uncertain transaction has settled, even during a DB outage.
    loop {
        match reconcile(pool, &jobs, deadlines).await {
            Ok(jobs) => return Ok(jobs),
            Err(error) => {
                warn!(%error, phase = "reconcile", "worker_claim_reconcile_retry");
                sleep(deadlines.retry).await;
            }
        }
    }
}

async fn reconcile(
    pool: &PgPool,
    jobs: &[WorkerJob],
    deadlines: ClaimDeadlines,
) -> anyhow::Result<Vec<WorkerJob>> {
    let mut connection = ClaimConnection::acquire(pool, deadlines).await?;
    let ids = jobs
        .iter()
        .map(|job| format!("'{}'::uuid", job.id))
        .collect::<Vec<_>>()
        .join(",");
    // Lock IDs without filtering on lease/status: at READ COMMITTED this waits
    // for the original COMMIT/rollback and sees its settled row. A plain SELECT
    // could observe the old queued row while a delayed COMMIT is still pending.
    let sql = format!(
        "{} WITH settled AS MATERIALIZED (SELECT id, lease_token, status, lease_expires_at \
         FROM private.worker_jobs WHERE id IN ({ids}) ORDER BY id FOR UPDATE) \
         SELECT id, lease_token, status, lease_expires_at > clock_timestamp() AS live FROM settled;",
        transaction_settings(),
    );
    let rows = timeout(
        deadlines.response,
        sqlx::Executor::fetch_all(connection.connection(), sql.as_str()),
    )
    .await
    .context("worker_claim_reconcile_timeout")??;
    let mut recovered = Vec::new();
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let token: Option<Uuid> = row.try_get("lease_token")?;
        let live: Option<bool> = row.try_get("live")?;
        let running = row.try_get::<String, _>("status")? == "running";
        if let Some(job) = jobs
            .iter()
            .find(|job| job.id == id && Some(job.lease_token) == token)
            && running
            && live == Some(true)
        {
            recovered.push(job.clone());
        }
    }
    timeout(
        deadlines.response,
        sqlx::Executor::execute(connection.connection(), "COMMIT"),
    )
    .await
    .context("worker_claim_reconcile_commit_timeout")??;
    connection.release();
    Ok(recovered)
}

#[cfg(test)]
mod tests;
