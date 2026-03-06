use std::{net::SocketAddr, str::FromStr, time::Duration};

use clap::{Parser, ValueEnum};

/// Solver worker launch mode.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RunMode {
    /// Only queue worker.
    Worker,
    /// Only internal HTTP server.
    Http,
    /// Run both components in one process.
    Both,
}

/// CLI + env config.
#[derive(Debug, Clone, Parser)]
#[command(name = "solver-worker")]
pub struct AppConfig {
    /// Launch mode.
    #[arg(long, env = "SOLVER_MODE", default_value = "both")]
    pub mode: RunMode,
    /// `PostgreSQL` URL (preferred env: `DATABASE_URL`, fallback: `CONN`).
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,
    /// `PostgreSQL` URL fallback used by this project in local `.env`.
    #[arg(long, env = "CONN")]
    pub conn: Option<String>,
    /// Queue name in pgmq.
    #[arg(long, env = "PGMQ_QUEUE", default_value = "lca_jobs")]
    pub pgmq_queue: String,
    /// Poll interval for queue worker (ms).
    #[arg(long, env = "WORKER_POLL_MS", default_value_t = 1_000_u64)]
    pub worker_poll_ms: u64,
    /// Message visibility timeout for pgmq.read.
    #[arg(long, env = "WORKER_VT_SECONDS", default_value_t = 30_i32)]
    pub worker_vt_seconds: i32,
    /// Internal HTTP bind address.
    #[arg(long, env = "HTTP_ADDR", default_value = "0.0.0.0:8080")]
    pub http_addr: String,
    /// S3-compatible endpoint for large result artifacts.
    #[arg(long, env = "S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,
    /// S3 region.
    #[arg(long, env = "S3_REGION")]
    pub s3_region: Option<String>,
    /// S3 bucket.
    #[arg(long, env = "S3_BUCKET")]
    pub s3_bucket: Option<String>,
    /// S3 access key id for `SigV4` authenticated uploads.
    #[arg(long, env = "S3_ACCESS_KEY_ID")]
    pub s3_access_key_id: Option<String>,
    /// S3 secret access key for `SigV4` authenticated uploads.
    #[arg(long, env = "S3_SECRET_ACCESS_KEY")]
    pub s3_secret_access_key: Option<String>,
    /// Optional S3 session token for temporary credentials.
    #[arg(long, env = "S3_SESSION_TOKEN")]
    pub s3_session_token: Option<String>,
    /// Object key prefix under the bucket.
    #[arg(long, env = "S3_PREFIX", default_value = "lca-results")]
    pub s3_prefix: String,
}

impl AppConfig {
    /// Returns resolved database URL from `DATABASE_URL` or `CONN`.
    pub fn resolved_database_url(&self) -> anyhow::Result<&str> {
        self.database_url
            .as_deref()
            .or(self.conn.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing database URL: set DATABASE_URL or CONN environment variable"
                )
            })
    }

    /// Poll interval as Duration.
    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.worker_poll_ms)
    }

    /// Parsed http socket addr.
    pub fn http_socket_addr(&self) -> anyhow::Result<SocketAddr> {
        SocketAddr::from_str(&self.http_addr)
            .map_err(|err| anyhow::anyhow!("invalid HTTP_ADDR {}: {err}", self.http_addr))
    }
}
