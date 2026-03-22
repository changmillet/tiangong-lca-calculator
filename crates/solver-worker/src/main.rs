use std::sync::Arc;

use axum::serve;
use clap::Parser;
use solver_worker::{
    config::{AppConfig, MIN_RECOMMENDED_WORKER_VT_SECONDS, RunMode},
    db::AppState,
    http, queue,
};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = AppConfig::parse();
    let effective_vt_seconds = config.effective_worker_vt_seconds();
    if effective_vt_seconds < MIN_RECOMMENDED_WORKER_VT_SECONDS {
        warn!(
            configured_vt_seconds = config.worker_vt_seconds,
            recommended_min_vt_seconds = MIN_RECOMMENDED_WORKER_VT_SECONDS,
            "worker visibility timeout is shorter than recommended for build_snapshot jobs; long tasks may be re-delivered"
        );
    }
    let state = Arc::new(AppState::new(&config).await?);

    match config.mode {
        RunMode::Worker => {
            info!("starting queue worker mode");
            let queue_name = config.pgmq_queue.clone();
            queue::run_worker_loop(
                state,
                queue_name,
                effective_vt_seconds,
                config.poll_interval(),
            )
            .await?;
        }
        RunMode::Http => {
            info!("starting internal HTTP mode");
            run_http(state, config.http_socket_addr()?).await?;
        }
        RunMode::Both => {
            info!("starting worker + internal HTTP mode");
            let worker_state = Arc::clone(&state);
            let queue_name = config.pgmq_queue.clone();
            let vt = effective_vt_seconds;
            let poll = config.poll_interval();
            let worker_handle = tokio::spawn(async move {
                queue::run_worker_loop(worker_state, queue_name, vt, poll).await
            });

            let http_handle =
                tokio::spawn(run_http(Arc::clone(&state), config.http_socket_addr()?));

            tokio::select! {
                worker_result = worker_handle => {
                    worker_result??;
                }
                http_result = http_handle => {
                    http_result??;
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("ctrl-c received, exiting");
                }
            }
        }
    }

    Ok(())
}

async fn run_http(state: Arc<AppState>, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let app = http::router(state);
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "internal HTTP listening");
    serve(listener, app).await?;
    Ok(())
}
