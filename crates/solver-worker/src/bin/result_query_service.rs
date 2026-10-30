//! Independently deployed, read-only result service with a dedicated disposable cache.
use clap::Parser;
use solver_worker::{
    calculation_bundle::query_artifact::transport,
    result_query::{
        self, Service,
        cache::{Cache, CacheLimits},
    },
};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long, env = "RESULT_QUERY_BIND", default_value = "127.0.0.1:8788")]
    bind: SocketAddr,
    /// Dedicated disposable directory, never a source-data directory.
    #[arg(long, env = "RESULT_QUERY_CACHE_DIR")]
    cache_dir: PathBuf,
    #[arg(long,env="RESULT_QUERY_CACHE_DISK_BYTES",default_value_t=20*1024*1024*1024)]
    disk_bytes: u64,
    #[arg(long,env="RESULT_QUERY_CACHE_MEMORY_BYTES",default_value_t=2*1024*1024*1024)]
    memory_bytes: u64,
    #[arg(long, env = "RESULT_QUERY_CACHE_ENTRIES", default_value_t = 8)]
    entries: usize,
    #[arg(long, env = "RESULT_QUERY_CONCURRENCY", default_value_t = 4)]
    concurrency: usize,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let secret = std::env::var("RESULT_QUERY_SERVICE_SECRET")
        .map_err(|_| anyhow::anyhow!("RESULT_QUERY_SERVICE_SECRET is required"))?;
    let cache = Cache::new(
        &args.cache_dir,
        transport::store_from_env()?,
        CacheLimits {
            disk_bytes: args.disk_bytes,
            memory_bytes: args.memory_bytes,
            entries: args.entries,
            ..CacheLimits::default()
        },
    )?;
    let app = result_query::router(Service::new(cache, secret.into_bytes(), args.concurrency)?);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
